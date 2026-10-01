//! WealthSpring 设计 token（docs/35）：UPDS 1.0 核心 + 交易领域包，**两个界面程序的唯一来源**。
//!
//! - Cockpit（iced）经 `flowsurface::ui` 适配层使用；
//! - Studio（GPUI）经 `wealthspring-studio::ui` 适配层使用（生成 gpui-component 主题 JSON）。
//!
//! 本 crate 不依赖任何 GUI 框架：它只给「颜色、尺寸、字号、语义」这些数，怎么画是适配层的事。
//! 所有值在编译期从 `upds/upds-tokens.json` 与 `pack/wealthspring-pack.json` 生成并校验
//! （对比度、主题完备、色弱可区分），见 `build.rs`。

pub mod bridge;
pub mod color;
pub mod prefs;

pub use color::Rgba;
pub use prefs::Prefs;

/// 一档密度的全部尺寸（UPDS V1 §06）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DensitySpec {
    /// 表格 / 列表行高
    pub row: f32,
    /// 控件高度（= 行高）
    pub control: f32,
    pub toolbar: f32,
    /// 面板标题栏高度
    pub panel_header: f32,
    /// 内容内边距
    pub padding: f32,
    /// 正文 / 标签字号
    pub font_size: f32,
    pub icon: f32,
}

/// 一个字号角色（UPDS V1 §04）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TypeSpec {
    pub size: f32,
    pub line_height: f32,
    pub weight: u64,
    /// em 为单位的字距
    pub tracking: f32,
    /// 用等宽字体（数字、代码、表头元数据）
    pub mono: bool,
    pub uppercase: bool,
}

include!(concat!(env!("OUT_DIR"), "/tokens.rs"));

// ── 主题 ─────────────────────────────────────────────────────────────

/// 四个主题（UPDS V1 §03）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum ThemeId {
    /// 长时间会话与密集遥测的缺省
    #[default]
    Dark,
    Light,
    /// 纯黑底，边框承担结构；暗室
    OledDark,
    /// 正文 ≥ 7:1，非文字 ≥ 4.5:1
    HighContrast,
}

impl ThemeId {
    pub const ALL: [ThemeId; 4] = [Self::Dark, Self::Light, Self::OledDark, Self::HighContrast];

    pub fn key(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
            Self::OledDark => "oledDark",
            Self::HighContrast => "highContrast",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Dark => "深色",
            Self::Light => "浅色",
            Self::OledDark => "OLED 深色",
            Self::HighContrast => "高对比",
        }
    }

    pub fn from_key(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.key() == s)
    }

    pub fn is_dark(self) -> bool {
        !matches!(self, Self::Light)
    }

    pub fn core(self) -> &'static Core {
        match self {
            Self::Dark => &CORE_DARK,
            Self::Light => &CORE_LIGHT,
            Self::OledDark => &CORE_OLED_DARK,
            Self::HighContrast => &CORE_HIGH_CONTRAST,
        }
    }

    pub fn pack(self) -> &'static Pack {
        match self {
            Self::Dark => &PACK_DARK,
            Self::Light => &PACK_LIGHT,
            Self::OledDark => &PACK_OLED_DARK,
            Self::HighContrast => &PACK_HIGH_CONTRAST,
        }
    }

    /// 下一个主题（Ctrl Alt T 循环）。
    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|t| *t == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
}

// ── 密度 ─────────────────────────────────────────────────────────────

/// 三档密度。**一个偏好、处处生效**；字号几乎不动，空间在动（UPDS V1 §06）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Density {
    /// 终端 / IDE / 分析工具的缺省
    #[default]
    Compact,
    Comfortable,
    Spacious,
}

impl Density {
    pub const ALL: [Density; 3] = [Self::Compact, Self::Comfortable, Self::Spacious];

    pub fn key(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Comfortable => "comfortable",
            Self::Spacious => "spacious",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Compact => "紧凑",
            Self::Comfortable => "舒适",
            Self::Spacious => "宽松",
        }
    }

    pub fn from_key(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.key() == s)
    }

    pub fn spec(self) -> &'static DensitySpec {
        match self {
            Self::Compact => &DENSITY_COMPACT,
            Self::Comfortable => &DENSITY_COMFORTABLE,
            Self::Spacious => &DENSITY_SPACIOUS,
        }
    }

    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|d| *d == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
}

// ── 字号角色 ─────────────────────────────────────────────────────────

/// 九个字号角色。**组件只说角色，不说字号**（docs/35 §4.4）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    Display,
    Title,
    Section,
    Body,
    Label,
    Caption,
    /// 表头、状态栏小字：10.5 / 等宽 / 大写
    Metadata,
    /// 一切数据：等宽数字、右对齐
    Numeric,
    Code,
}

