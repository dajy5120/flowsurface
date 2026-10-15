//! 工具注册接口（docs/46 G5）：**加一种新工具 = 在这个目录里新增一个文件**，引擎核心不用改。
//!
//! 新文件里定义一个实现 [`ToolDef`] 的类型，导出成 `pub static TOOL`：
//!
//! ```ignore
//! use super::*;
//! pub struct Arrow;
//! pub static TOOL: Arrow = Arrow;
//! impl ToolDef for Arrow {
//!     fn kind(&self) -> Kind { Kind("arrow") }
//!     fn label(&self) -> &'static str { "箭头" }
//!     fn anchors(&self) -> usize { 2 }
//!     fn prims(&self, _v: &DrawView, _d: &Drawing, pts: &[Point]) -> Vec<Prim> { … }
//! }
//! ```
//!
//! `build.rs` 构建时扫描本目录（`mod.rs` 除外），生成「每个文件一个模块 + 登记表」，所以不用在任何地方登记。
//! 引擎里所有按种类分派的地方（锚点数、点几下、名字、画法、菜单分组、快捷键、对象树里的一行）
//! 内置种类走自己的分支，其余一律问这里——画、命中测试、拖锚点、拖整体、撤销、存盘都是通用的（G2 起按图元走）。

pub use iced::Point;

#[allow(unused_imports)] // 给工具文件用（`use super::*`）
pub use super::geometry::{Align, DrawView, Prim, Role};
pub use super::model::{Drawing, Kind};

/// 一种绘图工具。
pub trait ToolDef: Sync {
    /// 存盘用的名字（不能与内置种类重名）。
    fn kind(&self) -> Kind;
    /// 菜单与对象树里的名字。
    fn label(&self) -> &'static str;
    /// 存几个锚点（0 = 不定，点到回车为止）。
    fn anchors(&self) -> usize;
    /// 放下它要点几下（缺省 = 锚点数）。
    fn clicks(&self) -> usize {
        self.anchors()
    }
    /// 菜单里放在哪一组（缺省「其他」；与内置组同名就并进那一组）。
    fn group(&self) -> &'static str {
        "其他"
    }
    /// 快捷键：`Alt+K` 或 `Alt+Shift+K`（空 = 没有）。
    fn shortcut(&self) -> &'static str {
        ""
    }
    /// 锚点 → 图元。`pts` 是锚点的屏幕坐标（与 `d.anchors` 一一对应）。
    fn prims(&self, v: &DrawView, d: &Drawing, pts: &[Point]) -> Vec<Prim>;
    /// 对象树里名字后面的那一段（缺省空）。
    fn summary(&self, _d: &Drawing, _decimals: usize) -> String {
        String::new()
    }
}

// build.rs 生成：本目录每个文件一个模块（`#[path]` 指过去）+ `REGISTERED: &[&dyn ToolDef]`
include!(concat!(env!("OUT_DIR"), "/drawing_tools.rs"));

/// 登记了的全部工具。
#[must_use]
pub fn all() -> &'static [&'static dyn ToolDef] {
    REGISTERED
}

/// 这个种类是不是登记的工具。
#[must_use]
pub fn find(k: Kind) -> Option<&'static dyn ToolDef> {
    REGISTERED.iter().copied().find(|t| t.kind() == k)
}

/// `Alt+K` / `Alt+Shift+K` 对上了哪个登记的工具。
#[must_use]
pub fn for_shortcut(c: &str, shift: bool) -> Option<Kind> {
    let want = format!("Alt+{}{}", if shift { "Shift+" } else { "" }, c.to_ascii_uppercase());
    REGISTERED.iter().find(|t| t.shortcut().eq_ignore_ascii_case(&want)).map(|t| t.kind())
}
