//! 策略中心 · AI 研究助理（docs/37 P6）：一句话 → `python -m factory.lab assist` → 草稿 → 填进优化表单。
//!
//! **AI 只起草，不发起**：这里能做的只有把校验通过的草稿填进「参数优化」表单，
//! 发起仍是人点「开始优化」（同一个按钮、同一套校验）。起草、校验、记账都在 Python 一处
//! （`factory.lab.assistant`）；这里是结果的数据结构和「草稿 → 表单」的纯函数（可测）。

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use super::strategy_center::{Entry, default_text};
use super::strategy_center_opt::{self as opt, OptForm};

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Trials {
    #[serde(default)]
    pub strategy_runs: u64,
    #[serde(default)]
    pub global: Option<u64>,
}

/// `factory.lab assist --json` 的结果。
#[derive(Deserialize, Clone, Debug, Default)]
pub struct Assist {
    pub assist_id: String,
    #[serde(default)]
    pub reply: String,
    #[serde(default)]
    pub feasible: bool,
    /// 有草稿且校验全过：可以填表发起
    #[serde(default)]
    pub ready: bool,
    #[serde(default)]
    pub spec: Option<Value>,
    #[serde(default)]
    pub errors: Vec<String>,
    #[serde(default)]
    pub cautions: Vec<String>,
    #[serde(default)]
    pub resolved: Value,
    #[serde(default)]
    pub n_calls: u64,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub trials: Trials,
}

