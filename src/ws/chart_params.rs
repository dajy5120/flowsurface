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
    /// 可编辑口径参数的规格（引擎下发）。
    pub editable: Vec<EditSpec>,
    /// 只读口径参数与原因（引擎下发）。
    pub read_only: Vec<(String, String)>,
}

/// 一个可编辑口径参数的规格（与引擎 `chart::EDITABLE` 同一份，随快照下发）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EditSpec {
    pub key: String,
    pub lo: f64,
    pub hi: f64,
    /// 1 = 一个数；2、3 = 一组从小到大的数；0 = 选项。
    pub n: usize,
    pub options: Vec<String>,
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
        editable: c["editable"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|e| EditSpec {
                key: s(&e["key"]),
                lo: e["lo"].as_f64().unwrap_or(0.0),
                hi: e["hi"].as_f64().unwrap_or(0.0),
                n: e["n"].as_u64().unwrap_or(1) as usize,
                options: e["options"].as_array().into_iter().flatten().map(s).collect(),
            })
            .collect(),
        read_only: c["read_only"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|x| (s(&x[0]), s(&x[1])))
            .collect(),
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
        "c11" => &["large_trade_mode"],
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
        "large_trade_mode" => "大单阈值",
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
        ("heatmap_levels" | "depth_levels", Some(x)) => format!("前 {x} 档"),
        ("large_trade_mode", _) if v.as_str() == Some("p95_5m") => "近 5 分钟单笔 P95（与大单特征同一阈值）".into(),
        ("pivot_formula", _) => match v.as_str() {
            Some("standard") => "标准".into(),
            Some("fibonacci") => "斐波那契".into(),
            Some("camarilla") => "Camarilla".into(),
            Some("woodie") => "Woodie".into(),
            _ => v.to_string(),
        },
        ("vwap_band_method", _) if v.as_str() == Some("vwap_variance") => "VWAP 方差".into(),
        ("cvd_reset", _) if v.as_str() == Some("session") => "按交易日".into(),
        ("atr_ma", _) if v.as_str() == Some("wilder") => "Wilder".into(),
        _ => match v {
            serde_json::Value::String(t) => t.clone(),
            serde_json::Value::Array(a) => a.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(" / "),
            other => other.to_string(),
        },
    }
}

// ── 图上设置 vs 卡片口径（docs/33 批 4）───────────────────────────────────────────

/// 订单流特征工作区里一张行情图的设置（主循环每帧从 dashboard 取一次发布过来）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneChart {
    /// Footprint（否则是 K 线）。
    pub footprint: bool,
    /// 时间周期（按笔数聚合的图为 `None`）。
    pub timeframe_ms: Option<u64>,
    /// Footprint 失衡研究的阈值（flowsurface 口径：对侧 × (100 + 阈值)% 才算失衡）。
    pub imbalance: Option<usize>,
}

static PANES: std::sync::Mutex<Vec<PaneChart>> = std::sync::Mutex::new(Vec::new());

/// 主循环发布图上设置（变了才替换）。
pub fn publish_pane_charts(v: Vec<PaneChart>) {
    if let Ok(mut g) = PANES.lock()
        && *g != v
    {
        *g = v;
    }
}

fn fmt_ms(ms: u64) -> String {
    if ms.is_multiple_of(60_000) { format!("{} 分钟", ms / 60_000) } else { format!("{} 秒", ms / 1_000) }
}

/// 某张卡片与图上设置不一致的地方（给人看的提示，空 = 一致或没有对应的图）。
#[must_use]
pub fn mismatches(card_id: &str, params: &serde_json::Map<String, serde_json::Value>) -> Vec<String> {
    let panes = PANES.lock().map(|g| g.clone()).unwrap_or_default();
    let mut out = Vec::new();
    let bar = params.get("bar_period_ms").and_then(serde_json::Value::as_u64);
    if matches!(card_id, "c01" | "c02" | "c06" | "c07" | "c10" | "c11")
        && let Some(b) = bar
    {
        {
            let mut seen = std::collections::BTreeSet::new();
            for p in &panes {
                if let Some(tf) = p.timeframe_ms.filter(|tf| *tf != b)
                    && seen.insert((p.footprint, tf))
                {
                    out.push(format!(
                        "图上{}是 {}，本卡的「当前根 / 上一根」按 {} 算——两边的根不是同一根",
                        if p.footprint { " Footprint " } else { "的 K 线" },
                        fmt_ms(tf),
                        fmt_ms(b)
                    ));
                }
            }
        }
    }
    if card_id == "c06" {
        let ratio = params.get("imbalance_ratio_pct").and_then(serde_json::Value::as_f64);
        for p in panes.iter().filter(|p| p.footprint) {
            if let (Some(t), Some(r)) = (p.imbalance, ratio) {
                let fs = 100.0 + t as f64;
                if (fs - r).abs() > 1e-9 {
                    out.push(format!(
                        "图上 Footprint 的失衡阈值是 {fs:.0}%（flowsurface 设置 {t}），本卡与特征按 {r:.0}% 算——图上标出的失衡格子与本卡的失衡数不是同一口径"
                    ));
                }
            }
        }
    }
    out
}

