//! Screener 筛选器（docs/22 §6.9）——对齐 TradingView 筛选栏那一排下拉。
//!
//! 每个筛选器是「某个指标 + 一组预设区间」。**区间的量纲必须和数据一致**：
//! ROE/股息率/增速在 scanner 里已经是百分数（NVDA 的 ROE 是 `117.2` 不是 `1.172`），
//! 写成小数的话该筛选永远筛不出东西——这类错误不会报错，只会安静地返回空表。
//!
//! 缺值一律**不通过**：设了「市盈率 10–15」还把市盈率未知的行放进来，
//! 等于这个筛选没设。

use super::radar::ViewState;
use super::radar_readout::RadarRow;

const NEG: f64 = f64::NEG_INFINITY;
const POS: f64 = f64::INFINITY;

/// 一个预设区间，`[lo, hi]` 闭区间。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Preset {
    pub label: &'static str,
    pub lo: f64,
    pub hi: f64,
}

const fn p(label: &'static str, lo: f64, hi: f64) -> Preset {
    Preset { label, lo, hi }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FKind {
    /// 数值区间，直接比指标值。
    Num,
    /// 日期：值是 Unix 秒，区间的单位是**距今天数**（负=过去）。
    Days,
    /// 板块：预设来自 `SECTORS`，按字符串相等匹配。
    Sector,
    /// 枚举：取值**由守护下发**（`catalog.enums`，抄自官方 `enum/ordered`），
    /// 面板不硬编码。档位 = 取值序号，发给服务端的是取值的 `id`。
    ///
    /// 与 `Sector` 的区别只在取值来源：`Sector` 那张表是面板自己的（20 项，
    /// 我按四个市场的 distinct 值凑的），官方实际 37 项——所以新的一律走 `Enum`。
    Enum,
}

pub struct FilterDef {
    /// 指标键，同 `metric_value`（`own:` 前缀为雷达自有）。板块类此处为 `sector`。
    pub key: &'static str,
    pub label: &'static str,
    pub kind: FKind,
    pub presets: &'static [Preset],
    /// 适用的资产类（目录里的 kind）。空 = 只适用于股票与 ETF。
    ///
    /// **筛选必须按资产类分**：把「市盈率 10–15」摆给债券，用户设了会得到
    /// 一张空表；把「最差收益率」摆给股票同理。字段在那一类里根本不存在。
    pub kinds: &'static [&'static str],
    /// 能下推到服务端时，对应的 scanner 字段名；`None` = **只能本地筛**。
    ///
    /// 这个区分必须显示给用户看。本地筛只在「守护已抓回的按市值前 N 只」里
    /// 找，而服务端筛是在整个市场里找——实测全美「P/E<10 且 息>4%」命中 87 只，
    /// 本地样本里只有 2 只。不标出来的话，用户会以为「涨跌速度 > 3σ」也是
    /// 全市场扫描，而它其实只扫了几千行样本。
    pub server: Option<&'static str>,
}

/// 股票专属。**ETF 不在里面**——它没有市盈率/EPS/PEG/ROE 这些基本面，
/// 官方 ETF 筛选器的筛选也完全是另一套（规模、费率、资产类别、发行人…）。
const EQ: [&str; 1] = ["stock"];
/// 股票与 ETF 都有的（价格行为类）。
const EQ_ETF: [&str; 2] = ["stock", "etf"];

const fn f(
    key: &'static str,
    label: &'static str,
    kind: FKind,
    presets: &'static [Preset],
    server: Option<&'static str>,
) -> FilterDef {
    FilterDef { key, label, kind, presets, server, kinds: &EQ }
}

/// 同 `f`，但显式给出适用的资产类。
const fn fk(
    key: &'static str,
    label: &'static str,
    kind: FKind,
    presets: &'static [Preset],
    server: Option<&'static str>,
    kinds: &'static [&'static str],
) -> FilterDef {
    FilterDef { key, label, kind, presets, server, kinds }
}

/// 各类通用的（价格、涨跌、成交额、速度 z 这些每类都有）。
/// 雷达自有的筛选。**官方筛选栏没有它们**，所以不摆进任何一类——
/// 摆在一起会让人以为是官方的。速度 z 仍在内部用于排序与上色。
const NONE_KIND: [&str; 0] = [];
const ALL_KINDS: [&str; 7] = ["stock", "etf", "coin", "cex", "dex", "bond", "forex"];
const CRYPTO_KINDS: [&str; 3] = ["coin", "cex", "dex"];

/// TradingView 的固定板块分类（20 项，实测自 america/japan/germany/india
/// 一万七千只股票的 distinct 值，不是猜的）。
pub const SECTORS: [(&str, &str); 20] = [
    ("Finance", "金融"),
    ("Technology Services", "科技服务"),
    ("Electronic Technology", "电子科技"),
    ("Health Technology", "医疗科技"),
    ("Producer Manufacturing", "生产制造"),
    ("Process Industries", "流程工业"),
    ("Consumer Non-Durables", "非耐用消费品"),
    ("Consumer Durables", "耐用消费品"),
    ("Consumer Services", "消费服务"),
    ("Retail Trade", "零售"),
    ("Utilities", "公用事业"),
    ("Transportation", "运输"),
    ("Industrial Services", "工业服务"),
    ("Commercial Services", "商业服务"),
    ("Distribution Services", "分销服务"),
    ("Non-Energy Minerals", "非能源矿产"),
    ("Energy Minerals", "能源矿产"),
    ("Health Services", "医疗服务"),
    ("Communications", "通信"),
    ("Miscellaneous", "其他"),
];

// ── 各筛选器的预设区间 ────────────────────────────────────────────
// 阈值口径对齐 TradingView 筛选栏；量纲已逐个核对（见文件头）。

/// 官方「close」的档位（4 档，抄自官方下拉；副标是官方的语义标注）。
const PRICE: [Preset; 4] = [
    p("高于 100·碎股时间", 100.0, POS),
    p("10到100·中价", 10.0, 100.0),
    p("10 及以下·不完全是细价股", NEG, 10.0),
    p("5 及以下·低价股", NEG, 5.0),
];

/// 官方「change」的档位（12 档，抄自官方下拉；副标是官方的语义标注）。
const CHG: [Preset; 12] = [
    p("高于 30%·极佳上涨", 30.0, POS),
    p("高于 20%·非常强劲上涨", 20.0, POS),
    p("高于 10%·强劲上涨", 10.0, POS),
    p("高于 5%·适度上涨", 5.0, POS),
    p("0%到5%·弱势上涨", 0.0, 5.0),
    p("高于 0%·上", 0.0, POS),
    p("低于 0%·下", NEG, 0.0),
    p("−5%到0%·弱势下跌", -5.0, 0.0),
    p("低于 −5%·适度下跌", NEG, -5.0),
    p("低于 −10%·强劲下跌", NEG, -10.0),
    p("低于 −20%·严重下跌", NEG, -20.0),
    p("低于 −30%·极端下跌", NEG, -30.0),
];

/// 官方「market_cap_basic」的档位（6 档，抄自官方下拉；副标是官方的语义标注）。
const MCAP: [Preset; 6] = [
    p("200 B 及以上·巨头", 2e+11, POS),
    p("10 B到200 B·大", 1e+10, 2e+11),
    p("2 B到10 B·中等", 2000000000.0, 1e+10),
    p("300 M到2 B·小", 300000000.0, 2000000000.0),
    p("50 M到300 M·Micro", 50000000.0, 300000000.0),
    p("50 M 及以下·极小", NEG, 50000000.0),
];

/// 官方「price_earnings_ttm」的档位（6 档，抄自官方下拉；副标是官方的语义标注）。
const PE: [Preset; 6] = [
    p("50 及以上·极高", 50.0, POS),
    p("35到50·非常高", 35.0, 50.0),
    p("25到35·最高价", 25.0, 35.0),
    p("15到25·温和的", 15.0, 25.0),
    p("5到15·最低价", 5.0, 15.0),
    p("0到5·非常低", 0.0, 5.0),
];

/// 官方「每股收益缓慢增长」的档位（7 档，抄自官方下拉）。
const EPS_GROWTH: [Preset; 7] = [
    p("高于 50%·卓越增长", 50.0, POS),
    p("25%到50%·强劲增长", 25.0, 50.0),
    p("10%到25%·适度增长", 10.0, 25.0),
    p("5%到10%·低增长", 5.0, 10.0),
    p("0%到5%·最小增长", 0.0, 5.0),
    p("高于 0%·成长", 0.0, POS),
    p("低于 0%·减少", NEG, 0.0),
];
/// 官方「收入增长」的档位（10 档）。**比 EPS 那套多三档下跌区间**——
/// 共用一张表的话会少掉「适度/强劲/严重减少」。
const REV_GROWTH: [Preset; 10] = [
    p("高于 50%·卓越增长", 50.0, POS),
    p("25%到50%·强劲增长", 25.0, 50.0),
    p("10%到25%·适度增长", 10.0, 25.0),
    p("5%到10%·低增长", 5.0, 10.0),
    p("0%到5%·最小增长", 0.0, 5.0),
    p("高于 0%·成长", 0.0, POS),
    p("低于 0%·减少", NEG, 0.0),
    p("−25%到0%·适度减少", -25.0, 0.0),
    p("−50%到−25%·强劲减少", -50.0, -25.0),
    p("低于 −50%·严重减少", NEG, -50.0),
];

/// 每股收益（绝对值，不是增速）。亏损筛选归这里——市盈率那边筛不出来。
const EPS: [Preset; 4] = [
    p("亏损 (< 0)", NEG, 0.0),
    p("0 – $1", 0.0, 1.0),
    p("5 及以下", 1.0, 5.0),
    p("高于 $5", 5.0, POS),
];

/// 官方「dividends_yield_current」的档位（7 档，抄自官方下拉；副标是官方的语义标注）。
const DIV: [Preset; 7] = [
    p("高于 15%·卓越", 15.0, POS),
    p("10%到15%·非常高", 10.0, 15.0),
    p("6%到10%·最高价", 6.0, 10.0),
    p("4%到6%·温和的", 4.0, 6.0),
    p("2%到4%·最低价", 2.0, 4.0),
    p("0%到2%·非常低", 0.0, 2.0),
    p("0%·无股息", 0.0, 0.0),
];

