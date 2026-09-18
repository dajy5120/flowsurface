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

use super::recorder::SERVICE;

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

/// 明细表的一行：一个（币种 × 数据类型 × 日期）分区。
///
/// 粒度选在「日」而不是「文件」：录制是 600 秒一段，一天一个流就有 144 个文件，
/// 十个币种四个流铺开是六位数行数，列出来没人看得完。日分区既能落到具体路径，
/// 行数又停在几千量级。段级细节压缩成「首段~末段 + 段数」两列。
#[derive(Clone)]
pub struct LakeRow {
    pub sym: String,
    pub stream: &'static str,
    pub date: String,
    /// 已封档的 parquet 段数。
    pub segs: u64,
    /// 该分区磁盘占用（含未封档/孤儿文件——它们真实占盘）。
    pub bytes: u64,
    /// 首段/末段的起点（`HH:MM`，UTC；文件名就是段起点）。
    pub first: String,
    pub last: String,
    /// 正在写、还没落 footer 的段。**读不了**，但占盘。当天通常恰好 1 个。
    pub inprogress: u64,
    /// 上次崩溃遗留、已改名留证的段（writer.rs `*.parquet.orphan`）。**读不了**。
    pub orphan: u64,
    /// 相对 `data_dir` 的路径（`raw/l2/BTCUSDT/2026-09-17`）。
    /// 不存绝对路径：根目录在表头显示一次就够，每行重复一遍既挤又没信息量。
    pub rel: String,
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
    /// 逐（币种 × 类型 × 日期）明细，**按日期倒序**（最近的在最前）。
    pub rows: Vec<LakeRow>,
    /// 数据落盘根目录的绝对路径（明细表的「存储位置」以此为基准）。
    pub data_dir: String,
    /// 交易所（从 recorder.toml 的 ws_url 推，不写死）。
    pub exchange: String,
    /// 实际在盘上出现过的币种（**不是** PRESETS）——面板加过的自定义币种也在内。
    pub syms_seen: Vec<String>,
    pub refreshed: String,
    pub started: bool,
    /// F0 72h 验收（`ws-f0-accept.service`）的运行状态 + 上次报告结论。
    /// 验收的对象就是本服务的录制质量，所以挂在录制驾驶舱里。
    pub accept: super::svcctl::UnitState,
    /// 上次验收报告的结论行（`PASS` / `FAIL(...)`；空=没有报告）。
    pub accept_verdict: String,
    /// 上次验收报告的文件名日期（空=从没跑过）。
    pub accept_day: String,
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
                poll_accept(&mut st, &super::recorder::expand_dir(&dd));
                let lock = STATE.get_or_init(|| Mutex::new(SvcState::default()));
                if let Ok(mut g) = lock.lock() {
                    *g = st;
                }
                WAKER.wait(Duration::from_secs(3));
            }
        });
    });
}

static WAKER: super::svcctl::Waker = super::svcctl::Waker::new();

/// 面板「刷新」按钮：叫醒 poller 立刻刷一轮（不阻塞 UI）。
pub fn request_refresh() {
    WAKER.request();
}

/// F0 72h 验收单元名（oneshot；跑一次写一份报告到数据目录）。
pub const ACCEPT_SVC: &str = "ws-f0-accept.service";

/// 手动跑一次 F0 验收。
///
/// **必须 `--no-block`**：oneshot 要扫两整天的 parquet 算覆盖率，同步等会冻死 UI
/// （同 nightly 踩过的坑，见 [`super::factory_readout::nightly_start`]）。
pub fn accept_start() -> String {
    let r = match Command::new("systemctl")
        .args(["--user", "start", "--no-block", ACCEPT_SVC])
        .status()
    {
        Ok(s) if s.success() => "▶ 已触发 F0 验收（后台跑，完成后看结论）".into(),
        Ok(s) => format!("✗ 触发失败，退出码 {:?}", s.code()),
        Err(e) => format!("✗ 触发失败：{e}"),
    };
    WAKER.request();
    r
}

/// 读最新一份 `f0-accept-*.log` 的结论行（脚本末行形如 `F0-ACCEPT: PASS`）。
fn poll_accept(st: &mut SvcState, data_dir: &Path) {
    st.accept = super::svcctl::query(ACCEPT_SVC);
    let Ok(rd) = std::fs::read_dir(data_dir) else { return };
    let mut latest: Option<(String, std::path::PathBuf)> = None;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if let Some(day) = name.strip_prefix("f0-accept-").and_then(|s| s.strip_suffix(".log"))
            && latest.as_ref().is_none_or(|(d, _)| day > d.as_str())
        {
            latest = Some((day.to_string(), e.path()));
        }
    }
    if let Some((day, path)) = latest {
        st.accept_day = day;
        if let Ok(c) = std::fs::read_to_string(&path) {
            st.accept_verdict = c
                .lines()
                .rev()
                .find_map(|l| l.trim().strip_prefix("F0-ACCEPT:").map(|v| v.trim().to_string()))
                .unwrap_or_default();
        }
    }
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

