//! 连接历史：连过的目标列在会话下面，下次一点就连。
//!
//! # 密钥不进这个文件
//!
//! 这份历史**留在磁盘上**，还会被一起备份走——比 tmpfs 上的请求文件更糟
//! （同 [`super::observatory_lib`] 的理由）。所以存之前过一遍：
//!
//! - `${环境变量名}` **原样留着**。它是引用不是值，留着才能一点就连，
//!   而且下次谁看到都知道要配哪个环境变量（docs/23 §13i）。
//! - 明文密钥**抹掉**，那一条标上「要重填密钥」。点它会把别的字段带回表单，
//!   密钥框留空等你填——比不给这条历史有用，比存下明文安全。
//!
//! # 只记连上过的
//!
//! 用户要的是「连接过的」。点了连接但没连上的目标记进来，列表很快会被
//! 一堆手滑打错的地址塞满。所以是**后台 poller 看到会话真的活了才记**，
//! 不是点击那一刻记。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

pub type Cfg = BTreeMap<String, String>;

/// 历史里的一条。
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Entry {
    pub adapter: String,
    /// 密钥已按上面的规则处理过。
    pub cfg: Cfg,
    /// 最近一次连上的时刻（`MM-DD HH:MM`）。
    pub last: String,
    /// 有字段因为是明文密钥被抹掉了——点它之后要重填。
    pub needs_secret: bool,
}

impl Entry {
    /// 列表上显示的地址。**完整，不省略。**
    ///
    /// 一度是「去掉协议头 + 截到 46 字」。那是错的：
    ///
    /// - 币安那种带一长串 `streams=` 的地址，两条只在结尾不同的历史，
    ///   截断之后**长得一模一样**——列表就没法用了。
    /// - `wss://` 和 `ws://` 是两回事，去掉之后看不出来。
    ///
    /// 太长就让它在界面上换行，不在这里裁。
    pub fn label(&self) -> String {
        for k in ["url", "group", "host"] {
            if let Some(v) = self.cfg.get(k).filter(|v| !v.trim().is_empty()) {
                // 端口另存一个字段的（FIX / TWS / UDP）要拼回去：
                // 同一台机器上两个端口是两回事
                return match self.cfg.get("port") {
                    Some(p) if !p.trim().is_empty() && !v.contains("://") => {
                        format!("{v}:{p}")
                    }
                    _ => v.clone(),
                };
            }
        }
        // 一个能认的字段都没有：至少别显示成空白按钮
        format!("{}（无地址）", self.adapter)
    }

    /// 同一个目标？用来去重。
    ///
    /// 比的是**整份配置**而不只是地址：同一个 URL 配不同的订阅报文
    /// 是两条不同的历史，合并掉的话点回来是错的。
    pub fn same_as(&self, o: &Entry) -> bool {
        self.adapter == o.adapter && self.cfg == o.cfg
    }
}

/// 最多留几条。**要有上限**：不限的话，一个每次换 symbol 的 URL
/// 能在一星期里攒出几百条，那个列表就没法用了。
pub const MAX: usize = 12;

static PATH_OVERRIDE: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn hist_path() -> PathBuf {
    if let Some(p) = PATH_OVERRIDE.lock().ok().and_then(|g| g.clone()) {
        return p;
    }
    super::observatory_lib::lib_path().with_file_name("observatory_history.json")
}

/// 把一份配置整理成可以落盘的样子。
///
/// 返回 `(处理过的配置, 是否抹掉了明文密钥)`。
pub fn sanitize(cfg: &Cfg) -> (Cfg, bool) {
    let mut out = Cfg::new();
    let mut stripped = false;
    for (k, v) in cfg {
        // 守护抹过的占位符没有留下来的意义——它既不是值也不是引用
        if v.contains("已抹去") {
            stripped = true;
            continue;
        }
        if super::observatory_lib::looks_like_a_secret(v)
            || (is_secret_key(k) && !v.trim().is_empty() && !is_env_ref(v))
        {
            stripped = true;
            continue;
        }
        out.insert(k.clone(), v.clone());
    }
    (out, stripped)
}

/// 键名像不像密钥。和守护那边 `check::looks_secret` 同一套词。
fn is_secret_key(k: &str) -> bool {
    let k = k.to_lowercase();
    ["key", "secret", "token", "pass", "sign", "auth", "credential"]
        .iter()
        .any(|w| k.contains(w))
}

/// 整个值就是一个 `${环境变量名}`。
fn is_env_ref(v: &str) -> bool {
    let t = v.trim();
    t.len() > 3 && t.starts_with("${") && t.ends_with('}') && !t[2..t.len() - 1].contains(['{', '}'])
}

// ── 落盘 ──────────────────────────────────────────────────────

fn to_json(v: &[Entry]) -> serde_json::Value {
    serde_json::json!(
        v.iter()
            .map(|e| serde_json::json!({
                "adapter": e.adapter,
                "cfg": e.cfg,
                "last": e.last,
                "needs_secret": e.needs_secret,
            }))
            .collect::<Vec<_>>()
    )
}

