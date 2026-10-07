use crate::{
    chart::{self, comparison::ComparisonChart, heatmap::HeatmapChart, kline::KlineChart},
    connector::{
        ResolvedStream,
        fetcher::{FetchSpec, InfoKind},
    },
    modal::{
        self, ModifierKind,
        pane::{
            Modal,
            mini_tickers_list::MiniPanel,
            settings::{
                comparison_cfg_view, heatmap_cfg_view, heatmap_shader_cfg_view, kline_cfg_view,
            },
            stack_modal,
        },
    },
    screen::dashboard::{
        panel::{self, ladder::Ladder, timeandsales::TimeAndSales},
        tickers_table::TickersTable,
    },
    style::{self, Icon, icon_text},
    widget::{
        self, button_with_tooltip, chart::heatmap::HeatmapShader, column_drag, link_group_button,
        toast::Toast,
    },
    window::{self, Window},
};
use data::{
    UserTimezone,
    chart::{
        Basis, ViewConfig,
        heatmap::HeatmapStudy,
        indicator::{HeatmapIndicator, Indicator, KlineIndicator, UiIndicator},
    },
    layout::pane::{ContentKind, LinkGroup, PaneSetup, Settings, VisualConfig},
    stream::PersistStreamKind,
};
use exchange::{
    Kline, OpenInterest, StreamPairKind, TickMultiplier, TickerInfo, Timeframe,
    adapter::{MarketKind, StreamKind, StreamTicksize},
    unit::PriceStep,
};
use iced::{
    Alignment, Element, Length, Renderer, Theme, padding,
    widget::{button, center, column, container, pane_grid, pick_list, row, text, tooltip},
};
use std::time::Instant;

#[derive(Debug, Clone)]
pub enum Effect {
    RefreshStreams,
    RequestFetch(Vec<FetchSpec>),
    SwitchTickersInGroup(TickerInfo),
    FocusWidget(iced::widget::Id),
    /// 把某个 scrollable 横向滚到 `x`（特征矩阵：表头跟着表体横向滚动）。
    ScrollX(iced::widget::Id, f32),
    /// 特征图表流换了一份（开始回放 / 切回实时）：各图清空重建，不把两段数据画进同一张图。
    ResetFeatureCharts,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub enum Status {
    #[default]
    Ready,
    Loading(InfoKind),
    Stale(String),
}

pub enum Action {
    Chart(chart::Action),
    Panel(panel::Action),
    ResolveStreams(Vec<PersistStreamKind>),
    ResolveContent,
}

#[derive(Debug, Clone)]
pub enum Message {
    PaneClicked(pane_grid::Pane),
    PaneResized(pane_grid::ResizeEvent),
    PaneDragged(pane_grid::DragEvent),
    ClosePane(pane_grid::Pane),
    SplitPane(pane_grid::Axis, pane_grid::Pane),
    MaximizePane(pane_grid::Pane),
    Restore,
    ReplacePane(pane_grid::Pane),
    Popout,
    Merge,
    SwitchLinkGroup(pane_grid::Pane, Option<LinkGroup>),
    VisualConfigChanged(pane_grid::Pane, VisualConfig, bool),
    PaneEvent(pane_grid::Pane, Event),
}

#[derive(Debug, Clone)]
pub enum Event {
    ShowModal(Modal),
    HideModal,
    ContentSelected(ContentKind),
    ChartInteraction(super::chart::Message),
    PanelInteraction(super::panel::Message),
    ToggleIndicator(UiIndicator),
    DeleteNotification(usize),
    ReorderIndicator(column_drag::DragEvent),
    ClusterKindSelected(data::chart::kline::ClusterKind),
    ClusterScalingSelected(data::chart::kline::ClusterScaling),
    StudyConfigurator(modal::pane::settings::study::StudyMessage),
    StreamModifierChanged(modal::stream::Message),
    ComparisonChartInteraction(super::chart::comparison::Message),
    HeatmapShaderInteraction(crate::widget::chart::heatmap::Message),
    MiniTickersListInteraction(modal::pane::mini_tickers_list::Message),
    /// 录制驾驶舱交互（docs/08 F6-P3）：服务启停 / 配置编辑。
    RecorderInteraction(crate::ws::recorder::RecorderMsg),
    /// Tardis 历史回放交互（docs/20 Phase 5）：选符号/日期/时段/倍速 + 起停。
    TardisReplayInteraction(crate::ws::tardis_replay::TardisReplayMsg),
    /// Tardis 历史面板交互（docs/20 §9）：选源/类型/窗口 + 加载。
    TardisBoardInteraction(crate::ws::tardis_board::TardisBoardMsg),
    PmReplayInteraction(crate::ws::pm_replay::PmReplayMsg),
    /// Factory 面板交互（docs/20 §26）：nightly 手动启停。
    FactoryInteraction(crate::ws::factory::FactoryMsg),
    /// C4 活体影子交互：maker 影子守护启停（服务默认不自启，全由面板控制）。
    C4Interaction(crate::ws::c4::C4Msg),
    /// 数据接口观察终端（docs/23）：守护启停 + 连/断（副作用为 systemctl 与请求文件）。
    ObsInteraction(crate::ws::observatory_view::ObsMsg),
    /// 网络出口总闸：启停各路出口。
    EgressInteraction(crate::ws::egress_view::EgressMsg),
    /// 订单面板：两张网格的排序 / 分组 / 调宽 / 选中 / 滚动（状态在 ws::orders_view 的静态里）。
    OrdersInteraction(crate::ws::orders_view::OrdersMsg),
    /// 进程页：常驻单元启停（docs/26 S4）。
    ProcsInteraction(crate::ws::procs_view::ProcsMsg),
    /// 新闻资讯：守护启停 + 打开原文。
    NewsInteraction(crate::ws::news_view::NewsMsg),
    /// 预测市场面板交互：夜跑手动启停 + 每日定时开关（同上，默认不自启）。
    PredictionInteraction(crate::ws::prediction::PredictionMsg),
    /// 全市场雷达交互（docs/22 P0b）：守护启停 + 窗口/排序口径切换。
    RadarInteraction(crate::ws::radar::RadarMsg),
    FeatureMatrixInteraction(crate::ws::feature_matrix::FeatureMatrixMsg),
    StrategyCenterInteraction(crate::ws::strategy_center::ScMsg),
    StrategyLayersInteraction(crate::ws::strategy_layers::LyMsg),
    OfmsLabInteraction(crate::ws::ofms_lab::OfMsg),
    /// 回测结果面板顶上的「发起回测」（共用数据选择组件 + 起 runner）。
    BacktestLaunchInteraction(crate::ws::backtest_launch::LaunchMsg),
    /// 点了链路徽标：跳到对应面板的数据源选择。
    LinkBadgeClicked,
    /// 冻结 / 解冻这个面板（停止重画，docs/35 §8）
    ToggleFreeze,
    /// 标题栏「⋯」：展开 / 收起不常用的动作（docs/35 §10 第 1 条：标题栏 ≤ 3 个动作 + 溢出）
    ToggleOverflow,
    /// K 线数据表视图里的网格交互
    KlineTable(crate::ui::grid::GridMsg),
    /// 自绘图（自有数据图、Tardis 历史面板）的数据表视图交互
    SeriesTable(crate::ui::grid::GridMsg),
    /// 期权面板策略表的网格交互
    OptionsGrid(crate::ui::grid::GridMsg),
    /// 回测结果两张统计表的网格交互（0 = 运行信息，1 = 绩效统计）
    BacktestGrid(u8, crate::ui::grid::GridMsg),
}

pub struct State {
    id: uuid::Uuid,
    pub modal: Option<Modal>,
    pub content: Content,
    pub settings: Settings,
    pub notifications: Vec<Toast>,
    pub streams: ResolvedStream,
    pub status: Status,
    pub link_group: Option<LinkGroup>,
    /// 冻结（docs/35 §8，UPDS V5 §30）：停止重画，便于阅读和截图。数据照收，解冻后追上。
    /// 不存盘——重启后总是不冻结
    pub frozen: bool,
    /// 数据表视图（Ctrl Shift D，docs/35 §6.3）：K 线面板显示成表格。不存盘
    pub table_view: bool,
    /// 标题栏「⋯」展开着（不常用的动作露出来）。不存盘
    pub overflow: bool,
}

impl State {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_config(
        content: Content,
        streams: Vec<PersistStreamKind>,
        settings: Settings,
        link_group: Option<LinkGroup>,
    ) -> Self {
        Self {
            content,
            settings,
            streams: ResolvedStream::waiting(streams),
            link_group,
            ..Default::default()
        }
    }

    pub fn stream_pair(&self) -> Option<TickerInfo> {
        self.streams.find_ready_map(|stream| match stream {
            StreamKind::Kline { ticker_info, .. } => Some(*ticker_info),
            StreamKind::Depth { ticker_info, .. } => Some(*ticker_info),
            StreamKind::Trades { ticker_info, .. } => Some(*ticker_info),
        })
    }

    pub fn stream_pair_kind(&self) -> Option<StreamPairKind> {
        let ready_streams = self.streams.ready_iter()?;
        let mut unique = vec![];

        for stream in ready_streams {
            let ticker = stream.ticker_info();
            if !unique.contains(&ticker) {
                unique.push(ticker);
            }
        }

        match unique.len() {
            0 => None,
            1 => Some(StreamPairKind::SingleSource(unique[0])),
            _ => Some(StreamPairKind::MultiSource(unique)),
        }
    }

    pub fn set_content_and_streams(
        &mut self,
        tickers: Vec<TickerInfo>,
        kind: ContentKind,
    ) -> Vec<StreamKind> {
        if !(self.content.kind() == kind) {
            self.settings.selected_basis = None;
            self.settings.tick_multiply = None;
        }

        let base_ticker = tickers[0];
        let prev_base_ticker = self.stream_pair();

        let derived_plan = PaneSetup::new(
            kind,
            base_ticker,
            prev_base_ticker,
            self.settings.selected_basis,
            self.settings.tick_multiply,
        );

        self.settings.selected_basis = derived_plan.basis;
        self.settings.tick_multiply = derived_plan.tick_multiplier;

        let (content, streams) = {
            let kline_stream = |ti: TickerInfo, tf: Timeframe| StreamKind::Kline {
                ticker_info: ti,
                timeframe: tf,
            };
            let depth_stream = |derived_plan: &PaneSetup| StreamKind::Depth {
                ticker_info: derived_plan.ticker_info,
                depth_aggr: derived_plan.depth_aggr,
                push_freq: derived_plan.push_freq,
            };
            let trades_stream = |derived_plan: &PaneSetup| StreamKind::Trades {
                ticker_info: derived_plan.ticker_info,
            };

            match kind {
                ContentKind::HeatmapChart => {
                    let content = Content::new_heatmap(
                        &self.content,
                        derived_plan.ticker_info,
                        &self.settings,
                        derived_plan.price_step,
                    );

                    let streams = vec![depth_stream(&derived_plan), trades_stream(&derived_plan)];

                    (content, streams)
                }
                ContentKind::FootprintChart => {
                    let content = Content::new_kline(
                        kind,
                        &self.content,
                        derived_plan.ticker_info,
                        &self.settings,
                        derived_plan.price_step,
                    );

                    let streams = by_basis_default(
                        derived_plan.basis,
                        Timeframe::M5,
                        |tf| {
                            vec![
                                trades_stream(&derived_plan),
                                kline_stream(derived_plan.ticker_info, tf),
                            ]
                        },
                        || vec![trades_stream(&derived_plan)],
                    );

                    (content, streams)
                }
                ContentKind::CandlestickChart => {
                    let content = {
                        let base_ticker = tickers[0];
                        Content::new_kline(
                            kind,
                            &self.content,
                            derived_plan.ticker_info,
                            &self.settings,
                            base_ticker.min_ticksize.into(),
                        )
                    };

                    let time_basis_stream = |tf| vec![kline_stream(derived_plan.ticker_info, tf)];
                    let tick_basis_stream = || {
                        let depth_aggr = derived_plan
                            .ticker_info
                            .exchange()
                            .stream_ticksize(None, TickMultiplier(50));
                        let temp = PaneSetup {
                            depth_aggr,
                            ..derived_plan
                        };
                        vec![trades_stream(&temp)]
                    };

                    let streams = by_basis_default(
                        derived_plan.basis,
                        Timeframe::M15,
                        time_basis_stream,
                        tick_basis_stream,
                    );

                    (content, streams)
                }
                ContentKind::TimeAndSales => {
                    let config = self
                        .settings
                        .visual_config
                        .clone()
                        .and_then(|cfg| cfg.time_and_sales());
                    let content = Content::TimeAndSales(Some(TimeAndSales::new(
                        config,
                        derived_plan.ticker_info,
                    )));

                    let temp = PaneSetup {
                        push_freq: exchange::PushFrequency::ServerDefault,
                        ..derived_plan
                    };

                    let streams = vec![trades_stream(&temp)];

                    (content, streams)
                }
                ContentKind::Ladder => {
                    let config = self
                        .settings
                        .visual_config
                        .clone()
                        .and_then(|cfg| cfg.ladder());
                    let content = Content::Ladder(Some(Ladder::new(
                        config,
                        derived_plan.ticker_info,
                        derived_plan.price_step,
                    )));

                    let streams = vec![depth_stream(&derived_plan), trades_stream(&derived_plan)];

                    (content, streams)
                }
                ContentKind::ComparisonChart => {
                    let config = self
                        .settings
                        .visual_config
                        .clone()
                        .and_then(|cfg| cfg.comparison());

                    let timeframe = {
                        let supports = |tf| {
                            tickers
                                .iter()
                                .all(|ti| ti.exchange().supports_kline_timeframe(tf))
                        };

                        if let Some(tf) = derived_plan.basis.and_then(|basis| match basis {
                            Basis::Time(tf) => Some(tf),
                            Basis::Tick(_) => None,
                        }) && supports(tf)
                        {
                            tf
                        } else {
                            let fallback = Timeframe::M15;
                            if supports(fallback) {
                                fallback
                            } else {
                                Timeframe::KLINE
                                    .iter()
                                    .copied()
                                    .find(|tf| supports(*tf))
                                    .unwrap_or(fallback)
                            }
                        }
                    };

                    let basis = Basis::Time(timeframe);
                    self.settings.selected_basis = Some(basis);
                    let content =
                        Content::Comparison(Some(ComparisonChart::new(basis, &tickers, config)));

                    let streams = tickers
                        .iter()
                        .copied()
                        .map(|ti| kline_stream(ti, timeframe))
                        .collect();

                    (content, streams)
                }
                ContentKind::ShaderHeatmap => {
                    let basis = derived_plan
                        .basis
                        .unwrap_or(Basis::default_heatmap_time(Some(derived_plan.ticker_info)));

                    let (studies, indicators) = if let Content::ShaderHeatmap {
                        chart,
                        indicators,
                        studies,
                    } = &self.content
                    {
                        (
                            chart
                                .as_ref()
                                .map_or(studies.clone(), |c| c.studies.clone()),
                            indicators.clone(),
                        )
                    } else {
                        (
                            vec![HeatmapStudy::VolumeProfile(
                                data::chart::heatmap::ProfileKind::default(),
                            )],
                            vec![HeatmapIndicator::Volume],
                        )
                    };

                    let content = Content::ShaderHeatmap {
                        chart: Some(Box::new(HeatmapShader::new(
                            basis,
                            derived_plan.price_step,
                            base_ticker,
                            studies.clone(),
                            indicators.clone(),
                        ))),
                        studies,
                        indicators,
                    };

                    let streams = vec![depth_stream(&derived_plan), trades_stream(&derived_plan)];

                    (content, streams)
                }
                // WealthSpring 面板无行情流（数据走 Redis/shmem 旁路）；
                // 正常经 ContentSelected → placeholder 创建，这里仅为穷举完整性兜底。
                ContentKind::WealthSpring => {
                    (Content::WealthSpring(data::layout::pane::WsPaneMode::Any), vec![])
                }
                ContentKind::SelfChart => {
                    (Content::WealthSpring(data::layout::pane::WsPaneMode::SelfChart), vec![])
                }
                ContentKind::Factory => (Content::Factory, vec![]),
                ContentKind::C4Shadow => (Content::C4Shadow, vec![]),
                ContentKind::Observatory => (Content::Observatory, vec![]),
                ContentKind::NetEgress => (Content::NetEgress, vec![]),
                ContentKind::Procs => (Content::Procs, vec![]),
                ContentKind::News => (Content::News, vec![]),
                ContentKind::OptionsBoard => (Content::OptionsBoard, vec![]),
                ContentKind::PredictionBoard => (Content::PredictionBoard, vec![]),
                ContentKind::PmBinance => (Content::PmBinance, vec![]),
                ContentKind::FeatureLab => (Content::FeatureLab, vec![]),
                ContentKind::FeatureMatrix => (Content::FeatureMatrix, vec![]),
                ContentKind::StrategyCenter => (Content::StrategyCenter, vec![]),
                ContentKind::StrategyLayers => (Content::StrategyLayers, vec![]),
                ContentKind::OfmsLab => (Content::OfmsLab, vec![]),
                ContentKind::PmReplay => {
                    (Content::PmReplay(crate::ws::pm_replay::PmReplayState::load()), vec![])
                }
                ContentKind::MarketMap => (Content::MarketMap, vec![]),
                ContentKind::Recorder => {
                    (Content::Recorder(crate::ws::recorder::RecorderPaneState::load()), vec![])
                }
                ContentKind::TardisReplay => (
                    Content::TardisReplay(crate::ws::tardis_replay::TardisReplayState::load()),
                    vec![],
                ),
                ContentKind::TardisBoard => (
                    Content::TardisBoard(crate::ws::tardis_board::TardisBoardState::load()),
                    vec![],
                ),
                ContentKind::BacktestResult => (Content::BacktestResult, vec![]),
            ContentKind::Orders => (Content::Orders, vec![]),
                ContentKind::Starter => unreachable!(),
            }
        };

        self.content = content;
        self.streams = ResolvedStream::Ready(streams.clone());

        streams
    }

