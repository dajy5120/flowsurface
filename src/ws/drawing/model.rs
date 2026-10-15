//! 绘图对象模型（docs/46 §3）：锚点存（时间, 价格），不存像素。
//!
//! 对象 JSON 里认不得的字段原样留着（`extra`）：旧程序打开新格式写的对象，存回去时不丢东西
//! （docs/45 C8 的教训）。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// 对象格式版本（存在库的 `meta` 表里）。
pub const FORMAT: u32 = 1;

/// 一个锚点：时间（毫秒，UTC）+ 价格（定点 `Price::units`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    pub t: u64,
    pub p: i64,
}

/// 工具 / 对象种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// 水平线：一个锚点，只看价格，横贯整张图
    HLine,
    /// 垂直线：一个锚点，只看时间
    VLine,
    /// 趋势线：两个锚点之间的线段
    Trend,
    /// 射线：从第一个锚点出发、穿过第二个锚点、一直延到图边
    Ray,
    /// 矩形：两个对角
    Rect,
}

impl Kind {
    pub const ALL: [Self; 5] = [Self::HLine, Self::VLine, Self::Trend, Self::Ray, Self::Rect];

    /// 要放几个锚点。
    #[must_use]
    pub const fn anchors(self) -> usize {
        match self {
            Self::HLine | Self::VLine => 1,
            Self::Trend | Self::Ray | Self::Rect => 2,
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::HLine => "水平线",
            Self::VLine => "垂直线",
            Self::Trend => "趋势线",
            Self::Ray => "射线",
            Self::Rect => "矩形",
        }
    }
}

/// 线型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dash {
    #[default]
    Solid,
    Dashed,
    Dotted,
}

/// 样式。颜色是用户数据（RGBA 字节），画的时候经 `ui::chart::drawing_color` 换成界面颜色。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Style {
    pub color: [u8; 4],
    pub width: f32,
    pub dash: Dash,
    /// 矩形的填充（`None` = 用线色的 15%）
    pub fill: Option<[u8; 4]>,
}

impl Default for Style {
    fn default() -> Self {
        Self { color: [41, 98, 255, 255], width: 1.5, dash: Dash::Solid, fill: None }
    }
}

/// 对象从哪来（docs/46 §2 第 3 条）。只有手工的能拖、能删；其余只读，可以复制成手工对象（G4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    #[default]
    Manual,
    Backtest,
    Strategy,
    Calendar,
    Order,
}

/// 一个绘图对象。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Drawing {
    pub id: String,
    pub kind: Kind,
    pub anchors: Vec<Anchor>,
    #[serde(default)]
    pub style: Style,
    #[serde(default)]
    pub locked: bool,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub source: Source,
    /// 分组（对象树里按它归类，整组隐藏 / 锁定；G1）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// 在哪几类周期上**不**显示（[`TF_CATS`] 的键；空 = 处处显示；G1）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hide_on: Vec<String>,
    /// 认不得的字段（新版本加的）原样保留
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Drawing {
    /// 新的手工对象。锚点数不对时补齐 / 截断到该种类要的个数。
    #[must_use]
    pub fn new(kind: Kind, mut anchors: Vec<Anchor>) -> Self {
        let n = kind.anchors();
        if let Some(&last) = anchors.last() {
            anchors.resize(n, last);
        }
        Self {
            id: new_id(),
            kind,
            anchors,
            style: Style::default(),
            locked: false,
            hidden: false,
            source: Source::Manual,
            group: None,
            hide_on: Vec::new(),
            extra: Map::new(),
        }
    }

    /// 在这一类周期上显示吗（[`tf_cat`]）。
    #[must_use]
    pub fn shown_on(&self, cat: &str) -> bool {
        !self.hide_on.iter().any(|c| c == cat)
    }

    /// 能不能拖、能不能删（锁定的、系统生成的都不行）。
    #[must_use]
    pub fn editable(&self) -> bool {
        !self.locked && self.source == Source::Manual
    }
}

/// 周期类别（可见周期按类设，与 TradingView 的「可见性」同一个思路）：(键, 名称)。
pub const TF_CATS: [(&str, &str); 5] = [("tick", "逐笔"), ("sec", "秒级"), ("min", "分钟"), ("hour", "小时"), ("day", "日线")];

/// 一张图的周期 → 类别。按时间的图看周期毫秒，按逐笔的图是 `tick`。
#[must_use]
pub fn tf_cat(interval_ms: Option<u64>) -> &'static str {
    match interval_ms {
        None => "tick",
        Some(ms) if ms < 60_000 => "sec",
        Some(ms) if ms < 3_600_000 => "min",
        Some(ms) if ms < 86_400_000 => "hour",
        Some(_) => "day",
    }
}

/// 价格定点（`exchange::unit::Price::units`）的刻度：1 = 10⁻¹¹。
pub const PRICE_SCALE: i64 = 100_000_000_000;

