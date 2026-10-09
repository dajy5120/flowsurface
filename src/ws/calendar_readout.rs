//! 金融日历的旁路快照（docs/43 K2）：读 `calendar_board.json`，面板只画内存里的副本。
//!
//! 与新闻面板同一个模式：后台线程按文件修改时间重读、渲染线程只取快照；面板不显示时释放
//! （`svcctl::Demand`）。**面板里没有一行网络代码**——抓取在 `ws-news` 守护的日历管线里。
//!
//! 打开日历时数据超过 12 小时（或还没有快照），自动起一次 `ws-news --once calendar`
//! （docs/43 §12 第 2 条，用户定）。不加定时器：只有面板在显示时才会判断。

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use chrono::{Datelike, NaiveDate, TimeZone};

/// 快照里的一个事件。
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Ev {
    pub id: String,
    pub series: Option<String>,
    /// 系列的中文名（守护按系列目录填）。
    pub series_name: Option<String>,
    pub kind: String,
    pub title: String,
    pub country: String,
    /// IANA 时区名（事件当地）。
    pub tz: String,
    pub date_local: Option<NaiveDate>,
    /// 当地时刻 `HH:MM`；没有时刻为 `None`。
    pub time_local: Option<String>,
    /// UTC 毫秒。
    pub scheduled_at: Option<i64>,
    /// 多日会议的第一天。
    pub span_start: Option<NaiveDate>,
    /// `exact` / `conventional` / `date_only` / `estimated`
    pub precision: String,
    /// `scheduled` / `unlisted` / `conflict`
    pub status: String,
    /// 0 = P0 … 3 = P3
    pub importance: u8,
    pub assets: Vec<String>,
    pub url: Option<String>,
    pub note: Option<String>,
    pub owner_tier: String,
    pub sources: Vec<String>,
    pub revisions: i64,
    pub values: Vec<ValueRow>,
}

impl Ev {
    /// 列表里显示的那一行数值（第一行）。
    pub fn headline(&self) -> Option<&ValueRow> {
        self.values.first()
    }
}

/// 一行数值：预期 / 前值 / 实际（CPI 有环比、同比、核心几行）。缺的就是 `None`。
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ValueRow {
    pub label: String,
    pub actual: Option<String>,
    pub consensus: Option<String>,
    pub previous: Option<String>,
}

/// 一个日历源的覆盖情况（健康状态在新闻快照的源行里）。
#[derive(Debug, Default, Clone, PartialEq)]
pub struct CalSource {
    pub id: String,
    pub label: String,
    pub org: String,
    pub coverage_to: Option<String>,
    pub coverage_days_left: Option<i64>,
    pub last_report: String,
    /// 滚动发布（国债、Nasdaq、币安、规则）：覆盖期短是常态，不说「尚未公布」、不标黄。守护在快照里给。
    pub rolling: bool,
}

#[derive(Debug, Default, Clone)]
pub struct CalReadout {
    /// 有快照文件且读得出来。
    pub present: bool,
    pub generated_ms: i64,
    pub today: Option<NaiveDate>,
    /// 快照覆盖的日期范围。范围外的日子不是「没有事件」，是快照里没带。
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    pub events: Vec<Ev>,
    pub sources: Vec<CalSource>,
    /// 面板读到这份快照的时刻（`HH:MM:SS`）。
    pub refreshed: String,
}

fn s(v: &serde_json::Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}
fn os(v: &serde_json::Value, k: &str) -> Option<String> {
    v.get(k).and_then(|x| x.as_str()).map(str::to_string)
}
fn date(v: &serde_json::Value, k: &str) -> Option<NaiveDate> {
    v.get(k).and_then(|x| x.as_str()).and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
}

