//! 全市场雷达面板只读快照（docs/22 §4.2）。
//!
//! **独立新增面板**（`Content::MarketMap`）——不改任何既有面板/readout。数据源（全只读）：
//! `$XDG_RUNTIME_DIR/wealthspring/radar_board.json`（`ws-radar` 守护产出，
//! 落在 tmpfs 上、不写硬盘，见 `runtime_dir_from`）。沿用
//! c4_readout / prediction_readout 的旁路 poller 模式，但节拍取 2s——守护 5s 出一版，
//! 10s 会让面板明显滞后于数据。

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// 窗口顺序必须与 `wealthspring_radar::metrics::Window::ALL` 一致。
pub const WINDOWS: [&str; 6] = ["1m", "5m", "15m", "1h", "4h", "24h"];
pub const N_WIN: usize = 6;

/// 一个标的一行。
#[derive(Default, Clone)]
pub struct RadarRow {
    pub symbol: String,
    pub venue: String,
    /// 数据等级 A/B/C/D（docs/22 §0）。**逐行**给——一张表里混着 A 档加密和
    /// C 档延迟股票，不标就会被当成同一等级。
    pub tier: String,
    /// 资产类（`crypto` / `equity` / …）。由数据源声明，面板据此过滤。
    pub asset: String,
    /// 全称（股票源有，加密源空）。
    pub name: String,
    pub sector: String,
    pub country: String,
    pub currency: String,
    pub mcap: f64,
    /// TradingView 口径的全量指标（键见 docs/22 §4.3）。缺的指标**不在表里**，
    /// 不是 0——面板据此显示「—」并把格子置中性。
    pub m: std::collections::HashMap<String, f64>,
    /// 文本值的指标列（评级 / 财务期间 / K 线形态）。与 `m` 分开是因为它们
    /// 不是数字——混进 `m` 会让排序和着色拿到没法比较的值。
    pub t: std::collections::HashMap<String, String>,
    /// 加密分类（TradingView `crypto_common_categories`）。「来源」下拉据此过滤。
    pub cats: Vec<String>,
    pub price: f64,
    pub quote_vol_24h: f64,
    /// 各窗口对数收益。`None` = **该窗口还没热身**，不是「没涨没跌」。
    pub ret: [Option<f64>; N_WIN],
    /// 各窗口的波动归一化 z 值（docs/22 §3 的「涨跌速度」）。
    pub z_ret: [Option<f64>; N_WIN],
    pub z_vol: Option<f64>,
    pub z_cnt: Option<f64>,
    /// 自身样本足够、z 可信。false → 灰显。
    pub sigma_ok: bool,
    /// z 借了横截面中位数基线（暂且一看，不是结论）→ 同样降级渲染。
    pub z_provisional: bool,
}

impl RadarRow {
    /// 面板配色/排序用的主口径：5m 的 z，缺则退 1m。
    pub fn headline_z(&self) -> Option<f64> {
        self.z_ret[1].or(self.z_ret[0])
    }
    /// 读数是否可当真（两个降级标都干净）。
    pub fn trustworthy(&self) -> bool {
        self.sigma_ok && !self.z_provisional
    }
}

/// 回填进度（docs/22 P0c）。
#[derive(Default, Clone, PartialEq)]
pub struct BackfillView {
    pub done: i64,
    pub total: i64,
    pub failed: i64,
    pub running: bool,
    pub finished: bool,
}

impl BackfillView {
    pub fn pct(&self) -> f64 {
        if self.total <= 0 {
            0.0
        } else {
            self.done as f64 / self.total as f64
        }
    }
}

/// 一个市场的宽度（docs/22 §2 ③）。
#[derive(Default, Clone, PartialEq)]
pub struct BreadthRow {
    pub market: String,
    pub n: i64,
    pub adv: i64,
    pub dec: i64,
    pub unch: i64,
    pub new_high: i64,
    pub new_low: i64,
    pub net_new_high: i64,
    /// 该市场的股票**总数**。宽度只覆盖其中前 N 只。
    pub total: i64,
    /// 样本占全市场的比例。**必须显示出来**——覆盖 28% 与覆盖 100% 是两个
    /// 不同的结论（实测美国前 200 只上涨占比 34%、前 2000 只 49%，相反）。
    pub coverage: Option<f64>,
    /// 比值一律 `Option`——分母为 0 时给 `None`，面板显示「—」而不是 inf。
    pub adv_pct: Option<f64>,
    pub ad_ratio: Option<f64>,
    pub above_ma200_pct: Option<f64>,
}

/// 一行全球总览（docs/22 §2 ②）。
#[derive(Default, Clone, PartialEq)]
pub struct OverviewRow {
    pub label: String,
    pub ticker: String,
    pub currency: String,
    /// 本币对数收益，键为「日/周/月/YTD」。
    pub local: [Option<f64>; 4],
    /// 美元对数收益。**缺席即缺席**——拿不到汇率时绝不退回本币值。
    pub usd: [Option<f64>; 4],
}

pub const OV_WINDOWS: [&str; 4] = ["日", "周", "月", "YTD"];

/// 一家交易所（某个类型）的汇总（docs/22 §7）。
#[derive(Default, Clone, PartialEq)]
pub struct VenueRow {
    pub venue: String,
    pub label: String,
    /// `spot` / `perp` / `option`。
    pub kind: String,
    pub pairs: i64,
    pub vol_usd: f64,
    pub btc: Option<f64>,
    /// 抓取失败的原因。**有值就说明这行的数字不可信**——一家挂掉只表现为
    /// 成交额变 0 的话，和「今天没人交易」在面板上一模一样。
    pub err: String,
}

/// 一行加密行情（热门/涨/跌共用）。
#[derive(Default, Clone, PartialEq)]
pub struct CoinRow {
    pub symbol: String,
    pub venue: String,
    pub kind: String,
    pub price: f64,
    pub chg_pct: f64,
    pub vol_usd: Option<f64>,
    /// 该所这个对的网页。空 = 没有链接可给。
    pub url: String,
}

/// 一行永续。
#[derive(Default, Clone, PartialEq)]
pub struct PerpRow {
    pub symbol: String,
    pub venue: String,
    pub price: f64,
    pub chg_pct: f64,
    /// 单期费率。
    pub funding: f64,
    /// **年化**费率。各所结算间隔不同（HL 1h、其余 8h），只有这个能横比。
    pub funding_apr: f64,
    pub oi_usd: Option<f64>,
    pub vol_usd: Option<f64>,
    pub url: String,
}

/// 期权链汇总（Deribit 按币种）。
#[derive(Default, Clone, PartialEq)]
pub struct OptionRow {
    pub venue: String,
    pub currency: String,
    pub n: i64,
    pub oi_usd: f64,
    /// **权利金**成交额，不是名义值——两者差两个数量级。
    pub vol_usd: f64,
    pub vol_notional_usd: f64,
    pub iv: Option<f64>,
    pub underlying: Option<f64>,
    pub url: String,
}

/// 新上市。
#[derive(Default, Clone, PartialEq)]
pub struct ListingRow {
    pub symbol: String,
    pub venue: String,
    pub listed_ms: i64,
    pub url: String,
}

/// 加密全景整块。
#[derive(Default, Clone, PartialEq)]
pub struct Panorama {
    pub venues: Vec<VenueRow>,
    pub hot: Vec<CoinRow>,
    pub gainers: Vec<CoinRow>,
    pub losers: Vec<CoinRow>,
    pub perps: Vec<PerpRow>,
    pub options: Vec<OptionRow>,
    pub listings: Vec<ListingRow>,
}

/// 一行预测市场。
#[derive(Default, Clone, PartialEq)]
pub struct PredRow {
    pub platform: String,
    pub category: String,
    pub title: String,
    pub outcome: String,
    /// 概率（0~1）。
    pub prob: Option<f64>,
    /// 24h **概率点**变化（0.10 = 十个点），不是收益率。
    pub chg_24h: Option<f64>,
    pub vol_usd: f64,
    /// 成交额窗口：`24h`（Polymarket）或 `近期`（Kalshi）。
    /// **两家口径不同，必须跟着数字一起显示**。
    pub vol_window: String,
    pub liquidity_usd: Option<f64>,
    pub spread: Option<f64>,
    pub close_ms: Option<i64>,
    pub start_ms: Option<i64>,
    pub slug: String,
    /// 原盘页面。空 = 没有链接可给。
    pub url: String,
}

/// 预测市场平台状态。
#[derive(Default, Clone, PartialEq)]
pub struct PredSource {
    pub platform: String,
    pub label: String,
    pub rows: i64,
    pub err: String,
}

/// 美股市场状态。
#[derive(Default, Clone, PartialEq)]
pub struct MarketStatus {
    pub country: String,
    pub indicator: String,
    pub countdown: String,
    /// **上一交易日**。全表的价格就是这一天的收盘——休市日那是几天前的数，
    /// 不显示出来它和实时价长得一模一样。
    pub previous_trade_date: String,
    pub next_trade_date: String,
}

/// 一只股票。
#[derive(Default, Clone, PartialEq)]
pub struct StockRow {
    pub symbol: String,
    pub name: String,
    pub price: f64,
    pub net_change: f64,
    pub chg_pct: f64,
    pub volume: f64,
    /// 成交额 = 价 × 量（原表没有这一列）。
    pub turnover: f64,
    /// 0 = 原表为空（多为 ETF），**不是市值为零**。
    pub mcap: f64,
    pub sector: String,
    pub industry: String,
    pub country: String,
    pub ipo_year: i64,
    /// 纳斯达克个股页。
    pub url: String,
}

/// 一个板块。
#[derive(Default, Clone, PartialEq)]
pub struct SectorRow {
    pub sector: String,
    pub n: i64,
    pub adv: i64,
    pub dec: i64,
    /// 市值加权涨跌（%）。只用有市值的那些算。
    pub wtd_chg: Option<f64>,
    /// 加权覆盖了几只。与 `n` 差很多时这个数代表性就差。
    pub wtd_n: i64,
    /// 中位涨跌（%）。等权口径，不受几只权重股绑架。
    pub median_chg: Option<f64>,
    pub turnover: f64,
}

/// 一条新股。
#[derive(Default, Clone, PartialEq)]
pub struct IpoRow {
    pub symbol: String,
    pub company: String,
    pub exchange: String,
    pub status: String,
    pub price: String,
    pub shares: String,
    pub value: String,
    pub date: String,
    pub url: String,
}

/// 一条指数报价（Cboe 延迟）。
#[derive(Default, Clone, PartialEq)]
pub struct IndexQuote {
    pub symbol: String,
    pub label: String,
    pub price: f64,
    pub chg: f64,
    pub chg_pct: f64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub prev_close: f64,
    /// 最后成交时间——Cboe 是延迟数据，这就是延迟多少的证据。
    pub last_trade: String,
    pub url: String,
}

