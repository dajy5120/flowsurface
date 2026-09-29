//! 网络出口总闸的渲染。
//!
//! 一页看全「谁在往外发包」，每一路都能手动启停。清单与计数在
//! [`super::egress`]，这里只画。

use iced::widget::{button, column, container, row, scrollable, text};
use iced::{Color, Element, Length};

use super::egress::{self, Kind, Scope};

const C_HEAD: Color = Color::from_rgb(0.55, 0.8, 1.0);
const C_DIM: Color = Color::from_rgb(0.55, 0.55, 0.6);
const C_TXT: Color = Color::from_rgb(0.85, 0.87, 0.92);
const C_GOLD: Color = Color::from_rgb(0.9, 0.8, 0.4);
const C_OK: Color = Color::from_rgb(0.35, 0.78, 0.98);
/// 对外那一块的主色：暖色——「这里在花流量」。
const C_EXT: Color = Color::from_rgb(0.98, 0.62, 0.32);
/// 内部那一块的主色：绿——「这里不花外网流量」。
const C_INT: Color = Color::from_rgb(0.45, 0.85, 0.55);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressMsg {
    /// `(出口 key, systemctl 动作)`。
    Act(String, &'static str),
    /// 对外全部停止。**只动对外那一块、不动开机自启**。
    StopAll,
    /// 对外全部开启（定时任务只开设了自启的）。
    StartAll,
    /// 启动 Cockpit 时对外连接开（true）还是关（false）。只改设置，不动现在的连接。
    SetStartup(bool),
    Refresh,
}

fn chip<'a>(label: &str, msg: EgressMsg) -> Element<'a, EgressMsg> {
    button(text(label.to_string()).size(11))
        .padding([2, 7])
        .style(|t, st| crate::style::button::modifier(t, st, false))
        .on_press(msg)
        .into()
}

/// 二选一按钮里的一格：选中的那格高亮。
fn seg<'a>(label: &str, active: bool, msg: EgressMsg) -> Element<'a, EgressMsg> {
    button(text(label.to_string()).size(11))
        .padding([2, 9])
        .style(move |t, st| crate::style::button::modifier(t, st, active))
        .on_press(msg)
        .into()
}

fn cell<'a>(s: String, w: f32, c: Color, numeric: bool) -> Element<'a, EgressMsg> {
    container(text(s).size(11).color(c))
        .width(Length::Fixed(w))
        .align_x(if numeric { iced::Alignment::End } else { iced::Alignment::Start })
        .into()
}

pub fn handle(m: EgressMsg) -> String {
    match m {
        EgressMsg::Act(k, a) => egress::action(&k, a),
        EgressMsg::StopAll => egress::stop_all().1,
        EgressMsg::StartAll => egress::start_all().1,
        EgressMsg::SetStartup(on) => egress::set_startup_external(on),
        EgressMsg::Refresh => {
            egress::waker().request();
            String::new()
        }
    }
}

/// 表格各列宽。两块共用一套，上下对齐，扫一眼就能对比。
const COLS: [(&str, f32, bool); 9] = [
    ("出口", 150.0, false),
    ("连谁", 320.0, false),
    ("状态", 96.0, false),
    ("连接", 130.0, true),
    ("速度", 200.0, false),
    ("今日", 84.0, true),
    ("下次 / 已运行", 120.0, false),
    ("开机自启", 84.0, false),
    ("", 150.0, false),
];

fn table_head<'a>(conn_title: &'static str) -> Element<'a, EgressMsg> {
    let mut h = row![].spacing(4);
    for (t, w, n) in COLS {
        let t = if t == "连接" { conn_title } else { t };
        h = h.push(cell(t.into(), w, C_HEAD, n));
    }
    h.into()
}

