//! 特征库面板 — 只读数据（docs/30）。
//!
//! 读 `~/ws-data/cockpit/feature_lab.json`，由主仓 `factory.prediction.feature_lab`
//! 批量算好落盘。**零交易所连接。**

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

/// 一个注册特征的元数据。`hypothesis` 是必填——说不出机制假设的特征不该进库。
#[derive(Clone, Default)]
pub struct Reg {
    pub name: String,
    pub family: String,
    pub source: String,
    pub window_s: Option<i64>,
    pub doc: String,
    pub hypothesis: String,
    pub prior: i64,
    pub control: bool,
}

/// 覆盖健康。**面板上最该先看的是这张表**：NaN 率 40% 的特征，
/// 它的「显著性」多半来自剩下 60% 的选择偏差。
#[derive(Clone, Default)]
pub struct Cov {
    pub name: String,
    pub family: String,
    pub present: bool,
    pub cover: f64,
    pub p05: Option<f64>,
    pub p50: Option<f64>,
    pub p95: Option<f64>,
    pub degenerate: bool,
    pub control: bool,
}

/// 一个特征的条件检验结果。
#[derive(Clone, Default)]
pub struct Row {
    pub feature: String,
    pub family: String,
    pub coef: f64,
    pub ci_lo: f64,
    pub ci_hi: f64,
    pub p: f64,
    pub pts_per_sd: f64,
    pub n_round: i64,
    pub n_snap: i64,
    /// p<0.05。**单看它没有意义**——35 个检验里总会有几个。
    pub naive_sig: bool,
    /// Benjamini–Hochberg 之后仍然通过。这个才算数。
    pub fdr_pass: bool,
}

#[derive(Clone, Default)]
pub struct Power {
    pub n_tests: i64,
    pub single: f64,
    pub multi: f64,
    pub have: i64,
    pub days: f64,
}

#[derive(Clone, Default)]
pub struct FeatureLab {
    pub present: bool,
    pub ready: bool,
    pub reason: String,
    pub stamp: String,
    pub n_round: i64,
    pub n_snap: i64,
    pub base_rate: f64,
    pub registry: Vec<Reg>,
    pub coverage: Vec<Cov>,
    pub rows: Vec<Row>,
    pub n_tests: i64,
    pub naive_hits: i64,
    pub fdr_hits: i64,
    pub expected_by_chance: f64,
    pub q: f64,
    pub power: Power,
    pub refreshed: String,
}

static CACHE: OnceLock<Mutex<(Option<SystemTime>, FeatureLab)>> = OnceLock::new();

pub fn board_path() -> PathBuf {
    std::env::var("WS_FEATURE_BOARD").map(PathBuf::from).unwrap_or_else(|_| {
        super::paths::data_dir().join("cockpit").join("feature_lab.json")
    })
}

pub fn snapshot() -> FeatureLab {
    let lock = CACHE.get_or_init(|| Mutex::new((None, FeatureLab::default())));
    let Ok(mut g) = lock.lock() else { return FeatureLab::default() };
    let p = board_path();
    let mt = std::fs::metadata(&p).ok().and_then(|m| m.modified().ok());
    if g.0.is_some() && g.0 == mt {
        return g.1.clone();
    }
    let mut v = match std::fs::read_to_string(&p) {
        Ok(t) => parse(&t),
        Err(_) => FeatureLab::default(),
    };
    v.refreshed = chrono::Local::now().format("%H:%M:%S").to_string();
    *g = (mt, v.clone());
    v
}

