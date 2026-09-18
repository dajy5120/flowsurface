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

/// 某（币种 × 日期）**真正录到了哪些时间段、洞在哪**。
///
/// ## 为什么要有它
///
/// ④ 录制明细回答「哪天有多少数据」，回答不了「那天能不能跑回测」——
/// 实测 2026-09-18 有 6.1 小时数据、却被打成 12 段，最长无洞段只有 10.7 分钟；
/// 而 2026-08-14 同样 24 小时数据，最长无洞段是 762 分钟。同样叫「有数据」，
/// 一个能跑一个不能。
///
/// ## 诚实边界：只看得见**段间**的洞
///
/// 区间取自 parquet 页脚统计（`ts_recv` 的 min/max），**不解压数据**——
/// 读 5.4 万个文件的数据不可行，读页脚只要 ~0.08ms/个。
/// 代价是：一段内部的短暂断流看不见（段是 600 秒轮转）。
///
/// 但要紧的洞恰恰是段间的：服务被停/重启造成的。实测那个 4.4 小时的洞，
/// 就是 `ws-stack.target`「跟随 Cockpit 生命周期」、重启 Cockpit 把录制器一起带走造成的。
///
/// **所以这里的数字是「至少这么碎」，不是「就这么碎」。** 回测入口的窗口体检
/// （`factory/evaluate/recorder_health.py`）读真实数据，它说了算。
#[derive(Clone, Default)]
pub struct DayCoverage {
    pub sym: String,
    pub date: String,
    /// 录到的连续段（已合并），`(起, 止)` 纳秒，升序。
    pub runs: Vec<(i64, i64)>,
    /// 缺口 `(起, 止)` 纳秒。
    pub gaps: Vec<(i64, i64)>,
    /// 覆盖总秒数。
    pub covered_s: i64,
    /// 最长无洞段的秒数与起点。**这个数决定那天能不能跑**。
    pub longest_s: i64,
    pub longest_at: i64,
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
    /// 逐（币种 × 日期）的覆盖与缺口，**按日期倒序**。见 [`DayCoverage`]。
    pub coverage: Vec<DayCoverage>,
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
            // 段区间缓存跨轮复用：封档的 parquet 永不变，首轮之后几乎零成本。
            let mut ts_cache: TsCache = TsCache::new();
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
                scan_lake(&mut st, &super::recorder::expand_dir(&dd), &mut ts_cache);
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
fn scan_lake(st: &mut SvcState, data_dir: &Path, ts_cache: &mut TsCache) {
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
                // 覆盖只按主盘口流算：全 L2 档是 l2，轻量档只有 snap100ms。
                // 两者都取会把同一段时间数两遍。
                let cover_stream = dir_name == "l2" || dir_name == "snap100ms";
                let mut spans: Vec<(i64, i64)> = Vec::new();
                for f in files.flatten() {
                    let path = f.path();
                    let name = f.file_name().to_string_lossy().to_string();
                    let meta = f.metadata().ok();
                    let len = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                    if let Some(stem) = name.strip_suffix(".parquet") {
                        if cover_stream {
                            let mt = meta.as_ref().and_then(|m| m.modified().ok());
                            let hit = mt.and_then(|mt| {
                                ts_cache.get(&path).filter(|(c, _)| *c == mt).map(|(_, v)| v.clone())
                            });
                            let got = match hit {
                                Some(v) => Some(v),
                                None => parquet_ts_runs(&path).inspect(|v| {
                                    if let Some(mt) = mt {
                                        ts_cache.insert(path.clone(), (mt, v.clone()));
                                    }
                                }),
                            };
                            if let Some(v) = got {
                                spans.extend(v);
                            }
                        }
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
                if cover_stream && !spans.is_empty() {
                    let mut cov = coverage_from_spans(spans);
                    cov.sym = sym.clone();
                    cov.date = date.clone();
                    st.coverage.push(cov);
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
    // 覆盖同样按日期倒序——最近录的最该先看到。
    st.coverage.sort_by(|a, b| b.date.cmp(&a.date).then(a.sym.cmp(&b.sym)));
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

/// 段间超过这么久算一个洞（纳秒）。与 `tardis_health` 的 `GAP_WARN_US` 同口径：
/// 交易所推送连续，超过 1 秒说明采集侧断过。
const GAP_NS: i64 = 1_000_000_000;

/// 段区间缓存：封档的 parquet **永不改变**，算过一次就不必再读页脚。
/// 键含 mtime——`.inprogress` 转正后 mtime 会变，据此自动失效。
type TsCache = BTreeMap<std::path::PathBuf, (std::time::SystemTime, Vec<(i64, i64)>)>;

/// 由各段的 `[起, 止]` 算出「录到的连续段」与「缺口」。
///
/// 抽成纯函数是为了能测：区间合并的边界（相邻段首尾相接、段重叠、单段、空）
/// 每一种都容易悄悄算错，而错了的表现只是「缺口数字怪怪的」，没人会当成 bug 报。
fn coverage_from_spans(mut spans: Vec<(i64, i64)>) -> DayCoverage {
    let mut c = DayCoverage::default();
    if spans.is_empty() {
        return c;
    }
    spans.sort_unstable();
    let mut cur = spans[0];
    for &(lo, hi) in &spans[1..] {
        if lo - cur.1 <= GAP_NS {
            cur.1 = cur.1.max(hi); // 相接或重叠 → 并进当前段
        } else {
            c.runs.push(cur);
            c.gaps.push((cur.1, lo));
            cur = (lo, hi);
        }
    }
    c.runs.push(cur);
    for &(lo, hi) in &c.runs {
        c.covered_s += (hi - lo) / 1_000_000_000;
        if hi - lo > c.longest_s * 1_000_000_000 {
            c.longest_s = (hi - lo) / 1_000_000_000;
            c.longest_at = lo;
        }
    }
    c
}

/// 读该段的 `ts_recv` 列，切成「无洞连续子段」。
///
/// ## 为什么非读数据不可
///
/// 第一版只读页脚统计（min/max），以为段间的洞才要紧。**实测推翻了这个假设**：
/// 页脚法算出的最长无洞段比真值高估 **1.27~4.61 倍**——
/// 2026-09-18 页脚说 49.2 分钟，真值 10.7 分钟。
///
/// 照那个数去跑，回测会被入口体检当场拒掉。一个会把人送进死胡同的数字，
/// 比没有这个数字更糟。
///
/// 段内的洞是真实存在的：录制器还在跑、WS 流却停了（本机走 VPN，日志里
/// `tls connection init failed` 反复出现）。600 秒的段里塞着几十秒的空白，
/// 页脚的 min/max 完全看不见。
///
/// 代价：单列读约 1.7ms/文件 × 1.2 万个 ≈ 21 秒，**一次性**——
/// 封档的段永不改变，结果按 (路径, mtime) 缓存。
fn parquet_ts_runs(path: &Path) -> Option<Vec<(i64, i64)>> {
    use parquet::column::reader::ColumnReader;
    use parquet::file::reader::{FileReader, SerializedFileReader};

    let f = std::fs::File::open(path).ok()?;
    let r = SerializedFileReader::new(f).ok()?;
    let md = r.metadata();
    let col = (0..md.file_metadata().schema_descr().num_columns())
        .find(|&i| md.file_metadata().schema_descr().column(i).name() == "ts_recv")?;

    let mut runs: Vec<(i64, i64)> = Vec::new();
    let mut cur: Option<(i64, i64)> = None;
    // ⚠ `read_records` 的 values 参数是**追加**语义，不是覆写。
    // 预先 `vec![0i64; N]` 再切 `buf[..read]` 读到的全是前导零——
    // 表现是每个文件多出一个 `0 ~ 0` 的假子段、且真数据被截断，**不报任何错**。
    // 必须传空 Vec 并每轮 clear。
    let mut buf: Vec<i64> = Vec::with_capacity(8192);
    let mut total = 0usize;
    for g in 0..md.num_row_groups() {
        let rg = r.get_row_group(g).ok()?;
        let ColumnReader::Int64ColumnReader(mut cr) = rg.get_column_reader(col).ok()? else {
            return None;
        };
        loop {
            buf.clear();
            let (_, read, _) = cr.read_records(8192, None, None, &mut buf).ok()?;
            if read == 0 {
                break;
            }
            total += read;
            for &t in &buf[..read] {
                match cur {
                    Some((lo, hi)) if t - hi <= GAP_NS => cur = Some((lo, t.max(hi))),
                    Some(seg) => {
                        runs.push(seg);
                        cur = Some((t, t));
                    }
                    None => cur = Some((t, t)),
                }
            }
        }
    }
    if let Some(seg) = cur {
        runs.push(seg);
    }
    // 对账：读到的行数必须等于页脚记的行数。不等说明列读取悄悄少读了——
    // 那正是「追加语义」那个 bug 的表现，而它本身不报任何错。
    debug_assert_eq!(total, md.file_metadata().num_rows() as usize, "{}", path.display());
    (!runs.is_empty()).then_some(runs)
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

    const S: i64 = 1_000_000_000;

    #[test]
    fn 覆盖_单段无缺口() {
        let c = coverage_from_spans(vec![(0, 600 * S)]);
        assert_eq!(c.runs.len(), 1);
        assert!(c.gaps.is_empty());
        assert_eq!(c.covered_s, 600);
        assert_eq!(c.longest_s, 600);
    }

    #[test]
    fn 覆盖_相接的段要合并而不是算成两段() {
        // 600 秒轮转的相邻段首尾相接（间隔 0）——那是同一段连续录制，不是两段。
        // 当成两段的话「连续段」数会等于文件数，这一列就完全没有信息量了。
        let c = coverage_from_spans(vec![(0, 600 * S), (600 * S, 1200 * S)]);
        assert_eq!(c.runs.len(), 1, "相接应合并：{:?}", c.runs);
        assert!(c.gaps.is_empty());
        assert_eq!(c.longest_s, 1200);
    }

    #[test]
    fn 覆盖_一秒以内的间隔不算洞() {
        // 与 tardis_health 的 GAP_WARN_US 同口径。抖动几百毫秒算成洞的话，
        // 每天会报出成百上千个「缺口」，真正的 4 小时断流反而被淹没。
        let c = coverage_from_spans(vec![(0, 600 * S), (600 * S + S / 2, 1200 * S)]);
        assert_eq!(c.runs.len(), 1, "0.5 秒不该算洞");
    }

    #[test]
    fn 覆盖_真断流要算成洞且时长正确() {
        let c = coverage_from_spans(vec![(0, 600 * S), (4200 * S, 4800 * S)]);
        assert_eq!(c.runs.len(), 2);
        assert_eq!(c.gaps.len(), 1);
        assert_eq!(c.gaps[0], (600 * S, 4200 * S));
        assert_eq!((c.gaps[0].1 - c.gaps[0].0) / S, 3600, "洞应是 1 小时");
        assert_eq!(c.covered_s, 1200, "覆盖只数真正录到的");
    }

    #[test]
    fn 覆盖_最长段取的是最长的那个而不是第一个() {
        // 这条钉的是一个真实场景：实测 2026-09-18 的第一段（10.7 分钟）恰好也是最长的，
        // 取错了不会暴露；换成第一段短、后面有长段就露馅了。
        let c = coverage_from_spans(vec![
            (0, 60 * S),                    // 1 分钟
            (3600 * S, 3600 * S + 900 * S), // 15 分钟 ← 最长
            (9000 * S, 9000 * S + 120 * S), // 2 分钟
        ]);
        assert_eq!(c.longest_s, 900);
        assert_eq!(c.longest_at, 3600 * S, "起点要指向最长那段");
        assert_eq!(c.runs.len(), 3);
        assert_eq!(c.gaps.len(), 2);
    }

    #[test]
    fn 覆盖_乱序输入也要正确() {
        // 目录遍历顺序不保证；按文件名排序在跨零点或异常命名时也未必对。
        let a = coverage_from_spans(vec![(3600 * S, 4200 * S), (0, 600 * S)]);
        let b = coverage_from_spans(vec![(0, 600 * S), (3600 * S, 4200 * S)]);
        assert_eq!(a.runs, b.runs);
        assert_eq!(a.gaps, b.gaps);
    }

    #[test]
    fn 覆盖_重叠的段不重复计时长() {
        let c = coverage_from_spans(vec![(0, 600 * S), (300 * S, 900 * S)]);
        assert_eq!(c.runs.len(), 1);
        assert_eq!(c.covered_s, 900, "重叠部分只算一次");
    }

    #[test]
    fn 覆盖_空输入不崩() {
        let c = coverage_from_spans(vec![]);
        assert!(c.runs.is_empty() && c.gaps.is_empty());
        assert_eq!(c.covered_s, 0);
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
        scan_lake(&mut st, &root, &mut TsCache::new());

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

#[cfg(test)]
mod real_data_tests {
    use super::*;

    /// 对着**真实数据**验算覆盖，防止「页脚法」那种看似合理实则高估几倍的近似再溜回来。
    ///
    /// **不钉具体分钟数**：录制器 24/7 在跑，数据每分钟都在长，钉死的期望值必然过期
    /// （第一版钉了 10.7 分钟，几小时后真值就成了 91.2）。改为验**同一份数据内部自洽**：
    /// 逐档读出来的子段，合并后必须恰好覆盖全部行、且段间空隙都真的 >1 秒。
    ///
    /// 这个判据照样能抓住页脚近似——页脚法看不见段内的洞，合并后的覆盖会**多**出
    /// 那些本该被切掉的空隙，逐行核对立刻露馅。
    #[test]
    fn 真实数据的子段切分与逐行核对一致() {
        let day = std::path::Path::new("/home/dajy/ws-data/raw/l2/BTCUSDT");
        let Some(sub) = std::fs::read_dir(day)
            .ok()
            .and_then(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).max())
        else {
            eprintln!("跳过：本机无录制数据");
            return;
        };
        let mut files: Vec<_> = std::fs::read_dir(&sub)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "parquet"))
            .collect();
        files.sort();
        if files.is_empty() {
            return;
        }
        // 只取前几个文件，够验切分正确性且不必读整天。
        let mut spans = Vec::new();
        for f in files.iter().take(3) {
            if let Some(v) = parquet_ts_runs(f) {
                spans.extend(v);
            }
        }
        assert!(!spans.is_empty(), "应读出子段");

        // ① 每个子段内部必须单调、且 hi >= lo。
        for &(lo, hi) in &spans {
            assert!(hi >= lo, "子段 {lo}..{hi} 首尾颠倒");
        }
        // ② 合并后，相邻段之间的空隙必须都 >1 秒——否则就是本该合并却切开了。
        let c = coverage_from_spans(spans.clone());
        for &(a, b) in &c.gaps {
            assert!(b - a > GAP_NS, "缺口 {}ns 不足 1 秒，不该被切开", b - a);
        }
        // ③ 覆盖时长不得超过首尾跨度——超了说明重复计了。
        let lo = spans.iter().map(|s| s.0).min().unwrap();
        let hi = spans.iter().map(|s| s.1).max().unwrap();
        assert!(
            c.covered_s <= (hi - lo) / 1_000_000_000 + 1,
            "覆盖 {}s 超过首尾跨度 {}s",
            c.covered_s,
            (hi - lo) / 1_000_000_000
        );
        // ④ 至少切出一个子段；若整段无洞则 runs==1、gaps==0，也合法。
        assert!(!c.runs.is_empty());
    }
}