/// 一块带色边框的区域：标题 + 一句「这一块是什么」+ 内容。
fn section<'a>(
    title: String,
    sub: &'static str,
    accent: Color,
    content: iced::widget::Column<'a, EgressMsg>,
) -> Element<'a, EgressMsg> {
    container(
        column![
            row![
                text("▍").size(14).color(accent),
                text(title).size(13).color(accent),
                text(sub).size(10).color(C_DIM),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center),
            content,
        ]
        .spacing(6),
    )
    .padding(8)
    .width(Length::Fill)
    .style(move |_t: &iced::Theme| container::Style {
        background: Some(iced::Background::Color(Color { a: 0.04, ..accent })),
        border: iced::Border { color: Color { a: 0.55, ..accent }, width: 1.0, radius: 4.0.into() },
        ..Default::default()
    })
    .into()
}

/// 一路出口的一行（两块共用）。
fn source_line<'a>(s: &egress::Source, r: &egress::Row) -> Element<'a, EgressMsg> {
    let external = s.scope == Scope::External;
    let state = match (s.kind, r.on) {
        (Kind::Foreign, true) => ("● 在跑", C_DIM),
        (Kind::Foreign, false) => ("○ 没跑", C_DIM),
        // timer 的「在跑」是「会按时触发」，不是「此刻在执行」。
        // 用同一个词会让人以为它一直在耗流量
        (Kind::Timer, true) => ("⏱ 会触发", C_GOLD),
        (Kind::Timer, false) => ("○ 不触发", C_DIM),
        (_, true) => ("● 在跑", if external { C_EXT } else { C_INT }),
        (_, false) => ("○ 停着", C_DIM),
    };
    let when = match s.kind {
        Kind::Timer => r.next.clone(),
        _ if r.on => super::svcctl::fmt_dur(r.uptime_secs),
        _ => String::new(),
    };
    // 对外那块：数出网的（直连 + 经本机），顺带标出它连本机服务的几条；
    // 内部那块：只数本机服务连接
    let conns = match r.conns {
        None => ("—".to_string(), C_DIM),
        Some(c) if external => {
            let mut t = c.label();
            if c.internal > 0 {
                t.push_str(&format!(" · 内部{}", c.internal));
            }
            (t, if c.total() > 0 { C_EXT } else { C_DIM })
        }
        Some(c) => (
            format!("内部 {}", c.internal),
            if c.internal > 0 { C_INT } else { C_DIM },
        ),
    };
    let mut line = row![
        cell(s.label.into(), 150.0, C_TXT, false),
        cell(s.what.into(), 320.0, C_DIM, false),
        cell(state.0.into(), 96.0, state.1, false),
        cell(conns.0, 130.0, conns.1, true),
        // 「—」= 还没有两次采样，或这一路根本没在跑。
        // **和「0B/s」是两回事**：后者是「连着但没在传」
        cell(
            r.bps.map(|r| r.label()).unwrap_or_else(|| "—".into()),
            200.0,
            if r.bps.is_some_and(|r| r.total_down() > 1024.0) { C_EXT } else { C_DIM },
            false
        ),
        // 今日累计。0 就留空——一列 0B 只是噪声
        cell(
            if r.today.0 + r.today.1 > 0 {
                format!("↓{}", egress::human_bytes(r.today.0))
            } else {
                String::new()
            },
            84.0,
            C_DIM,
            true
        ),
        cell(when, 120.0, C_DIM, false),
    ]
    .spacing(4)
    .align_y(iced::Alignment::Center);

    // 开机自启
    line = line.push(match s.kind {
        Kind::InProcess | Kind::Foreign => cell("—".into(), 84.0, C_DIM, false),
        _ if r.enabled => chip("关自启", EgressMsg::Act(s.key.into(), "disable")),
        _ => chip("设自启", EgressMsg::Act(s.key.into(), "enable")),
    });

    // 启停
    line = line.push(match s.kind {
        // 停掉本机代理，机器上别的东西全断——这一页不给这个按钮
        Kind::Foreign => cell("（不归这页管）".into(), 150.0, C_DIM, false),
        _ if r.on => chip("停止", EgressMsg::Act(s.key.into(), "stop")),
        _ => chip("启动", EgressMsg::Act(s.key.into(), "start")),
    });
    line.into()
}

