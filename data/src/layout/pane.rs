use exchange::{TickMultiplier, TickerInfo, Timeframe};
use serde::{Deserialize, Serialize};

use crate::chart::{comparison, heatmap, kline};
use crate::panel::{ladder, timeandsales};
use crate::stream::PersistStreamKind;
use crate::util::ok_or_default;

use crate::chart::{
    Basis, ViewConfig,
    heatmap::HeatmapStudy,
    indicator::{HeatmapIndicator, KlineIndicator},
    kline::KlineChartKind,
};

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum Pane {
    Split {
        axis: Axis,
        ratio: f32,
        a: Box<Pane>,
        b: Box<Pane>,
    },
    Starter {
        #[serde(deserialize_with = "ok_or_default", default)]
        link_group: Option<LinkGroup>,
    },
    HeatmapChart {
        layout: ViewConfig,
        #[serde(deserialize_with = "ok_or_default", default)]
        studies: Vec<HeatmapStudy>,
        #[serde(deserialize_with = "ok_or_default", default)]
        stream_type: Vec<PersistStreamKind>,
        #[serde(deserialize_with = "ok_or_default")]
        settings: Settings,
        #[serde(deserialize_with = "ok_or_default", default)]
        indicators: Vec<HeatmapIndicator>,
        #[serde(deserialize_with = "ok_or_default", default)]
        link_group: Option<LinkGroup>,
    },
    ShaderHeatmap {
        #[serde(deserialize_with = "ok_or_default", default)]
        studies: Vec<HeatmapStudy>,
        #[serde(deserialize_with = "ok_or_default", default)]
        stream_type: Vec<PersistStreamKind>,
        #[serde(deserialize_with = "ok_or_default")]
        settings: Settings,
        #[serde(deserialize_with = "ok_or_default", default)]
        indicators: Vec<HeatmapIndicator>,
        #[serde(deserialize_with = "ok_or_default", default)]
        link_group: Option<LinkGroup>,
    },
    KlineChart {
        layout: ViewConfig,
        kind: KlineChartKind,
        #[serde(deserialize_with = "ok_or_default", default)]
        stream_type: Vec<PersistStreamKind>,
        #[serde(deserialize_with = "ok_or_default")]
        settings: Settings,
        #[serde(deserialize_with = "ok_or_default", default)]
        indicators: Vec<KlineIndicator>,
        #[serde(deserialize_with = "ok_or_default", default)]
        link_group: Option<LinkGroup>,
    },
    ComparisonChart {
        stream_type: Vec<PersistStreamKind>,
        #[serde(deserialize_with = "ok_or_default")]
        settings: Settings,
        #[serde(deserialize_with = "ok_or_default", default)]
        link_group: Option<LinkGroup>,
    },
    TimeAndSales {
        stream_type: Vec<PersistStreamKind>,
        settings: Settings,
        #[serde(deserialize_with = "ok_or_default", default)]
        link_group: Option<LinkGroup>,
    },
    Ladder {
        stream_type: Vec<PersistStreamKind>,
        settings: Settings,
        #[serde(deserialize_with = "ok_or_default", default)]
        link_group: Option<LinkGroup>,
    },
    WealthSpring {
        #[serde(deserialize_with = "ok_or_default", default)]
        mode: WsPaneMode,
        #[serde(deserialize_with = "ok_or_default", default)]
        settings: Settings,
        #[serde(deserialize_with = "ok_or_default", default)]
        link_group: Option<LinkGroup>,
    },
    Factory {
        #[serde(deserialize_with = "ok_or_default", default)]
        settings: Settings,
        #[serde(deserialize_with = "ok_or_default", default)]
        link_group: Option<LinkGroup>,
    },
    PredictionBoard {
        #[serde(deserialize_with = "ok_or_default", default)]
        settings: Settings,
        #[serde(deserialize_with = "ok_or_default", default)]
        link_group: Option<LinkGroup>,
    },
    PmBinance {
        #[serde(deserialize_with = "ok_or_default", default)]
        settings: Settings,
        #[serde(deserialize_with = "ok_or_default", default)]
        link_group: Option<LinkGroup>,
    },
    PmReplay {
        #[serde(deserialize_with = "ok_or_default", default)]
        settings: Settings,
        #[serde(deserialize_with = "ok_or_default", default)]
        link_group: Option<LinkGroup>,
    },
    FeatureLab {
        #[serde(deserialize_with = "ok_or_default", default)]
        settings: Settings,
        #[serde(deserialize_with = "ok_or_default", default)]
        link_group: Option<LinkGroup>,
    },
    FeatureMatrix {
        #[serde(deserialize_with = "ok_or_default", default)]
        settings: Settings,
        #[serde(deserialize_with = "ok_or_default", default)]
        link_group: Option<LinkGroup>,
    },
    MarketMap {
        #[serde(deserialize_with = "ok_or_default", default)]
        settings: Settings,
        #[serde(deserialize_with = "ok_or_default", default)]
        link_group: Option<LinkGroup>,
    },
    OptionsBoard {
        #[serde(deserialize_with = "ok_or_default", default)]
        settings: Settings,
        #[serde(deserialize_with = "ok_or_default", default)]
        link_group: Option<LinkGroup>,
    },
    C4Shadow {
        #[serde(deserialize_with = "ok_or_default", default)]
        settings: Settings,
        #[serde(deserialize_with = "ok_or_default", default)]
        link_group: Option<LinkGroup>,
    },
    /// 数据接口观察终端（docs/23）。无行情流、无图表状态，只有窗口设置。
    /// 新闻资讯：交易所/监管/媒体的统一时间线 + 源健康（docs/25）。
    News {
        #[serde(default)]
        settings: Settings,
        #[serde(default)]
        link_group: Option<LinkGroup>,
    },
    /// 网络出口总闸：一页看全谁在往外发包，每一路都能手动启停。
    NetEgress {
        #[serde(default)]
        settings: Settings,
        #[serde(default)]
        link_group: Option<LinkGroup>,
    },
    /// 进程页（docs/26 S4）：WealthSpring 名下常驻单元的状态与启停。
    /// 与「网络出口」页是邻居——一个管进程、一个管出口。
    Procs {
        #[serde(default)]
        settings: Settings,
        #[serde(default)]
        link_group: Option<LinkGroup>,
    },
    Observatory {
        #[serde(deserialize_with = "ok_or_default", default)]
        settings: Settings,
        #[serde(deserialize_with = "ok_or_default", default)]
        link_group: Option<LinkGroup>,
    },
    Recorder {
        #[serde(deserialize_with = "ok_or_default", default)]
        settings: Settings,
        #[serde(deserialize_with = "ok_or_default", default)]
        link_group: Option<LinkGroup>,
    },
    TardisReplay {
        #[serde(deserialize_with = "ok_or_default", default)]
        settings: Settings,
        #[serde(deserialize_with = "ok_or_default", default)]
        link_group: Option<LinkGroup>,
    },
    TardisBoard {
        #[serde(deserialize_with = "ok_or_default", default)]
        settings: Settings,
        #[serde(deserialize_with = "ok_or_default", default)]
        link_group: Option<LinkGroup>,
    },
    BacktestResult {
        #[serde(deserialize_with = "ok_or_default", default)]
        settings: Settings,
        #[serde(deserialize_with = "ok_or_default", default)]
        link_group: Option<LinkGroup>,
    },
    Orders {
        #[serde(deserialize_with = "ok_or_default", default)]
        settings: Settings,
        #[serde(deserialize_with = "ok_or_default", default)]
        link_group: Option<LinkGroup>,
    },
}

