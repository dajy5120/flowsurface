//! 「非内容高度」测量（docs/42 第 4 期，防回潮）。
//!
//! docs/35 规定：面板主体在内容之上**只允许一行标题 + 一行视图切换**（约 56px）。规定写在文档里挡不住回潮，
//! 所以量出来：
//!
//! - [`body`] 包住面板主体（pane 标题栏以下的整块）；
//! - 面板在自己的内容起点（表格、图、时间线……）放一个 [`content`]；
//! - 画的时候两者的 y 相减，就是内容之上被控件、状态、说明占掉的高度。
//!
//! 主区始终减负（docs/42），所以**每个面板都量**。样张模式每截一页取一次 [`take`]（最近一秒里画过的面板），
//! 写进 `chrome.json`，`scripts/ui_chrome_gate.py` 按阈值判定。
//! 平时不开样张也照样记一个数，代价是每帧几次比较。

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::{Operation, Tree};
use iced::advanced::{Clipboard, Shell, Widget};
use iced::{Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector, mouse};

/// 一次测量：面板名 + 非内容高度（`None` = 这个面板没放内容标记）。
#[derive(Debug, Clone, PartialEq)]
pub struct Measure {
    pub panel: String,
    pub px: Option<f32>,
}

thread_local! {
    /// 正在画的面板主体：(面板名, 主体顶 y, 已量到的高度)
    static CUR: RefCell<Option<(String, f32, Option<f32>)>> = const { RefCell::new(None) };
}

/// 面板名（同名面板加主体顶 y 区分）→ (测量, 画的时刻)
static SEEN: Mutex<BTreeMap<(String, i32), (Measure, Instant)>> = Mutex::new(BTreeMap::new());

/// 最近一秒里画过的全部面板的测量（样张截图时取；取完清空，上一页残留的不会混进来）。
pub fn take() -> Vec<Measure> {
    let Ok(mut g) = SEEN.lock() else { return vec![] };
    let now = Instant::now();
    let out = g.values().filter(|(_, t)| now.duration_since(*t) < Duration::from_secs(1)).map(|(m, _)| m.clone()).collect();
    g.clear();
    out
}

enum Kind {
    Body { panel: String },
    Content,
    /// 量宽度（[`width`]）：布局时记下可用宽度，下一帧给需要「撑满」的组件用
    Width(String),
}

/// 键 → (宽, 高, 最近一次布局的时刻)
static WIDTHS: Mutex<BTreeMap<String, (f32, f32, Instant)>> = Mutex::new(BTreeMap::new());

/// 包住一块，布局时把它拿到的宽度记在 `key` 下（[`width_of`] 取）。
///
/// 给「内容宽度要按可用宽度算」的组件用（如表格列宽撑满）：iced 的横向滚动区里不能放 Fill，
/// 只能先量出可用宽度、下一帧按它定宽——晚一帧，窗口拖宽时表格跟着撑开会慢一拍，看不出来。
pub fn width<'a, M: 'a>(key: String, content: impl Into<Element<'a, M, Theme, Renderer>>) -> Element<'a, M, Theme, Renderer> {
    Element::new(Mark { content: content.into(), kind: Kind::Width(key) })
}

/// 上一帧量到的宽度。
pub fn width_of(key: &str) -> Option<f32> {
    WIDTHS.lock().ok().and_then(|g| g.get(key).map(|s| s.0))
}

/// 上一帧量到的高度（同一个包装记的）。
pub fn height_of(key: &str) -> Option<f32> {
    WIDTHS.lock().ok().and_then(|g| g.get(key).map(|s| s.1))
}

pub struct Mark<'a, M> {
    content: Element<'a, M, Theme, Renderer>,
    kind: Kind,
}

/// 包住面板主体（pane 标题栏以下的整块）。
pub fn body<'a, M: 'a>(panel: String, content: impl Into<Element<'a, M, Theme, Renderer>>) -> Element<'a, M, Theme, Renderer> {
    Element::new(Mark { content: content.into(), kind: Kind::Body { panel } })
}

/// 标出内容起点（放在表格 / 图 / 时间线等真正的内容外面）。
pub fn content<'a, M: 'a>(content: impl Into<Element<'a, M, Theme, Renderer>>) -> Element<'a, M, Theme, Renderer> {
    Element::new(Mark { content: content.into(), kind: Kind::Content })
}

/// 零高度的内容起点（面板的内容是一串零散的块、没有一个整体可包时用）。
pub fn here<'a, M: 'a>() -> Element<'a, M, Theme, Renderer> {
    // 1px 高：父容器按可见区裁剪子组件，零高度的会被跳过不画，也就记不到
    content(iced::widget::container(iced::widget::column![]).width(Length::Fill).height(Length::Fixed(1.0)))
}

impl<M> Widget<M, Theme, Renderer> for Mark<'_, M> {
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &layout::Limits) -> layout::Node {
        let node = self.content.as_widget_mut().layout(&mut tree.children[0], renderer, limits);
        if let Kind::Width(key) = &self.kind
            && let Ok(mut g) = WIDTHS.lock()
        {
            let now = Instant::now();
            // 有的表每帧新建状态（键每帧都变）：条目多了就淘汰 10 秒没布局过的，正在显示的不受影响
            if g.len() > 512 {
                g.retain(|_, v| now.duration_since(v.2) < Duration::from_secs(10));
            }
            g.insert(key.clone(), (node.size().width, node.size().height, now));
        }
        node
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn operate(&mut self, tree: &mut Tree, layout: Layout<'_>, renderer: &Renderer, operation: &mut dyn Operation) {
        self.content.as_widget_mut().operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(&mut tree.children[0], event, layout, cursor, renderer, clipboard, shell, viewport);
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let y = layout.bounds().y;
        match &self.kind {
            Kind::Body { panel } => {
                let prev = CUR.with(|c| c.replace(Some((panel.clone(), y, None))));
                self.content.as_widget().draw(&tree.children[0], renderer, theme, style, layout, cursor, viewport);
                if let Some((panel, top, px)) = CUR.with(|c| c.replace(prev))
                    && let Ok(mut g) = SEEN.lock()
                {
                    g.insert((panel.clone(), top as i32), (Measure { panel, px }, Instant::now()));
                }
            }
            Kind::Width(_) => {
                self.content.as_widget().draw(&tree.children[0], renderer, theme, style, layout, cursor, viewport);
            }
            Kind::Content => {
                // 只认第一个内容标记（面板里可能有多块内容，起点是最上面那块）
                CUR.with(|c| {
                    if let Some((_, top, px @ None)) = c.borrow_mut().as_mut() {
                        *px = Some((y - *top).max(0.0));
                    }
                });
                self.content.as_widget().draw(&tree.children[0], renderer, theme, style, layout, cursor, viewport);
            }
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(&tree.children[0], layout, cursor, viewport, renderer)
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(&mut tree.children[0], layout, renderer, viewport, translation)
    }
}
