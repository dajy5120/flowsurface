//! 「图表参数」视图的数据（docs/33）：快照 `chart` 段的解析，与每一行取值。
//!
//! **面板不算任何数**：特征行读快照 `slots` 里的那个值，图表参数行读 `chart.values`。
//! 卡片定义（哪张卡、哪几行、什么格式）随快照下发，与引擎版本一致——面板不另存一份。

use super::feature_matrix_readout::{Matrix, Slot};

/// 卡片上一行。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Row {
    pub label: String,
    pub key: String,
    /// `feature` / `chart`。
    pub src: String,
    pub fmt: String,
    pub window_ms: Option<u32>,
    pub formula: String,
    pub sierra: String,
    pub status: String,
}

impl Row {
    #[must_use]
    pub fn is_chart(&self) -> bool {
        self.src == "chart"
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Card {
    pub id: String,
    pub title: String,
    pub sierra: String,
    pub question: String,
    pub ladder: bool,
    pub markets: Vec<String>,
    pub rows: Vec<Row>,
}

/// 一个图表参数的值。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChartValue {
    pub v: Option<f64>,
    pub text: Option<String>,
    pub quality: String,
    pub reason: String,
}

/// 快照的 `chart` 段。
#[derive(Debug, Clone, Default)]
pub struct ChartSnap {
    /// 引擎带了 `chart` 段（旧版引擎没有）。
    pub present: bool,
    pub enabled: bool,
    pub cards: Vec<Card>,
    pub values: std::collections::HashMap<String, ChartValue>,
    /// 口径参数（原样，按名取）。
    pub params: serde_json::Map<String, serde_json::Value>,
}

fn s(v: &serde_json::Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

#[must_use]
pub fn parse(v: &serde_json::Value) -> ChartSnap {
    let c = &v["chart"];
    if !c.is_object() {
        return ChartSnap::default();
    }
    let cards = c["cards"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|k| Card {
            id: s(&k["id"]),
            title: s(&k["title"]),
            sierra: s(&k["sierra"]),
            question: s(&k["question"]),
            ladder: k["ladder"].as_bool().unwrap_or(false),
            markets: k["markets"].as_array().into_iter().flatten().map(s).collect(),
            rows: k["rows"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|r| Row {
                    label: s(&r["label"]),
                    key: s(&r["key"]),
                    src: s(&r["src"]),
                    fmt: s(&r["fmt"]),
                    window_ms: r["window_ms"].as_u64().map(|x| x as u32),
                    formula: s(&r["formula"]),
                    sierra: s(&r["sierra"]),
                    status: s(&r["status"]),
                })
                .collect(),
        })
        .collect();
    let values = c["values"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|x| {
            (
                s(&x["key"]),
                ChartValue {
                    v: x["v"].as_f64(),
                    text: x["text"].as_str().map(str::to_string),
                    quality: s(&x["quality"]),
                    reason: s(&x["reason"]),
                },
            )
        })
        .collect();
    ChartSnap {
        present: true,
        enabled: c["enabled"].as_bool().unwrap_or(false),
        cards,
        values,
        params: c["params"].as_object().cloned().unwrap_or_default(),
    }
}

/// 一行解析出来的显示状态。
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    /// 有值（数值 / 文本、质量、实际用到的窗口）。
    Value { v: Option<f64>, text: Option<String>, quality: String, window_ms: Option<u32> },
    /// 已登记·待实现（图表参数的 `status = spec`，或特征本身未实现）。
    Pending,
    /// 本部署没启用这条特征。
    Disabled,
    /// 取不到值（不可用 / 样本不足 / 不适用……），附原因。
    Missing(String),
}

/// 按键索引特征 slot（key → [(窗口, slot)]），一帧建一次。
pub struct SlotIndex<'a> {
    by_key: std::collections::HashMap<&'a str, Vec<&'a Slot>>,
}

impl<'a> SlotIndex<'a> {
    #[must_use]
    pub fn new(m: &'a Matrix) -> Self {
        let mut by_key: std::collections::HashMap<&str, Vec<&Slot>> = std::collections::HashMap::new();
        for sl in &m.slots {
            by_key.entry(sl.key.as_str()).or_default().push(sl);
        }
        Self { by_key }
    }

    /// 该特征最接近 `want` 的那个窗口（运行时自定义窗口里可能没有字典窗口）。
    #[must_use]
    pub fn slot(&self, key: &str, want: Option<u32>) -> Option<&'a Slot> {
        let v = self.by_key.get(key)?;
        let want = i64::from(want.unwrap_or(0));
        v.iter().min_by_key(|s| (i64::from(s.window_ms) - want).abs()).copied()
    }
}

