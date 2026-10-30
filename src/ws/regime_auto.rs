//! 市场状态跟随图表自动算（docs/69 B、docs/70 B）：K 线图每帧报到「我（图 id）在看 品种 × 周期、加载了哪段」，
//! 后台线程 `ws-regime-auto` 看到有图在看、而图要用的那份状态没有 / 过期 / 参数改了，就跑一次 `python -m factory.regime run`。
//!
//! - **实时图**（最后一根就是现在）：从币安 U 本位永续拉 K 线算（`--days`），新 K 线收盘 20 秒后增量重算。
//! - **历史图**（最后一根比现在早 3 个周期以上：本地文件、回放）：用**图上加载的这段 K 线**算（docs/70 B）——
//!   图把 K 线写成 CSV（[`export_request`] / [`export_ready`]），`--bars <文件>`；结果存 `chart.<代码>_<周期>.<首日>-<末日>.json`，
//!   不覆盖实时那份。L1 用本地缓存的资金费 / 持仓，不联网。已有盖得住这段的同周期那份、参数没改 → 不再算（历史不会变）。
//! - 只算打开着的图（最近 15 秒报到过），一次只跑一个；子进程留在 Cockpit 的 cgroup 里，关窗即停。
//!   这是「耗资源任务人工触发」的例外（用户 10-30 同意）：量小，只为正在看的图。
//! - 各周期都算全部维度（docs/70 A：不再 `--l2-only`；短周期的 L1 在 Python 里改用 15 分钟 K 线算再对齐）。
//! - 拉不到的代码记下原因，10 分钟内不再试；一次超过 3 分钟没算完（网络卡住）就停掉、记失败。
//! - 守护 `ws-regime` 照旧不 enable；实时图与它写同一份文件。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::regime::RegimeFile;

/// 报到后多久内算「在看」。
const SEEN_TTL: Duration = Duration::from_secs(15);
/// 收盘后等多久再取（币安收盘后几秒 K 线才可取）。
const CLOSE_DELAY_MS: u64 = 20_000;
/// 刚算完至少隔这么久才再算同一个（币安还没出新 K 线时不空转）。
const MIN_GAP: Duration = Duration::from_secs(30);
/// 出错后隔多久再试。
const ERR_BACKOFF: Duration = Duration::from_secs(10 * 60);
/// 一次最多算多久：网络卡住（下载停在半路、迟迟不超时）时杀掉，别把后面的图都堵住。
const HARD_TIMEOUT: Duration = Duration::from_secs(120);
/// 历史图：同周期那份盖住图上这段的比例够这么多就不再算。
const HIST_COVER: f64 = 0.9;

const TFS: [(u64, &str); 12] = [
    (60_000, "1m"),
    (180_000, "3m"),
    (300_000, "5m"),
    (900_000, "15m"),
    (1_800_000, "30m"),
    (3_600_000, "1h"),
    (7_200_000, "2h"),
    (14_400_000, "4h"),
    (21_600_000, "6h"),
    (28_800_000, "8h"),
    (43_200_000, "12h"),
    (86_400_000, "1d"),
];

/// 图的周期（毫秒）→ 币安周期字符串。
#[must_use]
pub fn tf_str(iv: u64) -> Option<&'static str> {
    TFS.iter().find(|(ms, _)| *ms == iv).map(|(_, s)| *s)
}

/// 这张图的叠加层实际用的周期：图的显示设置里指定了就是它，否则跟图（docs/69 B：算的就是图上画的那份）。
#[must_use]
pub fn target(chart: &str, chart_iv: u64) -> Option<(&'static str, u64)> {
    if let Some(w) = super::regime::view(chart).tf
        && let Some((ms, s)) = TFS.iter().find(|(_, s)| *s == w)
    {
        return Some((s, *ms));
    }
    tf_str(chart_iv).map(|s| (s, chart_iv))
}

