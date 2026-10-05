//! 策略中心 · 参数优化（docs/37 §6）：表单 → 优化规格，研究库 studies 表 → 视图数据。
//!
//! 纯数据与纯函数（可测）；状态与 IO 在 [`super::strategy_center`]。优化本身在
//! `python -m factory.lab.optimize`（经 ws-control 的 `run_study` 起），面板只发规格、读结果。

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Value, json};

use super::strategy_center::{Entry, Param};

/// 搜索方式（与 factory.lab.optimize.SAMPLERS 一致）。
pub const SAMPLERS: [(&str, &str); 4] = [("grid", "网格"), ("random", "随机"), ("tpe", "TPE"), ("cmaes", "CMA-ES")];
/// 目标函数（全部求最大；与 factory.lab.optimize.OBJECTIVES 一致）。
pub const OBJECTIVES: [(&str, &str); 6] = [
    ("calmar", "收益率 ÷ 最大回撤"),
    ("pnl", "费后盈亏"),
    ("pnl_pct", "费后收益率"),
    ("sharpe", "夏普"),
    ("sortino", "Sortino"),
    ("net_bp_mean", "每笔净 bp（快速回测）"),
];
pub const MAX_GRID: usize = 2000;

/// 表单里一个参数的搜索设置。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OptField {
    pub on: bool,
    pub low: String,
    pub high: String,
    pub step: String,
}

#[derive(Debug, Clone)]
pub struct OptForm {
    pub fields: BTreeMap<String, OptField>,
    pub sampler: String,
    pub trials: String,
    pub workers: String,
    pub objective: String,
    /// full / quick（策略支持 quick 时可选；快速回测几秒一次，适合大范围扫）
    pub engine: String,
    /// 表单是按哪次 AI 研究助理的草稿填的（发起时带上，研究库能追溯）
    pub assist_id: Option<String>,
}

impl Default for OptForm {
    fn default() -> Self {
        Self {
            fields: BTreeMap::new(),
            sampler: "tpe".into(),
            trials: "20".into(),
            workers: "2".into(),
            objective: "calmar".into(),
            engine: "full".into(),
            assist_id: None,
        }
    }
}

fn num_text(v: Option<f64>, integer: bool) -> String {
    match v {
        Some(x) if integer => format!("{}", x as i64),
        Some(x) => format!("{x}"),
        None => String::new(),
    }
}

/// 参数能不能搜：数值（有上下限）、枚举、布尔。
pub fn searchable(p: &Param) -> bool {
    p.choices.is_some() || p.kind == "boolean" || matches!(p.kind.as_str(), "integer" | "number")
}

/// 按策略注解初始化：标了 x-optimize 的默认勾上，范围 / 步长取注解。
pub fn init_form(params: &[Param]) -> OptForm {
    let fields = params
        .iter()
        .filter(|p| searchable(p))
        .map(|p| {
            let int = p.kind == "integer";
            // 标了 x-optimize 但注解缺上下限的（如只有下限的阈值）默认不勾：
            // 勾上就得先填范围，否则一点「开始优化」就失败，而且不容易看出为什么
            let bounded = p.choices.is_some() || p.kind == "boolean" || (p.minimum.is_some() && p.maximum.is_some());
            (
                p.name.clone(),
                OptField {
                    on: p.optimize && bounded,
                    low: num_text(p.minimum, int),
                    high: num_text(p.maximum, int),
                    step: num_text(p.step, int),
                },
            )
        })
        .collect();
    OptForm { fields, ..OptForm::default() }
}

/// 一个数值参数在网格里有几个取值（枚举 / 布尔 = 选项数）。
fn grid_count(p: &Param, f: &OptField) -> Result<usize, String> {
    if let Some(ch) = &p.choices {
        return Ok(ch.len());
    }
    if p.kind == "boolean" {
        return Ok(2);
    }
    let (lo, hi) = (num(&f.low, "下限")?, num(&f.high, "上限")?);
    let step = f.step.trim().parse::<f64>().map_err(|_| "网格要步长".to_string())?;
    if step <= 0.0 {
        return Err("步长要 > 0".into());
    }
    Ok(((hi - lo) / step + 1e-9).floor() as usize + 1)
}

fn num(s: &str, what: &str) -> Result<f64, String> {
    s.trim().parse::<f64>().map_err(|_| format!("{what}要数字"))
}

