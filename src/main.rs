#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod chart;
mod connector;
mod layout;
mod logger;
mod modal;
mod notify;
mod screen;
mod style;
// UPDS 适配层（docs/35）：设计 token → iced。适配层先于调用点落地（批 2），
// 度量与领域色的取值函数在批 3–7 迁移面板时才陆续用上
#[allow(dead_code)]
mod ui;
mod version;
mod widget;
mod window;
mod ws; // WealthSpring 集成（docs/08）：Redis 控制面 + 回测行情流

use data::{layout::WindowSpec, sidebar};
use layout::{LayoutId, configuration};
use modal::{
    LayoutManager, ThemeEditor,
    audio::AudioStream,
    network_manager::{self, NetworkManager},
};
use modal::{dashboard_modal, main_dialog_modal};
use notify::Notifications;
use screen::dashboard::{self, Dashboard};
use widget::{
    confirm_dialog_container,
    toast::{self, Toast},
    tooltip,
};

use iced::{
    Alignment, Element, Subscription, Task, keyboard, padding,
    widget::{
        button, column, container, pane_grid, pick_list, row, rule, scrollable, text,
        tooltip::Position as TooltipPosition,
    },
};
use std::{borrow::Cow, collections::HashMap, vec};

fn main() {
    ui::perf::mark_start();
    logger::install_panic_hook();

    if let Err(err) = logger::setup(cfg!(debug_assertions)) {
        logger::report_stderr(&format!("Failed to initialize logger: {err}"));
    }

    std::thread::spawn(data::cleanup_old_market_data);

    // 后台守护跟随本进程的生命周期（docs/26 S4b，用户要求）：
    // 启动即拉起全部守护，退出即停掉。用 `--no-block`，不让 GUI 卡在
    // 启动画面上等十一个服务依次起来——各面板本来就能处理「守护还没起」。
    // 崩溃时也停掉守护。只覆盖 panic——SIGKILL 那一档要靠
    // `systemctl --user start ws-cockpit`（那时 BindsTo 接管）。
    // 样张模式（docs/35）是日常 Cockpit 之外的第二个实例：**不碰守护**——
    // 否则它退出时 stop_all 会把日常那个 Cockpit 的全部守护一起停掉。
    let specimen = ws::specimen::enabled();
    if !specimen {
        ws::lifecycle::install_panic_hook();
        if let Err(e) = ws::lifecycle::start_all() {
            log::warn!("拉起后台守护失败（{}）：{e}", ws::lifecycle::TARGET);
        }
    }

    // 网络出口的轮询**在这里起**，不是等谁打开那一页。
    // 「今日用量」要连续才有意义——挂在渲染上的话，那个数的真实含义
    // 会变成「你盯着那一页看的时候我看到了多少」。没人看时 20 秒一轮
    ws::egress::start();
    // 「启动时对外连接开/关」（网络出口页上的设置）。**必须在 iced 起来之前**：
    // 设成关时行情订阅要在第一帧建连前就关掉，否则是先连上再断开
    if !specimen {
        ws::egress::apply_startup();
    }

    log::info!("[ui] 字体：{}；偏好 {}", ui::text::describe_fonts(), ui::tokens::Prefs::path().display());

    let daemon = iced::daemon(Flowsurface::new, Flowsurface::update, Flowsurface::view)
        .settings(iced::Settings {
            antialiasing: true,
            fonts: vec![
                Cow::Borrowed(style::AZERET_MONO_BYTES),
                Cow::Borrowed(style::ICONS_BYTES),
            ],
            default_text_size: style::text_size::BODY.into(),
            // 界面字体带中文（docs/35 §4.3）：此前中文全靠系统回退，字重与度量和拉丁字母对不上
            default_font: ui::text::ui_font(),
            ..Default::default()
        })
        .title(Flowsurface::title)
        .theme(Flowsurface::theme)
        .scale_factor(Flowsurface::scale_factor)
        .subscription(Flowsurface::subscription);

    if let Err(err) = daemon.run() {
        let message = format!("Runtime error: {err}");
        log::error!("{message}");
        logger::report_stderr(&message);
    }
}

/// 窄窗口断点（UPDS V1 §08，docs/35 附录 A）：窄于它时检查器不再占宽度，改为盖在工作区右侧的抽屉。
/// 侧栏本来就是 44px 的图标条，不必再收。
const NARROW_PX: f32 = 900.0;

/// 全局快捷键（UPDS V9 §77 Linux 列，docs/35 §5.1）。
///
/// 用 `listen_with` 而不是 `keyboard::listen`：后者只给「没被控件吃掉」的按键，
/// 而命令面板的输入框聚焦时会吃掉 Esc——那样按 Esc 关不掉面板。
/// 所以：命令面板的 ↑↓ / Esc 不管有没有被吃都收；其余快捷键只收没被吃掉的
/// （输入框里打字时 Ctrl J 之类不该触发外壳动作）。
fn shortcut(
    event: iced::Event,
    status: iced::event::Status,
    _window: window::Id,
) -> Option<Message> {
    use keyboard::key::Named;
    use ui::command::Cmd;
    use ui::shell::ShellEvent;
    // 修饰键变化：记下来给网格的 Ctrl+点击多选用（按钮按下事件不带修饰键）
    if let iced::Event::Keyboard(keyboard::Event::ModifiersChanged(m)) = &event {
        ui::set_modifiers(m.control(), m.shift());
        return None;
    }
    let iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event else {
        return None;
    };
    let captured = status == iced::event::Status::Captured;
    match key.as_ref() {
        keyboard::Key::Named(Named::ArrowUp) => {
            return Some(Message::Shell(ShellEvent::PaletteMove(-1)));
        }
        keyboard::Key::Named(Named::ArrowDown) => {
            return Some(Message::Shell(ShellEvent::PaletteMove(1)));
        }
        keyboard::Key::Named(Named::Escape) => {
            return Some(if captured {
                Message::Shell(ShellEvent::PaletteClose)
            } else {
                Message::GoBack
            });
        }
        _ => {}
    }
    if captured {
        return None;
    }
    let (ctrl, shift, alt) = (modifiers.control(), modifiers.shift(), modifiers.alt());
    let cmd = match key.as_ref() {
        // 页面（docs/41 §3.4）：Ctrl PgUp / PgDn 前后页；Alt 1–9 第 n 页（Ctrl 1–9 已是「聚焦第 n 个面板」）
        keyboard::Key::Named(Named::PageDown) if ctrl => Some(Cmd::NextPage),
        keyboard::Key::Named(Named::PageUp) if ctrl => Some(Cmd::PrevPage),
        keyboard::Key::Character(c) if alt && !ctrl && !shift && matches!(c.parse::<usize>(), Ok(1..=9)) => {
            Some(Cmd::Page(c.parse::<usize>().unwrap_or(1) - 1))
        }
        keyboard::Key::Named(Named::F6) => Some(if shift {
            Cmd::FocusPrevPane
        } else {
            Cmd::FocusNextPane
        }),
        keyboard::Key::Character(c) if ctrl => {
            let c = c.to_ascii_lowercase();
            match (shift, alt, c.as_str()) {
                (false, false, "k") => Some(Cmd::TogglePalette),
                (false, false, "p") => Some(Cmd::PaletteScope("#")),
                (true, false, "p") => Some(Cmd::PaletteScope("@")),
                (false, false, "/") => Some(Cmd::PaletteScope("?")),
                (false, false, "b") => Some(Cmd::ToggleSidebar),
                (_, false, "=" | "+") => Some(Cmd::ZoomIn),
                (false, false, "-") => Some(Cmd::ZoomOut),
                (false, false, "0") => Some(Cmd::ZoomReset),
                // Ctrl 1–9：聚焦第 n 个面板
                (false, false, d @ ("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")) => {
                    Some(Cmd::FocusPane(d.parse::<usize>().unwrap_or(1) - 1))
                }
                (false, false, "j") => Some(Cmd::ToggleBottom),
                (false, false, "i") => Some(Cmd::ToggleInspector),
                (false, false, ",") => Some(Cmd::OpenSettings),
                (true, false, "m") => Some(Cmd::ToggleMaximize),
                (true, false, "d") => Some(Cmd::ToggleDataTable),
                (false, true, "t") => Some(Cmd::CycleTheme),
                (true, false, "t") => Some(Cmd::PageUndo),
                (false, true, "d") => Some(Cmd::CycleDensity),
                // Ctrl Shift 1–6：各组的第一个工作区（docs/41 §2）
                (true, false, d @ ("1" | "2" | "3" | "4" | "5" | "6")) => {
                    let i = d.parse::<usize>().unwrap_or(1) - 1;
                    ws::workspace::GROUPS
                        .get(i)
                        .and_then(|(_, ws)| ws.first())
                        .map(|n| Cmd::Workspace((*n).to_string()))
                }
                _ => None,
            }
        }
        _ => None,
    };
    cmd.map(Message::RunCommand)
}

struct Flowsurface {
    main_window: window::Window,
    sidebar: dashboard::Sidebar,
    handles: exchange::adapter::AdapterHandles,
    layout_manager: LayoutManager,
    theme_editor: ThemeEditor,
    network: NetworkManager,
    audio_stream: AudioStream,
    confirm_dialog: Option<screen::ConfirmDialog<Message>>,
    volume_size_unit: exchange::SizeUnit,
    ui_scale_factor: data::ScaleFactor,
    timezone: data::UserTimezone,
    theme: data::Theme,
    notifications: Notifications,
    ws_active: Option<ws::active_run::ActiveRun>, // WealthSpring 三态：当前活动 run（None=实时看盘）
    /// 每个工作区上次停留的页面（docs/41 §1）：侧栏点工作区回到这一页
    last_page: std::collections::HashMap<String, uuid::Uuid>,
    /// 页面元数据（顺序、自建页、关掉的模板页、锁定、模板指纹；`pages.json`）
    pages: ws::pages::PagesState,
    /// 模板与上次应用时不同的页（页签上「↻」，用户点「恢复默认」才覆盖）
    outdated: std::collections::BTreeSet<String>,
    /// 页签栏下面的页面工具条是否展开；改名输入框
    page_menu: bool,
    page_rename: String,
    /// 面板库：搜索词、是否加到浮动层
    lib_search: String,
    lib_float: bool,
    /// 本次运行里关掉的页（Ctrl Shift T 找回；不落盘）
    closed_pages: Vec<(String, data::Dashboard)>,
    ws_orders: ws::orders::OrderState,            // WealthSpring 订单/PnL（events.* 聚合，F3）
    ws_flow: ws::flow::FlowState,                 // WealthSpring 订单流：CVD/不平衡/背离（F4a）
    ws_factory: ws::factory::FactoryPool,         // WealthSpring Factory 现役池（F4c）
    ws_signals: Option<ws::signals::Signals>,     // WealthSpring 引擎信号：吸收/撤补/冰山（F4b–d 精确版）
    specimen: Option<ws::specimen::Specimen>,     // 界面样张模式（docs/35）：轮换工作区自截图
    shell: ui::shell::Shell,                      // 外壳（docs/35 批 3）：命令面板 / 底部面板 / 检查器
    commands: Vec<ui::command::Entry>,            // 命令注册表（UPDS V2 §12）
    /// 设置窗口的搜索词
    settings_query: String,
    /// 连不上的交易所行情流：交易所 → (第一次断开的时刻, 原因)。重连成功即移除。
    /// 上游每秒重试一次、只记 INFO，界面上只有「Waiting for data…」——离线时看不出为什么
    /// （docs/35 §16.14 测试矩阵 offline 场景查出来的）。
    stream_down: std::collections::BTreeMap<String, (std::time::Instant, String)>,
    /// 上一次写到磁盘的状态（序列化后的文本）：内容没变就不重写
    last_saved: String,
    /// 上一次自动存盘的时刻
    last_autosave: std::time::Instant,
    /// 主窗口宽度（逻辑像素）：窄于 [`NARROW_PX`] 时检查器变浮层抽屉
    main_width: f32,
    /// 命令面板实际列的条目 = 注册表 + 当前工作区的面板（打开面板时刷新）
    palette_entries: Vec<ui::command::Entry>,
    gallery: Option<ui::gallery::Gallery>,        // 组件样张页（样张模式 WS_UI_SPECIMEN_COMPONENTS）
}

