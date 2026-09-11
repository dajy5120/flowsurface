//! 全市场雷达 pane 的视图渲染（docs/22 §2 ① / §6）。
//!
//! **独立新增面板**（`Content::MarketMap`）——不改任何既有面板；数据走
//! [`super::radar_readout`] 旁路快照，交互状态走 [`super::radar`]。
//!
//! 表达形式对齐 TradingView 的 Heatmap 与 Screener：
//!
//! - **离散分档色阶 + 图例**，不是连续渐变。几百个小格上，连续渐变的相邻档根本分辨不出，
//!   而分档能让人一眼数出「这格比那格深两档」。图例与上色**共用同一份分档定义**——
//!   图例和格子对不上比没有图例更糟。
//! - **Size by / Color by / 分组** 三个选择器（TV 热图顶部那排）。
//! - **字号随格子面积缩放的双行标签**（代号 + 数值），小于可辨识尺寸就只留色块。
//! - 代号显示**基础币**（BTC 而非 BTCUSDT），同 TV 的加密热图。
//! - Screener：**点列头排序**（带 ▲▼，同列再点翻向）+ 列组切换。
//!
//! 配色用蓝(涨)–橙(跌)而非 TV 默认的红绿：树图上格子多且小，红绿对色盲不可分辨。

use iced::mouse;
use iced::widget::canvas::{self, Cache, Frame, Geometry, Text};
use iced::widget::{
    button, canvas as canvas_widget, column, container, pick_list, progress_bar, row, scrollable,
    text,
};
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Size, Theme};

use super::radar::{
    Form,
    order, scale_kind, Opt, visible, AssetFilter, ColumnSet, GroupBy, Palette, RadarMsg,
    ScaleKind, SortKey, ViewMode, ViewState, COLOR_OPTS, SIZE_OPTS,
};
use super::radar_filter::{self, FILTERS};
use super::radar_readout::{
    Catalog, CatalogItem, CoinRow, ColumnTab, BreadthRow, EquityPanorama, IpoRow, ListingRow,
    MacroBoard, MacroRow, NewsRow, OverviewRow, Panorama, PerpRow, PredRow, Prediction, RadarRow,
    SectorRow, StockRow, OV_WINDOWS, WINDOWS,
};
use super::treemap::{squarify_nested, Rect};

const C_HEAD: Color = Color::from_rgb(0.55, 0.8, 1.0);
const C_DIM: Color = Color::from_rgb(0.55, 0.55, 0.6);
const C_TXT: Color = Color::from_rgb(0.85, 0.87, 0.92);
const C_GOLD: Color = Color::from_rgb(0.9, 0.8, 0.4);
const C_BAD: Color = Color::from_rgb(0.9, 0.45, 0.4);
/// 状态「正常/运行中」的提示色（与涨跌色板无关，不随色板切换）。
const C_OK: Color = Color::from_rgb(0.35, 0.78, 0.98);
/// 可点开原文的单元格。**必须与普通文本明显不同**——看不出哪些能点，
/// 等于这些链接不存在。
const C_LINK: Color = Color::from_rgb(0.47, 0.72, 1.0);

// ───────────────────────── 离散色阶 ─────────────────────────

const C_NEUTRAL: Color = Color::from_rgb(0.24, 0.25, 0.29);

/// 一套色阶：3 档/侧 + 中性，由弱到强。
///
/// **7 档而非 9 档**，同 TradingView（图例就是 −13/−8/−3/0/3/8/13 七格）。
/// 档位越少每档越可分辨；9 档在几百个小格上相邻档已经看不出差别。
///
/// TV 在浅色背景上把「最极端」做成**深色**（深红/深绿）；深色背景要反过来——
/// 最极端 = 最亮，否则极端值反而沉进背景里。
pub(crate) struct Ramp {
    pub up: [Color; 3],
    pub down: [Color; 3],
}

const BLUE: [Color; 3] = [
    Color::from_rgb(0.24, 0.42, 0.56),
    Color::from_rgb(0.20, 0.55, 0.82),
    Color::from_rgb(0.32, 0.72, 1.00),
];
const ORANGE: [Color; 3] = [
    Color::from_rgb(0.60, 0.40, 0.26),
    Color::from_rgb(0.82, 0.48, 0.19),
    Color::from_rgb(1.00, 0.62, 0.20),
];
const GREEN: [Color; 3] = [
    Color::from_rgb(0.22, 0.47, 0.36),
    Color::from_rgb(0.13, 0.64, 0.42),
    Color::from_rgb(0.22, 0.83, 0.52),
];
const RED: [Color; 3] = [
    Color::from_rgb(0.56, 0.29, 0.32),
    Color::from_rgb(0.82, 0.27, 0.31),
    Color::from_rgb(1.00, 0.37, 0.39),
];

/// 文字专用（同分档、更高亮度）：填充档是给大色块设计的，
/// 最弱档直接当深色背景上的文字读不出来。
fn brighten(c: Color) -> Color {
    Color::from_rgb(
        (c.r * 0.45 + 0.55).min(1.0),
        (c.g * 0.45 + 0.55).min(1.0),
        (c.b * 0.45 + 0.55).min(1.0),
    )
}

pub(crate) fn ramp(p: Palette) -> Ramp {
    match p {
        Palette::BlueOrange => Ramp { up: BLUE, down: ORANGE },
        Palette::GreenUp => Ramp { up: GREEN, down: RED },
        Palette::RedUp => Ramp { up: RED, down: GREEN },
    }
}

/// 分档边界（对称，绝对值递增）。3 个边界 → 7 档。
///
/// 按指标的**量纲**分族：百分数类用 %，z 类用 σ，相对成交量是倍数。
/// 混用一套边界的话，`Perf.YTD` 那种动辄 ±30% 的指标会全档饱和，
/// 而 `change|60` 那种 ±0.5% 的会全落中性。
/// 该口径的色阶边界。百分数类用资产类下发的官方边界；
/// 其余（相对成交量、雷达自有的 σ）保留原有量纲。
/// 总览（指数）与市场宽度用的色阶。这两处是**指数与汇率**，不属于任何一个
/// 资产类，目录里也没有它们的条目，所以取股票那档（官方指数热图同口径）。
pub(crate) const STOCK_SCALE: [f64; 3] = [1.0, 2.0, 3.0];

pub(crate) fn scale_edges(key: &str, scale: [f64; 3]) -> [f64; 3] {
    if key.starts_with("own:") || key == "relative_volume_10d_calc" {
        return edges(key);
    }
    scale
}

pub(crate) fn edges(key: &str) -> [f64; 3] {
    match key {
        "own:speed_z" | "own:zvol" => [0.4, 1.2, 2.5],
        // 长视界的表现类天然幅度更大，边界要跟着放
        "Perf.3M" | "Perf.6M" | "Perf.YTD" | "Perf.Y" => [3.0, 10.0, 25.0],
        "Perf.W" | "Perf.1M" => [1.5, 5.0, 12.0],
        "relative_volume_10d_calc" => [0.3, 1.0, 2.5],
        "Volatility.D" => [1.0, 2.5, 5.0],
        // 其余百分数类（涨跌 1h/4h/1天、盘前盘后、跳空、自有涨跌幅）
        _ => [0.5, 1.5, 3.0],
    }
}

/// 该指标的中性点（相对成交量以 1.0 为常态）。
fn center(key: &str) -> f64 {
    match scale_kind(key) {
        ScaleKind::AroundOne => 1.0,
        _ => 0.0,
    }
}

/// 落在第几档：0 = 中性，1..=3 由弱到强。
pub(crate) fn bucket(v: f64, e: &[f64; 3]) -> usize {
    let a = v.abs();
    if !a.is_finite() {
        return 0;
    }
    e.iter().filter(|x| a >= **x).count()
}

fn pick(
    v: Option<f64>,
    key: &str,
    scale: [f64; 3],
    r: &Ramp,
    bright: bool,
    zero: Color,
) -> Color {
    match v {
        Some(v) if v.is_finite() => {
            let d = v - center(key);
            // **与图例同一条 `scale_edges`**：分开走两套刻度的话，图例说 ±13%
            // 而格子在 ±3% 就顶满，看图的人无从察觉（加密实测整片深色）
            let b = bucket(d, &scale_edges(key, scale));
            if b == 0 {
                return zero;
            }
            // 量级型（波动率）恒非负，双向上色会把「低波动」画成跌，是错的
            let up = matches!(scale_kind(key), ScaleKind::Magnitude) || d >= 0.0;
            let c = if up { r.up[b - 1] } else { r.down[b - 1] };
            if bright { brighten(c) } else { c }
        }
        _ => zero,
    }
}

fn fade(c: Color, toward: Color, t: f32) -> Color {
    Color::from_rgb(
        c.r + (toward.r - c.r) * t,
        c.g + (toward.g - c.g) * t,
        c.b + (toward.b - c.b) * t,
    )
}

/// 值 → 树图填充色。`scale` 是**该资产类**的色阶（目录下发，见 `asset_scale`）——
/// 每类不同：股票 ±1/2/3%、加密 ±3/8/13%。共用一套的话加密整片顶到最深档。
///
/// `trusted=false`（未热身 / 借横截面基线）向中性去饱和，视觉上就弱一等。
pub(crate) fn scale_color(
    v: Option<f64>,
    key: &str,
    scale: [f64; 3],
    p: Palette,
    trusted: bool,
) -> Color {
    let c = pick(v, key, scale, &ramp(p), false, C_NEUTRAL);
    if trusted { c } else { fade(c, C_NEUTRAL, 0.6) }
}

/// 值 → 表格文字色（同一分档，更高亮度）。
pub(crate) fn scale_text(
    v: Option<f64>,
    key: &str,
    scale: [f64; 3],
    p: Palette,
    trusted: bool,
) -> Color {
    let c = pick(v, key, scale, &ramp(p), true, C_DIM);
    if trusted { c } else { fade(c, C_DIM, 0.55) }
}

fn edge_label(key: &str, e: f64) -> String {
    if key.starts_with("own:") && key != "own:ret_pct" {
        format!("{e:.1}σ")
    } else if key == "relative_volume_10d_calc" {
        format!("{:.1}×", 1.0 + e)
    } else {
        format!("{e:.1}%")
    }
}

// ───────────────────────── 取值与格式化 ─────────────────────────

/// 按指标键取值。`own:` 前缀是雷达自有指标，其余从快照的 `m` 字典取
/// （TradingView 口径，全量随行情一次取回）。
///
/// 百分数类一律返回**百分数**（与 TradingView 同量纲），自有的对数收益要换算——
/// 不换的话同一个色阶下 0.02 的对数收益会被当成 0.02%。
pub(crate) fn metric_value(r: &RadarRow, key: &str, win: usize) -> Option<f64> {
    match key {
        "equal" => Some(1.0),
        "own:turnover" => Some(r.quote_vol_24h.max(0.0)),
        // 加密行的 `m` 里没有 `close`（那是股票 scanner 的列），
        // 价格筛选用行上的 price 字段才能同时覆盖两类
        "own:price" => Some(r.price).filter(|x| *x > 0.0),
        "own:speed_z" => r.z_ret[win],
        "own:zvol" => r.z_vol,
        "own:ret_pct" => r.ret[win].map(|x| (x.exp() - 1.0) * 100.0),
        k => r.m.get(k).copied(),
    }
}

/// 树图面积权重。负值/缺失一律 0——负权重会让树图布局出负尺寸。
fn area_weight(r: &RadarRow, v: ViewState) -> f64 {
    metric_value(r, v.size.key, v.win)
        .filter(|x| x.is_finite())
        .unwrap_or(0.0)
        .max(0.0)
}

/// `BTCUSDT` → `BTC`。TV 的加密热图显示基础币，格子里放得下且更好认。
pub(crate) fn base_asset(sym: &str) -> &str {
    for q in ["USDT", "USDC", "FDUSD", "BTC", "ETH", "BNB"] {
        if let Some(b) = sym.strip_suffix(q)
            && !b.is_empty() {
                return b;
            }
    }
    sym
}

fn usd(v: f64) -> String {
    // 市值动辄万亿——没有 T 档的话 NVDA 会显示成 5240.0B（TradingView 是 5.24 T）
    if v.abs() >= 1e12 {
        format!("{:.2}T", v / 1e12)
    } else if v >= 1e9 {
        format!("{:.1}B", v / 1e9)
    } else if v >= 1e6 {
        format!("{:.1}M", v / 1e6)
    } else if v >= 1e3 {
        format!("{:.0}K", v / 1e3)
    } else {
        format!("{v:.0}")
    }
}

/// 金额格：**只有大额聚合量才缩写**。每股金额（价格/EPS/每股股息/目标价）
/// 要保留小数——`usd()` 从千位就缩写，会把 MELI 的 $1963.49 显示成 `2K`。
fn money_cell(v: f64) -> String {
    if v.abs() >= 1e6 {
        usd(v)
    } else {
        format!("{v:.2}")
    }
}

/// Unix 秒 → `MM-DD`（同年）或 `YY-MM-DD`。财报日期用。
///
/// 不加这一路，表里会直接显示 `1795608000`。
fn date_cell(secs: f64) -> String {
    use chrono::{Datelike, TimeZone};
    if !secs.is_finite() || secs <= 0.0 {
        return "—".into();
    }
    let Some(t) = chrono::Local.timestamp_opt(secs as i64, 0).single() else {
        return "—".into();
    };
    let now = chrono::Local::now();
    if t.year() == now.year() {
        format!("{:02}-{:02}", t.month(), t.day())
    } else {
        format!("{:02}-{:02}-{:02}", t.year() % 100, t.month(), t.day())
    }
}

/// `YYYYMMDD` 整数 → `YYYY-MM-DD`。债券到期日用。
///
/// 与 `date_cell`（Unix 秒）分开：不区分的话 20290214 会被当成秒、
/// 显示成 1970 年；完全不标注则被当成数量、显示成 `20.3M`。
fn ymd_cell(v: f64) -> String {
    let n = v as i64;
    let (y, m, d) = (n / 10_000, (n / 100) % 100, n % 100);
    if !(1900..=2999).contains(&y) || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return "—".into();
    }
    format!("{y}-{m:02}-{d:02}")
}

fn price(v: f64) -> String {
    if v >= 1000.0 {
        format!("{v:.1}")
    } else if v >= 1.0 {
        format!("{v:.3}")
    } else {
        format!("{v:.6}")
    }
}

fn opt_pct(v: Option<f64>) -> String {
    v.map(|x| format!("{:+.2}%", x * 100.0)).unwrap_or_else(|| "—".into())
}

fn opt_z(v: Option<f64>) -> String {
    v.map(|x| format!("{x:+.2}")).unwrap_or_else(|| "—".into())
}

/// 树图窄格用的紧凑写法（少一位小数 / 去掉百分号）。
fn opt_pct_compact(v: Option<f64>) -> String {
    v.map(|x| format!("{:+.1}", x * 100.0)).unwrap_or_else(|| "—".into())
}
fn opt_z_compact(v: Option<f64>) -> String {
    v.map(|x| format!("{x:+.1}")).unwrap_or_else(|| "—".into())
}

// ───────────────────────── Screener 列定义 ─────────────────────────

pub(crate) struct Col {
    pub key: SortKey,
    pub title: String,
    pub width: f32,
}

/// 当前列组下要显示的列（对应 TV Screener 顶部的 Overview/Performance/… 标签）。
///
/// TV 的九个列组由**快照下发的目录**给出，面板不硬编码列名——加列只改守护侧一处。
/// 另有三个雷达自有的列组（多窗口涨跌幅 / 多窗口速度 z / 参考数据），
/// 是 TradingView 没有的。
pub(crate) fn columns(v: ViewState, cat: &Catalog) -> Vec<Col> {
    let c = |key, title: &str, width| Col {
        key,
        title: title.to_string(),
        width,
    };
    let mut out = vec![
        c(SortKey::Symbol, "标的", 112.0),
        c(SortKey::Tier, "档", 30.0),
        c(SortKey::Venue, "市场", 76.0),
    ];
    match v.cols {
        ColumnSet::Speed => {
            for (i, w) in WINDOWS.iter().enumerate() {
                out.push(c(SortKey::Ret(i), w, 74.0));
            }
        }
        ColumnSet::SpeedZ => {
            for (i, w) in WINDOWS.iter().enumerate() {
                out.push(c(SortKey::Z(i), w, 68.0));
            }
            out.push(c(SortKey::VolZ, "量异常z", 74.0));
            out.push(c(SortKey::CntZ, "笔数异常z", 80.0));
        }
        ColumnSet::Reference => {
            out.push(c(SortKey::Country, "国别", 108.0));
            out.push(c(SortKey::Sector, "板块", 150.0));
            out.push(c(SortKey::Mcap, "市值", 84.0));
            out.push(c(SortKey::Price, "价格", 88.0));
        }
        ColumnSet::Tv(i) => {
            // **用当前资产类的列组**，不是股票那套。债券没有市盈率、
            // DEX 没有板块——拿股票的列去取，整表都是「—」
            if let Some(t) = asset_tabs(cat, v.asset).get(i) {
                for k in &t.cols {
                    out.push(Col {
                        key: SortKey::Metric(intern(k)),
                        title: title_of(cat, k).to_string(),
                        width: metric_width(cat, k),
                    });
                }
            }
        }
    }
    out
}

/// 指标键 → 列头中文名。未收录的键直接显示原键——比显示空白强，
/// 也让「加了列但忘了配名字」这件事一眼看得见。
/// 列的中文标题。**优先用守护下发的**（`catalog.titles`）——
/// 列在守护侧定义，标题也该在那边，两边各留一份必然漂移。
///
/// [`metric_title`] 只作为旧守护的回退，不再是唯一来源。
pub(crate) fn title_of<'a>(cat: &'a Catalog, k: &'a str) -> &'a str {
    cat.titles.get(k).map(String::as_str).unwrap_or_else(|| metric_title(k))
}

pub(crate) fn metric_title(k: &str) -> &str {
    match k {
        "close" => "价格",
        "change" => "涨跌%",
        "change|60" => "1h%",
        "change|240" => "4h%",
        "volume" => "成交量",
        "volume_change" => "量变化%",
        "relative_volume_10d_calc" => "相对量",
        "average_volume_10d_calc" => "10日均量",
        "average_volume_30d_calc" => "30日均量",
        "Value.Traded" => "成交额",
        "market_cap_basic" => "市值",
        "enterprise_value_current" => "企业价值",
        "price_earnings_ttm" => "市盈率",
        "price_earnings_growth_ttm" => "PEG",
        "price_sales_current" => "市销率",
        "price_book_fq" => "市净率",
        "price_to_cash_f_operating_activities_ttm" => "P/CF",
        "price_free_cash_flow_ttm" => "P/FCF",
        "price_cash_flow_current" => "价格/现金",
        "enterprise_value_ebitda_ttm" => "EV/EBITDA",
        "earnings_per_share_diluted_ttm" => "摊薄EPS",
        "earnings_per_share_basic_ttm" => "基本EPS",
        "earnings_per_share_diluted_yoy_growth_ttm" => "EPS增速%",
        "earnings_per_share_forecast_next_fq" => "EPS预测",
        "dividends_yield_current" => "股息率%",
        "dividends_per_share_fq" => "每股股息",
        "dividend_payout_ratio_ttm" => "派息率%",
        "dps_common_stock_prim_issue_yoy_growth_fy" => "股息增速%",
        "continuous_dividend_payout" => "连续派息",
        "continuous_dividend_growth" => "连续增息",
        "Perf.W" => "1周%",
        "Perf.1M" => "1月%",
        "Perf.3M" => "3月%",
        "Perf.6M" => "6月%",
        "Perf.YTD" => "YTD%",
        "Perf.Y" => "1年%",
        "Perf.5Y" => "5年%",
        "Perf.10Y" => "10年%",
        "Perf.All" => "全部%",
        "Volatility.W" => "周波动%",
        "Volatility.M" => "月波动%",
        "Volatility.D" => "日波动%",
        "Recommend.All" => "技术评级",
        "Recommend.MA" => "均线评级",
        "Recommend.Other" => "震荡评级",
        "RSI" => "RSI",
        "Mom" => "动量",
        "AO" => "AO",
        "CCI20" => "CCI",
        "Stoch.K" => "KDJ-K",
        "Stoch.D" => "KDJ-D",
        "premarket_close" => "盘前价",
        "premarket_change" => "盘前%",
        "premarket_gap" => "盘前跳空%",
        "premarket_volume" => "盘前量",
        "postmarket_close" => "盘后价",
        "postmarket_change" => "盘后%",
        "postmarket_volume" => "盘后量",
        "gap" => "跳空%",
        "gross_margin" => "毛利率%",
        "operating_margin" => "营业利润率%",
        "pre_tax_margin" => "税前利润率%",
        "net_margin" => "净利率%",
        "free_cash_flow_margin_ttm" => "FCF利润率%",
        "return_on_assets" => "ROA%",
        "return_on_equity" => "ROE%",
        "return_on_invested_capital" => "ROIC%",
        "total_revenue_ttm" => "营收",
        "total_revenue_yoy_growth_ttm" => "营收增速%",
        "gross_profit" => "毛利",
        "oper_income_ttm" => "营业利润",
        "net_income_ttm" => "净利润",
        "ebitda_ttm" => "EBITDA",
        "sector" => "板块",
        "AnalystRating" => "分析师评级",
        "TechRating_1D" => "技术评级",
        "MARating_1D" => "MA评级",
        "OsRating_1D" => "Os评级",
        "candlestick_patterns_1D" => "形态",
        "earnings_per_share_forecast_next_fy" => "EPS预估FY",
        "revenue_forecast_next_fy" => "收入预估FY",
        "net_income_estimate_ntm" => "净利预估NTM",
        "free_cash_flow_estimate_ntm" => "FCF预估NTM",
        "price_earnings_fwd" => "远期市盈率",
        "enterprise_value_ebitda_fwd" => "EV/EBITDA预估",
        "price_sales_fwd" => "远期市销率",
        "total_debt_estimate_fy" => "总债务预估FY",
        "book_value_per_share_estimate_fy" => "每股账面预估",
        "dps_estimate_ntm" => "每股分红预估",
        "Perf.1Y.MarketCap" => "市值表现1年%",
        "price_to_cash_ratio" => "价格/现金",
        "enterprise_value_to_revenue_ttm" => "EV/收入",
        "enterprise_value_to_ebit_ttm" => "EV/EBIT",
        "dps_common_stock_prim_issue_fy" => "每股股息FY",
        "dps_common_stock_prim_issue_fq" => "每股股息FQ",
        "dividends_yield" => "股息率%",
        "gross_margin_ttm" => "毛利率%",
        "operating_margin_ttm" => "营业利润率%",
        "pre_tax_margin_ttm" => "税前利润率%",
        "net_margin_ttm" => "净利率%",
        "return_on_assets_fq" => "ROA%",
        "return_on_equity_fq" => "ROE%",
        "return_on_invested_capital_fq" => "ROIC%",
        "research_and_dev_ratio_ttm" => "研发比率%",
        "sell_gen_admin_exp_other_ratio_ttm" => "SG&A比率%",
        "fiscal_period_current" => "财务期间",
        "fiscal_period_end_current" => "财务期末",
        "gross_profit_ttm" => "毛利润",
        "total_assets_fq" => "总资产",
        "total_current_assets_fq" => "流动资产",
        "cash_n_short_term_invest_fq" => "手头现金",
        "total_liabilities_fq" => "总负债",
        "total_debt_fq" => "总债务",
        "net_debt_fq" => "净债务",
        "total_equity_fq" => "权益总额",
        "current_ratio_fq" => "流动比率",
        "quick_ratio_fq" => "速动比率",
        "debt_to_equity_fq" => "债务/股权",
        "cash_n_short_term_invest_to_total_debt_fq" => "现金/债务",
        "cash_f_operating_activities_ttm" => "经营CF",
        "cash_f_investing_activities_ttm" => "投资CF",
        "cash_f_financing_activities_ttm" => "融资CF",
        "free_cash_flow_ttm" => "自由现金流",
        "neg_capital_expenditures_ttm" => "资本开支",
        "revenue_per_share_ttm" => "每股收入",
        "operating_cash_flow_per_share_ttm" => "每股经营CF",
        "free_cash_flow_per_share_ttm" => "每股FCF",
        "ebit_per_share_ttm" => "每股EBIT",
        "ebitda_per_share_ttm" => "每股EBITDA",
        "book_value_per_share_fq" => "每股账面价值",
        "total_debt_per_share_fq" => "每股总债务",
        "cash_per_share_fq" => "每股现金",
        "crypto_common_categories.tr" => "分类",
        "exchange.tr" => "交易所",
        "blockchain-id.tr" => "链",
        "isin-displayed" => "ISIN",
        "bid" => "买价",
        "ask" => "卖价",
        "high" => "最高",
        "low" => "最低",
        "Perf.5D" => "表现 5天%",
        "total_shares_diluted" => "总量",
        "circulating_to_max_supply_ratio" => "流通/上限%",
        "altrank" => "AltRank",
        "24h_vol_change_cmc" => "24h量变化%",
        "market_cap_diluted_calc" => "完全稀释市值",
        "MACD.macd" => "MACD",
        "coupon_frequency.tr" => "付息频率",
        "accrued_coupon_interest" => "应计利息",
        "coupon_date_prev" => "上次付息",
        "coupon_date_next" => "下次付息",
        "coupon_currency" => "票息币种",
        "bond_issuer_snp_rating_lt.tr" => "发行人标普评级",
        "aum" => "规模",
        "expense_ratio" => "费率%",
        "etf_holdings_count" => "持仓数",
        "asset_class.tr" => "资产类别",
        "focus.tr" => "投向",
        "issuer.tr" => "发行人",
        "nav" => "净值",
        "nav_discount_premium" => "折溢价%",
        "weight_top_10" => "前10权重%",
        "weight_top_25" => "前25权重%",
        "weight_top_50" => "前50权重%",
        "holdings_region.tr" => "持仓地区",
        "actively_managed.tr" => "管理方式",
        "index_tracked.tr" => "跟踪指数",
        "leverage.tr" => "杠杆",
        "indicated_annual_dividend" => "年化股息",
        "dividends_frequency.tr" => "派息频率",
        "dividend_treatment.tr" => "股息处理",
        "beta_3_year" => "Beta 3年",
        "beta_5_year" => "Beta 5年",
        "fund_flows.1M" => "资金流 1M",
        "fund_flows.3M" => "资金流 3M",
        "fund_flows.1Y" => "资金流 1Y",
        "fund_flows.3Y" => "资金流 3Y",
        "fund_flows.YTD" => "资金流 YTD",
        "nav_perf.1M" => "净值表现 1M",
        "nav_perf.3M" => "净值表现 3M",
        "nav_perf.1Y" => "净值表现 1Y",
        "nav_perf.3Y" => "净值表现 3Y",
        "nav_perf.YTD" => "净值表现 YTD",
        "nav_total_return.1M" => "净值总回报 1M",
        "nav_total_return.3M" => "净值总回报 3M",
        "nav_total_return.1Y" => "净值总回报 1Y",
        "nav_total_return.3Y" => "净值总回报 3Y",
        "nav_total_return.YTD" => "净值总回报 YTD",
        "name" => "名称",
        "value_traded_10d" => "价格×量 ~1周",
        "value_traded_30d" => "价格×量 ~1月",
        "close_pct" => "净价%",
        "close_net" => "应计利息",
        "yield_to_worst" => "最差收益率%",
        "current_coupon" => "当期票息%",
        "maturity_date" => "到期日",
        "bond_snp_rating_lt.tr" => "标普评级",
        "bond_fitch_rating_lt.tr" => "惠誉评级",
        "bond_issuer_type.tr" => "发行人类型",
        "redemption_type.tr" => "赎回类型",
        "24h_close_change|5" => "涨跌 24h%",
        "market_cap_calc" => "市值",
        "24h_vol_cmc" => "24h成交量",
        "24h_vol_to_market_cap" => "量/市值",
        "circulating_supply" => "流通量",
        "crypto_total_rank" => "排名",
        "socialdominance" => "社交热度",
        "24h_vol|5" => "24h成交额",
        "24h_vol_change|5" => "24h量变化%",
        "exchange" => "交易所",
        "dex_txs_count_24h" => "24h笔数",
        "dex_trading_volume_24h" => "24h成交额",
        "dex_txs_count_uniq_24h" => "24h独立地址",
        "dex_total_liquidity" => "流动性",
        "fully_diluted_value" => "完全稀释市值",
        "crypto_category" => "分类",
        "country" => "国别",
        "no_group" => "无分组",
        "earnings_release_date" => "上次财报",
        "earnings_release_next_date" => "下次财报",
        "beta_1_year" => "Beta",
        "price_target_average" => "目标价",
        "recommendation_total" => "分析师数",
        "recommendation_mark" => "分析师评级",
        other => other,
    }
}

