//! 进程页的渲染。清单与状态在 [`super::procs`]，这里只画。

use iced::widget::{button, column, container, row, text};
use iced::{Color, Element, Length};

use super::procs::{self, Row};
use crate::ui::grid::{self, Cell, Column, GridMsg, GridState};
use crate::ui::widgets::Tone;

/// 停掉即**永久数据缺口**的服务（关窗口会停掉它们）。
/// 其余守护停了只是停更，重启就补回来。
const IRREVERSIBLE: [&str; 2] = ["recorder", "maker-shadow"];

#[derive(Debug, Clone)]
pub enum ProcsMsg {
    /// 第 n 张表的网格交互（0 常驻守护 / 1 按点触发 / 2 依赖；ui::grid，docs/35 §16.13 第 5 项）
    Grid(u8, GridMsg),
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
    button(text(label.to_string()).size(crate::ui::text::s_small()))
        .padding(crate::ui::metrics::pad2(0, 2))
        .style(|t, st| crate::style::button::modifier(t, st, false))
        .on_press(msg)
        .into()
}

fn cell<'a>(s: String, w: f32, c: Color, numeric: bool) -> Element<'a, ProcsMsg> {
    container(text(s).size(crate::ui::text::s_small()).color(c))
        .width(Length::Fixed(w))
        .align_x(if numeric { iced::Alignment::End } else { iced::Alignment::Start })
        .into()
}

/// 常驻守护表里两个操作列的列号（handle 按它认是哪个操作）
const ACT_TOGGLE: usize = 7;
const ACT_RESTART: usize = 8;
/// 按点触发表的操作列
const TIMER_ACT: usize = 3;

fn daemon_cols() -> Vec<Column> {
    vec![
        Column::text("服务", 130.0).key().pinned(),
        Column::text("干什么", 360.0),
        Column::text("状态", 110.0).groupable(),
        Column::num("已运行", None, 96.0),
        Column::num("重启", None, 56.0),
        // 资源可见性（docs/35 §8，UPDS V7 §60）：谁最占内存 / CPU，一眼看得出来该先停哪个
        Column::num("内存", Some("MB"), 76.0),
        Column::num("CPU", Some("%"), 64.0),
        Column { sort: crate::ui::grid::SortKind::None, ..Column::text("启停", 72.0) },
        Column { sort: crate::ui::grid::SortKind::None, ..Column::text("重启", 72.0) },
        Column::text("停了会怎样", 360.0),
        Column::text("提醒", 420.0),
    ]
}

fn timer_cols() -> Vec<Column> {
    vec![
        Column::text("任务", 130.0).key(),
        Column::text("干什么", 360.0),
        Column::text("下次触发", 172.0),
        Column { sort: crate::ui::grid::SortKind::None, ..Column::text("操作", 72.0) },
    ]
}

fn dep_cols() -> Vec<Column> {
    vec![
        Column::text("依赖", 140.0).key(),
        Column::text("在本项目里干什么", 360.0),
        Column::text("当前", 172.0),
        Column::text("最新", 130.0),
        Column { sort: crate::ui::grid::SortKind::None, ..Column::text("操作", 72.0) },
        Column::text("备注", 360.0),
    ]
}

thread_local! {
    static GRIDS: std::cell::RefCell<std::collections::HashMap<u8, GridState>> = std::cell::RefCell::new(std::collections::HashMap::new());
}

fn cols_of(n: u8) -> Vec<Column> {
    match n {
        0 => daemon_cols(),
        1 => timer_cols(),
        _ => dep_cols(),
    }
}

fn grid_update(n: u8, m: GridMsg) {
    let cols = cols_of(n);
    GRIDS.with(|g| g.borrow_mut().entry(n).or_insert_with(|| GridState::new(&cols)).update(m, &cols, &[]));
}

/// 一张按行数定高的网格（外层是滚动容器）。
fn ptable<'a>(n: u8, cols: Vec<Column>, rows: Vec<Vec<Cell>>) -> Element<'a, ProcsMsg> {
    let k = rows.len();
    let st = GRIDS.with(|g| {
        let mut g = g.borrow_mut();
        let st = g.entry(n).or_insert_with(|| GridState::new(&cols));
        st.resort(&cols, &rows);
        st.clone()
    });
    let h = crate::ui::metrics::panel_header() + crate::ui::metrics::row_height() * (k.max(1) as f32 + 1.0) + 60.0;
    container(grid::view(cols, rows, st, None, move |m| ProcsMsg::Grid(n, m))).height(Length::Fixed(h)).into()
}