impl Default for Pane {
    fn default() -> Self {
        Pane::Starter { link_group: None }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct Settings {
    pub tick_multiply: Option<exchange::TickMultiplier>,
    pub visual_config: Option<VisualConfig>,
    pub selected_basis: Option<Basis>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum LinkGroup {
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
}

impl LinkGroup {
    pub const ALL: [LinkGroup; 9] = [
        LinkGroup::A,
        LinkGroup::B,
        LinkGroup::C,
        LinkGroup::D,
        LinkGroup::E,
        LinkGroup::F,
        LinkGroup::G,
        LinkGroup::H,
        LinkGroup::I,
    ];
}

impl std::fmt::Display for LinkGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let c = match self {
            LinkGroup::A => "1",
            LinkGroup::B => "2",
            LinkGroup::C => "3",
            LinkGroup::D => "4",
            LinkGroup::E => "5",
            LinkGroup::F => "6",
            LinkGroup::G => "7",
            LinkGroup::H => "8",
            LinkGroup::I => "9",
        };
        write!(f, "{c}")
    }
}

/// Defines the specific configuration for different types of pane settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum VisualConfig {
    Heatmap(heatmap::Config),
    TimeAndSales(timeandsales::Config),
    Kline(kline::Config),
    Ladder(ladder::Config),
    Comparison(comparison::Config),
}