/// 列宽。由表头实际排版宽度反推：`text_units` 是**字号的倍数**（中文 1.0、
/// 西文 0.62），表头字号 11，排序箭头再占约 2 个单位，左右内边距 8。
///
/// 原来按 `chars().count()` 判断长短：中文标题都短，看不出问题；一旦某个键
/// 漏了中文名而回退到原始键（`earnings_release_date` 折合 13 个单位 ≈143px），
/// 就会算出 78px 然后和右边一列叠在一起。
fn metric_width(cat: &Catalog, k: &str) -> f32 {
    ((text_units(title_of(cat, k)) + 2.0) * 11.0 + 8.0).clamp(78.0, 132.0)
}

/// 树图去重键。
///
/// 同一个币在 `binance:spot` 与 `binance:linear` 各有一行、市值相同——
/// 按市值铺图时两者各占一个满格，**面积被重复计算**（实测 BTC/ETH 各出现两次）。
/// 加密按基础币去重；股票必须带**市场段**——港股 `700` 与别处的 `700` 不是一回事。
pub(crate) fn dedup_key(r: &RadarRow) -> String {
    if r.asset == "crypto" {
        format!("crypto:{}", base_asset(&r.symbol))
    } else {
        format!("{}:{}", r.venue, r.symbol)
    }
}

/// 按 24h 成交额取前 n 个（树图选行）。**同一标的只保留成交额最大的那条挂牌**。
pub(crate) fn top_by_turnover(rows: &[RadarRow], n: usize) -> Vec<usize> {
    top_by_turnover_within(rows, &(0..rows.len()).collect::<Vec<_>>(), n)
}

/// 只在给定子集里取前 n（资产类过滤后用）。
pub(crate) fn top_by_turnover_within(rows: &[RadarRow], subset: &[usize], n: usize) -> Vec<usize> {
    let mut idx: Vec<usize> = subset.to_vec();
    idx.sort_by(|&a, &b| {
        rows[b]
            .quote_vol_24h
            .partial_cmp(&rows[a].quote_vol_24h)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| rows[a].symbol.cmp(&rows[b].symbol))
    });
    // 已按成交额降序，去重时天然保留最活跃的那条挂牌
    let mut seen = std::collections::HashSet::new();
    idx.retain(|&i| seen.insert(dedup_key(&rows[i])));
    idx.truncate(n);
    idx
}

/// 一格的文本与颜色。
pub(crate) fn cell_text(
    r: &RadarRow,
    k: SortKey,
    cat: &Catalog,
    p: Palette,
) -> (String, Color) {
    let trusted = r.trustworthy();
    let sc = row_scale(cat, r);
    match k {
        SortKey::Symbol => (
            r.symbol.clone(),
            if trusted { C_TXT } else { C_DIM },
        ),
        SortKey::Venue => (r.venue.trim_start_matches("binance:").to_string(), C_DIM),
        // 数据等级用颜色分档：A 常态、C/D 明显发暗——扫一眼就知道哪些行是延迟的
        SortKey::Tier => (
            r.tier.clone(),
            match r.tier.as_str() {
                "A" => C_TXT,
                "B" => C_HEAD,
                _ => C_GOLD,
            },
        ),
        SortKey::Country => (r.country.clone(), C_DIM),
        SortKey::Sector => (r.sector.clone(), C_DIM),
        SortKey::Mcap => (
            if r.mcap > 0.0 { usd(r.mcap) } else { "—".into() },
            C_DIM,
        ),
        SortKey::Metric(key) => {
            // 文本列（评级 / 财务期间 / K 线形态）走 `t` 段：它们不是数字，
            // 从 `m` 里取只会永远拿到 None、整列显示「—」
            if let Some(s) = r.t.get(key) {
                return (s.clone(), C_TXT);
            }
            let v = r.m.get(key).copied();
            let txt = match v {
                None => "—".to_string(),
                // 量纲由目录标注：百分数带 %、金额缩写并加 $、其余原样
                Some(x) if cat.pct_keys.contains(key) => format!("{x:+.2}%"),
                Some(x) if cat.money_keys.contains(key) => money_cell(x),
                Some(x) if cat.date_keys.contains(key) => date_cell(x),
                Some(x) if cat.ymd_keys.contains(key) => ymd_cell(x),
                Some(x) if x.abs() >= 1e6 => usd(x),
                Some(x) => format!("{x:.2}"),
            };
            // 只有涨跌类着色，估值/比率类保持中性——把 P/E 按涨跌上色是误导
            let col = if cat.pct_keys.contains(key) && key.contains("chang")
                || key.starts_with("Perf.")
                || key == "change"
                || key == "gap"
            {
                scale_text(v, "change", sc, p, true)
            } else {
                C_TXT
            };
            (txt, col)
        }
        SortKey::Price => (price(r.price), C_TXT),
        SortKey::Turnover => (usd(r.quote_vol_24h), C_DIM),
        SortKey::Ret(i) => (opt_pct(r.ret[i]), scale_text(r.ret[i].map(|x| (x.exp() - 1.0) * 100.0), "change", sc, p, trusted)),
        SortKey::Z(i) => (opt_z(r.z_ret[i]), scale_text(r.z_ret[i], "own:speed_z", sc, p, trusted)),
        SortKey::VolZ => (opt_z(r.z_vol), scale_text(r.z_vol, "own:zvol", sc, p, trusted)),
        SortKey::CntZ => (opt_z(r.z_cnt), scale_text(r.z_cnt, "own:zvol", sc, p, trusted)),
    }
}

// ───────────────────────── 树图画布 ─────────────────────────

struct TileData {
    label: String,
    /// 数值的两种写法：完整优先，放不下退紧凑；两个都放不下就**不画**。
    /// 绝不对数字做省略号截断——`-0.…` 分不出是 -0.1 还是 -0.9，比没有更糟。
    value: String,
    value_compact: String,
    weight: f64,
    color: Color,
}

struct GroupData {
    title: String,
    tiles: Vec<TileData>,
}

struct TreemapCanvas {
    groups: Vec<GroupData>,
    header_h: f32,
    cache: std::rc::Rc<Cache>,
}

/// 半角字符的平均字宽 / 字号。canvas 里拿不到实际排版宽度，只能估。
const ADV_NARROW: f32 = 0.62;
/// 全角（CJK 等）字宽 / 字号。Binance 有中文名标的（如「我踏马来了」），
/// 按半角估会严重低估宽度，标签直接压到隔壁格子上。
const ADV_WIDE: f32 = 1.0;

fn is_wide(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF | 0xFE30..=0xFE6F | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6 | 0x1F300..=0x1FAFF | 0x20000..=0x3FFFD)
}

/// 文本宽度，以「字号的倍数」计。
pub(crate) fn text_units(s: &str) -> f32 {
    s.chars()
        .map(|c| if is_wide(c) { ADV_WIDE } else { ADV_NARROW })
        .sum()
}

/// 让 `s` 恰好装进 `avail` 的字号（不超过 `max`）。
pub(crate) fn fit_font(s: &str, avail: f32, max: f32) -> f32 {
    let u = text_units(s);
    if u <= 0.0 {
        return max;
    }
    (avail / u).min(max)
}

/// 把文本裁到 `avail` 像素内，截断时补省略号（同 TradingView 的 `Consumer non-dur…`）。
/// 裁到 2 个真实字符以下就整个不画——一两个字母认不出是谁。
pub(crate) fn fit_text(s: &str, font_size: f32, avail: f32) -> Option<String> {
    if avail <= 0.0 || font_size <= 0.0 {
        return None;
    }
    if text_units(s) * font_size <= avail {
        return Some(s.to_string());
    }
    // 省略号本身要占位，否则补上去又溢出了
    let budget = avail - ADV_NARROW * font_size;
    let mut used = 0.0f32;
    let mut out = String::new();
    let mut n = 0usize;
    for c in s.chars() {
        let w = if is_wide(c) { ADV_WIDE } else { ADV_NARROW } * font_size;
        if used + w > budget {
            break;
        }
        used += w;
        out.push(c);
        n += 1;
    }
    if n < 2 {
        return None;
    }
    out.push('…');
    Some(out)
}

const MIN_LABEL_W: f32 = 30.0;
const MIN_LABEL_H: f32 = 16.0;
/// 格子之间的留白（每边）。TradingView 用**间隙**分隔格子，不描边——
/// 1px 描边在密集小格上会连成一片网格线，反而盖过颜色本身。
const TILE_GAP: f32 = 1.5;
/// 文字最多用掉格子宽度的比例。用满会让相邻格的标签视觉上贴在一起
/// （实测 PROM|SOL 两格的字几乎连成一体）。
const TEXT_WIDTH_FRAC: f32 = 0.86;
/// 分组标题带高度（标题画在面板底色上，不填充色块，同 TV 的 `Finance ›`）。
const GROUP_HEADER_H: f32 = 15.0;

impl<M> canvas::Program<M> for TreemapCanvas {
    type State = ();

    fn draw(
        &self,
        _s: &(),
        r: &Renderer,
        _t: &Theme,
        b: Rectangle,
        _c: mouse::Cursor,
    ) -> Vec<Geometry> {
        let geo = self.cache.draw(r, b.size(), |frame: &mut Frame| {
            let (w, h) = (frame.width(), frame.height());
            if w <= 8.0 || h <= 8.0 {
                return;
            }
            if self.groups.iter().all(|g| g.tiles.is_empty()) {
                frame.fill_text(Text {
                    content: "暂无数据——启动守护并等待首轮快照".into(),
                    position: Point::new(8.0, h / 2.0),
                    color: C_DIM,
                    size: iced::Pixels(11.0),
                    ..Default::default()
                });
                return;
            }
            let members: Vec<Vec<f64>> = self
                .groups
                .iter()
                .map(|g| g.tiles.iter().map(|t| t.weight).collect())
                .collect();

            for gl in squarify_nested(&members, Rect::new(0.0, 0.0, w, h), self.header_h) {
                let g = &self.groups[gl.group_idx];
                if self.header_h > 0.0 && !g.title.is_empty() {
                    // 标题画在底色上（不填色块），末尾带 › ——同 TV 的 `Finance ›`
                    if let Some(t) =
                        fit_text(&format!("{} ›", g.title), 10.0, gl.header.w - 4.0)
                    {
                        frame.fill_text(Text {
                            content: t,
                            position: Point::new(gl.header.x + 2.0, gl.header.y + 1.0),
                            color: C_TXT,
                            size: iced::Pixels(10.0),
                            ..Default::default()
                        });
                    }
                }
                for t in gl.tiles {
                    let d = &g.tiles[t.idx];
                    // 用留白而非描边分隔
                    let x = t.rect.x + TILE_GAP;
                    let y = t.rect.y + TILE_GAP;
                    let tw = t.rect.w - TILE_GAP * 2.0;
                    let th = t.rect.h - TILE_GAP * 2.0;
                    if tw <= 0.0 || th <= 0.0 {
                        continue;
                    }
                    frame.fill_rectangle(Point::new(x, y), Size::new(tw, th), d.color);
                    if tw < MIN_LABEL_W || th < MIN_LABEL_H {
                        continue;
                    }

                    // 文字居中（水平 + 作为整块垂直居中），同 TV。
                    //
                    // **字号只由格子尺寸决定**，不按标签长度反推：按长度反推会让
                    // SAMSUNG 缩到 8px 而邻格 ADA 是 20px，同样大的格子字号却差一倍；
                    // 而且会把文字撑满整格宽，相邻格的标签视觉上贴到一起。
                    let avail = tw * TEXT_WIDTH_FRAC;
                    let fs = (th * 0.30).min(tw * 0.40).clamp(8.0, 24.0);
                    let vfs = (fs * 0.76).max(8.0);
                    let two_lines = th >= fs + vfs + 4.0;
                    // 数值：完整 → 紧凑 → 不画（数字不做省略号截断）
                    let val = [&d.value, &d.value_compact]
                        .into_iter()
                        .find(|v| text_units(v) * vfs <= avail)
                        .cloned();
                    let lab = fit_text(&d.label, fs, avail);
                    let show_val = two_lines && val.is_some();

                    let block_h = match (&lab, show_val) {
                        (Some(_), true) => fs + vfs + 2.0,
                        (Some(_), false) => fs,
                        (None, true) => vfs,
                        (None, false) => continue,
                    };
                    let cx = x + tw / 2.0;
                    let mut cy = y + (th - block_h) / 2.0;

                    if let Some(lab) = lab {
                        frame.fill_text(Text {
                            content: lab.clone(),
                            position: Point::new(cx - text_units(&lab) * fs / 2.0, cy),
                            color: Color::WHITE,
                            size: iced::Pixels(fs),
                            ..Default::default()
                        });
                        cy += fs + 2.0;
                    }
                    if show_val {
                        let v = val.unwrap();
                        frame.fill_text(Text {
                            content: v.clone(),
                            position: Point::new(cx - text_units(&v) * vfs / 2.0, cy),
                            color: Color::from_rgba(1.0, 1.0, 1.0, 0.88),
                            size: iced::Pixels(vfs),
                            ..Default::default()
                        });
                    }
                }
            }
        });
        vec![geo]
    }
}

// ───────────────────────── 图例 ─────────────────────────

/// 图例：7 格色块，标签**居中压在各自色块上方**表示该档代表值（同 TradingView：
/// `−13% −8% −3% 0 3% 8% 13%`）。第一版在色块**之间**标边界值，导致标签和色块
/// 一一对不上，读起来要在心里错半格。
/// 色阶图例。`scale` 是**按资产类下发的**三个正向边界（百分数）——
/// 官方股票热图是 −3/−2/−1/0/1/2/3 %，加密是 −13/−8/−3/0/3/8/13 %。
/// 共用一套的话，加密那种日内动辄 ±10% 的品种会整片顶到最深档。
fn legend<'a>(key: &'static str, p: Palette, scale: [f64; 3]) -> Element<'a, RadarMsg> {
    const SW: f32 = 34.0;
    let e = scale_edges(key, scale);
    let r = ramp(p);
    let cells: [(Color, String); 7] = [
        (r.down[2], format!("-{}", edge_label(key, e[2]))),
        (r.down[1], format!("-{}", edge_label(key, e[1]))),
        (r.down[0], format!("-{}", edge_label(key, e[0]))),
        (C_NEUTRAL, if center(key) == 1.0 { "1×".into() } else { "0".into() }),
        (r.up[0], format!("+{}", edge_label(key, e[0]))),
        (r.up[1], format!("+{}", edge_label(key, e[1]))),
        (r.up[2], format!("+{}", edge_label(key, e[2]))),
    ];
    let mut labels = row![].spacing(1);
    let mut swatches = row![].spacing(1);
    for (c, l) in cells {
        labels = labels.push(
            container(text(l).size(9).color(C_DIM))
                .width(Length::Fixed(SW))
                .align_x(iced::Alignment::Center),
        );
        swatches = swatches.push(
            container(text(" ").size(8))
                .width(Length::Fixed(SW))
                .height(Length::Fixed(9.0))
                .style(move |_: &Theme| container::Style {
                    background: Some(c.into()),
                    ..Default::default()
                }),
        );
    }
    column![labels, swatches].spacing(1).into()
}

// ───────────────────────── 小部件 ─────────────────────────

/// 选择器 chip。**选中态用高亮而非置灰**——对齐 TradingView（活动标签是高亮药丸）。
/// 置灰会让人分不清「当前是这个」还是「这个不可选」，且按钮始终可点更符合直觉。
/// 用项目既有的 `style::button::modifier`（指标/标的表都是这套）。
/// 筛选下拉的一个选项：第 `fi` 个筛选器的第 `pi` 档（`pi = 0` 为不限）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FSel {
    pub fi: usize,
    pub pi: u8,
}

/// 枚举下拉的一项。索引用 `usize`——`FSel.pi` 是 `u8`，装不下债券发行人
/// 那 16897 个取值。`i == usize::MAX` 是「不限」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ESel {
    pub fi: usize,
    pub i: usize,
}

impl std::fmt::Display for ESel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.i == usize::MAX {
            return f.write_str("不限");
        }
        f.write_str(&radar_filter::enum_display(self.fi, self.i))
    }
}

impl std::fmt::Display for FSel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(radar_filter::preset_label(self.fi, self.pi))
    }
}

/// 筛选栏（对齐 TradingView 筛选器顶部那排下拉）。
///
/// 收起时只显示已设的几个（点一下即清除），展开才铺开全部 19 个——
/// 一直铺着的话热图就没地方了。
fn filter_bar<'a>(v: ViewState, cat: &Catalog) -> Element<'a, RadarMsg> {
    let n = radar_filter::active_count(&v);
    let mut head = row![chip(
        &if n > 0 { format!("筛选 {n}") } else { "筛选".into() },
        n > 0 || v.show_filters,
        RadarMsg::ToggleFilters,
    )]
    .spacing(3)
    .align_y(iced::Alignment::Center);

    if !v.show_filters {
        // 收起时把已设的列出来，否则「为什么表里只剩 3 行」无从查起
        for (fi, &pi) in v.filters.iter().enumerate() {
            if pi != 0 {
                head = head.push(chip(
                    &format!("{} {} ✕", FILTERS[fi].label, radar_filter::preset_label(fi, pi)),
                    true,
                    RadarMsg::SetFilter { fi, pi: 0 },
                ));
            }
        }
    }
    if n > 0 {
        head = head.push(chip("清空", false, RadarMsg::ClearFilters));
        // 下推到服务端的条件在**整个市场**上筛，本地条件只在守护已抓回的样本里筛。
        // 不区分的话，用户会以为「涨跌速度 > 3σ」也扫了全市场——实测全美
        // 「P/E<10 且 息>4%」命中 87 只，本地样本里只有 2 只。
        let local = radar_filter::local_only(&v);
        let pushed = n - local.len();
        head = head.push(
            text(if local.is_empty() {
                format!("　{pushed} 条全市场筛选")
            } else if pushed == 0 {
                format!("　仅本地样本：{}", local.join("、"))
            } else {
                format!("　{pushed} 条全市场 · {} 仅本地样本", local.join("、"))
            })
            .size(10)
            .color(if local.is_empty() { C_DIM } else { C_GOLD }),
        );
    }

    let mut col = column![head].spacing(3);
    if v.show_filters {
        // 每行 4 个，比横向滚动好找
        let mut n_shown = 0usize;
        let mut r = row![].spacing(6).align_y(iced::Alignment::Center);
        // **只列当前资产类适用的筛选**：把「市盈率」摆给债券、把「最差收益率」
        // 摆给股票，用户设了会得到一张空表而且没有任何提示
        for fi in radar_filter::for_kind_ordered(v.asset.kind(), cat.filters_for(v.asset.kind())) {
            let d = &FILTERS[fi];
            let is_enum = d.kind == radar_filter::FKind::Enum;
            // 官方**每个数值下拉末尾都有「手动设置」**；债券的数值筛选更是只有它
            let mut opts: Vec<FSel> = (0..=radar_filter::n_presets(fi) as u8)
                .map(|pi| FSel { fi, pi })
                .collect();
            if !is_enum && d.kind != radar_filter::FKind::Sector {
                opts.push(FSel { fi, pi: radar_filter::PI_MANUAL });
            }
            // ⓢ = 能下推到服务端（全市场），无标记 = 只在已加载的行里筛
            r = r.push(
                text(format!("{}{} ", d.label, if d.server.is_some() { "ⓢ" } else { "" }))
                    .size(11)
                    .color(C_DIM),
            );
            if is_enum {
                // 官方枚举下拉 = **搜索框 + 多选复选框列表 + 虚拟滚动**
                // （债券发行人 16897 项、货币 46628 项，没有搜索根本没法用）。
                // iced 没有现成的可搜下拉，拆成「搜索框 + 过滤后的 pick_list」。
                let q = radar_filter::enum_query(fi);
                if radar_filter::n_presets(fi) > 20 {
                    r = r.push(
                        iced::widget::text_input("搜索", &q)
                            .on_input(move |t| {
                                radar_filter::set_enum_query(fi, t);
                                RadarMsg::ManualEdited
                            })
                            .size(11)
                            .padding([2, 6])
                            .width(Length::Fixed(90.0)),
                    );
                }
                // 一次至多列 200 项：再多 pick_list 会卡，而官方靠虚拟滚动。
                // 搜索框就是用来把范围收窄到这 200 项以内的。
                const CAP: usize = 200;
                let mut opts: Vec<ESel> = vec![ESel { fi, i: usize::MAX }];
                opts.extend(
                    radar_filter::enum_matching(fi, CAP).into_iter().map(|i| ESel { fi, i }),
                );
                let sel = radar_filter::enum_selection(fi);
                r = r.push(
                    pick_list(opts, None::<ESel>, move |o: ESel| {
                        if o.i == usize::MAX {
                            RadarMsg::ClearEnum { fi: o.fi }
                        } else {
                            RadarMsg::ToggleEnum { fi: o.fi, i: o.i }
                        }
                    })
                    .placeholder(if sel.is_empty() {
                        "不限".to_string()
                    } else {
                        format!("已选 {}", sel.len())
                    })
                    .text_size(11)
                    .padding([2, 6])
                    .width(Length::Fixed(132.0)),
                );
                // 已选的列出来，点 ✕ 去掉——只显示「已选 N」看不出选了什么
                for &j in sel.iter().take(3) {
                    r = r.push(chip(
                        &format!("{} ✕", radar_filter::enum_display(fi, j)),
                        true,
                        RadarMsg::ToggleEnum { fi, i: j },
                    ));
                }
                if sel.len() > 3 {
                    r = r.push(text(format!("+{}", sel.len() - 3)).size(10).color(C_DIM));
                }
                n_shown += 1;
                if n_shown.is_multiple_of(4) {
                    col = col.push(std::mem::replace(
                        &mut r,
                        row![].spacing(6).align_y(iced::Alignment::Center),
                    ));
                }
                continue;
            }
            r = r.push(
                pick_list(opts, Some(FSel { fi, pi: v.filters[fi] }), |o: FSel| {
                    RadarMsg::SetFilter { fi: o.fi, pi: o.pi }
                })
                .text_size(11)
                .padding([2, 6])
                .width(Length::Fixed(132.0)),
            );
            // 选了「手动设置」就地出两个输入框（下界 / 上界，空 = 该侧不限）
            if v.filters[fi] == radar_filter::PI_MANUAL {
                let (lo, hi) = radar_filter::manual_text(fi);
                for (is_lo, cur, ph) in [(true, lo, "下界"), (false, hi, "上界")] {
                    r = r.push(
                        iced::widget::text_input(ph, &cur)
                            .on_input(move |t| {
                                radar_filter::set_manual(fi, is_lo, t);
                                RadarMsg::ManualEdited
                            })
                            .size(11)
                            .padding([2, 6])
                            .width(Length::Fixed(72.0)),
                    );
                }
            }
            n_shown += 1;
            if n_shown.is_multiple_of(4) {
                col = col.push(std::mem::replace(
                    &mut r,
                    row![].spacing(6).align_y(iced::Alignment::Center),
                ));
            }
        }
        if !n_shown.is_multiple_of(4) {
            col = col.push(r);
        }
        col = col.push(
            text(
                "带 ⓢ 的下推到服务端、在整个市场上筛（生效有一轮延迟）；\
                 其余只在已加载的行里筛。列出的都是当前资产类适用的——换一类会换一批",
            )
            .size(10)
            .color(C_DIM),
        );
    }
    col.into()
}

