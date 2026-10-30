//! 市场状态引擎的用户参数（docs/69 C）：按市场 / 周期 / 品种逐层覆盖，存 `<状态文件目录>/params.json`。
//!
//! 解析顺序与 Python `factory/regime/user_params.py` 一致（两边单测用同一份例子）：
//! `default` → `market.<市场>` → `tf.<周期>` → `symbol.<代码>` → `symbol.<代码>@<周期>`，后面的盖前面。
//! Python 的 `run` / `live` 按它算并把套用的覆盖写进时间线文件的 `user_params`；这里用来编辑、显示，
//! 以及判断「参数改了、要重算」（[`same`]）。检验框架不读它——docs/62 的结论不受影响。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

pub type Layer = BTreeMap<String, serde_json::Value>;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ParamsFile {
    pub default: Layer,
    pub market: BTreeMap<String, Layer>,
    pub tf: BTreeMap<String, Layer>,
    pub symbol: BTreeMap<String, Layer>,
}

/// 来源 → 市场（与 Python `user_params.MARKET` 相同）。
#[must_use]
pub fn market_of(source: &str) -> &'static str {
    match source {
        "databento_futures" => "futures",
        "databento_equities" | "databento_1m" => "equities",
        _ => "crypto",
    }
}

#[must_use]
pub fn market_name(m: &str) -> &'static str {
    match m {
        "futures" => "期货",
        "equities" => "美股",
        _ => "加密",
    }
}

/// 改哪一层。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    All,
    Market(String),
    Symbol(String),
    SymbolTf(String, String),
}

impl Scope {
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Scope::All => "全部".into(),
            Scope::Market(m) => format!("{}市场", market_name(m)),
            Scope::Symbol(c) => c.clone(),
            Scope::SymbolTf(c, tf) => format!("{c} {tf}"),
        }
    }
    fn layer_mut<'a>(&self, f: &'a mut ParamsFile) -> &'a mut Layer {
        match self {
            Scope::All => &mut f.default,
            Scope::Market(m) => f.market.entry(m.clone()).or_default(),
            Scope::Symbol(c) => f.symbol.entry(c.clone()).or_default(),
            Scope::SymbolTf(c, tf) => f.symbol.entry(format!("{c}@{tf}")).or_default(),
        }
    }
}

/// 逐层（从先到后）：(层名, 这一层)。
fn layers<'a>(f: &'a ParamsFile, market: &str, code: &str, tf: &str) -> Vec<(String, Option<&'a Layer>)> {
    vec![
        ("全部".into(), Some(&f.default)),
        (format!("{}市场", market_name(market)), f.market.get(market)),
        (format!("周期 {tf}"), f.tf.get(tf)),
        (code.to_string(), f.symbol.get(code)),
        (format!("{code} {tf}"), f.symbol.get(&format!("{code}@{tf}"))),
    ]
}

/// 套到这个（市场, 代码, 周期）上的覆盖：{字段: 值}（只认数字）。
#[must_use]
pub fn resolve(f: &ParamsFile, market: &str, code: &str, tf: &str) -> BTreeMap<String, f64> {
    let mut out = BTreeMap::new();
    for (_, l) in layers(f, market, code, tf) {
        for (k, v) in l.into_iter().flatten() {
            if let Some(x) = v.as_f64() {
                out.insert(k.clone(), x);
            }
        }
    }
    out
}

/// 这个字段最后由哪一层定的（没人改 = None）。
#[must_use]
pub fn origin(f: &ParamsFile, market: &str, code: &str, tf: &str, key: &str) -> Option<String> {
    layers(f, market, code, tf).into_iter().rev().find(|(_, l)| l.is_some_and(|l| l.get(key).is_some_and(serde_json::Value::is_number))).map(|(n, _)| n)
}

/// 文件里记的覆盖与现在解析出来的是否一样（键相同、值差不到 1e-9）。
#[must_use]
pub fn same(a: &BTreeMap<String, f64>, b: &BTreeMap<String, f64>) -> bool {
    a.len() == b.len() && a.iter().all(|(k, x)| b.get(k).is_some_and(|y| (x - y).abs() < 1e-9))
}

