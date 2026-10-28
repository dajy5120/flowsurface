//! 提醒管理器（docs/65 §4 第 11 条）：把分散在四处的提醒规则汇总成一张表——指标提醒（各图的指标设置里）、
//! 画线到价（线上）、日历规则（日历面板）、新闻关键词（news.toml）。
//!
//! **规则仍存原处**，这里只汇总、启停、跳到触发历史；改条件回原处改。读库（画线）在后台线程里做，
//! 结果放进快照；开着时每 5 秒刷新一次。

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleKind {
    Indicator,
    Drawing,
    Calendar,
    News,
}

impl RuleKind {
    pub const ALL: [RuleKind; 4] = [RuleKind::Indicator, RuleKind::Drawing, RuleKind::Calendar, RuleKind::News];
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            RuleKind::Indicator => "指标",
            RuleKind::Drawing => "画线",
            RuleKind::Calendar => "日历",
            RuleKind::News => "新闻关键词",
        }
    }
}

/// 启停落到哪里。
#[derive(Debug, Clone, PartialEq)]
pub enum RuleAct {
    Indicator { chart: String, uid: u64 },
    Drawing { symbol: String, id: String },
    Calendar(i64),
}

/// 表里的一行。
#[derive(Debug, Clone)]
pub struct Row {
    pub kind: RuleKind,
    /// 与消息中心的 `rule_ref` 同一个
    pub rule_ref: String,
    pub symbol: String,
    pub name: String,
    pub cond: String,
    pub level: u8,
    pub channels: String,
    pub on: bool,
    /// 补充：图不在任何页上 / 改在哪里
    pub note: String,
    /// 能在这里启停的
    pub act: Option<RuleAct>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RulesMsg {
    Toggle,
    Close,
    /// 只看一类（`None` = 全部）
    Kind(Option<RuleKind>),
    Set(RuleAct, bool),
    /// 在消息中心里看这条规则的全部触发
    History(String),
}

#[derive(Default)]
struct State {
    open: bool,
    kind: Option<RuleKind>,
    rows: Arc<Vec<Row>>,
    loading: bool,
    last: Option<Instant>,
    note: String,
}

fn st() -> std::sync::MutexGuard<'static, State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(Default::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[must_use]
pub fn is_open() -> bool {
    st().open
}

/// 界面读：（只看哪类, 行, 在读, 说明）。
#[must_use]
pub fn view_state() -> (Option<RuleKind>, Arc<Vec<Row>>, bool, String) {
    let s = st();
    (s.kind, s.rows.clone(), s.loading, s.note.clone())
}

pub fn handle(m: RulesMsg) {
    match m {
        RulesMsg::Toggle => {
            let open = {
                let mut s = st();
                s.open = !s.open;
                s.open
            };
            if open {
                refresh();
            }
        }
        RulesMsg::Close => st().open = false,
        RulesMsg::Kind(k) => st().kind = k,
        RulesMsg::Set(act, on) => {
            match &act {
                RuleAct::Indicator { chart, uid } => super::studies::handle(super::studies::StudyMsg::Alert {
                    chart: chart.clone(),
                    uid: *uid,
                    op: super::studies::AlertOp::Armed(on),
                }),
                RuleAct::Drawing { symbol, id } => {
                    let (symbol, id) = (symbol.clone(), id.clone());
                    super::spawn_named("ws-alert-rules", move || super::drawing::set_line_alert(&symbol, &id, on));
                }
                RuleAct::Calendar(id) => super::calendar::handle(super::calendar_view::CalMsg::AlertToggle(*id, on)),
            }
            // 先改表里那一行（日历的快照要等它的线程下一轮才跟上）
            let mut s = st();
            let mut rows: Vec<Row> = (*s.rows).clone();
            for r in rows.iter_mut().filter(|r| r.act.as_ref() == Some(&act)) {
                r.on = on;
            }
            s.rows = Arc::new(rows);
            s.note = if on { "✔ 已恢复".into() } else { "✔ 已暂停（条件、冷却都还在，恢复后接着判）".into() };
        }
        RulesMsg::History(r) => super::inbox::show_rule(&r),
    }
}

/// 开着时每帧调：每 5 秒在后台重读一次。
pub fn tick() {
    let due = {
        let s = st();
        s.open && !s.loading && s.last.is_none_or(|t| t.elapsed() >= Duration::from_secs(5))
    };
    if due {
        refresh();
    }
}

fn refresh() {
    {
        let mut s = st();
        if s.loading {
            return;
        }
        s.loading = true;
    }
    super::spawn_named("ws-alert-rules", || {
        let rows = collect();
        let mut s = st();
        s.rows = Arc::new(rows);
        s.loading = false;
        s.last = Some(Instant::now());
    });
}

fn chans(telegram: bool) -> String {
    if telegram { "应用内 · 桌面 · Telegram".into() } else { "应用内 · 桌面".into() }
}

fn code(sym: &str) -> String {
    sym.rsplit(':').next().unwrap_or(sym).to_string()
}

fn collect() -> Vec<Row> {
    let mut v = Vec::new();
    for r in super::studies::alert_rules() {
        let a = &r.alert;
        v.push(Row {
            kind: RuleKind::Indicator,
            rule_ref: format!("indicator:{}:{}", r.chart, r.uid),
            symbol: code(&r.symbol),
            name: r.label,
            cond: format!("{} · 冷却 {}s{}", r.cond, a.cooldown_s, if a.once { " · 只响一次" } else { "" }),
            level: 2,
            channels: chans(a.telegram),
            on: a.armed,
            note: if r.live { String::new() } else { "这张图不在任何页上：不判".into() },
            act: Some(RuleAct::Indicator { chart: r.chart, uid: r.uid }),
        });
    }
    for (sym, d) in super::drawing::alert_lines() {
        let Some(a) = d.alert.as_ref() else { continue };
        let mut name = d.kind.label().to_string();
        if let Some(t) = d.text.as_ref().filter(|t| !t.is_empty()) {
            name.push_str(&format!("（{t}）"));
        }
        v.push(Row {
            kind: RuleKind::Drawing,
            rule_ref: format!("drawing:{sym}:{}", d.id),
            symbol: code(&sym),
            name,
            cond: format!("价格{}这条线{}", a.when.label(), if a.once { " · 只响一次" } else { "" }),
            level: 1,
            channels: chans(a.telegram),
            on: a.armed,
            note: String::new(),
            act: Some(RuleAct::Drawing { symbol: sym.clone(), id: d.id.clone() }),
        });
    }
    let snap = super::calendar_alerts::snapshot();
    for r in &snap.rules {
        let offs: Vec<String> = r.offsets.iter().map(|m| super::calendar_alerts::offset_label(*m)).collect();
        v.push(Row {
            kind: RuleKind::Calendar,
            rule_ref: format!("calendar:{}", r.id),
            symbol: String::new(),
            name: r.name.clone(),
            cond: format!("{} {} · {}", r.scope.label(), r.target, offs.join("、")),
            level: r.max_importance.min(3),
            channels: r.channels.iter().map(|c| c.label()).collect::<Vec<_>>().join(" · "),
            on: r.enabled,
            note: String::new(),
            act: Some(RuleAct::Calendar(r.id)),
        });
    }
    let path = super::paths::repo_root().join("crates/wealthspring-news/news.toml");
    for (name, query, cool, per_hour) in news_rules(&std::fs::read_to_string(&path).unwrap_or_default()) {
        v.push(Row {
            kind: RuleKind::News,
            rule_ref: format!("news:{name}"),
            symbol: String::new(),
            cond: format!("{query} · 冷却 {cool}s · 每小时 ≤{per_hour}"),
            name,
            level: 2,
            channels: "应用内 · 桌面".into(),
            on: true,
            note: "在 news.toml 的 [[alert]] 里改，新闻守护重启后生效".into(),
            act: None,
        });
    }
    v
}

/// news.toml 里的 `[[alert]]`：（名字, 查询, 冷却秒, 每小时上限）。只认这四个键，别的忽略。
fn news_rules(text: &str) -> Vec<(String, String, i64, i64)> {
    let mut out = Vec::new();
    let mut cur: Option<(String, String, i64, i64)> = None;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            if let Some(r) = cur.take().filter(|r| !r.0.is_empty()) {
                out.push(r);
            }
            if t == "[[alert]]" {
                cur = Some((String::new(), String::new(), 0, 0));
            }
            continue;
        }
        let Some(r) = cur.as_mut() else { continue };
        let Some((k, v)) = t.split_once('=') else { continue };
        let v = v.split(" #").next().unwrap_or("").trim();
        let s = v.trim_matches('"').to_string();
        match k.trim() {
            "name" => r.0 = s,
            "query" => r.1 = s,
            "cooldown_secs" => r.2 = v.parse().unwrap_or(0),
            "max_per_hour" => r.3 = v.parse().unwrap_or(0),
            _ => {}
        }
    }
    if let Some(r) = cur.filter(|r| !r.0.is_empty()) {
        out.push(r);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 读_news_toml_的告警规则() {
        let t = "window_secs = 1\n[[alert]]\nname = \"上币/下架\"\nquery = \"list\"\ncooldown_secs = 60\nmax_per_hour = 10\n\n[[source]]\nname = \"x\"\n[[alert]]\nname = \"停牌\"\nquery = \"kind:halt\" # 注释\n";
        let v = news_rules(t);
        assert_eq!(v, vec![("上币/下架".into(), "list".into(), 60, 10), ("停牌".into(), "kind:halt".into(), 0, 0)]);
    }
}