/// 实时图算多少天（另加预热）。
#[must_use]
pub fn days_for(iv: u64) -> u32 {
    match iv {
        0..=60_000 => 14,
        60_001..=180_000 => 30,
        180_001..=300_000 => 45,
        300_001..=1_800_000 => 365,
        1_800_001..=14_400_000 => 730,
        _ => 1460,
    }
}

/// 历史图：最后一根收盘比现在早 3 个周期以上（本地文件、回放）。`range` = (第一根开盘, 最后一根收盘)。
#[must_use]
pub fn is_hist(range: Option<(u64, u64)>, iv: u64, now_ms: u64) -> bool {
    range.is_some_and(|(_, end)| end + 3 * iv < now_ms)
}

/// 一次计算的进度。
#[derive(Debug, Clone)]
pub enum Job {
    /// `why`：为什么算
    Running { since: Instant, why: String },
    Done { at: Instant, secs: f32 },
    Failed { at: Instant, msg: String },
}

#[derive(Debug, Clone)]
struct Seen {
    symbol: String,
    iv: u64,
    range: Option<(u64, u64)>,
    at: Instant,
}

fn seen() -> &'static Mutex<HashMap<String, Seen>> {
    static S: OnceLock<Mutex<HashMap<String, Seen>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

/// 进度按（代码, 周期, 实时 / 历史段）记：同一品种的实时图与历史图互不挡。
fn jobs() -> &'static Mutex<HashMap<String, Job>> {
    static J: OnceLock<Mutex<HashMap<String, Job>>> = OnceLock::new();
    J.get_or_init(Default::default)
}

fn forced() -> &'static Mutex<HashSet<String>> {
    static F: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    F.get_or_init(Default::default)
}

/// 历史图要的 K 线：图 id → (写到哪, 并成多大的周期)；图取走后写 CSV，写好了登记到 `exported`。
fn export_req() -> &'static Mutex<HashMap<String, (PathBuf, u64)>> {
    static E: OnceLock<Mutex<HashMap<String, (PathBuf, u64)>>> = OnceLock::new();
    E.get_or_init(Default::default)
}

/// 图 id → (写好的 CSV, 写的那段)。
type Exported = HashMap<String, (PathBuf, (u64, u64))>;

fn exported() -> &'static Mutex<Exported> {
    static E: OnceLock<Mutex<Exported>> = OnceLock::new();
    E.get_or_init(Default::default)
}

fn code_of(symbol_key: &str) -> String {
    symbol_key.rsplit(':').next().unwrap_or(symbol_key).to_uppercase()
}

fn now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
}

fn job_key(code: &str, tf: &str, hist: Option<(u64, u64)>) -> String {
    match hist {
        Some((a, b)) => format!("{code} {tf} 历史 {a}-{b}"),
        None => format!("{code} {tf}"),
    }
}

/// K 线图每帧调：我（图 id）在看这个品种这个周期、加载了 `range` 这段。
pub fn note_chart(chart: &str, symbol_key: &str, iv: u64, range: Option<(u64, u64)>) {
    if cfg!(test) || iv == 0 || tf_str(iv).is_none() {
        return;
    }
    if let Ok(mut g) = seen().lock() {
        g.insert(chart.to_string(), Seen { symbol: symbol_key.to_string(), iv, range, at: Instant::now() });
    }
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        super::spawn_named("ws-regime-auto", run);
    });
}

/// 这张图现在对应的进度键（实时 / 历史段）。
fn key_for(chart: &str, symbol_key: &str, chart_iv: u64) -> Option<String> {
    let (tf, _) = target(chart, chart_iv)?;
    let range = super::regime::range_of(chart);
    let hist = is_hist(range, chart_iv, now_ms()).then_some(range).flatten();
    Some(job_key(&code_of(symbol_key), tf, hist))
}

/// 「现在算」：不管开关、不管是不是最新，下一轮就算这张图。
pub fn force(chart: &str, symbol_key: &str, iv: u64) {
    let Some(k) = key_for(chart, symbol_key, iv) else { return };
    if let Ok(mut g) = jobs().lock()
        && matches!(g.get(&k), Some(Job::Failed { .. }))
    {
        g.remove(&k);
    }
    if let Ok(mut g) = forced().lock() {
        g.insert(k);
    }
    note_chart(chart, symbol_key, iv, super::regime::range_of(chart));
}