pub fn handle(m: ProcsMsg) -> String {
    match m {
        // 操作格：按同样的规则重新取一次清单，用行号找到是哪个服务（与 pane_body 建表顺序一致）
        ProcsMsg::Grid(n, GridMsg::Action(row, col)) => {
            if n == 2 {
                return match super::deps::rows().get(row) {
                    Some(d) => handle(ProcsMsg::UpdateDep(d.key.clone())),
                    None => String::new(),
                };
            }
            let rows = procs::rows();
            let list: Vec<&Row> = rows.iter().filter(|r| r.timer == (n == 1)).collect();
            let Some(r) = list.get(row) else { return String::new() };
            let act = match (n, col) {
                (0, ACT_TOGGLE) | (1, TIMER_ACT) => if r.st.active { "stop" } else { "start" },
                (0, ACT_RESTART) => "restart",
                _ => return String::new(),
            };
            procs::action(&r.key, act)
        }
        ProcsMsg::Grid(n, g) => {
            grid_update(n, g);
            String::new()
        }
        ProcsMsg::Act(k, a) => procs::action(&k, a),
        ProcsMsg::Refresh => {
            procs::waker().request();
            super::deps::refresh_local_bg(); // 本地版本，零网络；后台读，不卡帧
            String::new()
        }
        // 十几个 HTTP 请求。**绝不能在渲染线程里做**——界面会整个卡住
        // （docs/20 §19.2 的每帧开销教训）。丢后台线程，回执写进进程级静态。
        ProcsMsg::CheckDeps => {
            super::spawn_named("ws-procsv", || {
                let r = super::deps::check_all();
                super::deps::set_note(&r);
            });
            "正在查上游版本…".into()
        }
        ProcsMsg::UpdateDep(k) => {
            super::spawn_named("ws-procsv", move || {
                let r = super::deps::update(&k);
                super::deps::set_note(&r);
            });
            "正在更新…".into()
        }
    }
}

/// 进程页的说明（面板内联显示；检查器托管时在「说明」页）。
pub const NOTES: [&str; 4] = [
    "下面这些守护跟随本窗口：Cockpit 一起来就拉起，关掉就一并停止。",
    "级联由 systemd 负责（各单元 PartOf=ws-stack.target），所以停止是原子的、\
     也不需要面板懂依赖次序。但 Cockpit 被 kill -9 时没有任何用户态代码能跑——\
     那种情况下守护会留着，下次启动 Cockpit 时被同一个 target 接管，不会变成孤儿。",
    "编排交给 systemd：进程挂了自动按退避重启，「重启」列的数字就是它爬起来过几次。",
    "有外部连接的那几个（两个 feed）在「网络出口」页上也能停——两边操作的是同一个单元。",
];