// ── 卡片列宽（表头拖分隔线调；所有卡片共用，存盘）──────────────────────────────

/// 卡片表格里可调宽的列（「参数」列占剩下的宽度）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChartCol {
    Value,
    Arrow,
    Tag,
}

impl ChartCol {
    pub const ALL: [Self; 3] = [Self::Value, Self::Arrow, Self::Tag];

    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Value => "value",
            Self::Arrow => "arrow",
            Self::Tag => "tag",
        }
    }

    #[must_use]
    pub const fn default_w(self) -> f32 {
        match self {
            Self::Value => 110.0,
            Self::Arrow => 48.0,
            Self::Tag => 22.0,
        }
    }
}

/// 列宽的上下限。
pub const COL_MIN: f32 = 16.0;
pub const COL_MAX: f32 = 320.0;

/// 列宽消息。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ChartUiMsg {
    /// 在某条分隔线上按下：`(左边那列, 右边那列, 两列当前宽度)`。左边是「参数」列时为 `None`
    /// （参数列不存宽度，占卡片剩下的宽度）。往右拖 d：左列宽 d、右列窄 d——分隔线跟着鼠标走，别的列不动。
    DragStart(Option<ChartCol>, ChartCol, f32, f32),
    /// 鼠标在表头里移动（相对表头的 x）。
    Move(f32),
    DragEnd,
    /// 双击分隔线：这一列回到默认宽度。
    Auto(ChartCol),
}

#[derive(Debug, Clone, Default)]
struct Ui {
    widths: std::collections::HashMap<ChartCol, f32>,
    mouse_x: f32,
    /// (左列, 右列, 按下时的 x, 左列宽, 右列宽)。
    drag: Option<(Option<ChartCol>, ChartCol, f32, f32, f32)>,
}

static UI: std::sync::OnceLock<std::sync::Mutex<Ui>> = std::sync::OnceLock::new();

fn ui_path() -> std::path::PathBuf {
    super::paths::data_dir().join("cockpit").join("chart_params_ui.json")
}

fn ui_cell() -> &'static std::sync::Mutex<Ui> {
    UI.get_or_init(|| {
        let v: serde_json::Value =
            std::fs::read_to_string(ui_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let widths = ChartCol::ALL
            .iter()
            .filter_map(|c| v["widths"][c.key()].as_f64().map(|w| (*c, (w as f32).clamp(COL_MIN, COL_MAX))))
            .collect();
        std::sync::Mutex::new(Ui { widths, ..Ui::default() })
    })
}

/// 某列当前宽度。
#[must_use]
pub fn col_w(c: ChartCol) -> f32 {
    ui_cell().lock().ok().and_then(|g| g.widths.get(&c).copied()).unwrap_or(c.default_w())
}

fn save_ui(g: &Ui) {
    let o: serde_json::Map<String, serde_json::Value> =
        g.widths.iter().map(|(c, w)| (c.key().to_string(), serde_json::json!(w.round()))).collect();
    let p = ui_path();
    let _ = std::fs::create_dir_all(p.parent().unwrap_or(std::path::Path::new(".")));
    let _ = std::fs::write(p, serde_json::json!({ "widths": o }).to_string());
}

/// 处理列宽消息。
pub fn handle_ui(m: ChartUiMsg) {
    let Ok(mut g) = ui_cell().lock() else { return };
    match m {
        ChartUiMsg::Move(x) => {
            g.mouse_x = x;
            if let Some((left, right, x0, wl, wr)) = g.drag {
                // 两列都要留在上下限内：先按右列能让出 / 能接收的量截住 d，左列同样截
                let mut d = x - x0;
                d = d.clamp(wr - COL_MAX, wr - COL_MIN);
                if left.is_some() {
                    d = d.clamp(COL_MIN - wl, COL_MAX - wl);
                }
                g.widths.insert(right, wr - d);
                if let Some(l) = left {
                    g.widths.insert(l, wl + d);
                }
            }
        }
        ChartUiMsg::DragStart(left, right, wl, wr) => {
            let x = g.mouse_x;
            g.drag = Some((left, right, x, wl, wr));
        }
        ChartUiMsg::DragEnd => {
            // 鼠标每次离开表头都会发 DragEnd：没在拖就什么都不做，别每次都写文件
            if g.drag.take().is_some() {
                save_ui(&g);
            }
        }
        ChartUiMsg::Auto(c) => {
            g.widths.remove(&c);
            g.drag = None;
            save_ui(&g);
        }
    }
}