impl Assist {
    /// 草稿一句话摘要：引擎 · 搜索方式 · 次数 · 目标 · 窗口 · 空间。
    pub fn brief(&self) -> Option<String> {
        let s = self.spec.as_ref()?;
        let space = s["space"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|x| {
                        let n = x["name"].as_str().unwrap_or("?");
                        match (x["low"].as_f64(), x["high"].as_f64()) {
                            (Some(l), Some(h)) => format!("{n}∈[{l}, {h}]{}", x["step"].as_f64().map(|st| format!("/{st}")).unwrap_or_default()),
                            _ => n.to_string(),
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("，")
            })
            .unwrap_or_default();
        let n = self.resolved["n_trials"].as_u64().or_else(|| s["n_trials"].as_u64()).unwrap_or(0);
        Some(format!(
            "{} · {} · {n} 次 · 目标 {} · {}…{} · {space}",
            if s["engine"] == "quick" { "快速回测" } else { "完整回测" },
            s["sampler"].as_str().unwrap_or("?"),
            s["objective"].as_str().unwrap_or("?"),
            s["data"]["start"].as_str().unwrap_or("缺省"),
            s["data"]["end"].as_str().unwrap_or("缺省"),
        ))
    }
}

fn num_text(v: &Value, integer: bool) -> String {
    match v.as_f64() {
        Some(x) if integer => format!("{}", x.round() as i64),
        Some(x) => format!("{x}"),
        None => String::new(),
    }
}

/// 草稿 → 表单。表单先回到策略缺省，再按草稿填：搜索的参数勾上并填范围，固定参数写进详情表单，
/// 窗口写进回测窗口，`assist_id` 跟着表单走（发起时带上，研究库里能查到这次优化来自哪次 AI 调用）。
/// 返回填不进表单的部分（如只搜部分枚举值、换了标的）——要在界面上说清，不悄悄丢掉。
pub fn apply(
    spec: &Value,
    assist_id: &str,
    e: &Entry,
    form: &mut BTreeMap<String, String>,
    of: &mut OptForm,
    start: &mut String,
    end: &mut String,
) -> Result<Vec<String>, String> {
    let space = spec["space"].as_array().ok_or("草稿里没有搜索空间")?;
    let mut notes = vec![];
    *form = e.params.iter().map(|p| (p.name.clone(), default_text(p))).collect();
    *of = opt::init_form(&e.params);
    for f in of.fields.values_mut() {
        f.on = false;
    }
    for sp in space {
        let name = sp["name"].as_str().ok_or("搜索空间里有参数没名字")?;
        let p = e.params.iter().find(|p| p.name == name).ok_or(format!("策略没有参数 {name}"))?;
        let f = of.fields.get_mut(name).ok_or(format!("{name} 不能搜索"))?;
        f.on = true;
        let int = p.kind == "integer";
        if !sp["low"].is_null() {
            f.low = num_text(&sp["low"], int);
        }
        if !sp["high"].is_null() {
            f.high = num_text(&sp["high"], int);
        }
        f.step = if sp["step"].is_null() { String::new() } else { num_text(&sp["step"], int) };
        if let (Some(ch), Some(all)) = (sp["choices"].as_array(), &p.choices)
            && ch.len() < all.len()
        {
            notes.push(format!("{name}：草稿只搜 {} 个取值，表单只能搜全部 {} 个", ch.len(), all.len()));
        }
    }
    if let Some(ps) = spec["params"].as_object() {
        for (k, v) in ps {
            if !form.contains_key(k) {
                return Err(format!("策略没有参数 {k}"));
            }
            form.insert(k.clone(), match v {
                Value::String(s) => s.clone(),
                Value::Null => String::new(),
                other => other.to_string(),
            });
        }
    }
    for (key, slot) in [("sampler", &mut of.sampler), ("objective", &mut of.objective), ("engine", &mut of.engine)] {
        if let Some(s) = spec[key].as_str() {
            *slot = s.to_string();
        }
    }
    if let Some(n) = spec["n_trials"].as_u64() {
        of.trials = n.to_string();
    }
    if let Some(n) = spec["workers"].as_u64() {
        of.workers = n.to_string();
    }
    *start = spec["data"]["start"].as_str().map(str::to_string).unwrap_or_else(|| e.bt("start"));
    *end = spec["data"]["end"].as_str().map(str::to_string).unwrap_or_else(|| e.bt("end"));
    if let Some(sy) = spec["data"]["symbols"].as_array() {
        let want = sy.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("+");
        if !want.is_empty() && want != e.bt("symbols") {
            notes.push(format!("草稿要用 {want}，表单只能用策略声明的 {}", e.bt("symbols")));
        }
    }
    of.assist_id = Some(assist_id.to_string());
    Ok(notes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ws::strategy_center::Param;
    use serde_json::json;

    fn entry() -> Entry {
        let p = |name: &str, kind: &str, default: Value, lo: f64, hi: f64| Param {
            name: name.into(),
            kind: kind.into(),
            default,
            minimum: Some(lo),
            maximum: Some(hi),
            optimize: true,
            ..Default::default()
        };
        Entry {
            id: "orderflow.ofi_direction".into(),
            backtest: Some(json!({"symbols": "BTCUSDT", "start": "2026-06-06"})),
            params: vec![p("entry_z", "number", json!(1.5), 0.25, 5.0), p("max_hold_secs", "integer", json!(60), 1.0, 3600.0),
                         p("warmup_n", "integer", json!(600), 10.0, 5000.0)],
            ..Default::default()
        }
    }

    #[test]
    fn draft_fills_the_form_and_carries_assist_id() {
        let spec = json!({"space": [{"name": "entry_z", "low": 3.0, "high": 5.0, "step": 0.5},
                                    {"name": "max_hold_secs", "low": 180.0, "high": 900.0, "step": 180.0}],
                          "params": {"warmup_n": 1200}, "data": {"start": "2026-06-01", "end": "2026-06-20", "symbols": ["BTCUSDT"]},
                          "sampler": "grid", "n_trials": 25, "engine": "quick", "workers": 4, "objective": "net_bp_mean"});
        let e = entry();
        let (mut form, mut of, mut s, mut t) = (BTreeMap::new(), OptForm::default(), String::new(), String::new());
        let notes = apply(&spec, "A-1", &e, &mut form, &mut of, &mut s, &mut t).unwrap();
        assert!(notes.is_empty());
        let f = &of.fields["max_hold_secs"];
        assert!(f.on && f.low == "180" && f.high == "900" && f.step == "180", "整数参数按整数写");
        assert!(!of.fields["warmup_n"].on);
        assert_eq!(form["warmup_n"], "1200");
        assert_eq!((of.engine.as_str(), of.trials.as_str(), of.objective.as_str()), ("quick", "25", "net_bp_mean"));
        assert_eq!((s.as_str(), t.as_str()), ("2026-06-01", "2026-06-20"));
        assert_eq!(of.assist_id.as_deref(), Some("A-1"));
        // 填好的表单发出去的规格带 assist_id
        let st = opt::study_spec(&e, &of, &Default::default(), None, "").unwrap();
        assert_eq!(st["assist_id"], "A-1");
        assert_eq!(st["n_trials"], 25);
    }

    #[test]
    fn unknown_params_and_unrepresentable_parts_are_reported() {
        let e = entry();
        let (mut form, mut of, mut s, mut t) = (BTreeMap::new(), OptForm::default(), String::new(), String::new());
        let bad = json!({"space": [{"name": "nope"}]});
        assert!(apply(&bad, "A-1", &e, &mut form, &mut of, &mut s, &mut t).unwrap_err().contains("nope"));
        let other = json!({"space": [{"name": "entry_z", "low": 1.0, "high": 2.0}], "data": {"symbols": ["ETHUSDT"]}});
        let notes = apply(&other, "A-2", &e, &mut form, &mut of, &mut s, &mut t).unwrap();
        assert!(notes[0].contains("ETHUSDT"));
        assert_eq!(s, "2026-06-06", "草稿没给窗口：回到策略声明");
    }
}