/// 股票全景整块。
#[derive(Default, Clone, PartialEq)]
pub struct EquityPanorama {
    pub status: MarketStatus,
    pub indices: Vec<IndexQuote>,
    pub universe: i64,
    pub hot: Vec<StockRow>,
    pub gainers: Vec<StockRow>,
    pub losers: Vec<StockRow>,
    pub sectors: Vec<SectorRow>,
    pub ipos: Vec<IpoRow>,
    pub errors: Vec<(String, String)>,
    /// 守护**实际查的**新股月份（`YYYY-MM`）。显示这个而不是面板自己请求的那个：
    /// 换月的那一两轮里两者不一样，显示请求值会让人以为已经换好了。
    pub month: String,
}

/// 一个宏观读数。
#[derive(Default, Clone, PartialEq)]
pub struct MacroRow {
    pub source: String,
    pub area: String,
    pub label: String,
    pub value: f64,
    pub unit: String,
    /// **观测日期**。同一张表里能差近一年（政策利率是当天、世行 GDP 是去年），
    /// 缺了这一列整张表就没法读。
    pub obs: String,
    pub chg: Option<f64>,
    /// 该序列在原站上的页面。
    pub url: String,
}

/// 一条新闻。
#[derive(Default, Clone, PartialEq)]
pub struct NewsRow {
    pub source: String,
    pub title: String,
    pub link: String,
    pub published: String,
    pub ts_ms: i64,
}

/// 宏观来源状态。
#[derive(Default, Clone, PartialEq)]
pub struct MacroSource {
    pub key: String,
    pub label: String,
    pub rows: i64,
    /// 空 = 正常。「未配置 …」与抓取失败都走这里，但文案不同。
    pub err: String,
}

/// 宏观 + 新闻整块。
#[derive(Default, Clone, PartialEq)]
pub struct MacroBoard {
    pub rows: Vec<MacroRow>,
    pub news: Vec<NewsRow>,
    pub sources: Vec<MacroSource>,
}

/// 各块的抓取时刻（Unix 毫秒）。名字避开已有的 `Fetched`（进度里那个）。
#[derive(Default, Clone, Copy, PartialEq)]
pub struct FetchedAt {
    pub panorama: i64,
    pub prediction: i64,
    pub equity: i64,
    pub macros: i64,
    pub breadth: i64,
    pub overview: i64,
}

/// 预测市场整块。
#[derive(Default, Clone, PartialEq)]
pub struct Prediction {
    pub sources: Vec<PredSource>,
    pub hot: Vec<PredRow>,
    pub movers: Vec<PredRow>,
    pub fresh: Vec<PredRow>,
}

/// 一个列组标签。
#[derive(Default, Clone, PartialEq)]
pub struct ColumnTab {
    pub key: String,
    pub label: String,
    pub cols: Vec<String>,
}

/// 来源目录的一项。
#[derive(Default, Clone, PartialEq)]
pub struct CatalogItem {
    pub code: String,
    pub label: String,
    /// 市场才有；类型/分类为空。
    pub region: String,
    /// 「所有 X 公司」那一项的显示名。空 = 由 `label` 拼。
    /// 欧洲组下有两项（所有欧盟公司 / 所有欧洲公司）共用组名「欧洲」，拼不出来。
    pub all_label: String,
}

/// 一个资产类的可选项（目录下发，面板不硬编码任何一类的选项）。
///
/// **每类的字段集完全不同**：股票 `market_cap_basic`、加密 `market_cap_calc`、
/// DEX `dex_total_liquidity`。共用一套扁平选项的话，下拉里一半取不到值。
#[derive(Clone, Default)]
pub struct AssetSpec {
    pub kind: String,
    pub label: String,
    /// 官方只有 股票 / ETF / 加密 三种热图。
    pub heatmap: bool,
    pub tabs: Vec<ColumnTab>,
    pub size: Vec<CatalogItem>,
    pub color: Vec<CatalogItem>,
    pub group: Vec<CatalogItem>,
    /// 色阶的三个正向边界（百分数）。**每类不同**（股票 ±1/2/3%、
    /// 加密 ±3/8/13%），共用一套的话加密会整片顶到最深档。
    pub scale: [f64; 3],
}

/// 来源目录（docs/22 §6.5）：面板「来源」下拉的**全部可选项**，随快照下发。
/// 只列已加载的 venue 的话，用户永远只能在守护恰好在拉的那几个市场里打转。
#[derive(Default, Clone)]
pub struct Catalog {
    /// Screener 的列组（对齐 TV 的 Overview/Performance/…）。随快照下发，
    /// 面板不硬编码列名——加列只改守护侧一处。
    pub tabs: Vec<ColumnTab>,
    /// 每个资产类自己的列组与热图选项（docs/22 §6.13）。面板据此组织界面——
    /// 先选资产类，下面的选项才跟着走。
    pub assets: Vec<AssetSpec>,
    /// 百分数口径的指标键。
    pub pct_keys: std::collections::HashSet<String>,
    /// 本币金额口径的指标键（已换算成美元）。
    /// 值是 Unix 秒的指标（财报日期）——不单列一类会显示成十位整数。
    pub date_keys: std::collections::HashSet<String>,
    /// 值是 **YYYYMMDD 整数**的指标（债券到期日）。与 `date_keys` 分开：
    /// 两者都叫日期但解码方式不同，混用会把 2029-02-14 显示成 1970 年，
    /// 不标注则显示成 `20.3M`。
    pub ymd_keys: std::collections::HashSet<String>,
    pub money_keys: std::collections::HashSet<String>,
    pub markets: Vec<CatalogItem>,
    /// 指数成分股来源。`code` 是来源 id（`america@SPX`），`region` 借用为所属市场。
    pub indices: Vec<CatalogItem>,
    pub types: Vec<CatalogItem>,
    pub crypto_cats: Vec<CatalogItem>,
    /// 加密热图「来源」的 13 项官方预设（含分类映射与排除规则）。
    pub coin_presets: Vec<super::radar::CoinPreset>,
    /// 列键 → 中文标题。**由守护下发**，面板不再维护平行表。
    pub titles: std::collections::HashMap<String, String>,
    /// ETF「来源」覆盖的市场代码（24 个，**已按官方顺序排好**：英文国名
    /// A→Z、不置顶，美国落在 U 段）。股票那份是 60 个、且中国置顶，两份不能共用。
    pub etf_markets: Vec<String>,
    /// **筛选器**的「市场」下拉（71 国 + 全球，拼音序）。
    /// 与 `markets`（热图的「来源」，60 国 + 指数）**是两个控件**，不能互相替代。
    pub screener_markets: Vec<CatalogItem>,
    /// **每类的筛选清单**（守护下发，逐类抄自官方筛选栏）。
    /// 每类完全不同——共用一套的话，一半筛选在另一半类上是空表。
    pub filters: Vec<(String, Vec<FilterItem>)>,
    /// 枚举型筛选的取值（抄自官方 `enum/ordered`）。按 `field` 与 `filters` 关联。
    pub enums: Vec<EnumItem>,
}

/// 一条筛选（目录下发）。
#[derive(Default, Clone, PartialEq, Debug)]
pub struct FilterItem {
    pub field: String,
    pub label: String,
    /// `num` / `days` / `enum` / `index`。
    pub kind: String,
}

/// 一个枚举字段的取值表。
#[derive(Default, Clone, PartialEq, Debug)]
pub struct EnumItem {
    pub field: String,
    pub label: String,
    /// 匹配算子：多数 `equal`；数组型（加密分类）必须 `has`。
    pub op: String,
    /// `(发给服务端的 id, 中文显示名)`。
    pub values: Vec<(String, String)>,
}

impl Catalog {
    /// 某一类的筛选清单。目录没给（旧守护）时返回空。
    pub fn filters_for(&self, kind: &str) -> &[FilterItem] {
        self.filters.iter().find(|(k, _)| k == kind).map(|(_, v)| v.as_slice()).unwrap_or(&[])
    }

    /// 某个枚举字段的取值。
    pub fn enum_of(&self, field: &str) -> Option<&EnumItem> {
        self.enums.iter().find(|e| e.field == field)
    }
}

/// 守护的抓取进度（面板进度条）。
#[derive(Default, Clone, PartialEq, Debug)]
pub struct FetchProgress {
    pub done: usize,
    pub total: usize,
    /// 正在抓的来源（`spain/stock`）。空 = 本轮跑完了。
    pub cur: String,
    pub eta_s: u64,
    /// 面板点名要的来源里还没抓到的个数。
    pub pending: usize,
    /// 点名来源各自的结果。有它才分得清「还在抓」和「抓完了一行都没有」。
    pub wanted: Vec<Fetched>,
}

/// 一个点名来源这一轮的结果。
#[derive(Default, Clone, PartialEq, Debug)]
pub struct Fetched {
    pub src: String,
    pub sec: String,
    pub rows: usize,
    /// 本轮是否已经抓过。false = 还在队列里等。
    pub done: bool,
    /// 失败原因；空 = 没失败。**抓失败与抓到 0 行是两回事**。
    pub err: String,
}

impl FetchProgress {
    /// 某个来源这一轮的结果。`None` = 守护还没把它列进来。
    pub fn of(&self, src: &str) -> Option<&Fetched> {
        self.wanted.iter().find(|w| w.src == src)
    }

    /// 0~1。`total` 为 0 时是 1（没活干 = 已完成），不是 0——
    /// 否则空闲时进度条停在最左边，看着像卡住。
    pub fn frac(&self) -> f32 {
        if self.total == 0 {
            return 1.0;
        }
        (self.done as f32 / self.total as f32).clamp(0.0, 1.0)
    }
}

#[derive(Default, Clone)]
pub struct RadarReadout {
    /// 数据版本号，每轮轮询递增。**只用来判断「数据变没变」**——
    /// 树图的 canvas 缓存据此决定要不要重画（见 radar_view 的 TREEMAP_CACHE）。
    pub generation: u64,
    pub stamp: String,
    /// 守护的抓取进度。选了一个还没拉的来源时，面板用它画进度条 + 倒计时。
    pub progress: FetchProgress,
    pub source: String,
    /// 数据等级（docs/22 §0）：加密直连 = "A"。面板**必须**把它显示出来。
    pub tier: String,
    /// 资产类（`crypto` / `equity` / …）。由数据源声明，面板据此过滤。
    pub asset: String,
    pub n_symbols: i64,
    pub refreshed_ms: i64,
    pub rows: Vec<RadarRow>,
    pub backfill: BackfillView,
    pub catalog: Catalog,
    pub breadth: Vec<BreadthRow>,
    pub overview: Vec<OverviewRow>,
    pub panorama: Panorama,
    pub prediction: Prediction,
    pub equity: EquityPanorama,
    pub macros: MacroBoard,
    /// 各块**各自**的抓取时刻（Unix 毫秒，0 = 还没抓过）。
    /// 三个节拍差着两个数量级，拿一个总的快照时间当每块的时间，
    /// 会把一小时前的宏观数据标成「刚刚更新」。
    pub fetched: FetchedAt,
    pub refreshed: String,
    /// 慢层快照的时间戳（股票 60s 一刷，与热层不同步——面板要分别标注，
    /// 否则会拿热层的时间当成股票数据的时间）。
    pub slow_stamp: String,
    pub present: bool,
    /// 守护 service 状态。后台 poller 刷——**不能在 view 里查**，那是每帧一次
    /// systemctl 子进程（docs/20 §19.2 的每帧开销教训）。
    pub svc: super::svcctl::UnitState,
}

