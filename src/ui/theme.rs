//! UPDS 语义色 → iced `Extended` 调色板。
//!
//! FlowSurface 上游的全部样式函数（`style.rs`）与图表都读 `theme.extended_palette()`，
//! 槽位的语义是固定的——把 UPDS 的语义色逐一放进对应槽位，上游代码不改一行就跟着 token 走：
//!
//! | iced 槽位 | 上游拿它做什么 | UPDS |
//! |---|---|---|
//! | `background.base` | 窗口底、面板之间的缝 | `surface.base` |
//! | `background.weakest` | **面板底色**、输入框底 | `surface.primary` |
//! | `background.weaker` | 凹陷区 | `surface.well` |
//! | `background.weak` | 悬停、控件底、细边框 | `surface.secondary` |
//! | `background.neutral` | — | `border.subtle` |
//! | `background.strong` | 边框、更强的悬停 | `border.default` |
//! | `background.stronger` | 禁用文字 | `border.strong` |
//! | `background.strongest` | 占位文字、选区、按下 | `text.tertiary` |
//! | `primary` | 选中、焦点、分割线拖动 | `accent.primary` / `accent.soft` |
//! | `secondary` | 次要文字、链接 | `text.secondary` / `text.tertiary` / `text.primary` |
//! | `success` / `warning` / `danger` | 状态 | `status.*` |

use iced::theme::palette::{
    Background, Danger, Extended, Pair, Palette, Primary, Secondary, Success, Warning,
};
use iced::{Color, Theme};

use super::{color, tokens};

/// 构造 iced 主题。
pub fn iced_theme(id: tokens::ThemeId) -> Theme {
    let c = id.core();
    let palette = Palette {
        background: color(c.surface_base),
        text: color(c.text_primary),
        primary: color(c.accent_primary),
        success: color(c.status_success),
        warning: color(c.status_warning),
        danger: color(c.status_danger),
    };
    let dark = id.is_dark();
    Theme::custom_with_fn(format!("WealthSpring {}", id.label()), palette, move |_| extended(id, dark))
}

fn extended(id: tokens::ThemeId, is_dark: bool) -> Extended {
    let c = id.core();
    let text = color(c.text_primary);
    let on_surface = |bg: tokens::Rgba| Pair { color: solid(bg, c.surface_primary), text };
    // 状态色上的文字：深色主题里状态色是亮的，配深色字；浅色主题反之
    let on_status = if is_dark { color(c.surface_base) } else { color(c.surface_elevated) };
    let status = |s: tokens::Rgba| {
        let base = color(s);
        (
            Pair { color: base, text: on_status },
            Pair { color: solid(s.with_alpha(0.24), c.surface_primary), text },
            Pair { color: lighten(base, is_dark), text: on_status },
        )
    };

    let (sb, sw, ss) = status(c.status_success);
    let (wb, ww, ws) = status(c.status_warning);
    let (db, dw, ds) = status(c.status_danger);
    let accent = color(c.accent_primary);

    Extended {
        background: Background {
            base: on_surface(c.surface_base),
            weakest: on_surface(c.surface_primary),
            weaker: on_surface(c.surface_well),
            weak: on_surface(c.surface_secondary),
            neutral: on_surface(c.border_subtle),
            strong: on_surface(c.border_default),
            stronger: on_surface(c.border_strong),
            strongest: Pair { color: color(c.text_tertiary), text },
        },
        primary: Primary {
            base: Pair { color: accent, text: Color::WHITE },
            weak: Pair { color: solid(c.accent_soft, c.surface_primary), text },
            strong: Pair { color: lighten(accent, is_dark), text: Color::WHITE },
        },
        secondary: Secondary {
            base: Pair { color: color(c.text_secondary), text: color(c.surface_base) },
            weak: Pair { color: color(c.text_tertiary), text: color(c.surface_base) },
            strong: Pair { color: text, text: color(c.surface_base) },
        },
        success: Success { base: sb, weak: sw, strong: ss },
        warning: Warning { base: wb, weak: ww, strong: ws },
        danger: Danger { base: db, weak: dw, strong: ds },
        is_dark,
    }
}

/// 带透明度的 token 叠到面板底色上，得到不透明色——iced 的调色板槽位期望实色。
fn solid(c: tokens::Rgba, bg: tokens::Rgba) -> Color {
    if c.a >= 1.0 { color(c) } else { color(c.over(bg)) }
}

/// 悬停态：深色主题提亮、浅色主题压暗 8%（UPDS：悬停 = 叠一层 hover 洗色，不位移）。
fn lighten(c: Color, is_dark: bool) -> Color {
    let k = 0.08;
    let f = |x: f32| if is_dark { x + (1.0 - x) * k } else { x * (1.0 - k) };
    Color { r: f(c.r), g: f(c.g), b: f(c.b), a: c.a }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 四个主题都能构造_且面板底色取_surface_primary() {
        for id in tokens::ThemeId::ALL {
            let t = iced_theme(id);
            let e = t.extended_palette();
            assert_eq!(e.background.weakest.color, color(id.core().surface_primary), "{id:?}");
            assert_eq!(e.is_dark, id.is_dark());
        }
    }
}
