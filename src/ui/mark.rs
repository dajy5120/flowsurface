//! 「非内容高度」测量（docs/42 第 4 期，防回潮）。
//!
//! docs/35 规定：面板主体在内容之上**只允许一行标题 + 一行视图切换**（约 56px）。规定写在文档里挡不住回潮，
//! 所以量出来：
//!
//! - [`body`] 包住面板主体（pane 标题栏以下的整块）；
//! - 面板在自己的内容起点（表格、图、时间线……）放一个 [`content`]；
//! - 画的时候两者的 y 相减，就是内容之上被控件、状态、说明占掉的高度。
//!
//! 只记**检查器托管着的那个面板**（它是减负规则作用的对象；没托管的面板按设计会内联显示全部设置）。
//! 样张模式每截一页取一次 [`take`]，写进 `chrome.json`，`scripts/ui_chrome_gate.py` 按阈值判定。
//! 平时不开样张也照样记一个数，代价是每帧几次比较。

use std::cell::RefCell;
use std::sync::Mutex;

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
    /// 正在画的面板主体：(面板名, 是否被托管, 主体顶 y, 已量到的高度)
    static CUR: RefCell<Option<(String, bool, f32, Option<f32>)>> = const { RefCell::new(None) };
}

static LAST: Mutex<Option<Measure>> = Mutex::new(None);

/// 最近一帧被托管面板的测量（样张截图时取）。
pub fn take() -> Option<Measure> {
    LAST.lock().ok().and_then(|mut g| g.take())
}

enum Kind {
    Body { panel: String, hosted: bool },
    Content,
}

pub struct Mark<'a, M> {
    content: Element<'a, M, Theme, Renderer>,
    kind: Kind,
}

/// 包住面板主体。`hosted` = 检查器正托管这个面板（只有它会被记录）。
pub fn body<'a, M: 'a>(panel: String, hosted: bool, content: impl Into<Element<'a, M, Theme, Renderer>>) -> Element<'a, M, Theme, Renderer> {
    Element::new(Mark { content: content.into(), kind: Kind::Body { panel, hosted } })
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
        self.content.as_widget_mut().layout(&mut tree.children[0], renderer, limits)
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
            Kind::Body { panel, hosted } => {
                let prev = CUR.with(|c| c.replace(Some((panel.clone(), *hosted, y, None))));
                self.content.as_widget().draw(&tree.children[0], renderer, theme, style, layout, cursor, viewport);
                if let Some((panel, true, _, px)) = CUR.with(|c| c.replace(prev))
                    && let Ok(mut g) = LAST.lock()
                {
                    *g = Some(Measure { panel, px });
                }
            }
            Kind::Content => {
                // 只认第一个内容标记（面板里可能有多块内容，起点是最上面那块）
                CUR.with(|c| {
                    if let Some((_, _, top, px @ None)) = c.borrow_mut().as_mut() {
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
