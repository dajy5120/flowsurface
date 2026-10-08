//! Cockpit 外壳的个人设置（docs/42 第 2 期）：检查器宽度、是否钉住、当前页。
//!
//! 存在 Cockpit 自己的数据目录（`data::data_path("shell.json")`，与 `saved-state.json` 同处，样张实例自动隔离），
//! **不放 `ui.json`**：那份与 Studio 共读，旧版 Studio 存盘时会丢掉它不认识的字段。

use serde::{Deserialize, Serialize};

const FILE: &str = "shell.json";
/// 检查器缺省宽度（双击拖动条复位到它）。
pub const INSPECTOR_DEFAULT: f32 = 300.0;
/// 检查器最窄（再窄控件挤成竖排、拖动条也难抓）。
pub const INSPECTOR_MIN: f32 = 240.0;
/// 检查器最宽 = 窗口宽 × 这个比例（免得把主区挤没）。
pub const INSPECTOR_MAX_FRAC: f32 = 0.7;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ShellPrefs {
    pub inspector_width: f32,
    /// 钉住：常开（Ctrl I 切换）；不钉住时按「选中面板有没有可设置的东西」自动展开 / 收起
    pub inspector_pinned: bool,
}

impl Default for ShellPrefs {
    fn default() -> Self {
        Self { inspector_width: INSPECTOR_DEFAULT, inspector_pinned: false }
    }
}

pub fn load() -> ShellPrefs {
    std::fs::read_to_string(data::data_path(Some(FILE)))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save(p: &ShellPrefs) {
    if let Ok(j) = serde_json::to_string_pretty(p) {
        if let Err(e) = data::write_json_to_file(&j, FILE) {
            log::warn!("写 {FILE} 失败：{e}");
        }
    }
}

/// 把宽度夹到 [最窄, 窗口 × 70%]。
pub fn clamp_width(w: f32, window_w: f32) -> f32 {
    let max = (window_w * INSPECTOR_MAX_FRAC).max(INSPECTOR_MIN);
    w.clamp(INSPECTOR_MIN, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_and_defaults() {
        assert_eq!(clamp_width(100.0, 1920.0), INSPECTOR_MIN);
        assert_eq!(clamp_width(5000.0, 1000.0), 700.0);
        assert_eq!(clamp_width(420.0, 1920.0), 420.0);
        // 窗口很窄时上限不低于最窄
        assert_eq!(clamp_width(500.0, 200.0), INSPECTOR_MIN);
        let p: ShellPrefs = serde_json::from_str("{}").unwrap();
        assert_eq!(p, ShellPrefs::default());
    }
}
