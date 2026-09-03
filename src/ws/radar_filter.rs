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

const EQ: [&str; 2] = ["stock", "etf"];

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

const PRICE: [Preset; 6] = [
    p("低于 $1", NEG, 1.0),
    p("$1 – $5", 1.0, 5.0),
    p("$5 – $20", 5.0, 20.0),
    p("$20 – $50", 20.0, 50.0),
    p("$50 – $100", 50.0, 100.0),
    p("高于 $100", 100.0, POS),
];

const CHG: [Preset; 6] = [
    p("跌超 5%", NEG, -5.0),
    p("跌 1 – 5%", -5.0, -1.0),
    p("横盘 ±1%", -1.0, 1.0),
    p("涨 1 – 5%", 1.0, 5.0),
    p("涨超 5%", 5.0, POS),
    p("涨超 10%", 10.0, POS),
];

const MCAP: [Preset; 5] = [
    p("微盘 < $3亿", NEG, 3e8),
    p("小盘 $3亿 – $20亿", 3e8, 2e9),
    p("中盘 $20亿 – $100亿", 2e9, 1e10),
    p("大盘 $100亿 – $2000亿", 1e10, 2e11),
    p("巨盘 > $2000亿", 2e11, POS),
];

/// **没有「亏损」档**：TradingView 对亏损公司的 `price_earnings_ttm` 直接给
/// `null` 而不是负数（实测 248 个 EPS<0 的行市盈率全为空），做一个负数档
/// 等于放一个永远筛不出东西的死选项。想筛亏损公司用下面的「每股收益」。
const PE: [Preset; 5] = [
    p("0 – 10", 0.0, 10.0),
    p("10 – 15", 10.0, 15.0),
    p("15 – 25", 15.0, 25.0),
    p("25 – 50", 25.0, 50.0),
    p("高于 50", 50.0, POS),
];

/// 增速类共用（EPS 增速 / 营收增速），单位百分数。
const GROWTH: [Preset; 5] = [
    p("负增长", NEG, 0.0),
    p("0 – 10%", 0.0, 10.0),
    p("10 – 25%", 10.0, 25.0),
    p("25 – 50%", 25.0, 50.0),
    p("高于 50%", 50.0, POS),
];

/// 每股收益（绝对值，不是增速）。亏损筛选归这里——市盈率那边筛不出来。
const EPS: [Preset; 4] = [
    p("亏损 (< 0)", NEG, 0.0),
    p("0 – $1", 0.0, 1.0),
    p("$1 – $5", 1.0, 5.0),
    p("高于 $5", 5.0, POS),
];

