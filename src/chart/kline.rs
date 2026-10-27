use super::{
    Action, Basis, Chart, Interaction, Message, PlotConstants, PlotData, TEXT_SIZE, ViewState,
    indicator, request_fetch, scale::linear::PriceInfoLabel,
};
use crate::chart::indicator::kline::KlineIndicatorImpl;
use crate::connector::fetcher::{FetchRange, RequestHandler, is_trade_fetch_enabled};
use crate::{modal::pane::settings::study, style};
use data::aggr::ticks::TickAggr;
use data::aggr::time::TimeSeries;
use data::chart::indicator::{Indicator, KlineIndicator};
use data::chart::kline::{
    ClusterKind, ClusterScaling, Config, FootprintStudy, KlineDataPoint, KlineTrades, NPoc,
    PointOfControl,
};
use data::chart::{Autoscale, KlineChartKind, ViewConfig};

use data::util::abbr_large_numbers;
use exchange::unit::{Price, PriceStep, Qty};
use exchange::{Kline, OpenInterest as OIData, TickerInfo, Trade, UnixMs};

use iced::task::Handle;
use iced::theme::palette::Extended;
use iced::widget::canvas::{self, Event, Geometry, Path, Stroke};
use iced::{Alignment, Element, Point, Rectangle, Renderer, Size, Theme, Vector, mouse};

use enum_map::EnumMap;
use std::time::Instant;

impl Chart for KlineChart {
    type IndicatorKind = KlineIndicator;

    fn state(&self) -> &ViewState {
        &self.chart
    }

    fn mut_state(&mut self) -> &mut ViewState {
        &mut self.chart
    }

    fn invalidate_crosshair(&mut self) {
        self.chart.cache.clear_crosshair();
        self.indicators
            .values_mut()
            .filter_map(Option::as_mut)
            .for_each(|indi| indi.clear_crosshair_caches());
    }

    fn invalidate_all(&mut self) {
        self.invalidate(None);
    }

    fn view_indicators(&'_ self, enabled: &[Self::IndicatorKind]) -> Vec<Element<'_, Message>> {
        let chart_state = self.state();
        let visible_region = chart_state.visible_region(chart_state.bounds.size());
        let (earliest, latest) = chart_state.interval_range(&visible_region);
        if earliest > latest {
            return vec![];
        }

        let data_labels_always_visible = self.visual_config.data_labels_always_visible;

        let market = chart_state.ticker_info.market_type();
        let mut elements = vec![];

        for selected_indicator in enabled {
            if !KlineIndicator::for_market(market).contains(selected_indicator) {
                continue;
            }
            if let Some(indi) = self.indicators[*selected_indicator].as_ref() {
                elements.push(indi.element(
                    chart_state,
                    data_labels_always_visible,
                    earliest..=latest,
                ));
            }
        }
        elements
    }

    fn visible_timerange(&self) -> Option<(u64, u64)> {
        let chart = self.state();
        let region = chart.visible_region(chart.bounds.size());

        if region.width == 0.0 {
            return None;
        }

        Some(chart.interval_range(&region))
    }

    fn interval_keys(&self) -> Option<Vec<u64>> {
        match &self.data_source {
            PlotData::TimeBased(_) => None,
            PlotData::TickBased(tick_aggr) => Some(
                tick_aggr
                    .datapoints
                    .iter()
                    .map(|dp| dp.kline.time.as_u64())
                    .collect(),
            ),
        }
    }

    fn autoscaled_coords(&self) -> Vector {
        let chart = self.state();
        let x_translation = match &self.kind {
            KlineChartKind::Footprint { .. } => {
                0.5 * (chart.bounds.width / chart.scaling) - (chart.cell_width / chart.scaling)
            }
            KlineChartKind::Candles => {
                0.5 * (chart.bounds.width / chart.scaling)
                    - (8.0 * chart.cell_width / chart.scaling)
            }
        };
        Vector::new(x_translation, chart.translation.y)
    }

    fn supports_fit_autoscaling(&self) -> bool {
        true
    }

    fn is_empty(&self) -> bool {
        match &self.data_source {
            PlotData::TimeBased(timeseries) => timeseries.datapoints.is_empty(),
            PlotData::TickBased(tick_aggr) => tick_aggr.datapoints.is_empty(),
        }
    }

    fn drawing(&self) -> Option<(String, crate::ws::drawing::geometry::DrawView)> {
        let st = self.state();
        let ohlc = |k: &exchange::Kline| [k.open.units, k.high.units, k.low.units, k.close.units];
        // 看得见的 K 线（磁吸用）：按时间的图取视野内那几根；按逐笔的图取最近 2000 根
        let bars: Vec<(u64, [i64; 4])> = match &self.data_source {
            PlotData::TimeBased(ts) => {
                let (lo, hi) = st.visible_interval_range();
                ts.datapoints.range(UnixMs(lo)..=UnixMs(hi)).map(|(t, dp)| (t.as_u64(), ohlc(&dp.kline))).collect()
            }
            PlotData::TickBased(ta) => {
                let skip = ta.datapoints.len().saturating_sub(2000);
                ta.datapoints.iter().skip(skip).map(|dp| (dp.kline.time.as_u64(), ohlc(&dp.kline))).collect()
            }
        };
        let symbol = crate::ws::drawing::symbol_key(&st.ticker_info.ticker);
        let mut view = st.draw_view(self.interval_keys(), bars)?;
        // 成交量分布（G4）：这个品种有那种对象才带全部 K 线的（开始, 低, 高, 量）
        if crate::ws::drawing::needs_volume(&symbol) {
            // 有足迹数据（逐价位买卖量）就带上：成交量分布按价位精确落档
            let row = |k: &exchange::Kline, fp: &KlineTrades| {
                let levels = fp.trades.iter().map(|(p, g)| (p.units, f64::from((g.buy_qty + g.sell_qty).to_f32_lossy()))).collect();
                (k.time.as_u64(), k.low.units, k.high.units, f64::from(k.volume.total().to_f32_lossy()), levels)
            };
            view.vol = match &self.data_source {
                PlotData::TimeBased(ts) => ts.datapoints.values().map(|dp| row(&dp.kline, &dp.footprint)).collect(),
                PlotData::TickBased(ta) => ta.datapoints.iter().map(|dp| row(&dp.kline, &dp.footprint)).collect(),
            };
        }
        Some((symbol, view))
    }
}

impl PlotConstants for KlineChart {
    fn min_scaling(&self) -> f32 {
        self.kind.min_scaling()
    }

    fn max_scaling(&self) -> f32 {
        self.kind.max_scaling()
    }

    fn max_cell_width(&self) -> f32 {
        self.kind.max_cell_width()
    }

    fn min_cell_width(&self) -> f32 {
        self.kind.min_cell_width()
    }

    fn max_cell_height(&self) -> f32 {
        self.kind.max_cell_height()
    }

    fn min_cell_height(&self) -> f32 {
        self.kind.min_cell_height()
    }

    fn default_cell_width(&self) -> f32 {
        self.kind.default_cell_width()
    }
}

pub struct KlineChart {
    chart: ViewState,
    data_source: PlotData<KlineDataPoint>,
    raw_trades: Vec<Trade>,
    indicators: EnumMap<KlineIndicator, Option<Box<dyn KlineIndicatorImpl>>>,
    fetching_trades: (bool, Option<Handle>),
    pub(crate) kind: KlineChartKind,
    request_handler: RequestHandler,
    study_configurator: study::Configurator<FootprintStudy>,
    last_tick: Instant,
    visual_config: Config,
}

impl KlineChart {
    pub fn new(
        layout: ViewConfig,
        basis: Basis,
        step: PriceStep,
        klines_raw: &[Kline],
        raw_trades: Vec<Trade>,
        enabled_indicators: &[KlineIndicator],
        ticker_info: TickerInfo,
        kind: &KlineChartKind,
        visual_config: Option<Config>,
    ) -> Self {
        let visual_config = visual_config.unwrap_or_default();

        match basis {
            Basis::Time(interval) => {
                let timeseries = TimeSeries::<KlineDataPoint>::new(interval, step, klines_raw)
                    .with_trades(&raw_trades);

                let base_price_y = timeseries.base_price();
                let latest_x = timeseries
                    .latest_timestamp()
                    .map_or(0, |timestamp| timestamp.as_u64());
                let (scale_high, scale_low) = timeseries.price_scale({
                    match kind {
                        KlineChartKind::Footprint { .. } => 12,
                        KlineChartKind::Candles => 60,
                    }
                });

                let low_rounded = scale_low.round_to_side_step(true, step);
                let high_rounded = scale_high.round_to_side_step(false, step);

                let y_ticks = Price::steps_between_inclusive(low_rounded, high_rounded, step)
                    .map(|n| n.saturating_sub(1))
                    .unwrap_or(1)
                    .max(1) as f32;

                let cell_width = match kind {
                    KlineChartKind::Footprint { .. } => 80.0,
                    KlineChartKind::Candles => 4.0,
                };
                let cell_height = match kind {
                    KlineChartKind::Footprint { .. } => 800.0 / y_ticks,
                    KlineChartKind::Candles => 200.0 / y_ticks,
                };

                let mut chart = ViewState::new(
                    basis,
                    step,
                    step.decimal_places(),
                    ticker_info,
                    ViewConfig {
                        splits: layout.splits.clone(),
                        autoscale: Some(Autoscale::FitToVisible),
                    },
                    cell_width,
                    cell_height,
                );
                chart.base_price_y = base_price_y;
                chart.latest_x = latest_x;

                let x_translation = match &kind {
                    KlineChartKind::Footprint { .. } => {
                        0.5 * (chart.bounds.width / chart.scaling)
                            - (chart.cell_width / chart.scaling)
                    }
                    KlineChartKind::Candles => {
                        0.5 * (chart.bounds.width / chart.scaling)
                            - (8.0 * chart.cell_width / chart.scaling)
                    }
                };
                chart.translation.x = x_translation;

                let data_source = PlotData::TimeBased(timeseries);

                let mut indicators = EnumMap::default();
                for &i in enabled_indicators {
                    let mut indi = indicator::kline::make_empty(i);
                    indi.rebuild_from_source(&data_source);
                    indicators[i] = Some(indi);
                }

                KlineChart {
                    chart,
                    visual_config,
                    data_source,
                    raw_trades,
                    indicators,
                    fetching_trades: (false, None),
                    request_handler: RequestHandler::default(),
                    kind: kind.clone(),
                    study_configurator: study::Configurator::new(),
                    last_tick: Instant::now(),
                }
            }
            Basis::Tick(interval) => {
                let cell_width = match kind {
                    KlineChartKind::Footprint { .. } => 80.0,
                    KlineChartKind::Candles => 4.0,
                };
                let cell_height = match kind {
                    KlineChartKind::Footprint { .. } => 90.0,
                    KlineChartKind::Candles => 8.0,
                };

                let mut chart = ViewState::new(
                    basis,
                    step,
                    step.decimal_places(),
                    ticker_info,
                    ViewConfig {
                        splits: layout.splits.clone(),
                        autoscale: Some(Autoscale::FitToVisible),
                    },
                    cell_width,
                    cell_height,
                );

                let x_translation = match &kind {
                    KlineChartKind::Footprint { .. } => {
                        0.5 * (chart.bounds.width / chart.scaling)
                            - (chart.cell_width / chart.scaling)
                    }
                    KlineChartKind::Candles => {
                        0.5 * (chart.bounds.width / chart.scaling)
                            - (8.0 * chart.cell_width / chart.scaling)
                    }
                };
                chart.translation.x = x_translation;

                let data_source = PlotData::TickBased(TickAggr::new(interval, step, &raw_trades));

                let mut indicators = EnumMap::default();
                for &i in enabled_indicators {
                    let mut indi = indicator::kline::make_empty(i);
                    indi.rebuild_from_source(&data_source);
                    indicators[i] = Some(indi);
                }

                KlineChart {
                    chart,
                    visual_config,
                    data_source,
                    raw_trades,
                    indicators,
                    fetching_trades: (false, None),
                    request_handler: RequestHandler::default(),
                    kind: kind.clone(),
                    study_configurator: study::Configurator::new(),
                    last_tick: Instant::now(),
                }
            }
        }
    }

