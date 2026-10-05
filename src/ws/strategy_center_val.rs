//! 策略中心 · 验证与实验（docs/37 §7）：闸门清单、成本压力 / 参数扰动 / Walk-Forward、实验（预注册）。
//!
//! 纯数据与纯函数（可测）；状态与 IO 在 [`super::strategy_center`]。验证在
//! `python -m factory.lab.validate`（经 ws-control 的 `run_validation`），闸门由
//! `python -m factory.lab gates --json` 算——**判定规则只在 Python 一处**，面板只显示。

use serde::Deserialize;
use serde_json::{Value, json};

use super::strategy_center::Entry;

pub const KINDS: [(&str, &str); 3] = [("stress", "成本压力"), ("perturb", "参数扰动"), ("wfo", "Walk-Forward")];

pub fn kind_label(k: &str) -> &'static str {
    KINDS.iter().find(|(x, _)| *x == k).map(|(_, l)| *l).unwrap_or("验证")
}

#[derive(Clone, Debug, Default)]
pub struct ValRow {
    pub val_id: String,
    pub kind: String,
    pub status: String,
    pub passed: Option<bool>,
    pub result: Value,
    pub created_ts: i64,
    pub error: String,
    pub note: String,
}

impl ValRow {
    pub fn active(&self) -> bool {
        self.status == "running"
    }
    pub fn why(&self) -> String {
        self.result.get("why").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| self.error.clone())
    }
}

pub fn parse_val(r: &rusqlite::Row<'_>) -> rusqlite::Result<ValRow> {
    let res: Option<String> = r.get("result_json")?;
    Ok(ValRow {
        val_id: r.get("val_id")?,
        kind: r.get("kind")?,
        status: r.get("status")?,
        passed: r.get::<_, Option<i64>>("passed")?.map(|x| x != 0),
        result: res.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null),
        created_ts: r.get("created_ts")?,
        error: r.get::<_, Option<String>>("error")?.unwrap_or_default(),
        note: r.get::<_, Option<String>>("note")?.unwrap_or_default(),
    })
}

#[derive(Clone, Debug, Default)]
pub struct ExpRow {
    pub exp_id: String,
    pub hypothesis: String,
    pub dd_limit: Option<f64>,
    pub holdout: Option<(String, String)>,
    pub holdout_run_id: Option<String>,
    pub status: String,
    pub verdict: Option<String>,
    pub conclusion: String,
    pub created_ts: i64,
}

impl ExpRow {
    pub fn locked(&self) -> bool {
        self.status == "locked"
    }
}

pub fn parse_exp(r: &rusqlite::Row<'_>) -> rusqlite::Result<ExpRow> {
    let rules: String = r.get("rules_json")?;
    let rules: Value = serde_json::from_str(&rules).unwrap_or(Value::Null);
    let h: Option<String> = r.get("holdout_json")?;
    let h: Option<Value> = h.and_then(|s| serde_json::from_str(&s).ok());
    Ok(ExpRow {
        exp_id: r.get("exp_id")?,
        hypothesis: r.get("hypothesis")?,
        dd_limit: rules.get("max_dd_limit_pct").and_then(Value::as_f64),
        holdout: h.and_then(|h| Some((h.get("start")?.as_str()?.to_string(), h.get("end")?.as_str()?.to_string()))),
        holdout_run_id: r.get("holdout_run_id")?,
        status: r.get("status")?,
        verdict: r.get("verdict")?,
        conclusion: r.get::<_, Option<String>>("conclusion")?.unwrap_or_default(),
        created_ts: r.get("created_ts")?,
    })
}

