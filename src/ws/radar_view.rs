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
    button, canvas as canvas_widget, column, container, pick_list, row, scrollable, text,
};
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Size, Theme};

use super::radar::{
    Form,
    order, scale_kind, visible, AssetFilter, ColumnSet, GroupBy, Palette, RadarMsg,
    ScaleKind, SortKey, ViewMode, ViewState, COLOR_OPTS, SIZE_OPTS,
};
use super::radar_filter::{self, FILTERS, N_FILTERS};
use super::radar_readout::{BreadthRow, OverviewRow, RadarRow, OV_WINDOWS, WINDOWS};
use super::treemap::{squarify_nested, Rect};

const C_HEAD: Color = Color::from_rgb(0.55, 0.8, 1.0);
const C_DIM: Color = Color::from_rgb(0.55, 0.55, 0.6);
const C_TXT: Color = Color::from_rgb(0.85, 0.87, 0.92);
const C_GOLD: Color = Color::from_rgb(0.9, 0.8, 0.4);
const C_BAD: Color = Color::from_rgb(0.9, 0.45, 0.4);
/// 状态「正常/运行中」的提示色（与涨跌色板无关，不随色板切换）。
const C_OK: Color = Color::from_rgb(0.35, 0.78, 0.98);

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

fn pick(v: Option<f64>, key: &str, r: &Ramp, bright: bool, zero: Color) -> Color {
    match v {
        Some(v) if v.is_finite() => {
            let d = v - center(key);
            let b = bucket(d, &edges(key));
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

/// 值 → 树图填充色。`trusted=false`（未热身 / 借横截面基线）向中性去饱和，视觉上就弱一等。
pub(crate) fn scale_color(v: Option<f64>, key: &str, p: Palette, trusted: bool) -> Color {
    let c = pick(v, key, &ramp(p), false, C_NEUTRAL);
    if trusted { c } else { fade(c, C_NEUTRAL, 0.6) }
}

/// 值 → 表格文字色（同一分档，更高亮度）。
pub(crate) fn scale_text(v: Option<f64>, key: &str, p: Palette, trusted: bool) -> Color {
    let c = pick(v, key, &ramp(p), true, C_DIM);
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
        if let Some(b) = sym.strip_suffix(q) {
            if !b.is_empty() {
                return b;
            }
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
            if let Some(t) = cat.tabs.get(i) {
                for k in &t.cols {
                    out.push(Col {
                        key: SortKey::Metric(intern(k)),
                        title: metric_title(k).to_string(),
                        width: metric_width(k),
                    });
                }
            }
        }
    }
    out
}

/// 指标键 → 列头中文名。未收录的键直接显示原键——比显示空白强，
/// 也让「加了列但忘了配名字」这件事一眼看得见。
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
fn metric_width(k: &str) -> f32 {
    ((text_units(metric_title(k)) + 2.0) * 11.0 + 8.0).clamp(78.0, 132.0)
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
                Some(x) if x.abs() >= 1e6 => usd(x),
                Some(x) => format!("{x:.2}"),
            };
            // 只有涨跌类着色，估值/比率类保持中性——把 P/E 按涨跌上色是误导
            let col = if cat.pct_keys.contains(key) && key.contains("chang")
                || key.starts_with("Perf.")
                || key == "change"
                || key == "gap"
            {
                scale_text(v, "change", p, true)
            } else {
                C_TXT
            };
            (txt, col)
        }
        SortKey::Price => (price(r.price), C_TXT),
        SortKey::Turnover => (usd(r.quote_vol_24h), C_DIM),
        SortKey::Ret(i) => (opt_pct(r.ret[i]), scale_text(r.ret[i].map(|x| (x.exp() - 1.0) * 100.0), "change", p, trusted)),
        SortKey::Z(i) => (opt_z(r.z_ret[i]), scale_text(r.z_ret[i], "own:speed_z", p, trusted)),
        SortKey::VolZ => (opt_z(r.z_vol), scale_text(r.z_vol, "own:zvol", p, trusted)),
        SortKey::CntZ => (opt_z(r.z_cnt), scale_text(r.z_cnt, "own:zvol", p, trusted)),
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
fn legend<'a>(key: &'static str, p: Palette) -> Element<'a, RadarMsg> {
    const SW: f32 = 34.0;
    let e = edges(key);
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

impl std::fmt::Display for FSel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(radar_filter::preset_label(self.fi, self.pi))
    }
}

