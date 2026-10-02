//! 基础组件（UPDS V3 §13–17，docs/35 §6 / 批 4）。**面板里只用这些**，不再各写各的按钮与徽标。
//!
//! 交互状态统一：悬停 = 叠一层洗色、不位移不加阴影；按下即时；禁用 0.38 不透明度；
//! 选中 = 强调色 24% + 前缘标识；焦点环 2px 强调色。控件高度 = 行高，圆角 2。

use iced::widget::{Space, button, column, container, row, text};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding, Theme};

use super::fmt::{Absence, Provenance};
use super::metrics::{self, radius};
use super::{color, core, text as t};

// ── 语气 ─────────────────────────────────────────────────────────────

/// 徽标 / 提示的语气。**颜色从不单独承载意义**：每种语气配一个符号。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Accent,
    Success,
    Warning,
    Danger,
    Info,
}

impl Tone {
    pub fn color(self) -> Color {
        let c = core();
        color(match self {
            Self::Neutral => c.text_secondary,
            Self::Accent => c.accent_primary,
            Self::Success => c.status_success,
            Self::Warning => c.status_warning,
            Self::Danger => c.status_danger,
            Self::Info => c.status_info,
        })
    }

    /// 配套符号（灰度、色弱下也分得清）。
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Neutral => "",
            Self::Accent => "●",
            Self::Success => "✓",
            Self::Warning => "▲",
            Self::Danger => "■",
            Self::Info => "ⓘ",
        }
    }
}

/// 徽标：描边 + 12% 底色 + 可选符号。
pub fn badge<'a, M: 'a>(label: impl Into<String>, tone: Tone) -> Element<'a, M> {
    let fg = tone.color();
    let g = tone.glyph();
    let label: String = label.into();
    let s = if g.is_empty() { label } else { format!("{g} {label}") };
    container(t::label(s).color(fg))
        .padding([1, 6])
        .style(move |_| container::Style {
            border: Border { width: 1.0, color: Color { a: 0.7, ..fg }, radius: radius::SM.into() },
            background: Some(Background::Color(Color { a: 0.12, ..fg })),
            ..Default::default()
        })
        .into()
}

// ── 按钮（UPDS V3 §13）───────────────────────────────────────────────

/// 五种按钮。**一个作用域里只有一个 Primary**；Destructive 永远不用强调色填充。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Primary,
    Standard,
    Subtle,
    Ghost,
    Destructive,
}

pub fn button_style(kind: Kind, _theme: &Theme, status: button::Status) -> button::Style {
    let c = core();
    let hover = matches!(status, button::Status::Hovered);
    let pressed = matches!(status, button::Status::Pressed);
    let disabled = matches!(status, button::Status::Disabled);
    let wash = |base: Color| -> Color {
        if pressed {
            Color { r: base.r * 0.94, g: base.g * 0.94, b: base.b * 0.94, ..base }
        } else if hover {
            let h = color(c.state_hover);
            Color {
                r: base.r + (h.r - base.r) * h.a,
                g: base.g + (h.g - base.g) * h.a,
                b: base.b + (h.b - base.b) * h.a,
                a: base.a.max(h.a),
            }
        } else {
            base
        }
    };
    let (bg, fg, border) = match kind {
        Kind::Primary => (Some(wash(color(c.accent_primary))), Color::WHITE, Color::TRANSPARENT),
        Kind::Standard => {
            (Some(wash(color(c.surface_secondary))), color(c.text_primary), color(c.border_default))
        }
        Kind::Subtle => (hover.then(|| wash(color(c.surface_secondary))), color(c.text_primary), Color::TRANSPARENT),
        Kind::Ghost => (None, if hover { color(c.text_primary) } else { color(c.text_secondary) }, Color::TRANSPARENT),
        Kind::Destructive => (
            hover.then(|| Color { a: 0.12, ..color(c.status_danger) }),
            color(c.status_danger),
            color(c.status_danger),
        ),
    };
    let dim = |x: Color| if disabled { Color { a: x.a * tokens_disabled(), ..x } } else { x };
    button::Style {
        background: bg.map(|b| Background::Color(dim(b))),
        text_color: dim(fg),
        border: Border { width: if border.a > 0.0 { 1.0 } else { 0.0 }, color: dim(border), radius: radius::SM.into() },
        ..Default::default()
    }
}

fn tokens_disabled() -> f32 {
    super::tokens::OPACITY_DISABLED
}

