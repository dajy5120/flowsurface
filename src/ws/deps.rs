//! **依赖版本与更新**（用户要求）：两个核心开源依赖 + 其余实际用到的依赖，
//! 显示当前版本、上游最新版本，并提供手动更新。
//!
//! # 三条设计约束
//!
//! **① 不做后台轮询。** 用户对这个项目的一贯要求是「不要一直消耗网络流量」。
//! 一个每小时问一遍 GitHub/PyPI 的后台线程，正是那种「你没在看的时候它也在跑」。
//! 所以：当前版本随时可见（**全部来自本地，零网络**），最新版本**只在点
//! 「检查更新」时去问**。
//!
//! **② 用子进程 `curl`，不给面板加 HTTP 客户端。** 这个 fork 有条明确的分工：
//! 面板不碰网络，连接全在守护里（`src/ws/` 下原本 HTTP 调用数 = 0）。
//! 版本检查是面板发起的一次性动作，照 `procs.rs` 调 `systemctl` 的路数走子进程，
//! 既不破坏那条分工，也不引入 `reqwest` 到主 crate。
//!
//! **③ 更新动作按依赖类型区分，绝不「一键」到底。**
//! `pip install -U` 可以真跑；而 FlowSurface 是**带着六十多个自有模块的 fork**，
//! 在一个正在运行的 GUI 里替你 merge 上游、然后你脚下的二进制被重编——
//! 那不是便利，是事故。所以 fork 那一档只做 `git fetch`（只读，不动工作区）
//! 并告诉你落后多少、该敲什么命令。

use std::collections::HashMap;
use std::process::Command;
use std::sync::{Mutex, OnceLock};

/// 上游版本从哪问。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Src {
    PyPi,
    CratesIo,
    /// fork 的上游仓库（`owner/repo`）。
    GitHub(&'static str),
}

/// 更新怎么做。**区分开是因为代价不同**，不是为了好看。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Upd {
    /// `pip install -U`：能真跑。跑完用到它的守护要重启才生效。
    Pip,
    /// `cargo update -p`：改 Cargo.lock，要重新编译才生效。
    Cargo,
    /// 只 `git fetch`，**不 merge**。见模块文档 ③。
    FetchOnly,
}

pub struct Dep {
    pub key: &'static str,
    pub label: &'static str,
    /// 这东西在本项目里干什么。不写清楚的话，一列版本号没法帮人判断该不该升。
    pub what: &'static str,
    /// 包名（PyPI / crates.io 上的名字）。
    pub name: &'static str,
    pub src: Src,
    pub upd: Upd,
    /// 核心依赖排在前面并标出来。
    pub core: bool,
}