pub fn parse(v: &serde_json::Value) -> CalReadout {
    let events = v
        .get("events")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .map(|e| Ev {
                    id: s(e, "id"),
                    series: os(e, "series"),
                    series_name: os(e, "series_name"),
                    kind: s(e, "kind"),
                    title: s(e, "title"),
                    country: s(e, "country"),
                    tz: s(e, "tz"),
                    date_local: date(e, "date_local"),
                    time_local: os(e, "time_local"),
                    scheduled_at: e.get("scheduled_at").and_then(|x| x.as_i64()),
                    span_start: date(e, "span_start"),
                    precision: s(e, "precision"),
                    status: s(e, "status"),
                    importance: e.get("importance").and_then(|x| x.as_u64()).unwrap_or(3).min(3) as u8,
                    assets: e
                        .get("assets")
                        .and_then(|x| x.as_array())
                        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                        .unwrap_or_default(),
                    url: os(e, "url"),
                    note: os(e, "note"),
                    owner_tier: s(e, "owner_tier"),
                    sources: e
                        .get("sources")
                        .and_then(|x| x.as_array())
                        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                        .unwrap_or_default(),
                    revisions: e.get("revisions").and_then(|x| x.as_i64()).unwrap_or(0),
                    values: e
                        .get("values")
                        .and_then(|x| x.as_array())
                        .map(|a| {
                            a.iter()
                                .map(|v| ValueRow {
                                    label: s(v, "label"),
                                    actual: os(v, "actual"),
                                    consensus: os(v, "consensus"),
                                    previous: os(v, "previous"),
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default();
    let sources = v
        .get("sources")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .map(|r| CalSource {
                    id: s(r, "id"),
                    label: s(r, "label"),
                    org: s(r, "org"),
                    coverage_to: os(r, "coverage_to"),
                    coverage_days_left: r.get("coverage_days_left").and_then(|x| x.as_i64()),
                    last_report: s(r, "last_report"),
                    rolling: r.get("rolling").and_then(|x| x.as_bool()).unwrap_or(false),
                })
                .collect()
        })
        .unwrap_or_default();
    CalReadout {
        present: true,
        generated_ms: v.get("generated_ms").and_then(|x| x.as_i64()).unwrap_or(0),
        today: date(v, "today"),
        from: date(v, "from"),
        to: date(v, "to"),
        events,
        sources,
        refreshed: String::new(),
    }
}

pub fn board_path() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("WS_CALENDAR_BOARD") {
        return std::path::PathBuf::from(p);
    }
    super::paths::runtime_dir().join("calendar_board.json")
}

/// 事件库（查修订历史用；快照里只有修订条数）。
pub fn db_path() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("WS_CALENDAR_DB") {
        return std::path::PathBuf::from(p);
    }
    super::paths::data_dir().join("calendar.sqlite")
}

/// 数据多旧就自动刷新一次（docs/43 §12）。
pub const AUTO_REFRESH_AFTER_MS: i64 = 12 * 3600 * 1000;
/// 自动刷新失败后多久再试——源连不上时不能每 10 秒起一个进程。
const AUTO_RETRY_SECS: u64 = 30 * 60;

static READOUT: OnceLock<Mutex<std::sync::Arc<CalReadout>>> = OnceLock::new();
static POLLER: OnceLock<()> = OnceLock::new();
static DEMAND: super::svcctl::Demand = super::svcctl::Demand::new();
/// 最近一次自动刷新的说明（「数据」页显示）。
static AUTO_NOTE: Mutex<String> = Mutex::new(String::new());

pub fn auto_note() -> String {
    AUTO_NOTE.lock().map(|g| g.clone()).unwrap_or_default()
}

/// 该不该自动刷新：没快照、或快照比 12 小时旧。
pub fn needs_refresh(present: bool, generated_ms: i64, now_ms: i64) -> bool {
    !present || now_ms - generated_ms > AUTO_REFRESH_AFTER_MS
}

pub fn snapshot() -> std::sync::Arc<CalReadout> {
    DEMAND.touch();
    POLLER.get_or_init(|| {
        super::spawn_named("ws-calendar-rd", || {
            let mut last_mtime = None;
            let mut auto_at: Option<std::time::Instant> = None;
            loop {
                let w = DEMAND.wait(|| {
                    if let Some(m) = READOUT.get()
                        && let Ok(mut g) = m.lock()
                    {
                        *g = std::sync::Arc::new(CalReadout::default());
                    }
                });
                if w.released {
                    last_mtime = None;
                }
                let mt = std::fs::metadata(board_path()).ok().and_then(|m| m.modified().ok());
                let lock = READOUT.get_or_init(|| Mutex::new(std::sync::Arc::new(CalReadout::default())));
                if mt != last_mtime || mt.is_none() {
                    let snap = poll_once();
                    last_mtime = mt;
                    if let Ok(mut g) = lock.lock() {
                        *g = std::sync::Arc::new(snap);
                    }
                }
                // 自动刷新：面板在显示 + 数据旧了 + 没在跑 + 守护没常驻 + 离上次自动刷新够久
                let cur = lock.lock().map(|g| g.clone()).unwrap_or_default();
                let now = chrono::Local::now().timestamp_millis();
                let due = auto_at.is_none_or(|t| t.elapsed() >= Duration::from_secs(AUTO_RETRY_SECS));
                if due && needs_refresh(cur.present, cur.generated_ms, now) && !super::once::CALENDAR.running() {
                    let active = super::svcctl::query(super::news_readout::SERVICE).active;
                    if !active {
                        let m = super::once::CALENDAR.start(false);
                        let why = if cur.present { "数据超过 12 小时" } else { "还没有日历数据" };
                        if let Ok(mut g) = AUTO_NOTE.lock() {
                            *g = format!("{} {why}，自动刷新：{m}", chrono::Local::now().format("%H:%M"));
                        }
                    }
                    auto_at = Some(std::time::Instant::now());
                }
                std::thread::sleep(Duration::from_millis(1000));
            }
        });
    });
    READOUT
        .get_or_init(|| Mutex::new(std::sync::Arc::new(CalReadout::default())))
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default()
}

fn poll_once() -> CalReadout {
    let refreshed = chrono::Local::now().format("%H:%M:%S").to_string();
    let Ok(t) = std::fs::read_to_string(board_path()) else {
        return CalReadout { refreshed, ..Default::default() };
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) else {
        return CalReadout { refreshed, ..Default::default() };
    };
    let mut r = parse(&v);
    r.refreshed = refreshed;
    r
}

/// 一个事件的修订历史（新的在前）：（字段, 旧, 新, 来源, 时刻毫秒）。
///
/// 只在选中事件时查一次（检查器详情），不在渲染里查。只读打开，库被守护写着也不冲突（WAL）。
/// 一条修订：（字段, 旧值, 新值, 来源, 时刻毫秒）。
pub type Revision = (String, Option<String>, Option<String>, String, i64);

pub fn revisions(event_id: &str) -> Vec<Revision> {
    let Ok(c) = rusqlite::Connection::open_with_flags(db_path(), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return Vec::new();
    };
    let Ok(mut st) = c.prepare("SELECT field, old, new, source_id, at FROM event_revisions WHERE event_id=?1 ORDER BY id DESC LIMIT 20") else {
        return Vec::new();
    };
    st.query_map([event_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
        .map(|it| it.filter_map(|x| x.ok()).collect())
        .unwrap_or_default()
}

// ── 显示：时区 ─────────────────────────────────────────────────────

/// 时刻按哪个时区显示（工具栏 / 检查器里切）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DispTz {
    /// 本机时区。
    #[default]
    Local,
    Utc,
    /// 事件当地（CPI = 美东、ECB = 法兰克福）。
    Event,
}

impl DispTz {
    pub const ALL: [DispTz; 3] = [DispTz::Local, DispTz::Utc, DispTz::Event];
    pub fn label(self) -> &'static str {
        match self {
            DispTz::Local => "本地时间",
            DispTz::Utc => "UTC",
            DispTz::Event => "事件当地",
        }
    }
}

impl std::fmt::Display for DispTz {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// 列表里显示的标题：来源标题是英文原文（BLS / BEA）且有中文系列名时用系列名；
/// 原标题在检查器详情里照样能看到。
pub fn display_title(e: &Ev) -> String {
    let has_cjk = e.title.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c));
    match &e.series_name {
        Some(n) if !has_cjk => n.clone(),
        _ => e.title.clone(),
    }
}

/// 确定性的短写（表格列）。
pub fn precision_short(p: &str) -> &'static str {
    match p {
        "exact" => "精确",
        "conventional" => "≈ 按惯例",
        "date_only" => "仅日期",
        "estimated" => "预计",
        _ => "?",
    }
}

