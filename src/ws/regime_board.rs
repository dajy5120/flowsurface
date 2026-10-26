//! 「市场状态」面板的数据（docs/62 F5）：时间线文件上的**描述性**统计 + 检验台账。
//!
//! 只做描述：各状态占比、平均持续、每天切换、转移矩阵。**不在面板里算「状态之后的收益」**——
//! 那本身就是一次检验，放在面板里随手看，会让之后要写预注册的数据（例如期货日线）先被看过；
//! 状态有没有用只看检验框架（`python -m factory.regime.validate`）写进台账的结果。

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use super::regime::RegimeFile;

const DAY: f64 = 86_400_000.0;

/// 一个维度在 [lo, hi) 里的统计。
#[derive(Debug, Clone, PartialEq)]
pub struct DimStats {
    pub key: String,
    /// 每个状态：(状态下标, 时间占比 0–1, 段数, 平均持续小时)，按占比从大到小
    pub states: Vec<(usize, f64, usize, f64)>,
    /// 段起点落在范围里的段数 ÷ 天数
    pub switches_per_day: f64,
}

/// 各维度的统计（段按与 [lo, hi) 的重叠部分算时长）。
#[must_use]
pub fn dim_stats(f: &RegimeFile, lo: u64, hi: u64) -> Vec<DimStats> {
    let days = ((hi.saturating_sub(lo)) as f64 / DAY).max(1e-9);
    f.dims
        .iter()
        .map(|d| {
            let mut dur: BTreeMap<usize, (f64, usize)> = BTreeMap::new();
            let mut starts = 0usize;
            let mut total = 0.0;
            for s in f.overlapping(&d.key, lo, hi) {
                let a = s.t0.max(lo);
                let b = s.t1.unwrap_or(hi).min(hi);
                if b <= a {
                    continue;
                }
                let x = (b - a) as f64;
                let e = dur.entry(s.s).or_default();
                e.0 += x;
                e.1 += 1;
                total += x;
                if s.t0 >= lo {
                    starts += 1;
                }
            }
            let mut states: Vec<(usize, f64, usize, f64)> = dur
                .into_iter()
                .map(|(k, (x, n))| (k, if total > 0.0 { x / total } else { 0.0 }, n, x / n.max(1) as f64 / 3_600_000.0))
                .collect();
            states.sort_by(|a, b| b.1.total_cmp(&a.1));
            DimStats { key: d.key.clone(), states, switches_per_day: starts as f64 / days }
        })
        .collect()
}

/// 转移矩阵：范围里相邻两段 从 → 到 的次数，按行归一（行 = 从）。返回 (出现过的状态下标, 矩阵, 每行次数)。
#[must_use]
pub fn transitions(f: &RegimeFile, key: &str, lo: u64, hi: u64) -> (Vec<usize>, Vec<Vec<f64>>, Vec<usize>) {
    let segs = f.overlapping(key, lo, hi);
    let mut idx: Vec<usize> = segs.iter().map(|s| s.s).collect();
    idx.sort_unstable();
    idx.dedup();
    let pos = |s: usize| idx.iter().position(|x| *x == s).unwrap_or(0);
    let n = idx.len();
    let mut m = vec![vec![0usize; n]; n];
    for w in segs.windows(2) {
        if w[1].t0 >= lo && w[1].t0 < hi {
            m[pos(w[0].s)][pos(w[1].s)] += 1;
        }
    }
    let rows: Vec<usize> = m.iter().map(|r| r.iter().sum()).collect();
    let p = m
        .iter()
        .zip(&rows)
        .map(|(r, &t)| r.iter().map(|&c| if t > 0 { c as f64 / t as f64 } else { 0.0 }).collect())
        .collect();
    (idx, p, rows)
}

// ── 适用环境声明（docs/62 F4）────────────────────────────────────────────

/// 声明是否满足（与 `factory.regime.declare.check` 同一口径：快照里没有的维度算不满足）。
#[must_use]
pub fn check(req: &BTreeMap<String, Vec<String>>, snap: &BTreeMap<String, String>) -> (bool, Vec<String>) {
    let miss: Vec<String> = req
        .iter()
        .filter(|(k, v)| snap.get(*k).is_none_or(|s| !v.contains(s)))
        .map(|(k, v)| format!("{k}={}（要 {}）", snap.get(k).map_or("缺", String::as_str), v.join("/")))
        .collect();
    (miss.is_empty(), miss)
}