    pub fn insert_hist_oi(&mut self, req_id: Option<uuid::Uuid>, oi: &[OpenInterest]) {
        match &mut self.content {
            Content::Kline { chart, .. } => {
                let Some(chart) = chart else {
                    panic!("Kline chart wasn't initialized when inserting open interest");
                };
                chart.insert_open_interest(req_id, oi);
            }
            _ => {
                log::error!("pane content not candlestick");
            }
        }
    }

    pub fn insert_hist_klines(
        &mut self,
        req_id: Option<uuid::Uuid>,
        timeframe: Timeframe,
        ticker_info: TickerInfo,
        klines: &[Kline],
    ) {
        match &mut self.content {
            Content::Kline {
                chart, indicators, ..
            } => {
                let Some(chart) = chart else {
                    panic!("chart wasn't initialized when inserting klines");
                };

                if let Some(id) = req_id {
                    if chart.basis() != Basis::Time(timeframe) {
                        log::warn!(
                            "Ignoring stale kline fetch for timeframe {:?}; chart basis = {:?}",
                            timeframe,
                            chart.basis()
                        );
                        return;
                    }
                    chart.insert_hist_klines(id, klines);
                } else {
                    let (raw_trades, tick_size) = (chart.raw_trades(), chart.tick_size());
                    let layout = chart.chart_layout();
                    let visual_config = chart.visual_config();

                    *chart = KlineChart::new(
                        layout,
                        Basis::Time(timeframe),
                        tick_size,
                        klines,
                        raw_trades,
                        indicators,
                        ticker_info,
                        chart.kind(),
                        Some(visual_config),
                    );
                }
            }
            Content::Comparison(chart) => {
                let Some(chart) = chart else {
                    panic!("Comparison chart wasn't initialized when inserting klines");
                };

                if let Some(id) = req_id {
                    if chart.timeframe != timeframe {
                        log::warn!(
                            "Ignoring stale kline fetch for timeframe {:?}; chart timeframe = {:?}",
                            timeframe,
                            chart.timeframe
                        );
                        return;
                    }
                    chart.insert_history(id, ticker_info, klines);
                } else {
                    *chart = ComparisonChart::new(
                        Basis::Time(timeframe),
                        &[ticker_info],
                        Some(chart.serializable_config()),
                    );
                }
            }
            _ => {
                log::error!("pane content not candlestick or footprint");
            }
        }
    }

    fn has_stream(&self) -> bool {
        match &self.streams {
            ResolvedStream::Ready(streams) => !streams.is_empty(),
            ResolvedStream::Waiting { streams, .. } => !streams.is_empty(),
        }
    }

    pub fn view<'a>(
        &'a self,
        id: pane_grid::Pane,
        panes: usize,
        is_focused: bool,
        maximized: bool,
        window: window::Id,
        main_window: &'a Window,
        timezone: UserTimezone,
        tickers_table: &'a TickersTable,
    ) -> pane_grid::Content<'a, Message, Theme, Renderer> {
        let mut top_left_buttons = if Content::Starter == self.content {
            row![]
        } else {
            row![link_group_button(id, self.link_group, |id| {
                Message::PaneEvent(id, Event::ShowModal(Modal::LinkGroup))
            })]
        };

        if let Some(kind) = self.stream_pair_kind() {
            let (base_ti, extra) = match kind {
                StreamPairKind::MultiSource(list) => (list[0], list.len().saturating_sub(1)),
                StreamPairKind::SingleSource(ti) => (ti, 0),
            };

            let exchange_icon = icon_text(style::venue_icon(base_ti.ticker.exchange.venue()), 14);
            let mut label = {
                let symbol = base_ti.ticker.display_symbol_and_type().0;
                match base_ti.ticker.market_type() {
                    MarketKind::Spot => symbol,
                    MarketKind::LinearPerps | MarketKind::InversePerps => symbol + " PERP",
                }
            };
            if extra > 0 {
                label = format!("{label} +{extra}");
            }

            let content = row![
                exchange_icon.align_y(Alignment::Center).line_height(1.4),
                text(label)
                    .size(crate::style::text_size::SECTION)
                    .align_y(Alignment::Center)
                    .line_height(1.4)
            ]
            .align_y(Alignment::Center)
            .spacing(4);

            let tickers_list_btn = button(content)
                .on_press(Message::PaneEvent(
                    id,
                    Event::ShowModal(Modal::MiniTickersList(MiniPanel::new())),
                ))
                .style(|theme, status| {
                    style::button::modifier(
                        theme,
                        status,
                        !matches!(self.modal, Some(Modal::MiniTickersList(_))),
                    )
                })
                .height(widget::PANE_CONTROL_BTN_HEIGHT);

            top_left_buttons = top_left_buttons.push(tickers_list_btn);
            // 数据来源徽标（docs/28 §4.2）：**只给吃行情的面板，且常驻显示**。
            // 本分支的条件就是 `stream_pair_kind()` 非空——恰好是那 7 个面板；
            // 其余 17 个是零交易所流的读数面板，给它们加徽标是纯噪音。
            top_left_buttons = top_left_buttons.push(provenance_badge());
            // 数据链路徽标：数据从哪来、流到哪、通不通（与上面的「性质」各说一件事）。
            top_left_buttons = top_left_buttons.push(link_badge(
                id,
                &base_ti.ticker.exchange.venue().to_string(),
                &base_ti.ticker.display_symbol_and_type().0,
            ));
        } else if !matches!(
            self.content,
            Content::Starter
                | Content::WealthSpring(_)
                | Content::Factory
                | Content::C4Shadow
                | Content::Observatory
                | Content::NetEgress
                | Content::Procs
                | Content::News
                | Content::OptionsBoard
                | Content::PredictionBoard
                | Content::PmBinance
                | Content::PmReplay(_)
                | Content::FeatureLab
                | Content::FeatureMatrix
                | Content::StrategyCenter
                | Content::StrategyLayers
                | Content::OfmsLab
                | Content::MarketMap
                | Content::Recorder(_)
                | Content::TardisReplay(_)
                | Content::TardisBoard(_)
                | Content::BacktestResult
            | Content::Orders
        ) && !self.has_stream()
        {
            let content = row![
                text("Choose a ticker")
                    .size(crate::style::text_size::EMPHASIS)
                    .align_y(Alignment::Center)
                    .line_height(1.4)
            ]
            .align_y(Alignment::Center);

            let tickers_list_btn = button(content)
                .on_press(Message::PaneEvent(
                    id,
                    Event::ShowModal(Modal::MiniTickersList(MiniPanel::new())),
                ))
                .style(|theme, status| {
                    style::button::modifier(
                        theme,
                        status,
                        !matches!(self.modal, Some(Modal::MiniTickersList(_))),
                    )
                })
                .height(widget::PANE_CONTROL_BTN_HEIGHT);

            top_left_buttons = top_left_buttons.push(tickers_list_btn);
        } else if !matches!(self.content, Content::Starter) {
            // WS 自研面板：统一的标题栏标题 + 状态位（docs/35 §5.1，见 ws::panel_status）
            // 页面里的面板（docs/41）：配置里有页名（`Settings.view`）就显示页名
            top_left_buttons = top_left_buttons.push(
                text(self.settings.view.clone().unwrap_or_else(|| self.content.to_string()))
                    .size(crate::ui::text::s_body())
                    .color(crate::ui::pal::head())
                    .align_y(Alignment::Center)
                    .line_height(1.4),
            );
            if let Some((s, c)) = crate::ws::panel_status::status(&self.content) {
                top_left_buttons = top_left_buttons.push(
                    text(s).size(crate::ui::text::s_meta()).color(c).align_y(Alignment::Center).line_height(1.4),
                );
            }
        }

        if self.frozen {
            top_left_buttons = top_left_buttons.push(
                text("❄ 已冻结").size(crate::ui::text::s_meta()).color(crate::ui::pal::info()).align_y(Alignment::Center).line_height(1.4),
            );
        }

        let modifier: Option<modal::stream::Modifier> = self.modal.clone().and_then(|m| {
            if let Modal::StreamModifier(modifier) = m {
                Some(modifier)
            } else {
                None
            }
        });

        let compact_controls = if self.modal == Some(Modal::Controls) {
            Some(
                container(self.view_controls(id, panes, maximized, window != main_window.id))
                    .style(style::chart_modal)
                    .into(),
            )
        } else {
            None
        };

