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
    Space, button, column, container, mouse_area, opaque, row, scrollable, space, stack, text, text_input,
};
use iced::{Alignment, Background, Border, Element, Length, Padding};

use super::command::{self, Cmd, Entry};
use super::metrics::{self, Elevation, radius};
use super::{color, core, text as t};

// ── 工作区分组（docs/35 §5.3）────────────────────────────────────────

/// 侧栏的六组（docs/41 §2）。组内第一个工作区有 Ctrl Shift n 直达。
pub fn groups() -> [(&'static str, &'static [&'static str]); 6] {
    crate::ws::workspace::GROUPS
}

/// 某工作区若是组首，给出它的直达快捷键文字。
pub fn group_shortcut(name: &str) -> Option<&'static str> {
    const KEYS: [&str; 6] = ["Ctrl Shift 1", "Ctrl Shift 2", "Ctrl Shift 3", "Ctrl Shift 4", "Ctrl Shift 5", "Ctrl Shift 6"];
    groups().iter().position(|(_, ws)| ws.first() == Some(&name)).map(|i| KEYS[i])
}

// ── 页签栏（docs/41 §3）──────────────────────────────────────────────

/// 页签栏上的一页。
pub struct PageTab<V> {
    pub label: String,
    /// 状态角标（`page_status::Badge::glyph`）：● 后台在跑、⚠ 有问题
    pub badge: Option<&'static str>,
    pub value: V,
}

/// 工作区的页签栏：只有页名（工作区名在侧栏与标题栏上已有，不重复，docs/41）；激活页下缘强调色；右端是切页快捷键提示。
/// 页签与 [`super::widgets::tabs`] 同一套样式（只有一层页签，docs/41 §1）。只有一页的工作区不显示页签栏（调用方判断）。
pub fn page_bar<'a, M: Clone + 'a, V: PartialEq + Clone + 'a>(
    pages: Vec<PageTab<V>>,
    active: &V,
    on: impl Fn(V) -> M + 'a,
    extra: Vec<Element<'a, M>>,
) -> Element<'a, M> {
    use super::widgets::{Kind, Tone, button_style};
    let c = core();
    let mut r = row![].spacing(0).align_y(Alignment::End);
    let single = pages.len() <= 1;
    for p in pages {
        let is = p.value == *active;
        let fg = if is { color(c.text_primary) } else { color(c.text_secondary) };
        let under = container(Space::new().width(Length::Fill).height(Length::Fixed(2.0))).style(move |_| container::Style {
            background: is.then(|| Background::Color(color(core().accent_primary))),
            ..Default::default()
        });
        let mut label = row![t::label(p.label).color(fg)].spacing(metrics::space(1)).align_y(Alignment::Center);
        if let Some(g) = p.badge {
            let tone = if g == "⚠" { Tone::Warning } else { Tone::Info };
            label = label.push(t::metadata(g).color(tone.color()));
        }
        let cell = column![container(label).padding(Padding::from([metrics::space(2), metrics::space(4)])), under].width(Length::Shrink);
        r = r.push(button(cell).padding(0).on_press(on(p.value.clone())).style(|th, st| button_style(Kind::Ghost, th, st)));
    }
    for e in extra {
        r = r.push(container(e).padding(Padding::from([metrics::space(1), metrics::space(1)])));
    }
    r = r.push(Space::new().width(Length::Fill));
    if !single {
        r = r.push(
            container(t::metadata("Alt 1–9 · Ctrl PgUp / PgDn").color(color(c.text_tertiary)))
                .padding(Padding::from([metrics::space(2), metrics::space(3)])),
        );
    }
    container(r)
        .width(Length::Fill)
        .style(move |_| container::Style {
            border: Border { width: 0.0, ..Default::default() },
            ..Default::default()
        })
        .into()
}

// ── 状态 ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BottomTab {
    #[default]
    Log,
    Problems,
    Activity,
    /// 告警（docs/35 §5.1，UPDS V6 §49）：新闻守护的告警规则命中记录
    Alerts,
    /// 通知中心（docs/35 §6.1）：本次运行弹过的全部提示
    Notices,
}