/// 筛选栏（对齐 TradingView 筛选器顶部那排下拉）。
///
/// 收起时只显示已设的几个（点一下即清除），展开才铺开全部 19 个——
/// 一直铺着的话热图就没地方了。
fn filter_bar<'a>(v: ViewState) -> Element<'a, RadarMsg> {
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
        // 每行 4 个：19 个下拉铺成 5 行，比横向滚动好找
        let mut r = row![].spacing(6).align_y(iced::Alignment::Center);
        for (fi, d) in FILTERS.iter().enumerate() {
            let opts: Vec<FSel> = (0..=radar_filter::n_presets(fi) as u8)
                .map(|pi| FSel { fi, pi })
                .collect();
            // ⓢ = 能下推到服务端（全市场），无标记 = 只在已加载的行里筛
            r = r.push(
                text(format!("{}{} ", d.label, if d.server.is_some() { "ⓢ" } else { "" }))
                    .size(11)
                    .color(C_DIM),
            );
            r = r.push(
                pick_list(opts, Some(FSel { fi, pi: v.filters[fi] }), |o: FSel| {
                    RadarMsg::SetFilter { fi: o.fi, pi: o.pi }
                })
                .text_size(11)
                .padding([2, 6])
                .width(Length::Fixed(132.0)),
            );
            if fi % 4 == 3 || fi == N_FILTERS - 1 {
                col = col.push(std::mem::replace(
                    &mut r,
                    row![].spacing(6).align_y(iced::Alignment::Center),
                ));
            }
        }
        col = col.push(
            text(
                "带 ⓢ 的下推到服务端、在整个市场上筛（生效有一轮延迟）；\
                 其余只在已加载的行里筛。基本面类指标加密没有，设了会把加密行全筛掉",
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

use super::radar_readout::Catalog;

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
    fn of(code: &str, label: &str, region: &str, loaded: bool) -> Self {
        let shown = if region.is_empty() {
            label.to_string()
        } else if loaded {
            format!("{region} · {label}")
        } else {
            format!("{region} · {label} ＋")
        };
        MarketOpt {
            key: intern(code),
            label: intern(&shown),
        }
    }
}

/// 字符串驻留：同一份文本只泄漏一次，避免每帧重建下拉时无界泄漏。
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
        fn $name(generation: u64, v: ViewState, rows: &[RadarRow]) -> std::rc::Rc<Vec<usize>> {
            thread_local! {
                static CELL: std::cell::RefCell<Option<(u64, ViewState, std::rc::Rc<Vec<usize>>)>> =
                    const { std::cell::RefCell::new(None) };
            }
            CELL.with(|c| {
                let mut g = c.borrow_mut();
                match &*g {
                    Some((g0, vs, rc)) if *g0 == generation && *vs == v => rc.clone(),
                    _ => {
                        let f: fn(&[RadarRow], ViewState) -> Vec<usize> = $make;
                        let rc = std::rc::Rc::new(f(rows, v));
                        *g = Some((generation, v, rc.clone()));
                        rc
                    }
                }
            })
        }
    };
}

memo_idx!(memo_visible, |r, v| visible(r, v));
memo_idx!(memo_order, |r, v| order(r, v));