/// 时区名的短称（事件当地时间后面跟着）。
pub fn tz_short(tz: &str) -> &str {
    match tz {
        "America/New_York" => "美东",
        "America/Chicago" => "美中",
        "Europe/Berlin" | "Europe/Frankfurt" => "法兰克福",
        "Europe/London" => "伦敦",
        "Asia/Tokyo" => "东京",
        "UTC" => "UTC",
        other => other.rsplit('/').next().unwrap_or(other),
    }
}

/// 事件在显示时区里落在哪一天。
///
/// **有时刻的按时刻换算**：美东 14:00 的 FOMC 在北京是第二天 02:00，月历上该落在第二天。
/// 没有时刻的（休市、到期日）按当地日期。
pub fn display_date(e: &Ev, tz: DispTz) -> Option<NaiveDate> {
    match (tz, e.scheduled_at) {
        (DispTz::Local, Some(ms)) => chrono::Local.timestamp_millis_opt(ms).single().map(|t| t.date_naive()),
        (DispTz::Utc, Some(ms)) => chrono::DateTime::from_timestamp_millis(ms).map(|t| t.date_naive()),
        _ => e.date_local,
    }
}

/// 时刻文字。没有时刻的写「全天」——**不编一个**。
pub fn display_time(e: &Ev, tz: DispTz) -> String {
    let Some(ms) = e.scheduled_at else { return "全天".into() };
    match tz {
        DispTz::Local => chrono::Local.timestamp_millis_opt(ms).single().map_or_else(String::new, |t| t.format("%H:%M").to_string()),
        DispTz::Utc => chrono::DateTime::from_timestamp_millis(ms).map_or_else(String::new, |t| t.format("%H:%MZ").to_string()),
        DispTz::Event => format!("{} {}", e.time_local.clone().unwrap_or_default(), tz_short(&e.tz)),
    }
}