    pub fn update_latest_kline(&mut self, kline: &Kline) {
        match self.data_source {
            PlotData::TimeBased(ref mut timeseries) => {
                timeseries.insert_klines(&[*kline]);

                self.indicators
                    .values_mut()
                    .filter_map(Option::as_mut)
                    .for_each(|indi| indi.on_insert_klines(&[*kline], &self.data_source));

                let chart = self.mut_state();

                if kline.time.as_u64() > chart.latest_x {
                    chart.latest_x = kline.time.as_u64();
                }

                chart.last_price = Some(PriceInfoLabel::new(kline.close, kline.open));
                self.live_redraw();
            }
            PlotData::TickBased(_) => {}
        }
    }

    /// 实时数据到了就重画。原来 K 线图只按 1 秒的时钟重画（`pane::update_interval`），数据到了只进缓冲——
    /// 价格每秒最多动一次。这里只是清缓存、标记要重画，真正的绘制每帧最多一次（同一帧里多次标记自然合并），
    /// 所以不另外节流：数据到了的下一帧就画出来。
    fn live_redraw(&mut self) {
        self.invalidate_with(None, true);
    }

    /// 逐笔成交让正在走的那根 K 线跳（按时间周期的蜡烛图）：只改那根的收盘、最高、最低与价格标签；
    /// 那根还没由 K 线推送建出来就不管（K 线推送随后就到，它才是准数，到了会整根覆盖）。成交量不动。
    pub fn apply_live_trades(&mut self, buffer: &[Trade]) {
        let PlotData::TimeBased(ref mut timeseries) = self.data_source else { return };
        let iv = timeseries.interval.to_milliseconds();
        if iv == 0 || buffer.is_empty() {
            return;
        }
        let mut touched = false;
        for t in buffer {
            let bucket = UnixMs::new(t.time.as_u64() / iv * iv);
            if let Some(dp) = timeseries.datapoints.get_mut(&bucket) {
                let k = &mut dp.kline;
                if t.price > k.high {
                    k.high = t.price;
                }
                if t.price < k.low {
                    k.low = t.price;
                }
                k.close = t.price;
                touched = true;
            }
        }
        if !touched {
            return;
        }
        if let Some((_, dp)) = timeseries.datapoints.last_key_value() {
            let (close, open) = (dp.kline.close, dp.kline.open);
            self.mut_state().last_price = Some(PriceInfoLabel::new(close, open));
        }
        self.live_redraw();
    }

    pub fn kind(&self) -> &KlineChartKind {
        &self.kind
    }

    fn fetch_missing_data(&mut self) -> Option<Action> {
        match &self.data_source {
            PlotData::TimeBased(timeseries) => {
                let timeframe_ms = timeseries.interval.to_milliseconds();

                if timeseries.datapoints.is_empty() {
                    let latest = chrono::Utc::now().timestamp_millis() as u64;
                    let earliest = latest.saturating_sub(450 * timeframe_ms);

                    let range = FetchRange::Kline(UnixMs::new(earliest), UnixMs::new(latest));
                    if let Some(action) = request_fetch(&mut self.request_handler, range) {
                        return Some(action);
                    }
                }

                let (visible_earliest, visible_latest) = self.visible_timerange()?;
                let (kline_earliest, kline_latest) = timeseries.timerange();
                let visible_earliest_ms = UnixMs::new(visible_earliest);
                let visible_latest_ms = UnixMs::new(visible_latest);
                let visible_span = visible_latest.saturating_sub(visible_earliest);
                let prefetch_earliest = visible_earliest.saturating_sub(visible_span);

                // priority 1, initial klines for visible range
                if visible_earliest_ms < kline_earliest {
                    let range = FetchRange::Kline(UnixMs::new(prefetch_earliest), kline_earliest);

                    if let Some(action) = request_fetch(&mut self.request_handler, range) {
                        return Some(action);
                    }
                }

                // priority 2, trades
                if let KlineChartKind::Footprint { .. } = self.kind
                    && !self.fetching_trades.0
                    && is_trade_fetch_enabled()
                    && let Some((fetch_from, fetch_to)) =
                        timeseries.suggest_trade_fetch_range(visible_earliest_ms, visible_latest_ms)
                {
                    let range = FetchRange::Trades(fetch_from, fetch_to);
                    if let Some(action) = request_fetch(&mut self.request_handler, range) {
                        self.fetching_trades = (true, None);
                        return Some(action);
                    }
                }

                // priority 3, indicators
                // (e.g. open interest needs external fetch as it's not derived from klines)
                let ctx = indicator::kline::FetchCtx {
                    main_chart: &self.chart,
                    timeframe: timeseries.interval,
                    visible_earliest: visible_earliest_ms,
                    kline_latest,
                    prefetch_earliest: UnixMs::new(prefetch_earliest),
                };
                for indi in self.indicators.values_mut().filter_map(Option::as_mut) {
                    if let Some(range) = indi.fetch_range(&ctx)
                        && let Some(action) = request_fetch(&mut self.request_handler, range)
                    {
                        return Some(action);
                    }
                }

                // priority 4, missing klines & integrity check
                let check_earliest = UnixMs::new(prefetch_earliest).max(kline_earliest);
                let check_latest = visible_latest_ms.saturating_add(timeframe_ms);

                if let Some(missing_keys) =
                    timeseries.check_kline_integrity(check_earliest, check_latest)
                {
                    let latest = missing_keys
                        .iter()
                        .max()
                        .unwrap_or(&visible_latest_ms)
                        .saturating_add(timeframe_ms);
                    let earliest = missing_keys
                        .iter()
                        .min()
                        .unwrap_or(&visible_earliest_ms)
                        .saturating_sub(timeframe_ms);

                    let range = FetchRange::Kline(earliest, latest);
                    if let Some(action) = request_fetch(&mut self.request_handler, range) {
                        return Some(action);
                    }
                }
            }
            PlotData::TickBased(_) => {
                // TODO: implement trade fetch
            }
        }

        None
    }

    pub fn reset_request_handler(&mut self) {
        self.request_handler = RequestHandler::default();
        self.fetching_trades = (false, None);
    }

    pub fn raw_trades(&self) -> Vec<Trade> {
        self.raw_trades.clone()
    }

    pub fn set_handle(&mut self, handle: Handle) {
        self.fetching_trades.1 = Some(handle);
    }

    pub fn tick_size(&self) -> PriceStep {
        self.chart.tick_size
    }

    pub fn study_configurator(&self) -> &study::Configurator<FootprintStudy> {
        &self.study_configurator
    }

    pub fn update_study_configurator(&mut self, message: study::Message<FootprintStudy>) {
        let KlineChartKind::Footprint {
            ref mut studies, ..
        } = self.kind
        else {
            return;
        };

        match self.study_configurator.update(message) {
            Some(study::Action::ToggleStudy(study, is_selected)) => {
                if is_selected {
                    let already_exists = studies.iter().any(|s| s.is_same_type(&study));
                    if !already_exists {
                        studies.push(study);
                    }
                } else {
                    studies.retain(|s| !s.is_same_type(&study));
                }
            }
            Some(study::Action::ConfigureStudy(study)) => {
                if let Some(existing_study) = studies.iter_mut().find(|s| s.is_same_type(&study)) {
                    *existing_study = study;
                }
            }
            None => {}
        }

        self.invalidate(None);
    }

    pub fn chart_layout(&self) -> ViewConfig {
        self.chart.layout()
    }

    pub fn visual_config(&self) -> Config {
        self.visual_config
    }

    pub fn set_visual_config(&mut self, visual_config: Config) {
        self.visual_config = visual_config;
        self.chart.cache.clear_all();
        self.indicators
            .values_mut()
            .filter_map(Option::as_mut)
            .for_each(|indi| indi.clear_all_caches());
    }

    pub fn set_cluster_kind(&mut self, new_kind: ClusterKind) {
        if let KlineChartKind::Footprint {
            ref mut clusters, ..
        } = self.kind
        {
            *clusters = new_kind;
        }

        self.invalidate(None);
    }

    pub fn set_cluster_scaling(&mut self, new_scaling: ClusterScaling) {
        if let KlineChartKind::Footprint {
            ref mut scaling, ..
        } = self.kind
        {
            *scaling = new_scaling;
        }

        self.invalidate(None);
    }

    /// 最近 `n` 根 K 线，新的在前（数据表视图用，docs/35 §6.3）。
    pub fn recent_klines(&self, n: usize) -> Vec<Kline> {
        match &self.data_source {
            PlotData::TimeBased(ts) => ts.datapoints.values().rev().take(n).map(|dp| dp.kline).collect(),
            PlotData::TickBased(tick) => tick.datapoints.iter().rev().take(n).map(|dp| dp.kline).collect(),
        }
    }

    pub fn basis(&self) -> Basis {
        self.chart.basis
    }

    pub fn change_tick_size(&mut self, new_step: PriceStep) {
        let chart = self.mut_state();

        chart.cell_height *= (new_step.units as f32) / (chart.tick_size.units as f32);
        chart.tick_size = new_step;

        match self.data_source {
            PlotData::TickBased(ref mut tick_aggr) => {
                tick_aggr.change_tick_size(new_step, &self.raw_trades);
            }
            PlotData::TimeBased(ref mut timeseries) => {
                timeseries.change_tick_size(new_step, &self.raw_trades);
            }
        }

        self.indicators
            .values_mut()
            .filter_map(Option::as_mut)
            .for_each(|indi| indi.on_ticksize_change(&self.data_source));

        self.invalidate(None);
    }

    pub fn set_basis(&mut self, new_basis: Basis) -> Option<Action> {
        self.chart.last_price = None;
        self.chart.basis = new_basis;

        match new_basis {
            Basis::Time(interval) => {
                let step = self.chart.tick_size;
                let timeseries = TimeSeries::<KlineDataPoint>::new(interval, step, &[]);
                self.data_source = PlotData::TimeBased(timeseries);
            }
            Basis::Tick(tick_count) => {
                let step = self.chart.tick_size;
                let tick_aggr = TickAggr::new(tick_count, step, &self.raw_trades);
                self.data_source = PlotData::TickBased(tick_aggr);
            }
        }

        self.indicators
            .values_mut()
            .filter_map(Option::as_mut)
            .for_each(|indi| indi.on_basis_change(&self.data_source));

        self.reset_request_handler();
        self.invalidate(Some(Instant::now()))
    }