/// 界面开放的 8 个参数：(字段, 名字, 说明, 步长, 最小, 最大)。缺省值不在这里抄一份——显示的是时间线文件里实际用的值。
pub const KEYS: [(&str, &str, &str, f64, f64, f64); 8] = [
    ("ema_slow", "慢线长度", "方向票与斜率用的慢 EMA；越短越快", 1.0, 10.0, 200.0),
    ("slope_min", "斜率门槛", "慢线在 10 根里至少移动多少 ATR 才算有方向", 0.05, 0.0, 2.0),
    ("adx_min", "ADX 门槛", "DI 那一票要 ADX 至少这么高", 1.0, 5.0, 50.0),
    ("er_dir_min", "效率比门槛", "效率比那一票的门槛（0–1）", 0.05, 0.05, 0.9),
    ("swing_k", "波段确认", "从极值回撤多少 ATR 才确认波段点；越小越快、越碎", 0.1, 0.3, 5.0),
    ("box_n", "箱体根数", "区间 / 突破看前几根", 5.0, 10.0, 200.0),
    ("dwell", "最短停留", "方向 / 质量 / 综合要连续几根才切换；越小越快、越抖", 1.0, 1.0, 10.0),
    ("htf_mult", "大周期倍数", "大周期 = 图周期 × 它（日线来源用周线，不受它管）", 1.0, 2.0, 64.0),
];

/// 预设：(名字, 说明, [(字段, 值)])；空表 = 清掉这一层。
/// 预设的一套值：(字段, 值)。
pub type PresetVals = &'static [(&'static str, f64)];

pub const PRESETS: [(&str, &str, PresetVals); 3] = [
    ("反应快", "慢线 34、波段 1.0 ATR、箱体 30、停留 2、大周期 8：切换早、也更碎", &[("ema_slow", 34.0), ("swing_k", 1.0), ("box_n", 30.0), ("dwell", 2.0), ("htf_mult", 8.0)]),
    ("缺省", "清掉这一层的覆盖", &[]),
    ("稳一点", "慢线 89、波段 2.0 ATR、箱体 60、停留 4：切换晚、更少来回", &[("ema_slow", 89.0), ("swing_k", 2.0), ("box_n", 60.0), ("dwell", 4.0)]),
];

// ── 读写 ──────────────────────────────────────────────────────────────

#[must_use]
pub fn path() -> PathBuf {
    super::regime::files_dir().join("params.json")
}

struct Cached {
    mtime: Option<SystemTime>,
    file: ParamsFile,
    err: String,
}

fn cell() -> &'static Mutex<Cached> {
    static C: OnceLock<Mutex<Cached>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(Cached { mtime: None, file: ParamsFile::default(), err: String::new() }))
}

/// 现在的参数文件（文件改了就重读：手改的也认）。
#[must_use]
pub fn load() -> ParamsFile {
    let p = path();
    let mtime = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
    let mut c = cell().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if c.mtime != mtime {
        c.mtime = mtime;
        (c.file, c.err) = match std::fs::read_to_string(&p) {
            Ok(t) => match serde_json::from_str(&t) {
                Ok(f) => (f, String::new()),
                Err(e) => (ParamsFile::default(), format!("{} 读不了：{e}（按缺省算）", p.display())),
            },
            Err(_) => (ParamsFile::default(), String::new()),
        };
    }
    c.file.clone()
}

#[must_use]
pub fn load_err() -> String {
    let _ = load();
    cell().lock().map(|c| c.err.clone()).unwrap_or_default()
}

fn save(f: &ParamsFile) -> Result<(), String> {
    let p = path();
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let tmp = p.with_extension("json.tmp");
    let t = serde_json::to_string_pretty(f).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, t).and_then(|()| std::fs::rename(&tmp, &p)).map_err(|e| e.to_string())
}

fn tidy(f: &mut ParamsFile) {
    f.market.retain(|_, l| !l.is_empty());
    f.tf.retain(|_, l| !l.is_empty());
    f.symbol.retain(|_, l| !l.is_empty());
}

/// 这一层自己有没有写这个字段。
#[must_use]
pub fn has(f: &ParamsFile, scope: &Scope, key: &str) -> bool {
    let l = match scope {
        Scope::All => Some(&f.default),
        Scope::Market(m) => f.market.get(m),
        Scope::Symbol(c) => f.symbol.get(c),
        Scope::SymbolTf(c, tf) => f.symbol.get(&format!("{c}@{tf}")),
    };
    l.is_some_and(|l| l.contains_key(key))
}

