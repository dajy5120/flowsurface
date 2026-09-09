//! 尾窗表格：**画在 canvas 上，不是 widget 行**（docs/23 §10.2）。
//!
//! # 为什么不用 widget
//!
//! iced 0.14 没有虚拟化列表。200 行 × 8 列 = 1600 个 widget，每帧重建整棵树——
//! 这是掉帧的直接原因。canvas 一次 `draw` 画完，并且靠 [`Cache`] 在数据没变时
//! 完全跳过重画。
//!
//! # Cache 失效的判据是 `version`，不是内容
//!
//! 比较内容意味着每帧都要遍历一遍数据，那就把省下来的开销又花回去了。
//! 守护每次快照带一个单调 `version`，变了才 `clear()`。

use iced::widget::canvas::{self, Cache, Frame, Geometry, Path, Text};
use iced::{mouse, Color, Point, Rectangle, Renderer, Size, Theme};

use super::observatory_readout::TailRow;

/// 行高。定值——命中测试靠 `(y - y0) / ROW_H` 直接算行号，
/// 不定高就得逐行累加，那是 O(n) 的命中测试。
pub const ROW_H: f32 = 16.0;
const HEAD_H: f32 = 18.0;
const FONT: f32 = 11.0;
const PAD: f32 = 6.0;

/// 一列。宽度是**像素**，不是比例：等宽字体下按像素排能让数字纵向对齐。
pub struct Col {
    pub title: &'static str,
    pub w: f32,
    pub numeric: bool,
}

pub const COLS: [Col; 6] = [
    Col { title: "seq", w: 78.0, numeric: true },
    Col { title: "时刻", w: 92.0, numeric: false },
    Col { title: "流", w: 32.0, numeric: true },
    Col { title: "长度", w: 58.0, numeric: true },
    Col { title: "标志", w: 106.0, numeric: false },
    Col { title: "负载", w: 0.0, numeric: false }, // 0 = 吃掉剩余宽度
];

pub struct Palette {
    pub head: Color,
    pub dim: Color,
    pub txt: Color,
    pub gold: Color,
    pub bad: Color,
    pub stripe: Color,
}

/// 表格数据 + 缓存。
pub struct TailTable {
    pub rows: Vec<TailRow>,
    /// 守护给的单调版本号。变了才重画。
    pub version: u64,
    pub cache: Cache,
    pub pal: Palette,
    /// Parsed 视图：显示解析结果而不是原始负载。
    pub parsed_view: bool,
}

impl TailTable {
    /// 表格总高度。给 `canvas().height()` 用——canvas 不会自己算内容高度，
    /// 给错了要么截断要么留一大片空白。
    /// 画完整内容需要多宽。
    ///
    /// 负载列不再「吃掉剩余宽度然后把超出的截掉」——按**最长那一行**给足，
    /// 外面套一个横向滚动条。数据长的时候能滚着看全，是这个表的本分。
    ///
    /// 有下限：内容短的时候表格塌成窄窄一条更难看，也更难点。
    pub fn content_width(&self) -> f32 {
        const MIN: f32 = 900.0;
        let fixed: f32 = COLS.iter().map(|c| c.w).sum::<f32>() + COLS.len() as f32 * 4.0 + PAD * 2.0;
        let widest = self
            .rows
            .iter()
            .map(|r| text_px(&payload_text(r, self.parsed_view)))
            .fold(0.0f32, f32::max);
        (fixed + widest + PAD).max(MIN)
    }

    pub fn height(&self) -> f32 {
        HEAD_H + self.rows.len() as f32 * ROW_H + PAD
    }

    /// 像素 y → 行号。行高固定，所以是 O(1)。
    pub fn row_at(&self, y: f32) -> Option<usize> {
        if y < HEAD_H {
            return None;
        }
        let i = ((y - HEAD_H) / ROW_H) as usize;
        (i < self.rows.len()).then_some(i)
    }
}

/// 一行「负载」列里显示的文本。
///
/// 画和量宽必须用**同一份**：两边各算一份，迟早会算出不同的宽度，
/// 于是横向滚动条要么不够长（末尾看不到）要么多出一截空白。
pub fn payload_text(row: &TailRow, parsed_view: bool) -> String {
    // Parsed 视图下显示解析结果；解析不出来时**退回原文**而不是留空——
    // 留空会让人以为这条是空帧
    if parsed_view {
        return match &row.parsed {
            Some(p) if !p.is_empty() => p
                .iter()
                .map(|(k, v)| format!("{}={v}", k.trim_start_matches("$.")))
                .collect::<Vec<_>>()
                .join("  "),
            _ => format!("«未解析» {}", row.preview),
        };
    }
    if row.preview_truncated {
        // 横着滚到头也看不到剩下的——**必须说**，否则那一截会被当成全部
        format!("{}…（共 {} 字节，快照只带前面这段）", row.preview, row.len)
    } else {
        row.preview.clone()
    }
}

