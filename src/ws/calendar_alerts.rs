//! 金融日历的提醒（docs/43 §6、K4）。
//!
//! # 放在哪
//!
//! **判定和本地渠道在 Cockpit 里**（这个模块的后台线程），联网渠道（Telegram）交给资讯守护：Cockpit 只往
//! `alert_deliveries` 写一行「待送」，界面里没有网络代码（docs/43 §1 第 5 条）。Cockpit 开着时整个系统才在
//! （关窗即停），所以提醒线程随 Cockpit 启动常驻，**不依赖日历面板开没开**。
//!
//! 三张表（`alert_rules` / `alert_jobs` / `alert_deliveries`，外加 `alert_settings`）由这里独占，和事件库同一个
//! SQLite 文件（WAL，守护写事件、这里写提醒，互不干扰）。事件表只读。
//!
//! # 怎么判（docs/43 §6.2、§6.3）
//!
//! - **任务表驱动，不建定时器**：每一轮按「规则 × 事件 × 提前量」算出该有的任务，`INSERT OR IGNORE`；
//!   线程睡到最早一个待发任务（最多 30 秒）。
//! - **幂等键** = 事件 + 规则 + 提前量 + 事件时刻的版本号：同一个提醒不会发两次；事件改期后版本号变，
//!   旧任务撤销、新任务按新时刻建（新时刻已经过了 → 立即发，标「提前」）。
//! - **启动补报**：Cockpit 关着时到点的任务不弹窗，标「错过」，进通知中心、状态栏显示条数。
//! - **没有时刻的事件**（仅日期 / 预计）只用「提前 N 天」类提前量，当天早上 09:00（本地）提醒；「提前 30 分钟」不建任务——
//!   不编一个时刻。
//! - 免打扰时段里的跳过（P0 可设例外）；改期 / 撤下 / 公布各有开关。

use std::sync::Mutex;
use std::time::Duration;

use chrono::{Local, NaiveDate, NaiveTime, TimeZone};
use rusqlite::{params, Connection, OptionalExtension};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS alert_rules (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    name           TEXT NOT NULL,
    scope          TEXT NOT NULL,
    target         TEXT NOT NULL DEFAULT '',
    max_importance INTEGER NOT NULL DEFAULT 3,
    offsets        TEXT NOT NULL,
    channels       TEXT NOT NULL,
    on_change      INTEGER NOT NULL DEFAULT 1,
    on_release     INTEGER NOT NULL DEFAULT 0,
    enabled        INTEGER NOT NULL DEFAULT 1,
    created_at     INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS alert_jobs (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    idem_key   TEXT NOT NULL UNIQUE,
    event_id   TEXT NOT NULL,
    rule_id    INTEGER NOT NULL,
    offset_min INTEGER NOT NULL,
    what       TEXT NOT NULL,
    version    INTEGER NOT NULL,
    fire_at    INTEGER NOT NULL,
    state      TEXT NOT NULL,
    title      TEXT NOT NULL,
    body       TEXT NOT NULL,
    importance INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    fired_at   INTEGER,
    note       TEXT
);
CREATE INDEX IF NOT EXISTS alert_jobs_due ON alert_jobs(state, fire_at);
CREATE INDEX IF NOT EXISTS alert_jobs_event ON alert_jobs(event_id, rule_id);
CREATE TABLE IF NOT EXISTS alert_deliveries (
    job_id     INTEGER NOT NULL,
    channel    TEXT NOT NULL,
    state      TEXT NOT NULL,
    attempts   INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    sent_at    INTEGER,
    expires_at INTEGER,
    PRIMARY KEY (job_id, channel)
);
CREATE TABLE IF NOT EXISTS alert_settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

// ── 规则 ────────────────────────────────────────────────────────────

/// 规则作用在哪些事件上。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// 全部事件（按重要性门槛）。
    All,
    /// 某个系列（`us-cpi`）。
    Series,
    /// 单个事件。
    Event,
    /// 某一类（`central_bank`）。
    Kind,
    /// 某个地区（`US`）。
    Country,
}

impl Scope {
    pub const ALL: [Scope; 5] = [Scope::All, Scope::Series, Scope::Event, Scope::Kind, Scope::Country];
    pub fn key(self) -> &'static str {
        match self {
            Scope::All => "all",
            Scope::Series => "series",
            Scope::Event => "event",
            Scope::Kind => "kind",
            Scope::Country => "country",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Scope::All => "全部事件",
            Scope::Series => "系列",
            Scope::Event => "单个事件",
            Scope::Kind => "类别",
            Scope::Country => "地区",
        }
    }
    pub fn from_key(s: &str) -> Scope {
        Scope::ALL.into_iter().find(|x| x.key() == s).unwrap_or(Scope::All)
    }
}

/// 提醒渠道。**网络渠道只在守护里实现**（docs/43 §6.4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// 应用内：通知中心 + 状态栏。
    InApp,
    /// 桌面通知（`notify-send`）。
    Desktop,
    /// Telegram：Cockpit 只写「待送」，发送进程 `ws-telegram` 发（docs/43 §17）。
    Telegram,
}

impl Channel {
    pub const ALL: [Channel; 3] = [Channel::InApp, Channel::Desktop, Channel::Telegram];
    pub fn key(self) -> &'static str {
        match self {
            Channel::InApp => "inapp",
            Channel::Desktop => "desktop",
            Channel::Telegram => "telegram",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Channel::InApp => "应用内",
            Channel::Desktop => "桌面",
            Channel::Telegram => "Telegram",
        }
    }
    pub fn needs_network(self) -> bool {
        self == Channel::Telegram
    }
    pub fn from_key(s: &str) -> Option<Channel> {
        Channel::ALL.into_iter().find(|x| x.key() == s)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub id: i64,
    pub name: String,
    pub scope: Scope,
    pub target: String,
    /// 只提醒重要性 ≤ 这一档（全部 / 类别 / 地区 范围用；系列、单个事件不看）。
    pub max_importance: u8,
    /// 提前多少分钟（0 = 发生时）。
    pub offsets: Vec<i64>,
    pub channels: Vec<Channel>,
    /// 改期 / 撤下时通知。
    pub on_change: bool,
    /// 实际值公布时通知（只报数，不写利多利空）。
    pub on_release: bool,
    pub enabled: bool,
}

/// 提前量的写法。
pub fn offset_label(m: i64) -> String {
    match m {
        0 => "发生时".into(),
        m if m % 1440 == 0 => format!("提前 {} 天", m / 1440),
        m if m % 60 == 0 => format!("提前 {} 小时", m / 60),
        m => format!("提前 {m} 分钟"),
    }
}