impl BottomTab {
    fn label(self) -> &'static str {
        match self {
            Self::Log => "日志",
            Self::Problems => "问题",
            Self::Activity => "活动",
            Self::Alerts => "告警",
            Self::Notices => "通知",
        }
    }
}

/// 一条告警命中（来自新闻守护快照的 `alerts`，新的在前）。
#[derive(Debug, Clone)]
pub struct Alert {
    pub ts_ms: i64,
    pub time: String,
    pub rule: String,
    pub source: String,
    pub title: String,
    pub url: String,
}

/// 告警记录：底部面板开着时到点检查快照，文件变了才重读。
#[derive(Debug, Clone, Default)]
pub struct AlertTail {
    pub items: Vec<Alert>,
    read_at: Option<Instant>,
    mtime: Option<std::time::SystemTime>,
    /// 看过的最新一条（毫秒）。`None` = 还没读过快照：第一次读到时把已有的都算看过，
    /// 状态栏只数本次运行里新来的，不把历史告警当成新的
    seen_ms: Option<i64>,
}

impl AlertTail {
    pub fn refresh(&mut self) {
        if self.read_at.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
            return;
        }
        self.read_at = Some(Instant::now());
        let p = crate::ws::paths::runtime_dir().join("news_board.json");
        let mt = std::fs::metadata(&p).ok().and_then(|m| m.modified().ok());
        if mt.is_some() && mt == self.mtime {
            return;
        }
        self.mtime = mt;
        let Ok(t) = std::fs::read_to_string(&p) else { return };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) else { return };
        self.items = parse_alerts(&v);
        if self.seen_ms.is_none() {
            self.mark_seen();
        }
    }

    fn newest_ms(&self) -> i64 {
        self.items.iter().map(|a| a.ts_ms).max().unwrap_or(0)
    }

    /// 打开「告警」页时调：到目前为止的都算看过。
    pub fn mark_seen(&mut self) {
        self.seen_ms = Some(self.newest_ms());
    }

    /// 上次看过之后新来的条数（状态栏用）。
    pub fn unseen(&self) -> usize {
        let seen = self.seen_ms.unwrap_or(i64::MAX);
        self.items.iter().filter(|a| a.ts_ms > seen).count()
    }
}

fn parse_alerts(v: &serde_json::Value) -> Vec<Alert> {
    let s = |a: &serde_json::Value, k: &str| a.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
    v.get("alerts")
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .map(|a| Alert {
                    ts_ms: a.get("ts_ms").and_then(serde_json::Value::as_i64).unwrap_or(0),
                    time: a
                        .get("ts_ms")
                        .and_then(serde_json::Value::as_i64)
                        .and_then(chrono::DateTime::from_timestamp_millis)
                        .map(|t| t.with_timezone(&chrono::Local).format("%m-%d %H:%M:%S").to_string())
                        .unwrap_or_default(),
                    rule: s(a, "rule"),
                    source: s(a, "source"),
                    title: s(a, "title"),
                    url: s(a, "url"),
                })
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone, Default)]
pub struct Palette {
    pub query: String,
    pub sel: usize,
}

#[derive(Debug, Clone)]
pub enum ShellEvent {
    Run(Cmd),
    /// 检查器（docs/42 第 2 期）：切页 / 钉住 / 关掉 / 拖宽 / 双击复位
    InspTab(InspTab),
    InspPin,
    InspClose,
    InspDragStart,
    InspDragMove(f32),
    InspDragEnd,
    InspResetWidth,
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
    pub alerts: AlertTail,
    /// 通知中心的历史（时刻, 标题, 正文, 级别），新的在后；底部面板开在「通知」页时由 main 刷新
    pub notices: Vec<(String, String, String, Level)>,
    /// 侧栏收起（Ctrl B）
    pub sidebar_hidden: bool,
    /// 检查器当前页、宽度、是否在拖宽；`inspector` 字段 = 钉住（常开）
    pub insp_tab: InspTab,
    pub insp_width: f32,
    pub insp_drag: bool,
    /// 拖宽的起点（第一次移动时记下：光标 x、当时的宽度），之后按位移改宽，按下的位置不准也不跳
    insp_anchor: Option<(f32, f32)>,
    /// 上次按下拖动条的时刻（两次按下 0.4 秒内 = 双击复位）
    insp_last_press: Option<Instant>,
    /// 自动展开时用户点了 ✕：对这个面板不再自动展开，换了面板再说
    pub insp_dismissed: Option<String>,
    /// 主窗口宽（拖宽时算位置与上限，由 main 写入）
    pub window_w: f32,
}

