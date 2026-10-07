use serde::{Deserialize, Serialize};

use super::{WindowSpec, pane::Pane};
use crate::util::ok_or_default;

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Dashboard {
    #[serde(deserialize_with = "ok_or_default", default)]
    pub pane: Pane,
    #[serde(deserialize_with = "ok_or_default", default)]
    pub popout: Vec<(Pane, WindowSpec)>,
    /// 浮动层（docs/41 §4.2）：叠在平铺层上的图表浮窗，顺序即层次（后面的在上）
    #[serde(deserialize_with = "ok_or_default", default)]
    pub floating: Vec<(Pane, FloatRect)>,
    /// 浮动层收起（一键隐藏全部浮窗，回到干净的平铺视图）
    #[serde(default)]
    pub floats_hidden: bool,
}

/// 浮窗位置与大小，按页面内容区的**比例**存（0–1）：窗口大小变了，浮窗跟着等比缩放。
#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize)]
pub struct FloatRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Default for FloatRect {
    fn default() -> Self {
        Self { x: 0.1, y: 0.1, w: 0.45, h: 0.45 }
    }
}

impl FloatRect {
    /// 夹到内容区里，且不小于 `min_w` × `min_h`（比例）。
    #[must_use]
    pub fn clamped(self, min_w: f32, min_h: f32) -> Self {
        let w = self.w.clamp(min_w.min(1.0), 1.0);
        let h = self.h.clamp(min_h.min(1.0), 1.0);
        Self { x: self.x.clamp(0.0, 1.0 - w), y: self.y.clamp(0.0, 1.0 - h), w, h }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_dashboard_without_floating_still_loads() {
        let d: Dashboard = serde_json::from_str(r#"{"pane":{"Starter":{"link_group":null}},"popout":[]}"#).unwrap();
        assert!(d.floating.is_empty() && !d.floats_hidden);
    }

    #[test]
    fn clamp_keeps_inside() {
        let r = FloatRect { x: 0.9, y: -0.2, w: 0.5, h: 0.05 }.clamped(0.1, 0.1);
        assert!((r.x - 0.5).abs() < 1e-6 && r.y == 0.0 && (r.h - 0.1).abs() < 1e-6);
    }
}
