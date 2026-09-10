//! 进程页的渲染。清单与状态在 [`super::procs`]，这里只画。

use iced::widget::{button, column, container, row, scrollable, text};
use iced::{Color, Element, Length};

use super::procs::{self, Row};

/// 停掉即**永久数据缺口**的服务（关窗口会停掉它们）。
/// 其余守护停了只是停更，重启就补回来。
const IRREVERSIBLE: [&str; 2] = ["recorder", "maker-shadow"];

const C_HEAD: Color = Color::from_rgb(0.55, 0.8, 1.0);
const C_DIM: Color = Color::from_rgb(0.55, 0.55, 0.6);
const C_TXT: Color = Color::from_rgb(0.85, 0.87, 0.92);
const C_GOLD: Color = Color::from_rgb(0.9, 0.8, 0.4);
const C_OK: Color = Color::from_rgb(0.35, 0.78, 0.98);
const C_WARN: Color = Color::from_rgb(0.95, 0.6, 0.35);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcsMsg {
    /// `(条目 key, systemctl 动作)`。
    Act(String, &'static str),
    Refresh,
    /// 去问上游最新版本。**这是这一页唯一会发起外部连接的动作**，
    /// 所以必须是用户主动点的，不能是后台轮询。
    CheckDeps,
    /// `(依赖 key)`：执行更新。
    UpdateDep(String),
}

fn chip<'a>(label: &str, msg: ProcsMsg) -> Element<'a, ProcsMsg> {
    button(text(label.to_string()).size(11))
        .padding([2, 7])
        .style(|t, st| crate::style::button::modifier(t, st, false))
        .on_press(msg)
        .into()
}

fn cell<'a>(s: String, w: f32, c: Color, numeric: bool) -> Element<'a, ProcsMsg> {
    container(text(s).size(11).color(c))
        .width(Length::Fixed(w))
        .align_x(if numeric { iced::Alignment::End } else { iced::Alignment::Start })
        .into()
}

pub fn handle(m: ProcsMsg) -> String {
    match m {
        ProcsMsg::Act(k, a) => procs::action(&k, a),
        ProcsMsg::Refresh => {
            procs::waker().request();
            super::deps::refresh_local(); // 本地版本，零网络
            String::new()
        }
        // 十几个 HTTP 请求。**绝不能在渲染线程里做**——界面会整个卡住
        // （docs/20 §19.2 的每帧开销教训）。丢后台线程，回执写进进程级静态。
        ProcsMsg::CheckDeps => {
            std::thread::spawn(|| {
                let r = super::deps::check_all();
                super::deps::set_note(&r);
            });
            "正在查上游版本…".into()
        }
        ProcsMsg::UpdateDep(k) => {
            std::thread::spawn(move || {
                let r = super::deps::update(&k);
                super::deps::set_note(&r);
            });
            "正在更新…".into()
        }
    }
}

pub fn pane_body<'a>(note: &str) -> Element<'a, ProcsMsg> {
    let rows = procs::rows();
    let mut body = column![].spacing(6).padding(8);

    if rows.is_empty() {
        // poller 起来要一拍。**别显示一张空表**——空表看起来像「什么都没在跑」
        return column![text("正在查询…").size(11).color(C_DIM)].padding(8).into();
    }

    let daemons: Vec<&Row> = rows.iter().filter(|r| !r.timer).collect();
    let timers: Vec<&Row> = rows.iter().filter(|r| r.timer).collect();
    let up = daemons.iter().filter(|r| r.st.active).count();
    body = body.push(
        row![
            text("进程").size(13).color(C_HEAD),
            text(format!("{up} / {} 常驻守护在运行", daemons.len()))
                .size(13)
                .color(if up > 0 { C_OK } else { C_DIM }),
            chip("刷新", ProcsMsg::Refresh),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center),
    );
    body = body.push(
        text("下面这些守护**跟随本窗口**：Cockpit 一起来就拉起，关掉就一并停止。")
            .size(10)
            .color(C_OK),
    );
    body = body.push(
        text(
            "级联由 systemd 负责（各单元 PartOf=ws-stack.target），所以停止是原子的、\
             也不需要面板懂依赖次序。**但 Cockpit 被 kill -9 时没有任何用户态代码能跑**——\
             那种情况下守护会留着，下次启动 Cockpit 时被同一个 target 接管，不会变成孤儿。",
        )
        .size(10)
        .color(C_DIM),
    );
    body = body.push(
        text("编排交给 systemd：进程挂了自动按退避重启，「重启」列的数字就是它爬起来过几次。")
            .size(10)
            .color(C_DIM),
    );
    body = body.push(
        text(
            "有外部连接的那几个（两个 feed）在「网络出口」页上也能停——两边操作的是同一个单元。",
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
        ("服务", 130.0, false),
        ("干什么", 360.0, false),
        ("状态", 76.0, false),
        ("已运行", 96.0, true),
        ("重启", 56.0, true),
        ("操作", 150.0, false),
    ] {
        h = h.push(cell(t.into(), w, C_DIM, n));
    }
    body = body.push(h);

    for r in &daemons {
        let (st_txt, st_col) = if r.st.active {
            ("运行中".to_string(), C_OK)
        } else if r.st.ever_ran() && !r.st.last_ok() {
            (format!("停止（{}）", r.st.last_result), C_WARN)
        } else {
            ("已停止".to_string(), C_DIM)
        };
        let mut line = row![].spacing(4).align_y(iced::Alignment::Center);
        line = line.push(cell(r.label.clone(), 130.0, C_TXT, false));
        line = line.push(cell(r.what.clone(), 360.0, C_DIM, false));
        line = line.push(cell(st_txt, 76.0, st_col, false));
        line = line.push(cell(
            if r.st.active { super::svcctl::fmt_dur(r.st.uptime_secs) } else { "—".into() },
            96.0,
            C_TXT,
            true,
        ));
        // 重启次数不为 0 就标出来：它说明这个服务在反复爬起来，
        // 而「现在是运行中」会把这件事盖住
        line = line.push(cell(
            r.st.restarts.to_string(),
            56.0,
            if r.st.restarts > 0 { C_WARN } else { C_DIM },
            true,
        ));
        let acts = row![
            chip(if r.st.active { "停止" } else { "启动" },
                 ProcsMsg::Act(r.key.clone(), if r.st.active { "stop" } else { "start" })),
            chip("重启", ProcsMsg::Act(r.key.clone(), "restart")),
        ]
        .spacing(4);
        line = line.push(container(acts).width(Length::Fixed(150.0)));
        body = body.push(line);
        // 停了会怎样：只在已停止时显示。运行时挂着一行警告是噪音，
        // 停了却不说会让人以为「进程没了但功能还在」
        if !r.st.active {
            body = body.push(
                container(text(format!("↳ {}", r.if_stopped)).size(10).color(C_DIM))
                    .padding(iced::Padding { top: 0.0, right: 0.0, bottom: 0.0, left: 134.0 }),
            );
        }
        // 这两个停掉是**永久缺口**，不是停更。关窗口就会停，所以要一直显示，
        // 不能像 if_stopped 那样只在已停时才说——那时候已经晚了。
        if IRREVERSIBLE.contains(&r.key.as_str()) {
            body = body.push(
                container(
                    text("⚠ 停掉的时间就是数据的缺口，事后补不回来（关闭本窗口会停掉它）")
                        .size(10)
                        .color(C_WARN),
                )
                .padding(iced::Padding { top: 0.0, right: 0.0, bottom: 0.0, left: 134.0 }),
            );
        }
    }

    // ── 按点触发的任务 ──
    if !timers.is_empty() {
        body = body.push(text("按点触发（不跟随窗口）").size(13).color(C_HEAD));
        body = body.push(
            text(
                "这些是研究任务，绑进窗口生命周期等于「Cockpit 没开着就永远不跑」——\
                 那不是关掉后台，是把夜跑废掉。列在这里是为了让它们**可见**：\
                 关掉界面之后还会按时启动的，就只有这几个。",
            )
            .size(10)
            .color(C_DIM),
        );
        let mut h = row![].spacing(4);
        for (t, w, n) in [("任务", 130.0, false), ("干什么", 360.0, false), ("下次触发", 172.0, false), ("操作", 150.0, false)] {
            h = h.push(cell(t.into(), w, C_DIM, n));
        }
        body = body.push(h);
        for r in &timers {
            let mut line = row![].spacing(4).align_y(iced::Alignment::Center);
            line = line.push(cell(r.label.clone(), 130.0, C_TXT, false));
            line = line.push(cell(r.what.clone(), 360.0, C_DIM, false));
            // 「下次触发」是这一段的核心：只看「没在跑」会得出「已经全停了」的错误结论
            line = line.push(cell(
                if r.st.active { r.next.clone() } else { "已停用".into() },
                172.0,
                if r.st.active { C_GOLD } else { C_DIM },
                false,
            ));
            let acts = row![chip(
                if r.st.active { "停用" } else { "启用" },
                ProcsMsg::Act(r.key.clone(), if r.st.active { "stop" } else { "start" })
            )]
            .spacing(4);
            line = line.push(container(acts).width(Length::Fixed(150.0)));
            body = body.push(line);
        }
    }

    // ── 依赖版本 ──
    let drows = super::deps::rows();
    let dnote = super::deps::note();
    let checked = drows.iter().filter(|r| r.checked()).count();
    let stale = drows.iter().filter(|r| r.outdated()).count();
    body = body.push(text("依赖").size(13).color(C_HEAD));
    body = body.push(
        row![
            chip("检查更新", ProcsMsg::CheckDeps),
            text(if checked == 0 {
                "当前版本全部来自本地（零网络）。上游版本要点上面那个按钮才去问。".to_string()
            } else if stale == 0 {
                format!("已查 {checked} 项，全部是最新")
            } else {
                format!("已查 {checked} 项，{stale} 项有新版本")
            })
            .size(10)
            .color(if stale > 0 { C_WARN } else { C_DIM }),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center),
    );
    body = body.push(
        text(
            "不做后台轮询——你没在看的时候它不会去问上游。「最新版本」空着表示\
             **还没查过**，不是「已经是最新」。",
        )
        .size(10)
        .color(C_DIM),
    );
    if !dnote.is_empty() {
        body = body.push(text(dnote).size(11).color(C_GOLD));
    }
    let mut dh = row![].spacing(4);
    for (t, w, n) in [
        ("依赖", 130.0, false),
        ("在本项目里干什么", 360.0, false),
        ("当前", 172.0, false),
        ("最新", 130.0, false),
        ("操作", 110.0, false),
    ] {
        dh = dh.push(cell(t.into(), w, C_DIM, n));
    }
    body = body.push(dh);
    for r in &drows {
        let mut line = row![].spacing(4).align_y(iced::Alignment::Center);
        line = line.push(cell(
            if r.core { format!("★ {}", r.label) } else { r.label.clone() },
            130.0,
            if r.core { C_OK } else { C_TXT },
            false,
        ));
        line = line.push(cell(r.what.clone(), 360.0, C_DIM, false));
        line = line.push(cell(
            if r.current.is_empty() { "读不到".into() } else { r.current.clone() },
            172.0,
            if r.current.is_empty() { C_WARN } else { C_TXT },
            false,
        ));
        // 「未查」和「已最新」必须看得出区别——混在一起是这一页最容易骗人的地方
        line = line.push(cell(
            if !r.checked() { "未查".into() } else { r.latest.clone() },
            130.0,
            if r.outdated() {
                C_WARN
            } else if r.checked() {
                C_OK
            } else {
                C_DIM
            },
            false,
        ));
        line = line.push(container(chip("更新", ProcsMsg::UpdateDep(r.key.clone())))
            .width(Length::Fixed(110.0)));
        body = body.push(line);
        if !r.note.is_empty() {
            body = body.push(
                container(text(format!("↳ {}", r.note)).size(10).color(C_DIM))
                    .padding(iced::Padding { top: 0.0, right: 0.0, bottom: 0.0, left: 134.0 }),
            );
        }
    }
    body = body.push(
        text(
            "更新的代价按类型不同：pip 包更新完**用到它的守护要重启**；Rust 依赖\
             只改 Cargo.lock，**要重新编译才生效**；FlowSurface 是带着六十多个自有\
             模块的 fork，「更新」只做 git fetch **不合并**——在运行中的界面里替你\
             merge 上游、再把你脚下的二进制重编，那是事故不是便利。",
        )
        .size(10)
        .color(C_DIM),
    );

    scrollable(body).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_button_offers_the_action_that_makes_sense_now() {
        // 一个已经在跑的服务给「启动」按钮 = 点了什么也不变，
        // 看起来就像按钮坏了。
        for r in procs::rows() {
            let expect = if r.st.active { "stop" } else { "start" };
            let m = ProcsMsg::Act(r.key.clone(), expect);
            assert!(matches!(m, ProcsMsg::Act(_, a) if a == expect));
        }
    }

    #[test]
    fn refresh_returns_no_note() {
        // 刷新不需要回执文字——时间戳自己会跳。回一句「已刷新」是噪音。
        assert!(handle(ProcsMsg::Refresh).is_empty());
    }
}
