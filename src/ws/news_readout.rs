//! 新闻面板的旁路读数（docs/25 N2）。
//!
//! 面板里**一行网络代码都没有**——所有连接在 `ws-news` 守护里，
//! 这边只读 tmpfs 上的 `news_board.json`。

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

pub const SERVICE: &str = "ws-news";

/// 一个源的健康。
#[derive(Default, Clone, PartialEq)]
pub struct SourceRow {
    pub id: String,
    pub label: String,
    pub tier: String,
    pub tier_label: String,
    /// 源地址。**测试按钮要用它**——表格里的按钮只有 id，
    /// 而要测的是地址
    pub url: String,
    pub lang: String,
    /// 见过的最新一条（毫秒）。**不受时间窗影响**——库里出窗了它还在。
    pub newest_ms: Option<i64>,
    /// 窗内还剩几条。和 `newest_ms` 是两件事。
    pub in_window: i64,
    pub stale_secs: Option<i64>,
    pub stale_after_secs: i64,
    /// **这一行是这一页的理由**：HTTP 200 也可能是死的。
    pub is_stale: bool,
    /// **源不给时间戳**（实测 ESMA 就是）。和「还没抓到」是两回事——
    /// 都显示成「—」的话，一个好源看起来像没通。
    pub no_timestamps: bool,
    /// 我们上次从这个源看到新条目的时刻。源不给时间时，看门狗靠它。
    pub last_new_ms: Option<i64>,
    /// 内置的源**不能删只能关**：删了下次启动又被播种回来，
    /// 那种「删不掉」比不给删更让人困惑。
    pub builtin: bool,
    pub enabled: bool,
    pub ok: i64,
    pub not_modified: i64,
    pub fails: i64,
    pub consecutive_fails: i64,
    pub last_status: String,
    pub seeded: bool,
}

impl SourceRow {
    /// 从没抓到过任何条目。**和「陈了」是两回事**：可能只是刚启动。
    pub fn never_seen(&self) -> bool {
        self.newest_ms.is_none()
    }
    /// 一直在失败。
    pub fn failing(&self) -> bool {
        self.consecutive_fails > 0
    }
}

#[derive(Default, Clone, PartialEq)]
pub struct NewsRow {
    pub id: i64,
    pub source: String,
    pub tier: String,
    pub kind: String,
    pub title: String,
    pub url: String,
    pub lang: String,
    pub published_ms: Option<i64>,
    /// 「说的那件事什么时候发生」。和发布时间能差一个月。
    pub effective_ms: Option<i64>,
    pub sort_ms: i64,
    /// 排序用的时间是猜的（源没给发布时间）。**必须显示**。
    pub time_guessed: bool,
    pub symbols: Vec<String>,
    /// 还有哪些源发了同一条。
    pub dupes: Vec<String>,
    pub revised: bool,
}

/// 「测试这个源」的结果。
#[derive(Default, Clone, PartialEq)]
pub struct ProbeView {
    pub url: String,
    pub status: u16,
    pub ms: i64,
    pub bytes: i64,
    pub is_feed: bool,
    pub items: i64,
    pub newest_age_secs: Option<i64>,
    pub verdict: String,
    pub ok: bool,
}

/// 一条检索命中。
#[derive(Default, Clone, PartialEq)]
pub struct SearchHit {
    pub who: String,
    pub form: String,
    pub date: String,
    pub url: String,
    /// 8-K 的条款码翻成人话。
    pub what: String,
}

/// 检索结果。**和时间线分开**：时间线是「最近发生了什么」，
/// 这里是「帮我找东西」。
#[derive(Default, Clone, PartialEq)]
pub struct SearchView {
    pub q: String,
    /// 失败也要说出来——空结果和「没搜到」看起来一样。
    pub status: String,
    pub hits: Vec<SearchHit>,
}

/// 一个订阅的标的。
#[derive(Default, Clone, PartialEq)]
pub struct WatchRow {
    pub symbol: String,
    pub ok: i64,
    pub not_modified: i64,
    pub fails: i64,
    pub last_status: String,
    pub newest_ms: Option<i64>,
    pub in_window: i64,
}