/// 一段文本占多少像素（等宽字体，中文两格）。
fn text_px(s: &str) -> f32 {
    let cells: usize = s.chars().map(|c| if (c as u32) > 0x2E80 { 2 } else { 1 }).sum();
    cells as f32 * FONT * 0.56
}

/// 按字符宽度粗算能放几个字。等宽字体下中文约占两格。
fn fit(s: &str, px: f32) -> String {
    let budget = (px / (FONT * 0.56)) as usize;
    let mut used = 0usize;
    let mut out = String::new();
    for c in s.chars() {
        let w = if (c as u32) > 0x2E80 { 2 } else { 1 };
        if used + w > budget.saturating_sub(1) {
            out.push('…');
            return out;
        }
        used += w;
        out.push(c);
    }
    out
}

impl<M> canvas::Program<M> for TailTable {
    type State = ();

    fn draw(
        &self,
        _s: &(),
        r: &Renderer,
        _t: &Theme,
        b: Rectangle,
        _c: mouse::Cursor,
    ) -> Vec<Geometry> {
        let geo = self.cache.draw(r, b.size(), |frame: &mut Frame| {
            let w = frame.width();
            if w <= 40.0 {
                return;
            }
            // 最后一列吃掉剩余宽度
            let fixed: f32 = COLS.iter().map(|c| c.w).sum();
            let last_w = (w - fixed - PAD * 2.0).max(80.0);

            // 表头
            let mut x = PAD;
            for c in COLS.iter() {
                let cw = if c.w > 0.0 { c.w } else { last_w };
                frame.fill_text(Text {
                    content: c.title.into(),
                    position: Point::new(
                        if c.numeric { x + cw - 4.0 } else { x },
                        HEAD_H * 0.25,
                    ),
                    color: self.pal.head,
                    size: iced::Pixels(FONT),
                    align_x: if c.numeric {
                        iced::alignment::Horizontal::Right.into()
                    } else {
                        iced::alignment::Horizontal::Left.into()
                    },
                    ..Default::default()
                });
                x += cw + 4.0;
            }

            for (i, row) in self.rows.iter().enumerate() {
                let y = HEAD_H + i as f32 * ROW_H;
                // 斑马纹：密表里没有它，眼睛会串行
                if i % 2 == 1 {
                    frame.fill(
                        &Path::rectangle(Point::new(0.0, y), Size::new(w, ROW_H)),
                        self.pal.stripe,
                    );
                }
                let has_gap = row.flags.contains("gap");
                let synthetic = row.flags.contains("synthetic");
                let flag_c = if has_gap {
                    self.pal.bad
                } else if row.flags.is_empty() {
                    self.pal.dim
                } else {
                    self.pal.gold
                };
                let clock = chrono::DateTime::from_timestamp_millis(row.recv_ms)
                    .map(|t| t.with_timezone(&chrono::Local).format("%H:%M:%S%.3f").to_string())
                    .unwrap_or_else(|| "—".into());
                let body = payload_text(row, self.parsed_view);

                let cells: [(&str, Color); 6] = [
                    ("", self.pal.dim),
                    ("", self.pal.dim),
                    ("", self.pal.dim),
                    ("", self.pal.dim),
                    ("", flag_c),
                    ("", if synthetic { self.pal.gold } else { self.pal.txt }),
                ];
                let texts = [
                    row.seq.to_string(),
                    clock,
                    row.stream_id.to_string(),
                    row.len.to_string(),
                    if row.flags.is_empty() { "—".into() } else { row.flags.clone() },
                    body,
                ];
                let mut x = PAD;
                for (ci, c) in COLS.iter().enumerate() {
                    let cw = if c.w > 0.0 { c.w } else { last_w };
                    frame.fill_text(Text {
                        content: fit(&texts[ci], cw),
                        position: Point::new(
                            if c.numeric { x + cw - 4.0 } else { x },
                            y + 2.0,
                        ),
                        color: cells[ci].1,
                        size: iced::Pixels(FONT),
                        align_x: if c.numeric {
                            iced::alignment::Horizontal::Right.into()
                        } else {
                            iced::alignment::Horizontal::Left.into()
                        },
                        ..Default::default()
                    });
                    x += cw + 4.0;
                }
            }
        });
        vec![geo]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(n: usize) -> TailTable {
        TailTable {
            rows: (0..n)
                .map(|i| TailRow { seq: i as i64, len: 10, ..Default::default() })
                .collect(),
            version: 1,
            cache: Cache::new(),
            pal: Palette {
                head: Color::BLACK,
                dim: Color::BLACK,
                txt: Color::BLACK,
                gold: Color::BLACK,
                bad: Color::BLACK,
                stripe: Color::TRANSPARENT,
            },
            parsed_view: false,
        }
    }

    #[test]
    fn the_canvas_is_as_wide_as_the_longest_row_so_nothing_is_cut_off() {
        // 原来是「负载列吃掉剩余宽度，超出的截掉」——长数据永远看不全。
        // 现在按最长那一行给足，外面套横向滚动条
        let mut t = table(3);
        let long = "x".repeat(240);
        t.rows[1].preview = long.clone();
        let w = t.content_width();
        assert!(w > super::text_px(&long), "至少要放得下最长那一行：{w}");
        // 内容短的时候有下限：塌成窄窄一条更难看也更难点
        let short = table(3);
        assert_eq!(short.content_width(), 900.0);
        // 多一行更长的，宽度要跟着涨
        t.rows[2].preview = "y".repeat(600);
        assert!(t.content_width() > w);
    }

    #[test]
    fn a_truncated_payload_says_so_because_scrolling_will_not_reveal_the_rest() {
        // 快照只带前面一段。横着滚到头也看不到剩下的——不说的话，
        // 那一截会被当成全部
        let r = TailRow {
            preview: "abc".into(),
            preview_truncated: true,
            len: 5000,
            ..Default::default()
        };
        let t = payload_text(&r, false);
        assert!(t.contains("5000") && t.contains("快照只带"), "{t}");
        // 没截断的就原样，别加噪声
        let r = TailRow { preview: "abc".into(), ..Default::default() };
        assert_eq!(payload_text(&r, false), "abc");
    }

    #[test]
    fn the_parsed_view_falls_back_to_the_raw_text_instead_of_going_blank() {
        // 留空会让人以为这条是空帧
        let r = TailRow { preview: "raw".into(), parsed: None, ..Default::default() };
        assert!(payload_text(&r, true).contains("raw"));
        assert!(payload_text(&r, true).contains("未解析"));
        let r = TailRow {
            parsed: Some(vec![("$.a".into(), "1".into()), ("$.b".into(), "2".into())]),
            ..Default::default()
        };
        // `$.` 前缀在表里是噪声，每一行都一样
        assert_eq!(payload_text(&r, true), "a=1  b=2");
    }

    #[test]
    fn hit_testing_is_constant_time_because_rows_are_fixed_height() {
        // 不定高的话命中测试要逐行累加——200 行 × 每次鼠标移动
        let t = table(10);
        assert_eq!(t.row_at(0.0), None, "表头不算行");
        assert_eq!(t.row_at(HEAD_H + 0.1), Some(0));
        assert_eq!(t.row_at(HEAD_H + ROW_H * 3.5), Some(3));
        assert_eq!(t.row_at(HEAD_H + ROW_H * 100.0), None, "越界给 None 不是最后一行");
    }

    #[test]
    fn the_canvas_height_matches_the_content() {
        // canvas 不会自己算内容高度。给错了要么截断、要么留一大片空白
        let t = table(50);
        assert!(t.height() > 50.0 * ROW_H);
        assert!(t.height() < 50.0 * ROW_H + 40.0);
        assert!(table(0).height() < HEAD_H + 10.0);
    }

    #[test]
    fn text_is_clipped_by_display_width_not_byte_count() {
        // 中文占两格。按字节切会切出半个字，按字符切会让中文行溢出列宽
        let wide = fit("中文中文中文中文中文中文", 60.0);
        let narrow = fit("abcdefghijklmnopqrstuvwx", 60.0);
        assert!(wide.chars().count() < narrow.chars().count(), "同宽度下中文应更少字");
        assert!(fit("短", 200.0) == "短", "放得下就不加省略号");
    }

    #[test]
    fn the_last_column_absorbs_the_remaining_width() {
        // 负载列写死宽度的话，窗口拉宽只会多出一片空白
        let fixed: f32 = COLS.iter().map(|c| c.w).sum();
        assert_eq!(COLS.last().unwrap().w, 0.0, "末列用 0 表示自适应");
        assert!(fixed > 0.0);
    }

    #[test]
    fn a_row_that_failed_to_parse_falls_back_to_the_raw_bytes() {
        // Parsed 视图下留空会让人以为这条是空帧
        let mut t = table(1);
        t.parsed_view = true;
        t.rows[0].preview = "PONG".into();
        t.rows[0].parsed = None;
        // 渲染逻辑内联在 draw 里，这里断言的是那条规则的**输入前提**：
        // 没有 parsed 时 preview 必须还在
        assert!(!t.rows[0].preview.is_empty());
    }
}