pub fn from_json(v: &serde_json::Value) -> Vec<Entry> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|e| {
                    let adapter = e.get("adapter")?.as_str()?.to_string();
                    // 没有 adapter 的条目点了也连不上，等于死数据
                    if adapter.trim().is_empty() {
                        return None;
                    }
                    Some(Entry {
                        adapter,
                        cfg: e
                            .get("cfg")
                            .and_then(|c| c.as_object())
                            .map(|o| {
                                o.iter()
                                    .filter_map(|(k, x)| {
                                        Some((k.clone(), x.as_str()?.to_string()))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                        last: e.get("last").and_then(|x| x.as_str()).unwrap_or("").into(),
                        needs_secret: e
                            .get("needs_secret")
                            .and_then(|x| x.as_bool())
                            .unwrap_or(false),
                    })
                })
                .take(MAX)
                .collect()
        })
        .unwrap_or_default()
}

static HIST: Mutex<Option<Vec<Entry>>> = Mutex::new(None);

pub fn all() -> Vec<Entry> {
    let Ok(mut g) = HIST.lock() else { return Vec::new() };
    if g.is_none() {
        *g = Some(
            std::fs::read_to_string(hist_path())
                .ok()
                .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                .map(|v| from_json(&v))
                .unwrap_or_default(),
        );
    }
    g.clone().unwrap_or_default()
}

fn store(v: Vec<Entry>) {
    let p = hist_path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    // 原子写：半截 JSON 会让整份历史读不出来
    let tmp = p.with_extension("json.tmp");
    if let Ok(t) = serde_json::to_string_pretty(&to_json(&v))
        && std::fs::write(&tmp, t).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    if let Ok(mut g) = HIST.lock() {
        *g = Some(v);
    }
}

/// 记一次**连上了**的目标。已有的同目标只更新时间并挪到最前。
///
/// 返回是否真的改动了——poller 每秒都会调它，没变化就别写盘。
pub fn record(adapter: &str, cfg: &Cfg, now: &str) -> bool {
    if adapter.trim().is_empty() {
        return false;
    }
    let (clean, stripped) = sanitize(cfg);
    let e = Entry {
        adapter: adapter.to_string(),
        cfg: clean,
        last: now.to_string(),
        needs_secret: stripped,
    };
    let mut v = all();
    if let Some(i) = v.iter().position(|x| x.same_as(&e)) {
        // 同一分钟内重复调用不写盘：poller 一秒一轮，不然就是每秒一次落盘
        if v[i].last == e.last && i == 0 {
            return false;
        }
        v.remove(i);
    }
    v.insert(0, e);
    v.truncate(MAX);
    store(v);
    true
}

pub fn remove(adapter: &str, cfg: &Cfg) {
    let mut v = all();
    let before = v.len();
    v.retain(|x| !(x.adapter == adapter && &x.cfg == cfg));
    if v.len() != before {
        store(v);
    }
}

pub fn clear() {
    store(Vec::new());
}