/// 提前量选项（界面上的勾选项）。
pub const OFFSET_CHOICES: [i64; 6] = [7 * 1440, 1440, 60, 30, 5, 0];

/// 预设模板（docs/43 §5「提醒」页：一键启用）。
pub fn presets() -> Vec<(&'static str, Vec<Rule>)> {
    let r = |name: &str, scope: Scope, target: &str, imp: u8, offsets: &[i64]| Rule {
        id: 0,
        name: name.into(),
        scope,
        target: target.into(),
        max_importance: imp,
        offsets: offsets.to_vec(),
        channels: vec![Channel::InApp, Channel::Desktop],
        on_change: true,
        on_release: false,
        enabled: true,
    };
    vec![
        (
            "FOMC + CPI + 非农",
            vec![
                r("FOMC 利率决议", Scope::Series, "fomc-decision", 3, &[1440, 30]),
                r("美国 CPI", Scope::Series, "us-cpi", 3, &[1440, 30]),
                r("美国非农就业", Scope::Series, "us-nfp", 3, &[1440, 30]),
            ],
        ),
        ("所有 P0", vec![r("所有 P0", Scope::All, "", 0, &[1440, 30])]),
        ("所有 P0 + P1", vec![r("所有 P0 + P1", Scope::All, "", 1, &[30])]),
        ("美股休市（提前 1 天）", vec![r("美股休市", Scope::Series, "us-holiday", 3, &[1440])]),
    ]
}

// ── 事件（只读）────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct Ev {
    pub id: String,
    pub series: Option<String>,
    pub kind: String,
    pub title: String,
    pub country: String,
    pub tz: String,
    pub date_local: NaiveDate,
    pub time_local: Option<String>,
    pub scheduled_at: Option<i64>,
    pub precision: String,
    pub status: String,
    pub importance: u8,
    pub version: i64,
}

impl Ev {
    fn matches(&self, r: &Rule) -> bool {
        match r.scope {
            Scope::All => self.importance <= r.max_importance,
            Scope::Series => self.series.as_deref() == Some(r.target.as_str()),
            Scope::Event => self.id == r.target,
            Scope::Kind => self.kind == r.target && self.importance <= r.max_importance,
            Scope::Country => self.country == r.target && self.importance <= r.max_importance,
        }
    }

    /// 这个提前量下该在什么时候提醒。没有时刻的事件只认整天的提前量（当天 09:00 本地），否则 `None`——不编时刻。
    pub fn fire_at(&self, offset_min: i64) -> Option<i64> {
        match self.scheduled_at {
            Some(t) if self.precision != "date_only" && self.precision != "estimated" => Some(t - offset_min * 60_000),
            _ if offset_min >= 1440 && offset_min % 1440 == 0 => {
                let day = self.date_local - chrono::Duration::days(offset_min / 1440);
                Local
                    .from_local_datetime(&day.and_time(NaiveTime::from_hms_opt(9, 0, 0)?))
                    .earliest()
                    .map(|t| t.timestamp_millis())
            }
            _ => None,
        }
    }

    /// 通知里写的时刻：本地 + 事件当地。
    fn when_text(&self) -> String {
        match self.scheduled_at {
            Some(t) => {
                let local = Local.timestamp_millis_opt(t).single().map(|x| x.format("%m-%d %H:%M").to_string()).unwrap_or_default();
                let tz = super::calendar_readout::tz_short(&self.tz);
                let conv = if self.precision == "conventional" { "（时刻按惯例）" } else { "" };
                format!("{local} 本地 · {} {tz}{conv}", self.time_local.clone().unwrap_or_default())
            }
            None => format!("{} 全天{}", self.date_local.format("%m-%d"), if self.precision == "estimated" { "（预计）" } else { "" }),
        }
    }
}

fn importance_tag(i: u8) -> &'static str {
    match i {
        0 => "P0",
        1 => "P1",
        2 => "P2",
        _ => "P3",
    }
}

// ── 库 ──────────────────────────────────────────────────────────────

pub fn db_path() -> std::path::PathBuf {
    super::calendar_readout::db_path()
}

pub fn open(path: &std::path::Path) -> rusqlite::Result<Connection> {
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let c = Connection::open(path)?;
    c.pragma_update(None, "journal_mode", "WAL")?;
    c.busy_timeout(Duration::from_secs(5))?;
    c.execute_batch(SCHEMA)?;
    Ok(c)
}

fn now_ms() -> i64 {
    Local::now().timestamp_millis()
}

pub fn rules(c: &Connection) -> rusqlite::Result<Vec<Rule>> {
    let mut st = c.prepare("SELECT id, name, scope, target, max_importance, offsets, channels, on_change, on_release, enabled FROM alert_rules ORDER BY id")?;
    let v = st
        .query_map([], |r| {
            Ok(Rule {
                id: r.get(0)?,
                name: r.get(1)?,
                scope: Scope::from_key(&r.get::<_, String>(2)?),
                target: r.get(3)?,
                max_importance: r.get::<_, i64>(4)?.clamp(0, 3) as u8,
                offsets: serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or_default(),
                channels: serde_json::from_str::<Vec<String>>(&r.get::<_, String>(6)?)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|k| Channel::from_key(k))
                    .collect(),
                on_change: r.get::<_, i64>(7)? != 0,
                on_release: r.get::<_, i64>(8)? != 0,
                enabled: r.get::<_, i64>(9)? != 0,
            })
        })?
        .filter_map(|x| x.ok())
        .collect();
    Ok(v)
}

pub fn add_rule(c: &Connection, r: &Rule) -> rusqlite::Result<i64> {
    let ch: Vec<&str> = r.channels.iter().map(|x| x.key()).collect();
    c.execute(
        "INSERT INTO alert_rules (name, scope, target, max_importance, offsets, channels, on_change, on_release, enabled, created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![
            r.name,
            r.scope.key(),
            r.target,
            r.max_importance as i64,
            serde_json::to_string(&r.offsets).unwrap_or_else(|_| "[]".into()),
            serde_json::to_string(&ch).unwrap_or_else(|_| "[]".into()),
            r.on_change as i64,
            r.on_release as i64,
            r.enabled as i64,
            now_ms(),
        ],
    )?;
    Ok(c.last_insert_rowid())
}

pub fn set_rule_enabled(c: &Connection, id: i64, on: bool) -> rusqlite::Result<()> {
    c.execute("UPDATE alert_rules SET enabled=?2 WHERE id=?1", params![id, on as i64])?;
    if !on {
        // 停用：它还没发的任务一并撤销（重新启用时按当时的时刻重建）
        c.execute("UPDATE alert_jobs SET state='cancelled', note='规则停用' WHERE rule_id=?1 AND state='pending'", [id])?;
    }
    Ok(())
}