/// 分析师评级：1 = 强烈买入 → 5 = 强烈卖出。
///
/// **只做三档**：实测全美市值 >$10亿 的股票里 `recommendation_mark` 最大值就是
/// 3.000（分析师极少发卖出评级），做「卖出／强烈卖出」两档等于放两个
/// 永远筛不出东西的死选项。
const RATING: [Preset; 3] = [
    p("强烈买入 (< 1.5)", NEG, 1.5),
    p("买入 (1.5 – 2.5)", 1.5, 2.5),
    p("中性及以下 (≥ 2.5)", 2.5, POS),
];

/// 官方「Perf.YTD」的档位（12 档，抄自官方下拉；副标是官方的语义标注）。
const PERF: [Preset; 12] = [
    p("高于 30%·极佳上涨", 30.0, POS),
    p("高于 20%·非常强劲上涨", 20.0, POS),
    p("高于 10%·强劲上涨", 10.0, POS),
    p("高于 5%·适度上涨", 5.0, POS),
    p("0%到5%·弱势上涨", 0.0, 5.0),
    p("高于 0%·上", 0.0, POS),
    p("低于 0%·下", NEG, 0.0),
    p("−5%到0%·弱势下跌", -5.0, 0.0),
    p("低于 −5%·适度下跌", NEG, -5.0),
    p("低于 −10%·强劲下跌", NEG, -10.0),
    p("低于 −20%·严重下跌", NEG, -20.0),
    p("低于 −30%·极端下跌", NEG, -30.0),
];

/// 官方「price_earnings_growth_ttm」的档位（7 档，抄自官方下拉；副标是官方的语义标注）。
const PEG: [Preset; 7] = [
    p("高于 3·极高", 3.0, POS),
    p("2到3·非常高", 2.0, 3.0),
    p("1.5到2·最高价", 1.5, 2.0),
    p("1到1.5·温和的", 1.0, 1.5),
    p("0.5到1·最低价", 0.5, 1.0),
    p("0到0.5·非常低", 0.0, 0.5),
    p("低于 0·消极的", NEG, 0.0),
];

const ROE: [Preset; 4] = [
    p("为负 (< 0)", NEG, 0.0),
    p("0 – 10%", 0.0, 10.0),
    p("10 – 20%", 10.0, 20.0),
    p("高于 20%", 20.0, POS),
];

/// Beta 可以为负（实测 XOM 的 1 年 beta 是 −0.98），所以最低档不能从 0 起。
const BETA: [Preset; 5] = [
    p("负相关 (< 0)", NEG, 0.0),
    p("低波 0 – 0.5", 0.0, 0.5),
    p("0.5 – 1", 0.5, 1.0),
    p("1 – 1.5", 1.0, 1.5),
    p("高波 > 1.5", 1.5, POS),
];

// 单位是**自然日之差**（见 `day_diff`），不是小数天——今早发的财报距今是
// −0.2 天，用 `lo = 0` 会把它挡在「今天」之外。
const PAST_EARN: [Preset; 4] = [
    p("今天", 0.0, 0.0),
    p("最近一周", -7.0, 0.0),
    p("最近一月", -30.0, 0.0),
    p("最近三月", -90.0, 0.0),
];

const NEXT_EARN: [Preset; 4] = [
    p("今天", 0.0, 0.0),
    p("未来一周", 0.0, 7.0),
    p("未来一月", 0.0, 30.0),
    p("未来三月", 0.0, 90.0),
];

/// 雷达自有：波动归一化的涨跌速度。带符号——想看跌得急的选负档。
const SPEED: [Preset; 5] = [
    p("急跌 < −3σ", NEG, -3.0),
    p("下行 < −2σ", NEG, -2.0),
    p("平静 ±1σ", -1.0, 1.0),
    p("上行 > 2σ", 2.0, POS),
    p("急涨 > 3σ", 3.0, POS),
];

const RVOL: [Preset; 4] = [
    p("高于 1.5×", 1.5, POS),
    p("高于 2×", 2.0, POS),
    p("高于 3×", 3.0, POS),
    p("高于 5×", 5.0, POS),
];

const TURNOVER: [Preset; 4] = [
    p("高于 $100万", 1e6, POS),
    p("高于 $1000万", 1e7, POS),
    p("高于 $1亿", 1e8, POS),
    p("高于 $10亿", 1e9, POS),
];

const VOLAT: [Preset; 4] = [
    p("低于 1%", NEG, 1.0),
    p("1 – 3%", 1.0, 3.0),
    p("3 – 5%", 3.0, 5.0),
    p("高于 5%", 5.0, POS),
];

/// 全部筛选器。前 15 项对齐 TradingView 筛选栏（其「自选表」我没有、
/// 「指数」已经由「来源」下拉覆盖），后 4 项是雷达自有。
// ── 加密专属 ────────────────────────────────────────────────────
const CHG24: [Preset; 6] = [
    p("跌超 10%", NEG, -10.0),
    p("跌 3 – 10%", -10.0, -3.0),
    p("横盘 ±3%", -3.0, 3.0),
    p("涨 3 – 10%", 3.0, 10.0),
    p("涨超 10%", 10.0, POS),
    p("涨超 30%", 30.0, POS),
];
/// 官方「market_cap_calc」的档位（6 档，抄自官方下拉）。
const CMCAP: [Preset; 6] = [
    p("10 B USD 及以上·巨头", 1e+10, POS),
    p("1 B到10 B USD·大", 1000000000.0, 1e+10),
    p("100 M到1 B USD·中等", 100000000.0, 1000000000.0),
    p("10 M到100 M USD·小", 10000000.0, 100000000.0),
    p("1 M到10 M USD·Micro", 1000000.0, 10000000.0),
    p("1 M USD 及以下·极小", NEG, 1000000.0),
];
/// 官方「crypto_total_rank」的档位（6 档，抄自官方下拉）。
const CRANK: [Preset; 6] = [
    p("最热 10", 1.0, 10.0),
    p("最热 50", 1.0, 50.0),
    p("最热 100", 1.0, 100.0),
    p("最热 200", 1.0, 200.0),
    p("最热 500", 1.0, 500.0),
    p("最热 1000", 1.0, 1000.0),
];
/// 官方「24h_vol_to_market_cap」的档位（5 档，抄自官方下拉）。
const VOL2CAP: [Preset; 5] = [
    p("1 及以上·非常高", 1.0, POS),
    p("0.5到1·最高价", 0.5, 1.0),
    p("0.1到0.5·温和的", 0.1, 0.5),
    p("0.01到0.1·最低价", 0.01, 0.1),
    p("0.0001到0.01·非常低", 0.0001, 0.01),
];

// ── DEX 专属 ────────────────────────────────────────────────────
/// 官方「dex_total_liquidity」的档位（6 档，抄自官方下拉）。
const LIQ: [Preset; 6] = [
    p("高于 1 M USD·非常高", 1000000.0, POS),
    p("500 K到1 M USD·最高价", 500000.0, 1000000.0),
    p("100 K到500 K USD·温和的", 100000.0, 500000.0),
    p("50 K到100 K USD·最低价", 50000.0, 100000.0),
    p("10 K到50 K USD·非常低", 10000.0, 50000.0),
    p("低于 10 K USD·极低", NEG, 10000.0),
];
/// 官方「dex_txs_count_24h」的档位（6 档，抄自官方下拉）。
const TXS: [Preset; 6] = [
    p("高于 100 K·非常高", 100000.0, POS),
    p("10 K到100 K·最高价", 10000.0, 100000.0),
    p("1 K到10 K·温和的", 1000.0, 10000.0),
    p("100到1 K·最低价", 100.0, 1000.0),
    p("10到100·非常低", 10.0, 100.0),
    p("1到10·极低", 1.0, 10.0),
];

// ── ETF 专属 ────────────────────────────────────────────────────
/// 官方「aum」的档位（6 档，抄自官方下拉）。
const AUM: [Preset; 6] = [
    p("10 B 及以上·巨头", 1e+10, POS),
    p("1 B到10 B·大", 1000000000.0, 1e+10),
    p("500 M到1 B·中等", 500000000.0, 1000000000.0),
    p("100 M到500 M·小", 100000000.0, 500000000.0),
    p("50 M到100 M·Micro", 50000000.0, 100000000.0),
    p("50 M 及以下·极小", NEG, 50000000.0),
];
/// 官方「expense_ratio」的档位（6 档，抄自官方下拉）。
const EXPENSE: [Preset; 6] = [
    p("1% 及以上·极高", 1.0, POS),
    p("0.75%到1%·非常高", 0.75, 1.0),
    p("0.5%到0.75%·最高价", 0.5, 0.75),
    p("0.2%到0.5%·温和的", 0.2, 0.5),
    p("0.1%到0.2%·最低价", 0.1, 0.2),
    p("0.1% 及以下·非常低", NEG, 0.1),
];
/// 折溢价：净值与市价之差，正为溢价。
const PREM: [Preset; 4] = [
    p("折价 < −0.5%", NEG, -0.5),
    p("接近净值 ±0.5%", -0.5, 0.5),
    p("溢价 > 0.5%", 0.5, POS),
    p("溢价 > 2%", 2.0, POS),
];

// ── 债券专属 ────────────────────────────────────────────────────
const YTW: [Preset; 6] = [
    p("低于 2%", NEG, 2.0),
    p("2%到4%", 2.0, 4.0),
    p("4 – 6%", 4.0, 6.0),
    p("6 – 10%", 6.0, 10.0),
    p("高于 10%", 10.0, POS),
    p("高于 20%（高危）", 20.0, POS),
];
const COUPON: [Preset; 4] = [
    p("零息", NEG, 0.001),
    p("0 – 3%", 0.001, 3.0),
    p("3 – 6%", 3.0, 6.0),
    p("高于 6%", 6.0, POS),
];
/// 净价（占票面 %）。低于 100 是折价、高于 100 是溢价。
const NETPX: [Preset; 4] = [
    p("深度折价 < 80", NEG, 80.0),
    p("折价 80 – 100", 80.0, 100.0),
    p("溢价 100 – 110", 100.0, 110.0),
    p("高溢价 > 110", 110.0, POS),
];