        let uninitialized_base = |kind: ContentKind| -> Element<'a, Message> {
            if self.has_stream() {
                center(text("Loading…").size(crate::style::text_size::TITLE)).into()
            } else {
                let content = column![
                    text(kind.to_string()).size(crate::style::text_size::TITLE),
                    text("No ticker selected").size(crate::style::text_size::SECTION)
                ]
                .spacing(8)
                .align_x(Alignment::Center);

                center(content).into()
            }
        };

        let body = match &self.content {
            Content::Starter => {
                let content_picklist =
                    pick_list(ContentKind::ALL, Some(ContentKind::Starter), move |kind| {
                        Message::PaneEvent(id, Event::ContentSelected(kind))
                    });

                let base: Element<_> = widget::toast::Manager::new(
                    center(
                        column![
                            text("Choose a view to get started")
                                .size(crate::style::text_size::TITLE),
                            content_picklist
                        ]
                        .align_x(Alignment::Center)
                        .spacing(12),
                    ),
                    &self.notifications,
                    Alignment::End,
                    move |msg| Message::PaneEvent(id, Event::DeleteNotification(msg)),
                )
                .into();

                self.compose_stack_view(
                    base,
                    id,
                    None,
                    compact_controls,
                    || column![].into(),
                    None,
                    tickers_table,
                )
            }
            Content::WealthSpring(mode) => {
                // WealthSpring 读数面板（docs/08）：渲染走 ws::readout 旁路快照，无行情流。
                // 方案 2：按 pane 的三态过滤（Live/Backtest 仅在对应态显读数）。
                // 自有数据图在数据表视图下（Ctrl Shift D，docs/35 §6.3）显示成表
                let base = if self.table_view && *mode == data::layout::pane::WsPaneMode::SelfChart {
                    let pid = self.id;
                    crate::ws::series_table::view(pid, &crate::ws::series_table::from_selfdata())
                        .map(move |m| Message::PaneEvent(id, Event::SeriesTable(m)))
                } else {
                    crate::ws::view::pane_body(*mode)
                };
                self.compose_stack_view(
                    base,
                    id,
                    None,
                    compact_controls,
                    || column![].into(),
                    None,
                    tickers_table,
                )
            }
            Content::Factory => {
                // Alpha Factory 仪表盘（docs/08 F6-P2）：渲染走 ws::factory_readout 旁路快照；
                // ⑦ 区的 nightly 启停按钮发 FactoryMsg → 包成 pane 事件。
                let base = crate::ws::factory_view::pane_body()
                    .map(move |m| Message::PaneEvent(id, Event::FactoryInteraction(m)));
                self.compose_stack_view(
                    base,
                    id,
                    None,
                    compact_controls,
                    || column![].into(),
                    None,
                    tickers_table,
                )
            }
            Content::News => {
                // 新闻资讯（docs/25）：渲染走 ws::news_readout 旁路快照。
                // **面板里没有一行网络代码**——连接全在 ws-news 守护里
                let base = crate::ws::news_view::pane_body(self.settings.view.as_deref())
                    .map(move |m| Message::PaneEvent(id, Event::NewsInteraction(m)));
                self.compose_stack_view(
                    base,
                    id,
                    None,
                    compact_controls,
                    || column![].into(),
                    None,
                    tickers_table,
                )
            }
            Content::Procs => {
                // 进程页：清单与状态在 ws::procs，这里只把消息包成 pane 事件
                let base = crate::ws::procs_view::pane_body(&crate::ws::procs::note(), self.settings.view.as_deref())
                    .map(move |m| Message::PaneEvent(id, Event::ProcsInteraction(m)));
                self.compose_stack_view(
                    base,
                    id,
                    None,
                    compact_controls,
                    || column![].into(),
                    None,
                    tickers_table,
                )
            }
            Content::NetEgress => {
                // 网络出口总闸：清单与计数在 ws::egress，这里只把消息包成 pane 事件
                let base = crate::ws::egress_view::pane_body(&crate::ws::egress::note())
                    .map(move |m| Message::PaneEvent(id, Event::EgressInteraction(m)));
                self.compose_stack_view(
                    base,
                    id,
                    None,
                    compact_controls,
                    || column![].into(),
                    None,
                    tickers_table,
                )
            }
            Content::Observatory => {
                // 数据接口观察终端（docs/23）：渲染走旁路快照；
                // 顶部守护启停按钮发 ObsMsg → 包成 pane 事件
                let base = crate::ws::observatory_view::pane_body()
                    .map(move |m| Message::PaneEvent(id, Event::ObsInteraction(m)));
                self.compose_stack_view(
                    base,
                    id,
                    None,
                    compact_controls,
                    || column![].into(),
                    None,
                    tickers_table,
                )
            }
            Content::C4Shadow => {
                // C4 活体影子（docs/14 §2）：渲染走 ws::c4_readout 旁路快照（checkpoint+Registry）；
                // 顶部守护启停按钮发 C4Msg → 包成 pane 事件。
                let base = crate::ws::c4_view::pane_body()
                    .map(move |m| Message::PaneEvent(id, Event::C4Interaction(m)));
                self.compose_stack_view(
                    base,
                    id,
                    None,
                    compact_controls,
                    || column![].into(),
                    None,
                    tickers_table,
                )
            }
            Content::StrategyCenter => {
                // 策略中心（docs/37 P1）：目录来自 factory.lab、运行记录只读 research.sqlite，
                // 回测经 ws-control 发起。面板不跑回测、不算指标。
                let base = crate::ws::strategy_center_view::pane_body()
                    .map(move |m| Message::PaneEvent(id, Event::StrategyCenterInteraction(m)));
                self.compose_stack_view(
                    base,
                    id,
                    None,
                    compact_controls,
                    || column![].into(),
                    None,
                    tickers_table,
                )
            }
            Content::StrategyLayers => {
                // 七层（docs/39）：跟随策略中心选中的策略与运行，读那次运行的 layers.json
                let base = crate::ws::strategy_layers_view::pane_body()
                    .map(move |m| Message::PaneEvent(id, Event::StrategyLayersInteraction(m)));
                self.compose_stack_view(
                    base,
                    id,
                    None,
                    compact_controls,
                    || column![].into(),
                    None,
                    tickers_table,
                )
            }
            Content::OfmsLab => {
                // 订单流层析（docs/40）：OFMS-10 时间轴 · 因果链 · 特征字典
                let base = crate::ws::ofms_lab_view::pane_body(self.settings.view.as_deref())
                    .map(move |m| Message::PaneEvent(id, Event::OfmsLabInteraction(m)));
                self.compose_stack_view(
                    base,
                    id,
                    None,
                    compact_controls,
                    || column![].into(),
                    None,
                    tickers_table,
                )
            }
            Content::OptionsBoard => {
                // 期权/0DTE 回测（docs/18）：渲染走 ws::options_readout 旁路快照（options_board.json）。
                let base = crate::ws::options_view::pane_body()
                    .map(move |m| Message::PaneEvent(id, Event::OptionsGrid(m)));
                self.compose_stack_view(
                    base,
                    id,
                    None,
                    compact_controls,
                    || column![].into(),
                    None,
                    tickers_table,
                )
            }
            Content::FeatureLab => {
                // 特征库（docs/30）：只读 ~/ws-data/cockpit/feature_lab.json。
                // **零交易所流**——数据由 factory.prediction.feature_lab 批量算好落盘。
                self.compose_stack_view(
                    crate::ws::feature_lab_view::pane_body(),
                    id,
                    None,
                    compact_controls,
                    || column![].into(),
                    None,
                    tickers_table,
                )
            }
            Content::FeatureMatrix => {
                // 特征矩阵（docs/31 §8.1）：只读 ~/ws-data/cockpit/feature_matrix.json，
                // 由 wealthspring-features 的引擎周期性写出。**零交易所流、零计算**。
                // 筛选/视图切换发 FeatureMatrixMsg → 包成 pane 事件。
                let base = crate::ws::feature_matrix_view::pane_body(crate::ws::inspector_props::hosted(id), self.settings.view.as_deref())
                    .map(move |m| Message::PaneEvent(id, Event::FeatureMatrixInteraction(m)));
                self.compose_stack_view(
                    base,
                    id,
                    None,
                    compact_controls,
                    || column![].into(),
                    None,
                    tickers_table,
                )
            }
            Content::PmReplay(pr) => {
                // 预测市场回放：读自录 parquet 生成的图表 JSON，**零交易所连接**。
                let base = crate::ws::pm_replay_view::pane_body(pr)
                    .map(move |m| Message::PaneEvent(id, Event::PmReplayInteraction(m)));
                self.compose_stack_view(
                    base,
                    id,
                    None,
                    compact_controls,
                    || column![].into(),
                    None,
                    tickers_table,
                )
            }
            Content::PmBinance => {
                // 币安钱包预测市场：只读 ~/ws-data/live/pm_binance.json 旁路快照。
                // **零交易所流**——WS 连接在 ws-pm-recorder 守护里，面板里没有一行网络代码。
                self.compose_stack_view(
                    crate::ws::pm_binance_view::pane_body(),
                    id,
                    None,
                    compact_controls,
                    || column![].into(),
                    None,
                    tickers_table,
                )
            }
            Content::PredictionBoard => {
                // 预测市场 Polymarket（docs/19）：渲染走 ws::prediction_readout 旁路快照（prediction_board.json）；
                // 顶部夜跑启停按钮发 PredictionMsg → 包成 pane 事件。
                let base = crate::ws::prediction_view::pane_body()
                    .map(move |m| Message::PaneEvent(id, Event::PredictionInteraction(m)));
                self.compose_stack_view(
                    base,
                    id,
                    None,
                    compact_controls,
                    || column![].into(),
                    None,
                    tickers_table,
                )
            }
            Content::MarketMap => {
                // 全市场雷达（docs/22 P0b）：树图 + 排行，渲染走 ws::radar_readout 旁路快照
                // （radar_board.json）；顶部按钮发 RadarMsg → 包成 pane 事件。
                let base = crate::ws::radar_view::pane_body(self.settings.view.as_deref())
                    .map(move |m| Message::PaneEvent(id, Event::RadarInteraction(m)));
                self.compose_stack_view(
                    base,
                    id,
                    None,
                    compact_controls,
                    || column![].into(),
                    None,
                    tickers_table,
                )
            }
            Content::TardisBoard(tb) => {
                // Tardis 历史面板（docs/20 §9）：源→类型→图，全自绘、零交易所流。
                // 数据表视图（Ctrl Shift D）：当前那张主图的数据换成表；没有可列的图时照常显示面板
                let table = self.table_view.then(crate::ws::series_table::from_tardis).flatten();
                let base = match table {
                    Some(t) => {
                        let pid = self.id;
                        crate::ws::series_table::view(pid, &t).map(move |m| Message::PaneEvent(id, Event::SeriesTable(m)))
                    }
                    None => crate::ws::tardis_board_view::pane_body(tb, crate::ws::inspector_props::hosted(id))
                        .map(move |m| Message::PaneEvent(id, Event::TardisBoardInteraction(m))),
                };
                self.compose_stack_view(
                    base,
                    id,
                    None,
                    compact_controls,
                    || column![].into(),
                    None,
                    tickers_table,
                )
            }
            Content::TardisReplay(tr) => {
                // Tardis 历史回放（docs/20 Phase 5）：交互视图发 TardisReplayMsg → 包成 pane 事件。
                let base = crate::ws::tardis_replay_view::pane_body(tr, crate::ws::inspector_props::hosted(id))
                    .map(move |m| Message::PaneEvent(id, Event::TardisReplayInteraction(m)));
                self.compose_stack_view(
                    base,
                    id,
                    None,
                    compact_controls,
                    || column![].into(),
                    None,
                    tickers_table,
                )
            }
            Content::Recorder(rec) => {
                // 录制驾驶舱（docs/08 F6-P3）：交互视图发 RecorderMsg → 包成 pane 事件。
                let base = crate::ws::recorder_view::pane_body(rec, self.settings.view.as_deref())
                    .map(move |m| Message::PaneEvent(id, Event::RecorderInteraction(m)));
                self.compose_stack_view(
                    base,
                    id,
                    None,
                    compact_controls,
                    || column![].into(),
                    None,
                    tickers_table,
                )
            }
            Content::Orders => {
                // 数据走 ws::readout 旁路快照；表格交互（排序、分组、调宽）包成 pane 事件。
                crate::ws::orders_view::pane_body(self.frozen).map(move |m| Message::PaneEvent(id, Event::OrdersInteraction(m)))
            }
            Content::BacktestResult => {
                // 回测结果（docs/08 F6-P7）：渲染走 ws::backtest_readout 旁路快照。
                // 顶上是「发起回测」（选策略 + 共用数据选择组件 → 起 runner）。
                let launch = crate::ws::backtest_launch_view::view(crate::ws::inspector_props::hosted(id))
                    .map(move |m| Message::PaneEvent(id, Event::BacktestLaunchInteraction(m)));
                let body = crate::ws::backtest_view::pane_body()
                    .map(move |(w, m)| Message::PaneEvent(id, Event::BacktestGrid(w, m)));
                let base = column![launch, body].into();
                self.compose_stack_view(
                    base,
                    id,
                    None,
                    compact_controls,
                    || column![].into(),
                    None,
                    tickers_table,
                )
            }
            Content::Comparison(chart) => {
                if let Some(c) = chart {
                    let selected_basis = Basis::Time(c.timeframe);
                    let kind = ModifierKind::Comparison(selected_basis);

                    let modifiers =
                        row![basis_modifier(id, selected_basis, modifier, kind),].spacing(4);

                    top_left_buttons = top_left_buttons.push(modifiers);

                    let base = c.view(timezone).map(move |message| {
                        Message::PaneEvent(id, Event::ComparisonChartInteraction(message))
                    });

                    let settings_modal = || comparison_cfg_view(id, c);

                    self.compose_stack_view(
                        base,
                        id,
                        None,
                        compact_controls,
                        settings_modal,
                        Some(c.selected_tickers()),
                        tickers_table,
                    )
                } else {
                    let base = uninitialized_base(ContentKind::ComparisonChart);
                    self.compose_stack_view(
                        base,
                        id,
                        None,
                        compact_controls,
                        || column![].into(),
                        None,
                        tickers_table,
                    )
                }
            }
            Content::TimeAndSales(panel) => {
                if let Some(panel) = panel {
                    let base = panel::view(panel, timezone).map(move |message| {
                        Message::PaneEvent(id, Event::PanelInteraction(message))
                    });

                    let settings_modal =
                        || modal::pane::settings::timesales_cfg_view(panel.config, id);

                    self.compose_stack_view(
                        base,
                        id,
                        None,
                        compact_controls,
                        settings_modal,
                        None,
                        tickers_table,
                    )
                } else {
                    let base = uninitialized_base(ContentKind::TimeAndSales);
                    self.compose_stack_view(
                        base,
                        id,
                        None,
                        compact_controls,
                        || column![].into(),
                        None,
                        tickers_table,
                    )
                }
            }
            Content::Ladder(panel) => {
                if let Some(panel) = panel {
                    let basis = self
                        .settings
                        .selected_basis
                        .unwrap_or(Basis::default_heatmap_time(self.stream_pair()));
                    let tick_multiply = self.settings.tick_multiply.unwrap_or(TickMultiplier(1));

                    let stream_pair = self.stream_pair();

                    let price_step = stream_pair
                        .map(|ti| {
                            tick_multiply.unscale_step_or_min_tick(panel.step, ti.min_ticksize)
                        })
                        .unwrap_or_else(|| tick_multiply.unscale_step(panel.step));

                    let exchange = stream_pair.map(|ti| ti.ticker.exchange);
                    let min_ticksize = stream_pair.map(|ti| ti.min_ticksize);

                    let modifiers = ticksize_modifier(
                        id,
                        price_step,
                        min_ticksize,
                        tick_multiply,
                        modifier,
                        ModifierKind::Orderbook(basis, tick_multiply),
                        exchange,
                    );

                    top_left_buttons = top_left_buttons.push(modifiers);

                    let base = panel::view(panel, timezone).map(move |message| {
                        Message::PaneEvent(id, Event::PanelInteraction(message))
                    });

                    let settings_modal =
                        || modal::pane::settings::ladder_cfg_view(panel.config, id);

                    self.compose_stack_view(
                        base,
                        id,
                        None,
                        compact_controls,
                        settings_modal,
                        None,
                        tickers_table,
                    )
                } else {
                    let base = uninitialized_base(ContentKind::Ladder);
                    self.compose_stack_view(
                        base,
                        id,
                        None,
                        compact_controls,
                        || column![].into(),
                        None,
                        tickers_table,
                    )
                }
            }
            Content::Heatmap {
                chart, indicators, ..
            } => {
                if let Some(chart) = chart {
                    let ticker_info = self.stream_pair();
                    let exchange = ticker_info.as_ref().map(|info| info.ticker.exchange);

                    let basis = self
                        .settings
                        .selected_basis
                        .unwrap_or(Basis::default_heatmap_time(ticker_info));
                    let tick_multiply = self.settings.tick_multiply.unwrap_or(TickMultiplier(5));

                    let kind = ModifierKind::Heatmap(basis, tick_multiply);
                    let price_step = ticker_info
                        .map(|ti| {
                            tick_multiply
                                .unscale_step_or_min_tick(chart.tick_size(), ti.min_ticksize)
                        })
                        .unwrap_or_else(|| tick_multiply.unscale_step(chart.tick_size()));
                    let min_ticksize = ticker_info.map(|ti| ti.min_ticksize);

                    let modifiers = row![
                        basis_modifier(id, basis, modifier, kind),
                        ticksize_modifier(
                            id,
                            price_step,
                            min_ticksize,
                            tick_multiply,
                            modifier,
                            kind,
                            exchange
                        ),
                    ]
                    .spacing(4);

                    top_left_buttons = top_left_buttons.push(modifiers);

                    let base = chart::view(chart, indicators, timezone).map(move |message| {
                        Message::PaneEvent(id, Event::ChartInteraction(message))
                    });
                    let settings_modal = || {
                        heatmap_cfg_view(
                            chart.visual_config(),
                            id,
                            chart.study_configurator(),
                            &chart.studies,
                            basis,
                        )
                    };

                    let indicator_modal = if self.modal == Some(Modal::Indicators) {
                        Some(modal::indicators::view(
                            id,
                            self,
                            indicators,
                            self.stream_pair().map(|i| i.ticker.market_type()),
                        ))
                    } else {
                        None
                    };

                    self.compose_stack_view(
                        base,
                        id,
                        indicator_modal,
                        compact_controls,
                        settings_modal,
                        None,
                        tickers_table,
                    )
                } else {
                    let base = uninitialized_base(ContentKind::HeatmapChart);
                    self.compose_stack_view(
                        base,
                        id,
                        None,
                        compact_controls,
                        || column![].into(),
                        None,
                        tickers_table,
                    )
                }
            }
            Content::Kline {
                chart,
                indicators,
                kind: chart_kind,
                ..
            } => {
                if let Some(chart) = chart {
                    match chart_kind {
                        data::chart::KlineChartKind::Footprint { .. } => {
                            let basis = chart.basis();
                            let tick_multiply =
                                self.settings.tick_multiply.unwrap_or(TickMultiplier(10));

                            let kind = ModifierKind::Footprint(basis, tick_multiply);
                            let stream_pair = self.stream_pair();
                            let price_step = stream_pair
                                .map(|ti| {
                                    tick_multiply.unscale_step_or_min_tick(
                                        chart.tick_size(),
                                        ti.min_ticksize,
                                    )
                                })
                                .unwrap_or_else(|| tick_multiply.unscale_step(chart.tick_size()));

                            let exchange = stream_pair.as_ref().map(|info| info.ticker.exchange);
                            let min_ticksize = stream_pair.map(|ti| ti.min_ticksize);

                            let modifiers = row![
                                basis_modifier(id, basis, modifier, kind),
                                ticksize_modifier(
                                    id,
                                    price_step,
                                    min_ticksize,
                                    tick_multiply,
                                    modifier,
                                    kind,
                                    exchange
                                ),
                            ]
                            .spacing(4);

                            top_left_buttons = top_left_buttons.push(modifiers);
                        }
                        data::chart::KlineChartKind::Candles => {
                            let selected_basis = chart.basis();
                            let kind = ModifierKind::Candlestick(selected_basis);

                            let modifiers =
                                row![basis_modifier(id, selected_basis, modifier, kind),]
                                    .spacing(4);

                            top_left_buttons = top_left_buttons.push(modifiers);
                        }
                    }

                    // Ctrl Shift D：图 ↔ 数据表（docs/35 §6.3）
                    let base = if self.table_view {
                        crate::ws::kline_table::view(
                            self.id,
                            &chart.recent_klines(5000),
                            self.stream_pair().map(|ti| ti.min_ticksize),
                        )
                        .map(move |m| Message::PaneEvent(id, Event::KlineTable(m)))
                    } else {
                        chart::view(chart, indicators, timezone).map(move |message| {
                            Message::PaneEvent(id, Event::ChartInteraction(message))
                        })
                    };
                    let settings_modal = || {
                        kline_cfg_view(
                            chart.study_configurator(),
                            chart.visual_config(),
                            chart_kind,
                            id,
                            chart.basis(),
                        )
                    };

                    let indicator_modal = if self.modal == Some(Modal::Indicators) {
                        Some(modal::indicators::view(
                            id,
                            self,
                            indicators,
                            self.stream_pair().map(|i| i.ticker.market_type()),
                        ))
                    } else {
                        None
                    };

                    self.compose_stack_view(
                        base,
                        id,
                        indicator_modal,
                        compact_controls,
                        settings_modal,
                        None,
                        tickers_table,
                    )
                } else {
                    let content_kind = match chart_kind {
                        data::chart::KlineChartKind::Candles => ContentKind::CandlestickChart,
                        data::chart::KlineChartKind::Footprint { .. } => {
                            ContentKind::FootprintChart
                        }
                    };
                    let base = uninitialized_base(content_kind);
                    self.compose_stack_view(
                        base,
                        id,
                        None,
                        compact_controls,
                        || column![].into(),
                        None,
                        tickers_table,
                    )
                }
            }
            Content::ShaderHeatmap {
                chart, indicators, ..
            } => {
                if let Some(chart) = chart {
                    let base = HeatmapShader::view(chart, timezone).map(move |message| {
                        Message::PaneEvent(id, Event::HeatmapShaderInteraction(message))
                    });

                    let ticker_info = self.stream_pair();
                    let exchange = ticker_info.as_ref().map(|info| info.ticker.exchange);

                    let basis = self
                        .settings
                        .selected_basis
                        .unwrap_or(Basis::default_heatmap_time(ticker_info));
                    let tick_multiply = self.settings.tick_multiply.unwrap_or(TickMultiplier(5));

                    let kind = ModifierKind::Heatmap(basis, tick_multiply);

                    let price_step = ticker_info
                        .map(|ti| {
                            tick_multiply
                                .unscale_step_or_min_tick(chart.tick_size(), ti.min_ticksize)
                        })
                        .unwrap_or_else(|| tick_multiply.unscale_step(chart.tick_size()));
                    let min_ticksize = ticker_info.map(|ti| ti.min_ticksize);

                    let settings_modal = || {
                        heatmap_shader_cfg_view(
                            chart.visual_config(),
                            id,
                            chart.study_configurator(),
                            &chart.studies,
                            basis,
                        )
                    };

                    let indicator_modal = if self.modal == Some(Modal::Indicators) {
                        Some(modal::indicators::view(
                            id,
                            self,
                            indicators,
                            self.stream_pair().map(|i| i.ticker.market_type()),
                        ))
                    } else {
                        None
                    };

                    let modifiers = row![
                        basis_modifier(id, basis, modifier, kind),
                        ticksize_modifier(
                            id,
                            price_step,
                            min_ticksize,
                            tick_multiply,
                            modifier,
                            kind,
                            exchange
                        ),
                    ]
                    .spacing(4);

                    top_left_buttons = top_left_buttons.push(modifiers);

                    self.compose_stack_view(
                        base,
                        id,
                        indicator_modal,
                        compact_controls,
                        settings_modal,
                        None,
                        tickers_table,
                    )
                } else {
                    let base = uninitialized_base(ContentKind::HeatmapChart);
                    self.compose_stack_view(
                        base,
                        id,
                        None,
                        compact_controls,
                        || column![].into(),
                        None,
                        tickers_table,
                    )
                }
            }
        };

        match &self.status {
            Status::Loading(InfoKind::FetchingKlines) => {
                top_left_buttons = top_left_buttons.push(text("Fetching Klines..."));
            }
            Status::Loading(InfoKind::FetchingTrades(count)) => {
                top_left_buttons =
                    top_left_buttons.push(text(format!("Fetching Trades... {count} fetched")));
            }
            Status::Loading(InfoKind::FetchingOI) => {
                top_left_buttons = top_left_buttons.push(text("Fetching Open Interest..."));
            }
            Status::Stale(msg) => {
                top_left_buttons = top_left_buttons.push(text(msg));
            }
            Status::Ready => {}
        }

        let content = pane_grid::Content::new(body)
            .style(move |theme| style::pane_background(theme, is_focused));

        let top_right_buttons = {
            let compact_control = container(
                button(
                    text("...")
                        .size(crate::style::text_size::EMPHASIS)
                        .align_y(Alignment::End),
                )
                .on_press(Message::PaneEvent(id, Event::ShowModal(Modal::Controls)))
                .style(move |theme, status| {
                    style::button::transparent(
                        theme,
                        status,
                        self.modal == Some(Modal::Controls) || self.modal == Some(Modal::Settings),
                    )
                }),
            )
            .align_y(Alignment::Center)
            .padding(crate::ui::metrics::space(1));

            if self.modal == Some(Modal::Controls) {
                pane_grid::Controls::new(compact_control)
            } else {
                pane_grid::Controls::dynamic(
                    self.view_controls(id, panes, maximized, window != main_window.id),
                    compact_control,
                )
            }
        };

        // UPDS V2 §11：聚焦面板 = 标题栏前缘 2px 强调色条 + 强边框（边框在 pane_background）。
        // 失焦时色条透明但仍占位，标题不会因为焦点变化左右跳。
        let accent_bar = container(iced::widget::space::horizontal())
            .width(Length::Fixed(2.0))
            .height(Length::Fill)
            .style(move |_| container::Style {
                background: is_focused
                    .then(|| crate::ui::color(crate::ui::core().accent_primary).into()),
                ..Default::default()
            });
        let header_h = crate::ui::metrics::panel_header() + 4.0;
        let title_bar = pane_grid::TitleBar::new(
            row![
                accent_bar,
                top_left_buttons
                    .padding(padding::left(4))
                    .align_y(Alignment::Center)
                    .spacing(8)
                    .height(Length::Fixed(header_h)),
            ]
            .height(Length::Fixed(header_h)),
        )
        .controls(top_right_buttons)
        .style(style::pane_title_bar);

        content.title_bar(if self.modal.is_none() {
            title_bar
        } else {
            title_bar.always_show_controls()
        })
    }

    pub fn update(&mut self, msg: Event) -> Option<Effect> {
        match msg {
            Event::ShowModal(requested_modal) => {
                // 从「⋯」里点开了设置 / 指标：收起溢出
                self.overflow = false;
                return self.show_modal_with_focus(requested_modal);
            }
            Event::HideModal => {
                self.modal = None;
            }
            Event::ContentSelected(kind) => {
                self.content = Content::placeholder(kind);

                // WealthSpring 面板无行情数据源（数据走 Redis/shmem 旁路）→ 不弹选 ticker 弹窗，
                // placeholder 即成品。
                if !matches!(
                    kind,
                    ContentKind::Starter
                        | ContentKind::WealthSpring
                        | ContentKind::SelfChart
                        | ContentKind::Factory
                        | ContentKind::C4Shadow
                        | ContentKind::Observatory
                        | ContentKind::NetEgress
                        | ContentKind::Procs
                        | ContentKind::News
                        | ContentKind::OptionsBoard
                        | ContentKind::PredictionBoard
                        | ContentKind::PmBinance
                        | ContentKind::PmReplay
                        | ContentKind::FeatureLab
                        | ContentKind::FeatureMatrix
                        | ContentKind::StrategyCenter
                        | ContentKind::StrategyLayers
                        | ContentKind::OfmsLab
                        | ContentKind::MarketMap
                        | ContentKind::Recorder
                        | ContentKind::TardisReplay
                        | ContentKind::TardisBoard
                ) {
                    self.streams = ResolvedStream::waiting(vec![]);
                    let modal = Modal::MiniTickersList(MiniPanel::new());

                    if let Some(effect) = self.show_modal_with_focus(modal) {
                        return Some(effect);
                    }
                }
            }
            Event::ChartInteraction(msg) => match &mut self.content {
                Content::Heatmap { chart: Some(c), .. } => {
                    super::chart::update(c, &msg);
                }
                Content::Kline { chart: Some(c), .. } => {
                    super::chart::update(c, &msg);
                }
                _ => {}
            },
            Event::PanelInteraction(msg) => match &mut self.content {
                Content::Ladder(Some(p)) => super::panel::update(p, msg),
                Content::TimeAndSales(Some(p)) => super::panel::update(p, msg),
                _ => {}
            },
            Event::RecorderInteraction(m) => {
                // 录制驾驶舱（docs/08 F6-P3）：改可编辑状态 + 副作用（systemctl / 写 toml）。
                if let Content::Recorder(rec) = &mut self.content {
                    crate::ws::recorder::handle(rec, m);
                }
            }
            Event::FactoryInteraction(m) => {
                // Factory 面板（docs/20 §26）：nightly 手动启停（副作用为 systemctl）。
                crate::ws::factory::handle(m);
            }
            Event::C4Interaction(m) => {
                // C4 活体影子：maker 影子守护启停（副作用为 systemctl，无面板状态）。
                crate::ws::c4::handle(m);
            }
            Event::ObsInteraction(m) => {
                // 接口观察终端（docs/23）：守护启停 + 连/断（副作用为 systemctl 与请求文件）。
                crate::ws::observatory::handle(m);
            }
            Event::NewsInteraction(m) => {
                // 新闻资讯：守护启停 + 打开原文（副作用为 systemctl / xdg-open）。
                crate::ws::news::handle(m);
            }
            Event::ProcsInteraction(m) => {
                // 进程页：systemctl 启停。回执写进进程级静态，下一帧显示。
                crate::ws::procs::set_note(&crate::ws::procs_view::handle(m));
            }
            Event::EgressInteraction(m) => {
                // 网络出口总闸：systemctl 启停 + Cockpit 自己的行情订阅开关。
                // 回执写进 ws::egress 的进程级静态，下一帧显示在面板上
                crate::ws::egress::set_note(&crate::ws::egress_view::handle(m));
            }
            Event::PredictionInteraction(m) => {
                // 预测市场：夜跑启停 + 定时开关（副作用为 systemctl，无面板状态）。
                crate::ws::prediction::handle(m);
            }
            Event::BacktestLaunchInteraction(m) => crate::ws::backtest_launch::handle(m),
            Event::OrdersInteraction(m) => crate::ws::orders_view::handle(m),
            Event::LinkBadgeClicked => crate::ws::provenance::on_link_click(),
            Event::ToggleFreeze => self.frozen = !self.frozen,
            Event::ToggleOverflow => self.overflow = !self.overflow,
            Event::KlineTable(m) => crate::ws::kline_table::handle(self.id, m),
            Event::SeriesTable(m) => crate::ws::series_table::handle(self.id, m),
            Event::OptionsGrid(m) => crate::ws::options_view::grid_update(m),
            Event::StrategyCenterInteraction(m) => crate::ws::strategy_center::handle(m),
            Event::StrategyLayersInteraction(m) => crate::ws::strategy_layers::handle(m),
            Event::OfmsLabInteraction(m) => crate::ws::ofms_lab::handle(m),
            Event::BacktestGrid(w, m) => crate::ws::backtest_view::grid_update(w, m),
            Event::FeatureMatrixInteraction(crate::ws::feature_matrix::FeatureMatrixMsg::Source(m)) => {
                // 特征数据源：选择 / 开始 / 停止回放。换了图表流就请上层清图。
                if crate::ws::feature_source::handle(m) {
                    return Some(Effect::ResetFeatureCharts);
                }
            }
            Event::FeatureMatrixInteraction(m) => {
                // 特征矩阵（docs/31 §8.1）：筛选/折叠只改视图状态；唯一的副作用是
                // 总开关 `Engine(start|stop)`——systemctl 启停 ws-features（后台线程，回执写进
                // ws::feature_matrix 的进程级静态）。面板本身只读旁路 JSON，不触发计算。
                // 同一份筛选对所有矩阵 pane 生效。
                // 表体横向滚动时回一个「把表头滚到同一位置」的操作，表头才能既固定又跟着横滚。
                if let Some(x) = crate::ws::feature_matrix::handle(m) {
                    return Some(Effect::ScrollX(
                        iced::widget::Id::new(crate::ws::feature_matrix::HEAD_ID),
                        x,
                    ));
                }
            }
            Event::RadarInteraction(m) => {
                // 全市场雷达（docs/22 P0b）：守护启停 + 窗口/排序切换（状态在 ws::radar 的
                // 进程级静态里，与 pane 无关——同一份口径对所有雷达 pane 生效）。
                crate::ws::radar::handle(m);
            }
            Event::TardisBoardInteraction(m) => {
                // Tardis 历史面板（docs/20 §9）：改选择 + 副作用（调 Python 生成面板 JSON）。
                if let Content::TardisBoard(tb) = &mut self.content {
                    crate::ws::tardis_board::handle(tb, m);
                }
            }
            Event::PmReplayInteraction(m) => {
                // 预测市场回放：改选择 + 副作用（调 Python 生成图表 JSON、本地推播放头）。
                if let Content::PmReplay(pr) = &mut self.content {
                    crate::ws::pm_replay::handle(pr, m);
                }
            }
            Event::TardisReplayInteraction(m) => {
                // Tardis 历史回放（docs/20 Phase 5）：改选择 + 副作用（起停 feeder 子进程）。
                if let Content::TardisReplay(tr) = &mut self.content {
                    crate::ws::tardis_replay::handle(tr, m);
                }
            }
            Event::ToggleIndicator(ind) => {
                self.content.toggle_indicator(ind);
            }
            Event::DeleteNotification(idx) => {
                if idx < self.notifications.len() {
                    self.notifications.remove(idx);
                }
            }
            Event::ReorderIndicator(e) => {
                self.content.reorder_indicators(&e);
            }
            Event::ClusterKindSelected(kind) => {
                if let Content::Kline {
                    chart, kind: cur, ..
                } = &mut self.content
                    && let Some(c) = chart
                {
                    c.set_cluster_kind(kind);
                    *cur = c.kind.clone();
                }
            }
            Event::ClusterScalingSelected(scaling) => {
                if let Content::Kline { chart, kind, .. } = &mut self.content
                    && let Some(c) = chart
                {
                    c.set_cluster_scaling(scaling);
                    *kind = c.kind.clone();
                }
            }
            Event::StudyConfigurator(study_msg) => match study_msg {
                modal::pane::settings::study::StudyMessage::Footprint(m) => {
                    if let Content::Kline { chart, kind, .. } = &mut self.content
                        && let Some(c) = chart
                    {
                        c.update_study_configurator(m);
                        *kind = c.kind.clone();
                    }
                }
                modal::pane::settings::study::StudyMessage::Heatmap(m) => {
                    if let Content::Heatmap { chart, studies, .. } = &mut self.content
                        && let Some(c) = chart
                    {
                        c.update_study_configurator(m);
                        *studies = c.studies.clone();
                    } else if let Content::ShaderHeatmap { chart, studies, .. } = &mut self.content
                        && let Some(c) = chart
                    {
                        c.update_study_configurator(m);
                        *studies = c.studies.clone();
                    }
                }
            },
            Event::StreamModifierChanged(message) => {
                if let Some(Modal::StreamModifier(mut modifier)) = self.modal.take() {
                    let mut effect: Option<Effect> = None;

                    if let Some(action) = modifier.update(message) {
                        match action {
                            modal::stream::Action::TabSelected(tab) => {
                                modifier.tab = tab;
                            }
                            modal::stream::Action::TicksizeSelected(tm) => {
                                modifier.update_kind_with_multiplier(tm);
                                self.settings.tick_multiply = Some(tm);

                                if let Some(ticker) = self.stream_pair() {
                                    match &mut self.content {
                                        Content::Kline { chart: Some(c), .. } => {
                                            c.change_tick_size(
                                                tm.multiply_with_min_tick_step(ticker),
                                            );
                                            c.reset_request_handler();
                                        }
                                        Content::Heatmap { chart: Some(c), .. } => {
                                            c.change_tick_size(
                                                tm.multiply_with_min_tick_step(ticker),
                                            );
                                        }
                                        Content::Ladder(Some(p)) => {
                                            p.set_tick_size(tm.multiply_with_min_tick_step(ticker));
                                        }
                                        Content::ShaderHeatmap {
                                            chart: Some(c),
                                            indicators,
                                            studies,
                                            ..
                                        } => {
                                            **c = HeatmapShader::new(
                                                c.basis,
                                                tm.multiply_with_min_tick_step(ticker),
                                                c.ticker_info,
                                                studies.clone(),
                                                indicators.clone(),
                                            );
                                        }
                                        _ => {}
                                    }
                                }

                                let is_client = self
                                    .stream_pair()
                                    .map(|ti| ti.exchange().is_depth_client_aggr())
                                    .unwrap_or(false);

                                if let Some(mut it) = self.streams.ready_iter_mut() {
                                    for s in &mut it {
                                        if let StreamKind::Depth { depth_aggr, .. } = s {
                                            *depth_aggr = if is_client {
                                                StreamTicksize::Client
                                            } else {
                                                StreamTicksize::ServerSide(tm)
                                            };
                                        }
                                    }
                                }
                                if !is_client {
                                    effect = Some(Effect::RefreshStreams);
                                }
                            }
                            modal::stream::Action::BasisSelected(new_basis) => {
                                modifier.update_kind_with_basis(new_basis);
                                self.settings.selected_basis = Some(new_basis);

                                let base_ticker = self.stream_pair();

                                match &mut self.content {
                                    Content::Heatmap { chart: Some(c), .. } => {
                                        c.set_basis(new_basis);

                                        if let Some(stream_type) =
                                            self.streams.ready_iter_mut().and_then(|mut it| {
                                                it.find(|s| matches!(s, StreamKind::Depth { .. }))
                                            })
                                            && let StreamKind::Depth {
                                                push_freq,
                                                ticker_info,
                                                ..
                                            } = stream_type
                                            && ticker_info.exchange().is_custom_push_freq()
                                        {
                                            match new_basis {
                                                Basis::Time(tf) => {
                                                    *push_freq = exchange::PushFrequency::Custom(tf)
                                                }
                                                Basis::Tick(_) => {
                                                    *push_freq =
                                                        exchange::PushFrequency::ServerDefault
                                                }
                                            }
                                        }

                                        effect = Some(Effect::RefreshStreams);
                                    }
                                    Content::ShaderHeatmap {
                                        chart: Some(c),
                                        indicators,
                                        ..
                                    } => {
                                        **c = HeatmapShader::new(
                                            new_basis,
                                            c.tick_size(),
                                            c.ticker_info,
                                            c.studies.clone(),
                                            indicators.clone(),
                                        );

                                        if let Some(stream_type) =
                                            self.streams.ready_iter_mut().and_then(|mut it| {
                                                it.find(|s| matches!(s, StreamKind::Depth { .. }))
                                            })
                                            && let StreamKind::Depth {
                                                push_freq,
                                                ticker_info,
                                                ..
                                            } = stream_type
                                            && ticker_info.exchange().is_custom_push_freq()
                                        {
                                            match new_basis {
                                                Basis::Time(tf) => {
                                                    *push_freq = exchange::PushFrequency::Custom(tf)
                                                }
                                                Basis::Tick(_) => {
                                                    *push_freq =
                                                        exchange::PushFrequency::ServerDefault
                                                }
                                            }
                                        }

                                        effect = Some(Effect::RefreshStreams);
                                    }
                                    Content::Kline { chart: Some(c), .. } => {
                                        if let Some(base_ticker) = base_ticker {
                                            match new_basis {
                                                Basis::Time(tf) => {
                                                    let kline_stream = StreamKind::Kline {
                                                        ticker_info: base_ticker,
                                                        timeframe: tf,
                                                    };
                                                    let mut streams = vec![kline_stream];

                                                    if matches!(
                                                        c.kind,
                                                        data::chart::KlineChartKind::Footprint { .. }
                                                    ) {
                                                        streams.push(StreamKind::Trades {
                                                            ticker_info: base_ticker,
                                                        });
                                                    }

                                                    self.streams = ResolvedStream::Ready(streams);
                                                    let action = c.set_basis(new_basis);

                                                    if let Some(chart::Action::RequestFetch(
                                                        fetch,
                                                    )) = action
                                                    {
                                                        effect = Some(Effect::RequestFetch(fetch));
                                                    }
                                                }
                                                Basis::Tick(_) => {
                                                    self.streams = ResolvedStream::Ready(vec![
                                                        StreamKind::Trades {
                                                            ticker_info: base_ticker,
                                                        },
                                                    ]);
                                                    c.set_basis(new_basis);
                                                    effect = Some(Effect::RefreshStreams);
                                                }
                                            }
                                        }
                                    }
                                    Content::Comparison(Some(c)) => {
                                        if let Basis::Time(tf) = new_basis {
                                            let streams: Vec<StreamKind> = c
                                                .selected_tickers()
                                                .iter()
                                                .copied()
                                                .map(|ti| StreamKind::Kline {
                                                    ticker_info: ti,
                                                    timeframe: tf,
                                                })
                                                .collect();

                                            self.streams = ResolvedStream::Ready(streams);
                                            let action = c.set_basis(new_basis);

                                            if let Some(chart::Action::RequestFetch(fetch)) = action
                                            {
                                                effect = Some(Effect::RequestFetch(fetch));
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }

                    self.modal = Some(Modal::StreamModifier(modifier));

                    if let Some(e) = effect {
                        return Some(e);
                    }
                }
            }
            Event::ComparisonChartInteraction(message) => {
                if let Content::Comparison(chart_opt) = &mut self.content
                    && let Some(chart) = chart_opt
                    && let Some(action) = chart.update(message)
                {
                    match action {
                        super::chart::comparison::Action::SeriesColorChanged(t, color) => {
                            chart.set_series_color(t, color);
                        }
                        super::chart::comparison::Action::SeriesNameChanged(t, name) => {
                            chart.set_series_name(t, name);
                        }
                        super::chart::comparison::Action::OpenSeriesEditor => {
                            self.modal = Some(Modal::Settings);
                        }
                        super::chart::comparison::Action::RemoveSeries(ti) => {
                            let rebuilt = chart.remove_ticker(&ti);
                            self.streams = ResolvedStream::Ready(rebuilt);

                            return Some(Effect::RefreshStreams);
                        }
                    }
                }
            }
            Event::HeatmapShaderInteraction(message) => {
                if let Content::ShaderHeatmap { chart: Some(c), .. } = &mut self.content {
                    c.update(message);
                }
            }
            Event::MiniTickersListInteraction(message) => {
                if let Some(Modal::MiniTickersList(ref mut mini_panel)) = self.modal
                    && let Some(action) = mini_panel.update(message)
                {
                    self.modal = Some(Modal::MiniTickersList(mini_panel.clone()));

                    let crate::modal::pane::mini_tickers_list::Action::RowSelected(sel) = action;
                    match sel {
                        crate::modal::pane::mini_tickers_list::RowSelection::Add(ti) => {
                            if let Content::Comparison(chart) = &mut self.content
                                && let Some(c) = chart
                            {
                                let rebuilt = c.add_ticker(&ti);
                                self.streams = ResolvedStream::Ready(rebuilt);
                                return Some(Effect::RefreshStreams);
                            }
                        }
                        crate::modal::pane::mini_tickers_list::RowSelection::Remove(ti) => {
                            if let Content::Comparison(chart) = &mut self.content
                                && let Some(c) = chart
                            {
                                let rebuilt = c.remove_ticker(&ti);
                                self.streams = ResolvedStream::Ready(rebuilt);
                                return Some(Effect::RefreshStreams);
                            }
                        }
                        crate::modal::pane::mini_tickers_list::RowSelection::Switch(ti) => {
                            return Some(Effect::SwitchTickersInGroup(ti));
                        }
                    }
                }
            }
        }
        None
    }

    fn view_controls(
        &'_ self,
        pane: pane_grid::Pane,
        total_panes: usize,
        is_maximized: bool,
        is_popout: bool,
    ) -> Element<'_, Message> {
        let modal_btn_style = |modal: Modal| {
            let is_active = self.modal == Some(modal);
            move |theme: &Theme, status: button::Status| {
                style::button::transparent(theme, status, is_active)
            }
        };

        let control_btn_style = |is_active: bool| {
            move |theme: &Theme, status: button::Status| {
                style::button::transparent(theme, status, is_active)
            }
        };

        let treat_as_starter =
            matches!(&self.content, Content::Starter) || !self.content.initialized();

        let tooltip_pos = tooltip::Position::Bottom;
        // 标题栏动作分两类（docs/35 §10 第 1 条，UPDS V2 §11：≤ 3 个动作 + 溢出菜单）：
        // 常驻 = 冻结、最大化、关闭；其余（设置、指标、弹出窗口）超过 3 个时收进「⋯」
        let mut buttons: Vec<Element<'_, Message>> = Vec::new();
        let mut primary: Vec<Element<'_, Message>> = Vec::new();

        let show_modal = |modal: Modal| Message::PaneEvent(pane, Event::ShowModal(modal));

        if !treat_as_starter {
            buttons.push(button_with_tooltip(
                icon_text(Icon::Cog, 12),
                show_modal(Modal::Settings),
                None,
                tooltip_pos,
                modal_btn_style(Modal::Settings),
            ));
        }
        if !treat_as_starter
            && matches!(
                &self.content,
                Content::Heatmap { .. } | Content::Kline { .. } | Content::ShaderHeatmap { .. }
            )
        {
            buttons.push(button_with_tooltip(
                icon_text(Icon::ChartOutline, 12),
                show_modal(Modal::Indicators),
                Some("Indicators"),
                tooltip_pos,
                modal_btn_style(Modal::Indicators),
            ));
        }

        // 冻结（docs/35 §8）：行情图表、盘口、逐笔、订单表
        if !treat_as_starter && (self.stream_pair_kind().is_some() || matches!(&self.content, Content::Orders)) {
            primary.push(button_with_tooltip(
                text(if self.frozen { "▶" } else { "❄" }).size(crate::ui::text::s_small()),
                Message::PaneEvent(pane, Event::ToggleFreeze),
                Some(if self.frozen { "解冻（追上最新数据）" } else { "冻结：停止刷新，便于阅读和截图" }),
                tooltip_pos,
                control_btn_style(self.frozen),
            ));
        }

        // 锁定布局的页：不弹出、不关闭面板（最大化 / 还原只是临时查看，照常可用）
        let locked = crate::ws::pages::active_locked();
        if is_popout {
            buttons.push(button_with_tooltip(
                icon_text(Icon::Popout, 12),
                Message::Merge,
                Some("Merge"),
                tooltip_pos,
                control_btn_style(is_popout),
            ));
        } else if total_panes > 1 && !locked {
            buttons.push(button_with_tooltip(
                icon_text(Icon::Popout, 12),
                Message::Popout,
                Some("Pop out"),
                tooltip_pos,
                control_btn_style(is_popout),
            ));
        }

        if total_panes > 1 {
            let (resize_icon, message) = if is_maximized {
                (Icon::ResizeSmall, Message::Restore)
            } else {
                (Icon::ResizeFull, Message::MaximizePane(pane))
            };

            primary.push(button_with_tooltip(
                icon_text(resize_icon, 12),
                message,
                None,
                tooltip_pos,
                control_btn_style(is_maximized),
            ));

            if !locked {
                primary.push(button_with_tooltip(
                    icon_text(Icon::Close, 12),
                    Message::ClosePane(pane),
                    None,
                    tooltip_pos,
                    control_btn_style(false),
                ));
            }
        }

        let mut bar = row![];
        let crowded = buttons.len() + primary.len() > 3;
        if !crowded || self.overflow {
            for b in buttons {
                bar = bar.push(b);
            }
        }
        if crowded {
            bar = bar.push(button_with_tooltip(
                text("⋯").size(crate::ui::text::s_small()),
                Message::PaneEvent(pane, Event::ToggleOverflow),
                Some(if self.overflow { "收起" } else { "更多：设置、指标、弹出窗口" }),
                tooltip_pos,
                control_btn_style(self.overflow),
            ));
        }
        for b in primary {
            bar = bar.push(b);
        }
        bar
            .padding(padding::right(4).left(4))
            .align_y(Alignment::Center)
            .height(Length::Fixed(32.0))
            .into()
    }

    fn compose_stack_view<'a, F>(
        &'a self,
        base: Element<'a, Message>,
        pane: pane_grid::Pane,
        indicator_modal: Option<Element<'a, Message>>,
        compact_controls: Option<Element<'a, Message>>,
        settings_modal: F,
        selected_tickers: Option<&'a [TickerInfo]>,
        tickers_table: &'a TickersTable,
    ) -> Element<'a, Message>
    where
        F: FnOnce() -> Element<'a, Message>,
    {
        let base =
            widget::toast::Manager::new(base, &self.notifications, Alignment::End, move |msg| {
                Message::PaneEvent(pane, Event::DeleteNotification(msg))
            })
            .into();

        let on_blur = Message::PaneEvent(pane, Event::HideModal);

        match &self.modal {
            Some(Modal::LinkGroup) => {
                let content = link_group_modal(pane, self.link_group);

                stack_modal(
                    base,
                    content,
                    on_blur,
                    padding::right(12).left(4),
                    Alignment::Start,
                )
            }
            Some(Modal::StreamModifier(modifier)) => stack_modal(
                base,
                modifier.view(self.stream_pair_kind()).map(move |message| {
                    Message::PaneEvent(pane, Event::StreamModifierChanged(message))
                }),
                Message::PaneEvent(pane, Event::HideModal),
                padding::right(12).left(48),
                Alignment::Start,
            ),
            Some(Modal::MiniTickersList(panel)) => {
                let mini_list = panel
                    .view(tickers_table, selected_tickers, self.stream_pair())
                    .map(move |msg| {
                        Message::PaneEvent(pane, Event::MiniTickersListInteraction(msg))
                    });

                let content: Element<_> = container(mini_list)
                    .max_width(260)
                    .padding(crate::ui::metrics::space(5))
                    .style(style::chart_modal)
                    .into();

                stack_modal(
                    base,
                    content,
                    Message::PaneEvent(pane, Event::HideModal),
                    padding::left(12),
                    Alignment::Start,
                )
            }
            Some(Modal::Settings) => stack_modal(
                base,
                settings_modal(),
                on_blur,
                padding::right(12).left(12),
                Alignment::End,
            ),
            Some(Modal::Indicators) => stack_modal(
                base,
                indicator_modal.unwrap_or_else(|| column![].into()),
                on_blur,
                padding::right(12).left(12),
                Alignment::End,
            ),
            Some(Modal::Controls) => stack_modal(
                base,
                if let Some(controls) = compact_controls {
                    controls
                } else {
                    column![].into()
                },
                on_blur,
                padding::left(12),
                Alignment::End,
            ),
            None => base,
        }
    }

    pub fn matches_stream(&self, stream: &StreamKind) -> bool {
        self.streams.matches_stream(stream)
    }

    fn show_modal_with_focus(&mut self, requested_modal: Modal) -> Option<Effect> {
        let should_toggle_close = match (&self.modal, &requested_modal) {
            (Some(Modal::StreamModifier(open)), Modal::StreamModifier(req)) => {
                open.view_mode == req.view_mode
            }
            (Some(open), req) => core::mem::discriminant(open) == core::mem::discriminant(req),
            _ => false,
        };

        if should_toggle_close {
            self.modal = None;
            return None;
        }

        let focus_widget_id = match &requested_modal {
            Modal::MiniTickersList(m) => Some(m.search_box_id.clone()),
            _ => None,
        };

        self.modal = Some(requested_modal);
        focus_widget_id.map(Effect::FocusWidget)
    }

    pub fn invalidate(&mut self, now: Instant) -> Option<Action> {
        match &mut self.content {
            Content::Heatmap { chart, .. } => chart
                .as_mut()
                .and_then(|c| c.invalidate(Some(now)).map(Action::Chart)),
            Content::Kline { chart, .. } => chart
                .as_mut()
                .and_then(|c| c.invalidate(Some(now)).map(Action::Chart)),
            Content::TimeAndSales(panel) => panel
                .as_mut()
                .and_then(|p| p.invalidate(Some(now)).map(Action::Panel)),
            Content::Ladder(panel) => panel
                .as_mut()
                .and_then(|p| p.invalidate(Some(now)).map(Action::Panel)),
            Content::Starter => None,
            Content::Comparison(chart) => chart
                .as_mut()
                .and_then(|c| c.invalidate(Some(now)).map(Action::Chart)),
            Content::ShaderHeatmap { chart, .. } => chart
                .as_mut()
                .and_then(|c| c.invalidate(Some(now)).map(Action::Chart)),
            Content::WealthSpring(_) | Content::Factory | Content::C4Shadow | Content::Observatory | Content::NetEgress | Content::Procs | Content::News | Content::OptionsBoard | Content::PredictionBoard | Content::PmBinance | Content::PmReplay(_) | Content::FeatureLab | Content::FeatureMatrix | Content::StrategyCenter | Content::StrategyLayers | Content::OfmsLab | Content::MarketMap | Content::Recorder(_) | Content::TardisReplay(_) | Content::TardisBoard(_) | Content::BacktestResult
            | Content::Orders => None,
        }
    }

    pub fn park_for_inactive_layout(&mut self) {
        if let Content::ShaderHeatmap { chart, .. } = &mut self.content {
            *chart = None;
            self.status = Status::Ready;
        }
    }

    pub fn update_interval(&self) -> Option<u64> {
        match &self.content {
            Content::Kline { .. } | Content::Comparison(_) => Some(1000),
            Content::Heatmap { chart, .. } => {
                if let Some(chart) = chart {
                    chart.basis_interval()
                } else {
                    None
                }
            }
            Content::Ladder(_) | Content::TimeAndSales(_) => Some(100),
            Content::ShaderHeatmap { .. } => None,
            Content::Starter => None,
            Content::WealthSpring(_)
            | Content::Factory
            | Content::C4Shadow
            | Content::Observatory
            | Content::NetEgress
            | Content::Procs
            | Content::News
            | Content::OptionsBoard
            | Content::PredictionBoard
            | Content::PmBinance
            | Content::PmReplay(_)
            | Content::FeatureLab
            | Content::FeatureMatrix
            | Content::StrategyCenter
            | Content::StrategyLayers
            | Content::OfmsLab
            | Content::MarketMap
            | Content::Recorder(_)
            | Content::TardisReplay(_)
            | Content::TardisBoard(_)
            | Content::BacktestResult
            | Content::Orders => None,
        }
    }

    pub fn last_tick(&self) -> Option<Instant> {
        self.content.last_tick()
    }

    pub fn tick(&mut self, now: Instant) -> Option<Action> {
        let invalidate_interval: Option<u64> = self.update_interval();
        let last_tick: Option<Instant> = self.last_tick();

        if let Some(streams) = self.streams.due_streams_to_resolve(now) {
            return Some(Action::ResolveStreams(streams));
        }

        if !self.content.initialized() {
            return Some(Action::ResolveContent);
        }

        // 冻结：不按时钟重画（图表收数据只进缓冲、重画全靠这里），画面停在冻结那一刻
        if self.frozen {
            return None;
        }

        match (invalidate_interval, last_tick) {
            (Some(interval_ms), Some(previous_tick_time)) => {
                if interval_ms > 0 {
                    let interval_duration = std::time::Duration::from_millis(interval_ms);
                    if now.duration_since(previous_tick_time) >= interval_duration {
                        return self.invalidate(now);
                    }
                }
            }
            (Some(interval_ms), None) => {
                if interval_ms > 0 {
                    return self.invalidate(now);
                }
            }
            (None, _) => {
                return self.invalidate(now);
            }
        }

        None
    }

    pub fn unique_id(&self) -> uuid::Uuid {
        self.id
    }
}

impl Default for State {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4(),
            modal: None,
            content: Content::Starter,
            settings: Settings::default(),
            streams: ResolvedStream::waiting(vec![]),
            notifications: vec![],
            status: Status::Ready,
            link_group: None,
            frozen: false,
            table_view: false,
            overflow: false,
        }
    }
}

#[derive(Default)]
pub enum Content {
    #[default]
    Starter,
    Heatmap {
        chart: Option<HeatmapChart>,
        indicators: Vec<HeatmapIndicator>,
        layout: data::chart::ViewConfig,
        studies: Vec<data::chart::heatmap::HeatmapStudy>,
    },
    ShaderHeatmap {
        chart: Option<Box<HeatmapShader>>,
        indicators: Vec<HeatmapIndicator>,
        studies: Vec<data::chart::heatmap::HeatmapStudy>,
    },
    Kline {
        chart: Option<KlineChart>,
        indicators: Vec<KlineIndicator>,
        layout: data::chart::ViewConfig,
        kind: data::chart::KlineChartKind,
    },
    TimeAndSales(Option<TimeAndSales>),
    Ladder(Option<Ladder>),
    Comparison(Option<ComparisonChart>),
    /// WealthSpring 读数面板（docs/08）：无图表/无行情流，渲染走 `ws::readout` 旁路快照。
    /// 携带三态过滤（docs/08 F6 方案 2）：Live/Backtest pane 仅在对应态渲染读数。
    WealthSpring(data::layout::pane::WsPaneMode),
    /// Alpha Factory 仪表盘（docs/08 F6-P2）：无行情流，渲染走 `ws::factory_readout` 旁路快照。
    Factory,
    /// C4 活体影子（docs/14 §2）：无行情流，渲染走 `ws::c4_readout` 旁路快照。
    C4Shadow,
    /// 数据接口观察终端（docs/23 P0）：无行情流，渲染走 `ws::observatory_readout` 旁路快照。
    /// **面板里没有一行网络代码**——连接全在 `ws-observatory` 守护里。
    Observatory,
    /// 网络出口总闸：一页看全谁在往外发包，每一路都能手动启停。
    /// **面板里没有一行网络代码**——它只数 /proc 和调 systemctl。
    NetEgress,
    /// 进程页（docs/26 S4）：WealthSpring 名下常驻单元的状态与启停。
    /// **面板里没有一行网络代码**——它只调 systemctl。
    Procs,
    /// 新闻资讯（docs/25）：无行情流，渲染走 `ws::news_readout` 旁路快照。
    /// **面板里没有一行网络代码**——连接全在 `ws-news` 守护里。
    News,
    /// 期权/0DTE 回测·探针（docs/18）：无行情流，渲染走 `ws::options_readout` 旁路快照。
    OptionsBoard,
    /// 预测市场 Polymarket（docs/19）：无行情流，渲染走 `ws::prediction_readout` 旁路快照。
    PredictionBoard,
    /// 币安钱包预测市场（BTC 5 分钟涨跌）：无行情流，渲染走 `ws::pm_binance_readout` 旁路快照。
    /// **独立于 [`Content::PredictionBoard`]**——那个是 Polymarket 的日线级决策支持，
    /// 这个是逐笔盘口（约 5 条/秒），两者放一个 pane 里谁都看不清。
    PmBinance,
    /// 预测市场回放（自录 pm_book）：携带 日期/轮次/倍速/播放位置 的可编辑状态。
    PmReplay(crate::ws::pm_replay::PmReplayState),
    /// 特征库（docs/30）：只读 feature_lab.json 旁路快照，零交易所流。
    FeatureLab,
    /// 订单流特征矩阵（docs/31 §8.1）：只读 feature_matrix.json 旁路快照，
    /// 零交易所流、**零计算**。筛选/折叠状态在 `ws::feature_matrix` 的进程级静态里，
    /// 所以这里不带载荷——同一份口径对所有矩阵 pane 生效。
    FeatureMatrix,
    /// 策略中心（docs/37 P1）：策略库 · 参数表单 · 发起回测 · 运行记录与概况。
    /// 状态在 `ws::strategy_center` 的进程级静态里（目录 / 研究库各只有一份），这里不带载荷。
    StrategyCenter,
    /// 七层（docs/39）：状态在 `ws::strategy_layers` 的进程级静态里，这里不带载荷。
    StrategyLayers,
    /// 订单流层析（docs/40）：状态在 `ws::ofms_lab` 的进程级静态里。
    OfmsLab,
    /// 全市场雷达（docs/22 P0）：无行情流，渲染走 `ws::radar_readout` 旁路快照。
    /// ⚠ 与 `Content::Heatmap`（订单簿深度热图）无关，别混（docs/22 §10 坑 1）。
    MarketMap,
    /// 录制驾驶舱（docs/08 F6-P3）：交互式控制中心，携带可编辑配置状态。
    Recorder(crate::ws::recorder::RecorderPaneState),
    /// Tardis 历史回放（docs/20 Phase 5）：交互式控制面板，携带选择状态。
    TardisReplay(crate::ws::tardis_replay::TardisReplayState),
    /// Tardis 历史面板（docs/20 §9）：源/类型/窗口选择状态；图从本地 JSON 读、自绘。
    TardisBoard(crate::ws::tardis_board::TardisBoardState),
    /// 回测结果（docs/08 F6-P7）：收益曲线/回撤/统计，渲染走 `ws::backtest_readout` 旁路快照。
    BacktestResult,
    /// 订单（docs/27 §10）：持仓/收益读数 + 活动挂单 + 逐笔明细，回测/实盘过程中实时更新。
    /// 无行情流，渲染走 `ws::readout` 旁路快照。
    Orders,
}

impl Content {
    fn new_heatmap(
        current_content: &Content,
        ticker_info: TickerInfo,
        settings: &Settings,
        price_step: exchange::unit::PriceStep,
    ) -> Self {
        let (enabled_indicators, layout, prev_studies) = if let Content::Heatmap {
            chart,
            indicators,
            studies,
            layout,
        } = current_content
        {
            (
                indicators.clone(),
                chart
                    .as_ref()
                    .map(|c| c.chart_layout())
                    .unwrap_or(layout.clone()),
                chart
                    .as_ref()
                    .map_or(studies.clone(), |c| c.studies.clone()),
            )
        } else {
            (
                vec![HeatmapIndicator::Volume],
                ViewConfig {
                    splits: vec![],
                    autoscale: Some(data::chart::Autoscale::CenterLatest),
                },
                vec![],
            )
        };

        let basis = settings
            .selected_basis
            .unwrap_or_else(|| Basis::default_heatmap_time(Some(ticker_info)));
        let config = settings.visual_config.clone().and_then(|cfg| cfg.heatmap());

        let chart = HeatmapChart::new(
            layout.clone(),
            basis,
            price_step,
            &enabled_indicators,
            ticker_info,
            config,
            prev_studies.clone(),
        );

        Content::Heatmap {
            chart: Some(chart),
            indicators: enabled_indicators,
            layout,
            studies: prev_studies,
        }
    }

    fn new_kline(
        content_kind: ContentKind,
        current_content: &Content,
        ticker_info: TickerInfo,
        settings: &Settings,
        step: exchange::unit::PriceStep,
    ) -> Self {
        let (prev_indis, prev_layout, prev_kind_opt) = if let Content::Kline {
            chart,
            indicators,
            kind,
            layout,
        } = current_content
        {
            (
                Some(indicators.clone()),
                Some(chart.as_ref().map_or(layout.clone(), |c| c.chart_layout())),
                Some(chart.as_ref().map_or(kind.clone(), |c| c.kind().clone())),
            )
        } else {
            (None, None, None)
        };

        let (default_tf, determined_chart_kind) = match content_kind {
            ContentKind::FootprintChart => (
                Timeframe::M5,
                prev_kind_opt
                    .filter(|k| matches!(k, data::chart::KlineChartKind::Footprint { .. }))
                    .unwrap_or_else(|| data::chart::KlineChartKind::Footprint {
                        clusters: data::chart::kline::ClusterKind::default(),
                        scaling: data::chart::kline::ClusterScaling::default(),
                        studies: vec![],
                    }),
            ),
            ContentKind::CandlestickChart => (Timeframe::M15, data::chart::KlineChartKind::Candles),
            _ => unreachable!("invalid content kind for kline chart"),
        };

        let basis = settings.selected_basis.unwrap_or(Basis::Time(default_tf));

        let enabled_indicators = {
            let available = KlineIndicator::for_market(ticker_info.market_type());
            prev_indis.map_or_else(
                || vec![KlineIndicator::Volume],
                |indis| {
                    indis
                        .into_iter()
                        .filter(|i| available.contains(i))
                        .collect()
                },
            )
        };

        let splits = {
            let main_chart_split: f32 = 0.8;
            let mut splits_vec = vec![main_chart_split];

            if !enabled_indicators.is_empty() {
                let num_indicators = enabled_indicators.len();

                if num_indicators > 0 {
                    let indicator_total_height_ratio = 1.0 - main_chart_split;
                    let height_per_indicator_pane =
                        indicator_total_height_ratio / num_indicators as f32;

                    let mut current_split_pos = main_chart_split;
                    for _ in 0..(num_indicators - 1) {
                        current_split_pos += height_per_indicator_pane;
                        splits_vec.push(current_split_pos);
                    }
                }
            }
            splits_vec
        };

        let layout = prev_layout
            .filter(|l| l.splits.len() == splits.len())
            .unwrap_or(ViewConfig {
                splits,
                autoscale: Some(data::chart::Autoscale::FitToVisible),
            });
        let visual_config = settings.visual_config.as_ref().and_then(|cfg| cfg.kline());

        let chart = KlineChart::new(
            layout.clone(),
            basis,
            step,
            &[],
            vec![],
            &enabled_indicators,
            ticker_info,
            &determined_chart_kind,
            visual_config,
        );

        Content::Kline {
            chart: Some(chart),
            indicators: enabled_indicators,
            layout,
            kind: determined_chart_kind,
        }
    }

    fn placeholder(kind: ContentKind) -> Self {
        match kind {
            ContentKind::Starter => Content::Starter,
            ContentKind::CandlestickChart => Content::Kline {
                chart: None,
                indicators: vec![KlineIndicator::Volume],
                kind: data::chart::KlineChartKind::Candles,
                layout: ViewConfig {
                    splits: vec![],
                    autoscale: Some(data::chart::Autoscale::FitToVisible),
                },
            },
            ContentKind::FootprintChart => Content::Kline {
                chart: None,
                indicators: vec![KlineIndicator::Volume],
                kind: data::chart::KlineChartKind::Footprint {
                    clusters: data::chart::kline::ClusterKind::default(),
                    scaling: data::chart::kline::ClusterScaling::default(),
                    studies: vec![],
                },
                layout: ViewConfig {
                    splits: vec![],
                    autoscale: Some(data::chart::Autoscale::FitToVisible),
                },
            },
            ContentKind::ShaderHeatmap => Content::ShaderHeatmap {
                chart: None,
                indicators: vec![HeatmapIndicator::Volume],
                studies: vec![data::chart::heatmap::HeatmapStudy::VolumeProfile(
                    data::chart::heatmap::ProfileKind::default(),
                )],
            },
            ContentKind::HeatmapChart => Content::Heatmap {
                chart: None,
                indicators: vec![HeatmapIndicator::Volume],
                studies: vec![],
                layout: ViewConfig {
                    splits: vec![],
                    autoscale: Some(data::chart::Autoscale::CenterLatest),
                },
            },
            ContentKind::ComparisonChart => Content::Comparison(None),
            ContentKind::TimeAndSales => Content::TimeAndSales(None),
            ContentKind::Ladder => Content::Ladder(None),
            ContentKind::WealthSpring => Content::WealthSpring(data::layout::pane::WsPaneMode::Any),
            ContentKind::SelfChart => {
                Content::WealthSpring(data::layout::pane::WsPaneMode::SelfChart)
            }
            ContentKind::Factory => Content::Factory,
            ContentKind::C4Shadow => Content::C4Shadow,
            ContentKind::Observatory => Content::Observatory,
            ContentKind::NetEgress => Content::NetEgress,
            ContentKind::Procs => Content::Procs,
            ContentKind::News => Content::News,
            ContentKind::OptionsBoard => Content::OptionsBoard,
            ContentKind::PredictionBoard => Content::PredictionBoard,
            ContentKind::PmBinance => Content::PmBinance,
            ContentKind::FeatureLab => Content::FeatureLab,
            ContentKind::FeatureMatrix => Content::FeatureMatrix,
            ContentKind::StrategyCenter => Content::StrategyCenter,
            ContentKind::StrategyLayers => Content::StrategyLayers,
            ContentKind::OfmsLab => Content::OfmsLab,
            ContentKind::PmReplay => {
                Content::PmReplay(crate::ws::pm_replay::PmReplayState::load())
            }
            ContentKind::MarketMap => Content::MarketMap,
            ContentKind::Recorder => {
                Content::Recorder(crate::ws::recorder::RecorderPaneState::load())
            }
            ContentKind::TardisReplay => {
                Content::TardisReplay(crate::ws::tardis_replay::TardisReplayState::load())
            }
            ContentKind::TardisBoard => {
                Content::TardisBoard(crate::ws::tardis_board::TardisBoardState::load())
            }
            ContentKind::BacktestResult => Content::BacktestResult,
            ContentKind::Orders => Content::Orders,
        }
    }

    pub fn last_tick(&self) -> Option<Instant> {
        match self {
            Content::Heatmap { chart, .. } => Some(chart.as_ref()?.last_update()),
            Content::Kline { chart, .. } => Some(chart.as_ref()?.last_update()),
            Content::TimeAndSales(panel) => Some(panel.as_ref()?.last_update()),
            Content::Ladder(panel) => Some(panel.as_ref()?.last_update()),
            Content::Comparison(chart) => Some(chart.as_ref()?.last_update()),
            Content::Starter => None,
            Content::ShaderHeatmap { chart, .. } => Some(chart.as_ref()?.last_tick?),
            Content::WealthSpring(_)
            | Content::Factory
            | Content::C4Shadow
            | Content::Observatory
            | Content::NetEgress
            | Content::Procs
            | Content::News
            | Content::OptionsBoard
            | Content::PredictionBoard
            | Content::PmBinance
            | Content::PmReplay(_)
            | Content::FeatureLab
            | Content::FeatureMatrix
            | Content::StrategyCenter
            | Content::StrategyLayers
            | Content::OfmsLab
            | Content::MarketMap
            | Content::Recorder(_)
            | Content::TardisReplay(_)
            | Content::TardisBoard(_)
            | Content::BacktestResult
            | Content::Orders => None,
        }
    }

    pub fn chart_kind(&self) -> Option<data::chart::KlineChartKind> {
        match self {
            Content::Kline { chart, .. } => Some(chart.as_ref()?.kind().clone()),
            _ => None,
        }
    }

    pub fn toggle_indicator(&mut self, indicator: UiIndicator) {
        match (self, indicator) {
            (
                Content::Heatmap {
                    chart, indicators, ..
                },
                UiIndicator::Heatmap(ind),
            ) => {
                let Some(chart) = chart else {
                    return;
                };

                if indicators.contains(&ind) {
                    indicators.retain(|i| i != &ind);
                } else {
                    indicators.push(ind);
                }
                chart.toggle_indicator(ind);
            }
            (
                Content::Kline {
                    chart, indicators, ..
                },
                UiIndicator::Kline(ind),
            ) => {
                let Some(chart) = chart else {
                    return;
                };

                if indicators.contains(&ind) {
                    indicators.retain(|i| i != &ind);
                } else {
                    indicators.push(ind);
                }
                chart.toggle_indicator(ind);
            }
            (
                Content::ShaderHeatmap {
                    chart, indicators, ..
                },
                UiIndicator::Heatmap(ind),
            ) => {
                let Some(chart) = chart else {
                    return;
                };

                if indicators.contains(&ind) {
                    indicators.retain(|i| i != &ind);
                } else {
                    indicators.push(ind);
                }
                chart.toggle_indicator(ind);
            }
            _ => panic!("indicator toggle on {indicator:?} pane",),
        }
    }

    pub fn reorder_indicators(&mut self, event: &column_drag::DragEvent) {
        match self {
            Content::Heatmap { indicators, .. } => column_drag::reorder_vec(indicators, event),
            Content::Kline { indicators, .. } => column_drag::reorder_vec(indicators, event),
            Content::TimeAndSales(_)
            | Content::Ladder(_)
            | Content::Starter
            | Content::Comparison(_)
            | Content::WealthSpring(_)
            | Content::Factory
            | Content::C4Shadow
            | Content::Observatory
            | Content::NetEgress
            | Content::Procs
            | Content::News
            | Content::OptionsBoard
            | Content::PredictionBoard
            | Content::PmBinance
            | Content::PmReplay(_)
            | Content::FeatureLab
            | Content::FeatureMatrix
            | Content::StrategyCenter
            | Content::StrategyLayers
            | Content::OfmsLab
            | Content::MarketMap
            | Content::Recorder(_)
            | Content::TardisReplay(_)
            | Content::TardisBoard(_)
            | Content::BacktestResult
            | Content::Orders
            | Content::ShaderHeatmap { .. } => {
                panic!("indicator reorder on {} pane", self)
            }
        }
    }

    pub fn change_visual_config(&mut self, config: VisualConfig) {
        match (self, config) {
            (Content::Kline { chart: Some(c), .. }, VisualConfig::Kline(cfg)) => {
                c.set_visual_config(cfg);
            }
            (Content::Heatmap { chart: Some(c), .. }, VisualConfig::Heatmap(cfg)) => {
                c.set_visual_config(cfg);
            }
            (Content::ShaderHeatmap { chart: Some(c), .. }, VisualConfig::Heatmap(cfg)) => {
                c.set_visual_config(cfg);
            }
            (Content::Comparison(Some(chart)), VisualConfig::Comparison(cfg)) => {
                chart.config = cfg;
            }
            (Content::TimeAndSales(Some(panel)), VisualConfig::TimeAndSales(cfg)) => {
                panel.config = cfg;
            }
            (Content::Ladder(Some(panel)), VisualConfig::Ladder(cfg)) => {
                panel.config = cfg;
            }
            _ => {}
        }
    }

    pub fn studies(&self) -> Option<data::chart::Study> {
        match &self {
            Content::Heatmap { studies, .. } => Some(data::chart::Study::Heatmap(studies.clone())),
            Content::ShaderHeatmap { studies, .. } => {
                Some(data::chart::Study::Heatmap(studies.clone()))
            }
            Content::Kline { kind, .. } => {
                if let data::chart::KlineChartKind::Footprint { studies, .. } = kind {
                    Some(data::chart::Study::Footprint(studies.clone()))
                } else {
                    None
                }
            }
            Content::TimeAndSales(_)
            | Content::Ladder(_)
            | Content::Starter
            | Content::WealthSpring(_)
            | Content::Factory
            | Content::C4Shadow
            | Content::Observatory
            | Content::NetEgress
            | Content::Procs
            | Content::News
            | Content::OptionsBoard
            | Content::PredictionBoard
            | Content::PmBinance
            | Content::PmReplay(_)
            | Content::FeatureLab
            | Content::FeatureMatrix
            | Content::StrategyCenter
            | Content::StrategyLayers
            | Content::OfmsLab
            | Content::MarketMap
            | Content::Recorder(_)
            | Content::TardisReplay(_)
            | Content::TardisBoard(_)
            | Content::BacktestResult
            | Content::Orders
            | Content::Comparison(_) => None,
        }
    }

    pub fn update_studies(&mut self, studies: data::chart::Study) {
        match (self, studies) {
            (
                Content::Heatmap {
                    chart,
                    studies: previous,
                    ..
                },
                data::chart::Study::Heatmap(studies),
            ) => {
                chart
                    .as_mut()
                    .expect("heatmap chart not initialized")
                    .studies = studies.clone();
                *previous = studies;
            }
            (
                Content::ShaderHeatmap {
                    chart,
                    studies: previous,
                    ..
                },
                data::chart::Study::Heatmap(studies),
            ) => {
                chart
                    .as_mut()
                    .expect("shader heatmap chart not initialized")
                    .studies = studies.clone();
                *previous = studies;
            }
            (Content::Kline { chart, kind, .. }, data::chart::Study::Footprint(studies)) => {
                chart
                    .as_mut()
                    .expect("kline chart not initialized")
                    .set_studies(studies.clone());
                if let data::chart::KlineChartKind::Footprint {
                    studies: k_studies, ..
                } = kind
                {
                    *k_studies = studies;
                }
            }
            _ => {}
        }
    }

    pub fn kind(&self) -> ContentKind {
        match self {
            Content::Heatmap { .. } => ContentKind::HeatmapChart,
            Content::Kline { kind, .. } => match kind {
                data::chart::KlineChartKind::Footprint { .. } => ContentKind::FootprintChart,
                data::chart::KlineChartKind::Candles => ContentKind::CandlestickChart,
            },
            Content::TimeAndSales(_) => ContentKind::TimeAndSales,
            Content::Ladder(_) => ContentKind::Ladder,
            Content::Comparison(_) => ContentKind::ComparisonChart,
            Content::Starter => ContentKind::Starter,
            Content::ShaderHeatmap { .. } => ContentKind::ShaderHeatmap,
            Content::WealthSpring(data::layout::pane::WsPaneMode::SelfChart) => {
                ContentKind::SelfChart
            }
            Content::WealthSpring(_) => ContentKind::WealthSpring,
            Content::Factory => ContentKind::Factory,
            Content::C4Shadow => ContentKind::C4Shadow,
            Content::Observatory => ContentKind::Observatory,
            Content::NetEgress => ContentKind::NetEgress,
            Content::Procs => ContentKind::Procs,
            Content::News => ContentKind::News,
            Content::OptionsBoard => ContentKind::OptionsBoard,
            Content::PredictionBoard => ContentKind::PredictionBoard,
            Content::PmBinance => ContentKind::PmBinance,
            Content::PmReplay(_) => ContentKind::PmReplay,
            Content::FeatureLab => ContentKind::FeatureLab,
            Content::FeatureMatrix => ContentKind::FeatureMatrix,
            Content::StrategyCenter => ContentKind::StrategyCenter,
            Content::StrategyLayers => ContentKind::StrategyLayers,
            Content::OfmsLab => ContentKind::OfmsLab,
            Content::MarketMap => ContentKind::MarketMap,
            Content::Recorder(_) => ContentKind::Recorder,
            Content::TardisReplay(_) => ContentKind::TardisReplay,
            Content::TardisBoard(_) => ContentKind::TardisBoard,
            Content::BacktestResult => ContentKind::BacktestResult,
            Content::Orders => ContentKind::Orders,
        }
    }

    pub fn update_theme(&mut self, theme: &iced_core::Theme) {
        if let Content::ShaderHeatmap { chart: Some(c), .. } = self {
            c.update_theme(theme);
        }
    }

    fn initialized(&self) -> bool {
        match self {
            Content::Heatmap { chart, .. } => chart.is_some(),
            Content::ShaderHeatmap { chart, .. } => chart.is_some(),
            Content::Kline { chart, .. } => chart.is_some(),
            Content::TimeAndSales(panel) => panel.is_some(),
            Content::Ladder(panel) => panel.is_some(),
            Content::Comparison(chart) => chart.is_some(),
            Content::Starter => true,
            Content::WealthSpring(_) | Content::Factory | Content::C4Shadow | Content::Observatory | Content::NetEgress | Content::Procs | Content::News | Content::OptionsBoard | Content::PredictionBoard | Content::PmBinance | Content::PmReplay(_) | Content::FeatureLab | Content::FeatureMatrix | Content::StrategyCenter | Content::StrategyLayers | Content::OfmsLab | Content::MarketMap | Content::Recorder(_) | Content::TardisReplay(_) | Content::TardisBoard(_) | Content::BacktestResult
            | Content::Orders => true,
        }
    }
}

impl std::fmt::Display for Content {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.kind())
    }
}

