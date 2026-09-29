//! 程序外壳（UPDS V2 §9，docs/35 §5.1 / 批 3）：命令栏、状态栏、底部面板、检查器、命令面板。
//!
//! ```text
//! ┌ 命令栏：工作区 · 环境徽标 · 运行 ········ ⌕ 搜索或运行命令 Ctrl K ┐
//! ├ 侧栏 ┬ 工作区（pane_grid，不变）                 ┬ 检查器（Ctrl I）┤
//! │      ├ 底部面板：日志 · 问题 · 活动（Ctrl J）     │                 │
//! └ 状态栏：环境 │ 对外流量 │ 行情订阅 │ 活动 │ 工作区 │ 外观 │ UTC ────┘
//! ```
//!
//! 这里只画、只管外壳自己的小状态（命令面板输入、底部面板页签、日志尾巴）。
//! 要改应用状态的动作一律抛 [`ShellEvent::Run`]，由 `main.rs` 执行——
//! 与命令面板、快捷键走同一条路（UPDS：一个命令一个定义）。

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use iced::widget::{
    button, column, container, mouse_area, opaque, row, scrollable, space, stack, text, text_input,
};
use iced::{Alignment, Background, Border, Element, Length, Padding};

use super::command::{self, Cmd, Entry};
use super::metrics::{self, Elevation, radius};
use super::{color, core, text as t};

// ── 工作区分组（docs/35 §5.3）────────────────────────────────────────

/// 侧栏的五组。组内第一个工作区有 Ctrl Shift n 直达。
pub fn groups() -> [(&'static str, &'static [&'static str]); 5] {
    crate::ws::workspace::GROUPS
}

/// 某工作区若是组首，给出它的直达快捷键文字。
pub fn group_shortcut(name: &str) -> Option<&'static str> {
    const KEYS: [&str; 5] = ["Ctrl Shift 1", "Ctrl Shift 2", "Ctrl Shift 3", "Ctrl Shift 4", "Ctrl Shift 5"];
    groups().iter().position(|(_, ws)| ws.first() == Some(&name)).map(|i| KEYS[i])
}

// ── 状态 ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BottomTab {
    #[default]
    Log,
    Problems,
    Activity,
}