/// 检查器的三页（docs/42 §3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InspTab {
    #[default]
    Props,
    Data,
    About,
}

impl Default for Shell {
    fn default() -> Self {
        Self {
            palette: None,
            bottom: false,
            bottom_tab: BottomTab::Log,
            inspector: false,
            log: LogTail::default(),
            alerts: AlertTail::default(),
            notices: Vec::new(),
            sidebar_hidden: false,
            insp_tab: InspTab::Props,
            insp_width: crate::ws::shell_prefs::INSPECTOR_DEFAULT,
            insp_drag: false,
            insp_anchor: None,
            insp_last_press: None,
            insp_dismissed: None,
            window_w: 1920.0,
        }
    }
}

/// 命令面板输入框的 id（打开时把焦点给它）。
pub fn palette_input_id() -> iced::widget::Id {
    iced::widget::Id::new("ws-command-palette")
}

impl Shell {
    fn save_insp(&self) {
        crate::ws::shell_prefs::save(&crate::ws::shell_prefs::ShellPrefs {
            inspector_width: self.insp_width,
            inspector_pinned: self.inspector,
        });
    }

    /// 读回上次的检查器宽度与钉住状态（启动时调）。
    pub fn load_insp(&mut self) {
        let p = crate::ws::shell_prefs::load();
        self.insp_width = p.inspector_width;
        self.inspector = p.inspector_pinned;
    }

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
            ShellEvent::InspTab(t) => self.insp_tab = t,
            ShellEvent::InspPin => {
                self.inspector = !self.inspector;
                self.save_insp();
            }
            ShellEvent::InspClose => {
                // 钉住时 ✕ = 取消钉住；自动展开时 ✕ = 这个面板先别自动开（由 main 记下是哪个面板）
                if self.inspector {
                    self.inspector = false;
                    self.save_insp();
                }
                return Some(Cmd::InspectorDismiss);
            }
            ShellEvent::InspDragStart => {
                // 双击（两次按下间隔 0.4 秒内）= 复位到缺省宽度。不能靠 mouse_area 的双击：第一下按下
                // 就开始拖、整窗盖上接鼠标的层，第二下落在那层上，拖动条收不到双击
                let now = Instant::now();
                if self.insp_last_press.is_some_and(|t| now.duration_since(t) < Duration::from_millis(400)) {
                    self.insp_last_press = None;
                    self.insp_drag = false;
                    self.insp_anchor = None;
                    self.insp_width = crate::ws::shell_prefs::INSPECTOR_DEFAULT;
                    self.save_insp();
                } else {
                    self.insp_last_press = Some(now);
                    self.insp_drag = true;
                    self.insp_anchor = None;
                }
            }
            ShellEvent::InspDragMove(x) => {
                if self.insp_drag {
                    match self.insp_anchor {
                        None => self.insp_anchor = Some((x, self.insp_width)),
                        // 检查器贴在右边：光标往左 = 变宽
                        Some((x0, w0)) => {
                            self.insp_width = crate::ws::shell_prefs::clamp_width(w0 + (x0 - x), self.window_w);
                        }
                    }
                }
            }
            ShellEvent::InspDragEnd => {
                if self.insp_drag {
                    self.insp_drag = false;
                    self.insp_anchor = None;
                    self.save_insp();
                }
            }
            ShellEvent::InspResetWidth => {
                self.insp_width = crate::ws::shell_prefs::INSPECTOR_DEFAULT;
                self.save_insp();
            }
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
    /// 实盘（真实下单）。目前没有实盘入口，留给接实盘时用（docs/35 §9）
    Live,
    Paper,
    Backtest,
    Replay,
    /// 回放工作区但没有活动运行
    Idle,
}

