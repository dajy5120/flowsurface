//! API 请求库（docs/23 §9 Mode 1：「请求库：保存/命名/参数化」）。
//!
//! # 为什么要落盘，以及落在哪
//!
//! 本项目的约定是**快照不写硬盘**——那说的是数据。请求库是**用户配置**：
//! 十几条几百字节的字符串，而且不存下来这个功能就不存在（关掉面板就没了）。
//! 所以放 `~/.config/wealthspring/observatory_requests.json`，
//! 不放录制目录、不放 tmpfs。
//!
//! # 两种占位符，故意长得不一样
//!
//! - `${环境变量名}` —— **守护**在建连那一刻从环境里取。给密钥用（§13i）。
//!   保存进库里的就是这五个字符，真值从来不进这个文件。
//! - `{{参数名}}` —— **面板**在发送那一刻从参数框里填。给「同一个请求换个
//!   交易对再发一次」用。
//!
//! 共用一种语法的话，「这个值是谁负责填的」就说不清了，而这正是
//! 「我的密钥怎么跑到磁盘上去了」的经典成因。
//!
//! # 库文件里出现明文密钥比请求文件更糟
//!
//! 请求文件在 tmpfs 上，重启就没了。这个文件**留在磁盘上**，还会被
//! 一起备份走。所以保存时同样查一遍，查出来就说，不静默收下。

use std::path::PathBuf;
use std::sync::Mutex;

/// 库里的一条。
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Saved {
    pub name: String,
    /// 存下来是为了**说清这条是给谁写的**——不是用来禁止跨 Adapter 用。
    /// 同一段 JSON 在两个交易所的 WS 上都能发，禁掉反而碍事。
    pub adapter: String,
    pub body: String,
    /// `{{名字}}` → 默认值。
    pub params: Vec<(String, String)>,
}

/// 测试用的路径改写。生产路径永远走下面那条。
static PATH_OVERRIDE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// 库文件路径。
pub fn lib_path() -> PathBuf {
    if let Some(p) = PATH_OVERRIDE.lock().ok().and_then(|g| g.clone()) {
        return p;
    }
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".config")
        });
    base.join("wealthspring").join("observatory_requests.json")
}