/// 目录名 → 面板显示名。目录名是录制器定的（writer.rs），这里只做展示映射。
const STREAMS: [(&str, &str); 4] = [
    ("l2", "L2 增量盘口"),
    ("trades", "逐笔成交"),
    ("mark", "标记价/资金费"),
    ("snap100ms", "顶档快照"),
];

/// 跨全部日期扫数据湖：明细行 + 每币种汇总 + 时间跨度/总大小。
///
/// 币种来自**目录实际存在的**，不是 `PRESETS`——面板能加自定义币种，
/// 按预设列会把它们整个漏掉（漏得还很安静：汇总少一行，没有任何提示）。
fn scan_lake(st: &mut SvcState, data_dir: &Path) {
    let raw = data_dir.join("raw");
    st.data_dir = data_dir.to_string_lossy().into_owned();
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let mut all_dates: BTreeSet<String> = Default::default();
    let mut syms: BTreeSet<String> = Default::default();

    for (dir_name, label) in STREAMS {
        let Ok(sym_dirs) = std::fs::read_dir(raw.join(dir_name)) else {
            continue;
        };
        for sent in sym_dirs.flatten() {
            if !sent.path().is_dir() {
                continue;
            }
            let sym = sent.file_name().to_string_lossy().to_string();
            let Ok(date_dirs) = std::fs::read_dir(sent.path()) else {
                continue;
            };
            for dent in date_dirs.flatten() {
                if !dent.path().is_dir() {
                    continue;
                }
                let date = dent.file_name().to_string_lossy().to_string();
                let Ok(files) = std::fs::read_dir(dent.path()) else {
                    continue;
                };
                let mut r = LakeRow {
                    sym: sym.clone(),
                    stream: label,
                    date: date.clone(),
                    segs: 0,
                    bytes: 0,
                    first: String::new(),
                    last: String::new(),
                    inprogress: 0,
                    orphan: 0,
                    rel: format!("raw/{dir_name}/{sym}/{date}"),
                };
                let mut rows_today = 0u64;
                for f in files.flatten() {
                    let path = f.path();
                    let name = f.file_name().to_string_lossy().to_string();
                    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
                    if let Some(stem) = name.strip_suffix(".parquet") {
                        r.segs += 1;
                        r.bytes += len;
                        if r.first.is_empty() || stem < r.first.as_str() {
                            r.first = stem.to_string();
                        }
                        if stem > r.last.as_str() {
                            r.last = stem.to_string();
                        }
                        if date == today {
                            rows_today += parquet_rows(&path).unwrap_or(0);
                        }
                    } else if name.ends_with(".parquet.inprogress") {
                        r.inprogress += 1;
                        r.bytes += len;
                    } else if name.ends_with(".parquet.orphan") {
                        r.orphan += 1;
                        r.bytes += len;
                    }
                }
                if r.segs == 0 && r.inprogress == 0 && r.orphan == 0 {
                    continue; // 空目录，不占一行
                }
                r.first = hhmm(&r.first);
                r.last = hhmm(&r.last);
                syms.insert(sym.clone());
                all_dates.insert(date.clone());

                let agg = st.lake.entry(sym.clone()).or_default();
                agg.bytes += r.bytes;
                agg.today_rows += rows_today;
                st.total_bytes += r.bytes;
                st.rows.push(r);
            }
        }
    }

    // 「录制天数」按币种去重的日期数算：四个流同一天各有一行，直接数行会翻四倍。
    for (sym, agg) in st.lake.iter_mut() {
        agg.days = st
            .rows
            .iter()
            .filter(|r| &r.sym == sym)
            .map(|r| r.date.as_str())
            .collect::<BTreeSet<_>>()
            .len() as i64;
    }

    // 日期倒序：最近录的最该先看到。同日内按币种、类型排稳定。
    st.rows.sort_by(|a, b| {
        b.date.cmp(&a.date).then(a.sym.cmp(&b.sym)).then(a.stream.cmp(b.stream))
    });
    st.syms_seen = syms.into_iter().collect();
    st.exchange = super::recorder::config_exchange();
    st.span_days = all_dates.len() as i64;
    if let Some(f) = all_dates.iter().next() {
        st.span_first = f.clone();
    }
    if let Some(l) = all_dates.iter().next_back() {
        st.span_last = l.clone();
    }
}

