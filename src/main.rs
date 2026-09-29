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
        keyboard::Key::Named(Named::F6) => Some(if shift {
            Cmd::FocusPrevPane
        } else {
            Cmd::FocusNextPane
        }),
        keyboard::Key::Character(c) if ctrl => {
            let c = c.to_ascii_lowercase();
            match (shift, alt, c.as_str()) {
                (false, false, "k") => Some(Cmd::TogglePalette),
                (false, false, "j") => Some(Cmd::ToggleBottom),
                (false, false, "i") => Some(Cmd::ToggleInspector),
                (false, false, ",") => Some(Cmd::OpenSettings),
                (true, false, "m") => Some(Cmd::ToggleMaximize),
                (false, true, "t") => Some(Cmd::CycleTheme),
                (false, true, "d") => Some(Cmd::CycleDensity),
                // Ctrl Shift 1–5：各组的第一个工作区
                (true, false, d @ ("1" | "2" | "3" | "4" | "5")) => {
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
    ws_orders: ws::orders::OrderState,            // WealthSpring 订单/PnL（events.* 聚合，F3）
    ws_flow: ws::flow::FlowState,                 // WealthSpring 订单流：CVD/不平衡/背离（F4a）
    ws_factory: ws::factory::FactoryPool,         // WealthSpring Factory 现役池（F4c）
    ws_signals: Option<ws::signals::Signals>,     // WealthSpring 引擎信号：吸收/撤补/冰山（F4b–d 精确版）
    specimen: Option<ws::specimen::Specimen>,     // 界面样张模式（docs/35）：轮换工作区自截图
    shell: ui::shell::Shell,                      // 外壳（docs/35 批 3）：命令面板 / 底部面板 / 检查器
    commands: Vec<ui::command::Entry>,            // 命令注册表（UPDS V2 §12）
    gallery: Option<ui::gallery::Gallery>,        // 组件样张页（样张模式 WS_UI_SPECIMEN_COMPONENTS）
}

#[derive(Debug, Clone)]
enum Message {
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
    GoBack,
    DataFolderRequested,
    OpenUrlRequested(Cow<'static, str>),
    ScaleFactorChanged(data::ScaleFactor),
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
            let (position, size) = saved_state.window();
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
            ws_orders: ws::orders::OrderState::default(),
            ws_flow: ws::flow::FlowState::default(),
            ws_factory: ws::factory::FactoryPool::default(),
            ws_signals: None,
            specimen: ws::specimen::Specimen::from_env(),
            shell: ui::shell::Shell::default(),
            commands: ui::command::registry(&ws::workspace::WORKSPACES),
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
        ws::workspace::ensure_seeded(&mut state.layout_manager);

        // 样张模式下可把外壳的三个浮动区全打开，截图验证它们（docs/35 批 3）
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
            for name in ws::workspace::WORKSPACES {
                if let Some(l) = state.layout_manager.layouts.iter().find(|l| l.id.name == name) {
                    sp.queue.push((l.id.unique, name.to_string()));
                }
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
                    exchange::Event::Connected(_exchange) => {}
                    exchange::Event::Disconnected(exchange, reason) => {
                        log::info!("a stream disconnected from {exchange} WS: {reason:?}");
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
                if let Some(cmd) = self.shell.update(ev, &self.commands) {
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
                // 界面偏好可能被 Studio 改了（每秒最多看一次文件修改时间）
                if ui::poll() {
                    self.apply_ui_change();
                }
                // 底部面板开着才读日志尾巴（每 2 秒一次）
                if self.shell.bottom {
                    self.shell.log.refresh();
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
                    if let Some(l) = self
                        .layout_manager
                        .layouts
                        .iter_mut()
                        .find(|l| l.id.name == ws::workspace::WS_FEATURES)
                    {
                        let _ = l.dashboard.reset_chart_panes_to(main_window_id, Some(ti));
                    }
                }
                // 订单流特征工作区的图上设置（K 线周期、Footprint 失衡阈值）发布给「图表参数」视图比对口径
                if let Some(l) = self.layout_manager.layouts.iter().find(|l| l.id.name == ws::workspace::WS_FEATURES) {
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
                        return self.load_layout(uid, main).chain(scroll);
                    }
                    ws::specimen::Step::Shoot => {
                        return iced::window::screenshot(main).map(Message::SpecimenShot);
                    }
                    ws::specimen::Step::Nothing => {}
                }
            }
            Message::SpecimenShot(shot) => {
                let done = self
                    .specimen
                    .as_mut()
                    .is_some_and(|sp| sp.save(&shot.rgba, shot.size.width, shot.size.height));
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

                            let dashboard = Dashboard::from_config(
                                configuration(ser_dashboard.pane.clone()),
                                popout_windows,
                                old_id,
                            );

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

        let dashboard = self.active_dashboard();
        let sidebar_pos = self.sidebar.position();

        let tickers_table = &self.sidebar.tickers_table;

        let content = if id == self.main_window.id {
            // WealthSpring 工作区（docs/08 F6 — P1）：按固定顺序取 5 个工作区的 (uuid, 名, 是否活动)，
            // 合并进 FS 原生侧边栏顶部（单一侧边栏，图标切换不同窗口）。
            let active_layout = self.layout_manager.active_layout_id().map(|l| l.unique);
            let workspaces: Vec<(uuid::Uuid, &'static str, bool)> = ws::workspace::WORKSPACES
                .iter()
                .filter_map(|&name| {
                    self.layout_manager
                        .layouts
                        .iter()
                        .find(|l| l.id.name == name)
                        .map(|l| (l.id.unique, name, active_layout == Some(l.id.unique)))
                })
                .collect();

            let sidebar_view = self
                .sidebar
                .view(self.audio_stream.volume(), &workspaces)
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
                row![work, ui::shell::inspector(&info).map(Message::Shell)]
                    .spacing(4)
                    .into()
            } else {
                work
            };
            let command_bar = ui::shell::command_bar(&info).map(Message::Shell);
            let status_bar = ui::shell::status_bar(&info).map(Message::Shell);

            let base = column![
                header_title,
                command_bar,
                match sidebar_pos {
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
                        ui::shell::palette_overlay(p, &self.commands).map(Message::Shell);
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
            self.notifications.toasts(),
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

    fn title(&self, _window: window::Id) -> String {
        if let Some(id) = self.layout_manager.active_layout_id() {
            format!("Flowsurface [{}]", id.name)
        } else {
            "Flowsurface".to_string()
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
        let active_ws = self.layout_manager.active_layout_id().map(|l| l.name.clone());
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

        let tick = iced::window::frames().map(Message::Tick);
        let specimen = if self.specimen.is_some() {
            iced::time::every(std::time::Duration::from_millis(500)).map(Message::SpecimenStep)
        } else {
            Subscription::none()
        };

        let hotkeys = iced::event::listen_with(shortcut);

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
        ])
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
            Cmd::Workspace(name) => {
                let uid = self
                    .layout_manager
                    .layouts
                    .iter()
                    .find(|l| l.id.name == name)
                    .map(|l| l.id.unique);
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
            Cmd::HeatmapScale(v) => ui::update(|p| p.heatmap_scale = v.to_string()),
            Cmd::TogglePalette => {
                if self.shell.palette.take().is_none() {
                    self.shell.palette = Some(ui::shell::Palette::default());
                    return iced::widget::operation::focus(ui::shell::palette_input_id());
                }
            }
            Cmd::ToggleBottom => self.shell.bottom = !self.shell.bottom,
            Cmd::ToggleInspector => self.shell.inspector = !self.shell.inspector,
            Cmd::BottomTab(tab) => {
                self.shell.bottom = true;
                self.shell.bottom_tab = tab;
                self.shell.log.refresh();
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
        }
        Task::none()
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
        ui::shell::Info {
            workspace: self
                .layout_manager
                .active_layout_id()
                .map(|l| l.name.clone())
                .unwrap_or_default(),
            env: ui::shell::Env::from_badge(&badge.label),
            env_label: badge.label,
            env_detail: badge.detail,
            run,
            streams_on: ws::egress::streams_enabled(),
            wire: ws::egress::wire_rate(),
            egress_conns: ws::egress::external_conns(),
            focused,
            pane_count: dashboard.panes.len(),
            activity,
        }
    }

    fn load_layout(&mut self, layout_uid: uuid::Uuid, main_window: window::Id) -> Task<Message> {
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
                    // 外观（docs/35 批 7）：与 Studio 共用 ~/.config/wealthspring/ui.json，改了两边同时变。
                    // 取代上游的主题下拉框与主题编辑器——自定义颜色会绕过 token 的对比度与色弱检查。
                    let appearance = {
                        use ui::command::Cmd;
                        let p = ui::prefs();
                        let run = Message::RunCommand;
                        let item = |label: &'static str, body: Element<'static, Message>| -> Element<'static, Message> {
                            column![ui::text::caption(label), body].spacing(4).into()
                        };
                        column![
                            item("主题", ui::widgets::segmented(
                                &[("深色", ui::ThemeId::Dark), ("浅色", ui::ThemeId::Light), ("OLED", ui::ThemeId::OledDark), ("高对比", ui::ThemeId::HighContrast)],
                                &p.theme, move |t| run(Cmd::Theme(t)))),
                            item("密度", ui::widgets::segmented(
                                &[("紧凑", ui::Density::Compact), ("舒适", ui::Density::Comfortable), ("宽松", ui::Density::Spacious)],
                                &p.density, move |d| run(Cmd::Density(d)))),
                            item("涨跌颜色", ui::widgets::segmented(
                                &[("绿涨红跌", ui::UpDown::International), ("红涨绿跌", ui::UpDown::China)],
                                &p.up_down, move |v| run(Cmd::UpDown(v)))),
                            item("色弱安全配色", ui::widgets::segmented(&[("关", false), ("开", true)], &p.cvd_safe, move |v| run(Cmd::Cvd(v)))),
                            item("隐藏数值（演示 / 截图）", ui::widgets::segmented(&[("关", false), ("开", true)], &p.hide_values, move |v| run(Cmd::HideValues(v)))),
                            item("热图色阶", ui::widgets::segmented(
                                &[("inferno", "inferno"), ("viridis", "viridis"), ("cividis", "cividis")],
                                &match p.heatmap_scale.as_str() { "viridis" => "viridis", "cividis" => "cividis", _ => "inferno" },
                                move |v| run(Cmd::HeatmapScale(v)))),
                            ui::text::caption("主题编辑器已下线：自定义颜色会绕过对比度与色弱检查。快捷键 Ctrl Alt T / D 循环主题 / 密度。"),
                        ]
                        .spacing(10)
                    };

                    let toggle_network_editor = button(text("Network")).on_press(Message::Sidebar(
                        dashboard::sidebar::Message::ToggleSidebarMenu(Some(
                            sidebar::Menu::Network,
                        )),
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
                            .label("Size in quote currency")
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
                                "Display sizes/volumes in quote currency (USD)\nHas no effect on inverse perps or open interest",
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
                            .label("Fetch trades (Binance)")
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
                            Some("Try to fetch trades for footprint charts"),
                            TooltipPosition::Top,
                        )
                    };

                    let open_data_folder = {
                        let button =
                            button(text("Open data folder")).on_press(Message::DataFolderRequested);

                        tooltip(
                            button,
                            Some("Open the folder where the data & config is stored"),
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

                    let column_content = split_column![
                        column![open_data_folder,].spacing(8),
                        column![text("Sidebar position").size(crate::style::text_size::SECTION), sidebar_pos_picklist,].spacing(12),
                        column![text("Time zone").size(crate::style::text_size::SECTION), timezone_picklist,].spacing(12),
                        column![text("Market data").size(crate::style::text_size::SECTION), size_in_quote_currency_checkbox,].spacing(12),
                        column![text("外观").size(crate::style::text_size::SECTION), appearance,].spacing(12),
                        column![text("Interface scale").size(crate::style::text_size::SECTION), scale_factor,].spacing(12),
                        column![
                            text("Experimental").size(crate::style::text_size::SECTION),
                            column![trade_fetch_checkbox, toggle_network_editor].spacing(8),
                        ]
                        .spacing(12),
                        footer,
                        ; spacing = 16, align_x = Alignment::Start
                    ];

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
                    sidebar::Position::Left => (Alignment::Start, padding::left(44).top(40)),
                    sidebar::Position::Right => (Alignment::End, padding::right(44).top(40)),
                };

                dashboard_modal(
                    base,
                    manage_layout_modal,
                    Message::Sidebar(dashboard::sidebar::Message::ToggleSidebarMenu(None)),
                    padding,
                    Alignment::Start,
                    align_x,
                )
            }
            sidebar::Menu::Audio => {
                let (align_x, padding) = match sidebar_pos {
                    sidebar::Position::Left => (Alignment::Start, padding::left(44).top(76)),
                    sidebar::Position::Right => (Alignment::End, padding::right(44).top(76)),
                };

                let trade_streams_list = dashboard.streams.trade_streams(None);

                dashboard_modal(
                    base,
                    self.audio_stream
                        .view(trade_streams_list)
                        .map(Message::AudioStream),
                    Message::Sidebar(dashboard::sidebar::Message::ToggleSidebarMenu(None)),
                    padding,
                    Alignment::Start,
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
            Ok(layout_str) => {
                let file_name = data::SAVED_STATE_PATH;
                if let Err(e) = data::write_json_to_file(&layout_str, file_name) {
                    log::error!("Failed to write layout state to file: {}", e);
                } else {
                    log::info!("Persisted state to {file_name}");
                }
            }
            Err(e) => log::error!("Failed to serialize layout: {}", e),
        }
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