pub fn delete_rule(c: &Connection, id: i64) -> rusqlite::Result<()> {
    c.execute("DELETE FROM alert_rules WHERE id=?1", [id])?;
    c.execute("UPDATE alert_jobs SET state='cancelled', note='规则已删' WHERE rule_id=?1 AND state='pending'", [id])?;
    Ok(())
}

/// 免打扰：`(开始, 结束, P0 例外)`，`HH:MM`；开始 == 结束 = 关。
#[derive(Debug, Clone, PartialEq)]
pub struct Quiet {
    pub from: String,
    pub to: String,
    pub p0_exempt: bool,
}

impl Default for Quiet {
    fn default() -> Self {
        Self { from: "00:00".into(), to: "00:00".into(), p0_exempt: true }
    }
}

impl Quiet {
    pub fn on(&self) -> bool {
        self.from != self.to
    }
    /// 本地时刻 `t` 在免打扰时段里吗（支持跨午夜，如 23:00–07:00）。
    pub fn covers(&self, t: NaiveTime) -> bool {
        let (Ok(a), Ok(b)) = (NaiveTime::parse_from_str(&self.from, "%H:%M"), NaiveTime::parse_from_str(&self.to, "%H:%M")) else {
            return false;
        };
        if a == b {
            false
        } else if a < b {
            t >= a && t < b
        } else {
            t >= a || t < b
        }
    }
}

fn setting(c: &Connection, k: &str) -> Option<String> {
    c.query_row("SELECT value FROM alert_settings WHERE key=?1", [k], |r| r.get(0)).optional().ok().flatten()
}

pub fn quiet(c: &Connection) -> Quiet {
    let d = Quiet::default();
    Quiet {
        from: setting(c, "quiet_from").unwrap_or(d.from),
        to: setting(c, "quiet_to").unwrap_or(d.to),
        p0_exempt: setting(c, "quiet_p0_exempt").is_none_or(|v| v == "1"),
    }
}

pub fn set_quiet(c: &Connection, q: &Quiet) -> rusqlite::Result<()> {
    for (k, v) in [("quiet_from", q.from.clone()), ("quiet_to", q.to.clone()), ("quiet_p0_exempt", if q.p0_exempt { "1".into() } else { "0".into() })] {
        c.execute("INSERT INTO alert_settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value=?2", params![k, v])?;
    }
    Ok(())
}

/// 读事件表（只读）。事件表不存在（守护还没跑过）时返回空。
fn events(c: &Connection, from: NaiveDate, to: NaiveDate) -> Vec<Ev> {
    let Ok(mut st) = c.prepare(
        "SELECT id, series, kind, title, country, tz, date_local, time_local, scheduled_at, precision, status, importance, version
         FROM events WHERE date_local BETWEEN ?1 AND ?2",
    ) else {
        return Vec::new();
    };
    st.query_map(params![from.to_string(), to.to_string()], |r| {
        Ok(Ev {
            id: r.get(0)?,
            series: r.get(1)?,
            kind: r.get(2)?,
            title: r.get(3)?,
            country: r.get(4)?,
            tz: r.get(5)?,
            date_local: NaiveDate::parse_from_str(&r.get::<_, String>(6)?, "%Y-%m-%d").unwrap_or_default(),
            time_local: r.get(7)?,
            scheduled_at: r.get(8)?,
            precision: r.get(9)?,
            status: r.get(10)?,
            importance: r.get::<_, i64>(11)?.clamp(0, 3) as u8,
            version: r.get(12)?,
        })
    })
    .map(|it| it.filter_map(|x| x.ok()).collect())
    .unwrap_or_default()
}

/// 事件的头一行数值（公布通知用）。
fn headline_value(c: &Connection, event_id: &str) -> Option<String> {
    c.query_row(
        "SELECT label, actual, consensus FROM event_values WHERE event_id=?1 AND actual IS NOT NULL ORDER BY source_id, seq LIMIT 1",
        [event_id],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?)),
    )
    .optional()
    .ok()
    .flatten()
    .map(|(l, a, cons)| {
        let mut s = format!("{l} 实际 {}", a.unwrap_or_default());
        if let Some(x) = cons {
            s.push_str(&format!("（预期 {x}）"));
        }
        s
    })
}

// ── 规划：规则 × 事件 → 任务 ─────────────────────────────────────────

/// 一轮规划的结果（测试与日志用）。
#[derive(Debug, Default, PartialEq)]
pub struct PlanReport {
    pub created: usize,
    /// 因改期撤掉的旧任务。
    pub superseded: usize,
    /// 改期后新时刻已过、立即发的（「提前」）。
    pub advanced: usize,
    pub notices: usize,
}

#[allow(clippy::too_many_arguments)]
fn insert_job(
    c: &Connection,
    key: &str,
    e: &Ev,
    rule_id: i64,
    offset: i64,
    what: &str,
    fire_at: i64,
    state: &str,
    title: &str,
    body: &str,
    note: Option<&str>,
) -> rusqlite::Result<bool> {
    let n = c.execute(
        "INSERT OR IGNORE INTO alert_jobs (idem_key, event_id, rule_id, offset_min, what, version, fire_at, state, title, body, importance, created_at, note)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
        params![key, e.id, rule_id, offset, what, e.version, fire_at, state, title, body, e.importance as i64, now_ms(), note],
    )?;
    Ok(n > 0)
}