/// 表单 → 优化规格（与 `factory.lab.optimize.StudySpec` 同形）。
/// `fixed` = 详情表单里改过的参数（搜索的参数从里面剔掉）；`data` = 回测窗口覆盖。
pub fn study_spec(e: &Entry, form: &OptForm, fixed: &serde_json::Map<String, Value>, data: Option<Value>, note: &str) -> Result<Value, String> {
    let mut space = vec![];
    let mut grid: usize = 1;
    for p in e.params.iter().filter(|p| searchable(p)) {
        let Some(f) = form.fields.get(&p.name).filter(|f| f.on) else { continue };
        if p.choices.is_some() || p.kind == "boolean" {
            space.push(json!({"name": p.name}));
        } else {
            let (lo, hi) = (num(&f.low, &format!("{} 下限", p.name))?, num(&f.high, &format!("{} 上限", p.name))?);
            if lo >= hi {
                return Err(format!("{}：下限要小于上限", p.name));
            }
            if let Some(m) = p.minimum
                && lo < m
            {
                return Err(format!("{}：下限不能小于注解的 {m}", p.name));
            }
            if let Some(m) = p.maximum
                && hi > m
            {
                return Err(format!("{}：上限不能大于注解的 {m}", p.name));
            }
            let mut sp = json!({"name": p.name, "low": lo, "high": hi});
            if !f.step.trim().is_empty() {
                sp["step"] = json!(num(&f.step, &format!("{} 步长", p.name))?);
            }
            space.push(sp);
        }
        if form.sampler == "grid" {
            grid = grid.saturating_mul(grid_count(p, f).map_err(|e| format!("{}：{e}", p.name))?);
        }
    }
    if space.is_empty() {
        return Err("至少勾选一个要搜索的参数".into());
    }
    if form.sampler == "grid" && grid > MAX_GRID {
        return Err(format!("网格共 {grid} 组，超过 {MAX_GRID}：收窄范围或加大步长，或改用 TPE"));
    }
    let trials: usize = form.trials.trim().parse().map_err(|_| "尝试次数要整数".to_string())?;
    if !(1..=MAX_GRID).contains(&trials) {
        return Err(format!("尝试次数要在 1..{MAX_GRID}"));
    }
    let workers: usize = form.workers.trim().parse().map_err(|_| "并发要整数".to_string())?;
    let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
    if workers < 1 || workers > cpus.saturating_sub(1).max(1) {
        return Err(format!("并发要在 1..{}", cpus.saturating_sub(1).max(1)));
    }
    let searched: Vec<&str> = space.iter().filter_map(|s| s["name"].as_str()).collect();
    let params: serde_json::Map<String, Value> =
        fixed.iter().filter(|(k, _)| !searched.contains(&k.as_str())).map(|(k, v)| (k.clone(), v.clone())).collect();
    let mut spec = json!({
        "strategy": e.id, "space": space, "params": params,
        "sampler": form.sampler, "n_trials": if form.sampler == "grid" { grid.min(trials) } else { trials },
        "workers": workers, "objective": form.objective, "engine": form.engine,
    });
    if let Some(d) = data {
        spec["data"] = d;
    }
    if !note.trim().is_empty() {
        spec["note"] = Value::String(note.trim().into());
    }
    if let Some(a) = &form.assist_id {
        spec["assist_id"] = Value::String(a.clone());
    }
    Ok(spec)
}

/// 网格模式下的总组数（表单上实时显示；算不出来返回 None）。
pub fn grid_estimate(e: &Entry, form: &OptForm) -> Option<usize> {
    let mut n: usize = 1;
    let mut any = false;
    for p in e.params.iter().filter(|p| searchable(p)) {
        let Some(f) = form.fields.get(&p.name).filter(|f| f.on) else { continue };
        n = n.saturating_mul(grid_count(p, f).ok()?);
        any = true;
    }
    any.then_some(n)
}

