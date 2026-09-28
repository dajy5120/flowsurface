//! 特征选择集：把自定义启用集存成有名字的配置，可保存 / 加载 / 更新 / 改名 / 删除 / 导出 / 导入。
//!
//! 每个选择集一个文件：`~/ws-data/cockpit/feature_presets/<名字>.json`（`WS_FEATURE_PRESETS` 可覆盖目录）。
//! 导出的文件与库里的是**同一个格式**，所以导出给别人、别人导入，拿到的就是同一份。
//!
//! ```json
//! {"format":"wealthspring-feature-preset","version":1,"name":"盘口失衡",
//!  "note":"…","keys":["obi_l2","obi_l3"],"updated":"2026-09-28 21:40:00",
//!  "windows":{"global":[60000,300000],"overrides":{"obi_l2":[10000]}}}
//! ```
//!
//! `windows` 是随选择集一起保存的时间窗口（全局一组 + 单条覆盖，毫秒）；旧文件没有它 = 默认窗口。
//!
//! 引擎不读这个目录——引擎只认 `feature_config.json`。应用一个选择集 = 把它的键写进那里再重启。

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub const FORMAT: &str = "wealthspring-feature-preset";

/// 随选择集保存的时间窗口。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PresetWindows {
    /// 全局一组（`None` = 字典默认）。
    pub global: Option<Vec<u32>>,
    /// 单条覆盖：特征键 → 窗口。
    pub overrides: std::collections::BTreeMap<String, Vec<u32>>,
}