#[must_use]
pub fn job(chart: &str, symbol_key: &str, iv: u64) -> Option<Job> {
    let k = key_for(chart, symbol_key, iv)?;
    jobs().lock().ok()?.get(&k).cloned()
}

/// 这张图要用的那个周期、盖得住它加载的那段的时间线（实时图：币安那份；历史图：任何来源、盖住 ≥ 90%）。
#[must_use]
pub fn exact_file(chart: &str, symbol_key: &str, iv: u64) -> Option<Arc<RegimeFile>> {
    let (tf, _) = target(chart, iv)?;
    let range = super::regime::range_of(chart);
    let files = super::regime::files_for(symbol_key);
    if is_hist(range, iv, now_ms()) {
        files
            .into_iter()
            .filter(|f| f.timeframe == tf && super::regime::coverage(f, range) >= HIST_COVER)
            .max_by(|a, b| super::regime::coverage(a, range).total_cmp(&super::regime::coverage(b, range)))
    } else {
        files.into_iter().find(|f| f.timeframe == tf && f.source == "binance" && !f.symbol.contains(':'))
    }
}

/// 现在要套的用户参数（币安 / 图上的加密 K 线 = 加密市场）。
#[must_use]
pub fn want_params(code: &str, tf: &str) -> BTreeMap<String, f64> {
    let f = super::regime_params::load();
    super::regime_params::resolve(&f, "crypto", code, tf)
}

/// 文件的状态：（过期了吗, 参数改了没重算吗）。历史数据算的那份（`chart`）不会过期。
#[must_use]
pub fn file_state(f: &RegimeFile, now_ms: u64) -> (bool, bool) {
    let stale = f.source != "chart" && f.last_ms.is_none_or(|l| now_ms >= l + f.tf_ms + CLOSE_DELAY_MS);
    let code = f.symbol.rsplit(':').next().unwrap_or(&f.symbol);
    let want = super::regime_params::resolve(&super::regime_params::load(), super::regime_params::market_of(&f.source), code, &f.timeframe);
    (stale, !super::regime_params::same(&want, &f.user_params))
}

/// 该不该算、为什么（纯判断）。`file`：(last_ms, tf_ms, 文件里记的用户参数)；`hist`：历史图（文件不会过期）。
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn due(file: Option<(Option<u64>, u64, &BTreeMap<String, f64>)>, want: &BTreeMap<String, f64>, job: Option<&Job>, forced: bool, auto: bool, hist: bool, now_ms: u64, now: Instant) -> Option<String> {
    match job {
        Some(Job::Running { .. }) => return None,
        Some(Job::Failed { at, .. }) if !forced && now.duration_since(*at) < ERR_BACKOFF => return None,
        Some(Job::Done { at, .. }) if !forced && now.duration_since(*at) < MIN_GAP => return None,
        _ => {}
    }
    if forced {
        return Some("手动".into());
    }
    if !auto {
        return None;
    }
    let Some((last, tf_ms, have)) = file else {
        return Some(if hist { "图上这段还没有状态".into() } else { "还没有这个周期的状态".into() });
    };
    if !super::regime_params::same(want, have) {
        return Some("参数改了".into());
    }
    if hist {
        return None;
    }
    match last {
        None => Some("老文件".into()),
        Some(l) if now_ms >= l + tf_ms + CLOSE_DELAY_MS => Some("有新 K 线收盘".into()),
        Some(_) => None,
    }
}

// ── 历史图的 K 线：图写 CSV ────────────────────────────────────────────

/// 图每帧问：后台要不要我的 K 线？要的话给（写到哪, 并成多大的周期）。取走就清掉（只写一次）。
#[must_use]
pub fn export_request(chart: &str) -> Option<(PathBuf, u64)> {
    export_req().lock().ok()?.remove(chart)
}