/// 三种时区一起写（检查器详情用）。
pub fn all_times(e: &Ev) -> String {
    if e.scheduled_at.is_none() {
        return format!("{} 全天（{}）", e.date_local.map(|d| d.to_string()).unwrap_or_default(), tz_short(&e.tz));
    }
    let d = |tz| display_date(e, tz).map(|d| d.format("%m-%d").to_string()).unwrap_or_default();
    format!(
        "{} {} 本地 · {} {} · {} {}",
        d(DispTz::Local),
        display_time(e, DispTz::Local),
        d(DispTz::Utc),
        display_time(e, DispTz::Utc),
        d(DispTz::Event),
        display_time(e, DispTz::Event)
    )
}

// ── 显示：确定性、状态、重要性 ───────────────────────────────────────

/// 确定性的短记号（列表里时刻后面）与说明。**预计的不能画得和确认的一样**（docs/43 §5）。
pub fn precision_mark(p: &str) -> (&'static str, &'static str) {
    match p {
        "exact" => ("", "精确时刻（来源给出）"),
        "conventional" => ("≈", "时刻按惯例（来源只给日期）"),
        "date_only" => ("", "仅日期"),
        "estimated" => ("预计", "预计（规则推算或供应商估计）"),
        _ => ("?", "确定性未知"),
    }
}

pub fn status_label(s: &str) -> Option<&'static str> {
    match s {
        "unlisted" => Some("来源已撤下"),
        "conflict" => Some("与官方不符"),
        "released" => Some("已发布"),
        _ => None,
    }
}