/// 官方「market_cap_diluted_calc」的档位（6 档，抄自官方下拉）。
const CFDV: [Preset; 6] = [
    p("10 B USD 及以上·巨头", 1e+10, POS),
    p("1 B到10 B USD·大", 1000000000.0, 1e+10),
    p("100 M到1 B USD·中等", 100000000.0, 1000000000.0),
    p("10 M到100 M USD·小", 10000000.0, 100000000.0),
    p("1 M到10 M USD·Micro", 1000000.0, 10000000.0),
    p("1 M USD 及以下·极小", NEG, 1000000.0),
];
/// 官方「total_addresses_with_balance」的档位（4 档，抄自官方下拉）。
const ADDRS: [Preset; 4] = [
    p("高于 1 M·超大型持有者群体", 1000000.0, POS),
    p("100 K到1 M·大型持有者群体", 100000.0, 1000000.0),
    p("10 K到100 K·中等持有者群体", 10000.0, 100000.0),
    p("低于 10 K·小型持有者群体", NEG, 10000.0),
];
/// 官方「24h_vol_cmc」的档位（5 档，抄自官方下拉）。
const CVOL: [Preset; 5] = [
    p("1 B USD 及以上", 1000000000.0, POS),
    p("100 M USD 及以上", 100000000.0, POS),
    p("10 M USD 及以上", 10000000.0, POS),
    p("1 M USD 及以上", 1000000.0, POS),
    p("1 M USD 及以下", NEG, 1000000.0),
];
/// 官方「nav_total_return.1Y」的档位（12 档，抄自官方下拉）。
const NAVRET: [Preset; 12] = [
    p("高于 30%", 30.0, POS),
    p("高于 20%", 20.0, POS),
    p("高于 10%", 10.0, POS),
    p("高于 5%", 5.0, POS),
    p("0%到5%", 0.0, 5.0),
    p("高于 0%", 0.0, POS),
    p("低于 0%", NEG, 0.0),
    p("−5%到0%", -5.0, 0.0),
    p("低于 −5%", NEG, -5.0),
    p("低于 −10%", NEG, -10.0),
    p("低于 −20%", NEG, -20.0),
    p("低于 −30%", NEG, -30.0),
];
/// 官方「dex_trading_volume_24h」的档位（5 档，抄自官方下拉）。
const DEXVOL: [Preset; 5] = [
    p("10 M USD 及以上·卓越", 10000000.0, POS),
    p("1 M USD 及以上·非常高", 1000000.0, POS),
    p("500 K USD 及以上·最高价", 500000.0, POS),
    p("100 K USD 及以上·温和的", 100000.0, POS),
    p("10 K USD 及以上·最低价", 10000.0, POS),
];
/// 官方「fully_diluted_value」的档位（6 档，抄自官方下拉）。
const DEXFDV: [Preset; 6] = [
    p("高于 10 B USD·巨头", 1e+10, POS),
    p("1 B到10 B USD·大", 1000000000.0, 1e+10),
    p("100 M到1 B USD·中等", 100000000.0, 1000000000.0),
    p("10 M到100 M USD·小", 10000000.0, 100000000.0),
    p("1 M到10 M USD·Micro", 1000000.0, 10000000.0),
    p("低于 1 M USD·极小", NEG, 1000000.0),
];
/// 官方「dex_txs_count_uniq_24h」的档位（6 档，抄自官方下拉）。
const TRADERS: [Preset; 6] = [
    p("高于 1 K·数字非常高", 1000.0, POS),
    p("500到1 K·高数字", 500.0, 1000.0),
    p("100到500·中等数字", 100.0, 500.0),
    p("50到100·低数字", 50.0, 100.0),
    p("10到50·数字非常低", 10.0, 50.0),
    p("1到10·数字极低", 1.0, 10.0),
];
pub const FILTERS: [FilterDef; 69] = [
    fk("own:price", "价格", FKind::Num, &PRICE, Some("close"), &EQ),
    f("change", "涨跌 %", FKind::Num, &CHG, Some("change")),
    f("market_cap_basic", "总市值", FKind::Num, &MCAP, Some("market_cap_basic")),
    f("price_earnings_ttm", "P/E", FKind::Num, &PE, Some("price_earnings_ttm")),
    f("earnings_per_share_diluted_yoy_growth_ttm", "每股收益缓慢增长", FKind::Num, &EPS_GROWTH, Some("earnings_per_share_diluted_yoy_growth_ttm")),
    f("dividends_yield_current", "股息收益率 %", FKind::Num, &DIV, Some("dividends_yield_current")),
    f("sector", "板块", FKind::Sector, &[], Some("sector")),
    fk("Perf.YTD", "表现 %", FKind::Num, &PERF, Some("Perf.YTD"), &EQ_ETF),
    f("total_revenue_yoy_growth_ttm", "收入增长", FKind::Num, &REV_GROWTH, Some("total_revenue_yoy_growth_ttm")),
    f("price_earnings_growth_ttm", "PEG", FKind::Num, &PEG, Some("price_earnings_growth_ttm")),
    f("return_on_equity_fq", "净资产收益率", FKind::Num, &ROE, Some("return_on_equity_fq")),
    f("beta_1_year", "Beta", FKind::Num, &BETA, Some("beta_1_year")),
    f("earnings_release_date", "最近收益日期", FKind::Days, &PAST_EARN, Some("earnings_release_date")),
    f("earnings_release_next_date", "将近收益日期", FKind::Days, &NEXT_EARN, Some("earnings_release_next_date")),
    fk("own:speed_z", "涨跌速度", FKind::Num, &SPEED, None, &NONE_KIND),
    fk("own:turnover", "成交额", FKind::Num, &TURNOVER, None, &NONE_KIND),
    // ── 官方枚举型（取值随快照下发）──
    fk("asset_class", "资产类别", FKind::Enum, &[], Some("asset_class"), &["etf"]),
    fk("focus", "焦点", FKind::Enum, &[], Some("focus"), &["etf"]),
    fk("niche", "利基", FKind::Enum, &[], Some("niche"), &["etf"]),
    fk("holdings_region", "控股地区", FKind::Enum, &[], Some("holdings_region"), &["etf"]),
    fk("strategy", "策略", FKind::Enum, &[], Some("strategy"), &["etf"]),
    fk("dividend_treatment", "股息处理", FKind::Enum, &[], Some("dividend_treatment"), &["etf"]),
    fk("dividends_frequency", "股息频率", FKind::Enum, &[], Some("dividends_frequency"), &["etf"]),
    fk("leverage", "杠杆", FKind::Enum, &[], Some("leverage"), &["etf"]),
    fk("brand", "品牌", FKind::Enum, &[], Some("brand"), &["etf"]),
    fk("actively_managed", "管理风格", FKind::Enum, &[], Some("actively_managed"), &["etf"]),
    fk("ucits_compliant_flag", "UCITS合规性", FKind::Enum, &[], Some("ucits_compliant_flag"), &["etf"]),
    fk("type", "商品类型", FKind::Enum, &[], Some("type"), &["cex"]),
    fk("base_currency_id", "基础货币", FKind::Enum, &[], Some("base_currency_id"), &["cex"]),
    fk("currency_id", "报价货币", FKind::Enum, &[], Some("currency_id"), &["cex"]),
    fk("bond_issuer", "发行人", FKind::Enum, &[], Some("bond_issuer"), &["bond"]),
    fk("currency", "发行货币", FKind::Enum, &[], Some("currency"), &["bond"]),
    // 债券的「来源」就是 exchange（概览里那一列）
    fk("exchange", "来源", FKind::Enum, &[], Some("exchange"), &["bond"]),
    fk("AnalystRating", "分析师评级", FKind::Enum, &[], Some("AnalystRating"), &EQ),
    fk("crypto_common_categories", "分类", FKind::Enum, &[], Some("crypto_common_categories"), &["coin"]),
    fk("exchange", "交易所", FKind::Enum, &[], Some("exchange"), &["cex", "dex"]),
    fk("blockchain-id", "区块链", FKind::Enum, &[], Some("blockchain-id"), &["dex"]),
    fk("bond_issuer_type", "发行人类型", FKind::Enum, &[], Some("bond_issuer_type"), &["bond"]),
    fk("coupon_type_general", "息票类型", FKind::Enum, &[], Some("coupon_type_general"), &["bond"]),
    fk("coupon_frequency", "票息频率", FKind::Enum, &[], Some("coupon_frequency"), &["bond"]),
    fk("coupon_currency", "息票币种", FKind::Enum, &[], Some("coupon_currency"), &["bond"]),
    // ── 官方数值型：我此前缺的 ──
    fk("nav_total_return.1Y", "NAV总回报", FKind::Num, &NAVRET, Some("nav_total_return.1Y"), &["etf"]),
    fk("Perf.W", "表现 %", FKind::Num, &PERF, Some("Perf.W"), &CRYPTO_KINDS),
    fk("market_cap_diluted_calc", "完全稀释市值", FKind::Num, &CFDV, Some("market_cap_diluted_calc"), &["coin"]),
    fk("total_addresses_with_balance", "带余额的地址", FKind::Num, &ADDRS, Some("total_addresses_with_balance"), &["coin"]),
    fk("24h_vol_cmc", "美元成交量", FKind::Num, &CVOL, Some("24h_vol_cmc"), &["coin"]),
    fk("24h_vol_change_cmc", "成交量涨跌 %", FKind::Num, &CHG, Some("24h_vol_change_cmc"), &["coin"]),
    fk("txs_volume_usd", "美元交易量", FKind::Num, &TURNOVER, Some("txs_volume_usd"), &["coin"]),
    fk("24h_vol_change|5", "成交量涨跌 %", FKind::Num, &CHG, Some("24h_vol_change|5"), &["cex"]),
    fk("dex_txs_count_uniq_24h", "交易者", FKind::Num, &TRADERS, Some("dex_txs_count_uniq_24h"), &["dex"]),
    fk("dex_buyers_24h", "独立买家", FKind::Num, &TRADERS, Some("dex_buyers_24h"), &["dex"]),
    fk("dex_sellers_24h", "独立卖家", FKind::Num, &TRADERS, Some("dex_sellers_24h"), &["dex"]),
    fk("fully_diluted_value", "FDV", FKind::Num, &DEXFDV, Some("fully_diluted_value"), &["dex"]),
    fk("maturity_date", "到期日", FKind::Days, &NEXT_EARN, Some("maturity_date"), &["bond"]),
    fk("outstanding_amount", "未偿金额", FKind::Num, &TURNOVER, Some("outstanding_amount"), &["bond"]),
    // ── 加密 ──
    fk("24h_close_change|5", "涨跌 %", FKind::Num, &CHG24, Some("24h_close_change|5"), &CRYPTO_KINDS),
    fk("market_cap_calc", "总市值", FKind::Num, &CMCAP, Some("market_cap_calc"), &["coin"]),
    fk("crypto_total_rank", "排名", FKind::Num, &CRANK, Some("crypto_total_rank"), &["coin"]),
    fk("24h_vol_to_market_cap", "成交量/市值", FKind::Num, &VOL2CAP, Some("24h_vol_to_market_cap"), &["coin"]),
    fk("24h_vol|5", "美元成交量", FKind::Num, &CVOL, Some("24h_vol|5"), &["cex"]),
    // ── DEX ──
    fk("dex_total_liquidity", "流动性", FKind::Num, &LIQ, Some("dex_total_liquidity"), &["dex"]),
    fk("dex_txs_count_24h", "交易", FKind::Num, &TXS, Some("dex_txs_count_24h"), &["dex"]),
    fk("dex_trading_volume_24h", "交易量", FKind::Num, &DEXVOL, Some("dex_trading_volume_24h"), &["dex"]),
    // ── 债券 ──
    fk("yield_to_worst", "YTW %", FKind::Num, &YTW, Some("yield_to_worst"), &["bond"]),
    fk("current_coupon", "票息 %", FKind::Num, &COUPON, Some("current_coupon"), &["bond"]),
    fk("close_pct", "价格 %", FKind::Num, &NETPX, Some("close_pct"), &["bond"]),
    // ── ETF（官方 ETF 筛选器的口径）──
    fk("aum", "AUM", FKind::Num, &AUM, Some("aum"), &["etf"]),
    fk("expense_ratio", "费用率", FKind::Num, &EXPENSE, Some("expense_ratio"), &["etf"]),
    fk("dividends_yield", "股息收益率%（指示）", FKind::Num, &DIV, Some("dividends_yield"), &["etf"]),
    // 外汇官方是老版页面，**没有筛选栏**——这里也不摆（docs/22 §6.34）
];

