//! 命令注册表（UPDS V2 §12，docs/35 D8）：**一个命令一个定义**，命令面板、快捷键表、
//! 以后的菜单与右键菜单都从这里生成——不存在只能用鼠标到达的功能。
//!
//! 这里只描述「有哪些命令、叫什么、快捷键是什么」；怎么执行在 `main.rs` 的
//! `Message::RunCommand` 里，那边才拿得到应用状态。

use super::{Density, ThemeId};

/// 一条命令要做的事。
#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    /// 切到某个工作区（按名字）
    Workspace(String),
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
        e(Cmd::FocusNextPane, "面板", "聚焦下一个面板", "F6"),
        e(Cmd::FocusPrevPane, "面板", "聚焦上一个面板", "Shift F6"),
        e(Cmd::ToggleMaximize, "面板", "最大化 / 还原当前面板", "Ctrl Shift M"),
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
    ]);
    v
}

/// 按输入过滤并排序：标题开头命中 > 词首命中 > 任意位置命中；空输入返回全部（原顺序）。
///
/// 大小写不敏感；也匹配分组名（输入「外观」列出全部外观命令）。
pub fn filter<'a>(entries: &'a [Entry], query: &str) -> Vec<&'a Entry> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return entries.iter().collect();
    }
    let mut scored: Vec<(u8, usize, &Entry)> = entries
        .iter()
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
