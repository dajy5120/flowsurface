//! 命令注册表（UPDS V2 §12，docs/35 D8）：**一个命令一个定义**，命令面板、快捷键表、
//! 以后的菜单与右键菜单都从这里生成——不存在只能用鼠标到达的功能。
//!
//! 这里只描述「有哪些命令、叫什么、快捷键是什么」；怎么执行在 `main.rs` 的
//! `Message::RunCommand` 里，那边才拿得到应用状态。

use super::{Density, ThemeId};

/// 一条命令要做的事。
#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    /// 切到某个工作区（按名字；回到它上次停留的页面）
    Workspace(String),
    /// 当前工作区的第 n 页（0 起，Alt 1–9；docs/41 §3.4）
    Page(usize),
    /// 当前工作区的下一页 / 上一页（Ctrl PgDn / Ctrl PgUp，循环）
    NextPage,
    PrevPage,
    Theme(ThemeId),
    CycleTheme,
    Density(Density),
    CycleDensity,
    /// 涨跌约定在「绿涨红跌 / 红涨绿跌」间切换
    ToggleUpDown,
    /// 色弱安全配色开关
    ToggleCvd,
    /// 隐藏数值（演示 / 截图）
    ToggleHideValues,
    /// 直接设定（设置面板的分段按钮用）
    UpDown(super::UpDown),
    Cvd(bool),
    HideValues(bool),
    /// 「系统」组工作区在紧凑下改用舒适（docs/35 §5.3）
    SystemComfortable(bool),
    HeatmapScale(&'static str),
    TogglePalette,
    ToggleBottom,
    ToggleInspector,
    FocusNextPane,
    FocusPrevPane,
    /// 最大化 / 还原当前面板
    ToggleMaximize,
    OpenSettings,
    OpenLayouts,
    OpenDataFolder,
    /// 底部面板切到某一页并打开
    BottomTab(super::shell::BottomTab),
    /// 跨进程：把当前策略发给 Studio 打开（docs/35 §16.5 第 4 项）
    OpenInStudio,
    /// 聚焦当前工作区的第 n 个面板（0 起，Ctrl 1–9；docs/35 §5.1）
    FocusPane(usize),
    /// 侧栏：显示 / 隐藏（Ctrl B）
    ToggleSidebar,
    /// 界面缩放：放大 / 缩小 / 复位（Ctrl + / Ctrl − / Ctrl 0）
    ZoomIn,
    ZoomOut,
    ZoomReset,
    /// 用浏览器打开一个地址（底部「告警」页点原文）
    OpenUrl(String),
    /// 聚焦的 K 线面板：图 ↔ 数据表（Ctrl Shift D，docs/35 §6.3）
    ToggleDataTable,
    /// 打开命令面板并预填范围前缀（Ctrl P → `#`，Ctrl Shift P → `@`，Ctrl / → `?`）
    PaletteScope(&'static str),
}

/// 命令面板里的一行。
#[derive(Debug, Clone)]
pub struct Entry {
    pub cmd: Cmd,
    pub title: String,
    /// 分组（命令面板里显示在标题前）
    pub category: &'static str,
    /// 快捷键的显示文字（Linux 拼写，UPDS V9 §77）。没有就空。
    pub shortcut: &'static str,
}

fn e(cmd: Cmd, category: &'static str, title: impl Into<String>, shortcut: &'static str) -> Entry {
    Entry { cmd, title: title.into(), category, shortcut }
}

/// 当前工作区的面板，作为命令面板里的「对象」（`#` 范围；Ctrl 1–9 直达前 9 个）。
pub fn pane_entries(names: &[String]) -> Vec<Entry> {
    const SC: [&str; 9] = ["Ctrl 1", "Ctrl 2", "Ctrl 3", "Ctrl 4", "Ctrl 5", "Ctrl 6", "Ctrl 7", "Ctrl 8", "Ctrl 9"];
    names
        .iter()
        .enumerate()
        .map(|(i, n)| e(Cmd::FocusPane(i), "面板对象", format!("聚焦面板 {}：{n}", i + 1), SC.get(i).copied().unwrap_or("")))
        .collect()
}

/// 全部命令。`workspaces` 按侧栏顺序，第 n 组的第一个工作区有 Ctrl Shift n 直达。
pub fn registry(workspaces: &[&str]) -> Vec<Entry> {
    let mut v = Vec::new();
    for name in workspaces {
        let sc = super::shell::group_shortcut(name).unwrap_or("");
        v.push(e(Cmd::Workspace(name.to_string()), "工作区", format!("切到：{name}"), sc));
    }
    v.extend([
        e(Cmd::TogglePalette, "视图", "命令面板", "Ctrl K"),
        e(Cmd::ToggleBottom, "视图", "底部面板：显示 / 隐藏", "Ctrl J"),
        e(Cmd::ToggleInspector, "视图", "检查器：显示 / 隐藏", "Ctrl I"),
        e(Cmd::BottomTab(super::shell::BottomTab::Log), "视图", "底部面板：日志", ""),
        e(Cmd::BottomTab(super::shell::BottomTab::Problems), "视图", "底部面板：问题（警告与错误）", ""),
        e(Cmd::BottomTab(super::shell::BottomTab::Activity), "视图", "底部面板：活动（运行中的回测 / 回放）", ""),
        e(Cmd::BottomTab(super::shell::BottomTab::Alerts), "视图", "底部面板：告警（新闻规则命中）", ""),
        e(Cmd::BottomTab(super::shell::BottomTab::Notices), "视图", "底部面板：通知中心（弹过的全部提示）", ""),
        e(Cmd::NextPage, "页面", "下一页（当前工作区）", "Ctrl PgDn"),
        e(Cmd::PrevPage, "页面", "上一页（当前工作区）", "Ctrl PgUp"),
        e(Cmd::FocusNextPane, "面板", "聚焦下一个面板", "F6"),
        e(Cmd::FocusPrevPane, "面板", "聚焦上一个面板", "Shift F6"),
        e(Cmd::ToggleMaximize, "面板", "最大化 / 还原当前面板", "Ctrl Shift M"),
        e(Cmd::ToggleDataTable, "面板", "K 线：图 ↔ 数据表", "Ctrl Shift D"),
        e(Cmd::PaletteScope("#"), "视图", "快速切换：面板与工作区", "Ctrl P"),
        e(Cmd::PaletteScope("@"), "视图", "切换工作区", "Ctrl Shift P"),
        e(Cmd::PaletteScope("?"), "视图", "快捷键速查", "Ctrl /"),
        e(Cmd::ToggleSidebar, "视图", "侧栏：显示 / 隐藏", "Ctrl B"),
        e(Cmd::ZoomIn, "外观", "界面放大", "Ctrl +"),
        e(Cmd::ZoomOut, "外观", "界面缩小", "Ctrl −"),
        e(Cmd::ZoomReset, "外观", "界面缩放复位", "Ctrl 0"),
        e(Cmd::CycleTheme, "外观", "循环主题", "Ctrl Alt T"),
        e(Cmd::CycleDensity, "外观", "循环密度", "Ctrl Alt D"),
    ]);
    for t in ThemeId::ALL {
        v.push(e(Cmd::Theme(t), "外观", format!("主题：{}", t.label()), ""));
    }
    for d in Density::ALL {
        v.push(e(Cmd::Density(d), "外观", format!("密度：{}", d.label()), ""));
    }
    v.extend([
        e(Cmd::ToggleUpDown, "外观", "涨跌颜色：绿涨红跌 ⇄ 红涨绿跌", ""),
        e(Cmd::ToggleCvd, "外观", "色弱安全配色：开 / 关", ""),
        e(Cmd::ToggleHideValues, "外观", "隐藏数值（演示 / 截图）：开 / 关", ""),
        e(Cmd::OpenSettings, "设置", "打开设置", "Ctrl ,"),
        e(Cmd::OpenLayouts, "设置", "布局管理", ""),
        e(Cmd::OpenDataFolder, "设置", "打开数据文件夹", ""),
        e(Cmd::OpenInStudio, "运行", "在 Studio 中打开策略", ""),
    ]);
    v
}

/// 按输入过滤并排序：标题开头命中 > 词首命中 > 任意位置命中；空输入返回全部（原顺序）。
///
/// 大小写不敏感；也匹配分组名（输入「外观」列出全部外观命令）。
/// 范围前缀（UPDS V2 §12，docs/35 §6.1）：`@` 工作区、`#` 面板与工作区（对象）、`>` 命令、
/// `:` 外观与设置、`?` 有快捷键的条目（速查）。返回（该条目在不在范围内，去掉前缀后的查询）。
fn scope(query: &str) -> (fn(&Entry) -> bool, &str) {
    let q = query.trim_start();
    let mut chars = q.chars();
    let f: fn(&Entry) -> bool = match chars.next() {
        Some('@') => |e| e.category == "工作区",
        Some('#') => |e| e.category == "面板对象" || e.category == "工作区",
        Some('>') => |e| e.category != "工作区" && e.category != "面板对象",
        Some(':') => |e| e.category == "外观" || e.category == "设置",
        Some('?') => |e| !e.shortcut.is_empty(),
        _ => return (|_| true, q),
    };
    (f, chars.as_str())
}

pub fn filter<'a>(entries: &'a [Entry], query: &str) -> Vec<&'a Entry> {
    let (in_scope, rest) = scope(query);
    let entries: Vec<&'a Entry> = entries.iter().filter(|e| in_scope(e)).collect();
    let q = rest.trim().to_lowercase();
    if q.is_empty() {
        return entries;
    }
    let mut scored: Vec<(u8, usize, &Entry)> = entries
        .into_iter()
        .enumerate()
        .filter_map(|(i, en)| {
            let t = en.title.to_lowercase();
            let c = en.category.to_lowercase();
            let s = if t.starts_with(&q) {
                0
            } else if t.split(['：', ' ', '/', '（']).any(|w| w.starts_with(&q)) {
                1
            } else if t.contains(&q) {
                2
            } else if c.contains(&q) {
                3
            } else {
                return None;
            };
            Some((s, i, en))
        })
        .collect();
    scored.sort_by_key(|(s, i, _)| (*s, *i));
    scored.into_iter().map(|(_, _, en)| en).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 共有命令与_studio_同名同键() {
        // 唯一真相源在 wealthspring_ui_tokens::commands::SHARED；Studio 那边有同样的测试
        let r = registry(&["回测"]);
        for (id, title, sc) in wealthspring_ui_tokens::commands::SHARED {
            assert!(r.iter().any(|e| e.title == title && e.shortcut == sc), "Cockpit 缺「{title}」（{sc}，id {id}）");
        }
    }

    #[test]
    fn 范围前缀() {
        let mut r = registry(&["回测", "订单流特征"]);
        r.extend(pane_entries(&["K 线".into(), "特征矩阵".into()]));
        assert!(filter(&r, "@").iter().all(|e| e.category == "工作区"));
        assert_eq!(filter(&r, "@").len(), 2);
        let objs = filter(&r, "#");
        assert!(objs.iter().any(|e| e.cmd == Cmd::FocusPane(1)), "面板对象在 # 里");
        assert!(objs.iter().any(|e| e.category == "工作区"), "# 也能切工作区");
        assert!(filter(&r, ">").iter().all(|e| e.category != "工作区" && e.category != "面板对象"));
        assert!(filter(&r, ":主题").iter().all(|e| e.category == "外观"));
        assert!(filter(&r, "?").iter().all(|e| !e.shortcut.is_empty()), "速查只列有快捷键的");
        assert_eq!(filter(&r, "#特征")[0].cmd, Cmd::FocusPane(1), "前缀后照常按标题搜");
        assert_eq!(pane_entries(&["a".into()])[0].shortcut, "Ctrl 1");
    }

    #[test]
    fn 过滤_按命中位置排序_大小写不敏感() {
        let r = registry(&["回测", "订单流特征"]);
        let f = filter(&r, "主题");
        assert!(f.len() >= 5, "循环主题 + 四个主题");
        assert!(f[0].title.starts_with("主题"), "开头命中排前：{}", f[0].title);
        assert!(filter(&r, "ctrl").is_empty(), "只搜标题与分组，不搜快捷键文字");
        assert_eq!(filter(&r, "").len(), r.len());
        assert_eq!(filter(&r, "外观").len(), r.iter().filter(|e| e.category == "外观").count());
    }

    #[test]
    fn 每个工作区都有命令() {
        let ws = ["官方原生", "回测"];
        let r = registry(&ws);
        for w in ws {
            assert!(r.iter().any(|e| e.cmd == Cmd::Workspace(w.into())));
        }
    }
}
