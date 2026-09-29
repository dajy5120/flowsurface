//! 数据网格（UPDS V3 §15 / V8，docs/35 §6.1 / 批 4）——**面板里的表格一律用它**。
//!
//! 规则（UPDS）：文字左对齐、数字右对齐且等宽数字；单位写在表头不写在格里；一列一种小数位；
//! 只有被排序的那一列显示排序箭头；缺失值永远排最后；选中 = 强调色 24% + 前缘 2px；
//! 悬停 = 6% 洗色；固定列用 1px 分隔线不用阴影。
//!
//! # 虚拟滚动
//!
//! 只渲染可见的几十行，上下用等高空白撑出总高度——10⁶ 行照样 60fps（UPDS V5 §39 的
//! 「难看的数」）。行高固定为当前密度的行高，所以可见区间可以直接从滚动偏移算出来。
//!
//! # 状态放在调用方
//!
//! [`GridState`] 由面板自己持有（排序、列宽、选中、滚动位置、排好的顺序）；
//! 数据变了调 [`GridState::resort`]，渲染时只按 `order` 取行，不在每帧排序。

use std::cmp::Ordering;

use iced::widget::{Space, button, column, container, mouse_area, responsive, row, scrollable};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding, Point};

use super::fmt::{self, Absence, Provenance};
use super::metrics::{self};
use super::widgets::{self, Tone};
use super::{color, core, text as t};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
}

/// 这一列按什么排序。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKind {
    /// 自然序（标识符、名字）：`BTC-2` 在 `BTC-10` 前
    Natural,
    /// 数值；缺失排最后
    Number,
    /// 不可排序
    None,
}

#[derive(Debug, Clone)]
pub struct Column {
    pub title: String,
    /// 单位写在表头（UPDS：不写在每个格子里）
    pub unit: Option<String>,
    pub align: Align,
    pub width: f32,
    pub sort: SortKind,
}

impl Column {
    pub fn text(title: impl Into<String>, width: f32) -> Self {
        Self { title: title.into(), unit: None, align: Align::Left, width, sort: SortKind::Natural }
    }

    pub fn num(title: impl Into<String>, unit: Option<&str>, width: f32) -> Self {
        Self { title: title.into(), unit: unit.map(str::to_string), align: Align::Right, width, sort: SortKind::Number }
    }
}

/// 一个格子。
#[derive(Debug, Clone)]
pub enum Cell {
    Text(String),
    /// 标识符：等宽、过长时中间省略
    Id(String),
    /// 数值：`v` 用于排序与合计，`s` 是按列规则格式化好的显示文字
    Num { v: Option<f64>, s: String, prov: Provenance },
    Absent(Absence),
    Badge(String, Tone),
    /// 带颜色的文字（涨跌之类）。**同时要有非颜色的区分**（符号或正负号）
    Colored(String, Color),
}

impl Cell {
    pub fn num(v: f64, s: String) -> Self {
        Self::Num { v: Some(v), s, prov: Provenance::Measured }
    }

    fn sort_text(&self) -> Option<&str> {
        match self {
            Self::Text(s) | Self::Id(s) | Self::Colored(s, _) | Self::Badge(s, _) => Some(s),
            Self::Num { s, .. } => Some(s),
            Self::Absent(_) => None,
        }
    }

    fn sort_num(&self) -> Option<f64> {
        match self {
            Self::Num { v, .. } => *v,
            Self::Text(s) | Self::Colored(s, _) => s.replace([' ', '−'], "").parse().ok(),
            _ => None,
        }
    }