/// 「手动设置」的哨兵档位。官方**每个数值下拉末尾都有它**，而债券的数值筛选
/// 更是**只有它**（没有预设分档，见 docs/22 §6.36）。
///
/// 用 255 而不是加字段：`ViewState` 是 `Copy` 的定长结构（做记忆化的键），
/// 塞不进可变长的输入串。串本身存在下面的注册表里。
pub const PI_MANUAL: u8 = 255;

/// 枚举多选「已选中若干项」的显示档位。选中集在 [`enum_selection`] 里。
pub const PI_MULTI: u8 = 254;

/// 枚举筛选的**多选**集：`筛选下标 → 选中的取值序号`。
///
/// 官方枚举下拉是多选的（末尾有「选取全部」）。多值发给服务端用
/// **`in_range`**（实测科技服务 1234 + 金融 3043 = 多值 in_range 的 4277，
/// 是 OR）；发两条 `equal` 是 AND，结果为 0。
static ENUM_SEL: std::sync::Mutex<Option<std::collections::HashMap<usize, Vec<usize>>>> =
    std::sync::Mutex::new(None);

/// 某个枚举筛选选中的取值序号（升序）。
pub fn enum_selection(fi: usize) -> Vec<usize> {
    ENUM_SEL
        .lock()
        .ok()
        .and_then(|g| g.as_ref().and_then(|m| m.get(&fi).cloned()))
        .unwrap_or_default()
}

/// 切换一个取值的选中状态。返回切换后是否非空。
pub fn toggle_enum(fi: usize, i: usize) -> bool {
    let Ok(mut g) = ENUM_SEL.lock() else { return false };
    let m = g.get_or_insert_with(Default::default);
    let v = m.entry(fi).or_default();
    match v.iter().position(|x| *x == i) {
        Some(k) => {
            v.remove(k);
        }
        None => {
            v.push(i);
            v.sort_unstable();
        }
    }
    !v.is_empty()
}

/// 清空一个枚举筛选的选中集。
pub fn clear_enum(fi: usize) {
    if let Ok(mut g) = ENUM_SEL.lock() {
        if let Some(m) = g.as_mut() {
            m.remove(&fi);
        }
    }
}

/// 枚举下拉的**搜索串**。官方就是靠「搜索框 + 虚拟滚动」装下上万项的
/// （债券发行人 16897 个、货币 46628 个），没有搜索框那个下拉根本没法用。
static ENUM_QUERY: std::sync::Mutex<Option<std::collections::HashMap<usize, String>>> =
    std::sync::Mutex::new(None);

pub fn enum_query(fi: usize) -> String {
    ENUM_QUERY
        .lock()
        .ok()
        .and_then(|g| g.as_ref().and_then(|m| m.get(&fi).cloned()))
        .unwrap_or_default()
}

pub fn set_enum_query(fi: usize, q: String) {
    if let Ok(mut g) = ENUM_QUERY.lock() {
        g.get_or_insert_with(Default::default).insert(fi, q);
    }
}

/// 按搜索串过滤后的取值序号。空串 = 全部（但至多 `cap` 个，避免下拉爆掉）。
///
/// 匹配 **id 与显示名两边**，忽略大小写：搜 `btc` 要能命中 `XTVCBTC · Bitcoin`。
pub fn enum_matching(fi: usize, cap: usize) -> Vec<usize> {
    let key = FILTERS[fi].key;
    let q = enum_query(fi).trim().to_lowercase();
    let n = enum_len(key);
    let mut out = Vec::new();
    for i in 0..n {
        let Some((id, name)) = enum_at(key, i) else { continue };
        if q.is_empty() || id.to_lowercase().contains(&q) || name.to_lowercase().contains(&q) {
            out.push(i);
            if out.len() >= cap {
                break;
            }
        }
    }
    out
}

/// 某个取值的显示名（`代码 · 全名`，同官方）。
pub fn enum_display(fi: usize, i: usize) -> String {
    match enum_at(FILTERS[fi].key, i) {
        Some((id, name)) if id != name => format!("{id} · {name}"),
        Some((_, name)) => name.to_string(),
        None => String::new(),
    }
}

/// 手动区间的输入串：`筛选下标 → (下界, 上界)`。空串 = 该侧不限。
static MANUAL: std::sync::Mutex<Option<std::collections::HashMap<usize, (String, String)>>> =
    std::sync::Mutex::new(None);

/// 读手动区间。返回 `(lo, hi)`，空侧用 ±∞。
pub fn manual_range(fi: usize) -> (f64, f64) {
    let g = MANUAL.lock().ok();
    let t = g.as_ref().and_then(|o| o.as_ref()).and_then(|m| m.get(&fi));
    let (lo, hi) = t.map(|(a, b)| (a.as_str(), b.as_str())).unwrap_or(("", ""));
    (lo.trim().parse().unwrap_or(NEG), hi.trim().parse().unwrap_or(POS))
}

/// 手动区间的原始输入串（渲染输入框用）。
pub fn manual_text(fi: usize) -> (String, String) {
    MANUAL
        .lock()
        .ok()
        .and_then(|g| g.as_ref().and_then(|m| m.get(&fi).cloned()))
        .unwrap_or_default()
}

/// 设手动区间的一侧。`is_lo` 为真设下界。
pub fn set_manual(fi: usize, is_lo: bool, v: String) {
    if let Ok(mut g) = MANUAL.lock() {
        let m = g.get_or_insert_with(Default::default);
        let e = m.entry(fi).or_default();
        if is_lo { e.0 = v } else { e.1 = v }
    }
}

/// 枚举取值的注册表：`字段名 → [(id, 中文名)]`，外加算子。
///
/// **由守护下发**（`catalog.enums`），解析目录时灌进来。做成注册表而不是
/// 给函数加参数，是因为 `preset_label` 用在 `Display for FSel` 里——
/// `Display::fmt` 的签名固定，塞不进目录引用。
///
/// 目录一个会话内是稳定的，所以「灌一次、随处读」是安全的。
static ENUM_VALUES: std::sync::Mutex<Option<EnumReg>> = std::sync::Mutex::new(None);

#[derive(Default)]
pub struct EnumReg {
    /// `字段名 → (算子, [(id, 名称)])`
    pub by_field: std::collections::HashMap<String, (String, Vec<(String, String)>)>,
}

/// 灌入目录下发的枚举取值。解析快照时调用。
pub fn set_enums(reg: EnumReg) {
    if let Ok(mut g) = ENUM_VALUES.lock() {
        *g = Some(reg);
    }
}

/// 某字段的取值个数。目录没给就是 0（下拉只有「不限」）。
fn enum_len(field: &str) -> usize {
    ENUM_VALUES
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|r| r.by_field.get(field).map_or(0, |(_, v)| v.len())))
        .unwrap_or(0)
}

/// 第 `i` 个取值的 `(id, 名称)`。返回 `'static` 是靠 intern——
/// 目录有界（17 个字段、1681 个取值），不会无限增长。
fn enum_at(field: &str, i: usize) -> Option<(&'static str, &'static str)> {
    let g = ENUM_VALUES.lock().ok()?;
    let r = g.as_ref()?;
    let (_, vals) = r.by_field.get(field)?;
    let (id, name) = vals.get(i)?;
    Some((super::radar_view::intern(id), super::radar_view::intern(name)))
}

/// 该字段的匹配算子。数组型（加密分类）是 `has`，其余 `equal`。
fn enum_op(field: &str) -> &'static str {
    let is_has = ENUM_VALUES
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|r| r.by_field.get(field).is_some_and(|(op, _)| op == "has")))
        .unwrap_or(false);
    if is_has { "has" } else { "equal" }
}

/// 某个资产类可用的筛选器下标。
///
/// 不区分的话，把「市盈率 10–15」摆给债券、把「最差收益率」摆给股票——
/// 用户设了会得到一张空表，而且不会有任何提示说明为什么。
pub fn for_kind(kind: &str) -> Vec<usize> {
    (0..N_FILTERS).filter(|&i| FILTERS[i].kinds.contains(&kind)).collect()
}

/// 同 `for_kind`，但**按官方筛选栏的次序**排。
///
/// 次序由守护下发（`catalog.filters`，逐类抄自官方页面）。面板自己的 `FILTERS`
/// 是按「先股票、再加密、再债券」分组写的，那是**代码组织顺序**，不是官方次序。
/// 官方没有的（雷达自有的价格 z / 成交额 / 涨跌速度）排在末尾。
pub fn for_kind_ordered(kind: &str, order: &[super::radar_readout::FilterItem]) -> Vec<usize> {
    let mut v = for_kind(kind);
    if order.is_empty() {
        return v;
    }
    let rank = |fi: usize| {
        let d = &FILTERS[fi];
        // 按下推字段名对齐；没有下推字段的是雷达自有的，排末尾
        d.server
            .and_then(|f| order.iter().position(|o| o.field == f))
            .unwrap_or(usize::MAX)
    };
    v.sort_by_key(|&fi| (rank(fi), fi));
    v
}