pub fn pane_body<'a>(note: &str) -> Element<'a, EgressMsg> {
    let rows = egress::rows();
    let inner = egress::inner_rows();
    let mut body = column![].spacing(6).padding(8);

    if rows.is_empty() {
        // poller 起来要一拍。**别显示一张空表**——空表看起来像「什么都没在跑」，
        // 那正是这一页最不能给人的错觉
        return column![text("正在扫描…").size(11).color(C_DIM)].padding(8).into();
    }

    // ── 总数 ──
    let total = egress::total_conns(&rows);
    let wire = egress::wire_rate();
    let ours: u32 = rows
        .iter()
        .zip(egress::ALL)
        .filter(|(_, s)| s.kind != Kind::Foreign)
        .filter_map(|(r, _)| r.conns)
        .map(|c| c.total())
        .sum();
    let internal_n: u32 = rows.iter().filter_map(|r| r.conns).map(|c| c.internal).sum::<u32>()
        + inner.iter().filter_map(|r| r.conns).map(|c| c.internal).sum::<u32>();
    body = body.push(
        row![
            text("网络出口").size(13).color(C_HEAD),
            // 整机网速放在最显眼处：这是「我这会儿到底在耗多少流量」的答案
            text(match wire {
                Some((d, u)) => format!("整机 {}", egress::human_rate(d, u)),
                None => "整机 测量中…".into(),
            })
            .size(13)
            .color(C_OK),
            // 开机以来整机用量：网卡计数器自开机累计，**没有缺口**
            text({
                let ((rx, tx), up) = egress::since_boot();
                format!(
                    "开机 {} 以来 ↓{} ↑{}",
                    super::svcctl::fmt_dur(up),
                    egress::human_bytes(rx),
                    egress::human_bytes(tx)
                )
            })
            .size(11)
            .color(C_DIM),
            text(format!("对外：本项目 {ours} 条 · 全机 {}", total.label()))
                .size(11)
                .color(if ours > 0 { C_EXT } else { C_DIM }),
            text(format!("内部 {internal_n} 条"))
                .size(11)
                .color(if internal_n > 0 { C_INT } else { C_DIM }),
            chip("对外全部停止", EgressMsg::StopAll),
            chip("对外全部开启", EgressMsg::StartAll),
            chip("刷新", EgressMsg::Refresh),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center),
    );

    // ── 启动时的默认 ──
    let startup_on = egress::startup_external_on();
    body = body.push(
        row![
            text("启动 Cockpit 时对外连接：").size(11).color(C_TXT),
            seg("开启", startup_on, EgressMsg::SetStartup(true)),
            seg("全部关闭", !startup_on, EgressMsg::SetStartup(false)),
            text(if startup_on {
                "当前：照常开启（守护随 Cockpit 拉起，行情图照常订阅）"
            } else {
                "当前：启动即关闭全部对外连接（内部连接照常）；要用时点「对外全部开启」或逐路启动"
            })
            .size(10)
            .color(if startup_on { C_DIM } else { C_GOLD }),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center),
    );
    body = body.push(
        text(
            "这个设置下次启动才生效，只改设置、不动现在的连接。「关闭」会停掉对外的服务、\
             行情订阅和对外的定时任务——定时任务不跟随窗口，停了之后要等下次以「开启」启动\
             （只补开设了自启的）或手动启动才会再触发。",
        )
        .size(10)
        .color(C_DIM),
    );

    // 两个数**本来就对不上**。不说清的话，用户会以为哪儿算错了
    body = body.push(
        text(format!(
            "整机 = 真实网卡（{}）上的字节，含 IP/TCP 头，且是整台机器的（浏览器、系统更新都在内）；             每一路数的是 TCP 载荷。走本机代理的流量两边各算一次，所以逐行相加会大于整机。",
            egress::wire_ifaces().join(" + ")
        ))
        .size(10)
        .color(C_DIM),
    );
    body = body.push(
        text("「现在 0 条」不等于「不会再用流量」——定时任务平时就是 0，到点照拉。看「下次」那一列")
            .size(10)
            .color(C_DIM),
    );
    // 这个缺口必须说。不说的话「今日 3.2G」会被当成一整天的真实用量
    body = body.push(
        text(
            "「今日」只在 Cockpit 开着的时候数（不用停在这一页，程序一起来就在数）——\
             但面板关掉的那段时间没人记。想要无缺口的数就看上面那个「开机以来」：\
             网卡计数器不依赖任何程序开着。",
        )
        .size(10)
        .color(C_DIM),
    );
    if !note.is_empty() {
        body = body.push(text(note.to_string()).size(11).color(C_GOLD));
    }

    // ── 对外连接：耗外网流量 ──
    let ext: Vec<_> = egress::ALL.iter().zip(&rows).filter(|(s, _)| s.scope == Scope::External).collect();
    let ext_on = ext.iter().filter(|(s, r)| r.on && s.kind != Kind::Foreign).count();
    let mut ext_col = column![table_head("连接（出网）")].spacing(6);
    for (s, r) in &ext {
        ext_col = ext_col.push(source_line(s, r));
    }
    ext_col = ext_col.push(
        text(
            "停 Cockpit 行情图 = 关掉交易所订阅，连接会真的断开（不是只让界面不显示）；\
             再打开时图表自己会重新连上。",
        )
        .size(10)
        .color(C_DIM),
    );
    ext_col = ext_col.push(
        text(
            "速度每 2 秒采一次，是**下限**——两次采样之间关掉的连接，它最后那段字节数没算进来。\
             「—」是还没测出来或这一路没在跑，「0B/s」是连着但没在传。\
             一路同时有「直连」和「经本机」两个数时，那多半是个代理——两边是同一份字节。",
        )
        .size(10)
        .color(C_DIM),
    );
    body = body.push(section(
        format!("对外连接 · 耗外网流量（{ext_on}/{} 路开着）", ext.iter().filter(|(s, _)| s.kind != Kind::Foreign).count()),
        "直连 = 对端是外网地址；经本机 = 先连本机代理（xray 127.0.0.1:10808）再出网——两种都在花流量",
        C_EXT,
        ext_col,
    ));

    // ── 内部连接：不耗外网流量 ──
    let mut int_col = column![table_head("连接（本机）")].spacing(6);
    for (s, r) in egress::ALL.iter().zip(&rows).filter(|(s, _)| s.scope == Scope::Internal) {
        int_col = int_col.push(source_line(s, r));
    }
    // 确认过不出网的守护：只读。启停在「进程」页——这里放按钮就把两页的职责搅在一起了
    for r in &inner {
        let (st, c) = if r.on { ("● 在跑", C_INT) } else { ("○ 停着", C_DIM) };
        let conns = match r.conns {
            Some(k) => (format!("内部 {}", k.internal), if k.internal > 0 { C_INT } else { C_DIM }),
            None => ("—".to_string(), C_DIM),
        };
        int_col = int_col.push(
            row![
                cell(r.unit.into(), 150.0, C_TXT, false),
                cell(r.why.into(), 320.0, C_DIM, false),
                cell(st.into(), 96.0, c, false),
                cell(conns.0, 130.0, conns.1, true),
                cell("不出网".into(), 200.0, C_DIM, false),
                cell(String::new(), 84.0, C_DIM, true),
                cell(String::new(), 120.0, C_DIM, false),
                cell("—".into(), 84.0, C_DIM, false),
                cell("（启停在「进程」页）".into(), 150.0, C_DIM, false),
            ]
            .spacing(4)
            .align_y(iced::Alignment::Center),
        );
    }
    int_col = int_col.push(
        text(
            "「内部 N」数的是连到本机服务（Redis 6379）的 TCP 连接；UDS、共享内存环、读写本地文件\
             不走 TCP，数不到但同样不出网。Redis 跑在容器里，它自己那一行的连接数是 0，那是对的。\
             停掉这一块省不了外网流量，只会让本机的面板和信号链断掉。",
        )
        .size(10)
        .color(C_DIM),
    );
    body = body.push(section(
        "内部连接 · 不耗外网流量".into(),
        "只连本机（Redis 总线 / UDS / 共享内存环 / 本地文件）——「对外全部停止」和启动设置都不动它们",
        C_INT,
        int_col,
    ));

    scrollable(body).width(Length::Fill).height(Length::Fill).into()
}
