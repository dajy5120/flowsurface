//! 策略中心「模块」页（docs/44 W6）：策略工作流引擎（SWE）的模块注册中心。
//!
//! 数据只有一个来源：`python -m strategies.swe.module --json`（模块的说明、参数、被哪些工作流引用、
//! 验证记录，外加最近的消融流水账）。面板不解析工作流、不判断模块好坏——只显示。
//!
//! 这一页与左栏选中的策略无关：它看的是整个模块库。

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct ModParam {
    pub name: String,
    pub required: bool,
    /// 缺省值的 JSON 文本（必填参数为空）
    pub default: String,
    pub description: String,
    /// 标了「值得优化」
    pub optimize: bool,
}

/// 一条验证记录：这个模块在哪个市场、哪段数据上验证过、结论是什么。
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct ModValidation {
    pub market: String,
    pub data: String,
    pub verdict: String,
    pub evidence: String,
    pub date: String,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Module {
    #[serde(rename = "ref")]
    pub refer: String,
    pub id: String,
    pub version: String,
    pub kind: String,
    pub category: String,
    pub path: String,
    pub error: String,
    pub origin: String,
    pub markets: Vec<String>,
    pub doc: String,
    pub params: Vec<ModParam>,
    pub used_by: Vec<String>,
    pub validations: Vec<ModValidation>,
}

/// 一次消融对比（`python -m strategies.swe.ablate` 记的流水账）。
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Ablation {
    pub ts: i64,
    pub workflow: String,
    pub range: Vec<String>,
    pub set: Vec<String>,
    pub swap: Vec<String>,
    pub note: String,
    pub base: Value,
    pub variant: Value,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Registry {
    pub modules: Vec<Module>,
    pub ablations: Vec<Ablation>,
    pub n_ablations: usize,
}

/// 环节（顺序 = 生命周期里的先后；与 `strategies/swe/module.py` 的 KINDS 一致）。
pub const KINDS: [(&str, &str); 11] = [
    ("features", "特征"),
    ("regime", "市场状态"),
    ("router", "路由"),
    ("scale", "缩放"),
    ("setup", "信号"),
    ("confirm", "确认 / 过滤"),
    ("entry", "入场"),
    ("stop", "止损"),
    ("sizing", "仓位"),
    ("exit", "出场 / 持仓管理"),
    ("risk", "组合风控"),
];

pub fn kind_label(k: &str) -> &'static str {
    KINDS.iter().find(|(id, _)| *id == k).map(|(_, l)| *l).unwrap_or("其他")
}

pub fn category_label(c: &str) -> &'static str {
    match c {
        "pure" => "纯计算",
        "state" => "带状态",
        "decision" => "决策",
        _ => "",
    }
}

/// 验证结论的说法。`parity` 只说明「拆出来的模块与原策略行为一致」，对有没有优势不说明任何事。
pub fn verdict_label(v: &str) -> &'static str {
    match v {
        "parity" => "行为一致",
        "alive" => "存活",
        "falsified" => "已证伪",
        _ => "未验证",
    }
}

/// 消融流水账里的一个数字（期末净值、回撤……），没有就是「—」。
pub fn num(v: &Value, key: &str, dp: usize) -> String {
    v.get(key).and_then(Value::as_f64).map(|x| format!("{x:.dp$}")).unwrap_or_else(|| "—".to_string())
}

impl Registry {
    /// 按环节分组（空环节不出现）。
    pub fn grouped(&self) -> Vec<(&'static str, Vec<&Module>)> {
        let mut out = Vec::new();
        for (kind, label) in KINDS {
            let ms: Vec<&Module> = self.modules.iter().filter(|m| m.kind == kind).collect();
            if !ms.is_empty() {
                out.push((label, ms));
            }
        }
        let rest: Vec<&Module> = self.modules.iter().filter(|m| !KINDS.iter().any(|(k, _)| *k == m.kind)).collect();
        if !rest.is_empty() {
            out.push(("其他", rest));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_registry_json_and_groups_by_stage() {
        let raw = r#"{"modules": [
            {"ref": "exit.breakeven@1.0", "id": "exit.breakeven", "version": "1.0", "kind": "exit", "category": "decision",
             "path": "strategies/modules/exit/breakeven.py", "error": "", "doc": "保本",
             "params": [{"name": "start", "required": true, "default": "", "description": "启动值", "optimize": true}],
             "used_by": ["S1@gold_reaper.swe.toml"],
             "validations": [{"module": "exit.breakeven@1.0", "market": "XAUUSD", "data": "2026", "verdict": "parity",
                              "evidence": "逐笔一致", "date": "2026-10-13"}]},
            {"ref": "regime.ema_trend@1.0", "id": "regime.ema_trend", "version": "1.0", "kind": "regime", "category": "state"},
            {"ref": "x.y@1", "kind": "brand_new_stage"}],
          "ablations": [{"ts": 1, "workflow": "w.toml", "swap": ["a=b"], "base": {"equity_end": 100.5}, "variant": {}}],
          "n_ablations": 7}"#;
        let r: Registry = serde_json::from_str(raw).unwrap();
        assert_eq!(r.modules.len(), 3);
        assert_eq!(r.modules[0].refer, "exit.breakeven@1.0");
        assert!(r.modules[0].params[0].optimize && r.modules[0].params[0].required);
        assert_eq!(r.modules[0].validations[0].verdict, "parity");
        // 分组顺序跟生命周期走：市场状态在出场之前；不认识的环节归到「其他」而不是丢掉
        let g = r.grouped();
        assert_eq!(g.iter().map(|(l, ms)| (*l, ms.len())).collect::<Vec<_>>(), vec![("市场状态", 1), ("出场 / 持仓管理", 1), ("其他", 1)]);
        assert_eq!(r.n_ablations, 7);
        assert_eq!(num(&r.ablations[0].base, "equity_end", 2), "100.50");
        assert_eq!(num(&r.ablations[0].variant, "equity_end", 2), "—");
        assert_eq!(verdict_label("parity"), "行为一致");
    }
}