/// 图写好 CSV 了（在图的后台线程里调）。`range`：写的那段（第一根开盘, 最后一根收盘）。
pub fn export_ready(chart: &str, path: PathBuf, range: (u64, u64)) {
    if let Ok(mut g) = exported().lock() {
        g.insert(chart.to_string(), (path, range));
    }
}

/// 写 CSV（`ts,open,high,low,close,volume`，ts = 开盘毫秒）。
pub fn write_bars(path: &std::path::Path, bars: &[super::studies::Ohlcv]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("csv.tmp");
    let mut f = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
    writeln!(f, "ts,open,high,low,close,volume")?;
    for b in bars {
        writeln!(f, "{},{},{},{},{},{}", b.0, b.1, b.2, b.3, b.4, b.5)?;
    }
    f.flush()?;
    drop(f);
    std::fs::rename(&tmp, path)
}

// ── 后台线程 ──────────────────────────────────────────────────────────

/// 两条道各一次只跑一个：实时（要联网，可能卡）与历史（不联网），互不挡。
static LANE_BUSY: [std::sync::atomic::AtomicBool; 2] = [std::sync::atomic::AtomicBool::new(false), std::sync::atomic::AtomicBool::new(false)];

fn lane_busy(hist: bool) -> bool {
    LANE_BUSY[usize::from(hist)].load(std::sync::atomic::Ordering::Relaxed)
}

/// 在这条道上另起线程算（道占着就不起）。
fn spawn_compute(hist: bool, key: String, code: String, tf: &'static str, iv: u64, bars: Option<PathBuf>, why: String) {
    use std::sync::atomic::Ordering;
    if LANE_BUSY[usize::from(hist)].swap(true, Ordering::AcqRel) {
        return;
    }
    // 先记「在跑」，免得下一轮又排进来
    if let Ok(mut g) = jobs().lock() {
        g.insert(key.clone(), Job::Running { since: Instant::now(), why: why.clone() });
    }
    super::spawn_named(if hist { "ws-regime-hist" } else { "ws-regime-live" }, move || {
        compute(&key, &code, tf, iv, bars.as_deref(), why, hist);
        LANE_BUSY[usize::from(hist)].store(false, Ordering::Release);
    });
}

fn run() {
    loop {
        std::thread::sleep(Duration::from_secs(2));
        let auto = super::regime::settings().auto;
        let now = Instant::now();
        let now_ms = now_ms();
        let mut look: Vec<(String, Seen)> = {
            let mut g = seen().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            g.retain(|_, s| now.duration_since(s.at) < SEEN_TTL);
            g.iter().map(|(c, s)| (c.clone(), s.clone())).collect()
        };
        look.sort_by_key(|(_, s)| std::cmp::Reverse(s.at));
        for (chart, s) in look {
            if !super::regime::view(&chart).on && !forced_has(&chart, &s) {
                continue;
            }
            let Some((tf, iv)) = target(&chart, s.iv) else { continue };
            let code = code_of(&s.symbol);
            let hist = is_hist(s.range, s.iv, now_ms);
            if lane_busy(hist) {
                continue;
            }
            // 历史图只能把图上的 K 线并成更大的周期，指定了更小 / 不整除的周期就算不了
            if hist && (iv < s.iv || iv % s.iv != 0) {
                continue;
            }
            let key = job_key(&code, tf, hist.then_some(s.range).flatten());
            let is_forced = forced().lock().is_ok_and(|g| g.contains(&key));
            let want = want_params(&code, tf);
            let file = exact_file(&chart, &s.symbol, s.iv);
            let j = jobs().lock().ok().and_then(|g| g.get(&key).cloned());
            let Some(why) = due(file.as_ref().map(|f| (f.last_ms, f.tf_ms, &f.user_params)), &want, j.as_ref(), is_forced, auto, hist, now_ms, now) else {
                continue;
            };
            if hist {
                // 先要图上的 K 线：已写好的就用，没有就请图写，下一轮再来
                let ready = exported().lock().ok().and_then(|mut g| g.remove(&chart));
                match ready {
                    Some((path, _)) => {
                        if let Ok(mut g) = forced().lock() {
                            g.remove(&key);
                        }
                        spawn_compute(true, key, code, tf, iv, Some(path), why);
                    }
                    None => {
                        let path = super::paths::runtime_dir().join(format!("regime-bars-{}.csv", sanitize(&chart)));
                        if let Ok(mut g) = export_req().lock() {
                            g.insert(chart.clone(), (path, iv));
                        }
                    }
                }
            } else {
                if let Ok(mut g) = forced().lock() {
                    g.remove(&key);
                }
                spawn_compute(false, key, code, tf, iv, None, why);
            }
        }
    }
}