/// 数值行的一句话：「实际 0.4% · 预期 0.3% · 前值 0.4%」。没有的项不写。
pub fn value_text(v: &ValueRow) -> String {
    let mut parts = Vec::new();
    if let Some(a) = &v.actual {
        parts.push(format!("实际 {a}"));
    }
    if let Some(c) = &v.consensus {
        parts.push(format!("预期 {c}"));
    }
    if let Some(p) = &v.previous {
        parts.push(format!("前值 {p}"));
    }
    parts.join(" · ")
}

pub fn importance_label(i: u8) -> &'static str {
    match i {
        0 => "P0",
        1 => "P1",
        2 => "P2",
        _ => "P3",
    }
}

pub fn kind_label(k: &str) -> &'static str {
    match k {
        "macro" => "宏观数据",
        "central_bank" => "央行",
        "auction" => "国债拍卖",
        "expiry" => "到期",
        "delivery" => "合约交割",
        "holiday" => "休市",
        "early_close" => "提前收市",
        "period_end" => "月末 / 季末",
        "earnings" => "财报",
        "dividend" => "除息",
        "ipo" => "新股",
        "listing" => "上新",
        "delisting" => "下架",
        "maintenance" => "维护",
        _ => "其他",
    }
}

pub const KINDS: [&str; 14] = [
    "central_bank",
    "macro",
    "auction",
    "expiry",
    "delivery",
    "holiday",
    "early_close",
    "period_end",
    "earnings",
    "dividend",
    "ipo",
    "listing",
    "delisting",
    "maintenance",
];

pub fn country_label(c: &str) -> &str {
    match c {
        "US" => "美国",
        "EU" => "欧元区",
        "CRYPTO" => "加密",
        "GLOBAL" => "全球",
        "DE" => "德国",
        "GB" => "英国",
        "FR" => "法国",
        "IT" => "意大利",
        "ES" => "西班牙",
        "JP" => "日本",
        "CN" => "中国",
        "CA" => "加拿大",
        "AU" => "澳大利亚",
        "NZ" => "新西兰",
        "CH" => "瑞士",
        "IN" => "印度",
        "KR" => "韩国",
        "BR" => "巴西",
        "RU" => "俄罗斯",
        "SG" => "新加坡",
        "ZA" => "南非",
        other => other,
    }
}

// ── 面板状态（只在面板内存里；守护照常收全部）────────────────────────

/// 页面视图（docs/41：页签锁定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    #[default]
    Month,
    Year,
    Stream,
    /// 提醒：规则、通知中心、免打扰与渠道（docs/43 K4）。
    Alerts,
}

impl View {
    pub const ALL: [View; 4] = [View::Month, View::Year, View::Stream, View::Alerts];
    pub fn label(self) -> &'static str {
        match self {
            View::Month => "月历",
            View::Year => "全年",
            View::Stream => "事件流",
            View::Alerts => "提醒",
        }
    }
}

/// 全年页的两种看法（2026-10-09 用户：两种都要，自由选）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum YearMode {
    /// 从本月起往后 12 个月——「接下来一年有什么」。
    #[default]
    Rolling,
    /// 某一个自然年的 1–12 月——「今年 / 明年整体是什么样」，可以翻年。
    Natural,
}

/// 筛选与显示设置（检查器「属性」页）。
#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    /// 只显示重要性 ≤ 这一档（0 = 只看 P0，3 = 全部）。
    pub max_importance: u8,
    /// 不看的类别。
    pub hidden_kinds: Vec<String>,
    /// 不看的地区。
    pub hidden_countries: Vec<String>,
    /// 显示「来源已撤下」的事件。
    pub show_unlisted: bool,
    /// 搜索（标题 / 系列 / 标的，不分大小写，空格 = 都要有）。
    pub query: String,
    pub tz: DispTz,
    /// 事件流的范围（天）。
    pub stream_days: i64,
}

