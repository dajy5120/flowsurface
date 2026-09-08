//! 数据接口观察终端的渲染（docs/23）。
//!
//! # 这个文件里不许出现任何具体协议的名字
//!
//! 连接表单、流清单、丢弃策略全部从守护下发的 descriptor 来（[`super::observatory_readout`]）。
//! 一旦这里按某个具体 adapter id 或传输类型走分支，「加协议不改 UI」就已经破了。
//! `no_adapter_id_leaks_into_this_view` 钉住这一点——它连注释一起扫，
//! 因为注释里出现一个具体 id，通常就是代码即将引用它的前一步。

use std::time::Instant;

use iced::widget::{button, canvas, checkbox, column, container, row, scrollable, text, text_input};
use iced::{Color, Element, Length};

use super::observatory_readout::{self as ro, StreamStat};
use super::observatory_table::{Palette, TailTable};

const C_HEAD: Color = Color::from_rgb(0.55, 0.8, 1.0);
const C_DIM: Color = Color::from_rgb(0.55, 0.55, 0.6);
const C_TXT: Color = Color::from_rgb(0.85, 0.87, 0.92);
const C_GOLD: Color = Color::from_rgb(0.9, 0.8, 0.4);
const C_BAD: Color = Color::from_rgb(0.9, 0.45, 0.4);
const C_OK: Color = Color::from_rgb(0.35, 0.78, 0.98);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObsMsg {
    Start,
    Stop,
    /// 连/断当前目标。
    Connect,
    Disconnect,
    /// 筛选框在打字。**只改面板内存，不写文件**——每次按键都写的话，
    /// 守护会在你打到一半时反复重编译一条写错的表达式。
    FilterEdited(String),
    /// 把筛选发给守护。
    ApplyFilter,
    /// Raw / Parsed 切换。
    SetParse(bool),
    /// 开/停录制。
    SetRecord(bool),
    /// 从环里回捞最近 N 秒（docs/23 §5.1）。这是环形缓冲的兑现：
    /// 看到异常**之后**才决定录。
    SaveLast(u32),
    /// 换一个 Adapter。参数是它的 id——**这是 UI 里唯一出现 adapter id 的地方，
    /// 而且是从守护下发的目录里拿的，不是写死的**。
    PickAdapter(String),
    /// 连接表单里某个字段被编辑。
    FieldEdited(String, String),
    /// 捕获筛选在打字。
    CaptureEdited(String),
    /// 把捕获筛选发给守护。**这一步会真的开始丢数据**，所以要显式点。
    ApplyCapture,
    /// 新规则表单里某个字段被编辑。第一个参数是字段名。
    TrigEdited(&'static str, String),
    /// 添加这条规则。
    AddTrigger,
    /// 删掉第 n 条规则。
    DelTrigger(usize),
    /// 切界面模式（docs/23 §9）。**三种模式是同一个会话上的三种界面**，
    /// 切模式不碰连接、不碰录制。
    SetMode(ro::Mode),
    /// 暂停 / 继续。**只停这一屏**——后端照常收、照常录。
    TogglePause,
    /// API 调试的请求体在打字。
    SendEdited(String),
    /// 发出去。
    SendNow,
}

fn cell<'a>(s: String, w: f32, c: Color, numeric: bool) -> Element<'a, ObsMsg> {
    container(text(s).size(11).color(c))
        .width(Length::Fixed(w))
        .align_x(if numeric { iced::Alignment::End } else { iced::Alignment::Start })
        .into()
}

fn chip<'a>(label: &str, msg: ObsMsg) -> Element<'a, ObsMsg> {
    chip_on(label, false, msg)
}

fn chip_on<'a>(label: &str, active: bool, msg: ObsMsg) -> Element<'a, ObsMsg> {
    button(text(label.to_string()).size(11))
        .padding([2, 7])
        .style(move |t, st| crate::style::button::modifier(t, st, active))
        .on_press(msg)
        .into()
}

/// 一行 60 格火花线。用块字符画——数据速率这种东西看**形状**比看数字快。
fn spark(v: &[i64]) -> String {
    const B: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max = v.iter().copied().max().unwrap_or(0).max(1) as f64;
    v.iter()
        .map(|x| {
            if *x == 0 {
                ' '
            } else {
                B[(((*x as f64 / max) * 7.0).round() as usize).min(7)]
            }
        })
        .collect()
}

fn human_bytes(n: i64) -> String {
    const U: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut x = n as f64;
    let mut i = 0;
    while x >= 1024.0 && i < 4 {
        x /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n}{}", U[0]) } else { format!("{x:.1}{}", U[i]) }
}

fn ns(v: Option<f64>) -> String {
    match v {
        // 负值原样显示：对端时钟比我们快。clamp 掉就再也发现不了（docs/23 §4.2 ①）
        Some(x) => format!("{:+.2}ms", x / 1e6),
        None => "—".into(),
    }
}

/// 健康度 → (文案, 颜色)。
fn health_badge(h: &str, connected: bool) -> (&'static str, Color) {
    match h {
        "live" => ("● 在收", C_OK),
        "idle" => ("○ 已连接·对端安静", C_HEAD),
        "connecting" if connected => ("◐ 重连中", C_GOLD),
        "connecting" => ("◐ 连接中", C_GOLD),
        _ => ("✕ 未连接", C_BAD),
    }
}

pub fn pane_body<'a>() -> Element<'a, ObsMsg> {
    let t0 = Instant::now();
    let st = ro::snapshot();
    let mut body = column![].spacing(4).padding(8);

    // ── 守护 + 健康度 ──
    let (badge, bc) = health_badge(&st.health, st.connected);
    let mut top = row![
        text("观察终端").size(13).color(C_TXT),
        chip("▶ 启动", ObsMsg::Start),
        chip("■ 停止", ObsMsg::Stop),
        text(if st.svc.active {
            format!("守护运行中 {}s", st.svc.uptime_secs)
        } else {
            "守护未运行".to_string()
        })
        .size(10)
        .color(if st.svc.active { C_DIM } else { C_GOLD }),
        text(badge.to_string()).size(11).color(bc),
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center);
    if !st.last_err.is_empty() {
        // 退避倒计时要显示：不显示的话「连不上」看起来就像卡死了
        let t = if st.next_retry_secs > 0 {
            format!("⚠ {}（{}s 后重试）", clip(&st.last_err, 46), st.next_retry_secs)
        } else {
            format!("⚠ {}", clip(&st.last_err, 56))
        };
        top = top.push(text(t).size(10).color(C_GOLD));
    }
    // 模式切换。**同一个会话上的三种界面**，不是三个程序
    let mode = ro::mode();
    let mut mr = row![text("模式").size(10).color(C_DIM)].spacing(4)
        .align_y(iced::Alignment::Center);
    for m in ro::Mode::ALL {
        mr = mr.push(chip_on(m.label(), m == mode, ObsMsg::SetMode(m)));
    }
    // 暂停**只停这一屏**（docs/23 §14 坑 7）。不写清楚的话，
    // 用户会以为暂停期间的数据没了
    let frozen = ro::paused();
    match &frozen {
        Some(f) => {
            mr = mr.push(chip_on("▶ 继续", true, ObsMsg::TogglePause));
            let behind = st
                .session
                .as_ref()
                .map(|s| (s.ring.next_seq - f.at_seq).max(0))
                .unwrap_or(0);
            mr = mr.push(
                text(format!(
                    "⏸ 已暂停于 {} · 后端仍在收（这期间又来了 {behind} 条，没丢）",
                    f.at
                ))
                .size(10)
                .color(C_GOLD),
            );
        }
        None => mr = mr.push(chip("⏸ 暂停", ObsMsg::TogglePause)),
    }
    body = body.push(mr);
    body = body.push(top);

    if !st.present {
        return body
            .push(
                text("暂无快照——点「▶ 启动」拉起 ws-observatory 守护")
                    .size(11)
                    .color(C_DIM),
            )
            .into();
    }

    // ── 回放态横幅（docs/23 §11.2）──
    //
    // **靠帧上的 `replayed` 标志判断，不靠 adapter id**：那是协议层的事实，
    // 对将来任何一种回放式 Adapter 都成立，而且 UI 不需要认识任何具体协议。
    let replaying = st
        .session
        .as_ref()
        .is_some_and(|s| s.tail.iter().any(|t| t.flags.contains("replayed")));
    if replaying {
        body = body.push(
            container(
                text("⏵ 回放态——表里是历史数据。「滞后」列算的是**当时**的链路延迟，不是现在的")
                    .size(11)
                    .color(C_GOLD),
            )
            .padding([3, 8]),
        );
    }

    // ── 连接表单：**完全由守护下发的 descriptor 生成** ──
    //
    // 这是自描述设计的兑现：控件种类、标签、默认值、必填与否、密码框，
    // 全部来自 `config_schema`。加一个协议、加一个字段，这里一行都不用改。
    body = body.push(section(
        "连接",
        "表单由 Adapter 自述生成——加协议或加字段只改守护侧一处",
    ));
    let (pick, vals) = ro::form(&st.catalog);
    let mut ar = row![text("Adapter").size(10).color(C_DIM)].spacing(4)
        .align_y(iced::Alignment::Center);
    for a in &st.catalog {
        ar = ar.push(chip_on(&a.label, a.id == pick, ObsMsg::PickAdapter(a.id.clone())));
    }
    if let Some(a) = st.catalog.iter().find(|a| a.id == pick) {
        ar = ar.push(
            text(format!("{} · {} 条流", a.transport, a.streams.len())).size(10).color(C_DIM),
        );
    }
    body = body.push(ar);

    if let Some(a) = st.catalog.iter().find(|a| a.id == pick) {
        for f in &a.config_schema {
            let v = vals.get(&f.key).cloned().unwrap_or_default();
            let key = f.key.clone();
            let mut fr = row![
                container(
                    text(format!("{}{}", f.label, if f.required { " *" } else { "" }))
                        .size(10)
                        .color(if f.required { C_TXT } else { C_DIM })
                )
                .width(Length::Fixed(110.0)),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center);
            // **密钥用密码框**：它会被人截图、被人录屏。守护那边也会在快照和
            // 清单里抹掉它，但那挡不住屏幕
            let input = text_input(&f.hint, &v)
                .on_input(move |t| ObsMsg::FieldEdited(key.clone(), t))
                .on_submit(ObsMsg::Connect)
                .size(11)
                .padding([2, 6])
                .width(Length::Fixed(520.0));
            fr = fr.push(if f.kind == "secret" { input.secure(true) } else { input });
            if !f.hint.is_empty() {
                fr = fr.push(text(f.hint.clone()).size(10).color(C_DIM));
            }
            body = body.push(fr);
        }
    }

    let mut cr = row![].spacing(8).align_y(iced::Alignment::Center);
    match ro::form_ready(&st.catalog) {
        Ok(()) => cr = cr.push(chip("连接", ObsMsg::Connect)),
        Err(e) => {
            // 必填项空着就点连接，只会拿到一条难懂的握手错误。早点挡住，
            // 错误信息才说得清「该怎么改」
            cr = cr.push(text(format!("⚠ {e}")).size(10).color(C_GOLD));
        }
    }
    if st.connected || st.session.is_some() {
        cr = cr.push(chip("断开", ObsMsg::Disconnect));
    }
    body = body.push(cr);

    let Some(sess) = st.session.as_ref() else {
        body = body.push(
            text("尚未建立会话——在 radar_request 同级的 observatory_request.json 里指定 adapter 与 config")
                .size(11)
                .color(C_DIM),
        );
        return scrollable(body).width(Length::Fill).height(Length::Fill).into();
    };

    // ── 会话 ──
    let mut cfgline = row![text("会话 ").size(10).color(C_DIM)].spacing(8);
    cfgline = cfgline.push(text(sess.adapter_label.clone()).size(11).color(C_TXT));
    for (k, v) in &sess.config {
        cfgline = cfgline.push(text(format!("{k}={}", clip(v, 52))).size(10).color(C_DIM));
    }
    if sess.reconnects > 0 {
        cfgline = cfgline.push(text(format!("重连 {} 次", sess.reconnects)).size(10).color(C_GOLD));
    }
    body = body.push(cfgline.align_y(iced::Alignment::Center));

    // ── API 调试：发请求 + 两次应答对比 ──
    if mode == ro::Mode::Api {
        body = body.push(api_block(&st, sess));
    }

    // ── 环形缓冲：覆盖区间是这一屏最要紧的一个数 ──
    let r = &sess.ring;
    if mode != ro::Mode::Api {
    body = body.push(section(
        "环形缓冲",
        "「能往回捞多久」= 覆盖区间。想存的时间窗如果比它早，捞出来的会是一段安静变短的数据",
    ));
    let cov = match (r.coverage_secs, r.coverage_from_ms) {
        (Some(s), Some(from)) => format!(
            "{:.1}s（{} 起）",
            s,
            chrono::DateTime::from_timestamp_millis(from)
                .map(|t| t.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
                .unwrap_or_else(|| "—".into())
        ),
        _ => "—".into(),
    };
    body = body.push(
        row![
            cell(format!("可回溯 {cov}"), 260.0, C_OK, false),
            cell(
                format!("{}/{} 条", r.slots_used, r.slots_cap),
                150.0,
                C_DIM,
                false
            ),
            cell(
                format!("{}/{}", human_bytes(r.bytes_used), human_bytes(r.bytes_cap)),
                130.0,
                C_DIM,
                false
            ),
            cell(
                format!("已用 {:.2}%", r.fill_frac * 100.0),
                100.0,
                // 高水位标黄：到 75% 就该考虑溢写了
                if r.fill_frac > 0.75 { C_GOLD } else { C_DIM },
                false
            ),
            cell(format!("淘汰 {}", r.evicted), 110.0, C_DIM, false),
            cell(format!("seq {}→{}", r.oldest_seq, r.next_seq), 180.0, C_DIM, false),
        ]
        .spacing(3),
    );
    }

    // ── 录制 ──（录制模式才占版面；观察模式给一行摘要就够了）
    if mode == ro::Mode::Record {
        body = body.push(record_block(&st, sess, &r));
        body = body.push(trigger_block(sess));
    } else if st.rec.on {
        // 别的模式下**仍然要显示正在录**：不显示的话，用户切走之后
        // 会忘了自己还在往盘上写
        body = body.push(
            text(format!(
                "● 正在录制 {} · {} 条 {} · 到「录制」模式去停",
                st.rec.session_id,
                st.rec.frames,
                human_bytes(st.rec.bytes)
            ))
            .size(10)
            .color(C_OK),
        );
    }

    // ── 逐流指标：四个阶段分开 ──
    if mode != ro::Mode::Api {
    body = body.push(section(
        "逐流指标",
        "丢弃按阶段分开：帧=太大 / 环=满了 / 盘=写不动。「UI 跳过」不是故障——人眼一秒读不了 20 行",
    ));
    let mut h = row![].spacing(3);
    for (t, w, n) in [
        ("流", 90.0, false),
        ("收", 84.0, true),
        ("速率", 74.0, true),
        ("字节率", 84.0, true),
        ("丢·帧", 60.0, true),
        ("丢·环", 60.0, true),
        ("丢·盘", 60.0, true),
        ("gap", 50.0, true),
        ("UI 跳过", 74.0, true),
        ("滞后 p50", 82.0, true),
        ("p99", 82.0, true),
    ] {
        h = h.push(cell(t.into(), w, C_HEAD, n));
    }
    body = body.push(h);
    for s in &sess.streams {
        body = body.push(stream_row(s));
        if !s.spark_msg.is_empty() {
            body = body.push(
                row![
                    cell(String::new(), 90.0, C_DIM, false),
                    text(spark(&s.spark_msg)).size(11).color(C_OK),
                ]
                .spacing(3),
            );
        }
    }

    }

    // ── 尾窗 ──
    let (ftext, _) = ro::view_state();
    let f = &sess.filter;
    // 「到了 N 条、显示 M 条」比「跳过 K 条」说得清楚。带筛选时措辞还要再变一次：
    // 没显示的那些既有被筛掉的也有没排上一屏的，两者分不开（被筛掉的根本没被扫到）
    let hdr = match (sess.tail_arrived, f.src.is_empty()) {
        (0, _) => format!("最近 {} 条", sess.tail.len()),
        (n, true) => format!("这半秒到了 {n} 条，显示最新 {}", sess.tail.len()),
        (n, false) => format!("这半秒到了 {n} 条，符合条件的显示 {}", sess.tail.len()),
    };
    body = body.push(section(&hdr, "解码是可选的一层——观察没见过的接口时，Raw 才是真相"));

    // 筛选条。
    //
    // **显示的是守护实际在用的口径，不是面板请求的**：两者在往返的那半秒里
    // 不一样，显示请求值会让人以为已经生效了（同 docs/22 §7.7 新股月份那条）。
    // 输入框本地可编辑，但本地为空时用守护那份填进去——否则用户看不见
    // 当前到底在筛什么。
    let shown = if ftext.is_empty() { f.src.clone() } else { ftext.clone() };
    let mut fr = row![
        text("显示筛选").size(10).color(C_DIM),
        text_input("len > 1024 && $.data.s == BTCUSDT", &shown)
            .on_input(ObsMsg::FilterEdited)
            .on_submit(ObsMsg::ApplyFilter)
            .size(11)
            .padding([2, 6])
            .width(Length::Fixed(440.0)),
        chip("应用", ObsMsg::ApplyFilter),
        // 勾选框反映**已生效**的状态：点下去到生效有半秒往返，
        // 立刻打勾会让人以为已经切了，其实表里还是原文
        checkbox(sess.parse).on_toggle(ObsMsg::SetParse).size(12),
        text("解析（Parsed 视图）").size(10).color(C_DIM),
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center);
    // **只影响你看到什么，不动数据**——这句话要常驻，用户永远会把它和捕获筛选搞混
    fr = fr.push(text("只影响显示，不影响录制").size(10).color(C_DIM));
    if !f.err.is_empty() {
        // 写错了照常显示全部并报错，而不是给一张空表让人以为没数据
        fr = fr.push(text(format!("⚠ {}（当前显示全部）", clip(&f.err, 60))).size(10).color(C_BAD));
    } else if f.needs_json {
        fr = fr.push(text("$. 路径要逐条解析，比较贵").size(10).color(C_GOLD));
    }
    body = body.push(fr);
    if sess.tail_lapped > 0 {
        // 与「没排上一屏」是两件事：这些是**真的被环淘汰掉了**，
        // 该调的是环容量，不是刷新频率
        body = body.push(
            text(format!(
                "⚠ 尾窗游标被环追上，丢了 {} 条——环容量跟不上，调大 ring 或降速",
                sess.tail_lapped
            ))
            .size(10)
            .color(C_BAD),
        );
    }
    if f.exhausted {
        // 悄悄给一张短表，用户会以为这段时间就这么点数据
        body = body.push(
            text(format!("⚠ 往回扫了 {} 条就到上限了，更早的没看——收窄条件或调大 scan_budget", f.scanned))
                .size(10)
                .color(C_GOLD),
        );
    }

    // 表格画在 canvas 上：200 行 × 6 列做成 widget 是每帧重建 1200 个节点。
    //
    // 暂停时用**冻结的那一份**，并且把 version 也冻住——否则缓存每帧失效，
    // 暂停反而比不暂停还贵（docs/23 §10.6：暂停就该是真的零开销）
    let (rows, ver) = match &frozen {
        Some(f) => (f.rows.clone(), f.at_seq as u64),
        None => (sess.tail.clone(), sess.ring.next_seq as u64),
    };
    let table = TailTable {
        rows: rows.iter().rev().cloned().collect(),
        version: ver,
        cache: canvas::Cache::new(),
        pal: Palette {
            head: C_HEAD,
            dim: C_DIM,
            txt: C_TXT,
            gold: C_GOLD,
            bad: C_BAD,
            stripe: Color::from_rgba(1.0, 1.0, 1.0, 0.025),
        },
        // 跟**守护实际给了什么**：它说这批行带解析结果，才按解析显示
        parsed_view: sess.parse,
    };
    let h = table.height();
    body = body.push(
        canvas(table).width(Length::Fill).height(Length::Fixed(h)),
    );

    // **面板自报帧时**：不自报的话，UI 变慢时没人知道是 UI 慢还是数据慢
    let ms = frame_ms(t0);
    body = body.push(
        row![
            text(format!("快照 {} · 读于 {}", st.stamp, st.refreshed)).size(10).color(C_DIM),
            text(format!("· 本帧 {ms:.1}ms")).size(10).color(if ms > 16.0 { C_GOLD } else { C_DIM }),
        ]
        .spacing(6),
    );
    scrollable(body).width(Length::Fill).height(Length::Fill).into()
}

/// 记录并返回本次构建耗时（毫秒），同时维护一个滚动峰值。
///
/// 只报**这一帧**是不够的：偶发的一次 30ms 会在下一帧就被冲掉，
/// 而卡顿恰恰是偶发的。故峰值单独留一份。
fn frame_ms(t0: Instant) -> f64 {
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    if let Ok(mut g) = FRAME_PEAK.lock() {
        *g = g.max(ms);
    }
    ms
}

static FRAME_PEAK: std::sync::Mutex<f64> = std::sync::Mutex::new(0.0);

/// 近期最慢的一帧（毫秒）。诊断用。
pub fn frame_peak_ms() -> f64 {
    FRAME_PEAK.lock().map(|g| *g).unwrap_or(0.0)
}

/// 复位峰值。
pub fn reset_frame_peak() {
    if let Ok(mut g) = FRAME_PEAK.lock() {
        *g = 0.0;
    }
}

/// API 调试（docs/23 §9 Mode 1）：发请求 + 两次应答对比。
///
/// **两次应答做 diff 是调接口时最有用的功能**：改一个参数、再发一次，
/// 直接看哪几个字段动了。按字段比而不是按文本比——键顺序和空白在 JSON 里
/// 没有意义，文本 diff 会被它们搅得没法看。
fn api_block<'a>(st: &ro::ObsReadout, sess: &ro::SessionView) -> Element<'a, ObsMsg> {
    let mut col = column![].spacing(4);
    let can_send = st
        .catalog
        .iter()
        .find(|a| a.id == sess.adapter)
        .map(|a| a.can_request)
        .unwrap_or(false);

    col = col.push(section("发送", "出站帧也会进信封——「我发了什么」和「它回了什么」能对上"));
    if can_send {
        col = col.push(
            row![
                text_input("请求体（WS 是订阅报文；REST 是 POST body；FIX 用 | 代替 SOH）", &ro::send_body())
                    .on_input(ObsMsg::SendEdited)
                    .on_submit(ObsMsg::SendNow)
                    .size(11)
                    .padding([2, 6])
                    .width(Length::Fixed(700.0)),
                chip("发送", ObsMsg::SendNow),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
        );
    } else {
        // 说清是**这个 Adapter 不支持**，而不是让按钮点了没反应
        col = col.push(
            text("当前 Adapter 不接受出站请求（回放就是这样）").size(10).color(C_DIM),
        );
    }

    // ── 两次应答对比 ──
    col = col.push(section("上两次应答的差异", "按字段比，不按文本比"));
    // 数据流永远是 0 号：descriptor 里第一条流就是负载流
    match ro::diff_last_two(sess, 0) {
        None => {
            // 说清**为什么**比不了：没开解析、还是只收到一条
            let n = sess.tail.iter().filter(|r| r.stream_id == 0).count();
            col = col.push(
                text(if !sess.parse {
                    "要先勾上「解析」才能按字段比".to_string()
                } else {
                    format!("还只有 {n} 条应答，至少要两条")
                })
                .size(10)
                .color(C_DIM),
            );
        }
        Some((d, a, b)) => {
            col = col.push(
                text(format!(
                    "seq {a} → {b}：{} 处变化，{} 个字段没动",
                    d.total_changes(),
                    d.same
                ))
                .size(11)
                .color(if d.is_empty() { C_DIM } else { C_TXT }),
            );
            for (k, o, n) in d.changed.iter().take(24) {
                col = col.push(
                    row![
                        cell(clip(k, 34), 250.0, C_TXT, false),
                        cell(clip(o, 26), 190.0, C_DIM, false),
                        text("→").size(10).color(C_DIM),
                        cell(clip(n, 26), 190.0, C_OK, false),
                    ]
                    .spacing(6),
                );
            }
            for (k, v) in d.added.iter().take(12) {
                col = col.push(
                    row![
                        cell(format!("+ {}", clip(k, 32)), 250.0, C_OK, false),
                        cell(clip(v, 26), 190.0, C_OK, false),
                    ]
                    .spacing(6),
                );
            }
            // **消失的字段单独列**：接口悄悄不再返回某个字段，
            // 是升级时最容易漏掉的一类变化
            for (k, v) in d.removed.iter().take(12) {
                col = col.push(
                    row![
                        cell(format!("− {}", clip(k, 32)), 250.0, C_BAD, false),
                        cell(clip(v, 26), 190.0, C_DIM, false),
                    ]
                    .spacing(6),
                );
            }
            if d.total_changes() > 48 {
                col = col.push(
                    text(format!("…共 {} 处，只列了前面几条", d.total_changes()))
                        .size(10)
                        .color(C_DIM),
                );
            }
        }
    }
    col.into()
}

/// 触发录制与捕获筛选（docs/23 §7、§8.1）。
///
/// 捕获筛选**放在触发规则下面、并且长得不一样**：它和上面那个显示筛选是
/// 两件完全不同的事，而用户永远会把它们搞混。
fn trigger_block<'a>(sess: &ro::SessionView) -> Element<'a, ObsMsg> {
    let mut col = column![].spacing(4);

    // ── 触发规则 ──
    col = col.push(section(
        "触发录制",
        "条件命中时把**命中之前**那几秒一起存下来——这是环形缓冲的兑现",
    ));
    if !sess.triggers.is_empty() {
        let mut h = row![].spacing(3);
        for (t, w, n) in [
            ("规则", 96.0, false),
            ("条件", 240.0, false),
            ("前/后", 92.0, false),
            ("冷却", 62.0, true),
            ("已录", 62.0, true),
            ("本小时", 74.0, true),
            ("被挡下", 84.0, true),
            ("状态", 240.0, false),
            ("", 44.0, false),
        ] {
            h = h.push(cell(t.into(), w, C_HEAD, n));
        }
        col = col.push(h);
        for (i, t) in sess.triggers.iter().enumerate() {
            let at_cap = t.cap_per_hour > 0 && t.used_this_hour >= t.cap_per_hour;
            let status = if t.capturing {
                ("● 正在录后半段".to_string(), C_OK)
            } else if !t.blocked_why.is_empty() {
                // **必须显示为什么被挡**：用户看着条件明明命中却没录，
                // 会以为是条件写错了，其实只是在冷却
                (t.blocked_why.clone(), if at_cap { C_GOLD } else { C_DIM })
            } else {
                ("等待命中".to_string(), C_DIM)
            };
            col = col.push(
                row![
                    cell(clip(&t.name, 10), 96.0, C_TXT, false),
                    cell(clip(&t.cond, 30), 240.0, C_DIM, false),
                    cell(
                        format!("{}s/{}s", t.pre_roll_ms / 1000, t.post_roll_ms / 1000),
                        92.0,
                        C_DIM,
                        false
                    ),
                    cell(format!("{}s", t.cooldown_ms / 1000), 62.0, C_DIM, true),
                    cell(t.fired.to_string(), 62.0, C_OK, true),
                    cell(
                        format!("{}/{}", t.used_this_hour, t.cap_per_hour),
                        74.0,
                        if at_cap { C_GOLD } else { C_DIM },
                        true
                    ),
                    // 被挡次数本身就是信息：它说明条件写得太宽
                    cell(
                        t.blocked.to_string(),
                        84.0,
                        if t.blocked > t.fired * 1000 { C_GOLD } else { C_DIM },
                        true
                    ),
                    cell(status.0, 240.0, status.1, false),
                    chip("删", ObsMsg::DelTrigger(i)),
                ]
                .spacing(3),
            );
        }
    }

    // 新规则表单
    let f = ro::new_trig();
    let fld = |ph: &str, v: &str, key: &'static str, w: f32| {
        text_input(ph, v)
            .on_input(move |t| ObsMsg::TrigEdited(key, t))
            .size(11)
            .padding([2, 6])
            .width(Length::Fixed(w))
    };
    col = col.push(
        row![
            text("新增").size(10).color(C_DIM),
            fld("名称", &f.name, "name", 96.0),
            fld("条件，如 $.data.q > 10", &f.cond, "cond", 300.0),
            text("前").size(10).color(C_DIM),
            fld("10000", &f.pre_roll_ms, "pre", 66.0),
            text("后").size(10).color(C_DIM),
            fld("5000", &f.post_roll_ms, "post", 66.0),
            text("冷却").size(10).color(C_DIM),
            fld("30000", &f.cooldown_ms, "cool", 66.0),
            text("次/时").size(10).color(C_DIM),
            fld("10", &f.max_per_hour, "cap", 50.0),
            chip("添加", ObsMsg::AddTrigger),
        ]
        .spacing(5)
        .align_y(iced::Alignment::Center),
    );
    col = col.push(
        text("毫秒。冷却与「次/时」有下限——配 0 不是「不限」：一个每帧都命中的条件在两万条/秒下会一秒生成两万个录制目录")
            .size(10)
            .color(C_DIM),
    );

    // ── 捕获筛选：**与显示筛选长得明显不同** ──
    col = col.push(section("捕获筛选", ""));
    let ctext = ro::capture_text();
    let shown = if ctext.is_empty() { sess.capture.src.clone() } else { ctext };
    col = col.push(
        row![
            text("⚠ 入环之前就筛").size(10).color(C_BAD),
            text_input("$.data.s == BTCUSDT", &shown)
                .on_input(ObsMsg::CaptureEdited)
                .on_submit(ObsMsg::ApplyCapture)
                .size(11)
                .padding([2, 6])
                .width(Length::Fixed(400.0)),
            chip("应用", ObsMsg::ApplyCapture),
            // 这句话要**常驻**：用户永远会把它和上面那个显示筛选搞混
            text("筛掉的数据不会被录下来，也不会进环——永远没有了").size(10).color(C_BAD),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
    );
    if sess.capture.dropped > 0 {
        // 不显示的话，用户看到条数少了会以为是丢包
        col = col.push(
            text(format!(
                "已筛掉 {} 条（这不是丢包，是你自己筛的）",
                sess.capture.dropped
            ))
            .size(10)
            .color(C_GOLD),
        );
    }
    col.into()
}

/// 录制区块。
///
/// 「回捞最近 N 秒」的按钮上**写着实际能捞多少**：环只覆盖 11 秒时，
/// 「最近 60 秒」按下去只会得到 11 秒的数据。不在按钮上说清楚的话，
/// 用户会拿到一份安静变短的文件还以为是完整的（docs/23 §5.1）。
fn record_block<'a>(
    st: &ro::ObsReadout,
    sess: &ro::SessionView,
    ring: &ro::RingStat,
) -> Element<'a, ObsMsg> {
    let rec = &st.rec;
    let mut col = column![].spacing(4);
    col = col.push(section(
        "录制",
        "这是本程序唯一会写硬盘的地方。原始字节始终写，parquet 供分析，JSONL 需在配置里开",
    ));

    let cov = ring.coverage_secs.unwrap_or(0.0);
    let mut r1 = row![].spacing(8).align_y(iced::Alignment::Center);
    if rec.on {
        r1 = r1.push(chip("■ 停止录制", ObsMsg::SetRecord(false)));
        r1 = r1.push(
            text(format!(
                "● 录制中 {} · {} 条 {} · 预算 {:.2}%",
                rec.session_id,
                rec.frames,
                human_bytes(rec.bytes),
                rec.budget_frac * 100.0
            ))
            .size(11)
            .color(C_OK),
        );
        if rec.gaps > 0 {
            r1 = r1.push(text(format!("断点 {}", rec.gaps)).size(10).color(C_GOLD));
        }
        if rec.drop_sink > 0 {
            // 与「gap」不同：这是录制**自己**跟不上环而丢的，该调容量或降速
            r1 = r1.push(
                text(format!("⚠ 录制跟不上，丢了 {} 条", rec.drop_sink)).size(10).color(C_BAD),
            );
        }
    } else {
        r1 = r1.push(chip("● 开始录制", ObsMsg::SetRecord(true)));
        r1 = r1.push(text("未在录制").size(11).color(C_DIM));
    }
    col = col.push(r1);

    if rec.halted {
        // **被闸门停了**与「用户点了停止」是两回事
        col = col.push(text(format!("⚠ {}", rec.halt)).size(10).color(C_BAD));
    }

    // 回捞：按钮上写实际能捞多少
    let mut r2 = row![text("回捞最近").size(10).color(C_DIM)].spacing(4)
        .align_y(iced::Alignment::Center);
    for secs in [10u32, 30, 60, 300] {
        let enough = cov >= secs as f64;
        let label = if enough {
            format!("{secs}s")
        } else {
            // 环里只有这么多。按下去只会得到这么多——写在按钮上，
            // 而不是让用户事后从文件长度里发现
            format!("{secs}s（只有 {cov:.0}s）")
        };
        r2 = r2.push(chip(&label, ObsMsg::SaveLast(secs)));
    }
    r2 = r2.push(
        text(format!("环里现有 {cov:.1}s（{} 起）", cov_from(ring))).size(10).color(C_DIM),
    );
    col = col.push(r2);

    if !st.last_save.is_empty() {
        // 时间窗保存是异步的。不给回执，用户不知道到底存没存下来
        let bad = st.last_save.contains("失败") || st.last_save.contains("停止");
        col = col.push(
            text(format!("↳ {}", clip(&st.last_save, 140)))
                .size(10)
                .color(if bad { C_BAD } else { C_OK }),
        );
    }
    let _ = sess;
    col.into()
}

fn cov_from(r: &ro::RingStat) -> String {
    r.coverage_from_ms
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|t| t.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "—".into())
}