/// 定点价格 → 文字（`decimals` 位小数，四舍五入）。
#[must_use]
pub fn price_text(units: i64, decimals: usize) -> String {
    let d = decimals.min(11);
    let step = 10_i64.pow(11 - d as u32);
    let r = (units as i128 + if units >= 0 { step as i128 / 2 } else { -(step as i128) / 2 }) / step as i128;
    let neg = r < 0;
    let r = r.unsigned_abs();
    let base = 10_u128.pow(d as u32);
    let (int, frac) = (r / base, r % base);
    let sign = if neg { "-" } else { "" };
    if d == 0 { format!("{sign}{int}") } else { format!("{sign}{int}.{frac:0d$}") }
}

/// 文字 → 定点价格（不经浮点，不丢精度）。认不得返回 `None`。
#[must_use]
pub fn parse_price(s: &str) -> Option<i64> {
    let s = s.trim();
    let (neg, s) = s.strip_prefix('-').map_or((false, s), |r| (true, r));
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    if int.is_empty() && frac.is_empty() || frac.len() > 11 || !int.chars().chain(frac.chars()).all(|c| c.is_ascii_digit()) {
        return None;
    }
    let int: i64 = if int.is_empty() { 0 } else { int.parse().ok()? };
    let frac_units: i64 = if frac.is_empty() { 0 } else { format!("{frac:0<11}").parse().ok()? };
    let v = int.checked_mul(PRICE_SCALE)?.checked_add(frac_units)?;
    Some(if neg { -v } else { v })
}

/// 毫秒 → `YYYY-MM-DD HH:MM:SS`（UTC）。
#[must_use]
pub fn time_text(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64).map_or_else(|| ms.to_string(), |t| t.format("%Y-%m-%d %H:%M:%S").to_string())
}

/// `YYYY-MM-DD HH:MM[:SS]`（UTC）→ 毫秒。
#[must_use]
pub fn parse_time(s: &str) -> Option<u64> {
    let s = s.trim();
    let t = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M"))
        .ok()?;
    u64::try_from(t.and_utc().timestamp_millis()).ok()
}

/// 对象 id：毫秒时间戳 + 进程内序号（同一毫秒里连建几个也不撞）。
fn new_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    format!("d{ms:x}-{}", SEQ.fetch_add(1, Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_fields_survive_and_defaults_fill_in() {
        let j = r#"{"id":"x","kind":"trend","anchors":[{"t":1,"p":2},{"t":3,"p":4}],"visible_tfs":[60],"style":{"width":3.0}}"#;
        let d: Drawing = serde_json::from_str(j).unwrap();
        assert_eq!((d.kind, d.style.width, d.style.color, d.source), (Kind::Trend, 3.0, Style::default().color, Source::Manual));
        let back = serde_json::to_value(&d).unwrap();
        assert_eq!(back["visible_tfs"], serde_json::json!([60]), "新版本的字段存回去不能丢");
    }

    #[test]
    fn price_and_time_text_round_trip_without_floats() {
        assert_eq!(exchange::unit::Price::from_units(PRICE_SCALE).to_f32_lossy(), 1.0, "刻度与 Price 一致");
        assert_eq!(parse_price("0.3640"), Some(36_400_000_000));
        assert_eq!(parse_price("63445.1"), Some(6_344_510_000_000_000));
        assert_eq!(parse_price("-2.5"), Some(-250_000_000_000));
        assert_eq!((parse_price("abc"), parse_price("1.2.3"), parse_price("")), (None, None, None));
        assert_eq!(price_text(36_400_000_000, 4), "0.3640");
        assert_eq!(price_text(36_405_000_000, 3), "0.364", "四舍五入到 3 位：0.36405 → 0.364（第 4 位是 0）");
        assert_eq!(price_text(36_450_000_000, 3), "0.365");
        assert_eq!(price_text(6_344_510_000_000_000, 1), "63445.1");
        assert_eq!(price_text(-250_000_000_000, 0), "-3");
        assert_eq!(parse_time("2026-10-15 04:30"), Some(1_792_038_600_000));
        assert_eq!(time_text(1_792_038_600_000), "2026-10-15 04:30:00");
        assert_eq!(parse_time("10/15"), None);
    }

    #[test]
    fn timeframe_categories_and_visibility() {
        assert_eq!((tf_cat(None), tf_cat(Some(100)), tf_cat(Some(60_000)), tf_cat(Some(14_400_000)), tf_cat(Some(86_400_000))), ("tick", "sec", "min", "hour", "day"));
        let mut d = Drawing::new(Kind::HLine, vec![Anchor { t: 1, p: 1 }]);
        assert!(d.shown_on("min"));
        d.hide_on.push("min".into());
        assert!(!d.shown_on("min") && d.shown_on("hour"));
        let j = serde_json::to_value(&Drawing::new(Kind::HLine, vec![Anchor { t: 1, p: 1 }])).unwrap();
        assert!(j.get("group").is_none() && j.get("hide_on").is_none(), "没设的不写进库，G0 的对象原样");
    }

    #[test]
    fn new_drawing_has_the_right_number_of_anchors() {
        let a = Anchor { t: 1, p: 2 };
        assert_eq!(Drawing::new(Kind::Rect, vec![a]).anchors, vec![a, a]);
        assert_eq!(Drawing::new(Kind::HLine, vec![a, Anchor { t: 5, p: 6 }]).anchors, vec![a]);
        assert_ne!(Drawing::new(Kind::HLine, vec![a]).id, Drawing::new(Kind::HLine, vec![a]).id);
    }
}