// ── 口径设置（docs/33 批 4）────────────────────────────────────────────────────

/// 口径设置的消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChartEditMsg {
    Open,
    Close,
    /// `(参数, 输入框文本)`。
    Input(String, String),
    /// 应用：写配置文件 `chart` 段并重启特征引擎。
    Apply,
    /// 恢复默认：删掉配置里的 `chart` 段并重启。
    Defaults,
}

/// 编辑中的草稿：每个可编辑参数一段文本（`bar_period_ms` 以秒显示，数组用逗号分隔）。
#[derive(Debug, Clone, Default)]
pub struct Draft {
    pub text: std::collections::BTreeMap<String, String>,
    pub error: String,
}

static DRAFT: std::sync::OnceLock<std::sync::Mutex<Option<Draft>>> = std::sync::OnceLock::new();

fn draft_cell() -> &'static std::sync::Mutex<Option<Draft>> {
    DRAFT.get_or_init(|| std::sync::Mutex::new(None))
}

/// 当前的草稿（没在编辑是 `None`）。
#[must_use]
pub fn draft() -> Option<Draft> {
    draft_cell().lock().ok().and_then(|g| g.clone())
}

/// 参数值 → 输入框文本。
#[must_use]
pub fn to_text(key: &str, v: &serde_json::Value) -> String {
    match (key, v) {
        ("bar_period_ms", _) => v.as_f64().map_or_else(String::new, |x| format!("{}", x / 1_000.0)),
        (_, serde_json::Value::Array(a)) => a.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(", "),
        (_, serde_json::Value::String(t)) => t.clone(),
        _ => v.to_string(),
    }
}

/// 输入框文本 → 配置值；不合规返回给人看的原因。
pub fn from_text(spec: &EditSpec, t: &str) -> Result<serde_json::Value, String> {
    let label = param_label(&spec.key);
    if spec.n == 0 {
        let t = t.trim();
        return if spec.options.iter().any(|o| o == t) {
            Ok(serde_json::json!(t))
        } else {
            Err(format!("{label} 只能是 {}", spec.options.join(" / ")))
        };
    }
    let nums: Result<Vec<f64>, _> = t.split([',', '，', ' ']).filter(|x| !x.trim().is_empty()).map(|x| x.trim().parse::<f64>()).collect();
    let Ok(mut nums) = nums else { return Err(format!("{label} 要填数字")) };
    if spec.key == "bar_period_ms" {
        for x in &mut nums {
            *x *= 1_000.0;
        }
    }
    let (lo, hi) = if spec.key == "bar_period_ms" { (spec.lo / 1_000.0, spec.hi / 1_000.0) } else { (spec.lo, spec.hi) };
    if nums.len() != spec.n {
        return Err(format!("{label} 要填 {} 个数", spec.n));
    }
    if nums.iter().any(|x| !(spec.lo..=spec.hi).contains(x)) {
        return Err(format!("{label} 要在 {lo}–{hi} 之间"));
    }
    if nums.windows(2).any(|w| w[0] > w[1]) {
        return Err(format!("{label} 要从小到大"));
    }
    Ok(if spec.n == 1 { serde_json::json!(nums[0]) } else { serde_json::json!(nums) })
}