impl PartialEq for Content {
    fn eq(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (Content::Starter, Content::Starter)
                | (Content::Heatmap { .. }, Content::Heatmap { .. })
                | (Content::Kline { .. }, Content::Kline { .. })
                | (Content::TimeAndSales(_), Content::TimeAndSales(_))
                | (Content::Ladder(_), Content::Ladder(_))
        )
    }
}

/// 渲染数据来源徽标（docs/28 §4.2）。
///
/// 常驻而非藏进菜单：最贵的误会不是「选错源」，是**不知道自己在比两个源**——
/// 图上是管线 A 的实时、旁边的特征算的是管线 B 的历史，差异细微到足以让人
/// 追几天不存在的 bug（docs/20 §4、docs/27 §2.1 各踩过一次）。
///
/// tooltip 直接用 iced 原生而不走 `widget::tooltip` 助手：后者要 `&'a str`，
/// 而徽标详情是现算出来的 `String`，借不到那个生命周期。
fn provenance_badge<'a>() -> Element<'a, Message> {
    use crate::ws::provenance::{self, Tone};

    let b = provenance::badge(crate::ws::workspace::replay_mode());
    let color = match b.tone {
        Tone::Live => crate::ui::pal::ok(),
        Tone::Replay => crate::ui::color(crate::ui::domain().env_backtest),
        Tone::Warn => crate::ui::pal::warn(),
    };
    let chip = container(
        text(b.label).size(style::text_size::BODY).color(color).align_y(Alignment::Center),
    )
    .padding(padding::left(6).right(6))
    .height(widget::PANE_CONTROL_BTN_HEIGHT)
    .align_y(Alignment::Center);

    iced::widget::tooltip(
        chip,
        container(text(b.detail).size(style::text_size::BODY)).style(style::tooltip).padding(crate::ui::metrics::space(3)),
        tooltip::Position::Bottom,
    )
    .into()
}