static READOUT: OnceLock<Mutex<std::sync::Arc<RadarReadout>>> = OnceLock::new();
static POLLER: OnceLock<()> = OnceLock::new();
static WAKER: super::svcctl::Waker = super::svcctl::Waker::new();

pub const RADAR_SVC: &str = "ws-radar.service";

/// 快照落点：**内存文件系统，不写硬盘**。
///
/// 快照是纯易失数据，重启就该重新生成，没有一份值得留在盘上；写盘的唯一
/// 后果是磨损 SSD（实测 0.23 MB/s、一天 20GB）。按顺序取第一个可用的：
///   1. `$XDG_RUNTIME_DIR/wealthspring`（systemd 给每个用户挂的 tmpfs，0700、登出即清）
///   2. `/dev/shm/wealthspring-$USER`（tmpfs 兜底）
///   3. `$HOME/ws-data/live`（**会写盘**，只在系统没有 tmpfs 时走到）
///
/// 定义在 `ws::paths`（docs/26 S3 统一），那里另有守护侧的孪生说明。
fn runtime_dir_from(xdg: Option<&str>, user: Option<&str>, home: Option<&str>) -> PathBuf {
    super::paths::runtime_dir_from(xdg, user, home, std::path::Path::new("/dev/shm").is_dir())
}

fn runtime_dir() -> PathBuf {
    super::paths::runtime_dir()
}

/// 抓取进度文件。守护每抓完一个来源就更新一次（文件很小，在 tmpfs 上）。
fn progress_path() -> PathBuf {
    let b = board_path();
    let stem = b.file_stem().and_then(|x| x.to_str()).unwrap_or("radar_board");
    b.with_file_name(format!("{stem}_progress.json"))
}

fn board_path() -> PathBuf {
    std::env::var("WS_RADAR_BOARD")
        .map(PathBuf::from)
        .unwrap_or_else(|_| runtime_dir().join("radar_board.json"))
}

/// 慢层快照（股票/ETF + 宽度 + 总览），与热层同目录、文件名加 `_slow`。
///
/// 拆两个文件是为写盘量：加密 5s 一变、股票 60s 才变，合成一个的话股票那
/// 五十多列基本面指标会跟着每 5s 重写（实测 1.94 MB/s、一天 167GB）。
fn slow_path() -> PathBuf {
    let p = board_path();
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("radar_board");
    p.with_file_name(format!("{stem}_slow.json"))
}

/// 当前读数。**返回 `Arc`，不深拷贝**。
///
/// 视图每帧都调它一次。原来返回的是 `RadarReadout` 的深拷贝——4189 行、每行
/// 一个上百项的 `HashMap<String, f64>` 外加文本段和七八个 `String`，一帧要
/// 重新分配几千万字节。列数从 72 涨到 114 之后这笔开销翻倍，主线程直接
/// 打满一个核（实测 99.9%）。换成 `Arc` 后每帧只是一次引用计数加一。
pub fn snapshot() -> std::sync::Arc<RadarReadout> {
    ensure_poller();
    READOUT
        .get()
        .and_then(|m| m.lock().ok().map(|g| g.clone()))
        .unwrap_or_default()
}

pub fn request_refresh() {
    WAKER.request();
}

fn request_path() -> PathBuf {
    std::env::var("WS_RADAR_REQUEST")
        .map(PathBuf::from)
        .unwrap_or_else(|_| runtime_dir().join("radar_request.json"))
}

/// 纯构造（可单测）：选中的来源 → 请求 JSON。
///
/// 只带**当前选中项**，不累积：累积会让守护的请求数随用户点击单调增长，
/// 最后把非官方接口打爆。配置里的默认市场恒在，选中的额外加一个。
pub fn request_body(
    source: &str,
    security_types: &[&str],
    filters: &[super::radar_filter::Wire],
) -> String {
    let sources: Vec<serde_json::Value> = if source.is_empty() {
        Vec::new()
    } else {
        security_types
            .iter()
            .map(|t| serde_json::json!({"market": source, "security_type": t}))
            .collect()
    };
    // 筛选必须**下推到服务端**：只在守护抓回的样本里筛，等于只在按市值前 N 只
    // 里找。实测全美「P/E<10 且 息>4%」命中 87 只，样本里只有 2 只。
    let f: Vec<serde_json::Value> = filters
        .iter()
        .map(|w| {
            if !w.texts.is_empty() {
                // 枚举多选：文本的 in_range（OR）
                serde_json::json!({"field": w.field, "op": w.op, "texts": w.texts})
            } else if let Some(t) = w.text {
                serde_json::json!({"field": w.field, "op": w.op, "text": t})
            } else {
                serde_json::json!({"field": w.field, "op": w.op, "right": w.right})
            }
        })
        .collect();
    // `nonce`：**只为让内容变化**。守护是靠「请求文件内容变了」判断要不要
    // 提前抓的，而 `write_request` 又会把内容相同的写入去重掉——没有它的话，
    // 「立即获取」按钮在选择没变时是个空操作。
    serde_json::json!({ "sources": sources, "filters": f, "nonce": nonce() }).to_string()
}

/// 「立即获取」的计数器。加一次就让下一份请求体与上一份不同。
static NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn nonce() -> u64 {
    NONCE.load(std::sync::atomic::Ordering::Relaxed)
}

/// 点「立即获取」时调用：改变请求体内容，逼守护中断当前一轮去抓。
pub fn bump_nonce() {
    NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// 把选中的来源写给守护（原子写，同快照）。
///
/// 内容没变就不写：面板每帧都会走到这里，每帧重写文件既浪费也会让守护看到抖动。
/// 请求文件的**全部**内容都由这里出去。分成两路写会互相把对方的字段冲掉——
/// 守护读的是同一个文件，后写的那次没带 `sources` 就等于把来源清空了。
static REQ_PARTS: Mutex<Option<(String, Vec<String>, String)>> = Mutex::new(None);
static IPO_MONTH: Mutex<String> = Mutex::new(String::new());

pub fn write_request(
    source: &str,
    security_types: &[&str],
    filters: &[super::radar_filter::Wire],
) {
    if let Ok(mut g) = REQ_PARTS.lock() {
        *g = Some((
            source.to_string(),
            security_types.iter().map(|x| x.to_string()).collect(),
            request_body(source, security_types, filters),
        ));
    }
    flush_request();
}

/// 各块「立即刷新」的计数。只增不减——守护比对**变没变**，不解释值。
///
/// 不用布尔：布尔要守护读完清掉，那就得由守护写回请求文件，
/// 于是面板和守护同时写一个文件，先写的那个会被冲掉。
static FORCE: Mutex<Option<std::collections::BTreeMap<String, i64>>> = Mutex::new(None);

/// 请求守护立刻重取某一块。
pub fn force_block(block: &str) {
    if let Ok(mut g) = FORCE.lock() {
        let m = g.get_or_insert_with(Default::default);
        *m.entry(block.to_string()).or_insert(0) += 1;
    }
    flush_request();
}

/// 设新股日历要查的月份。与来源/筛选走同一个文件，故只改这一项、其余保留。
pub fn set_ipo_month(month: &str) {
    if let Ok(mut g) = IPO_MONTH.lock() {
        if *g == month {
            return;
        }
        *g = month.to_string();
    }
    flush_request();
}

fn flush_request() {
    static LAST: Mutex<String> = Mutex::new(String::new());
    // 还没写过来源时也要能发月份：新股面板可能是用户开机后点的第一个东西
    let base = REQ_PARTS
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|(_, _, b)| b.clone()))
        .unwrap_or_else(|| request_body("", &[], &[]));
    let month = IPO_MONTH.lock().map(|g| g.clone()).unwrap_or_default();
    let force = FORCE.lock().ok().and_then(|g| g.clone()).unwrap_or_default();
    let body = match serde_json::from_str::<serde_json::Value>(&base) {
        Ok(mut v) => {
            if !month.is_empty() {
                v["ipo_month"] = serde_json::json!(month);
            }
            if !force.is_empty() {
                v["force"] = serde_json::json!(force);
            }
            v.to_string()
        }
        Err(_) => base,
    };
    if let Ok(mut g) = LAST.lock() {
        if *g == body {
            return;
        }
        *g = body.clone();
    }
    let p = request_path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = p.with_extension("json.tmp");
    if std::fs::write(&tmp, body.as_bytes()).is_ok() {
        let _ = std::fs::rename(&tmp, &p);
    }
}

/// 链接登记表：`RadarMsg` 是 `Copy` 的、装不下可变长的串，而按帧下标发消息
/// 会在快照刷新后错位（点开的是别人的链接）。故用**内容哈希**做 id：
/// 同一个 URL 永远是同一个 id，登记表只增不改，旧 id 也一定解得对。
static LINKS: Mutex<Option<std::collections::HashMap<u64, String>>> = Mutex::new(None);

/// 表里最多留多少条。加密对约两千、美股七千，正常远到不了；
/// 上限只是防某天数据源开始吐随机 URL 时把内存吃光。
const MAX_LINKS: usize = 50_000;

fn hash_url(u: &str) -> u64 {
    // FNV-1a：够散且不引依赖。撞了也只是两条链接串号，不会崩
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in u.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

/// 登记一个链接，返回它的 id。空串返回 `None`——**没有链接就不该有按钮**，
/// 给个点了没反应的按钮比不给更糟。
pub fn register_link(url: &str) -> Option<u64> {
    if url.is_empty() {
        return None;
    }
    let id = hash_url(url);
    if let Ok(mut g) = LINKS.lock() {
        let m = g.get_or_insert_with(Default::default);
        if !m.contains_key(&id) {
            if m.len() >= MAX_LINKS {
                return None;
            }
            m.insert(id, url.to_string());
        }
    }
    Some(id)
}

/// 按 id 取回链接。
pub fn link_of(id: u64) -> Option<String> {
    LINKS.lock().ok().and_then(|g| g.as_ref().and_then(|m| m.get(&id).cloned()))
}

/// 用系统默认浏览器打开。**必须 detach**：`xdg-open` 会把子进程留在那儿，
/// 不 spawn-and-forget 的话面板会攒一堆僵尸；也不能 `status()` 等它，
/// 那是拿 UI 线程去等一个浏览器启动。
pub fn open_link(id: u64) -> String {
    let Some(url) = link_of(id) else {
        return "链接已失效".into();
    };
    // 只放行 http(s)：登记表里的串来自守护下发的快照，
    // 而 `xdg-open` 会按协议头去调任意处理器（`file://`、自定义 scheme…）
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return format!("拒绝打开非 http(s) 链接：{url}");
    }
    match std::process::Command::new("xdg-open")
        .arg(&url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_) => format!("已在浏览器打开 {url}"),
        Err(e) => format!("打开失败：{e}"),
    }
}