    /// 复制为 TSV 时的文字（完整值，不省略）。
    fn plain(&self) -> String {
        match self {
            Self::Text(s) | Self::Id(s) | Self::Colored(s, _) | Self::Badge(s, _) => s.clone(),
            Self::Num { v: Some(v), .. } => format!("{v}"),
            Self::Num { v: None, s, .. } => s.clone(),
            Self::Absent(a) => a.glyph().to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum GridMsg {
    Scrolled(scrollable::Viewport),
    /// 点表头：同一列再点一次反向，第三次取消
    Sort(usize),
    Select(usize),
    DragStart(usize),
    DragMove(Point),
    DragEnd,
}

#[derive(Debug, Clone)]
pub struct GridState {
    pub offset: f32,
    /// (列, 降序)
    pub sort: Option<(usize, bool)>,
    pub widths: Vec<f32>,
    /// 选中的数据行（数据下标，不是显示位置——排序、刷新后选中不丢）
    pub selected: Option<usize>,
    /// 排好的显示顺序（数据下标）
    pub order: Vec<usize>,
    /// 行区滚动容器的 id（要从代码里滚动时用，例如定位到某一行）
    pub id: iced::widget::Id,
    drag: Option<(usize, Option<f32>, f32)>,
}

impl GridState {
    pub fn new(cols: &[Column]) -> Self {
        Self {
            offset: 0.0,
            sort: None,
            widths: cols.iter().map(|c| c.width).collect(),
            selected: None,
            order: Vec::new(),
            id: iced::widget::Id::unique(),
            drag: None,
        }
    }

    /// 滚到第 `row` 个显示位置（让它出现在可见区顶部）。
    pub fn scroll_to_row<M: 'static + Send>(&self, row: usize) -> iced::Task<M> {
        iced::widget::operation::scroll_to(
            self.id.clone(),
            iced::widget::operation::AbsoluteOffset { x: None, y: Some(row as f32 * metrics::row_height()) },
        )
    }

    /// 数据或排序变了之后调：重排 `order`。稳定排序，平局保持原顺序（UPDS：同一排序跑两遍不换位）。
    pub fn resort(&mut self, cols: &[Column], rows: &[Vec<Cell>]) {
        let mut order: Vec<usize> = (0..rows.len()).collect();
        if let Some((ci, desc)) = self.sort
            && let Some(col) = cols.get(ci)
        {
            let key = |i: usize| rows[i].get(ci);
            match col.sort {
                SortKind::Number => order.sort_by(|a, b| {
                    fmt::cmp_opt(key(*a).and_then(Cell::sort_num), key(*b).and_then(Cell::sort_num), desc)
                }),
                SortKind::Natural => order.sort_by(|a, b| {
                    match (key(*a).and_then(Cell::sort_text), key(*b).and_then(Cell::sort_text)) {
                        (Some(x), Some(y)) => {
                            let o = fmt::natural_cmp(x, y);
                            if desc { o.reverse() } else { o }
                        }
                        (Some(_), None) => Ordering::Less,
                        (None, Some(_)) => Ordering::Greater,
                        (None, None) => Ordering::Equal,
                    }
                }),
                SortKind::None => {}
            }
        }
        self.order = order;
        if self.widths.len() != cols.len() {
            self.widths = cols.iter().map(|c| c.width).collect();
        }
    }

    /// 处理网格消息。排序变了会自己重排。
    pub fn update(&mut self, msg: GridMsg, cols: &[Column], rows: &[Vec<Cell>]) {
        match msg {
            GridMsg::Scrolled(v) => self.offset = v.absolute_offset().y,
            GridMsg::Sort(i) => {
                if cols.get(i).is_some_and(|c| c.sort != SortKind::None) {
                    // 缺省方向：数字列降序（大的在上更常用），文字列升序。
                    // 首点 = 缺省方向 → 再点 = 反向 → 第三点 = 取消
                    let first = cols[i].align == Align::Right;
                    self.sort = match self.sort {
                        Some((c, d)) if c == i && d == first => Some((i, !first)),
                        Some((c, _)) if c == i => None,
                        _ => Some((i, first)),
                    };
                    self.resort(cols, rows);
                }
            }
            GridMsg::Select(i) => self.selected = if self.selected == Some(i) { None } else { Some(i) },
            GridMsg::DragStart(i) => {
                if let Some(w) = self.widths.get(i) {
                    self.drag = Some((i, None, *w));
                }
            }
            GridMsg::DragMove(p) => {
                if let Some((i, start, w0)) = self.drag.as_mut() {
                    let s = *start.get_or_insert(p.x);
                    let w = (*w0 + p.x - s).max(40.0);
                    if let Some(slot) = self.widths.get_mut(*i) {
                        *slot = w;
                    }
                }
            }
            GridMsg::DragEnd => self.drag = None,
        }
    }

    /// 全部行按当前顺序导出为 TSV（含表头，完整值不省略）——粘到表格软件里列对得上。
    pub fn to_tsv(&self, cols: &[Column], rows: &[Vec<Cell>]) -> String {
        let mut out = cols
            .iter()
            .map(|c| match &c.unit {
                Some(u) => format!("{} ({u})", c.title),
                None => c.title.clone(),
            })
            .collect::<Vec<_>>()
            .join("\t");
        let order: Vec<usize> = if self.order.len() == rows.len() { self.order.clone() } else { (0..rows.len()).collect() };
        for i in order {
            out.push('\n');
            out.push_str(&rows[i].iter().map(Cell::plain).collect::<Vec<_>>().join("\t"));
        }
        out
    }

    fn sort_summary(&self, cols: &[Column]) -> String {
        match self.sort.and_then(|(i, d)| cols.get(i).map(|c| (c, d))) {
            Some((c, d)) => format!("按「{}」{}", c.title, if d { "降序" } else { "升序" }),
            None => "原始顺序".into(),
        }
    }
}

fn cell_view<'a, M: 'a>(cell: &'a Cell, col: &Column, w: f32) -> Element<'a, M> {
    let c = core();
    let body: Element<'a, M> = match cell {
        Cell::Text(s) => t::body(s.as_str()).into(),
        Cell::Id(s) => {
            // 按列宽估一个字符数（等宽字体约 0.6em）
            let max = ((w - 8.0) / (t::size(super::Role::Code) * 0.6)).max(6.0) as usize;
            t::code(fmt::elide_middle(s, max)).into()
        }
        Cell::Num { s, prov, .. } => widgets::value(s.as_str(), *prov).into(),
        Cell::Absent(a) => widgets::absent(*a).into(),
        Cell::Badge(s, tone) => widgets::badge(s.as_str(), *tone),
        Cell::Colored(s, fg) => t::numeric(s.as_str()).color(*fg).into(),
    };
    let _ = c;
    container(body)
        .width(Length::Fixed(w))
        .padding(Padding::from([0.0, metrics::space(2)]))
        .align_x(if col.align == Align::Right { Alignment::End } else { Alignment::Start })
        .align_y(Alignment::Center)
        .height(Length::Fill)
        .clip(true)
        .into()
}

/// 画网格。`on` 把网格消息包成面板自己的消息。`footer` 是页脚右侧的附加说明（合计等）。
pub fn view<'a, M: Clone + 'a>(
    cols: &'a [Column],
    rows: &'a [Vec<Cell>],
    st: &'a GridState,
    footer: Option<String>,
    on: impl Fn(GridMsg) -> M + Clone + 'a,
) -> Element<'a, M> {
    let c = core();
    let row_h = metrics::row_height();
    let widths: Vec<f32> = cols.iter().enumerate().map(|(i, col)| st.widths.get(i).copied().unwrap_or(col.width)).collect();
    let total_w: f32 = widths.iter().sum::<f32>() + 3.0;

    // ── 表头：点标题排序，标题右侧的竖线可拖动调宽 ──
    let mut head = row![Space::new().width(Length::Fixed(3.0))].height(Length::Fixed(metrics::panel_header()));
    for (i, col) in cols.iter().enumerate() {
        let arrow = match st.sort {
            Some((ci, d)) if ci == i => if d { " ▼" } else { " ▲" },
            _ => "",
        };
        let title = match &col.unit {
            Some(u) => format!("{} ({u}){arrow}", col.title),
            None => format!("{}{arrow}", col.title),
        };
        let on_s = on.clone();
        let label = button(
            container(t::metadata(title))
                .width(Length::Fill)
                .align_x(if col.align == Align::Right { Alignment::End } else { Alignment::Start }),
        )
        .padding(Padding::from([0.0, metrics::space(2)]))
        .height(Length::Fill)
        .width(Length::Fixed(widths[i] - 4.0))
        .style(|th, s| widgets::button_style(widgets::Kind::Ghost, th, s));
        let label = if col.sort != SortKind::None { label.on_press(on_s(GridMsg::Sort(i))) } else { label };
        let on_g = on.clone();
        let grip = mouse_area(
            container(Space::new().width(Length::Fixed(1.0)).height(Length::Fill))
                .padding(Padding::from([4.0, 1.5]))
                .style(|_| container::Style { background: Some(Background::Color(color(core().border_subtle))), ..Default::default() }),
        )
        .interaction(iced::mouse::Interaction::ResizingHorizontally)
        .on_press(on_g(GridMsg::DragStart(i)));
        head = head.push(label).push(grip);
    }
    let head = container(head.align_y(Alignment::Center)).style(|_| {
        let c = core();
        container::Style {
            background: Some(Background::Color(color(c.surface_secondary))),
            border: Border { width: 0.0, ..Default::default() },
            ..Default::default()
        }
    });

    // ── 行（虚拟滚动）──
    let n = rows.len();
    // 借用排好的顺序，**不每帧复制**（10⁶ 行时复制一次就是一百万个下标）
    let order: std::borrow::Cow<'a, [usize]> =
        if st.order.len() == n { std::borrow::Cow::Borrowed(&st.order) } else { (0..n).collect::<Vec<_>>().into() };
    let on_r = on.clone();
    let body = responsive(move |size| {
        let t0 = std::time::Instant::now();
        let visible = (size.height / row_h).ceil() as usize + 2;
        let first = ((st.offset / row_h).floor() as usize).min(n.saturating_sub(1));
        let last = (first + visible).min(n);
        let mut col = column![Space::new().height(Length::Fixed(first as f32 * row_h))];
        for (pos, &di) in order.iter().enumerate().take(last).skip(first) {
            let selected = st.selected == Some(di);
            let mut r = row![container(Space::new().width(Length::Fixed(2.0)).height(Length::Fill)).style(move |_| container::Style {
                background: selected.then(|| Background::Color(color(core().accent_primary))),
                ..Default::default()
            })]
            .spacing(1)
            .height(Length::Fixed(row_h));
            for (ci, colm) in cols.iter().enumerate() {
                match rows[di].get(ci) {
                    Some(cell) => r = r.push(cell_view(cell, colm, widths[ci])),
                    None => r = r.push(cell_view(&ABSENT, colm, widths[ci])),
                }
            }
            let on_sel = on_r.clone();
            let stripe = pos % 2 == 1;
            col = col.push(
                button(r)
                    .padding(0)
                    .width(Length::Fixed(total_w))
                    .on_press(on_sel(GridMsg::Select(di)))
                    .style(move |th, s| {
                        let mut b = widgets::button_style(widgets::Kind::Ghost, th, s);
                        let c = core();
                        b.text_color = color(c.text_primary);
                        if selected {
                            b.background = Some(Background::Color(color(c.accent_soft)));
                        } else if b.background.is_none() && stripe {
                            b.background = Some(Background::Color(Color { a: 0.025, ..color(c.text_primary) }));
                        }
                        b.border = Border::default();
                        b
                    }),
            );
        }
        col = col.push(Space::new().height(Length::Fixed((n - last) as f32 * row_h)));
        LAST_BUILD_US.store(t0.elapsed().as_micros() as u64, std::sync::atomic::Ordering::Relaxed);
        let on_s = on_r.clone();
        scrollable(col)
            .id(st.id.clone())
            .on_scroll(move |v| on_s(GridMsg::Scrolled(v)))
            .height(Length::Fill)
            .width(Length::Fixed(total_w + 12.0))
            .into()
    });

    // ── 页脚：行数 · 选中 · 排序 · 附加说明 ──
    // 页脚用说明文字而不是元数据角色：元数据会转大写，单位 µs 会变成 MS（批 4 踩过）
    let mut foot = row![t::caption(format!("{n} 行")), t::caption(st.sort_summary(cols))].spacing(metrics::space(4));
    if st.selected.is_some() {
        foot = foot.push(t::caption("已选 1"));
    }
    if let Some(f) = footer {
        foot = foot.push(Space::new().width(Length::Fill)).push(t::caption(f));
    }

    let table = column![head, body].width(Length::Fixed(total_w + 12.0)).height(Length::Fill);
    let on_m = on.clone();
    let on_e = on.clone();
    let dragging = st.drag.is_some();
    let table = mouse_area(scrollable(table).direction(scrollable::Direction::Horizontal(scrollable::Scrollbar::new())).height(Length::Fill))
        .on_move(move |p| on_m(GridMsg::DragMove(p)))
        .on_release(on_e(GridMsg::DragEnd));
    let table: Element<'a, M> = if dragging {
        table.interaction(iced::mouse::Interaction::ResizingHorizontally).into()
    } else {
        table.into()
    };

