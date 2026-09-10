//! 新闻面板的渲染（docs/25 §5、N2）。
//!
//! 两区：**源健康**在上、**时间线**在下。
//!
//! 健康区放在最上面是有理由的：这一页最容易出的错不是「新闻不好看」，
//! 是**一个源悄悄死了而列表看起来一切正常**（§3.1 的 WSJ，实测陈了
//! 589 天仍返回 200）。把健康藏在下面等于把这件事藏起来。

use iced::widget::{button, column, container, pick_list, row, scrollable, text, text_input};
use iced::{Color, Element, Length};

use super::news_readout::{self as ro, ago, NewsRow, SourcePick, SourceRow, View};

const C_HEAD: Color = Color::from_rgb(0.55, 0.8, 1.0);
const C_DIM: Color = Color::from_rgb(0.55, 0.55, 0.6);
const C_TXT: Color = Color::from_rgb(0.85, 0.87, 0.92);
const C_GOLD: Color = Color::from_rgb(0.9, 0.8, 0.4);
const C_BAD: Color = Color::from_rgb(0.9, 0.45, 0.4);
const C_OK: Color = Color::from_rgb(0.35, 0.78, 0.98);
/// 可点标题的颜色。**整行里最亮的东西**——它是唯一可交互的部分。
const C_LINK: Color = Color::from_rgb(0.55, 0.88, 1.00);
const C_LINK_HOVER: Color = Color::from_rgb(0.82, 0.95, 1.00);