impl BottomTab {
    fn label(self) -> &'static str {
        match self {
            Self::Log => "日志",
            Self::Problems => "问题",
            Self::Activity => "活动",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Palette {
    pub query: String,
    pub sel: usize,
}

#[derive(Debug, Clone)]
pub enum ShellEvent {
    Run(Cmd),
    PaletteQuery(String),
    PaletteMove(i32),
    PaletteSubmit,
    PaletteClose,
}

pub struct Shell {
    pub palette: Option<Palette>,
    pub bottom: bool,
    pub bottom_tab: BottomTab,
    pub inspector: bool,
    pub log: LogTail,
}

impl Default for Shell {
    fn default() -> Self {
        Self { palette: None, bottom: false, bottom_tab: BottomTab::Log, inspector: false, log: LogTail::default() }
    }
}

/// 命令面板输入框的 id（打开时把焦点给它）。
pub fn palette_input_id() -> iced::widget::Id {
    iced::widget::Id::new("ws-command-palette")
}

impl Shell {
    /// 处理外壳内部事件；需要应用去执行的命令原样返回。
    pub fn update(&mut self, ev: ShellEvent, entries: &[Entry]) -> Option<Cmd> {
        match ev {
            ShellEvent::Run(c) => return Some(c),
            ShellEvent::PaletteQuery(q) => {
                if let Some(p) = self.palette.as_mut() {
                    p.query = q;
                    p.sel = 0;
                }
            }
            ShellEvent::PaletteMove(d) => {
                if let Some(p) = self.palette.as_mut() {
                    let n = command::filter(entries, &p.query).len();
                    if n > 0 {
                        p.sel = ((p.sel as i32 + d).rem_euclid(n as i32)) as usize;
                    }
                }
            }
            ShellEvent::PaletteSubmit => {
                let picked = self.palette.as_ref().and_then(|p| {
                    command::filter(entries, &p.query).get(p.sel).map(|e| e.cmd.clone())
                });
                self.palette = None;
                return picked;
            }
            ShellEvent::PaletteClose => self.palette = None,
        }
        None
    }
}

// ── 日志尾巴（底部面板）──────────────────────────────────────────────

/// 读 `flowsurface-current.log` 的最后几百行，每 2 秒一次（只读文件尾 64KB）。
#[derive(Default)]
pub struct LogTail {
    pub lines: VecDeque<LogLine>,
    read_at: Option<Instant>,
}

#[derive(Debug, Clone)]
pub struct LogLine {
    pub time: String,
    pub level: Level,
    pub msg: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Error,
    Warn,
    Info,
    Other,
}

const LOG_KEEP: usize = 400;

impl LogTail {
    /// 到点就重读（在 `Tick` 里调；底部面板关着时不读）。
    pub fn refresh(&mut self) {
        if self.read_at.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
            return;
        }
        self.read_at = Some(Instant::now());
        let path = data::data_path(Some("flowsurface-current.log"));
        let Ok(bytes) = std::fs::read(&path) else { return };
        let start = bytes.len().saturating_sub(64 * 1024);
        let tail = String::from_utf8_lossy(&bytes[start..]);
        let mut lines: VecDeque<LogLine> = tail.lines().skip(usize::from(start > 0)).map(parse_line).collect();
        while lines.len() > LOG_KEEP {
            lines.pop_front();
        }
        self.lines = lines;
    }

    pub fn problems(&self) -> impl Iterator<Item = &LogLine> {
        self.lines.iter().filter(|l| matches!(l.level, Level::Error | Level::Warn))
    }
}

/// `14:18:46.400:INFO -- 消息`
fn parse_line(s: &str) -> LogLine {
    let (head, msg) = s.split_once(" -- ").unwrap_or(("", s));
    let (time, lvl) = head.rsplit_once(':').unwrap_or(("", head));
    let level = match lvl {
        "ERROR" => Level::Error,
        "WARN" => Level::Warn,
        "INFO" => Level::Info,
        _ => Level::Other,
    };
    LogLine { time: time.to_string(), level, msg: msg.to_string() }
}

// ── 外壳需要的信息（由 main.rs 每帧组装）─────────────────────────────

/// 环境（UPDS V7 §57）：标题栏 / 状态栏的环境徽标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Env {
    /// 只看图 / 实时引擎（不交易）
    View,
    Paper,
    Backtest,
    Replay,
    /// 回放工作区但没有活动运行
    Idle,
}

impl Env {
    /// 从性质徽标推出环境（词汇与性质徽标一致，只是位置提升到全局）。
    pub fn from_badge(label: &str) -> Self {
        if label.starts_with("模拟盘") {
            Self::Paper
        } else if label.starts_with("回测") {
            Self::Backtest
        } else if label.starts_with("回放") {
            Self::Replay
        } else if label.starts_with("等待") {
            Self::Idle
        } else {
            Self::View
        }
    }

    fn color(self) -> iced::Color {
        let d = super::domain();
        color(match self {
            Self::View => d.env_idle,
            Self::Paper => d.env_paper,
            Self::Backtest => d.env_backtest,
            Self::Replay => d.env_replay,
            Self::Idle => d.env_idle,
        })
    }
}

pub struct Info {
    pub workspace: String,
    pub env: Env,
    pub env_label: String,
    pub env_detail: String,
    /// 活动运行一行（没有就空）
    pub run: String,
    /// 交易所行情订阅开着吗（网络出口页的开关）
    pub streams_on: bool,
    /// 整机网速（下行, 上行）字节/秒
    pub wire: Option<(f64, f64)>,
    /// 本项目对外连接条数
    pub egress_conns: u32,
    /// 当前聚焦面板的名字
    pub focused: Option<String>,
    pub pane_count: usize,
    /// 运行中的后台活动（每条一行）
    pub activity: Vec<String>,
}