/// 找出 `body` 里所有 `{{名字}}`，按出现顺序、去重。
pub fn placeholders(body: &str) -> Vec<String> {
    let b = body.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i + 4 <= b.len() {
        if b[i] == b'{' && b[i + 1] == b'{' {
            if let Some(rel) = body[i + 2..].find("}}") {
                let name = body[i + 2..i + 2 + rel].trim();
                // 空的、带花括号的都不是干净的占位符，不猜
                if !name.is_empty() && !name.contains(['{', '}']) {
                    if !out.iter().any(|x| x == name) {
                        out.push(name.to_string());
                    }
                    i += rel + 4;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

/// 填参数的结果。
pub struct Filled {
    pub body: String,
    /// `body` 里有、但参数表里没有的名字。**不能静默留着 `{{x}}` 就发出去**——
    /// 对端会收到一段字面量，然后回一条看不懂的错误。
    pub unfilled: Vec<String>,
}

/// 把 `{{名字}}` 换成参数值。
pub fn fill(body: &str, params: &[(String, String)]) -> Filled {
    let mut out = body.to_string();
    let mut unfilled = Vec::new();
    for name in placeholders(body) {
        match params.iter().find(|(k, _)| *k == name) {
            Some((_, v)) => out = out.replace(&format!("{{{{{name}}}}}"), v),
            None => unfilled.push(name),
        }
    }
    Filled { body: out, unfilled }
}

/// 这段请求体里像不像有明文密钥。
///
/// 和守护那边 `check::looks_secret` 是同一套判断，但这里**只能看值**——
/// 请求体没有键名。所以看两样：有没有 `authorization`/`api-key` 这类词，
/// 以及抠掉 `${引用}` 之后还剩不剩一段长得像令牌的东西。
pub fn looks_like_a_secret(body: &str) -> bool {
    let v = body.to_lowercase();
    let marked = ["authorization", "bearer ", "api-key", "apikey", "api_key", "secret", "signature", "password", "token"]
        .iter()
        .any(|w| v.contains(w));
    if !marked {
        return false;
    }
    // `${VAR}` 是引用不是值——那正是我们希望用户写的形式，不该反过来警告
    let stripped = strip_env_refs(body);
    longest_opaque_run(&stripped) >= 16
}

fn strip_env_refs(v: &str) -> String {
    let mut out = String::new();
    let b = v.as_bytes();
    let mut i = 0;
    let mut last = 0;
    while i + 2 < b.len() {
        if b[i] == b'$' && b[i + 1] == b'{' {
            if let Some(rel) = v[i + 2..].find('}') {
                let name = &v[i + 2..i + 2 + rel];
                if !name.is_empty() && !name.contains(['{', '$']) {
                    out.push_str(&v[last..i]);
                    i += rel + 3;
                    last = i;
                    continue;
                }
            }
        }
        i += 1;
    }
    out.push_str(&v[last..]);
    out
}

fn longest_opaque_run(s: &str) -> usize {
    let (mut best, mut cur) = (0, 0);
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || "-_+/=".contains(c) {
            cur += 1;
            best = best.max(cur);
        } else {
            cur = 0;
        }
    }
    best
}

// ── 落盘 ──────────────────────────────────────────────────────────

fn to_json(v: &[Saved]) -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "entries": v.iter().map(|e| serde_json::json!({
            "name": e.name,
            "adapter": e.adapter,
            "body": e.body,
            "params": e.params.iter().map(|(k, x)|
                serde_json::json!({"name": k, "value": x})).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

pub fn from_json(v: &serde_json::Value) -> Vec<Saved> {
    v.get("entries")
        .and_then(|e| e.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|e| {
                    let name = e.get("name")?.as_str()?.to_string();
                    // 没名字的条目在界面上点不出来，等于死数据
                    if name.trim().is_empty() {
                        return None;
                    }
                    Some(Saved {
                        name,
                        adapter: e.get("adapter").and_then(|x| x.as_str()).unwrap_or("").into(),
                        body: e.get("body").and_then(|x| x.as_str()).unwrap_or("").into(),
                        params: e
                            .get("params")
                            .and_then(|p| p.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|p| {
                                        Some((
                                            p.get("name")?.as_str()?.to_string(),
                                            p.get("value")
                                                .and_then(|x| x.as_str())
                                                .unwrap_or("")
                                                .to_string(),
                                        ))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

static LIB: Mutex<Option<Vec<Saved>>> = Mutex::new(None);

/// 读库。第一次从磁盘读，之后走内存。
pub fn all() -> Vec<Saved> {
    let Ok(mut g) = LIB.lock() else { return Vec::new() };
    if g.is_none() {
        let v = std::fs::read_to_string(lib_path())
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .map(|v| from_json(&v))
            .unwrap_or_default();
        *g = Some(v);
    }
    g.clone().unwrap_or_default()
}

fn store(v: Vec<Saved>) {
    let p = lib_path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    // 原子写：同目录 tmp + rename。半截的 JSON 会让整个库读不出来，
    // 而这个文件是用户自己攒起来的，丢了没处找
    let tmp = p.with_extension("json.tmp");
    if let Ok(t) = serde_json::to_string_pretty(&to_json(&v)) {
        if std::fs::write(&tmp, t.as_bytes()).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
    if let Ok(mut g) = LIB.lock() {
        *g = Some(v);
    }
}

/// 存一条。同名覆盖——「另存为」在这个场景下只会攒出一堆 `xxx 2`。
///
/// 返回是否覆盖了已有的一条，界面据此说「已更新」还是「已保存」。
pub fn save(e: Saved) -> bool {
    let mut v = all();
    match v.iter().position(|x| x.name == e.name) {
        Some(i) => {
            v[i] = e;
            store(v);
            true
        }
        None => {
            v.push(e);
            store(v);
            false
        }
    }
}

pub fn remove(name: &str) {
    let mut v = all();
    v.retain(|x| x.name != name);
    store(v);
}

pub fn get(name: &str) -> Option<Saved> {
    all().into_iter().find(|x| x.name == name)
}

#[cfg(test)]
pub fn reset_for_test(path: Option<&std::path::Path>) {
    if let Ok(mut g) = PATH_OVERRIDE.lock() {
        *g = path.map(|p| p.to_path_buf());
    }
    if let Ok(mut g) = LIB.lock() {
        *g = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_are_found_in_order_deduped_and_nothing_is_guessed() {
        assert_eq!(placeholders("{{sym}}@depth {{sym}} {{lvl}}"), vec!["sym", "lvl"]);
        assert_eq!(placeholders("没有占位符"), Vec::<String>::new());
        // 不猜：没闭合、空的、带花括号的都当普通文本
        for b in ["{{unclosed", "{{}}", "{{a{b}}", "{ {x} }"] {
            assert!(placeholders(b).is_empty(), "{b} 不该被当成占位符");
        }
        // `${ENV}` 是**守护**填的，不归这儿管——两种占位符必须分得开
        assert!(placeholders("Bearer ${API_KEY}").is_empty());
    }

    #[test]
    fn a_placeholder_with_no_value_is_reported_not_sent_as_a_literal() {
        // 静默留着 {{x}} 发出去的话，对端收到一段字面量，
        // 然后回一条完全看不懂的错误
        let p = vec![("sym".to_string(), "btcusdt".to_string())];
        let f = fill("{{sym}}@depth", &p);
        assert_eq!(f.body, "btcusdt@depth");
        assert!(f.unfilled.is_empty());

        let f = fill("{{sym}}@{{lvl}}", &p);
        assert_eq!(f.unfilled, vec!["lvl".to_string()]);
        assert!(f.body.contains("{{lvl}}"), "填不了的原样留着，交给调用方拦");
    }

    #[test]
    fn a_plaintext_secret_in_the_body_is_flagged_but_an_env_reference_is_not() {
        // 这个文件留在磁盘上、会被一起备份走，比 tmpfs 上的请求文件更糟
        assert!(looks_like_a_secret(
            r#"{"Authorization":"Bearer AKIAIOSFODNN7EXAMPLE"}"#
        ));
        // 引用正是我们希望用户写的形式，不该反过来警告
        assert!(!looks_like_a_secret(r#"{"Authorization":"Bearer ${BINANCE_KEY}"}"#));
        // 普通订阅报文不该误报
        assert!(!looks_like_a_secret(r#"{"method":"SUBSCRIBE","params":["btcusdt@depth"]}"#));
    }

    /// 库是**进程级**的（`LIB` 静态 + 一个文件路径），几个测试并行跑必然互踩。
    /// 合成一条按顺序走——这是第四次栽在同一个坑上了。
    #[test]
    fn the_library_round_trips_through_disk_and_same_name_overwrites() {
        let dir = std::env::temp_dir().join(format!("ws-obs-lib-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("lib.json");
        let _ = std::fs::remove_file(&p);
        reset_for_test(Some(&p));

        assert!(all().is_empty(), "文件不存在时是空库，不是报错");

        let e = Saved {
            name: "币安深度".into(),
            adapter: "ws.raw".into(),
            body: r#"{"method":"SUBSCRIBE","params":["{{sym}}@depth"],"id":1}"#.into(),
            params: vec![("sym".into(), "btcusdt".into())],
        };
        assert!(!save(e.clone()), "第一次是新增");
        assert!(save(Saved { body: "改过了".into(), ..e.clone() }), "同名是覆盖");
        assert_eq!(all().len(), 1, "同名不该攒出第二条");

        // 从磁盘重读：内存清掉，验证真的落盘了
        reset_for_test(Some(&p));
        let v = all();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name, "币安深度");
        assert_eq!(v[0].body, "改过了");
        assert_eq!(v[0].params, vec![("sym".to_string(), "btcusdt".to_string())]);

        // 没名字的条目点不出来，等于死数据——读的时候就丢掉
        let _ = std::fs::write(&p, r#"{"version":1,"entries":[{"name":"  ","body":"x"},{"name":"好的","body":"y"}]}"#);
        reset_for_test(Some(&p));
        assert_eq!(all().len(), 1);
        assert_eq!(all()[0].name, "好的");

        // 坏文件当空库，不能让面板起不来
        let _ = std::fs::write(&p, "{ 这不是 json");
        reset_for_test(Some(&p));
        assert!(all().is_empty());

        remove("好的");
        reset_for_test(Some(&p));
        assert!(all().is_empty());

        reset_for_test(None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