impl PresetWindows {
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.global.is_none() && self.overrides.is_empty()
    }

    #[must_use]
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::json!({ "global": self.global, "overrides": self.overrides })
    }

    #[must_use]
    pub fn from_value(v: &serde_json::Value) -> Self {
        let list = |x: &serde_json::Value| -> Vec<u32> {
            x.as_array()
                .map(|a| a.iter().filter_map(|y| y.as_u64().map(|n| n as u32)).collect())
                .unwrap_or_default()
        };
        Self {
            global: v["global"].is_array().then(|| list(&v["global"])).filter(|g| !g.is_empty()),
            overrides: v["overrides"]
                .as_object()
                .map(|o| o.iter().map(|(k, x)| (k.clone(), list(x))).filter(|(_, x)| !x.is_empty()).collect())
                .unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Preset {
    pub name: String,
    pub note: String,
    pub keys: Vec<String>,
    pub updated: String,
    pub windows: PresetWindows,
}

impl Preset {
    #[must_use]
    pub fn to_json(&self) -> String {
        let v = serde_json::json!({
            "format": FORMAT,
            "version": 1,
            "name": self.name,
            "note": self.note,
            "keys": self.keys,
            "updated": self.updated,
            "windows": self.windows.to_value(),
        });
        serde_json::to_string_pretty(&v).unwrap_or_default()
    }

    /// 解析。`fallback_name` 给「文件里没写名字」时用（取文件名）。
    pub fn from_json(text: &str, fallback_name: &str) -> Result<Self, String> {
        let v: serde_json::Value =
            serde_json::from_str(text).map_err(|e| format!("不是合法的 JSON：{e}"))?;
        if let Some(f) = v["format"].as_str()
            && f != FORMAT
        {
            return Err(format!("不是特征选择集文件（format = {f}）"));
        }
        let keys: Vec<String> = v["keys"]
            .as_array()
            .ok_or("缺少 keys 数组")?
            .iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect();
        let name = v["name"].as_str().unwrap_or(fallback_name).trim().to_string();
        Ok(Self {
            name: if name.is_empty() { fallback_name.to_string() } else { name },
            note: v["note"].as_str().unwrap_or_default().to_string(),
            keys,
            updated: v["updated"].as_str().unwrap_or_default().to_string(),
            windows: PresetWindows::from_value(&v["windows"]),
        })
    }
}

/// 选择集目录。
#[must_use]
pub fn dir() -> PathBuf {
    std::env::var("WS_FEATURE_PRESETS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| super::paths::data_dir().join("cockpit").join("feature_presets"))
}

/// 名字 → 文件名。名字原样存在 JSON 里，文件名只去掉路径分隔等不能进文件名的字符。
#[must_use]
pub fn file_stem(name: &str) -> String {
    name.chars()
        .map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { '_' } else { c })
        .collect()
}

/// 名字是否可用（非空、不太长）。
pub fn check_name(name: &str) -> Result<(), String> {
    let n = name.trim();
    if n.is_empty() {
        return Err("名字不能为空".into());
    }
    if n.chars().count() > 40 {
        return Err("名字最长 40 个字".into());
    }
    if n.starts_with('.') {
        return Err("名字不能以 . 开头".into());
    }
    Ok(())
}

fn path_of(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{}.json", file_stem(name.trim())))
}

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

// ── 库（带缓存：面板每帧都要列一遍，而目录只在我们自己写的时候变）──────────────

static CACHE: OnceLock<Mutex<Option<Vec<Preset>>>> = OnceLock::new();

fn cache() -> &'static Mutex<Option<Vec<Preset>>> {
    CACHE.get_or_init(|| Mutex::new(None))
}

/// 让下一次 [`list`] 重读目录（打开选择页时、每次写入后调用）。
pub fn invalidate() {
    if let Ok(mut g) = cache().lock() {
        *g = None;
    }
}

/// 全部选择集，按名字排序。
#[must_use]
pub fn list() -> Vec<Preset> {
    if let Ok(g) = cache().lock()
        && let Some(v) = g.as_ref()
    {
        return v.clone();
    }
    let v = list_in(&dir());
    if let Ok(mut g) = cache().lock() {
        *g = Some(v.clone());
    }
    v
}

fn list_in(d: &Path) -> Vec<Preset> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let stem = p.file_stem().and_then(|x| x.to_str()).unwrap_or_default().to_string();
        if let Ok(t) = std::fs::read_to_string(&p)
            && let Ok(pr) = Preset::from_json(&t, &stem)
        {
            out.push(pr);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

#[must_use]
pub fn get(name: &str) -> Option<Preset> {
    list().into_iter().find(|p| p.name == name)
}

/// 写一个选择集（临时文件 + rename）。`overwrite = false` 时同名已存在则报错。
pub fn save(
    name: &str,
    note: &str,
    keys: &[String],
    windows: &PresetWindows,
    overwrite: bool,
) -> Result<Preset, String> {
    save_in(&dir(), name, note, keys, windows, overwrite).inspect(|_| invalidate())
}

fn save_in(
    d: &Path,
    name: &str,
    note: &str,
    keys: &[String],
    windows: &PresetWindows,
    overwrite: bool,
) -> Result<Preset, String> {
    check_name(name)?;
    if keys.is_empty() {
        return Err("一条特征都没选，不保存空选择集".into());
    }
    std::fs::create_dir_all(d).map_err(|e| format!("建目录 {} 失败：{e}", d.display()))?;
    let p = path_of(d, name);
    if !overwrite && list_in(d).iter().any(|x| x.name == name.trim() || path_of(d, &x.name) == p) {
        return Err(format!("已有同名选择集「{}」——要覆盖请用「更新」", name.trim()));
    }
    let mut keys = keys.to_vec();
    keys.sort();
    keys.dedup();
    let pr = Preset {
        name: name.trim().to_string(),
        note: note.trim().to_string(),
        keys,
        updated: now(),
        windows: windows.clone(),
    };
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, pr.to_json())
        .and_then(|()| std::fs::rename(&tmp, &p))
        .map_err(|e| format!("写 {} 失败：{e}", p.display()))?;
    Ok(pr)
}

/// 改名（连同内容一起更新）：先写新名字，再删旧文件。
pub fn rename_and_update(
    old: &str,
    new: &str,
    note: &str,
    keys: &[String],
    windows: &PresetWindows,
) -> Result<Preset, String> {
    let d = dir();
    if old != new.trim() && list_in(&d).iter().any(|x| x.name == new.trim()) {
        return Err(format!("已有同名选择集「{}」", new.trim()));
    }
    let pr = save_in(&d, new, note, keys, windows, true)?;
    if path_of(&d, old) != path_of(&d, new) {
        let _ = std::fs::remove_file(path_of(&d, old));
    }
    invalidate();
    Ok(pr)
}

pub fn delete(name: &str) -> Result<(), String> {
    let p = path_of(&dir(), name);
    std::fs::remove_file(&p).map_err(|e| format!("删 {} 失败：{e}", p.display()))?;
    invalidate();
    Ok(())
}

/// 导出到任意路径。
pub fn export_to(name: &str, path: &Path) -> Result<(), String> {
    let pr = get(name).ok_or_else(|| format!("没有选择集「{name}」"))?;
    std::fs::write(path, pr.to_json()).map_err(|e| format!("写 {} 失败：{e}", path.display()))
}

/// 从任意路径导入进库。重名时自动加后缀，不覆盖已有的。
pub fn import_from(path: &Path) -> Result<Preset, String> {
    let t = std::fs::read_to_string(path).map_err(|e| format!("读 {} 失败：{e}", path.display()))?;
    let stem = path.file_stem().and_then(|x| x.to_str()).unwrap_or("导入");
    let mut pr = Preset::from_json(&t, stem)?;
    let d = dir();
    let existing = list_in(&d);
    let base = pr.name.clone();
    let mut i = 1;
    while existing.iter().any(|x| x.name == pr.name) {
        i += 1;
        pr.name = format!("{base}（{i}）");
    }
    let r = save_in(&d, &pr.name, &pr.note, &pr.keys, &pr.windows, false);
    invalidate();
    r
}

// ── 系统对话框（zenity）。阻塞，调用方放后台线程 ─────────────────────────────

/// 「另存为」对话框。取消返回 `None`。
#[must_use]
pub fn ask_save_path(default_name: &str) -> Option<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let out = std::process::Command::new("zenity")
        .args([
            "--file-selection",
            "--save",
            "--confirm-overwrite",
            "--title=导出特征选择集",
            &format!("--filename={home}/{}.json", file_stem(default_name)),
            "--file-filter=特征选择集 (*.json) | *.json",
        ])
        .output()
        .ok()?;
    pick(out)
}