impl Default for Filter {
    fn default() -> Self {
        Self {
            max_importance: 2,
            hidden_kinds: Vec::new(),
            hidden_countries: Vec::new(),
            show_unlisted: true,
            query: String::new(),
            tz: DispTz::Local,
            stream_days: 30,
        }
    }
}

impl Filter {
    pub fn pass(&self, e: &Ev) -> bool {
        if e.importance > self.max_importance {
            return false;
        }
        if self.hidden_kinds.contains(&e.kind) || self.hidden_countries.contains(&e.country) {
            return false;
        }
        if !self.show_unlisted && e.status == "unlisted" {
            return false;
        }
        let q = self.query.trim().to_lowercase();
        if q.is_empty() {
            return true;
        }
        let hay = format!(
            "{} {} {} {} {}",
            e.title,
            e.series.clone().unwrap_or_default(),
            e.series_name.clone().unwrap_or_default(),
            e.assets.join(" "),
            kind_label(&e.kind)
        )
        .to_lowercase();
        q.split_whitespace().all(|w| hay.contains(w))
    }

    /// 改过几项（工具栏「⚙ 已改 n 项」）。
    pub fn changed(&self) -> Vec<String> {
        let d = Filter::default();
        let mut v = Vec::new();
        if self.max_importance != d.max_importance {
            v.push(format!("重要性 ≤ {}", importance_label(self.max_importance)));
        }
        if !self.hidden_kinds.is_empty() {
            v.push(format!("隐藏 {} 类", self.hidden_kinds.len()));
        }
        if !self.hidden_countries.is_empty() {
            v.push(format!("隐藏 {} 个地区", self.hidden_countries.len()));
        }
        if self.show_unlisted != d.show_unlisted {
            v.push("隐藏已撤下".into());
        }
        if !self.query.trim().is_empty() {
            v.push(format!("搜索「{}」", self.query.trim()));
        }
        if self.tz != d.tz {
            v.push(self.tz.label().into());
        }
        v
    }
}

#[derive(Debug, Default)]
struct Ui {
    view: View,
    /// 选中的日期（`None` = 今天）。
    selected: Option<NaiveDate>,
    /// 月历显示哪个月（年, 月）；`None` = 选中日期所在月。
    cursor: Option<(i32, u32)>,
    /// 选中的事件（检查器详情）。
    event: Option<String>,
    /// 选中事件的修订历史（选中时查一次）。
    revs: Vec<Revision>,
    filter: Option<Filter>,
    year_mode: YearMode,
    /// 自然年模式显示哪一年（`None` = 今年）。
    year: Option<i32>,
    /// 「新建规则」表单的草稿。
    draft: Option<super::calendar_alerts::Rule>,
    /// 免打扰的编辑值（保存前）：`(开始, 结束, P0 例外)`。
    quiet_edit: Option<(String, String, bool)>,
    /// 提醒页最近一次操作的回执。
    alert_note: String,
}

static UI: Mutex<Option<Ui>> = Mutex::new(None);

fn with_ui<T>(f: impl FnOnce(&mut Ui) -> T) -> T {
    let mut g = UI.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(Ui::default))
}

pub fn view() -> View {
    with_ui(|u| u.view)
}
pub fn set_view(v: View) {
    with_ui(|u| u.view = v);
}

pub fn filter() -> Filter {
    with_ui(|u| u.filter.clone().unwrap_or_default())
}
pub fn update_filter(f: impl FnOnce(&mut Filter)) {
    with_ui(|u| f(u.filter.get_or_insert_with(Filter::default)));
}

/// 今天（以快照里的为准，没有快照用本机日期）。
pub fn today(snap: &CalReadout) -> NaiveDate {
    snap.today.unwrap_or_else(|| chrono::Local::now().date_naive())
}

pub fn selected(snap: &CalReadout) -> NaiveDate {
    with_ui(|u| u.selected).unwrap_or_else(|| today(snap))
}

pub fn select(d: NaiveDate) {
    with_ui(|u| {
        u.selected = Some(d);
        u.cursor = Some((d.year(), d.month()));
    });
}

