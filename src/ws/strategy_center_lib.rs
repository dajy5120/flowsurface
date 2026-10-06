//! 策略中心 · 策略库的过滤与分组（docs/39）：目录树（从文件路径自动生成）、策略类型多选、分组方式。
//!
//! 纯数据与纯函数（可测）；状态在 [`super::strategy_center`]。过滤状态存本机
//! （`<config>/wealthspring/strategy_library.json`），下次打开保持。

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::strategy_center::Entry;

/// 策略类型：12 种经典交易策略 + 资产配置（与 factory.lab.contract.STYLES 一致，顺序 = 按钮顺序）。
pub const STYLES: [(&str, &str); 13] = [
    ("trend", "趋势跟踪"), ("breakout", "突破"), ("momentum", "动量"), ("mean_reversion", "均值回归"),
    ("stat_arb", "统计套利"), ("macro", "宏观"), ("reflexivity", "反身性"), ("value", "价值"),
    ("event", "事件驱动"), ("market_making", "做市"), ("orderflow", "订单流"), ("volatility", "波动率"),
    ("allocation", "资产配置"),
];

pub fn style_label(k: &str) -> &'static str {
    STYLES.iter().find(|(x, _)| *x == k).map(|(_, l)| *l).unwrap_or("未归类")
}

/// 分组方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum GroupBy {
    #[default]
    Dir,
    Style,
    Verdict,
}

pub const GROUPS: [(&str, GroupBy); 3] = [("按目录", GroupBy::Dir), ("按类型", GroupBy::Style), ("按结论", GroupBy::Verdict)];

/// 过滤状态（存本机）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LibFilter {
    /// 选中的类型（空 = 全部；多选之间取「或」）
    pub styles: BTreeSet<String>,
    /// 目录前缀（相对 `strategies/`，如 `research/masters/trend`；None = 全部）
    pub dir: Option<String>,
    pub group_by: GroupBy,
    /// 展开的目录节点
    pub open: BTreeSet<String>,
    /// 目录树是否折叠起来（省地方）
    pub tree_hidden: bool,
}

/// 策略文件所在目录（相对 `strategies/`）：`strategies/research/masters/trend/turtle.py` → `research/masters/trend`。
pub fn rel_dir(path: &str) -> String {
    let p = path.strip_prefix("strategies/").unwrap_or(path);
    match p.rfind('/') {
        Some(i) => p[..i].to_string(),
        None => String::new(),
    }
}

fn under(dir: &str, prefix: &str) -> bool {
    dir == prefix || dir.starts_with(&format!("{prefix}/"))
}

/// 类型 + 结论 + 搜索（**不含目录**：目录树的计数要在这一步之后算）。
pub fn matches(e: &Entry, f: &LibFilter, verdict: Option<&str>, query: &str) -> bool {
    let Some(m) = &e.meta else {
        return query.is_empty() && verdict.is_none() && f.styles.is_empty(); // 元数据写错的只在不筛选时露出
    };
    if let Some(v) = verdict
        && m.verdict != v
    {
        return false;
    }
    if !f.styles.is_empty() && !m.styles.iter().any(|s| f.styles.contains(s)) {
        return false;
    }
    let q = query.to_lowercase();
    q.is_empty()
        || m.name.to_lowercase().contains(&q)
        || m.id.contains(&q)
        || m.tags.iter().any(|t| t.contains(&q))
        || m.master.as_ref().is_some_and(|x| x.name.to_lowercase().contains(&q))
}

/// 加上目录过滤后的最终可见条目。
pub fn visible<'a>(entries: &'a [Entry], f: &LibFilter, verdict: Option<&str>, query: &str) -> Vec<&'a Entry> {
    entries
        .iter()
        .filter(|e| matches(e, f, verdict, query))
        .filter(|e| f.dir.as_deref().is_none_or(|d| under(&rel_dir(&e.path), d)))
        .collect()
}

/// 目录树的一个节点。
#[derive(Debug, Clone, PartialEq)]
pub struct TreeNode {
    pub path: String,
    pub label: String,
    pub depth: usize,
    /// 这个子树里（按类型 / 结论 / 搜索过滤后）有几个策略
    pub count: usize,
    pub has_children: bool,
    pub open: bool,
}

/// 由全部条目的目录生成树（计数用 `matches` 过滤后的条目），只列出「父节点都展开」的那些。
pub fn tree(entries: &[Entry], f: &LibFilter, verdict: Option<&str>, query: &str) -> Vec<TreeNode> {
    // 全部目录（含中间层）——结构不随过滤变，计数随过滤变
    let mut all: BTreeMap<String, usize> = BTreeMap::new();
    for e in entries {
        let d = rel_dir(&e.path);
        let parts: Vec<&str> = d.split('/').filter(|x| !x.is_empty()).collect();
        for k in 1..=parts.len() {
            all.entry(parts[..k].join("/")).or_insert(0);
        }
    }
    for e in entries.iter().filter(|e| matches(e, f, verdict, query)) {
        let d = rel_dir(&e.path);
        for p in all.clone().keys() {
            if under(&d, p) {
                *all.get_mut(p).expect("key") += 1;
            }
        }
    }
    let keys: Vec<String> = all.keys().cloned().collect();
    let mut out = vec![];
    for p in &keys {
        let depth = p.matches('/').count();
        // 祖先都展开才显示
        let parts: Vec<&str> = p.split('/').collect();
        let shown = (1..parts.len()).all(|k| f.open.contains(&parts[..k].join("/")));
        if !shown {
            continue;
        }
        let has_children = keys.iter().any(|q| q.starts_with(&format!("{p}/")));
        out.push(TreeNode {
            path: p.clone(),
            label: parts.last().copied().unwrap_or("").to_string(),
            depth,
            count: all[p],
            has_children,
            open: f.open.contains(p),
        });
    }
    out
}

