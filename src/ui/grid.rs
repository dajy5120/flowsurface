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
//! # 分组（docs/35 §16.5 第 2 项）
//!
//! 列标了 [`Column::groupable`] 的，页脚出现「分组」按钮，按该列的值把行归组；组头可折叠，
//! 组内仍按当前排序。组的先后 = 组内第一行在排序结果里的位置（按「净收益」降序时，
//! 最赚钱的那一组在最上面）。组头与数据行等高，虚拟滚动不受影响。
//!
//! # 状态放在调用方
//!
//! [`GridState`] 由面板自己持有（排序、分组、列宽、选中、滚动位置、排好的顺序）；
//! 数据变了调 [`GridState::resort`]，渲染时只按排好的顺序取行，不在每帧排序。
//!
//! 数据既可以借（`&[Column]` / `&[Vec<Cell>]` / `&GridState`，面板自己持有数据时），
//! 也可以交出所有权（`Vec` / `Rc`，数据来自每帧的旁路快照、借不出 `'a` 时）。

use std::borrow::Borrow;
use std::cmp::Ordering;
use std::collections::HashSet;

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
    /// 可以按这一列分组（页脚「分组」按钮在这些列之间轮换）
    pub group: bool,
    /// 行身份列（docs/35 §8）：变化闪烁、光标下不重排都靠它认行——不能用数据下标，
    /// 新行插在最上面时下标整体平移，整张表会一起闪
    pub key: bool,
    /// 固定在最左边（docs/35 §6.1）：其余列横向按整列翻页时它不动
    pub pinned: bool,
}

impl Column {
    pub fn text(title: impl Into<String>, width: f32) -> Self {
        Self { title: title.into(), unit: None, align: Align::Left, width, sort: SortKind::Natural, group: false, key: false, pinned: false }
    }

    pub fn num(title: impl Into<String>, unit: Option<&str>, width: f32) -> Self {
        Self {
            title: title.into(),
            unit: unit.map(str::to_string),
            align: Align::Right,
            width,
            sort: SortKind::Number,
            group: false,
            key: false,
            pinned: false,
        }
    }

    /// 固定在最左边。
    pub fn pinned(mut self) -> Self {
        self.pinned = true;
        self
    }

    /// 这一列是行身份（见 [`Column::key`] 字段）。
    pub fn key(mut self) -> Self {
        self.key = true;
        self
    }

    /// 允许按这一列分组。
    pub fn groupable(mut self) -> Self {
        self.group = true;
        self
    }
}

/// 过滤条件的比较方式（过滤器构建器，docs/35 §6.1 / UPDS V6 §43）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterOp {
    Gt,
    Ge,
    Lt,
    Le,
    Eq,
    Contains,
    NotContains,
}

impl FilterOp {
    pub const ALL: [FilterOp; 7] = [Self::Gt, Self::Ge, Self::Lt, Self::Le, Self::Eq, Self::Contains, Self::NotContains];
}

impl std::fmt::Display for FilterOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Gt => ">",
            Self::Ge => "≥",
            Self::Lt => "<",
            Self::Le => "≤",
            Self::Eq => "=",
            Self::Contains => "包含",
            Self::NotContains => "不含",
        })
    }
}

/// 一条过滤条件：第 `col` 列 `op` `value`。值解析不出数时数值比较不生效（不误删行）。
#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    pub col: usize,
    pub op: FilterOp,
    pub value: String,
}

impl Filter {
    fn keeps(&self, row: &[Cell]) -> bool {
        let Some(cell) = row.get(self.col) else { return true };
        let v = self.value.trim();
        if v.is_empty() {
            return true;
        }
        match self.op {
            FilterOp::Contains | FilterOp::NotContains => {
                let hit = cell.plain().to_lowercase().contains(&v.to_lowercase());
                hit == (self.op == FilterOp::Contains)
            }
            op => {
                let Ok(want) = v.replace(['_', ' '], "").parse::<f64>() else { return true };
                // 缺失值不满足任何数值条件（「> 0」不该把空格子留下）
                let Some(x) = cell.sort_num() else { return false };
                match op {
                    FilterOp::Gt => x > want,
                    FilterOp::Ge => x >= want,
                    FilterOp::Lt => x < want,
                    FilterOp::Le => x <= want,
                    _ => (x - want).abs() < 1e-9,
                }
            }
        }
    }
}

/// 网格上方展开的设置区：列管理器 / 过滤器构建器。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridPanel {
    Columns,
    Filters,
}