const DIV: [Preset; 4] = [
    p("有派息 (> 0)", 1e-9, POS),
    p("高于 2%", 2.0, POS),
    p("高于 4%", 4.0, POS),
    p("高于 6%", 6.0, POS),
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

const PERF: [Preset; 5] = [
    p("跌超 20%", NEG, -20.0),
    p("跌 0 – 20%", -20.0, 0.0),
    p("涨 0 – 20%", 0.0, 20.0),
    p("涨 20 – 50%", 20.0, 50.0),
    p("涨超 50%", 50.0, POS),
];

const PEG: [Preset; 4] = [
    p("负 (< 0)", NEG, 0.0),
    p("低估 0 – 1", 0.0, 1.0),
    p("1 – 2", 1.0, 2.0),
    p("高于 2", 2.0, POS),
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
const CMCAP: [Preset; 5] = [
    p("微型 < $1000万", NEG, 1e7),
    p("$1000万 – $1亿", 1e7, 1e8),
    p("$1亿 – $10亿", 1e8, 1e9),
    p("$10亿 – $100亿", 1e9, 1e10),
    p("巨型 > $100亿", 1e10, POS),
];
const CRANK: [Preset; 4] = [
    p("前 100", 1.0, 100.0),
    p("前 500", 1.0, 500.0),
    p("前 1000", 1.0, 1000.0),
    p("1000 名以后", 1000.0, POS),
];
/// 量/市值：换手强度。>1 说明一天的成交额超过整个市值——多半是异动。
const VOL2CAP: [Preset; 4] = [
    p("低于 0.05", NEG, 0.05),
    p("0.05 – 0.2", 0.05, 0.2),
    p("0.2 – 1", 0.2, 1.0),
    p("高于 1（异动）", 1.0, POS),
];

// ── DEX 专属 ────────────────────────────────────────────────────
const LIQ: [Preset; 4] = [
    p("高于 $1万", 1e4, POS),
    p("高于 $10万", 1e5, POS),
    p("高于 $100万", 1e6, POS),
    p("高于 $1000万", 1e7, POS),
];
const TXS: [Preset; 4] = [
    p("高于 100", 100.0, POS),
    p("高于 1000", 1000.0, POS),
    p("高于 1万", 1e4, POS),
    p("高于 10万", 1e5, POS),
];

// ── 债券专属 ────────────────────────────────────────────────────
const YTW: [Preset; 6] = [
    p("低于 2%", NEG, 2.0),
    p("2 – 4%", 2.0, 4.0),
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

pub const FILTERS: [FilterDef; 33] = [
    fk("own:price", "价格", FKind::Num, &PRICE, Some("close"), &ALL_KINDS),
    f("change", "涨跌 %", FKind::Num, &CHG, Some("change")),
    f("market_cap_basic", "市值", FKind::Num, &MCAP, Some("market_cap_basic")),
    f("price_earnings_ttm", "市盈率", FKind::Num, &PE, Some("price_earnings_ttm")),
    f("earnings_per_share_diluted_ttm", "每股收益", FKind::Num, &EPS, Some("earnings_per_share_diluted_ttm")),
    f("earnings_per_share_diluted_yoy_growth_ttm", "EPS 增速", FKind::Num, &GROWTH, Some("earnings_per_share_diluted_yoy_growth_ttm")),
    f("dividends_yield_current", "股息率", FKind::Num, &DIV, Some("dividends_yield_current")),
    f("sector", "板块", FKind::Sector, &[], Some("sector")),
    f("recommendation_mark", "分析师评级", FKind::Num, &RATING, Some("recommendation_mark")),
    f("Perf.YTD", "表现 YTD", FKind::Num, &PERF, Some("Perf.YTD")),
    f("total_revenue_yoy_growth_ttm", "营收增速", FKind::Num, &GROWTH, Some("total_revenue_yoy_growth_ttm")),
    f("price_earnings_growth_ttm", "PEG", FKind::Num, &PEG, Some("price_earnings_growth_ttm")),
    f("return_on_equity_fq", "ROE", FKind::Num, &ROE, Some("return_on_equity_fq")),
    f("beta_1_year", "Beta", FKind::Num, &BETA, Some("beta_1_year")),
    f("earnings_release_date", "近期财报", FKind::Days, &PAST_EARN, Some("earnings_release_date")),
    f("earnings_release_next_date", "未来财报", FKind::Days, &NEXT_EARN, Some("earnings_release_next_date")),
    fk("own:speed_z", "涨跌速度", FKind::Num, &SPEED, None, &ALL_KINDS),
    f("relative_volume_10d_calc", "相对成交量", FKind::Num, &RVOL, Some("relative_volume_10d_calc")),
    fk("own:turnover", "成交额", FKind::Num, &TURNOVER, None, &ALL_KINDS),
    f("Volatility.D", "日波动率", FKind::Num, &VOLAT, Some("Volatility.D")),
    // ── 加密 ──
    fk("24h_close_change|5", "涨跌 24h", FKind::Num, &CHG24, Some("24h_close_change|5"), &CRYPTO_KINDS),
    fk("market_cap_calc", "市值", FKind::Num, &CMCAP, Some("market_cap_calc"), &["coin", "cex"]),
    fk("crypto_total_rank", "排名", FKind::Num, &CRANK, Some("crypto_total_rank"), &["coin"]),
    fk("24h_vol_to_market_cap", "量/市值", FKind::Num, &VOL2CAP, Some("24h_vol_to_market_cap"), &["coin"]),
    fk("24h_vol|5", "24h 成交额", FKind::Num, &TURNOVER, Some("24h_vol|5"), &["cex"]),
    // ── DEX ──
    fk("dex_total_liquidity", "流动性", FKind::Num, &LIQ, Some("dex_total_liquidity"), &["dex"]),
    fk("dex_txs_count_24h", "24h 笔数", FKind::Num, &TXS, Some("dex_txs_count_24h"), &["dex"]),
    fk("dex_trading_volume_24h", "24h 成交额", FKind::Num, &TURNOVER, Some("dex_trading_volume_24h"), &["dex"]),
    // ── 债券 ──
    fk("yield_to_worst", "最差收益率", FKind::Num, &YTW, Some("yield_to_worst"), &["bond"]),
    fk("current_coupon", "当期票息", FKind::Num, &COUPON, Some("current_coupon"), &["bond"]),
    fk("close_pct", "净价", FKind::Num, &NETPX, Some("close_pct"), &["bond"]),
    // ── 外汇 ──
    fk("change", "涨跌 %", FKind::Num, &CHG, Some("change"), &["forex"]),
    fk("Volatility.D", "日波动率", FKind::Num, &VOLAT, Some("Volatility.D"), &["forex"]),
];

/// 某个资产类可用的筛选器下标。
///
/// 不区分的话，把「市盈率 10–15」摆给债券、把「最差收益率」摆给股票——
/// 用户设了会得到一张空表，而且不会有任何提示说明为什么。
pub fn for_kind(kind: &str) -> Vec<usize> {
    (0..N_FILTERS).filter(|&i| FILTERS[i].kinds.contains(&kind)).collect()
}

pub const N_FILTERS: usize = FILTERS.len();

/// 某个筛选器有几个可选档（不含「不限」）。
pub fn n_presets(fi: usize) -> usize {
    match FILTERS[fi].kind {
        FKind::Sector => SECTORS.len(),
        _ => FILTERS[fi].presets.len(),
    }
}

/// 档位显示名。`pi == 0` 是「不限」。
pub fn preset_label(fi: usize, pi: u8) -> &'static str {
    if pi == 0 {
        return "不限";
    }
    let i = pi as usize - 1;
    match FILTERS[fi].kind {
        FKind::Sector => SECTORS.get(i).map(|s| s.1).unwrap_or("不限"),
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
    let i = pi as usize - 1;
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
        let i = pi as usize - 1;
        if d.kind == FKind::Sector {
            if let Some(sec) = SECTORS.get(i) {
                out.push(Wire { field, op: "equal", right: vec![], text: Some(sec.0) });
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
            (true, true) => Wire { field, op: "in_range", right: vec![lo, hi], text: None },
            // 本地是闭区间，所以用「小于等于 / 大于等于」而不是严格不等
            (false, true) => Wire { field, op: "eless", right: vec![hi], text: None },
            (true, false) => Wire { field, op: "egreater", right: vec![lo], text: None },
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
        assert!(!names(&bond).contains(&"市盈率"), "债券没有市盈率");
        assert!(names(&bond).contains(&"最差收益率"));
        let stock = for_kind("stock");
        assert!(names(&stock).contains(&"市盈率"));
        assert!(!names(&stock).contains(&"最差收益率"), "股票没有到期收益率");
        assert!(!names(&stock).contains(&"流动性"), "流动性是 DEX 的");
        // DEX 有自己的三条
        let dex = for_kind("dex");
        for k in ["流动性", "24h 笔数"] {
            assert!(names(&dex).contains(&k), "DEX 缺 {k}");
        }
    }

    #[test]
    fn universal_filters_show_up_for_every_kind() {
        // 价格/速度 z/成交额每类都有，缺了就等于那一类没法按体量筛
        for k in ["stock", "etf", "coin", "cex", "dex", "bond", "forex"] {
            let names: Vec<_> = for_kind(k).iter().map(|&i| FILTERS[i].label).collect();
            assert!(!names.is_empty(), "{k} 一个筛选都没有");
            for want in ["价格", "涨跌速度", "成交额"] {
                assert!(names.contains(&want), "{k} 缺通用筛选 {want}");
            }
        }
    }

    #[test]
    fn every_filter_declares_at_least_one_kind() {
        // 空的话该筛选任何视图都不会出现——写了等于没写
        for d in &FILTERS {
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
        set(&mut v, "price_earnings_ttm", "高于 50");
        let w = wire(&v, 0);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].op, "egreater");
        assert_eq!(w[0].right, vec![50.0]);

        let mut v = ViewState::DEFAULT;
        set(&mut v, "market_cap_basic", "中盘 $20亿 – $100亿");
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

        set(&mut v, "price_earnings_ttm", "10 – 15");
        assert_eq!(wire(&v, 0).len(), 1, "能下推的照常下推");
        assert_eq!(local_only(&v).len(), 1);
    }

    #[test]
    fn every_server_field_is_a_real_column_of_its_kinds() {
        // 下推一个该类不请求的字段，服务端筛完会返回 0 行 → 筛选静默失效
        for d in &FILTERS {
            let Some(f) = d.server else { continue };
            if f == "sector" {
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

    fn set(v: &mut ViewState, key: &str, label: &str) {
        let fi = fi_of(key);
        let pi = (1..=n_presets(fi))
            .find(|&i| preset_label(fi, i as u8) == label)
            .unwrap_or_else(|| panic!("{key} 没有档位 {label}"));
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
        set(&mut v, "price_earnings_ttm", "10 – 15");
        assert!(!passes(&row(), &v, 0));
        assert!(passes(&with("price_earnings_ttm", 12.0), &v, 0));
        assert!(!passes(&with("price_earnings_ttm", 20.0), &v, 0));
    }

    #[test]
    fn non_finite_values_fail_rather_than_pass() {
        let mut v = ViewState::DEFAULT;
        set(&mut v, "price_earnings_ttm", "高于 50");
        // inf >= 50 在数值上成立，但那是脏数据不是「市盈率很高的公司」
        assert!(!passes(&with("price_earnings_ttm", f64::INFINITY), &v, 0));
        assert!(!passes(&with("price_earnings_ttm", f64::NAN), &v, 0));
    }

    #[test]
    fn filters_combine_with_and() {
        let mut r = with("price_earnings_ttm", 12.0);
        r.m.insert("dividends_yield_current".into(), 3.0);
        let mut v = ViewState::DEFAULT;
        set(&mut v, "price_earnings_ttm", "10 – 15");
        assert!(passes(&r, &v, 0));
        set(&mut v, "dividends_yield_current", "高于 4%");
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
    fn pe_has_no_loss_bucket_because_tradingview_reports_null() {
        // 实测：248 个 EPS<0 的行 price_earnings_ttm 全为 null，从不是负数。
        // 加一个负数档就是一个永远筛不出东西的死选项
        assert!(PE.iter().all(|p| p.lo >= 0.0), "市盈率不该有负数档");
        // 亏损筛选归「每股收益」
        let mut v = ViewState::DEFAULT;
        set(&mut v, "earnings_per_share_diluted_ttm", "亏损 (< 0)");
        assert!(passes(&with("earnings_per_share_diluted_ttm", -0.24), &v, 0));
        assert!(!passes(&with("earnings_per_share_diluted_ttm", 3.0), &v, 0));
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
            if d.key.starts_with("own:") || d.key == "sector" {
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
    fn kind_columns(kind: &str) -> Vec<&'static str> {
        let mut v = match kind {
            "coin" => vec![
                "close", "24h_close_change|5", "market_cap_calc", "24h_vol_cmc",
                "24h_vol_to_market_cap", "circulating_supply", "crypto_total_rank",
                "socialdominance", "altrank", "total_shares_diluted",
                "circulating_to_max_supply_ratio", "24h_vol_change_cmc",
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
            ],
            "bond" => vec![
                "close", "close_pct", "close_net", "yield_to_worst", "current_coupon",
                "maturity_date", "accrued_coupon_interest", "coupon_date_next",
                "coupon_date_prev", "coupon_currency",
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
            assert!(n > 0, "{} 一个档位都没有", d.label);
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
                    assert!(p.lo < p.hi, "{}／{} 的区间是空的", d.label, p.label);
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