    /// 清空已有数据，保留当前 basis / 指标 / 视图设置。
    ///
    /// 复用 `set_basis`（它本就会用空 `TimeSeries` 重建 `data_source`），不自己动数据结构
    /// ——那条路是换时间周期时天天在走的，已经被用熟了。
    ///
    /// 用在「开始新一次回测」：图表会跨 run 累积，不清的话上一次回测的蜡烛还留在左边，
    /// 和本次毫无关系（docs/27 §12）。
    pub fn clear_data(&mut self) -> Option<Action> {
        self.raw_trades.clear();
        // 视窗锚点也要复位，否则清空后视窗还停在旧数据的时间位置，新数据进得去但看不见。
        self.chart.reset_anchor();
        self.set_basis(self.chart.basis)
    }

    /// 清空数据但保留视图（缩放、列宽、自动缩放方式、平移）：特征回放往回跳之后重灌用（docs/51 P3）。
    /// 与 [`Self::clear_data`] 不同，它不把视野复位——人调好的缩放往回跳一步就丢，没法看。
    /// 「最新位置」照样复位（新数据比旧的早，不复位的话视窗不动、蜡烛在屏幕外）。
    pub fn clear_data_keep_view(&mut self) -> Option<Action> {
        // 图里还没数据：视图（格高等）是新建时按空数据定的、还没被纠正过，留着只会把错的带下去
        let empty = match &self.data_source {
            PlotData::TimeBased(ts) => ts.datapoints.is_empty(),
            PlotData::TickBased(ta) => ta.datapoints.is_empty(),
        };
        if empty {
            return self.clear_data();
        }
        let c = &self.chart;
        let keep = (c.scaling, c.cell_width, c.cell_height, c.layout.autoscale, c.translation);
        let r = self.clear_data();
        let c = &mut self.chart;
        (c.scaling, c.cell_width, c.cell_height, c.layout.autoscale, c.translation) = keep;
        r
    }

    pub fn studies(&self) -> Option<Vec<FootprintStudy>> {
        match &self.kind {
            KlineChartKind::Footprint { studies, .. } => Some(studies.clone()),
            _ => None,
        }
    }

    pub fn set_studies(&mut self, new_studies: Vec<FootprintStudy>) {
        if let KlineChartKind::Footprint {
            ref mut studies, ..
        } = self.kind
        {
            *studies = new_studies;
        }

        self.invalidate(None);
    }

    pub fn insert_trades(&mut self, buffer: &[Trade]) {
        self.raw_trades.extend_from_slice(buffer);

        match self.data_source {
            PlotData::TickBased(ref mut tick_aggr) => {
                let old_dp_len = tick_aggr.datapoints.len();
                tick_aggr.insert_trades(buffer);

                if let Some(last_dp) = tick_aggr.datapoints.last() {
                    self.chart.last_price =
                        Some(PriceInfoLabel::new(last_dp.kline.close, last_dp.kline.open));
                } else {
                    self.chart.last_price = None;
                }

                self.indicators
                    .values_mut()
                    .filter_map(Option::as_mut)
                    .for_each(|indi| indi.on_insert_trades(buffer, old_dp_len, &self.data_source));

                self.invalidate(None);
            }
            PlotData::TimeBased(ref mut timeseries) => {
                timeseries.insert_trades_existing_buckets(buffer);

                self.indicators
                    .values_mut()
                    .filter_map(Option::as_mut)
                    .for_each(|indi| indi.on_insert_trades(buffer, 0, &self.data_source));

                self.invalidate(None);
            }
        }
    }

    pub fn insert_raw_trades(&mut self, raw_trades: Vec<Trade>, is_batches_done: bool) {
        match self.data_source {
            PlotData::TickBased(ref mut tick_aggr) => {
                tick_aggr.insert_trades(&raw_trades);
            }
            PlotData::TimeBased(ref mut timeseries) => {
                timeseries.insert_trades_existing_buckets(&raw_trades);
            }
        }

        self.raw_trades.extend_from_slice(&raw_trades);

        self.indicators
            .values_mut()
            .filter_map(Option::as_mut)
            .for_each(|indi| indi.on_insert_trades(&raw_trades, 0, &self.data_source));

        if is_batches_done {
            self.fetching_trades = (false, None);
        }

        self.invalidate(None);
    }

    pub fn insert_hist_klines(&mut self, req_id: uuid::Uuid, klines_raw: &[Kline]) {
        match self.data_source {
            PlotData::TimeBased(ref mut timeseries) => {
                timeseries.insert_klines(klines_raw);
                timeseries.insert_trades_existing_buckets(&self.raw_trades);

                // 历史批量插入后补一次视口锚点：`latest_x` 原来只在**实时 K 线**到达时更新
                // （见 `insert_new_kline`），而回放态工作区把交易所流关掉了，永远等不到实时 tick
                // → `latest_x` 停在 0、视口锚在 epoch 0，图上有 1450 根历史 K 线却什么都看不见，
                // 只剩一条「0.1/0.0/-0.1、00:01~00:07」的假坐标轴（docs/20 §28）。
                if let Some(t) = timeseries.latest_timestamp() {
                    let newest = t.as_u64();
                    if self.chart.latest_x < newest {
                        self.chart.latest_x = newest;
                    }
                }

                self.indicators
                    .values_mut()
                    .filter_map(Option::as_mut)
                    .for_each(|indi| indi.on_insert_klines(klines_raw, &self.data_source));

                if klines_raw.is_empty() {
                    self.request_handler
                        .mark_failed(req_id, "No data received".to_string());
                } else {
                    self.request_handler.mark_completed(req_id);
                }
                self.invalidate(None);
            }
            PlotData::TickBased(_) => {}
        }
    }

    pub fn insert_open_interest(&mut self, req_id: Option<uuid::Uuid>, oi_data: &[OIData]) {
        if let Some(req_id) = req_id {
            if oi_data.is_empty() {
                self.request_handler
                    .mark_failed(req_id, "No data received".to_string());
            } else {
                self.request_handler.mark_completed(req_id);
            }
        }

        if let Some(indi) = self.indicators[KlineIndicator::OpenInterest].as_mut() {
            indi.on_open_interest(oi_data);
        }
    }

    fn calc_qty_scales(
        &self,
        earliest: u64,
        latest: u64,
        highest: Price,
        lowest: Price,
        step: PriceStep,
        cluster_kind: ClusterKind,
    ) -> f32 {
        let rounded_highest = highest.round_to_side_step(false, step).add_steps(1, step);
        let rounded_lowest = lowest.round_to_side_step(true, step).add_steps(-1, step);

        match &self.data_source {
            PlotData::TimeBased(timeseries) => timeseries
                .max_qty_ts_range(
                    cluster_kind,
                    UnixMs::new(earliest),
                    UnixMs::new(latest),
                    rounded_highest,
                    rounded_lowest,
                )
                .into(),
            PlotData::TickBased(tick_aggr) => {
                let earliest = earliest as usize;
                let latest = latest as usize;

                tick_aggr
                    .max_qty_idx_range(
                        cluster_kind,
                        earliest,
                        latest,
                        rounded_highest,
                        rounded_lowest,
                    )
                    .into()
            }
        }
    }

    pub fn last_update(&self) -> Instant {
        self.last_tick
    }

    pub fn invalidate(&mut self, now: Option<Instant>) -> Option<Action> {
        self.invalidate_with(now, false)
    }

    /// `from_live`：实时数据触发的。视野（自动缩放 / 跟随最新）算完没变，就只清主图、价格轴、十字线三层（[`ViewState::clear_live`]）。
    fn invalidate_with(&mut self, now: Option<Instant>, from_live: bool) -> Option<Action> {
        let view_before = self.chart.view_key();
        // 「在图上看」（docs/43 §8）：把请求的时刻放到视野中心，价格轴改回自动适配（否则那段 K 线可能在上下边外）
        if matches!(self.chart.basis, Basis::Time(_))
            && let Some((v, at)) = super::goto_time::pending(self.chart.goto_seen)
        {
            let fit = self.supports_fit_autoscaling();
            let chart = &mut self.chart;
            chart.goto_seen = v;
            chart.translation.x = -chart.interval_to_x(at);
            if fit {
                chart.layout.autoscale = Some(super::Autoscale::FitToVisible);
                chart.scaling = 1.0;
            }
            super::goto_time::ack(v);
        }
        // 特征回放（docs/51 P3）：别的时间轴图被拖动 / 缩放了，视野中心跟过去
        let fit = self.supports_fit_autoscaling();
        let live = self.autoscaled_coords();
        self.chart.apply_view_sync(fit, live);
        // 画线到价提醒（docs/46 G5）：每次有新数据，拿最新一根的收盘价查一遍（时间用那根 K 线的，回放时也对）
        let last = match &self.data_source {
            PlotData::TimeBased(ts) => ts.datapoints.values().next_back().map(|d| d.kline),
            PlotData::TickBased(ta) => ta.datapoints.last().map(|d| d.kline),
        };
        if let Some(k) = last {
            crate::ws::drawing::on_price(&self.chart.drawing_symbol(), k.close.units, k.time.as_u64(), self.chart.decimals);
        }
        let chart = &mut self.chart;

        if let Some(autoscale) = chart.layout.autoscale {
            match autoscale {
                super::Autoscale::CenterLatest => {
                    let x_translation = match &self.kind {
                        KlineChartKind::Footprint { .. } => {
                            0.5 * (chart.bounds.width / chart.scaling)
                                - (chart.cell_width / chart.scaling)
                        }
                        KlineChartKind::Candles => {
                            0.5 * (chart.bounds.width / chart.scaling)
                                - (8.0 * chart.cell_width / chart.scaling)
                        }
                    };
                    chart.translation.x = x_translation;

                    let calculate_target_y = |kline: exchange::Kline| -> f32 {
                        let y_low = chart.price_to_y(kline.low);
                        let y_high = chart.price_to_y(kline.high);
                        let y_close = chart.price_to_y(kline.close);

                        let mut target_y_translation = -(y_low + y_high) / 2.0;

                        if chart.bounds.height > f32::EPSILON && chart.scaling > f32::EPSILON {
                            let visible_half_height = (chart.bounds.height / chart.scaling) / 2.0;

                            let view_center_y_centered = -target_y_translation;

                            let visible_y_top = view_center_y_centered - visible_half_height;
                            let visible_y_bottom = view_center_y_centered + visible_half_height;

                            let padding = chart.cell_height;

                            if y_close < visible_y_top {
                                target_y_translation = -(y_close - padding + visible_half_height);
                            } else if y_close > visible_y_bottom {
                                target_y_translation = -(y_close + padding - visible_half_height);
                            }
                        }
                        target_y_translation
                    };

                    chart.translation.y = self.data_source.latest_y_midpoint(calculate_target_y);
                }
                super::Autoscale::FitToVisible => {
                    let visible_region = chart.visible_region(chart.bounds.size());
                    let (start_interval, end_interval) = chart.interval_range(&visible_region);

                    if let Some((lowest, highest)) = self
                        .data_source
                        .visible_price_range(start_interval, end_interval)
                    {
                        let chart_height = chart.bounds.height;
                        let tick_size = chart.tick_size.to_f32_lossy();

                        if chart_height > f32::EPSILON && tick_size > 0.0 {
                            let (fit_lowest, fit_highest) =
                                if let KlineChartKind::Footprint { .. } = self.kind {
                                    if let Some((footprint_low, footprint_high)) = self
                                        .data_source
                                        .visible_footprint_price_range(start_interval, end_interval)
                                    {
                                        let half_tick = tick_size * 0.5;
                                        (
                                            footprint_low.to_f32_lossy() - half_tick,
                                            footprint_high.to_f32_lossy() + half_tick,
                                        )
                                    } else {
                                        (lowest, highest)
                                    }
                                } else {
                                    (lowest, highest)
                                };

                            let visible_span = (fit_highest - fit_lowest).max(tick_size);
                            let base_padding = visible_span * 0.05; // 5% padding on top and bottom

                            let mut top_padding = base_padding;
                            let mut bottom_padding = base_padding;

                            if let KlineChartKind::Footprint { clusters, .. } = self.kind {
                                let provisional_span = visible_span + top_padding + bottom_padding;
                                if provisional_span > 0.0 {
                                    let provisional_cell_height =
                                        (chart_height * tick_size) / provisional_span;

                                    let outer_padding = price_padding_from_pixels(
                                        provisional_cell_height,
                                        tick_size,
                                    );

                                    top_padding += outer_padding;
                                    bottom_padding += outer_padding;

                                    bottom_padding = bottom_padding.max(footprint_summary_padding(
                                        provisional_cell_height,
                                        chart.scaling,
                                        chart.cell_width,
                                        tick_size,
                                        clusters,
                                    ));
                                }
                            }

                            let padded_span = visible_span + top_padding + bottom_padding;
                            if padded_span > 0.0 {
                                chart.cell_height = (chart_height * tick_size) / padded_span;
                                chart.base_price_y = Price::from_f32(fit_highest + top_padding);
                                chart.translation.y = -chart_height / 2.0;
                            }
                        }
                    }
                }
            }
        }

        if from_live && chart.view_key() == view_before {
            chart.clear_live();
        } else {
            chart.clear_all();
        }
        for indi in self.indicators.values_mut().filter_map(Option::as_mut) {
            indi.clear_all_caches();
        }

        if let Some(t) = now {
            self.last_tick = t;
            self.fetch_missing_data()
        } else {
            None
        }
    }