/// 标题链接的样式。
///
/// 「点了没反应」查了半天之后（xdg-open 那个 bug），另一半问题是
/// **看不出哪儿能点**。所以三件事一起给：
///
/// 1. 静止时就用**明显不同于正文的链接色**，不用等鼠标悬上去；
/// 2. 悬停时变亮 **并且给一块背景**——颜色变化对色觉不敏感的人不够，
///    背景块是形状上的变化；
/// 3. 按下时收一点，给一个按到了的回执。
fn link_style(_t: &iced::Theme, status: iced::widget::button::Status) -> iced::widget::button::Style {
    use iced::widget::button::Status;
    let (text_color, bg) = match status {
        Status::Hovered => (C_LINK_HOVER, Some(Color { a: 0.14, ..C_LINK })),
        Status::Pressed => (C_LINK_HOVER, Some(Color { a: 0.24, ..C_LINK })),
        Status::Disabled => (C_DIM, None),
        Status::Active => (C_LINK, None),
    };
    iced::widget::button::Style {
        text_color,
        background: bg.map(Into::into),
        border: iced::Border { radius: 3.0.into(), ..Default::default() },
        ..Default::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewsMsg {
    Start,
    Stop,
    /// 打开原文。
    Open(String),
    /// 折叠/展开源健康区。
    ToggleHealth,
    /// 筛选框在打字。**只改面板内存**——守护照常收全部，筛选只影响这一屏。
    FilterEdited(String),
    /// 订阅框在打字。
    WatchEdited(String),
    /// 把输入框里的标的加进订阅。
    WatchAdd,
    /// 退订一个标的。
    WatchRemove(String),
    /// 检索框在打字。
    SearchEdited(String),
    /// 跑一次检索。**按需，不轮询**——一条检索是给人看的一次动作。
    SearchRun,
    /// 清掉检索结果。
    SearchClear,
    /// 切视图。
    SetView(View),
    /// 开/关一个源。
    ToggleSource(String, bool),
    /// 删掉一个自定义源。
    DeleteSource(String),
    /// 测一个源。
    ProbeSource(String),
    /// 加源表单在打字。
    AddEdited(&'static str, String),
    /// 提交加源。
    AddSource,
    /// 下拉选了一个源（`None` = 全部）。
    PickSource(SourcePick),
}

fn chip<'a>(label: &str, msg: NewsMsg) -> Element<'a, NewsMsg> {
    button(text(label.to_string()).size(11))
        .padding([2, 7])
        .style(|t, st| crate::style::button::modifier(t, st, false))
        .on_press(msg)
        .into()
}

fn chip_on<'a>(label: &str, active: bool, msg: NewsMsg) -> Element<'a, NewsMsg> {
    button(text(label.to_string()).size(11))
        .padding([2, 7])
        .style(move |t, st| crate::style::button::modifier(t, st, active))
        .on_press(msg)
        .into()
}

fn cell<'a>(s: String, w: f32, c: Color, numeric: bool) -> Element<'a, NewsMsg> {
    container(text(s).size(11).color(c))
        .width(Length::Fixed(w))
        .align_x(if numeric { iced::Alignment::End } else { iced::Alignment::Start })
        .into()
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    format!("{}…", s.chars().take(n).collect::<String>())
}

/// 分级的颜色。一级来源要看得出来——媒体是派生的、更晚的。
///
/// 都比 [`C_LINK`] 暗：**整行里最亮的必须是可点的那部分**，
/// 否则「哪儿能点」这件事又要靠猜。
fn tier_color(tier: &str) -> Color {
    match tier {
        "regulator" => Color { a: 0.85, ..C_OK },
        "exchange" => Color { a: 0.80, ..C_HEAD },
        "corporate" => Color { a: 0.80, ..C_TXT },
        _ => C_DIM,
    }
}

/// 一个源的状态灯：`(灯, 颜色, 说明)`。
///
/// 四个状态要分得开，尤其是**「取得到但是陈的」和「取不到」**——
/// 前者只看状态码永远发现不了，后者一眼就能看见。
pub fn source_lamp(s: &SourceRow) -> (&'static str, Color, String) {
    if s.failing() {
        return ("✕", C_BAD, format!("连续失败 {} 次", s.consecutive_fails));
    }
    if s.is_stale {
        // **这一行是这一页的理由**
        let d = s.stale_secs.unwrap_or(0) / 86400;
        return ("⚠", C_BAD, format!("陈了 {d} 天（一直返回 200）"));
    }
    if s.no_timestamps {
        // 不是故障：源就是不给时间。但要说出来——看门狗对它用的是
        // 另一套判据（我们上次见到新条目是什么时候）
        return ("◍", C_HEAD, "源不给时间戳，按「上次有新条目」判新鲜度".into());
    }
    if s.never_seen() {
        return ("○", C_DIM, "还没抓到".into());
    }
    ("●", C_OK, String::new())
}

pub fn pane_body<'a>() -> Element<'a, NewsMsg> {
    let st = ro::snapshot();
    let mut body = column![].spacing(4).padding(8);

    // ── 顶栏 ──
    let stale_txt = if st.stale_sources > 0 {
        format!("⚠ {} 个源已陈", st.stale_sources)
    } else {
        "源全部新鲜".into()
    };
    body = body.push(
        row![
            text("新闻资讯").size(13).color(C_TXT),
            chip("▶ 启动", NewsMsg::Start),
            chip("■ 停止", NewsMsg::Stop),
            text(if st.svc.active {
                format!("守护运行中 {}s", st.svc.uptime_secs)
            } else {
                "守护未运行".into()
            })
            .size(10)
            .color(if st.svc.active { C_DIM } else { C_GOLD }),
            text(format!("{} 条在窗内", st.total)).size(11).color(C_TXT),
            text(stale_txt).size(11).color(if st.stale_sources > 0 { C_BAD } else { C_DIM }),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
    );

    // ── 视图切换 ──
    //
    // 默认是「新闻」：**打开就该看到新闻**，而不是先看一屏运维信息。
    // 源健康和源管理放到第二个视图里——它们重要，但不是每次打开都要看的
    let v = ro::view();
    let mut vr = row![].spacing(4).align_y(iced::Alignment::Center);
    for x in View::ALL {
        vr = vr.push(chip_on(x.label(), x == v, NewsMsg::SetView(x)));
    }
    // 有源出问题时，在「新闻」视图上也要看得见——否则一个源死了
    // 而用户从不切到第二页，就永远不知道
    if v == View::Feed && st.stale_sources > 0 {
        vr = vr.push(
            text(format!("⚠ {} 个源已陈，去「源管理」看", st.stale_sources))
                .size(10)
                .color(C_BAD),
        );
    }
    body = body.push(vr);

    if !st.present || st.sources.is_empty() {
        body = body.push(
            text("还没有快照——守护没起，或者刚起还没抓完第一轮").size(11).color(C_DIM),
        );
        return scrollable(body).width(Length::Fill).height(Length::Fill).into();
    }

    if v == View::Watch {
        body = body.push(watch_view(&st));
        return scrollable(body).width(Length::Fill).height(Length::Fill).into();
    }

    if v == View::Sources {
        // **只追加内容，不 return 一整棵树。**
        // 一度是 `return sources_view(...)`，那棵树里没有上面那行视图切换——
        // 进了源管理就再也回不去新闻页。现在头部由这里统一画，
        // 分支只能产出「内容」，结构上没有漏掉切换器的可能
        body = body.push(sources_view(&st));
        return scrollable(body).width(Length::Fill).height(Length::Fill).into();
    }

    // ── 时间线 ──
    let q = ro::filter_text();
    let pick = ro::source_pick();
    // 下拉和文本筛选是 **AND**：选了「美联储」再打个关键词，
    // 人的预期是「美联储的、并且含这个词的」
    let shown: Vec<&ro::NewsRow> = st
        .items
        .iter()
        .filter(|i| ro::matches_source(&pick, i) && ro::matches(&q, i))
        .collect();

    // 下拉的选项：带条数。**哪些源现在真的有新闻**比源的名字更有用
    let mut opts: Vec<SourcePick> = vec![SourcePick {
        id: None,
        label: "全部来源".into(),
        count: st.items.len() as i64,
    }];
    {
        let mut per: std::collections::HashMap<&str, i64> = Default::default();
        for i in &st.items {
            *per.entry(i.source.as_str()).or_default() += 1;
            for d in &i.dupes {
                *per.entry(d.as_str()).or_default() += 1;
            }
        }
        // 有新闻的排前面，其次按名字——一个 0 条的源沉在下面就够了，
        // 但**不能不列**：不列的话用户以为这个源没配上
        let mut ss: Vec<&SourceRow> = st.sources.iter().collect();
        ss.sort_by_key(|s| (-per.get(s.id.as_str()).copied().unwrap_or(0), s.label.clone()));
        for s in ss {
            opts.push(SourcePick {
                id: Some(s.id.clone()),
                label: s.label.clone(),
                count: per.get(s.id.as_str()).copied().unwrap_or(0),
            });
        }
    }
    let selected = opts
        .iter()
        .find(|o| o.id == pick)
        // 选中的源被删掉/关掉之后，下拉会指向一个不存在的项。
        // 回落到「全部」而不是留个空框
        .cloned()
        .unwrap_or_else(|| opts[0].clone());
    body = body.push(
        row![
            text("时间线").size(11).color(C_HEAD),
            pick_list(opts.clone(), Some(selected), NewsMsg::PickSource).text_size(11).padding([2, 6]),
            text_input("筛选：词=都要有 · \"词组\" · -排除 · tier:/kind:/sym:/lang:/src:", &q)
                .on_input(NewsMsg::FilterEdited)
                .size(11)
                .padding([2, 6])
                .width(Length::Fixed(420.0)),
            // **筛掉了多少必须说**。只显示一张短表的话，
            // 「筛选筛没了」和「本来就没有」分不出来
            text(if q.trim().is_empty() && pick.is_none() {
                format!("{} 条", st.items.len())
            } else {
                format!("{} / {} 条（筛掉 {}）", shown.len(), st.items.len(), st.items.len() - shown.len())
            })
            .size(10)
            .color(if shown.is_empty() && !st.items.is_empty() { C_GOLD } else { C_DIM }),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
    );
    body = body.push(
        text("`~` = 源没给发布时间，这里显示的是我们抓到的时刻").size(10).color(C_DIM),
    );
    for it in shown.iter().take(120) {
        body = body.push(item_row(it, st.now_ms));
    }
    if st.items.is_empty() {
        body = body.push(text("窗内还没有条目").size(11).color(C_DIM));
    } else if shown.is_empty() {
        // 空列表要说清是**筛选**筛没的，不是没新闻——
        // 而且要说清是哪一个筛的：下拉还是关键词
        let who = match (pick.is_some(), !q.trim().is_empty()) {
            (true, true) => "这个来源 + 这条关键词",
            (true, false) => "这个来源",
            _ => "这条筛选",
        };
        body = body.push(
            text(format!("{who}把 {} 条全筛掉了", st.items.len())).size(11).color(C_GOLD),
        );
    }

    body = body.push(
        text(format!("快照 {} · 读于 {}", st.stamp, st.refreshed)).size(10).color(C_DIM),
    );
    scrollable(body).width(Length::Fill).height(Length::Fill).into()
}

/// 第二个视图：**源健康 + 源管理**。
///
/// 两件事放一起是有理由的：判断一个源该不该留，靠的就是它的健康
/// （多久没更新、失败几次）。分成两页的话，用户得在两页之间来回对。
/// 第二个视图：**按标的订阅 + SEC 全文检索**。
///
/// 和「新闻」分开是因为它们是两种动作：新闻是「让我看看发生了什么」，
/// 这一页是「我要找某个东西」。混在一屏里，前者的入口被后者的表单挤下去。
///
/// 订阅**结果仍然进时间线**（源是 `sec-watch`）——这一页管的是订阅的
/// 增删，不是把申报藏起来。
fn watch_view<'a>(st: &ro::NewsReadout) -> iced::widget::Column<'a, NewsMsg> {
    let mut body = column![].spacing(4);
    // ── 按标的订阅（N5）──
    let mut wr = row![
        text("订阅").size(11).color(C_HEAD),
        text_input("标的代码，如 AAPL（回车添加）", &ro::watch_input())
            .on_input(NewsMsg::WatchEdited)
            .on_submit(NewsMsg::WatchAdd)
            .size(11)
            .padding([2, 6])
            .width(Length::Fixed(200.0)),
        chip("添加", NewsMsg::WatchAdd),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);
    for w in &st.watch {
        // 每一个都列，**包括一条申报都没抓到的**——不列的话，
        // 「这只票 SEC 认不出来」会看起来像「它最近没申报」
        let ok = w.fails == 0 && w.ok > 0;
        wr = wr.push(
            row![
                text(format!("{} ({})", w.symbol, w.in_window))
                    .size(11)
                    .color(if ok { C_OK } else { C_GOLD }),
                chip("×", NewsMsg::WatchRemove(w.symbol.clone())),
            ]
            .spacing(2)
            .align_y(iced::Alignment::Center),
        );
    }
    body = body.push(wr.wrap());
    if !st.watch.is_empty() {
        body = body.push(
            text("SEC 一手申报，默认只收重大表格（8-K/10-Q/13D…）——\
                  Form 4 内部人交易占了申报总量的六成，收进来会把 8-K 埋掉")
                .size(10)
                .color(C_DIM),
        );
    }

    // ── SEC 全文检索（N5 的另一半）──
    //
    // 结果**单独一段**，不混进时间线：时间线是「最近发生了什么」，
    // 这里是「帮我找东西」。混进去的话，一次搜索会在时间线里塞几十条
    // 几个月前的东西，而它们看起来和刚发生的一样
    body = body.push(
        row![
            text("SEC 全文检索").size(11).color(C_HEAD),
            text_input("在所有申报里找一个词，如 material weakness（回车）", &ro::search_input())
                .on_input(NewsMsg::SearchEdited)
                .on_submit(NewsMsg::SearchRun)
                .size(11)
                .padding([2, 6])
                .width(Length::Fixed(320.0)),
            chip("检索", NewsMsg::SearchRun),
            chip("清空", NewsMsg::SearchClear),
            // 失败也要说出来——空结果和「没搜到」看起来一样
            text(st.search.status.clone())
                .size(10)
                .color(if st.search.status.contains("失败") || st.search.status.contains("取不到") {
                    C_BAD
                } else {
                    C_DIM
                }),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center),
    );
    for h in st.search.hits.iter().take(40) {
        let what = if h.what.is_empty() { String::new() } else { format!(" · {}", h.what) };
        body = body.push(
            row![
                cell(h.date.clone(), 82.0, C_DIM, false),
                cell(h.form.clone(), 54.0, tier_color("regulator"), false),
                // 和时间线一致：标题就是链接
                button(text(format!("{}{}", h.who, what)).size(11).width(Length::Fill))
                    .padding([1, 4])
                    .style(link_style)
                    .on_press(NewsMsg::Open(h.url.clone()))
                    .width(Length::Fill),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center),
        );
    }

    body
}

fn sources_view<'a>(st: &ro::NewsReadout) -> iced::widget::Column<'a, NewsMsg> {
    let mut body = column![].spacing(4);

    // ── 加一个源 ──
    let (id, label, url, tier) = ro::add_form();
    body = body.push(
        row![
            text("加源").size(11).color(C_HEAD),
            text_input("id", &id).on_input(|t| NewsMsg::AddEdited("id", t)).size(11).padding([2, 6]).width(Length::Fixed(96.0)),
            text_input("显示名", &label).on_input(|t| NewsMsg::AddEdited("label", t)).size(11).padding([2, 6]).width(Length::Fixed(120.0)),
            text_input("RSS / Atom 地址", &url)
                .on_input(|t| NewsMsg::AddEdited("url", t))
                .on_submit(NewsMsg::AddSource)
                .size(11).padding([2, 6]).width(Length::Fixed(300.0)),
            text_input("分级(media)", &tier).on_input(|t| NewsMsg::AddEdited("tier", t)).size(11).padding([2, 6]).width(Length::Fixed(96.0)),
            chip("先测一下", NewsMsg::ProbeSource(url.clone())),
            chip("添加", NewsMsg::AddSource),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center)
        .wrap());
    let note = ro::add_note();
    if !note.is_empty() {
        body = body.push(
            text(note.clone()).size(10).color(if note.starts_with('✔') { C_OK } else { C_BAD }));
    }
    body = body.push(
        text("只能加 RSS / Atom。JSON 接口（交易所公告那种）要写提取规则，那是代码不是配置——\
              在配置里填错一个字段的表现是「这个源什么都抓不到」，查不出为什么")
            .size(10)
            .color(C_DIM));

    // ── 测试结果 ──
    if let Some(p) = &st.probe {
        body = body.push(
            row![
                text(if p.ok { "✔" } else { "✗" }).size(13).color(if p.ok { C_OK } else { C_BAD }),
                text(clip(&p.url, 52)).size(10).color(C_DIM),
                text(format!("HTTP {} · {}ms · {}B", p.status, p.ms, p.bytes)).size(10).color(C_DIM),
                text(p.verdict.clone()).size(11).color(if p.ok { C_OK } else { C_GOLD }),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center)
            .wrap());
    }

    // ── 源健康 + 管理 ──
    body = body.push(
        row![
            text("源").size(11).color(C_HEAD),
            text(format!("{} 个，{} 个开着", st.sources.len(), st.sources.iter().filter(|s| s.enabled).count()))
                .size(10)
                .color(C_DIM),
            text("HTTP 200 不等于有新闻——「见过的最新」那一列才是判断依据").size(10).color(C_DIM),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center));
    let mut h = row![].spacing(4);
    for (t, w, n) in [
        ("", 20.0, false),
        ("源", 118.0, false),
        ("分级", 52.0, false),
        ("见过的最新", 80.0, false),
        ("窗内", 40.0, true),
        ("拉/未变/失败", 100.0, false),
        ("状态", 210.0, false),
        ("", 150.0, false),
    ] {
        h = h.push(cell(t.into(), w, C_HEAD, n));
    }
    body = body.push(h);

    // 有问题的排在前面：一片正常里混着一行红，很容易被翻过去
    let mut rows: Vec<&SourceRow> = st.sources.iter().collect();
    rows.sort_by_key(|s| (!(s.is_stale || s.failing()), !s.enabled, s.label.clone()));
    for s in rows {
        let (lamp, lc, why) = source_lamp(s);
        // 关掉的源仍然列出来——删掉和关掉是两回事
        let (lamp, lc) = if s.enabled { (lamp, lc) } else { ("◌", C_DIM) };
        // 三种「没有时间」要分开：源不给时间 / 还没抓到 / 真的陈了
        let newest = match (s.newest_ms, s.no_timestamps, s.last_new_ms) {
            (Some(m), _, _) => ago(m, st.now_ms),
            // 源根本不给时间戳（实测 ESMA）。显示成「—」的话，
            // 一个好源看起来像没通
            (None, true, Some(t)) => format!("源无时间·{}", ago(t, st.now_ms)),
            (None, true, None) => "源无时间".into(),
            _ => "—".into(),
        };
        let mut r = row![
            cell(lamp.into(), 20.0, lc, false),
            cell(s.label.clone(), 118.0, if s.enabled { C_TXT } else { C_DIM }, false),
            cell(s.tier_label.clone(), 52.0, tier_color(&s.tier), false),
            cell(newest, 80.0, if s.is_stale { C_BAD } else { C_DIM }, false),
            cell(s.in_window.to_string(), 40.0, C_DIM, true),
            cell(
                format!("{}/{}/{}", s.ok, s.not_modified, s.fails),
                100.0,
                if s.fails > 0 { C_GOLD } else { C_DIM },
                false
            ),
            cell(
                if !s.enabled {
                    "已关闭".into()
                } else if why.is_empty() {
                    clip(&s.last_status, 26)
                } else {
                    why
                },
                210.0,
                lc,
                false
            ),
        ]
        .spacing(4)
        .align_y(iced::Alignment::Center);
        r = r.push(chip(if s.enabled { "关闭" } else { "启用" }, NewsMsg::ToggleSource(s.id.clone(), !s.enabled)));
        r = r.push(chip("测试", NewsMsg::ProbeSource(s.id.clone())));
        // **内置的不给删按钮**：删了下次启动又被播种回来，
        // 那种「删不掉」比不给删更让人困惑
        if !s.builtin {
            r = r.push(chip("删除", NewsMsg::DeleteSource(s.id.clone())));
        }
        body = body.push(r);
    }

    body
}

fn item_row<'a>(it: &NewsRow, now: i64) -> Element<'a, NewsMsg> {
    // 时间是猜的就打个记号。一条三天前的公告显示成刚刚发布，
    // 比不显示时间糟得多
    let when = format!("{}{}", ago(it.sort_ms, now), if it.time_guessed { "~" } else { "" });
    let mut r = row![
        cell(when, 62.0, if it.time_guessed { C_GOLD } else { C_DIM }, false),
        cell(it.source.clone(), 92.0, tier_color(&it.tier), false),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);

    if !it.symbols.is_empty() {
        r = r.push(cell(it.symbols.join(","), 70.0, C_HEAD, false));
    }
    // **标题本身就是链接**，点了在浏览器里看原文。
    //
    // 不做成末尾一个小「原文」按钮：那个按钮的点击目标只有几十像素，
    // 而标题横跨整行——用户想点的本来就是标题。
    //
    // 标题**完整显示**，长了折行——地址那次的教训（docs/24）：
    // 定宽格子会把区别切掉，而新闻标题的区别常常在后半截
    r = r.push(if it.url.is_empty() {
        // 没有链接的（Deribit 那种没有单条页面的）显示成普通文本，
        // 不给一个点了没反应的假按钮
        Element::from(text(it.title.clone()).size(11).color(C_DIM).width(Length::Fill))
    } else {
        button(text(it.title.clone()).size(11).width(Length::Fill))
            // 上下留 1px、左右 4px：悬停时那块背景才有形状，
            // padding 全 0 的话背景紧贴字，看着像渲染错误
            .padding([1, 4])
            .style(link_style)
            .on_press(NewsMsg::Open(it.url.clone()))
            .width(Length::Fill)
            .into()
    });

    // 「说的那件事什么时候发生」和「什么时候说的」不是一回事
    if let Some(e) = it.effective_ms {
        let gap = (e - it.sort_ms) / 86_400_000;
        if gap.abs() >= 1 {
            r = r.push(
                text(format!("生效 {}", ago(e, now))).size(10).color(C_GOLD),
            );
        }
    }
    if !it.dupes.is_empty() {
        r = r.push(text(format!("+{} 家", it.dupes.len())).size(10).color(C_DIM));
    }
    if it.revised {
        r = r.push(text("改过").size(10).color(C_GOLD));
    }
    r.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(is_stale: bool, fails: i64, newest: Option<i64>) -> SourceRow {
        SourceRow {
            id: "x".into(),
            label: "X".into(),
            is_stale,
            consecutive_fails: fails,
            newest_ms: newest,
            stale_secs: newest.map(|_| 589 * 86400),
            ok: 100,
            ..Default::default()
        }
    }

    #[test]
    fn a_source_that_fetches_fine_but_serves_old_news_is_red() {
        // **N2 的判据。** WSJ 实测陈了 589 天仍返回 200——
        // 「取得到」和「取到的是新的」是两件事
        let (lamp, c, why) = source_lamp(&src(true, 0, Some(1)));
        assert_eq!(lamp, "⚠");
        assert_eq!(c, C_BAD, "必须是红的");
        assert!(why.contains("589") && why.contains("200"), "{why}");
    }

    /// 视图切换器**必须每个视图都在**。
    ///
    /// 踩过一次：`sources_view` 直接 `return` 了自己的一整棵树，那棵树里
    /// 没有切换器——**点进源管理就再也回不去新闻页**。
    ///
    /// 修法不是「记得也画一份」，是让它结构上不可能：头部由 `pane_body`
    /// 统一画，分支只能产出**内容**（`Vec<Element>`），没有 return 一整棵树
    /// 的机会。这条测试钉的就是那个签名。
    #[test]
    fn a_sub_view_returns_content_not_a_whole_tree() {
        let src = include_str!("news_view.rs");
        let body = src.split("#[cfg(test)]").next().unwrap();
        // 子视图**返回 Column（内容），不返回 Element（整棵树）**。
        // 返回整棵树的那一版里没有头部，进了子视图就再也切不回来
        for f in ["sources_view", "watch_view"] {
            assert!(
                body.contains(&format!(
                    "fn {f}<'a>(st: &ro::NewsReadout) -> iced::widget::Column<'a, NewsMsg>"
                )),
                "{f} 必须只产出内容；返回 Element 的话，头部（含视图切换）会被丢掉"
            );
        }
        // 每个视图都要有分支，否则新加的视图会显示成空白
        for v in ["View::Watch", "View::Sources"] {
            assert!(body.contains(&format!("if v == {v}")), "{v} 没有对应的分支");
        }
        // 切换器要在**所有**分支之前画——只比第一个分支的话，
        // 以后在中间插一个视图仍然会漏
        let switcher = body.find("NewsMsg::SetView").expect("要有视图切换");
        for v in ["View::Watch", "View::Sources"] {
            let branch = body.find(&format!("if v == {v}")).unwrap();
            assert!(switcher < branch, "视图切换必须画在 {v} 的分支之前");
        }
    }

    #[test]
    fn a_source_without_timestamps_is_not_shown_as_broken() {
        // 实测 ESMA 的条目只有 title/link/description。显示成「还没抓到」
        // 或者一个红叉的话，一个好源看起来像坏的
        let s = SourceRow { no_timestamps: true, ok: 50, ..Default::default() };
        let (lamp, c, why) = source_lamp(&s);
        assert_eq!(lamp, "◍");
        assert_ne!(c, C_BAD, "不给时间戳不是故障");
        assert!(why.contains("不给时间"), "{why}");
    }

    #[test]
    fn failing_and_stale_are_different_lamps() {
        // 取不到（网络/被拒）和取得到但是陈的，要分得开：
        // 前者查网络，后者查源本身
        let (l1, _, w1) = source_lamp(&src(false, 3, Some(1)));
        assert_eq!(l1, "✕");
        assert!(w1.contains("连续失败"));

        // 从没抓到又是第三种：刚启动而已，不是故障
        let (l2, c2, w2) = source_lamp(&src(false, 0, None));
        assert_eq!(l2, "○");
        assert_ne!(c2, C_BAD, "刚启动不该是红的");
        assert!(w2.contains("还没抓到"));

        // 正常
        assert_eq!(source_lamp(&src(false, 0, Some(1))).0, "●");
    }

    #[test]
    fn a_failure_outranks_staleness_because_it_is_more_actionable() {
        // 同时又失败又陈：先说失败——取不到的话，陈不陈是后话
        assert_eq!(source_lamp(&src(true, 2, Some(1))).0, "✕");
    }

    #[test]
    fn a_clickable_title_looks_clickable_before_the_mouse_gets_there() {
        use iced::widget::button::Status;
        let t = iced::Theme::Dark;
        let active = link_style(&t, Status::Active);
        let hover = link_style(&t, Status::Hovered);

        // 静止时就该和正文不一样——不能等悬停才显形
        assert_ne!(active.text_color, C_TXT);
        assert_ne!(active.text_color, C_DIM);
        assert_eq!(active.text_color, C_LINK);

        // 悬停有两个变化：变亮 + 出现背景块。
        // 只靠颜色变化的话，对色觉不敏感的人等于没有反馈
        assert_ne!(hover.text_color, active.text_color);
        assert!(active.background.is_none());
        assert!(hover.background.is_some(), "悬停要给一块背景，颜色变化不够");

        // 按下要有回执，且比悬停更明显
        let pressed = link_style(&t, Status::Pressed);
        assert!(pressed.background.is_some());
    }

    #[test]
    fn nothing_else_in_a_news_row_outshines_the_link() {
        // 整行里最亮的必须是可点的那部分，否则「哪儿能点」又要靠猜。
        //
        // 比的是**这一行里实际用到的颜色**，不是全局的 C_TXT——
        // 拿近白的正文色比，任何一个还认得出是蓝色的链接色都会输，
        // 而正文色在新闻行里根本不出现（标题非链接时用的是 C_DIM）
        fn lum(c: Color) -> f32 {
            (0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b) * c.a
        }
        let in_a_row = [
            ("时间", C_DIM),
            ("时间是猜的 / 生效时间", C_GOLD),
            ("分级-监管", tier_color("regulator")),
            ("分级-交易所", tier_color("exchange")),
            ("分级-公司", tier_color("corporate")),
            ("分级-媒体", tier_color("media")),
            ("标的", C_HEAD),
        ];
        for (what, c) in in_a_row {
            assert!(lum(C_LINK) > lum(c), "{what} 比链接还亮");
        }
        // 而且要亮出可感知的差距，不是小数点后第三位
        assert!(lum(C_LINK) - lum(C_GOLD) > 0.02, "和最亮的那个只差一点，等于没差");
    }

    #[test]
    fn primary_sources_are_visually_distinct_from_media() {
        // 一个把 Fed 原文和转述画成一样的界面，没抓住重点
        assert_ne!(tier_color("regulator"), tier_color("media"));
        assert_ne!(tier_color("exchange"), tier_color("media"));
        assert_eq!(tier_color("aggregator"), tier_color("media"), "聚合和媒体同档即可");
    }
}