fn chip<'a>(label: &str, active: bool, msg: RadarMsg) -> Element<'a, RadarMsg> {
    button(text(label.to_string()).size(11))
        .padding([2, 7])
        .style(move |t, st| crate::style::button::modifier(t, st, active))
        .on_press(msg)
        .into()
}

/// 表格单元。数值列**右对齐**（同 TradingView）：右对齐后小数点纵向成列，
/// 一眼能比大小；左对齐的数字列要逐行读才知道谁大。
fn cell<'a>(s: String, w: f32, c: Color, numeric: bool) -> Element<'a, RadarMsg> {
    container(text(s).size(11).color(c))
        .width(Length::Fixed(w))
        .align_x(if numeric {
            iced::Alignment::End
        } else {
            iced::Alignment::Start
        })
        .into()
}

/// 该列是否为数值列（决定对齐方式）。
fn is_numeric(k: SortKey) -> bool {
    !matches!(
        k,
        SortKey::Symbol | SortKey::Venue | SortKey::Tier | SortKey::Country | SortKey::Sector
    )
}


/// 可点排序的列头。箭头**前置**且只出现在活动列上（同 TradingView 的 `↓ Mkt cap`）——
/// 后置箭头会把列标题推离它对齐的那列数字。
fn head_cell<'a>(col: &Col, v: ViewState) -> Element<'a, RadarMsg> {
    let active = col.key == v.sort;
    let label = if active {
        format!("{} {}", if v.desc { "↓" } else { "↑" }, col.title)
    } else {
        col.title.clone()
    };
    container(
        button(
            text(label)
                .size(11)
                .color(if active { C_HEAD } else { C_DIM }),
        )
        .padding([1, 2])
        .style(|t, st| crate::style::button::transparent(t, st, false))
        .on_press(RadarMsg::SortBy(col.key)),
    )
    .width(Length::Fixed(col.width))
    .align_x(if is_numeric(col.key) {
        iced::Alignment::End
    } else {
        iced::Alignment::Start
    })
    .into()
}

/// 抓取进度条：进度条本体 + `已抓 n/m` + 倒计时。
///
/// 数字和倒计时都给，是因为只有一条进度条时，人无法判断「它到底有没有在动」。
fn fetch_progress<'a>(p: &super::radar_readout::FetchProgress) -> Element<'a, RadarMsg> {
    let pct = (p.frac() * 100.0).round() as u32;
    let line = if p.total == 0 {
        "守护正在启动本轮抓取…".to_string()
    } else if p.pending > 0 {
        // 面板点名要的来源排在队首，所以这里的倒计时通常只有几秒
        format!(
            "已抓 {}/{} 个来源（{pct}%）· 你选的还差 {} 个 · 约 {}s",
            p.done, p.total, p.pending, p.eta_s
        )
    } else {
        format!("已抓 {}/{} 个来源（{pct}%）· 约 {}s 跑完本轮", p.done, p.total, p.eta_s)
    };
    let cur = if p.cur.is_empty() { String::new() } else { format!("　正在抓 {}", p.cur) };
    column![
        progress_bar(0.0..=1.0, p.frac()),
        text(format!("{line}{cur}")).size(10).color(C_DIM),
    ]
    .spacing(3)
    .into()
}

/// 当前资产类下真正生效的来源。对不上就返回空串（＝全部）。
///
/// 视图状态是一份，来源却是**按资产类分域**的：股票选市场码、加密选分类预设。
/// 换资产类的路径不止一条（点资产、切视图、恢复上次状态），只要有一条没清掉
/// 旧来源，就会得到一张空表——而下拉因为找不到它会回退显示「全部」，
/// 看起来一切正常。所以这里按目录**重新判定一次**，而不是指望每条路径都记得清。
pub(crate) fn effective_source(cat: &Catalog, v: ViewState) -> &'static str {
    if v.source.is_empty() {
        return "";
    }
    let ok = match v.asset {
        AssetFilter::Coin | AssetFilter::Cex | AssetFilter::Dex => {
            v.source == "all"
                || cat.coin_presets.iter().any(|p| p.code == v.source)
                || cat.crypto_cats.iter().any(|c| c.code == v.source)
        }
        AssetFilter::Etf => cat.etf_markets.iter().any(|c| c == v.source),
        AssetFilter::Stock | AssetFilter::All => {
            // `markets` 只含热图那 60 国；筛选器的 11 个国家与「全球」在
            // `screener_markets` 里——只查前者的话，筛选器选了它们会被判成无效
            cat.markets.iter().any(|m| m.code == v.source)
                || cat.screener_markets.iter().any(|m| m.code == v.source)
                || cat.indices.iter().any(|i| i.code == v.source)
        }
        // 债券/外汇没有来源下拉
        AssetFilter::Bond | AssetFilter::Forex => false,
    };
    if ok { v.source } else { "" }
}

/// 市场那一项（不是指数）的显示名。
///
/// 三条规则，都来自官方下拉：
/// - ETF 的条目**就是国名本身**，不是「所有 X 公司」
/// - 一般市场拼成「所有{国名}公司」
/// - 「欧洲」组下面有**两项**（所有欧盟公司 / 所有欧洲公司）共用组名「欧洲」，
///   拼不出来，只能用守护下发的 `all_label`——不用它的话两项会同名，
///   下拉里看起来就是「所有欧洲公司」重复了一遍。
fn all_entry_label(etf: bool, m: &CatalogItem) -> String {
    if etf {
        m.label.clone()
    } else if m.all_label.is_empty() {
        format!("所有{}公司", m.label)
    } else {
        m.all_label.clone()
    }
}

/// 来源市场下拉的一项。`key` 是快照里的 venue（空串 = 全部）。
///
/// venue 形如 `tv:america:stock` / `binance:linear`；显示时剥掉 `tv:` 前缀，
/// 否则每一项前面都顶着一样的前缀，看不出差别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarketOpt {
    pub key: &'static str,
    label: &'static str,
}

impl MarketOpt {
    fn all(crypto: bool) -> Self {
        MarketOpt {
            key: "",
            label: if crypto { "全部加密货币" } else { "全部市场" },
        }
    }

    /// 目录项 → 下拉项。未加载的市场标 `＋`，让人知道选了要等一轮才有数据。
    ///
    /// 目录来自快照、生命周期不是 `'static`；泄漏一份短字符串换取 `pick_list`
    /// 需要的 `'static`。目录是固定的 71 个市场 + 22 个分类，不会无界增长
    /// （同一项重复泄漏由 `INTERN` 去重）。
    /// `group` 是**国家名**（官方按国家分组，不是按地区）。
    /// 「所有 X 公司」里已经含国名，不再重复前缀。
    fn of(code: &str, label: &str, group: &str, loaded: bool) -> Self {
        let base = if group.is_empty() || label.starts_with("所有") {
            label.to_string()
        } else {
            format!("{group} · {label}")
        };
        let shown = if loaded { base } else { format!("{base} ＋") };
        MarketOpt {
            key: intern(code),
            label: intern(&shown),
        }
    }
}

/// 字符串驻留：同一份文本只泄漏一次，避免每帧重建下拉时无界泄漏。
/// 当前资产类的列组。目录里没有这一类（旧守护）就退回全局那套。
pub(crate) fn asset_tabs(cat: &Catalog, a: AssetFilter) -> &[ColumnTab] {
    cat.assets
        .iter()
        .find(|x| x.kind == a.kind())
        .map(|x| x.tabs.as_slice())
        .filter(|t| !t.is_empty())
        .unwrap_or(&cat.tabs)
}

/// 当前资产类的热图「大小/颜色」可选项。
///
/// 每类的字段名不同（股票 `market_cap_basic`、加密 `market_cap_calc`、
/// DEX `dex_total_liquidity`）——共用一套的话下拉里一半取不到值。
/// 板块英文名 → 中文。表来自 `radar_filter::SECTORS`（实测自一万七千只股票
/// 的 distinct 值，20 个固定分类），共用一份避免两处漂移。
fn zh_sector(en: &str) -> Option<String> {
    super::radar_filter::SECTORS
        .iter()
        .find(|(a, _)| *a == en)
        .map(|(_, zh)| (*zh).to_string())
}

/// ETF 资产类别英文名 → 中文。
fn zh_asset_class(en: &str) -> Option<String> {
    let zh = match en {
        "Equity" => "股票",
        "Fixed income" => "固定收益",
        "Commodity" | "Commodities" => "商品",
        "Currency" => "货币",
        "Multi-asset" | "Mixed allocation" => "多元资产",
        "Alternative" | "Alternatives" => "另类",
        "Real estate" => "房地产",
        "Preferred stock" => "优先股",
        "Digital assets" | "Cryptocurrency" => "数字资产",
        _ => return None,
    };
    Some(zh.to_string())
}

/// 该资产类的分组维度（目录下发）。官方股票只有「没有分组/板块」、
/// ETF 是「没有分组/资产类别」、加密**没有分组下拉**。
pub(crate) fn asset_groups(cat: &Catalog, a: AssetFilter) -> &[CatalogItem] {
    cat.assets
        .iter()
        .find(|x| x.kind == a.kind())
        .map(|x| x.group.as_slice())
        .unwrap_or(&[])
}

/// 该资产类的色阶边界。
/// **该行**所属资产类的色阶。表格里各类是混排的（「全部」视图下股票和加密同表），
/// 按「当前选中的资产类」取会给加密行套上股票的 ±1/2/3%，整片顶到最深档。
pub(crate) fn row_scale(cat: &Catalog, r: &RadarRow) -> [f64; 3] {
    // 行上的 asset 是数据侧的键（`equity`/`crypto`/`coin`…），
    // 目录里的 kind 是资产类的键——`crypto` 是 Binance 直连的交易对，归 CEX
    let kind = match r.asset.as_str() {
        "equity" => "stock",
        "crypto" => "cex",
        k => k,
    };
    cat.assets
        .iter()
        .find(|x| x.kind == kind)
        .map(|x| x.scale)
        .unwrap_or([1.0, 2.0, 3.0])
}

pub(crate) fn asset_scale(cat: &Catalog, a: AssetFilter) -> [f64; 3] {
    cat.assets
        .iter()
        .find(|x| x.kind == a.kind())
        .map(|x| x.scale)
        .unwrap_or([1.0, 2.0, 3.0])
}

/// 目录里的分组 key → 面板的分组枚举。
pub(crate) fn group_of(key: &str) -> GroupBy {
    match key {
        "sector" => GroupBy::Sector,
        "asset_class" => GroupBy::AssetClass,
        _ => GroupBy::None,
    }
}

pub(crate) fn asset_opts(
    cat: &Catalog,
    a: AssetFilter,
) -> Option<(&[CatalogItem], &[CatalogItem])> {
    cat.assets
        .iter()
        .find(|x| x.kind == a.kind())
        .filter(|x| !x.size.is_empty() && !x.color.is_empty())
        .map(|x| (x.size.as_slice(), x.color.as_slice()))
}

/// 记忆化「只依赖数据版本 + 当前口径」的行下标集合。
///
/// 视图每帧都会重建——上游用 `iced::window::frames()` 驱动图表动画，整个应用
/// 每秒重建 60 次视图（main.rs 的 subscription）。但可见集与排序只在数据换一轮
/// （2s）或用户改口径时才变，每帧重算 4189 行是纯浪费：实测排序一项就占掉
/// 一帧 8ms 里的 **4.5ms**。
///
/// `thread_local` 是因为 `Rc` 不是 `Send`，而视图只在主线程构建。
macro_rules! memo_idx {
    ($name:ident, $make:expr) => {
        fn $name(
            generation: u64,
            v: ViewState,
            rows: &[RadarRow],
            presets: &[super::radar::CoinPreset],
        ) -> std::rc::Rc<Vec<usize>> {
            thread_local! {
                static CELL: std::cell::RefCell<Option<(u64, ViewState, std::rc::Rc<Vec<usize>>)>> =
                    const { std::cell::RefCell::new(None) };
            }
            CELL.with(|c| {
                let mut g = c.borrow_mut();
                match &*g {
                    Some((g0, vs, rc)) if *g0 == generation && *vs == v => rc.clone(),
                    _ => {
                        let f: fn(&[RadarRow], ViewState, &[super::radar::CoinPreset]) -> Vec<usize> =
                            $make;
                        let rc = std::rc::Rc::new(f(rows, v, presets));
                        *g = Some((generation, v, rc.clone()));
                        rc
                    }
                }
            })
        }
    };
}

memo_idx!(memo_visible, |r, v, p| visible(r, v, p));
memo_idx!(memo_order, |r, v, p| order(r, v, p));

/// 树图取的那 180 格。同样只依赖数据版本 + 口径，不必每帧重挑一遍。
fn memo_picked(
    generation: u64,
    v: ViewState,
    rows: &[RadarRow],
    presets: &[super::radar::CoinPreset],
) -> std::rc::Rc<Vec<usize>> {
    thread_local! {
        static CELL: std::cell::RefCell<Option<(u64, ViewState, std::rc::Rc<Vec<usize>>)>> =
            const { std::cell::RefCell::new(None) };
    }
    CELL.with(|c| {
        let mut g = c.borrow_mut();
        match &*g {
            Some((g0, vs, rc)) if *g0 == generation && *vs == v => rc.clone(),
            _ => {
                let vis = memo_visible(generation, v, rows, presets);
                let rc = std::rc::Rc::new(top_by_turnover_within(rows, &vis, 180));
                *g = Some((generation, v, rc.clone()));
                rc
            }
        }
    })
}

/// 树图的 canvas 缓存。`key` 变了才清——`key` 由数据版本号与当前口径组成。
///
/// 用 `thread_local` 是因为 `Cache` 不是 `Send`；视图只在主线程构建。
fn treemap_cache(generation: u64, v: ViewState) -> std::rc::Rc<Cache> {
    thread_local! {
        static CELL: std::cell::RefCell<Option<(u64, ViewState, std::rc::Rc<Cache>)>> =
            const { std::cell::RefCell::new(None) };
    }
    CELL.with(|c| {
        let mut g = c.borrow_mut();
        match &*g {
            Some((g0, vs, rc)) if *g0 == generation && *vs == v => rc.clone(),
            _ => {
                let rc = std::rc::Rc::new(Cache::new());
                *g = Some((generation, v, rc.clone()));
                rc
            }
        }
    })
}

pub(crate) fn intern(s: &str) -> &'static str {
    use std::sync::Mutex;
    static POOL: Mutex<Option<std::collections::HashSet<&'static str>>> = Mutex::new(None);
    let mut g = match POOL.lock() {
        Ok(g) => g,
        Err(_) => return "",
    };
    let set = g.get_or_insert_with(std::collections::HashSet::new);
    if let Some(x) = set.get(s) {
        return x;
    }
    let leaked: &'static str = Box::leak(s.to_string().into_boxed_str());
    set.insert(leaked);
    leaked
}

impl std::fmt::Display for MarketOpt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label)
    }
}

/// 0..1 → 定宽条形。纯文本块拼的，够表达占比且不必再起一层 canvas。
fn bar<'a>(frac: Option<f64>, width: usize, c: Color) -> Element<'a, RadarMsg> {
    let Some(f) = frac else {
        return text("—").size(11).color(C_DIM).into();
    };
    let n = ((f.clamp(0.0, 1.0)) * width as f64).round() as usize;
    row![
        text("█".repeat(n)).size(11).color(c),
        text("░".repeat(width.saturating_sub(n))).size(11).color(C_NEUTRAL),
        text(format!(" {:>3.0}%", f * 100.0)).size(11).color(C_TXT),
    ]
    .into()
}

fn pct_log(v: Option<f64>) -> String {
    v.map(|x| format!("{:+.2}%", (x.exp() - 1.0) * 100.0))
        .unwrap_or_else(|| "—".into())
}

/// 加密全景（docs/22 §7）：八家交易所的公开 REST 汇成一屏。
///
/// 分五块，每块回答一个问题：交易所横向=「量在谁那儿、价差多大」、
/// 热门/涨跌=「今天谁在动」、永续=「杠杆那边什么姿势」、
/// 期权=「隐波和未平仓」、新上市=「有什么新东西」。
fn crypto_view<'a>(p: &Panorama, v: ViewState, fetched: i64) -> Element<'a, RadarMsg> {
    let mut col = column![].spacing(3);
    if p.venues.is_empty() {
        return text("暂无加密全景数据——守护还没跑完第一轮（默认 30s）")
            .size(11)
            .color(C_DIM)
            .into();
    }
    let up = ramp(v.palette).up[2];
    let dn = ramp(v.palette).down[2];
    let sign = |x: f64| if x >= 0.0 { up } else { dn };
    let money = |x: Option<f64>| x.map(usd).unwrap_or_else(|| "—".into());

    col = col.push(fresh_bar(
        Tier::Live,
        "八家交易所公开 REST 直连 · 无 key · 只读。成交额只统计美元计价的对",
        fetched,
        "30 秒",
        Some(blk::CRYPTO),
    ));

    col = col.push(crypto_listings(p));

    // ── 交易所横向 ──
    // 跨所价差用**中位价**做基准而不是某一家：拿一家当基准的话，
    // 那家自己抽风时会显示成「其余七家一起偏了」
    let mut btcs: Vec<f64> = p.venues.iter().filter_map(|x| x.btc).collect();
    btcs.sort_by(f64::total_cmp);
    let med = (!btcs.is_empty()).then(|| btcs[btcs.len() / 2]);
    col = col.push(section("交易所横向", "价差以各所 BTC 现价对中位价的偏离计，单位基点"));
    let mut hdr = row![].spacing(3);
    for (t, w, n) in [
        ("场所", 150.0, false),
        ("类型", 52.0, false),
        ("交易对", 62.0, true),
        ("24h 成交额", 104.0, true),
        ("BTC", 96.0, true),
        ("对中位价差", 104.0, true),
        ("状态", 320.0, false),
    ] {
        hdr = hdr.push(cell(t.into(), w, C_HEAD, n));
    }
    col = col.push(hdr);
    for x in &p.venues {
        let bp = match (x.btc, med) {
            (Some(b), Some(m)) if m > 0.0 => Some((b / m - 1.0) * 10_000.0),
            _ => None,
        };
        col = col.push(
            row![
                cell(x.label.clone(), 150.0, C_TXT, false),
                cell(
                    match x.kind.as_str() {
                        "spot" => "现货",
                        "perp" => "永续",
                        _ => "期权",
                    }
                    .into(),
                    52.0,
                    C_DIM,
                    false
                ),
                cell(x.pairs.to_string(), 62.0, C_DIM, true),
                // 抓取失败时成交额是 0；直接显示 0 和「今天没人交易」一模一样，
                // 故失败行的数值列一律显示「—」
                cell(
                    if x.err.is_empty() { usd(x.vol_usd) } else { "—".into() },
                    104.0,
                    C_TXT,
                    true
                ),
                cell(
                    x.btc.map(|b| format!("{b:.0}")).unwrap_or_else(|| "—".into()),
                    96.0,
                    C_TXT,
                    true
                ),
                cell(
                    bp.map(|b| format!("{b:+.1}")).unwrap_or_else(|| "—".into()),
                    104.0,
                    bp.map_or(C_DIM, |b| if b.abs() > 20.0 { C_GOLD } else { C_DIM }),
                    true
                ),
                cell(
                    if x.err.is_empty() { String::new() } else { format!("⚠ {}", x.err) },
                    320.0,
                    C_GOLD,
                    false
                ),
            ]
            .spacing(3),
        );
    }

    // ── 三张榜 ──
    const COIN_COLS: [Hd; 5] = [
        ("代号", 150.0, false),
        ("场所", 92.0, false),
        ("价格", 104.0, true),
        ("24h", 72.0, true),
        ("24h 成交额", 104.0, true),
    ];
    let coin_table = |table: u8, title: &str, note: &str, rows: &[CoinRow], dcol: u8| {
        let mut c = column![].spacing(3);
        c = c.push(section(title, note));
        c = c.push(sort_head(table, &COIN_COLS, dcol));
        for r in sort_rows(rows, table, dcol, |r: &CoinRow, i| match i {
            0 => sv(&r.symbol),
            1 => sv(&r.venue),
            2 => n(r.price),
            3 => n(r.chg_pct),
            _ => on(r.vol_usd),
        }) {
            c = c.push(
                row![
                    link_cell(r.symbol.clone(), &r.url, 150.0, C_TXT, false),
                    cell(r.venue.clone(), 92.0, C_DIM, false),
                    cell(money_cell(r.price), 104.0, C_TXT, true),
                    cell(format!("{:+.2}%", r.chg_pct), 72.0, sign(r.chg_pct), true),
                    cell(money(r.vol_usd), 104.0, C_TXT, true),
                ]
                .spacing(3),
            );
        }
        c
    };
    col = col.push(coin_table(
        tbl::CRY_HOT,
        "热门榜",
        "跨所按 24h 成交额。已剔除稳定币互换对——USDC/USDT 常年霸榜第一，但那不是行情",
        &p.hot,
        4,
    ));
    col = col.push(coin_table(
        tbl::CRY_GAIN,
        "涨幅榜",
        "已设成交额地板，否则榜首永远是几百美元成交的空气币",
        &p.gainers,
        3,
    ));
    col = col.push(coin_table(tbl::CRY_LOSE, "跌幅榜", "", &p.losers, 3));

    // ── 永续 ──
    col = col.push(section(
        "永续 · 资金费",
        "按**年化**费率绝对值排。各所结算间隔不同（Hyperliquid 每小时、其余 8 小时），只有年化能横比",
    ));
    const PERP_COLS: [Hd; 8] = [
        ("合约", 150.0, false),
        ("场所", 92.0, false),
        ("价格", 104.0, true),
        ("24h", 72.0, true),
        ("单期费率", 84.0, true),
        ("年化", 84.0, true),
        ("未平仓", 104.0, true),
        ("24h 成交额", 104.0, true),
    ];
    col = col.push(sort_head(tbl::CRY_PERP, &PERP_COLS, 5));
    for r in sort_rows(&p.perps, tbl::CRY_PERP, 5, |r: &PerpRow, i| match i {
        0 => sv(&r.symbol),
        1 => sv(&r.venue),
        2 => n(r.price),
        3 => n(r.chg_pct),
        4 => n(r.funding),
        // 默认按年化的**绝对值**排：极端负费率和极端正费率一样值得看
        5 => n(r.funding_apr.abs()),
        6 => on(r.oi_usd),
        _ => on(r.vol_usd),
    }) {
        col = col.push(
            row![
                link_cell(r.symbol.clone(), &r.url, 150.0, C_TXT, false),
                cell(r.venue.clone(), 92.0, C_DIM, false),
                cell(money_cell(r.price), 104.0, C_TXT, true),
                cell(format!("{:+.2}%", r.chg_pct), 72.0, sign(r.chg_pct), true),
                cell(format!("{:+.4}%", r.funding * 100.0), 84.0, sign(r.funding), true),
                cell(format!("{:+.0}%", r.funding_apr * 100.0), 84.0, sign(r.funding_apr), true),
                // 币安要逐对再请求一次才有未平仓，这里给不出——显示「—」而不是 0
                cell(money(r.oi_usd), 104.0, C_TXT, true),
                cell(money(r.vol_usd), 104.0, C_TXT, true),
            ]
            .spacing(3),
        );
    }

    // ── 期权 ──
    if !p.options.is_empty() {
        col = col.push(section(
            "期权（Deribit）",
            "权利金成交额是付出去的钱，名义成交额是标的口径——两者差两个数量级，别混着看",
        ));
        let mut h = row![].spacing(3);
        for (t, w, n) in [
            ("币种", 92.0, false),
            ("挂牌", 62.0, true),
            ("标的价", 96.0, true),
            ("未平仓名义", 116.0, true),
            ("权利金成交", 116.0, true),
            ("名义成交", 116.0, true),
            ("隐波", 72.0, true),
        ] {
            h = h.push(cell(t.into(), w, C_HEAD, n));
        }
        col = col.push(h);
        for r in &p.options {
            col = col.push(
                row![
                    link_cell(r.currency.clone(), &r.url, 92.0, C_TXT, false),
                    cell(r.n.to_string(), 62.0, C_DIM, true),
                    cell(
                        r.underlying.map(|x| format!("{x:.0}")).unwrap_or_else(|| "—".into()),
                        96.0,
                        C_TXT,
                        true
                    ),
                    cell(usd(r.oi_usd), 116.0, C_TXT, true),
                    cell(usd(r.vol_usd), 116.0, C_DIM, true),
                    cell(usd(r.vol_notional_usd), 116.0, C_TXT, true),
                    cell(
                        r.iv.map(|x| format!("{x:.1}%")).unwrap_or_else(|| "—".into()),
                        72.0,
                        C_GOLD,
                        true
                    ),
                ]
                .spacing(3),
            );
        }
    }

    col.into()
}