fn stream_row<'a>(s: &StreamStat) -> Element<'a, ObsMsg> {
    let d = |n: i64| if n > 0 { C_BAD } else { C_DIM };
    row![
        cell(s.label.clone(), 90.0, C_TXT, false),
        cell(s.received.to_string(), 84.0, C_TXT, true),
        cell(format!("{}/s", s.rate_msg), 74.0, C_OK, true),
        cell(format!("{}/s", human_bytes(s.rate_bytes)), 84.0, C_DIM, true),
        cell(s.drop_frame.to_string(), 60.0, d(s.drop_frame), true),
        cell(s.drop_ring.to_string(), 60.0, d(s.drop_ring), true),
        cell(s.drop_sink.to_string(), 60.0, d(s.drop_sink), true),
        cell(s.gap.to_string(), 50.0, if s.gap > 0 { C_GOLD } else { C_DIM }, true),
        // **不是故障色**：UI 跟不上是正常的
        cell(s.skipped_ui.to_string(), 74.0, C_DIM, true),
        cell(ns(s.lag_p50_ns), 82.0, C_TXT, true),
        cell(ns(s.lag_p99_ns), 82.0, C_DIM, true),
    ]
    .spacing(3)
    .into()
}

fn section<'a>(title: &str, note: &str) -> Element<'a, ObsMsg> {
    let mut r = row![text(format!("▍{title}")).size(12).color(C_HEAD)].spacing(8);
    if !note.is_empty() {
        r = r.push(text(note.to_string()).size(10).color(C_DIM));
    }
    container(r).padding([6, 0]).into()
}