/// 一个按钮：高度 = 控件高度，文字用标签角色。`msg = None` 即禁用。
pub fn btn<'a, M: Clone + 'a>(label: impl Into<String>, kind: Kind, msg: Option<M>) -> Element<'a, M> {
    let h = metrics::control_height();
    let mut b = button(container(t::label(label)).height(Length::Fill).align_y(Alignment::Center))
        .height(Length::Fixed(h))
        .padding(Padding::from([0.0, metrics::space(4)]))
        .style(move |th, st| button_style(kind, th, st));
    if let Some(m) = msg {
        b = b.on_press(m);
    }
    b.into()
}

// ── 标签页 / 分段（UPDS V3 §13、V6 §43）──────────────────────────────

/// 标签页：激活页下缘 2px 强调色，其余次要文字色。
pub fn tabs<'a, M: Clone + 'a, V: PartialEq + Clone + 'a>(
    items: &[(&'a str, V)],
    active: &V,
    on: impl Fn(V) -> M + 'a,
) -> Element<'a, M> {
    let c = core();
    let mut r = row![].spacing(0);
    for (label, v) in items {
        let is = v == active;
        let fg = if is { color(c.text_primary) } else { color(c.text_secondary) };
        let under = container(Space::new().width(Length::Fill).height(Length::Fixed(2.0))).style(move |_| container::Style {
            background: is.then(|| Background::Color(color(core().accent_primary))),
            ..Default::default()
        });
        let cell = column![
            container(t::label(*label).color(fg)).padding(Padding::from([metrics::space(2), metrics::space(4)])),
            under,
        ]
        .width(Length::Shrink);
        r = r.push(
            button(cell)
                .padding(0)
                .on_press(on(v.clone()))
                .style(|th, st| button_style(Kind::Ghost, th, st)),
        );
    }
    container(r)
        .style(move |_| container::Style {
            border: Border { width: 0.0, ..Default::default() },
            ..Default::default()
        })
        .into()
}

/// 分段按钮：一组互斥选项拼成一条，选中段强调色 24% 底。
pub fn segmented<'a, M: Clone + 'a, V: PartialEq + Clone + 'a>(
    items: &[(&'a str, V)],
    active: &V,
    on: impl Fn(V) -> M + 'a,
) -> Element<'a, M> {
    let mut r = row![].spacing(0);
    for (label, v) in items {
        let is = v == active;
        r = r.push(
            button(t::label(*label))
                .padding(Padding::from([metrics::space(1), metrics::space(4)]))
                .on_press(on(v.clone()))
                .style(move |th, st| {
                    let mut s = button_style(Kind::Subtle, th, st);
                    if is {
                        s.background = Some(Background::Color(color(core().accent_soft)));
                        s.text_color = color(core().text_primary);
                    }
                    s
                }),
        );
    }
    container(r)
        .style(|_| container::Style { border: metrics::hairline(radius::SM), ..Default::default() })
        .into()
}

// ── 面板标题栏（UPDS V2 §11）─────────────────────────────────────────

/// 面板标题栏：标题 · 状态 · 右侧最多 3 个动作。高度随密度。
pub fn panel_header<'a, M: 'a>(
    title: impl Into<String>,
    status: Option<Element<'a, M>>,
    actions: Vec<Element<'a, M>>,
) -> Element<'a, M> {
    let mut r = row![t::label(title)].spacing(metrics::space(3)).align_y(Alignment::Center);
    if let Some(s) = status {
        r = r.push(s);
    }
    r = r.push(Space::new().width(Length::Fill));
    for a in actions.into_iter().take(3) {
        r = r.push(a);
    }
    container(r)
        .height(Length::Fixed(metrics::panel_header()))
        .padding(Padding::from([0.0, metrics::pad()]))
        .align_y(Alignment::Center)
        .width(Length::Fill)
        .style(|_| {
            let c = core();
            container::Style {
                background: Some(Background::Color(color(c.surface_secondary))),
                border: Border { width: 0.0, ..Default::default() },
                ..Default::default()
            }
        })
        .into()
}

/// 分组标题：元数据角色 + 下方细线。
pub fn section<'a, M: 'a>(title: impl Into<String>) -> Element<'a, M> {
    column![
        t::metadata(title),
        container(Space::new().width(Length::Fill).height(Length::Fixed(1.0)))
            .style(|_| container::Style { background: Some(Background::Color(color(core().border_subtle))), ..Default::default() }),
    ]
    .spacing(metrics::space(1))
    .into()
}