/// 碰历史的测试都要先拿这把锁。
///
/// 历史是进程级的（一个静态 + 一个路径改写），并行跑必然互踩——而且踩起来
/// 是「另一个测试的条目出现在我的断言里」，看着像逻辑错。
/// 这是本轮第四次栽在同一个形状上（IPO 月份、连接表单、请求文件，现在是这个），
/// 前三次都是把测试合成一条；跨模块合不了，所以给一把共用的锁。
///
/// `Mutex` 中毒了也要继续——一个失败的测试不该把其余的全变成 panic。
#[cfg(test)]
pub static TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
pub fn lock_for_test() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
pub fn reset_for_test(path: Option<&std::path::Path>) {
    if let Ok(mut g) = PATH_OVERRIDE.lock() {
        *g = path.map(|p| p.to_path_buf());
    }
    if let Ok(mut g) = HIST.lock() {
        *g = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(v: &[(&str, &str)]) -> Cfg {
        v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn a_plaintext_secret_never_reaches_the_history_file_but_a_reference_does() {
        // 这份历史留在磁盘上、会被一起备份走。引用不是值，留着才有用
        let (c, stripped) = sanitize(&cfg(&[
            ("url", "wss://x"),
            ("api_key", "AKIAIOSFODNN7EXAMPLE"),
        ]));
        assert!(stripped, "明文密钥要抹掉，并且这条要标上「要重填密钥」");
        assert!(!c.contains_key("api_key"));
        assert_eq!(c["url"], "wss://x");

        let (c, stripped) = sanitize(&cfg(&[("url", "wss://x"), ("api_key", "${BINANCE_KEY}")]));
        assert!(!stripped, "引用可以原样留着");
        assert_eq!(c["api_key"], "${BINANCE_KEY}");

        // 守护抹过的占位符既不是值也不是引用，留着没意义
        let (c, stripped) = sanitize(&cfg(&[("api_key", "«已抹去»")]));
        assert!(stripped && !c.contains_key("api_key"));

        // 密钥嵌在普通字段里的也要认出来（现装 Adapter 一个 Secret 字段都没有）
        let (c, stripped) =
            sanitize(&cfg(&[("headers", "Authorization: Bearer AKIAIOSFODNN7EXAMPLE")]));
        assert!(stripped && !c.contains_key("headers"));
    }

    #[test]
    fn the_address_is_shown_whole_because_two_of_them_can_differ_only_at_the_end() {
        // 截断过一版。币安那种带一长串 streams= 的地址，两条只在结尾不同的
        // 历史截完**长得一模一样**，列表就没法用了
        let long = "wss://stream.binance.com:9443/stream?streams=btcusdt@aggTrade/ethusdt@aggTrade/solusdt@aggTrade";
        let e = Entry { cfg: cfg(&[("url", long)]), ..Default::default() };
        assert_eq!(e.label(), long, "一个字都不能少");
        // 协议头也留着：wss:// 和 ws:// 是两回事
        assert!(e.label().starts_with("wss://"));

        let a = Entry { cfg: cfg(&[("url", "wss://x/a?s=btc")]), ..Default::default() };
        let b = Entry { cfg: cfg(&[("url", "wss://x/a?s=eth")]), ..Default::default() };
        assert_ne!(a.label(), b.label(), "只在结尾不同的两条要能分出来");

        // 端口另存一个字段的（FIX / TWS / UDP）要拼回去
        let e = Entry { cfg: cfg(&[("host", "127.0.0.1"), ("port", "7497")]), ..Default::default() };
        assert_eq!(e.label(), "127.0.0.1:7497");
        // URL 里已经带端口的别再拼一次
        let e = Entry { cfg: cfg(&[("url", "wss://x:9443/a"), ("port", "7497")]), ..Default::default() };
        assert_eq!(e.label(), "wss://x:9443/a");

        // 一个能认的字段都没有时，别给一个空白按钮
        let e = Entry { adapter: "x.y".into(), ..Default::default() };
        assert!(e.label().contains("x.y"));
    }

    #[test]
    fn two_targets_differing_only_in_a_non_address_field_are_not_merged() {
        // 同一个 URL 配不同的订阅报文是两条不同的历史。
        // 只按地址去重的话，点回来的是另一条
        let a = Entry { adapter: "ws.raw".into(), cfg: cfg(&[("url", "wss://x"), ("subscribe", "A")]), ..Default::default() };
        let b = Entry { adapter: "ws.raw".into(), cfg: cfg(&[("url", "wss://x"), ("subscribe", "B")]), ..Default::default() };
        assert!(!a.same_as(&b));
        assert!(a.same_as(&a.clone()));
        // 换了 Adapter 也是两条
        let c = Entry { adapter: "rest.poll".into(), ..a.clone() };
        assert!(!a.same_as(&c));
    }

    /// 历史是**进程级**的（静态 + 一个文件），并行跑必然互踩。合成一条顺序走。
    #[test]
    fn the_history_round_trips_and_stays_bounded_and_most_recent_first() {
        let _g = lock_for_test();
        let dir = std::env::temp_dir().join(format!("ws-obs-hist-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("h.json");
        let _ = std::fs::remove_file(&p);
        reset_for_test(Some(&p));

        assert!(all().is_empty(), "文件不存在时是空历史，不是报错");

        assert!(record("ws.raw", &cfg(&[("url", "wss://a")]), "09-09 10:00"));
        assert!(record("ws.raw", &cfg(&[("url", "wss://b")]), "09-09 10:01"));
        // 最近的在最前：列表要能一眼看到刚才连的那个
        assert_eq!(all()[0].cfg["url"], "wss://b");

        // 同一个目标再连一次：不新增，挪到最前并更新时间
        assert!(record("ws.raw", &cfg(&[("url", "wss://a")]), "09-09 10:05"));
        assert_eq!(all().len(), 2, "同目标不该攒出第二条");
        assert_eq!(all()[0].cfg["url"], "wss://a");
        assert_eq!(all()[0].last, "09-09 10:05");

        // poller 一秒一轮，同一条重复记不该反复写盘
        assert!(!record("ws.raw", &cfg(&[("url", "wss://a")]), "09-09 10:05"));

        // 上限：不限的话，一个每次换 symbol 的 URL 一周能攒几百条
        for i in 0..30 {
            record("ws.raw", &cfg(&[("url", &format!("wss://x{i}"))]), "09-09 11:00");
        }
        assert_eq!(all().len(), MAX);

        // 从磁盘重读，验证真的落盘了
        reset_for_test(Some(&p));
        assert_eq!(all().len(), MAX);
        assert_eq!(all()[0].cfg["url"], "wss://x29");

        // 坏文件当空历史，不能让面板起不来
        let _ = std::fs::write(&p, "{ 这不是 json");
        reset_for_test(Some(&p));
        assert!(all().is_empty());

        record("ws.raw", &cfg(&[("url", "wss://z")]), "09-09 12:00");
        remove("ws.raw", &cfg(&[("url", "wss://z")]));
        assert!(all().is_empty());

        reset_for_test(None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