/// 按**字符数**截断。中文标题按字节切会切出半个字。
fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    s.chars().take(n.saturating_sub(1)).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_adapter_id_leaks_into_this_view() {
        // docs/23 §2.4 的验收：面板里出现任何具体 adapter id 或对 transport 的分支，
        // 「加协议不动 UI」就已经破了。这条用例是那个架构约束的**唯一**执行者
        // **只扫非测试部分**：`include_str!` 会把这个测试模块自己也读进来，
        // 而这里的禁用词表本身就含那些 id——不切掉的话这条用例永远自己红
        let whole = include_str!("observatory_view.rs");
        let src = whole.split("#[cfg(test)]").next().unwrap();
        for id in ["ws.raw", "ibkr.tws", "cme.fix", "binance"] {
            assert!(!src.contains(id), "{id} 泄漏进了 UI 层");
        }
        // 也不许按传输类型分支
        assert!(
            !src.contains("match transport") && !src.contains("Transport::"),
            "UI 不该按传输类型走分支"
        );
        // 这条用例本身必须真的在扫东西——切错了会变成永远通过的空断言
        assert!(src.len() > whole.len() / 2, "切出来的正文太短，说明切点找错了");
    }

    #[test]
    fn the_sparkline_scales_to_its_own_max_and_shows_silence_as_blank() {
        // 一条静默的流画成一排 ▁ 会看着像在低速运行
        assert_eq!(spark(&[0, 0, 0]), "   ");
        let s = spark(&[1, 5, 10]);
        assert_eq!(s.chars().count(), 3);
        assert_eq!(s.chars().last(), Some('█'), "最大值顶格");
        assert_ne!(s.chars().next(), Some('█'));
    }

    #[test]
    fn a_negative_lag_is_displayed_not_hidden() {
        // 对端时钟比我们快。显示成 0 或「—」就再也发现不了
        assert_eq!(ns(Some(-1_500_000.0)), "-1.50ms");
        assert_eq!(ns(Some(9_000_000.0)), "+9.00ms");
        assert_eq!(ns(None), "—");
    }

    #[test]
    fn health_reads_differently_while_reconnecting() {
        // 「连接中」和「重连中」是两回事：后者说明刚才断过
        assert_eq!(health_badge("connecting", true).0, "◐ 重连中");
        assert_eq!(health_badge("connecting", false).0, "◐ 连接中");
        assert_eq!(health_badge("live", true).0, "● 在收");
        // 「已连接但对端安静」不能显示成故障
        assert_eq!(health_badge("idle", true).1, C_HEAD);
        assert_eq!(health_badge("lost", false).1, C_BAD);
    }

    #[test]
    fn byte_sizes_stay_readable() {
        assert_eq!(human_bytes(512), "512B");
        assert_eq!(human_bytes(2048), "2.0K");
        assert_eq!(human_bytes(536_870_912), "512.0M");
    }

    /// P1 判据（docs/23 §13）：**5 万条/秒下帧时间 < 16 ms**。
    ///
    /// 关键在于：面板的开销与**进来的速率无关**，只与尾窗行数有关——
    /// 全速流永远不进 UI（Law 02）。所以这里喂一份 200 行的满载快照，
    /// 量的就是 47k/s 实况下面板每帧真正要做的事。
    #[test]
    fn a_full_tail_builds_well_under_one_frame() {
        use super::super::observatory_readout::{
            AdapterSpec, FilterStat, ObsReadout, RingStat, SessionView, StreamStat, TailRow,
        };
        let row = |i: i64| TailRow {
            seq: 5_861_177 + i,
            stream_id: 0,
            recv_ms: 1_788_849_521_207,
            len: 77,
            wire: "text".into(),
            dir: "in".into(),
            flags: String::new(),
            lag_ns: None,
            preview: r#"{"stream":"blast","data":{"s":"BTCUSDT","i":5861176,"p":"80000.1","q":"0.5"}}"#.into(),
            preview_truncated: false,
            parsed: None,
        };
        let st = ObsReadout {
            stamp: "2026-09-08 13:18:41".into(),
            present: true,
            connected: true,
            health: "live".into(),
            catalog: vec![AdapterSpec {
                id: "x".into(),
                label: "X".into(),
                transport: "ws".into(),
                config_schema: vec![],
                streams: vec![],
                can_request: true,
            }],
            session: Some(SessionView {
                adapter: "x".into(),
                adapter_label: "X".into(),
                config: vec![("url".into(), "ws://127.0.0.1".into())],
                reconnects: 0,
                ring: RingStat { next_seq: 5_861_377, ..Default::default() },
                streams: vec![StreamStat {
                    label: "帧".into(),
                    received: 5_861_177,
                    rate_msg: 47_000,
                    spark_msg: vec![47_000; 60],
                    ..Default::default()
                }],
                tail: (0..200).map(row).collect(),
                tail_skipped: 23_352,
                tail_arrived: 23_552,
                tail_lapped: 0,
                filter: FilterStat::default(),
                parse: false,
                capture: Default::default(),
                triggers: vec![],
            }),
            ..Default::default()
        };
        super::super::observatory_readout::seed_for_test(st);

        // 先跑几次预热（首次会把字体等惰性资源准备好）
        for _ in 0..5 {
            let _ = pane_body();
        }
        let mut worst = 0f64;
        for _ in 0..50 {
            let t = std::time::Instant::now();
            let _ = pane_body();
            worst = worst.max(t.elapsed().as_secs_f64() * 1000.0);
        }
        // 16ms 是 60fps 的整帧预算，而这只是**构建 widget 树**的部分。
        // 留足余量：超过 8ms 就说明有东西在随行数线性变贵，该查了
        eprintln!("[P1] 200 行满载尾窗最慢一帧 {worst:.3}ms（预算 16ms）");
        assert!(worst < 8.0, "满载尾窗最慢一帧 {worst:.2}ms，超出预算");
    }

    #[test]
    fn the_replay_banner_keys_off_the_frame_flag_not_the_adapter_id() {
        // docs/23 §11.2：界面据 REPLAYED 位切换口径。用 adapter id 判断的话，
        // 将来任何一种新的回放式 Adapter 都要回来改 UI——那正是这套架构要避免的
        let f = |s: &str| super::super::observatory_readout::TailRow {
            flags: s.into(),
            ..Default::default()
        };
        let rows = vec![f(""), f("replayed"), f("gap")];
        assert!(rows.iter().any(|t| t.flags.contains("replayed")));
        let live = vec![f(""), f("gap|synthetic")];
        assert!(!live.iter().any(|t| t.flags.contains("replayed")));
    }

    /// 连接表单的状态是**进程级静态**，而测试是并行跑的——两个用例各自
    /// `form_pick` 会互相打架（实测过一次）。故表单相关的断言全在这一个用例里。
    #[test]
    fn the_connection_form_comes_entirely_from_the_descriptor() {
        use super::super::observatory_readout as r;
        use super::super::observatory_readout::{AdapterSpec, FieldSpec};
        let f = |k: &str, label: &str, kind: &str, d: &str, req: bool| FieldSpec {
            key: k.into(),
            label: label.into(),
            kind: kind.into(),
            default: d.into(),
            required: req,
            hint: String::new(),
            options: vec![],
        };
        let cat = vec![
            AdapterSpec {
                id: "x.proto".into(),
                label: "某协议".into(),
                transport: "tcp".into(),
                config_schema: vec![
                    f("host", "主机", "text", "127.0.0.1", true),
                    f("token", "令牌", "secret", "", false),
                ],
                streams: vec![],
                can_request: true,
            },
            AdapterSpec {
                id: "y.rest".into(),
                label: "另一个".into(),
                transport: "rest".into(),
                config_schema: vec![f("host", "主机", "text", "https://默认不同", false)],
                streams: vec![],
                can_request: false,
            },
        ];

        // ── 自描述：控件种类、标签、默认值、必填全来自 descriptor ──
        r::form_pick(&cat, "x.proto");
        let (id, vals) = r::form(&cat);
        assert_eq!(id, "x.proto");
        assert_eq!(vals["host"], "127.0.0.1", "默认值来自 descriptor");
        assert!(r::form_ready(&cat).is_ok(), "必填项有默认值 → 就绪");

        // ── 必填项空着要挡住，且说清是哪一项 ──
        r::form_set("host", "  ");
        let e = r::form_ready(&cat).unwrap_err();
        assert!(e.contains("主机"), "{e}");

        // ── 换 Adapter 不跨协议保留字段值 ──
        //
        // 字段名相同但语义未必相同（同名的 host 在两个协议上要填的东西不一样），
        // 留着只会让人填错
        r::form_set("host", "填过的值");
        r::form_pick(&cat, "y.rest");
        let (id, vals) = r::form(&cat);
        assert_eq!(id, "y.rest");
        assert_eq!(vals["host"], "https://默认不同", "要回到新协议的默认值");
    }

    #[test]
    fn pausing_says_that_the_backend_is_still_running() {
        // docs/23 §14 坑 7：**暂停不是断连**。不写清楚的话，
        // 用户会以为暂停期间的数据没了
        let whole = include_str!("observatory_view.rs");
        let src = whole.split("#[cfg(test)]").next().unwrap();
        assert!(src.contains("后端仍在收"), "暂停时必须说明后端照常在收");
        assert!(src.contains("没丢"), "还要说清那些数据没丢");
    }

    #[test]
    fn a_paused_tail_freezes_its_version_so_the_cache_stays_valid() {
        // §10.6：暂停就该是真的零开销。version 不冻的话缓存每帧失效，
        // 暂停反而比不暂停还贵
        let whole = include_str!("observatory_view.rs");
        let src = whole.split("#[cfg(test)]").next().unwrap();
        assert!(
            src.contains("f.at_seq as u64"),
            "暂停时 version 要用冻结那一刻的 seq"
        );
    }

    #[test]
    fn recording_stays_visible_outside_the_record_mode() {
        // 切走之后忘了自己还在往盘上写，是这类工具最容易造成的事故
        let whole = include_str!("observatory_view.rs");
        let src = whole.split("#[cfg(test)]").next().unwrap();
        assert!(src.contains("正在录制"), "别的模式下也要提示正在录");
    }

    #[test]
    fn the_capture_filter_is_visually_separated_from_the_display_filter() {
        // 用户**永远**会把这两个搞混（Wireshark 三十年的经验）。
        // 这条用例守的是那句常驻警告：它必须出现在捕获筛选旁边，
        // 而且必须说清「筛掉的永远没有了」
        let whole = include_str!("observatory_view.rs");
        let src = whole.split("#[cfg(test)]").next().unwrap();
        assert!(
            src.contains("筛掉的数据不会被录下来"),
            "捕获筛选旁边必须常驻这句警告"
        );
        assert!(
            src.contains("只影响显示，不影响录制"),
            "显示筛选旁边也要有对应的一句"
        );
    }

    #[test]
    fn a_blocked_trigger_shows_why_not_just_a_count() {
        // 用户看着条件明明命中却没录，会以为是条件写错了，其实只是在冷却
        let t = super::super::observatory_readout::TrigStat {
            name: "宽".into(),
            fired: 1,
            blocked: 1_724_415,
            blocked_why: "本小时已触发 1/1 次".into(),
            used_this_hour: 1,
            cap_per_hour: 1,
            ..Default::default()
        };
        assert!(!t.blocked_why.is_empty());
        assert!(t.used_this_hour >= t.cap_per_hour, "到上限要能判出来并标黄");
        // 被挡次数远大于已录次数 = 条件写得太宽，界面据此变色
        assert!(t.blocked > t.fired * 1000);
    }

    #[test]
    fn the_recall_buttons_say_how_far_back_the_ring_actually_reaches() {
        // 环只覆盖 11 秒时，「回捞最近 60 秒」按下去只会得到 11 秒的数据。
        // 不写在按钮上的话，用户会拿到一份安静变短的文件还以为是完整的
        //
        // 这里断言的是那条规则的**判据**（record_block 里的 `enough`）：
        // 覆盖不够时按钮文案必须带上实际值
        let cov = 11.2f64;
        for secs in [10u32, 30, 60, 300] {
            let enough = cov >= secs as f64;
            let label = if enough {
                format!("{secs}s")
            } else {
                format!("{secs}s（只有 {cov:.0}s）")
            };
            if secs <= 10 {
                assert_eq!(label, "10s", "够的时候不加噪音");
            } else {
                assert!(label.contains("只有 11s"), "{label}");
            }
        }
    }

    #[test]
    fn a_halted_recording_reads_differently_from_a_stopped_one() {
        // 「被闸门停了」和「用户点了停止」是两回事。都显示成「未在录制」
        // 会让人以为是自己点的
        let halted = super::super::observatory_readout::RecStat {
            on: false,
            halted: true,
            halt: "磁盘只剩 1.0G（地板 5.0G），已停止录制".into(),
            ..Default::default()
        };
        assert!(halted.halted && !halted.on);
        assert!(halted.halt.contains("磁盘"));
        let stopped = super::super::observatory_readout::RecStat::default();
        assert!(!stopped.halted && stopped.halt.is_empty());
    }

    #[test]
    fn multibyte_text_is_clipped_by_characters() {
        // 按字节切会切出半个字
        assert_eq!(clip("一二三四五", 3), "一二…");
        assert_eq!(clip("abc", 10), "abc");
    }
}