impl Env {
    /// 从性质徽标推出环境（词汇与性质徽标一致，只是位置提升到全局）。
    pub fn from_badge(label: &str) -> Self {
        if label.starts_with("实盘") {
            Self::Live
        } else if label.starts_with("模拟盘") {
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
            Self::Live => d.env_live,
            Self::Paper => d.env_paper,
            Self::Backtest => d.env_backtest,
            Self::Replay => d.env_replay,
            Self::Idle => d.env_idle,
        })
    }
}

/// 实盘 / 模拟盘时窗口顶边的 2px 环境色条（UPDS V7 §57，docs/35 §9.1）：
/// 最大化、截图时都在，一眼分得清「这个界面会不会动真钱」。其他环境不画。
pub fn env_strip<'a, M: 'a>(env: Env) -> Option<Element<'a, M>> {
    matches!(env, Env::Live | Env::Paper).then(|| {
        let c = env.color();
        container(space::horizontal())
            .height(Length::Fixed(2.0))
            .width(Length::Fill)
            .style(move |_| container::Style { background: Some(Background::Color(c)), ..Default::default() })
            .into()
    })
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
    /// 当前数据链路（标签, 逐跳详情, 语气）；工作区里没有吃行情的面板时为空
    pub link: Option<(String, String, super::widgets::Tone)>,
    /// 连不上的行情流（交易所, 原因）；空 = 都好
    pub streams_down: Vec<(String, String)>,
    /// 上次打开「告警」页之后新命中的告警条数
    pub alerts_unseen: usize,
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
    toolbar(info, |e| e, row![].into())
}