/// 检查器里的一行「标签 · 值」：标签列定宽，所有行对齐同一条竖线（UPDS V6 §53 对齐律）。
pub fn kv<'a, M: 'a>(label: impl Into<String>, value: Element<'a, M>, label_w: f32) -> Element<'a, M> {
    row![container(t::caption(label)).width(Length::Fixed(label_w)), value]
        .spacing(metrics::space(3))
        .align_y(Alignment::Center)
        .into()
}

// ── 数值单元格（UPDS V8）──────────────────────────────────────────────

/// 带来源标记的数值文字：派生 `· `、覆盖 `^ `、模拟 `~ ` 前缀；过期变暗；估算加 `≈`
/// （iced 0.14 的文字没有下划线，用 `≈` 代替 UPDS 的点状下划线——排版标记，灰度下可见）。
pub fn value<'a>(s: impl Into<String>, prov: Provenance) -> iced::widget::Text<'a, Theme, iced::Renderer> {
    let c = core();
    let s: String = s.into();
    let body = match prov {
        Provenance::Estimated => format!("≈ {s}"),
        p => format!("{}{s}", p.prefix()),
    };
    let fg = match prov {
        Provenance::Stale => color(c.text_tertiary),
        Provenance::Derived => color(c.text_secondary),
        _ => color(c.text_primary),
    };
    t::numeric(body).color(fg)
}

/// 缺失类单元格：符号 + 三级文字色。
pub fn absent<'a>(a: Absence) -> iced::widget::Text<'a, Theme, iced::Renderer> {
    let c = core();
    let fg = if a == Absence::Invalid { color(c.status_danger) } else { color(c.text_tertiary) };
    t::numeric(a.glyph()).color(fg)
}

// ── 状态视图（UPDS V3 §16、V5 §41）───────────────────────────────────

/// 空态：说这里是干什么的、下一步做什么；**不写「无数据」**。
pub fn empty<'a, M: 'a>(title: impl Into<String>, hint: impl Into<String>) -> Element<'a, M> {
    container(column![t::label(title), t::caption(hint)].spacing(metrics::space(1)).max_width(520.0))
        .padding(metrics::space(5))
        .into()
}

/// 加载中：一行说明正在做什么（超过 400ms 才该出现，UPDS V5 §29）。
pub fn loading<'a, M: 'a>(what: impl Into<String>) -> Element<'a, M> {
    container(row![text("◌").color(color(core().text_tertiary)), t::caption(what)].spacing(metrics::space(2)))
        .padding(metrics::space(5))
        .into()
}

/// 骨架屏：保留最终行高，加载完不跳（UPDS V3 §16）。
pub fn skeleton<'a, M: 'a>(rows: usize) -> Element<'a, M> {
    let h = metrics::row_height();
    let mut col = column![].spacing(2);
    for i in 0..rows {
        let w = [0.92, 0.78, 0.85, 0.64][i % 4];
        col = col.push(
            container(Space::new().width(Length::FillPortion((w * 100.0) as u16)).height(Length::Fixed(h - 8.0)))
                .padding(Padding::from([4.0, 0.0]))
                .style(|_| container::Style { background: Some(Background::Color(color(core().surface_secondary))), ..Default::default() }),
        );
    }
    col.into()
}

/// 错误（UPDS V5 §33 错误结构）：发生了什么 · 哪个对象 · 为什么 · 怎么办 · 可复制的代码。
pub fn error<'a, M: 'a>(
    what: impl Into<String>,
    why: impl Into<String>,
    fix: impl Into<String>,
    code: Option<&'a str>,
) -> Element<'a, M> {
    let c = core();
    let mut col = column![
        row![text(Tone::Danger.glyph()).color(Tone::Danger.color()), t::label(what).color(color(c.status_danger))].spacing(6),
        t::body(why),
        t::caption(fix),
    ]
    .spacing(metrics::space(1));
    if let Some(code) = code {
        col = col.push(t::metadata(code));
    }
    container(col)
        .padding(metrics::space(4))
        .style(move |_| container::Style {
            border: Border { width: 1.0, color: Color { a: 0.6, ..Tone::Danger.color() }, radius: radius::SM.into() },
            background: Some(Background::Color(Color { a: 0.08, ..Tone::Danger.color() })),
            ..Default::default()
        })
        .into()
}