pub fn cursor(snap: &CalReadout) -> (i32, u32) {
    let sel = selected(snap);
    with_ui(|u| u.cursor).unwrap_or((sel.year(), sel.month()))
}

/// 翻月：`delta` 个月。
pub fn shift_month(snap: &CalReadout, delta: i32) {
    let (y, m) = cursor(snap);
    let idx = y * 12 + m as i32 - 1 + delta;
    with_ui(|u| u.cursor = Some((idx.div_euclid(12), idx.rem_euclid(12) as u32 + 1)));
}

pub fn go_today(snap: &CalReadout) {
    select(today(snap));
}

pub fn year_mode() -> YearMode {
    with_ui(|u| u.year_mode)
}

pub fn set_year_mode(m: YearMode) {
    with_ui(|u| u.year_mode = m);
}

/// 自然年模式显示的年份。
pub fn natural_year(snap: &CalReadout) -> i32 {
    with_ui(|u| u.year).unwrap_or_else(|| today(snap).year())
}

pub fn shift_year(snap: &CalReadout, delta: i32) {
    let y = natural_year(snap) + delta;
    with_ui(|u| u.year = Some(y));
}

/// 快照覆盖的日期范围（守护写的 `from` / `to`）。
pub fn window(snap: &CalReadout) -> Option<(NaiveDate, NaiveDate)> {
    Some((snap.from?, snap.to?))
}

/// 新建规则的缺省草稿：系列「美国 CPI」、提前 1 天与 30 分钟、应用内 + 桌面。
pub fn default_draft() -> super::calendar_alerts::Rule {
    use super::calendar_alerts::{Channel, Rule, Scope};
    Rule {
        id: 0,
        name: String::new(),
        scope: Scope::Series,
        target: "us-cpi".into(),
        max_importance: 1,
        offsets: vec![1440, 30],
        channels: vec![Channel::InApp, Channel::Desktop],
        on_change: true,
        on_release: false,
        enabled: true,
    }
}

pub fn draft() -> super::calendar_alerts::Rule {
    with_ui(|u| u.draft.clone()).unwrap_or_else(default_draft)
}

pub fn update_draft(f: impl FnOnce(&mut super::calendar_alerts::Rule)) {
    with_ui(|u| f(u.draft.get_or_insert_with(default_draft)));
}

pub fn reset_draft() {
    with_ui(|u| u.draft = None);
}

pub fn quiet_edit(saved: &super::calendar_alerts::Quiet) -> (String, String, bool) {
    with_ui(|u| u.quiet_edit.clone()).unwrap_or_else(|| (saved.from.clone(), saved.to.clone(), saved.p0_exempt))
}

pub fn set_quiet_edit(v: (String, String, bool)) {
    with_ui(|u| u.quiet_edit = Some(v));
}

pub fn clear_quiet_edit() {
    with_ui(|u| u.quiet_edit = None);
}

pub fn alert_note() -> String {
    with_ui(|u| u.alert_note.clone())
}

pub fn set_alert_note(t: impl Into<String>) {
    let t = t.into();
    with_ui(|u| u.alert_note = t);
}

pub fn selected_event() -> Option<String> {
    with_ui(|u| u.event.clone())
}

pub fn select_event(id: &str) {
    let revs = revisions(id);
    with_ui(|u| {
        u.event = Some(id.to_string());
        u.revs = revs;
    });
}

pub fn selected_revisions() -> Vec<Revision> {
    with_ui(|u| u.revs.clone())
}

/// 某天（显示时区）的事件，按时刻、再按重要性排；过筛选。
pub fn events_on<'a>(snap: &'a CalReadout, d: NaiveDate, f: &Filter) -> Vec<&'a Ev> {
    let mut v: Vec<&Ev> = snap.events.iter().filter(|e| display_date(e, f.tz) == Some(d) && f.pass(e)).collect();
    v.sort_by_key(|e| (e.scheduled_at.unwrap_or(i64::MIN), e.importance));
    v
}