pub fn parse(text: &str) -> FeatureLab {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return FeatureLab::default();
    };
    let mut out = FeatureLab {
        present: true,
        ready: v["ready"].as_bool().unwrap_or(false),
        reason: v["reason"].as_str().unwrap_or_default().to_string(),
        stamp: v["stamp"].as_str().unwrap_or_default().to_string(),
        n_round: v["n_round"].as_i64().unwrap_or(0),
        n_snap: v["n_snap"].as_i64().unwrap_or(0),
        base_rate: v["base_rate"].as_f64().unwrap_or(0.0),
        ..Default::default()
    };
    for r in v.get("registry").and_then(|x| x.as_array()).into_iter().flatten() {
        out.registry.push(Reg {
            name: r["name"].as_str().unwrap_or_default().to_string(),
            family: r["family"].as_str().unwrap_or_default().to_string(),
            source: r["source"].as_str().unwrap_or_default().to_string(),
            window_s: r["window_s"].as_i64(),
            doc: r["doc"].as_str().unwrap_or_default().to_string(),
            hypothesis: r["hypothesis"].as_str().unwrap_or_default().to_string(),
            prior: r["prior"].as_i64().unwrap_or(0),
            control: r["control"].as_bool().unwrap_or(false),
        });
    }
    for c in v.get("coverage").and_then(|x| x.as_array()).into_iter().flatten() {
        out.coverage.push(Cov {
            name: c["name"].as_str().unwrap_or_default().to_string(),
            family: c["family"].as_str().unwrap_or_default().to_string(),
            present: c["present"].as_bool().unwrap_or(false),
            cover: c["cover"].as_f64().unwrap_or(0.0),
            p05: c["p05"].as_f64(),
            p50: c["p50"].as_f64(),
            p95: c["p95"].as_f64(),
            degenerate: c["degenerate"].as_bool().unwrap_or(false),
            control: c["control"].as_bool().unwrap_or(false),
        });
    }
    let e = &v["eval"];
    for r in e.get("rows").and_then(|x| x.as_array()).into_iter().flatten() {
        out.rows.push(Row {
            feature: r["feature"].as_str().unwrap_or_default().to_string(),
            family: r["family"].as_str().unwrap_or_default().to_string(),
            coef: r["coef"].as_f64().unwrap_or(0.0),
            ci_lo: r["ci_lo"].as_f64().unwrap_or(0.0),
            ci_hi: r["ci_hi"].as_f64().unwrap_or(0.0),
            p: r["p"].as_f64().unwrap_or(1.0),
            pts_per_sd: r["pts_per_sd"].as_f64().unwrap_or(0.0),
            n_round: r["n_round"].as_i64().unwrap_or(0),
            n_snap: r["n_snap"].as_i64().unwrap_or(0),
            naive_sig: r["naive_sig"].as_bool().unwrap_or(false),
            fdr_pass: r["fdr_pass"].as_bool().unwrap_or(false),
        });
    }
    out.n_tests = e["n_tests"].as_i64().unwrap_or(0);
    out.naive_hits = e["naive_hits"].as_i64().unwrap_or(0);
    out.fdr_hits = e["fdr_hits"].as_i64().unwrap_or(0);
    out.expected_by_chance = e["expected_by_chance"].as_f64().unwrap_or(0.0);
    out.q = e["q"].as_f64().unwrap_or(0.1);
    let p = &v["power"];
    out.power = Power {
        n_tests: p["n_tests"].as_i64().unwrap_or(0),
        single: p["rounds_for_2pts_single"].as_f64().unwrap_or(0.0),
        multi: p["rounds_for_2pts_multi"].as_f64().unwrap_or(0.0),
        have: p["rounds_have"].as_i64().unwrap_or(0),
        days: p["days_needed"].as_f64().unwrap_or(0.0),
    };
    out
}

/// 样本是否够判定。**这是面板上第一句该说的话**——不够的时候，
/// 下面那张检验表里的任何「显著」都不该被当成发现。
pub fn sample_sufficient(f: &FeatureLab) -> bool {
    f.power.multi > 0.0 && (f.power.have as f64) >= f.power.multi
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"ready":true,"stamp":"2026-09-20T01:00:00Z","n_round":300,
      "n_snap":90000,"base_rate":0.52,
      "registry":[{"name":"timb_30s","family":"flow","source":"spot1s","window_s":30,
                   "doc":"d","hypothesis":"h","prior":1,"control":false}],
      "coverage":[{"name":"timb_30s","family":"flow","present":true,"cover":0.99,
                   "p05":-0.8,"p50":0.0,"p95":0.8,"degenerate":false,"control":false}],
      "eval":{"rows":[{"feature":"timb_30s","family":"flow","coef":0.3,"ci_lo":0.05,
                       "ci_hi":0.55,"p":0.02,"pts_per_sd":0.03,"n_round":300,
                       "n_snap":90000,"naive_sig":true,"fdr_pass":false}],
              "n_tests":35,"naive_hits":3,"fdr_hits":0,"q":0.1,"expected_by_chance":1.75},
      "power":{"n_tests":35,"rounds_for_2pts_single":9811,"rounds_for_2pts_multi":20306,
               "rounds_have":300,"days_needed":70.5}}"#;

    #[test]
    fn 解析完整面板() {
        let f = parse(SAMPLE);
        assert!(f.present && f.ready);
        assert_eq!(f.registry.len(), 1);
        assert_eq!(f.rows.len(), 1);
        assert_eq!(f.n_tests, 35);
        assert_eq!(f.power.multi as i64, 20306);
    }

    #[test]
    fn 朴素显著与fdr通过是两件事() {
        let f = parse(SAMPLE);
        let r = &f.rows[0];
        // p=0.02 < 0.05 故朴素显著，但 35 个检验做 BH 之后没通过
        assert!(r.naive_sig && !r.fdr_pass);
        assert!(f.naive_hits > f.fdr_hits, "面板必须能显示出多重比较吃掉了多少");
    }

    #[test]
    fn 样本不足要判得出来() {
        let f = parse(SAMPLE);
        assert!(!sample_sufficient(&f), "300 轮远不够 20306，必须判为不足");
    }

    #[test]
    fn 坏json不当成已就绪() {
        let f = parse("{不是 json");
        assert!(!f.present && !f.ready && f.rows.is_empty());
    }

    #[test]
    fn 朴素命中要能和偶然期望对照() {
        let f = parse(SAMPLE);
        // 35 个检验 × 0.05 = 1.75 —— 命中 3 个看着多，但不是 0 的对照
        assert!((f.expected_by_chance - 1.75).abs() < 1e-9);
    }
}