impl Role {
    /// 该角色在某档密度下的规格。只有正文 / 标签 / 数字 / 代码随密度走（UPDS：字号几乎不动）。
    pub fn spec(self, d: Density) -> TypeSpec {
        let base = match self {
            Self::Display => ROLE_DISPLAY,
            Self::Title => ROLE_TITLE,
            Self::Section => ROLE_SECTION,
            Self::Body => ROLE_BODY,
            Self::Label => ROLE_LABEL,
            Self::Caption => ROLE_CAPTION,
            Self::Metadata => ROLE_METADATA,
            Self::Numeric => ROLE_NUMERIC,
            Self::Code => ROLE_CODE,
        };
        match self {
            Self::Body | Self::Label | Self::Numeric | Self::Code => {
                let size = d.spec().font_size.max(if self == Self::Code { 12.0 } else { 0.0 });
                TypeSpec { size, line_height: base.line_height + (size - base.size), ..base }
            }
            _ => base,
        }
    }
}

// ── 领域色：按偏好解析 ───────────────────────────────────────────────

/// 涨跌颜色约定。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum UpDown {
    /// 绿涨红跌（缺省：交易的是加密与美股期货，外部工具与截图都是这个约定）
    #[default]
    International,
    /// 红涨绿跌
    China,
}

impl UpDown {
    pub fn key(self) -> &'static str {
        match self {
            Self::International => "international",
            Self::China => "china",
        }
    }

    pub fn from_key(s: &str) -> Option<Self> {
        match s {
            "international" => Some(Self::International),
            "china" => Some(Self::China),
            _ => None,
        }
    }
}

/// 按「主题 + 涨跌约定 + 色弱开关」解析好的领域色。**面板里只用这个**，不直接读 Pack。
///
/// 买 = 涨色、卖 = 跌色：买盘 / 主动买 / 买方失衡 / 买单成交一律跟涨色走，
/// 所以切到「红涨绿跌」时买盘自动变红——与国内软件的习惯一致。
#[derive(Clone, Copy, Debug)]
pub struct Domain {
    pub up: Rgba,
    pub down: Rgba,
    pub flat: Rgba,
    /// 买盘 / 主动买 / 买方失衡 / 买单成交
    pub buy: Rgba,
    /// 卖盘 / 主动卖 / 卖方失衡 / 卖单成交
    pub sell: Rgba,
    /// 盘口梯、深度条的底色（买卖色的 18%）
    pub buy_soft: Rgba,
    pub sell_soft: Rgba,
    pub large: Rgba,
    pub spread: Rgba,
    pub poc: Rgba,
    pub naked_poc: Rgba,
    pub order_working: Rgba,
    pub order_cancelled: Rgba,
    pub position_avg: Rgba,
    pub env_live: Rgba,
    pub env_paper: Rgba,
    pub env_backtest: Rgba,
    pub env_replay: Rgba,
    pub env_idle: Rgba,
    pub chart_grid: Rgba,
    pub chart_axis: Rgba,
    pub chart_crosshair: Rgba,
    pub chart_reference: Rgba,
    pub chart_series: [Rgba; 8],
    pub heatmap: [Rgba; 6],
}

impl Domain {
    pub fn resolve(theme: ThemeId, prefs: &Prefs) -> Self {
        let p = theme.pack();
        let (g, r) = if prefs.cvd_safe {
            (p.trade_green_cvd, p.trade_red_cvd)
        } else {
            (p.trade_green, p.trade_red)
        };
        let (up, down) = match prefs.up_down {
            UpDown::International => (g, r),
            UpDown::China => (r, g),
        };
        Self {
            up,
            down,
            flat: p.trade_flat,
            buy: up,
            sell: down,
            buy_soft: up.with_alpha(0.18),
            sell_soft: down.with_alpha(0.18),
            large: p.trade_large,
            spread: p.book_spread,
            poc: p.footprint_poc,
            naked_poc: p.footprint_naked_poc,
            order_working: p.order_working,
            order_cancelled: p.order_cancelled,
            position_avg: p.position_avg,
            env_live: p.env_live,
            env_paper: p.env_paper,
            env_backtest: p.env_backtest,
            env_replay: p.env_replay,
            env_idle: p.env_idle,
            chart_grid: p.chart_grid,
            chart_axis: p.chart_axis,
            chart_crosshair: p.chart_crosshair,
            chart_reference: p.chart_reference,
            chart_series: p.chart_series,
            heatmap: heatmap_scale(&prefs.heatmap_scale),
        }
    }
}

/// 热图色阶。不认识的名字回退 inferno。
pub fn heatmap_scale(name: &str) -> [Rgba; 6] {
    match name {
        "viridis" => HEATMAP_VIRIDIS,
        "cividis" => HEATMAP_CIVIDIS,
        _ => HEATMAP_INFERNO,
    }
}