/// 新上市（加密）。放在最上面：它是这一屏里**唯一带时效性**的一块，
/// 埋在四张榜下面的话每次都得滚到底才能看到。
fn crypto_listings<'a>(p: &Panorama) -> Element<'a, RadarMsg> {
    let mut col = column![].spacing(3);
    if p.listings.is_empty() {
        return col.into();
    }
    col = col.push(section(
        "新上市",
        "口径是币安期货的 onboardDate——公开接口里**只有它**给上市时间，故这一栏只覆盖币安永续",
    ));
    col = col.push(days_bar(
        tbl::CRY_LISTING,
        30,
        &[(7, "近 7 天"), (30, "近 30 天"), (90, "近 90 天"), (0, "全部")],
    ));
    let days = super::radar::table_days(tbl::CRY_LISTING, 30);
    const COLS: [Hd; 3] = [("合约", 150.0, false), ("场所", 116.0, false), ("上市", 116.0, true)];
    col = col.push(sort_head(tbl::CRY_LISTING, &COLS, 2));
    let kept: Vec<ListingRow> =
        p.listings.iter().filter(|r| within_days(r.listed_ms, days)).cloned().collect();
    if kept.is_empty() {
        // 筛空了要说是筛空的。空白会被当成「守护没抓到」
        col = col.push(
            text(format!("这段时间内没有新上市（共 {} 条，放宽时间范围看看）", p.listings.len()))
                .size(11)
                .color(C_DIM),
        );
        return col.into();
    }
    for r in sort_rows(&kept, tbl::CRY_LISTING, 2, |r: &ListingRow, i| match i {
        0 => sv(&r.symbol),
        1 => sv(&r.venue),
        _ => n(r.listed_ms as f64),
    }) {
        col = col.push(
            row![
                link_cell(r.symbol.clone(), &r.url, 150.0, C_TXT, false),
                cell(r.venue.clone(), 116.0, C_DIM, false),
                cell(day_cell(r.listed_ms), 116.0, C_TXT, true),
            ]
            .spacing(3),
        );
    }
    col.into()
}

/// 预测市场（docs/22 §7）：Polymarket + Kalshi。
fn prediction_view<'a>(p: &Prediction, v: ViewState, fetched: i64) -> Element<'a, RadarMsg> {
    let mut col = column![].spacing(3);
    if p.sources.is_empty() {
        return text("暂无预测市场数据——守护还没跑完第一轮（默认 30s）")
            .size(11)
            .color(C_DIM)
            .into();
    }
    let up = ramp(v.palette).up[2];
    let dn = ramp(v.palette).down[2];

    // 报价本身是当前值，但成交额那一列带窗口（一家 24h、一家「近期」），
    // 所以不是「实时」而是「准实时」
    col = col.push(fresh_bar(
        Tier::NearLive,
        "Polymarket Gamma + Kalshi 公开接口 · 无 key。报价即当前值；成交额带窗口，见「窗口」列",
        fetched,
        "30 秒",
        Some(blk::PREDICTION),
    ));
    col = col.push(
        text("价格就是概率；涨跌是**概率点**，不是收益率。")
            .size(10)
            .color(C_DIM),
    );
    let mut sr = row![text("来源 ").size(10).color(C_DIM)].spacing(6);
    for s in &p.sources {
        sr = sr.push(
            // 同宏观那边：有数据就把条数一起显示，别让部分失败看着像整块挂了
            text(match (s.rows, s.err.is_empty()) {
                (_, true) => format!("{} {} 条", s.label, s.rows),
                (0, false) => format!("{} ⚠ {}", s.label, clip(&s.err, 42)),
                (n, false) => format!("{} {n} 条 ⚠ {}", s.label, clip(&s.err, 34)),
            })
            .size(10)
            .color(if s.err.is_empty() {
                C_DIM
            } else if s.rows > 0 {
                C_GOLD
            } else {
                C_BAD
            }),
        );
    }
    col = col.push(sr);

    const PRED_COLS: [Hd; 9] = [
        ("平台", 88.0, false),
        ("分类", 84.0, false),
        ("问题", 340.0, false),
        ("结果", 150.0, false),
        ("概率", 62.0, true),
        ("24h 变化", 78.0, true),
        ("成交额", 96.0, true),
        ("窗口", 52.0, false),
        ("到期", 88.0, true),
    ];
    let table = |tid: u8, title: &str, note: &str, rows: &[PredRow], dcol: u8| {
        let mut c = column![].spacing(3);
        c = c.push(section(title, note));
        c = c.push(sort_head(tid, &PRED_COLS, dcol));
        for r in sort_rows(rows, tid, dcol, |r: &PredRow, i| match i {
            0 => sv(&r.platform),
            1 => sv(&r.category),
            2 => sv(&r.title),
            3 => sv(&r.outcome),
            4 => on(r.prob),
            // 异动按**绝对值**排：跌 30 个点和涨 30 个点一样值得看
            5 => on(r.chg_24h.map(f64::abs)),
            6 => n(r.vol_usd),
            7 => sv(&r.vol_window),
            _ => on(r.close_ms.map(|x| x as f64)),
        }) {
            let r = &r;
            let chg = r.chg_24h;
            c = c.push(
                row![
                    cell(r.platform.clone(), 88.0, C_DIM, false),
                    cell(r.category.clone(), 84.0, C_DIM, false),
                    // 问题本身是链接：这一列最宽、也最像人会去点的那一处
                    link_cell(clip(&r.title, 46), &r.url, 340.0, C_TXT, false),
                    cell(clip(&r.outcome, 20), 150.0, C_TXT, false),
                    cell(
                        r.prob.map(|x| format!("{:.0}%", x * 100.0)).unwrap_or_else(|| "—".into()),
                        62.0,
                        C_TXT,
                        true
                    ),
                    // 「+10pt」不是「+10%」：0.45→0.55 是十个概率点，
                    // 写成百分比会被读成 22% 的收益率
                    cell(
                        chg.map(|x| format!("{:+.0}pt", x * 100.0)).unwrap_or_else(|| "—".into()),
                        78.0,
                        chg.map_or(C_DIM, |x| if x >= 0.0 { up } else { dn }),
                        true
                    ),
                    cell(usd(r.vol_usd), 96.0, C_TXT, true),
                    // 两家的窗口不是同一个：Polymarket 严格 24h、Kalshi 是「近期」。
                    // 口径必须跟着数字走，否则这一列在混排下就是在比两个不同的量
                    cell(r.vol_window.clone(), 52.0, C_GOLD, false),
                    cell(
                        r.close_ms.map(day_cell).unwrap_or_else(|| "—".into()),
                        88.0,
                        C_DIM,
                        true
                    ),
                ]
                .spacing(3),
            );
        }
        c
    };

    // 新上市**放最上面**：它是这一屏里时效性最强的一块
    let days = super::radar::table_days(tbl::PRED_FRESH, 7);
    col = col.push(days_bar(
        tbl::PRED_FRESH,
        7,
        &[(1, "近 1 天"), (7, "近 7 天"), (30, "近 30 天"), (0, "全部")],
    ));
    let fresh: Vec<PredRow> = p
        .fresh
        .iter()
        .filter(|r| within_days(r.start_ms.unwrap_or(0), days))
        .cloned()
        .collect();
    col = col.push(table(
        tbl::PRED_FRESH,
        "新上市",
        "只收已有成交的新盘——纯按开盘时间排，全是每分钟机器生成的五分钟制小盘",
        &fresh,
        8,
    ));
    if fresh.is_empty() && !p.fresh.is_empty() {
        col = col.push(
            text(format!("这段时间内没有新盘（共 {} 条，放宽时间范围看看）", p.fresh.len()))
                .size(11)
                .color(C_DIM),
        );
    }
    col = col.push(table(
        tbl::PRED_HOT,
        "热门榜",
        "两家**各自排序后交错**，不跨平台比大小——成交额的窗口和单位都不同，直接混排会让一家整体压过另一家",
        &p.hot,
        // 守护已经把两家各自排好再交错了，别再按某一列拍平
        NO_SORT,
    ));
    col = col.push(table(
        tbl::PRED_MOVE,
        "概率异动",
        "已剔除首次成交（前价为 0 的盘，第一笔打在 97 分会报成「涨了 97 点」）；5% 冲到 99% 的结算行情保留",
        &p.movers,
        NO_SORT,
    ));
    col.into()
}

/// 块号。与 `radar::block_name` 一一对应。
mod blk {
    pub const CRYPTO: u8 = 1;
    pub const PREDICTION: u8 = 2;
    pub const EQUITY: u8 = 3;
    pub const MACROS: u8 = 4;
    /// 股票慢层（总览 + 宽度）。它由股票线程刷，与四个新看板不是一条线。
    pub const SLOW: u8 = 5;
}

/// 数据等级（docs/22 §0）。**每一屏都要标**——不标等级就是自欺：
/// 延迟 15 分钟的价格和实时价在表格里长得一模一样。
#[derive(Clone, Copy, PartialEq)]
enum Tier {
    /// 交易所直连，真实时。
    Live,
    /// 准实时：接口本身是当前值，但更新有节拍或口径带窗口。
    NearLive,
    /// 延迟约 15 分钟。
    Delayed,
    /// 只有收盘价。
    Close,
    /// 按期公布（月频/年频/事件驱动），没有「实时」一说。
    Periodic,
}

impl Tier {
    fn badge(self) -> &'static str {
        match self {
            Tier::Live => "A 实时",
            Tier::NearLive => "B 准实时",
            Tier::Delayed => "C 延迟",
            Tier::Close => "D 收盘价",
            Tier::Periodic => "E 按期公布",
        }
    }
    /// 颜色分档：实时用常态色，延迟/收盘价发暗或标黄——扫一眼就知道
    /// 这一屏能不能当实时用。
    fn color(self) -> Color {
        match self {
            Tier::Live => C_OK,
            Tier::NearLive => C_HEAD,
            Tier::Delayed | Tier::Close => C_GOLD,
            Tier::Periodic => C_DIM,
        }
    }
}

/// 一屏（或一块）顶部的数据说明条：等级 · 口径 · 上次更新 · 手动刷新。
///
/// `fetched_ms` 是**这一块自己的**抓取时刻，不是快照时间：这些块跑在
/// 三个节拍上（30s / 5min / 1h），共用快照时间会把一小时前的宏观数据
/// 标成「刚刚更新」。`0` = 还没抓过。
fn fresh_bar<'a>(
    tier: Tier,
    note: &str,
    fetched_ms: i64,
    period: &str,
    block: Option<u8>,
) -> Element<'a, RadarMsg> {
    let mut r = row![].spacing(6).align_y(iced::Alignment::Center);
    r = r.push(
        container(text(tier.badge().to_string()).size(10).color(tier.color()))
            .padding([1, 5])
            .style(move |_: &Theme| container::Style {
                border: iced::Border { color: tier.color(), width: 1.0, radius: 3.0.into() },
                ..Default::default()
            }),
    );
    r = r.push(text(note.to_string()).size(10).color(C_DIM));
    r = r.push(text(format!("· 自动 {period}")).size(10).color(C_DIM));
    r = r.push(text(format!("· {}", ago(fetched_ms))).size(10).color(age_color(fetched_ms)));
    if let Some(b) = block {
        r = r.push(chip("⟳ 立即刷新", false, RadarMsg::ForceBlock(b)));
    }
    r.into()
}

/// 「更新于 HH:MM:SS（N 秒前）」。`0` = 还没抓过。
fn ago(ms: i64) -> String {
    use chrono::TimeZone;
    if ms <= 0 {
        return "尚未抓取".into();
    }
    let Some(t) = chrono::Local.timestamp_millis_opt(ms).single() else {
        return "尚未抓取".into();
    };
    let d = (chrono::Local::now().timestamp_millis() - ms).max(0) / 1000;
    let human = if d < 60 {
        format!("{d} 秒前")
    } else if d < 3600 {
        format!("{} 分钟前", d / 60)
    } else {
        format!("{} 小时前", d / 3600)
    };
    format!("更新于 {}（{human}）", t.format("%H:%M:%S"))
}

/// 数据放久了要变色。**多久算久按块的节拍来**是做不到的（这里只有一个时间戳），
/// 故用一个统一的粗阈值：超过十分钟就标黄，超过一小时标红。
/// 宏观那种一小时一刷的块因此常年是黄的——那正是实情。
fn age_color(ms: i64) -> Color {
    if ms <= 0 {
        return C_GOLD;
    }
    match (chrono::Local::now().timestamp_millis() - ms).max(0) / 1000 {
        0..=599 => C_DIM,
        600..=3599 => C_GOLD,
        _ => C_BAD,
    }
}

/// 四个新看板里各张表的编号。用来给排序/时间范围的注册表做键——
/// 这些表的列各不相同，共用一套状态会让「按资金费排」跟着切到别的表上去。
mod tbl {
    pub const CRY_LISTING: u8 = 1;
    pub const CRY_VENUE: u8 = 2;
    pub const CRY_HOT: u8 = 3;
    pub const CRY_GAIN: u8 = 4;
    pub const CRY_LOSE: u8 = 5;
    pub const CRY_PERP: u8 = 6;
    pub const PRED_FRESH: u8 = 10;
    pub const PRED_HOT: u8 = 11;
    pub const PRED_MOVE: u8 = 12;
    pub const EQ_IPO: u8 = 20;
    pub const EQ_HOT: u8 = 21;
    pub const EQ_GAIN: u8 = 22;
    pub const EQ_LOSE: u8 = 23;
    pub const EQ_SECTOR: u8 = 24;
    pub const MACRO_ROWS: u8 = 30;
    pub const MACRO_NEWS: u8 = 31;
}