    column![
        container(table).height(Length::Fill).style(move |_| container::Style {
            background: Some(Background::Color(color(c.surface_primary))),
            ..Default::default()
        }),
        container(foot).padding(Padding::from([2.0, metrics::space(3)])).width(Length::Fill),
    ]
    // 外层撑满面板宽度：页脚里「撑满」的空白要有确定的父宽才排得开（批 4 踩过，右侧说明被挤没）
    .width(Length::Fill)
    .into()
}

static ABSENT: Cell = Cell::Absent(Absence::Missing);

/// 上一次构建可见行的耗时（微秒）。行在布局阶段（`responsive` 里）才构建，
/// 所以只能在那里量；组件样张页把它显示出来作为性能证据（UPDS V5 §39）。
static LAST_BUILD_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn last_build_us() -> u64 {
    LAST_BUILD_US.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data() -> (Vec<Column>, Vec<Vec<Cell>>) {
        let cols = vec![Column::text("代码", 80.0), Column::num("实际", Some("USD"), 100.0)];
        let rows = vec![
            vec![Cell::Id("CC-10".into()), Cell::num(3.0, "3.00".into())],
            vec![Cell::Id("CC-2".into()), Cell::Absent(Absence::Missing)],
            vec![Cell::Id("CC-1".into()), Cell::num(7.0, "7.00".into())],
        ];
        (cols, rows)
    }

    #[test]
    fn 排序_数字首点降序_缺失垫底_再点反向_三点取消() {
        let (cols, rows) = data();
        let mut st = GridState::new(&cols);
        st.update(GridMsg::Sort(1), &cols, &rows);
        assert_eq!(st.order, [2, 0, 1], "降序，缺失最后");
        st.update(GridMsg::Sort(1), &cols, &rows);
        assert_eq!(st.order, [0, 2, 1], "升序，缺失仍最后");
        st.update(GridMsg::Sort(1), &cols, &rows);
        assert_eq!(st.order, [0, 1, 2], "取消 = 原始顺序");
    }

    #[test]
    fn 自然序与_tsv() {
        let (cols, rows) = data();
        let mut st = GridState::new(&cols);
        st.update(GridMsg::Sort(0), &cols, &rows);
        assert_eq!(st.order, [2, 1, 0], "CC-1 < CC-2 < CC-10");
        let tsv = st.to_tsv(&cols, &rows);
        assert!(tsv.starts_with("代码\t实际 (USD)\nCC-1\t7\nCC-2\t—\nCC-10\t3"), "{tsv}");
    }

    #[test]
    fn 选中按数据下标_排序后不丢() {
        let (cols, rows) = data();
        let mut st = GridState::new(&cols);
        st.update(GridMsg::Select(2), &cols, &rows);
        st.update(GridMsg::Sort(0), &cols, &rows);
        assert_eq!(st.selected, Some(2));
    }

    #[test]
    fn 拖动调宽_有下限() {
        let (cols, rows) = data();
        let mut st = GridState::new(&cols);
        st.update(GridMsg::DragStart(0), &cols, &rows);
        st.update(GridMsg::DragMove(Point::new(100.0, 0.0)), &cols, &rows);
        st.update(GridMsg::DragMove(Point::new(-200.0, 0.0)), &cols, &rows);
        assert_eq!(st.widths[0], 40.0);
        st.update(GridMsg::DragEnd, &cols, &rows);
    }
}