/// 在色阶上取 t（0..=1）处的颜色（分段线性插值）。
pub fn sample_scale(scale: &[Rgba; 6], t: f32) -> Rgba {
    let t = t.clamp(0.0, 1.0) * 5.0;
    let i = (t.floor() as usize).min(4);
    let f = t - i as f32;
    let (a, b) = (scale[i], scale[i + 1]);
    Rgba::new(a.r + (b.r - a.r) * f, a.g + (b.g - a.g) * f, a.b + (b.b - a.b) * f, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oklch_换算对拍() {
        // 公认的参考点：sRGB 三原色在 OKLCH 里的坐标（Ottosson 2020；CSS Color 4 规范示例）
        for (s, want) in [
            ("oklch(0.62796 0.25768 29.234)", "#ff0000"),
            ("oklch(0.86644 0.29483 142.495)", "#00ff00"),
            ("oklch(0.45201 0.31321 264.052)", "#0000ff"),
        ] {
            assert_eq!(color::parse(s).unwrap().hex(), want, "{s}");
        }
        let c = color::parse("oklch(1 0 0)").unwrap();
        assert_eq!(c.hex(), "#ffffff");
        let c = color::parse("oklch(0 0 0)").unwrap();
        assert_eq!(c.hex(), "#000000");
        let c = color::parse("oklch(0.52 0.14 255 / 0.24)").unwrap();
        assert!((c.a - 0.24).abs() < 1e-6);
        let c = color::parse("#fca50a").unwrap();
        assert_eq!(c.hex(), "#fca50a");
    }

    #[test]
    fn 对比度公式() {
        let (w, k) = (Rgba::new(1.0, 1.0, 1.0, 1.0), Rgba::new(0.0, 0.0, 0.0, 1.0));
        assert!((color::contrast(w, k) - 21.0).abs() < 0.01);
        assert!((color::contrast(k, k) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn upds_标称对比度复核() {
        // UPDS V1 §03 标出的是**下限量级**：dark text.primary 13.4、secondary 6.4、tertiary 4.1；
        // light 12.1 / 5.9 / 3.6。按 WCAG 公式实测不应低于标称值的 90%（实测 dark 正文约 15.2）。
        for (t, want) in [(ThemeId::Dark, [13.4, 6.4, 4.1]), (ThemeId::Light, [12.1, 5.9, 3.6])] {
            let c = t.core();
            let got = [c.text_primary, c.text_secondary, c.text_tertiary]
                .map(|x| color::contrast(x, c.surface_primary));
            for (g, w) in got.iter().zip(want) {
                assert!(*g >= w * 0.9, "{t:?}: 实测 {g:.2} 远低于标称 {w}");
            }
        }
    }

    #[test]
    fn 涨跌约定与色弱() {
        let mut p = Prefs::default();
        let d = Domain::resolve(ThemeId::Dark, &p);
        assert_eq!(d.buy, d.up);
        assert_eq!(d.up, PACK_DARK.trade_green, "缺省国际约定：绿涨");
        p.up_down = UpDown::China;
        let d = Domain::resolve(ThemeId::Dark, &p);
        assert_eq!(d.up, PACK_DARK.trade_red, "中国约定：红涨");
        assert_eq!(d.buy, d.up, "买盘跟涨色走");
        p.cvd_safe = true;
        let d = Domain::resolve(ThemeId::Dark, &p);
        assert_eq!(d.down, PACK_DARK.trade_green_cvd);
    }

    #[test]
    fn 密度只动空间_字号几乎不动() {
        let c = Role::Label.spec(Density::Compact);
        let s = Role::Label.spec(Density::Spacious);
        // token 文件：紧凑 12、宽松 14——字号只动 2，行高动 14
        assert!(s.size - c.size <= 2.0);
        assert!(Density::Spacious.spec().row - Density::Compact.spec().row >= 14.0);
        assert_eq!(Role::Title.spec(Density::Compact), Role::Title.spec(Density::Spacious));
    }

    #[test]
    fn 色阶端点() {
        assert_eq!(sample_scale(&HEATMAP_INFERNO, 0.0), HEATMAP_INFERNO[0]);
        assert_eq!(sample_scale(&HEATMAP_INFERNO, 1.0).hex(), HEATMAP_INFERNO[5].hex());
    }

    #[test]
    fn 高对比主题满足合同() {
        let c = ThemeId::HighContrast.core();
        assert!(color::contrast(c.text_primary, c.surface_primary) >= 7.0);
        assert!(color::contrast(c.accent_primary, c.surface_primary) >= 4.5);
    }
}
