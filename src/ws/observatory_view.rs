//! 数据接口观察终端的渲染（docs/23）。
//!
//! # 这个文件里不许出现任何具体协议的名字
//!
//! 连接表单、流清单、丢弃策略全部从守护下发的 descriptor 来（[`super::observatory_readout`]）。
//! 一旦这里按某个具体 adapter id 或传输类型走分支，「加协议不改 UI」就已经破了。
//! `no_adapter_id_leaks_into_this_view` 钉住这一点——它连注释一起扫，
//! 因为注释里出现一个具体 id，通常就是代码即将引用它的前一步。

use iced::widget::{button, column, container, row, scrollable, text};
use iced::{Color, Element, Length};

use super::observatory_readout::{self as ro, StreamStat, TailRow};

const C_HEAD: Color = Color::from_rgb(0.55, 0.8, 1.0);
const C_DIM: Color = Color::from_rgb(0.55, 0.55, 0.6);
const C_TXT: Color = Color::from_rgb(0.85, 0.87, 0.92);
const C_GOLD: Color = Color::from_rgb(0.9, 0.8, 0.4);
const C_BAD: Color = Color::from_rgb(0.9, 0.45, 0.4);
const C_OK: Color = Color::from_rgb(0.35, 0.78, 0.98);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObsMsg {
    Start,
    Stop,
    /// 连/断当前目标。
    Connect,
    Disconnect,
}

fn cell<'a>(s: String, w: f32, c: Color, numeric: bool) -> Element<'a, ObsMsg> {
    container(text(s).size(11).color(c))
        .width(Length::Fixed(w))
        .align_x(if numeric { iced::Alignment::End } else { iced::Alignment::Start })
        .into()
}

fn chip<'a>(label: &str, msg: ObsMsg) -> Element<'a, ObsMsg> {
    button(text(label.to_string()).size(11)).padding([2, 7]).on_press(msg).into()
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

    // ── Adapter 目录：**完全由守护下发**，这里不认识任何协议 ──
    body = body.push(section("已注册的 Adapter", "加协议只改守护侧；这张表和下面的表单都是它下发的"));
    let mut h = row![].spacing(3);
    for (t, w, n) in [
        ("id", 130.0, false),
        ("名称", 170.0, false),
        ("传输", 60.0, false),
        ("配置项", 60.0, true),
        ("流", 46.0, true),
        ("可发请求", 70.0, false),
    ] {
        h = h.push(cell(t.into(), w, C_HEAD, n));
    }
    body = body.push(h);
    for a in &st.catalog {
        body = body.push(
            row![
                cell(a.id.clone(), 130.0, C_TXT, false),
                cell(a.label.clone(), 170.0, C_TXT, false),
                cell(a.transport.clone(), 60.0, C_DIM, false),
                cell(a.config_schema.len().to_string(), 60.0, C_DIM, true),
                cell(a.streams.len().to_string(), 46.0, C_DIM, true),
                cell(if a.can_request { "是" } else { "否" }.into(), 70.0, C_DIM, false),
            ]
            .spacing(3),
        );
    }

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

    // ── 环形缓冲：覆盖区间是这一屏最要紧的一个数 ──
    let r = &sess.ring;
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

    // ── 逐流指标：四个阶段分开 ──
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

    // ── 尾窗 ──
    let hdr = if sess.tail_skipped > 0 {
        // 假装全都显示了，是这类工具最常见的谎
        format!("最近 {} 条（本窗跳过 {} 条）", sess.tail.len(), sess.tail_skipped)
    } else {
        format!("最近 {} 条", sess.tail.len())
    };
    body = body.push(section(&hdr, "原始负载。解码是可选的一层——观察没见过的接口时，Raw 才是真相"));
    let mut h = row![].spacing(3);
    for (t, w, n) in [
        ("seq", 74.0, true),
        ("时刻", 84.0, false),
        ("流", 34.0, true),
        ("长度", 60.0, true),
        ("标志", 108.0, false),
        ("负载", 620.0, false),
    ] {
        h = h.push(cell(t.into(), w, C_HEAD, n));
    }
    body = body.push(h);
    for t in sess.tail.iter().rev() {
        body = body.push(tail_row(t));
    }

    body = body.push(
        text(format!("快照 {} · 读于 {}", st.stamp, st.refreshed)).size(10).color(C_DIM),
    );
    scrollable(body).width(Length::Fill).height(Length::Fill).into()
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

fn tail_row<'a>(t: &TailRow) -> Element<'a, ObsMsg> {
    let flag_c = if t.flags.contains("gap") {
        C_BAD
    } else if t.flags.is_empty() {
        C_DIM
    } else {
        C_GOLD
    };
    let clock = chrono::DateTime::from_timestamp_millis(t.recv_ms)
        .map(|x| x.with_timezone(&chrono::Local).format("%H:%M:%S%.3f").to_string())
        .unwrap_or_else(|| "—".into());
    let payload = if t.preview_truncated {
        format!("{}…（共 {} 字节）", clip(&t.preview, 90), t.len)
    } else {
        clip(&t.preview, 100)
    };
    row![
        cell(t.seq.to_string(), 74.0, C_DIM, true),
        cell(clock, 84.0, C_DIM, false),
        cell(t.stream_id.to_string(), 34.0, C_DIM, true),
        cell(t.len.to_string(), 60.0, C_DIM, true),
        cell(if t.flags.is_empty() { "—".into() } else { t.flags.clone() }, 108.0, flag_c, false),
        cell(payload, 620.0, if t.flags.contains("synthetic") { C_GOLD } else { C_TXT }, false),
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

    #[test]
    fn multibyte_text_is_clipped_by_characters() {
        // 按字节切会切出半个字
        assert_eq!(clip("一二三四五", 3), "一二…");
        assert_eq!(clip("abc", 10), "abc");
    }
}