// ── 视图 ─────────────────────────────────────────────────────────────

fn chip<'a>(label: String, fg: iced::Color, tip: String) -> Element<'a, ShellEvent> {
    let body = container(t::label(label).color(fg))
        .padding([1, 6])
        .style(move |_| container::Style {
            border: Border { width: 1.0, color: iced::Color { a: 0.7, ..fg }, radius: radius::SM.into() },
            background: Some(Background::Color(iced::Color { a: 0.12, ..fg })),
            ..Default::default()
        });
    iced::widget::tooltip(body, container(t::caption(tip)).padding(8).style(crate::style::tooltip), iced::widget::tooltip::Position::Bottom)
        .into()
}

fn bar_style(_: &iced::Theme) -> container::Style {
    let c = core();
    container::Style {
        background: Some(Background::Color(color(c.surface_base))),
        text_color: Some(color(c.text_secondary)),
        ..Default::default()
    }
}

/// 命令栏（UPDS V2 §9）：工作区名、环境徽标、活动运行、命令面板入口。
pub fn command_bar<'a>(info: &Info) -> Element<'a, ShellEvent> {
    let c = core();
    let search = button(
        row![
            t::label("⌕  搜索或运行命令").color(color(c.text_tertiary)),
            space::horizontal(),
            t::metadata("Ctrl K"),
        ]
        .align_y(Alignment::Center)
        .width(Length::Fixed(300.0)),
    )
    .padding([3, 8])
    .on_press(ShellEvent::Run(Cmd::TogglePalette))
    .style(|th, st| crate::style::button::modifier(th, st, false));

    let mut left = row![
        t::section(info.workspace.clone()).color(color(c.text_primary)),
        chip(info.env_label.clone(), info.env.color(), info.env_detail.clone()),
    ]
    .spacing(metrics::space(4))
    .align_y(Alignment::Center);
    if !info.run.is_empty() {
        left = left.push(t::caption(info.run.clone()));
    }

    container(
        row![left, space::horizontal(), search, toggle_btn("检查器", "Ctrl I", Cmd::ToggleInspector), toggle_btn("底部面板", "Ctrl J", Cmd::ToggleBottom)]
            .spacing(metrics::space(3))
            .align_y(Alignment::Center),
    )
    .height(Length::Fixed(metrics::dens().toolbar))
    .padding(Padding::from([0.0, metrics::space(4)]))
    .align_y(Alignment::Center)
    .style(bar_style)
    .into()
}

fn toggle_btn<'a>(label: &'static str, key: &'static str, cmd: Cmd) -> Element<'a, ShellEvent> {
    iced::widget::tooltip(
        button(t::label(label)).padding([3, 8]).on_press(ShellEvent::Run(cmd)).style(|th, st| crate::style::button::transparent(th, st, false)),
        container(t::caption(key)).padding(6).style(crate::style::tooltip),
        iced::widget::tooltip::Position::Bottom,
    )
    .into()
}