/// 顶部工具栏（docs/42 §2）：左 = 位置与数据环境；中 = **选中面板**的上下文控件（数据源、标的、周期、主动作、联动、页面菜单，
/// 由调用方按面板的 `ToolbarSpec` 拼好）；右 = 命令搜索、检查器、底部面板。高度同原命令栏，不多占一行。
pub fn toolbar<'a, M: Clone + 'a>(info: &Info, on: fn(ShellEvent) -> M, ctx: Element<'a, M>) -> Element<'a, M> {
    let c = core();
    let search = button(
        row![
            t::label("⌕  搜索或运行命令").color(color(c.text_tertiary)),
            space::horizontal(),
            t::metadata("Ctrl K"),
        ]
        .align_y(Alignment::Center)
        .width(Length::Fixed(220.0)),
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
    // 当前数据链路：B1/B2/B3 · 数据商 · 标的 · 状态（悬停看逐跳），与面板标题上的链路徽标同源
    if let Some((label, detail, tone)) = &info.link {
        left = left.push(chip(label.clone(), tone.color(), detail.clone()));
    }
    if !info.run.is_empty() {
        left = left.push(t::caption(info.run.clone()));
    }
    // 隐藏数值模式（docs/35 §9.3）：常驻提示，截图里也看得出这是演示画面
    if super::hide_values() {
        left = left.push(chip(
            "••• 数值已隐藏".into(),
            color(c.status_warning),
            "金额、持仓、账户标识显示为 •••，通知暂停；在设置或命令面板里关掉".into(),
        ));
    }
    let left: Element<'a, ShellEvent> = left.into();
    let right: Element<'a, ShellEvent> = row![
        search,
        toggle_btn("检查器", "Ctrl I", Cmd::ToggleInspector),
        toggle_btn("底部面板", "Ctrl J", Cmd::ToggleBottom)
    ]
    .spacing(metrics::space(3))
    .align_y(Alignment::Center)
    .into();

    container(
        row![left.map(on), ctx, space::horizontal(), right.map(on)]
            .spacing(metrics::space(3))
            .align_y(Alignment::Center),
    )
    .height(Length::Fixed(metrics::dens().toolbar))
    .padding(Padding::from([0.0, metrics::space(4)]))
    .align_y(Alignment::Center)
    .style(bar_style)
    .into()
}

/// 工具栏上的一个下拉式按钮：`标签 值 ▾`（值为空时只显示标签）。
pub fn tool_pick<'a, M: Clone + 'a>(label: &str, value: String, msg: Option<M>, active: bool) -> Element<'a, M> {
    let c = core();
    let mut r = row![].spacing(metrics::space(1)).align_y(Alignment::Center);
    if !label.is_empty() {
        r = r.push(t::metadata(label.to_string()).color(color(c.text_tertiary)));
    }
    r = r.push(t::label(value).color(color(c.text_primary)));
    if msg.is_some() {
        r = r.push(t::metadata("▾").color(color(c.text_tertiary)));
    }
    let b = button(r).padding([2, 6]).style(move |th, st| crate::style::button::modifier(th, st, active));
    match msg {
        Some(m) => b.on_press(m).into(),
        None => b.into(),
    }
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
            if info.streams_on && !info.streams_down.is_empty() {
                let names: Vec<&str> = info.streams_down.iter().map(|(k, _)| k.as_str()).collect();
                let why: Vec<String> = info.streams_down.iter().map(|(k, w)| format!("{k}：{w}")).collect();
                chip(format!("▲ 连不上 {}", names.join("、")), color(c.status_warning), format!("{}\n每秒重试；检查网络 / 代理设置", why.join("\n")))
            } else {
                iced::widget::Space::new().into()
            },
            sep(),
            // 告警（附录 A V6 §49）：上次打开「告警」页之后的新命中；点它打开告警页
            if info.alerts_unseen > 0 {
                button(t::metadata(format!("▲ 告警 {} 条新", info.alerts_unseen)).color(color(c.status_warning)))
                    .padding(0)
                    .style(|th, st| crate::style::button::transparent(th, st, false))
                    .on_press(ShellEvent::Run(Cmd::BottomTab(BottomTab::Alerts)))
                    .into()
            } else {
                Element::from(iced::widget::Space::new())
            },
            button(t::metadata(act)).padding(0).style(|th, st| crate::style::button::transparent(th, st, false)).on_press(ShellEvent::Run(Cmd::BottomTab(BottomTab::Activity))),
            space::horizontal(),
            // 选中计数与合计（任一网格有选中时；合计说明缺口，docs/35 §5.1 / §7.1）
            t::metadata(super::grid::current_selection().map(|s| format!("{}  │  ", s.label())).unwrap_or_default()),
            t::metadata(focus),
            sep(),
            t::metadata(look),
            sep(),
            t::metadata(if super::hide_values() { "••• 数值已隐藏 · 通知暂停" } else { "" }).color(color(c.status_warning)),
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
            BottomTab::Alerts if !shell.alerts.items.is_empty() => format!("{} {}", tb.label(), shell.alerts.items.len()),
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
        tab(BottomTab::Alerts),
        tab(BottomTab::Notices),
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
        BottomTab::Notices => {
            if shell.notices.is_empty() {
                empty("还没有通知", "弹出过的提示都会留在这里（最多 200 条）：同时最多显示 3 条，挤掉的和自动消失的在这里都能查到。")
            } else {
                let mut col = column![].spacing(1);
                for (time, title, body, level) in shell.notices.iter().rev() {
                    let l = LogLine { time: time.clone(), level: *level, msg: if title.is_empty() { body.clone() } else { format!("{title}　{body}") } };
                    col = col.push(line(&l));
                }
                scrollable(col).height(Length::Fill).into()
            }
        }
        BottomTab::Alerts => {
            if shell.alerts.items.is_empty() {
                empty(
                    "没有告警",
                    "新闻守护的告警规则（news.toml 的 [[alert]]）命中后会列在这里，同时弹桌面通知。新闻守护没在跑时这里不会更新。",
                )
            } else {
                let mut col = column![].spacing(1);
                for a in &shell.alerts.items {
                    let row_el = row![
                        container(t::code(a.time.clone()).color(color(c.text_tertiary))).width(Length::Fixed(110.0)),
                        container(t::label(a.rule.clone()).color(color(c.status_warning))).width(Length::Fixed(110.0)),
                        container(t::caption(a.source.clone())).width(Length::Fixed(110.0)),
                        t::body(a.title.clone()),
                    ]
                    .spacing(metrics::space(3));
                    col = col.push(if a.url.is_empty() {
                        Element::from(row_el)
                    } else {
                        button(row_el)
                            .padding(0)
                            .on_press(ShellEvent::Run(Cmd::OpenUrl(a.url.clone())))
                            .style(|th, st| crate::style::button::transparent(th, st, false))
                            .into()
                    });
                }
                scrollable(col).height(Length::Fill).into()
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
///
/// `props` = 聚焦面板的可编辑属性（数据选择器、口径设置……，docs/35 §16.5 第 3 项），由调用方
/// 按面板类型拼好、消息已包成调用方自己的类型；`on` 把外壳事件包成同一类型。
/// 检查器这一帧要显示的东西（docs/42 §3）。
pub struct InspView {
    pub tab: InspTab,
    /// 已夹好的宽度（像素）
    pub width: f32,
    pub pinned: bool,
    /// 选中面板的数据源摘要（与工具栏同一句）
    pub source: Option<String>,
    /// 选中面板的说明（用途、口径、纪律提示）
    pub about: Option<String>,
    /// 检查器作用的面板名（与工具栏同一个目标；没选中时是本页第一个有控件的面板）
    pub panel: Option<String>,
    /// 与缺省值不同的设置（「已改 n 项」，docs/42 §5.2）
    pub changed: Vec<String>,
}

/// 检查器（UPDS V2 §9，docs/42 §3）：三页——**属性**（选中面板的可编辑设置）、**数据**（数据源与数据环境）、
/// **说明**（面板用途与口径）。左边缘是拖动条：按住拖宽、双击复位；标题栏 📌 钉住常开、✕ 关。
pub fn inspector<'a, M: Clone + 'a>(
    info: &Info,
    v: &InspView,
    props: Option<Element<'a, M>>,
    data: Option<Element<'a, M>>,
    on: fn(ShellEvent) -> M,
) -> Element<'a, M> {
    use super::widgets::{Kind, btn};
    let c = core();
    let section = |title: &'a str| t::metadata(title);
    let kv = |k: &'a str, val: String| -> Element<'a, M> {
        row![container(t::caption(k)).width(Length::Fixed(72.0)), t::body(val)].spacing(8).into()
    };
    let has_props = props.is_some();
    let has_data = data.is_some();
    // 只有数据、没有属性的面板（如 Tardis 历史面板）：停在「属性」页时直接给「数据」页，不让用户先看一句空话
    let tab = if v.tab == InspTab::Props && !has_props && has_data { InspTab::Data } else { v.tab };
    let head = row![
        t::section("检查器"),
        space::horizontal(),
        btn(if v.pinned { "📌 已钉住" } else { "📌 钉住" }, if v.pinned { Kind::Standard } else { Kind::Ghost }, Some(on(ShellEvent::InspPin))),
        button(t::label("✕")).padding([2, 8]).on_press(on(ShellEvent::InspClose)).style(|th, st| crate::style::button::transparent(th, st, false)),
    ]
    .spacing(metrics::space(1))
    .align_y(Alignment::Center);
    let tabs = super::widgets::tabs(
        &[
            (
                if !v.changed.is_empty() {
                    "属性 · 已改"
                } else if has_props {
                    "属性 •"
                } else {
                    "属性"
                },
                InspTab::Props,
            ),
            (if has_data { "数据 •" } else { "数据" }, InspTab::Data),
            ("说明", InspTab::About),
        ],
        &tab,
        move |t| on(ShellEvent::InspTab(t)),
    );
    let mut col = column![head, tabs].spacing(metrics::space(3));
    let panel = match v.panel.as_ref().or(info.focused.as_ref()) {
        Some(n) => kv("面板", n.clone()),
        None => empty("没有选中的面板", "点一下面板（或按 F6），这里显示它的设置、数据与说明。"),
    };
    match tab {
        InspTab::Props => {
            col = col.push(panel);
            if !v.changed.is_empty() {
                // 「为什么我看到的和别人不一样」：改过的设置逐项列出
                col = col.push(
                    column![
                        t::label(format!("已改 {} 项", v.changed.len())).color(color(c.accent_primary)),
                        t::caption(v.changed.join(" · ")),
                    ]
                    .spacing(metrics::space(1)),
                );
            }
            match props {
                Some(p) => col = col.push(p),
                None => {
                    col = col.push(
                        t::caption(if has_data {
                            "这个面板的设置都在「数据」页（选数据）与顶部工具栏。"
                        } else {
                            "这个面板没有可在这里改的设置（标的、周期等在顶部工具栏）。"
                        })
                        .color(color(c.text_tertiary)),
                    )
                }
            }
        }
        InspTab::Data => {
            col = col.push(panel).push(kv("工作区", info.workspace.clone()));
            if let Some(src) = &v.source {
                col = col.push(section("数据源")).push(t::body(src.clone()));
            }
            // 选中面板自己的数据选择（docs/42 第 3 期：原来在面板顶上的选择器搬到这里）
            if let Some(d) = data {
                col = col.push(section("选择数据")).push(d);
            }
            if let Some((label, detail, _)) = &info.link {
                col = col.push(section("数据链路")).push(t::body(label.clone())).push(t::caption(detail.clone()));
            }
            col = col
                .push(section("数据环境"))
                .push(kv("性质", info.env_label.clone()))
                .push(t::caption(info.env_detail.clone()));
            if !info.run.is_empty() {
                col = col.push(kv("运行", info.run.clone()));
            }
        }
        InspTab::About => {
            col = col.push(panel);
            match &v.about {
                Some(a) => {
                    for para in a.split("\n\n") {
                        col = col.push(t::body(para.to_string()));
                    }
                }
                None => col = col.push(t::caption("这个面板还没有写说明。").color(color(c.text_tertiary))),
            }
        }
    }
    // 左边缘拖动条：按住拖宽（整窗接住鼠标由 main 负责），双击复位到缺省宽度
    let handle = iced::widget::mouse_area(
        container(Space::new().width(Length::Fixed(5.0)).height(Length::Fill)).style(move |_| container::Style {
            background: Some(Background::Color(color(c.border_default))),
            ..Default::default()
        }),
    )
    .on_press(on(ShellEvent::InspDragStart))
    .on_double_click(on(ShellEvent::InspResetWidth))
    .interaction(iced::mouse::Interaction::ResizingHorizontally);
    let body = container(scrollable(col.padding(Padding::from([metrics::space(4), metrics::space(4)]))))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: Some(Background::Color(color(c.surface_primary))),
            border: Border { width: 1.0, color: color(c.border_default), radius: 0.0.into() },
            text_color: Some(color(c.text_primary)),
            ..Default::default()
        });
    row![handle, body].width(Length::Fixed(v.width)).height(Length::Fill).into()
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
    let t0 = std::time::Instant::now();
    let hits = command::filter(entries, &p.query);
    super::perf::palette_filtered(t0.elapsed());
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
        assert_eq!(Env::from_badge("实盘 · Binance"), Env::Live);
        assert!(env_strip::<()>(Env::Live).is_some() && env_strip::<()>(Env::Paper).is_some());
        assert!(env_strip::<()>(Env::Backtest).is_none() && env_strip::<()>(Env::View).is_none(), "只有实盘 / 模拟盘画色条");
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
