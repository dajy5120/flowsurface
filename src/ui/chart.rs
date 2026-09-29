//! 图表的涨跌 / 买卖色（docs/35 批 5）。
//!
//! 上游图表原本拿 iced 调色板的 `success` / `danger`（状态色）画买卖与涨跌。那样
//! 切到「红涨绿跌」或色弱安全配色时图表不跟着变——而且不能反过来把状态色改成涨跌色，
//! 否则「红涨绿跌」下危险提示会变成绿色。所以图表改走交易领域包的涨跌色，
//! 三档浓淡与原先的 `base / strong / weak` 一一对应：
//!
//! | 原先 | 现在 |
//! |---|---|
//! | `success.base` / `danger.base` | [`up`] / [`down`] |
//! | `success.strong` / `danger.strong` | [`up_strong`] / [`down_strong`]（悬停档：深色主题提亮 8%） |
//! | `success.weak` / `danger.weak` | [`up_weak`] / [`down_weak`]（24% 叠在面板底色上的实色） |

use iced::Color;

use super::{color, core, domain, theme_id, tokens};

fn strong(c: tokens::Rgba) -> Color {
    let k = 0.08;
    let dark = theme_id().is_dark();
    let f = |x: f32| if dark { x + (1.0 - x) * k } else { x * (1.0 - k) };
    Color { r: f(c.r), g: f(c.g), b: f(c.b), a: c.a }
}

fn weak(c: tokens::Rgba) -> Color {
    color(c.with_alpha(0.24).over(core().surface_primary))
}

/// 涨 / 买 / 买盘
pub fn up() -> Color {
    color(domain().up)
}
pub fn up_strong() -> Color {
    strong(domain().up)
}
pub fn up_weak() -> Color {
    weak(domain().up)
}
/// 跌 / 卖 / 卖盘
pub fn down() -> Color {
    color(domain().down)
}
pub fn down_strong() -> Color {
    strong(domain().down)
}
pub fn down_weak() -> Color {
    weak(domain().down)
}