#[derive(Debug, Clone)]
enum Message {
    /// 页面操作（docs/41 B 期）：新建 / 复制 / 改名 / 排序 / 关闭 / 模板 / 锁定
    Page(ws::pages::PageMsg),
    Sidebar(dashboard::sidebar::Message),
    MarketWsEvent(exchange::Event),
    WsActiveRun(Option<ws::active_run::ActiveRun>), // WealthSpring 三态切换
    WsOrders(ws::orders::OrderState),               // WealthSpring 订单/PnL 更新（F3）
    WsFactory(ws::factory::FactoryPool),            // WealthSpring Factory 现役池更新（F4c）
    WsSignals(ws::signals::Signals),                // WealthSpring 引擎信号更新（F4b–d 精确版）
    Dashboard {
        /// If `None`, the active layout is used for the event.
        layout_id: Option<uuid::Uuid>,
        event: dashboard::Message,
    },
    Tick(std::time::Instant),
    WindowEvent(window::Event),
    ExitRequested(HashMap<window::Id, WindowSpec>),
    RestartRequested(Option<HashMap<window::Id, WindowSpec>>),
    SaveStateRequested(HashMap<window::Id, WindowSpec>),
    /// systemd 停服务（SIGTERM）：先存盘再退出。原先直接被杀，布局从 9 月 25 日起一直没落过盘
    TermSignal,
    /// 收到 SIGTERM 后取完窗口位置：存盘、收掉自己的子进程、退出（守护由 systemd 管，不在这里停）
    SaveAndQuit(HashMap<window::Id, WindowSpec>),
    /// 定时自动存盘（内容没变不写）：崩溃、断电也最多丢一分钟的界面调整
    AutoSave(HashMap<window::Id, WindowSpec>),
    GoBack,
    DataFolderRequested,
    OpenUrlRequested(Cow<'static, str>),
    ScaleFactorChanged(data::ScaleFactor),
    /// 设置窗口的搜索框
    SettingsQuery(String),
    SetTimezone(data::UserTimezone),
    ToggleTradeFetch(bool),
    ApplyVolumeSizeUnit(exchange::SizeUnit),
    RemoveNotification(usize),
    ToggleDialogModal(Option<screen::ConfirmDialog<Message>>),
    ThemeEditor(modal::theme_editor::Message),
    NetworkManager(modal::network_manager::Message),
    Layouts(modal::layout_manager::Message),
    AudioStream(modal::audio::Message),
    /// 样张模式（docs/35）：每 500ms 一拍。
    SpecimenStep(std::time::Instant),
    SpecimenShot(iced::window::Screenshot),
    /// 外壳事件（命令面板输入、上下移动……）
    Shell(ui::shell::ShellEvent),
    /// 执行一条命令（命令面板、快捷键、外壳按钮都走这里，UPDS V2 §12）
    RunCommand(ui::command::Cmd),
    /// 组件样张页的事件
    Gallery(ui::gallery::GalleryMsg),
}

impl Flowsurface {
    fn new() -> (Self, Task<Message>) {
        let saved_state = layout::load_saved_state();

        let (main_window_id, open_main_window) = {
            let (position, mut size) = saved_state.window();
            // 样张模式可指定窗口尺寸（WS_UI_SPECIMEN_WINDOW=1920x1800）：数据规模等场景要把整张网格截进来
            if ws::specimen::enabled()
                && let Some((w, h)) = std::env::var("WS_UI_SPECIMEN_WINDOW")
                    .ok()
                    .and_then(|s| s.split_once('x').and_then(|(w, h)| Some((w.parse::<f32>().ok()?, h.parse::<f32>().ok()?))))
            {
                size = iced::Size::new(w, h);
            }
            let config = window::Settings {
                size,
                position,
                exit_on_close_request: false,
                ..window::settings()
            };
            window::open(config)
        };

        let handles = exchange::adapter::AdapterHandles::spawn_venues(
            exchange::adapter::Venue::ALL,
            saved_state.proxy_cfg.as_ref(),
        );

        let (sidebar, launch_sidebar) = dashboard::Sidebar::new(&saved_state, handles.clone());

        let (audio_stream, audio_init_err) = AudioStream::new(saved_state.audio_cfg);

        let mut state = Self {
            main_window: window::Window::new(main_window_id),
            layout_manager: saved_state.layout_manager,
            theme_editor: ThemeEditor::new(saved_state.custom_theme),
            audio_stream,
            sidebar,
            handles,
            confirm_dialog: None,
            timezone: saved_state.timezone,
            ui_scale_factor: saved_state.scale_factor,
            volume_size_unit: saved_state.volume_size_unit,
            theme: saved_state.theme,
            notifications: Notifications::new(),
            network: NetworkManager::new(saved_state.proxy_cfg),
            ws_active: None,
            last_page: std::collections::HashMap::new(),
            pages: ws::pages::PagesState::default(),
            outdated: std::collections::BTreeSet::new(),
            page_menu: false,
            page_rename: String::new(),
            lib_search: String::new(),
            lib_float: false,
            closed_pages: Vec::new(),
            ws_orders: ws::orders::OrderState::default(),
            ws_flow: ws::flow::FlowState::default(),
            ws_factory: ws::factory::FactoryPool::default(),
            ws_signals: None,
            specimen: ws::specimen::Specimen::from_env(),
            shell: ui::shell::Shell::default(),
            commands: ui::command::registry(&ws::workspace::WORKSPACES),
            palette_entries: ui::command::registry(&ws::workspace::WORKSPACES),
            main_width: f32::MAX,
            settings_query: String::new(),
            stream_down: std::collections::BTreeMap::new(),
            last_saved: String::new(),
            last_autosave: std::time::Instant::now(),
            gallery: if ws::specimen::enabled() { ui::gallery::Gallery::from_env() } else { None },
        };

        if let Some(err) = audio_init_err {
            state
                .notifications
                .push(Toast::error(format!("Audio disabled: {err}")));
        }

        if state.layout_manager.layouts.is_empty() {
            log::error!("No layouts available after loading state; creating a default layout");
            state.layout_manager = LayoutManager::new();
        }

        // WealthSpring 工作区（docs/08 F6 — P1）：幂等播种 5 个固定工作区
        // （官方原生 / 实盘 / 回测 / 数据录制 / Alpha Factory），不动用户已有 layout。
        // docs/41 B 期：缺页才按模板建，已有的页不动；模板有更新的只记下来在页签上提示
        state.pages = ws::pages::load();
        let report = ws::workspace::seed_pages(&mut state.layout_manager, &mut state.pages);
        state.outdated = report.outdated.into_iter().collect();
        if state.specimen.is_none() {
            ws::pages::save(&state.pages);
        }

        // 样张模式下可把外壳的三个浮动区全打开，截图验证它们（docs/35 批 3）
        // 跨进程命令（Studio「在 Cockpit 中查看本次回测」）。样张模式不监听：不和正在用的实例抢
        if state.specimen.is_none() {
            ws::bridge::start();
        }
        if state.specimen.is_some() && std::env::var_os("WS_UI_SPECIMEN_SHELL").is_some() {
            state.shell.bottom = true;
            state.shell.inspector = true;
            state.shell.bottom_tab = ui::shell::BottomTab::Problems;
            state.shell.palette = Some(ui::shell::Palette { query: "主题".into(), sel: 1 });
            state.shell.log.refresh();
        }
        // 样张模式下打开设置面板截图（docs/35 批 7）
        if state.specimen.is_some() && std::env::var_os("WS_UI_SPECIMEN_SETTINGS").is_some() {
            state.sidebar.set_menu(Some(sidebar::Menu::Settings));
        }
        // 样张模式：按侧栏顺序排好要截的工作区
        if let Some(sp) = state.specimen.as_mut() {
            for name in ws::workspace::all_layouts() {
                if let Some(l) = state.layout_manager.layouts.iter().find(|l| l.id.name == name) {
                    sp.queue.push((l.id.unique, name.clone()));
                }
            }
            // 只截某一个工作区（性能对照等场景：WS_UI_SPECIMEN_ONLY=新闻资讯）
            if let Ok(only) = std::env::var("WS_UI_SPECIMEN_ONLY") {
                sp.queue.retain(|(_, n)| *n == only);
            }
            // 组件样张页只截一张
            if state.gallery.is_some() {
                sp.queue.truncate(1);
            }
            log::info!("[specimen] {} 个工作区 → {}", sp.queue.len(), sp.dir.display());
        }


        let active_layout_id = state
            .layout_manager
            .active_layout_id()
            .or_else(|| {
                state
                    .layout_manager
                    .layouts
                    .first()
                    .map(|layout| &layout.id)
            })
            .map(|layout| layout.unique);

        let load_layout = active_layout_id
            .map(|uid| state.load_layout(uid, main_window_id))
            .unwrap_or_else(|| {
                log::error!("No active layout could be selected at startup");
                Task::none()
            });

        (
            state,
            open_main_window
                .discard()
                .chain(load_layout)
                .chain(launch_sidebar.map(Message::Sidebar)),
        )
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::WsActiveRun(ar) => {
                // WealthSpring 三态：记录活动 run（subscription() 据此切 live/回测 流）。
                // run 变更则重置订单流累计（CVD 等按 run/会话重新计）。
                if self.ws_active.as_ref().map(|a| &a.run_id) != ar.as_ref().map(|a| &a.run_id) {
                    self.ws_flow = ws::flow::FlowState::default();
                    // 换了 run → 清空 K 线，只留本次回测的数据。图表本来会跨 run 累积，
                    // 上一次回测的蜡烛留在左边，与本次毫无关系（docs/27 §12）。
                    let main_window_id = self.main_window.id;
                    self.active_dashboard_mut().clear_kline_charts(main_window_id);
                }
                // 旁路给各面板：它们据此判断手上的数据属不属于当前这次运行。
                ws::active_run::publish_current(ar.clone());
                self.ws_active = ar;
                return Task::none();
            }
            Message::WsOrders(st) => {
                // **一律从实时 events.* 喂图上标注**（docs/27 §12）。
                //
                // 原先自有数据回测工作区被排除在外，理由是「由 selfdata 桥（result.json）喂，
                // 免得两源相争」——但 result.json 要等回测整个跑完才存在，于是成交标记
                // **全程不出现、结束瞬间一次性糊上去**，看不到过程。
                //
                // 两源其实不冲突：events.* 是实时且更全的那一份（带数量与序号），
                // selfdata 在跑完后推的是同一批成交，覆盖上去结果一致。
                ws::orders::publish_chart_fills(&st.fills); // F3b：图上 ▲▼ 标记旁路给 kline 画布
                ws::orders::publish_chart_position(&st); // §11.1：图上持仓线 + 费后 PnL 读数
                ws::orders::publish_chart_working(&st.working); // §11.1：图上活动挂单线标注
                self.ws_orders = st; // 订单/PnL 聚合（view() 叠加显示）
                return Task::none();
            }
            Message::WsFactory(p) => {
                self.ws_factory = p; // Factory 现役池（view() 叠加显示）
                return Task::none();
            }
            Message::WsSignals(s) => {
                // F4c：combo 总分进进程级旁路曲线（kline 画布叠加主图）。接收时戳与实时主图时间轴对齐。
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                ws::signals::push_combo(now_ms, s.combo);
                self.ws_signals = Some(s); // 引擎信号：吸收/撤补/冰山（view() 叠加显示）
                return Task::none();
            }
            Message::MarketWsEvent(event) => {
                // F4a：从逐笔成交累计 CVD/不平衡/背离（喂图前先 tap，避免与 dashboard 借用冲突）。
                if let exchange::Event::TradesReceived(_, _, buffer) = &event {
                    for t in buffer.iter() {
                        self.ws_flow.apply(t.price.to_f32() as f64, f32::from(t.qty) as f64, t.is_sell);
                    }
                }
                // F4b：从盘口快照算 spread + 盘口不平衡。
                if let exchange::Event::DepthReceived(_, _, depth) = &event {
                    self.ws_flow.apply_depth(depth);
                }
                let main_window_id = self.main_window.id;
                let dashboard = self.active_dashboard_mut();

                match event {
                    exchange::Event::Connected(exchange) => {
                        if let Some((t0, _)) = self.stream_down.remove(&exchange.to_string()) {
                            log::info!("[行情] {exchange} 已重新连上（断开了 {} 秒）", t0.elapsed().as_secs());
                        }
                    }
                    exchange::Event::Disconnected(exchange, reason) => {
                        log::info!("a stream disconnected from {exchange} WS: {reason:?}");
                        // 只在「从好变坏」时记一条警告（进问题页）；上游每秒重试，逐次记会刷屏
                        self.stream_down.entry(exchange.to_string()).or_insert_with(|| {
                            log::warn!("[行情] {exchange} 连不上：{reason}——在重试（每秒一次）");
                            (std::time::Instant::now(), reason.to_string())
                        });
                    }
                    exchange::Event::DepthReceived(stream, update_t, depth) => {
                        let task = dashboard
                            .ingest_depth(&stream, update_t, &depth, main_window_id)
                            .map(move |msg| Message::Dashboard {
                                layout_id: None,
                                event: msg,
                            });

                        return task;
                    }
                    exchange::Event::TradesReceived(stream, update_t, buffer) => {
                        let task = dashboard
                            .ingest_trades(&stream, &buffer, update_t, main_window_id)
                            .map(move |msg| Message::Dashboard {
                                layout_id: None,
                                event: msg,
                            });

                        if let Some(msg) = self.audio_stream.try_play_sound(&stream, &buffer) {
                            self.notifications.push(Toast::error(msg));
                        }

                        return task;
                    }
                    exchange::Event::KlineReceived(stream, kline) => {
                        return dashboard
                            .update_latest_klines(&stream, &kline, main_window_id)
                            .map(move |msg| Message::Dashboard {
                                layout_id: None,
                                event: msg,
                            });
                    }
                }
            }
            Message::Shell(ev) => {
                if let Some(cmd) = self.shell.update(ev, &self.palette_entries) {
                    return self.run_command(cmd);
                }
            }
            Message::RunCommand(cmd) => return self.run_command(cmd),
            Message::Gallery(m) => {
                if let Some(g) = self.gallery.as_mut() {
                    g.update(m);
                }
            }
            Message::Tick(now) => {
                // 定时自动存盘（每分钟；内容没变不写，见 save_state_to_disk）。样张模式不存
                if self.specimen.is_none() && now.duration_since(self.last_autosave) >= std::time::Duration::from_secs(60) {
                    self.last_autosave = now;
                    return window::collect_window_specs(self.all_window_ids(), Message::AutoSave);
                }
                // 网格右键菜单复制出来的文字（网格自己发不了剪贴板 Task）
                if let Some(t) = ui::grid::take_clipboard() {
                    self.notifications.push(Toast::info(format!("已复制 {} 行", t.lines().count().saturating_sub(1))));
                    return iced::clipboard::write(t);
                }
                // Studio 发来的跨进程命令（docs/35 §16.5 第 4 项）：切到回测工作区并把窗口提到前面
                let from_studio = ws::bridge::take();
                if !from_studio.is_empty() {
                    let mut tasks = Vec::new();
                    for c in from_studio {
                        if let ws::bridge::UiCommand::ShowRun { run_id } = c {
                            self.notifications.push(Toast::info(if run_id.is_empty() {
                                "Studio：查看本次回测".to_string()
                            } else {
                                format!("Studio：查看回测 run {run_id}")
                            }));
                            tasks.push(self.run_command(ui::command::Cmd::Workspace(ws::workspace::WS_BACKTEST.to_string())));
                            tasks.push(iced::window::gain_focus(self.main_window.id));
                        }
                    }
                    return Task::batch(tasks);
                }
                // 界面偏好可能被 Studio 改了（每秒最多看一次文件修改时间）
                if ui::poll() {
                    self.apply_ui_change();
                }
                // 告警常读（状态栏要数新命中）：每 2 秒只看一次修改时间，变了才重读
                self.shell.alerts.refresh();
                if self.shell.bottom && self.shell.bottom_tab == ui::shell::BottomTab::Alerts {
                    self.shell.alerts.mark_seen();
                }
                // 底部面板开着才读日志尾巴（每 2 秒一次）
                if self.shell.bottom {
                    self.shell.log.refresh();
                    if self.shell.bottom_tab == ui::shell::BottomTab::Notices {
                        self.refresh_notices();
                    }
                }
                let main_window_id = self.main_window.id;
                let handles = self.handles.clone();

                // 特征数据源换了标的（回放写出了图表流的 meta 行 / 切回实时）：
                // 按它重建「订单流特征」工作区的图（外壳 = 标的 + 最小刻度 + 最小量）
                if let Some(s) = ws::feature_source::take_shell() {
                    let ti = exchange::TickerInfo::new(
                        exchange::Ticker::new(&s.symbol, exchange::adapter::Exchange::BinanceLinear),
                        s.tick,
                        s.min_qty,
                        None,
                    );
                    for l in self
                        .layout_manager
                        .layouts
                        .iter_mut()
                        .filter(|l| ws::workspace::workspace_of(&l.id.name) == ws::workspace::WS_FEATURES)
                    {
                        let _ = l.dashboard.reset_chart_panes_to(main_window_id, Some(ti));
                    }
                }
                // 订单流特征工作区的图上设置（K 线周期、Footprint 失衡阈值）发布给「图表参数」视图比对口径
                // 多页时取正在看的那一页（在这个工作区里的话），否则第一页
                let active_uid = self.layout_manager.active_layout_id().map(|l| l.unique);
                let feat = self
                    .layout_manager
                    .layouts
                    .iter()
                    .filter(|l| ws::workspace::workspace_of(&l.id.name) == ws::workspace::WS_FEATURES)
                    .max_by_key(|l| (Some(l.id.unique) == active_uid, l.id.name == ws::workspace::WS_FEATURES));
                if let Some(l) = feat {
                    ws::chart_params::publish_pane_charts(l.dashboard.pane_charts(main_window_id));
                }

                return self
                    .active_dashboard_mut()
                    .tick(&handles, now, main_window_id)
                    .map(move |msg| Message::Dashboard {
                        layout_id: None,
                        event: msg,
                    });
            }
            Message::WindowEvent(event) => match event {
                window::Event::CloseRequested(window) => {
                    let main_window = self.main_window.id;
                    let dashboard = self.active_dashboard_mut();

                    if window != main_window {
                        dashboard.popout.remove(&window);
                        return window::close(window);
                    }

                    let mut active_windows = dashboard
                        .popout
                        .keys()
                        .copied()
                        .collect::<Vec<window::Id>>();
                    active_windows.push(main_window);

                    return window::collect_window_specs(active_windows, Message::ExitRequested);
                }
                window::Event::Resized(w, size) => {
                    if w == self.main_window.id {
                        self.main_width = size.width;
                    }
                }
            },
            Message::SpecimenStep(now) => {
                let main = self.main_window.id;
                let step = match self.specimen.as_mut() {
                    Some(sp) => sp.step(now),
                    None => return Task::none(),
                };
                match step {
                    ws::specimen::Step::Load(uid) => {
                        // 组件样张页：网格此时已渲染过，滚动指令才有对象可滚
                        let scroll = self.gallery.as_ref().map(|g| g.initial_scroll()).unwrap_or_else(Task::none);
                        let task = self.load_layout(uid, main);
                        // WS_UI_SPECIMEN_INSPECT：打开检查器并聚焦第一个有可编辑属性的面板（docs/35 §16.5 第 3 项）
                        if std::env::var_os("WS_UI_SPECIMEN_INSPECT").is_some() {
                            self.shell.inspector = true;
                            let d = self.active_dashboard_mut();
                            let target = d
                                .panes
                                .iter()
                                .find(|(_, st)| ws::inspector_props::view(&st.content).is_some())
                                .map(|(p, _)| *p);
                            if let Some(p) = target {
                                d.focus = Some((main, p));
                            }
                        }
                        return task.chain(scroll);
                    }
                    ws::specimen::Step::Shoot => {
                        return iced::window::screenshot(main).map(Message::SpecimenShot);
                    }
                    ws::specimen::Step::Nothing => {}
                }
            }
            Message::Page(m) => return self.page_action(m),
            Message::SpecimenShot(shot) => {
                // 焦点顺序表（docs/35 §13.3「每个工作区一份焦点顺序表」）：F6 / Ctrl 1–9 走的面板次序
                if let Some(sp) = &self.specimen {
                    let ws_name = self.layout_manager.active_layout_id().map(|l| l.name.clone()).unwrap_or_default();
                    let mut panes: Vec<(pane_grid::Pane, String)> =
                        self.active_dashboard().panes.iter().map(|(p, st)| (*p, st.content.to_string())).collect();
                    panes.sort_by_key(|(p, _)| *p);
                    ws::specimen::append_focus_order(&sp.dir, &ws_name, &panes.into_iter().map(|(_, n)| n).collect::<Vec<_>>());
                }
                let done = self
                    .specimen
                    .as_mut()
                    .is_some_and(|sp| sp.save(&shot.rgba, shot.size.width, shot.size.height));
                if done && let Some(sp) = &self.specimen {
                    ui::perf::write(&sp.dir);
                }
                if done {
                    log::info!("[specimen] 全部截完，退出");
                    return iced::exit();
                }
            }
            Message::ExitRequested(windows) => {
                self.save_state_to_disk(&windows);
                // 特征面板发起的回放是 Cockpit 的子进程：关窗一起停
                ws::feature_source::shutdown();
                ws::backtest_launch::shutdown();
                // 关界面 = 关掉全部后台守护（docs/26 S4b）。**先存盘再停**：
                // 反过来的话，停服务那几十毫秒里用户已经看不到窗口了，
                // 而布局还没落盘——崩在这中间就丢布局。
                if self.specimen.is_none()
                    && let Err(e) = ws::lifecycle::stop_all()
                {
                    log::warn!("停止后台守护失败（{}）：{e}", ws::lifecycle::TARGET);
                }
                return iced::exit();
            }
            Message::SaveStateRequested(windows) => {
                self.save_state_to_disk(&windows);
            }
            Message::TermSignal => {
                log::info!("收到 SIGTERM：存盘后退出");
                return window::collect_window_specs(self.all_window_ids(), Message::SaveAndQuit);
            }
            Message::SaveAndQuit(windows) => {
                self.save_state_to_disk(&windows);
                // 自己起的子进程（特征回放、回测）一起收掉；后台守护由 systemd 管（ws-stack.target），
                // 这里不停——否则 `systemctl restart ws-cockpit` 会连整套守护一起停
                ws::feature_source::shutdown();
                ws::backtest_launch::shutdown();
                return iced::exit();
            }
            Message::AutoSave(windows) => {
                self.save_state_to_disk(&windows);
            }
            Message::RestartRequested(Some(windows)) => {
                self.save_state_to_disk(&windows);
                return self.restart();
            }
            Message::RestartRequested(None) => {
                self.confirm_dialog = None;

                let mut active_windows = self
                    .active_dashboard()
                    .popout
                    .keys()
                    .copied()
                    .collect::<Vec<window::Id>>();
                active_windows.push(self.main_window.id);

                return window::collect_window_specs(active_windows, |windows| {
                    Message::RestartRequested(Some(windows))
                });
            }
            Message::GoBack => {
                let main_window = self.main_window.id;

                // Esc 每次只退一层作用域（UPDS V2 §12）：命令面板在最上层，先关它
                if self.shell.palette.is_some() {
                    self.shell.palette = None;
                } else if self.confirm_dialog.is_some() {
                    self.confirm_dialog = None;
                } else if self.sidebar.active_menu().is_some() {
                    self.sidebar.set_menu(None);
                } else {
                    let dashboard = self.active_dashboard_mut();

                    if dashboard.go_back(main_window) {
                        return Task::none();
                    } else if dashboard.panes.maximized().is_some() {
                        // 最大化的面板先还原（docs/41 §4.1）
                        dashboard.panes.restore();
                    } else if dashboard.focus.is_some() {
                        dashboard.focus = None;
                    } else {
                        self.sidebar.hide_tickers_table();
                    }
                }
            }
            Message::Dashboard {
                layout_id: id,
                event: msg,
            } => {
                let Some(active_layout) = self.layout_manager.active_layout_id() else {
                    log::error!("No active layout to handle dashboard message");
                    return Task::none();
                };

                let main_window = self.main_window;
                let layout_id = id.unwrap_or(active_layout.unique);
                let handles = self.handles.clone();

                if let Some(dashboard) = self.layout_manager.mut_dashboard(layout_id) {
                    let (main_task, event) =
                        dashboard.update(&handles, msg, &main_window, &layout_id);

                    let additional_task = match event {
                        Some(dashboard::Event::DistributeFetchedData {
                            layout_id,
                            pane_id,
                            data,
                            stream,
                        }) => dashboard
                            .distribute_fetched_data(main_window.id, pane_id, data, stream)
                            .map(move |msg| Message::Dashboard {
                                layout_id: Some(layout_id),
                                event: msg,
                            }),
                        Some(dashboard::Event::Notification(toast)) => {
                            self.notifications.push(toast);
                            Task::none()
                        }
                        Some(dashboard::Event::ResolveStreams { pane_id, streams }) => {
                            let tickers_info = self.sidebar.tickers_info();

                            let has_any_ticker_info =
                                tickers_info.values().any(|opt| opt.is_some());
                            if !has_any_ticker_info {
                                log::debug!(
                                    "Deferring persisted stream resolution for pane {pane_id}: ticker metadata not loaded yet"
                                );
                                return Task::none();
                            }

                            let resolved_streams =
                                streams.into_iter().try_fold(vec![], |mut acc, persist| {
                                    let resolver = |t: &exchange::Ticker| {
                                        tickers_info.get(t).and_then(|opt| *opt)
                                    };

                                    match persist.into_stream_kinds(resolver) {
                                        Ok(mut resolved) => {
                                            acc.append(&mut resolved);
                                            Ok(acc)
                                        }
                                        Err(err) => Err(format!(
                                            "Persisted stream still not resolvable: {err}"
                                        )),
                                    }
                                });

                            match resolved_streams {
                                Ok(resolved) => {
                                    if resolved.is_empty() {
                                        Task::none()
                                    } else {
                                        dashboard
                                            .resolve_streams(main_window.id, pane_id, resolved)
                                            .map(move |msg| Message::Dashboard {
                                                layout_id: None,
                                                event: msg,
                                            })
                                    }
                                }
                                Err(err) => {
                                    // This is typically a transient state (e.g. partial metadata, stale symbol)
                                    log::debug!("{err}");
                                    Task::none()
                                }
                            }
                        }
                        Some(dashboard::Event::RequestPalette) => {
                            // 图表的调色板来自设计 token（docs/35），不再是上游主题
                            let theme = ui::iced_theme();

                            let main_window = self.main_window.id;
                            self.active_dashboard_mut()
                                .theme_updated(main_window, &theme);

                            Task::none()
                        }
                        None => Task::none(),
                    };

                    return main_task
                        .map(move |msg| Message::Dashboard {
                            layout_id: Some(layout_id),
                            event: msg,
                        })
                        .chain(additional_task);
                }
            }
            Message::RemoveNotification(index) => {
                self.notifications.remove(index);
            }
            Message::SetTimezone(tz) => {
                self.timezone = tz;
            }
            Message::ScaleFactorChanged(value) => {
                self.ui_scale_factor = value;
            }
            Message::SettingsQuery(q) => self.settings_query = q,
            Message::ToggleTradeFetch(checked) => {
                self.layout_manager
                    .iter_dashboards_mut()
                    .for_each(|dashboard| {
                        dashboard.toggle_trade_fetch(checked, &self.main_window);
                    });

                if checked {
                    self.confirm_dialog = None;
                }
            }
            Message::ToggleDialogModal(dialog) => {
                self.confirm_dialog = dialog;
            }
            Message::Layouts(message) => {
                let action = self.layout_manager.update(message);

                match action {
                    Some(modal::layout_manager::Action::Select(layout)) => {
                        let active_popout_keys = self
                            .active_dashboard()
                            .popout
                            .keys()
                            .copied()
                            .collect::<Vec<_>>();

                        let window_tasks = Task::batch(
                            active_popout_keys
                                .iter()
                                .map(|&popout_id| window::close::<window::Id>(popout_id))
                                .collect::<Vec<_>>(),
                        )
                        .discard();

                        let old_layout_id = self
                            .layout_manager
                            .active_layout_id()
                            .as_ref()
                            .map(|layout| layout.unique);

                        return window::collect_window_specs(
                            active_popout_keys,
                            dashboard::Message::SavePopoutSpecs,
                        )
                        .map(move |msg| Message::Dashboard {
                            layout_id: old_layout_id,
                            event: msg,
                        })
                        .chain(window_tasks)
                        .chain(self.load_layout(layout, self.main_window.id));
                    }
                    Some(modal::layout_manager::Action::Clone(id)) => {
                        let manager = &mut self.layout_manager;

                        let source_data = manager.get(id).map(|layout| {
                            (
                                layout.id.name.clone(),
                                layout.id.unique,
                                data::Dashboard::from(&layout.dashboard),
                            )
                        });

                        if let Some((name, old_id, ser_dashboard)) = source_data {
                            let new_uid = uuid::Uuid::new_v4();
                            let new_layout = LayoutId {
                                unique: new_uid,
                                name: manager.ensure_unique_name(&name, new_uid),
                            };

                            let mut popout_windows = Vec::new();

                            for (pane, window_spec) in &ser_dashboard.popout {
                                let configuration = configuration(pane.clone());
                                popout_windows.push((configuration, *window_spec));
                            }

                            let floats = ser_dashboard.floating.iter().map(|(p, r)| (configuration(p.clone()), *r)).collect();
                            let dashboard = Dashboard::from_config(
                                configuration(ser_dashboard.pane.clone()),
                                popout_windows,
                                old_id,
                            )
                            .with_floating(floats, ser_dashboard.floats_hidden);

                            manager.insert_layout(new_layout.clone(), dashboard);
                        }
                    }
                    None => {}
                }
            }
            Message::AudioStream(message) => {
                if let Some(event) = self.audio_stream.update(message) {
                    match event {
                        modal::audio::UpdateEvent::RetryFailed(err) => {
                            self.notifications
                                .push(Toast::error(format!("Audio still unavailable: {err}")));
                        }
                        modal::audio::UpdateEvent::RetrySucceeded => {
                            self.notifications.push(Toast::info(
                                "Audio output re-initialized successfully".to_string(),
                            ));
                        }
                    }
                }
            }
            Message::DataFolderRequested => {
                if let Err(err) = data::open_data_folder() {
                    self.notifications
                        .push(Toast::error(format!("Failed to open data folder: {err}")));
                }
            }
            Message::OpenUrlRequested(url) => {
                if let Err(err) = data::open_url(url.as_ref()) {
                    self.notifications
                        .push(Toast::error(format!("Failed to open link: {err}")));
                }
            }
            Message::ThemeEditor(msg) => {
                let action = self.theme_editor.update(msg, &self.theme.clone().into());

                match action {
                    Some(modal::theme_editor::Action::Exit) => {
                        self.sidebar.set_menu(Some(sidebar::Menu::Settings));
                    }
                    Some(modal::theme_editor::Action::UpdateTheme(theme)) => {
                        self.theme = data::Theme(theme.clone());

                        let main_window = self.main_window.id;
                        self.active_dashboard_mut()
                            .theme_updated(main_window, &theme);
                    }
                    None => {}
                }
            }
            Message::NetworkManager(msg) => {
                let action = self.network.update(msg);

                match action {
                    Some(network_manager::Action::ApplyProxy) => {
                        if let Some(proxy) = self.network.proxy_cfg() {
                            data::config::proxy::save_proxy_auth(&proxy);
                        }

                        self.confirm_dialog = Some(
                            screen::ConfirmDialog::new(
                                "Proxy changes saved. Restart now to apply?".to_string(),
                                Box::new(Message::RestartRequested(None)),
                            )
                            .with_confirm_btn_text("Restart now".to_string()),
                        );

                        let main_window = self.main_window.id;
                        let dashboard = self.active_dashboard_mut();

                        let mut active_windows = dashboard
                            .popout
                            .keys()
                            .copied()
                            .collect::<Vec<window::Id>>();
                        active_windows.push(main_window);

                        return window::collect_window_specs(
                            active_windows,
                            Message::SaveStateRequested,
                        );
                    }
                    Some(network_manager::Action::Exit) => {
                        self.sidebar.set_menu(Some(sidebar::Menu::Settings));
                    }
                    None => {}
                }
            }
            Message::Sidebar(message) => {
                let (task, action) = self.sidebar.update(message);

                match action {
                    Some(dashboard::sidebar::Action::TickerSelected(ticker_info, content)) => {
                        let main_window_id = self.main_window.id;
                        let handles = self.handles.clone();

                        let task = {
                            if let Some(kind) = content {
                                self.active_dashboard_mut().init_focused_pane(
                                    &handles,
                                    main_window_id,
                                    ticker_info,
                                    kind,
                                )
                            } else {
                                self.active_dashboard_mut().switch_tickers_in_group(
                                    &handles,
                                    main_window_id,
                                    ticker_info,
                                )
                            }
                        };

                        return task.map(move |msg| Message::Dashboard {
                            layout_id: None,
                            event: msg,
                        });
                    }
                    Some(dashboard::sidebar::Action::ErrorOccurred(err)) => {
                        self.notifications.push(Toast::error(err.to_string()));
                    }
                    Some(dashboard::sidebar::Action::SelectWorkspace(uid)) => {
                        // 工作区切换：复用 LayoutManager 的 SelectActive 全流程（存 popout/切 layout）。
                        return self.update(Message::Layouts(
                            modal::layout_manager::Message::SelectActive(uid),
                        ));
                    }
                    None => {}
                }

                return task.map(Message::Sidebar);
            }
            Message::ApplyVolumeSizeUnit(pref) => {
                self.volume_size_unit = pref;
                self.confirm_dialog = None;

                let mut active_windows: Vec<window::Id> =
                    self.active_dashboard().popout.keys().copied().collect();
                active_windows.push(self.main_window.id);

                return window::collect_window_specs(active_windows, |windows| {
                    Message::RestartRequested(Some(windows))
                });
            }
        }
        Task::none()
    }

