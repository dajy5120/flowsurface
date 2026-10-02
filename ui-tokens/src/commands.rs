//! 两个程序共有的命令（docs/35 批 9：命令 id 对齐，UPDS V2 §12）。
//!
//! Cockpit 与 Studio 各有自己的命令注册表，但「同一件事」必须同名、同快捷键——
//! 在 Cockpit 按 Ctrl J 开底部面板、到 Studio 按 Ctrl J 却没反应，等于两套肌肉记忆。
//! 这里是唯一真相源；两边各有一个测试钉住自己的注册表与它一致。

/// (id, 名称, 快捷键)。快捷键按 UPDS V9 §77 Linux 列的写法。
pub const SHARED: [(&str, &str, &str); 7] = [
    ("palette", "命令面板", "Ctrl K"),
    ("inspector", "检查器：显示 / 隐藏", "Ctrl I"),
    ("bottom", "底部面板：显示 / 隐藏", "Ctrl J"),
    ("sidebar", "侧栏：显示 / 隐藏", "Ctrl B"),
    ("theme.cycle", "循环主题", "Ctrl Alt T"),
    ("density.cycle", "循环密度", "Ctrl Alt D"),
    ("shortcuts", "快捷键速查", "Ctrl /"),
];

/// 按 id 取（名称, 快捷键）。
pub fn shared(id: &str) -> Option<(&'static str, &'static str)> {
    SHARED.iter().find(|(i, _, _)| *i == id).map(|(_, t, k)| (*t, *k))
}