// ── 研究库 studies 表 → 视图 ───────────────────────────────────────────

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Gate {
    pub passed: Option<bool>,
    #[serde(default)]
    pub why: String,
    pub dsr: Option<f64>,
    pub n_trials: Option<f64>,
    pub periods: Option<f64>,
    pub sr_annual: Option<f64>,
    pub pbo: Option<f64>,
    pub splits: Option<f64>,
    pub mean_ratio: Option<f64>,
    pub min_ratio: Option<f64>,
    pub neighbors: Option<f64>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Heat {
    pub x: String,
    pub y: String,
    pub xs: Vec<Value>,
    pub ys: Vec<Value>,
    pub z: Vec<Vec<Option<f64>>>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct TrialRow {
    pub number: i64,
    #[serde(default)]
    pub params: Value,
    pub value: Option<f64>,
    #[serde(default)]
    pub run_id: Option<String>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Analysis {
    #[serde(default)]
    pub dsr: Option<Gate>,
    #[serde(default)]
    pub pbo: Option<Gate>,
    #[serde(default)]
    pub plateau: Option<Gate>,
    #[serde(default)]
    pub importance: BTreeMap<String, f64>,
    #[serde(default)]
    pub heatmap: Option<Heat>,
    #[serde(default)]
    pub trials: Vec<TrialRow>,
    #[serde(default)]
    pub returns_basis: Vec<String>,
    #[serde(default)]
    pub why: String,
}

#[derive(Clone, Debug, Default)]
pub struct StudyRow {
    pub study_id: String,
    pub status: String,
    pub sampler: String,
    pub objective: String,
    pub n_trials: i64,
    pub n_done: i64,
    pub n_failed: i64,
    pub best_value: Option<f64>,
    pub best_run_id: Option<String>,
    pub best_params: Value,
    pub analysis: Option<Analysis>,
    pub created_ts: i64,
    pub note: String,
    pub error: String,
}

impl StudyRow {
    pub fn active(&self) -> bool {
        self.status == "running"
    }
}

pub fn parse_study(r: &rusqlite::Row<'_>) -> rusqlite::Result<StudyRow> {
    let bp: Option<String> = r.get("best_params_json")?;
    let an: Option<String> = r.get("analysis_json")?;
    Ok(StudyRow {
        study_id: r.get("study_id")?,
        status: r.get("status")?,
        sampler: r.get("sampler")?,
        objective: r.get("objective")?,
        n_trials: r.get("n_trials")?,
        n_done: r.get("n_done")?,
        n_failed: r.get("n_failed")?,
        best_value: r.get("best_value")?,
        best_run_id: r.get("best_run_id")?,
        best_params: bp.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null),
        analysis: an.and_then(|s| serde_json::from_str(&s).ok()),
        created_ts: r.get("created_ts")?,
        note: r.get::<_, Option<String>>("note")?.unwrap_or_default(),
        error: r.get::<_, Option<String>>("error")?.unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(name: &str, kind: &str, lo: f64, hi: f64, step: Option<f64>, opt: bool) -> Param {
        Param {
            name: name.into(),
            kind: kind.into(),
            minimum: Some(lo),
            maximum: Some(hi),
            step,
            optimize: opt,
            ..Default::default()
        }
    }

    fn entry() -> Entry {
        Entry {
            id: "quantum_queen.v43".into(),
            params: vec![
                p("risk_level", "integer", 0.0, 6.0, Some(1.0), true),
                p("orders_max", "integer", 1.0, 1000.0, Some(5.0), true),
                Param { choices: Some(vec![json!("a"), json!("b")]), ..p("preset", "string", 0.0, 0.0, None, false) },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn form_defaults_follow_annotations() {
        let f = init_form(&entry().params);
        assert!(f.fields["risk_level"].on && !f.fields["preset"].on);
        assert_eq!((f.fields["risk_level"].low.as_str(), f.fields["risk_level"].high.as_str()), ("0", "6"));
    }

    #[test]
    fn starred_param_without_full_range_starts_unchecked() {
        // dd_value 这类只有下限的阈值：勾上就会因为没有上限而发不出去，所以缺省不勾
        let ps = vec![Param { maximum: None, ..p("dd_value", "number", 0.0, 0.0, Some(5.0), true) }];
        assert!(!init_form(&ps).fields["dd_value"].on);
    }

    #[test]
    fn grid_spec_counts_combos_and_drops_searched_from_fixed() {
        let e = entry();
        let mut f = init_form(&e.params);
        f.sampler = "grid".into();
        f.trials = "999".into();
        f.workers = "1".into();
        let o = f.fields.get_mut("orders_max").unwrap();
        (o.low, o.high, o.step) = ("20".into(), "100".into(), "40".into());
        assert_eq!(grid_estimate(&e, &f), Some(7 * 3));
        let fixed: serde_json::Map<String, Value> = [("risk_level".to_string(), json!(1)), ("preset".to_string(), json!("b"))].into_iter().collect();
        let s = study_spec(&e, &f, &fixed, None, "").unwrap();
        assert_eq!(s["n_trials"], 21, "网格的尝试数 = 组合数（不超过填写的上限）");
        assert_eq!(s["params"], json!({"preset": "b"}), "被搜索的 risk_level 不能同时作为固定参数");
        assert_eq!(s["space"][1], json!({"name": "orders_max", "low": 20.0, "high": 100.0, "step": 40.0}));
    }

    #[test]
    fn invalid_forms_are_refused() {
        let e = entry();
        let mut f = init_form(&e.params);
        f.workers = "1".into();
        f.fields.get_mut("risk_level").unwrap().low = "9".into();
        assert!(study_spec(&e, &f, &Default::default(), None, "").unwrap_err().contains("下限要小于上限"));
        let mut f = init_form(&e.params);
        f.workers = "1".into();
        f.fields.get_mut("orders_max").unwrap().high = "5000".into();
        assert!(study_spec(&e, &f, &Default::default(), None, "").unwrap_err().contains("注解"));
        let mut f = init_form(&e.params);
        f.workers = "1".into();
        for v in f.fields.values_mut() {
            v.on = false;
        }
        assert!(study_spec(&e, &f, &Default::default(), None, "").unwrap_err().contains("至少勾选"));
        let mut f = init_form(&e.params);
        f.sampler = "grid".into();
        f.workers = "1".into();
        f.fields.get_mut("orders_max").unwrap().step = "1".into();
        assert!(study_spec(&e, &f, &Default::default(), None, "").unwrap_err().contains("超过"));
    }
}