/// 一行的显示状态。
#[must_use]
pub fn cell(row: &Row, idx: &SlotIndex<'_>, chart: &ChartSnap) -> Cell {
    if row.is_chart() {
        if !chart.enabled {
            return Cell::Missing("图表参数层未启用".into());
        }
        return match chart.values.get(&row.key) {
            Some(cv) if cv.v.is_some() || cv.text.is_some() => Cell::Value {
                v: cv.v,
                text: cv.text.clone(),
                quality: cv.quality.clone(),
                window_ms: None,
            },
            Some(cv) => Cell::Missing(if cv.reason.is_empty() { cv.quality.clone() } else { cv.reason.clone() }),
            None if row.status == "spec" => Cell::Pending,
            None => Cell::Missing("引擎没有输出这个参数".into()),
        };
    }
    let Some(sl) = idx.slot(&row.key, row.window_ms) else {
        return Cell::Missing("快照里没有这个特征".into());
    };
    if sl.not_implemented() {
        return Cell::Pending;
    }
    if sl.disabled() {
        return Cell::Disabled;
    }
    match sl.value {
        Some(v) => Cell::Value { v: Some(v), text: None, quality: sl.quality.clone(), window_ms: Some(sl.window_ms) },
        None => Cell::Missing(if sl.reason.is_empty() { sl.quality.clone() } else { sl.reason.clone() }),
    }
}

/// 卡片在当前市场是否显示（卡片没限定市场 / 引擎没报市场 → 显示）。
#[must_use]
pub fn card_visible(card: &Card, market: Option<&str>) -> bool {
    card.markets.is_empty() || market.is_none_or(|m| card.markets.iter().any(|x| x == m))
}

/// 价格的小数位：按 tick（0.1 → 1 位，0.25 → 2 位，0.01 → 2 位）；没有 tick 按量级。
#[must_use]
pub fn price_decimals(tick: Option<f64>, v: f64) -> usize {
    if let Some(t) = tick.filter(|t| *t > 0.0 && t.is_finite()) {
        let mut d = 0;
        let mut x = t;
        while d < 8 && (x - x.round()).abs() > 1e-9 {
            x *= 10.0;
            d += 1;
        }
        return d;
    }
    let a = v.abs();
    if a >= 1_000.0 {
        2
    } else if a >= 1.0 {
        3
    } else {
        5
    }
}

/// 按行的格式把值变成文本。
#[must_use]
pub fn format(fmt: &str, v: Option<f64>, text: Option<&str>, tick: Option<f64>) -> String {
    if let Some(t) = text {
        return t.to_string();
    }
    let Some(v) = v else { return "—".into() };
    if !v.is_finite() {
        return "—".into();
    }
    match fmt {
        "price" => format!("{v:.*}", price_decimals(tick, v)),
        "bps" => format!("{v:.2} bp"),
        "pct" => format!("{v:.1}%"),
        "ratio" => format!("{v:.3}"),
        "count" => format!("{v:.0}"),
        "flag" => if v != 0.0 { "是".into() } else { "否".into() },
        "ms" => {
            let a = v.abs();
            if a >= 60_000.0 {
                format!("{:.1} 分", v / 60_000.0)
            } else if a >= 1_000.0 {
                format!("{:.1} 秒", v / 1_000.0)
            } else {
                format!("{v:.0} ms")
            }
        }
        "qty" => {
            let a = v.abs();
            if a >= 1e6 {
                format!("{:.2}M", v / 1e6)
            } else if a >= 1e4 {
                format!("{:.1}k", v / 1e3)
            } else if a >= 100.0 {
                format!("{v:.0}")
            } else if a >= 1.0 {
                format!("{v:.2}")
            } else {
                format!("{v:.4}")
            }
        }
        _ => {
            let a = v.abs();
            if a >= 1_000.0 { format!("{v:.1}") } else { format!("{v:.3}") }
        }
    }
}

/// 每张卡片底部显示哪些口径参数（docs/33 §5）。
#[must_use]
pub fn card_params(card_id: &str) -> &'static [&'static str] {
    match card_id {
        "c01" => &["bar_period_ms", "pivot_formula", "atr_length", "atr_ma", "ema_lengths"],
        "c02" => &["bar_period_ms"],
        "c03" => &["value_area_pct", "profile_ticks_per_level"],
        "c04" => &["tpo_period_min", "ib_periods", "ib_ext_pcts", "opening_range_min", "poor_extreme_ticks", "value_area_pct"],
        "c05" => &["vwap_band_method", "vwap_band_mults"],
        "c06" => &["bar_period_ms", "imbalance_ratio_pct", "stacked_levels"],
        "c07" => &["cvd_reset", "bar_period_ms"],
        "c08" => &["depth_levels", "recent_traded_secs"],
        "c09" => &["heatmap_levels"],
        "c10" => &["bar_period_ms"],
        "c11" => &["large_trade_mode", "large_trade_qty", "large_trade_median_pct"],
        _ => &[],
    }
}