/// 按规则把未来的事件排成任务。`now`：当前毫秒。
pub fn plan(c: &Connection, now: i64) -> rusqlite::Result<PlanReport> {
    let mut rep = PlanReport::default();
    let rules: Vec<Rule> = rules(c)?.into_iter().filter(|r| r.enabled).collect();
    if rules.is_empty() {
        return Ok(rep);
    }
    let today = Local.timestamp_millis_opt(now).single().map(|t| t.date_naive()).unwrap_or_default();
    let max_off = rules.iter().flat_map(|r| r.offsets.iter().copied()).max().unwrap_or(0);
    let evs = events(c, today - chrono::Duration::days(2), today + chrono::Duration::days(max_off / 1440 + 3));
    let tx = c.unchecked_transaction()?;
    for r in &rules {
        for e in evs.iter().filter(|e| e.matches(r)) {
            let tag = importance_tag(e.importance);
            // 撤下 / 不符：待发的撤掉，必要时发一条「撤下」通知
            if e.status == "unlisted" || e.status == "conflict" {
                let n = c.execute(
                    "UPDATE alert_jobs SET state='cancelled', note='事件已撤下' WHERE event_id=?1 AND rule_id=?2 AND state='pending' AND what='before'",
                    params![e.id, r.id],
                )?;
                if n > 0 && r.on_change && e.status == "unlisted" {
                    let key = format!("{}|{}|unlisted|v{}", e.id, r.id, e.version);
                    let title = format!("【{tag} · 已撤下】{}", e.title);
                    let body = format!("列过这件事的来源都撤下了它（原定 {}）——可能取消，也可能是来源改版", e.when_text());
                    if insert_job(c, &key, e, r.id, -1, "cancelled", now, "pending", &title, &body, None)? {
                        rep.notices += 1;
                    }
                }
                continue;
            }
            // 公布：只报数，不写利多利空
            if r.on_release
                && e.status == "released"
                && let Some(v) = headline_value(c, &e.id)
            {
                let key = format!("{}|{}|released", e.id, r.id);
                let title = format!("【{tag} · 已公布】{}", e.title);
                if insert_job(c, &key, e, r.id, -2, "released", now, "pending", &title, &v, None)? {
                    rep.notices += 1;
                }
            }
            // 改期：旧版本还没发的撤掉；规则要的话发一条「改期」
            #[allow(clippy::let_and_return)] // 迭代器借着 stmt，必须先收进 v、stmt 才能在块尾析构
            let stale: Vec<(i64, i64)> = {
                let mut st = c.prepare(
                    "SELECT id, version FROM alert_jobs WHERE event_id=?1 AND rule_id=?2 AND what='before' AND version<>?3 AND state IN ('pending','fired')",
                )?;
                let v = st.query_map(params![e.id, r.id, e.version], |x| Ok((x.get(0)?, x.get(1)?)))?.filter_map(|x| x.ok()).collect();
                v
            };
            let moved = !stale.is_empty();
            if moved {
                rep.superseded += c.execute(
                    "UPDATE alert_jobs SET state='cancelled', note='事件改期' WHERE event_id=?1 AND rule_id=?2 AND what='before' AND version<>?3 AND state='pending'",
                    params![e.id, r.id, e.version],
                )?;
                if r.on_change {
                    let key = format!("{}|{}|changed|v{}", e.id, r.id, e.version);
                    let title = format!("【{tag} · 改期】{}", e.title);
                    let body = format!("新时间 {}", e.when_text());
                    if insert_job(c, &key, e, r.id, -3, "changed", now, "pending", &title, &body, None)? {
                        rep.notices += 1;
                    }
                }
            }
            for &off in &r.offsets {
                let Some(at) = e.fire_at(off) else { continue };
                let key = format!("{}|{}|{off}|v{}", e.id, r.id, e.version);
                let title = format!("【{tag} · {}】{}", if off == 0 { "现在".to_string() } else { offset_label(off).replace("提前 ", "") + "后" }, e.title);
                let body = e.when_text();
                // 提醒时刻已经过了：
                // - 事件改期提前了（有旧版本）→ 立即发，标「提前」；
                // - 否则是规则刚建 / 事件刚出现时就已经过了 → 不补发，记一笔「建时已过」
                let (state, fire, note) = if at >= now - 60_000 {
                    ("pending", at, None)
                } else if moved && e.scheduled_at.is_some_and(|t| t > now) {
                    ("pending", now, Some("事件提前，提醒时刻已过，立即发"))
                } else {
                    ("skipped", at, Some("建任务时提醒时刻已过"))
                };
                if insert_job(c, &key, e, r.id, off, "before", fire, state, &title, &body, note)? && state == "pending" {
                    rep.created += 1;
                    if note.is_some() {
                        rep.advanced += 1;
                    }
                }
            }
        }
    }
    tx.commit()?;
    Ok(rep)
}

/// Cockpit 启动时：关着的那段时间到点的任务**不弹窗**，标「错过」。返回条数。
pub fn mark_missed(c: &Connection, now: i64) -> rusqlite::Result<usize> {
    c.execute("UPDATE alert_jobs SET state='missed' WHERE state='pending' AND fire_at < ?1", [now - 2 * 60_000])
}

// ── 发出 ────────────────────────────────────────────────────────────

/// Telegram 配置（`~/.config/wealthspring/telegram.json`，权限 600；**令牌不进快照、不进日志**）。
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct TelegramConfig {
    #[serde(default)]
    pub bot_token: String,
    /// 数字或字符串都认（多数教程里是数字、不带引号；按字符串读会整份读不出、被当成「未配置」）。
    #[serde(default, deserialize_with = "str_or_num")]
    pub chat_id: String,
    #[serde(default)]
    pub enabled: bool,
    /// 只发重要性 ≤ 这一档的（缺省 1 = P0 + P1）。
    #[serde(default = "one")]
    pub min_importance: u8,
}

fn one() -> u8 {
    1
}

fn str_or_num<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    use serde::Deserialize;
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::String(s) => s.trim().to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => String::new(),
    })
}

/// 发送 Telegram 的独立进程（docs/43 §17）：只发提醒、不抓新闻。
pub const SENDER_UNIT: &str = "ws-telegram";

/// Telegram 开着时，发送进程 `ws-telegram` 得在跑（随 Cockpit 起停）。
///
/// 不再拉起资讯守护：启动策略「对外连接关闭」（默认）会停掉它，为了发几条提醒让它当例外，
/// 等于全天轮询几十个新闻源（2026-10-09 用户选了单独的发送进程）。
/// 幂等：已经在跑就什么都不做。**必须 --no-block**（systemctl start 会等单元就绪，冻住调用方）。
pub fn ensure_sender_running() {
    let tg = telegram();
    if !(tg.enabled && tg.configured()) {
        return;
    }
    if super::svcctl::query(SENDER_UNIT).active {
        return;
    }
    let _ = std::process::Command::new("systemctl").args(["--user", "start", "--no-block", SENDER_UNIT]).status();
    log::info!("[calendar-alerts] Telegram 开着：拉起发送进程 {SENDER_UNIT}");
}

/// 关掉 Telegram 时停掉发送进程（它关着时本来也不发，停掉是为了出口总闸上那一行如实显示「没在跑」）。
pub fn stop_sender() {
    let _ = std::process::Command::new("systemctl").args(["--user", "stop", "--no-block", SENDER_UNIT]).status();
}

