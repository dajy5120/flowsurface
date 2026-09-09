//! 网络出口总闸的渲染。
//!
//! 一页看全「谁在往外发包」，每一路都能手动启停。清单与计数在
//! [`super::egress`]，这里只画。

use iced::widget::{button, column, container, row, scrollable, text};
use iced::{Color, Element, Length};

use super::egress::{self, Kind};

const C_HEAD: Color = Color::from_rgb(0.55, 0.8, 1.0);
const C_DIM: Color = Color::from_rgb(0.55, 0.55, 0.6);
const C_TXT: Color = Color::from_rgb(0.85, 0.87, 0.92);
const C_GOLD: Color = Color::from_rgb(0.9, 0.8, 0.4);
const C_OK: Color = Color::from_rgb(0.35, 0.78, 0.98);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressMsg {
    /// `(出口 key, systemctl 动作)`。
    Act(String, &'static str),
    /// 全部停止。**不动开机自启**——那是另一件事。
    StopAll,
    Refresh,
}

fn chip<'a>(label: &str, msg: EgressMsg) -> Element<'a, EgressMsg> {
    button(text(label.to_string()).size(11))
        .padding([2, 7])
        .style(|t, st| crate::style::button::modifier(t, st, false))
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
        EgressMsg::Refresh => {
            egress::waker().request();
            String::new()
        }
    }
}

pub fn pane_body<'a>(note: &str) -> Element<'a, EgressMsg> {
    let rows = egress::rows();
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
            text(format!("本项目 {ours} 条 · 全机 {}", total.label()))
                .size(11)
                .color(if ours > 0 { C_OK } else { C_DIM }),
            chip("全部停止", EgressMsg::StopAll),
            chip("刷新", EgressMsg::Refresh),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center),
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

    // ── 表头 ──
    let mut h = row![].spacing(4);
    for (t, w, n) in [
        ("出口", 150.0, false),
        ("连谁", 320.0, false),
        ("状态", 96.0, false),
        ("连接", 130.0, true),
        ("速度", 200.0, false),
        ("今日", 84.0, true),
        ("下次 / 已运行", 120.0, false),
        ("开机自启", 84.0, false),
        ("", 150.0, false),
    ] {
        h = h.push(cell(t.into(), w, C_HEAD, n));
    }
    body = body.push(h);

    for (s, r) in egress::ALL.iter().zip(&rows) {
        let state = match (s.kind, r.on) {
            (Kind::Foreign, true) => ("● 在跑", C_DIM),
            (Kind::Foreign, false) => ("○ 没跑", C_DIM),
            // timer 的「在跑」是「会按时触发」，不是「此刻在执行」。
            // 用同一个词会让人以为它一直在耗流量
            (Kind::Timer, true) => ("⏱ 会触发", C_GOLD),
            (Kind::Timer, false) => ("○ 不触发", C_DIM),
            (_, true) => ("● 在跑", C_OK),
            (_, false) => ("○ 停着", C_DIM),
        };
        let when = match s.kind {
            Kind::Timer => r.next.clone(),
            _ if r.on => super::svcctl::fmt_dur(r.uptime_secs),
            _ => String::new(),
        };
        let mut line = row![
            cell(s.label.into(), 150.0, C_TXT, false),
            cell(s.what.into(), 320.0, C_DIM, false),
            cell(state.0.into(), 96.0, state.1, false),
            cell(
                r.conns.map(|c| c.label()).unwrap_or_else(|| "—".into()),
                130.0,
                if r.conns.is_some_and(|c| c.total() > 0) { C_OK } else { C_DIM },
                true
            ),
            // 「—」= 还没有两次采样，或这一路根本没在跑。
            // **和「0B/s」是两回事**：后者是「连着但没在传」
            cell(
                r.bps.map(|r| r.label()).unwrap_or_else(|| "—".into()),
                200.0,
                if r.bps.is_some_and(|r| r.total_down() > 1024.0) { C_OK } else { C_DIM },
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
        body = body.push(line);
    }

    body = body.push(
        text(
            "停 Cockpit 行情图 = 关掉交易所订阅，连接会真的断开（不是只让界面不显示）；\
             再打开时图表自己会重新连上。",
        )
        .size(10)
        .color(C_DIM),
    );
    body = body.push(
        text(
            "速度每 2 秒采一次，是**下限**——两次采样之间关掉的连接，它最后那段字节数没算进来。\
             「—」是还没测出来或这一路没在跑，「0B/s」是连着但没在传。\
             一路同时有「直连」和「经本机」两个数时，那多半是个代理——两边是同一份字节。",
        )
        .size(10)
        .color(C_DIM),
    );
    scrollable(body).width(Length::Fill).height(Length::Fill).into()
}
