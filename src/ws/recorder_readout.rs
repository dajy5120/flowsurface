//! 录制驾驶舱只读状态快照（docs/08 F6 — P3）。
//!
//! 移植 `wealthspring-recorder-gui` 的后台 poller(每 3s):systemctl 服务状态、journal 录制实况、
//! 数据湖扫描(跨全部日期:时间跨度/总大小/每币种 天数/大小)。发布到进程级快照,供
//! `Content::Recorder` pane 渲染(`recorder_view`)。惰性起:打开过 数据录制 工作区才轮询。
//!
//! 「今日行数」读 parquet 页脚 `num_rows`,用 `parquet` crate 的 `default-features=false`
//! (不引入 arrow/压缩——行数在 Thrift 元数据里,无需解压列数据,依赖很轻)。

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use super::recorder::{PRESETS, SERVICE};

#[derive(Default, Clone)]
pub struct SymLive {
    pub l2: u64,
    pub trades: u64,
    pub mark: u64,
    pub snap20: u64,
    pub resyncs: u64,
    pub parse_errs: u64,
    pub growing: bool,
}

#[derive(Default, Clone)]
pub struct LakeSym {
    pub days: i64,
    pub bytes: u64,
    pub today_rows: u64,
}

#[derive(Default, Clone)]
pub struct SvcState {
    pub active: bool,
    pub uptime_secs: i64,
    pub restarts: i64,
    pub live: BTreeMap<String, SymLive>,
    pub span_first: String,
    pub span_last: String,
    pub span_days: i64,
    pub total_bytes: u64,
    pub lake: BTreeMap<String, LakeSym>,
    pub refreshed: String,
    pub started: bool,
}

static STATE: OnceLock<Mutex<SvcState>> = OnceLock::new();
static POLLER: OnceLock<()> = OnceLock::new();

/// pane 渲染时读快照(惰性起 poller)。
pub fn snapshot() -> SvcState {
    ensure_poller();
    STATE
        .get()
        .and_then(|m| m.lock().ok().map(|g| g.clone()))
        .unwrap_or_default()
}

fn ensure_poller() {
    POLLER.get_or_init(|| {
        std::thread::spawn(|| {
            let mut prev: BTreeMap<String, SymLive> = BTreeMap::new();
            loop {
                let mut st = SvcState {
                    refreshed: chrono::Local::now().format("%H:%M:%S").to_string(),
                    started: true,
                    ..Default::default()
                };
                poll_service(&mut st);
                poll_journal(&mut st, &prev);
                prev = st.live.clone();
                let dd = super::recorder::config_data_dir();
                scan_lake(&mut st, &super::recorder::expand_dir(&dd));
                let lock = STATE.get_or_init(|| Mutex::new(SvcState::default()));
                if let Ok(mut g) = lock.lock() {
                    *g = st;
                }
                std::thread::sleep(Duration::from_secs(3));
            }
        });
    });
}

fn sysctl(args: &[&str]) -> String {
    Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn poll_service(st: &mut SvcState) {
    st.active = sysctl(&["is-active", SERVICE]) == "active";
    st.restarts = sysctl(&["show", SERVICE, "-p", "NRestarts", "--value"])
        .parse()
        .unwrap_or(0);
    if st.active {
        let mono: f64 = sysctl(&["show", SERVICE, "-p", "ActiveEnterTimestampMonotonic", "--value"])
            .parse::<f64>()
            .unwrap_or(0.0)
            / 1e6;
        let up: f64 = std::fs::read_to_string("/proc/uptime")
            .ok()
            .and_then(|s| s.split_whitespace().next().and_then(|x| x.parse().ok()))
            .unwrap_or(0.0);
        st.uptime_secs = (up - mono).max(0.0) as i64;
    }
}

/// 解析 journal 最新每 symbol 状态行(`[recorder][SYM] l2=.. trades=.. ...`)。
fn poll_journal(st: &mut SvcState, prev: &BTreeMap<String, SymLive>) {
    let out = Command::new("journalctl")
        .args(["--user", "-u", SERVICE, "-n", "120", "--no-pager", "-o", "cat"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    for line in out.lines() {
        let Some(rest) = line.strip_prefix("[recorder][") else {
            continue;
        };
        let Some((sym, kv)) = rest.split_once("] ") else {
            continue;
        };
        if !kv.contains("l2=") {
            continue;
        }
        let mut s = SymLive::default();
        for tok in kv.split_whitespace() {
            if let Some((k, v)) = tok.split_once('=') {
                let n: u64 = v.parse().unwrap_or(0);
                match k {
                    "l2" => s.l2 = n,
                    "trades" => s.trades = n,
                    "mark" => s.mark = n,
                    "snap20" => s.snap20 = n,
                    "resyncs" => s.resyncs = n,
                    "parse_errs" => s.parse_errs = n,
                    _ => {}
                }
            }
        }
        if let Some(p) = prev.get(sym) {
            s.growing = s.l2 > p.l2 || s.trades > p.trades || s.snap20 > p.snap20;
        }
        st.live.insert(sym.to_string(), s);
    }
}

const STREAMS: [&str; 4] = ["l2", "trades", "mark", "snap100ms"];

/// 跨全部日期扫数据湖:时间跨度 + 总大小 + 每币种天数/大小/今日行数。
fn scan_lake(st: &mut SvcState, data_dir: &Path) {
    let raw = data_dir.join("raw");
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let mut all_dates: BTreeSet<String> = Default::default();
    for sym in PRESETS {
        let mut ls = LakeSym::default();
        let mut sym_dates: BTreeSet<String> = Default::default();
        for stream in STREAMS {
            let sdir = raw.join(stream).join(sym);
            let Ok(rd) = std::fs::read_dir(&sdir) else {
                continue;
            };
            for ent in rd.flatten() {
                if !ent.path().is_dir() {
                    continue;
                }
                let date = ent.file_name().to_string_lossy().to_string();
                sym_dates.insert(date.clone());
                all_dates.insert(date.clone());
                if let Ok(files) = std::fs::read_dir(ent.path()) {
                    for f in files.flatten() {
                        let p = f.path();
                        if p.extension().is_some_and(|e| e == "parquet") {
                            ls.bytes += std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                            if date == today {
                                ls.today_rows += parquet_rows(&p).unwrap_or(0);
                            }
                        }
                    }
                }
            }
        }
        ls.days = sym_dates.len() as i64;
        if ls.days > 0 {
            st.total_bytes += ls.bytes;
            st.lake.insert(sym.to_string(), ls);
        }
    }
    st.span_days = all_dates.len() as i64;
    if let Some(f) = all_dates.iter().next() {
        st.span_first = f.clone();
    }
    if let Some(l) = all_dates.iter().next_back() {
        st.span_last = l.clone();
    }
}

/// 只读 parquet 页脚元数据取行数（不解压列数据，故无需 arrow/压缩特性）。
fn parquet_rows(path: &Path) -> Option<u64> {
    use parquet::file::reader::{FileReader, SerializedFileReader};
    let f = std::fs::File::open(path).ok()?;
    Some(SerializedFileReader::new(f).ok()?.metadata().file_metadata().num_rows() as u64)
}