pub fn pane_body<'a>(note: &str, lock: Option<&str>) -> Element<'a, ProcsMsg> {
    if lock == Some("检查更新") {
        return deps_part(column![].spacing(6).padding(crate::ui::metrics::space(3)));
    }
    let rows = procs::rows();
    let mut body = column![].spacing(6).padding(crate::ui::metrics::space(3));

    if rows.is_empty() {
        // poller 起来要一拍。**别显示一张空表**——空表看起来像「什么都没在跑」
        return column![text("正在查询…").size(crate::ui::text::s_small()).color(crate::ui::pal::dim())].padding(crate::ui::metrics::space(3)).into();
    }

    let daemons: Vec<&Row> = rows.iter().filter(|r| !r.timer).collect();
    let timers: Vec<&Row> = rows.iter().filter(|r| r.timer).collect();
    let up = daemons.iter().filter(|r| r.st.active).count();
    body = body.push(
        row![
            text("进程").size(crate::ui::text::s_emph()).color(crate::ui::pal::head()),
            text(format!("{up} / {} 常驻守护在运行", daemons.len()))
                .size(crate::ui::text::s_emph())
                .color(if up > 0 { crate::ui::pal::ok() } else { crate::ui::pal::dim() }),
            chip("刷新", ProcsMsg::Refresh),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center),
    );
    // 长说明（docs/42 第 4 期）：检查器托管时进「说明」页，主区只留表
    if super::inspector_props::part().intro() {
        for (i, n) in NOTES.iter().enumerate() {
            body = body.push(text(*n).size(crate::ui::text::s_meta()).color(if i == 0 { crate::ui::pal::ok() } else { crate::ui::pal::dim() }));
        }
    }
    if !note.is_empty() {
        body = body.push(text(note.to_string()).size(crate::ui::text::s_small()).color(crate::ui::pal::warn()));
    }

    // ── 常驻守护（ui::grid）：原来挂在行下面的「停了会怎样」「永久缺口」提醒变成两列，一字不少 ──
    let rows_d: Vec<Vec<Cell>> = daemons
        .iter()
        .map(|r| {
            let (st_txt, st_col) = if r.st.active {
                ("运行中".to_string(), crate::ui::pal::ok())
            } else if r.st.ever_ran() && !r.st.last_ok() {
                (format!("停止（{}）", r.st.last_result), crate::ui::pal::warn())
            } else {
                ("已停止".to_string(), crate::ui::pal::dim())
            };
            vec![
                Cell::Text(r.label.clone()),
                Cell::Colored(r.what.clone(), crate::ui::pal::dim()),
                Cell::Colored(st_txt, st_col),
                if r.st.active { Cell::num(r.st.uptime_secs as f64, super::svcctl::fmt_dur(r.st.uptime_secs)) } else { Cell::Absent(crate::ui::fmt::Absence::NotApplicable) },
                // 重启次数不为 0 就标出来：它说明这个服务在反复爬起来，而「现在是运行中」会把这件事盖住
                Cell::Colored(r.st.restarts.to_string(), if r.st.restarts > 0 { crate::ui::pal::warn() } else { crate::ui::pal::dim() }),
                match r.st.mem_bytes.filter(|_| r.st.active) {
                    Some(b) => {
                        let mb = b as f64 / 1_048_576.0;
                        // 不到 10MB 留一位小数：取整会把几百 KB 的小服务显示成「0」
                        Cell::num(mb, if mb < 10.0 { format!("{mb:.1}") } else { format!("{mb:.0}") })
                    }
                    None => Cell::Absent(if r.st.active { crate::ui::fmt::Absence::Unknown } else { crate::ui::fmt::Absence::NotApplicable }),
                },
                match r.cpu_pct {
                    Some(p) => Cell::num(p, format!("{p:.1}")),
                    // 第一轮还没有上一次的值：「未取到」；没在跑：不适用
                    None => Cell::Absent(if r.st.active { crate::ui::fmt::Absence::Unknown } else { crate::ui::fmt::Absence::NotApplicable }),
                },
                Cell::Action(if r.st.active { "停止".into() } else { "启动".into() }, Tone::Neutral),
                Cell::Action("重启".into(), Tone::Neutral),
                // 停了会怎样：只在已停止时写（运行时挂着是噪音，停了却不说会以为功能还在）
                Cell::Colored(if r.st.active { String::new() } else { r.if_stopped.clone() }, crate::ui::pal::dim()),
                // 这两个停掉是永久缺口：一直显示（停了才说就晚了）
                Cell::Colored(
                    if IRREVERSIBLE.contains(&r.key.as_str()) { "⚠ 停掉的时间就是数据的缺口，事后补不回来（关闭本窗口会停掉它）".into() } else { String::new() },
                    crate::ui::pal::warn(),
                ),
            ]
        })
        .collect();
    body = body.push(ptable(0, daemon_cols(), rows_d));

    // ── 按点触发的任务 ──
    if !timers.is_empty() {
        body = body.push(crate::ui::widgets::major("按点触发（不跟随窗口）"));
        body = body.push(
            text(
                "这些是研究任务，绑进窗口生命周期等于「Cockpit 没开着就永远不跑」——\
                 那不是关掉后台，是把夜跑废掉。列在这里是为了让它们可见：\
                 关掉界面之后还会按时启动的，就只有这几个。",
            )
            .size(crate::ui::text::s_meta())
            .color(crate::ui::pal::dim()),
        );
        let rows_t: Vec<Vec<Cell>> = timers
            .iter()
            .map(|r| {
                vec![
                    Cell::Text(r.label.clone()),
                    Cell::Colored(r.what.clone(), crate::ui::pal::dim()),
                    // 「下次触发」是这一段的核心：只看「没在跑」会得出「已经全停了」的错误结论
                    Cell::Colored(if r.st.active { r.next.clone() } else { "已停用".into() }, if r.st.active { crate::ui::pal::warn() } else { crate::ui::pal::dim() }),
                    Cell::Action(if r.st.active { "停用".into() } else { "启用".into() }, Tone::Neutral),
                ]
            })
            .collect();
        body = body.push(ptable(1, timer_cols(), rows_t));
    }

    // 页面「进程」只到这里；依赖版本是「检查更新」页（docs/41）
    if lock == Some("进程") {
        return crate::ui::scroll(body).into();
    }
    deps_part(body)
}