/// 过期提示条：数据还显示着，但说清是多久以前的（UPDS V5 §30：旧数字不能装成新的）。
pub fn stale_bar<'a, M: 'a>(age: impl Into<String>, why: impl Into<String>) -> Element<'a, M> {
    let fg = Tone::Warning.color();
    container(row![text(Tone::Warning.glyph()).color(fg), t::caption(format!("过期 {} · {}", age.into(), why.into()))].spacing(6))
        .padding(Padding::from([2.0, metrics::space(3)]))
        .width(Length::Fill)
        .style(move |_| container::Style { background: Some(Background::Color(Color { a: 0.10, ..fg })), ..Default::default() })
        .into()
}

/// 无权限 / 不可用：说清原因，而不是一个点了没反应的按钮（UPDS V5 §34）。
pub fn unavailable<'a, M: 'a>(what: impl Into<String>, reason: impl Into<String>) -> Element<'a, M> {
    empty(format!("{} 不可用", what.into()), reason)
}


/// 历史数据的常用时段（相对于选中的那一天，UTC）：(名字, 起始 HH:MM, 分钟数)。
pub const TIME_PRESETS: [(&str, &str, u32); 5] = [
    ("整日", "00:00", 1440),
    ("亚洲时段", "00:00", 480),
    ("欧洲时段", "07:00", 510),
    ("美股常规时段", "13:30", 390),
    ("当日最后 1 小时", "23:00", 60),
];

/// 把「日期 + 起始（UTC）+ 时长」换算成人读的绝对范围：
/// `2026-09-01 13:30 → 20:00 UTC · 北京时间 21:30 → 次日 04:00`（UPDS V6 §48：相对范围要同时给出解析后的绝对范围与时区）。
pub fn resolve_window(date: &str, start: &str, minutes: u32) -> Option<String> {
    let d = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    let t = chrono::NaiveTime::parse_from_str(start.trim(), "%H:%M").ok()?;
    let from = d.and_time(t);
    let to = from + chrono::Duration::minutes(i64::from(minutes));
    let bj = chrono::Duration::hours(8);
    let span = |a: chrono::NaiveDateTime, b: chrono::NaiveDateTime| {
        let next = if b.date() > a.date() { "次日 " } else { "" };
        format!("{} → {next}{}", a.format("%H:%M"), b.format("%H:%M"))
    };
    Some(format!("{} {} UTC · 北京时间 {}", d.format("%Y-%m-%d"), span(from, to), span(from + bj, to + bj)))
}

/// 时间范围选择（docs/35 §6.1，UPDS V6 §48）：常用时段一键选 + 解析后的绝对范围。
/// `on(start, minutes)` 把选中的时段交回宿主。
pub fn time_range<'a, M: Clone + 'a>(date: Option<&str>, start: &str, minutes: u32, on: impl Fn(String, u32) -> M) -> Element<'a, M> {
    let mut presets = row![].spacing(metrics::space(1)).align_y(Alignment::Center);
    for (name, st, m) in TIME_PRESETS {
        let active = st == start && m == minutes;
        presets = presets.push(
            button(t::caption(name.to_string()))
                .padding(Padding::from([1.0, metrics::space(2)]))
                .on_press(on(st.to_string(), m))
                .style(move |th, s| button_style(if active { Kind::Standard } else { Kind::Ghost }, th, s)),
        );
    }
    let resolved = date
        .and_then(|d| resolve_window(d, start, minutes))
        .unwrap_or_else(|| "先选日期；起始填 HH:MM（UTC）".to_string());
    column![presets, t::caption(resolved)].spacing(metrics::space(1)).into()
}

#[cfg(test)]
mod time_range_tests {
    use super::*;

    #[test]
    fn 时段换算_跨日标次日() {
        assert_eq!(
            resolve_window("2026-09-01", "13:30", 390).as_deref(),
            Some("2026-09-01 13:30 → 20:00 UTC · 北京时间 21:30 → 次日 04:00")
        );
        assert_eq!(
            resolve_window("2026-09-01", "00:00", 1440).as_deref(),
            Some("2026-09-01 00:00 → 次日 00:00 UTC · 北京时间 08:00 → 次日 08:00")
        );
        assert_eq!(resolve_window("2026-09-01", "25:00", 60), None, "非法起始不硬猜");
    }
}