/// 「发一条测试消息」：写一条测试任务 + 一行 Telegram 待送，交给守护发。返回给界面的一句话。
pub fn queue_test_message(c: &Connection) -> Result<String, String> {
    let tg = telegram();
    if !tg.configured() {
        return Err(format!("还没配置：在 {} 写 bot_token 与 chat_id", telegram_path().display()));
    }
    if !tg.enabled {
        return Err("Telegram 关着：先在「资源｜网络出口」把「Telegram 提醒」打开".into());
    }
    let now = now_ms();
    let title = "WealthSpring 金融日历 · 测试消息";
    let body = format!("这条是测试：收到说明 Telegram 渠道通了（{}）。", Local::now().format("%m-%d %H:%M:%S"));
    c.execute(
        "INSERT INTO alert_jobs (idem_key, event_id, rule_id, offset_min, what, version, fire_at, state, title, body, importance, created_at, fired_at)
         VALUES (?1, '', 0, 0, 'test', 0, ?2, 'fired', ?3, ?4, 1, ?2, ?2)",
        params![format!("test|{now}"), now, title, body],
    )
    .map_err(|e| e.to_string())?;
    let id = c.last_insert_rowid();
    // 两分钟内没发出去就作废（守护没起来 / 网络不通）
    c.execute(
        "INSERT INTO alert_deliveries (job_id, channel, state, attempts, expires_at) VALUES (?1, 'telegram', 'pending', 0, ?2)",
        params![id, now + 120_000],
    )
    .map_err(|e| e.to_string())?;
    ensure_sender_running();
    Ok("✔ 测试消息已交给发送进程 ws-telegram（几秒内到；结果看下面通知中心的「渠道」列）".into())
}

pub fn telegram_path() -> std::path::PathBuf {
    super::paths::config_dir().join("telegram.json")
}

pub fn telegram() -> TelegramConfig {
    std::fs::read_to_string(telegram_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

impl TelegramConfig {
    pub fn configured(&self) -> bool {
        !self.bot_token.trim().is_empty() && !self.chat_id.trim().is_empty()
    }
}

/// 把 Telegram 的「开 / 关」写回配置（出口总闸用）。只改 `enabled`，别的字段原样保留。
pub fn set_telegram_enabled(on: bool) -> Result<(), String> {
    let p = telegram_path();
    let mut v: serde_json::Value = std::fs::read_to_string(&p).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_else(|| serde_json::json!({}));
    v["enabled"] = serde_json::Value::Bool(on);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(&p, serde_json::to_vec_pretty(&v).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// 配置文件权限是不是只有本人可读（令牌在里面）。
pub fn telegram_file_private() -> Option<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(telegram_path()).ok().map(|m| m.permissions().mode() & 0o077 == 0)
    }
    #[cfg(not(unix))]
    {
        None
    }
}

fn desktop_notify(title: &str, body: &str, importance: u8) -> Result<(), String> {
    let urgency = if importance == 0 { "critical" } else { "normal" };
    let out = std::process::Command::new("notify-send")
        .args(["-a", "WealthSpring 金融日历", "-u", urgency, title, body])
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("notify-send 退出码 {:?} {}", out.status.code(), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// 到点的任务发出去。`dry`：测试用，不真的弹桌面通知。返回发了几条。
pub fn fire_due(c: &Connection, now: i64, dry: bool) -> rusqlite::Result<usize> {
    #[allow(clippy::let_and_return)] // 同上：迭代器借着 stmt
    let due: Vec<(i64, i64, String, String, u8, i64)> = {
        let mut st = c.prepare(
            "SELECT j.id, j.rule_id, j.title, j.body, j.importance, e.scheduled_at FROM alert_jobs j
             LEFT JOIN events e ON e.id = j.event_id
             WHERE j.state='pending' AND j.fire_at <= ?1 ORDER BY j.fire_at",
        )?;
        let v = st
            .query_map([now], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get::<_, i64>(4)?.clamp(0, 3) as u8, r.get::<_, Option<i64>>(5)?.unwrap_or(now)))
            })?
            .filter_map(|x| x.ok())
            .collect();
        v
    };
    if due.is_empty() {
        return Ok(0);
    }
    let rules = rules(c)?;
    let q = quiet(c);
    let tg = telegram();
    let local_now = Local.timestamp_millis_opt(now).single().map(|t| t.time()).unwrap_or_default();
    let mut n = 0;
    for (id, rule_id, title, body, imp, event_at) in due {
        if q.on() && q.covers(local_now) && !(imp == 0 && q.p0_exempt) {
            c.execute("UPDATE alert_jobs SET state='suppressed', fired_at=?2, note='免打扰时段' WHERE id=?1", params![id, now])?;
            continue;
        }
        let chans = rules.iter().find(|r| r.id == rule_id).map(|r| r.channels.clone()).unwrap_or_else(|| vec![Channel::InApp]);
        for ch in chans {
            let (state, err): (&str, Option<String>) = match ch {
                Channel::InApp => ("sent", None),
                Channel::Desktop if dry => ("sent", None),
                Channel::Desktop => match desktop_notify(&title, &body, imp) {
                    Ok(()) => ("sent", None),
                    Err(e) => ("failed", Some(e)),
                },
                // 联网渠道：Cockpit 只写「待送」，ws-telegram 发。没配置 / 关着 / 不够重要的直接记下原因
                Channel::Telegram if !tg.configured() => ("disabled", Some("未配置".into())),
                Channel::Telegram if !tg.enabled => ("disabled", Some("已关（出口总闸 / 配置）".into())),
                Channel::Telegram if imp > tg.min_importance => ("disabled", Some(format!("重要性低于 Telegram 门槛 {}", importance_tag(tg.min_importance)))),
                Channel::Telegram => ("pending", None),
            };
            // Telegram 不补发过期的：事件过了还没送出就作废
            c.execute(
                "INSERT OR REPLACE INTO alert_deliveries (job_id, channel, state, attempts, last_error, sent_at, expires_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![id, ch.key(), state, (state != "pending" && state != "disabled") as i64, err, (state == "sent").then_some(now), event_at.max(now)],
            )?;
        }
        c.execute("UPDATE alert_jobs SET state='fired', fired_at=?2 WHERE id=?1", params![id, now])?;
        n += 1;
    }
    if !dry && n > 0 && tg.enabled && tg.configured() {
        ensure_sender_running();
    }
    // 过期的 Telegram 待送
    c.execute("UPDATE alert_deliveries SET state='expired' WHERE state='pending' AND expires_at < ?1", [now])?;
    Ok(n)
}

/// 下一个待发任务的时刻。
pub fn next_due(c: &Connection) -> Option<i64> {
    c.query_row("SELECT MIN(fire_at) FROM alert_jobs WHERE state='pending'", [], |r| r.get(0)).ok().flatten()
}

// ── 通知中心（界面读）──────────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Notice {
    pub id: i64,
    pub event_id: String,
    pub what: String,
    pub title: String,
    pub body: String,
    pub state: String,
    pub fire_at: i64,
    pub fired_at: Option<i64>,
    pub note: Option<String>,
    /// （渠道, 状态, 错误）
    pub deliveries: Vec<(String, String, Option<String>)>,
}