/// 口径参数的中文名。
#[must_use]
pub fn param_label(k: &str) -> &'static str {
    match k {
        "bar_period_ms" => "按根周期",
        "pivot_formula" => "枢轴公式",
        "atr_length" => "ATR 长度",
        "atr_ma" => "ATR 均线",
        "ema_lengths" => "EMA 长度",
        "value_area_pct" => "价值区",
        "profile_ticks_per_level" => "每档 tick",
        "tpo_period_min" => "TPO 字母",
        "ib_periods" => "IB 时段数",
        "ib_ext_pcts" => "IB 扩展",
        "opening_range_min" => "开盘区间",
        "poor_extreme_ticks" => "Poor 容差",
        "vwap_band_method" => "带宽算法",
        "vwap_band_mults" => "带倍数",
        "imbalance_ratio_pct" => "失衡比例",
        "stacked_levels" => "堆叠档数",
        "cvd_reset" => "CVD 重置",
        "depth_levels" => "深度档数",
        "recent_traded_secs" => "近期成交窗口",
        "heatmap_levels" => "热图档数",
        "large_trade_mode" => "大单模式",
        "large_trade_qty" => "大单固定阈值",
        "large_trade_median_pct" => "高于中位数",
        _ => "",
    }
}

/// 口径参数值的显示（`bar_period_ms` 60000 → 1 分钟，`value_area_pct` 70 → 70%）。
#[must_use]
pub fn param_text(k: &str, v: &serde_json::Value) -> String {
    let n = v.as_f64();
    match (k, n) {
        ("bar_period_ms", Some(ms)) => {
            if ms >= 60_000.0 { format!("{} 分钟", ms / 60_000.0) } else { format!("{} 秒", ms / 1_000.0) }
        }
        ("value_area_pct" | "imbalance_ratio_pct" | "large_trade_median_pct", Some(x)) => format!("{x}%"),
        ("tpo_period_min" | "opening_range_min", Some(x)) => format!("{x} 分钟"),
        ("recent_traded_secs", Some(x)) => format!("{x} 秒"),
        _ => match v {
            serde_json::Value::String(t) => t.clone(),
            serde_json::Value::Array(a) => a.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(" / "),
            other => other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 价格小数位按_tick() {
        assert_eq!(price_decimals(Some(0.1), 60000.0), 1);
        assert_eq!(price_decimals(Some(0.25), 6500.0), 2);
        assert_eq!(price_decimals(Some(0.01), 230.0), 2);
        assert_eq!(price_decimals(Some(1.0), 5.0), 0);
        assert_eq!(format("price", Some(60554.56), None, Some(0.1)), "60554.6");
    }

    #[test]
    fn 解析卡片并按窗口就近取特征() {
        let text = r#"{"slots":[
            {"key":"cvd","window_ms":30000,"value":1.0,"quality":"GOOD","status":"impl","default_on":true},
            {"key":"cvd","window_ms":300000,"value":5.0,"quality":"GOOD","status":"impl","default_on":true}],
          "chart":{"enabled":true,"cards":[{"id":"c07","title":"Delta","sierra":"x","question":"q","ladder":false,"markets":[],
            "rows":[{"label":"CVD","key":"cvd","src":"feature","fmt":"qty","window_ms":60000,"status":"impl"},
                    {"label":"CVD 高","key":"bar_cvd_high","src":"chart","fmt":"qty","window_ms":null,"status":"spec"}]}],
            "values":[],"params":{"cvd_reset":"session"}}}"#;
        let m = super::super::feature_matrix_readout::parse(text);
        let c = &m.chart;
        assert!(c.present && c.enabled);
        assert_eq!(c.cards.len(), 1);
        let idx = SlotIndex::new(&m);
        // 60s 不在运行时窗口里 → 就近取 30s
        assert!(matches!(cell(&c.cards[0].rows[0], &idx, c), Cell::Value { v: Some(x), window_ms: Some(30000), .. } if x == 1.0));
        assert_eq!(cell(&c.cards[0].rows[1], &idx, c), Cell::Pending);
        assert_eq!(param_text("cvd_reset", &c.params["cvd_reset"]), "session");
    }

    #[test]
    fn 衍生品卡片只在永续与期货显示() {
        let card = Card { markets: vec!["crypto_perp".into(), "futures".into()], ..Card::default() };
        assert!(card_visible(&card, Some("futures")));
        assert!(!card_visible(&card, Some("equity")));
        assert!(card_visible(&card, None));
    }
}