/// 处理一条口径设置消息。`write` = 写配置并重启引擎（特征矩阵那一份 `write_and_restart`，不另写一套）。
pub fn handle_edit(m: ChartEditMsg, snap: &ChartSnap, write: impl FnOnce(String, Box<dyn FnOnce(&mut serde_json::Value)>)) {
    let Ok(mut g) = draft_cell().lock() else { return };
    match m {
        ChartEditMsg::Open => {
            let text = snap
                .editable
                .iter()
                .map(|e| (e.key.clone(), snap.params.get(&e.key).map_or_else(String::new, |v| to_text(&e.key, v))))
                .collect();
            *g = Some(Draft { text, error: String::new() });
        }
        ChartEditMsg::Close => *g = None,
        ChartEditMsg::Input(k, t) => {
            if let Some(d) = g.as_mut() {
                d.text.insert(k, t);
                d.error.clear();
            }
        }
        ChartEditMsg::Defaults => {
            *g = None;
            drop(g);
            write("图表参数口径 → 默认".into(), Box::new(|v: &mut serde_json::Value| {
                if let Some(o) = v.as_object_mut() {
                    o.remove("chart");
                }
            }));
        }
        ChartEditMsg::Apply => {
            let Some(d) = g.as_mut() else { return };
            let mut obj = serde_json::Map::new();
            for e in &snap.editable {
                let t = d.text.get(&e.key).cloned().unwrap_or_default();
                match from_text(e, &t) {
                    Ok(v) => {
                        obj.insert(e.key.clone(), v);
                    }
                    Err(why) => {
                        d.error = why;
                        return;
                    }
                }
            }
            *g = None;
            drop(g);
            write("图表参数口径已更新".into(), Box::new(move |v: &mut serde_json::Value| {
                if !v.is_object() {
                    *v = serde_json::json!({});
                }
                v["chart"] = serde_json::Value::Object(obj);
            }));
        }
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
        assert_eq!(param_text("cvd_reset", &c.params["cvd_reset"]), "按交易日");
    }

    #[test]
    fn 图上设置与口径比对() {
        let params: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"bar_period_ms": 60000, "imbalance_ratio_pct": 300}"#).unwrap();
        // flowsurface 阈值 200 = 对侧 × 300% → 与 300% 一致；M1 = 60 秒一致
        publish_pane_charts(vec![PaneChart { footprint: true, timeframe_ms: Some(60_000), imbalance: Some(200) }]);
        assert!(mismatches("c06", &params).is_empty());
        publish_pane_charts(vec![
            PaneChart { footprint: true, timeframe_ms: Some(300_000), imbalance: Some(150) },
            PaneChart { footprint: false, timeframe_ms: Some(60_000), imbalance: None },
        ]);
        let w = mismatches("c06", &params);
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w.iter().any(|x| x.contains("250%")) && w.iter().any(|x| x.contains("5 分钟")));
        assert!(mismatches("c03", &params).is_empty(), "Volume Profile 不按根，不比周期");
        publish_pane_charts(Vec::new());
    }

    #[test]
    fn 口径输入框解析() {
        let bar = EditSpec { key: "bar_period_ms".into(), lo: 10_000.0, hi: 3_600_000.0, n: 1, options: vec![] };
        assert_eq!(from_text(&bar, "30").unwrap(), serde_json::json!(30_000.0));
        assert!(from_text(&bar, "5").is_err(), "秒数换成毫秒后比下限小");
        let m3 = EditSpec { key: "vwap_band_mults".into(), lo: 0.1, hi: 10.0, n: 3, options: vec![] };
        assert_eq!(from_text(&m3, "1, 2，3").unwrap(), serde_json::json!([1.0, 2.0, 3.0]));
        assert!(from_text(&m3, "2, 1, 3").is_err());
        let pv = EditSpec { key: "pivot_formula".into(), lo: 0.0, hi: 0.0, n: 0, options: vec!["standard".into(), "woodie".into()] };
        assert!(from_text(&pv, "woodie").is_ok() && from_text(&pv, "x").is_err());
        assert_eq!(to_text("bar_period_ms", &serde_json::json!(60000)), "60");
    }

    /// 拖一条分隔线只在它左右两列之间转移宽度（线跟着鼠标走、其余列不动），且两列都不越界。
    /// 只发 Move / DragStart（不发 DragEnd / Auto，那两个会写盘）。
    #[test]
    fn 拖分隔线在相邻两列之间转移宽度() {
        let (v0, a0) = (col_w(ChartCol::Value), col_w(ChartCol::Arrow));
        handle_ui(ChartUiMsg::Move(100.0));
        handle_ui(ChartUiMsg::DragStart(Some(ChartCol::Value), ChartCol::Arrow, v0, a0));
        handle_ui(ChartUiMsg::Move(110.0));
        assert_eq!(col_w(ChartCol::Value), v0 + 10.0);
        assert_eq!(col_w(ChartCol::Arrow), a0 - 10.0);
        // 拖过头：右列到下限就停，两列之和不变
        handle_ui(ChartUiMsg::Move(100.0 + 10_000.0));
        assert_eq!(col_w(ChartCol::Arrow), COL_MIN);
        assert!((col_w(ChartCol::Value) + col_w(ChartCol::Arrow) - (v0 + a0)).abs() < 1e-3);
        // 拖回原处 = 原宽度
        handle_ui(ChartUiMsg::Move(100.0));
        assert_eq!((col_w(ChartCol::Value), col_w(ChartCol::Arrow)), (v0, a0));
        // 结束拖动但不写盘：直接清掉拖动状态
        if let Ok(mut g) = ui_cell().lock() {
            g.drag = None;
        }
    }

    #[test]
    fn 衍生品卡片只在永续与期货显示() {
        let card = Card { markets: vec!["crypto_perp".into(), "futures".into()], ..Card::default() };
        assert!(card_visible(&card, Some("futures")));
        assert!(!card_visible(&card, Some("equity")));
        assert!(card_visible(&card, None));
    }
}