impl VisualConfig {
    pub fn heatmap(&self) -> Option<heatmap::Config> {
        match self {
            Self::Heatmap(cfg) => Some(*cfg),
            _ => None,
        }
    }

    pub fn time_and_sales(&self) -> Option<timeandsales::Config> {
        match self {
            Self::TimeAndSales(cfg) => Some(*cfg),
            _ => None,
        }
    }

    pub fn kline(&self) -> Option<kline::Config> {
        match self {
            Self::Kline(cfg) => Some(*cfg),
            _ => None,
        }
    }

    pub fn ladder(&self) -> Option<ladder::Config> {
        match self {
            Self::Ladder(cfg) => Some(*cfg),
            _ => None,
        }
    }

    pub fn comparison(&self) -> Option<comparison::Config> {
        match self {
            Self::Comparison(cfg) => Some(cfg.clone()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContentKind {
    Starter,
    HeatmapChart,
    ShaderHeatmap,
    FootprintChart,
    CandlestickChart,
    ComparisonChart,
    TimeAndSales,
    Ladder,
    /// WealthSpring 读数面板（docs/08）：订单/PnL · 订单流 · 引擎信号 · Factory 现役池。
    /// 无行情数据源——数据走 Redis/shmem 旁路快照，不需要选 ticker。
    WealthSpring,
    /// Alpha Factory 仪表盘（docs/08 F6-P2）：F0–F7 全流程状态，只读轮询 registry.sqlite。
    Factory,
    /// C4 活体影子（docs/14 §2）：maker 影子守护实时/影子日/活体vs重放。只读 checkpoint
    /// + registry.sqlite 旁路，无行情数据源。
    C4Shadow,
    /// 期权/0DTE 回测·探针（docs/18）：逐策略净 PnL + 摩擦分解 + 探针。只读 options_board.json 旁路。
    OptionsBoard,
    /// 预测市场 Polymarket（docs/19）：市场列表 + 基线关注 + AI 决策支持。只读 prediction_board.json 旁路。
    PredictionBoard,
    /// 币安钱包预测市场（BTC 5 分钟涨跌）：两本簿的完整档位 + 轮次 + 失衡 + 录制状态。
    ///
    /// **与 [`ContentKind::PredictionBoard`] 是两个面板，不是一个面板的两个源**：
    /// Polymarket 那边是「市场列表 + 日线级决策支持」，这边是**逐笔盘口**（WS 约 5 条/秒）。
    /// 刷新节奏、信息密度、要占的屏幕面积都不是一回事，挤在一个 pane 里两边都看不清。
    ///
    /// 只读 `~/ws-data/live/pm_binance.json` 旁路——**零交易所流**，WS 连接在
    /// `ws-pm-recorder` 守护里。
    PmBinance,
    /// 预测市场回放：按「日期 → 轮次」回放自录的两本簿（`raw/pm_book`）。
    ///
    /// **与 [`ContentKind::TardisBoard`] 并列而不是并入**：那套的类型词汇
    /// （trades/l2/deriv/强平/BBO）是给交易所行情设计的，预测市场一个都不对应——
    /// 它没有成交流、没有中间价、没有资金费，有的是两本独立的簿 + 一个二元结局。
    PmReplay,
    /// 订单流与微观结构**特征库**（docs/30）：注册表 + 覆盖健康 + FDR 控制的条件检验。
    ///
    /// **它是研究仪器，不是行情图。** 面板上最重要的不是特征值，而是
    /// 「做了多少次检验、朴素命中几个、FDR 之后还剩几个、样本还差多少」。
    FeatureLab,
    /// 订单流特征矩阵（docs/31 §8.1）：七阶段 × 63 条特征 × 窗口的逐 slot 状态。
    /// 只读 feature_matrix.json 旁路快照（由 wealthspring-features 的引擎写出），
    /// **零交易所流、零计算**——面板不重算任何特征值。
    ///
    /// 与 [`ContentKind::FeatureLab`] 分工：这一页回答「引擎此刻算出了什么、
    /// 每个 slot 什么质量」，那一页回答「这些值里有没有一个真的预测得动」
    /// （样本量/多重比较/FDR）。合成一页会让人把「93 个 slot 质量良好」
    /// 读成「93 个有效信号」。
    FeatureMatrix,
    /// 全市场雷达（docs/22 P0）：加密全市场树图 + 涨跌速度/量异常排行。
    /// 只读 radar_board.json 旁路，**零交易所流**（数据来自独立的 ws-radar 守护）。
    MarketMap,
    /// 数据接口观察终端（docs/23 P0）：REST/WS/TCP/FIX 统一到一处观察与录制。
    /// **零交易所流**：全部连接在独立的 `ws-observatory` 守护里，面板只读快照。
    Observatory,
    NetEgress,
    /// 进程页（docs/26 S4）：常驻单元状态与启停。**零交易所流**——只调 systemctl。
    Procs,
    News,
    /// 录制驾驶舱（docs/08 F6-P3）：24/7 守护录制控制中心（服务启停 + 配置 + 实况 + 总览）。
    Recorder,
    /// Tardis 历史回放（docs/20 Phase 5）：已购 30 天逐笔按变速推进 `ws:bt:{run}:trades` 喂图。
    /// 控制面板本身无行情数据源（行情进的是同工作区的图表 pane）。
    TardisReplay,
    /// Tardis 历史面板（docs/20 §9）：数据源(3) → 数据类型(8) → 图表（主图+衍生图）。
    /// **零交易所流**：全部数据来自本地历史文件，自绘渲染，不声明任何 ticker。
    TardisBoard,
    /// 回测结果（docs/08 F6-P7）：收益曲线 / 回撤 / 各维度统计（读回测导出的 result.json）。
    BacktestResult,
    /// 订单（docs/27 §10）：持仓/收益读数 + 活动挂单 + 逐笔明细，回测与实盘过程中实时更新。
    /// 数据走通道① `trader-{run}:stream:events.*`，渲染读 `ws::readout` 旁路快照。
    Orders,
    /// 自有数据图（docs/08）：读 CSV/JSON 数据文件的通用自适应图（多列折线/散点）。
    /// 实为 Content::WealthSpring(SelfChart)；列入内容选择器便于自由摆放。无行情数据源。
    SelfChart,
}

/// WealthSpring pane 的三态过滤（docs/08 F6 方案 2「真隔离」）：
/// `Live`/`Backtest` 的 pane 只在 `ws:active_run` 对应态时渲染读数，否则显占位——
/// 让「实盘」「回测」两个工作区即使共享同一份全局三态也各自只显属于自己的数据。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize, Default)]
pub enum WsPaneMode {
    /// 任意态都渲染（默认；从内容选择器手建的 pane 即此态）。
    #[default]
    Any,
    /// 仅实盘态（active_run.mode == "live"）。
    Live,
    /// 仅回测态（active_run.mode == "backtest"）。
    Backtest,
    /// 自有数据自适应图（读 CSV/JSON 数据文件，不显读数；自有数据回测左侧）。
    SelfChart,
}

impl ContentKind {
    pub const ALL: [ContentKind; 28] = [
        ContentKind::Starter,
        ContentKind::HeatmapChart,
        ContentKind::ShaderHeatmap,
        ContentKind::FootprintChart,
        ContentKind::CandlestickChart,
        ContentKind::ComparisonChart,
        ContentKind::TimeAndSales,
        ContentKind::Ladder,
        ContentKind::WealthSpring,
        ContentKind::SelfChart,
        ContentKind::Factory,
        ContentKind::C4Shadow,
        ContentKind::OptionsBoard,
        ContentKind::PredictionBoard,
        ContentKind::PmBinance,
        ContentKind::PmReplay,
        ContentKind::FeatureLab,
        ContentKind::FeatureMatrix,
        ContentKind::MarketMap,
        ContentKind::Observatory,
        ContentKind::NetEgress,
        ContentKind::Procs,
        ContentKind::News,
        ContentKind::Recorder,
        ContentKind::TardisReplay,
        ContentKind::TardisBoard,
        ContentKind::BacktestResult,
        ContentKind::Orders,
    ];
}

impl std::fmt::Display for ContentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ContentKind::Starter => "Starter Pane",
            ContentKind::HeatmapChart => "Heatmap Chart (Legacy)",
            ContentKind::ShaderHeatmap => "Heatmap Chart",
            ContentKind::FootprintChart => "Footprint Chart",
            ContentKind::CandlestickChart => "Candlestick Chart",
            ContentKind::ComparisonChart => "Comparison Chart",
            ContentKind::TimeAndSales => "Time&Sales",
            ContentKind::Ladder => "DOM/Ladder",
            ContentKind::WealthSpring => "WealthSpring",
            ContentKind::SelfChart => "自有数据图",
            ContentKind::Factory => "Alpha Factory",
            ContentKind::C4Shadow => "C4 影子(SOL)",
            ContentKind::OptionsBoard => "期权/0DTE",
            ContentKind::PredictionBoard => "预测市场",
            ContentKind::PmBinance => "币安预测(BTC 5m)",
            ContentKind::PmReplay => "预测市场回放",
            ContentKind::FeatureLab => "特征库",
            ContentKind::FeatureMatrix => "特征矩阵",
            ContentKind::MarketMap => "全市场雷达",
            ContentKind::Observatory => "接口观察终端",
            ContentKind::NetEgress => "网络出口",
            ContentKind::Procs => "进程",
            ContentKind::News => "新闻资讯",
            ContentKind::Recorder => "数据录制",
            ContentKind::TardisReplay => "Tardis 历史回放",
            ContentKind::TardisBoard => "Tardis 历史面板",
            ContentKind::BacktestResult => "回测结果",
            ContentKind::Orders => "订单",
        };
        write!(f, "{s}")
    }
}

#[derive(Clone, Copy)]
pub struct PaneSetup {
    pub ticker_info: exchange::TickerInfo,
    pub basis: Option<Basis>,
    pub tick_multiplier: Option<TickMultiplier>,
    pub price_step: exchange::unit::PriceStep,
    pub depth_aggr: exchange::adapter::StreamTicksize,
    pub push_freq: exchange::PushFrequency,
}

impl PaneSetup {
    pub fn new(
        content_kind: ContentKind,
        base_ticker: TickerInfo,
        prev_base_ticker: Option<TickerInfo>,
        current_basis: Option<Basis>,
        current_tick_multiplier: Option<TickMultiplier>,
    ) -> Self {
        let exchange = base_ticker.ticker.exchange;

        let is_client_aggr = exchange.is_depth_client_aggr();
        let prev_is_client_aggr = prev_base_ticker
            .map(|ti| ti.ticker.exchange.is_depth_client_aggr())
            .unwrap_or(is_client_aggr);

        let basis =
            match content_kind {
                ContentKind::HeatmapChart => {
                    let current = current_basis.and_then(|b| match b {
                        Basis::Time(tf) if exchange.supports_heatmap_timeframe(tf) => Some(b),
                        _ => None,
                    });

                    Some(current.unwrap_or_else(|| Basis::default_heatmap_time(Some(base_ticker))))
                }
                ContentKind::Ladder => Some(
                    current_basis.unwrap_or_else(|| Basis::default_heatmap_time(Some(base_ticker))),
                ),
                ContentKind::ShaderHeatmap => Some(
                    current_basis.unwrap_or_else(|| Basis::default_heatmap_time(Some(base_ticker))),
                ),
                ContentKind::FootprintChart => {
                    let current = current_basis.and_then(|b| match b {
                        Basis::Time(tf) if exchange.supports_kline_timeframe(tf) => Some(b),
                        Basis::Tick(_) => Some(b),
                        _ => None,
                    });

                    Some(current.unwrap_or_else(|| {
                        Basis::default_kline_time(Some(base_ticker), Timeframe::M5)
                    }))
                }
                ContentKind::CandlestickChart | ContentKind::ComparisonChart => {
                    let current = current_basis.and_then(|b| match b {
                        Basis::Time(tf) if exchange.supports_kline_timeframe(tf) => Some(b),
                        _ => None,
                    });

                    Some(current.unwrap_or_else(|| {
                        Basis::default_kline_time(Some(base_ticker), Timeframe::M15)
                    }))
                }
                ContentKind::Starter
                | ContentKind::TimeAndSales
                | ContentKind::WealthSpring
                | ContentKind::SelfChart
                | ContentKind::Factory
                | ContentKind::C4Shadow
                | ContentKind::OptionsBoard
                | ContentKind::PredictionBoard
                | ContentKind::PmBinance
                | ContentKind::PmReplay
                | ContentKind::FeatureLab
                | ContentKind::FeatureMatrix
                | ContentKind::MarketMap
                | ContentKind::Observatory
                | ContentKind::NetEgress
                | ContentKind::Procs
                | ContentKind::News
                | ContentKind::Recorder
                | ContentKind::TardisReplay
                | ContentKind::TardisBoard
                | ContentKind::BacktestResult
                | ContentKind::Orders => None,
            };

        let tick_multiplier = match content_kind {
            ContentKind::HeatmapChart | ContentKind::Ladder | ContentKind::ShaderHeatmap => {
                let tm = if !is_client_aggr && prev_is_client_aggr {
                    TickMultiplier(10)
                } else if let Some(tm) = current_tick_multiplier {
                    tm
                } else if is_client_aggr {
                    TickMultiplier(5)
                } else {
                    TickMultiplier(10)
                };
                Some(tm)
            }
            ContentKind::FootprintChart => {
                Some(current_tick_multiplier.unwrap_or(TickMultiplier(50)))
            }
            ContentKind::CandlestickChart
            | ContentKind::ComparisonChart
            | ContentKind::TimeAndSales
            | ContentKind::WealthSpring
            | ContentKind::SelfChart
            | ContentKind::Factory
            | ContentKind::C4Shadow
            | ContentKind::OptionsBoard
            | ContentKind::PredictionBoard
            | ContentKind::PmBinance
            | ContentKind::PmReplay
            | ContentKind::FeatureLab
            | ContentKind::FeatureMatrix
            | ContentKind::MarketMap
            | ContentKind::Observatory
            | ContentKind::NetEgress
            | ContentKind::Procs
            | ContentKind::News
            | ContentKind::Recorder
            | ContentKind::TardisReplay
            | ContentKind::TardisBoard
            | ContentKind::BacktestResult
            | ContentKind::Orders
            | ContentKind::Starter => current_tick_multiplier,
        };

        let price_step = match tick_multiplier {
            Some(tm) => tm.multiply_with_min_tick_step(base_ticker),
            None => base_ticker.min_ticksize.into(),
        };

        let depth_aggr = exchange.stream_ticksize(tick_multiplier, TickMultiplier(50));

        let push_freq = match content_kind {
            ContentKind::HeatmapChart if exchange.is_custom_push_freq() => match basis {
                Some(Basis::Time(tf)) if exchange.supports_heatmap_timeframe(tf) => {
                    exchange::PushFrequency::Custom(tf)
                }
                _ => exchange::PushFrequency::ServerDefault,
            },
            _ => exchange::PushFrequency::ServerDefault,
        };

        Self {
            ticker_info: base_ticker,
            basis,
            tick_multiplier,
            price_step,
            depth_aggr,
            push_freq,
        }
    }
}
