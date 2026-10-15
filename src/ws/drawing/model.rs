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
            extra: Map::new(),
        }
    }

    /// 能不能拖、能不能删（锁定的、系统生成的都不行）。
    #[must_use]
    pub fn editable(&self) -> bool {
        !self.locked && self.source == Source::Manual
    }
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
    fn new_drawing_has_the_right_number_of_anchors() {
        let a = Anchor { t: 1, p: 2 };
        assert_eq!(Drawing::new(Kind::Rect, vec![a]).anchors, vec![a, a]);
        assert_eq!(Drawing::new(Kind::HLine, vec![a, Anchor { t: 5, p: 6 }]).anchors, vec![a]);
        assert_ne!(Drawing::new(Kind::HLine, vec![a]).id, Drawing::new(Kind::HLine, vec![a]).id);
    }
}