/// 启动守护。**必须 `--no-block`**（同 prediction/factory 踩过的坑：`systemctl start`
/// 会等到 unit 就绪才返回，足以冻死 UI 线程）。
pub fn radar_start() -> String {
    match std::process::Command::new("systemctl")
        .args(["--user", "start", "--no-block", RADAR_SVC])
        .status()
    {
        Ok(s) if s.success() => {
            WAKER.request();
            "▶ 已启动雷达守护（各窗口需热身，见 docs/22 §8.1）".into()
        }
        Ok(s) => format!("✗ 启动失败，退出码 {:?}", s.code()),
        Err(e) => format!("✗ 启动失败：{e}"),
    }
}

pub fn radar_stop() -> String {
    match std::process::Command::new("systemctl")
        .args(["--user", "stop", RADAR_SVC])
        .status()
    {
        Ok(s) if s.success() => {
            WAKER.request();
            "■ 已停止雷达守护（内存中的 EWMA 状态会丢，重启需重新热身）".into()
        }
        Ok(s) => format!("✗ 停止失败，退出码 {:?}", s.code()),
        Err(e) => format!("✗ 停止失败：{e}"),
    }
}

fn ensure_poller() {
    POLLER.get_or_init(|| {
        std::thread::spawn(|| {
            let mut svc = super::svcctl::UnitState::default();
            let mut tick = 0u32;
            loop {
                let mut snap = poll_once();
                snap.generation = tick as u64;
                // `systemctl show` 是一次 **fork+exec**。每 2s 一次的话，光它就贡献了
                // 每秒近百次读系统调用（实测），而服务状态几乎不变。降到 10s 一次，
                // 中间沿用上次结果；面板按钮启停后会立刻 WAKER 唤醒，不必靠轮询看到。
                if tick % 5 == 0 {
                    svc = super::svcctl::query(RADAR_SVC);
                }
                tick = tick.wrapping_add(1);
                snap.svc = svc.clone();
                let lock =
                    READOUT.get_or_init(|| Mutex::new(std::sync::Arc::new(RadarReadout::default())));
                if let Ok(mut g) = lock.lock() {
                    *g = std::sync::Arc::new(snap);
                }
                WAKER.wait(Duration::from_secs(2));
            }
        });
    });
}

fn read_json(p: PathBuf) -> Option<serde_json::Value> {
    std::fs::read_to_string(p)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
}

fn poll_once() -> RadarReadout {
    let refreshed = chrono::Local::now().format("%H:%M:%S").to_string();
    let Some(hot) = read_json(board_path()) else {
        return RadarReadout { refreshed, ..Default::default() };
    };
    let mut st = RadarReadout { refreshed, ..parse_board(&hot) };
    // 枚举取值灌进筛选模块的注册表：`preset_label` 用在 `Display` 里，
    // 塞不进目录引用，只能走注册表（见 radar_filter::ENUM_VALUES）
    if !st.catalog.enums.is_empty() {
        let mut reg = super::radar_filter::EnumReg::default();
        for e in &st.catalog.enums {
            reg.by_field.insert(e.field.clone(), (e.op.clone(), e.values.clone()));
        }
        super::radar_filter::set_enums(reg);
    }
    // 抓取进度：单独一个小文件，守护每个来源更新一次。读不到就当没有进度条，
    // 不影响其余显示
    st.progress = read_json(progress_path()).map(|v| parse_progress(&v)).unwrap_or_default();
    // 慢层可缺（股票层没开、或还没写第一轮）——热层照常显示，不整个作废
    if let Some(slow) = read_json(slow_path()) {
        let s = parse_board(&slow);
        st.rows.extend(s.rows);
        st.breadth = s.breadth;
        st.overview = s.overview;
        st.panorama = s.panorama;
        st.prediction = s.prediction;
        st.equity = s.equity;
        st.macros = s.macros;
        st.fetched = s.fetched;
        // 标的总数是两层之和；单层的 n_symbols 只算自己那部分
        st.n_symbols += s.n_symbols;
        st.slow_stamp = s.stamp;
    }
    st
}