    pub fn toggle_indicator(&mut self, indicator: KlineIndicator) {
        let prev_indi_count = self.indicators.values().filter(|v| v.is_some()).count();

        if self.indicators[indicator].is_some() {
            self.indicators[indicator] = None;
        } else {
            let mut box_indi = indicator::kline::make_empty(indicator);
            box_indi.rebuild_from_source(&self.data_source);
            self.indicators[indicator] = Some(box_indi);
        }

        if let Some(main_split) = self.chart.layout.splits.first() {
            let current_indi_count = self.indicators.values().filter(|v| v.is_some()).count();
            self.chart.layout.splits = data::util::calc_panel_splits(
                *main_split,
                current_indi_count,
                Some(prev_indi_count),
            );
        }
    }
}

impl canvas::Program<Message> for KlineChart {
    type State = Interaction;

    fn update(
        &self,
        interaction: &mut Interaction,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        super::canvas_interaction(self, interaction, event, bounds, cursor)
    }

    fn draw(
        &self,
        interaction: &Interaction,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let chart = self.state();

        if chart.bounds.width == 0.0 {
            return vec![];
        }

        let bounds_size = bounds.size();
        let palette = theme.extended_palette();

        // 市场状态叠加层（docs/62 R2）画在主图缓存里：时间线文件或设置变了就重画主图
        // 图上指标（docs/63 I3）的设置变了也一样（`|` 不短路：两个版本都要记下）
        if matches!(chart.basis, Basis::Time(_))
            && (crate::ws::regime::changed_since(&chart.regime_seen) | crate::ws::studies::changed_since(&chart.studies_seen))
        {
            chart.cache.main.clear();
        }
        let regime_symbol = crate::ws::drawing::symbol_key(&chart.ticker_info.ticker);

        let klines = chart.cache.main.draw(renderer, bounds_size, |frame| {
            let center = Vector::new(bounds.width / 2.0, bounds.height / 2.0);

            frame.translate(center);
            frame.scale(chart.scaling);
            frame.translate(chart.translation);

            let region = chart.visible_region(frame.size());
            let (earliest, latest) = chart.interval_range(&region);

            let price_to_y = |price| chart.price_to_y(price);
            let interval_to_x = |interval| chart.interval_to_x(interval);

            match &self.kind {
                KlineChartKind::Footprint {
                    clusters,
                    scaling,
                    studies,
                } => {
                    let (highest, lowest) = chart.price_range(&region);

                    let max_cluster_qty = self.calc_qty_scales(
                        earliest,
                        latest,
                        highest,
                        lowest,
                        chart.tick_size,
                        *clusters,
                    );

                    let cell_height_unscaled = chart.cell_height * chart.scaling;
                    let cell_width_unscaled = chart.cell_width * chart.scaling;

                    let text_size =
                        footprint_cluster_text_size(cell_height_unscaled, cell_width_unscaled);

                    let candle_width = 0.1 * chart.cell_width;
                    let content_spacing = ContentGaps::from_view(candle_width, chart.scaling);

                    let imbalance = studies.iter().find_map(|study| {
                        if let FootprintStudy::Imbalance {
                            threshold,
                            color_scale,
                            ignore_zeros,
                        } = study
                        {
                            Some((*threshold, *color_scale, *ignore_zeros))
                        } else {
                            None
                        }
                    });

                    let show_text = should_show_text(
                        cell_height_unscaled,
                        cell_width_unscaled,
                        footprint_cluster_min_width(*clusters),
                    );

                    draw_all_npocs(
                        &self.data_source,
                        frame,
                        price_to_y,
                        interval_to_x,
                        candle_width,
                        chart.cell_width,
                        chart.cell_height,
                        palette,
                        studies,
                        earliest,
                        latest,
                        *clusters,
                        content_spacing,
                        imbalance.is_some(),
                    );

                    render_data_source(
                        &self.data_source,
                        frame,
                        earliest,
                        latest,
                        interval_to_x,
                        |frame, x_position, kline, trades| {
                            let cluster_scaling =
                                effective_cluster_qty(*scaling, max_cluster_qty, trades, *clusters);

                            draw_clusters(
                                frame,
                                price_to_y,
                                x_position,
                                chart.cell_width,
                                chart.cell_height,
                                candle_width,
                                cluster_scaling,
                                palette,
                                text_size,
                                self.tick_size(),
                                show_text,
                                imbalance,
                                kline,
                                trades,
                                *clusters,
                                content_spacing,
                            );
                        },
                    );
                }
                KlineChartKind::Candles => {
                    let candle_width = chart.cell_width * 0.8;

                    render_data_source(
                        &self.data_source,
                        frame,
                        earliest,
                        latest,
                        interval_to_x,
                        |frame, x_position, kline, _| {
                            draw_candle_dp(
                                frame,
                                price_to_y,
                                candle_width,
                                palette,
                                x_position,
                                kline,
                            );
                        },
                    );
                }
            }

            // 市场状态（docs/62 R2）：底色带 + 底部条带 + 切换标记，画在 K 线下面一层的颜色上（半透明）
            if let PlotData::TimeBased(ts) = &self.data_source {
                let iv = ts.interval.to_milliseconds();
                crate::ws::regime::draw_overlay(
                    frame,
                    region,
                    &interval_to_x,
                    chart.scaling,
                    earliest,
                    latest,
                    &regime_symbol,
                    iv,
                    crate::ws::studies::sub_height(&regime_symbol, iv, chart.scaling),
                );
                // 图上指标（docs/63 I3）：叠加线 / 带 + 副图分区（摞在状态条带上面）。计算在指标核心，按图增量
                if !crate::ws::studies::studies_for(&regime_symbol).is_empty() {
                    let kl: Vec<crate::ws::studies::Ohlcv> = ts
                        .datapoints
                        .iter()
                        .map(|(t, dp)| {
                            let k = &dp.kline;
                            let p = |x: Price| x.units as f64 / 1e11;
                            (t.as_u64(), p(k.open), p(k.high), p(k.low), p(k.close), k.volume.total().units as f64 / 1e8)
                        })
                        .collect();
                    // 主动买入量（K 线带买卖量的交易所才有；CVD 之类要它，docs/63 §10）
                    let buy: Vec<f64> = ts
                        .datapoints
                        .values()
                        .map(|dp| match dp.kline.volume {
                            exchange::Volume::BuySell(b, _) => b.units as f64 / 1e8,
                            exchange::Volume::TotalOnly(_) => f64::NAN,
                        })
                        .collect();
                    // 图上最后一根始终当未收盘：到了收盘时刻、交易所的最终版还没推来时，按「现在」会把它当已收盘，
                    // 最终版一到又判成「数据修正」（1m 图上每分钟一次）；交易所开出下一根，它才算收盘
                    let now = chrono::Utc::now().timestamp_millis().max(0) as u64;
                    let now = kl.last().map_or(now, |k| now.min(k.0 + iv - 1));
                    let comp = crate::ws::studies::compute_ex(&regime_symbol, iv, &kl, Some(&buy), now);
                    let price_to_y_f64 = |v: f64| chart.price_to_y(Price::from_units((v * 1e11).round() as i64));
                    crate::ws::studies::draw(
                        frame,
                        region,
                        &interval_to_x,
                        &price_to_y_f64,
                        chart.scaling,
                        earliest,
                        latest,
                        &regime_symbol,
                        iv,
                        &comp,
                        crate::ws::regime::bottom_reserved(&regime_symbol, iv, chart.scaling),
                    );
                }
            }

            // 缺口标记（docs/35 §6.3）：时间轴上缺了 K 线的地方画淡色带，不插值连过去
            if let PlotData::TimeBased(ts) = &self.data_source {
                draw_gaps(frame, ts, &interval_to_x, region, chart.scaling, earliest, latest);
            }

            // 金融日历（docs/43 K5b）：P0/P1 事件的时刻画虚线竖线——读提醒线程的旁路快照，不读库。仅时间基
            if matches!(chart.basis, Basis::Time(_)) {
                draw_calendar_edge_hint(frame, region, chart.scaling, earliest, latest);
            }

            // 成交 ▲▼、持仓线、挂单线、日历事件竖线：绘图对象（docs/46 G4，`ws::drawing::feeds`），由绘图层画

            // F4c：combo 实时总分曲线叠加主图底带（读 ws::signals 旁路），仅时间基。
            if matches!(chart.basis, Basis::Time(_)) {
                draw_ws_combo_overlay(
                    frame,
                    interval_to_x,
                    region,
                    chart.scaling,
                    palette,
                    earliest,
                    latest,
                );
            }


            chart.draw_last_price_line(frame, palette, region);
        });

        // 画线（docs/46）：单独一层；版本变了才重画
        let drawing = self.drawing();
        let drawings = chart.draw_drawings_layer(renderer, bounds_size, drawing.as_ref());

        chart.sync_linked_cursor();
        let crosshair = chart.cache.crosshair.draw(renderer, bounds_size, |frame| {
            if let Some((symbol, view)) = &drawing {
                crate::ws::drawing::draw_hint(frame, view, symbol, cursor.position_in(bounds));
            }
            let visible_region = chart.visible_region(bounds_size);
            let visible_range = chart.interval_range(&visible_region);
            // 特征回放的播放头（docs/51 P3）
            chart.draw_playhead(frame, theme, bounds_size);
            chart.draw_event_marks(frame, theme, bounds_size);

            if let Some(cursor_position) = cursor.position_in(bounds) {
                let (_, rounded_aggregation) =
                    chart.draw_crosshair(frame, theme, bounds_size, cursor_position, interaction);
                // 同页图表时间联动（docs/41 E 期）
                chart.publish_linked_cursor(Some(rounded_aggregation));

                draw_crosshair_tooltip(
                    &self.data_source,
                    &chart.ticker_info,
                    frame,
                    palette,
                    chart.basis,
                    Some(rounded_aggregation),
                    visible_range,
                );
            } else {
                chart.publish_linked_cursor(None);
                chart.draw_linked_cursor(frame, theme, bounds_size);
            }
            if cursor.position_in(bounds).is_none() && self.visual_config.data_labels_always_visible {
                draw_crosshair_tooltip(
                    &self.data_source,
                    &chart.ticker_info,
                    frame,
                    palette,
                    chart.basis,
                    None,
                    visible_range,
                );
            }
        });

        let mut out = vec![klines];
        out.extend(drawings);
        out.push(crosshair);
        out
    }