/// 表头一列的定义：`(标题, 宽度, 是否数值列)`。名字避开已有的 `Col`（树图用的那个）。
type Hd = (&'static str, f32, bool);

/// 可点排序的列头行。活动列前面加箭头，同主表的做法。
fn sort_head<'a>(table: u8, cols: &[Hd], default_col: u8) -> Element<'a, RadarMsg> {
    let (active, desc) = super::radar::table_sort(table, default_col);
    let mut h = row![].spacing(3);
    for (i, (t, w, numeric)) in cols.iter().enumerate() {
        let i = i as u8;
        let label = if i == active {
            format!("{} {t}", if desc { "↓" } else { "↑" })
        } else {
            (*t).to_string()
        };
        h = h.push(
            button(
                container(text(label).size(11).color(if i == active { C_TXT } else { C_HEAD }))
                    .width(Length::Fixed(*w))
                    .align_x(if *numeric {
                        iced::Alignment::End
                    } else {
                        iced::Alignment::Start
                    }),
            )
            .padding(0)
            .style(|_, _| button::Style::default())
            .on_press(RadarMsg::SortTable { table, col: i }),
        );
    }
    h.into()
}

/// 「保持守护给的顺序」。有些榜的次序**不是**某一列的排序结果：
/// 预测市场热门榜是两家各自排完再交错的，按成交额重排就把它拍回成
/// 跨平台比大小——而两家的成交额窗口和单位根本不是一个量。
const NO_SORT: u8 = 255;

/// 按注册表里的状态给一组行排序。`key` 给出每行在某列上的可比值。
///
/// 文本列与数值列分开：文本按字典序、数值按大小。混在一起用字符串比会让
/// `9` 排在 `10` 后面。
fn sort_rows<T: Clone>(
    rows: &[T],
    table: u8,
    default_col: u8,
    key: impl Fn(&T, u8) -> SortVal,
) -> Vec<T> {
    let (col, desc) = super::radar::table_sort(table, default_col);
    let mut v: Vec<T> = rows.to_vec();
    if col == NO_SORT {
        return v;
    }
    v.sort_by(|a, b| {
        let o = match (key(a, col), key(b, col)) {
            (SortVal::N(x), SortVal::N(y)) => x.total_cmp(&y),
            (SortVal::S(x), SortVal::S(y)) => x.cmp(&y),
            // 缺值一律沉底，**不论升降序**——升序时让一片「—」占满前几行
            // 没有任何用处
            (SortVal::None, SortVal::None) => std::cmp::Ordering::Equal,
            (SortVal::None, _) => std::cmp::Ordering::Greater,
            (_, SortVal::None) => std::cmp::Ordering::Less,
            _ => std::cmp::Ordering::Equal,
        };
        if desc && !matches!(key(a, col), SortVal::None) && !matches!(key(b, col), SortVal::None) {
            o.reverse()
        } else {
            o
        }
    });
    v
}

/// 排序取值。
enum SortVal {
    N(f64),
    S(String),
    None,
}

fn n(x: f64) -> SortVal {
    if x.is_finite() { SortVal::N(x) } else { SortVal::None }
}
fn on(x: Option<f64>) -> SortVal {
    x.filter(|v| v.is_finite()).map_or(SortVal::None, SortVal::N)
}
fn sv(x: &str) -> SortVal {
    SortVal::S(x.to_string())
}

/// 带链接的单元格。**没有链接就不给按钮**——一个点了没反应的按钮
/// 比没有按钮更让人困惑。
fn link_cell<'a>(s: String, url: &str, w: f32, c: Color, numeric: bool) -> Element<'a, RadarMsg> {
    match super::radar_readout::register_link(url) {
        Some(id) => button(
            container(text(s).size(11).color(C_LINK))
                .width(Length::Fixed(w))
                .align_x(if numeric { iced::Alignment::End } else { iced::Alignment::Start }),
        )
        .padding(0)
        .style(|_, _| button::Style::default())
        .on_press(RadarMsg::OpenLink(id))
        .into(),
        None => cell(s, w, c, numeric),
    }
}

/// 「近 N 天」选择条。
fn days_bar<'a>(table: u8, default_days: u16, opts: &[(u16, &str)]) -> Element<'a, RadarMsg> {
    let cur = super::radar::table_days(table, default_days);
    let mut r = row![text("时间 ").size(10).color(C_DIM)].spacing(3);
    for (d, l) in opts {
        r = r.push(chip(l, *d == cur, RadarMsg::SetDays { table, days: *d }));
    }
    r.align_y(iced::Alignment::Center).into()
}

/// 毫秒时间戳是否落在「近 N 天」内。`days == 0` 表示不限。
///
/// 时间戳为 0（解析失败）时**保留**：那是数据问题，不该被时间筛静默吃掉。
fn within_days(ts_ms: i64, days: u16) -> bool {
    if days == 0 || ts_ms <= 0 {
        return true;
    }
    let now = chrono::Local::now().timestamp_millis();
    now - ts_ms <= days as i64 * 86_400_000
}

/// 股票全景（docs/22 §7 第二批）：纳斯达克全表 + Cboe 延迟指数。
///
/// 这一屏最要紧的一行在最上面：**数据是哪个交易日的收盘**。休市时它是几天前的
/// 数字，而收盘价和实时价在表格里长得一模一样。
fn equity_view<'a>(p: &EquityPanorama, v: ViewState, fetched: i64) -> Element<'a, RadarMsg> {
    let mut col = column![].spacing(3);
    if p.universe == 0 && p.indices.is_empty() {
        return text("暂无股票全景数据——守护还没跑完第一轮（默认 5 分钟）")
            .size(11)
            .color(C_DIM)
            .into();
    }
    let up = ramp(v.palette).up[2];
    let dn = ramp(v.palette).down[2];
    let sign = |x: f64| if x >= 0.0 { up } else { dn };

    // **这一屏里有两种新鲜度**：全表是上一交易日收盘（D），Cboe 指数是延迟
    // 15 分钟（C）。只标一个会让人把另一半也当成同档
    col = col.push(fresh_bar(
        Tier::Close,
        &format!(
            "纳斯达克全表：{} 的收盘价，不是实时价（{}｜下一交易日 {}）",
            if p.status.previous_trade_date.is_empty() {
                "上一交易日"
            } else {
                &p.status.previous_trade_date
            },
            if p.status.indicator.is_empty() { "—" } else { &p.status.indicator },
            p.status.next_trade_date,
        ),
        fetched,
        "5 分钟",
        Some(blk::EQUITY),
    ));
    for (what, err) in &p.errors {
        col = col.push(text(format!("⚠ {what}：{err}")).size(10).color(C_GOLD));
    }

    col = col.push(equity_ipos(p));

    // ── 指数 ──
    if !p.indices.is_empty() {
        col = col.push(section("指数（Cboe）", "「最后成交」就是这份数据延迟多少的证据"));
        // 与上面那条不同档：指数是延迟报价，不是收盘价
        col = col.push(fresh_bar(
            Tier::Delayed,
            "Cboe 公开延迟报价，约 15 分钟",
            fetched,
            "5 分钟",
            None,
        ));
        let mut h = row![].spacing(3);
        for (t, w, n) in [
            ("指数", 116.0, false),
            ("现价", 104.0, true),
            ("涨跌", 88.0, true),
            ("涨跌幅", 80.0, true),
            ("开", 96.0, true),
            ("高", 96.0, true),
            ("低", 108.0, true),
            ("最后成交", 160.0, false),
        ] {
            h = h.push(cell(t.into(), w, C_HEAD, n));
        }
        col = col.push(h);
        for q in &p.indices {
            col = col.push(
                row![
                    link_cell(q.label.clone(), &q.url, 116.0, C_TXT, false),
                    cell(format!("{:.2}", q.price), 104.0, C_TXT, true),
                    cell(format!("{:+.2}", q.chg), 88.0, sign(q.chg), true),
                    cell(format!("{:+.2}%", q.chg_pct), 80.0, sign(q.chg), true),
                    cell(format!("{:.2}", q.open), 96.0, C_DIM, true),
                    cell(format!("{:.2}", q.high), 96.0, C_DIM, true),
                    cell(format!("{:.2}", q.low), 108.0, C_DIM, true),
                    cell(q.last_trade.clone(), 160.0, C_DIM, false),
                ]
                .spacing(3),
            );
        }
    }

    // ── 三张榜 ──
    const STOCK_COLS: [Hd; 7] = [
        ("代号", 76.0, false),
        ("名称", 250.0, false),
        ("板块", 150.0, false),
        ("价格", 96.0, true),
        ("涨跌幅", 76.0, true),
        ("成交额", 96.0, true),
        ("市值", 96.0, true),
    ];
    let stock_table = |tid: u8, title: &str, note: &str, rows: &[StockRow], dcol: u8| {
        let mut c = column![].spacing(3);
        c = c.push(section(title, note));
        c = c.push(sort_head(tid, &STOCK_COLS, dcol));
        for r in sort_rows(rows, tid, dcol, |r: &StockRow, i| match i {
            0 => sv(&r.symbol),
            1 => sv(&r.name),
            2 => sv(&r.sector),
            3 => n(r.price),
            4 => n(r.chg_pct),
            5 => n(r.turnover),
            // 0 是「原表没给」，按缺值处理沉底——当成市值为零会让 ETF
            // 全挤到升序榜首，看着像一堆一文不值的公司
            _ => (r.mcap > 0.0).then_some(r.mcap).map_or(SortVal::None, SortVal::N),
        }) {
            let r = &r;
            c = c.push(
                row![
                    link_cell(r.symbol.clone(), &r.url, 76.0, C_TXT, false),
                    link_cell(clip(&r.name, 32), &r.url, 250.0, C_DIM, false),
                    cell(clip(&r.sector, 18), 150.0, C_DIM, false),
                    cell(money_cell(r.price), 96.0, C_TXT, true),
                    cell(format!("{:+.2}%", r.chg_pct), 76.0, sign(r.chg_pct), true),
                    cell(usd(r.turnover), 96.0, C_TXT, true),
                    // 0 是「原表没给」（多为 ETF），不是市值为零
                    cell(
                        if r.mcap > 0.0 { usd(r.mcap) } else { "—".into() },
                        96.0,
                        C_DIM,
                        true
                    ),
                ]
                .spacing(3),
            );
        }
        c
    };
    col = col.push(stock_table(
        tbl::EQ_HOT,
        &format!("热门榜（全表 {} 只）", p.universe),
        "按**成交额**排，不按成交股数——1.7 美元的票成交 2.7 亿股，钱远不如 1016 美元那只多",
        &p.hot,
        5,
    ));
    col = col.push(stock_table(
        tbl::EQ_GAIN,
        "涨幅榜",
        "价格与成交额**两个地板都设了**：不设价格地板，榜首永远是 $0.016 涨 1130% 的仙股",
        &p.gainers,
        4,
    ));
    col = col.push(stock_table(tbl::EQ_LOSE, "跌幅榜", "", &p.losers, 4));

    // ── 板块 ──
    col = col.push(section(
        "板块表现",
        "加权说「钱的方向」、中位说「多数成分股的方向」。两者背离就是几只权重股在扛",
    ));
    const SECTOR_COLS: [Hd; 9] = [
        ("板块", 190.0, false),
        ("只数", 60.0, true),
        ("涨", 60.0, true),
        ("跌", 72.0, true),
        ("上涨占比", 150.0, false),
        ("市值加权", 88.0, true),
        ("加权覆盖", 76.0, true),
        ("中位", 80.0, true),
        ("成交额", 96.0, true),
    ];
    col = col.push(sort_head(tbl::EQ_SECTOR, &SECTOR_COLS, 8));
    for sr in sort_rows(&p.sectors, tbl::EQ_SECTOR, 8, |r: &SectorRow, i| match i {
        0 => sv(&r.sector),
        1 => n(r.n as f64),
        2 => n(r.adv as f64),
        3 => n(r.dec as f64),
        4 => (r.n > 0).then(|| r.adv as f64 / r.n as f64).map_or(SortVal::None, SortVal::N),
        5 => on(r.wtd_chg),
        // 排的是**覆盖比例**不是绝对条数：1086/1688 比 193/195 差得多，
        // 而按条数排前者反而在前
        6 => (r.n > 0).then(|| r.wtd_n as f64 / r.n as f64).map_or(SortVal::None, SortVal::N),
        7 => on(r.median_chg),
        _ => n(r.turnover),
    }) {
        let sr = &sr;
        let frac = (sr.n > 0).then(|| sr.adv as f64 / sr.n as f64);
        col = col.push(
            row![
                cell(clip(&sr.sector, 22), 190.0, C_TXT, false),
                cell(sr.n.to_string(), 60.0, C_DIM, true),
                cell(sr.adv.to_string(), 60.0, up, true),
                cell(sr.dec.to_string(), 72.0, dn, true),
                container(bar(frac, 14, up)).width(Length::Fixed(150.0)),
                cell(
                    sr.wtd_chg.map(|x| format!("{x:+.2}%")).unwrap_or_else(|| "—".into()),
                    88.0,
                    sr.wtd_chg.map_or(C_DIM, sign),
                    true
                ),
                // 加权只覆盖了几只——与「只数」差很多时那个加权值代表性就差
                cell(
                    format!("{}/{}", sr.wtd_n, sr.n),
                    76.0,
                    if sr.n > 0 && sr.wtd_n * 4 < sr.n * 3 { C_GOLD } else { C_DIM },
                    true
                ),
                cell(
                    sr.median_chg.map(|x| format!("{x:+.2}%")).unwrap_or_else(|| "—".into()),
                    80.0,
                    sr.median_chg.map_or(C_DIM, sign),
                    true
                ),
                cell(usd(sr.turnover), 96.0, C_TXT, true),
            ]
            .spacing(3),
        );
    }

    col.into()
}

/// 新股上市。**放在整屏最上面**：它是这一屏里唯一带日程的一块，
/// 而且是可以按月往回翻的——埋在四张榜下面的话每次都得滚到底。
fn equity_ipos<'a>(p: &EquityPanorama) -> Element<'a, RadarMsg> {
    let mut col = column![].spacing(3);
    col = col.push(section("新股上市", "纳斯达克新股日历：已定价 / 待上市 / 已申报 / 已撤回"));

    // 月份选择。显示的是**守护实际查回来的**月份，不是面板请求的那个：
    // 换月的那一两轮里两者不一样，显示请求值会让人以为已经换好了
    let want = super::radar::ipo_month();
    super::radar_readout::set_ipo_month(&want);
    let got = if p.month.is_empty() { "—" } else { p.month.as_str() };
    let mut mr = row![text("月份 ").size(10).color(C_DIM)].spacing(3);
    mr = mr.push(chip("‹ 上一月", false, RadarMsg::IpoMonth(-1)));
    mr = mr.push(
        container(text(got.to_string()).size(11).color(C_TXT)).width(Length::Fixed(76.0)),
    );
    mr = mr.push(chip("下一月 ›", false, RadarMsg::IpoMonth(1)));
    if want != p.month && !p.month.is_empty() {
        mr = mr.push(text(format!("　查询 {want} 中…")).size(10).color(C_GOLD));
    }
    col = col.push(mr.align_y(iced::Alignment::Center));

    if p.ipos.is_empty() {
        // 空月份要说清是「这个月没有」，别让人以为是抓取坏了
        col = col.push(text(format!("{got} 没有新股记录")).size(11).color(C_DIM));
        return col.into();
    }
    const COLS: [Hd; 8] = [
        ("代号", 76.0, false),
        ("公司", 280.0, false),
        ("状态", 76.0, false),
        ("交易所", 150.0, false),
        ("发行价", 88.0, true),
        ("股数", 116.0, true),
        ("募资额", 130.0, true),
        ("日期", 104.0, true),
    ];
    col = col.push(sort_head(tbl::EQ_IPO, &COLS, NO_SORT));
    for r in sort_rows(&p.ipos, tbl::EQ_IPO, NO_SORT, |r: &IpoRow, i| match i {
        0 => sv(&r.symbol),
        1 => sv(&r.company),
        2 => sv(&r.status),
        3 => sv(&r.exchange),
        // 这三列在原表里是带 $ 和千位逗号的串，排序要按**数**来
        4 => money_num(&r.price),
        5 => money_num(&r.shares),
        6 => money_num(&r.value),
        _ => mdy_num(&r.date),
    }) {
        col = col.push(
            row![
                link_cell(r.symbol.clone(), &r.url, 76.0, C_TXT, false),
                link_cell(clip(&r.company, 34), &r.url, 280.0, C_TXT, false),
                cell(r.status.clone(), 76.0, C_GOLD, false),
                cell(clip(&r.exchange, 18), 150.0, C_DIM, false),
                cell(dash(&r.price), 88.0, C_DIM, true),
                cell(dash(&r.shares), 116.0, C_DIM, true),
                cell(dash(&r.value), 130.0, C_TXT, true),
                cell(dash(&r.date), 104.0, C_DIM, true),
            ]
            .spacing(3),
        );
    }
    col.into()
}

/// `"$100,000,000"` / `"2,066,243"` → 数。排序用——按字符串排会让
/// `$34,500,000` 排在 `$100,000,000` 前面。
fn money_num(s: &str) -> SortVal {
    let t: String = s.chars().filter(|c| c.is_ascii_digit() || *c == '.').collect();
    t.parse::<f64>().map_or(SortVal::None, SortVal::N)
}

/// `"9/03/2026"` → 可比的数。按字符串排会让 `10/01` 排在 `9/03` 前面。
fn mdy_num(s: &str) -> SortVal {
    let p: Vec<&str> = s.split('/').collect();
    if p.len() != 3 {
        return SortVal::None;
    }
    match (p[2].parse::<f64>(), p[0].parse::<f64>(), p[1].parse::<f64>()) {
        (Ok(y), Ok(m), Ok(d)) => SortVal::N(y * 10_000.0 + m * 100.0 + d),
        _ => SortVal::None,
    }
}

/// 宏观 + 新闻（docs/22 §7 第二批）。
fn macro_view<'a>(m: &MacroBoard, v: ViewState, fetched: i64) -> Element<'a, RadarMsg> {
    let mut col = column![].spacing(3);
    if m.sources.is_empty() {
        return text("暂无宏观数据——守护还没跑完第一轮（默认 1 小时）")
            .size(11)
            .color(C_DIM)
            .into();
    }
    let up = ramp(v.palette).up[2];
    let dn = ramp(v.palette).down[2];

    // 「抓取时刻」与「观测日期」是两回事：这里一小时抓一次，但抓回来的
    // 通胀数据本身可能是九个月前公布的。两个都要显示，只显示前者更误导
    col = col.push(fresh_bar(
        Tier::Periodic,
        "央行/统计局按期公布，不是行情。**抓取时刻 ≠ 观测日期**——见每行的「观测」列",
        fetched,
        "1 小时",
        Some(blk::MACROS),
    ));
    col = col.push(
        text("⚠ 每行的观测日期都不一样：政策利率是当天、通胀是上个月、世行数据是去年。不看「观测」列就会把去年的数当今天的。")
            .size(10)
            .color(C_GOLD),
    );
    let mut sr = row![text("来源 ").size(10).color(C_DIM)].spacing(8);
    for s in &m.sources {
        sr = sr.push(
            // **条数和错误都要显示**：欧央行五条序列里超时一条时，只显示错误
            // 会让人以为整个来源挂了，其实四条都在表里。
            // 错误文案要截断——接口返回的长英文原样铺开会把整行挤出屏幕，
            // 把后面几个来源的状态一起顶没
            text(match (s.rows, s.err.is_empty()) {
                (_, true) => format!("{} {}", s.label, s.rows),
                (0, false) => format!("{} · {}", s.label, clip(&s.err, 42)),
                (n, false) => format!("{} {n} · 部分失败：{}", s.label, clip(&s.err, 34)),
            })
            .size(10)
            // 「未配置」是等用户去配，不是故障——用暗色，别拿警告色喊
            .color(if s.err.is_empty() {
                C_DIM
            } else if s.err.starts_with("未配置") {
                // 「未配置」是等用户去配，不是故障
                C_NEUTRAL
            } else if s.rows > 0 {
                // 部分失败：还有数据，别用整块失败那个颜色喊
                C_GOLD
            } else {
                C_BAD
            }),
        );
    }
    col = col.push(sr);

    col = col.push(section("宏观指标", "变化列是与上一期之差；世行是年频，没有上一期。点指标名开原站页面"));
    const MACRO_COLS: [Hd; 7] = [
        ("来源", 96.0, false),
        ("地区", 130.0, false),
        ("指标", 200.0, false),
        ("数值", 128.0, true),
        ("单位", 60.0, false),
        ("较上期", 96.0, true),
        ("观测", 104.0, true),
    ];
    // 默认**不排序**：守护是按「欧央行 → 劳工统计局 → 世行」分块给的，
    // 同一来源的几行挨在一起才好读；按数值拍平会把利率和 GDP 增速混在一起
    col = col.push(sort_head(tbl::MACRO_ROWS, &MACRO_COLS, NO_SORT));
    for r in sort_rows(&m.rows, tbl::MACRO_ROWS, NO_SORT, |r: &MacroRow, i| match i {
        0 => sv(&r.source),
        1 => sv(&r.area),
        2 => sv(&r.label),
        3 => n(r.value),
        4 => sv(&r.unit),
        5 => on(r.chg),
        // 观测日期是 `2026-09-07` / `2026-08` / `2025` 三种粒度混在一起。
        // 按字符串排刚好是对的（都是零填充的 ISO 前缀）
        _ => sv(&r.obs),
    }) {
        let r = &r;
        col = col.push(
            row![
                cell(r.source.clone(), 96.0, C_DIM, false),
                cell(clip(&r.area, 14), 130.0, C_TXT, false),
                link_cell(clip(&r.label, 22), &r.url, 200.0, C_TXT, false),
                cell(format!("{:.3}", r.value), 128.0, C_TXT, true),
                cell(r.unit.clone(), 60.0, C_DIM, false),
                cell(
                    r.chg.map(|x| format!("{x:+.3}")).unwrap_or_else(|| "—".into()),
                    96.0,
                    r.chg.map_or(C_DIM, |x| if x >= 0.0 { up } else { dn }),
                    true
                ),
                // 这一列是整张表能不能读的关键，用高亮色
                cell(r.obs.clone(), 104.0, C_HEAD, true),
            ]
            .spacing(3),
        );
    }

    col = col.push(section("央行与市场新闻", "欧洲央行 / 美联储 RSS 无需 key；EODHD 需配 token。点标题在浏览器里看原文"));
    col = col.push(days_bar(
        tbl::MACRO_NEWS,
        0,
        &[(1, "今天"), (7, "近 7 天"), (30, "近 30 天"), (0, "全部")],
    ));
    let days = super::radar::table_days(tbl::MACRO_NEWS, 0);
    const NEWS_COLS: [Hd; 3] =
        [("来源", 116.0, false), ("标题", 820.0, false), ("时间", 220.0, false)];
    col = col.push(sort_head(tbl::MACRO_NEWS, &NEWS_COLS, 2));
    let kept: Vec<NewsRow> =
        m.news.iter().filter(|x| within_days(x.ts_ms, days)).cloned().collect();
    if kept.is_empty() && !m.news.is_empty() {
        col = col.push(
            text(format!("这段时间内没有新闻（共 {} 条，放宽时间范围看看）", m.news.len()))
                .size(11)
                .color(C_DIM),
        );
    }
    for x in sort_rows(&kept, tbl::MACRO_NEWS, 2, |x: &NewsRow, i| match i {
        0 => sv(&x.source),
        1 => sv(&x.title),
        // 按**时间戳**排，不按那个显示串——两个源的时间格式不一样
        // （RFC822 带时区名），字符串排出来是按星期几排的
        _ => n(x.ts_ms as f64),
    }) {
        col = col.push(
            row![
                cell(x.source.clone(), 116.0, C_DIM, false),
                link_cell(clip(&x.title, 96), &x.link, 820.0, C_TXT, false),
                cell(x.published.clone(), 220.0, C_DIM, false),
            ]
            .spacing(3),
        );
    }
    col.into()
}

/// 空串显示成「—」。新股日历里发行价/募资额常年为空，留白会让人以为是渲染坏了。
fn dash(s: &str) -> String {
    if s.trim().is_empty() { "—".into() } else { s.to_string() }
}

/// 小节标题 + 一句口径说明。口径写在标题旁边而不是文档里——
/// 看板上的数字要能自己解释自己。
fn section<'a>(title: &str, note: &str) -> Element<'a, RadarMsg> {
    let mut r = row![text(format!("▍{title}")).size(12).color(C_HEAD)].spacing(8);
    if !note.is_empty() {
        r = r.push(text(note.to_string()).size(10).color(C_DIM));
    }
    container(r).padding([6, 0]).into()
}

/// 毫秒 → `MM-DD HH:MM`。0 或负数当没有。
fn day_cell(ms: i64) -> String {
    use chrono::TimeZone;
    if ms <= 0 {
        return "—".into();
    }
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format("%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "—".into())
}

/// 按**字符数**截断（不是字节）：中文标题按字节切会切出半个字。
fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    s.chars().take(n.saturating_sub(1)).collect::<String>() + "…"
}

/// World Overview（docs/22 §2 ②）：各国指数横向对比，本币 vs 美元并列。
fn overview_view<'a>(rows: &[OverviewRow], v: ViewState, fetched: i64) -> Element<'a, RadarMsg> {
    let mut col = column![].spacing(3);
    if rows.is_empty() {
        return text("暂无总览数据——需开启股票层（radar.toml 的 [equities]）")
            .size(11)
            .color(C_DIM)
            .into();
    }
    // TradingView 的指数报价是延迟档（docs/22 §0）。总览一屏全是它
    col = col.push(fresh_bar(
        Tier::Delayed,
        "TradingView 指数报价 · 各国指数按美元归一化",
        fetched,
        "60 秒",
        Some(blk::SLOW),
    ));
    col = col.push(
        text("各国指数 · 本币 / 美元并列。本币计价的国家横比是假的——指数涨而本币贬，对美元投资者可能是亏的。")
            .size(10)
            .color(C_GOLD),
    );
    let mut hdr = row![cell("指数".into(), 150.0, C_HEAD, false), cell("币".into(), 40.0, C_HEAD, false)]
        .spacing(3);
    for w in OV_WINDOWS {
        hdr = hdr.push(cell(format!("{w} 本币"), 82.0, C_HEAD, true));
        hdr = hdr.push(cell(format!("{w} 美元"), 82.0, C_HEAD, true));
    }
    col = col.push(hdr);
    for r in rows {
        let mut tr = row![
            cell(r.label.clone(), 150.0, C_TXT, false),
            cell(r.currency.clone(), 40.0, C_DIM, false)
        ]
        .spacing(3);
        for i in 0..OV_WINDOWS.len() {
            tr = tr.push(cell(
                pct_log(r.local[i]),
                82.0,
                scale_text(r.local[i].map(|x| (x.exp() - 1.0) * 100.0), "change", STOCK_SCALE, v.palette, true),
                true,
            ));
            // 美元口径缺席就是缺席——绝不退回本币值
            tr = tr.push(cell(
                pct_log(r.usd[i]),
                82.0,
                scale_text(r.usd[i].map(|x| (x.exp() - 1.0) * 100.0), "change", STOCK_SCALE, v.palette, true),
                true,
            ));
        }
        col = col.push(tr);
    }
    col.into()
}

/// 市场宽度（docs/22 §2 ③）。
fn breadth_view<'a>(rows: &[BreadthRow], v: ViewState, fetched: i64) -> Element<'a, RadarMsg> {
    let mut col = column![].spacing(3);
    if rows.is_empty() {
        return text("暂无宽度数据——需开启股票层（radar.toml 的 [equities]）")
            .size(11)
            .color(C_DIM)
            .into();
    }
    col = col.push(fresh_bar(
        Tier::Delayed,
        "TradingView 全市场扫描 · 每市场独立取前 N 只，不带用户筛选",
        fetched,
        "60 秒",
        Some(blk::SLOW),
    ));
    // 覆盖率**逐行显示**（下面「样本」那一列），不再笼统说「是前 N 只」——
    // 澳大利亚 100% 与美国 28% 的可信度完全不同，一句话概括不了
    col = col.push(
        text("⚠ 每市场按市值取前 N 只算，覆盖率见「样本」列。覆盖不足时这是大盘股宽度，不是全市场 A/D。")
            .size(10)
            .color(C_GOLD),
    );
    col = col.push(
        text("热图说今天谁红谁绿；宽度说这个市场是健康上涨，还是靠几只权重撑着。")
            .size(10)
            .color(C_DIM),
    );
    let up = ramp(v.palette).up[2];
    let dn = ramp(v.palette).down[2];
    let mut hdr = row![].spacing(3);
    for (t, w) in [
        ("市场", 92.0),
        ("样本", 130.0),
        ("涨", 46.0),
        ("跌", 46.0),
        ("涨跌比", 56.0),
    ] {
        hdr = hdr.push(cell(t.into(), w, C_HEAD, t != "市场"));
    }
    hdr = hdr.push(cell("上涨占比".into(), 172.0, C_HEAD, false));
    hdr = hdr.push(cell("站上 MA200".into(), 172.0, C_HEAD, false));
    hdr = hdr.push(cell("新高".into(), 46.0, C_HEAD, true));
    hdr = hdr.push(cell("新低".into(), 46.0, C_HEAD, true));
    hdr = hdr.push(cell("净新高".into(), 56.0, C_HEAD, true));
    col = col.push(hdr);

    for b in rows {
        let ratio = b
            .ad_ratio
            .map(|x| format!("{x:.2}"))
            .unwrap_or_else(|| "—".into());
        let net_c = if b.net_new_high > 0 {
            up
        } else if b.net_new_high < 0 {
            dn
        } else {
            C_DIM
        };
        col = col.push(
            row![
                cell(b.market.clone(), 92.0, C_TXT, false),
                // 「样本/总数 覆盖率」——覆盖 28% 与 100% 的可信度差得远，
                // 只显示样本数看不出这一点
                cell(
                    match b.coverage {
                        Some(c) => format!("{}/{} {:.0}%", b.n, b.total, c * 100.0),
                        None => b.n.to_string(),
                    },
                    130.0,
                    // 覆盖不足一半的标黄：那更接近「大盘股宽度」
                    if b.coverage.is_some_and(|c| c < 0.5) { C_GOLD } else { C_DIM },
                    true,
                ),
                cell(b.adv.to_string(), 46.0, up, true),
                cell(b.dec.to_string(), 46.0, dn, true),
                cell(ratio, 56.0, C_TXT, true),
                container(bar(b.adv_pct, 16, up)).width(Length::Fixed(172.0)),
                container(bar(b.above_ma200_pct, 16, up)).width(Length::Fixed(172.0)),
                cell(b.new_high.to_string(), 46.0, up, true),
                cell(b.new_low.to_string(), 46.0, dn, true),
                cell(format!("{:+}", b.net_new_high), 56.0, net_c, true),
            ]
            .spacing(3),
        );
    }
    col.into()
}

// ───────────────────────── 面板体 ─────────────────────────

pub fn pane_body<'a>() -> Element<'a, RadarMsg> {
    // Arc：每帧只是引用计数加一，不再深拷贝整份读数（见 snapshot 的说明）
    let st = super::radar_readout::snapshot();
    let v = super::radar::view();
    let mut body = column![].spacing(6).padding(10);

    body = body.push(
        text("全市场雷达 · 加密热层（docs/22 · 发现工具·非交易信号）")
            .size(14)
            .color(C_HEAD),
    );

    // ── 守护控制条 ──
    let running = st.svc.active;
    body = body.push(
        row![
            text("守护 ").size(11).color(C_DIM),
            chip("▶ 启动", running, RadarMsg::Start),
            chip("■ 停止", !running, RadarMsg::Stop),
            text(format!("　{}", if running { "运行中" } else { "未运行" }))
                .size(11)
                .color(if running { C_OK } else { C_DIM }),
            text("　").size(11),
            button(text("⟳ 立即获取").size(11)).padding([2, 7]).on_press(RadarMsg::Refresh),
            text(format!("  刷新于 {}", st.refreshed)).size(10).color(C_DIM),
        ]
        .spacing(4)
        .align_y(iced::Alignment::Center),
    );
    let am = super::radar::action_message();
    if !am.is_empty() {
        let bad = am.starts_with('✗');
        body = body.push(text(am).size(10).color(if bad { C_BAD } else { C_OK }));
    }

    if !st.present || st.rows.is_empty() {
        body = body.push(
            text("暂无快照——点上方「▶ 启动」拉起 ws-radar 守护（首轮约 5s 出数据）")
                .size(11)
                .color(C_DIM),
        );
        return scrollable(body).width(Length::Fill).height(Length::Fill).into();
    }

    // ── 状态行：数据等级 + 热身 + 回填（docs/22 §0：不标等级 = 自欺）──
    let warm = st.rows.iter().filter(|r| r.trustworthy()).count();
    let prov = st.rows.iter().filter(|r| r.z_provisional).count();
    // 逐行 tier 的构成。一张表里混着 A 档实时加密和 C 档延迟股票，
    // 只显示一个「等级 A」会让人把整张表当成实时的。
    let mut tiers: Vec<(String, usize)> = {
        let mut m = std::collections::BTreeMap::new();
        for r in &st.rows {
            *m.entry(if r.tier.is_empty() { "?".to_string() } else { r.tier.clone() })
                .or_insert(0usize) += 1;
        }
        m.into_iter().collect()
    };
    tiers.sort_by(|a, b| a.0.cmp(&b.0));
    // 上一版把等级拼成一个字符串（`A×693 C×5960`）。改成逐档徽章之后
    // 那个串就没人用了——顺手删掉，留着会让下次读代码的人找它的用处
    // 这一屏**一张表里混着两档**：加密是交易所直连的实时价，股票是
    // TradingView 的延迟报价。只标一个等级会让人把整张表当成同一档，
    // 故徽章按行数分档列出来，两层的更新时间也分开显示
    let mut tr = row![].spacing(6).align_y(iced::Alignment::Center);
    for (t, n) in &tiers {
        let tier = match t.as_str() {
            "A" => Tier::Live,
            "B" => Tier::NearLive,
            "D" => Tier::Close,
            _ => Tier::Delayed,
        };
        tr = tr.push(
            container(
                text(format!("{} ×{n}", tier.badge())).size(10).color(tier.color()),
            )
            .padding([1, 5])
            .style(move |_: &Theme| container::Style {
                border: iced::Border { color: tier.color(), width: 1.0, radius: 3.0.into() },
                ..Default::default()
            }),
        );
    }
    tr = tr.push(
        text(format!(
            "加密层 {}{} · 股票层 {}{}",
            if st.stamp.is_empty() { "—" } else { &st.stamp },
            super::staleness::suffix(&st.stamp),
            if st.slow_stamp.is_empty() { "—" } else { &st.slow_stamp },
            super::staleness::suffix(&st.slow_stamp),
        ))
        .size(10)
        .color(C_DIM),
    );
    tr = tr.push(chip("⟳ 立即刷新", false, RadarMsg::Refresh));
    body = body.push(tr);
    body = body.push(
        text(format!(
            "{} 标的 · 单轮 {}ms · z 可信 {}/{}{}",
            st.n_symbols,
            st.refreshed_ms,
            warm,
            st.rows.len(),
            if prov > 0 {
                format!(" · {prov} 行借横截面基线")
            } else {
                String::new()
            }
        ))
        .size(10)
        .color(if warm == 0 { C_GOLD } else { C_DIM }),
    );
    let bf = &st.backfill;
    if bf.running {
        body = body.push(
            text(format!(
                "⟳ σ 回填中 {}/{}（{:.0}%）{}",
                bf.done,
                bf.total,
                bf.pct() * 100.0,
                if bf.failed > 0 { format!("　失败 {}", bf.failed) } else { String::new() }
            ))
            .size(10)
            .color(C_HEAD),
        );
    } else if warm == 0 {
        body = body.push(
            text(if bf.finished {
                "⏳ 回填已完成但仍无可信 z——检查 K 线端点（journalctl --user -u ws-radar）"
            } else {
                "⏳ 尚未热身，z 值不可信（等待表见 docs/22 §8.1）"
            })
            .size(10)
            .color(C_GOLD),
        );
    }

    // ── 视图切换 ──
    let mut mr = row![text("视图 ").size(11).color(C_DIM)].spacing(3);
    for (m, l) in [
        (ViewMode::Heatmap, "热图"),
        (ViewMode::Screener, "筛选器"),
        (ViewMode::Overview, "全球总览"),
        (ViewMode::Breadth, "市场宽度"),
        (ViewMode::Crypto, "加密全景"),
        (ViewMode::Prediction, "预测市场"),
        (ViewMode::Equity, "股票全景"),
        (ViewMode::Macro, "宏观新闻"),
    ] {
        mr = mr.push(chip(l, m == v.mode, RadarMsg::SetMode(m)));
    }
    body = body.push(mr.align_y(iced::Alignment::Center));
    // 表达形式**单独一行**（官方页面左上角那三个小图标）。热图页没有这个
    // 选择——它本来就是热图。
    if v.mode == ViewMode::Screener {
        let mut fr = row![text("形式 ").size(11).color(C_DIM)].spacing(3);
        for (f, l) in [(Form::Table, "表格"), (Form::Heatmap, "热图")] {
            fr = fr.push(chip(l, f == v.form, RadarMsg::SetForm(f)));
        }
        body = body.push(fr.align_y(iced::Alignment::Center));
    }

    if !matches!(v.mode, ViewMode::Heatmap | ViewMode::Screener) {
        // 动作回执（打开链接、启停守护）**要在提前返回之前推进去**：
        // 原来它挂在下面热图那一段里，四个新看板点了链接后没有任何反馈
        let am = super::radar::action_message();
        if !am.is_empty() {
            let bad = am.starts_with('✗') || am.starts_with("拒绝") || am.starts_with("打开失败");
            body = body.push(text(am).size(10).color(if bad { C_BAD } else { C_OK }));
        }
        let inner = match v.mode {
            ViewMode::Overview => overview_view(&st.overview, v, st.fetched.overview),
            ViewMode::Crypto => crypto_view(&st.panorama, v, st.fetched.panorama),
            ViewMode::Prediction => prediction_view(&st.prediction, v, st.fetched.prediction),
            ViewMode::Equity => equity_view(&st.equity, v, st.fetched.equity),
            ViewMode::Macro => macro_view(&st.macros, v, st.fetched.macros),
            _ => breadth_view(&st.breadth, v, st.fetched.breadth),
        };
        body = body.push(inner);
        // 用**慢层**的快照时间：这几屏的数据都来自慢层文件，
        // 显示热层时间（5 秒一刷）会让人以为它们也那么快
        body = body.push(
            text(format!(
                "慢层快照 {}{}",
                if st.slow_stamp.is_empty() { "—" } else { &st.slow_stamp },
                super::staleness::suffix(&st.slow_stamp),
            ))
            .size(10)
            .color(C_DIM),
        );
        return scrollable(body).width(Length::Fill).height(Length::Fill).into();
    }

    // 来源与资产类对不上时（例如从股票切过来时留下的市场码），按「全部」处理。
    // **只在下拉里回退显示是不够的**：下拉会显示成「全部」，而过滤仍按旧值走，
    // 于是「界面看着正常、表格一行都没有」——这是最难排查的一种坏。
    let v = ViewState { source: effective_source(&st.catalog, v), ..v };

    // ── 资产类过滤 ──
    // 股票的成交额远大于加密，同图时加密会被挤到几乎看不见（实测 BTC 只剩一个小格）。
    let vis = memo_visible(st.generation, v, &st.rows, &st.catalog.coin_presets);
    let mut ar = row![text("资产 ").size(11).color(C_DIM)].spacing(3);
    // 每个视图允许的资产类不同：官方热图只有 股票/ETF/加密，筛选器有六类。
    // 把两套混在一起就是原来「下拉一半无效」的根源。
    let kinds: &[AssetFilter] = if v.mode == ViewMode::Screener {
        &AssetFilter::SCREENER
    } else {
        &AssetFilter::HEATMAP
    };
    for &a in kinds {
        ar = ar.push(chip(a.label(), a == v.asset, RadarMsg::SetAsset(a)));
    }
    // 「来源」下拉（TV 的「来源」）。列的是**全部可选项**（随快照下发的目录），
    // 不是已加载的 venue——只列已加载的话，用户永远只能在守护恰好在拉的
    // 那几个市场里打转。选了没在拉的市场会通过 radar_request.json 通知守护去拉。
    let crypto_mode = matches!(v.asset, AssetFilter::Coin | AssetFilter::Cex | AssetFilter::Dex);
    let mut opts: Vec<MarketOpt> = vec![MarketOpt::all(crypto_mode)];
    if crypto_mode {
        // **加密用官方那 13 项精选来源**，不是原始分类码。官方给的是
        // 「加密货币 / 不含比特币 / 不含稳定币 / DeFi 币 / Layer 1 代币…」，
        // 其中三项是全域变体、「游戏和元宇宙」还对应两个分类。
        for it in &st.catalog.coin_presets {
            if !it.code.is_empty() {
                opts.push(MarketOpt::of(&it.code, &it.label, "", true));
            }
        }
        // 目录没给（旧守护）才退回原始分类码
        if st.catalog.coin_presets.is_empty() {
            for it in &st.catalog.crypto_cats {
                if !it.code.is_empty() {
                    opts.push(MarketOpt::of(&it.code, &it.label, "", true));
                }
            }
        }
    } else {
        // 已加载的来源 id（venue 中段）——用于在下拉里标注哪些是现成的
        let loaded: std::collections::HashSet<&str> = st
            .rows
            .iter()
            .filter_map(|r| r.venue.split(':').nth(1))
            .collect();
        // 官方来源下拉的组织（用户录屏逐项核对）：
        //   股票  **按国家分组**，组内**指数在前、「所有 X 公司」在后**
        //   ETF   **扁平**：没有指数、条目就是国名本身（不是「所有 X 公司」）
        //
        // 国家之间的顺序由守护下发时排好（英文国名 A→Z，股票那份把中国置顶、
        // ETF 那份不置顶），面板照单铺开，不自己排。
        let etf = v.asset == AssetFilter::Etf;
        // **筛选器与热图是两个控件**（docs/22 §6.32）：
        //   热图「来源」  60 国 + 指数 + 欧盟/欧洲，英文国名序
        //   筛选器「市场」 71 国 + 全球，拼音序，**没有指数**
        // 一度共用一份清单，于是筛选器少了 11 个国家和「全球」。
        let screener = v.mode == ViewMode::Screener && !st.catalog.screener_markets.is_empty();
        let pick = |codes: &[String]| -> Vec<&CatalogItem> {
            codes
                .iter()
                .filter_map(|c| st.catalog.markets.iter().find(|m| &m.code == c))
                .collect()
        };
        let ordered: Vec<&CatalogItem> = if screener {
            st.catalog.screener_markets.iter().collect()
        } else if etf {
            pick(&st.catalog.etf_markets)
        } else {
            st.catalog.markets.iter().collect()
        };
        // 官方把「欧洲」组下的两项（所有欧盟公司 / 所有欧洲公司）放在同一个组名
        // 底下，且**指数在前、两个「所有」在后**。所以按组名切分连续段，
        // 段内先铺完所有指数、再铺所有「所有 X 公司」——单市场的组走同一条路。
        let mut i = 0;
        while i < ordered.len() {
            let mut j = i;
            while j < ordered.len() && ordered[j].label == ordered[i].label {
                j += 1;
            }
            let group = &ordered[i..j];
            // 指数只在热图的「来源」里；筛选器的「市场」下拉没有指数
            if !etf && !screener {
                for m in group {
                    for ix in st.catalog.indices.iter().filter(|x| x.region == m.code) {
                        opts.push(MarketOpt::of(
                            &ix.code,
                            &ix.label,
                            &m.label,
                            loaded.contains(ix.code.as_str()),
                        ));
                    }
                }
            }
            for m in group {
                let label = all_entry_label(etf, m);
                opts.push(MarketOpt::of(
                    &m.code,
                    &label,
                    &m.label,
                    loaded.contains(m.code.as_str()),
                ));
            }
            i = j;
        }
    }

    let cur = opts
        .iter()
        .find(|o| o.key == v.source)
        .copied()
        .unwrap_or_else(|| MarketOpt::all(crypto_mode));

    // 把选中的来源写给守护——选了没在拉的市场，下一轮就会去取
    if !crypto_mode {
        // 筛选一并下发：能下推的那些由服务端在**整个市场**上筛，
        // 而不是在守护已抓回的样本里筛
        super::radar_readout::write_request(
            v.source,
            &["stock", "fund"],
            &radar_filter::wire(&v, radar_filter::now_s()),
        );
    }
    ar = ar.push(text("　来源 ").size(11).color(C_DIM));
    ar = ar.push(
        pick_list(opts, Some(cur), |o: MarketOpt| RadarMsg::SetSource(o.key))
            .text_size(11)
            .padding([2, 6]),
    );
    // 选了一个守护还没在拉的来源时，等的是守护下一次抓取。放个按钮在下拉
    // 旁边，让「我要现在就看」有个明确的入口，而不是干等。
    ar = ar.push(text(" ").size(11));
    ar = ar.push(
        button(text("⟳ 立即获取").size(11)).padding([2, 6]).on_press(RadarMsg::Refresh),
    );
    ar = ar.push(
        text(format!("　{} / {} 行", vis.len(), st.rows.len()))
            .size(10)
            .color(C_DIM),
    );
    body = body.push(ar.align_y(iced::Alignment::Center));
    // 筛选栏放在空表判断**之前**——被筛空时也得有清除的入口。
    // 热图页不出筛选：官方热图没有筛选栏，靠「来源」选范围
    if v.mode != ViewMode::Heatmap {
        body = body.push(filter_bar(v, &st.catalog));
    }
    if vis.is_empty() {
        let nf = radar_filter::active_count(&v);
        body = body.push(
            text(if nf > 0 {
                // 空表最常见的原因就是筛选，而不是没数据——直接说，并给出清除入口
                format!("{nf} 个筛选把 {} 行全筛掉了——点上面的「清空」恢复", st.rows.len())
            } else if v.source.is_empty() {
                "该资产类暂无数据——股票层需在 radar.toml 的 [equities] 里开启".to_string()
            } else {
                // 抓完了才说结论，没抓完只说在抓——不区分的话，
                // 一个本来就没有标的的来源会一直显示「正在抓取」
                match st.progress.of(v.source) {
                    Some(f) if f.done && !f.err.is_empty() => {
                        format!("「{}」抓取失败：{}", cur, f.err)
                    }
                    Some(f) if f.done => format!(
                        "「{}」已抓取，但没有符合条件的标的（0 行）",
                        cur
                    ),
                    _ => format!("「{}」正在抓取", cur),
                }
            })
            .size(11)
            .color(C_GOLD),
        );
        // 干等而没有任何反馈时，人分不清是「在抓」还是「卡住了」。
        // 进度条 + 数字 + 倒计时，三样都给。**抓完了就不再显示**——
        // 一直转的进度条会让「这个来源本来就是空的」看着像卡死。
        let settled = st.progress.of(v.source).is_some_and(|f| f.done);
        if nf == 0 && !v.source.is_empty() && !settled {
            body = body.push(fetch_progress(&st.progress));
        }
        return scrollable(body).width(Length::Fill).height(Length::Fill).into();
    }

    // 是否要画热图：热图页恒画；筛选器页只有选了「热图」形式才画。
    let draw_map = v.mode == ViewMode::Heatmap || v.form == Form::Heatmap;

    // 「窗口」那一行去掉了：它只服务雷达自有的涨跌幅/速度 z 列组，而那三组
    // 已按官方原样移除（官方筛选器没有窗口切换）。窗口仍在内部用于 z 值计算。

    // 「大小/颜色」的可选项**跟着资产类走**：股票 market_cap_basic、
    // 加密 market_cap_calc、DEX dex_total_liquidity——共用一套的话
    // 下拉里一半取不到值。目录没给这一类就退回雷达自有的那套。
    let (size_list, color_list) = match asset_opts(&st.catalog, v.asset) {
        Some((sz, cl)) => (
            sz.iter().map(|i| Opt { key: intern(&i.code), label: intern(&i.label) }).collect::<Vec<_>>(),
            // **不再插入雷达自有的「涨跌速度 z」**：颜色口径按官方原样。
            // 速度 z 仍在表格的「速度 z」列组里，那才是它该在的地方。
            cl.iter().map(|i| Opt { key: intern(&i.code), label: intern(&i.label) }).collect::<Vec<_>>(),
        ),
        None => (SIZE_OPTS.to_vec(), COLOR_OPTS.to_vec()),
    };
    // 换资产类后原来选中的项可能不在新清单里。**回退的结果要同时用于绘图**，
    // 不能只用来填下拉：加密的市值键是 `market_cap_calc`、股票是
    // `market_cap_basic`，带着股票的键切过去每个方块权重都是 0，
    // 下拉却显示着一个正常的口径——又是「看着正常、内容是空的」。
    let cur_size = size_list.iter().find(|o| o.key == v.size.key).copied()
        .unwrap_or_else(|| size_list[0]);
    let cur_color = color_list.iter().find(|o| o.key == v.color.key).copied()
        .unwrap_or_else(|| color_list[0]);
    let v = ViewState { size: cur_size, color: cur_color, ..v };

    // ── 热图控制条 ──
    //
    // **四个下拉一排**，顺序同官方页面：来源 / 大小 / 颜色 / 分组。
    // 「来源」已在上面的资产行里（它同时兼作资产类的范围选择）。
    // 分组维度由目录按资产类下发——官方股票只有「没有分组 / 板块」，
    // ETF 是「没有分组 / 资产类别」，加密**根本没有分组下拉**。
    if draw_map {
        let mut r1 = row![text("大小 ").size(11).color(C_DIM)].spacing(3);
        r1 = r1.push(
            pick_list(size_list.clone(), Some(cur_size), RadarMsg::SetSize)
                .text_size(11)
                .padding([2, 6])
                .width(Length::Fixed(190.0)),
        );
        r1 = r1.push(text("　颜色 ").size(11).color(C_DIM));
        r1 = r1.push(
            pick_list(color_list.clone(), Some(cur_color), RadarMsg::SetColor)
                .text_size(11)
                .padding([2, 6])
                .width(Length::Fixed(210.0)),
        );
        let groups = asset_groups(&st.catalog, v.asset);
        if !groups.is_empty() {
            r1 = r1.push(text("　分组 ").size(11).color(C_DIM));
            for g in groups {
                let gb = group_of(&g.code);
                r1 = r1.push(chip(&g.label, gb == v.group_by, RadarMsg::SetGroupBy(gb)));
            }
        }
        body = body.push(r1.align_y(iced::Alignment::Center));

        // 色板与色阶**单独一排**
        let mut r2 = row![text("色板 ").size(11).color(C_DIM)].spacing(3);
        for (pal, l) in [
            (Palette::BlueOrange, "蓝橙"),
            (Palette::GreenUp, "绿涨红跌"),
            (Palette::RedUp, "红涨绿跌"),
        ] {
            r2 = r2.push(chip(l, pal == v.palette, RadarMsg::SetPalette(pal)));
        }
        r2 = r2.push(text("　").size(11));
        r2 = r2.push(legend(v.color.key, v.palette, asset_scale(&st.catalog, v.asset)));
        body = body.push(r2.align_y(iced::Alignment::Center));
    }

    let hint = match v.color.key {
        "own:ret_pct" => Some("⚠ 裸涨跌幅跨标的不可比——小市值/低流动标的会占满深色档，对照用"),
        "own:zvol" => Some("ⓘ 量异常只看正尾：负值来自 24 小时前那段量掉出滚动窗口，不代表当前清淡"),
        "Volatility.D" => Some("ⓘ 波动率恒非负，用单向量级色阶（深=波动大），不是涨跌方向"),
        "relative_volume_10d_calc" => Some("ⓘ 相对成交量以 1.0× 为常态，中性档即常态量"),
        "premarket_change" | "postmarket_change" => Some("ⓘ 盘前/盘后只有股票有；加密与休市市场会是空值"),
        _ => None,
    };
    if let Some(h) = hint.filter(|_| draw_map) {
        body = body.push(text(h).size(10).color(C_GOLD));
    }

    // ── 树图 ──
    // 树图选行**按体量**，不按当前排序：热图要回答「整个市场此刻什么样」，
    // 取排序前 N 会让整张图只剩涨幅榜（实测就是满屏一片蓝，看不到任何下跌）。
    // 排序只管下面的 Screener。超过 180 格就小于可辨识尺寸，只会拖慢绘制。
    let picked = memo_picked(st.generation, v, &st.rows, &st.catalog.coin_presets);
    // 选中的大小口径对当前这批行没有数据 → 权重全 0 → 树图整个是空的。
    // 空白且无提示是最难排查的一种坏（实测：切到加密后图没了，因为默认口径是
    // 市值而 Binance 不给市值）。这里说清楚缺的是哪个口径，并给出可用的替代。
    let sized = picked
        .iter()
        .filter(|&&i| metric_value(&st.rows[i], v.size.key, v.win).is_some_and(|x| x > 0.0))
        .count();
    // 只有真要画树图时才提示口径不对——表格模式下这条提示牛头不对马嘴
    if sized == 0 && draw_map {
        let alt = size_list.iter().find(|o| {
            o.key != v.size.key
                && picked.iter().any(|&i| {
                    metric_value(&st.rows[i], o.key, v.win).is_some_and(|x| x > 0.0)
                })
        });
        body = body.push(
            text(match alt {
                Some(a) => format!(
                    "⚠ 当前「大小」口径「{}」在这批标的上没有数据，树图无法绘制——改用「{}」",
                    v.size.label, a.label
                ),
                None => format!(
                    "⚠ 当前「大小」口径「{}」在这批标的上没有数据，树图无法绘制",
                    v.size.label
                ),
            })
            .size(11)
            .color(C_GOLD),
        );
    }
    let mk_tile = |i: usize| {
        let r = &st.rows[i];
        let m = metric_value(r, v.color.key, v.win);
        TileData {
            label: base_asset(&r.symbol).to_string(),
            // 百分数类带 %，z/倍数类不带
            value: if crate::ws::radar::scale_kind(v.color.key) == ScaleKind::AroundOne {
                m.map(|x| format!("{x:.2}×")).unwrap_or_else(|| "—".into())
            } else if v.color.key.starts_with("own:") && v.color.key != "own:ret_pct" {
                opt_z(m)
            } else {
                m.map(|x| format!("{x:+.2}%")).unwrap_or_else(|| "—".into())
            },
            value_compact: if v.color.key.starts_with("own:") && v.color.key != "own:ret_pct" {
                opt_z_compact(m)
            } else {
                m.map(|x| format!("{x:+.1}")).unwrap_or_else(|| "—".into())
            },
            weight: area_weight(r, v),
            color: scale_color(m, v.color.key, row_scale(&st.catalog, r), v.palette, r.trustworthy()),
        }
    };
    /// 一行归入哪个分组。空值统一落到「其他」，免得散成一堆无名组。
    fn group_key(r: &RadarRow, g: GroupBy) -> String {
        let pick = |s: &str, fallback: &str| {
            if s.is_empty() { fallback.to_string() } else { s.to_string() }
        };
        // **分组名要中文**：快照里的 sector / asset_class 是英文原值
        //（`Technology Services`、`Fixed income`），直接拿来当组标题
        // 会在中文界面上突兀，而且宽度也对不上
        match g {
            GroupBy::None => String::new(),
            // 加密没有板块，单独成组而不是混进「其他」
            GroupBy::Sector => {
                zh_sector(&r.sector).unwrap_or_else(|| pick(&r.sector, if r.tier == "A" { "加密" } else { "其他" }))
            }
            // ETF 的资产类别（Equity / Fixed income / Commodity…）走文本段
            GroupBy::AssetClass => {
                let raw = r.t.get("asset_class.tr").map(String::as_str).unwrap_or("");
                zh_asset_class(raw).unwrap_or_else(|| pick(raw, "其他"))
            }
        }
    }

    let (groups, header_h) = match v.group_by {
        GroupBy::None => (
            vec![GroupData {
                title: String::new(),
                tiles: picked.iter().map(|&i| mk_tile(i)).collect(),
            }],
            0.0,
        ),
        g => {
            let mut names: Vec<String> =
                picked.iter().map(|&i| group_key(&st.rows[i], g)).collect();
            names.sort();
            names.dedup();
            let gs = names
                .iter()
                .map(|n| GroupData {
                    title: n.clone(),
                    tiles: picked
                        .iter()
                        .filter(|&&i| &group_key(&st.rows[i], g) == n)
                        .map(|&i| mk_tile(i))
                        .collect(),
                })
                .collect();
            (gs, GROUP_HEADER_H)
        }
    };
    if draw_map {
    body = body.push(
        canvas_widget(TreemapCanvas {
            groups,
            header_h,
            // **只在数据或口径真的变了时清缓存。**
            //
            // 原来是每帧 `Cache::new()`——理由是「Cache 不感知内容变化，复用会
            // 把画面冻住」。那确实是个真问题，但代价是缓存彻底失效：180 个格子
            // 的文字排版 + 填充 + 描边每帧全量重画，主线程直接打满一个核
            // （实测 99.9%）。正确做法不是不缓存，而是给数据一个版本号，
            // 版本或 ViewState 变了才 clear。
            cache: treemap_cache(st.generation, v),
        })
        .width(Length::Fill)
        // 热图页把剩余空间全部铺满；筛选器的热图形式下面还有表格，给固定高度
        .height(if v.mode == ViewMode::Heatmap {
            Length::Fill
        } else {
            Length::Fixed(340.0)
        }),
    );
    }

    // ── Screener（表格）──
    if v.mode == ViewMode::Heatmap {
        // 热图页不带表格。原来两者挤在一个视图里，控制条上一半的下拉
        // 对当前内容无效——那正是「乱」的来源。
        //
        // **不套 scrollable**：滚动容器只给子元素自然高度，`Length::Fill`
        // 在里面不生效，热图会被压成固定的一小条、下面一大片空白。
        return body.width(Length::Fill).height(Length::Fill).into();
    }
    let cols = columns(v, &st.catalog);
    // 切到 TV 列组后，原排序键（如 5m 速度z）可能不在显示的列里——表头上就没有
    // 排序指示，用户看不出表是按什么排的。退到该列组的第一个数值列。
    let mut ev = v;
    if !cols.iter().any(|c| c.key == v.sort)
        && let Some(c) = cols.iter().find(|c| is_numeric(c.key)) {
            ev.sort = c.key;
            ev.desc = true;
        }
    let idx = memo_order(st.generation, ev, &st.rows, &st.catalog.coin_presets);
    // **只列官方的列组**。雷达自有的三组（涨跌幅 / 速度 z / 参考）已移除——
    // 官方筛选器没有它们，摆在一起让人以为是官方的。
    let mut cs = row![text("列组 ").size(11).color(C_DIM)].spacing(3);
    for (i, t) in asset_tabs(&st.catalog, v.asset).iter().enumerate() {
        let c = ColumnSet::Tv(i);
        cs = cs.push(chip(&t.label, c == v.cols, RadarMsg::SetColumns(c)));
    }
    let shown = idx.len().min(80);
    cs = cs.push(
        text(format!("　{} / {} 条 · 点列头排序", shown, idx.len()))
            .size(10)
            .color(C_DIM),
    );
    // 色板不在这一行：官方筛选器的表格没有配色切换（热图页才有）。
    body = body.push(cs.align_y(iced::Alignment::Center));

    let mut hdr = row![].spacing(3);
    for c in &cols {
        hdr = hdr.push(head_cell(c, ev));
    }
    body = body.push(hdr);

    for &i in idx.iter().take(80) {
        let r = &st.rows[i];
        let mut tr = row![].spacing(3);
        for c in &cols {
            let (s, col) = cell_text(r, c.key, &st.catalog, v.palette);
            tr = tr.push(cell(s, c.width, col, is_numeric(c.key)));
        }
        let flag = if !r.sigma_ok && r.z_provisional {
            "≈"
        } else if !r.sigma_ok {
            "⏳"
        } else {
            ""
        };
        tr = tr.push(cell(flag.into(), 22.0, C_GOLD, false));
        body = body.push(tr);
    }

    body = body.push(
        text(format!(
            "源 {} · 快照 {}{} · ⏳=未热身 ≈=借横截面基线（均不可当结论）",
            st.source,
            st.stamp,
            super::staleness::suffix(&st.stamp),
        ))
        .size(10)
        .color(C_DIM),
    );

    scrollable(body).width(Length::Fill).height(Length::Fill).into()
}

#[cfg(test)]
mod tests {
    use super::all_entry_label;
    use super::super::radar_readout::CatalogItem;

    fn mk(code: &str, label: &str, all: &str) -> CatalogItem {
        CatalogItem {
            code: code.into(),
            label: label.into(),
            region: label.into(),
            all_label: all.into(),
        }
    }

    #[test]
    fn the_catalog_title_wins_over_the_local_fallback() {
        // 标题在守护侧定义。面板那张表只是旧守护的回退——它赢了的话，
        // 守护改了标题面板还显示旧的，而且没人会发现
        use super::super::radar_readout::Catalog;
        let mut cat = Catalog::default();
        cat.titles.insert("close".into(), "收盘价".into());
        assert_eq!(super::title_of(&cat, "close"), "收盘价", "该用下发的");
        assert_eq!(super::metric_title("close"), "价格", "本地那份仍在，只是不优先");
        // 下发里没有的键回退到本地表
        assert_eq!(super::title_of(&cat, "change"), "涨跌%");
        // 两边都没有 → 用键名本身，不是空白
        assert_eq!(super::title_of(&cat, "zzz_unknown"), "zzz_unknown");
    }

    #[test]
    fn tile_colour_uses_the_same_scale_as_the_legend() {
        // 图例走 scale_edges(读目录下发的每类色阶)，格子走 pick()→edges()(写死的表)。
        // 两者不一致时，图例说 ±13%、格子却在 ±3% 就顶满——看图的人被骗了。
        const CRYPTO: [f64; 3] = [3.0, 8.0, 13.0]; // 守护为加密下发的色阶
        let key = "change|60";
        let legend_edges = super::scale_edges(key, CRYPTO);
        assert_eq!(legend_edges, CRYPTO, "图例用的是目录色阶");

        // 一个 +5% 的加密标的：按目录色阶落在第 1 档（3~8%）。
        // 早先格子走写死的 [0.5,1.5,3.0]，同一个数字会顶到第 3 档。
        // 这里比的是**格子实际用的颜色**，不是再算一遍分档——
        // 比分档只能证明我算得对，比颜色才能证明画出来的对
        let b_legend = super::bucket(5.0, &legend_edges);
        assert_eq!(b_legend, 1, "按加密色阶 +5% 是第 1 档");
        let r = super::ramp(super::Palette::BlueOrange);
        let tile = super::scale_color(Some(5.0), key, CRYPTO, super::Palette::BlueOrange, true);
        assert_eq!(tile, r.up[0], "格子该用第 1 档的颜色");
        assert_ne!(tile, r.up[2], "用写死的色阶就会顶到最深档");
    }

    #[test]
    fn a_source_from_another_asset_class_is_ignored_not_obeyed() {
        // 换资产类的路径不止一条，只要有一条没清掉旧来源，过滤就会把整张表滤空，
        // 而下拉因为找不到它会回退显示「全部」——界面看着正常、内容却是空的
        use super::super::radar::{AssetFilter, ViewState};
        use super::super::radar_readout::{Catalog, CatalogItem};
        let mut cat = Catalog::default();
        cat.markets.push(CatalogItem {
            code: "china".into(), label: "中国".into(), region: "亚太".into(), all_label: String::new(),
        });
        cat.coin_presets.push(super::super::radar::CoinPreset {
            code: "defi".into(), label: "DeFi 币".into(), ..Default::default()
        });
        let with = |a, s| ViewState { asset: a, source: s, ..ViewState::DEFAULT };
        // 股票市场码带到加密上 → 当作「全部」
        assert_eq!(super::effective_source(&cat, with(AssetFilter::Coin, "china")), "");
        // 加密自己的预设 → 保留
        assert_eq!(super::effective_source(&cat, with(AssetFilter::Coin, "defi")), "defi");
        // 加密分类带到股票上 → 当作「全部」
        assert_eq!(super::effective_source(&cat, with(AssetFilter::Stock, "defi")), "");
        assert_eq!(super::effective_source(&cat, with(AssetFilter::Stock, "china")), "china");
        // 空串本来就是「全部」
        assert_eq!(super::effective_source(&cat, with(AssetFilter::Coin, "")), "");
        // 筛选器独有的市场（热图的 60 国里没有）也必须被认出来
        cat.screener_markets.push(CatalogItem {
            code: "venezuela".into(), label: "委内瑞拉".into(),
            region: "美洲".into(), all_label: String::new(),
        });
        assert_eq!(super::effective_source(&cat, with(AssetFilter::Stock, "venezuela")), "venezuela");
    }

    #[test]
    fn the_two_europe_entries_do_not_collide() {
        // 两项共用组名「欧洲」。不用守护给的 all_label 的话，两项都会被拼成
        // 「所有欧洲公司」——下拉里看起来就是同一项重复了一遍（实际发生过：
        // 面板跑的是加 all_label 之前的二进制）
        let eu = mk("eu", "欧洲", "所有欧盟公司");
        let europe = mk("europe", "欧洲", "所有欧洲公司");
        let a = all_entry_label(false, &eu);
        let b = all_entry_label(false, &europe);
        assert_eq!(a, "所有欧盟公司");
        assert_eq!(b, "所有欧洲公司");
        assert_ne!(a, b, "两项同名就等于下拉里重复一项");
    }

    #[test]
    fn ordinary_markets_still_compose_their_own_label() {
        assert_eq!(all_entry_label(false, &mk("america", "美国", "")), "所有美国公司");
    }

    #[test]
    fn etf_entries_are_the_country_name_itself() {
        // 官方 ETF 下拉里条目就是国名，没有「所有…公司」的措辞（录屏核对）
        assert_eq!(all_entry_label(true, &mk("america", "美国", "")), "美国");
        assert_eq!(all_entry_label(true, &mk("japan", "日本", "")), "日本");
    }


    /// **所有资产类**会下发的列键（不只是股票那 12 组）。上一版的
    /// `ALL_TAB_COLS` 只覆盖股票，于是债券/DEX 那批列漏了中文名、
    /// 表头直接显示 `yield_to_worst`。
    const ALL_ASSET_COLS: [&str; 207] = [
            "close",
            "change",
            "volume",
            "relative_volume_10d_calc",
            "market_cap_basic",
            "price_earnings_ttm",
            "earnings_per_share_diluted_ttm",
            "earnings_per_share_diluted_yoy_growth_ttm",
            "dividends_yield_current",
            "sector",
            "AnalystRating",
            "Perf.W",
            "Perf.1M",
            "Perf.3M",
            "Perf.6M",
            "Perf.YTD",
            "Perf.Y",
            "Perf.5Y",
            "Perf.10Y",
            "Perf.All",
            "Volatility.W",
            "Volatility.M",
            "TechRating_1D",
            "MARating_1D",
            "OsRating_1D",
            "RSI",
            "Mom",
            "AO",
            "CCI20",
            "Stoch.K",
            "Stoch.D",
            "candlestick_patterns_1D",
            "premarket_close",
            "premarket_change",
            "premarket_gap",
            "premarket_volume",
            "gap",
            "volume_change",
            "postmarket_close",
            "postmarket_change",
            "postmarket_volume",
            "earnings_per_share_forecast_next_fy",
            "revenue_forecast_next_fy",
            "net_income_estimate_ntm",
            "free_cash_flow_estimate_ntm",
            "price_earnings_fwd",
            "enterprise_value_ebitda_fwd",
            "price_sales_fwd",
            "total_debt_estimate_fy",
            "book_value_per_share_estimate_fy",
            "dps_estimate_ntm",
            "earnings_release_date",
            "earnings_release_next_date",
            "Perf.1Y.MarketCap",
            "price_earnings_growth_ttm",
            "price_sales_current",
            "price_book_fq",
            "price_to_cash_f_operating_activities_ttm",
            "price_free_cash_flow_ttm",
            "price_to_cash_ratio",
            "enterprise_value_current",
            "enterprise_value_to_revenue_ttm",
            "enterprise_value_to_ebit_ttm",
            "enterprise_value_ebitda_ttm",
            "dps_common_stock_prim_issue_fy",
            "dps_common_stock_prim_issue_fq",
            "dividends_yield",
            "dividend_payout_ratio_ttm",
            "dps_common_stock_prim_issue_yoy_growth_fy",
            "continuous_dividend_payout",
            "continuous_dividend_growth",
            "gross_margin_ttm",
            "operating_margin_ttm",
            "pre_tax_margin_ttm",
            "net_margin_ttm",
            "free_cash_flow_margin_ttm",
            "return_on_assets_fq",
            "return_on_equity_fq",
            "return_on_invested_capital_fq",
            "research_and_dev_ratio_ttm",
            "sell_gen_admin_exp_other_ratio_ttm",
            "fiscal_period_current",
            "fiscal_period_end_current",
            "total_revenue_ttm",
            "total_revenue_yoy_growth_ttm",
            "gross_profit_ttm",
            "oper_income_ttm",
            "net_income_ttm",
            "ebitda_ttm",
            "total_assets_fq",
            "total_current_assets_fq",
            "cash_n_short_term_invest_fq",
            "total_liabilities_fq",
            "total_debt_fq",
            "net_debt_fq",
            "total_equity_fq",
            "current_ratio_fq",
            "quick_ratio_fq",
            "debt_to_equity_fq",
            "cash_n_short_term_invest_to_total_debt_fq",
            "cash_f_operating_activities_ttm",
            "cash_f_investing_activities_ttm",
            "cash_f_financing_activities_ttm",
            "free_cash_flow_ttm",
            "neg_capital_expenditures_ttm",
            "revenue_per_share_ttm",
            "earnings_per_share_basic_ttm",
            "operating_cash_flow_per_share_ttm",
            "free_cash_flow_per_share_ttm",
            "ebit_per_share_ttm",
            "ebitda_per_share_ttm",
            "book_value_per_share_fq",
            "total_debt_per_share_fq",
            "cash_per_share_fq",
            "average_volume_10d_calc",
            "average_volume_30d_calc",
            "Value.Traded",
            "value_traded_10d",
            "value_traded_30d",
            "change|60",
            "change|240",
            "Volatility.D",
            "no_group",
            "country",
            "aum",
            "nav_total_return.3Y",
            "etf_holdings_count",
            "expense_ratio",
            "asset_class.tr",
            "focus.tr",
            "fund_flows.1M",
            "fund_flows.3M",
            "fund_flows.1Y",
            "fund_flows.3Y",
            "fund_flows.YTD",
            "indicated_annual_dividend",
            "dividends_frequency.tr",
            "dividend_treatment.tr",
            "nav",
            "nav_discount_premium",
            "nav_perf.1M",
            "nav_perf.3M",
            "nav_perf.1Y",
            "nav_perf.3Y",
            "nav_perf.YTD",
            "nav_total_return.1M",
            "nav_total_return.3M",
            "nav_total_return.1Y",
            "nav_total_return.YTD",
            "issuer.tr",
            "weight_top_10",
            "weight_top_25",
            "weight_top_50",
            "holdings_region.tr",
            "actively_managed.tr",
            "index_tracked.tr",
            "leverage.tr",
            "beta_1_year",
            "beta_3_year",
            "beta_5_year",
            "crypto_total_rank",
            "24h_close_change|5",
            "market_cap_calc",
            "24h_vol_cmc",
            "circulating_supply",
            "24h_vol_to_market_cap",
            "socialdominance",
            "crypto_common_categories.tr",
            "Perf.5D",
            "altrank",
            "24h_vol_change_cmc",
            "total_shares_diluted",
            "circulating_to_max_supply_ratio",
            "crypto_category",
            "exchange.tr",
            "24h_vol|5",
            "24h_vol_change|5",
            "market_cap_diluted_calc",
            "high",
            "low",
            "MACD.macd",
            "blockchain-id.tr",
            "dex_txs_count_24h",
            "dex_trading_volume_24h",
            "dex_txs_count_uniq_24h",
            "dex_total_liquidity",
            "fully_diluted_value",
            "bid",
            "ask",
            "Recommend.All",
            "Recommend.MA",
            "isin-displayed",
            "yield_to_worst",
            "close_pct",
            "close_net",
            "current_coupon",
            "maturity_date",
            "redemption_type.tr",
            "bond_issuer_type.tr",
            "bond_snp_rating_lt.tr",
            "bond_fitch_rating_lt.tr",
            "coupon_frequency.tr",
            "accrued_coupon_interest",
            "coupon_date_prev",
            "coupon_date_next",
            "coupon_currency",
            "bond_issuer_snp_rating_lt.tr",
    ];

    #[test]
    fn every_asset_class_column_has_a_chinese_title() {
        let miss: Vec<_> = ALL_ASSET_COLS
            .iter()
            .filter(|k| metric_title(k) == **k && !KEEP_AS_IS.contains(k))
            .copied()
            .collect();
        assert!(miss.is_empty(), "这些列没有中文名: {miss:?}");
    }

    #[test]
    fn ymd_dates_render_as_dates_not_numbers() {
        // 债券到期日是 20290214；不单列一类会显示成 20.3M
        assert_eq!(ymd_cell(20290214.0), "2029-02-14");
        assert_eq!(ymd_cell(20261015.0), "2026-10-15");
        // 明显不是日期的值不能硬凑出一个
        assert_eq!(ymd_cell(0.0), "—");
        assert_eq!(ymd_cell(1785456000.0), "—", "Unix 秒不是 YYYYMMDD");
        assert_eq!(ymd_cell(20291345.0), "—", "13 月 45 日");
    }

    fn memo_row(sym: &str, tv: f64) -> RadarRow {
        RadarRow {
            symbol: sym.into(),
            venue: "binance:linear".into(),
            asset: "crypto".into(),
            quote_vol_24h: tv,
            ..Default::default()
        }
    }

    #[test]
    fn memoized_index_recomputes_when_data_or_view_state_changes() {
        // 记忆化的键必须**同时**含数据版本与口径。少一个就是一类静默错误：
        // 只用版本 → 点了排序表格不动；只用口径 → 数据更新了表格不刷新。
        // 引入记忆化是因为上游用 window::frames() 每秒重建 60 次视图，
        // 而排序 4189 行占掉一帧 8ms 里的 4.5ms（实测）。
        let rows = vec![memo_row("A", 1.0), memo_row("B", 2.0)];
        let mut v = ViewState::DEFAULT;
        v.sort = SortKey::Turnover;
        v.desc = true;

        let first = memo_order(1, v, &rows, &[]);
        assert_eq!(rows[first[0]].symbol, "B");
        assert!(std::rc::Rc::ptr_eq(&first, &memo_order(1, v, &rows, &[])), "同键该命中缓存");

        v.desc = false;
        assert_eq!(rows[memo_order(1, v, &rows, &[])[0]].symbol, "A", "改了排序方向却没重算");

        let rows2 = vec![memo_row("C", 9.0)];
        assert_eq!(rows2[memo_order(2, v, &rows2, &[])[0]].symbol, "C", "数据换了一轮却没重算");
    }

    /// 守护会下发的全部列键（对齐 `metricdef::TABS`）。加列时要同步这里——
    /// 靠下面那条单测保证「加了列却忘了写中文名」会当场失败，
    /// 而不是在表头上安静地显示一行 `earnings_release_next_date`。
    const ALL_TAB_COLS: [&str; 72] = [
            "close",
            "change",
            "volume",
            "relative_volume_10d_calc",
            "market_cap_basic",
            "price_earnings_ttm",
            "earnings_per_share_diluted_ttm",
            "earnings_per_share_diluted_yoy_growth_ttm",
            "dividends_yield_current",
            "Perf.W",
            "Perf.1M",
            "Perf.3M",
            "Perf.6M",
            "Perf.YTD",
            "Perf.Y",
            "Perf.5Y",
            "Perf.10Y",
            "Perf.All",
            "Volatility.W",
            "Volatility.M",
            "Recommend.All",
            "Recommend.MA",
            "Recommend.Other",
            "RSI",
            "Mom",
            "AO",
            "CCI20",
            "Stoch.K",
            "Stoch.D",
            "premarket_close",
            "premarket_change",
            "premarket_gap",
            "premarket_volume",
            "gap",
            "volume_change",
            "postmarket_close",
            "postmarket_change",
            "postmarket_volume",
            "price_target_average",
            "recommendation_total",
            "recommendation_mark",
            "earnings_per_share_forecast_next_fq",
            "earnings_release_date",
            "earnings_release_next_date",
            "price_earnings_growth_ttm",
            "price_sales_current",
            "price_book_fq",
            "price_to_cash_f_operating_activities_ttm",
            "price_free_cash_flow_ttm",
            "price_cash_flow_current",
            "enterprise_value_current",
            "enterprise_value_ebitda_ttm",
            "dividends_per_share_fq",
            "dividend_payout_ratio_ttm",
            "dps_common_stock_prim_issue_yoy_growth_fy",
            "continuous_dividend_payout",
            "continuous_dividend_growth",
            "gross_margin",
            "operating_margin",
            "pre_tax_margin",
            "net_margin",
            "free_cash_flow_margin_ttm",
            "return_on_assets",
            "return_on_equity",
            "return_on_invested_capital",
            "total_revenue_ttm",
            "total_revenue_yoy_growth_ttm",
            "gross_profit",
            "oper_income_ttm",
            "net_income_ttm",
            "ebitda_ttm",
            "earnings_per_share_basic_ttm",
    ];

    /// 这几个指标在中文语境里就用原名，不算「漏翻」。
    const KEEP_AS_IS: [&str; 2] = ["RSI", "AO"];

    #[test]
    fn every_column_has_a_chinese_title() {
        // metric_title 的兜底是原样返回键名——漏一个不会报错，只会让表头
        // 显示原始英文键，还会因为算出来的列宽装不下而和右边一列叠在一起
        //（实测 earnings_release_date / earnings_release_next_date 就是这样）
        let miss: Vec<_> = ALL_TAB_COLS
            .iter()
            .filter(|k| metric_title(k) == **k && !KEEP_AS_IS.contains(k))
            .copied()
            .collect();
        assert!(miss.is_empty(), "这些列没有中文名: {miss:?}");
    }

    #[test]
    fn every_column_is_wide_enough_for_its_own_header() {
        // 装不下就会和右边一列叠字。上界（132）也得真的够用，
        // 不能靠 clamp 把问题压掉
        // 空目录 = 全部回退到本地表，正是最宽（英文原名）的那种情况
        let cat = super::super::radar_readout::Catalog::default();
        for k in ALL_TAB_COLS {
            let need = (text_units(title_of(&cat, k)) + 2.0) * 11.0 + 8.0;
            assert!(
                metric_width(&cat, k) >= need,
                "{k}（{}）需要 {need:.0}px，只给了 {:.0}px",
                title_of(&cat, k),
                metric_width(&cat, k)
            );
        }
    }
    use super::*;

    const P: Palette = Palette::BlueOrange;

    fn row_of(sym: &str, z5: Option<f64>) -> RadarRow {
        let mut r = RadarRow {
            symbol: sym.into(),
            venue: "binance:spot".into(),
            quote_vol_24h: 1e6,
            sigma_ok: z5.is_some(),
            ..Default::default()
        };
        r.z_ret[1] = z5;
        r
    }

    #[test]
    fn buckets_are_discrete_and_symmetric() {
        // 从 edges() 推探针，别硬编码——调过一次分档边界，硬编码的断言会假失败
        let e = edges("own:speed_z");
        assert_eq!(bucket(0.0, &e), 0);
        assert_eq!(bucket(e[0] - 1e-9, &e), 0);
        for (i, x) in e.iter().enumerate() {
            assert_eq!(bucket(*x, &e), i + 1, "边界值应落进上一档（>=）");
            assert_eq!(bucket(-*x, &e), i + 1, "分档按绝对值，正负对称");
        }
        assert_eq!(bucket(e[2] * 100.0, &e), 3, "超出最强档应封顶，不越界");
        assert_eq!(bucket(f64::NAN, &e), 0);
        assert_eq!(bucket(f64::INFINITY, &e), 0);
    }

    #[test]
    fn legend_and_tiles_share_one_definition() {
        // 图例画 4 档跌 + 中性 + 4 档涨；上色也必须只用这 9 个颜色。
        // 两者若各写一套，图例和格子会对不上——那比没有图例更糟。
        for cb in ["own:speed_z", "change", "Perf.YTD", "relative_volume_10d_calc"] {
            for probe in [-9.0, -3.0, -1.0, -0.1, 0.0, 0.1, 1.0, 3.0, 9.0] {
                let c = scale_color(Some(probe), cb, STOCK_SCALE, P, true);
                let r = ramp(P);
                let known = std::iter::once(C_NEUTRAL)
                    .chain(r.up.iter().copied())
                    .chain(r.down.iter().copied())
                    .any(|k| (k.r - c.r).abs() < 1e-6 && (k.g - c.g).abs() < 1e-6);
                assert!(known, "{cb:?} 在 {probe} 处上了色阶之外的颜色");
            }
            assert_eq!(edges(cb).len(), 3, "7 档色阶 = 3 个边界");
        }
    }

    #[test]
    fn color_is_diverging_and_desaturates_untrusted() {
        let up = scale_color(Some(4.0), "own:speed_z", STOCK_SCALE, P, true);
        let down = scale_color(Some(-4.0), "own:speed_z", STOCK_SCALE, P, true);
        assert!(up.b > up.r, "涨端应偏蓝");
        assert!(down.r > down.b, "跌端应偏橙");
        let dist = |c: Color| (c.r - C_NEUTRAL.r).abs() + (c.b - C_NEUTRAL.b).abs();
        assert!(dist(scale_color(Some(4.0), "own:speed_z", STOCK_SCALE, P, false)) < dist(up));
    }

    #[test]
    fn missing_value_is_neutral() {
        let c = scale_color(None, "own:speed_z", STOCK_SCALE, P, true);
        assert!((c.r - C_NEUTRAL.r).abs() < 1e-6 && (c.b - C_NEUTRAL.b).abs() < 1e-6);
    }

    #[test]
    fn base_asset_strips_quote_currency() {
        assert_eq!(base_asset("BTCUSDT"), "BTC");
        assert_eq!(base_asset("ETHBTC"), "ETH");
        assert_eq!(base_asset("1000PEPEUSDT"), "1000PEPE");
        // 只剩计价币本身时不该剥成空串
        assert_eq!(base_asset("USDT"), "USDT");
        assert_eq!(base_asset("FOO"), "FOO");
    }

    /// 目录夹具：两个 TV 列组 + 量纲标注。
    fn cat() -> Catalog {
        Catalog {
            tabs: vec![
                super::super::radar_readout::ColumnTab {
                    key: "overview".into(),
                    label: "概览".into(),
                    cols: vec!["close".into(), "change".into(), "market_cap_basic".into()],
                },
                super::super::radar_readout::ColumnTab {
                    key: "valuation".into(),
                    label: "估值".into(),
                    cols: vec!["price_earnings_ttm".into(), "price_book_fq".into()],
                },
            ],
            pct_keys: ["change", "Perf.YTD"].iter().map(|s| s.to_string()).collect(),
            money_keys: ["close", "market_cap_basic"].iter().map(|s| s.to_string()).collect(),
            date_keys: ["earnings_release_next_date"].iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn column_sets_differ_and_always_lead_with_symbol() {
        let c = cat();
        let mut v = ViewState::DEFAULT;
        for cs in [
            ColumnSet::Speed,
            ColumnSet::SpeedZ,
            ColumnSet::Reference,
            ColumnSet::Tv(0),
            ColumnSet::Tv(1),
        ] {
            v.cols = cs;
            let cols = columns(v, &c);
            assert_eq!(cols[0].key, SortKey::Symbol, "{cs:?} 首列应是标的");
            assert!(cols.len() >= 4, "{cs:?} 列太少");
        }
        v.cols = ColumnSet::SpeedZ;
        assert_eq!(
            columns(v, &c).iter().filter(|c| matches!(c.key, SortKey::Z(_))).count(),
            WINDOWS.len(),
            "速度列组应给出全部窗口的 z"
        );
    }

    #[test]
    fn tv_tabs_come_from_the_catalog_not_hardcoded() {
        // 列名由快照下发；加列只改守护侧一处
        let c = cat();
        let mut v = ViewState::DEFAULT;
        v.cols = ColumnSet::Tv(1);
        let keys: Vec<_> = columns(v, &c)
            .into_iter()
            .filter_map(|x| match x.key {
                SortKey::Metric(k) => Some(k),
                _ => None,
            })
            .collect();
        assert_eq!(keys, vec!["price_earnings_ttm", "price_book_fq"]);
        // 越界的下标不该 panic，只是没有指标列
        v.cols = ColumnSet::Tv(99);
        assert!(columns(v, &c).iter().all(|x| !matches!(x.key, SortKey::Metric(_))));
    }

    #[test]
    fn per_share_amounts_keep_decimals() {
        // usd() 从千位就缩写，会把 MELI 的 $1963.49 显示成 `2K`
        assert_eq!(money_cell(1963.49), "1963.49");
        assert_eq!(money_cell(29.43), "29.43");
        assert_eq!(money_cell(0.0), "0.00");
        // 大额聚合量仍然缩写
        assert_eq!(money_cell(99.5e9), "99.5B");
        assert_eq!(money_cell(1.092e12), "1.09T");
    }

    #[test]
    fn usd_scales_up_to_trillions() {
        // 没有 T 档的话 NVDA 的 5.24 万亿会显示成 5240.0B
        // 全表不带 $ 前缀（金额列已统一换算成美元，列头说明口径即可）
        assert_eq!(usd(5.24e12), "5.24T");
        assert_eq!(usd(2.9e9), "2.9B");
        assert_eq!(usd(1.5e6), "1.5M");
        assert_eq!(usd(-3.1e12), "-3.10T", "负值也要走 T 档");
    }

    #[test]
    fn metric_cells_respect_the_declared_unit() {
        let c = cat();
        let mut r = row_of("NVDA", Some(1.0));
        r.m.insert("change".into(), -4.57);
        r.m.insert("market_cap_basic".into(), 5.24e12);
        r.m.insert("price_earnings_ttm".into(), 27.5);

        assert_eq!(cell_text(&r, SortKey::Metric("change"), &c, P).0, "-4.57%");
        assert_eq!(cell_text(&r, SortKey::Metric("market_cap_basic"), &c, P).0, "5.24T");
        r.m.insert("close".into(), 1963.49);
        assert_eq!(
            cell_text(&r, SortKey::Metric("close"), &c, P).0,
            "1963.49",
            "每股价格不该被缩写成 2K"
        );
        assert_eq!(
            cell_text(&r, SortKey::Metric("price_earnings_ttm"), &c, P).0,
            "27.50",
            "比率既不是百分数也不是金额"
        );
        assert_eq!(cell_text(&r, SortKey::Metric("gross_margin"), &c, P).0, "—");
    }

    #[test]
    fn only_change_like_metrics_get_sign_colour() {
        // 把 P/E 按涨跌上色是误导
        let c = cat();
        let mut r = row_of("X", Some(1.0));
        r.m.insert("change".into(), -4.0);
        r.m.insert("price_earnings_ttm".into(), 27.5);
        let neutral = cell_text(&r, SortKey::Metric("price_earnings_ttm"), &c, P).1;
        assert!((neutral.r - C_TXT.r).abs() < 1e-6, "估值类应保持中性色");
        let chg = cell_text(&r, SortKey::Metric("change"), &c, P).1;
        assert!(chg.r > chg.b, "跌应偏橙");
    }

    #[test]
    fn cell_text_marks_missing_as_dash_not_zero() {
        let r = row_of("XUSDT", None);
        assert_eq!(cell_text(&r, SortKey::Z(1), &cat(), P).0, "—");
        assert_eq!(cell_text(&r, SortKey::Ret(1), &cat(), P).0, "—");
        assert_eq!(cell_text(&r, SortKey::VolZ, &cat(), P).0, "—");
    }

    #[test]
    fn area_weight_equal_mode_ignores_turnover() {
        let mut r = row_of("A", Some(1.0));
        r.quote_vol_24h = 9e9;
        let mut v = ViewState::DEFAULT;
        v.size = SIZE_OPTS.iter().find(|o| o.key == "equal").copied().unwrap();
        assert!((area_weight(&r, v) - 1.0).abs() < 1e-12);
        v.size = SIZE_OPTS.iter().find(|o| o.key == "own:turnover").copied().unwrap();
        assert!((area_weight(&r, v) - 9e9).abs() < 1e-3);
    }

    #[test]
    fn treemap_selection_is_by_turnover_not_by_sort() {
        // 按排序选行会让热图只剩涨幅榜（实测满屏全蓝，一个下跌都看不见）。
        let mut big = row_of("BIG", Some(-0.1));
        big.quote_vol_24h = 9e9;
        let mut small = row_of("SMALL", Some(9.0));
        small.quote_vol_24h = 1.0;
        let rows = vec![small, big];
        let top = top_by_turnover(&rows, 1);
        assert_eq!(rows[top[0]].symbol, "BIG", "体量最大的必须进图，哪怕它 z 最低");
    }

    #[test]
    fn treemap_deduplicates_the_same_coin_across_venues() {
        // 同一个币在 spot 与 linear 各一行、市值相同——按市值铺图时各占一个满格，
        // 面积被重复计算（实测 BTC/ETH/XRP/SOL 各出现两次，半张图是冗余的）
        let mk = |venue: &str, vol: f64| {
            let mut r = row_of("BTCUSDT", Some(1.0));
            r.asset = "crypto".into();
            r.venue = venue.into();
            r.quote_vol_24h = vol;
            r
        };
        let rows = vec![mk("binance:spot", 1e8), mk("binance:linear", 9e8)];
        let top = top_by_turnover_within(&rows, &[0, 1], 10);
        assert_eq!(top.len(), 1, "同一个币只该占一格");
        assert_eq!(rows[top[0]].venue, "binance:linear", "保留成交额最大的挂牌");
    }

    #[test]
    fn different_markets_may_share_a_ticker_without_being_merged() {
        // 港股 700 与别处的 700 不是一回事；股票去重必须带市场段
        let mk = |venue: &str| {
            let mut r = row_of("700", Some(1.0));
            r.asset = "equity".into();
            r.venue = venue.into();
            r.quote_vol_24h = 1e8;
            r
        };
        let rows = vec![mk("tv:hongkong:stock"), mk("tv:japan:stock")];
        assert_eq!(top_by_turnover_within(&rows, &[0, 1], 10).len(), 2);
    }

    #[test]
    fn missing_size_metric_is_detectable_not_a_blank_map() {
        // 切到加密后树图整个空掉，因为默认口径是市值而 Binance 不给市值。
        // 空白且无提示是最难排查的一种坏——面板必须能判定这个状态。
        let mut r = row_of("BTCUSDT", Some(1.0));
        r.asset = "crypto".into();
        r.quote_vol_24h = 5e8;
        let mut v = ViewState::DEFAULT;
        v.size = *SIZE_OPTS.iter().find(|o| o.key == "market_cap_basic").unwrap();
        assert!(
            metric_value(&r, v.size.key, v.win).is_none(),
            "构造有误：这行不该有市值"
        );
        // 存在可用的替代口径
        let alt = SIZE_OPTS
            .iter()
            .find(|o| o.key != v.size.key && metric_value(&r, o.key, v.win).is_some_and(|x| x > 0.0));
        assert!(alt.is_some(), "应能找到有数据的替代口径");

        // 有市值时恢复正常
        r.m.insert("market_cap_basic".into(), 1.5e12);
        assert_eq!(metric_value(&r, "market_cap_basic", v.win), Some(1.5e12));
    }

    #[test]
    fn treemap_selection_honours_the_asset_subset() {
        // 股票成交额远大于加密；不按子集选行的话，切到「加密」后图上还是股票
        let mut eq = row_of("NVDA", Some(1.0));
        eq.quote_vol_24h = 9e9;
        eq.asset = "equity".into();
        let mut cr = row_of("BTCUSDT", Some(1.0));
        cr.quote_vol_24h = 1e6;
        cr.asset = "crypto".into();
        let rows = vec![eq, cr];
        // Binance 交易对归 CEX（「加密货币」是 coin 端点的币）
        let sub = super::visible(&rows, { let mut v = ViewState::DEFAULT; v.asset = AssetFilter::Cex; v }, &[]);
        let top = top_by_turnover_within(&rows, &sub, 5);
        assert_eq!(top.len(), 1);
        assert_eq!(rows[top[0]].symbol, "BTCUSDT");
    }

    #[test]
    fn treemap_selection_is_stable_and_bounded() {
        let rows: Vec<RadarRow> = (0..50).map(|i| row_of(&format!("S{i}"), Some(1.0))).collect();
        let a = top_by_turnover(&rows, 10);
        assert_eq!(a, top_by_turnover(&rows, 10), "同额时必须定序，否则每轮刷新树图会抖");
        assert_eq!(a.len(), 10);
        assert_eq!(top_by_turnover(&rows, 999).len(), 50, "n 大于行数不该越界");
    }

    #[test]
    fn fit_text_truncates_instead_of_overflowing() {
        assert_eq!(fit_text("BTC", 10.0, 200.0).as_deref(), Some("BTC"));
        // 放不下就截断并补省略号（同 TV 的 `Consumer non-dur…`），
        // 绝不返回超长串（溢出会把 PENGU+ONDO 拼成 PENGUONDO）
        let cut = fit_text("1000PEPE", 20.0, 40.0).unwrap();
        assert!(cut.ends_with('…'), "截断后应补省略号：{cut}");
        let stem: String = cut.chars().take_while(|c| *c != '…').collect();
        assert!(stem.chars().count() < 8, "没截断：{cut}");
        assert!("1000PEPE".starts_with(&stem));
        // 只剩一两个字母认不出是谁，不如不画
        assert!(fit_text("ABCDEF", 20.0, 10.0).is_none());
        assert!(fit_text("ABC", 10.0, 0.0).is_none());
    }

    #[test]
    fn fit_text_never_exceeds_available_width() {
        for s in ["BTC", "1000PEPE", "我踏马来了", "BROCCOLI714", "混合ABC"] {
            for avail in [12.0f32, 30.0, 55.0, 120.0] {
                if let Some(out) = fit_text(s, 14.0, avail) {
                    assert!(
                        text_units(&out) * 14.0 <= avail + 1e-3,
                        "{s:?} 裁成 {out:?} 仍超宽（avail={avail}）"
                    );
                }
            }
        }
    }

    #[test]
    fn wide_chars_count_as_full_width() {
        // Binance 有中文名标的（「我踏马来了」）。按半角估宽会让标签压到隔壁格子上。
        assert!(text_units("我踏马来了") > text_units("ABCDE") * 1.5);
        assert!((text_units("我A") - (ADV_WIDE + ADV_NARROW)).abs() < 1e-6);
    }

    #[test]
    fn pct_log_converts_log_return_back_to_percent() {
        // 快照里存的是对数收益；面板要显示的是人看得懂的百分比
        assert_eq!(pct_log(None), "—");
        let p = pct_log(Some(1.3010f64.ln()));
        assert_eq!(p, "+30.10%");
        let n = pct_log(Some(0.92f64.ln()));
        assert_eq!(n, "-8.00%");
        assert!(pct_log(Some(0.0)).starts_with('+'), "零也要带符号，免得和缺值混淆");
    }

    #[test]
    fn compact_number_format_is_shorter_but_still_signed() {
        // 窄格退而用紧凑写法，而不是把数字截成 `-0.…`（分不出 -0.1 还是 -0.9）
        assert_eq!(opt_z(Some(-1.234)), "-1.23");
        assert_eq!(opt_z_compact(Some(-1.234)), "-1.2");
        assert!(text_units(&opt_z_compact(Some(-1.234))) < text_units(&opt_z(Some(-1.234))));
        assert_eq!(opt_pct_compact(Some(0.0512)), "+5.1");
        assert!(opt_z_compact(Some(2.0)).starts_with('+'), "紧凑写法也必须带符号");
    }

    #[test]
    fn fit_font_shrinks_to_make_label_fit() {
        // 只按格子高算字号会让 TRUMP 压到隔壁 AVAX 上（实测拼成 TRUMPAVAX）
        let avail = 40.0;
        let fs = fit_font("TRUMP", avail, 24.0);
        assert!(fs < 24.0, "长标签必须缩字号，得到 {fs}");
        assert!(text_units("TRUMP") * fs <= avail + 1e-3);
        // 短标签不该被无谓缩小
        assert!((fit_font("A", 200.0, 24.0) - 24.0).abs() < 1e-6);
    }

    #[test]
    fn weakest_bucket_still_reads_as_up_or_down() {
        // 最弱档若跟中性灰同色，市场常态波动在图上就完全看不出方向（第一版的实测问题）。
        let sep = |a: Color, b: Color| {
            (a.r - b.r).abs() + (a.g - b.g).abs() + (a.b - b.b).abs()
        };
        let up1 = scale_color(Some(0.4), "own:speed_z", STOCK_SCALE, P, true);
        let dn1 = scale_color(Some(-0.4), "own:speed_z", STOCK_SCALE, P, true);
        let neu = scale_color(Some(0.0), "own:speed_z", STOCK_SCALE, P, true);
        assert!(sep(up1, neu) > 0.15, "最弱涨档与中性太接近：{:.3}", sep(up1, neu));
        assert!(sep(dn1, neu) > 0.15, "最弱跌档与中性太接近：{:.3}", sep(dn1, neu));
        assert!(up1.b > up1.r && dn1.r > dn1.b, "最弱档必须仍带方向色相");
    }

    #[test]
    fn speed_edges_do_not_swallow_the_market_in_neutral() {
        // 首档边界应在实测 |z| 中位数附近或之下，好让**至少一半市场**能显出方向。
        // 取得太高的话中性档吞掉半张图，图上就没有信息了（第一版取 0.5 的实测问题）。
        const MEASURED_MEDIAN_ABS_Z: f64 = 0.5;
        let e = edges("own:speed_z");
        assert!(
            e[0] <= MEASURED_MEDIAN_ABS_Z,
            "首档边界 {} 高过实测 |z| 中位数，半个市场会变中性",
            e[0]
        );
        assert_eq!(bucket(MEASURED_MEDIAN_ABS_Z, &e), 1, "常态波动应落进有色档");
        assert!(e[1] > e[0] && e[2] > e[1], "边界必须递增");
    }

    #[test]
    fn table_text_is_brighter_than_tile_fill() {
        let lum = |c: Color| 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
        for probe in [0.6, 1.8, -0.6, -3.9] {
            let fill = scale_color(Some(probe), "own:speed_z", STOCK_SCALE, P, true);
            let txt = scale_text(Some(probe), "own:speed_z", STOCK_SCALE, P, true);
            assert!(
                lum(txt) > lum(fill),
                "z={probe} 文字色不比填充色亮：{:.3} vs {:.3}",
                lum(txt),
                lum(fill)
            );
        }
    }

    #[test]
    fn text_scale_keeps_direction_and_neutral() {
        let up = scale_text(Some(3.0), "own:speed_z", STOCK_SCALE, P, true);
        let dn = scale_text(Some(-3.0), "own:speed_z", STOCK_SCALE, P, true);
        assert!(up.b > up.r, "涨端应偏蓝");
        assert!(dn.r > dn.b, "跌端应偏橙");
        assert!((scale_text(None, "own:speed_z", STOCK_SCALE, P, true).r - C_DIM.r).abs() < 1e-6);
    }

    #[test]
    fn area_weight_never_negative() {
        let mut r = row_of("A", Some(1.0));
        r.quote_vol_24h = -5.0; // 脏数据
        let mut v = ViewState::DEFAULT;
        v.size = SIZE_OPTS.iter().find(|o| o.key == "own:turnover").copied().unwrap();
        assert!(area_weight(&r, v) >= 0.0, "负权重会让树图布局出负尺寸");
    }

    #[test]
    fn metric_value_covers_own_and_tv_keys() {
        let mut r = row_of("A", Some(2.0));
        r.ret[1] = Some(0.03);
        r.z_vol = Some(5.0);
        assert_eq!(metric_value(&r, "own:speed_z", 1), Some(2.0));
        assert_eq!(metric_value(&r, "own:zvol", 1), Some(5.0));
        r.m.insert("Perf.YTD".into(), 16.3);
        assert_eq!(metric_value(&r, "Perf.YTD", 1), Some(16.3));
        assert!(metric_value(&r, "gap", 1).is_none(), "缺的指标应为 None");
    }
}

#[cfg(test)]
mod board_table_tests {
    use super::*;

    #[test]
    fn a_deliberately_ordered_list_is_left_alone() {
        // 预测市场热门榜是两家各自排完再交错的。按成交额重排就把它拍回成
        // 跨平台比大小——而两家的成交额窗口和单位根本不是一个量
        let rows = vec![("kalshi", 1e7), ("polymarket", 1e5), ("kalshi", 9e6)];
        let out = sort_rows(&rows, 200, NO_SORT, |r: &(&str, f64), i| match i {
            0 => sv(r.0),
            _ => n(r.1),
        });
        assert_eq!(out, rows, "NO_SORT 必须原样返回");
    }

    #[test]
    fn missing_values_sink_in_both_directions() {
        // 升序时让一片「—」占满前几行没有任何用处
        let rows = vec![("有", Some(3.0)), ("缺", None), ("小", Some(1.0))];
        let key = |r: &(&str, Option<f64>), _i: u8| on(r.1);
        let desc = sort_rows(&rows, 201, 0, key);
        assert_eq!(desc.last().unwrap().0, "缺");
        // 再点一次翻成升序
        super::super::radar::apply(ViewState::DEFAULT, RadarMsg::SortTable { table: 201, col: 0 });
        let asc = sort_rows(&rows, 201, 0, key);
        assert_eq!(asc.last().unwrap().0, "缺", "升序时缺值也要沉底");
        assert_eq!(asc[0].0, "小");
    }

    #[test]
    fn money_and_date_strings_sort_as_numbers() {
        // 按字符串排会让 $34,500,000 排在 $100,000,000 前面、10/01 排在 9/03 前面
        let a = match money_num("$34,500,000") {
            SortVal::N(x) => x,
            _ => panic!(),
        };
        let b = match money_num("$100,000,000") {
            SortVal::N(x) => x,
            _ => panic!(),
        };
        assert!(b > a);
        assert!(matches!(money_num(""), SortVal::None));

        let sep = match mdy_num("9/03/2026") {
            SortVal::N(x) => x,
            _ => panic!(),
        };
        let oct = match mdy_num("10/01/2026") {
            SortVal::N(x) => x,
            _ => panic!(),
        };
        assert!(oct > sep);
        assert!(matches!(mdy_num("待定"), SortVal::None));
    }

    #[test]
    fn a_zero_market_cap_sinks_instead_of_leading_the_ascending_list() {
        // 0 是「原表没给」（多为 ETF）。当成市值为零，升序榜首会挤满 ETF，
        // 看着像一堆一文不值的公司
        let rows = vec![
            StockRow { symbol: "ETF".into(), mcap: 0.0, ..Default::default() },
            StockRow { symbol: "小".into(), mcap: 1e8, ..Default::default() },
        ];
        let out = sort_rows(&rows, 202, 6, |r: &StockRow, _i| {
            (r.mcap > 0.0).then_some(r.mcap).map_or(SortVal::None, SortVal::N)
        });
        assert_eq!(out.last().unwrap().symbol, "ETF");
    }

    #[test]
    fn a_row_without_a_url_gets_no_button() {
        // 点了没反应的按钮比没有按钮更让人困惑
        assert!(super::super::radar_readout::register_link("").is_none());
        let id = super::super::radar_readout::register_link("https://x/a").unwrap();
        // 同一个 URL 永远是同一个 id——按帧下标发消息会在快照刷新后错位
        assert_eq!(super::super::radar_readout::register_link("https://x/a"), Some(id));
        assert_eq!(super::super::radar_readout::link_of(id).as_deref(), Some("https://x/a"));
    }

    #[test]
    fn only_http_links_are_opened() {
        // 登记表里的串来自守护下发的快照，而 `xdg-open` 会按协议头去调
        // 任意处理器
        let id = super::super::radar_readout::register_link("file:///etc/passwd").unwrap();
        assert!(super::super::radar_readout::open_link(id).starts_with("拒绝打开"));
    }

    #[test]
    fn the_day_filter_keeps_rows_whose_timestamp_failed_to_parse() {
        // 时间戳为 0 是数据问题，不该被时间筛静默吃掉
        assert!(within_days(0, 7));
        assert!(within_days(1, 0), "days=0 表示不限");
        let now = chrono::Local::now().timestamp_millis();
        assert!(within_days(now - 86_400_000, 7));
        assert!(!within_days(now - 30 * 86_400_000, 7));
    }
}

#[cfg(test)]
mod freshness_tests {
    use super::*;

    #[test]
    fn every_block_id_maps_to_a_name_the_daemon_accepts() {
        // 块号与守护的白名单对不上的话，按钮点了守护完全不理，
        // 而界面上什么都不会说
        const DAEMON_WHITELIST: [&str; 5] =
            ["crypto", "prediction", "equity", "macros", "slow"];
        for b in [blk::CRYPTO, blk::PREDICTION, blk::EQUITY, blk::MACROS, blk::SLOW] {
            let name = super::super::radar::block_name(b);
            assert!(DAEMON_WHITELIST.contains(&name), "{b} → {name} 不在守护白名单里");
            assert!(!super::super::radar::block_label(b).is_empty());
        }
        // 五个块号互不相同，否则「刷新加密」会去刷别的块
        let mut ns: Vec<&str> = [blk::CRYPTO, blk::PREDICTION, blk::EQUITY, blk::MACROS, blk::SLOW]
            .iter()
            .map(|b| super::super::radar::block_name(*b))
            .collect();
        ns.sort();
        ns.dedup();
        assert_eq!(ns.len(), 5);
    }

    #[test]
    fn a_never_fetched_block_says_so_instead_of_showing_1970() {
        assert_eq!(ago(0), "尚未抓取");
        assert_eq!(ago(-1), "尚未抓取");
        let now = chrono::Local::now().timestamp_millis();
        assert!(ago(now).contains("秒前"));
        assert!(ago(now - 300_000).contains("分钟前"));
        assert!(ago(now - 7_200_000).contains("小时前"));
    }

    #[test]
    fn stale_data_changes_color() {
        // 一屏数据放了一小时还显示成常态色，等于没标
        let now = chrono::Local::now().timestamp_millis();
        assert_eq!(age_color(now), C_DIM);
        assert_eq!(age_color(now - 1_200_000), C_GOLD);
        assert_eq!(age_color(now - 7_200_000), C_BAD);
        // 没抓过也要显眼
        assert_eq!(age_color(0), C_GOLD);
    }

    #[test]
    fn each_tier_has_a_distinct_badge() {
        // 五档的文案必须能一眼分开——「延迟」和「收盘价」是两件事
        let all = [Tier::Live, Tier::NearLive, Tier::Delayed, Tier::Close, Tier::Periodic];
        let mut b: Vec<&str> = all.iter().map(|t| t.badge()).collect();
        assert!(b.iter().all(|x| !x.is_empty()));
        b.sort();
        b.dedup();
        assert_eq!(b.len(), 5);
        // 实时与非实时的颜色必须不同，否则扫一眼分不出
        assert_ne!(Tier::Live.color(), Tier::Delayed.color());
        assert_ne!(Tier::Delayed.color(), Tier::Periodic.color());
    }
}