fn human_rate(b: f64) -> String {
    const U: [&str; 4] = ["B", "K", "M", "G"];
    let mut x = b.max(0.0);
    let mut i = 0;
    while x >= 1024.0 && i < U.len() - 1 {
        x /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{x:.0}{}/s", U[i]) } else { format!("{x:.1}{}/s", U[i]) }
}

/// 状态栏（UPDS V2 §9）：「环境真相」常驻处。每一项都是文字 + 符号，不只靠颜色。
pub fn status_bar<'a>(info: &Info) -> Element<'a, ShellEvent> {
    let c = core();
    let sep = || t::metadata("│");
    let dot = |ok: bool| if ok { "●" } else { "○" };
    let env = row![text("■").size(10).color(info.env.color()), t::metadata(info.env_label.clone())]
        .spacing(4)
        .align_y(Alignment::Center);
    let net = match info.wire {
        Some((d, u)) => format!("对外 {} 条 · ↓{} ↑{}", info.egress_conns, human_rate(d), human_rate(u)),
        None => format!("对外 {} 条 · 测量中", info.egress_conns),
    };
    let streams = format!("{} 行情订阅{}", dot(info.streams_on), if info.streams_on { "开" } else { "关" });
    let act = if info.activity.is_empty() { "无后台活动".to_string() } else { format!("活动 {}", info.activity.len()) };
    let focus = match &info.focused {
        Some(n) => format!("面板 {n} · 共 {}", info.pane_count),
        None => format!("面板 {} 个", info.pane_count),
    };
    let look = format!("{} · {}", super::theme_id().label(), super::density().label());
    let utc = chrono::Utc::now().format("UTC %H:%M:%S").to_string();

    container(
        row![
            env,
            sep(),
            t::metadata(net),
            sep(),
            t::metadata(streams),
            sep(),
            button(t::metadata(act)).padding(0).style(|th, st| crate::style::button::transparent(th, st, false)).on_press(ShellEvent::Run(Cmd::BottomTab(BottomTab::Activity))),
            space::horizontal(),
            t::metadata(focus),
            sep(),
            t::metadata(look),
            sep(),
            t::metadata(utc).color(color(c.text_secondary)),
        ]
        .spacing(metrics::space(3))
        .align_y(Alignment::Center),
    )
    .height(Length::Fixed(24.0))
    .padding(Padding::from([0.0, metrics::space(4)]))
    .align_y(Alignment::Center)
    .style(bar_style)
    .into()
}

/// 底部面板（UPDS V2 §9）：日志 / 问题 / 活动。
pub fn bottom_panel<'a>(shell: &'a Shell, info: &Info) -> Element<'a, ShellEvent> {
    let c = core();
    let n_prob = shell.log.problems().count();
    let tab = |tb: BottomTab| {
        let active = shell.bottom_tab == tb;
        let label = match tb {
            BottomTab::Problems if n_prob > 0 => format!("{} {n_prob}", tb.label()),
            _ => tb.label().to_string(),
        };
        button(t::label(label))
            .padding([2, 10])
            .on_press(ShellEvent::Run(Cmd::BottomTab(tb)))
            .style(move |th, st| crate::style::button::transparent(th, st, active))
    };
    let header = row![
        tab(BottomTab::Log),
        tab(BottomTab::Problems),
        tab(BottomTab::Activity),
        space::horizontal(),
        button(t::label("✕")).padding([2, 8]).on_press(ShellEvent::Run(Cmd::ToggleBottom)).style(|th, st| crate::style::button::transparent(th, st, false)),
    ]
    .spacing(2)
    .align_y(Alignment::Center);

    let line = |l: &LogLine| -> Element<'a, ShellEvent> {
        let fg = match l.level {
            Level::Error => color(c.status_danger),
            Level::Warn => color(c.status_warning),
            _ => color(c.text_secondary),
        };
        let tag = match l.level {
            Level::Error => "错误",
            Level::Warn => "警告",
            Level::Info => "信息",
            Level::Other => "",
        };
        row![
            container(t::code(l.time.clone()).color(color(c.text_tertiary))).width(Length::Fixed(96.0)),
            container(t::code(tag).color(fg)).width(Length::Fixed(40.0)),
            t::code(l.msg.clone()).color(if l.level == Level::Info { color(c.text_primary) } else { fg }),
        ]
        .spacing(metrics::space(3))
        .into()
    };

    let body: Element<'a, ShellEvent> = match shell.bottom_tab {
        BottomTab::Log => {
            let mut col = column![].spacing(1);
            for l in shell.log.lines.iter() {
                col = col.push(line(l));
            }
            scrollable(col).anchor_bottom().height(Length::Fill).into()
        }
        BottomTab::Problems => {
            if n_prob == 0 {
                empty("没有问题", "日志里的警告与错误会列在这里。数据体检判 C、守护起不来、订阅断开都会先出现在这一页。")
            } else {
                let mut col = column![].spacing(1);
                for l in shell.log.problems() {
                    col = col.push(line(l));
                }
                scrollable(col).anchor_bottom().height(Length::Fill).into()
            }
        }
        BottomTab::Activity => {
            if info.activity.is_empty() {
                empty("没有后台活动", "在回测结果面板顶上「发起回测」、或在订单流特征面板开始回放后，运行中的任务会列在这里。")
            } else {
                let mut col = column![].spacing(metrics::space(2));
                for a in &info.activity {
                    col = col.push(t::body(a.clone()));
                }
                col.into()
            }
        }
    };

    container(column![header, container(body).padding([4, 8]).height(Length::Fill)].spacing(2))
        .height(Length::Fixed(200.0))
        .width(Length::Fill)
        .style(|_| {
            let c = core();
            container::Style {
                background: Some(Background::Color(color(c.surface_primary))),
                border: Border { width: 1.0, color: color(c.border_default), radius: 0.0.into() },
                ..Default::default()
            }
        })
        .into()
}