/// 「打开」对话框。取消返回 `None`。
#[must_use]
pub fn ask_open_path() -> Option<PathBuf> {
    let out = std::process::Command::new("zenity")
        .args([
            "--file-selection",
            "--title=导入特征选择集",
            "--file-filter=特征选择集 (*.json) | *.json",
        ])
        .output()
        .ok()?;
    pick(out)
}

fn pick(out: std::process::Output) -> Option<PathBuf> {
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then(|| PathBuf::from(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ws-presets-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn 保存后能列出且键去重排序() {
        let d = tmpdir("save");
        let k = vec!["obi_l2".to_string(), "mid_price".into(), "obi_l2".into()];
        save_in(&d, "盘口", "备注", &k, &PresetWindows::default(), false).unwrap();
        let l = list_in(&d);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].name, "盘口");
        assert_eq!(l[0].keys, vec!["mid_price".to_string(), "obi_l2".into()]);
        assert_eq!(l[0].note, "备注");
    }

    #[test]
    fn 同名不覆盖除非明确更新() {
        let d = tmpdir("dup");
        save_in(&d, "a", "", &["x".into()], &PresetWindows::default(), false).unwrap();
        assert!(save_in(&d, "a", "", &["y".into()], &PresetWindows::default(), false).is_err());
        save_in(&d, "a", "", &["y".into()], &PresetWindows::default(), true).unwrap();
        assert_eq!(list_in(&d)[0].keys, vec!["y".to_string()]);
    }

    #[test]
    fn 空选择集与坏名字被拒() {
        let d = tmpdir("bad");
        assert!(save_in(&d, "a", "", &[], &PresetWindows::default(), false).is_err());
        assert!(save_in(&d, "  ", "", &["x".into()], &PresetWindows::default(), false).is_err());
        assert!(save_in(&d, ".隐藏", "", &["x".into()], &PresetWindows::default(), false).is_err());
    }

    #[test]
    fn 名字里的路径分隔符不会逃出目录() {
        let d = tmpdir("esc");
        save_in(&d, "a/../../etc/坏", "", &["x".into()], &PresetWindows::default(), false).unwrap();
        let files: Vec<_> = std::fs::read_dir(&d).unwrap().flatten().collect();
        assert_eq!(files.len(), 1, "文件必须落在选择集目录里");
        // 名字原样保留在 JSON 里
        assert_eq!(list_in(&d)[0].name, "a/../../etc/坏");
    }

    #[test]
    fn 导出格式能原样读回() {
        let mut w = PresetWindows { global: Some(vec![60_000, 300_000]), ..PresetWindows::default() };
        w.overrides.insert("a".into(), vec![10_000]);
        let p = Preset { name: "n".into(), note: "备注".into(), keys: vec!["a".into(), "b".into()], updated: "t".into(), windows: w };
        let back = Preset::from_json(&p.to_json(), "x").unwrap();
        assert_eq!(back, p);
        // 别的面板的 JSON 不能被当成选择集
        assert!(Preset::from_json(r#"{"format":"别的","keys":[]}"#, "x").is_err());
        assert!(Preset::from_json(r#"{"name":"没有键"}"#, "x").is_err());
        // 没写名字的取文件名；旧文件没有 windows = 默认窗口
        let old = Preset::from_json(r#"{"keys":["a"]}"#, "文件名").unwrap();
        assert_eq!(old.name, "文件名");
        assert!(old.windows.is_default());
    }
}
