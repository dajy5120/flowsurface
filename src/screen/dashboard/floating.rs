//! 页面的浮动层（docs/41 §4.2，C 期）：叠在平铺层上的图表浮窗，多图同时显示时由用户自由组合。
//!
//! 每个浮窗是**只有一个面板的 pane_grid**，配一个合成的窗口 id（`window::Id::unique()`，不对应真窗口）。
//! Dashboard 按窗口 id 找面板的那几处（`get_pane` / `get_mut_pane` / `iter_all_panes`）把浮窗也算进去，
//! 于是面板标题栏、设置、数据流、tick 全部沿用现有机制——浮窗就是一个「不开系统窗口的弹出窗口」。
//!
//! 位置按内容区比例存（`data::layout::FloatRect`）；拖动 / 缩放时吸附到内容区边缘与其他浮窗的边，显示参考线。

use data::layout::FloatRect;
use iced::widget::pane_grid;
use iced::{Point, Size, window};

use super::pane;

/// 吸附距离（像素）。
pub const SNAP_PX: f32 = 8.0;
/// 浮窗最小尺寸（像素）。
pub const MIN_W: f32 = 260.0;
pub const MIN_H: f32 = 180.0;
/// 浮窗顶部的拖动条高度（像素）。
pub const GRIP_H: f32 = 22.0;

pub struct Float {
    pub id: window::Id,
    pub panes: pane_grid::State<pane::State>,
    pub rect: FloatRect,
}