pub static ALL: &[Dep] = &[
    // ── 两个核心 ──
    Dep {
        key: "nautilus",
        label: "NautilusTrader",
        what: "P1 核心：回测/实盘引擎、数据模型、MessageBus（通道①）",
        name: "nautilus_trader",
        src: Src::PyPi,
        upd: Upd::Pip,
        core: true,
    },
    Dep {
        key: "flowsurface",
        label: "FlowSurface",
        what: "P2 Cockpit 的基座。本仓库是 fork，自有模块六十余个",
        name: "flowsurface-rs/flowsurface",
        src: Src::GitHub("flowsurface-rs/flowsurface"),
        upd: Upd::FetchOnly,
        core: true,
    },
    // ── Python（ws-venv）──
    Dep { key: "hftbacktest", label: "hftbacktest", what: "高频回测（排队仿真、做市影子）", name: "hftbacktest", src: Src::PyPi, upd: Upd::Pip, core: false },
    Dep { key: "duckdb", label: "DuckDB", what: "历史面板的查询引擎（parquet 直查）", name: "duckdb", src: Src::PyPi, upd: Upd::Pip, core: false },
    Dep { key: "scipy", label: "SciPy", what: "因子统计检验", name: "scipy", src: Src::PyPi, upd: Upd::Pip, core: false },
    Dep { key: "py-redis", label: "redis (py)", what: "Python 侧通道① 客户端", name: "redis", src: Src::PyPi, upd: Upd::Pip, core: false },
    Dep { key: "websockets", label: "websockets", what: "两个 Binance feed 的 WS 客户端", name: "websockets", src: Src::PyPi, upd: Upd::Pip, core: false },
    Dep { key: "anthropic", label: "anthropic", what: "预测市场夜跑的 AI 决策支持", name: "anthropic", src: Src::PyPi, upd: Upd::Pip, core: false },
    Dep { key: "pytest", label: "pytest", what: "Python 侧测试", name: "pytest", src: Src::PyPi, upd: Upd::Pip, core: false },
    // ── Rust ──
    Dep { key: "iced", label: "iced", what: "Cockpit 的 GUI 框架（经 FlowSurface）", name: "iced", src: Src::CratesIo, upd: Upd::Cargo, core: false },
    Dep { key: "gpui", label: "gpui", what: "P3 Studio 的 GUI 框架（Zed 的）", name: "gpui", src: Src::CratesIo, upd: Upd::Cargo, core: false },
    Dep { key: "tokio", label: "tokio", what: "异步运行时", name: "tokio", src: Src::CratesIo, upd: Upd::Cargo, core: false },
    Dep { key: "rs-redis", label: "redis (rs)", what: "Rust 侧通道① 客户端", name: "redis", src: Src::CratesIo, upd: Upd::Cargo, core: false },
    Dep { key: "parquet", label: "parquet", what: "录制落盘格式", name: "parquet", src: Src::CratesIo, upd: Upd::Cargo, core: false },
    Dep { key: "pyo3", label: "PyO3", what: "Rust↔Python 桥（ShmemSink）", name: "pyo3", src: Src::CratesIo, upd: Upd::Cargo, core: false },
    Dep { key: "ureq", label: "ureq", what: "守护侧 HTTP（雷达/新闻/观察终端）", name: "ureq", src: Src::CratesIo, upd: Upd::Cargo, core: false },
    Dep { key: "quick-xml", label: "quick-xml", what: "新闻 RSS/Atom 解析", name: "quick-xml", src: Src::CratesIo, upd: Upd::Cargo, core: false },
    Dep { key: "shmem-ipc", label: "shmem-ipc", what: "通道②（零拷贝 SPSC）", name: "shmem-ipc", src: Src::CratesIo, upd: Upd::Cargo, core: false },
];

#[derive(Clone, Default)]
pub struct Row {
    pub key: String,
    pub label: String,
    pub what: String,
    pub core: bool,
    /// 本地装着的版本。空 = 没读到。
    pub current: String,
    /// 上游最新。空 = **还没查过**，不是「没有更新」。这两者必须在界面上分得开。
    pub latest: String,
    /// 落后情况的人话描述（fork 用「落后 N 个提交」，包用版本号比较）。
    pub note: String,
}

impl Row {
    /// 有没有新版本可用。**没查过时恒为 false**——「不知道」不能显示成「已最新」。
    pub fn outdated(&self) -> bool {
        !self.latest.is_empty() && !self.current.is_empty() && self.latest != self.current
    }
    pub fn checked(&self) -> bool {
        !self.latest.is_empty()
    }
}

static ROWS: OnceLock<Mutex<Vec<Row>>> = OnceLock::new();
static NOTE: OnceLock<Mutex<String>> = OnceLock::new();

fn rows_cell() -> &'static Mutex<Vec<Row>> {
    ROWS.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn note() -> String {
    NOTE.get_or_init(|| Mutex::new(String::new())).lock().map(|g| g.clone()).unwrap_or_default()
}

pub fn set_note(t: &str) {
    if let Ok(mut g) = NOTE.get_or_init(|| Mutex::new(String::new())).lock() {
        *g = t.to_string();
    }
}

// ───────────────────────── 本地版本（零网络） ─────────────────────────

