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
    pub lang: String,
    /// 见过的最新一条（毫秒）。**不受时间窗影响**——库里出窗了它还在。
    pub newest_ms: Option<i64>,
    /// 窗内还剩几条。和 `newest_ms` 是两件事。
    pub in_window: i64,
    pub stale_secs: Option<i64>,
    pub stale_after_secs: i64,
    /// **这一行是这一页的理由**：HTTP 200 也可能是死的。
    pub is_stale: bool,
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

#[derive(Default, Clone)]
pub struct NewsReadout {
    pub present: bool,
    pub stamp: String,
    pub now_ms: i64,
    pub total: i64,
    pub stale_sources: i64,
    pub sources: Vec<SourceRow>,
    pub items: Vec<NewsRow>,
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
                        lang: s(r, "lang"),
                        newest_ms: oi(r, "newest_ms"),
                        in_window: i(r, "in_window"),
                        stale_secs: oi(r, "stale_secs"),
                        stale_after_secs: i(r, "stale_after_secs"),
                        is_stale: b(r, "is_stale"),
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
        refreshed: String::new(),
        svc: Default::default(),
    }
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