/// 空态（UPDS V3 §16）：说这里是干什么的、下一步做什么；不写「无数据」。
pub fn empty<'a, M: 'a>(title: &'a str, hint: &'a str) -> Element<'a, M> {
    container(column![t::label(title), t::caption(hint)].spacing(4).max_width(520))
        .padding(12)
        .into()
}

/// 检查器（UPDS V2 §9）：当前选中对象的属性。批 3 先放聚焦面板与数据环境，
/// 面板里的「编辑口径」「数据源选择」等可编辑属性在批 5–7 迁进来。
pub fn inspector<'a>(info: &Info) -> Element<'a, ShellEvent> {
    let c = core();
    let section = |title: &'a str| t::metadata(title);
    let kv = |k: &'a str, v: String| -> Element<'a, ShellEvent> {
        row![container(t::caption(k)).width(Length::Fixed(72.0)), t::body(v)].spacing(8).into()
    };
    let mut col = column![
        row![t::section("检查器"), space::horizontal(), button(t::label("✕")).padding([2, 8]).on_press(ShellEvent::Run(Cmd::ToggleInspector)).style(|th, st| crate::style::button::transparent(th, st, false))].align_y(Alignment::Center),
        section("当前面板"),
    ]
    .spacing(metrics::space(3));
    match &info.focused {
        Some(n) => col = col.push(kv("面板", n.clone())),
        None => col = col.push(empty("没有聚焦的面板", "点一下面板或按 F6 聚焦，这里会显示它的属性。")),
    }
    col = col
        .push(kv("工作区", info.workspace.clone()))
        .push(section("数据环境"))
        .push(kv("性质", info.env_label.clone()))
        .push(t::caption(info.env_detail.clone()));
    if !info.run.is_empty() {
        col = col.push(kv("运行", info.run.clone()));
    }
    container(scrollable(col.padding(Padding::from([metrics::space(4), metrics::space(4)]))))
        .width(Length::Fixed(300.0))
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: Some(Background::Color(color(c.surface_primary))),
            border: Border { width: 1.0, color: color(c.border_default), radius: 0.0.into() },
            text_color: Some(color(c.text_primary)),
            ..Default::default()
        })
        .into()
}