/// 树图取的那 180 格。同样只依赖数据版本 + 口径，不必每帧重挑一遍。
fn memo_picked(generation: u64, v: ViewState, rows: &[RadarRow]) -> std::rc::Rc<Vec<usize>> {
    thread_local! {
        static CELL: std::cell::RefCell<Option<(u64, ViewState, std::rc::Rc<Vec<usize>>)>> =
            const { std::cell::RefCell::new(None) };
    }
    CELL.with(|c| {
        let mut g = c.borrow_mut();
        match &*g {
            Some((g0, vs, rc)) if *g0 == generation && *vs == v => rc.clone(),
            _ => {
                let vis = memo_visible(generation, v, rows);
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

fn intern(s: &str) -> &'static str {
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

/// World Overview（docs/22 §2 ②）：各国指数横向对比，本币 vs 美元并列。
fn overview_view<'a>(rows: &[OverviewRow], v: ViewState) -> Element<'a, RadarMsg> {
    let mut col = column![].spacing(3);
    if rows.is_empty() {
        return text("暂无总览数据——需开启股票层（radar.toml 的 [equities]）")
            .size(11)
            .color(C_DIM)
            .into();
    }
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
                scale_text(r.local[i].map(|x| (x.exp() - 1.0) * 100.0), "change", v.palette, true),
                true,
            ));
            // 美元口径缺席就是缺席——绝不退回本币值
            tr = tr.push(cell(
                pct_log(r.usd[i]),
                82.0,
                scale_text(r.usd[i].map(|x| (x.exp() - 1.0) * 100.0), "change", v.palette, true),
                true,
            ));
        }
        col = col.push(tr);
    }
    col.into()
}

/// 市场宽度（docs/22 §2 ③）。
fn breadth_view<'a>(rows: &[BreadthRow], v: ViewState) -> Element<'a, RadarMsg> {
    let mut col = column![].spacing(3);
    if rows.is_empty() {
        return text("暂无宽度数据——需开启股票层（radar.toml 的 [equities]）")
            .size(11)
            .color(C_DIM)
            .into();
    }
    col = col.push(
        text("⚠ 口径是各市场按市值排序的前 N 只，不是全市场——这是大盘股宽度。真正的 A/D 线要拉全部上市证券。")
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
        ("只数", 46.0),
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
                cell(b.n.to_string(), 46.0, C_DIM, true),
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
            button(text("⟳ 刷新").size(11)).padding([2, 7]).on_press(RadarMsg::Refresh),
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
    let tier_txt = tiers
        .iter()
        .map(|(t, n)| format!("{t}×{n}"))
        .collect::<Vec<_>>()
        .join(" ");
    body = body.push(
        text(format!(
            "等级 {} （A=交易所直连·真实时 C=延迟约15分钟）· {} 标的 · 单轮 {}ms · z 可信 {}/{}{}",
            tier_txt,
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
    ] {
        mr = mr.push(chip(l, m == v.mode, RadarMsg::SetMode(m)));
    }
    // 筛选器的表达形式（官方页面左上角那三个小图标）。热图页没有这个选择——
    // 它本来就是热图。
    if v.mode == ViewMode::Screener {
        mr = mr.push(text("　形式 ").size(11).color(C_DIM));
        for (f, l) in [(Form::Table, "表格"), (Form::Heatmap, "热图")] {
            mr = mr.push(chip(l, f == v.form, RadarMsg::SetForm(f)));
        }
    }
    body = body.push(mr.align_y(iced::Alignment::Center));

    if !matches!(v.mode, ViewMode::Heatmap | ViewMode::Screener) {
        let inner = match v.mode {
            ViewMode::Overview => overview_view(&st.overview, v),
            _ => breadth_view(&st.breadth, v),
        };
        body = body.push(inner);
        body = body.push(
            text(format!(
                "源 {} · 快照 {}{}",
                st.source,
                st.stamp,
                super::staleness::suffix(&st.stamp),
            ))
            .size(10)
            .color(C_DIM),
        );
        return scrollable(body).width(Length::Fill).height(Length::Fill).into();
    }

    // ── 资产类过滤 ──
    // 股票的成交额远大于加密，同图时加密会被挤到几乎看不见（实测 BTC 只剩一个小格）。
    let vis = memo_visible(st.generation, v, &st.rows);
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
        for it in &st.catalog.crypto_cats {
            if !it.code.is_empty() {
                opts.push(MarketOpt::of(&it.code, &it.label, "", true));
            }
        }
    } else {
        // 已加载的来源 id（venue 中段）——用于在下拉里标注哪些是现成的
        let loaded: std::collections::HashSet<&str> = st
            .rows
            .iter()
            .filter_map(|r| r.venue.split(':').nth(1))
            .collect();
        for m in &st.catalog.markets {
            // 「所有 X 公司」+ 该市场下的每个指数（同 TradingView 的来源分组）
            opts.push(MarketOpt::of(
                &m.code,
                &format!("所有{}公司", m.label),
                &m.region,
                loaded.contains(m.code.as_str()),
            ));
            for ix in st.catalog.indices.iter().filter(|i| i.region == m.code) {
                opts.push(MarketOpt::of(
                    &ix.code,
                    &ix.label,
                    &m.region,
                    loaded.contains(ix.code.as_str()),
                ));
            }
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
    ar = ar.push(
        text(format!("　{} / {} 行", vis.len(), st.rows.len()))
            .size(10)
            .color(C_DIM),
    );
    body = body.push(ar.align_y(iced::Alignment::Center));
    // 筛选栏放在空表判断**之前**——被筛空时也得有清除的入口
    body = body.push(filter_bar(v));
    if vis.is_empty() {
        let nf = radar_filter::active_count(&v);
        body = body.push(
            text(if nf > 0 {
                // 空表最常见的原因就是筛选，而不是没数据——直接说，并给出清除入口
                format!("{nf} 个筛选把 {} 行全筛掉了——点上面的「清空」恢复", st.rows.len())
            } else if v.source.is_empty() {
                "该资产类暂无数据——股票层需在 radar.toml 的 [equities] 里开启".to_string()
            } else {
                format!(
                    "「{}」尚未加载——已通知守护去取，下一轮（≤{}s）出数据",
                    cur.to_string(),
                    60
                )
            })
            .size(11)
            .color(C_GOLD),
        );
        return scrollable(body).width(Length::Fill).height(Length::Fill).into();
    }

    // 是否要画热图：热图页恒画；筛选器页只有选了「热图」形式才画。
    let draw_map = v.mode == ViewMode::Heatmap || v.form == Form::Heatmap;

    // 「窗口」两种形式都要：表格里雷达自有的涨跌幅/速度 z 列组也按它取值。
    // 其余（大小/分组/颜色）只对热图有意义。
    if !draw_map {
        let mut wr = row![text("窗口 ").size(11).color(C_DIM)].spacing(3);
        for (i, w) in WINDOWS.iter().enumerate() {
            wr = wr.push(chip(w, i == v.win, RadarMsg::SetWindow(i)));
        }
        body = body.push(wr.align_y(iced::Alignment::Center));
    }

    // ── 热图控制条 ──（只在真要画热图时才出，否则一排无效下拉）
    let mut r1 = row![text("窗口 ").size(11).color(C_DIM)].spacing(3);
    for (i, w) in WINDOWS.iter().enumerate() {
        r1 = r1.push(chip(w, i == v.win, RadarMsg::SetWindow(i)));
    }
    r1 = r1.push(text("　大小 ").size(11).color(C_DIM));
    r1 = r1.push(
        pick_list(SIZE_OPTS.to_vec(), Some(v.size), RadarMsg::SetSize)
            .text_size(11)
            .padding([2, 6]),
    );
    r1 = r1.push(text("　分组 ").size(11).color(C_DIM));
    for (g, l) in [
        (GroupBy::None, "无"),
        (GroupBy::Venue, "venue"),
        (GroupBy::Country, "国别"),
        (GroupBy::Sector, "板块"),
    ] {
        r1 = r1.push(chip(l, g == v.group_by, RadarMsg::SetGroupBy(g)));
    }
    if draw_map {
        body = body.push(r1.align_y(iced::Alignment::Center));
    }

    let mut r2 = row![text("颜色 ").size(11).color(C_DIM)].spacing(3);
    r2 = r2.push(
        pick_list(COLOR_OPTS.to_vec(), Some(v.color), RadarMsg::SetColor)
            .text_size(11)
            .padding([2, 6]),
    );
    r2 = r2.push(text("　").size(11));
    r2 = r2.push(legend(v.color.key, v.palette));
    if draw_map {
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
    let picked = memo_picked(st.generation, v, &st.rows);
    // 选中的大小口径对当前这批行没有数据 → 权重全 0 → 树图整个是空的。
    // 空白且无提示是最难排查的一种坏（实测：切到加密后图没了，因为默认口径是
    // 市值而 Binance 不给市值）。这里说清楚缺的是哪个口径，并给出可用的替代。
    let sized = picked
        .iter()
        .filter(|&&i| metric_value(&st.rows[i], v.size.key, v.win).is_some_and(|x| x > 0.0))
        .count();
    if sized == 0 {
        let alt = SIZE_OPTS.iter().find(|o| {
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
            color: scale_color(m, v.color.key, v.palette, r.trustworthy()),
        }
    };
    /// 一行归入哪个分组。空值统一落到「其他」，免得散成一堆无名组。
    fn group_key(r: &RadarRow, g: GroupBy) -> String {
        let pick = |s: &str, fallback: &str| {
            if s.is_empty() { fallback.to_string() } else { s.to_string() }
        };
        match g {
            GroupBy::None => String::new(),
            GroupBy::Venue => r.venue.trim_start_matches("binance:").to_string(),
            // 加密没有国别/板块，单独成组而不是混进「其他」
            GroupBy::Country => pick(&r.country, if r.tier == "A" { "加密" } else { "其他" }),
            GroupBy::Sector => pick(&r.sector, if r.tier == "A" { "加密" } else { "其他" }),
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
        .height(Length::Fixed(340.0)),
    );
    }

    // ── Screener（表格）──
    if v.mode == ViewMode::Heatmap {
        // 热图页不带表格。原来两者挤在一个视图里，控制条上一半的下拉
        // 对当前内容无效——那正是「乱」的来源。
        return scrollable(body).width(Length::Fill).height(Length::Fill).into();
    }
    let cols = columns(v, &st.catalog);
    // 切到 TV 列组后，原排序键（如 5m 速度z）可能不在显示的列里——表头上就没有
    // 排序指示，用户看不出表是按什么排的。退到该列组的第一个数值列。
    let mut ev = v;
    if !cols.iter().any(|c| c.key == v.sort) {
        if let Some(c) = cols.iter().find(|c| is_numeric(c.key)) {
            ev.sort = c.key;
            ev.desc = true;
        }
    }
    let idx = memo_order(st.generation, ev, &st.rows);
    let mut cs = row![text("列组 ").size(11).color(C_DIM)].spacing(3);
    for (c, l) in [
        (ColumnSet::Speed, "涨跌幅"),
        (ColumnSet::SpeedZ, "速度 z"),
        (ColumnSet::Reference, "参考"),
    ] {
        cs = cs.push(chip(l, c == v.cols, RadarMsg::SetColumns(c)));
    }
    for (i, t) in st.catalog.tabs.iter().enumerate() {
        let c = ColumnSet::Tv(i);
        cs = cs.push(chip(&t.label, c == v.cols, RadarMsg::SetColumns(c)));
    }
    let shown = idx.len().min(80);
    cs = cs.push(
        text(format!("　{} / {} 条 · 点列头排序", shown, idx.len()))
            .size(10)
            .color(C_DIM),
    );
    cs = cs.push(text("　色板 ").size(11).color(C_DIM));
    for (pal, l) in [
        (Palette::BlueOrange, "蓝橙"),
        (Palette::GreenUp, "绿涨红跌"),
        (Palette::RedUp, "红涨绿跌"),
    ] {
        cs = cs.push(chip(l, pal == v.palette, RadarMsg::SetPalette(pal)));
    }
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

        let first = memo_order(1, v, &rows);
        assert_eq!(rows[first[0]].symbol, "B");
        assert!(std::rc::Rc::ptr_eq(&first, &memo_order(1, v, &rows)), "同键该命中缓存");

        v.desc = false;
        assert_eq!(rows[memo_order(1, v, &rows)[0]].symbol, "A", "改了排序方向却没重算");

        let rows2 = vec![memo_row("C", 9.0)];
        assert_eq!(rows2[memo_order(2, v, &rows2)[0]].symbol, "C", "数据换了一轮却没重算");
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
        for k in ALL_TAB_COLS {
            let need = (text_units(metric_title(k)) + 2.0) * 11.0 + 8.0;
            assert!(
                metric_width(k) >= need,
                "{k}（{}）需要 {need:.0}px，只给了 {:.0}px",
                metric_title(k),
                metric_width(k)
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
                let c = scale_color(Some(probe), cb, P, true);
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
        let up = scale_color(Some(4.0), "own:speed_z", P, true);
        let down = scale_color(Some(-4.0), "own:speed_z", P, true);
        assert!(up.b > up.r, "涨端应偏蓝");
        assert!(down.r > down.b, "跌端应偏橙");
        let dist = |c: Color| (c.r - C_NEUTRAL.r).abs() + (c.b - C_NEUTRAL.b).abs();
        assert!(dist(scale_color(Some(4.0), "own:speed_z", P, false)) < dist(up));
    }

    #[test]
    fn missing_value_is_neutral() {
        let c = scale_color(None, "own:speed_z", P, true);
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
        let sub = super::visible(&rows, { let mut v = ViewState::DEFAULT; v.asset = AssetFilter::Coin; v });
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
        let up1 = scale_color(Some(0.4), "own:speed_z", P, true);
        let dn1 = scale_color(Some(-0.4), "own:speed_z", P, true);
        let neu = scale_color(Some(0.0), "own:speed_z", P, true);
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
            let fill = scale_color(Some(probe), "own:speed_z", P, true);
            let txt = scale_text(Some(probe), "own:speed_z", P, true);
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
        let up = scale_text(Some(3.0), "own:speed_z", P, true);
        let dn = scale_text(Some(-3.0), "own:speed_z", P, true);
        assert!(up.b > up.r, "涨端应偏蓝");
        assert!(dn.r > dn.b, "跌端应偏橙");
        assert!((scale_text(None, "own:speed_z", P, true).r - C_DIM.r).abs() < 1e-6);
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