    fn mouse_interaction(
        &self,
        interaction: &Interaction,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        match interaction {
            Interaction::Panning { .. } | Interaction::DrawDrag => mouse::Interaction::Grabbing,
            Interaction::Zoomin { .. } => mouse::Interaction::ZoomIn,
            // 画线（docs/46 G1）：没选工具时悬停在对象上 = 手形，提示可以点
            Interaction::None
                if crate::ws::drawing::tool().is_none()
                    && let Some(p) = cursor.position_in(bounds)
                    && let Some((symbol, v)) = self.drawing()
                    && crate::ws::drawing::hovering(&symbol, &v, p) =>
            {
                mouse::Interaction::Pointer
            }
            Interaction::None | Interaction::Ruler { .. } | Interaction::DrawPlace => {
                if cursor.is_over(bounds) {
                    mouse::Interaction::Crosshair
                } else {
                    mouse::Interaction::default()
                }
            }
        }
    }
}

fn draw_footprint_kline(
    frame: &mut canvas::Frame,
    price_to_y: impl Fn(Price) -> f32,
    x_position: f32,
    candle_width: f32,
    kline: &Kline,
    _palette: &Extended,
) {
    let y_open = price_to_y(kline.open);
    let y_high = price_to_y(kline.high);
    let y_low = price_to_y(kline.low);
    let y_close = price_to_y(kline.close);

    let body_color = if kline.close >= kline.open {
        crate::ui::chart::up_weak()
    } else {
        crate::ui::chart::down_weak()
    };
    frame.fill_rectangle(
        Point::new(x_position - (candle_width / 8.0), y_open.min(y_close)),
        Size::new(candle_width / 4.0, (y_open - y_close).abs()),
        body_color,
    );

    let wick_color = if kline.close >= kline.open {
        crate::ui::chart::up_weak()
    } else {
        crate::ui::chart::down_weak()
    };
    let marker_line = Stroke::with_color(
        Stroke {
            width: 1.0,
            ..Default::default()
        },
        wick_color.scale_alpha(0.6),
    );
    frame.stroke(
        &Path::line(
            Point::new(x_position, y_high),
            Point::new(x_position, y_low),
        ),
        marker_line,
    );
}

fn draw_candle_dp(
    frame: &mut canvas::Frame,
    price_to_y: impl Fn(Price) -> f32,
    candle_width: f32,
    _palette: &Extended,
    x_position: f32,
    kline: &Kline,
) {
    let y_open = price_to_y(kline.open);
    let y_high = price_to_y(kline.high);
    let y_low = price_to_y(kline.low);
    let y_close = price_to_y(kline.close);

    let body_color = if kline.close >= kline.open {
        crate::ui::chart::up()
    } else {
        crate::ui::chart::down()
    };
    frame.fill_rectangle(
        Point::new(x_position - (candle_width / 2.0), y_open.min(y_close)),
        Size::new(candle_width, (y_open - y_close).abs()),
        body_color,
    );

    let wick_color = if kline.close >= kline.open {
        crate::ui::chart::up()
    } else {
        crate::ui::chart::down()
    };
    frame.fill_rectangle(
        Point::new(x_position - (candle_width / 8.0), y_high),
        Size::new(candle_width / 4.0, (y_high - y_low).abs()),
        wick_color,
    );
}

/// F3b：图上成交标记。买=绿 ▲（成交价下方、指向上），卖=红 ▼（成交价上方、指向下）。
/// 标记尺寸用 像素/scaling 抵消 `frame.scale`，保证屏幕上恒定大小；带描边以在红绿蜡烛上保持可见。
/// 数据来自进程级旁路缓存 `ws::orders::chart_fills_snapshot()`（不侵入 FS 的 ContentKind/数据源）。
#[allow(clippy::too_many_arguments)]
/// 金融日历的事件竖线（docs/43 K5b）：P0 红、P1 橙（状态色，不是涨跌色），顶部写短名（FOMC、CPI、非农……）。
/// 「提醒」页可以关。线宽 / 字号 ÷scaling，屏幕上恒定。视野里没有事件时，左右边缘提示最近的前一个 / 下一个。
/// 金融日历（docs/43 K5b）：视野里一条事件竖线都没有时，在左右边缘标出最近的前一个 / 下一个事件。
/// 视野里的竖线本身是绘图对象（docs/46 G4，`ws::drawing::feeds` 的日历来源），由绘图层画。
fn draw_calendar_edge_hint(frame: &mut canvas::Frame, region: Rectangle, scaling: f32, earliest: u64, latest: u64) {
    if scaling <= f32::EPSILON {
        return;
    }
    let snap = crate::ws::calendar_alerts::snapshot();
    if !snap.chart_lines {
        return;
    }
    let in_view = |m: &&crate::ws::calendar_alerts::ChartMark| m.at_ms >= 0 && (m.at_ms as u64) >= earliest && (m.at_ms as u64) <= latest;
    // 视野里一条都没有（1 分钟线只看得到几个小时）：在左右边缘标出最近的前一个 / 下一个事件，告诉用户往哪边拖
    if !snap.marks.iter().any(|m| in_view(&m)) {
        let prev = snap.marks.iter().filter(|m| m.at_ms >= 0 && (m.at_ms as u64) < earliest).max_by_key(|m| m.at_ms);
        let next = snap.marks.iter().filter(|m| m.at_ms >= 0 && (m.at_ms as u64) > latest).min_by_key(|m| m.at_ms);
        let day = |ms: i64| {
            chrono::TimeZone::timestamp_millis_opt(&chrono::Local, ms).single().map(|t| t.format("%m-%d").to_string()).unwrap_or_default()
        };
        for (m, left) in [(prev, true), (next, false)] {
            let Some(m) = m else { continue };
            let col = if m.importance == 0 { crate::ui::widgets::Tone::Danger.color() } else { crate::ui::widgets::Tone::Warning.color() };
            let (content, x, align) = if left {
                (format!("← {} {}", m.label, day(m.at_ms)), region.x + 4.0 / scaling, Alignment::Start)
            } else {
                (format!("{} {} →", m.label, day(m.at_ms)), region.x + region.width - 4.0 / scaling, Alignment::End)
            };
            frame.fill_text(canvas::Text {
                content,
                position: Point::new(x, region.y + 2.0 / scaling),
                size: iced::Pixels(10.0 / scaling),
                color: col.scale_alpha(0.85),
                font: crate::ui::text::ui_font(),
                align_x: align.into(),
                align_y: Alignment::Start.into(),
                ..canvas::Text::default()
            });
        }
    }
}

/// F4c：combo 现役池实时总分曲线，叠加在主图底部 ~22% 带内（自适应值域 + 0 基线）。
/// 数据来自进程级旁路 `ws::signals::combo_snapshot()`（接收时本机时戳，仅时间基有意义）。
/// 线宽/字号 ÷scaling 保持屏幕恒定；末点圆点 + 当前值标签按符号着色。
#[allow(clippy::too_many_arguments)]
fn draw_ws_combo_overlay(
    frame: &mut canvas::Frame,
    interval_to_x: impl Fn(u64) -> f32,
    region: Rectangle,
    scaling: f32,
    palette: &Extended,
    earliest: u64,
    latest: u64,
) {
    if scaling <= f32::EPSILON {
        return;
    }
    let vis: Vec<(u64, f64)> = crate::ws::signals::combo_snapshot()
        .into_iter()
        .filter(|(t, _)| *t >= earliest && *t <= latest)
        .collect();
    if vis.len() < 2 {
        return;
    }
    // 底带几何：region 下方 22% 带、底留 4% 边距
    let band_h = (region.height * 0.22).max(1.0);
    let bottom = region.y + region.height * 0.96;
    let top = bottom - band_h;
    // combo 值域（含 0 基线）
    let mut lo = 0.0_f64;
    let mut hi = 0.0_f64;
    for (_, v) in &vis {
        lo = lo.min(*v);
        hi = hi.max(*v);
    }
    if (hi - lo).abs() < 1e-9 {
        hi = lo + 1.0;
    }
    let y_of = |v: f64| -> f32 { bottom - ((v - lo) / (hi - lo)) as f32 * band_h };

    // 0 基线
    let zy = y_of(0.0);
    frame.stroke(
        &Path::line(
            Point::new(region.x, zy),
            Point::new(region.x + region.width, zy),
        ),
        Stroke::default()
            .with_width(1.0 / scaling)
            .with_color(palette.background.base.text.scale_alpha(0.3)),
    );
    // combo 折线
    let line = Path::new(|b| {
        let (t0, v0) = vis[0];
        b.move_to(Point::new(interval_to_x(t0), y_of(v0)));
        for (t, v) in vis.iter().skip(1) {
            b.line_to(Point::new(interval_to_x(*t), y_of(*v)));
        }
    });
    frame.stroke(
        &line,
        Stroke::default()
            .with_width(1.5 / scaling)
            .with_color(palette.primary.strong.color),
    );
    // 末点 + 当前值标签（按符号着色）
    let (lt, lv) = *vis.last().unwrap();
    let lc = if lv >= 0.0 {
        crate::ui::chart::up_strong()
    } else {
        crate::ui::chart::down_strong()
    };
    frame.fill(
        &Path::circle(Point::new(interval_to_x(lt), y_of(lv)), 2.5 / scaling),
        lc,
    );
    frame.fill_text(canvas::Text {
        content: format!("combo {lv:+.3}"),
        position: Point::new(region.x + 4.0 / scaling, top - 2.0 / scaling),
        size: iced::Pixels(11.0 / scaling),
        color: lc,
        font: style::AZERET_MONO,
        ..canvas::Text::default()
    });
}



fn render_data_source<F>(
    data_source: &PlotData<KlineDataPoint>,
    frame: &mut canvas::Frame,
    earliest: u64,
    latest: u64,
    interval_to_x: impl Fn(u64) -> f32,
    draw_fn: F,
) where
    F: Fn(&mut canvas::Frame, f32, &Kline, &KlineTrades),
{
    match data_source {
        PlotData::TickBased(tick_aggr) => {
            let earliest = earliest as usize;
            let latest = latest as usize;

            tick_aggr
                .datapoints
                .iter()
                .rev()
                .enumerate()
                .filter(|(index, _)| *index <= latest && *index >= earliest)
                .for_each(|(index, tick_aggr)| {
                    let x_position = interval_to_x(index as u64);

                    draw_fn(frame, x_position, &tick_aggr.kline, &tick_aggr.footprint);
                });
        }
        PlotData::TimeBased(timeseries) => {
            if latest < earliest {
                return;
            }

            timeseries
                .datapoints
                .range(UnixMs::new(earliest)..=UnixMs::new(latest))
                .for_each(|(timestamp, dp)| {
                    let x_position = interval_to_x(timestamp.as_u64());

                    draw_fn(frame, x_position, &dp.kline, &dp.footprint);
                });
        }
    }
}

fn draw_all_npocs(
    data_source: &PlotData<KlineDataPoint>,
    frame: &mut canvas::Frame,
    price_to_y: impl Fn(Price) -> f32,
    interval_to_x: impl Fn(u64) -> f32,
    candle_width: f32,
    cell_width: f32,
    cell_height: f32,
    palette: &Extended,
    studies: &[FootprintStudy],
    visible_earliest: u64,
    visible_latest: u64,
    cluster_kind: ClusterKind,
    spacing: ContentGaps,
    imb_study_on: bool,
) {
    let Some(lookback) = studies.iter().find_map(|study| {
        if let FootprintStudy::NPoC { lookback } = study {
            Some(*lookback)
        } else {
            None
        }
    }) else {
        return;
    };

    let (filled_color, naked_color) = (
        palette.background.strong.color,
        if palette.is_dark {
            palette.warning.weak.color.scale_alpha(0.5)
        } else {
            palette.warning.strong.color
        },
    );

    let line_height = cell_height.min(1.0);

    let bar_width_factor: f32 = 0.9;
    let inset = (cell_width * (1.0 - bar_width_factor)) / 2.0;

    let candle_lane_factor: f32 = match cluster_kind {
        ClusterKind::VolumeProfile | ClusterKind::DeltaProfile => 0.25,
        ClusterKind::BidAsk => 1.0,
    };

    let start_x_for = |cell_center_x: f32| -> f32 {
        match cluster_kind {
            ClusterKind::BidAsk => cell_center_x + (candle_width / 2.0) + spacing.candle_to_cluster,
            ClusterKind::VolumeProfile | ClusterKind::DeltaProfile => {
                let content_left = (cell_center_x - (cell_width / 2.0)) + inset;
                let candle_lane_left = content_left
                    + if imb_study_on {
                        candle_width + spacing.marker_to_candle
                    } else {
                        0.0
                    };
                candle_lane_left + candle_width * candle_lane_factor + spacing.candle_to_cluster
            }
        }
    };

    let wick_x_for = |cell_center_x: f32| -> f32 {
        match cluster_kind {
            ClusterKind::BidAsk => cell_center_x, // not used for BidAsk clustering
            ClusterKind::VolumeProfile | ClusterKind::DeltaProfile => {
                let content_left = (cell_center_x - (cell_width / 2.0)) + inset;
                let candle_lane_left = content_left
                    + if imb_study_on {
                        candle_width + spacing.marker_to_candle
                    } else {
                        0.0
                    };
                candle_lane_left + (candle_width * candle_lane_factor) / 2.0
                    - (spacing.candle_to_cluster * 0.5)
            }
        }
    };

    let end_x_for = |cell_center_x: f32| -> f32 {
        match cluster_kind {
            ClusterKind::BidAsk => cell_center_x - (candle_width / 2.0) - spacing.candle_to_cluster,
            ClusterKind::VolumeProfile | ClusterKind::DeltaProfile => wick_x_for(cell_center_x),
        }
    };

    let rightmost_cell_center_x = {
        let earliest_x = interval_to_x(visible_earliest);
        let latest_x = interval_to_x(visible_latest);
        if earliest_x > latest_x {
            earliest_x
        } else {
            latest_x
        }
    };

    let mut draw_the_line = |interval: u64, poc: &PointOfControl| {
        let start_x = start_x_for(interval_to_x(interval));

        let (line_width, color) = match poc.status {
            NPoc::Naked => {
                let end_x = end_x_for(rightmost_cell_center_x);
                let line_width = end_x - start_x;
                if line_width.abs() <= cell_width {
                    return;
                }
                (line_width, naked_color)
            }
            NPoc::Filled { at } => {
                let end_x = end_x_for(interval_to_x(at));
                let line_width = end_x - start_x;
                if line_width.abs() <= cell_width {
                    return;
                }
                (line_width, filled_color)
            }
            _ => return,
        };

        frame.fill_rectangle(
            Point::new(start_x, price_to_y(poc.price) - line_height / 2.0),
            Size::new(line_width, line_height),
            color,
        );
    };

    match data_source {
        PlotData::TickBased(tick_aggr) => {
            tick_aggr
                .datapoints
                .iter()
                .rev()
                .enumerate()
                .take(lookback)
                .filter_map(|(index, dp)| dp.footprint.poc.as_ref().map(|poc| (index as u64, poc)))
                .for_each(|(interval, poc)| draw_the_line(interval, poc));
        }
        PlotData::TimeBased(timeseries) => {
            timeseries
                .datapoints
                .iter()
                .rev()
                .take(lookback)
                .filter_map(|(timestamp, dp)| {
                    dp.footprint
                        .poc
                        .as_ref()
                        .map(|poc| (timestamp.as_u64(), poc))
                })
                .for_each(|(interval, poc)| draw_the_line(interval, poc));
        }
    }
}

fn effective_cluster_qty(
    scaling: ClusterScaling,
    visible_max: f32,
    footprint: &KlineTrades,
    cluster_kind: ClusterKind,
) -> f32 {
    let individual_max = match cluster_kind {
        ClusterKind::BidAsk => footprint
            .trades
            .values()
            .map(|group| group.buy_qty.max(group.sell_qty))
            .max()
            .unwrap_or_default(),
        ClusterKind::DeltaProfile => footprint
            .trades
            .values()
            .map(|group| group.buy_qty.abs_diff(group.sell_qty))
            .max()
            .unwrap_or_default(),
        ClusterKind::VolumeProfile => footprint
            .trades
            .values()
            .map(|group| group.buy_qty + group.sell_qty)
            .max()
            .unwrap_or_default(),
    };
    let individual_max_f32 = f32::from(individual_max);

    match scaling {
        ClusterScaling::VisibleRange => Qty::scale_or_one(visible_max),
        ClusterScaling::Datapoint => individual_max.to_scale_or_one(),
        ClusterScaling::Hybrid { weight } => {
            let w = weight.clamp(0.0, 1.0);
            Qty::scale_or_one(visible_max * w + individual_max_f32 * (1.0 - w))
        }
    }
}

fn draw_clusters(
    frame: &mut canvas::Frame,
    price_to_y: impl Fn(Price) -> f32,
    x_position: f32,
    cell_width: f32,
    cell_height: f32,
    candle_width: f32,
    max_cluster_qty: f32,
    palette: &Extended,
    text_size: f32,
    step: PriceStep,
    show_text: bool,
    imbalance: Option<(usize, Option<usize>, bool)>,
    kline: &Kline,
    footprint: &KlineTrades,
    cluster_kind: ClusterKind,
    spacing: ContentGaps,
) {
    let text_color = palette.background.weakest.text;

    let bar_width_factor: f32 = 0.9;
    let inset = (cell_width * (1.0 - bar_width_factor)) / 2.0;

    let cell_left = x_position - (cell_width / 2.0);
    let content_left = cell_left + inset;
    let content_right = x_position + (cell_width / 2.0) - inset;

    match cluster_kind {
        ClusterKind::VolumeProfile | ClusterKind::DeltaProfile => {
            let area = ProfileArea::new(
                content_left,
                content_right,
                candle_width,
                spacing,
                imbalance.is_some(),
            );
            let bar_alpha = if show_text { 0.25 } else { 1.0 };

            for (price, group) in &footprint.trades {
                let buy_qty = f32::from(group.buy_qty);
                let sell_qty = f32::from(group.sell_qty);
                let y = price_to_y(*price);

                match cluster_kind {
                    ClusterKind::VolumeProfile => {
                        super::draw_volume_bar(
                            frame,
                            area.bars_left,
                            y,
                            buy_qty,
                            sell_qty,
                            max_cluster_qty,
                            area.bars_width,
                            cell_height,
                            crate::ui::chart::up(),
                            crate::ui::chart::down(),
                            bar_alpha,
                            true,
                        );

                        if show_text {
                            draw_cluster_text(
                                frame,
                                &abbr_large_numbers(f32::from(group.total_qty())),
                                Point::new(area.bars_left, y),
                                text_size,
                                text_color,
                                Alignment::Start,
                                Alignment::Center,
                            );
                        }
                    }
                    ClusterKind::DeltaProfile => {
                        let delta = f32::from(group.delta_qty());
                        if show_text {
                            draw_cluster_text(
                                frame,
                                &abbr_large_numbers(delta),
                                Point::new(area.bars_left, y),
                                text_size,
                                text_color,
                                Alignment::Start,
                                Alignment::Center,
                            );
                        }

                        let bar_width = (delta.abs() / max_cluster_qty) * area.bars_width;
                        if bar_width > 0.0 {
                            let color = if delta >= 0.0 {
                                crate::ui::chart::up().scale_alpha(bar_alpha)
                            } else {
                                crate::ui::chart::down().scale_alpha(bar_alpha)
                            };
                            frame.fill_rectangle(
                                Point::new(area.bars_left, y - (cell_height / 2.0)),
                                Size::new(bar_width, cell_height),
                                color,
                            );
                        }
                    }
                    _ => {}
                }

                if let Some((threshold, color_scale, ignore_zeros)) = imbalance {
                    let higher_price =
                        Price::from_f32(price.to_f32() + step.to_f32_lossy()).round_to_step(step);

                    let rect_w = ((area.imb_marker_width - 1.0) / 2.0).max(1.0);
                    let buyside_x = area.imb_marker_left + area.imb_marker_width - rect_w;
                    let sellside_x =
                        area.imb_marker_left + area.imb_marker_width - (2.0 * rect_w) - 1.0;

                    draw_imbalance_markers(
                        frame,
                        &price_to_y,
                        footprint,
                        *price,
                        sell_qty,
                        higher_price,
                        threshold,
                        color_scale,
                        ignore_zeros,
                        cell_height,
                        palette,
                        buyside_x,
                        sellside_x,
                        rect_w,
                    );
                }
            }

            draw_footprint_kline(
                frame,
                &price_to_y,
                area.candle_center_x,
                candle_width,
                kline,
                palette,
            );
        }
        ClusterKind::BidAsk => {
            let area = BidAskArea::new(
                x_position,
                content_left,
                content_right,
                candle_width,
                spacing,
            );

            let bar_alpha = if show_text { 0.25 } else { 1.0 };

            let imb_marker_reserve = if imbalance.is_some() {
                ((area.imb_marker_width - 1.0) / 2.0).max(1.0)
            } else {
                0.0
            };

            let right_max_x =
                area.bid_area_right - imb_marker_reserve - (2.0 * spacing.marker_to_bars);
            let right_area_width = (right_max_x - area.bid_area_left).max(0.0);

            let left_min_x =
                area.ask_area_left + imb_marker_reserve + (2.0 * spacing.marker_to_bars);
            let left_area_width = (area.ask_area_right - left_min_x).max(0.0);

            for (price, group) in &footprint.trades {
                let buy_qty = f32::from(group.buy_qty);
                let sell_qty = f32::from(group.sell_qty);
                let y = price_to_y(*price);

                if buy_qty > 0.0 && right_area_width > 0.0 {
                    if show_text {
                        draw_cluster_text(
                            frame,
                            &abbr_large_numbers(buy_qty),
                            Point::new(area.bid_area_left, y),
                            text_size,
                            text_color,
                            Alignment::Start,
                            Alignment::Center,
                        );
                    }

                    let bar_width = (buy_qty / max_cluster_qty) * right_area_width;
                    if bar_width > 0.0 {
                        frame.fill_rectangle(
                            Point::new(area.bid_area_left, y - (cell_height / 2.0)),
                            Size::new(bar_width, cell_height),
                            crate::ui::chart::up().scale_alpha(bar_alpha),
                        );
                    }
                }
                if sell_qty > 0.0 && left_area_width > 0.0 {
                    if show_text {
                        draw_cluster_text(
                            frame,
                            &abbr_large_numbers(sell_qty),
                            Point::new(area.ask_area_right, y),
                            text_size,
                            text_color,
                            Alignment::End,
                            Alignment::Center,
                        );
                    }

                    let bar_width = (sell_qty / max_cluster_qty) * left_area_width;
                    if bar_width > 0.0 {
                        frame.fill_rectangle(
                            Point::new(area.ask_area_right, y - (cell_height / 2.0)),
                            Size::new(-bar_width, cell_height),
                            crate::ui::chart::down().scale_alpha(bar_alpha),
                        );
                    }
                }

                if let Some((threshold, color_scale, ignore_zeros)) = imbalance
                    && area.imb_marker_width > 0.0
                {
                    let higher_price =
                        Price::from_f32(price.to_f32() + step.to_f32_lossy()).round_to_step(step);

                    let rect_width = ((area.imb_marker_width - 1.0) / 2.0).max(1.0);

                    let buyside_x = area.bid_area_right - rect_width - spacing.marker_to_bars;
                    let sellside_x = area.ask_area_left + spacing.marker_to_bars;

                    draw_imbalance_markers(
                        frame,
                        &price_to_y,
                        footprint,
                        *price,
                        sell_qty,
                        higher_price,
                        threshold,
                        color_scale,
                        ignore_zeros,
                        cell_height,
                        palette,
                        buyside_x,
                        sellside_x,
                        rect_width,
                    );
                }
            }

            draw_footprint_kline(
                frame,
                &price_to_y,
                area.candle_center_x,
                candle_width,
                kline,
                palette,
            );
        }
    }

    if show_text {
        let mut total_buy = Qty::zero();
        let mut total_sell = Qty::zero();
        let mut total_delta = Qty::zero();

        for group in footprint.trades.values() {
            total_buy += group.buy_qty;
            total_sell += group.sell_qty;
            total_delta += group.delta_qty();
        }

        let summary_y = price_to_y(kline.low) + cell_height * 1.5;
        let line_spacing = text_size * 1.2;

        let total_vol = total_buy + total_sell;

        draw_cluster_text(
            frame,
            &format!("V: {}", abbr_large_numbers(total_vol.to_f32_lossy())),
            Point::new(x_position, summary_y),
            text_size * 0.9,
            palette.background.weakest.text,
            Alignment::Center,
            Alignment::Start,
        );

        let delta_color = if total_delta >= Qty::zero() {
            crate::ui::chart::up()
        } else {
            crate::ui::chart::down()
        };

        draw_cluster_text(
            frame,
            &format!("Δ: {}", abbr_large_numbers(total_delta.to_f32_lossy())),
            Point::new(x_position, summary_y + line_spacing),
            text_size * 0.9,
            delta_color,
            Alignment::Center,
            Alignment::Start,
        );
    }
}

fn draw_imbalance_markers(
    frame: &mut canvas::Frame,
    price_to_y: &impl Fn(Price) -> f32,
    footprint: &KlineTrades,
    price: Price,
    sell_qty: f32,
    higher_price: Price,
    threshold: usize,
    color_scale: Option<usize>,
    ignore_zeros: bool,
    cell_height: f32,
    _palette: &Extended,
    buyside_x: f32,
    sellside_x: f32,
    rect_width: f32,
) {
    if ignore_zeros && sell_qty <= 0.0 {
        return;
    }

    if let Some(group) = footprint.trades.get(&higher_price) {
        let diagonal_buy_qty = f32::from(group.buy_qty);

        if ignore_zeros && diagonal_buy_qty <= 0.0 {
            return;
        }

        let rect_height = cell_height / 2.0;

        let alpha_from_ratio = |ratio: f32| -> f32 {
            if let Some(scale) = color_scale {
                let divisor = (scale as f32 / 10.0) - 1.0;
                (0.2 + 0.8 * ((ratio - 1.0) / divisor).min(1.0)).min(1.0)
            } else {
                1.0
            }
        };

        if diagonal_buy_qty >= sell_qty {
            let required_qty = sell_qty * (100 + threshold) as f32 / 100.0;
            if diagonal_buy_qty > required_qty {
                let ratio = diagonal_buy_qty / required_qty;
                let alpha = alpha_from_ratio(ratio);

                let y = price_to_y(higher_price);
                frame.fill_rectangle(
                    Point::new(buyside_x, y - (rect_height / 2.0)),
                    Size::new(rect_width, rect_height),
                    crate::ui::chart::up_weak().scale_alpha(alpha),
                );
            }
        } else {
            let required_qty = diagonal_buy_qty * (100 + threshold) as f32 / 100.0;
            if sell_qty > required_qty {
                let ratio = sell_qty / required_qty;
                let alpha = alpha_from_ratio(ratio);

                let y = price_to_y(price);
                frame.fill_rectangle(
                    Point::new(sellside_x, y - (rect_height / 2.0)),
                    Size::new(rect_width, rect_height),
                    crate::ui::chart::down_weak().scale_alpha(alpha),
                );
            }
        }
    }
}

impl ContentGaps {
    fn from_view(candle_width: f32, scaling: f32) -> Self {
        let px = |p: f32| p / scaling;
        let base = (candle_width * 0.2).max(px(2.0));
        Self {
            marker_to_candle: base,
            candle_to_cluster: base,
            marker_to_bars: px(2.0),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ContentGaps {
    /// Space between imb. markers candle body
    marker_to_candle: f32,
    /// Space between candle body and clusters
    candle_to_cluster: f32,
    /// Inner space reserved between imb. markers and clusters (used for BidAsk)
    marker_to_bars: f32,
}

fn draw_cluster_text(
    frame: &mut canvas::Frame,
    text: &str,
    position: Point,
    text_size: f32,
    color: iced::Color,
    align_x: Alignment,
    align_y: Alignment,
) {
    frame.fill_text(canvas::Text {
        content: text.to_string(),
        position,
        size: iced::Pixels(text_size),
        color,
        align_x: align_x.into(),
        align_y: align_y.into(),
        font: style::AZERET_MONO,
        ..canvas::Text::default()
    });
}

/// 时间轴 K 线的缺口：相邻两根相隔超过 1.5 个周期，就在中间画一道淡色带并标「缺口」。
/// 数据空洞（采集中断、交易所断流）原先在图上完全看不出来——两根蜡烛照样挨着画。
fn draw_gaps(
    frame: &mut canvas::Frame,
    ts: &TimeSeries<KlineDataPoint>,
    interval_to_x: &impl Fn(u64) -> f32,
    region: Rectangle,
    scaling: f32,
    earliest: u64,
    latest: u64,
) {
    let iv = ts.interval.to_milliseconds();
    if iv == 0 {
        return;
    }
    let lo = earliest.saturating_sub(iv);
    let mut prev: Option<u64> = None;
    for (t, _) in ts.datapoints.range(UnixMs::new(lo)..=UnixMs::new(latest.saturating_add(iv))) {
        let t = t.as_u64();
        if let Some(p) = prev
            && t > p + iv + iv / 2
        {
            let cell = interval_to_x(p + iv) - interval_to_x(p);
            let x0 = interval_to_x(p + iv) - cell / 2.0;
            let x1 = interval_to_x(t) - cell / 2.0;
            let (l, r) = (x0.min(x1), x0.max(x1));
            frame.fill_rectangle(
                Point::new(l, region.y),
                Size::new(r - l, region.height),
                crate::ui::pal::alpha(crate::ui::pal::dim(), 0.10),
            );
            frame.fill_text(canvas::Text {
                content: format!("缺口 {} 根", (t - p) / iv - 1),
                position: Point::new(l + 2.0 / scaling, region.y + 4.0 / scaling),
                size: iced::Pixels(10.0 / scaling),
                color: crate::ui::pal::dim(),
                ..canvas::Text::default()
            });
        }
        prev = Some(t);
    }
}

fn draw_crosshair_tooltip(
    data: &PlotData<KlineDataPoint>,
    ticker_info: &TickerInfo,
    frame: &mut canvas::Frame,
    palette: &Extended,
    basis: Basis,
    at_interval: Option<u64>,
    visible_range: (u64, u64),
) {
    let (visible_earliest, visible_latest) = visible_range;

    // 十字线落在缺口上（前后都有 K 线、这一格没有）：明说是缺口，不借前一根的数（docs/35 §6.3 不插值）
    if let (PlotData::TimeBased(ts), Some(at)) = (data, at_interval)
        && let (Some((first, _)), Some((last, _))) = (ts.datapoints.first_key_value(), ts.datapoints.last_key_value())
        && at > first.as_u64()
        && at < last.as_u64()
        && !ts.datapoints.contains_key(&UnixMs::new(at))
    {
        let label = "缺口 · 这一根没有数据（不插值）";
        frame.fill_rectangle(
            Point::new(8.0, 8.0),
            Size::new(label.chars().count() as f32 * TEXT_SIZE, 16.0),
            palette.background.weakest.color.scale_alpha(0.9),
        );
        frame.fill_text(canvas::Text {
            content: label.to_string(),
            position: Point::new(8.0, 8.0),
            size: iced::Pixels(crate::style::text_size::BODY),
            color: crate::ui::pal::warn(),
            ..canvas::Text::default()
        });
        return;
    }

    let kline_opt = match (data, at_interval) {
        (PlotData::TimeBased(timeseries), Some(at_interval)) => {
            let in_visible = at_interval >= visible_earliest && at_interval <= visible_latest;

            timeseries
                .datapoints
                .get(&UnixMs::new(at_interval))
                .map(|dp| &dp.kline)
                .or_else(|| {
                    if in_visible {
                        let search_end = at_interval.min(visible_latest);
                        timeseries
                            .datapoints
                            .range(UnixMs::new(visible_earliest)..=UnixMs::new(search_end))
                            .next_back()
                            .map(|(_, dp)| &dp.kline)
                    } else {
                        None
                    }
                })
                .or_else(|| {
                    let right_of_latest = match basis {
                        Basis::Time(_) => at_interval > visible_latest,
                        Basis::Tick(_) => at_interval < visible_earliest,
                    };

                    if right_of_latest {
                        timeseries
                            .datapoints
                            .range(UnixMs::new(visible_earliest)..=UnixMs::new(visible_latest))
                            .next_back()
                            .map(|(_, dp)| &dp.kline)
                    } else {
                        None
                    }
                })
                .or_else(|| {
                    let (last_time, dp) = timeseries.datapoints.last_key_value()?;
                    (at_interval > last_time.as_u64()).then_some(&dp.kline)
                })
        }
        (PlotData::TickBased(tick_aggr), Some(at_interval)) => {
            let kline_at = |interval: u64| {
                let index = (interval / u64::from(tick_aggr.interval.0)) as usize;
                (index < tick_aggr.datapoints.len())
                    .then(|| &tick_aggr.datapoints[tick_aggr.datapoints.len() - 1 - index].kline)
            };

            let in_visible = at_interval >= visible_earliest && at_interval <= visible_latest;

            kline_at(at_interval).or_else(|| {
                let right_of_latest = match basis {
                    Basis::Time(_) => at_interval > visible_latest,
                    Basis::Tick(_) => at_interval < visible_earliest,
                };

                if in_visible || right_of_latest {
                    kline_at(visible_earliest)
                } else {
                    None
                }
            })
        }
        (PlotData::TimeBased(timeseries), None) => timeseries
            .datapoints
            .last_key_value()
            .map(|(_, dp)| &dp.kline),
        (PlotData::TickBased(tick_aggr), None) => tick_aggr.datapoints.last().map(|dp| &dp.kline),
    };

    // 估宽：中文按整字宽，ASCII 按约六成
    let est = |s: &str| s.chars().map(|c| if c.is_ascii() { TEXT_SIZE * 0.62 } else { TEXT_SIZE }).sum::<f32>();
    const GAP: f32 = 14.0;
    // 市场状态（docs/62 R2）：OHLC 下面每层一行（L2 价格状态、L1 资金费 + 持仓），再下一行是底色维度这一段的切换原因
    let mut regime = match (data, at_interval) {
        (PlotData::TimeBased(ts), Some(at)) => {
            let symbol = crate::ws::drawing::symbol_key(&ticker_info.ticker);
            let iv = ts.interval.to_milliseconds();
            crate::ws::regime::describe_at(&symbol, iv, at).map(|(tf, layers)| {
                // 每层一行（第一行写周期，其余用文件给的行名）；一行放不下就折到下一行，折出来的行不再写行名
                let max_w = (frame.width() - 90.0).max(240.0);
                let dim_c = palette.background.base.text.scale_alpha(0.6);
                let mut lines: Vec<Vec<(String, iced::Color)>> = Vec::new();
                for (k, (name, items)) in layers.into_iter().enumerate() {
                    let head = if k == 0 { format!("状态 {tf}") } else { name };
                    let mut cur = vec![(head, dim_c)];
                    let mut w = est(&cur[0].0) + GAP;
                    for (dim, st, c) in items {
                        let s = format!("{dim} {st}");
                        let sw = est(&s) + GAP;
                        if w + sw > max_w && cur.len() > 1 {
                            lines.push(std::mem::take(&mut cur));
                            cur.push(("　".to_string(), dim_c));
                            w = est("　") + GAP;
                        }
                        w += sw;
                        cur.push((s, c.unwrap_or(palette.background.base.text)));
                    }
                    lines.push(cur);
                }
                (lines, crate::ws::regime::reason_at(&symbol, iv, at))
            })
        }
        _ => None,
    };
    // 图上指标（docs/63 I3）：再加一行「指标」（放不下折行），在状态各行之后、切换原因之前
    if let (PlotData::TimeBased(ts), Some(at)) = (data, at_interval) {
        let symbol = crate::ws::drawing::symbol_key(&ticker_info.ticker);
        let items = crate::ws::studies::describe_at(&symbol, ts.interval.to_milliseconds(), at);
        if !items.is_empty() {
            let max_w = (frame.width() - 90.0).max(240.0);
            let dim_c = palette.background.base.text.scale_alpha(0.6);
            let entry = regime.get_or_insert_with(|| (Vec::new(), None));
            let mut cur = vec![("指标".to_string(), dim_c)];
            let mut w = est("指标") + GAP;
            for (s, c) in items {
                let sw = est(&s) + GAP;
                if w + sw > max_w && cur.len() > 1 {
                    entry.0.push(std::mem::take(&mut cur));
                    cur.push(("　".to_string(), dim_c));
                    w = est("　") + GAP;
                }
                w += sw;
                cur.push((s, c));
            }
            entry.0.push(cur);
        }
    }
    const ROW_H: f32 = 18.0;
    // 图顶第一行留给日历事件标签（docs/43 K5b），读数从它下面开始
    const TOP: f32 = 18.0;

    if let Some(kline) = kline_opt {
        let change_pct = ((kline.close - kline.open).to_f32() / kline.open.to_f32()) * 100.0;
        let change_color = if change_pct >= 0.0 {
            crate::ui::chart::up()
        } else {
            crate::ui::chart::down()
        };

        let base_color = palette.background.base.text;
        let precision = ticker_info.min_ticksize;

        let segments = [
            ("O", base_color, false),
            (&kline.open.to_string(precision), change_color, true),
            ("H", base_color, false),
            (&kline.high.to_string(precision), change_color, true),
            ("L", base_color, false),
            (&kline.low.to_string(precision), change_color, true),
            ("C", base_color, false),
            (&kline.close.to_string(precision), change_color, true),
            (&format!("{change_pct:+.2}%"), change_color, true),
        ];

        let ohlc_width: f32 = segments
            .iter()
            .map(|(s, _, v)| s.len() as f32 * 8.0 + if *v { 6.0 } else { 2.0 })
            .sum();

        // OHLC 和状态行共用一块连续底板，行间不留缝，免得图上的切换标记文字从缝里透出来
        let mut width = ohlc_width;
        let mut rows = 1.0;
        if let Some((lines, why)) = &regime {
            for segs in lines {
                width = width.max(segs.iter().map(|(s, _)| est(s) + GAP).sum());
                rows += 1.0;
            }
            if let Some(w) = why {
                width = width.max(est(w));
                rows += 1.0;
            }
        }
        frame.fill_rectangle(
            Point::new(4.0, TOP - 3.0),
            Size::new(width + 8.0, rows * ROW_H + 4.0),
            palette.background.weakest.color.scale_alpha(0.96),
        );

        let position = Point::new(8.0, TOP);
        let mut x = position.x;
        for (text, seg_color, is_value) in segments {
            frame.fill_text(canvas::Text {
                content: text.to_string(),
                position: Point::new(x, position.y),
                size: iced::Pixels(crate::style::text_size::BODY),
                color: seg_color,
                font: style::AZERET_MONO,
                ..canvas::Text::default()
            });
            x += text.len() as f32 * 8.0;
            x += if is_value { 6.0 } else { 2.0 };
        }

        if let Some((lines, why)) = regime {
            let mut y = TOP;
            for segs in lines {
                y += ROW_H;
                let mut x = 8.0;
                for (s, c) in segs {
                    let w = est(&s) + GAP;
                    frame.fill_text(canvas::Text {
                        content: s,
                        position: Point::new(x, y),
                        size: iced::Pixels(crate::style::text_size::BODY),
                        color: c,
                        font: crate::ui::text::ui_font(),
                        ..canvas::Text::default()
                    });
                    x += w;
                }
            }
            if let Some(why) = why {
                frame.fill_text(canvas::Text {
                    content: why,
                    position: Point::new(8.0, y + ROW_H),
                    size: iced::Pixels(crate::style::text_size::BODY),
                    color: palette.background.base.text.scale_alpha(0.7),
                    font: crate::ui::text::ui_font(),
                    ..canvas::Text::default()
                });
            }
        }
    }
}

struct ProfileArea {
    imb_marker_left: f32,
    imb_marker_width: f32,
    bars_left: f32,
    bars_width: f32,
    candle_center_x: f32,
}

impl ProfileArea {
    fn new(
        content_left: f32,
        content_right: f32,
        candle_width: f32,
        gaps: ContentGaps,
        has_imbalance: bool,
    ) -> Self {
        let candle_lane_left = if has_imbalance {
            content_left + candle_width + gaps.marker_to_candle
        } else {
            content_left
        };
        let candle_lane_width = candle_width * 0.25;

        let bars_left = candle_lane_left + candle_lane_width + gaps.candle_to_cluster;
        let bars_width = (content_right - bars_left).max(0.0);

        let candle_center_x = candle_lane_left + (candle_lane_width / 2.0);

        Self {
            imb_marker_left: content_left,
            imb_marker_width: if has_imbalance { candle_width } else { 0.0 },
            bars_left,
            bars_width,
            candle_center_x,
        }
    }
}

struct BidAskArea {
    bid_area_left: f32,
    bid_area_right: f32,
    ask_area_left: f32,
    ask_area_right: f32,
    candle_center_x: f32,
    imb_marker_width: f32,
}

impl BidAskArea {
    fn new(
        x_position: f32,
        content_left: f32,
        content_right: f32,
        candle_width: f32,
        spacing: ContentGaps,
    ) -> Self {
        let candle_body_width = candle_width * 0.25;

        let candle_left = x_position - (candle_body_width / 2.0);
        let candle_right = x_position + (candle_body_width / 2.0);

        let ask_area_right = candle_left - spacing.candle_to_cluster;
        let bid_area_left = candle_right + spacing.candle_to_cluster;

        Self {
            bid_area_left,
            bid_area_right: content_right,
            ask_area_left: content_left,
            ask_area_right,
            candle_center_x: x_position,
            imb_marker_width: candle_width,
        }
    }
}

#[inline]
fn footprint_cluster_min_width(cluster_kind: ClusterKind) -> f32 {
    match cluster_kind {
        ClusterKind::VolumeProfile | ClusterKind::DeltaProfile => 80.0,
        ClusterKind::BidAsk => 120.0,
    }
}

#[inline]
fn footprint_cluster_text_size(cell_height_unscaled: f32, cell_width_unscaled: f32) -> f32 {
    let text_size_from_height = cell_height_unscaled.round().min(16.0) - 3.0;
    let text_size_from_width = (cell_width_unscaled * 0.1).round().min(16.0) - 3.0;

    text_size_from_height.min(text_size_from_width)
}

#[inline]
fn price_padding_from_pixels(cell_height: f32, tick_size: f32) -> f32 {
    const OUTER_BOUND_PADDING_PX: f32 = 4.0;

    if cell_height <= f32::EPSILON {
        return 0.0;
    }

    (OUTER_BOUND_PADDING_PX / cell_height) * tick_size
}

fn footprint_summary_padding(
    cell_height: f32,
    scaling: f32,
    cell_width: f32,
    tick_size: f32,
    cluster_kind: ClusterKind,
) -> f32 {
    if cell_height <= f32::EPSILON {
        return 0.0;
    }

    let cell_height_unscaled = cell_height * scaling;
    let cell_width_unscaled = cell_width * scaling;

    if !should_show_text(
        cell_height_unscaled,
        cell_width_unscaled,
        footprint_cluster_min_width(cluster_kind),
    ) {
        return 0.0;
    }

    let text_size = footprint_cluster_text_size(cell_height_unscaled, cell_width_unscaled);
    let line_spacing = text_size * 1.2;

    let summary_text_height_px = text_size * 0.9;
    let summary_y_start_px = cell_height * 1.5;

    let second_line_y_start_px = summary_y_start_px + line_spacing;
    let summary_y_end_px = second_line_y_start_px + summary_text_height_px;

    let extra_bottom_padding_px = summary_text_height_px;
    let summary_y_end_with_padding_px = summary_y_end_px + extra_bottom_padding_px;
    let summary_ticks = summary_y_end_with_padding_px / cell_height;

    summary_ticks * tick_size
}

#[inline]
fn should_show_text(cell_height_unscaled: f32, cell_width_unscaled: f32, min_w: f32) -> bool {
    cell_height_unscaled > 8.0 && cell_width_unscaled > min_w
}
