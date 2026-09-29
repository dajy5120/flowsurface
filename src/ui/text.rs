//! 字号角色与字体（UPDS V1 §04，docs/35 §4.3–4.4）。
//!
//! **组件只说角色、不说字号**：`ui::text::label("…")` 而不是 `text("…").size(12)`。
//! 角色的字号随密度走（正文 / 标签 / 数字 / 代码），标题类不随密度变。
//!
//! 字体按领域包的候选列表取**第一个装了的**：界面字体必须带中文，等宽字体必须中英 2:1
//! 严格等宽（表格里中文与数字混排才不错位）。启动时用 `fc-list` 查一次系统字体。

use std::sync::LazyLock;

use iced::font::Weight;
use iced::widget::{Text, text};
use iced::{Font, Renderer, Theme};

use super::{Role, color, density, tokens};

/// 系统里装了哪些字体族（`fc-list : family`，启动时查一次）。
static FAMILIES: LazyLock<Vec<String>> = LazyLock::new(|| {
    std::process::Command::new("fc-list")
        .args([":", "family"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .flat_map(|l| l.split(',').map(|s| s.trim().to_string()))
                .collect()
        })
        .unwrap_or_default()
});

fn pick(candidates: &'static [&'static str]) -> Option<&'static str> {
    candidates.iter().copied().find(|c| FAMILIES.iter().any(|f| f == c))
}

static UI_FAMILY: LazyLock<Option<&'static str>> = LazyLock::new(|| pick(tokens::FONT_UI));
static MONO_FAMILY: LazyLock<Option<&'static str>> = LazyLock::new(|| pick(tokens::FONT_MONO));

/// 界面字体（缺省 Noto Sans CJK SC）。一个都没装时退回 iced 缺省字体。
pub fn ui_font() -> Font {
    match *UI_FAMILY {
        Some(name) => Font::with_name(name),
        None => Font::DEFAULT,
    }
}

/// 等宽字体（缺省 Noto Sans Mono CJK SC）：数字、代码、表头、日志。
pub fn mono_font() -> Font {
    match *MONO_FAMILY {
        Some(name) => Font::with_name(name),
        None => Font::MONOSPACE,
    }
}

/// 启动日志里写一行，排查「怎么又是方块字」时第一个看这里。
pub fn describe_fonts() -> String {
    format!(
        "界面 {} · 等宽 {}",
        UI_FAMILY.unwrap_or("（未找到候选，用缺省）"),
        MONO_FAMILY.unwrap_or("（未找到候选，用缺省）")
    )
}

/// 当前密度下某角色的字号。
pub fn size(role: Role) -> f32 {
    role.spec(density()).size
}

fn font_for(spec: tokens::TypeSpec) -> Font {
    let base = if spec.mono { mono_font() } else { ui_font() };
    let weight = match spec.weight {
        0..=450 => Weight::Normal,
        451..=550 => Weight::Medium,
        _ => Weight::Semibold,
    };
    Font { weight, ..base }
}

/// 按角色排一段文字。`Metadata` 角色自动转大写（只影响拉丁字母）。
pub fn role<'a>(r: Role, s: impl Into<String>) -> Text<'a, Theme, Renderer> {
    let spec = r.spec(density());
    let s: String = s.into();
    let s = if spec.uppercase { s.to_uppercase() } else { s };
    text(s)
        .size(spec.size)
        .line_height(iced::widget::text::LineHeight::Absolute(spec.line_height.into()))
        .font(font_for(spec))
}

pub fn title<'a>(s: impl Into<String>) -> Text<'a, Theme, Renderer> {
    role(Role::Title, s)
}

pub fn section<'a>(s: impl Into<String>) -> Text<'a, Theme, Renderer> {
    role(Role::Section, s)
}

pub fn body<'a>(s: impl Into<String>) -> Text<'a, Theme, Renderer> {
    role(Role::Body, s)
}

/// 控件、菜单、标签页、面板标题。
pub fn label<'a>(s: impl Into<String>) -> Text<'a, Theme, Renderer> {
    role(Role::Label, s)
}

/// 字段下方的一行提示，次要文字色。
pub fn caption<'a>(s: impl Into<String>) -> Text<'a, Theme, Renderer> {
    role(Role::Caption, s).color(color(super::core().text_secondary))
}

/// 表头、状态栏小字：等宽、大写、三级文字色。
pub fn metadata<'a>(s: impl Into<String>) -> Text<'a, Theme, Renderer> {
    role(Role::Metadata, s).color(color(super::core().text_tertiary))
}

/// 一切数据：等宽数字。**右对齐由容器负责**（列宽固定 + `align_x(End)`）。
pub fn numeric<'a>(s: impl Into<String>) -> Text<'a, Theme, Renderer> {
    role(Role::Numeric, s)
}

pub fn code<'a>(s: impl Into<String>) -> Text<'a, Theme, Renderer> {
    role(Role::Code, s)
}

/// 次要文字（说明、单位）。
pub fn secondary<'a>(s: impl Into<String>) -> Text<'a, Theme, Renderer> {
    role(Role::Body, s).color(color(super::core().text_secondary))
}

/// 三级文字（「—」之类的缺失符号、极次要的提示）。
pub fn tertiary<'a>(s: impl Into<String>) -> Text<'a, Theme, Renderer> {
    role(Role::Body, s).color(color(super::core().text_tertiary))
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::font::Family;

    #[test]
    fn 字体族名能匹配到() {
        // 不断言一定装了（CI 机器可能没有），只要求不 panic、有描述
        assert!(!describe_fonts().is_empty());
        let _ = ui_font();
        let _ = mono_font();
    }

    #[test]
    fn 元数据角色是等宽() {
        assert!(Role::Metadata.spec(tokens::Density::Compact).mono);
        assert!(!Role::Label.spec(tokens::Density::Compact).mono);
        assert!(matches!(mono_font().family, Family::Name(_) | Family::Monospace));
    }
}

// ── 旧字号的迁移目标（docs/35 批 5–7）─────────────────────────────────
//
// 各面板原本手写 `.size(10/11/12/13/14)`。迁移时按就近原则归到角色，字号从此随密度走：
//
// | 旧 | 新 | 紧凑 / 舒适 / 宽松 |
// |---|---|---|
// | 10 | [`s_meta`]（元数据字号，不转大写） | 10.5 |
// | 11 | [`s_small`]（说明） | 12 |
// | 12 | [`s_body`]（标签 / 正文） | 12 / 13 / 14 |
// | 13 | [`s_emph`]（正文 + 1） | 13 / 14 / 15 |
// | 14–16 | [`s_section`]（分组标题） | 15 |
// | ≥ 17 | [`s_title`]（标题） | 20 |

pub fn s_meta() -> f32 {
    size(Role::Metadata)
}
pub fn s_small() -> f32 {
    size(Role::Caption)
}
pub fn s_body() -> f32 {
    size(Role::Label)
}
pub fn s_emph() -> f32 {
    size(Role::Label) + 1.0
}
pub fn s_section() -> f32 {
    size(Role::Section)
}
pub fn s_title() -> f32 {
    size(Role::Title)
}