/// 改一个字段（`None` = 去掉这一层的覆盖）。
pub fn set(f: &mut ParamsFile, scope: &Scope, key: &str, v: Option<f64>) {
    let l = scope.layer_mut(f);
    match v {
        Some(x) => {
            // 整数字段存整数（Python 也会按字段类型转，这里只为文件好看）
            let val = if x.fract() == 0.0 { serde_json::json!(x as i64) } else { serde_json::json!((x * 1000.0).round() / 1000.0) };
            l.insert(key.to_string(), val);
        }
        None => {
            l.remove(key);
        }
    }
    tidy(f);
}

/// 套一个预设：先清掉这一层，再写预设的值。
pub fn apply_preset(f: &mut ParamsFile, scope: &Scope, preset: usize) {
    scope.layer_mut(f).clear();
    if let Some((_, _, vals)) = PRESETS.get(preset) {
        for (k, v) in *vals {
            set(f, scope, k, Some(*v));
        }
    }
    tidy(f);
}

/// 改完存盘；失败返回说明。
pub fn edit(op: impl FnOnce(&mut ParamsFile)) -> Result<(), String> {
    let mut f = load();
    op(&mut f);
    save(&f)?;
    let _ = load();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 与 Python `factory/tests/test_regime_user_params.py` 的 EXAMPLE 同一份。
    const EXAMPLE: &str = r#"{
        "default": {"dwell": 2},
        "market": {"crypto": {"swing_k": 1.2, "box_n": 30}, "futures": {"swing_k": 2.0}},
        "tf": {"15m": {"ema_slow": 34}},
        "symbol": {"BTCUSDT": {"box_n": 50}, "BTCUSDT@15m": {"swing_k": 1.0}}
    }"#;

    fn m(v: &[(&str, f64)]) -> BTreeMap<String, f64> {
        v.iter().map(|(k, x)| ((*k).to_string(), *x)).collect()
    }

    #[test]
    fn 逐层覆盖_与python一致() {
        let f: ParamsFile = serde_json::from_str(EXAMPLE).unwrap();
        assert_eq!(resolve(&f, "crypto", "BTCUSDT", "15m"), m(&[("dwell", 2.0), ("swing_k", 1.0), ("box_n", 50.0), ("ema_slow", 34.0)]));
        assert_eq!(resolve(&f, "crypto", "ETHUSDT", "1h"), m(&[("dwell", 2.0), ("swing_k", 1.2), ("box_n", 30.0)]));
        assert_eq!(resolve(&f, "futures", "ES", "1d"), m(&[("dwell", 2.0), ("swing_k", 2.0)]));
        assert_eq!(origin(&f, "crypto", "BTCUSDT", "15m", "swing_k").as_deref(), Some("BTCUSDT 15m"));
        assert_eq!(origin(&f, "crypto", "BTCUSDT", "15m", "box_n").as_deref(), Some("BTCUSDT"));
        assert_eq!(origin(&f, "crypto", "BTCUSDT", "15m", "adx_min"), None);
    }

    #[test]
    fn 改与恢复_预设() {
        let mut f = ParamsFile::default();
        let sc = Scope::SymbolTf("BTCUSDT".into(), "5m".into());
        set(&mut f, &sc, "swing_k", Some(1.2));
        set(&mut f, &sc, "box_n", Some(35.0));
        assert_eq!(serde_json::to_value(&f.symbol["BTCUSDT@5m"]).unwrap(), serde_json::json!({"box_n": 35, "swing_k": 1.2}));
        set(&mut f, &sc, "box_n", None);
        set(&mut f, &sc, "swing_k", None);
        assert!(f.symbol.is_empty(), "空层删掉");
        apply_preset(&mut f, &Scope::Market("crypto".into()), 0);
        assert_eq!(resolve(&f, "crypto", "X", "1h").get("ema_slow"), Some(&34.0));
        apply_preset(&mut f, &Scope::Market("crypto".into()), 1);
        assert!(resolve(&f, "crypto", "X", "1h").is_empty());
    }

    #[test]
    fn 比对() {
        assert!(same(&m(&[("a", 1.0)]), &m(&[("a", 1.0 + 1e-12)])));
        assert!(!same(&m(&[("a", 1.0)]), &m(&[])));
        assert!(!same(&m(&[("a", 1.0)]), &m(&[("a", 1.1)])));
        assert_eq!(market_of("binance"), "crypto");
        assert_eq!(market_of("databento_1m"), "equities");
    }
}