/// 数据类型的显示名列表（明细表筛选下拉用）。
pub fn stream_labels() -> Vec<&'static str> {
    STREAMS.iter().map(|(_, label)| *label).collect()
}

/// `"081000"` → `"08:10"`（文件名即段起点，UTC）。非预期格式原样返回。
fn hhmm(stem: &str) -> String {
    if stem.len() == 6 && stem.bytes().all(|b| b.is_ascii_digit()) {
        format!("{}:{}", &stem[0..2], &stem[2..4])
    } else {
        stem.to_string()
    }
}

/// 只读 parquet 页脚元数据取行数（不解压列数据，故无需 arrow/压缩特性）。
fn parquet_rows(path: &Path) -> Option<u64> {
    use parquet::file::reader::{FileReader, SerializedFileReader};
    let f = std::fs::File::open(path).ok()?;
    Some(SerializedFileReader::new(f).ok()?.metadata().file_metadata().num_rows() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一棵 `raw/{流}/{币}/{日}/{HHMMSS}.parquet` 目录树。
    fn mk(root: &Path, stream: &str, sym: &str, date: &str, files: &[(&str, usize)]) {
        let d = root.join("raw").join(stream).join(sym).join(date);
        std::fs::create_dir_all(&d).unwrap();
        for (name, len) in files {
            std::fs::write(d.join(name), vec![0u8; *len]).unwrap();
        }
    }

    #[test]
    fn hhmm_只格式化六位数字() {
        assert_eq!(hhmm("081000"), "08:10");
        assert_eq!(hhmm("000000"), "00:00");
        // 非预期格式原样返回，而不是 panic 或截出乱码
        assert_eq!(hhmm(""), "");
        assert_eq!(hhmm("abc"), "abc");
    }

    #[test]
    fn 扫描出明细并正确汇总() {
        let root = std::env::temp_dir().join(format!("ws-lake-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);

        // 同一天、同一币、两个流 —— 「录制天数」必须算 1 天而不是 2
        mk(&root, "l2", "BTCUSDT", "2026-09-10", &[("000000.parquet", 100), ("001000.parquet", 200)]);
        mk(&root, "trades", "BTCUSDT", "2026-09-10", &[("000000.parquet", 50)]);
        // 另一天 + 一个未封档 + 一个孤儿：都占盘但读不了
        mk(
            &root,
            "l2",
            "BTCUSDT",
            "2026-09-11",
            &[
                ("120000.parquet", 10),
                ("121000.parquet.inprogress", 7),
                ("110000.parquet.orphan", 3),
            ],
        );
        // PRESETS 里没有的自定义币种，必须也扫得到
        mk(&root, "l2", "FOOUSDT", "2026-09-11", &[("000000.parquet", 1)]);
        // 空目录不该占一行
        std::fs::create_dir_all(root.join("raw/mark/BTCUSDT/2026-09-12")).unwrap();

        let mut st = SvcState::default();
        scan_lake(&mut st, &root);

        // 日期倒序，同日内按币种
        let keys: Vec<_> =
            st.rows.iter().map(|r| (r.date.as_str(), r.sym.as_str(), r.stream)).collect();
        assert_eq!(
            keys,
            vec![
                ("2026-09-11", "BTCUSDT", "L2 增量盘口"),
                ("2026-09-11", "FOOUSDT", "L2 增量盘口"),
                ("2026-09-10", "BTCUSDT", "L2 增量盘口"),
                ("2026-09-10", "BTCUSDT", "逐笔成交"),
            ],
            "空目录不该出现；排序应为日期倒序"
        );

        let r = &st.rows[0]; // BTCUSDT 2026-09-11 L2
        assert_eq!((r.segs, r.inprogress, r.orphan), (1, 1, 1));
        assert_eq!(r.bytes, 20, "大小要含未封档与孤儿——那是真实磁盘占用");
        assert_eq!((r.first.as_str(), r.last.as_str()), ("12:00", "12:00"));
        assert_eq!(r.rel, "raw/l2/BTCUSDT/2026-09-11");

        // 跨流同日去重：BTC 是 09-10 与 09-11 两天，不是四行=四天
        assert_eq!(st.lake["BTCUSDT"].days, 2);
        assert_eq!(st.lake["BTCUSDT"].bytes, 100 + 200 + 50 + 20);
        assert_eq!(st.syms_seen, vec!["BTCUSDT", "FOOUSDT"]);
        assert_eq!((st.span_first.as_str(), st.span_last.as_str()), ("2026-09-10", "2026-09-11"));
        assert_eq!(st.span_days, 2);
        assert_eq!(st.total_bytes, 371);

        let _ = std::fs::remove_dir_all(&root);
    }
}