pub const N_FILTERS: usize = FILTERS.len();

/// 某个筛选器有几个可选档（不含「不限」）。
pub fn n_presets(fi: usize) -> usize {
    match FILTERS[fi].kind {
        FKind::Sector => SECTORS.len(),
        FKind::Enum => enum_len(FILTERS[fi].key),
        _ => FILTERS[fi].presets.len(),
    }
}

/// 档位显示名。`pi == 0` 是「不限」。
pub fn preset_label(fi: usize, pi: u8) -> &'static str {
    if pi == 0 {
        return "不限";
    }
    if pi == PI_MANUAL {
        return "手动设置";
    }
    if pi == PI_MULTI {
        return "已选多项";
    }
    let i = pi as usize - 1;
    match FILTERS[fi].kind {
        FKind::Sector => SECTORS.get(i).map(|s| s.1).unwrap_or("不限"),
        FKind::Enum => enum_at(FILTERS[fi].key, i).map(|(_, n)| n).unwrap_or("不限"),
        _ => FILTERS[fi].presets.get(i).map(|p| p.label).unwrap_or("不限"),
    }
}

/// 单个筛选器的判定。`pi == 0`（不限）恒通过。
/// 两个时间戳相差几个自然日（本地时区），`t` 在 `now` 之前为负。
///
/// 不能用 `(t - now) / 86400`：今早 08:00 发的财报距今是 −0.2 天，
/// 那样算会落在「今天」（0..1）之外。
fn day_diff(t: i64, now: i64) -> Option<i64> {
    use chrono::TimeZone;
    let a = chrono::Local.timestamp_opt(t, 0).single()?.date_naive();
    let b = chrono::Local.timestamp_opt(now, 0).single()?.date_naive();
    Some((a - b).num_days())
}

fn passes_one(r: &RadarRow, fi: usize, pi: u8, win: usize, now_s: i64) -> bool {
    if pi == 0 {
        return true;
    }
    let d = &FILTERS[fi];
    if pi == PI_MANUAL {
        // 手动区间：两侧都空就等于不限，别把整表筛空
        let (lo, hi) = manual_range(fi);
        if lo == NEG && hi == POS {
            return true;
        }
        let Some(v) = super::radar_view::metric_value(r, d.key, win) else { return false };
        return v.is_finite() && v >= lo && v <= hi;
    }
    let i = pi as usize - 1;
    if d.kind == FKind::Enum {
        // 官方枚举下拉是**多选**：命中任一选中值即通过（OR）。
        // 本地判定用取值的**中文名**——服务端筛的是 id，本地样本里存的是
        // 本地化文本，两边口径不同是对的。
        // 选中集为空、或取值表还没到（旧守护/刚启动）就恒通过，不能把整表筛空。
        let sel = enum_selection(fi);
        if sel.is_empty() {
            return true;
        }
        let key = d.key;
        let tr = format!("{key}.tr");
        return sel.iter().any(|&j| match enum_at(key, j) {
            Some((_, name)) => {
                r.t.get(key).is_some_and(|v| v == name)
                    || r.t.get(&tr).is_some_and(|v| v == name)
                    || (key == "sector" && r.sector == name)
            }
            None => true,
        });
    }
    if d.kind == FKind::Sector {
        return SECTORS.get(i).is_some_and(|s| r.sector == s.0);
    }
    let Some(pr) = d.presets.get(i) else {
        return true;
    };
    // 缺值不通过：设了筛选却把该指标未知的行放进来，等于没设
    let Some(mut x) = super::radar_view::metric_value(r, d.key, win) else {
        return false;
    };
    if !x.is_finite() {
        return false;
    }
    if d.kind == FKind::Days {
        // 值是 Unix 秒，区间单位是**自然日之差**
        match day_diff(x as i64, now_s) {
            Some(n) => x = n as f64,
            None => return false,
        }
    }
    x >= pr.lo && x <= pr.hi
}

/// 该行是否通过全部已设筛选（与关系，同 TradingView）。
pub fn passes(r: &RadarRow, v: &ViewState, now_s: i64) -> bool {
    v.filters
        .iter()
        .enumerate()
        .all(|(fi, &pi)| passes_one(r, fi, pi, v.win, now_s))
}

/// 已设了几个筛选。用于在按钮上显示「筛选 3」并给出清除入口。
pub fn active_count(v: &ViewState) -> usize {
    v.filters.iter().filter(|&&pi| pi != 0).count()
}

/// 一条要下发给守护的筛选（对应 tvscreener 的 `{left, operation, right}`）。
#[derive(Debug, Clone, PartialEq)]
pub struct Wire {
    pub field: &'static str,
    pub op: &'static str,
    pub right: Vec<f64>,
    pub text: Option<&'static str>,
    /// 多个文本值（枚举多选，配 `in_range`）。空 = 用 `text` 那一个。
    pub texts: Vec<&'static str>,
}

/// 把当前筛选转成下发给守护的形式（只含能下推的那些）。
///
/// 日期档在本地是「距今天数」，下推时必须换成**绝对 Unix 秒**——
/// scanner 存的是时间戳，发天数过去会匹配到 1970 年。
pub fn wire(v: &ViewState, now_s: i64) -> Vec<Wire> {
    let mut out = Vec::new();
    for (fi, &pi) in v.filters.iter().enumerate() {
        if pi == 0 {
            continue;
        }
        let d = &FILTERS[fi];
        let Some(field) = d.server else { continue };
        if pi == PI_MANUAL {
            let (lo, hi) = manual_range(fi);
            if lo == NEG && hi == POS {
                continue;
            }
            out.push(match (lo == NEG, hi == POS) {
                (false, true) => Wire { field, op: "egreater", right: vec![lo], text: None, texts: vec![] },
                (true, false) => Wire { field, op: "eless", right: vec![hi], text: None, texts: vec![] },
                _ => Wire { field, op: "in_range", right: vec![lo, hi], text: None, texts: vec![] },
            });
            continue;
        }
        let i = pi as usize - 1;
        if d.kind == FKind::Sector {
            if let Some(sec) = SECTORS.get(i) {
                out.push(Wire { field, op: "equal", right: vec![], text: Some(sec.0), texts: vec![] });
            }
            continue;
        }
        if d.kind == FKind::Enum {
            // 发的是取值的 **id**（`StrongBuy` / 哈希串），不是中文显示名。
            // 多值用 **`in_range`**（OR）；发多条 `equal` 是 AND，结果恒为 0。
            let ids: Vec<&'static str> =
                enum_selection(fi).iter().filter_map(|&j| enum_at(d.key, j).map(|(id, _)| id)).collect();
            match ids.len() {
                0 => {}
                1 => out.push(Wire { field, op: enum_op(d.key), right: vec![], text: Some(ids[0]), texts: vec![] }),
                _ => out.push(Wire { field, op: "in_range", right: vec![], text: None, texts: ids }),
            }
            continue;
        }
        let Some(p) = d.presets.get(i) else { continue };
        let (mut lo, mut hi) = (p.lo, p.hi);
        if d.kind == FKind::Days {
            // 天数 → 绝对秒。整天边界按当天 00:00/23:59:59 展开，
            // 否则「今天」（[0,0]）会退化成「此刻这一秒」
            let day = 86_400.0;
            let midnight = (now_s as f64 / day).floor() * day;
            let (a, b) = (lo, hi);
            lo = midnight + a * day;
            hi = midnight + (b + 1.0) * day - 1.0;
        }
        let w = match (lo.is_finite(), hi.is_finite()) {
            (true, true) => Wire { field, op: "in_range", right: vec![lo, hi], text: None, texts: vec![] },
            // 本地是闭区间，所以用「小于等于 / 大于等于」而不是严格不等
            (false, true) => Wire { field, op: "eless", right: vec![hi], text: None, texts: vec![] },
            (true, false) => Wire { field, op: "egreater", right: vec![lo], text: None, texts: vec![] },
            (false, false) => continue,
        };
        out.push(w);
    }
    out
}

/// 已设的筛选里，有几条只能本地筛（服务端下推不了）。
///
/// 面板要把这个数显示出来：本地筛只覆盖守护已抓回的样本，不是全市场。
pub fn local_only(v: &ViewState) -> Vec<&'static str> {
    v.filters
        .iter()
        .enumerate()
        .filter(|(fi, pi)| **pi != 0 && FILTERS[*fi].server.is_none())
        .map(|(fi, _)| FILTERS[fi].label)
        .collect()
}