impl Float {
    pub fn new(state: pane::State, rect: FloatRect) -> Self {
        Self { id: window::Id::unique(), panes: pane_grid::State::new(state).0, rect }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DragKind {
    Move,
    Resize,
}

#[derive(Debug, Clone, Copy)]
pub struct Drag {
    pub id: window::Id,
    pub kind: DragKind,
    /// 第一次移动时记下：光标相对浮窗左上角的偏移（像素）
    pub offset: Option<(f32, f32)>,
}

/// 拖动时的参考线：竖线（x）/ 横线（y），像素。
#[derive(Debug, Clone, Default)]
pub struct Guides {
    pub xs: Vec<f32>,
    pub ys: Vec<f32>,
}

#[derive(Debug, Clone)]
pub enum FloatMsg {
    /// 光标在浮窗拖动条上的位置（相对浮窗左上角）：按下时就是拖动偏移，拖起来不跳
    Hover(window::Id, Point),
    /// 按下浮窗的拖动条（`false`）或右下角缩放柄（`true`）
    Grab(window::Id, bool),
    /// 拖动中：光标位置与内容区尺寸（像素）
    Move(Point, Size),
    Release,
    /// 停靠回平铺层
    Dock(window::Id),
    /// 弹出成独立系统窗口
    PopOut(window::Id),
    Close(window::Id),
    /// 一键整理：按网格平铺排好全部浮窗
    Tidy,
    /// 收起 / 展开浮动层
    ToggleHidden,
    /// 新建一个空白浮窗（起始面板里选图表类型）
    NewBlank,
}

/// 新浮窗的位置：每多一个往右下错开一点，免得完全叠住。
pub fn cascade(n: usize) -> FloatRect {
    let k = (n % 8) as f32;
    FloatRect { x: 0.08 + 0.03 * k, y: 0.08 + 0.03 * k, w: 0.46, h: 0.46 }
}

/// 一键整理：n 个浮窗按网格铺满内容区（留 1% 边距）。
pub fn tidy(n: usize) -> Vec<FloatRect> {
    if n == 0 {
        return vec![];
    }
    let cols = (n as f32).sqrt().ceil() as usize;
    let rows = n.div_ceil(cols);
    let m = 0.01;
    let (cw, ch) = ((1.0 - m) / cols as f32, (1.0 - m) / rows as f32);
    (0..n)
        .map(|i| {
            let (c, r) = ((i % cols) as f32, (i / cols) as f32);
            FloatRect { x: m + c * cw, y: m + r * ch, w: cw - m, h: ch - m }
        })
        .collect()
}

fn to_px(r: FloatRect, size: Size) -> (f32, f32, f32, f32) {
    (r.x * size.width, r.y * size.height, r.w * size.width, r.h * size.height)
}

/// 拖动 / 缩放一步：返回新位置（比例）与参考线。`others` = 其他浮窗的位置（吸附对象）。
pub fn step(drag: &mut Drag, rect: FloatRect, p: Point, size: Size, others: &[FloatRect]) -> (FloatRect, Guides) {
    let (x, y, w, h) = to_px(rect, size);
    // 吸附候选：内容区边缘 + 其他浮窗的四条边
    let mut cx = vec![0.0, size.width];
    let mut cy = vec![0.0, size.height];
    for o in others {
        let (ox, oy, ow, oh) = to_px(*o, size);
        cx.extend([ox, ox + ow]);
        cy.extend([oy, oy + oh]);
    }
    let snap = |v: f32, cands: &[f32]| -> Option<f32> {
        cands.iter().copied().filter(|c| (c - v).abs() <= SNAP_PX).min_by(|a, b| (a - v).abs().total_cmp(&(b - v).abs()))
    };
    let mut g = Guides::default();
    let (nx, ny, nw, nh) = match drag.kind {
        DragKind::Move => {
            let (ox, oy) = *drag.offset.get_or_insert((p.x - x, p.y - y));
            let (mut nx, mut ny) = (p.x - ox, p.y - oy);
            // 左边或右边，哪个近吸哪个
            match (snap(nx, &cx), snap(nx + w, &cx)) {
                (Some(s), _) => {
                    nx = s;
                    g.xs.push(s);
                }
                (None, Some(s)) => {
                    nx = s - w;
                    g.xs.push(s);
                }
                _ => {}
            }
            match (snap(ny, &cy), snap(ny + h, &cy)) {
                (Some(s), _) => {
                    ny = s;
                    g.ys.push(s);
                }
                (None, Some(s)) => {
                    ny = s - h;
                    g.ys.push(s);
                }
                _ => {}
            }
            (nx, ny, w, h)
        }
        DragKind::Resize => {
            let (mut r, mut b) = (p.x, p.y);
            if let Some(s) = snap(r, &cx) {
                r = s;
                g.xs.push(s);
            }
            if let Some(s) = snap(b, &cy) {
                b = s;
                g.ys.push(s);
            }
            (x, y, (r - x).max(MIN_W), (b - y).max(MIN_H))
        }
    };
    let (sw, sh) = (size.width.max(1.0), size.height.max(1.0));
    let r = FloatRect { x: nx / sw, y: ny / sh, w: nw / sw, h: nh / sh }.clamped(MIN_W / sw, MIN_H / sh);
    (r, g)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SZ: Size = Size { width: 1000.0, height: 500.0 };

    #[test]
    fn tidy_fills_grid_without_overlap() {
        let r = tidy(3);
        assert_eq!(r.len(), 3);
        for (i, a) in r.iter().enumerate() {
            assert!(a.x >= 0.0 && a.y >= 0.0 && a.x + a.w <= 1.0 + 1e-6 && a.y + a.h <= 1.0 + 1e-6);
            for b in &r[i + 1..] {
                let overlap = a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;
                assert!(!overlap, "{a:?} {b:?}");
            }
        }
        assert!(tidy(0).is_empty());
    }

    #[test]
    fn move_snaps_to_edges_and_neighbours() {
        let start = FloatRect { x: 0.3, y: 0.3, w: 0.2, h: 0.2 };
        // 光标按在浮窗左上角右下 10px 处，往左拖到离左边缘 5px
        let mut d = Drag { id: window::Id::unique(), kind: DragKind::Move, offset: None };
        let _ = step(&mut d, start, Point::new(310.0, 160.0), SZ, &[]);
        let (r, g) = step(&mut d, start, Point::new(15.0, 160.0), SZ, &[]);
        assert!(r.x.abs() < 1e-6, "左边吸到 0：{r:?}");
        assert_eq!(g.xs, vec![0.0]);
        // 吸到邻居的右边
        let other = FloatRect { x: 0.0, y: 0.0, w: 0.25, h: 0.5 };
        let (r, _) = step(&mut d, start, Point::new(10.0 + 254.0, 160.0), SZ, &[other]);
        assert!((r.x - 0.25).abs() < 1e-6, "{r:?}");
    }

    #[test]
    fn resize_respects_minimum() {
        let start = FloatRect { x: 0.5, y: 0.5, w: 0.3, h: 0.3 };
        let mut d = Drag { id: window::Id::unique(), kind: DragKind::Resize, offset: None };
        let (r, _) = step(&mut d, start, Point::new(510.0, 260.0), SZ, &[]);
        assert!((r.w * SZ.width - MIN_W).abs() < 1e-3 && (r.h * SZ.height - MIN_H).abs() < 1e-3, "{r:?}");
    }
}