/// 命令面板浮层（UPDS V2 §12）：e3、居中偏上、输入即过滤，↑↓ 选、↵ 运行、Esc 关。
///
/// 返回的是**只有浮层**的一层（铺满全窗口的遮罩 + 面板），调用方把它叠在界面上。
/// 注意 iced 的 `stack` 以第一层的尺寸为准：第一层必须是铺满的遮罩，
/// 垫一个空白在最底下会把整层压成 0 高（批 3 踩过，命令面板整个不显示）。
pub fn palette_overlay<'a>(
    p: &'a Palette,
    entries: &'a [Entry],
) -> Element<'a, ShellEvent> {
    let c = core();
    let hits = command::filter(entries, &p.query);
    let input = text_input("输入命令、工作区或设置……", &p.query)
        .id(palette_input_id())
        .on_input(ShellEvent::PaletteQuery)
        .on_submit(ShellEvent::PaletteSubmit)
        .padding(8)
        .size(t::size(super::Role::Label) + 1.0);

    let mut list = column![].spacing(1);
    // 选中项保持在可见窗口里：最多列 14 条，窗口跟着选中项滑动
    let win = 14usize;
    let start = p.sel.saturating_sub(win - 1);
    for (i, en) in hits.iter().enumerate().skip(start).take(win) {
        let selected = i == p.sel;
        let item = row![
            container(t::metadata(en.category)).width(Length::Fixed(56.0)),
            t::label(en.title.clone()),
            space::horizontal(),
            t::metadata(en.shortcut),
        ]
        .align_y(Alignment::Center)
        .spacing(8);
        list = list.push(
            button(item)
                .width(Length::Fill)
                .padding([4, 10])
                .on_press(ShellEvent::Run(en.cmd.clone()))
                .style(move |th, st| crate::style::button::transparent(th, st, selected)),
        );
    }
    let footer = row![
        t::metadata("↑↓ 选择 · ↵ 运行 · Esc 关闭"),
        space::horizontal(),
        t::metadata(format!("{} 条", hits.len())),
    ];
    let body = if hits.is_empty() {
        column![input, empty("没有匹配的命令", "换个词试试：工作区名、「主题」「密度」「面板」……")]
    } else {
        column![input, list, footer]
    }
    .spacing(6)
    .padding(8);

    let panel = container(body).width(Length::Fixed(620.0)).style(move |_| container::Style {
        background: Some(Background::Color(color(c.surface_elevated))),
        border: metrics::hairline(radius::LG),
        shadow: metrics::shadow(Elevation::E3),
        text_color: Some(color(c.text_primary)),
        ..Default::default()
    });

    let scrim = mouse_area(container(space::horizontal()).width(Length::Fill).height(Length::Fill).style(move |_| container::Style {
        background: Some(Background::Color(color(c.state_scrim))),
        ..Default::default()
    }))
    .on_press(ShellEvent::PaletteClose);

    stack![
        scrim,
        container(opaque(panel)).width(Length::Fill).align_x(Alignment::Center).padding(Padding { top: 72.0, ..Padding::ZERO }),
    ]
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 日志行解析() {
        let l = parse_line("14:18:46.400:WARN -- 拉起后台守护失败");
        assert_eq!(l.time, "14:18:46.400");
        assert_eq!(l.level, Level::Warn);
        assert_eq!(l.msg, "拉起后台守护失败");
        assert_eq!(parse_line("随便一行").level, Level::Other);
    }

    #[test]
    fn 环境来自性质徽标() {
        assert_eq!(Env::from_badge("回测 A"), Env::Backtest);
        assert_eq!(Env::from_badge("模拟盘"), Env::Paper);
        assert_eq!(Env::from_badge("回放·引擎同源"), Env::Replay);
        assert_eq!(Env::from_badge("实时·仅看图"), Env::View);
        assert_eq!(Env::from_badge("等待运行"), Env::Idle);
    }

    #[test]
    fn 命令面板_上下移动循环_回车取选中项() {
        let entries = command::registry(&["回测", "订单流特征"]);
        let mut s = Shell { palette: Some(Palette::default()), ..Default::default() };
        s.update(ShellEvent::PaletteMove(-1), &entries);
        assert_eq!(s.palette.as_ref().unwrap().sel, entries.len() - 1, "从第一条往上回到最后");
        s.update(ShellEvent::PaletteQuery("订单流".into()), &entries);
        let picked = s.update(ShellEvent::PaletteSubmit, &entries);
        assert_eq!(picked, Some(Cmd::Workspace("订单流特征".into())));
        assert!(s.palette.is_none(), "运行后关闭");
    }
}