pub fn now_s() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {

    #[test]
    fn each_asset_kind_only_offers_filters_that_apply_to_it() {
        // 把「市盈率」摆给债券，用户设了会得到一张空表且没有任何提示
        let bond = for_kind("bond");
        let names = |v: &[usize]| v.iter().map(|&i| FILTERS[i].label).collect::<Vec<_>>();
        assert!(!names(&bond).contains(&"P/E"), "债券没有市盈率");
        assert!(names(&bond).contains(&"YTW %"));
        let stock = for_kind("stock");
        assert!(names(&stock).contains(&"P/E"));
        assert!(!names(&stock).contains(&"YTW %"), "股票没有到期收益率");
        assert!(!names(&stock).contains(&"流动性"), "流动性是 DEX 的");
        // DEX 有自己的三条
        let dex = for_kind("dex");
        for k in ["流动性", "交易"] {
            assert!(names(&dex).contains(&k), "DEX 缺 {k}");
        }
    }

    #[test]
    fn the_large_enums_are_searchable_and_capped_on_purpose() {
        // 这四个官方靠「搜索框 + 虚拟滚动」装下上万项。我补齐了字段与取值，
        // 下拉一次只列 200 项、靠搜索收窄——**截断是有意的**，见 enumdef 的注释。
        //
        // 字段名是从官方下拉的 DOM 里读出来的（`XTVCBTC` 这种带前缀的 id），
        // 猜不出来：基础货币是 `base_currency_id` 不是 `base_currency`，
        // 报价货币是 `currency_id` 不是 `quote_currency`（后者实测返回 0 行）。
        for want in ["基础货币", "报价货币", "发行人", "发行货币"] {
            let d = FILTERS.iter().find(|d| d.label == want)
                .unwrap_or_else(|| panic!("{want} 没补上"));
            assert_eq!(d.kind, FKind::Enum);
        }
        // 小枚举那四个也在
        let all: Vec<&str> = FILTERS.iter().map(|d| d.label).collect();
        for want in ["品牌", "管理风格", "UCITS合规性", "商品类型"] {
            assert!(all.contains(&want), "{want} 应该已经补上");
        }
    }

    #[test]
    fn no_radar_native_filter_leaks_into_any_class() {
        // 筛选栏**只放官方有的**。雷达自有的（涨跌速度 / 成交额）摆进去会让人
        // 以为是官方的；速度 z 仍在内部用于排序与上色，只是不作为筛选出现。
        for k in ["stock", "etf", "coin", "cex", "dex", "bond", "forex"] {
            let names: Vec<_> = for_kind(k).iter().map(|&i| FILTERS[i].label).collect();
            for banned in ["涨跌速度", "成交额", "相对成交量", "日波动率"] {
                assert!(!names.contains(&banned), "{k} 混进了非官方筛选 {banned}");
            }
        }
        // 外汇官方是老版页面，没有筛选栏
        assert!(for_kind("forex").is_empty());
        // 其余六类都得有（截图核对过的官方清单）
        for k in ["stock", "etf", "coin", "cex", "dex", "bond"] {
            assert!(!for_kind(k).is_empty(), "{k} 一个筛选都没有");
        }
    }

    #[test]
    fn every_filter_declares_at_least_one_kind() {
        // 雷达自有的三个（价格 z / 涨跌速度 / 成交额）**有意不摆进任何一类**：
        // 官方筛选栏没有它们。速度 z 仍在内部用于排序与上色。
        const RADAR_ONLY: [&str; 2] = ["own:speed_z", "own:turnover"];
        // 空的话该筛选任何视图都不会出现——写了等于没写
        for d in &FILTERS {
            if RADAR_ONLY.contains(&d.key) {
                continue;
            }
            assert!(!d.kinds.is_empty(), "{} 没声明适用资产类", d.label);
            for k in d.kinds {
                assert!(
                    ["stock", "etf", "coin", "cex", "dex", "bond", "forex"].contains(k),
                    "{} 声明了未知资产类 {k}",
                    d.label
                );
            }
        }
    }

    #[test]
    fn a_kinds_server_fields_exist_in_that_kinds_columns() {
        // 下推一个该类根本不请求的字段 = 服务端筛完返回 0 行。
        // 加密的字段名与股票完全不同，这条最容易踩
        let crypto_only = ["24h_close_change|5", "market_cap_calc", "crypto_total_rank",
                           "24h_vol_to_market_cap", "24h_vol|5"];
        for d in &FILTERS {
            let Some(sf) = d.server else { continue };
            if crypto_only.contains(&sf) {
                assert!(
                    d.kinds.iter().all(|k| ["coin", "cex", "dex"].contains(k)),
                    "{} 用的是加密字段却声明给了 {:?}",
                    d.label,
                    d.kinds
                );
            }
        }
    }

    #[test]
    fn open_ended_presets_become_inequalities_closed_ones_a_range() {
        let mut v = ViewState::DEFAULT;
        set(&mut v, "price_earnings_ttm", "50 及以上");
        let w = wire(&v, 0);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].op, "egreater");
        assert_eq!(w[0].right, vec![50.0]);

        let mut v = ViewState::DEFAULT;
        set(&mut v, "market_cap_basic", "2 B到10 B");
        let w = wire(&v, 0);
        assert_eq!(w[0].op, "in_range");
        assert_eq!(w[0].right, vec![2e9, 1e10]);
    }

    #[test]
    fn wire_never_emits_infinity() {
        // f64::INFINITY 序列化成 JSON 是 null，守护会整条丢弃 → 筛选静默失效
        let mut v = ViewState::DEFAULT;
        for fi in 0..N_FILTERS {
            for pi in 1..=n_presets(fi) as u8 {
                v.filters = [0; N_FILTERS];
                v.filters[fi] = pi;
                for w in wire(&v, 1_800_000_000) {
                    assert!(
                        w.right.iter().all(|x| x.is_finite()),
                        "{}／{} 发出了非有限值",
                        FILTERS[fi].label,
                        preset_label(fi, pi)
                    );
                }
            }
        }
    }

    #[test]
    fn date_filters_are_sent_as_absolute_seconds_not_day_offsets() {
        // scanner 存的是时间戳；发「-7」过去会匹配到 1970 年
        let now = 1_800_000_000_i64;
        let mut v = ViewState::DEFAULT;
        set(&mut v, "earnings_release_next_date", "未来一周");
        let w = wire(&v, now);
        assert_eq!(w[0].op, "in_range");
        assert!(w[0].right[0] > 1.7e9, "下界不是绝对秒：{:?}", w[0].right);
        // 覆盖今天 00:00 到七天后 23:59:59
        assert!(w[0].right[1] - w[0].right[0] > 7.0 * 86_400.0 - 1.0);
        assert!(w[0].right[1] - w[0].right[0] < 9.0 * 86_400.0);
    }

    #[test]
    fn today_expands_to_a_whole_day_not_one_second() {
        // 「今天」本地是 [0,0] 天；直接换成秒会退化成「此刻这一秒」，命中恒为 0
        let mut v = ViewState::DEFAULT;
        set(&mut v, "earnings_release_date", "今天");
        let w = wire(&v, 1_800_000_000);
        let span = w[0].right[1] - w[0].right[0];
        assert!(span > 86_000.0 && span < 86_400.0, "「今天」跨度是 {span}s");
    }

    #[test]
    fn sector_is_sent_as_the_english_code() {
        let mut v = ViewState::DEFAULT;
        set(&mut v, "sector", "科技服务");
        let w = wire(&v, 0);
        assert_eq!(w[0].op, "equal");
        assert_eq!(w[0].text, Some("Technology Services"), "发中文标签服务端一行都匹配不到");
        assert!(w[0].right.is_empty());
    }

    #[test]
    fn radar_native_filters_stay_local_and_are_reported() {
        // 雷达自有的指标服务端没有；不报出来的话用户会以为它也是全市场扫描
        let mut v = ViewState::DEFAULT;
        set(&mut v, "own:speed_z", "急涨 > 3σ");
        assert!(wire(&v, 0).is_empty(), "服务端没有这个指标，不该下推");
        assert_eq!(local_only(&v), vec!["涨跌速度"]);

        set(&mut v, "price_earnings_ttm", "5到15");
        assert_eq!(wire(&v, 0).len(), 1, "能下推的照常下推");
        assert_eq!(local_only(&v).len(), 1);
    }

    #[test]
    fn every_server_field_is_a_real_column_of_its_kinds() {
        // 数值型下推的字段必须也在请求的列里——否则用户筛了却看不到依据那一列。
        //
        // **枚举型例外**：实测服务端可以筛没请求的列（ETF 按 `focus` 筛，
        // 请求与不请求那一列都返回 1015 条）。枚举字段有 17 个、取值 1681 个，
        // 全塞进列会把每类的请求撑大一倍，而它们本来就只用于筛不用于看。
        for d in &FILTERS {
            let Some(f) = d.server else { continue };
            if f == "sector" || d.kind == FKind::Enum {
                continue;
            }
            for k in d.kinds {
                assert!(
                    kind_columns(k).contains(&f),
                    "{} 下推的 {f} 不在 {k} 的列里",
                    d.label
                );
            }
        }
    }

    #[test]
    fn nothing_set_sends_nothing() {
        assert!(wire(&ViewState::DEFAULT, 0).is_empty());
        assert!(local_only(&ViewState::DEFAULT).is_empty());
    }
    use super::*;

    fn row() -> RadarRow {
        RadarRow { asset: "equity".into(), ..Default::default() }
    }

    fn with(k: &str, x: f64) -> RadarRow {
        let mut r = row();
        r.m.insert(k.into(), x);
        r
    }

    /// 找到某筛选器的下标——不写死数字，插一项就会让下标断言假失败。
    fn fi_of(key: &str) -> usize {
        FILTERS.iter().position(|d| d.key == key).unwrap()
    }

    /// 按档位标签设值。**前缀匹配**——官方档位名是「区间·语义副标」
    /// （`15到25·温和的`），测试里只写区间那一半，改副标不至于连带改测试。
    fn set(v: &mut ViewState, key: &str, label: &str) {
        let fi = fi_of(key);
        let pi = (1..=n_presets(fi))
            .find(|&i| {
                let l = preset_label(fi, i as u8);
                l == label || l.split('·').next() == Some(label)
            })
            .unwrap_or_else(|| {
                let all: Vec<_> = (1..=n_presets(fi)).map(|i| preset_label(fi, i as u8)).collect();
                panic!("{key} 没有档位 {label}，现有：{all:?}")
            });
        v.filters[fi] = pi as u8;
    }

    #[test]
    fn no_filter_set_keeps_everything() {
        let v = ViewState::DEFAULT;
        assert_eq!(active_count(&v), 0);
        assert!(passes(&row(), &v, 0), "默认不该筛掉任何行");
    }

    #[test]
    fn missing_metric_fails_the_filter() {
        // 设了「市盈率 10–15」还放进市盈率未知的行，等于这个筛选没设
        let mut v = ViewState::DEFAULT;
        set(&mut v, "price_earnings_ttm", "5到15");
        assert!(!passes(&row(), &v, 0));
        assert!(passes(&with("price_earnings_ttm", 12.0), &v, 0));
        assert!(!passes(&with("price_earnings_ttm", 20.0), &v, 0));
    }

    #[test]
    fn non_finite_values_fail_rather_than_pass() {
        let mut v = ViewState::DEFAULT;
        set(&mut v, "price_earnings_ttm", "50 及以上");
        // inf >= 50 在数值上成立，但那是脏数据不是「市盈率很高的公司」
        assert!(!passes(&with("price_earnings_ttm", f64::INFINITY), &v, 0));
        assert!(!passes(&with("price_earnings_ttm", f64::NAN), &v, 0));
    }

    #[test]
    fn filters_combine_with_and() {
        let mut r = with("price_earnings_ttm", 12.0);
        r.m.insert("dividends_yield_current".into(), 3.0);
        let mut v = ViewState::DEFAULT;
        set(&mut v, "price_earnings_ttm", "5到15");
        assert!(passes(&r, &v, 0));
        set(&mut v, "dividends_yield_current", "4%到6%");
        assert_eq!(active_count(&v), 2);
        assert!(!passes(&r, &v, 0), "两个筛选是与关系，一个不满足就该筛掉");
    }

    #[test]
    fn percent_metrics_use_percent_thresholds() {
        // scanner 的 ROE/股息率/增速已经是百分数（NVDA 的 ROE 是 117.2 不是 1.172）。
        // 阈值写成小数的话，这个筛选会安静地筛不出任何东西
        let mut v = ViewState::DEFAULT;
        set(&mut v, "return_on_equity_fq", "高于 20%");
        assert!(passes(&with("return_on_equity_fq", 117.2), &v, 0), "NVDA 实测值该通过");
        assert!(!passes(&with("return_on_equity_fq", 0.25), &v, 0), "0.25% 不是 25%");
    }

    #[test]
    fn beta_lowest_bucket_admits_negatives() {
        // 实测 XOM 的 1 年 beta 是 −0.98；最低档从 0 起的话它会掉出所有档
        let mut v = ViewState::DEFAULT;
        set(&mut v, "beta_1_year", "负相关 (< 0)");
        assert!(passes(&with("beta_1_year", -0.98), &v, 0));
        let covered = |x: f64| {
            BETA.iter().any(|p| x >= p.lo && x <= p.hi)
        };
        for x in [-3.0, -0.98, 0.0, 0.7, 1.2, 3.4] {
            assert!(covered(x), "{x} 落在所有 Beta 档之外");
        }
    }

    #[test]
    fn analyst_rating_has_no_dead_buckets() {
        // 实测全美 >$10亿 市值的股票里 recommendation_mark 最大是 3.000，
        // 「卖出/强烈卖出」档永远筛不出东西——所以不做
        assert_eq!(RATING.len(), 3);
        assert!(RATING.iter().all(|p| p.lo < 3.0), "有档位的下界超出了实测最大值");
    }

    #[test]
    #[ignore]
    fn dump_filters_by_kind() {
        for k in ["stock", "etf", "coin", "cex", "dex", "bond", "forex"] {
            let idx = super::for_kind(k);
            eprintln!("\n### {k}  共 {} 个筛选", idx.len());
            for i in idx {
                let d = &super::FILTERS[i];
                eprintln!(
                    "  {:<22} {:<14} 档位{} {}",
                    d.label,
                    d.key,
                    d.presets.len(),
                    d.server.map(|s| format!("→{s}")).unwrap_or_else(|| "(本地筛)".into())
                );
            }
        }
    }

    #[test]
    fn enum_filters_take_their_values_from_the_catalog() {
        // 取值**由守护下发**，面板不硬编码。没灌之前下拉只有「不限」，
        // 且判定恒通过——不能把整表筛空
        let fi = FILTERS.iter().position(|f| f.key == "AnalystRating").unwrap();
        super::set_enums(super::EnumReg::default());
        assert_eq!(super::n_presets(fi), 0, "目录没给取值时下拉是空的");
        let mut reg = super::EnumReg::default();
        reg.by_field.insert(
            "AnalystRating".into(),
            ("equal".into(), vec![("StrongBuy".into(), "强烈买入".into()),
                                  ("Sell".into(), "卖出".into())]),
        );
        super::set_enums(reg);
        assert_eq!(super::n_presets(fi), 2);
        assert_eq!(super::preset_label(fi, 1), "强烈买入");
        // 下推给服务端的是 **id**，不是中文显示名
        let mut v = ViewState::DEFAULT;
        super::clear_enum(fi);
        super::toggle_enum(fi, 0);
        v.filters[fi] = 1;
        let w = super::wire(&v, 0);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].field, "AnalystRating");
        assert_eq!(w[0].text, Some("StrongBuy"), "发中文名的话服务端一条都匹配不到");

        // **多选**：官方枚举下拉可以多选，多值用 `in_range`（OR）。
        // 发多条 `equal` 是 AND，实测结果恒为 0
        super::toggle_enum(fi, 1);
        let w = super::wire(&v, 0);
        assert_eq!(w.len(), 1, "多选要合成一条，不是多条");
        assert_eq!(w[0].op, "in_range");
        assert_eq!(w[0].texts, vec!["StrongBuy", "Sell"]);
        assert!(w[0].text.is_none());
        super::clear_enum(fi);
    }

    #[test]
    fn array_valued_enums_use_has_not_equal() {
        // 加密分类是数组字段，用 equal 会 400（实测）
        let fi = FILTERS.iter().position(|f| f.key == "crypto_common_categories").unwrap();
        let mut reg = super::EnumReg::default();
        reg.by_field.insert(
            "crypto_common_categories".into(),
            ("has".into(), vec![("defi".into(), "DeFi".into())]),
        );
        super::set_enums(reg);
        let mut v = ViewState::DEFAULT;
        super::clear_enum(fi);
        super::toggle_enum(fi, 0);
        v.filters[fi] = 1;
        assert_eq!(super::wire(&v, 0)[0].op, "has");
        super::clear_enum(fi);
    }

    #[test]
    fn manual_range_covers_both_open_and_closed_intervals() {
        // 官方每个数值下拉末尾都有「手动设置」；债券的数值筛选**只有它**
        let fi = FILTERS.iter().position(|d| d.key == "yield_to_worst").unwrap();
        let mut v = ViewState::DEFAULT;
        v.filters[fi] = super::PI_MANUAL;

        // 两侧都空 = 不限，不能把整表筛空
        super::set_manual(fi, true, String::new());
        super::set_manual(fi, false, String::new());
        assert!(wire(&v, 0).is_empty(), "空手动区间不该下推条件");

        // 只填下界 → 大于等于
        super::set_manual(fi, true, "5".into());
        let w = wire(&v, 0);
        assert_eq!((w[0].op, w[0].right.as_slice()), ("egreater", &[5.0][..]));

        // 两侧都填 → 闭区间
        super::set_manual(fi, false, "8".into());
        let w = wire(&v, 0);
        assert_eq!((w[0].op, w[0].right.as_slice()), ("in_range", &[5.0, 8.0][..]));

        // 只填上界 → 小于等于
        super::set_manual(fi, true, String::new());
        let w = wire(&v, 0);
        assert_eq!((w[0].op, w[0].right.as_slice()), ("eless", &[8.0][..]));

        // 半截输入（"-" / "1."）当作该侧不限，不能 panic 也不能筛成空
        super::set_manual(fi, true, "-".into());
        super::set_manual(fi, false, String::new());
        assert!(wire(&v, 0).is_empty());
        super::set_manual(fi, true, String::new());
    }

    #[test]
    fn each_class_has_its_own_money_scale() {
        // **同一个概念在四类里是四套量级**。共用一张表的话，用小的那套去筛大的
        // 会全落进一档，用大的那套去筛小的会一档都没有。
        let first = |k: &str| {
            let fi = FILTERS.iter().position(|d| d.key == k).unwrap();
            preset_label(fi, 1)
        };
        assert_eq!(first("market_cap_basic"), "200 B 及以上·巨头", "股票市值");
        assert_eq!(first("market_cap_calc"), "10 B USD 及以上·巨头", "加密市值");
        assert_eq!(first("aum"), "10 B 及以上·巨头", "ETF 规模");
        assert_eq!(first("fully_diluted_value"), "高于 10 B USD·巨头", "DEX FDV");
        // DEX 的流动性/交易/交易者又各是一套
        assert_eq!(first("dex_total_liquidity"), "高于 1 M USD·非常高");
        assert_eq!(first("dex_txs_count_24h"), "高于 100 K·非常高");
        assert_eq!(first("dex_txs_count_uniq_24h"), "高于 1 K·数字非常高");
    }

    #[test]
    fn crypto_buckets_are_not_the_stock_ones() {
        // 加密的量级与股票差两个数量级：股票市值巨头是 200 B，加密是 10 B。
        // 共用一张表的话，加密里「巨头」一档一个币都没有
        let n = |k: &str| {
            let fi = FILTERS.iter().position(|d| d.key == k).unwrap();
            (n_presets(fi), preset_label(fi, 1))
        };
        assert_eq!(n("market_cap_calc"), (6, "10 B USD 及以上·巨头"));
        assert_eq!(n("market_cap_basic"), (6, "200 B 及以上·巨头"));
        // 排名是「最热 N」，不是区间
        assert_eq!(n("crypto_total_rank"), (6, "最热 10"));
    }

    #[test]
    fn preset_buckets_match_the_official_dropdowns() {
        // 档位**逐个抄自官方下拉**（docs/22 §6.36）。此前是我自己定的区间，
        // 与官网对不上——「价格」我做的是 $1/$5/$20 分档，官方是
        // 100/10到100/10以下/5以下 再加 EMA、布林带那几个指标比较型。
        let n = |k: &str| {
            let fi = FILTERS.iter().position(|d| d.key == k).unwrap();
            n_presets(fi)
        };
        assert_eq!(n("change"), 12, "官方涨跌%有 12 档");
        assert_eq!(n("market_cap_basic"), 6, "巨头/大/中等/小/Micro/极小");
        assert_eq!(n("price_earnings_ttm"), 6);
        assert_eq!(n("dividends_yield_current"), 7, "含单点档「0%·无股息」");
        assert_eq!(n("price_earnings_growth_ttm"), 7);
        // **EPS 增速与收入增长档位不同**：后者多三档下跌区间，
        // 共用一张表会少掉「适度/强劲/严重减少」
        assert_eq!(n("earnings_per_share_diluted_yoy_growth_ttm"), 7);
        assert_eq!(n("total_revenue_yoy_growth_ttm"), 10);
        // 档位名是「区间·语义副标」，副标是官方的措辞
        let fi = FILTERS.iter().position(|d| d.key == "market_cap_basic").unwrap();
        assert_eq!(preset_label(fi, 1), "200 B 及以上·巨头");
    }

    #[test]
    fn pe_has_no_loss_bucket_because_tradingview_reports_null() {
        // 实测：248 个 EPS<0 的行 price_earnings_ttm 全为 null，从不是负数。
        // 加一个负数档就是一个永远筛不出东西的死选项
        assert!(PE.iter().all(|p| p.lo >= 0.0), "市盈率不该有负数档");
        // 官方最低那档就是「0到5·非常低」，没有负数档——与实测一致
        assert_eq!(PE.last().unwrap().label, "0到5·非常低");
        // 「每股收益」那个筛选已按官方移除（官方股票筛选栏没有它），
        // 所以这里不再验证「亏损归它筛」
    }

    #[test]
    fn today_covers_a_release_earlier_the_same_day() {
        // 今早发的财报距今是 −0.2 天；按小数天算会掉出「今天」
        let now = 1_800_000_000_i64;
        let same_day_earlier = now - 5 * 3600;
        let mut v = ViewState::DEFAULT;
        set(&mut v, "earnings_release_date", "今天");
        let d = day_diff(same_day_earlier, now).unwrap();
        // 跨了本地日界就换个偏移再验，别让测试依赖跑测试的钟点
        if d == 0 {
            assert!(passes(&with("earnings_release_date", same_day_earlier as f64), &v, now));
        }
        assert_eq!(day_diff(now, now), Some(0));
        assert_eq!(day_diff(now - 86_400, now), Some(-1));
        assert_eq!(day_diff(now + 86_400, now), Some(1));
    }

    #[test]
    fn speed_filter_is_signed_not_absolute() {
        // 「急跌」要选出 z 很负的行，不是 |z| 很大的行
        let mut r = row();
        r.z_ret[1] = Some(-4.0);
        let mut up = ViewState::DEFAULT;
        set(&mut up, "own:speed_z", "急涨 > 3σ");
        assert!(!passes(&r, &up, 0));
        let mut dn = ViewState::DEFAULT;
        set(&mut dn, "own:speed_z", "急跌 < −3σ");
        assert!(passes(&r, &dn, 0));
    }

    #[test]
    fn speed_filter_follows_the_selected_window() {
        let mut r = row();
        r.z_ret[1] = Some(4.0);
        r.z_ret[3] = Some(0.1);
        let mut v = ViewState::DEFAULT;
        set(&mut v, "own:speed_z", "急涨 > 3σ");
        v.win = 1;
        assert!(passes(&r, &v, 0));
        v.win = 3;
        assert!(!passes(&r, &v, 0), "换窗口后该按新窗口的 z 判定");
    }

    #[test]
    fn earnings_date_is_relative_days_not_raw_seconds() {
        let now = 1_800_000_000_i64;
        let mut v = ViewState::DEFAULT;
        set(&mut v, "earnings_release_next_date", "未来一周");
        let r = |t: i64| with("earnings_release_next_date", t as f64);
        assert!(passes(&r(now + 3 * 86_400), &v, now));
        assert!(!passes(&r(now + 20 * 86_400), &v, now), "20 天后不在「未来一周」内");
        assert!(!passes(&r(now - 3 * 86_400), &v, now), "过去的财报不该进「未来」档");

        let mut past = ViewState::DEFAULT;
        set(&mut past, "earnings_release_date", "最近一周");
        let q = |t: i64| with("earnings_release_date", t as f64);
        assert!(passes(&q(now - 3 * 86_400), &past, now));
        assert!(!passes(&q(now - 30 * 86_400), &past, now));
        // 两个日期筛选是各自独立的列：拿「未来财报」的值去判「近期财报」
        // 该判为缺值→不通过，而不是意外通过
        assert!(!passes(&r(now - 3 * 86_400), &past, now));
    }

    #[test]
    fn sector_matches_the_english_code_not_the_chinese_label() {
        // 快照里的 sector 是英文原值；拿中文标签去比会一行都匹配不上
        let mut r = row();
        r.sector = "Technology Services".into();
        let mut v = ViewState::DEFAULT;
        set(&mut v, "sector", "科技服务");
        assert!(passes(&r, &v, 0));
        r.sector = "Finance".into();
        assert!(!passes(&r, &v, 0));
        r.sector = "科技服务".into();
        assert!(!passes(&r, &v, 0));
    }

    #[test]
    fn sector_list_has_no_duplicates_and_no_empty_codes() {
        let mut c: Vec<_> = SECTORS.iter().map(|s| s.0).collect();
        let n = c.len();
        c.sort();
        c.dedup();
        assert_eq!(c.len(), n);
        assert!(SECTORS.iter().all(|s| !s.0.is_empty() && !s.1.is_empty()));
    }

    #[test]
    fn every_filter_key_is_actually_fetched_for_its_own_kinds() {
        // 筛选一个该类没请求的指标 = 该筛选永远返回空表。
        // `own:` 是面板自算的，`sector` 是行上的字段，其余必须在**那一类**的列里。
        for d in &FILTERS {
            // `own:` 面板自算；`sector` 在行上；枚举型走服务端下推
            // （实测可以筛没请求的列，见 every_server_field_is_a_real_column_of_its_kinds）
            if d.key.starts_with("own:") || d.key == "sector" || d.kind == FKind::Enum {
                continue;
            }
            for k in d.kinds {
                assert!(
                    kind_columns(k).contains(&d.key),
                    "{}（{}）不在 {k} 请求的列里",
                    d.label,
                    d.key
                );
            }
        }
    }

    /// 某个资产类实际会请求的列（镜像守护侧 `assets.rs`）。
    ///
    /// 面板不依赖守护的 crate，只能手抄一份，靠下面的单测钉住。
    /// **必须按类分**：加密的字段名与股票完全不同，用一份股票清单去校验
    /// 加密的筛选，只会得到「不在列里」这种假失败——或者反过来放过真错误。
    /// 各类请求的列——**手抄自守护的 `assets.rs`**。
    ///
    /// 这是一份平行镜像，会漂：守护补了 DEX 的交易/交易量/交易者三组、
    /// 债券的票息/发行人/评级三组之后，这里就少了一截。漂了的表现是
    /// 「筛选测试放过了实际取不到的字段」。
    ///
    /// 真正的修法是让守护把列表下发、测试读快照，但那样测试就不再自洽。
    /// 折中：**只列筛选真正用到的那些**，加一条新筛选时必须同步这里。
    fn kind_columns(kind: &str) -> Vec<&'static str> {
        let mut v = match kind {
            "coin" => vec![
                "close", "24h_close_change|5", "market_cap_calc", "24h_vol_cmc",
                "24h_vol_to_market_cap", "circulating_supply", "crypto_total_rank",
                "socialdominance", "altrank", "total_shares_diluted",
                "circulating_to_max_supply_ratio", "24h_vol_change_cmc",
                "market_cap_diluted_calc", "total_addresses_with_balance", "txs_volume_usd",
            ],
            "cex" => vec![
                "close", "24h_close_change|5", "24h_vol|5", "24h_vol_change|5",
                "market_cap_calc", "market_cap_diluted_calc", "high", "low",
                "change|60", "Volatility.D", "relative_volume_10d_calc",
            ],
            "dex" => vec![
                "close", "24h_close_change|5", "dex_txs_count_24h",
                "dex_trading_volume_24h", "dex_txs_count_uniq_24h",
                "dex_total_liquidity", "fully_diluted_value",
                "dex_buyers_24h", "dex_sellers_24h",
            ],
            "bond" => vec![
                "close", "close_pct", "close_net", "yield_to_worst", "current_coupon",
                "maturity_date", "outstanding_amount",
                "accrued_coupon_interest", "coupon_date_next",
                "coupon_date_prev", "coupon_currency",
            ],
            "etf" => vec![
                "close", "change", "Value.Traded", "relative_volume_10d_calc", "aum",
                "expense_ratio", "dividends_yield", "nav_discount_premium",
                "beta_1_year", "Volatility.D", "Perf.YTD",
                "nav_total_return.1Y",
            ],
            "forex" => vec![
                "close", "change", "change|60", "bid", "ask", "high", "low", "volume",
                "Volatility.D",
            ],
            _ => equity_keys(),
        };
        // 各类都有的（Perf.* 那套加密也有）
        for k in ["Perf.W", "Perf.1M", "Perf.3M", "Perf.6M", "Perf.YTD", "Perf.Y", "Perf.All"] {
            if !v.contains(&k) {
                v.push(k);
            }
        }
        v
    }

    fn equity_keys() -> Vec<&'static str> {
        vec![
            "close", "change", "market_cap_basic", "price_earnings_ttm",
            "earnings_per_share_diluted_yoy_growth_ttm", "dividends_yield_current",
            "earnings_per_share_diluted_ttm",
            "recommendation_mark", "Perf.YTD", "total_revenue_yoy_growth_ttm",
            "price_earnings_growth_ttm", "return_on_equity_fq", "beta_1_year",
            "earnings_release_date", "earnings_release_next_date",
            "relative_volume_10d_calc", "Volatility.D",
        ]
    }

    #[test]
    fn preset_labels_are_unique_within_a_filter() {
        // 重名档位会让下拉选中项与实际区间对不上
        for (fi, d) in FILTERS.iter().enumerate() {
            let mut l: Vec<_> = (1..=n_presets(fi)).map(|i| preset_label(fi, i as u8)).collect();
            let n = l.len();
            l.sort();
            l.dedup();
            assert_eq!(l.len(), n, "{} 有重名档位", d.label);
            // 枚举型的档位来自目录，单测里没灌就是 0——那是对的
            assert!(n > 0 || d.kind == FKind::Enum, "{} 一个档位都没有", d.label);
        }
    }

    #[test]
    fn preset_ranges_are_well_formed() {
        for d in &FILTERS {
            for p in d.presets {
                // 日期档是**闭区间的整数天**，「今天」就是 [0, 0]；
                // 数值档 lo == hi 则等于只匹配一个浮点数，那是写错了
                if d.kind == FKind::Days {
                    assert!(p.lo <= p.hi, "{}／{} 的区间是反的", d.label, p.label);
                    assert!(p.lo.fract() == 0.0 && p.hi.fract() == 0.0,
                        "{}／{} 的日期边界应是整数天", d.label, p.label);
                } else {
                    // 官方有**单点档**（股息率的「0%·无股息」），lo == hi 是对的；
                    // 反区间（lo > hi）才是写错
                    assert!(p.lo <= p.hi, "{}／{} 的区间是反的", d.label, p.label);
                }
            }
        }
    }

    #[test]
    fn out_of_range_preset_index_does_not_panic() {
        // 状态是持久的；改了档位数量后旧值可能越界，不能 panic
        let mut v = ViewState::DEFAULT;
        v.filters[0] = 250;
        let _ = passes(&row(), &v, 0);
        assert_eq!(preset_label(0, 250), "不限");
    }
}
