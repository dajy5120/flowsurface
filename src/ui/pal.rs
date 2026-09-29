//! 自研面板的语义色板（docs/35 批 5–7）：旧的 `const C_*: Color` 按语义逐一映射到 token。
//!
//! 各面板当初各自定义了一套几乎同名的颜色常量（C_HEAD / C_DIM / C_TXT / C_OK / C_BAD …），
//! 迁移时统一换成这里的函数——它们跟着主题、涨跌约定、色弱开关走，写死的 RGB 做不到。
//!
//! | 旧常量 | 语义 | 新来源 |
//! |---|---|---|
//! | C_TXT | 正文 | `text.primary` |
//! | C_HEAD | 标题、表头 | `text.primary`（层级靠字号与位置，不靠染蓝：UPDS 强调色只留给选中 / 焦点 / 主要操作） |
//! | C_DIM | 次要说明 | `text.secondary` |
//! | C_PEND | 待定、占位 | `text.tertiary` |
//! | C_OK / C_BAD / C_WARN | 状态好 / 坏 / 注意 | `status.success / danger / warning` |
//! | C_POS / C_NEG、C_BUY / C_SELL、C_UP / C_DOWN、C_EQUITY / C_DD | 盈亏、买卖、涨跌 | 领域包涨 / 跌色（随涨跌约定与色弱开关） |
//! | C_BLUE / C_FEAT | 信息、特征系列 | `status.info` / 图表系列 1 |
//! | C_CHART | 图表参数系列 | 图表系列 6 |
//! | C_NOW | 「当前」标记 | 参考线色 |
//! | C_LINE / C_BORDER / C_BODY_LINE | 细分隔线 | `border.subtle` |
//! | C_HEAD_LINE | 表头下分隔线 | `border.default` |
//! | C_BODY_GROUP / C_HEAD_GROUP | 分组分隔线 | `border.strong` |
//! | C_BAND / C_SECT | 分组色带 | `accent.soft` |
//! | C_CARD_BG | 卡片底 | `surface.secondary` |
//! | C_GRID / C_AXIS | 图表网格 / 坐标轴 | 领域包 `chart.grid / chart.axis` |

use iced::Color;

use super::{color, core, domain};

pub fn txt() -> Color {
    color(core().text_primary)
}
pub fn head() -> Color {
    color(core().text_primary)
}
pub fn dim() -> Color {
    color(core().text_secondary)
}
pub fn pend() -> Color {
    color(core().text_tertiary)
}
pub fn ok() -> Color {
    color(core().status_success)
}
pub fn bad() -> Color {
    color(core().status_danger)
}
pub fn warn() -> Color {
    color(core().status_warning)
}
pub fn info() -> Color {
    color(core().status_info)
}
pub fn accent() -> Color {
    color(core().accent_primary)
}
/// 涨 / 买 / 盈利
pub fn up() -> Color {
    color(domain().up)
}
/// 跌 / 卖 / 亏损
pub fn down() -> Color {
    color(domain().down)
}
pub fn series(i: usize) -> Color {
    color(domain().chart_series[i % 8])
}
pub fn reference() -> Color {
    color(domain().chart_reference)
}
pub fn line() -> Color {
    color(core().border_subtle)
}
pub fn head_line() -> Color {
    color(core().border_default)
}
pub fn group_line() -> Color {
    color(core().border_strong)
}
pub fn band() -> Color {
    color(core().accent_soft)
}
pub fn card_bg() -> Color {
    color(core().surface_secondary)
}
pub fn grid() -> Color {
    color(domain().chart_grid)
}
pub fn axis() -> Color {
    color(domain().chart_axis)
}
/// 带透明度的语义色（旧代码里 `Color { a: .., ..C_X }` 的写法用它）。
pub fn alpha(c: Color, a: f32) -> Color {
    Color { a, ..c }
}

/// 「过期」：落在「降级（琥珀）」与「错误（红）」之间，三者分得开（UPDS：过期要显眼但不是错误）。
pub fn stale() -> Color {
    let (w, b) = (warn(), bad());
    Color { r: (w.r + b.r) / 2.0, g: (w.g + b.g) / 2.0, b: (w.b + b.b) / 2.0, a: 1.0 }
}