/// 依赖版本与「检查更新」（docs/41：资源工作区的单独一页；不锁页时接在进程表下面）。
fn deps_part<'a>(mut body: iced::widget::Column<'a, ProcsMsg>) -> Element<'a, ProcsMsg> {
    let drows = super::deps::rows();
    let dnote = super::deps::note();
    let checked = drows.iter().filter(|r| r.checked()).count();
    let stale = drows.iter().filter(|r| r.outdated()).count();
    body = body.push(crate::ui::widgets::major("依赖"));
    body = body.push(
        row![
            // 查询要十几个 HTTP 请求：查的时候按钮转圈、宽度不变、不能重复点
            crate::ui::widgets::btn_busy("检查更新", crate::ui::widgets::Kind::Standard, Some(ProcsMsg::CheckDeps), super::deps::checking()),
            text(if checked == 0 {
                "当前版本全部来自本地（零网络）。上游版本要点上面那个按钮才去问。".to_string()
            } else if stale == 0 {
                format!("已查 {checked} 项，全部是最新")
            } else {
                format!("已查 {checked} 项，{stale} 项有新版本")
            })
            .size(crate::ui::text::s_meta())
            .color(if stale > 0 { crate::ui::pal::warn() } else { crate::ui::pal::dim() }),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center),
    );
    body = body.push(
        text(
            "不做后台轮询——你没在看的时候它不会去问上游。「最新版本」空着表示\
             还没查过，不是「已经是最新」。",
        )
        .size(crate::ui::text::s_meta())
        .color(crate::ui::pal::dim()),
    );
    if drows.is_empty() && super::deps::loading() {
        body = body.push(text("正在读取本地版本…").size(crate::ui::text::s_small()).color(crate::ui::pal::dim()));
    }
    if !dnote.is_empty() {
        body = body.push(text(dnote).size(crate::ui::text::s_small()).color(crate::ui::pal::warn()));
    }
    let rows_dep: Vec<Vec<Cell>> = drows
        .iter()
        .map(|r| {
            vec![
                Cell::Colored(if r.core { format!("★ {}", r.label) } else { r.label.clone() }, if r.core { crate::ui::pal::ok() } else { crate::ui::pal::txt() }),
                Cell::Colored(r.what.clone(), crate::ui::pal::dim()),
                Cell::Colored(if r.current.is_empty() { "读不到".into() } else { r.current.clone() }, if r.current.is_empty() { crate::ui::pal::warn() } else { crate::ui::pal::txt() }),
                // 「未查」和「已最新」必须看得出区别——混在一起是这一页最容易骗人的地方
                Cell::Colored(
                    if !r.checked() { "未查".into() } else { r.latest.clone() },
                    if r.outdated() { crate::ui::pal::warn() } else if r.checked() { crate::ui::pal::ok() } else { crate::ui::pal::dim() },
                ),
                Cell::Action("更新".into(), Tone::Neutral),
                Cell::Colored(r.note.clone(), crate::ui::pal::dim()),
            ]
        })
        .collect();
    body = body.push(ptable(2, dep_cols(), rows_dep));
    body = body.push(
        text(
            "更新的代价按类型不同：pip 包更新完用到它的守护要重启；Rust 依赖\
             只改 Cargo.lock，要重新编译才生效；FlowSurface 是带着六十多个自有\
             模块的 fork，「更新」只做 git fetch 不合并——在运行中的界面里替你\
             merge 上游、再把你脚下的二进制重编，那是事故不是便利。",
        )
        .size(crate::ui::text::s_meta())
        .color(crate::ui::pal::dim()),
    );

    crate::ui::scroll(body).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 操作列号与表头对得上() {
        // handle 靠列号认是哪个操作；列表改了顺序而常量没跟着改，点「重启」会变成「停止」
        assert_eq!(daemon_cols()[ACT_TOGGLE].title, "启停");
        assert_eq!(daemon_cols()[ACT_RESTART].title, "重启");
        assert_eq!(timer_cols()[TIMER_ACT].title, "操作");
    }

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