/// 一个显示位置上画什么：组头，或第几条数据行。
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Group { key: String, count: usize, collapsed: bool },
    Row(usize),
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
            // 只取数字部分再解析：来源前缀（~ ≈ ^ ·）、方向符号（▲ ▼）、单位（%）、千分位空格都去掉，
            // 否则「▲ +4.97%」这类格子既排不了序也过滤不了（曾经整列当缺失）
            Self::Text(s) | Self::Colored(s, _) => {
                let t: String = s
                    .chars()
                    .map(|c| if c == '−' { '-' } else { c })
                    .filter(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E'))
                    .collect();
                let t = t.trim_start_matches(['e', 'E']);
                if t.is_empty() { None } else { t.parse().ok() }
            }
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

    /// 分组用的键：显示文字；缺失归到一组。
    fn group_key(&self) -> String {
        self.sort_text().map(str::to_string).unwrap_or_else(|| "（空）".into())
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
    /// 页脚「分组」按钮：无 → 第一个可分组列 → 下一个 → … → 无
    CycleGroup,
    /// 点组头：折叠 / 展开这一组
    ToggleGroup(String),
    /// 鼠标进 / 出行区：在里面时不重排（docs/35 §8，UPDS V5 §30）
    Hover(bool),
    /// 展开 / 收起列管理器或过滤器
    Panel(GridPanel),
    /// 列管理器：显示 / 隐藏第 i 列
    ToggleCol(usize),
    /// 列管理器：第 i 列前移（-1）/ 后移（+1）
    MoveCol(usize, i8),
    /// 列管理器：恢复全部列与原始顺序
    ResetCols,
    /// 过滤器：加一条 / 改第 k 条的列、比较方式、值 / 删第 k 条 / 全清
    AddFilter,
    FilterCol(usize, usize),
    FilterOp(usize, FilterOp),
    FilterVal(usize, String),
    RemoveFilter(usize),
    ClearFilters,
    /// 有固定列时，其余列按整列翻页（+1 右翻 / -1 左翻）
    ColPage(i32),
    /// 在第 i 行上右键：在光标处弹出菜单（UPDS V2 §12）
    RowMenu(usize),
    CloseMenu,
    /// 菜单项：复制这一行 / 复制选中的行（TSV，含表头）
    CopyRow(usize),
    CopySelected,
    /// 菜单项：全选 / 取消选中
    SelectAll,
    ClearSelection,
}

/// 网格复制出来、等主循环写进剪贴板的文字（网格自己发不了 iced 的剪贴板 Task）。
static CLIP: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// 主循环每个 Tick 取一次：有就写进剪贴板。
pub fn take_clipboard() -> Option<String> {
    CLIP.lock().ok().and_then(|mut g| g.take())
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum CopyReq {
    Row(usize),
    Selected,
}

/// 选中行的计数与合计（见 [`GridState::selection_summary`]）。
#[derive(Debug, Clone, PartialEq)]
pub struct SelSummary {
    pub count: usize,
    /// (列名, 合计, 缺失个数)
    pub sum: Option<(String, f64, usize)>,
}

impl SelSummary {
    /// 「选中 3 · Σ 净 12.50」「选中 12 · Σ 净 2 639.10 · 12 个中 3 个缺失」
    pub fn label(&self) -> String {
        let mut s = format!("选中 {}", self.count);
        if let Some((name, v, missing)) = &self.sum {
            s.push_str(&format!(" · Σ {name} {}", super::fmt::withheld_if(super::hide_values(), fmt::number(*v, 2, fmt::Rounding::Money))));
            if *missing > 0 {
                s.push_str(&format!(" · {} 个中 {missing} 个缺失", self.count));
            }
        }
        s
    }
}

static SEL: std::sync::Mutex<Option<(std::time::Instant, SelSummary)>> = std::sync::Mutex::new(None);

/// 网格画出来时发布自己的选中合计；状态栏读最近 1 秒内发布的那份（多张网格时以最后画的为准）。
pub fn publish_selection(s: Option<SelSummary>) {
    if let (Some(s), Ok(mut g)) = (s, SEL.lock()) {
        *g = Some((std::time::Instant::now(), s));
    }
}

/// 状态栏用：当前可见网格的选中合计（1 秒内没有网格发布 = 没有选中）。
pub fn current_selection() -> Option<SelSummary> {
    SEL.lock().ok().and_then(|g| g.as_ref().filter(|(t, _)| t.elapsed().as_secs() < 1).map(|(_, s)| s.clone()))
}

#[derive(Debug, Clone)]
pub struct GridState {
    pub offset: f32,
    /// (列, 降序)
    pub sort: Option<(usize, bool)>,
    pub widths: Vec<f32>,
    /// 选中的数据行（数据下标，不是显示位置——排序、刷新后选中不丢）
    pub selected: Option<usize>,
    /// Ctrl+点击加选的其他行（数据下标；与 `selected` 合起来是全部选中）
    pub extra: std::collections::BTreeSet<usize>,
    /// 排好的显示顺序（数据下标）
    pub order: Vec<usize>,
    /// 行区滚动容器的 id（要从代码里滚动时用，例如定位到某一行）
    pub id: iced::widget::Id,
    /// 按哪一列分组（`None` = 不分组）
    pub group_by: Option<usize>,
    /// 折叠着的组（按组键；数据刷新后折叠不丢）
    pub collapsed: HashSet<String>,
    /// 分组时的显示序列（组头 + 行）；不分组时为空，直接用 `order`
    pub items: Vec<Item>,
    drag: Option<(usize, Option<f32>, f32)>,
    /// 上一次每行（按键列）各格的显示文字：比出哪些格变了
    prev: std::collections::HashMap<String, Vec<String>>,
    /// 变了的格（键, 列）→ 变化时刻；120ms 内画高亮
    flashed: std::collections::HashMap<(String, usize), std::time::Instant>,
    /// 光标在行区里
    hovering: bool,
    /// 悬停期间积压的「本该移动」的行数；移开后提示「N 行已移动」
    pending_moves: usize,
    moved_note: Option<(usize, std::time::Instant)>,
    /// 上一次数据里每行（数据下标）的键：悬停期间按键保持原有行序
    prev_keys: Vec<String>,
    /// 列的显示顺序（列下标；空 = 原始顺序）与隐藏的列（列管理器）
    pub col_order: Vec<usize>,
    pub hidden: std::collections::BTreeSet<usize>,
    /// 过滤条件（全部满足才显示）
    pub filters: Vec<Filter>,
    /// 展开着的设置区
    pub panel: Option<GridPanel>,
    /// 有固定列时，非固定列从第几个开始显示（按整列翻页）
    pub col_start: usize,
    /// 右键菜单：哪一行、弹在哪（相对网格左上角）
    menu: Option<(usize, Point)>,
    /// 光标位置（相对网格），右键菜单定位用
    cursor: Point,
    /// 待办的复制（等下一次带数据的 resort 生成 TSV）
    copy_req: Option<CopyReq>,
}

/// 变化高亮持续多久（UPDS V5 §30）。
const FLASH: std::time::Duration = std::time::Duration::from_millis(120);

fn key_of(cols: &[Column], row: &[Cell]) -> Option<String> {
    let k = cols.iter().position(|c| c.key)?;
    row.get(k).map(Cell::plain)
}

impl GridState {
    pub fn new(cols: &[Column]) -> Self {
        Self {
            offset: 0.0,
            sort: None,
            widths: cols.iter().map(|c| c.width).collect(),
            selected: None,
            extra: Default::default(),
            order: Vec::new(),
            id: iced::widget::Id::unique(),
            group_by: None,
            collapsed: HashSet::new(),
            items: Vec::new(),
            drag: None,
            prev: Default::default(),
            flashed: Default::default(),
            hovering: false,
            pending_moves: 0,
            moved_note: None,
            prev_keys: Vec::new(),
            col_order: Vec::new(),
            hidden: Default::default(),
            filters: Vec::new(),
            panel: None,
            col_start: 0,
            menu: None,
            cursor: Point::ORIGIN,
            copy_req: None,
        }
    }

    /// 选中行（或指定一行）的 TSV：表头 + 行，按当前可见列。
    fn rows_tsv(&self, cols: &[Column], rows: &[Vec<Cell>], which: &[usize]) -> String {
        let vis = self.ordered_cols(cols.len());
        let mut out = vis.iter().map(|&i| cols[i].title.clone()).collect::<Vec<_>>().join("\t");
        for &r in which {
            if let Some(row) = rows.get(r) {
                out.push('\n');
                out.push_str(&vis.iter().map(|&i| row.get(i).map_or_else(String::new, Cell::plain)).collect::<Vec<_>>().join("\t"));
            }
        }
        out
    }

    /// 列的显示顺序（列下标），不含隐藏的。
    pub fn ordered_cols(&self, n: usize) -> Vec<usize> {
        let base: Vec<usize> = if self.col_order.len() == n { self.col_order.clone() } else { (0..n).collect() };
        base.into_iter().filter(|i| !self.hidden.contains(i)).collect()
    }

    /// 实际画出来的列：固定列在前，其余从 `col_start` 起（没有固定列时就是全部）。
    pub fn visible_cols(&self, cols: &[Column]) -> Vec<usize> {
        let ord = self.ordered_cols(cols.len());
        let (pinned, rest): (Vec<usize>, Vec<usize>) = ord.into_iter().partition(|&i| cols[i].pinned);
        if pinned.is_empty() {
            return rest;
        }
        let start = self.col_start.min(rest.len().saturating_sub(1));
        pinned.into_iter().chain(rest.into_iter().skip(start)).collect()
    }

    fn filters_active(&self) -> bool {
        self.filters.iter().any(|f| !f.value.trim().is_empty())
    }

    /// 这一格是不是刚变过（120ms 内）。
    fn flashing(&self, key: &str, col: usize) -> bool {
        self.flashed.get(&(key.to_string(), col)).is_some_and(|t| t.elapsed() < FLASH)
    }

    /// 比出变了的格；更新比较基准。空数据（只处理消息、没给数据的调用）不动基准。
    fn diff_cells(&mut self, cols: &[Column], rows: &[Vec<Cell>]) {
        if rows.is_empty() || !cols.iter().any(|c| c.key) {
            return;
        }
        let now = std::time::Instant::now();
        self.flashed.retain(|_, t| now.duration_since(*t) < FLASH);
        let mut next = std::collections::HashMap::with_capacity(rows.len());
        for row in rows {
            let Some(k) = key_of(cols, row) else { continue };
            let cells: Vec<String> = row.iter().map(Cell::plain).collect();
            if let Some(old) = self.prev.get(&k) {
                for (ci, (a, b)) in old.iter().zip(cells.iter()).enumerate() {
                    if a != b {
                        self.flashed.insert((k.clone(), ci), now);
                    }
                }
            }
            next.insert(k, cells);
        }
        self.prev = next;
    }

    /// 全部选中的数据行。
    pub fn selection(&self) -> Vec<usize> {
        self.selected.into_iter().chain(self.extra.iter().copied()).collect()
    }

    fn is_selected(&self, di: usize) -> bool {
        self.selected == Some(di) || self.extra.contains(&di)
    }

    /// 选中行的计数与合计（状态栏用，docs/35 §5.1 / §7.1：合计必须说明缺口）。
    /// 合计列 = 当前排序列（若是数值列），否则第一个数值列；没有数值列只给计数。
    pub fn selection_summary(&self, cols: &[Column], rows: &[Vec<Cell>]) -> Option<SelSummary> {
        let sel = self.selection();
        if sel.is_empty() {
            return None;
        }
        let col = self
            .sort
            .map(|(c, _)| c)
            .filter(|c| cols.get(*c).is_some_and(|x| x.sort == SortKind::Number))
            .or_else(|| cols.iter().position(|c| c.sort == SortKind::Number));
        let sum = col.map(|ci| {
            let vals: Vec<Option<f64>> = sel.iter().map(|&r| rows.get(r).and_then(|row| row.get(ci)).and_then(Cell::sort_num)).collect();
            let missing = vals.iter().filter(|v| v.is_none()).count();
            (cols[ci].title.clone(), vals.iter().flatten().sum::<f64>(), missing)
        });
        Some(SelSummary { count: sel.len(), sum })
    }

    /// 显示位置的个数（分组时含组头、不含折叠掉的行）。
    pub fn len(&self, rows: usize) -> usize {
        if self.group_by.is_some() {
            self.items.len()
        } else if self.filters_active() {
            self.order.len()
        } else {
            rows
        }
    }

    /// 第 `pos` 个显示位置画什么。
    fn item(&self, pos: usize) -> Item {
        if self.group_by.is_some() {
            self.items[pos].clone()
        } else if self.order.len() > pos {
            Item::Row(self.order[pos])
        } else {
            Item::Row(pos)
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
        // 待办的复制：有数据时生成 TSV 交给主循环写剪贴板
        if !rows.is_empty()
            && let Some(req) = self.copy_req.take()
        {
            let which = match req {
                CopyReq::Row(r) => vec![r],
                CopyReq::Selected => self.selection(),
            };
            if let Ok(mut g) = CLIP.lock() {
                *g = Some(self.rows_tsv(cols, rows, &which));
            }
        }
        self.diff_cells(cols, rows);
        let held: Option<Vec<String>> = (self.hovering && !rows.is_empty() && !self.order.is_empty())
            .then(|| self.order.iter().filter_map(|&i| self.prev_keys.get(i).cloned()).collect());
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
        // 光标在行区里：不在眼前重排（UPDS V5 §30）。已有的行按原来的先后，新行按排序接在后面；
        // 移开后再按排序排好，并提示「N 行已移动」
        if let Some(held) = held.filter(|_| cols.iter().any(|c| c.key)) {
            let keys_now: Vec<String> = rows.iter().map(|r| key_of(cols, r).unwrap_or_default()).collect();
            let pos: std::collections::HashMap<&str, usize> =
                keys_now.iter().enumerate().map(|(i, k)| (k.as_str(), i)).collect();
            let mut kept: Vec<usize> = held.iter().filter_map(|k| pos.get(k.as_str()).copied()).collect();
            let seen: std::collections::HashSet<usize> = kept.iter().copied().collect();
            kept.extend(order.iter().copied().filter(|i| !seen.contains(i)));
            self.pending_moves = kept.iter().zip(order.iter()).filter(|(a, b)| a != b).count();
            order = kept;
        }
        if !rows.is_empty() {
            self.prev_keys = rows.iter().map(|r| key_of(cols, r).unwrap_or_default()).collect();
        }
        // 过滤器（全部条件都满足才留下）
        if self.filters_active() {
            order.retain(|&i| self.filters.iter().all(|f| f.keeps(&rows[i])));
        }
        self.order = order;
        self.items.clear();
        if let Some(gc) = self.group_by.filter(|c| *c < cols.len()) {
            // 组的先后 = 组内第一行在排序结果里的位置；组内保持排序结果的相对顺序
            let mut keys: Vec<String> = Vec::new();
            let mut members: std::collections::HashMap<String, Vec<usize>> = std::collections::HashMap::new();
            for &i in &self.order {
                let k = rows[i].get(gc).map_or_else(|| "（空）".to_string(), Cell::group_key);
                members.entry(k.clone()).or_insert_with(|| {
                    keys.push(k);
                    Vec::new()
                }).push(i);
            }
            for k in keys {
                let m = &members[&k];
                let collapsed = self.collapsed.contains(&k);
                self.items.push(Item::Group { key: k, count: m.len(), collapsed });
                if !collapsed {
                    self.items.extend(m.iter().map(|&i| Item::Row(i)));
                }
            }
        } else {
            self.group_by = None;
        }
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
            GridMsg::Select(i) => {
                if super::ctrl_held() {
                    // Ctrl+点击：加选 / 取消这一行，不动其他选中
                    if self.selected == Some(i) {
                        self.selected = self.extra.pop_first();
                    } else if !self.extra.remove(&i) {
                        match self.selected {
                            None => self.selected = Some(i),
                            Some(_) => {
                                self.extra.insert(i);
                            }
                        }
                    }
                } else {
                    self.extra.clear();
                    self.selected = if self.selected == Some(i) { None } else { Some(i) };
                }
            }
            GridMsg::DragStart(i) => {
                if let Some(w) = self.widths.get(i) {
                    self.drag = Some((i, None, *w));
                }
            }
            GridMsg::RowMenu(i) => self.menu = Some((i, self.cursor)),
            GridMsg::CloseMenu => self.menu = None,
            GridMsg::CopyRow(i) => {
                self.copy_req = Some(CopyReq::Row(i));
                self.menu = None;
                self.resort(cols, rows);
            }
            GridMsg::CopySelected => {
                self.copy_req = Some(CopyReq::Selected);
                self.menu = None;
                self.resort(cols, rows);
            }
            GridMsg::SelectAll => {
                let all: Vec<usize> = self.order.clone();
                self.selected = all.first().copied();
                self.extra = all.into_iter().skip(1).collect();
                self.menu = None;
            }
            GridMsg::ClearSelection => {
                self.selected = None;
                self.extra.clear();
                self.menu = None;
            }
            GridMsg::DragMove(p) => {
                self.cursor = p;
                if let Some((i, start, w0)) = self.drag.as_mut() {
                    let s = *start.get_or_insert(p.x);
                    let w = (*w0 + p.x - s).max(40.0);
                    if let Some(slot) = self.widths.get_mut(*i) {
                        *slot = w;
                    }
                }
            }
            GridMsg::DragEnd => self.drag = None,
            GridMsg::CycleGroup => {
                let groupable: Vec<usize> = cols.iter().enumerate().filter(|(_, c)| c.group).map(|(i, _)| i).collect();
                self.group_by = match self.group_by.and_then(|g| groupable.iter().position(|&c| c == g)) {
                    None => groupable.first().copied(),
                    Some(p) => groupable.get(p + 1).copied(),
                };
                self.collapsed.clear();
                self.resort(cols, rows);
            }
            GridMsg::Hover(h) => {
                if self.hovering && !h && self.pending_moves > 0 {
                    self.moved_note = Some((self.pending_moves, std::time::Instant::now()));
                    self.pending_moves = 0;
                }
                self.hovering = h;
                // 不在这里重排：下一次 resort（有数据时）自然按排序排
            }
            GridMsg::Panel(pn) => self.panel = if self.panel == Some(pn) { None } else { Some(pn) },
            GridMsg::ToggleCol(i) => {
                // 至少留一列
                if !self.hidden.remove(&i) && self.hidden.len() + 1 < cols.len() {
                    self.hidden.insert(i);
                }
            }
            GridMsg::MoveCol(i, dir) => {
                if self.col_order.len() != cols.len() {
                    self.col_order = (0..cols.len()).collect();
                }
                if let Some(p) = self.col_order.iter().position(|&c| c == i) {
                    let q = p as i64 + i64::from(dir);
                    if (0..cols.len() as i64).contains(&q) {
                        self.col_order.swap(p, q as usize);
                    }
                }
            }
            GridMsg::ResetCols => {
                self.col_order.clear();
                self.hidden.clear();
                self.col_start = 0;
            }
            GridMsg::AddFilter => {
                let col = cols.iter().position(|c| c.sort == SortKind::Number).unwrap_or(0);
                let op = if cols.get(col).is_some_and(|c| c.sort == SortKind::Number) { FilterOp::Gt } else { FilterOp::Contains };
                self.filters.push(Filter { col, op, value: String::new() });
                self.panel = Some(GridPanel::Filters);
            }
            GridMsg::FilterCol(k, c) => {
                if let Some(f) = self.filters.get_mut(k) {
                    f.col = c;
                }
                self.resort(cols, rows);
            }
            GridMsg::FilterOp(k, op) => {
                if let Some(f) = self.filters.get_mut(k) {
                    f.op = op;
                }
                self.resort(cols, rows);
            }
            GridMsg::FilterVal(k, v) => {
                if let Some(f) = self.filters.get_mut(k) {
                    f.value = v;
                }
                self.resort(cols, rows);
            }
            GridMsg::RemoveFilter(k) => {
                if k < self.filters.len() {
                    self.filters.remove(k);
                }
                self.resort(cols, rows);
            }
            GridMsg::ClearFilters => {
                self.filters.clear();
                self.resort(cols, rows);
            }
            GridMsg::ColPage(d) => {
                let rest = self.ordered_cols(cols.len()).into_iter().filter(|&i| !cols[i].pinned).count();
                let next = self.col_start as i64 + i64::from(d);
                self.col_start = next.clamp(0, rest.saturating_sub(1) as i64) as usize;
            }
            GridMsg::ToggleGroup(k) => {
                if !self.collapsed.remove(&k) {
                    self.collapsed.insert(k);
                }
                self.resort(cols, rows);
            }
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

    fn group_summary(&self, cols: &[Column]) -> String {
        match self.group_by.and_then(|i| cols.get(i)) {
            Some(c) => format!("分组：{} ▸", c.title),
            None => "分组：无 ▸".into(),
        }
    }
}

/// 数据行的底色：选中 > 悬停由按钮状态给 > 斑马纹。别的自绘表格（特征矩阵）也用它，保证全仓一致。
pub fn row_tint(pos: usize, highlighted: bool) -> Option<Color> {
    let c = core();
    if highlighted {
        Some(color(c.accent_soft))
    } else if pos % 2 == 1 {
        Some(Color { a: 0.025, ..color(c.text_primary) })
    } else {
        None
    }
}

/// 组头条（横贯整表）：底色、内边距、字重一处定义。特征矩阵的阶段分节条也用它。
pub fn group_band<'a, M: 'a>(content: impl Into<Element<'a, M>>, width: f32) -> container::Container<'a, M> {
    container(content)
        .width(Length::Fixed(width))
        .height(Length::Fixed(metrics::row_height()))
        .align_y(Alignment::Center)
        .padding(Padding::from([0.0, metrics::space(2)]))
        .style(|_| container::Style { background: Some(Background::Color(color(core().surface_secondary))), ..Default::default() })
}

/// 组头的标题：`▾ 键 · n 行`（折叠时 `▸`）。
pub fn group_title(key: &str, count: usize, collapsed: bool) -> String {
    format!("{} {key} · {count} 行", if collapsed { "▸" } else { "▾" })
}

fn cell_view<'a, M: 'a>(cell: &Cell, align: Align, w: f32) -> Element<'a, M> {
    let body: Element<'a, M> = match cell {
        Cell::Text(s) => t::body(s.clone()).into(),
        Cell::Id(s) => {
            // 按列宽估一个字符数（等宽字体约 0.6em）
            let max = ((w - 8.0) / (t::size(super::Role::Code) * 0.6)).max(6.0) as usize;
            t::code(fmt::elide_middle(s, max)).into()
        }
        Cell::Num { s, prov, .. } => widgets::value(s.clone(), *prov).into(),
        Cell::Absent(a) => widgets::absent(*a).into(),
        Cell::Badge(s, tone) => widgets::badge(s.clone(), *tone),
        Cell::Colored(s, fg) => t::numeric(s.clone()).color(*fg).into(),
    };
    container(body)
        .width(Length::Fixed(w))
        .padding(Padding::from([0.0, metrics::space(2)]))
        .align_x(if align == Align::Right { Alignment::End } else { Alignment::Start })
        .align_y(Alignment::Center)
        .height(Length::Fill)
        .clip(true)
        .into()
}

/// 画网格。`on` 把网格消息包成面板自己的消息。`footer` 是页脚右侧的附加说明（合计等）。
///
/// 数据可借可交（见模块说明）：`&[Column]`、`Vec<Column>`、`Rc<[Column]>` 都行，行与状态同理。
pub fn view<'a, M, C, R, S>(cols: C, rows: R, st: S, footer: Option<String>, on: impl Fn(GridMsg) -> M + Clone + 'a) -> Element<'a, M>
where
    M: Clone + 'a,
    C: AsRef<[Column]>,
    R: AsRef<[Vec<Cell>]> + 'a,
    S: Borrow<GridState> + 'a,
{
    let cols = cols.as_ref();
    let c = core();
    let row_h = metrics::row_height();
    let widths: Vec<f32> =
        cols.iter().enumerate().map(|(i, col)| st.borrow().widths.get(i).copied().unwrap_or(col.width)).collect();
    let aligns: Vec<Align> = cols.iter().map(|c| c.align).collect();
    let key_col = cols.iter().position(|c| c.key);
    // 列管理器隐藏的、固定列翻页翻过去的不画
    let vis: Vec<usize> = st.borrow().visible_cols(cols);
    let total_w: f32 = vis.iter().map(|&i| widths[i]).sum::<f32>() + 3.0;

    // ── 表头：点标题排序，标题右侧的竖线可拖动调宽 ──
    let mut head = row![Space::new().width(Length::Fixed(3.0))].height(Length::Fixed(metrics::panel_header()));
    for &i in &vis {
        let col = &cols[i];
        let arrow = match st.borrow().sort {
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
    let n = rows.as_ref().len();
    let shown = st.borrow().len(n);
    // 页脚要的东西先取出来：状态随后整个移进行区的闭包
    let mut foot = row![t::caption(format!("{n} 行")), t::caption(st.borrow().sort_summary(cols))].spacing(metrics::space(4));
    if cols.iter().any(|c| c.group) {
        let on_g = on.clone();
        foot = foot.push(
            button(t::caption(st.borrow().group_summary(cols)))
                .padding(Padding::from([0.0, metrics::space(1)]))
                .on_press(on_g(GridMsg::CycleGroup))
                .style(|th, s| widgets::button_style(widgets::Kind::Ghost, th, s)),
        );
    }
    {
        let b = st.borrow();
        let ghost = |label: String, msg: GridMsg, on: &dyn Fn(GridMsg) -> M| {
            button(t::caption(label))
                .padding(Padding::from([0.0, metrics::space(1)]))
                .on_press(on(msg))
                .style(|th, s| widgets::button_style(widgets::Kind::Ghost, th, s))
        };
        let n_hidden = b.hidden.len();
        foot = foot.push(ghost(
            if n_hidden > 0 { format!("列 ▸（隐藏 {n_hidden}）") } else { "列 ▸".into() },
            GridMsg::Panel(GridPanel::Columns),
            &on,
        ));
        let n_f = b.filters.iter().filter(|f| !f.value.trim().is_empty()).count();
        foot = foot.push(ghost(
            if n_f > 0 { format!("筛选 ▸（{n_f} 条，剩 {} 行）", b.order.len()) } else { "筛选 ▸".into() },
            GridMsg::Panel(GridPanel::Filters),
            &on,
        ));
        if cols.iter().any(|c| c.pinned) {
            foot = foot.push(ghost("◀".into(), GridMsg::ColPage(-1), &on)).push(ghost("▶".into(), GridMsg::ColPage(1), &on));
        }
    }
    if let Some((n, t0)) = st.borrow().moved_note
        && t0.elapsed().as_secs() < 3
    {
        foot = foot.push(t::caption(format!("{n} 行已移动")).color(color(c.status_info)));
    }
    if st.borrow().pending_moves > 0 {
        foot = foot.push(t::caption("光标在表上，暂不重排"));
    }
    let summary = st.borrow().selection_summary(cols, rows.as_ref());
    if let Some(s) = &summary {
        foot = foot.push(t::caption(s.label()));
    }
    publish_selection(summary);
    if let Some(f) = footer {
        foot = foot.push(container(t::caption(f)).width(Length::Fill).align_x(Alignment::End));
    }
    let dragging = st.borrow().drag.is_some();
    let scroll_id = st.borrow().id.clone();
    let settings = settings_panel(cols, st.borrow(), &on);
    // 右键菜单要的东西也先取出来（状态随后移进行区的闭包）
    let menu_at = st.borrow().menu;
    let n_sel = st.borrow().selection().len();

    let on_r = on.clone();
    let body = responsive(move |size| {
        let st: &GridState = st.borrow();
        let rows = rows.as_ref();
        let t0 = std::time::Instant::now();
        let visible = (size.height / row_h).ceil() as usize + 2;
        let first = ((st.offset / row_h).floor() as usize).min(shown.saturating_sub(1));
        let last = (first + visible).min(shown);
        let mut col = column![Space::new().height(Length::Fixed(first as f32 * row_h))];
        for pos in first..last {
            let di = match st.item(pos) {
                Item::Row(di) => di,
                Item::Group { key, count, collapsed } => {
                    let on_t = on_r.clone();
                    col = col.push(
                        button(group_band(t::label(group_title(&key, count, collapsed)), total_w))
                            .padding(0)
                            .on_press(on_t(GridMsg::ToggleGroup(key)))
                            .style(|th, s| {
                                let mut b = widgets::button_style(widgets::Kind::Ghost, th, s);
                                b.text_color = color(core().text_primary);
                                b.border = Border::default();
                                b
                            }),
                    );
                    continue;
                }
            };
            let selected = st.is_selected(di);
            let mut r = row![container(Space::new().width(Length::Fixed(2.0)).height(Length::Fill)).style(move |_| container::Style {
                background: selected.then(|| Background::Color(color(core().accent_primary))),
                ..Default::default()
            })]
            .spacing(1)
            .height(Length::Fixed(row_h));
            let rkey = key_col.and_then(|k| rows[di].get(k)).map(Cell::plain);
            for &ci in &vis {
                let align = aligns[ci];
                let cell = cell_view(rows[di].get(ci).unwrap_or(&ABSENT), align, widths[ci]);
                // 变化的格闪一下（120ms，UPDS V5 §30）
                if rkey.as_deref().is_some_and(|k| st.flashing(k, ci)) {
                    r = r.push(container(cell).style(|_| container::Style {
                        background: Some(Background::Color(color(core().accent_soft))),
                        ..Default::default()
                    }));
                } else {
                    r = r.push(cell);
                }
            }
            let on_sel = on_r.clone();
            let stripe = pos % 2 == 1;
            let on_menu = on_r.clone();
            col = col.push(
                mouse_area(
                    button(r)
                        .padding(0)
                        .width(Length::Fixed(total_w))
                        .on_press(on_sel(GridMsg::Select(di)))
                        .style(move |th, s| {
                            let mut b = widgets::button_style(widgets::Kind::Ghost, th, s);
                            let c = core();
                            b.text_color = color(c.text_primary);
                            if selected || b.background.is_none() {
                                b.background = row_tint(if stripe { 1 } else { 0 }, selected).map(Background::Color);
                            }
                            b.border = Border::default();
                            b
                        }),
                )
                // 右键：在光标处弹菜单（UPDS V2 §12）
                .on_right_press(on_menu(GridMsg::RowMenu(di))),
            );
        }
        col = col.push(Space::new().height(Length::Fixed((shown - last) as f32 * row_h)));
        LAST_BUILD_US.store(t0.elapsed().as_micros() as u64, std::sync::atomic::Ordering::Relaxed);
        let on_s = on_r.clone();
        scrollable(col)
            .id(scroll_id.clone())
            .on_scroll(move |v| on_s(GridMsg::Scrolled(v)))
            .height(Length::Fill)
            .width(Length::Fixed(total_w + 12.0))
            .into()
    });

    // 页脚（行数 · 排序 · 分组 · 选中 · 附加说明）在上面已拼好——用说明文字而不是元数据角色：
    // 元数据会转大写，单位 µs 会变成 MS（批 4 踩过）

    // 行区的进出：在里面时不重排（GridMsg::Hover）
    let (on_in, on_out) = (on.clone(), on.clone());
    let body = mouse_area(body).on_enter(on_in(GridMsg::Hover(true))).on_exit(on_out(GridMsg::Hover(false)));
    let table = column![head, body].width(Length::Fixed(total_w + 12.0)).height(Length::Fill);
    let on_m = on.clone();
    let on_e = on.clone();
    let table = mouse_area(scrollable(table).direction(scrollable::Direction::Horizontal(scrollable::Scrollbar::new())).height(Length::Fill))
        .on_move(move |p| on_m(GridMsg::DragMove(p)))
        .on_release(on_e(GridMsg::DragEnd));
    let table: Element<'a, M> = if dragging {
        table.interaction(iced::mouse::Interaction::ResizingHorizontally).into()
    } else {
        table.into()
    };
    // 右键菜单：叠在表上、左上角对准光标；点菜单外面收起（UPDS V2 §12：最多 9 项）
    let table: Element<'a, M> = match menu_at {
        Some((di, at)) => {
            let item = |label: &str, msg: GridMsg| -> Element<'a, M> {
                button(t::body(label.to_string()))
                    .width(Length::Fill)
                    .padding(Padding::from([metrics::space(1), metrics::space(3)]))
                    .on_press(on(msg))
                    .style(|th, s| widgets::button_style(widgets::Kind::Ghost, th, s))
                    .into()
            };
            let menu = container(
                column![
                    item("复制这一行（TSV）", GridMsg::CopyRow(di)),
                    item(&format!("复制选中的 {n_sel} 行（TSV）"), GridMsg::CopySelected),
                    item("全选（当前筛选结果）", GridMsg::SelectAll),
                    item("取消选中", GridMsg::ClearSelection),
                ]
                .width(Length::Fixed(220.0)),
            )
            .padding(metrics::space(1))
            .style(|_| {
                let c = core();
                container::Style {
                    background: Some(Background::Color(color(c.surface_elevated))),
                    border: Border { width: 1.0, color: color(c.border_default), radius: metrics::radius::MD.into() },
                    ..Default::default()
                }
            });
            let on_close = on.clone();
            iced::widget::stack![
                table,
                mouse_area(container(Space::new()).width(Length::Fill).height(Length::Fill)).on_press(on_close(GridMsg::CloseMenu)),
                container(menu).padding(Padding { top: at.y, left: at.x, right: 0.0, bottom: 0.0 }),
            ]
            .into()
        }
        None => table,
    };

    column![
        settings.unwrap_or_else(|| Space::new().into()),
        container(table).height(Length::Fill).style(move |_| container::Style {
            background: Some(Background::Color(color(c.surface_primary))),
            ..Default::default()
        }),
        container(foot.width(Length::Fill).align_y(Alignment::Center)).padding(Padding::from([2.0, metrics::space(3)])).width(Length::Fill),
    ]
    // 外层撑满面板宽度：页脚里「撑满」的空白要有确定的父宽才排得开（批 4 踩过，右侧说明被挤没）
    .width(Length::Fill)
    .into()
}

/// 列管理器 / 过滤器构建器（网格上方展开的设置区，docs/35 §6.1）。
fn settings_panel<'a, M: Clone + 'a>(cols: &[Column], st: &GridState, on: &(impl Fn(GridMsg) -> M + Clone + 'a)) -> Option<Element<'a, M>> {
    let ghost = |label: &str, msg: GridMsg| -> Element<'a, M> {
        button(t::caption(label.to_string()))
            .padding(Padding::from([0.0, metrics::space(1)]))
            .on_press(on(msg))
            .style(|th, s| widgets::button_style(widgets::Kind::Ghost, th, s))
            .into()
    };
    let body: Element<'a, M> = match st.panel? {
        GridPanel::Columns => {
            let order: Vec<usize> = if st.col_order.len() == cols.len() { st.col_order.clone() } else { (0..cols.len()).collect() };
            let mut list = column![].spacing(1);
            for i in order {
                let on_t = on.clone();
                let title = format!("{}{}", cols[i].title, if cols[i].pinned { "（固定）" } else { "" });
                list = list.push(
                    row![
                        iced::widget::checkbox(!st.hidden.contains(&i)).label(title).on_toggle(move |_| on_t(GridMsg::ToggleCol(i))).size(t::size(super::Role::Caption)),
                        Space::new().width(Length::Fill),
                        ghost("↑", GridMsg::MoveCol(i, -1)),
                        ghost("↓", GridMsg::MoveCol(i, 1)),
                    ]
                    .align_y(Alignment::Center),
                );
            }
            column![
                row![t::label("列"), Space::new().width(Length::Fill), ghost("全部恢复", GridMsg::ResetCols), ghost("✕", GridMsg::Panel(GridPanel::Columns))]
                    .align_y(Alignment::Center),
                scrollable(list).height(Length::Fixed(180.0)),
            ]
            .spacing(metrics::space(1))
            .into()
        }
        GridPanel::Filters => {
            let titles: Vec<String> = cols.iter().map(|c| c.title.clone()).collect();
            let mut list = column![].spacing(metrics::space(1));
            for (k, f) in st.filters.iter().enumerate() {
                let (on_c, on_o, on_v) = (on.clone(), on.clone(), on.clone());
                let ts = titles.clone();
                list = list.push(
                    row![
                        iced::widget::pick_list(titles.clone(), titles.get(f.col).cloned(), move |t: String| {
                            on_c(GridMsg::FilterCol(k, ts.iter().position(|x| *x == t).unwrap_or(0)))
                        })
                        .text_size(t::size(super::Role::Caption)),
                        iced::widget::pick_list(FilterOp::ALL, Some(f.op), move |o| on_o(GridMsg::FilterOp(k, o)))
                            .text_size(t::size(super::Role::Caption)),
                        iced::widget::text_input("值", &f.value)
                            .on_input(move |v| on_v(GridMsg::FilterVal(k, v)))
                            .size(t::size(super::Role::Caption))
                            .width(Length::Fixed(120.0)),
                        ghost("✕", GridMsg::RemoveFilter(k)),
                    ]
                    .spacing(metrics::space(1))
                    .align_y(Alignment::Center),
                );
            }
            column![
                row![
                    t::label("筛选（全部满足才显示；数值列缺失值不满足任何数值条件）"),
                    Space::new().width(Length::Fill),
                    ghost("＋ 条件", GridMsg::AddFilter),
                    ghost("全清", GridMsg::ClearFilters),
                    ghost("✕", GridMsg::Panel(GridPanel::Filters)),
                ]
                .align_y(Alignment::Center),
                list,
            ]
            .spacing(metrics::space(1))
            .into()
        }
    };
    Some(
        container(body)
            .padding(Padding::from([metrics::space(2), metrics::space(3)]))
            .width(Length::Fill)
            .style(|_| container::Style { background: Some(Background::Color(color(core().surface_secondary))), ..Default::default() })
            .into(),
    )
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
    fn 分组_组序随排序_组内保序_折叠不丢() {
        let cols = vec![Column::text("向", 40.0).groupable(), Column::num("净", None, 80.0)];
        let rows = vec![
            vec![Cell::Text("买".into()), Cell::num(1.0, "1".into())],
            vec![Cell::Text("卖".into()), Cell::num(5.0, "5".into())],
            vec![Cell::Text("买".into()), Cell::num(3.0, "3".into())],
            vec![Cell::Absent(Absence::Missing), Cell::num(0.0, "0".into())],
        ];
        let mut st = GridState::new(&cols);
        st.update(GridMsg::Sort(1), &cols, &rows); // 净 降序：1 2 0 3
        st.update(GridMsg::CycleGroup, &cols, &rows);
        assert_eq!(st.group_by, Some(0));
        let g = |k: &str, n| Item::Group { key: k.into(), count: n, collapsed: false };
        assert_eq!(st.items, [g("卖", 1), Item::Row(1), g("买", 2), Item::Row(2), Item::Row(0), g("（空）", 1), Item::Row(3)]);
        assert_eq!(st.len(rows.len()), 7);

        st.update(GridMsg::ToggleGroup("买".into()), &cols, &rows);
        assert_eq!(st.items[2], Item::Group { key: "买".into(), count: 2, collapsed: true });
        assert_eq!(st.len(rows.len()), 5, "折叠的组只剩组头");
        st.resort(&cols, &rows); // 数据刷新
        assert_eq!(st.len(rows.len()), 5, "刷新后折叠不丢");

        st.update(GridMsg::CycleGroup, &cols, &rows);
        assert_eq!(st.group_by, None, "只有一个可分组列：再点回到不分组");
        assert_eq!(st.len(rows.len()), 4);
    }

    #[test]
    fn 带来源前缀的数照样按数值排() {
        let cols = vec![Column::num("净", None, 80.0)];
        let rows = vec![
            vec![Cell::Colored("~ +2.5".into(), Color::WHITE)],
            vec![Cell::Colored("~ -10".into(), Color::WHITE)],
            vec![Cell::Colored("~ +30".into(), Color::WHITE)],
        ];
        let mut st = GridState::new(&cols);
        st.update(GridMsg::Sort(0), &cols, &rows);
        assert_eq!(st.order, [2, 0, 1], "降序：30 > 2.5 > -10");
    }

    #[test]
    fn 数据可交出所有权() {
        // 旁路快照每帧现拼，借不出 'a——网格要能直接吃 Vec / Rc
        let (cols, rows) = data();
        let mut st = GridState::new(&cols);
        st.resort(&cols, &rows);
        let _: Element<'static, GridMsg> = view(cols, rows, std::rc::Rc::new(st), None, |m| m);
    }

    #[test]
    fn 选中合计_说明缺口() {
        let (cols, rows) = data(); // 实际列：3.0 / 缺失 / 7.0
        let mut st = GridState::new(&cols);
        st.selected = Some(0);
        st.extra.insert(1);
        st.extra.insert(2);
        let s = st.selection_summary(&cols, &rows).expect("有选中");
        assert_eq!(s.count, 3);
        assert_eq!(s.sum, Some(("实际".into(), 10.0, 1)));
        assert!(s.label().contains("3 个中 1 个缺失"), "{}", s.label());
        st.update(GridMsg::Select(0), &cols, &rows); // 没按 Ctrl：回到单选（再点一次 = 取消）
        assert!(st.extra.is_empty());
    }

    #[test]
    fn 变化的格按键认行_插在最上面不整表闪() {
        let cols = vec![Column::text("号", 40.0).key(), Column::num("值", None, 60.0)];
        let mut st = GridState::new(&cols);
        let r1 = vec![vec![Cell::Text("a".into()), Cell::num(1.0, "1".into())]];
        st.resort(&cols, &r1);
        // 新行插在最上面（下标平移），a 的值也变了
        let r2 = vec![
            vec![Cell::Text("b".into()), Cell::num(9.0, "9".into())],
            vec![Cell::Text("a".into()), Cell::num(2.0, "2".into())],
        ];
        st.resort(&cols, &r2);
        assert!(st.flashing("a", 1), "a 的值变了要闪");
        assert!(!st.flashing("a", 0), "a 的键没变");
        assert!(!st.flashing("b", 1), "新行不算变化");
        st.resort(&cols, &[]); // 只处理消息、没给数据：不动基准
        st.resort(&cols, &r2);
        assert!(!st.prev.is_empty());
    }

    #[test]
    fn 光标在表上不重排_移开后提示() {
        let cols = vec![Column::text("号", 40.0).key(), Column::num("值", None, 60.0)];
        let rows = |a: f64, b: f64| {
            vec![vec![Cell::Text("a".into()), Cell::num(a, a.to_string())], vec![Cell::Text("b".into()), Cell::num(b, b.to_string())]]
        };
        let mut st = GridState::new(&cols);
        st.update(GridMsg::Sort(1), &cols, &rows(1.0, 2.0)); // 降序：b a
        assert_eq!(st.order, [1, 0]);
        st.update(GridMsg::Hover(true), &cols, &[]);
        st.resort(&cols, &rows(5.0, 2.0)); // 本该变成 a b
        assert_eq!(st.order, [1, 0], "光标在表上：顺序不动");
        assert_eq!(st.pending_moves, 2);
        st.update(GridMsg::Hover(false), &cols, &[]);
        assert_eq!(st.moved_note.map(|(n, _)| n), Some(2), "移开后提示 2 行已移动");
        st.resort(&cols, &rows(5.0, 2.0));
        assert_eq!(st.order, [0, 1], "移开后按排序");
    }

    #[test]
    fn 过滤_数值与文字_缺失不满足数值条件() {
        let (cols, rows) = data(); // CC-10 3 / CC-2 缺失 / CC-1 7
        let mut st = GridState::new(&cols);
        st.update(GridMsg::AddFilter, &cols, &rows);
        assert_eq!(st.filters[0], Filter { col: 1, op: FilterOp::Gt, value: String::new() }, "缺省落在第一个数值列");
        st.update(GridMsg::FilterVal(0, "5".into()), &cols, &rows);
        assert_eq!(st.order, [2], "> 5 只剩 CC-1（缺失的那行也不留）");
        assert_eq!(st.len(rows.len()), 1);
        st.update(GridMsg::FilterCol(0, 0), &cols, &rows);
        st.update(GridMsg::FilterOp(0, FilterOp::Contains), &cols, &rows);
        st.update(GridMsg::FilterVal(0, "cc-1".into()), &cols, &rows);
        assert_eq!(st.order, [0, 2], "包含不分大小写：CC-10、CC-1");
        st.update(GridMsg::FilterVal(0, "abc".into()), &cols, &rows);
        st.update(GridMsg::FilterOp(0, FilterOp::Gt), &cols, &rows);
        assert_eq!(st.order.len(), 3, "数值条件填了解析不出的值：不生效、不误删");
        st.update(GridMsg::ClearFilters, &cols, &rows);
        assert_eq!(st.len(rows.len()), 3);
    }

    #[test]
    fn 列管理_隐藏_移动_至少留一列_固定列翻页() {
        let cols = vec![Column::text("代码", 80.0).pinned(), Column::num("a", None, 60.0), Column::num("b", None, 60.0), Column::num("c", None, 60.0)];
        let mut st = GridState::new(&cols);
        st.update(GridMsg::ToggleCol(2), &cols, &[]);
        assert_eq!(st.visible_cols(&cols), [0, 1, 3]);
        st.update(GridMsg::MoveCol(3, -1), &cols, &[]);
        assert_eq!(st.ordered_cols(4), [0, 1, 3], "隐藏的 2 不显示；3 前移到 2 的位置");
        st.update(GridMsg::ColPage(1), &cols, &[]);
        assert_eq!(st.visible_cols(&cols), [0, 3], "固定列不动，其余右翻一列");
        st.update(GridMsg::ColPage(9), &cols, &[]);
        assert_eq!(st.visible_cols(&cols), [0, 3], "翻到头为止，至少还有一列");
        for i in 0..4 {
            st.update(GridMsg::ToggleCol(i), &cols, &[]);
        }
        assert!(st.hidden.len() < 4, "至少留一列");
        st.update(GridMsg::ResetCols, &cols, &[]);
        assert_eq!(st.visible_cols(&cols), [0, 1, 2, 3]);
    }

    #[test]
    fn 带方向符号和单位的数照样能比() {
        for (s, v) in [("▲ +4.97%", 4.97), ("▼ −0.15%", -0.15), ("~ 1 024.50", 1024.5), ("+30", 30.0)] {
            assert_eq!(Cell::Colored(s.into(), Color::WHITE).sort_num(), Some(v), "{s}");
        }
        assert_eq!(Cell::Text("等待".into()).sort_num(), None);
    }

    #[test]
    fn 右键复制_拿不到数据时等下一次重排() {
        let (cols, rows) = data();
        let mut st = GridState::new(&cols);
        st.update(GridMsg::RowMenu(2), &cols, &[]);
        assert!(st.menu.is_some());
        st.update(GridMsg::CopyRow(2), &cols, &[]); // 订单表的 handle() 就是这样：没有数据
        assert!(st.menu.is_none());
        assert_eq!(take_clipboard(), None, "没有数据时还生成不了");
        st.resort(&cols, &rows); // 下一帧 view 带数据重排
        assert_eq!(take_clipboard().as_deref(), Some("代码\t实际\nCC-1\t7"));
        st.update(GridMsg::SelectAll, &cols, &rows);
        assert_eq!(st.selection().len(), 3);
        st.update(GridMsg::CopySelected, &cols, &rows);
        assert_eq!(take_clipboard().map(|t| t.lines().count()), Some(4), "表头 + 3 行");
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