/// venv 里装了什么。一次 `pip list --format=json`，不是一个包一个子进程。
fn pip_versions() -> HashMap<String, String> {
    let py = super::paths::python();
    let pip = py.parent().map(|d| d.join("pip")).unwrap_or_default();
    let out = Command::new(&pip).args(["list", "--format=json"]).output();
    let Ok(o) = out else { return HashMap::new() };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&o.stdout) else {
        return HashMap::new();
    };
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|e| {
                    Some((
                        // pip 输出的名字大小写/分隔符不稳定（`nautilus_trader` vs
                        // `nautilus-trader`），统一成小写连字符再比
                        norm(e.get("name")?.as_str()?),
                        e.get("version")?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 包名归一：小写 + 下划线转连字符。PyPI 与 pip 对同一个包的写法不一致。
fn norm(s: &str) -> String {
    s.to_lowercase().replace('_', "-")
}

/// 从 `Cargo.lock` 读已解析的版本。**不跑 cargo**——`cargo tree` 会触发网络与解析，
/// 而 lock 文件里就是确切答案。
fn lock_versions(paths: &[&str]) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for p in paths {
        let Ok(s) = std::fs::read_to_string(p) else { continue };
        let mut name: Option<String> = None;
        for line in s.lines() {
            if let Some(v) = line.strip_prefix("name = ") {
                name = Some(v.trim_matches('"').to_string());
            } else if let Some(v) = line.strip_prefix("version = ") {
                if let Some(n) = name.take() {
                    // 同名多版本时保留**先出现的**：两个 lock 文件里
                    // flowsurface 那份更贴近面板实际跑的东西
                    m.entry(norm(&n)).or_insert_with(|| v.trim_matches('"').to_string());
                }
            }
        }
    }
    m
}

/// fork 当前的提交（短 sha + 日期）。
fn fork_head() -> String {
    let repo = super::paths::repo_root().join("vendor/flowsurface");
    let out = Command::new("git")
        .args(["-C", &repo.to_string_lossy(), "log", "-1", "--format=%h (%cd)", "--date=short"])
        .output();
    out.ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// 读一遍本地版本。**零网络**，随时可调。
pub fn refresh_local() {
    let pips = pip_versions();
    let root = super::paths::repo_root();
    let locks = [
        root.join("vendor/flowsurface/Cargo.lock").to_string_lossy().into_owned(),
        root.join("Cargo.lock").to_string_lossy().into_owned(),
        root.join("crates/wealthspring-studio/Cargo.lock").to_string_lossy().into_owned(),
    ];
    let lockrefs: Vec<&str> = locks.iter().map(String::as_str).collect();
    let crates = lock_versions(&lockrefs);

    let prev: HashMap<String, Row> =
        rows_cell().lock().map(|g| g.iter().map(|r| (r.key.clone(), r.clone())).collect()).unwrap_or_default();

    let rows: Vec<Row> = ALL
        .iter()
        .map(|d| {
            let current = match d.src {
                Src::PyPi => pips.get(&norm(d.name)).cloned().unwrap_or_default(),
                Src::CratesIo => crates.get(&norm(d.name)).cloned().unwrap_or_default(),
                Src::GitHub(_) => fork_head(),
            };
            let old = prev.get(d.key);
            Row {
                key: d.key.into(),
                label: d.label.into(),
                what: d.what.into(),
                core: d.core,
                current,
                // 已经查到的上游版本保留——重读本地不该把它擦回「未查」
                latest: old.map(|r| r.latest.clone()).unwrap_or_default(),
                note: old.map(|r| r.note.clone()).unwrap_or_default(),
            }
        })
        .collect();
    if let Ok(mut g) = rows_cell().lock() {
        *g = rows;
    }
}

pub fn rows() -> Vec<Row> {
    if rows_cell().lock().map(|g| g.is_empty()).unwrap_or(true) {
        refresh_local();
    }
    rows_cell().lock().map(|g| g.clone()).unwrap_or_default()
}

// ───────────────────────── 上游版本（要网络，只在点按钮时） ─────────────────────────

/// 一次 HTTP GET。子进程 `curl`——见模块文档 ②。
fn get(url: &str) -> Option<String> {
    let o = Command::new("curl")
        .args(["-sS", "--max-time", "20", "-H", "User-Agent: wealthspring-deps", url])
        .output()
        .ok()?;
    if !o.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&o.stdout).into_owned())
}

fn json_str(body: &str, path: &[&str]) -> Option<String> {
    let mut v: serde_json::Value = serde_json::from_str(body).ok()?;
    for k in path {
        v = v.get(k)?.clone();
    }
    v.as_str().map(str::to_string)
}

/// 查一个依赖的上游版本。**这是本模块唯一发起外部连接的地方。**
fn latest_of(d: &Dep) -> (String, String) {
    match d.src {
        Src::PyPi => {
            let body = get(&format!("https://pypi.org/pypi/{}/json", d.name));
            let v = body.as_deref().and_then(|b| json_str(b, &["info", "version"])).unwrap_or_default();
            (v, String::new())
        }
        Src::CratesIo => {
            let body = get(&format!("https://crates.io/api/v1/crates/{}", d.name));
            // max_stable_version：不把预发布版本当成「最新」，那会让整列常年显示可升级
            let v = body
                .as_deref()
                .and_then(|b| json_str(b, &["crate", "max_stable_version"]))
                .unwrap_or_default();
            (v, String::new())
        }
        Src::GitHub(repo) => {
            let body = get(&format!("https://api.github.com/repos/{repo}/releases/latest"));
            let tag = body.as_deref().and_then(|b| json_str(b, &["tag_name"])).unwrap_or_default();
            // fork 落后多少个提交：`git fetch` 上游后数 rev-list。
            // **只 fetch，不动工作区**——见模块文档 ③。
            let note = fork_behind(repo);
            (tag, note)
        }
    }
}

/// fork 落后上游多少提交。会 `git fetch`（只读远端，不改工作区）。
fn fork_behind(repo: &str) -> String {
    let root = super::paths::repo_root().join("vendor/flowsurface");
    let dir = root.to_string_lossy().into_owned();
    // 上游 remote 可能没配过（fork 一般只有 origin）。幂等地补一个。
    let has = Command::new("git")
        .args(["-C", &dir, "remote", "get-url", "upstream"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !has {
        let _ = Command::new("git")
            .args(["-C", &dir, "remote", "add", "upstream", &format!("https://github.com/{repo}")])
            .status();
    }
    if !Command::new("git")
        .args(["-C", &dir, "fetch", "--quiet", "upstream"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return "上游 fetch 失败（没网？）".into();
    }
    let out = Command::new("git")
        .args(["-C", &dir, "rev-list", "--count", "HEAD..upstream/main"])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let n = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if n == "0" {
                "已跟上上游".into()
            } else {
                format!("落后 {n} 个提交（本仓库是 fork，合并请手动做）")
            }
        }
        _ => "数不出落后多少（上游分支名不是 main？）".into(),
    }
}

/// 查全部依赖的上游版本。**只在用户点「检查更新」时调**，且要在后台线程里跑
/// （十几个 HTTP 请求，进渲染线程会卡住整个界面）。
pub fn check_all() -> String {
    refresh_local();
    let mut found = 0;
    let mut failed = 0;
    for d in ALL {
        let (latest, note) = latest_of(d);
        if latest.is_empty() && note.is_empty() {
            failed += 1;
        }
        if let Ok(mut g) = rows_cell().lock()
            && let Some(r) = g.iter_mut().find(|r| r.key == d.key)
        {
            r.latest = latest;
            r.note = note;
            if r.outdated() {
                found += 1;
            }
        }
    }
    match (found, failed) {
        (0, 0) => "✔ 全部已是最新".into(),
        (n, 0) => format!("✔ 查完：{n} 个有新版本"),
        (n, f) => format!("✔ 查完：{n} 个有新版本，{f} 个没查到（没网 / 包名对不上）"),
    }
}

/// 执行更新。返回给人看的回执。
pub fn update(key: &str) -> String {
    let Some(d) = ALL.iter().find(|d| d.key == key) else {
        return format!("✗ 未知依赖 {key}");
    };
    match d.upd {
        Upd::Pip => {
            let py = super::paths::python();
            let pip = py.parent().map(|p| p.join("pip")).unwrap_or_default();
            let r = Command::new(&pip).args(["install", "-U", d.name]).output();
            match r {
                Ok(o) if o.status.success() => {
                    refresh_local();
                    format!("✔ {} 已更新——**用到它的守护要重启才生效**", d.label)
                }
                Ok(o) => format!("✗ pip 失败：{}", String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("")),
                Err(e) => format!("✗ 起不了 pip：{e}"),
            }
        }
        Upd::Cargo => {
            let root = super::paths::repo_root();
            let r = Command::new("cargo")
                .args(["update", "-p", d.name])
                .current_dir(&root)
                .output();
            match r {
                Ok(o) if o.status.success() => {
                    refresh_local();
                    format!("✔ {} 的 Cargo.lock 已更新——**要重新编译才生效**", d.label)
                }
                Ok(o) => format!("✗ cargo update 失败：{}", String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("")),
                Err(e) => format!("✗ 起不了 cargo：{e}"),
            }
        }
        Upd::FetchOnly => {
            // 故意不 merge。这个 fork 有六十多个自有模块，在运行中的 GUI 里
            // 替用户合并上游、然后脚下的二进制被重编——那是事故，不是便利。
            let Src::GitHub(repo) = d.src else { return "✗ 配置不一致".into() };
            let n = fork_behind(repo);
            format!(
                "已 fetch 上游（未合并）：{n}。要合并请手动：\
                 cd vendor/flowsurface && git merge upstream/main"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_yet_checked_is_not_the_same_as_up_to_date() {
        // 这是这一页最容易骗人的地方：还没查过就显示「已最新」，
        // 会让人以为依赖是新的。空的 latest 必须两者都为 false。
        let r = Row { current: "1.0".into(), latest: String::new(), ..Default::default() };
        assert!(!r.outdated());
        assert!(!r.checked());
    }

    #[test]
    fn a_differing_upstream_version_is_reported_as_outdated() {
        let r = Row { current: "1.228.0".into(), latest: "1.231.0".into(), ..Default::default() };
        assert!(r.outdated() && r.checked());
    }

    #[test]
    fn the_same_version_is_not_outdated() {
        let r = Row { current: "1.231.0".into(), latest: "1.231.0".into(), ..Default::default() };
        assert!(!r.outdated() && r.checked());
    }

    #[test]
    fn package_names_are_normalised_before_comparing() {
        // pip 报 `nautilus_trader`，PyPI 用 `nautilus-trader`。不归一的话
        // 当前版本永远读不到，整行显示成「没装」。
        assert_eq!(norm("nautilus_trader"), "nautilus-trader");
        assert_eq!(norm("Quick-XML"), "quick-xml");
    }

    #[test]
    fn keys_are_unique() {
        // 重复 key 会让 update() 的 find 命中第一个，按钮更新错的包。
        let mut ks: Vec<&str> = ALL.iter().map(|d| d.key).collect();
        ks.sort_unstable();
        let n = ks.len();
        ks.dedup();
        assert_eq!(ks.len(), n, "deps::ALL 有重复 key");
    }

    #[test]
    fn the_same_crate_name_can_appear_twice_under_different_keys() {
        // `redis` 在 Python 和 Rust 两侧都有。key 必须不同，name 允许相同。
        let redis: Vec<&Dep> = ALL.iter().filter(|d| d.name == "redis").collect();
        assert_eq!(redis.len(), 2);
        assert_ne!(redis[0].key, redis[1].key);
    }

    #[test]
    fn the_fork_is_never_auto_merged() {
        // 在运行中的 GUI 里替用户 merge 上游、重编脚下的二进制 = 事故。
        let fs = ALL.iter().find(|d| d.key == "flowsurface").expect("flowsurface");
        assert_eq!(fs.upd, Upd::FetchOnly);
    }

    #[test]
    fn every_dep_says_what_it_is_for() {
        // 一列光秃秃的版本号没法帮人判断该不该升。
        for d in ALL {
            assert!(!d.what.is_empty(), "{} 没写它在本项目里干什么", d.key);
        }
    }

    #[test]
    fn an_unknown_key_does_not_silently_do_nothing() {
        assert!(update("没有这个").starts_with("✗"));
    }

    #[test]
    fn a_lock_file_yields_name_version_pairs() {
        // 解析 Cargo.lock 的形状：name 行后面跟 version 行。
        let d = std::env::temp_dir().join("ws_deps_lock_test");
        let _ = std::fs::create_dir_all(&d);
        let f = d.join("Cargo.lock");
        std::fs::write(&f, "[[package]]\nname = \"tokio\"\nversion = \"1.40.0\"\n").unwrap();
        let m = lock_versions(&[&f.to_string_lossy()]);
        assert_eq!(m.get("tokio").map(String::as_str), Some("1.40.0"));
        let _ = std::fs::remove_file(&f);
    }
}