/// 面包屑：`research/masters/trend` → [("research", "research"), ("masters", "research/masters"), ("trend", …)]。
pub fn breadcrumb(dir: &str) -> Vec<(String, String)> {
    let parts: Vec<&str> = dir.split('/').filter(|x| !x.is_empty()).collect();
    (1..=parts.len()).map(|k| (parts[k - 1].to_string(), parts[..k].join("/"))).collect()
}

/// 分组键（排序用）与显示名。
pub fn group_of(e: &Entry, g: GroupBy) -> (String, String) {
    let Some(m) = &e.meta else {
        return ("~".into(), "（元数据有误）".into());
    };
    match g {
        GroupBy::Dir => {
            let d = rel_dir(&e.path);
            (d.clone(), d.replace('/', " › "))
        }
        GroupBy::Style => match m.styles.first() {
            Some(s) => {
                let k = STYLES.iter().position(|(x, _)| x == s).unwrap_or(98);
                (format!("{k:02}"), style_label(s).to_string())
            }
            None => ("99".into(), "未归类".into()),
        },
        GroupBy::Verdict => {
            const ORDER: [&str; 7] = ["live", "paper", "alive", "candidate", "untested", "falsified", "retired"];
            let k = ORDER.iter().position(|x| *x == m.verdict).unwrap_or(9);
            (format!("{k}"), super::strategy_center_view::verdict_label(&m.verdict).to_string())
        }
    }
}

fn state_path() -> std::path::PathBuf {
    super::paths::config_dir().join("strategy_library.json")
}

pub fn load() -> LibFilter {
    std::fs::read(state_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save(f: &LibFilter) {
    let p = state_path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(b) = serde_json::to_vec_pretty(f) {
        let tmp = p.with_extension("tmp");
        if std::fs::write(&tmp, b).is_ok() {
            let _ = std::fs::rename(tmp, p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ws::strategy_center::Meta;

    fn e(id: &str, path: &str, styles: &[&str], verdict: &str) -> Entry {
        Entry {
            id: id.into(),
            path: path.into(),
            meta: Some(Meta {
                id: id.into(),
                name: id.into(),
                verdict: verdict.into(),
                styles: styles.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn sample() -> Vec<Entry> {
        vec![
            e("masters.turtle", "strategies/research/masters/trend/turtle.py", &["trend", "breakout"], "untested"),
            e("masters.darvas", "strategies/research/masters/breakout/darvas.py", &["breakout"], "untested"),
            e("orderflow.ofi", "strategies/research/orderflow/ofi.py", &["orderflow"], "falsified"),
            e("examples.qi", "strategies/examples/qi.py", &["momentum"], "untested"),
        ]
    }

    #[test]
    fn dir_tree_counts_follow_other_filters() {
        let es = sample();
        let mut f = LibFilter::default();
        let t = tree(&es, &f, None, "");
        assert_eq!(t.iter().map(|n| n.path.as_str()).collect::<Vec<_>>(), vec!["examples", "research"]);
        assert_eq!(t[1].count, 3);
        f.open.insert("research".into());
        f.open.insert("research/masters".into());
        f.styles.insert("breakout".into());
        let t = tree(&es, &f, None, "");
        let by: BTreeMap<_, _> = t.iter().map(|n| (n.path.clone(), n.count)).collect();
        assert_eq!(by["research"], 2, "多类型策略（海龟 = 趋势 + 突破）也算");
        assert_eq!(by["research/masters/trend"], 1);
        assert_eq!(by["research/orderflow"], 0);
        assert!(t.iter().find(|n| n.path == "research/masters").unwrap().has_children);
    }

    #[test]
    fn dir_filter_is_a_subtree_and_styles_are_or() {
        let es = sample();
        let mut f = LibFilter { dir: Some("research/masters".into()), ..Default::default() };
        assert_eq!(visible(&es, &f, None, "").len(), 2);
        f.dir = Some("research/master".into());
        assert!(visible(&es, &f, None, "").is_empty(), "前缀必须按目录边界匹配");
        f.dir = None;
        f.styles = ["orderflow".to_string(), "momentum".to_string()].into_iter().collect();
        assert_eq!(visible(&es, &f, None, "").len(), 2);
        assert_eq!(visible(&es, &f, Some("falsified"), "").len(), 1);
    }

    #[test]
    fn grouping_and_breadcrumb() {
        let es = sample();
        assert_eq!(group_of(&es[0], GroupBy::Style).1, "趋势跟踪", "按主类型（第一个）分组");
        assert_eq!(group_of(&es[0], GroupBy::Dir).1, "research › masters › trend");
        assert!(group_of(&es[0], GroupBy::Style).0 < group_of(&es[2], GroupBy::Style).0);
        assert_eq!(breadcrumb("research/masters/trend").last().unwrap(), &("trend".to_string(), "research/masters/trend".to_string()));
        let f = LibFilter { styles: ["trend".to_string()].into_iter().collect(), group_by: GroupBy::Style, ..Default::default() };
        let back: LibFilter = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert_eq!(back, f);
    }
}