// ── 检验台账（factory.regime.validate 写的）──────────────────────────────────

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct LedgerRow {
    #[serde(default)]
    pub time: String,
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub trials: u32,
    #[serde(default)]
    pub verdict: Option<bool>,
    #[serde(default)]
    pub config_sha: String,
    #[serde(default)]
    pub result: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct HistoryRow {
    pub name: String,
    pub trials: u32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Ledger {
    pub history: Vec<HistoryRow>,
    pub rows: Vec<LedgerRow>,
}

impl Ledger {
    /// 累计试验数：框架之前的历史 + 正式检验（`final`）各自登记的项数；dev 与复现不计（与 Python 同一口径）。
    #[must_use]
    pub fn cumulative(&self) -> u32 {
        self.history.iter().map(|h| h.trials).sum::<u32>() + self.rows.iter().filter(|r| r.kind == "final").map(|r| r.trials).sum::<u32>()
    }
}

#[must_use]
pub fn ledger_dir() -> PathBuf {
    std::env::var_os("WS_REGIME_LEDGER")
        .map(PathBuf::from)
        .and_then(|p| p.parent().map(PathBuf::from))
        .unwrap_or_else(|| crate::ws::paths::data_dir().join("regime").join("validate"))
}

/// 解析台账（坏行跳过，不让一行写坏挡住整张表）。
#[must_use]
pub fn parse_ledger(jsonl: &str, history: &str) -> Ledger {
    Ledger {
        history: serde_json::from_str(history).unwrap_or_default(),
        rows: jsonl.lines().filter(|l| !l.trim().is_empty()).filter_map(|l| serde_json::from_str(l).ok()).collect(),
    }
}

#[must_use]
pub fn ledger() -> Ledger {
    let d = ledger_dir();
    let j = std::fs::read_to_string(d.join("ledger.jsonl")).unwrap_or_default();
    let h = std::fs::read_to_string(d.join("history.json")).unwrap_or_default();
    parse_ledger(&j, &h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ws::regime::parse;

    const J: &str = r#"{"format": 1, "symbol": "X", "timeframe": "1h", "tf_ms": 3600000,
      "dimensions": [{"key": "c", "layer": "L2", "name": "综合", "states": [
        {"key": "a", "name": "甲", "tone": "bull"}, {"key": "b", "name": "乙", "tone": "bear"}]}],
      "segments": {"c": [[0, 3600000, 0, ""], [3600000, 10800000, 1, ""], [10800000, null, 0, ""]]}}"#;

    #[test]
    fn 占比_持续_切换() {
        let f = parse(J).unwrap();
        let st = dim_stats(&f, 0, 4 * 3_600_000);
        let c = &st[0];
        assert_eq!(c.states[0].0, 0, "甲 2 小时、乙 2 小时一样多时按下标稳定排序也行，但占比都是 0.5");
        assert!((c.states[0].1 - 0.5).abs() < 1e-9 && (c.states[1].1 - 0.5).abs() < 1e-9);
        assert!((c.states.iter().find(|s| s.0 == 1).unwrap().3 - 2.0).abs() < 1e-9, "乙平均持续 2 小时");
        assert!((c.switches_per_day - 3.0 * 6.0).abs() < 1e-9, "4 小时里 3 段起点 = 每天 18 次");
        // 范围从 1.5 小时起：第一段不算起点、乙只算 1.5 小时
        let st = dim_stats(&f, 5_400_000, 4 * 3_600_000);
        assert!((st[0].states.iter().find(|s| s.0 == 1).unwrap().1 - 0.6).abs() < 1e-9);
    }

    #[test]
    fn 最后一根收盘决定右端() {
        let f = parse(J).unwrap();
        assert_eq!(f.span(), Some((0, 10_800_000 + 3_600_000)), "老文件：最后一段开始 + 一根");
        let g = parse(&J.replacen("\"tf_ms\": 3600000,", "\"tf_ms\": 3600000, \"last_ms\": 36000000,", 1)).unwrap();
        assert_eq!(g.span(), Some((0, 36_000_001)), "给了 last_ms 就用它：最后一段持续了很久也不低估");
        assert_eq!(g.snapshot(36_000_000).get("c").map(String::as_str), Some("a"));
    }

    #[test]
    fn 转移矩阵按行归一() {
        let f = parse(J).unwrap();
        let (idx, p, rows) = transitions(&f, "c", 0, 4 * 3_600_000);
        assert_eq!(idx, vec![0, 1]);
        assert_eq!(rows, vec![1, 1]);
        assert_eq!(p, vec![vec![0.0, 1.0], vec![1.0, 0.0]]);
    }

    #[test]
    fn 声明判定() {
        let req: BTreeMap<String, Vec<String>> = [("c".to_string(), vec!["a".to_string()])].into_iter().collect();
        let snap: BTreeMap<String, String> = [("c".to_string(), "a".to_string())].into_iter().collect();
        assert!(check(&req, &snap).0);
        assert!(!check(&req, &BTreeMap::new()).0, "快照里没有的维度算不满足");
        assert!(check(&BTreeMap::new(), &BTreeMap::new()).0, "没声明 = 不限");
    }

    #[test]
    fn 台账累计与坏行() {
        let l = parse_ledger(
            "{\"name\":\"a\",\"kind\":\"final\",\"trials\":2}\n坏行\n{\"name\":\"b\",\"kind\":\"dev\",\"trials\":0}\n{\"name\":\"c\",\"kind\":\"replication\",\"trials\":0}\n",
            r#"[{"name":"r5","trials":9}]"#,
        );
        assert_eq!(l.rows.len(), 3);
        assert_eq!(l.cumulative(), 11, "历史 9 + final 2；dev 与复现不计");
    }
}