#[derive(Default, Clone)]
pub struct NewsReadout {
    pub present: bool,
    pub stamp: String,
    pub now_ms: i64,
    pub total: i64,
    pub stale_sources: i64,
    pub sources: Vec<SourceRow>,
    pub items: Vec<NewsRow>,
    pub watch: Vec<WatchRow>,
    pub search: SearchView,
    pub probe: Option<ProbeView>,
    pub refreshed: String,
    pub svc: super::svcctl::UnitState,
}

fn s(v: &serde_json::Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}
fn i(v: &serde_json::Value, k: &str) -> i64 {
    v.get(k).and_then(|x| x.as_i64()).unwrap_or(0)
}
fn oi(v: &serde_json::Value, k: &str) -> Option<i64> {
    v.get(k).and_then(|x| x.as_i64())
}
fn b(v: &serde_json::Value, k: &str) -> bool {
    v.get(k).and_then(|x| x.as_bool()).unwrap_or(false)
}
fn strs(v: &serde_json::Value, k: &str) -> Vec<String> {
    v.get(k)
        .and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

pub fn parse(v: &serde_json::Value) -> NewsReadout {
    NewsReadout {
        present: true,
        stamp: s(v, "stamp"),
        now_ms: i(v, "now_ms"),
        total: i(v, "total"),
        stale_sources: i(v, "stale_sources"),
        sources: v
            .get("sources")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .map(|r| SourceRow {
                        id: s(r, "id"),
                        label: s(r, "label"),
                        tier: s(r, "tier"),
                        tier_label: s(r, "tier_label"),
                        url: s(r, "url"),
                        lang: s(r, "lang"),
                        newest_ms: oi(r, "newest_ms"),
                        in_window: i(r, "in_window"),
                        stale_secs: oi(r, "stale_secs"),
                        stale_after_secs: i(r, "stale_after_secs"),
                        is_stale: b(r, "is_stale"),
                        no_timestamps: b(r, "no_timestamps"),
                        last_new_ms: oi(r, "last_new_ms"),
                        builtin: b(r, "builtin"),
                        enabled: v.get("sources").is_some() && b(r, "enabled"),
                        ok: i(r, "ok"),
                        not_modified: i(r, "not_modified"),
                        fails: i(r, "fails"),
                        consecutive_fails: i(r, "consecutive_fails"),
                        last_status: s(r, "last_status"),
                        seeded: b(r, "seeded"),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        items: v
            .get("items")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .map(|r| NewsRow {
                        id: i(r, "id"),
                        source: s(r, "source"),
                        tier: s(r, "tier"),
                        kind: s(r, "kind"),
                        title: s(r, "title"),
                        url: s(r, "url"),
                        lang: s(r, "lang"),
                        published_ms: oi(r, "published_ms"),
                        effective_ms: oi(r, "effective_ms"),
                        sort_ms: i(r, "sort_ms"),
                        time_guessed: b(r, "time_guessed"),
                        symbols: strs(r, "symbols"),
                        dupes: strs(r, "dupes"),
                        revised: b(r, "revised"),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        watch: v
            .get("watch")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .map(|r| WatchRow {
                        symbol: s(r, "symbol"),
                        ok: i(r, "ok"),
                        not_modified: i(r, "not_modified"),
                        fails: i(r, "fails"),
                        last_status: s(r, "last_status"),
                        newest_ms: oi(r, "newest_ms"),
                        in_window: i(r, "in_window"),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        search: v
            .get("search")
            .map(|x| SearchView {
                q: s(x, "q"),
                status: s(x, "status"),
                hits: x
                    .get("hits")
                    .and_then(|h| h.as_array())
                    .map(|a| {
                        a.iter()
                            .map(|h| SearchHit {
                                who: s(h, "who"),
                                form: s(h, "form"),
                                date: s(h, "date"),
                                url: s(h, "url"),
                                what: s(h, "what"),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            })
            .unwrap_or_default(),
        probe: v.get("probe").filter(|x| !x.is_null()).map(|x| ProbeView {
            url: s(x, "url"),
            status: i(x, "status") as u16,
            ms: i(x, "ms"),
            bytes: i(x, "bytes"),
            is_feed: b(x, "is_feed"),
            items: i(x, "items"),
            newest_age_secs: oi(x, "newest_age_secs"),
            verdict: s(x, "verdict"),
            ok: b(x, "ok"),
        }),
        refreshed: String::new(),
        svc: Default::default(),
    }
}

// ── 源管理（用户自定义源）──

/// 用户那份源清单的位置。**和守护读的是同一个文件**——
/// 两边各存一份的话，面板显示加了而守护没加。
pub fn sources_path() -> std::path::PathBuf {
    #[cfg(test)]
    if let Some(p) = tests::src_override() {
        return p;
    }
    if let Ok(p) = std::env::var("WS_NEWS_SOURCES") {
        return std::path::PathBuf::from(p);
    }
    let base = std::env::var("XDG_CONFIG_HOME").map(std::path::PathBuf::from).unwrap_or_else(|_| {
        std::path::PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".config")
    });
    base.join("wealthspring").join("news_sources.json")
}

/// 用户源清单里的一条。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UserSource {
    pub id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub tier: String,
    #[serde(default)]
    pub lang: String,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

pub fn read_sources() -> Vec<UserSource> {
    std::fs::read_to_string(sources_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_sources(v: &[UserSource]) {
    let p = sources_path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    // 原子写：半截 JSON 会让守护当成一个自定义源都没有
    let tmp = p.with_extension("json.tmp");
    if let Ok(t) = serde_json::to_string_pretty(v) {
        if std::fs::write(&tmp, t).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

/// 开/关一个源（内置的也能关）。
pub fn set_enabled(id: &str, on: bool) {
    let mut v = read_sources();
    match v.iter_mut().find(|u| u.id == id) {
        Some(u) => u.enabled = on,
        None => v.push(UserSource {
            id: id.to_string(),
            label: String::new(),
            url: String::new(),
            tier: String::new(),
            lang: String::new(),
            enabled: on,
        }),
    }
    write_sources(&v);
}

/// 加一个自定义源。返回错误消息（空 = 成功）。
pub fn add_source(id: &str, label: &str, url: &str, tier: &str) -> String {
    let id = id.trim().to_ascii_lowercase();
    let url = url.trim();
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return "id 只能是字母数字和 - _".into();
    }
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return "地址要以 http:// 或 https:// 开头".into();
    }
    let mut v = read_sources();
    if v.iter().any(|u| u.id == id) {
        return format!("已经有一个叫「{id}」的源了");
    }
    v.push(UserSource {
        id,
        label: if label.trim().is_empty() { url.to_string() } else { label.trim().to_string() },
        url: url.to_string(),
        tier: if tier.trim().is_empty() { "media".into() } else { tier.trim().to_string() },
        lang: "en".into(),
        enabled: true,
    });
    write_sources(&v);
    String::new()
}

/// 删一个自定义源。**内置的删不掉**——它下次启动会被播种回来。
pub fn remove_source(id: &str) {
    let v: Vec<UserSource> = read_sources().into_iter().filter(|u| u.id != id).collect();
    write_sources(&v);
}

// ── 订阅（N5）：面板 → 守护的请求文件 ──

fn request_path() -> std::path::PathBuf {
    #[cfg(test)]
    if let Some(p) = tests::req_override() {
        return p;
    }
    if let Ok(p) = std::env::var("WS_NEWS_REQUEST") {
        return std::path::PathBuf::from(p);
    }
    board_path().with_file_name("news_request.json")
}

/// 面板的两个视图。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    /// 只看新闻。默认——**打开就该看到新闻**，而不是先看一屏运维信息。
    #[default]
    Feed,
    /// 源健康 + 源管理。
    Sources,
}

impl View {
    pub const ALL: [View; 2] = [View::Feed, View::Sources];
    pub fn label(self) -> &'static str {
        match self {
            View::Feed => "新闻",
            View::Sources => "源管理",
        }
    }
}

static VIEW: Mutex<View> = Mutex::new(View::Feed);

pub fn view() -> View {
    VIEW.lock().map(|g| *g).unwrap_or_default()
}

pub fn set_view(v: View) {
    if let Ok(mut g) = VIEW.lock() {
        *g = v;
    }
}

/// 「加源」表单：`(id, 名字, 地址, 分级)`。
static ADD_FORM: Mutex<(String, String, String, String)> =
    Mutex::new((String::new(), String::new(), String::new(), String::new()));

pub fn add_form() -> (String, String, String, String) {
    ADD_FORM.lock().map(|g| g.clone()).unwrap_or_default()
}

pub fn set_add_form(field: &str, v: &str) {
    if let Ok(mut g) = ADD_FORM.lock() {
        match field {
            "id" => g.0 = v.to_string(),
            "label" => g.1 = v.to_string(),
            "url" => g.2 = v.to_string(),
            "tier" => g.3 = v.to_string(),
            _ => {}
        }
    }
}

pub fn clear_add_form() {
    if let Ok(mut g) = ADD_FORM.lock() {
        *g = Default::default();
    }
}

/// 加源时的提示（错误或成功）。
static ADD_NOTE: Mutex<String> = Mutex::new(String::new());

pub fn add_note() -> String {
    ADD_NOTE.lock().map(|g| g.clone()).unwrap_or_default()
}

pub fn set_add_note(t: &str) {
    if let Ok(mut g) = ADD_NOTE.lock() {
        *g = t.to_string();
    }
}

/// 让守护测一个源。走请求文件的 `probe` 字段，**局部修改**。
pub fn request_probe(url: &str) {
    use std::sync::atomic::{AtomicI64, Ordering};
    static N: AtomicI64 = AtomicI64::new(5000);
    let p = request_path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let mut v: serde_json::Value = std::fs::read_to_string(&p)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .filter(|x: &serde_json::Value| x.is_object())
        .unwrap_or_else(|| serde_json::json!({}));
    v["probe"] = serde_json::json!(url);
    // 按 nonce 触发：对同一个源再点一次「测试」是想重测
    v["nonce"] = serde_json::json!(N.fetch_add(1, Ordering::Relaxed));
    let tmp = p.with_extension("json.tmp");
    if std::fs::write(&tmp, v.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, &p);
    }
}

/// 检索框。
static SEARCH_INPUT: Mutex<String> = Mutex::new(String::new());

pub fn search_input() -> String {
    SEARCH_INPUT.lock().map(|g| g.clone()).unwrap_or_default()
}

pub fn set_search_input(t: &str) {
    if let Ok(mut g) = SEARCH_INPUT.lock() {
        *g = t.to_string();
    }
}

/// 把检索请求写给守护。保留当前订阅——**整份重写会把订阅冲掉**
/// （docs/22 §7.7、docs/23 §13k 都栽过：局部修改要从磁盘现状起步）。
pub fn write_search(q: &str, forms: &str) {
    use std::sync::atomic::{AtomicI64, Ordering};
    static N: AtomicI64 = AtomicI64::new(1000);
    let p = request_path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let mut v: serde_json::Value = std::fs::read_to_string(&p)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .filter(|x: &serde_json::Value| x.is_object())
        .unwrap_or_else(|| serde_json::json!({}));
    v["search"] = serde_json::json!(q);
    v["search_forms"] = serde_json::json!(forms);
    v["nonce"] = serde_json::json!(N.fetch_add(1, Ordering::Relaxed));
    let tmp = p.with_extension("json.tmp");
    if std::fs::write(&tmp, v.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, &p);
    }
}

/// 订阅列表的编辑框。
static WATCH_INPUT: Mutex<String> = Mutex::new(String::new());

pub fn watch_input() -> String {
    WATCH_INPUT.lock().map(|g| g.clone()).unwrap_or_default()
}

pub fn set_watch_input(t: &str) {
    if let Ok(mut g) = WATCH_INPUT.lock() {
        *g = t.to_string();
    }
}

/// 把订阅列表写给守护。**原子写**：半截 JSON 会让守护当成没有订阅。
pub fn write_watch(symbols: &[String]) {
    use std::sync::atomic::{AtomicI64, Ordering};
    static N: AtomicI64 = AtomicI64::new(1);
    let p = request_path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let mut v: serde_json::Value = std::fs::read_to_string(&p)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .filter(|x: &serde_json::Value| x.is_object())
        .unwrap_or_else(|| serde_json::json!({}));
    v["watch"] = serde_json::json!(symbols);
    // 只增计数：布尔要守护读完清掉，那就得由守护写回请求文件，
    // 于是两个进程同时写一个文件（docs/22 §7.8）
    v["nonce"] = serde_json::json!(N.fetch_add(1, Ordering::Relaxed));
    let tmp = p.with_extension("json.tmp");
    if std::fs::write(&tmp, v.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, &p);
    }
}

/// 读回当前订阅（面板重启后要能接上）。
pub fn read_watch() -> Vec<String> {
    std::fs::read_to_string(request_path())
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| {
            v.get("watch")?
                .as_array()
                .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        })
        .unwrap_or_default()
}

/// 规范化一个用户输入的标的。返回 `None` = 不是个能用的 ticker。
///
/// 和守护那边同一套规则——**两边不一致的话，面板显示订阅了而守护没订**。
pub fn clean_symbol(s: &str) -> Option<String> {
    let s = s.trim().to_ascii_uppercase();
    if s.is_empty() || s.len() > 12 {
        return None;
    }
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        .then_some(s)
}

pub fn board_path() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("WS_NEWS_BOARD") {
        return std::path::PathBuf::from(p);
    }
    let base = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    std::path::PathBuf::from(base).join("wealthspring").join("news_board.json")
}

static READOUT: OnceLock<Mutex<std::sync::Arc<NewsReadout>>> = OnceLock::new();
static POLLER: OnceLock<()> = OnceLock::new();

pub fn snapshot() -> std::sync::Arc<NewsReadout> {
    POLLER.get_or_init(|| {
        std::thread::spawn(|| loop {
            let mut snap = poll_once();
            snap.svc = super::svcctl::query(SERVICE);
            let lock =
                READOUT.get_or_init(|| Mutex::new(std::sync::Arc::new(NewsReadout::default())));
            if let Ok(mut g) = lock.lock() {
                *g = std::sync::Arc::new(snap);
            }
            // 读文件 + 解析在**后台线程**。渲染线程只取内存快照
            std::thread::sleep(Duration::from_millis(800));
        });
    });
    READOUT
        .get_or_init(|| Mutex::new(std::sync::Arc::new(NewsReadout::default())))
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default()
}

fn poll_once() -> NewsReadout {
    let refreshed = chrono::Local::now().format("%H:%M:%S").to_string();
    let Ok(t) = std::fs::read_to_string(board_path()) else {
        return NewsReadout { refreshed, ..Default::default() };
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) else {
        return NewsReadout { refreshed, ..Default::default() };
    };
    let mut r = parse(&v);
    r.refreshed = refreshed;
    r
}

/// 启停守护。**必须 `--no-block`**：`systemctl start` 会等 unit 就绪才返回，
/// 足以冻死 UI 线程（观察终端/预测/工厂三处都踩过）。
pub fn svc_action(action: &str) -> String {
    match std::process::Command::new("systemctl")
        .args(["--user", action, "--no-block", SERVICE])
        .status()
    {
        Ok(s) if s.success() => format!(
            "✔ 新闻守护已{}",
            match action {
                "start" => "启动",
                "stop" => "停止",
                _ => "操作",
            }
        ),
        Ok(s) => format!("✗ systemctl {action} 退出码 {:?}", s.code()),
        Err(e) => format!("✗ systemctl 失败：{e}"),
    }
}

/// 面板侧的筛选框。**只在面板内存里**——守护那边不需要知道，
/// 它照常收全部；筛选只影响这一屏看到什么。
static FILTER: Mutex<String> = Mutex::new(String::new());

pub fn filter_text() -> String {
    FILTER.lock().map(|g| g.clone()).unwrap_or_default()
}

pub fn set_filter_text(t: &str) {
    if let Ok(mut g) = FILTER.lock() {
        *g = t.to_string();
    }
}

/// 一条新闻中不中当前筛选。
///
/// 语法和守护那边 `query.rs` 一致（词=AND、`"词组"`、`-排除`、
/// `tier:`/`kind:`/`sym:`/`lang:`/`src:`）。这里是个精简实现：
/// 面板不该为了一个搜索框去依赖守护那个 crate。
pub fn matches(q: &str, it: &NewsRow) -> bool {
    let terms = tokenize(q);
    if terms.is_empty() {
        // **空查询看到全部**，不是零条
        return true;
    }
    let hay = format!(
        "{} {} {}",
        it.title.to_lowercase(),
        it.source.to_lowercase(),
        it.symbols.join(" ").to_lowercase()
    );
    terms.iter().all(|t| {
        let (neg, body) = match t.strip_prefix('-') {
            Some(r) if !r.is_empty() => (true, r),
            _ => (false, t.as_str()),
        };
        let hit = match body.split_once(':') {
            Some((f, v)) if !v.is_empty() => {
                let v = v.to_ascii_lowercase();
                match f.to_ascii_lowercase().as_str() {
                    "tier" => it.tier == v,
                    "kind" => it.kind == v,
                    "lang" => it.lang.eq_ignore_ascii_case(&v),
                    "src" => it.source.eq_ignore_ascii_case(&v),
                    "sym" => it.symbols.iter().any(|s| s.eq_ignore_ascii_case(&v)),
                    // 认不出的字段名当普通关键词，行为可预期
                    _ => hay.contains(&body.to_lowercase()),
                }
            }
            _ => hay.contains(&body.to_lowercase()),
        };
        hit != neg
    })
}

fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut neg = false;
    for c in s.chars() {
        match c {
            '"' => {
                if quoted && !cur.is_empty() {
                    out.push(if neg { format!("-{cur}") } else { cur.clone() });
                    cur.clear();
                    neg = false;
                }
                quoted = !quoted;
            }
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                neg = false;
            }
            '-' if cur.is_empty() && !quoted => neg = true,
            c => {
                if neg && cur.is_empty() {
                    cur.push('-');
                    neg = false;
                }
                cur.push(c);
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// 人读的「多久以前」。
pub fn ago(ms: i64, now_ms: i64) -> String {
    let s = ((now_ms - ms) / 1000).max(0);
    match s {
        0..=59 => format!("{s}秒前"),
        60..=3599 => format!("{}分前", s / 60),
        3600..=86399 => format!("{}小时前", s / 3600),
        _ => format!("{}天前", s / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试期间请求文件指到临时目录。默认就改——不改的话跑一遍测试
    /// 就会写进用户正在用的那份订阅（观察终端那次的教训，docs/23 §13k）。
    static REQ_PATH: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);

    /// 碰请求文件的测试都要先拿这把锁。
    ///
    /// 请求文件和 `REQ_PATH` 都是**进程级**的，并行跑必然互踩，
    /// 而且踩起来是「另一个测试写的订阅出现在我的断言里」，看着像逻辑错。
    /// 这是本项目第五次栽在同一个形状上（IPO 月份、连接表单、请求文件、
    /// 连接历史，现在是这个）。
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn lock_for_test() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    static SRC_PATH: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);

    pub(super) fn src_override() -> Option<std::path::PathBuf> {
        Some(SRC_PATH.lock().ok().and_then(|g| g.clone()).unwrap_or_else(|| {
            std::env::temp_dir()
                .join(format!("ws-news-test-{}", std::process::id()))
                .join("news_sources.json")
        }))
    }

    pub(super) fn req_override() -> Option<std::path::PathBuf> {
        Some(REQ_PATH.lock().ok().and_then(|g| g.clone()).unwrap_or_else(|| {
            std::env::temp_dir()
                .join(format!("ws-news-test-{}", std::process::id()))
                .join("news_request.json")
        }))
    }

    #[test]
    fn a_user_source_round_trips_and_a_builtin_can_be_turned_off_but_not_deleted() {
        let _g = lock_for_test();
        let dir = std::env::temp_dir().join(format!("ws-news-s-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("news_sources.json");
        let _ = std::fs::remove_file(&p);
        if let Ok(mut g) = SRC_PATH.lock() {
            *g = Some(p.clone());
        }
        assert!(read_sources().is_empty(), "没有文件时是空清单，不是报错");

        // 加一个
        assert_eq!(add_source("myfeed", "我的源", "https://example.com/rss", "media"), "");
        assert_eq!(read_sources().len(), 1);
        assert_eq!(read_sources()[0].url, "https://example.com/rss");

        // 挡住会变成坏 URL 或坏 id 的输入
        assert!(!add_source("my feed", "x", "https://a/b", "").is_empty(), "id 有空格该拒");
        assert!(!add_source("ok", "x", "ftp://a/b", "").is_empty(), "非 http 该拒");
        assert!(!add_source("myfeed", "x", "https://c/d", "").is_empty(), "重名该拒");
        assert_eq!(read_sources().len(), 1, "被拒的一个都不该写进去");

        // **内置源：只关不删**。写进清单的是一条「关掉」记录
        set_enabled("coindesk", false);
        let v = read_sources();
        assert_eq!(v.len(), 2);
        assert!(v.iter().any(|u| u.id == "coindesk" && !u.enabled && u.url.is_empty()));
        set_enabled("coindesk", true);
        assert!(read_sources().iter().any(|u| u.id == "coindesk" && u.enabled));

        // 自定义源可以删
        remove_source("myfeed");
        assert!(!read_sources().iter().any(|u| u.id == "myfeed"));

        if let Ok(mut g) = SRC_PATH.lock() {
            *g = None;
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writing_one_field_does_not_wipe_the_other() {
        let _g = lock_for_test();
        // docs/22 §7.7 和 docs/23 §13k 都栽过：局部修改要从磁盘现状起步，
        // 整份重写会把别人写的键冲掉。这里是订阅和检索互相冲
        let dir = std::env::temp_dir().join(format!("ws-news-x-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("news_request.json");
        let _ = std::fs::remove_file(&p);
        if let Ok(mut g) = REQ_PATH.lock() {
            *g = Some(p.clone());
        }
        write_watch(&["AAPL".into()]);
        write_search("material weakness", "8-K");
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["watch"][0], "AAPL", "写检索把订阅冲掉了");
        assert_eq!(v["search"], "material weakness");

        write_watch(&["AAPL".into(), "NVDA".into()]);
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["search"], "material weakness", "写订阅把检索冲掉了");
        assert_eq!(v["watch"].as_array().unwrap().len(), 2);

        if let Ok(mut g) = REQ_PATH.lock() {
            *g = None;
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_symbol_is_cleaned_the_same_way_on_both_sides() {
        // 两边规则不一致的话，面板显示订阅了而守护根本没订
        assert_eq!(clean_symbol(" aapl "), Some("AAPL".into()));
        assert_eq!(clean_symbol("brk-b"), Some("BRK-B".into()));
        assert_eq!(clean_symbol("ok.a"), Some("OK.A".into()));
        for bad in ["", "  ", "A B", "../../etc", "TOOOOOOOOLONGSYM", "中文"] {
            assert_eq!(clean_symbol(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_watch_list_round_trips_through_the_request_file() {
        let _g = lock_for_test();
        let dir = std::env::temp_dir().join(format!("ws-news-w-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("news_request.json");
        if let Ok(mut g) = REQ_PATH.lock() {
            *g = Some(p.clone());
        }
        assert!(read_watch().is_empty(), "没有文件时是空订阅，不是报错");
        write_watch(&["AAPL".into(), "NVDA".into()]);
        assert_eq!(read_watch(), vec!["AAPL".to_string(), "NVDA".to_string()]);
        // nonce 要变，否则守护认不出订阅改了
        let a: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        write_watch(&["AAPL".into()]);
        let b: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert!(b["nonce"].as_i64() > a["nonce"].as_i64(), "nonce 只增");
        assert_eq!(read_watch(), vec!["AAPL".to_string()]);
        if let Ok(mut g) = REQ_PATH.lock() {
            *g = None;
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stale_source_survives_the_trip_from_daemon_to_panel() {
        // **N2 的判据**：一个 HTTP 200 但内容陈掉的源，界面上要能红。
        // 守护那边判，面板只负责显示——所以这条要一路验到读数
        let j = r#"{"stamp":"12:00","now_ms":1788930000000,"total":3,"stale_sources":1,
          "sources":[
            {"id":"okx","label":"OKX 公告","tier":"exchange","tier_label":"交易所",
             "newest_ms":1788357366699,"in_window":0,"stale_secs":572633,
             "stale_after_secs":432000,"is_stale":true,"ok":5,"not_modified":0,"fails":0,
             "last_status":"20 条 · 新 0","seeded":true},
            {"id":"coindesk","label":"CoinDesk","tier":"media","tier_label":"媒体",
             "newest_ms":1788929000000,"in_window":21,"stale_secs":1000,
             "stale_after_secs":172800,"is_stale":false,"ok":9,"not_modified":3,"fails":0,
             "last_status":"25 条","seeded":true}],
          "items":[]}"#;
        let r = parse(&serde_json::from_str(j).unwrap());
        assert_eq!(r.stale_sources, 1);
        let okx = r.sources.iter().find(|s| s.id == "okx").unwrap();
        assert!(okx.is_stale, "陈了的源必须一路带到面板");
        assert_eq!(okx.in_window, 0);
        // 它一直在成功取数——只看状态码永远发现不了
        assert_eq!((okx.ok, okx.fails), (5, 0));
        assert!(!okx.failing() && !okx.never_seen(), "不是取不到，是取到的东西是陈的");

        let cd = r.sources.iter().find(|s| s.id == "coindesk").unwrap();
        assert!(!cd.is_stale);
    }

    #[test]
    fn never_fetched_is_distinguishable_from_stale() {
        // 刚启动时一条都还没有。报成「陈了」的话每次启动都是一片红
        let j = r#"{"sources":[{"id":"fed","newest_ms":null,"is_stale":false,"in_window":0}]}"#;
        let r = parse(&serde_json::from_str(j).unwrap());
        let fed = &r.sources[0];
        assert!(fed.never_seen());
        assert!(!fed.is_stale);
        assert_eq!(fed.stale_secs, None);
    }

    #[test]
    fn a_guessed_timestamp_and_a_business_time_both_reach_the_panel() {
        // Binance 没有发布时间；OKX 的生效时间和发布时间能差一个月。
        // 两件事都得让人看见
        let j = r#"{"items":[
          {"id":1,"source":"binance","tier":"exchange","title":"上新","url":"https://x",
           "published_ms":null,"effective_ms":null,"sort_ms":1788930000000,"time_guessed":true},
          {"id":2,"source":"okx","tier":"exchange","title":"下架","url":"https://y",
           "published_ms":1788357366699,"effective_ms":1785137700000,"sort_ms":1788357366699,
           "time_guessed":false,"dupes":["coindesk"],"symbols":["BTC"]}]}"#;
        let r = parse(&serde_json::from_str(j).unwrap());
        assert!(r.items[0].time_guessed && r.items[0].published_ms.is_none());
        assert!(!r.items[1].time_guessed);
        assert!(r.items[1].effective_ms.unwrap() < r.items[1].published_ms.unwrap());
        assert_eq!(r.items[1].dupes, vec!["coindesk".to_string()]);
        assert_eq!(r.items[1].symbols, vec!["BTC".to_string()]);
    }

    #[test]
    fn a_missing_or_broken_board_is_an_empty_panel_not_a_crash() {
        // 守护没起、或者快照写到一半被读到
        let r = parse(&serde_json::json!({}));
        assert!(r.sources.is_empty() && r.items.is_empty());
        assert_eq!(r.total, 0);
    }

    fn row(title: &str, source: &str, tier: &str, syms: &[&str], lang: &str) -> NewsRow {
        NewsRow {
            title: title.into(),
            source: source.into(),
            tier: tier.into(),
            lang: lang.into(),
            symbols: syms.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn the_panel_filter_behaves_like_the_daemons_query() {
        let fed = row("Federal Reserve cuts rates", "fed", "regulator", &[], "en");
        let cd = row("Fed rate cut explained", "coindesk", "media", &[], "en");
        let ko = row("인젝티브(INJ) 입출금 중단", "upbit", "exchange", &["INJ"], "ko");

        // 空 = 全部
        assert!(matches("", &fed) && matches("   ", &cd));
        // 多词 = 都要有
        assert!(matches("federal reserve", &fed));
        assert!(!matches("federal reserve", &cd));
        // 词组
        assert!(matches("\"rate cut\"", &cd));
        assert!(!matches("\"rate cut\"", &fed), "两个词都在但不相邻");
        // 排除
        assert!(!matches("fed -coindesk", &cd));
        assert!(matches("reserve -coindesk", &fed));
        // 字段
        assert!(matches("tier:regulator", &fed) && !matches("tier:regulator", &cd));
        assert!(matches("src:upbit sym:inj lang:ko", &ko));
        assert!(!matches("-tier:media", &cd));
        // 无空格语言按子串找
        assert!(matches("입출금", &ko));
    }

    #[test]
    fn a_half_typed_filter_never_hides_everything_by_accident() {
        // 搜索框一边打字一边生效。打到一半（引号没闭合）不该突然全空
        let it = row("Binance Will List FOO", "binance", "exchange", &[], "en");
        assert!(matches("", &it));
        assert!(matches("\"", &it), "只打了个引号，等于还没输入");
        assert!(matches("-", &it), "只打了个减号，等于还没输入");
    }

    #[test]
    fn ages_read_the_way_a_person_says_them() {
        let n = 1_000_000_000i64;
        assert_eq!(ago(n, n), "0秒前");
        assert_eq!(ago(n - 90_000, n), "1分前");
        assert_eq!(ago(n - 7_200_000, n), "2小时前");
        assert_eq!(ago(n - 3 * 86_400_000, n), "3天前");
        // 未来时间（源的时钟比我们快）不该显示成负数
        assert_eq!(ago(n + 5000, n), "0秒前");
    }
}