/// `factory.lab gates --json` 的结果。
#[derive(Deserialize, Clone, Debug, Default)]
pub struct Gate {
    pub id: String,
    pub name: String,
    pub status: String,
    #[serde(default)]
    pub value: Value,
    #[serde(default)]
    pub why: String,
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Gates {
    #[serde(default)]
    pub gates: Vec<Gate>,
    #[serde(default)]
    pub verdict: String,
    #[serde(default)]
    pub experiment_id: Option<String>,
    /// 留出数据那一次（研究结束后只跑一次；单列，不计入 G1–G7）
    #[serde(default)]
    pub holdout: Option<Gate>,
}

/// 发起验证的表单（WFO 的训练 / 测试天数；其余沿用详情表单与优化表单）。
#[derive(Debug, Clone)]
pub struct ValForm {
    pub train_days: String,
    pub test_days: String,
}

impl Default for ValForm {
    fn default() -> Self {
        Self { train_days: "60".into(), test_days: "30".into() }
    }
}

/// 新建实验的表单。
#[derive(Debug, Clone, Default)]
pub struct ExpForm {
    pub hypothesis: String,
    pub dd_limit: String,
    pub holdout_start: String,
    pub holdout_end: String,
    pub verdict: String,
    pub conclusion: String,
    /// 跑留出数据要点两次（只许一次的事，不能手滑）
    pub holdout_armed: bool,
}

fn date_ok(s: &str) -> bool {
    chrono::NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").is_ok()
}

/// 验证规格。`fixed` = 详情表单里改过的参数；`window` = 回测窗口覆盖；`study` = 优化表单生成的规格（WFO 借它的搜索空间）。
#[allow(clippy::too_many_arguments)]
pub fn validation_spec(
    e: &Entry,
    kind: &str,
    fixed: &serde_json::Map<String, Value>,
    start: &str,
    end: &str,
    study: Option<&Value>,
    form: &ValForm,
    workers: &str,
    objective: &str,
    exp_id: Option<&str>,
    note: &str,
) -> Result<Value, String> {
    let (start, end) = (start.trim(), end.trim());
    let mut data = serde_json::Map::new();
    if !start.is_empty() {
        data.insert("start".into(), Value::String(start.into()));
    }
    if !end.is_empty() {
        data.insert("end".into(), Value::String(end.into()));
    }
    let workers: u64 = workers.trim().parse().map_err(|_| "并发要整数".to_string())?;
    let mut spec = json!({"strategy": e.id, "kind": kind, "params": fixed, "data": data,
                          "workers": workers, "objective": objective});
    if kind == "wfo" {
        if start.is_empty() || end.is_empty() || !date_ok(start) || !date_ok(end) {
            return Err("Walk-Forward 要在「回测窗口」里写明开始和结束日期（整个研究区间）".into());
        }
        let tr: u64 = form.train_days.trim().parse().map_err(|_| "训练天数要整数".to_string())?;
        let te: u64 = form.test_days.trim().parse().map_err(|_| "测试天数要整数".to_string())?;
        if tr < 5 || te < 2 {
            return Err("训练至少 5 天、测试至少 2 天".into());
        }
        let st = study.ok_or("Walk-Forward 每个训练窗要做一次优化：先在「参数优化」里勾好参数（表单要能发出去）")?;
        spec["train_days"] = json!(tr);
        spec["test_days"] = json!(te);
        spec["space"] = st["space"].clone();
        spec["sampler"] = st["sampler"].clone();
        spec["n_trials"] = st["n_trials"].clone();
        // 被搜索的参数不能同时作为固定参数
        if let (Some(ps), Some(sp)) = (spec["params"].as_object_mut(), st["space"].as_array()) {
            for s in sp {
                if let Some(n) = s["name"].as_str() {
                    ps.remove(n);
                }
            }
        }
    }
    if let Some(x) = exp_id {
        spec["experiment_id"] = Value::String(x.into());
    }
    if !note.trim().is_empty() {
        spec["note"] = Value::String(note.trim().into());
    }
    Ok(spec)
}

/// 新建实验的规格（`python -m factory.lab exp new --spec-json`）。
pub fn experiment_spec(e: &Entry, f: &ExpForm) -> Result<Value, String> {
    if f.hypothesis.trim().is_empty() {
        return Err("先写下假设：你认为这个策略会怎样".into());
    }
    let mut spec = json!({"strategy": e.id, "hypothesis": f.hypothesis.trim()});
    if !f.dd_limit.trim().is_empty() {
        let d: f64 = f.dd_limit.trim().parse().map_err(|_| "回撤上限要数字（%）".to_string())?;
        if !(0.0..=100.0).contains(&d) || d == 0.0 {
            return Err("回撤上限要在 0–100（%）之间".into());
        }
        spec["max_dd_limit_pct"] = json!(d);
    }
    let (hs, he) = (f.holdout_start.trim(), f.holdout_end.trim());
    match (hs.is_empty(), he.is_empty()) {
        (true, true) => {}
        (false, false) => {
            if !date_ok(hs) || !date_ok(he) {
                return Err("留出窗口日期要 YYYY-MM-DD".into());
            }
            if he < hs {
                return Err("留出窗口结束早于开始".into());
            }
            spec["holdout"] = json!({"start": hs, "end": he});
        }
        _ => return Err("留出窗口要开始和结束都填（或都不填）".into()),
    }
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e() -> Entry {
        Entry { id: "quantum_queen.v43".into(), ..Default::default() }
    }

    #[test]
    fn wfo_needs_full_window_and_borrows_search_space() {
        let fixed: serde_json::Map<String, Value> = [("risk_level".to_string(), json!(1)), ("preset".to_string(), json!("b"))].into_iter().collect();
        let f = ValForm::default();
        assert!(validation_spec(&e(), "wfo", &fixed, "", "2026-07-31", None, &f, "2", "calmar", None, "").unwrap_err().contains("开始和结束"));
        let st = json!({"space": [{"name": "risk_level", "low": 1, "high": 3, "step": 2}], "sampler": "grid", "n_trials": 2});
        let s = validation_spec(&e(), "wfo", &fixed, "2026-01-01", "2026-07-31", Some(&st), &f, "2", "calmar", Some("E-1"), "").unwrap();
        assert_eq!(s["params"], json!({"preset": "b"}), "被搜索的 risk_level 不能同时固定");
        assert_eq!(s["train_days"], 60);
        assert_eq!(s["experiment_id"], "E-1");
    }

    #[test]
    fn stress_spec_is_simple() {
        let s = validation_spec(&e(), "stress", &Default::default(), "", "", None, &ValForm::default(), "3", "calmar", None, " 看看 ").unwrap();
        assert_eq!(s, json!({"strategy": "quantum_queen.v43", "kind": "stress", "params": {}, "data": {},
                              "workers": 3, "objective": "calmar", "note": "看看"}));
    }

    #[test]
    fn experiment_form_rules() {
        let mut f = ExpForm::default();
        assert!(experiment_spec(&e(), &f).unwrap_err().contains("假设"));
        f.hypothesis = "回撤 < 40%".into();
        f.holdout_start = "2026-08-01".into();
        assert!(experiment_spec(&e(), &f).unwrap_err().contains("都填"));
        f.holdout_end = "2026-10-02".into();
        f.dd_limit = "40".into();
        let s = experiment_spec(&e(), &f).unwrap();
        assert_eq!(s["holdout"], json!({"start": "2026-08-01", "end": "2026-10-02"}));
        assert_eq!(s["max_dd_limit_pct"], 40.0);
    }
}