fn forced_has(chart: &str, s: &Seen) -> bool {
    let Some((tf, _)) = target(chart, s.iv) else { return false };
    let hist = is_hist(s.range, s.iv, now_ms());
    let key = job_key(&code_of(&s.symbol), tf, hist.then_some(s.range).flatten());
    forced().lock().is_ok_and(|g| g.contains(&key))
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect()
}

fn compute(key: &str, code: &str, tf: &str, iv: u64, bars: Option<&std::path::Path>, why: String, hist: bool) {
    log::info!("[regime-auto] {key}：{why} → 重算");
    // 两条道各写各的日志；带进程号：隔离实例与正式实例共用运行目录也不串
    let log_path = super::paths::runtime_dir().join(format!("regime-auto-{}-{}.log", if hist { "hist" } else { "live" }, std::process::id()));
    let t0 = Instant::now();
    let days = days_for(iv).to_string();
    let bars_s = bars.map(|p| p.display().to_string());
    let mut args = vec!["-m", "factory.regime", "run", code, "--tf", tf];
    match bars_s.as_deref() {
        Some(p) => args.extend(["--bars", p]),
        None => args.extend(["--days", days.as_str()]),
    }
    let child = std::fs::File::create(&log_path).and_then(|f| {
        let e = f.try_clone()?;
        std::process::Command::new(super::paths::python())
            .args(&args)
            .env("PYTHONUNBUFFERED", "1")
            .current_dir(super::paths::repo_root())
            .stdin(std::process::Stdio::null())
            .stdout(f)
            .stderr(e)
            .spawn()
    });
    let st = child.and_then(|mut c| loop {
        if let Some(s) = c.try_wait()? {
            break Ok(Some(s));
        }
        if t0.elapsed() >= HARD_TIMEOUT {
            let _ = c.kill();
            let _ = c.wait();
            break Ok(None);
        }
        std::thread::sleep(Duration::from_millis(200));
    });
    let last_line = || {
        let tail = std::fs::read_to_string(&log_path).unwrap_or_default();
        tail.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string()
    };
    let job = match st {
        Ok(Some(s)) if s.success() => Job::Done { at: Instant::now(), secs: t0.elapsed().as_secs_f32() },
        Ok(Some(s)) => Job::Failed { at: Instant::now(), msg: format!("退出码 {:?}：{}", s.code(), last_line()) },
        Ok(None) => Job::Failed { at: Instant::now(), msg: format!("{} 秒没算完（多半是网络卡住），停掉了；最后一行：{}", HARD_TIMEOUT.as_secs(), last_line()) },
        Err(e) => Job::Failed { at: Instant::now(), msg: format!("起不来 Python：{e}") },
    };
    match &job {
        Job::Done { secs, .. } => log::info!("[regime-auto] {key} 算完（{secs:.1} 秒）"),
        Job::Failed { msg, .. } => log::warn!("[regime-auto] {key} 失败：{msg}"),
        Job::Running { .. } => {}
    }
    if let Ok(mut g) = jobs().lock() {
        g.insert(key.to_string(), job);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 周期与天数() {
        assert_eq!(tf_str(300_000), Some("5m"));
        assert_eq!(tf_str(86_400_000), Some("1d"));
        assert_eq!(tf_str(1_000), None, "秒级 / 按笔的图不算");
        assert_eq!((days_for(60_000), days_for(300_000), days_for(900_000), days_for(3_600_000), days_for(86_400_000)), (14, 45, 365, 730, 1460));
        assert_eq!(code_of("Binance Linear:btcusdt"), "BTCUSDT");
        assert_eq!(sanitize("a:b c"), "a_b_c");
    }

    #[test]
    fn 历史图与实时图() {
        let m = 60_000;
        let now = 1_000_000_000;
        assert!(!is_hist(Some((0, now + m)), m, now), "最后一根还没收盘：实时");
        assert!(!is_hist(Some((0, now - 2 * m)), m, now), "晚两根：网络慢一点，仍算实时");
        assert!(is_hist(Some((0, now - 10 * m)), m, now));
        assert!(!is_hist(None, m, now));
        assert_eq!(job_key("BTCUSDT", "1m", Some((1, 2))), "BTCUSDT 1m 历史 1-2");
        assert_eq!(job_key("BTCUSDT", "1m", None), "BTCUSDT 1m");
    }

    #[test]
    fn 该不该算() {
        let now = Instant::now();
        let none = BTreeMap::new();
        let mut p = BTreeMap::new();
        p.insert("swing_k".to_string(), 1.2);
        let tf = 900_000;
        let l = 1_000_000_000;
        let f = |last: Option<u64>, have| Some((last, tf, have));
        // 没文件 → 算
        assert!(due(None, &none, None, false, true, false, l, now).is_some());
        // 最新的 → 不算；下一根收盘 + 20 秒 → 算
        assert!(due(f(Some(l), &none), &none, None, false, true, false, l + tf + 19_999, now).is_none());
        assert_eq!(due(f(Some(l), &none), &none, None, false, true, false, l + tf + 20_000, now).as_deref(), Some("有新 K 线收盘"));
        // 历史图：有就不再算（历史不会变），参数改了照样重算
        assert!(due(f(Some(l), &none), &none, None, false, true, true, l + 100 * tf, now).is_none());
        assert_eq!(due(f(Some(l), &none), &p, None, false, true, true, l, now).as_deref(), Some("参数改了"));
        assert_eq!(due(None, &none, None, false, true, true, l, now).as_deref(), Some("图上这段还没有状态"));
        // 参数改了 → 算
        assert_eq!(due(f(Some(l), &none), &p, None, false, true, false, l, now).as_deref(), Some("参数改了"));
        // 自动关着：只有手动才算
        assert!(due(None, &none, None, false, false, false, l, now).is_none());
        assert_eq!(due(f(Some(l), &none), &none, None, true, false, false, l, now).as_deref(), Some("手动"));
        // 在跑 / 刚算完 / 刚出错：不算
        let run = Job::Running { since: now, why: String::new() };
        assert!(due(None, &none, Some(&run), true, true, false, l, now).is_none());
        let done = Job::Done { at: now, secs: 1.0 };
        assert!(due(None, &none, Some(&done), false, true, false, l, now).is_none());
        assert!(due(None, &none, Some(&done), false, true, false, l, now + MIN_GAP).is_some());
        let bad = Job::Failed { at: now, msg: "x".into() };
        assert!(due(None, &none, Some(&bad), false, true, false, l, now + Duration::from_secs(60)).is_none());
        assert!(due(None, &none, Some(&bad), true, true, false, l, now).is_some(), "「现在算」不等出错的冷却");
    }

    #[test]
    fn 写_k线_csv() {
        let p = std::env::temp_dir().join(format!("ws-regime-bars-{}.csv", std::process::id()));
        write_bars(&p, &[(60_000, 1.0, 2.0, 0.5, 1.5, 10.0), (120_000, 1.5, 1.6, 1.4, 1.45, 3.0)]).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "ts,open,high,low,close,volume\n60000,1,2,0.5,1.5,10\n120000,1.5,1.6,1.4,1.45,3\n");
        let _ = std::fs::remove_file(&p);
    }
}