/// 纯解析（可单测）：进度文件 → [`FetchProgress`]。
fn parse_progress(v: &serde_json::Value) -> FetchProgress {
    let n = |k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
    FetchProgress {
        done: n("done") as usize,
        total: n("total") as usize,
        cur: v.get("cur").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        eta_s: n("eta_s"),
        pending: n("pending") as usize,
        wanted: v
            .get("wanted")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .map(|o| Fetched {
                        src: o.get("src").and_then(|x| x.as_str()).unwrap_or("").into(),
                        sec: o.get("sec").and_then(|x| x.as_str()).unwrap_or("").into(),
                        rows: o.get("rows").and_then(|x| x.as_u64()).unwrap_or(0) as usize,
                        done: o.get("done").and_then(|x| x.as_bool()).unwrap_or(false),
                        err: o.get("err").and_then(|x| x.as_str()).unwrap_or("").into(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// 从窗口字典里按 [`WINDOWS`] 顺序取值。**缺席即 `None`，不补 0**——
/// 补 0 会把「还没热身」显示成「没涨没跌」，是这个面板最容易犯的谎。
fn win_arr(o: Option<&serde_json::Value>) -> [Option<f64>; N_WIN] {
    let mut out = [None; N_WIN];
    if let Some(m) = o {
        for (i, k) in WINDOWS.iter().enumerate() {
            out[i] = m.get(*k).and_then(|x| x.as_f64());
        }
    }
    out
}

/// 纯解析（可单测）：radar_board.json → RadarReadout。
fn parse_board(v: &serde_json::Value) -> RadarReadout {
    let s = |o: &serde_json::Value, k: &str| {
        o.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
    };
    let rows = v
        .get("rows")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .map(|o| RadarRow {
                    symbol: s(o, "symbol"),
                    venue: s(o, "venue"),
                    tier: s(o, "tier"),
                    asset: s(o, "asset"),
                    name: s(o, "name"),
                    sector: s(o, "sector"),
                    country: s(o, "country"),
                    currency: s(o, "currency"),
                    mcap: o.get("mcap").and_then(|x| x.as_f64()).unwrap_or(0.0),
                    cats: o
                        .get("cats")
                        .and_then(|x| x.as_array())
                        .map(|a| {
                            a.iter().filter_map(|c| c.as_str().map(String::from)).collect()
                        })
                        .unwrap_or_default(),
                    m: o
                        .get("m")
                        .and_then(|x| x.as_object())
                        .map(|mm| {
                            mm.iter()
                                .filter_map(|(k, v)| v.as_f64().map(|f| (k.clone(), f)))
                                .collect()
                        })
                        .unwrap_or_default(),
                    t: o
                        .get("t")
                        .and_then(|x| x.as_object())
                        .map(|mm| {
                            mm.iter()
                                .filter_map(|(k, v)| {
                                    v.as_str().filter(|s| !s.is_empty()).map(|s| (k.clone(), s.to_string()))
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                    price: o.get("price").and_then(|x| x.as_f64()).unwrap_or(0.0),
                    quote_vol_24h: o
                        .get("quote_vol_24h")
                        .and_then(|x| x.as_f64())
                        .unwrap_or(0.0),
                    ret: win_arr(o.get("ret")),
                    z_ret: win_arr(o.get("z_ret")),
                    z_vol: o.get("z_vol").and_then(|x| x.as_f64()),
                    z_cnt: o.get("z_cnt").and_then(|x| x.as_f64()),
                    sigma_ok: o.get("sigma_ok").and_then(|x| x.as_bool()).unwrap_or(false),
                    z_provisional: o
                        .get("z_provisional")
                        .and_then(|x| x.as_bool())
                        .unwrap_or(false),
                })
                .collect()
        })
        .unwrap_or_default();
    let bf = v.get("backfill");
    let bi = |k: &str| {
        bf.and_then(|o| o.get(k))
            .and_then(|x| x.as_i64())
            .unwrap_or(0)
    };
    let bb = |k: &str| {
        bf.and_then(|o| o.get(k))
            .and_then(|x| x.as_bool())
            .unwrap_or(false)
    };
    let i64_of = |o: &serde_json::Value, k: &str| o.get(k).and_then(|x| x.as_i64()).unwrap_or(0);
    let optf_of = |o: &serde_json::Value, k: &str| o.get(k).and_then(|x| x.as_f64());
    let items = |k: &str| -> Vec<CatalogItem> {
        v.get("catalog")
            .and_then(|c| c.get(k))
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .map(|o| CatalogItem {
                        code: s(o, "code"),
                        label: s(o, "label"),
                        region: s(o, "region"),
                        all_label: s(o, "all_label"),
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    // 指数与市场用**同一套键**（code/label/region），共用 `items` 解析器。
    // 原来指数走的是 id/market 一套单独的键——两边都对，但多一套就多一处
    // 会漂移的地方（改守护时就把它改漂了一次，指数来源整个消失）。
    let index_items: Vec<CatalogItem> = items("indices");
    let cat = |k: &str| v.get("catalog").and_then(|c| c.get(k));
    let tabs: Vec<ColumnTab> = cat("tabs")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .map(|o| ColumnTab {
                    key: s(o, "key"),
                    label: s(o, "label"),
                    cols: o
                        .get("cols")
                        .and_then(|x| x.as_array())
                        .map(|c| c.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                        .unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default();
    let keys = |k: &str| -> std::collections::HashSet<String> {
        cat("units")
            .and_then(|u| u.get(k))
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default()
    };
    let parse_items = |v: Option<&serde_json::Value>| -> Vec<CatalogItem> {
        v.and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .map(|o| CatalogItem {
                        code: o.get("key").and_then(|x| x.as_str()).unwrap_or_default().into(),
                        label: o.get("label").and_then(|x| x.as_str()).unwrap_or_default().into(),
                        region: String::new(),
                        all_label: String::new(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let parse_tabs = |v: Option<&serde_json::Value>| -> Vec<ColumnTab> {
        v.and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .map(|o| ColumnTab {
                        key: o.get("key").and_then(|x| x.as_str()).unwrap_or_default().into(),
                        label: o.get("label").and_then(|x| x.as_str()).unwrap_or_default().into(),
                        cols: o
                            .get("cols")
                            .and_then(|x| x.as_array())
                            .map(|c| c.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                            .unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let assets: Vec<AssetSpec> = cat("assets")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .map(|o| AssetSpec {
                    kind: o.get("kind").and_then(|x| x.as_str()).unwrap_or_default().into(),
                    label: o.get("label").and_then(|x| x.as_str()).unwrap_or_default().into(),
                    heatmap: o.get("heatmap").and_then(|x| x.as_bool()).unwrap_or(false),
                    tabs: parse_tabs(o.get("tabs")),
                    size: parse_items(o.get("size")),
                    color: parse_items(o.get("color")),
                    group: parse_items(o.get("group")),
                    scale: o
                        .get("scale")
                        .and_then(|x| x.as_array())
                        .filter(|a| a.len() == 3)
                        .map(|a| {
                            let g = |i: usize| a[i].as_f64().unwrap_or(0.0);
                            [g(0), g(1), g(2)]
                        })
                        .unwrap_or([1.0, 2.0, 3.0]),
                })
                .collect()
        })
        .unwrap_or_default();

    let catalog = Catalog {
        assets,
        tabs,
        pct_keys: keys("pct"),
        money_keys: keys("money"),
        date_keys: keys("date"),
        ymd_keys: keys("ymd"),
        markets: items("markets"),
        indices: index_items,
        types: items("types"),
        crypto_cats: items("crypto_cats"),
        titles: cat("titles")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|o| {
                        Some((
                            o.get("key")?.as_str()?.to_string(),
                            o.get("title")?.as_str()?.to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default(),
        coin_presets: cat("coin_presets")
            .and_then(|x| x.as_array())
            .map(|a| {
                let strs = |o: &serde_json::Value, k: &str| -> Vec<String> {
                    o.get(k)
                        .and_then(|x| x.as_array())
                        .map(|c| c.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                        .unwrap_or_default()
                };
                a.iter()
                    .map(|o| super::radar::CoinPreset {
                        code: o.get("code").and_then(|x| x.as_str()).unwrap_or_default().into(),
                        label: o.get("label").and_then(|x| x.as_str()).unwrap_or_default().into(),
                        cats: strs(o, "cats"),
                        exclude: strs(o, "exclude"),
                        exclude_base: strs(o, "exclude_base"),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        screener_markets: items("screener_markets"),
        filters: cat("filters")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|o| {
                        let k = o.get("kind")?.as_str()?.to_string();
                        let its = o.get("items")?.as_array()?.iter()
                            .map(|i| FilterItem {
                                field: i.get("field").and_then(|x| x.as_str()).unwrap_or("").into(),
                                label: i.get("label").and_then(|x| x.as_str()).unwrap_or("").into(),
                                kind: i.get("kind").and_then(|x| x.as_str()).unwrap_or("num").into(),
                            })
                            .collect();
                        Some((k, its))
                    })
                    .collect()
            })
            .unwrap_or_default(),
        enums: cat("enums")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .map(|o| EnumItem {
                        field: o.get("field").and_then(|x| x.as_str()).unwrap_or("").into(),
                        label: o.get("label").and_then(|x| x.as_str()).unwrap_or("").into(),
                        op: o.get("op").and_then(|x| x.as_str()).unwrap_or("equal").into(),
                        values: o.get("values").and_then(|x| x.as_array())
                            .map(|vs| vs.iter().filter_map(|v| Some((
                                v.get("id")?.as_str()?.to_string(),
                                v.get("name")?.as_str()?.to_string(),
                            ))).collect())
                            .unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        etf_markets: cat("etf_markets")
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default(),
    };
    let breadth = v
        .get("breadth")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .map(|o| BreadthRow {
                    market: s(o, "market"),
                    n: i64_of(o, "n"),
                    adv: i64_of(o, "adv"),
                    dec: i64_of(o, "dec"),
                    unch: i64_of(o, "unch"),
                    new_high: i64_of(o, "new_high"),
                    new_low: i64_of(o, "new_low"),
                    net_new_high: i64_of(o, "net_new_high"),
                    total: i64_of(o, "total"),
                    coverage: optf_of(o, "coverage"),
                    adv_pct: optf_of(o, "adv_pct"),
                    ad_ratio: optf_of(o, "ad_ratio"),
                    above_ma200_pct: optf_of(o, "above_ma200_pct"),
                })
                .collect()
        })
        .unwrap_or_default();
    let ov_arr = |o: Option<&serde_json::Value>| {
        let mut out = [None; 4];
        if let Some(m) = o {
            for (i, k) in OV_WINDOWS.iter().enumerate() {
                out[i] = m.get(*k).and_then(|x| x.as_f64());
            }
        }
        out
    };
    let overview = v
        .get("overview")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .map(|o| OverviewRow {
                    label: s(o, "label"),
                    ticker: s(o, "ticker"),
                    currency: s(o, "currency"),
                    local: ov_arr(o.get("local")),
                    usd: ov_arr(o.get("usd")),
                })
                .collect()
        })
        .unwrap_or_default();
    let f_of = |o: &serde_json::Value, k: &str| o.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0);
    let arr = |k: &str| v.get(k).and_then(|x| x.as_array()).cloned().unwrap_or_default();
    let sub = |root: &str, k: &str| {
        v.get(root)
            .and_then(|x| x.get(k))
            .and_then(|x| x.as_array())
            .cloned()
            .unwrap_or_default()
    };
    let _ = &arr;
    let coins = |a: Vec<serde_json::Value>| -> Vec<CoinRow> {
        a.iter()
            .map(|o| CoinRow {
                symbol: s(o, "symbol"),
                venue: s(o, "venue"),
                kind: s(o, "kind"),
                price: f_of(o, "price"),
                chg_pct: f_of(o, "chg_pct"),
                vol_usd: optf_of(o, "vol_usd"),
                url: s(o, "url"),
            })
            .collect()
    };
    let panorama = Panorama {
        venues: sub("panorama", "venues")
            .iter()
            .map(|o| VenueRow {
                venue: s(o, "venue"),
                label: s(o, "label"),
                kind: s(o, "kind"),
                pairs: i64_of(o, "pairs"),
                vol_usd: f_of(o, "vol_usd"),
                btc: optf_of(o, "btc"),
                err: s(o, "err"),
            })
            .collect(),
        hot: coins(sub("panorama", "hot")),
        gainers: coins(sub("panorama", "gainers")),
        losers: coins(sub("panorama", "losers")),
        perps: sub("panorama", "perps")
            .iter()
            .map(|o| PerpRow {
                symbol: s(o, "symbol"),
                venue: s(o, "venue"),
                price: f_of(o, "price"),
                chg_pct: f_of(o, "chg_pct"),
                funding: f_of(o, "funding"),
                funding_apr: f_of(o, "funding_apr"),
                oi_usd: optf_of(o, "oi_usd"),
                vol_usd: optf_of(o, "vol_usd"),
                url: s(o, "url"),
            })
            .collect(),
        options: sub("panorama", "options")
            .iter()
            .map(|o| OptionRow {
                venue: s(o, "venue"),
                currency: s(o, "currency"),
                n: i64_of(o, "n"),
                oi_usd: f_of(o, "oi_usd"),
                vol_usd: f_of(o, "vol_usd"),
                vol_notional_usd: f_of(o, "vol_notional_usd"),
                iv: optf_of(o, "iv"),
                underlying: optf_of(o, "underlying"),
                url: s(o, "url"),
            })
            .collect(),
        listings: sub("panorama", "listings")
            .iter()
            .map(|o| ListingRow {
                symbol: s(o, "symbol"),
                venue: s(o, "venue"),
                listed_ms: i64_of(o, "listed_ms"),
                url: s(o, "url"),
            })
            .collect(),
    };
    let preds = |a: Vec<serde_json::Value>| -> Vec<PredRow> {
        a.iter()
            .map(|o| PredRow {
                platform: s(o, "platform"),
                category: s(o, "category"),
                title: s(o, "title"),
                outcome: s(o, "outcome"),
                prob: optf_of(o, "prob"),
                chg_24h: optf_of(o, "chg_24h"),
                vol_usd: f_of(o, "vol_usd"),
                vol_window: s(o, "vol_window"),
                liquidity_usd: optf_of(o, "liquidity_usd"),
                spread: optf_of(o, "spread"),
                close_ms: o.get("close_ms").and_then(|x| x.as_i64()),
                start_ms: o.get("start_ms").and_then(|x| x.as_i64()),
                slug: s(o, "slug"),
                url: s(o, "url"),
            })
            .collect()
    };
    let prediction = Prediction {
        sources: sub("prediction", "sources")
            .iter()
            .map(|o| PredSource {
                platform: s(o, "platform"),
                label: s(o, "label"),
                rows: i64_of(o, "rows"),
                err: s(o, "err"),
            })
            .collect(),
        hot: preds(sub("prediction", "hot")),
        movers: preds(sub("prediction", "movers")),
        fresh: preds(sub("prediction", "fresh")),
    };
    let stocks = |a: Vec<serde_json::Value>| -> Vec<StockRow> {
        a.iter()
            .map(|o| StockRow {
                symbol: s(o, "symbol"),
                name: s(o, "name"),
                price: f_of(o, "price"),
                net_change: f_of(o, "net_change"),
                chg_pct: f_of(o, "chg_pct"),
                volume: f_of(o, "volume"),
                turnover: f_of(o, "turnover"),
                mcap: f_of(o, "mcap"),
                sector: s(o, "sector"),
                industry: s(o, "industry"),
                country: s(o, "country"),
                ipo_year: i64_of(o, "ipo_year"),
                url: s(o, "url"),
            })
            .collect()
    };
    let est = |k: &str| v.get("equity").and_then(|x| x.get("status")).map(|x| s(x, k)).unwrap_or_default();
    let equity = EquityPanorama {
        status: MarketStatus {
            country: est("country"),
            indicator: est("indicator"),
            countdown: est("countdown"),
            previous_trade_date: est("previous_trade_date"),
            next_trade_date: est("next_trade_date"),
        },
        indices: sub("equity", "indices")
            .iter()
            .map(|o| IndexQuote {
                symbol: s(o, "symbol"),
                label: s(o, "label"),
                price: f_of(o, "price"),
                chg: f_of(o, "chg"),
                chg_pct: f_of(o, "chg_pct"),
                open: f_of(o, "open"),
                high: f_of(o, "high"),
                low: f_of(o, "low"),
                prev_close: f_of(o, "prev_close"),
                last_trade: s(o, "last_trade"),
                url: s(o, "url"),
            })
            .collect(),
        universe: v.get("equity").and_then(|x| x.get("universe")).and_then(|x| x.as_i64()).unwrap_or(0),
        hot: stocks(sub("equity", "hot")),
        gainers: stocks(sub("equity", "gainers")),
        losers: stocks(sub("equity", "losers")),
        sectors: sub("equity", "sectors")
            .iter()
            .map(|o| SectorRow {
                sector: s(o, "sector"),
                n: i64_of(o, "n"),
                adv: i64_of(o, "adv"),
                dec: i64_of(o, "dec"),
                wtd_chg: optf_of(o, "wtd_chg"),
                wtd_n: i64_of(o, "wtd_n"),
                median_chg: optf_of(o, "median_chg"),
                turnover: f_of(o, "turnover"),
            })
            .collect(),
        ipos: sub("equity", "ipos")
            .iter()
            .map(|o| IpoRow {
                symbol: s(o, "symbol"),
                company: s(o, "company"),
                exchange: s(o, "exchange"),
                status: s(o, "status"),
                price: s(o, "price"),
                shares: s(o, "shares"),
                value: s(o, "value"),
                date: s(o, "date"),
                url: s(o, "url"),
            })
            .collect(),
        errors: sub("equity", "errors").iter().map(|o| (s(o, "what"), s(o, "err"))).collect(),
        month: v.get("equity").map(|x| s(x, "month")).unwrap_or_default(),
    };
    let macros = MacroBoard {
        rows: sub("macros", "rows")
            .iter()
            .map(|o| MacroRow {
                source: s(o, "source"),
                area: s(o, "area"),
                label: s(o, "label"),
                value: f_of(o, "value"),
                unit: s(o, "unit"),
                obs: s(o, "obs"),
                chg: optf_of(o, "chg"),
                url: s(o, "url"),
            })
            .collect(),
        news: sub("macros", "news")
            .iter()
            .map(|o| NewsRow {
                source: s(o, "source"),
                title: s(o, "title"),
                link: s(o, "link"),
                published: s(o, "published"),
                ts_ms: i64_of(o, "ts_ms"),
            })
            .collect(),
        sources: sub("macros", "sources")
            .iter()
            .map(|o| MacroSource {
                key: s(o, "key"),
                label: s(o, "label"),
                rows: i64_of(o, "rows"),
                err: s(o, "err"),
            })
            .collect(),
    };
    let fe = |k: &str| {
        v.get("fetched").and_then(|x| x.get(k)).and_then(|x| x.as_i64()).unwrap_or(0)
    };
    let fetched = FetchedAt {
        panorama: fe("panorama"),
        prediction: fe("prediction"),
        equity: fe("equity"),
        macros: fe("macros"),
        breadth: fe("breadth"),
        overview: fe("overview"),
    };
    RadarReadout {
        stamp: s(v, "stamp"),
        source: s(v, "source"),
        tier: s(v, "tier"),
        n_symbols: v.get("n_symbols").and_then(|x| x.as_i64()).unwrap_or(0),
        refreshed_ms: v.get("refreshed_ms").and_then(|x| x.as_i64()).unwrap_or(0),
        rows,
        catalog,
        breadth,
        overview,
        panorama,
        prediction,
        equity,
        macros,
        fetched,
        backfill: BackfillView {
            done: bi("done"),
            total: bi("total"),
            failed: bi("failed"),
            running: bb("running"),
            finished: bb("finished"),
        },
        refreshed: String::new(),
        present: true,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn indices_and_markets_share_one_key_shape() {
        // 两者用同一套 code/label/region。多一套键就多一处会漂移的地方——
        // 实测漂过一次：守护改成 code/region 而面板还读 id/market，
        // 指数来源整个消失且不报错
        let j = r#"{"stamp":"s","source":"tv","rows":[],"catalog":{
          "markets":[{"code":"america","label":"美国","region":"美洲"}],
          "indices":[{"code":"america@SPX","label":"标准普尔500指数","region":"america"}]}}"#;
        let r = parse_board(&serde_json::from_str(j).unwrap());
        assert_eq!(r.catalog.indices.len(), 1);
        let ix = &r.catalog.indices[0];
        assert_eq!(ix.code, "america@SPX");
        assert_eq!(ix.region, "america", "region 要能挂到市场的 code 上");
        assert!(r.catalog.markets.iter().any(|m| m.code == ix.region), "挂不上就不会显示");
    }

    #[test]
    fn text_valued_columns_parse_into_their_own_map() {
        // 评级返回 "StrongBuy"、财务期间返回 "2026-Q2"；当数字解析会整列「—」
        let j = r#"{"stamp":"s","source":"tv","rows":[{"symbol":"NVDA","venue":"tv:america:stock",
          "m":{"close":217.4},"t":{"AnalystRating":"StrongBuy","fiscal_period_current":"2026-Q2","x":""}}]}"#;
        let r = parse_board(&serde_json::from_str(j).unwrap());
        let row = &r.rows[0];
        assert_eq!(row.t.get("AnalystRating").map(String::as_str), Some("StrongBuy"));
        assert_eq!(row.t.get("fiscal_period_current").map(String::as_str), Some("2026-Q2"));
        assert!(row.t.get("x").is_none(), "空串等于没有，不该占一格");
        assert_eq!(row.m.get("close").copied(), Some(217.4));
        assert!(row.m.get("AnalystRating").is_none());
    }

    #[test]
    fn a_snapshot_without_the_text_map_still_parses() {
        // 守护可能是旧版本，没有 `t` 段——不能因此整份快照解析失败
        let j = r#"{"stamp":"s","source":"tv","rows":[{"symbol":"A","venue":"v","m":{"close":1.0}}]}"#;
        let r = parse_board(&serde_json::from_str(j).unwrap());
        assert_eq!(r.rows.len(), 1);
        assert!(r.rows[0].t.is_empty());
    }

    #[test]
    fn the_panorama_keeps_the_caveats_attached_to_the_numbers() {
        let j = r#"{"stamp":"s","rows":[],"panorama":{
          "venues":[{"venue":"kraken","label":"Kraken","kind":"spot","pairs":731,
                     "vol_usd":8.2e8,"btc":79637.0,"err":null},
                    {"venue":"okx","label":"欧易","kind":"spot","pairs":0,
                     "vol_usd":0.0,"btc":null,"err":"超时"}],
          "hot":[{"symbol":"BTCUSDT","base":"BTC","venue":"binance","kind":"spot",
                  "price":79643.0,"chg_pct":-0.49,"vol_usd":7.6e8}],
          "perps":[{"symbol":"BTC","venue":"hyperliquid","price":79609.0,"chg_pct":-0.5,
                    "funding":0.0000125,"funding_apr":0.1095,"oi_usd":null,"vol_usd":9.3e8}],
          "options":[{"venue":"deribit","currency":"BTC","n":964,"oi_usd":3.35e10,
                      "vol_usd":3.7e6,"vol_notional_usd":3.27e8,"iv":40.9,"underlying":79632.0}],
          "listings":[{"symbol":"BYDUSDT","venue":"binance-perp","listed_ms":1788746400000}]}}"#;
        let r = parse_board(&serde_json::from_str(j).unwrap());
        let p = &r.panorama;
        // 抓取失败要能看出来。只表现为成交额 0 的话，和「今天没人交易」一模一样
        assert_eq!(p.venues[1].err, "超时");
        assert!(p.venues[0].err.is_empty());
        // 币安永续的未平仓要逐对再请求，这里天生拿不到——`None` 才对，0 是假的
        assert_eq!(p.perps[0].oi_usd, None);
        assert_eq!(p.perps[0].vol_usd, Some(9.3e8));
        // 权利金成交额和名义成交额差两个数量级，两个都要有
        assert!(p.options[0].vol_notional_usd > p.options[0].vol_usd * 50.0);
        assert_eq!(p.listings[0].symbol, "BYDUSDT");
        // `base` 只在守护侧用来剔稳定币互换对，面板不需要
        assert_eq!(p.hot[0].symbol, "BTCUSDT");
    }

    #[test]
    fn prediction_rows_carry_the_window_their_volume_was_measured_over() {
        // Polymarket 是严格 24h、Kalshi 是「近期」。窗口不跟着数字走的话，
        // 这一列在混排下就是在比两个不同的量
        let j = r#"{"stamp":"s","rows":[],"prediction":{
          "sources":[{"platform":"kalshi","label":"Kalshi","rows":50,"err":null}],
          "hot":[{"platform":"kalshi","category":"Sports","title":"谁赢","outcome":"A",
                  "prob":0.81,"chg_24h":0.1,"vol_usd":2.6e7,"vol_window":"近期",
                  "liquidity_usd":null,"spread":0.01,"close_ms":1788746400000,
                  "start_ms":null,"slug":"KX-1"}],
          "movers":[],"fresh":[]}}"#;
        let r = parse_board(&serde_json::from_str(j).unwrap());
        let h = &r.prediction.hot[0];
        assert_eq!(h.vol_window, "近期");
        assert_eq!(h.prob, Some(0.81));
        // 概率**点**，不是收益率
        assert_eq!(h.chg_24h, Some(0.1));
        assert_eq!(h.liquidity_usd, None, "缺就是缺，不能变成 0");
        assert_eq!(r.prediction.sources[0].rows, 50);
    }

    #[test]
    fn the_equity_table_keeps_the_trade_date_and_the_missing_caps_apart() {
        let j = r#"{"stamp":"s","rows":[],"equity":{
          "status":{"country":"U.S.","indicator":"Market Closed",
                    "countdown":"Market Opens in 1D","previous_trade_date":"Sep 3, 2026",
                    "next_trade_date":"Sep 8, 2026"},
          "indices":[{"symbol":"^VIX","label":"VIX 波动率","price":14.53,"chg":0.21,
                      "chg_pct":1.4453,"open":14.08,"high":14.58,"low":13.93,
                      "prev_close":14.53,"last_trade":"2026-09-04T16:15:01"}],
          "universe":7131,
          "hot":[{"symbol":"MU","name":"Micron","price":1016.59,"chg_pct":6.1,
                  "volume":3.5e7,"turnover":3.57e10,"mcap":1.1e12,"sector":"Technology"},
                 {"symbol":"SPY","name":"SPDR","price":700.0,"chg_pct":-0.4,
                  "volume":1e7,"turnover":7e9,"mcap":0.0,"sector":""}],
          "gainers":[],"losers":[],
          "sectors":[{"sector":"Technology","n":784,"adv":378,"dec":372,
                      "wtd_chg":0.44,"wtd_n":781,"median_chg":0.0,"turnover":3.16e11}],
          "ipos":[{"symbol":"TRBG","company":"Turbogen Ltd.","status":"已定价",
                   "date":"9/03/2026","value":""}],
          "errors":[{"what":"新股日历","err":"超时"}]}}"#;
        let r = parse_board(&serde_json::from_str(j).unwrap());
        let e = &r.equity;
        // 全表是收盘价。不带这个日期，休市日的几天前数据和实时价长得一样
        assert_eq!(e.status.previous_trade_date, "Sep 3, 2026");
        assert_eq!(e.universe, 7131);
        // 0 市值是「原表没给」（ETF），不是市值为零——面板据此显示「—」
        assert_eq!(e.hot[1].mcap, 0.0);
        assert_eq!(e.hot[0].turnover, 3.57e10);
        // 加权覆盖了几只要能看见
        assert_eq!((e.sectors[0].wtd_n, e.sectors[0].n), (781, 784));
        assert_eq!(e.errors[0].0, "新股日历");
        assert_eq!(e.ipos[0].status, "已定价");
        // Cboe 的延迟证据
        assert_eq!(e.indices[0].last_trade, "2026-09-04T16:15:01");
    }

    #[test]
    fn every_macro_row_carries_its_own_observation_date() {
        // 同一张表里政策利率是当天、HICP 是九个月前、世行 GDP 是去年
        let j = r#"{"stamp":"s","rows":[],"macros":{
          "rows":[{"source":"ECB","area":"欧元区","label":"主要再融资利率","value":2.4,
                   "unit":"%","obs":"2026-09-07","chg":0.0},
                  {"source":"WorldBank","area":"United States","label":"GDP 增速",
                   "value":2.16,"unit":"%","obs":"2025","chg":null}],
          "news":[{"source":"美联储","title":"Federal Reserve Board announces X",
                   "link":"https://x","published":"Fri, 4 Sep 2026 15:00:00 GMT",
                   "ts_ms":1788534000000}],
          "sources":[{"key":"fred","label":"FRED","rows":0,
                      "err":"未配置 api_key（[panorama] fred_api_key）"},
                     {"key":"ecb","label":"欧洲央行","rows":5,"err":""}]}}"#;
        let r = parse_board(&serde_json::from_str(j).unwrap());
        let m = &r.macros;
        assert_eq!(m.rows[0].obs, "2026-09-07");
        assert_eq!(m.rows[1].obs, "2025", "年频的必须能看出是去年");
        assert_eq!(m.rows[1].chg, None, "年频没有上一期，不能变成 0");
        // 「未配置」要能和抓取失败分开——面板据此用不同颜色
        assert!(m.sources[0].err.starts_with("未配置"));
        assert!(m.sources[1].err.is_empty());
        assert!(m.news[0].ts_ms > 0, "CDATA 包着的时间也要能解析出来");
    }

    #[test]
    fn a_snapshot_from_a_daemon_without_these_sections_still_parses() {
        // 守护可能是旧版本。整份快照不能因为少两段就解析失败
        let j = r#"{"stamp":"s","rows":[]}"#;
        let r = parse_board(&serde_json::from_str(j).unwrap());
        assert!(r.panorama.venues.is_empty() && r.prediction.hot.is_empty());
        assert!(r.equity.hot.is_empty() && r.macros.rows.is_empty());
        assert_eq!(r.equity.universe, 0);
        assert!(r.present);
    }

    #[test]
    fn breadth_carries_its_coverage() {
        // 覆盖率必须到面板：覆盖 28% 与 100% 是两个不同的结论
        // （实测美国前 200 只上涨占比 34%、前 2000 只 49%——相反）
        let v = serde_json::json!({"stamp":"t","rows":[],"breadth":[
            {"market":"america","n":2000,"total":7190,"coverage":0.278,
             "adv":970,"dec":1011,"unch":19,"new_high":5,"new_low":3,"net_new_high":2,
             "adv_pct":0.49,"ad_ratio":0.96,"above_ma200_pct":0.66}]});
        let b = &parse_board(&v).breadth[0];
        assert_eq!((b.n, b.total), (2000, 7190));
        assert!((b.coverage.unwrap() - 0.278).abs() < 1e-9);
        // 旧守护没给这两个字段时不能崩，也不该显示成 0%
        let old = serde_json::json!({"stamp":"t","rows":[],"breadth":[
            {"market":"x","n":10,"adv":5,"dec":5,"unch":0,"new_high":0,"new_low":0,"net_new_high":0}]});
        let o = &parse_board(&old).breadth[0];
        assert_eq!(o.total, 0);
        assert!(o.coverage.is_none(), "缺字段要给 None，面板才会退回只显示样本数");
    }

    #[test]
    fn a_settled_empty_source_is_not_the_same_as_still_fetching() {
        // 面板据此决定「显示进度条」还是「这个来源本来就没有标的」。
        // 不区分的话 0 行的来源会一直转进度条，看着像永远在循环抓取
        let p = super::parse_progress(&serde_json::json!({
            "done": 5, "total": 5, "pending": 0,
            "wanted": [
                {"src": "belgium", "sec": "fund", "rows": 0, "done": true},
                {"src": "spain", "sec": "stock", "rows": 0, "done": false},
                {"src": "italy", "sec": "stock", "rows": 0, "done": true, "err": "429 限流"}
            ]
        }));
        assert!(p.of("belgium").unwrap().done, "抓完了，只是空的");
        assert!(!p.of("spain").unwrap().done, "还在队列里等");
        assert_eq!(p.of("italy").unwrap().err, "429 限流", "抓失败≠抓到 0 行");
        assert!(p.of("france").is_none(), "没点名的来源不该出现");
    }

    #[test]
    fn progress_parses_and_idle_reads_as_complete() {
        // 进度条要能表达三种状态：还没开始 / 抓到一半 / 跑完
        let p = super::parse_progress(&serde_json::json!({
            "done": 3, "total": 28, "cur": "spain/stock", "eta_s": 12, "pending": 1
        }));
        assert_eq!((p.done, p.total, p.pending, p.eta_s), (3, 28, 1, 12));
        assert_eq!(p.cur, "spain/stock");
        assert!((p.frac() - 3.0 / 28.0).abs() < 1e-6);
        // 字段缺失不该 panic，也不该显示成「0%」卡住的样子
        let empty = super::parse_progress(&serde_json::json!({}));
        assert_eq!(empty.frac(), 1.0, "没活干应显示为已完成，不是停在最左");
    }

    #[test]
    fn bumping_the_nonce_changes_the_body() {
        // 「立即获取」靠内容变化触发：守护比对请求文件内容、`write_request`
        // 又对相同内容去重——没有 nonce 的话，选择不变时按钮是个空操作
        let a = super::request_body("america", &["stock"], &[]);
        super::bump_nonce();
        let b = super::request_body("america", &["stock"], &[]);
        assert_ne!(a, b, "nonce 没让请求体变化，按钮就不会触发抓取");
    }

    #[test]
    fn request_body_carries_the_pushdown_filters() {
        use super::super::radar_filter::Wire;
        let f = vec![
            Wire { field: "price_earnings_ttm", op: "eless", right: vec![10.0], text: None , texts: vec![]},
            Wire { field: "sector", op: "equal", right: vec![], text: Some("Finance") , texts: vec![]},
        ];
        let v: serde_json::Value =
            serde_json::from_str(&request_body("america", &["stock"], &f)).unwrap();
        let a = v["filters"].as_array().unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(a[0]["field"], "price_earnings_ttm");
        assert_eq!(a[0]["right"][0], 10.0);
        // 文本条件走 text 字段，不塞进 right——守护按字段名分派算子
        assert_eq!(a[1]["text"], "Finance");
        assert!(a[1].get("right").is_none());
    }

    #[test]
    fn request_body_always_has_a_filters_key() {
        // 缺键的话守护那边 serde 用 default 也能过，但显式写出来才看得出「就是没筛」
        let v: serde_json::Value =
            serde_json::from_str(&request_body("america", &["stock"], &[])).unwrap();
        assert!(v["filters"].as_array().unwrap().is_empty());
    }

    // ── 快照落点（与守护侧 runtime.rs 逐字相同的行为，两边同名单测钉住）──

    #[test]
    fn xdg_runtime_dir_wins() {
        assert_eq!(
            runtime_dir_from(Some("/run/user/1000"), Some("dajy"), Some("/home/dajy")),
            PathBuf::from("/run/user/1000/wealthspring")
        );
    }

    #[test]
    fn empty_env_counts_as_unset() {
        // systemd 里未设的变量常常是空串；当成已设会得到 "/wealthspring"
        let p = runtime_dir_from(Some(""), Some("dajy"), Some("/home/dajy"));
        assert_ne!(p, PathBuf::from("/wealthspring"));
        assert!(p.is_absolute());
    }

    #[test]
    fn falls_back_to_shm_when_xdg_missing() {
        if std::path::Path::new("/dev/shm").is_dir() {
            assert_eq!(
                runtime_dir_from(None, Some("dajy"), Some("/home/dajy")),
                PathBuf::from("/dev/shm/wealthspring-dajy")
            );
            assert_eq!(
                runtime_dir_from(None, None, Some("/home/dajy")),
                PathBuf::from("/dev/shm/wealthspring-ws")
            );
        }
    }

    #[test]
    fn never_returns_a_relative_or_empty_path() {
        // 相对路径会让守护和面板按各自的 cwd 解析到不同文件，
        // 表现是面板一直「等待数据」而守护日志一切正常
        for c in [
            (Some("/run/user/1000"), Some("u"), Some("/home/u")),
            (None, Some("u"), Some("/home/u")),
            (None, None, None),
        ] {
            let p = runtime_dir_from(c.0, c.1, c.2);
            assert!(p.is_absolute() && p.as_os_str().len() > 1, "{p:?}");
        }
    }

    #[test]
    fn default_location_is_not_on_disk() {
        // 这几条测试存在的理由：默认落点必须在 tmpfs 上
        let d = runtime_dir();
        let tmpfs = d.starts_with("/run/user") || d.starts_with("/dev/shm");
        assert!(
            tmpfs || !std::path::Path::new("/dev/shm").is_dir(),
            "有 tmpfs 却把快照默认读写到了 {d:?}"
        );
    }

    #[test]
    fn hot_and_slow_and_request_share_one_directory() {
        // 三个文件必须同目录：慢层路径是从热层派生的，
        // 控制通道分家的话面板改的来源守护看不到
        assert_eq!(board_path().parent(), slow_path().parent());
        assert_eq!(board_path().parent(), request_path().parent());
    }
    use super::*;

    const BOARD: &str = r#"{
      "stamp":"2026-08-30 07:00:00","source":"binance:linear+binance:spot","tier":"A",
      "n_symbols":577,"n_rows":2,"refreshed_ms":1312,
      "backfill":{"done":37,"total":574,"failed":2,"running":true,"finished":false},
      "catalog":{"markets":[{"code":"america","label":"美国","region":"美洲"},
                            {"code":"japan","label":"日本","region":"亚太"}],
                 "types":[{"code":"stock","label":"股票"},{"code":"fund","label":"ETF"}],
                 "tabs":[{"key":"overview","label":"概览","cols":["close","change","volume"]},
                         {"key":"valuation","label":"估值","cols":["price_earnings_ttm","price_book_fq"]}],
                 "units":{"pct":["change","Perf.YTD"],"money":["close","market_cap_basic"]},
                 "indices":[{"region":"america","code":"america@SPX","label":"标准普尔500指数"},
                            {"region":"japan","code":"japan@NI225","label":"日经225指数"}],
                 "crypto_cats":[{"code":"","label":"全部加密货币"},{"code":"defi","label":"DeFi"}]},
      "breadth":[{"market":"japan","n":300,"adv":194,"dec":103,"unch":3,
                  "new_high":1,"new_low":0,"net_new_high":1,"above_ma200":231,"ma200_n":300,
                  "adv_pct":0.653,"ad_ratio":1.883,"above_ma200_pct":0.77},
                 {"market":"x","n":1,"adv":1,"dec":0,"unch":0,"new_high":0,"new_low":0,
                  "net_new_high":0,"above_ma200":0,"ma200_n":0,
                  "adv_pct":1.0,"ad_ratio":null,"above_ma200_pct":null}],
      "overview":[{"market":"india","ticker":"NSE:NIFTY","label":"印度 Nifty 50","currency":"INR",
                   "local":{"日":-0.0039,"YTD":-0.0834},"usd":{"日":-0.0018,"YTD":-0.1394}},
                  {"market":"japan","ticker":"TVC:NI225","label":"日本 日经 225","currency":"JPY",
                   "local":{"日":0.0008},"usd":{}}],
      "rows":[
        {"symbol":"KNCUSDT","venue":"binance:linear","tier":"A","asset":"crypto","cats":["layer-1","defi"],"price":0.42,"quote_vol_24h":5.1e6,
         "ret":{"1m":0.00165,"24h":0.042},"z_ret":{"1m":3.55},
         "z_vol":null,"z_cnt":null,"sigma_ok":true,"z_provisional":false},
        {"symbol":"NVDA","venue":"tv:america","tier":"C","asset":"equity","name":"NVIDIA Corporation",
         "sector":"Electronic Technology","country":"United States","currency":"USD","mcap":5.2e12,
         "price":1.0,"quote_vol_24h":2e6,
         "cats":[],
         "m":{"Perf.YTD":16.3,"gap":-0.6,"relative_volume_10d_calc":1.42,"market_cap_basic":5.2e12},
         "ret":{"1m":0.001},"z_ret":{"1m":1.2},
         "z_vol":4.4,"z_cnt":2.1,"sigma_ok":false,"z_provisional":true}
      ]}"#;

    fn board() -> RadarReadout {
        parse_board(&serde_json::from_str(BOARD).unwrap())
    }

    #[test]
    fn parses_header_and_rows() {
        let r = board();
        assert_eq!(r.tier, "A");
        assert_eq!(r.n_symbols, 577);
        assert_eq!(r.refreshed_ms, 1312);
        assert_eq!(r.rows.len(), 2);
        assert!(r.present);
    }

    #[test]
    fn absent_windows_stay_none_not_zero() {
        let r = board();
        let row = &r.rows[0];
        assert_eq!(row.ret[0], Some(0.00165)); // 1m
        assert!(row.ret[1].is_none(), "5m 缺席必须是 None，不能补 0");
        assert!(row.ret[2].is_none());
        assert_eq!(row.ret[5], Some(0.042)); // 24h
        assert!(row.z_ret[1].is_none());
    }

    #[test]
    fn degradation_flags_round_trip() {
        let r = board();
        assert!(r.rows[0].trustworthy());
        assert!(!r.rows[1].trustworthy(), "provisional 行不可当真");
        assert!(r.rows[1].z_provisional);
        assert!(!r.rows[1].sigma_ok);
    }

    #[test]
    fn headline_z_falls_back_to_1m() {
        let r = board();
        assert_eq!(r.rows[0].headline_z(), Some(3.55));
    }

    #[test]
    fn parses_per_row_tier_and_reference_data() {
        let r = board();
        assert_eq!(r.rows[0].tier, "A", "加密应是 A 档");
        let eq = &r.rows[1];
        assert_eq!(eq.tier, "C", "延迟股票必须标 C，不能被当成实时");
        assert_eq!(eq.asset, "equity");
        assert_eq!(r.rows[0].asset, "crypto");
        assert_eq!(eq.name, "NVIDIA Corporation");
        assert_eq!(eq.sector, "Electronic Technology");
        assert_eq!(eq.country, "United States");
        assert_eq!(eq.currency, "USD");
        assert!((eq.mcap - 5.2e12).abs() < 1.0);
        // 加密行没有这些参考数据，应是空而不是乱填
        assert_eq!(r.rows[0].country, "");
    }

    #[test]
    fn parses_breadth_with_null_ratios() {
        let b = &board().breadth;
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].market, "japan");
        assert_eq!((b[0].adv, b[0].dec, b[0].n), (194, 103, 300));
        assert!((b[0].above_ma200_pct.unwrap() - 0.77).abs() < 1e-9);
        // 分母为 0 的比值必须是 None，面板显示「—」而不是 inf
        assert!(b[1].ad_ratio.is_none());
        assert!(b[1].above_ma200_pct.is_none());
        assert_eq!(b[1].adv_pct, Some(1.0));
    }

    #[test]
    fn parses_overview_and_keeps_usd_absent_when_fx_missing() {
        let o = &board().overview;
        assert_eq!(o.len(), 2);
        let ind = &o[0];
        assert_eq!(ind.currency, "INR");
        assert!((ind.local[0].unwrap() - (-0.0039)).abs() < 1e-9);
        assert!((ind.usd[3].unwrap() - (-0.1394)).abs() < 1e-9);
        assert!(ind.local[1].is_none(), "缺的窗口应为 None");
        // 拿不到汇率时美元口径必须全空，绝不退回本币值
        assert!(o[1].usd.iter().all(Option::is_none));
        assert!(o[1].local[0].is_some());
    }

    #[test]
    fn request_body_carries_only_the_current_selection() {
        // 累积式请求会让守护的请求数随点击单调增长，最后把非官方接口打爆
        let b = request_body("japan", &["stock", "fund"], &[]);
        let v: serde_json::Value = serde_json::from_str(&b).unwrap();
        let s = v["sources"].as_array().unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0]["market"], "japan");
        assert_eq!(s[0]["security_type"], "stock");
        assert_eq!(s[1]["security_type"], "fund");
    }

    #[test]
    fn request_body_carries_index_source_ids() {
        let v: serde_json::Value =
            serde_json::from_str(&request_body("america@SPX", &["stock"], &[])).unwrap();
        assert_eq!(v["sources"][0]["market"], "america@SPX");
    }

    #[test]
    fn empty_selection_requests_nothing() {
        // 选「全部市场」时不该请求任何额外来源——那等于要求守护拉全世界
        let v: serde_json::Value =
            serde_json::from_str(&request_body("", &["stock"], &[])).unwrap();
        assert!(v["sources"].as_array().unwrap().is_empty());
    }

    #[test]
    fn slow_layer_absence_does_not_void_the_hot_layer() {
        // 股票层没开、或慢层还没写第一轮时，加密照常显示
        let hot = parse_board(&serde_json::from_str(
            r#"{"stamp":"t","layer":"hot","rows":[{"symbol":"BTCUSDT","asset":"crypto"}]}"#,
        ).unwrap());
        assert_eq!(hot.rows.len(), 1);
        assert!(hot.breadth.is_empty());
        assert!(hot.slow_stamp.is_empty());
    }

    #[test]
    fn parses_the_source_catalog() {
        // 面板据此建「来源」下拉；漏了的话下拉只剩已加载的那几个市场
        let c = board().catalog;
        assert_eq!(c.markets.len(), 2);
        assert_eq!(c.markets[0].code, "america");
        assert_eq!(c.markets[0].label, "美国");
        assert_eq!(c.markets[0].region, "美洲");
        assert_eq!(c.types.len(), 2);
        assert_eq!(c.crypto_cats[0].code, "", "首项必须是「全部」");
        assert_eq!(c.indices.len(), 2);
        assert_eq!(c.indices[0].code, "america@SPX");
        assert_eq!(c.indices[0].label, "标准普尔500指数");
        assert_eq!(c.indices[0].region, "america", "指数要知道自己属于哪个市场");
        assert_eq!(c.tabs.len(), 2);
        assert_eq!(c.tabs[0].key, "overview");
        assert_eq!(c.tabs[0].cols, vec!["close", "change", "volume"]);
        assert!(c.pct_keys.contains("Perf.YTD"));
        assert!(c.money_keys.contains("market_cap_basic"));
        assert!(!c.pct_keys.contains("market_cap_basic"), "金额不是百分数");
    }

    #[test]
    fn parses_crypto_categories_per_row() {
        let b = board();
        assert_eq!(b.rows[0].cats, vec!["layer-1", "defi"]);
        assert!(b.rows[1].cats.is_empty(), "股票用板块，不该有加密分类");
    }

    #[test]
    fn missing_catalog_is_empty_not_a_parse_failure() {
        // 老版守护写的快照没有 catalog——面板必须照常工作
        let r = parse_board(&serde_json::from_str(r#"{"stamp":"t","rows":[]}"#).unwrap());
        assert!(r.catalog.markets.is_empty());
    }

    #[test]
    fn parses_tv_metric_map() {
        let eq = &board().rows[1];
        assert_eq!(eq.m.get("Perf.YTD"), Some(&16.3));
        assert_eq!(eq.m.get("gap"), Some(&-0.6));
        assert_eq!(eq.m.get("relative_volume_10d_calc"), Some(&1.42));
        assert!(
            eq.m.get("premarket_change").is_none(),
            "没给的指标必须缺席——补 0 会把「没有数据」显示成「值是 0」"
        );
        assert!(board().rows[0].m.is_empty(), "加密行没有 m 字段时应为空表");
    }

    #[test]
    fn parses_backfill_progress() {
        let b = board().backfill;
        assert_eq!((b.done, b.total, b.failed), (37, 574, 2));
        assert!(b.running && !b.finished);
        assert!((b.pct() - 37.0 / 574.0).abs() < 1e-9);
    }

    #[test]
    fn backfill_absent_is_zeroed_not_a_parse_failure() {
        // 老版守护写的快照没有 backfill 字段——面板必须照常工作
        let v: serde_json::Value =
            serde_json::from_str(r#"{"stamp":"t","rows":[]}"#).unwrap();
        let r = parse_board(&v);
        assert_eq!(r.backfill.total, 0);
        assert!(!r.backfill.running);
        assert!((r.backfill.pct() - 0.0).abs() < 1e-12, "total=0 时不该除零");
    }

    #[test]
    fn missing_board_is_not_present() {
        let r = parse_board(&serde_json::json!({}));
        assert_eq!(r.rows.len(), 0);
        assert_eq!(r.n_symbols, 0);
    }
}