/// 最近的通知（已发 / 错过 / 撤销 / 免打扰挡掉 / 待发），新的在前。
pub fn notices(c: &Connection, limit: usize) -> Vec<Notice> {
    let Ok(mut st) = c.prepare(
        "SELECT id, event_id, what, title, body, state, fire_at, fired_at, note FROM alert_jobs
         WHERE state <> 'skipped' ORDER BY CASE state WHEN 'pending' THEN 1 ELSE 0 END, COALESCE(fired_at, fire_at) DESC LIMIT ?1",
    ) else {
        return Vec::new();
    };
    let mut v: Vec<Notice> = st
        .query_map([limit as i64], |r| {
            Ok(Notice {
                id: r.get(0)?,
                event_id: r.get(1)?,
                what: r.get(2)?,
                title: r.get(3)?,
                body: r.get(4)?,
                state: r.get(5)?,
                fire_at: r.get(6)?,
                fired_at: r.get(7)?,
                note: r.get(8)?,
                deliveries: Vec::new(),
            })
        })
        .map(|it| it.filter_map(|x| x.ok()).collect())
        .unwrap_or_default();
    if let Ok(mut d) = c.prepare("SELECT channel, state, last_error FROM alert_deliveries WHERE job_id=?1 ORDER BY channel") {
        for n in &mut v {
            n.deliveries = d.query_map([n.id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map(|it| it.filter_map(|x| x.ok()).collect()).unwrap_or_default();
        }
    }
    v
}

/// 待发的条数、错过的条数（状态栏）。
pub fn counts(c: &Connection) -> (i64, i64) {
    let q = |s: &str| c.query_row("SELECT COUNT(*) FROM alert_jobs WHERE state=?1", [s], |r| r.get(0)).unwrap_or(0);
    (q("pending"), q("missed"))
}

/// 把「错过」的标成已读（通知中心里点「知道了」）。
pub fn ack_missed(c: &Connection) -> rusqlite::Result<usize> {
    c.execute("UPDATE alert_jobs SET state='missed_ack' WHERE state='missed'", [])
}

// ── 后台线程 ────────────────────────────────────────────────────────

/// 线程的最近状态（界面读）。
#[derive(Debug, Clone, Default)]
pub struct Status {
    pub running: bool,
    pub last_tick: String,
    pub last_error: String,
    pub pending: i64,
    pub missed: i64,
    pub next_due: Option<i64>,
}

static STATUS: Mutex<Option<Status>> = Mutex::new(None);

/// 界面读的快照（线程每一轮刷新；界面线程不查库）。
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub rules: Vec<Rule>,
    pub notices: Vec<Notice>,
    pub quiet: Quiet,
}

static SNAP: Mutex<Option<std::sync::Arc<Snapshot>>> = Mutex::new(None);

pub fn snapshot() -> std::sync::Arc<Snapshot> {
    SNAP.lock().ok().and_then(|g| g.clone()).unwrap_or_default()
}

/// 界面上的一次写操作（增删规则、改免打扰）：开一个短连接写完，叫醒线程刷新。
pub fn write(f: impl FnOnce(&Connection) -> rusqlite::Result<()>) -> Result<(), String> {
    let c = open(&db_path()).map_err(|e| format!("打不开事件库：{e}"))?;
    f(&c).map_err(|e| e.to_string())?;
    wake();
    Ok(())
}
static STARTED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
/// 规则改了 / 事件变了：提前叫醒线程。
static WAKE: (Mutex<bool>, std::sync::Condvar) = (Mutex::new(false), std::sync::Condvar::new());

pub fn status() -> Status {
    STATUS.lock().ok().and_then(|g| g.clone()).unwrap_or_default()
}

pub fn wake() {
    if let Ok(mut g) = WAKE.0.lock() {
        *g = true;
        WAKE.1.notify_all();
    }
}

/// 样张模式（截图用的隔离实例）：只把规则与通知读进快照，**不规划、不发出**——
/// 样张实例弹出的桌面通知会落在用户正在用的桌面上（notify-send 走 D-Bus 会话总线，不看 DISPLAY）。
pub fn load_snapshot_only() {
    let Ok(c) = open(&db_path()) else { return };
    if let Ok(mut g) = SNAP.lock() {
        *g = Some(std::sync::Arc::new(Snapshot { rules: rules(&c).unwrap_or_default(), notices: notices(&c, 200), quiet: quiet(&c) }));
    }
}

/// 启动提醒线程（Cockpit 启动时调一次；样张模式不调）。
pub fn start() {
    STARTED.get_or_init(|| {
        super::spawn_named("ws-cal-alerts", || {
            let path = db_path();
            let c = match open(&path) {
                Ok(c) => c,
                Err(e) => {
                    log::warn!("[calendar-alerts] 打不开 {}：{e}", path.display());
                    if let Ok(mut g) = STATUS.lock() {
                        *g = Some(Status { last_error: format!("打不开事件库：{e}"), ..Default::default() });
                    }
                    return;
                }
            };
            // Telegram 开着：发送在资讯守护里，保证它在跑
            ensure_sender_running();
            // 启动补报：关着的那段时间到点的，不弹窗
            if let Ok(n) = mark_missed(&c, now_ms())
                && n > 0
            {
                log::info!("[calendar-alerts] Cockpit 关着时错过 {n} 条提醒");
            }
            let mut board_mtime = None;
            loop {
                let now = now_ms();
                let mut err = String::new();
                // 事件快照变了（守护同步过）或者被叫醒：重新规划
                let mt = std::fs::metadata(super::calendar_readout::board_path()).and_then(|m| m.modified()).ok();
                let woke = WAKE.0.lock().map(|mut g| std::mem::replace(&mut *g, false)).unwrap_or(false);
                if mt != board_mtime || woke || board_mtime.is_none() {
                    board_mtime = mt;
                    if let Err(e) = plan(&c, now) {
                        err = format!("规划失败：{e}");
                    }
                }
                if let Err(e) = fire_due(&c, now, false) {
                    err = format!("发出失败：{e}");
                }
                let (pending, missed) = counts(&c);
                let nd = next_due(&c);
                if let Ok(mut g) = SNAP.lock() {
                    *g = Some(std::sync::Arc::new(Snapshot { rules: rules(&c).unwrap_or_default(), notices: notices(&c, 200), quiet: quiet(&c) }));
                }
                if let Ok(mut g) = STATUS.lock() {
                    *g = Some(Status {
                        running: true,
                        last_tick: Local::now().format("%H:%M:%S").to_string(),
                        last_error: err,
                        pending,
                        missed,
                        next_due: nd,
                    });
                }
                // 睡到下一个任务（最多 30 秒——事件快照可能变了），或者被叫醒
                let wait = nd.map_or(30_000, |t| (t - now_ms()).clamp(200, 30_000)) as u64;
                let g = WAKE.0.lock().unwrap_or_else(|e| e.into_inner());
                let _ = WAKE.1.wait_timeout_while(g, Duration::from_millis(wait), |w| !*w);
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个带事件表的内存库（事件表的形状与守护的一致；这里只建提醒要读的列）。
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c.execute_batch(
            "CREATE TABLE events (id TEXT PRIMARY KEY, series TEXT, kind TEXT, title TEXT, country TEXT, tz TEXT, date_local TEXT,
                time_local TEXT, scheduled_at INTEGER, precision TEXT, status TEXT, importance INTEGER, version INTEGER);
             CREATE TABLE event_values (event_id TEXT, source_id TEXT, label TEXT, seq INTEGER, actual TEXT, consensus TEXT, previous TEXT, updated_at INTEGER);",
        )
        .unwrap();
        c
    }

    const T0: i64 = 1_792_000_000_000; // 2026-10-13 前后

    fn put(c: &Connection, id: &str, at: Option<i64>, prec: &str, status: &str, version: i64) {
        let date = Local.timestamp_millis_opt(at.unwrap_or(T0 + 3 * 86_400_000)).single().unwrap().date_naive();
        c.execute(
            "INSERT OR REPLACE INTO events VALUES (?1,'us-cpi','macro','美国 CPI','US','America/New_York',?2,'08:30',?3,?4,?5,1,?6)",
            params![id, date.to_string(), at, prec, status, version],
        )
        .unwrap();
    }

    fn rule(c: &Connection, offsets: &[i64], on_change: bool) -> i64 {
        add_rule(
            c,
            &Rule {
                id: 0,
                name: "CPI".into(),
                scope: Scope::Series,
                target: "us-cpi".into(),
                max_importance: 3,
                offsets: offsets.to_vec(),
                channels: vec![Channel::InApp, Channel::Desktop, Channel::Telegram],
                on_change,
                on_release: true,
                enabled: true,
            },
        )
        .unwrap()
    }

    fn state_of(c: &Connection, off: i64) -> Vec<String> {
        let mut st = c.prepare("SELECT state FROM alert_jobs WHERE offset_min=?1 ORDER BY id").unwrap();
        st.query_map([off], |r| r.get(0)).unwrap().filter_map(|x| x.ok()).collect()
    }

    #[test]
    fn planning_twice_creates_each_reminder_once() {
        // 幂等：同一提醒不建两次
        let c = db();
        put(&c, "e1", Some(T0 + 2 * 86_400_000), "exact", "scheduled", 1);
        rule(&c, &[1440, 30], true);
        assert_eq!(plan(&c, T0).unwrap().created, 2);
        assert_eq!(plan(&c, T0).unwrap().created, 0);
        // 到点发一次，再到点不再发
        assert_eq!(fire_due(&c, T0 + 2 * 86_400_000 - 30 * 60_000, true).unwrap(), 2);
        assert_eq!(fire_due(&c, T0 + 2 * 86_400_000, true).unwrap(), 0);
    }

    #[test]
    fn a_rescheduled_event_cancels_the_old_reminder_and_builds_a_new_one() {
        // docs/43 K4 判据：改一个事件时刻 → 旧任务撤、新任务建；改期通知一条
        let c = db();
        let at = T0 + 2 * 86_400_000;
        put(&c, "e1", Some(at), "exact", "scheduled", 1);
        rule(&c, &[30], true);
        plan(&c, T0).unwrap();
        put(&c, "e1", Some(at + 86_400_000), "exact", "scheduled", 2); // 推迟一天、版本 +1
        let r = plan(&c, T0).unwrap();
        assert_eq!((r.created, r.superseded, r.notices), (1, 1, 1));
        assert_eq!(state_of(&c, 30), ["cancelled", "pending"]);
        let new_at: i64 = c.query_row("SELECT fire_at FROM alert_jobs WHERE offset_min=30 AND state='pending'", [], |r| r.get(0)).unwrap();
        assert_eq!(new_at, at + 86_400_000 - 30 * 60_000);
    }

    #[test]
    fn an_event_moved_earlier_past_its_reminder_time_fires_immediately() {
        let c = db();
        put(&c, "e1", Some(T0 + 2 * 86_400_000), "exact", "scheduled", 1);
        rule(&c, &[1440], false);
        plan(&c, T0).unwrap();
        // 提前到 10 小时后：「提前 1 天」的时刻已经过了 → 立即发
        put(&c, "e1", Some(T0 + 10 * 3_600_000), "exact", "scheduled", 2);
        let r = plan(&c, T0).unwrap();
        assert_eq!(r.advanced, 1);
        assert_eq!(fire_due(&c, T0, true).unwrap(), 1);
    }

    #[test]
    fn reminders_due_while_cockpit_was_closed_are_missed_not_popped() {
        // docs/43 K4 判据：关 Cockpit 跨过提醒时刻再开 → 只出「错过 1 条」，不弹窗
        let c = db();
        put(&c, "e1", Some(T0 + 86_400_000), "exact", "scheduled", 1);
        rule(&c, &[30], true);
        plan(&c, T0).unwrap();
        let reopen = T0 + 86_400_000; // 提醒时刻（发生前 30 分钟）已过
        assert_eq!(mark_missed(&c, reopen).unwrap(), 1);
        assert_eq!(fire_due(&c, reopen, true).unwrap(), 0, "错过的不弹");
        assert_eq!(counts(&c), (0, 1));
    }

    #[test]
    fn a_rule_created_after_the_reminder_time_does_not_backfill() {
        let c = db();
        put(&c, "e1", Some(T0 + 10 * 60_000), "exact", "scheduled", 1);
        rule(&c, &[30], true);
        assert_eq!(plan(&c, T0).unwrap().created, 0);
        assert_eq!(state_of(&c, 30), ["skipped"]);
        assert!(notices(&c, 10).is_empty(), "建时已过的不进通知中心");
    }

    #[test]
    fn date_only_events_only_get_whole_day_offsets_at_nine_local() {
        let e = Ev {
            id: "h".into(),
            series: None,
            kind: "holiday".into(),
            title: "美股休市".into(),
            country: "US".into(),
            tz: "America/New_York".into(),
            date_local: NaiveDate::from_ymd_opt(2026, 11, 26).unwrap(),
            time_local: None,
            scheduled_at: None,
            precision: "estimated".into(),
            status: "scheduled".into(),
            importance: 1,
            version: 1,
        };
        assert!(e.fire_at(30).is_none(), "没有时刻：提前 30 分钟不建任务，不编时刻");
        let at = Local.timestamp_millis_opt(e.fire_at(1440).unwrap()).single().unwrap();
        assert_eq!(at.format("%m-%d %H:%M").to_string(), "11-25 09:00");
    }

    #[test]
    fn unlisted_event_cancels_pending_and_notifies_once() {
        let c = db();
        put(&c, "e1", Some(T0 + 2 * 86_400_000), "exact", "scheduled", 1);
        rule(&c, &[30], true);
        plan(&c, T0).unwrap();
        put(&c, "e1", Some(T0 + 2 * 86_400_000), "exact", "unlisted", 1);
        let r = plan(&c, T0).unwrap();
        assert_eq!(r.notices, 1);
        assert_eq!(state_of(&c, 30), ["cancelled"]);
        assert_eq!(plan(&c, T0).unwrap().notices, 0, "撤下通知只发一次");
    }

    #[test]
    fn quiet_hours_suppress_except_p0_and_cross_midnight() {
        let q = Quiet { from: "23:00".into(), to: "07:00".into(), p0_exempt: true };
        let t = |s: &str| NaiveTime::parse_from_str(s, "%H:%M").unwrap();
        assert!(q.covers(t("23:30")) && q.covers(t("02:00")) && !q.covers(t("07:00")) && !q.covers(t("12:00")));
        assert!(!Quiet::default().on());
    }

    #[test]
    fn telegram_is_queued_only_when_configured_and_never_sent_from_here() {
        // 没配置：记「未配置」，不是待送；Cockpit 不发网络请求
        let c = db();
        put(&c, "e1", Some(T0 + 86_400_000), "exact", "scheduled", 1);
        rule(&c, &[0], true);
        plan(&c, T0).unwrap();
        fire_due(&c, T0 + 86_400_000, true).unwrap();
        let n = notices(&c, 5);
        let tg = n[0].deliveries.iter().find(|d| d.0 == "telegram").unwrap();
        // 测试机上没有 telegram.json（或没配置）：不会是 pending
        if !telegram().configured() {
            assert_eq!(tg.1, "disabled");
        }
        assert!(n[0].deliveries.iter().any(|d| d.0 == "inapp" && d.1 == "sent"));
    }

    #[test]
    fn telegram_chat_id_may_be_a_number() {
        // 多数教程里 chat_id 不带引号；按字符串读会整份读不出、被当成「未配置」
        let a: TelegramConfig = serde_json::from_str(r#"{"bot_token":"1:x","chat_id":123456789}"#).unwrap();
        let b: TelegramConfig = serde_json::from_str(r#"{"bot_token":"1:x","chat_id":"123456789","enabled":true}"#).unwrap();
        assert_eq!((a.chat_id.as_str(), a.configured(), a.min_importance), ("123456789", true, 1));
        assert!(b.enabled && b.configured());
    }

    #[test]
    fn release_notice_reports_the_number_only() {
        let c = db();
        put(&c, "e1", Some(T0 - 60_000), "exact", "released", 1);
        c.execute("INSERT INTO event_values VALUES ('e1','nasdaq-econ','CPI',0,'0.4%','0.3%','0.4%',0)", []).unwrap();
        rule(&c, &[30], false);
        assert_eq!(plan(&c, T0).unwrap().notices, 1);
        let body: String = c.query_row("SELECT body FROM alert_jobs WHERE what='released'", [], |r| r.get(0)).unwrap();
        assert_eq!(body, "CPI 实际 0.4%（预期 0.3%）");
        assert!(!body.contains("利多") && !body.contains("利空"));
    }

    /// 端到端：真起后台线程，事件 3 秒后发生、提醒「发生时」，看它到点发出、被叫醒后立刻规划。
    /// 只用「应用内」渠道——不在开发者的桌面上弹真通知。改进程级环境变量，单独跑：
    /// `cargo test --release calendar_alerts::tests::the_thread -- --ignored`
    #[test]
    #[ignore]
    fn the_thread_fires_a_reminder_on_time_and_wakes_on_rule_change() {
        let dir = std::env::temp_dir().join(format!("ws-cal-alert-e2e-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("calendar.sqlite");
        let _ = std::fs::remove_file(&p);
        // SAFETY：这个测试单独跑（#[ignore]），没有别的线程同时读环境变量
        unsafe {
            std::env::set_var("WS_CALENDAR_DB", &p);
            std::env::set_var("WS_CALENDAR_BOARD", dir.join("calendar_board.json"));
        }
        let c = open(&p).unwrap();
        c.execute_batch(
            "CREATE TABLE IF NOT EXISTS events (id TEXT PRIMARY KEY, series TEXT, kind TEXT, title TEXT, country TEXT, tz TEXT, date_local TEXT,
                time_local TEXT, scheduled_at INTEGER, precision TEXT, status TEXT, importance INTEGER, version INTEGER);
             CREATE TABLE IF NOT EXISTS event_values (event_id TEXT, source_id TEXT, label TEXT, seq INTEGER, actual TEXT, consensus TEXT, previous TEXT, updated_at INTEGER);",
        )
        .unwrap();
        let at = now_ms() + 3_000;
        let date = Local.timestamp_millis_opt(at).single().unwrap().date_naive();
        c.execute(
            "INSERT INTO events VALUES ('e2e','us-cpi','macro','美国 CPI','US','America/New_York',?1,'08:30',?2,'exact','scheduled',1,1)",
            params![date.to_string(), at],
        )
        .unwrap();
        start();
        std::thread::sleep(Duration::from_millis(500));
        // 规则在线程起来之后才加：靠 wake() 立刻规划，不等 30 秒
        write(|c| {
            add_rule(
                c,
                &Rule {
                    id: 0,
                    name: "e2e".into(),
                    scope: Scope::Event,
                    target: "e2e".into(),
                    max_importance: 3,
                    offsets: vec![0],
                    channels: vec![Channel::InApp],
                    on_change: false,
                    on_release: false,
                    enabled: true,
                },
            )
            .map(|_| ())
        })
        .unwrap();
        std::thread::sleep(Duration::from_millis(800));
        assert_eq!(status().pending, 1, "wake() 之后应该已经规划好了：{:?}", status());
        std::thread::sleep(Duration::from_millis(3_500));
        let st: String = c.query_row("SELECT state FROM alert_jobs WHERE event_id='e2e'", [], |r| r.get(0)).unwrap();
        let fired_at: i64 = c.query_row("SELECT fired_at FROM alert_jobs WHERE event_id='e2e'", [], |r| r.get(0)).unwrap();
        assert_eq!(st, "fired");
        assert!((fired_at - at).abs() < 1_500, "到点误差 {} ms", fired_at - at);
        assert_eq!(snapshot().notices.first().map(|n| n.state.clone()).as_deref(), Some("fired"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
