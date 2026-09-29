//! 度量：间距、圆角、边框、层级阴影、密度尺寸（UPDS V1 §05–06、V6 §53）。

use iced::{Border, Color, Shadow, Vector};

use super::{color, core, density, tokens};

/// 间距阶梯 space.0..space.8 = 2 4 6 8 12 16 24 32 48。**没有任意值**。
pub fn space(i: usize) -> f32 {
    tokens::SPACE[i.min(tokens::SPACE.len() - 1)]
}

/// 圆角：网格 / 分割线 / 面板 0；输入框 / 按钮 / 标签 2；菜单 / 弹出 / 吐司 4；对话框 6。
pub mod radius {
    use super::tokens;
    pub const NONE: f32 = tokens::RADIUS_NONE;
    pub const SM: f32 = tokens::RADIUS_SM;
    pub const MD: f32 = tokens::RADIUS_MD;
    pub const LG: f32 = tokens::RADIUS_LG;
}

/// 当前密度的尺寸。
pub fn dens() -> &'static tokens::DensitySpec {
    density().spec()
}

/// 行高（表格、列表）。
pub fn row_height() -> f32 {
    dens().row
}

/// 控件高度 = 行高。
pub fn control_height() -> f32 {
    dens().control
}

/// 面板标题栏高度。
pub fn panel_header() -> f32 {
    dens().panel_header
}

/// 面板内容内边距。
pub fn pad() -> f32 {
    dens().padding
}

/// 细分隔线（结构、分组）：1px `border.default`。**分隔靠边框不靠阴影**（UPDS V1 §05）。
pub fn hairline(radius: f32) -> Border {
    Border { width: tokens::BORDER_HAIRLINE, color: color(core().border_default), radius: radius.into() }
}

/// 弱分隔线：1px `border.subtle`。
pub fn hairline_subtle(radius: f32) -> Border {
    Border { width: tokens::BORDER_HAIRLINE, color: color(core().border_subtle), radius: radius.into() }
}

/// 聚焦面板的边：1px `border.strong`（前缘 2px 强调色条在批 3 的面板标题栏里画）。
pub fn strong(radius: f32) -> Border {
    Border { width: tokens::BORDER_STRONG, color: color(core().border_strong), radius: radius.into() }
}

/// 焦点环：2px 强调色。
pub fn focus_ring(radius: f32) -> Border {
    Border { width: tokens::BORDER_FOCUS, color: color(core().accent_primary), radius: radius.into() }
}

/// 五级层级（UPDS V1 §05）：e0 平面（面板、网格）· e1 提示 · e2 菜单 / 弹出 · e3 对话框 / 命令面板 · e4 浮动窗口。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Elevation {
    E0,
    E1,
    E2,
    E3,
    E4,
}

pub fn shadow(e: Elevation) -> Shadow {
    let (y, blur, a) = match e {
        Elevation::E0 => return Shadow::default(),
        Elevation::E1 => (1.0, 2.0, 0.10),
        Elevation::E2 => (2.0, 6.0, 0.12),
        Elevation::E3 => (6.0, 18.0, 0.16),
        Elevation::E4 => (14.0, 40.0, 0.20),
    };
    // 深色主题上黑影几乎看不见，加深一档；边框仍是主要的分隔手段
    let a = if super::theme_id().is_dark() { a * 2.5 } else { a };
    Shadow { color: Color { a, ..Color::BLACK }, offset: Vector::new(0.0, y), blur_radius: blur }
}