    fn view(&self, id: window::Id) -> Element<'_, Message> {
        let t0 = std::time::Instant::now();
        let el = self.view_inner(id);
        // 性能预算的测量（docs/35 §13.2）：冷启动、切换工作区
        ui::perf::view_done(t0.elapsed());
        el
    }

    fn view_inner(&self, id: window::Id) -> Element<'_, Message> {
        // WealthSpring 原生 dockable pane（docs/08）：每帧把 ws_* 状态旁路给 readout 快照，
        // 供 `Content::WealthSpring` 面板渲染（pane 视图拿不到 &App，沿用旁路模式）。
        {
            let o = &self.ws_orders;
            let f = &self.ws_flow;
            let fac = &self.ws_factory;
            let mut r = ws::readout::Readout {
                mode: self.ws_active.as_ref().map(|a| a.mode.clone()).unwrap_or_default(),
                run_id: o.run_id.clone(),
                has_orders: o.has_summary,
                pos_side: o.pos_side.clone(),
                net_qty: o.net_qty,
                avg_px: o.avg_px,
                realized: o.realized,
                unrealized: o.unrealized,
                n_fills: o.fills.len(),
                n_buy: o.n_buy,
                n_sell: o.n_sell,
                capital: if o.capital > 0.0 { o.capital } else { 10_000.0 },
                equity: (if o.capital > 0.0 { o.capital } else { 10_000.0 }) + o.realized_net,
                return_pct: o.realized_net
                    / (if o.capital > 0.0 { o.capital } else { 10_000.0 })
                    * 100.0,
                realized_net: o.realized_net,
                fee_total: o.fee_total,
                trades: o.trades.clone(),
                working: o.working.values().cloned().collect(),
                cvd: f.cvd,
                imbalance: f.imbalance,
                divergence: f.divergence,
                book_imb: f.book_imb,
                spread: f.spread,
                absorbed_bid: f.absorbed_bid,
                absorbed_ask: f.absorbed_ask,
                pulled_bid: f.pulled_bid,
                pulled_ask: f.pulled_ask,
                fac_alphas: fac.alphas,
                fac_n_pool: fac.n_pool,
                fac_evals: fac.evals,
                pool: fac.pool.clone(),
                ..Default::default()
            };
            if let Some(s) = &self.ws_signals {
                r.has_signals = true;
                r.sess_traded_bid = s.sess_traded_bid;
                r.sess_traded_ask = s.sess_traded_ask;
                r.sess_pulled_bid = s.sess_pulled_bid;
                r.sess_pulled_ask = s.sess_pulled_ask;
                r.iceberg_bid = s.iceberg_bid;
                r.iceberg_ask = s.iceberg_ask;
                r.depth_bid = s.depth_bid;
                r.depth_ask = s.depth_ask;
                r.sig_combo = s.combo;
                r.sig_n_combo = s.n_combo;
                r.sig_n_pool = s.n_pool;
            }
            ws::readout::publish(r);
        }

        ws::pages::set_active_locked(self.pages.locked.contains(&self.active_name()));
        let dashboard = self.active_dashboard();
        let sidebar_pos = self.sidebar.position();

        let tickers_table = &self.sidebar.tickers_table;

        // 检查器托管聚焦面板的可编辑属性（docs/35 §16.5 第 3 项）：先登记托管的是哪个 pane，
        // 面板视图据此把原处的数据选择器 / 口径设置收成一行提示。
        let main_id = self.main_window.id;
        let host = if self.shell.inspector {
            dashboard.focus.filter(|(w, _)| *w == main_id).map(|(_, p)| p)
        } else {
            None
        };
        ws::inspector_props::set_host(host);

        let content = if id == self.main_window.id {
            // WealthSpring 工作区（docs/08 F6 — P1）：按固定顺序取 5 个工作区的 (uuid, 名, 是否活动)，
            // 合并进 FS 原生侧边栏顶部（单一侧边栏，图标切换不同窗口）。
            // 页面（docs/41）：侧栏一个工作区一个图标，点了回到它上次停留的页；当前页属于它就算激活
            let active_name = self.layout_manager.active_layout_id().map(|l| l.name.clone()).unwrap_or_default();
            let active_ws = ws::workspace::workspace_of(&active_name).to_string();
            let workspaces: Vec<(uuid::Uuid, &'static str, bool, Option<&'static str>)> = ws::workspace::WORKSPACES
                .iter()
                .filter_map(|&name| {
                    self.workspace_target(name).map(|uid| {
                        (uid, name, active_ws == name, self.workspace_badge(name).map(|b| b.glyph()))
                    })
                })
                .collect();

            let sidebar_view = self
                .sidebar
                .view(&workspaces)
                .map(Message::Sidebar);

            let dashboard_view: Element<'_, Message> = match &self.gallery {
                // 组件样张页替换工作区（外壳照常，便于同时检查命令栏 / 状态栏）
                Some(g) => g.view().map(Message::Gallery),
                None => dashboard
                    .view(&self.main_window, tickers_table, self.timezone)
                    .map(move |msg| Message::Dashboard {
                        layout_id: None,
                        event: msg,
                    }),
            };

            // 页签栏（docs/41 §3）：当前工作区的页面；只有一页的工作区不显示（除非打开了页面工具条）
            let pages = self.pages_of(&active_ws);
            let dashboard_view: Element<'_, Message> = if self.gallery.is_some() || (pages.len() <= 1 && !self.page_menu) {
                dashboard_view
            } else {
                let cur = self.layout_manager.active_layout_id().map(|l| l.unique).unwrap_or_default();
                let bt = self.backtest_running();
                let tabs: Vec<ui::shell::PageTab<uuid::Uuid>> = pages
                    .into_iter()
                    .map(|(uid, n)| {
                        let mut label = ws::workspace::page_title(&n);
                        if self.pages.locked.contains(&n) {
                            label.push_str(" 🔒");
                        }
                        if self.outdated.contains(&n) {
                            label.push_str(" ↻");
                        }
                        ui::shell::PageTab {
                            label,
                            badge: self
                                .layout_manager
                                .layouts
                                .iter()
                                .find(|l| l.id.unique == uid)
                                .and_then(|l| ws::page_status::of(&l.dashboard, bt))
                                .map(|b| b.glyph()),
                            value: uid,
                        }
                    })
                    .collect();
                let more = ui::widgets::btn(
                    if self.page_menu { "⋯ 页面 ▴" } else { "⋯ 页面" },
                    ui::widgets::Kind::Ghost,
                    Some(Message::Page(ws::pages::PageMsg::ToggleMenu)),
                );
                let bar = ui::shell::page_bar(
                    tabs,
                    &cur,
                    |uid| Message::Layouts(modal::layout_manager::Message::SelectActive(uid)),
                    vec![more],
                );
                let mut col = column![bar];
                if self.page_menu {
                    col = col.push(self.page_toolbar());
                }
                col.push(dashboard_view).into()
            };

            let header_title = {
                #[cfg(target_os = "macos")]
                {
                    iced::widget::center(
                        text("FLOWSURFACE")
                            .font(iced::Font {
                                weight: iced::font::Weight::Bold,
                                ..Default::default()
                            })
                            .size(crate::style::text_size::TITLE)
                            .style(style::title_text),
                    )
                    .height(20)
                    .align_y(Alignment::Center)
                    .padding(padding::top(4))
                }
                #[cfg(not(target_os = "macos"))]
                {
                    column![]
                }
            };

            // 外壳（docs/35 批 3，UPDS V2 §9）：命令栏 / 侧栏 / 工作区 + 底部面板 / 检查器 / 状态栏
            let info = self.shell_info();
            let work: Element<'_, Message> = if self.shell.bottom {
                column![
                    dashboard_view,
                    ui::shell::bottom_panel(&self.shell, &info).map(Message::Shell),
                ]
                .spacing(4)
                .into()
            } else {
                dashboard_view
            };
            let work: Element<'_, Message> = if self.shell.inspector {
                // 检查器里的控件发的是聚焦 pane 自己的事件，原样送回那个 pane 的处理函数
                let props = host.and_then(|p| {
                    let st = dashboard.panes.get(p)?;
                    let el = ws::inspector_props::view(&st.content)?;
                    Some(el.map(move |ev| Message::Dashboard {
                        layout_id: None,
                        event: dashboard::Message::Pane(main_id, dashboard::pane::Message::PaneEvent(p, ev)),
                    }))
                });
                let insp = ui::shell::inspector(&info, props, Message::Shell);
                if self.main_width < NARROW_PX {
                    // 窄窗口：抽屉盖在工作区右侧，不挤占面板宽度。stack 以第一层为尺寸基准，工作区放第一层
                    iced::widget::stack![work, row![iced::widget::space::horizontal(), insp]].into()
                } else {
                    row![work, insp].spacing(4).into()
                }
            } else {
                work
            };
            let command_bar = ui::shell::command_bar(&info).map(Message::Shell);
            let status_bar = ui::shell::status_bar(&info).map(Message::Shell);

            let base = column![
                header_title,
                // 实盘 / 模拟盘：顶边 2px 环境色条（docs/35 §9.1）
                ui::shell::env_strip(info.env).unwrap_or_else(|| column![].into()),
                command_bar,
                match sidebar_pos {
                    // Ctrl B 收起侧栏
                    _ if self.shell.sidebar_hidden => row![work],
                    sidebar::Position::Left => row![sidebar_view, work,],
                    sidebar::Position::Right => row![work, sidebar_view],
                }
                .height(iced::Length::Fill)
                .spacing(4)
                .padding(iced::Padding { top: 4.0, right: 8.0, bottom: 4.0, left: 8.0 }),
                status_bar,
            ];

            let base: Element<'_, Message> = if let Some(menu) = self.sidebar.active_menu() {
                self.view_with_modal(base.into(), dashboard, menu)
            } else {
                base.into()
            };

            // 命令面板（Ctrl K）浮在一切之上（UPDS V2 §12：e3 覆盖层）
            match &self.shell.palette {
                Some(p) => {
                    let overlay =
                        ui::shell::palette_overlay(p, &self.palette_entries).map(Message::Shell);
                    iced::widget::stack![base, overlay].into()
                }
                None => base,
            }
        } else {
            container(
                dashboard
                    .view_window(id, &self.main_window, tickers_table, self.timezone)
                    .map(move |msg| Message::Dashboard {
                        layout_id: None,
                        event: msg,
                    }),
            )
            .padding(padding::top(style::TITLE_PADDING_TOP))
            .into()
        };

        // WealthSpring 读数已迁为原生 dockable pane（`Content::WealthSpring`，docs/08 F6）：
        // 数据每帧由本函数顶部 `ws::readout::publish` 旁路给 pane 渲染（ws/view.rs）。
        // 旧的 App 级右上角悬浮框已移除——同等信息在停靠面板里，排版更专业、可拆分/持久化。

        toast::Manager::new(
            content,
            // 隐藏数值模式：通知暂停（不弹出，队列照常保留，关掉后恢复显示），docs/35 §9.3
            if ui::hide_values() { &[] } else { self.notifications.toasts() },
            match sidebar_pos {
                sidebar::Position::Left => Alignment::Start,
                sidebar::Position::Right => Alignment::End,
            },
            Message::RemoveNotification,
        )
        .into()
    }

    fn theme(&self, _window: window::Id) -> iced_core::Theme {
        // 主题来自设计 token（docs/35 批 2），由 ~/.config/wealthspring/ui.json 决定、与 Studio 共用。
        // `self.theme`（上游主题选择器 / 主题编辑器）暂时不再生效，批 7 下线主题编辑器。
        ui::iced_theme()
    }

    /// 窗口标题（docs/35 §5.1 / §9.1 / §9.3）：程序名 — 工作区 · 环境徽标 [· 数值已隐藏]。
    /// 任务栏、窗口切换器、录屏里都看得见「这个窗口会不会动真钱」与「是不是在演示模式」。
    fn title(&self, _window: window::Id) -> String {
        let ws = self.layout_manager.active_layout_id().map(|l| l.name.clone()).unwrap_or_default();
        let env = ws::provenance::badge(ws::workspace::replay_mode()).label;
        let hidden = if ui::hide_values() { " · ••• 数值已隐藏" } else { "" };
        if ws.is_empty() {
            format!("WealthSpring Cockpit · {env}{hidden}")
        } else {
            format!("WealthSpring Cockpit — {ws} · {env}{hidden}")
        }
    }

    fn scale_factor(&self, _window: window::Id) -> f32 {
        self.ui_scale_factor.into()
    }

    fn subscription(&self) -> Subscription<Message> {
        let window_events = window::events().map(Message::WindowEvent);
        let sidebar = self.sidebar.subscription().map(Message::Sidebar);

        // WealthSpring 工作区级数据隔离（docs/08 F6-P4）：图表数据源跟随**活动工作区**，
        // 不再跟全局三态——「回测」工作区只走回测 replay（replay 自身已按 active_run mode=backtest
        // 自门控：无回测运行时空闲，绝不混入实时），其余工作区（官方/实盘…）只走 FS 原生实时。
        // 故即便后台正跑回测，「实盘」工作区图表仍是实时；切到「回测」才看回测行情。
        // 数据源跟随活动工作区：回测 / Tardis 历史回放→replay；回测另加 result.json 桥；
        // 其余→实时。回放工作区共用同一条 `ws:bt:{run}:trades` 入图链路（docs/20 Phase 5）。
        // 页面（docs/41）：「回测｜结果报告」也属于「回测」工作区
        let active_ws = self
            .layout_manager
            .active_layout_id()
            .map(|l| ws::workspace::workspace_of(&l.name).to_string());
        let is_backtest = active_ws.as_deref() == Some(ws::workspace::WS_BACKTEST);
        let is_tardis = active_ws.as_deref() == Some(ws::workspace::WS_TARDIS);
        // 回放态：开 replay 订阅 + 关实时流（否则实时行情会盖掉回放，且历史 K 线落在
        // 实时时间窗之外根本看不见——Tardis 数据距今数月，这一条是必须的）。
        let is_replay = is_backtest || is_tardis;
        // 「回测」工作区**两条都开**（docs/28 §4.3 合并）：replay 管过程（边跑边画）、
        // selfdata 管跑完后的定稿（完整价格序列与成交点）。
        //
        // 合并前只有「自有数据回测」开 selfdata，「录制数据回测」没开——于是录制路跑完
        // 之后图上只有过程中 replay 推过的那些，没有定稿。合并顺带补上了这个缺口。
        let is_selfdata = is_backtest;
        // 「实时数据回测」工作区 + **有实盘 run 正在跑** → 图切到 Nautilus 管线（docs/28 §5）。
        //
        // 目的不是「多一个源可选」，是**让图表显示策略眼里看到的东西**：策略在 Nautilus 里
        // 看到的簿，和图表用 flowsurface 自己那条流画出来的，永远有细微差异；调策略时
        // 那个差异就是噪音。
        //
        // 只在跑起来时切（docs/28 §5.1）：没有活动 run 时仍走原生实时，否则这个工作区
        // 平时就是一片空白。
        let is_live_ws = active_ws.as_deref() == Some(ws::workspace::WS_LIVE);
        let live_feed =
            is_live_ws && self.ws_active.as_ref().is_some_and(|a| a.mode == "live");
        // 回放类工作区：禁止图表向交易所补拉历史 K 线（那些蜡烛与本次回测无关，
        // 混在旁边看着却一模一样）。判定点在 dashboard 深处，走进程级旁路。
        // `live_feed` 也算在内——它同样是「图上的数据不来自交易所原生流」。
        // 订单流特征工作区（docs/31 §8.2）：四张图全部由特征引擎的事件流驱动。
        let is_features = active_ws.as_deref() == Some(ws::workspace::WS_FEATURES);
        ws::workspace::set_replay_mode(is_replay || is_selfdata || live_feed || is_features);
        // 来源徽标要说「特征引擎」而不是「等待运行」——后者在这个工作区里是错的。
        ws::workspace::set_feature_feed(is_features);
        let ws_redis_url = std::env::var("WS_REDIS_URL")
            .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());

        // 网络出口总闸（ws::egress）：关掉时这一支返回 `Subscription::none()`，
        // iced 会把订阅连同底下的 WS 一起丢掉——**连接是真的断**，
        // 不是「界面不显示了」。这是面板上唯一能停掉 Cockpit 自身出口的地方，
        // systemctl 管不着本进程
        let exchange_streams = if is_replay
            || is_selfdata
            || live_feed
            || is_features
            || !ws::egress::streams_enabled()
        {
            Subscription::none()
        } else {
            self.active_dashboard().market_subscriptions(&self.handles).map(Message::MarketWsEvent)
        };
        // 「自有数据回测」也开 replay（docs/27 §12）：TradeTap 在**回测跑的过程中**就往
        // `ws:bt:{run}:trades` 推逐笔，图因此能边跑边画。
        //
        // 只靠 selfdata 桥不行——它读的是 `result.json`，那东西要等回测整个跑完才存在，
        // 于是全程一片空白、结束瞬间才一次性出现。两者并存：replay 管过程，selfdata 管
        // 跑完后的定稿（含完整价格序列与成交点）。
        let ws_replay_streams = if is_replay || is_selfdata || live_feed {
            self.active_dashboard()
                .ws_replay_subscriptions(
                    ws_redis_url.clone(),
                    // **回测与实盘都传**。这个过滤以前在 replay 线程内部做，
                    // 随「订阅只服务自身 run」一起挪到这里（docs/28 §4.4）——
                    // 判据留在一处，否则两处各自演化迟早对不上。
                    //
                    // 实盘也算：docs/28 §5 的 Nautilus 管线走的是同一条
                    // `ws:bt:{run}:trades`（TradeTap 回测/实盘同用）。
                    // `stopped` 态不传——那时没有数据在产。
                    // 空串 = 没有活动运行，订阅建起来即退出，不空转。
                    self.ws_active
                        .as_ref()
                        .filter(|a| a.mode == "backtest" || a.mode == "live")
                        .map(|a| a.run_id.clone())
                        .unwrap_or_default(),
                )
                .map(Message::MarketWsEvent)
        } else {
            Subscription::none()
        };
        // 特征图表流（docs/31 §8.2）：只在这个工作区开。
        //
        // 它与 `exchange_streams` 是**互斥**的：上面那一支已经把交易所流关掉了。
        // 两条同时开的话，图上会混进交易所实时的簿与成交，而它们与引擎算特征
        // 用的那条流不是同一条——那个差异正好是这个工作区要消除的东西。
        let ws_feature_feed = if is_features {
            let path = crate::ws::feature_feed::feed_path().to_string_lossy().into_owned();
            self.active_dashboard()
                .ws_feature_feed_subscriptions(path)
                .map(Message::MarketWsEvent)
        } else {
            Subscription::none()
        };
        let ws_selfdata_streams = if is_selfdata {
            self.active_dashboard().ws_selfdata_subscriptions().map(Message::MarketWsEvent)
        } else {
            Subscription::none()
        };
        // 始终轮询活动 run → 驱动三态切换（接 P3 跑回测/实盘）。
        let ws_active_run =
            ws::active_run::subscription(ws_redis_url.clone()).map(Message::WsActiveRun);
        // 始终读 events.* → 订单/PnL（回测/实盘态有 run 才有数据）。
        let ws_orders = ws::orders::subscription(ws_redis_url.clone()).map(Message::WsOrders);
        // 始终读 ws:factory:pool → Factory 现役池（factory_pool_bridge.py 发布）。
        let ws_factory = ws::factory::subscription(ws_redis_url.clone()).map(Message::WsFactory);
        // 始终读 ws:signals:{symbol} → 引擎吸收/撤补/冰山（ws_signals 发布器，F4b–d 精确版）。
        let ws_signals =
            ws::signals::subscription(ws_redis_url, "BTCUSDT".to_string()).map(Message::WsSignals);

        // **与官方一致**（docs/35 §16.10）：`unconditional-rendering` + 逐帧 Tick。图表按时钟刷新——
        // 行情只进缓冲，各面板到了自己的刷新间隔（K 线 1s、Ladder / 逐笔 100ms、热图按基础周期、
        // GPU 热图每帧）才清缓存重画；行情稀疏时时间轴照样平滑推进。
        // 曾为「空闲 CPU < 2%」改成无数据时降到 2Hz，结果行情稀疏 / 回放暂停 / 断网时热图一顿一顿，
        // 且实测空闲开销的大头根本不在渲染（主线程约 1%），已撤回。
        let tick = if self.specimen.is_some() {
            // 样张实例跑在 Xvfb 上：没有垂直同步，逐帧订阅会空转到 100%，改用 60Hz 定时
            iced::time::every(std::time::Duration::from_millis(16))
                .map(|_| Message::Tick(std::time::Instant::now()))
        } else {
            iced::window::frames().map(Message::Tick)
        };
        let specimen = if self.specimen.is_some() {
            iced::time::every(std::time::Duration::from_millis(500)).map(Message::SpecimenStep)
        } else {
            Subscription::none()
        };

        let hotkeys = iced::event::listen_with(shortcut);

        // systemd 停服务发的是 SIGTERM：收到后先存盘再退出（样张模式不需要）
        // （样张模式另可用 WS_UI_SPECIMEN_SIGTERM=1 打开，在隔离实例里验证这条路径）
        let sigterm = if self.specimen.is_none() || std::env::var_os("WS_UI_SPECIMEN_SIGTERM").is_some() {
            Subscription::run(|| {
                iced::stream::channel(1, |mut out: iced::futures::channel::mpsc::Sender<Message>| async move {
                    use iced::futures::SinkExt;
                    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                        Ok(mut s) => {
                            while s.recv().await.is_some() {
                                let _ = out.send(Message::TermSignal).await;
                            }
                        }
                        Err(e) => {
                            log::warn!("监听 SIGTERM 失败（被 systemd 停时不会存盘）：{e}");
                            std::future::pending::<()>().await;
                        }
                    }
                })
            })
        } else {
            Subscription::none()
        };

        Subscription::batch(vec![
            specimen,
            exchange_streams,
            ws_replay_streams,
            ws_selfdata_streams,
            ws_feature_feed,
            ws_active_run,
            ws_orders,
            ws_factory,
            ws_signals,
            sidebar,
            window_events,
            tick,
            hotkeys,
            sigterm,
        ])
    }

    /// 一个工作区的全部页面 (uid, 布局名)：先模板顺序，再是模板外的（以后用户自建的页）。
    fn pages_of(&self, wsn: &str) -> Vec<(uuid::Uuid, String)> {
        // 用户排过的顺序优先，其次模板顺序，最后是其余属于这个工作区的页
        let mut names: Vec<String> = self.pages.order.get(wsn).cloned().unwrap_or_default();
        for n in ws::workspace::page_layouts(wsn) {
            if !names.contains(&n) {
                names.push(n);
            }
        }
        let mut out: Vec<(uuid::Uuid, String)> = names
            .into_iter()
            .filter_map(|n| self.layout_manager.layouts.iter().find(|l| l.id.name == n).map(|l| (l.id.unique, n)))
            .collect();
        for l in &self.layout_manager.layouts {
            if ws::workspace::workspace_of(&l.id.name) == wsn && !out.iter().any(|(u, _)| *u == l.id.unique) {
                out.push((l.id.unique, l.id.name.clone()));
            }
        }
        out
    }

    fn current_pages(&self) -> Vec<(uuid::Uuid, String)> {
        let name = self.layout_manager.active_layout_id().map(|l| l.name.clone()).unwrap_or_default();
        self.pages_of(ws::workspace::workspace_of(&name))
    }

    fn active_name(&self) -> String {
        self.layout_manager.active_layout_id().map(|l| l.name.clone()).unwrap_or_default()
    }

    /// 工作区里一个没被占用的页面布局名：「工作区｜标题」，与布局名或本工作区任一页的**页签名**重了都加序号
    /// （第一页的布局名是工作区名、页签名另有，比如「资源」的页签名是「进程」）。
    fn free_page_name(&self, wsn: &str, title: &str) -> String {
        let base = format!("{wsn}{}{}", ws::workspace::PAGE_SEP, title.trim().replace(ws::workspace::PAGE_SEP, " "));
        let titles: Vec<String> = self.pages_of(wsn).iter().map(|(_, n)| ws::workspace::page_title(n)).collect();
        let taken = |n: &str| {
            self.layout_manager.layouts.iter().any(|l| l.id.name == n) || titles.contains(&ws::workspace::page_title(n))
        };
        if !taken(&base) {
            return base;
        }
        (2..).map(|i| format!("{base} {i}")).find(|n| !taken(n)).unwrap_or(base)
    }

    /// 把新页插进布局表、记进页面顺序（放在当前页后面），然后切过去。
    fn add_page(&mut self, name: String, dashboard: Dashboard, user: bool) -> Task<Message> {
        let wsn = ws::workspace::workspace_of(&name).to_string();
        let mut order: Vec<String> = self.pages_of(&wsn).into_iter().map(|(_, n)| n).collect();
        let at = order.iter().position(|n| *n == self.active_name()).map_or(order.len(), |i| i + 1);
        order.insert(at.min(order.len()), name.clone());
        self.pages.order.insert(wsn, order);
        if user {
            self.pages.user.insert(name.clone());
        }
        let uid = uuid::Uuid::new_v4();
        self.layout_manager.insert_layout(layout::LayoutId { unique: uid, name }, dashboard);
        ws::pages::save(&self.pages);
        self.update(Message::Layouts(modal::layout_manager::Message::SelectActive(uid)))
    }

    /// 页签栏下面的页面工具条（docs/41 B 期）。
    fn page_toolbar(&self) -> Element<'_, Message> {
        use ui::widgets::{self as w, Kind};
        use ws::pages::PageMsg as P;
        let name = self.active_name();
        let wsn = ws::workspace::workspace_of(&name).to_string();
        let first = name == wsn;
        let is_template = ws::workspace::all_layouts().contains(&name);
        let locked = self.pages.locked.contains(&name);
        let pm = |m: P| Some(Message::Page(m));
        let mut r1 = row![
            w::btn("＋ 新建空白页", Kind::Standard, pm(P::NewBlank)),
            w::btn("复制本页", Kind::Ghost, pm(P::Duplicate)),
            iced::widget::text_input("页名", &self.page_rename)
                .on_input(|t| Message::Page(P::RenameInput(t)))
                .on_submit(Message::Page(P::RenameApply))
                .width(iced::Length::Fixed(140.0))
                .size(ui::text::s_small()),
            w::btn("改名", Kind::Ghost, (!first).then_some(Message::Page(P::RenameApply))),
            w::btn("◀ 左移", Kind::Ghost, (!first).then_some(Message::Page(P::Move(-1)))),
            w::btn("右移 ▶", Kind::Ghost, (!first).then_some(Message::Page(P::Move(1)))),
            w::btn("关闭本页", Kind::Ghost, (!first).then_some(Message::Page(P::Close))),
            w::btn(
                if self.outdated.contains(&name) { "恢复默认（模板有更新）" } else { "恢复默认" },
                Kind::Ghost,
                is_template.then_some(Message::Page(P::ResetDefault)),
            ),
            w::btn("存为模板", Kind::Ghost, pm(P::SaveTemplate)),
            w::btn(if locked { "🔒 已锁定（点击解锁）" } else { "锁定布局" }, if locked { Kind::Standard } else { Kind::Ghost }, pm(P::ToggleLock)),
        ]
        .spacing(ui::metrics::space(1))
        .align_y(Alignment::Center);
        if !self.closed_pages.is_empty() {
            r1 = r1.push(w::btn(format!("找回刚关的页（{}）", self.closed_pages.len()), Kind::Ghost, pm(P::Undo)));
        }
        // 浮动层（docs/41 §4.2）：图表浮窗自由组合；面板标题栏的「◰」把图表浮起来
        let (n_float, hidden) = {
            let d = self.active_dashboard();
            (d.floating.len(), d.floats_hidden)
        };
        let fl = |m: dashboard::floating::FloatMsg| Some(Message::Dashboard { layout_id: None, event: dashboard::Message::Float(m) });
        r1 = r1.push(ui::text::metadata("│ 浮动层").color(ui::pal::dim()));
        r1 = r1.push(w::btn("＋ 浮动窗口", Kind::Ghost, (!locked).then(|| fl(dashboard::floating::FloatMsg::NewBlank)).flatten()));
        if n_float > 0 {
            r1 = r1.push(w::btn("一键整理", Kind::Ghost, (!locked).then(|| fl(dashboard::floating::FloatMsg::Tidy)).flatten()));
            r1 = r1.push(w::btn(
                if hidden { format!("展开浮动层（{n_float}）") } else { format!("收起浮动层（{n_float}）") },
                if hidden { Kind::Standard } else { Kind::Ghost },
                fl(dashboard::floating::FloatMsg::ToggleHidden),
            ));
        }
        // 第二行：关掉的模板页 + 模板库
        let mut r2 = row![ui::text::metadata("从模板新建：").color(ui::pal::dim())].spacing(ui::metrics::space(1)).align_y(Alignment::Center);
        let mut any = false;
        for n in ws::workspace::page_layouts(&wsn).into_iter().filter(|n| self.pages.closed.contains(n)) {
            any = true;
            r2 = r2.push(w::btn(format!("↺ {}", ws::workspace::page_title(&n)), Kind::Ghost, pm(P::Reopen(n))));
        }
        for (p, t) in ws::pages::templates() {
            any = true;
            let label = if t.workspace == wsn { t.title.clone() } else { format!("{} · {}", t.workspace, t.title) };
            r2 = r2.push(w::btn(label, Kind::Ghost, pm(P::FromFile(p))));
        }
        if !any {
            r2 = r2.push(
                ui::text::metadata(format!("还没有模板——「存为模板」会存到 {}；别的机器拷进同名目录即导入", ws::pages::template_dir().display()))
                    .color(ui::pal::dim()),
            );
        }
        // 第三行：面板库（docs/41 D 期）——搜索 + 按分类列出；单实例的标「单」，能浮动的在「浮窗」开关下才放浮动层
        let q = self.lib_search.trim().to_lowercase();
        let mut r3 = row![
            ui::text::metadata("添加面板：").color(ui::pal::dim()),
            iced::widget::text_input("搜索面板…", &self.lib_search)
                .on_input(|t| Message::Page(P::LibrarySearch(t)))
                .width(iced::Length::Fixed(140.0))
                .size(ui::text::s_small()),
            w::btn(
                if self.lib_float { "加到：浮动层（图表）" } else { "加到：平铺层" },
                if self.lib_float { Kind::Standard } else { Kind::Ghost },
                pm(P::LibraryFloat(!self.lib_float)),
            ),
        ]
        .spacing(ui::metrics::space(1))
        .align_y(Alignment::Center);
        let mut last_cat = "";
        let mut kinds: Vec<_> = data::layout::pane::ContentKind::ALL
            .iter()
            .copied()
            .filter(|k| *k != data::layout::pane::ContentKind::Starter)
            .filter(|k| !self.lib_float || ws::pages::floatable(*k))
            .filter(|k| q.is_empty() || k.to_string().to_lowercase().contains(&q) || ws::pages::category(*k).contains(&q))
            .collect();
        kinds.sort_by_key(|k| ["行情", "订单流", "研究", "策略", "数据与资讯", "系统", "其他"].iter().position(|c| *c == ws::pages::category(*k)));
        for k in kinds {
            let cat = ws::pages::category(k);
            if cat != last_cat {
                r3 = r3.push(ui::text::metadata(format!("│ {cat}")).color(ui::pal::dim()));
                last_cat = cat;
            }
            let label = if ws::pages::single_instance(k) { format!("{k} ·单") } else { k.to_string() };
            r3 = r3.push(w::btn(label, Kind::Ghost, (!locked).then(|| Message::Page(P::AddPanel(k)))));
        }
        column![r1.wrap(), r2.wrap(), r3.wrap()].spacing(ui::metrics::space(1)).padding(ui::metrics::pad2(1, 3)).into()
    }

    /// 页面操作（docs/41 B 期）。
    fn page_action(&mut self, m: ws::pages::PageMsg) -> Task<Message> {
        use ws::pages::PageMsg as P;
        let name = self.active_name();
        let wsn = ws::workspace::workspace_of(&name).to_string();
        let first = name == wsn;
        let note = |s: &mut Self, t: String| s.notifications.push(Toast::info(t));
        match m {
            P::ToggleMenu => {
                self.page_menu = !self.page_menu;
                self.page_rename = ws::workspace::page_title(&name);
            }
            P::NewBlank => {
                let n = self.free_page_name(&wsn, "新页面");
                let d = ws::workspace::blank_dashboard();
                return self.add_page(n, d, true);
            }
            P::Duplicate => {
                let Some(l) = self.layout_manager.layouts.iter().find(|l| l.id.name == name) else { return Task::none() };
                let data = data::Dashboard::from(&l.dashboard);
                let n = self.free_page_name(&wsn, &format!("{} 副本", ws::workspace::page_title(&name)));
                return self.add_page(n, layout::dashboard_from_data(data), true);
            }
            P::Reopen(n) => {
                if let Some(d) = ws::workspace::template_dashboard(&n) {
                    self.pages.closed.remove(&n);
                    // 从模板找回了，就不再算在「刚关的页」里（不然再点「找回」会多出一个重复页）
                    self.closed_pages.retain(|(c, _)| *c != n);
                    self.pages.applied.insert(n.clone(), ws::workspace::template_fingerprint(&n));
                    return self.add_page(n, d, false);
                }
            }
            P::FromFile(p) => {
                match std::fs::read_to_string(&p).ok().and_then(|t| serde_json::from_str::<ws::pages::PageTemplate>(&t).ok()) {
                    Some(t) => {
                        let n = self.free_page_name(&wsn, &t.title);
                        return self.add_page(n, layout::dashboard_from_data(t.dashboard), true);
                    }
                    None => self.notifications.push(Toast::warn(format!("模板读不了：{}", p.display()))),
                }
            }
            P::RenameInput(t) => self.page_rename = t,
            P::RenameApply => {
                let title = self.page_rename.trim().to_string();
                if first {
                    note(self, "第一页的名字跟着工作区（它是这个工作区的入口）；其余页可以改名".into());
                } else if !title.is_empty() && title != ws::workspace::page_title(&name) {
                    let new = self.free_page_name(&wsn, &title);
                    if let Some(l) = self.layout_manager.layouts.iter_mut().find(|l| l.id.name == name) {
                        l.id.name = new.clone();
                    }
                    self.pages.rename(&name, &new);
                    // 改了名的模板页就是用户的页了：模板不再管它，原模板页算「关掉」，可从工具条找回
                    if ws::workspace::all_layouts().contains(&name) {
                        self.pages.closed.insert(name.clone());
                        self.pages.user.insert(new.clone());
                    }
                    self.outdated.remove(&name);
                    ws::pages::save(&self.pages);
                }
            }
            P::Move(d) => {
                let mut order: Vec<String> = self.pages_of(&wsn).into_iter().map(|(_, n)| n).collect();
                if let Some(i) = order.iter().position(|n| *n == name) {
                    let j = i as i64 + i64::from(d);
                    // 第一页固定在最前（它是工作区的入口）
                    if j >= 1 && (j as usize) < order.len() && i >= 1 {
                        order.swap(i, j as usize);
                        self.pages.order.insert(wsn, order);
                        ws::pages::save(&self.pages);
                    }
                }
            }
            P::Close => {
                if first {
                    note(self, "第一页不能关（它是这个工作区的入口）；可以「恢复默认」或改它的内容".into());
                    return Task::none();
                }
                let pages = self.pages_of(&wsn);
                let Some(i) = pages.iter().position(|(_, n)| *n == name) else { return Task::none() };
                let (uid, _) = pages[i].clone();
                let next = pages.get(i + 1).or_else(|| i.checked_sub(1).and_then(|k| pages.get(k))).map(|(u, _)| *u);
                if let Some(l) = self.layout_manager.layouts.iter().find(|l| l.id.unique == uid) {
                    self.closed_pages.push((name.clone(), data::Dashboard::from(&l.dashboard)));
                }
                if ws::workspace::all_layouts().contains(&name) {
                    self.pages.closed.insert(name.clone());
                }
                if let Some(n) = next {
                    return self
                        .update(Message::Layouts(modal::layout_manager::Message::SelectActive(n)))
                        .chain(Task::done(Message::Page(P::Remove(uid))));
                }
            }
            P::Remove(uid) => {
                if self.layout_manager.active_layout_id().map(|l| l.unique) != Some(uid) {
                    if let Some(n) = self.layout_manager.layouts.iter().find(|l| l.id.unique == uid).map(|l| l.id.name.clone()) {
                        self.layout_manager.layouts.retain(|l| l.id.unique != uid);
                        let keep_closed = self.pages.closed.contains(&n);
                        self.pages.forget(&n);
                        if keep_closed {
                            self.pages.closed.insert(n);
                        }
                        ws::pages::save(&self.pages);
                    }
                }
            }
            P::Undo => {
                if let Some((n, d)) = self.closed_pages.pop() {
                    self.pages.closed.remove(&n);
                    let is_user = !ws::workspace::all_layouts().contains(&n);
                    let n = if self.layout_manager.layouts.iter().any(|l| l.id.name == n) {
                        self.free_page_name(ws::workspace::workspace_of(&n), &ws::workspace::page_title(&n))
                    } else {
                        n
                    };
                    return self.add_page(n, layout::dashboard_from_data(d), is_user);
                }
                note(self, "本次运行里没有关掉的页".into());
            }
            P::ResetDefault => match ws::workspace::template_dashboard(&name).filter(|_| ws::workspace::all_layouts().contains(&name)) {
                Some(d) => {
                    let uid = self.layout_manager.active_layout_id().map(|l| l.unique);
                    if let Some(l) = self.layout_manager.layouts.iter_mut().find(|l| Some(l.id.unique) == uid) {
                        l.dashboard = d;
                    }
                    self.pages.applied.insert(name.clone(), ws::workspace::template_fingerprint(&name));
                    self.outdated.remove(&name);
                    ws::pages::save(&self.pages);
                    if let Some(u) = uid {
                        return self.load_layout(u, self.main_window.id);
                    }
                }
                None => note(self, "这一页是自建的，没有默认模板".into()),
            },
            P::SaveTemplate => {
                let Some(l) = self.layout_manager.layouts.iter().find(|l| l.id.name == name) else { return Task::none() };
                let t = ws::pages::PageTemplate {
                    title: ws::workspace::page_title(&name),
                    workspace: wsn.clone(),
                    dashboard: data::Dashboard::from(&l.dashboard),
                };
                match ws::pages::save_template(&t) {
                    Ok(p) => note(self, format!("已存为模板：{}（拷到别的机器同名目录即导入）", p.display())),
                    Err(e) => self.notifications.push(Toast::warn(format!("存模板失败：{e}"))),
                }
            }
            P::LibrarySearch(t) => self.lib_search = t,
            P::LibraryFloat(b) => self.lib_float = b,
            P::AddPanel(kind) => {
                return self.update(Message::Dashboard {
                    layout_id: None,
                    event: dashboard::Message::AddPanel { kind, float: self.lib_float },
                });
            }
            P::ToggleLock => {
                if !self.pages.locked.remove(&name) {
                    self.pages.locked.insert(name.clone());
                }
                ws::pages::save(&self.pages);
            }
        }
        Task::none()
    }

    /// 侧栏点工作区去哪一页：上次停留的（还在的话），否则第一页。
    fn workspace_target(&self, wsn: &str) -> Option<uuid::Uuid> {
        let pages = self.pages_of(wsn);
        self.last_page
            .get(wsn)
            .filter(|u| pages.iter().any(|(p, _)| p == *u))
            .copied()
            .or_else(|| pages.first().map(|(u, _)| *u))
    }

    fn backtest_running(&self) -> bool {
        self.ws_active.as_ref().is_some_and(|a| a.mode == "backtest")
    }

    /// 一个工作区的角标：它所有页面里最严重的那个。
    fn workspace_badge(&self, wsn: &str) -> Option<ws::page_status::Badge> {
        let bt = self.backtest_running();
        self.pages_of(wsn)
            .iter()
            .filter_map(|(u, _)| self.layout_manager.layouts.iter().find(|l| l.id.unique == *u))
            .filter_map(|l| ws::page_status::of(&l.dashboard, bt))
            .max()
    }

    fn active_dashboard(&self) -> &Dashboard {
        let active_layout = self
            .layout_manager
            .active_layout_id()
            .expect("No active layout");
        self.layout_manager
            .get(active_layout.unique)
            .map(|layout| &layout.dashboard)
            .expect("No active dashboard")
    }

    fn active_dashboard_mut(&mut self) -> &mut Dashboard {
        let active_layout = self
            .layout_manager
            .active_layout_id()
            .expect("No active layout");
        self.layout_manager
            .get_mut(active_layout.unique)
            .map(|layout| &mut layout.dashboard)
            .expect("No active dashboard")
    }

    /// 外观偏好变了（本进程改的或 Studio 改的）：通知图表重取调色板。
    ///
    /// 图表会缓存颜色（着色器热图的色阶、K 线的涨跌色……），原来靠上游的 `ThemeSelected`
    /// 通知它们；主题改由设计 token 决定之后，这一步必须显式做，否则要重启图表才变色。
    fn apply_ui_change(&mut self) {
        let main = self.main_window.id;
        let theme = ui::iced_theme();
        for dashboard in self.layout_manager.iter_dashboards_mut() {
            dashboard.theme_updated(main, &theme);
        }
    }

    /// 执行一条命令（docs/35 批 3）。命令面板、快捷键、外壳按钮共用这一个入口。
    fn run_command(&mut self, cmd: ui::command::Cmd) -> Task<Message> {
        let look = matches!(
            cmd,
            ui::command::Cmd::Theme(_)
                | ui::command::Cmd::CycleTheme
                | ui::command::Cmd::Density(_)
                | ui::command::Cmd::CycleDensity
                | ui::command::Cmd::ToggleUpDown
                | ui::command::Cmd::ToggleCvd
                | ui::command::Cmd::UpDown(_)
                | ui::command::Cmd::Cvd(_)
                | ui::command::Cmd::HeatmapScale(_)
                | ui::command::Cmd::SystemComfortable(_)
        );
        let task = self.run_command_inner(cmd);
        if look {
            self.apply_ui_change();
        }
        task
    }

    fn run_command_inner(&mut self, cmd: ui::command::Cmd) -> Task<Message> {
        use ui::command::Cmd;
        let main = self.main_window.id;
        match cmd {
            Cmd::Page(i) => {
                let pages = self.current_pages();
                if let Some((uid, _)) = pages.get(i) {
                    return self.update(Message::Layouts(modal::layout_manager::Message::SelectActive(*uid)));
                }
            }
            Cmd::FloatNew | Cmd::FloatTidy | Cmd::FloatToggle => {
                use dashboard::floating::FloatMsg as F;
                let m = match cmd {
                    Cmd::FloatNew => F::NewBlank,
                    Cmd::FloatTidy => F::Tidy,
                    _ => F::ToggleHidden,
                };
                return self.update(Message::Dashboard { layout_id: None, event: dashboard::Message::Float(m) });
            }
            Cmd::PageMenu => return self.page_action(ws::pages::PageMsg::ToggleMenu),
            Cmd::PageNew => return self.page_action(ws::pages::PageMsg::NewBlank),
            Cmd::PageDuplicate => return self.page_action(ws::pages::PageMsg::Duplicate),
            Cmd::PageClose => return self.page_action(ws::pages::PageMsg::Close),
            Cmd::PageUndo => return self.page_action(ws::pages::PageMsg::Undo),
            Cmd::PageReset => return self.page_action(ws::pages::PageMsg::ResetDefault),
            Cmd::PageSaveTemplate => return self.page_action(ws::pages::PageMsg::SaveTemplate),
            Cmd::PageLock => return self.page_action(ws::pages::PageMsg::ToggleLock),
            Cmd::NextPage | Cmd::PrevPage => {
                let pages = self.current_pages();
                let cur = self.layout_manager.active_layout_id().map(|l| l.unique);
                if pages.len() > 1 {
                    let at = pages.iter().position(|(u, _)| Some(*u) == cur).unwrap_or(0);
                    let n = pages.len();
                    let to = if matches!(cmd, Cmd::NextPage) { (at + 1) % n } else { (at + n - 1) % n };
                    return self.update(Message::Layouts(modal::layout_manager::Message::SelectActive(pages[to].0)));
                }
            }
            Cmd::Workspace(name) => {
                // 工作区名 → 回到它上次停留的页面；也接受具体页面的布局名
                let uid = if name.contains(ws::workspace::PAGE_SEP) {
                    self.layout_manager.layouts.iter().find(|l| l.id.name == name).map(|l| l.id.unique)
                } else {
                    self.workspace_target(&name)
                };
                if let Some(uid) = uid {
                    return self.update(Message::Layouts(
                        modal::layout_manager::Message::SelectActive(uid),
                    ));
                }
            }
            Cmd::Theme(t) => ui::update(|p| p.theme = t),
            Cmd::CycleTheme => {
                ui::cycle_theme();
                self.notifications
                    .push(Toast::info(format!("主题：{}", ui::theme_id().label())));
            }
            Cmd::Density(d) => ui::update(|p| p.density = d),
            Cmd::CycleDensity => {
                ui::cycle_density();
                self.notifications
                    .push(Toast::info(format!("密度：{}", ui::density().label())));
            }
            Cmd::ToggleUpDown => ui::update(|p| {
                p.up_down = match p.up_down {
                    ui::UpDown::International => ui::UpDown::China,
                    ui::UpDown::China => ui::UpDown::International,
                }
            }),
            Cmd::ToggleCvd => ui::update(|p| p.cvd_safe = !p.cvd_safe),
            Cmd::ToggleHideValues => ui::update(|p| p.hide_values = !p.hide_values),
            Cmd::UpDown(v) => ui::update(|p| p.up_down = v),
            Cmd::Cvd(v) => ui::update(|p| p.cvd_safe = v),
            Cmd::HideValues(v) => ui::update(|p| p.hide_values = v),
            Cmd::SystemComfortable(v) => ui::update(|p| p.system_comfortable = v),
            Cmd::HeatmapScale(v) => ui::update(|p| p.heatmap_scale = v.to_string()),
            Cmd::TogglePalette => {
                if self.shell.palette.take().is_none() {
                    self.refresh_palette_entries();
                    self.shell.palette = Some(ui::shell::Palette::default());
                    return iced::widget::operation::focus(ui::shell::palette_input_id());
                }
            }
            Cmd::PaletteScope(prefix) => {
                self.refresh_palette_entries();
                self.shell.palette = Some(ui::shell::Palette { query: prefix.to_string(), sel: 0 });
                return iced::widget::operation::focus(ui::shell::palette_input_id());
            }
            Cmd::FocusPane(i) => {
                let dashboard = self.active_dashboard_mut();
                let mut panes: Vec<pane_grid::Pane> = dashboard.panes.iter().map(|(p, _)| *p).collect();
                panes.sort();
                if let Some(p) = panes.get(i) {
                    dashboard.focus = Some((main, *p));
                }
            }
            Cmd::ToggleSidebar => self.shell.sidebar_hidden = !self.shell.sidebar_hidden,
            Cmd::ToggleDataTable => {
                let dashboard = self.active_dashboard_mut();
                let focus = dashboard.focus.filter(|(w, _)| *w == main).map(|(_, p)| p);
                let note = match focus.and_then(|p| dashboard.panes.get_mut(p)) {
                    Some(st)
                        if matches!(
                            st.content,
                            dashboard::pane::Content::Kline { .. }
                                | dashboard::pane::Content::TardisBoard(_)
                                | dashboard::pane::Content::WealthSpring(data::layout::pane::WsPaneMode::SelfChart)
                        ) =>
                    {
                        st.table_view = !st.table_view;
                        None
                    }
                    Some(_) => Some("这个面板没有数据表视图：K 线、Tardis 历史面板、自有数据图有；盘口与逐笔本身就是表格"),
                    None => Some("先聚焦一个图表面板（点一下或按 F6）"),
                };
                if let Some(n) = note {
                    self.notifications.push(Toast::info(n.to_string()));
                }
            }
            Cmd::OpenUrl(url) => return self.update(Message::OpenUrlRequested(url.into())),
            Cmd::ZoomIn | Cmd::ZoomOut | Cmd::ZoomReset => {
                let cur: f32 = self.ui_scale_factor.into();
                let next = match cmd {
                    Cmd::ZoomIn => cur + 0.1,
                    Cmd::ZoomOut => cur - 0.1,
                    _ => 1.0,
                };
                return self.update(Message::ScaleFactorChanged(next.into()));
            }
            Cmd::ToggleBottom => self.shell.bottom = !self.shell.bottom,
            Cmd::ToggleInspector => self.shell.inspector = !self.shell.inspector,
            Cmd::BottomTab(tab) => {
                self.shell.bottom = true;
                self.shell.bottom_tab = tab;
                self.shell.log.refresh();
                self.shell.alerts.refresh();
            }
            Cmd::FocusNextPane | Cmd::FocusPrevPane => {
                let step: i64 = if cmd == Cmd::FocusNextPane { 1 } else { -1 };
                let dashboard = self.active_dashboard_mut();
                let mut panes: Vec<pane_grid::Pane> =
                    dashboard.panes.iter().map(|(p, _)| *p).collect();
                panes.sort();
                if !panes.is_empty() {
                    let cur = dashboard
                        .focus
                        .filter(|(w, _)| *w == main)
                        .and_then(|(_, p)| panes.iter().position(|x| *x == p));
                    let n = panes.len() as i64;
                    let next = match cur {
                        Some(i) => (i as i64 + step).rem_euclid(n) as usize,
                        None => 0,
                    };
                    dashboard.focus = Some((main, panes[next]));
                }
            }
            Cmd::ToggleMaximize => {
                let dashboard = self.active_dashboard_mut();
                if dashboard.panes.maximized().is_some() {
                    dashboard.panes.restore();
                } else if let Some((w, p)) = dashboard.focus
                    && w == main
                {
                    dashboard.panes.maximize(p);
                }
            }
            Cmd::OpenSettings => self.sidebar.set_menu(Some(sidebar::Menu::Settings)),
            Cmd::OpenLayouts => self.sidebar.set_menu(Some(sidebar::Menu::Layout)),
            Cmd::OpenDataFolder => return self.update(Message::DataFolderRequested),
            Cmd::OpenInStudio => match ws::bridge::open_in_studio() {
                Ok(m) => self.notifications.push(Toast::info(m)),
                Err(m) => self.notifications.push(Toast::warn(m)),
            },
        }
        Task::none()
    }

    /// 通知中心：把提示历史抄进外壳（底部面板开在「通知」页时，每个 Tick 一次；最多 200 条）。
    fn refresh_notices(&mut self) {
        use crate::widget::toast::Status;
        self.shell.notices = self
            .notifications
            .history()
            .map(|(t, toast)| {
                let level = match toast.status() {
                    Status::Danger => ui::shell::Level::Error,
                    Status::Warning => ui::shell::Level::Warn,
                    _ => ui::shell::Level::Info,
                };
                (t.clone(), toast.title().to_string(), toast.body().to_string(), level)
            })
            .collect();
    }

    /// 命令面板条目 = 注册表 + 当前工作区的面板（按面板次序，与 F6 / Ctrl 1–9 同一顺序）。
    fn refresh_palette_entries(&mut self) {
        let dashboard = self.active_dashboard();
        let mut panes: Vec<(pane_grid::Pane, String)> =
            dashboard.panes.iter().map(|(p, st)| (*p, st.content.to_string())).collect();
        panes.sort_by_key(|(p, _)| *p);
        let names: Vec<String> = panes.into_iter().map(|(_, n)| n).collect();
        let mut v = self.commands.clone();
        v.extend(ui::command::pane_entries(&names));
        self.palette_entries = v;
    }

    /// 外壳需要的一帧信息（命令栏、状态栏、检查器）。
    fn shell_info(&self) -> ui::shell::Info {
        let badge = ws::provenance::badge(ws::workspace::replay_mode());
        let current = ws::active_run::current();
        let run = match &current {
            Some(ar) if ar.mode != "stopped" => {
                let mut s = format!("run {}", ar.run_id);
                if !ar.symbol.is_empty() {
                    s.push_str(&format!(" · {}", ar.symbol));
                }
                if !ar.source.is_empty() {
                    s.push_str(&format!(" · {}", ar.source));
                }
                s
            }
            _ => String::new(),
        };
        let mut activity = Vec::new();
        if let Some(ar) = current.filter(|a| a.mode == "backtest" || a.mode == "live") {
            let what = if ar.mode == "live" { "模拟盘运行" } else { "回测运行" };
            let src = if ar.source.is_empty() { "来源未声明".to_string() } else { ar.source.clone() };
            activity.push(format!("● {what} · run {} · {} · {src}", ar.run_id, ar.symbol));
        }
        if ws::feature_source::chart_override().is_some() {
            activity.push("● 特征回放进行中（订单流特征工作区）".to_string());
        }
        let dashboard = self.active_dashboard();
        let focused = dashboard
            .focus
            .filter(|(w, _)| *w == self.main_window.id)
            .and_then(|(_, p)| dashboard.panes.get(p))
            .map(|st| st.content.to_string());
        // 命令栏的数据链路（docs/35 §5.1）：聚焦面板吃行情就取它，否则取第一个吃行情的面板；
        // 与面板标题上的链路徽标同一份计算（ws::provenance::link）
        let link_pane = dashboard
            .focus
            .filter(|(w, _)| *w == self.main_window.id)
            .and_then(|(_, p)| dashboard.panes.get(p))
            .and_then(|st| st.stream_pair())
            .or_else(|| dashboard.panes.iter().find_map(|(_, st)| st.stream_pair()));
        let link = link_pane.map(|ti| {
            let l = ws::provenance::link(
                ws::workspace::replay_mode(),
                &ti.ticker.exchange.venue().to_string(),
                &ti.ticker.display_symbol_and_type().0,
            );
            let tone = match l.tone {
                ws::provenance::LinkTone::Ok => ui::widgets::Tone::Success,
                ws::provenance::LinkTone::Running => ui::widgets::Tone::Info,
                ws::provenance::LinkTone::Warn | ws::provenance::LinkTone::Idle => ui::widgets::Tone::Warning,
                ws::provenance::LinkTone::Bad => ui::widgets::Tone::Danger,
            };
            (format!("⇢ {}", l.label), l.detail, tone)
        });
        ui::shell::Info {
            alerts_unseen: self.shell.alerts.unseen(),
            link,
            workspace: self
                .layout_manager
                .active_layout_id()
                .map(|l| {
                    let w = ws::workspace::workspace_of(&l.name);
                    if w == l.name { w.to_string() } else { format!("{w} · {}", ws::workspace::page_title(&l.name)) }
                })
                .unwrap_or_default(),
            env: ui::shell::Env::from_badge(&badge.label),
            env_label: badge.label,
            env_detail: badge.detail,
            run,
            streams_on: ws::egress::streams_enabled(),
            // 断开超过 3 秒才算（正常的断线重连一两秒就好，不该闪）
            streams_down: self
                .stream_down
                .iter()
                .filter(|(_, (t0, _))| t0.elapsed().as_secs() >= 3)
                .map(|(k, (_, why))| (k.clone(), why.clone()))
                .collect(),
            wire: ws::egress::wire_rate(),
            egress_conns: ws::egress::external_conns(),
            focused,
            pane_count: dashboard.panes.len(),
            activity,
        }
    }

    fn load_layout(&mut self, layout_uid: uuid::Uuid, main_window: window::Id) -> Task<Message> {
        if let Some(l) = self.layout_manager.layouts.iter().find(|l| l.id.unique == layout_uid) {
            ui::perf::switch_started(&l.id.name);
            // 工作区缺省密度（docs/35 §5.3）：「系统」组在紧凑下改用舒适
            let wsn = ws::workspace::workspace_of(&l.id.name).to_string();
            let system = ws::workspace::GROUPS.iter().any(|(g, names)| *g == "系统" && names.contains(&wsn.as_str()));
            // 改名框跟着当前页走（不然会把上一页的名字套到这一页上）
            self.page_rename = ws::workspace::page_title(&l.id.name);
            self.last_page.insert(wsn, layout_uid);
            if ui::set_system_workspace(system) {
                self.apply_ui_change();
            }
        }

        if let Err(err) = self.layout_manager.set_active_layout(layout_uid) {
            log::error!("Failed to set active layout: {}", err);
            return Task::none();
        }

        self.layout_manager
            .park_inactive_layouts(layout_uid, main_window);

        self.layout_manager
            .get_mut(layout_uid)
            .map(|layout| {
                layout
                    .dashboard
                    .load_layout(main_window)
                    .map(move |msg| Message::Dashboard {
                        layout_id: Some(layout_uid),
                        event: msg,
                    })
            })
            .unwrap_or_else(|| {
                log::error!("Active layout missing after selection: {}", layout_uid);
                Task::none()
            })
    }

    fn view_with_modal<'a>(
        &'a self,
        base: Element<'a, Message>,
        dashboard: &'a Dashboard,
        menu: sidebar::Menu,
    ) -> Element<'a, Message> {
        let sidebar_pos = self.sidebar.position();

        match menu {
            sidebar::Menu::Settings => {
                let settings_modal = {
                    // 设置窗口（docs/35 §10 最后一行，UPDS V5 §28）：可搜索、每项写明值从哪来、改过的能单项重置。
                    // 外观类偏好在 ~/.config/wealthspring/ui.json（与 Studio 共用，改了两边同时变）；
                    // 其余在本机状态文件（saved-state.json）。主题编辑器已下线：自定义颜色会绕过对比度与色弱检查。
                    use ui::command::Cmd;
                    let p = ui::prefs();
                    let d = wealthspring_ui_tokens::Prefs::default();
                    let run = Message::RunCommand;
                    let heat = match p.heatmap_scale.as_str() { "viridis" => "viridis", "cividis" => "cividis", _ => "inferno" };
                    let heat_def: &'static str = match d.heatmap_scale.as_str() { "viridis" => "viridis", "cividis" => "cividis", _ => "inferno" };
                    let toggle_network_editor = button(text("网络与代理…")).on_press(Message::Sidebar(
                        dashboard::sidebar::Message::ToggleSidebarMenu(Some(
                            sidebar::Menu::Network,
                        )),
                    ));

                    // 声音面板从侧栏移到这里（侧栏只留工作区 + 搜索标的 / 布局 / 设置）
                    let toggle_audio_editor = button(text("声音与成交提示音…")).on_press(Message::Sidebar(
                        dashboard::sidebar::Message::ToggleSidebarMenu(Some(sidebar::Menu::Audio)),
                    ));

                    let timezone_picklist = pick_list(
                        [data::UserTimezone::Utc, data::UserTimezone::Local],
                        Some(self.timezone),
                        Message::SetTimezone,
                    );

                    let size_in_quote_currency_checkbox = {
                        let is_active = match self.volume_size_unit {
                            exchange::SizeUnit::Quote => true,
                            exchange::SizeUnit::Base => false,
                        };

                        let checkbox = iced::widget::checkbox(is_active)
                            .label("量按计价币显示")
                            .on_toggle(|checked| {
                                let on_dialog_confirm = Message::ApplyVolumeSizeUnit(if checked {
                                    exchange::SizeUnit::Quote
                                } else {
                                    exchange::SizeUnit::Base
                                });

                                let confirm_dialog = screen::ConfirmDialog::new(
                                    "Changing size display currency requires application restart"
                                        .to_string(),
                                    Box::new(on_dialog_confirm.clone()),
                                )
                                .with_confirm_btn_text("Restart now".to_string());

                                Message::ToggleDialogModal(Some(confirm_dialog))
                            });

                        tooltip(
                            checkbox,
                            Some(
                                "量与成交额按计价币（USD）显示\n对反向永续与持仓量无效；改了要重启",
                            ),
                            TooltipPosition::Top,
                        )
                    };

                    let sidebar_pos_picklist = pick_list(
                        [sidebar::Position::Left, sidebar::Position::Right],
                        Some(sidebar_pos),
                        |pos| {
                            Message::Sidebar(dashboard::sidebar::Message::SetSidebarPosition(pos))
                        },
                    );

                    let scale_factor = {
                        let current_value: f32 = self.ui_scale_factor.into();

                        let decrease_btn = if current_value > data::config::MIN_SCALE {
                            button(text("-"))
                                .on_press(Message::ScaleFactorChanged((current_value - 0.1).into()))
                        } else {
                            button(text("-"))
                        };

                        let increase_btn = if current_value < data::config::MAX_SCALE {
                            button(text("+"))
                                .on_press(Message::ScaleFactorChanged((current_value + 0.1).into()))
                        } else {
                            button(text("+"))
                        };

                        container(
                            row![
                                decrease_btn,
                                text(format!("{:.0}%", current_value * 100.0))
                                    .size(crate::style::text_size::SECTION),
                                increase_btn,
                            ]
                            .align_y(Alignment::Center)
                            .spacing(8)
                            .padding(crate::ui::metrics::space(1)),
                        )
                        .style(style::modal_container)
                    };

                    let trade_fetch_checkbox = {
                        let is_active = connector::fetcher::is_trade_fetch_enabled();

                        let checkbox = iced::widget::checkbox(is_active)
                            .label("补拉逐笔成交（Binance）")
                            .on_toggle(|checked| {
                                if checked {
                                    let confirm_dialog = screen::ConfirmDialog::new(
                                        "This might be unreliable and take some time to complete. Proceed?"
                                            .to_string(),
                                        Box::new(Message::ToggleTradeFetch(true)),
                                    );
                                    Message::ToggleDialogModal(Some(confirm_dialog))
                                } else {
                                    Message::ToggleTradeFetch(false)
                                }
                            });

                        tooltip(
                            checkbox,
                            Some("为足迹图补拉历史逐笔（实验性，可能慢且不稳）"),
                            TooltipPosition::Top,
                        )
                    };

                    let open_data_folder = {
                        let button =
                            button(text("打开数据文件夹")).on_press(Message::DataFolderRequested);

                        tooltip(
                            button,
                            Some("数据与配置所在的文件夹"),
                            TooltipPosition::Top,
                        )
                    };

                    let version_info = {
                        let (version_label, commit_label) = version::app_build_version_parts();

                        let github_link_button =
                            button(text(version_label).size(crate::style::text_size::EMPHASIS))
                                .padding(iced::Padding::ZERO)
                                .style(style::button::text_link)
                                .on_press(Message::OpenUrlRequested(Cow::Borrowed(
                                    version::GITHUB_REPOSITORY_URL,
                                )));

                        let github_button: Element<'_, Message> = iced::widget::tooltip(
                            github_link_button,
                            container(
                                row![
                                    text("GitHub"),
                                    style::icon_text(style::Icon::ExternalLink, 12),
                                ]
                                .spacing(4)
                                .align_y(Alignment::Center),
                            )
                            .style(style::tooltip)
                            .padding(crate::ui::metrics::space(3)),
                            TooltipPosition::Top,
                        )
                        .into();

                        if let (Some(commit_label), Some(commit_url)) =
                            (commit_label, version::build_commit_url())
                        {
                            let commit_button =
                                button(text(commit_label).size(crate::style::text_size::SMALL))
                                    .padding(iced::Padding::ZERO)
                                    .style(style::button::text_link_secondary)
                                    .on_press(Message::OpenUrlRequested(Cow::Owned(commit_url)));

                            column![github_button, commit_button]
                                .spacing(2)
                                .align_x(Alignment::End)
                                .into()
                        } else {
                            github_button
                        }
                    };

                    let footer = column![
                        container(version_info)
                            .width(iced::Length::Fill)
                            .align_x(Alignment::End),
                    ]
                    .spacing(8);

                    // 每一项：(名称, 搜索关键词, 改过没有, 存在哪, 重置消息, 控件)
                    let tz_def = data::UserTimezone::default();
                    let scale_now: f32 = self.ui_scale_factor.into();
                    let items: Vec<(&str, &str, bool, &str, Option<Message>, Element<'_, Message>)> = vec![
                        ("主题", "theme 深色 浅色 oled 高对比 外观", p.theme != d.theme, "ui.json", Some(run(Cmd::Theme(d.theme))),
                            ui::widgets::segmented(&[("深色", ui::ThemeId::Dark), ("浅色", ui::ThemeId::Light), ("OLED", ui::ThemeId::OledDark), ("高对比", ui::ThemeId::HighContrast)], &p.theme, move |t| run(Cmd::Theme(t)))),
                        ("密度", "density 紧凑 舒适 宽松 外观", p.density != d.density, "ui.json", Some(run(Cmd::Density(d.density))),
                            ui::widgets::segmented(&[("紧凑", ui::Density::Compact), ("舒适", ui::Density::Comfortable), ("宽松", ui::Density::Spacious)], &p.density, move |v| run(Cmd::Density(v)))),
                        ("系统组工作区用舒适密度", "density 密度 舒适 资源 新闻 系统", p.system_comfortable != d.system_comfortable, "ui.json", Some(run(Cmd::SystemComfortable(d.system_comfortable))),
                            ui::widgets::segmented(&[("关", false), ("开", true)], &p.system_comfortable, move |v| run(Cmd::SystemComfortable(v)))),
                        ("涨跌颜色", "绿涨红跌 红涨绿跌 颜色 外观", p.up_down != d.up_down, "ui.json", Some(run(Cmd::UpDown(d.up_down))),
                            ui::widgets::segmented(&[("绿涨红跌", ui::UpDown::International), ("红涨绿跌", ui::UpDown::China)], &p.up_down, move |v| run(Cmd::UpDown(v)))),
                        ("色弱安全配色", "cvd 色盲 颜色 外观", p.cvd_safe != d.cvd_safe, "ui.json", Some(run(Cmd::Cvd(d.cvd_safe))),
                            ui::widgets::segmented(&[("关", false), ("开", true)], &p.cvd_safe, move |v| run(Cmd::Cvd(v)))),
                        ("隐藏数值（演示 / 截图）", "hide 隐私 演示 截图 金额", p.hide_values != d.hide_values, "ui.json", Some(run(Cmd::HideValues(d.hide_values))),
                            ui::widgets::segmented(&[("关", false), ("开", true)], &p.hide_values, move |v| run(Cmd::HideValues(v)))),
                        ("热图色阶", "heatmap inferno viridis cividis 颜色", heat != heat_def, "ui.json", Some(run(Cmd::HeatmapScale(heat_def))),
                            ui::widgets::segmented(&[("inferno", "inferno"), ("viridis", "viridis"), ("cividis", "cividis")], &heat, move |v| run(Cmd::HeatmapScale(v)))),
                        ("界面缩放", "scale 缩放 字号 ctrl", (scale_now - 1.0).abs() > 1e-3, "本机状态", Some(Message::ScaleFactorChanged(1.0.into())), scale_factor.into()),
                        ("时区", "timezone utc 本地 时间", self.timezone != tz_def, "本机状态", Some(Message::SetTimezone(tz_def)), timezone_picklist.into()),
                        ("侧栏位置", "sidebar 左 右", sidebar_pos != sidebar::Position::default(), "本机状态",
                            Some(Message::Sidebar(dashboard::sidebar::Message::SetSidebarPosition(sidebar::Position::default()))), sidebar_pos_picklist.into()),
                        // 改计价单位要重启，不给一键重置（免得误点就重启）
                        ("量的计价单位", "size quote base usd 成交额", self.volume_size_unit != exchange::SizeUnit::default(), "本机状态", None, size_in_quote_currency_checkbox.into()),
                        ("补拉逐笔成交（实验）", "trades fetch footprint 足迹 实验", connector::fetcher::is_trade_fetch_enabled(), "本机状态",
                            Some(Message::ToggleTradeFetch(false)), trade_fetch_checkbox.into()),
                        ("声音", "audio sound 声音 音量 提示音 成交", false, "", None, toggle_audio_editor.into()),
                        ("网络与代理", "network proxy 代理 网络", false, "", None, toggle_network_editor.into()),
                        ("数据文件夹", "data folder 文件夹 配置", false, "", None, open_data_folder.into()),
                    ];
                    let q = self.settings_query.trim().to_lowercase();
                    let mut list = column![
                        iced::widget::text_input("搜索设置……", &self.settings_query)
                            .on_input(Message::SettingsQuery)
                            .padding(ui::metrics::space(2))
                            .size(ui::text::s_small()),
                    ]
                    .spacing(14);
                    let mut shown = 0;
                    for (label, keys, changed, store, reset, body) in items {
                        if !q.is_empty() && !label.to_lowercase().contains(&q) && !keys.to_lowercase().contains(&q) {
                            continue;
                        }
                        shown += 1;
                        // 值的来源：缺省 / 已改（存在哪）——UPDS V5 §28「每项显示值来源」
                        let src = if changed { format!("已改 · {store}") } else if store.is_empty() { String::new() } else { "缺省".into() };
                        let mut head = row![ui::text::caption(label), iced::widget::space::horizontal(), ui::text::caption(src).color(if changed { ui::pal::warn() } else { ui::pal::dim() })]
                            .spacing(6)
                            .align_y(Alignment::Center);
                        if changed && let Some(m) = reset {
                            head = head.push(
                                button(ui::text::caption("↺ 重置"))
                                    .padding(crate::ui::metrics::pad2(0, 1))
                                    .on_press(m)
                                    .style(|th, st| style::button::transparent(th, st, false)),
                            );
                        }
                        list = list.push(column![head, body].spacing(4));
                    }
                    if shown == 0 {
                        list = list.push(ui::text::caption(format!("没有和「{}」相关的设置", self.settings_query.trim())));
                    }
                    let column_content = column![list, footer].spacing(16).align_x(Alignment::Start);

                    let content = scrollable::Scrollable::with_direction(
                        column_content,
                        scrollable::Direction::Vertical(
                            scrollable::Scrollbar::new().width(8).scroller_width(6),
                        ),
                    );

                    container(content)
                        .align_x(Alignment::Start)
                        .max_width(320) // 外观一组的四段分段按钮约需 300px，240 会把「高对比」挤成三行
                        .padding(crate::ui::metrics::space(6))
                        .style(style::dashboard_modal)
                };

                let (align_x, padding) = match sidebar_pos {
                    sidebar::Position::Left => (Alignment::Start, padding::left(44).bottom(4)),
                    sidebar::Position::Right => (Alignment::End, padding::right(44).bottom(4)),
                };

                let base_content = dashboard_modal(
                    base,
                    settings_modal,
                    Message::Sidebar(dashboard::sidebar::Message::ToggleSidebarMenu(None)),
                    padding,
                    Alignment::End,
                    align_x,
                );

                if let Some(dialog) = &self.confirm_dialog {
                    let dialog_content =
                        confirm_dialog_container(dialog.clone(), Message::ToggleDialogModal(None));

                    main_dialog_modal(
                        base_content,
                        dialog_content,
                        Message::ToggleDialogModal(None),
                    )
                } else {
                    base_content
                }
            }
            sidebar::Menu::Layout => {
                let main_window = self.main_window.id;

                let manage_pane = if let Some((window_id, pane_id)) = dashboard.focus {
                    let selected_pane_str =
                        if let Some(state) = dashboard.get_pane(main_window, window_id, pane_id) {
                            let link_group_name: String =
                                state.link_group.as_ref().map_or_else(String::new, |g| {
                                    " - Group ".to_string() + &g.to_string()
                                });

                            state.content.to_string() + &link_group_name
                        } else {
                            "".to_string()
                        };

                    let is_main_window = window_id == main_window;

                    let reset_pane_button = {
                        let btn = button(text("Reset").align_x(Alignment::Center))
                            .width(iced::Length::Fill);
                        if is_main_window {
                            let dashboard_msg = Message::Dashboard {
                                layout_id: None,
                                event: dashboard::Message::Pane(
                                    main_window,
                                    dashboard::pane::Message::ReplacePane(pane_id),
                                ),
                            };

                            btn.on_press(dashboard_msg)
                        } else {
                            btn
                        }
                    };
                    let split_pane_button = {
                        let btn = button(text("Split").align_x(Alignment::Center))
                            .width(iced::Length::Fill);
                        if is_main_window {
                            let dashboard_msg = Message::Dashboard {
                                layout_id: None,
                                event: dashboard::Message::Pane(
                                    main_window,
                                    dashboard::pane::Message::SplitPane(
                                        pane_grid::Axis::Horizontal,
                                        pane_id,
                                    ),
                                ),
                            };
                            btn.on_press(dashboard_msg)
                        } else {
                            btn
                        }
                    };

                    column![
                        text(selected_pane_str),
                        row![
                            tooltip(
                                reset_pane_button,
                                if is_main_window {
                                    Some("Reset selected pane")
                                } else {
                                    None
                                },
                                TooltipPosition::Top,
                            ),
                            tooltip(
                                split_pane_button,
                                if is_main_window {
                                    Some("Split selected pane horizontally")
                                } else {
                                    None
                                },
                                TooltipPosition::Top,
                            ),
                        ]
                        .spacing(8)
                    ]
                    .spacing(8)
                } else {
                    column![text("No pane selected"),].spacing(8)
                };

                let manage_layout_modal = {
                    let col = column![
                        manage_pane,
                        rule::horizontal(1.0).style(style::split_ruler),
                        self.layout_manager.view().map(Message::Layouts)
                    ];

                    container(col.align_x(Alignment::Center).spacing(20))
                        .width(260)
                        .padding(crate::ui::metrics::space(6))
                        .style(style::dashboard_modal)
                };

                let (align_x, padding) = match sidebar_pos {
                    sidebar::Position::Left => (Alignment::Start, padding::left(44).bottom(4)),
                    sidebar::Position::Right => (Alignment::End, padding::right(44).bottom(4)),
                };

                dashboard_modal(
                    base,
                    manage_layout_modal,
                    Message::Sidebar(dashboard::sidebar::Message::ToggleSidebarMenu(None)),
                    padding,
                    Alignment::End,
                    align_x,
                )
            }
            sidebar::Menu::Audio => {
                let (align_x, padding) = match sidebar_pos {
                    sidebar::Position::Left => (Alignment::Start, padding::left(44).bottom(4)),
                    sidebar::Position::Right => (Alignment::End, padding::right(44).bottom(4)),
                };

                let trade_streams_list = dashboard.streams.trade_streams(None);

                dashboard_modal(
                    base,
                    self.audio_stream
                        .view(trade_streams_list)
                        .map(Message::AudioStream),
                    Message::Sidebar(dashboard::sidebar::Message::ToggleSidebarMenu(None)),
                    padding,
                    Alignment::End,
                    align_x,
                )
            }
            sidebar::Menu::ThemeEditor => {
                let (align_x, padding) = match sidebar_pos {
                    sidebar::Position::Left => (Alignment::Start, padding::left(44).bottom(4)),
                    sidebar::Position::Right => (Alignment::End, padding::right(44).bottom(4)),
                };

                dashboard_modal(
                    base,
                    self.theme_editor
                        .view(&self.theme.0)
                        .map(Message::ThemeEditor),
                    Message::Sidebar(dashboard::sidebar::Message::ToggleSidebarMenu(None)),
                    padding,
                    Alignment::End,
                    align_x,
                )
            }
            sidebar::Menu::Network => {
                let (align_x, padding) = match sidebar_pos {
                    sidebar::Position::Left => (Alignment::Start, padding::left(44).bottom(4)),
                    sidebar::Position::Right => (Alignment::End, padding::right(44).bottom(4)),
                };

                let base_content = dashboard_modal(
                    base,
                    self.network.view().map(Message::NetworkManager),
                    Message::Sidebar(dashboard::sidebar::Message::ToggleSidebarMenu(None)),
                    padding,
                    Alignment::End,
                    align_x,
                );

                if let Some(dialog) = &self.confirm_dialog {
                    let dialog_content =
                        confirm_dialog_container(dialog.clone(), Message::ToggleDialogModal(None));

                    main_dialog_modal(
                        base_content,
                        dialog_content,
                        Message::ToggleDialogModal(None),
                    )
                } else {
                    base_content
                }
            }
        }
    }

    fn save_state_to_disk(&mut self, windows: &HashMap<window::Id, WindowSpec>) {
        self.active_dashboard_mut()
            .popout
            .iter_mut()
            .for_each(|(id, (_, window_spec))| {
                if let Some(new_window_spec) = windows.get(id) {
                    *window_spec = *new_window_spec;
                }
            });

        self.sidebar.sync_tickers_table_settings();

        let mut ser_layouts = vec![];
        for layout in &self.layout_manager.layouts {
            if let Some(layout) = self.layout_manager.get(layout.id.unique) {
                let serialized_dashboard = data::Dashboard::from(&layout.dashboard);
                ser_layouts.push(data::Layout {
                    name: layout.id.name.clone(),
                    dashboard: serialized_dashboard,
                });
            }
        }

        let layouts = data::Layouts {
            layouts: ser_layouts,
            active_layout: self
                .layout_manager
                .active_layout_id()
                .map(|layout| layout.name.to_string())
                .clone(),
        };

        let main_window_spec = windows
            .iter()
            .find(|(id, _)| **id == self.main_window.id)
            .map(|(_, spec)| *spec);

        let audio_cfg = data::AudioStream::from(&self.audio_stream);

        let proxy_cfg_persisted = self.network.proxy_cfg().map(|p| p.without_auth());

        let state = data::State::from_parts(
            layouts,
            self.theme.clone(),
            self.theme_editor.custom_theme.clone().map(data::Theme),
            main_window_spec,
            self.timezone,
            self.sidebar.state.clone(),
            self.ui_scale_factor,
            audio_cfg,
            connector::fetcher::is_trade_fetch_enabled(),
            self.volume_size_unit,
            proxy_cfg_persisted,
        );

        match serde_json::to_string(&state).map(ws::feature_source::scrub_saved) {
            Ok(layout_str) if layout_str == self.last_saved => {}
            Ok(layout_str) => {
                let file_name = data::SAVED_STATE_PATH;
                self.last_saved = layout_str.clone();
                if let Err(e) = data::write_json_to_file(&layout_str, file_name) {
                    log::error!("Failed to write layout state to file: {}", e);
                } else {
                    log::info!("Persisted state to {file_name}");
                }
            }
            Err(e) => log::error!("Failed to serialize layout: {}", e),
        }
    }

    /// 主窗口 + 当前工作区的全部弹出窗口（存盘要取它们的位置）。
    fn all_window_ids(&self) -> Vec<window::Id> {
        let mut v: Vec<window::Id> = self.active_dashboard().popout.keys().copied().collect();
        v.push(self.main_window.id);
        v
    }

    fn restart(&mut self) -> Task<Message> {
        let mut windows_to_close: Vec<window::Id> =
            self.active_dashboard().popout.keys().copied().collect();
        windows_to_close.push(self.main_window.id);

        let close_windows = Task::batch(
            windows_to_close
                .into_iter()
                .map(window::close)
                .collect::<Vec<_>>(),
        );

        let (new_state, init_task) = Flowsurface::new();
        *self = new_state;

        close_windows.chain(init_task)
    }
}