/// 月历格子里先放哪几个：重要的在前，同档按时刻。
pub fn top_for_cell<'a>(evs: &[&'a Ev]) -> Vec<&'a Ev> {
    let mut v = evs.to_vec();
    v.sort_by_key(|e| (e.importance, e.scheduled_at.unwrap_or(i64::MIN)));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(imp: u8, kind: &str, ms: Option<i64>, date: &str) -> Ev {
        Ev {
            importance: imp,
            kind: kind.into(),
            scheduled_at: ms,
            date_local: NaiveDate::parse_from_str(date, "%Y-%m-%d").ok(),
            tz: "America/New_York".into(),
            time_local: ms.map(|_| "14:00".into()),
            title: "FOMC 利率决议".into(),
            status: "scheduled".into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_timed_event_lands_on_the_day_it_happens_in_the_display_timezone() {
        // 美东 2026-10-28 14:00 = 18:00Z：UTC 下还是 10-28；当地日期也是 10-28
        let e = ev(0, "central_bank", Some(1_793_210_400_000), "2026-10-28");
        assert_eq!(display_date(&e, DispTz::Utc), NaiveDate::from_ymd_opt(2026, 10, 28));
        assert_eq!(display_date(&e, DispTz::Event), NaiveDate::from_ymd_opt(2026, 10, 28));
        // 没有时刻的按当地日期，不换算
        let h = ev(1, "holiday", None, "2026-11-26");
        assert_eq!(display_date(&h, DispTz::Utc), NaiveDate::from_ymd_opt(2026, 11, 26));
        assert_eq!(display_time(&h, DispTz::Local), "全天");
        assert_eq!(display_time(&e, DispTz::Event), "14:00 美东");
    }

    #[test]
    fn filter_by_importance_kind_and_words() {
        let mut f = Filter::default();
        let e = ev(0, "central_bank", None, "2026-10-28");
        assert!(f.pass(&e));
        assert!(!f.pass(&ev(3, "period_end", None, "2026-10-30")), "默认只看 P0–P2");
        f.hidden_kinds.push("central_bank".into());
        assert!(!f.pass(&e));
        f.hidden_kinds.clear();
        f.query = "fomc 决议".into();
        assert!(f.pass(&e));
        f.query = "fomc cpi".into();
        assert!(!f.pass(&e), "空格 = 都要有");
        assert_eq!(f.changed().len(), 1);
    }

    #[test]
    fn stale_or_missing_data_triggers_one_refresh() {
        let now = 1_800_000_000_000;
        assert!(needs_refresh(false, 0, now), "还没有快照");
        assert!(needs_refresh(true, now - AUTO_REFRESH_AFTER_MS - 1, now));
        assert!(!needs_refresh(true, now - 3600_000, now));
    }

    #[test]
    fn parses_the_board_written_by_the_daemon() {
        let v = serde_json::json!({
            "generated_ms": 5, "today": "2026-10-09",
            "events": [{"id":"bls-schedule:u1","series":"us-cpi","kind":"macro","title":"Consumer Price Index","country":"US",
                        "tz":"America/New_York","date_local":"2026-10-14","time_local":"08:30","scheduled_at":1_792_067_400_000_i64,
                        "precision":"exact","status":"scheduled","importance":1,"assets":["ES"],"sources":["bls-schedule"],"revisions":0}],
            "sources": [{"id":"bls-schedule","label":"BLS","org":"美国劳工统计局","coverage_to":"2026-12-30","coverage_days_left":82,"last_report":"x"}]
        });
        let r = parse(&v);
        assert!(r.present);
        assert_eq!(r.events[0].importance, 1);
        assert_eq!(r.events[0].date_local, NaiveDate::from_ymd_opt(2026, 10, 14));
        assert_eq!(r.sources[0].coverage_days_left, Some(82));
        assert_eq!(today(&r), NaiveDate::from_ymd_opt(2026, 10, 9).unwrap());
    }
}