/// 渲染数据链路徽标：描边 + `⇢`，与左边实心的「性质」徽标一眼分开。
///
/// 能跳转时是按钮：点它展开对应面板的数据源选择。
fn link_badge<'a>(pane_id: pane_grid::Pane, venue: &str, symbol: &str) -> Element<'a, Message> {
    use crate::ws::provenance::{self, LinkTone};

    let l = provenance::link(crate::ws::workspace::replay_mode(), venue, symbol);
    let color = match l.tone {
        LinkTone::Ok => crate::ui::pal::ok(),
        LinkTone::Running => crate::ui::pal::info(),
        LinkTone::Warn => crate::ui::pal::warn(),
        LinkTone::Bad => crate::ui::pal::bad(),
        LinkTone::Idle => crate::ui::pal::pend(),
    };
    // 始终显示完整标签（约 180px）。不用 `responsive` 按宽度收起：它恒占满剩余宽度，
    // 描边会被拉成一整条、把标题栏右侧的按钮挤走。
    let body = text(format!("⇢ {}", l.label)).size(style::text_size::BODY).color(color);
    let chip = container(body)
        .padding(padding::left(6).right(6))
        .height(widget::PANE_CONTROL_BTN_HEIGHT)
        .align_y(Alignment::Center)
        .style(move |_theme| container::Style {
            border: iced::Border { color: iced::Color { a: 0.55, ..color }, width: 1.0, radius: 3.0.into() },
            ..Default::default()
        });
    let el: Element<'a, Message> = if l.clickable {
        button(chip)
            .padding(iced::Padding::ZERO)
            .style(|_theme, _status| button::Style::default())
            .on_press(Message::PaneEvent(pane_id, Event::LinkBadgeClicked))
            .into()
    } else {
        chip.into()
    };
    iced::widget::tooltip(
        el,
        container(text(l.detail).size(style::text_size::BODY)).style(style::tooltip).padding(crate::ui::metrics::space(3)),
        tooltip::Position::Bottom,
    )
    .into()
}

