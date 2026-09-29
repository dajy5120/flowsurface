//! 界面偏好 `~/.config/wealthspring/ui.json`（docs/35 D9）：Cockpit 与 Studio **共读一份**，
//! 任一边改了，另一边监听到文件变化即时生效——「一个产品」的最低要求。
//!
//! 读的时候**宽容**：缺字段取缺省、不认识的值取缺省、文件坏了整份取缺省并告警，
//! 界面偏好不值得让程序起不来。

use serde_json::{Value, json};
use std::path::PathBuf;

use crate::{Density, ThemeId, UpDown};

#[derive(Clone, Debug, PartialEq)]
pub struct Prefs {
    pub theme: ThemeId,
    pub density: Density,
    /// 涨跌颜色约定（缺省绿涨红跌）
    pub up_down: UpDown,
    /// 色弱安全配色：涨跌改用蓝 / 橙（UPDS V1 §03 的 deuteranopia 变体）
    pub cvd_safe: bool,
    /// 界面缩放 0.8..=2.0，与系统缩放、密度相互独立（UPDS V6 §53）
    pub ui_scale: f32,
    /// 减弱动效：所有过渡改为 0ms 淡入淡出
    pub reduce_motion: bool,
    /// 隐藏数值（演示 / 截图）：金额、持仓、账户标识显示为 •••（UPDS V7 §57）
    pub hide_values: bool,
    /// 热图色阶：inferno / viridis / cividis
    pub heatmap_scale: String,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            theme: ThemeId::from_key(crate::DEFAULT_THEME).unwrap_or_default(),
            density: Density::from_key(crate::DEFAULT_DENSITY).unwrap_or_default(),
            up_down: UpDown::from_key(crate::DEFAULT_UP_DOWN).unwrap_or_default(),
            cvd_safe: false,
            ui_scale: 1.0,
            reduce_motion: false,
            hide_values: false,
            heatmap_scale: crate::DEFAULT_HEATMAP_SCALE.to_string(),
        }
    }
}

impl Prefs {
    /// `$XDG_CONFIG_HOME/wealthspring/ui.json`（缺省 `~/.config`）。
    /// `WS_UI_PREFS` 可覆盖（测试、样张模式用）。
    pub fn path() -> PathBuf {
        if let Some(p) = std::env::var_os("WS_UI_PREFS") {
            return PathBuf::from(p);
        }
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("wealthspring").join("ui.json")
    }

    /// 读偏好；文件不存在或坏了都返回缺省。
    pub fn load() -> Self {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .map(|v| Self::from_json(&v))
            .unwrap_or_default()
    }

    pub fn from_json(v: &Value) -> Self {
        let d = Self::default();
        let s = |k: &str| v.get(k).and_then(Value::as_str);
        let b = |k: &str, dflt: bool| v.get(k).and_then(Value::as_bool).unwrap_or(dflt);
        Self {
            theme: s("theme").and_then(ThemeId::from_key).unwrap_or(d.theme),
            density: s("density").and_then(Density::from_key).unwrap_or(d.density),
            up_down: s("upDown").and_then(UpDown::from_key).unwrap_or(d.up_down),
            cvd_safe: b("cvdSafe", d.cvd_safe),
            ui_scale: v
                .get("uiScale")
                .and_then(Value::as_f64)
                .map(|x| (x as f32).clamp(0.8, 2.0))
                .unwrap_or(d.ui_scale),
            reduce_motion: b("reduceMotion", d.reduce_motion),
            hide_values: b("hideValues", d.hide_values),
            heatmap_scale: s("heatmapScale")
                .filter(|x| ["inferno", "viridis", "cividis"].contains(x))
                .map(str::to_string)
                .unwrap_or(d.heatmap_scale),
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "theme": self.theme.key(),
            "density": self.density.key(),
            "upDown": self.up_down.key(),
            "cvdSafe": self.cvd_safe,
            "uiScale": self.ui_scale,
            "reduceMotion": self.reduce_motion,
            "hideValues": self.hide_values,
            "heatmapScale": self.heatmap_scale,
        })
    }

    /// 写回文件。**先写临时文件再改名**：另一个程序正在监听这个文件，
    /// 半截的 JSON 会让它读到缺省值、界面闪一下。
    pub fn save(&self) -> std::io::Result<()> {
        let p = Self::path();
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&self.to_json()).unwrap_or_default())?;
        std::fs::rename(tmp, p)
    }

    /// 文件修改时间（监听用：两个程序各自轮询它，变了就重读）。
    pub fn mtime() -> Option<std::time::SystemTime> {
        std::fs::metadata(Self::path()).and_then(|m| m.modified()).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 坏值取缺省_不报错() {
        let p = Prefs::from_json(&json!({"theme": "紫色", "density": 3, "uiScale": 9.0, "heatmapScale": "jet"}));
        assert_eq!(p.theme, ThemeId::Dark);
        assert_eq!(p.density, Density::Compact);
        assert_eq!(p.ui_scale, 2.0, "夹到上限");
        assert_eq!(p.heatmap_scale, "inferno");
    }

    #[test]
    fn 读写往返() {
        let p = Prefs {
            theme: ThemeId::Light,
            density: Density::Spacious,
            up_down: UpDown::China,
            cvd_safe: true,
            ui_scale: 1.25,
            reduce_motion: true,
            hide_values: true,
            heatmap_scale: "viridis".into(),
        };
        assert_eq!(Prefs::from_json(&p.to_json()), p);
    }

    #[test]
    fn 缺省值来自领域包() {
        let p = Prefs::default();
        assert_eq!(p.theme, ThemeId::Dark);
        assert_eq!(p.density, Density::Compact);
        assert_eq!(p.up_down, UpDown::International);
    }
}