fn link_group_modal<'a>(
    pane: pane_grid::Pane,
    selected_group: Option<LinkGroup>,
) -> Element<'a, Message> {
    let mut grid = column![].spacing(4);
    let rows = LinkGroup::ALL.chunks(3);

    for row_groups in rows {
        let mut button_row = row![].spacing(4);

        for &group in row_groups {
            let is_selected = selected_group == Some(group);
            let btn_content = text(group.to_string()).font(style::AZERET_MONO);

            let btn = if is_selected {
                button_with_tooltip(
                    btn_content.align_x(iced::Alignment::Center),
                    Message::SwitchLinkGroup(pane, None),
                    Some("Unlink"),
                    tooltip::Position::Bottom,
                    move |theme, status| style::button::menu_body(theme, status, true),
                )
            } else {
                button(btn_content.align_x(iced::Alignment::Center))
                    .on_press(Message::SwitchLinkGroup(pane, Some(group)))
                    .style(move |theme, status| style::button::menu_body(theme, status, false))
                    .into()
            };

            button_row = button_row.push(btn);
        }

        grid = grid.push(button_row);
    }

    container(grid)
        .max_width(240)
        .padding(crate::ui::metrics::space(5))
        .style(style::chart_modal)
        .into()
}

fn ticksize_modifier<'a>(
    id: pane_grid::Pane,
    price_step: PriceStep,
    min_ticksize: Option<exchange::unit::MinTicksize>,
    multiplier: TickMultiplier,
    modifier: Option<modal::stream::Modifier>,
    kind: ModifierKind,
    exchange: Option<exchange::adapter::Exchange>,
) -> Element<'a, Message> {
    let modifier_modal =
        Modal::StreamModifier(modal::stream::Modifier::new(kind).with_ticksize_view(
            price_step,
            min_ticksize,
            multiplier,
            exchange,
        ));

    let is_active = modifier.is_some_and(|m| {
        matches!(
            m.view_mode,
            modal::stream::ViewMode::TicksizeSelection { .. }
        )
    });

    button(text(multiplier.to_string()).align_y(Alignment::Center))
        .style(move |theme, status| style::button::modifier(theme, status, !is_active))
        .on_press(Message::PaneEvent(id, Event::ShowModal(modifier_modal)))
        .height(widget::PANE_CONTROL_BTN_HEIGHT)
        .into()
}

fn basis_modifier<'a>(
    id: pane_grid::Pane,
    selected_basis: Basis,
    modifier: Option<modal::stream::Modifier>,
    kind: ModifierKind,
) -> Element<'a, Message> {
    let modifier_modal = Modal::StreamModifier(
        modal::stream::Modifier::new(kind).with_view_mode(modal::stream::ViewMode::BasisSelection),
    );

    let is_active =
        modifier.is_some_and(|m| m.view_mode == modal::stream::ViewMode::BasisSelection);

    button(text(selected_basis.to_string()).align_y(Alignment::Center))
        .style(move |theme, status| style::button::modifier(theme, status, !is_active))
        .on_press(Message::PaneEvent(id, Event::ShowModal(modifier_modal)))
        .height(widget::PANE_CONTROL_BTN_HEIGHT)
        .into()
}

fn by_basis_default<T>(
    basis: Option<Basis>,
    default_tf: Timeframe,
    on_time: impl FnOnce(Timeframe) -> T,
    on_tick: impl FnOnce() -> T,
) -> T {
    match basis.unwrap_or(Basis::Time(default_tf)) {
        Basis::Time(tf) => on_time(tf),
        Basis::Tick(_) => on_tick(),
    }
}
