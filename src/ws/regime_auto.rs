//! 市场状态跟随图表自动算（docs/69 B）：K 线图每帧报到「我在看 品种 × 周期」，后台线程 `ws-regime-auto`
//! 看到有图在看、而这个周期的状态没有 / 有新 K 线收盘了 / 用户参数改了，就跑一次
//! `python -m factory.regime run <代码> --tf <图的周期>`。
//!
//! - 只算打开着的图（最近 15 秒报到过），一次只跑一个；子进程留在 Cockpit 的 cgroup 里，关窗即停。
//!   这是「耗资源任务人工触发」的例外（用户 10-30 同意）：量小（增量一两秒），只为正在看的图。
//! - 数据来源仍是币安 U 本位永续 K 线（与 `factory.regime` 原来一致）；拉不到的代码记下原因，10 分钟内不再试；一次超过 3 分钟没算完（网络卡住）就停掉、记失败。
//! - 小于 15 分钟的周期只算 L2（`--l2-only`）：L1 拥挤度分位要此前半年的 K 线，1 分钟就是 26 万根。
//! - 守护 `ws-regime` 照旧不 enable；它与这里写的是同一份文件。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 报到后多久内算「在看」。
const SEEN_TTL: Duration = Duration::from_secs(15);
/// 收盘后等多久再取（币安收盘后几秒 K 线才可取）。
const CLOSE_DELAY_MS: u64 = 20_000;
/// 刚算完至少隔这么久才再算同一个（币安还没出新 K 线时不空转）。
const MIN_GAP: Duration = Duration::from_secs(30);
/// 出错后隔多久再试。
const ERR_BACKOFF: Duration = Duration::from_secs(10 * 60);
/// 一次最多算多久：网络卡住（下载停在半路、迟迟不超时）时杀掉，别把后面的图都堵住。
const HARD_TIMEOUT: Duration = Duration::from_secs(180);

/// 图的周期（毫秒）→ 币安周期字符串。
#[must_use]
pub fn tf_str(iv: u64) -> Option<&'static str> {
    TFS.iter().find(|(ms, _)| *ms == iv).map(|(_, s)| *s)
}

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

/// 叠加层实际用的周期：检查器里指定了就是它，否则跟图（docs/69 B：算的就是图上画的那份）。
#[must_use]
pub fn target(chart_iv: u64) -> Option<(&'static str, u64)> {
    if let Some(w) = super::regime::settings().tf
        && let Some((ms, s)) = TFS.iter().find(|(_, s)| *s == w)
    {
        return Some((s, *ms));
    }
    tf_str(chart_iv).map(|s| (s, chart_iv))
}

/// 算多少天（另加预热）。
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

#[must_use]
pub fn l2_only(iv: u64) -> bool {
    iv < 900_000
}

/// 一个（代码, 周期）的进度。
#[derive(Debug, Clone)]
pub enum Job {
    Running { since: Instant, why: String },
    Done { at: Instant, secs: f32 },
    Failed { at: Instant, msg: String },
}

type Key = (String, String);

fn seen() -> &'static Mutex<HashMap<(String, u64), Instant>> {
    static S: OnceLock<Mutex<HashMap<(String, u64), Instant>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

fn jobs() -> &'static Mutex<HashMap<Key, Job>> {
    static J: OnceLock<Mutex<HashMap<Key, Job>>> = OnceLock::new();
    J.get_or_init(Default::default)
}

fn forced() -> &'static Mutex<HashSet<Key>> {
    static F: OnceLock<Mutex<HashSet<Key>>> = OnceLock::new();
    F.get_or_init(Default::default)
}

fn code_of(symbol_key: &str) -> String {
    symbol_key.rsplit(':').next().unwrap_or(symbol_key).to_uppercase()
}

/// K 线图每帧调：我在看这个品种这个周期（时间基的图才调）。
pub fn note_chart(symbol_key: &str, iv: u64) {
    if cfg!(test) || iv == 0 || tf_str(iv).is_none() {
        return;
    }
    if let Ok(mut g) = seen().lock() {
        g.insert((symbol_key.to_string(), iv), Instant::now());
    }
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        super::spawn_named("ws-regime-auto", run);
    });
}

/// 「现在算」：不管开关、不管是不是最新，下一轮就算这张图的周期。
pub fn force(symbol_key: &str, iv: u64) {
    let Some((tf, _)) = target(iv) else { return };
    if let Ok(mut g) = forced().lock() {
        g.insert((code_of(symbol_key), tf.to_string()));
    }
    if let Ok(mut g) = jobs().lock() {
        // 出过错的也重试
        let k = (code_of(symbol_key), tf.to_string());
        if matches!(g.get(&k), Some(Job::Failed { .. })) {
            g.remove(&k);
        }
    }
    // 没报到过的图（检查器先开）也让线程起来
    note_chart(symbol_key, iv);
}

#[must_use]
pub fn job(symbol_key: &str, iv: u64) -> Option<Job> {
    let (tf, _) = target(iv)?;
    jobs().lock().ok()?.get(&(code_of(symbol_key), tf.to_string())).cloned()
}

/// 这张图要用的那个周期的币安时间线（自动算写的那份）。
#[must_use]
pub fn exact_file(symbol_key: &str, iv: u64) -> Option<std::sync::Arc<super::regime::RegimeFile>> {
    let (tf, _) = target(iv)?;
    super::regime::files_for(symbol_key).into_iter().find(|f| f.timeframe == tf && f.source == "binance" && !f.symbol.contains(':'))
}

/// 现在要套的用户参数（币安来源 = 加密市场）。
#[must_use]
pub fn want_params(code: &str, tf: &str) -> BTreeMap<String, f64> {
    let f = super::regime_params::load();
    super::regime_params::resolve(&f, "crypto", code, tf)
}

/// 文件的状态：（过期了吗, 参数改了没重算吗）。
#[must_use]
pub fn file_state(f: &super::regime::RegimeFile, now_ms: u64) -> (bool, bool) {
    let stale = f.last_ms.is_none_or(|l| now_ms >= l + f.tf_ms + CLOSE_DELAY_MS);
    let code = f.symbol.rsplit(':').next().unwrap_or(&f.symbol);
    let want = super::regime_params::resolve(&super::regime_params::load(), super::regime_params::market_of(&f.source), code, &f.timeframe);
    (stale, !super::regime_params::same(&want, &f.user_params))
}

/// 该不该算、为什么（纯判断）。`file`：(last_ms, tf_ms, 文件里记的用户参数)。
#[must_use]
pub fn due(file: Option<(Option<u64>, u64, &BTreeMap<String, f64>)>, want: &BTreeMap<String, f64>, job: Option<&Job>, forced: bool, auto: bool, now_ms: u64, now: Instant) -> Option<String> {
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
    let Some((last, tf_ms, have)) = file else { return Some("还没有这个周期的状态".into()) };
    if !super::regime_params::same(want, have) {
        return Some("参数改了".into());
    }
    match last {
        None => Some("老文件".into()),
        Some(l) if now_ms >= l + tf_ms + CLOSE_DELAY_MS => Some("有新 K 线收盘".into()),
        Some(_) => None,
    }
}

fn run() {
    loop {
        std::thread::sleep(Duration::from_secs(2));
        let auto = super::regime::settings().auto;
        let now = Instant::now();
        let now_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
        let mut look: Vec<(String, u64, Instant)> = {
            let mut g = seen().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            g.retain(|_, t| now.duration_since(*t) < SEEN_TTL);
            g.iter().map(|((s, iv), t)| (s.clone(), *iv, *t)).collect()
        };
        look.sort_by_key(|(_, _, t)| std::cmp::Reverse(*t));
        for (sym, chart_iv, _) in look {
            let Some((tf, iv)) = target(chart_iv) else { continue };
            let code = code_of(&sym);
            let key = (code.clone(), tf.to_string());
            let is_forced = forced().lock().is_ok_and(|g| g.contains(&key));
            let want = want_params(&code, tf);
            let file = exact_file(&sym, chart_iv);
            let j = jobs().lock().ok().and_then(|g| g.get(&key).cloned());
            let why = due(file.as_ref().map(|f| (f.last_ms, f.tf_ms, &f.user_params)), &want, j.as_ref(), is_forced, auto, now_ms, now);
            if let Some(why) = why {
                if let Ok(mut g) = forced().lock() {
                    g.remove(&key);
                }
                compute(&key, iv, why);
                break;
            }
        }
    }
}

fn compute(key: &Key, iv: u64, why: String) {
    let (code, tf) = key;
    if let Ok(mut g) = jobs().lock() {
        g.insert(key.clone(), Job::Running { since: Instant::now(), why: why.clone() });
    }
    log::info!("[regime-auto] {code} {tf}：{why} → 重算");
    let log_path = super::paths::runtime_dir().join("regime-auto.log");
    let t0 = Instant::now();
    let days = days_for(iv).to_string();
    let mut args = vec!["-m", "factory.regime", "run", code.as_str(), "--tf", tf.as_str(), "--days", days.as_str()];
    if l2_only(iv) {
        args.push("--l2-only");
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
        Job::Done { secs, .. } => log::info!("[regime-auto] {code} {tf} 算完（{secs:.1} 秒）"),
        Job::Failed { msg, .. } => log::warn!("[regime-auto] {code} {tf} 失败：{msg}"),
        Job::Running { .. } => {}
    }
    if let Ok(mut g) = jobs().lock() {
        g.insert(key.clone(), job);
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
        assert!(l2_only(300_000) && !l2_only(900_000));
        assert_eq!(code_of("Binance Linear:btcusdt"), "BTCUSDT");
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
        assert!(due(None, &none, None, false, true, l, now).is_some());
        // 最新的 → 不算；下一根收盘 + 20 秒 → 算
        assert!(due(f(Some(l), &none), &none, None, false, true, l + tf + 19_999, now).is_none());
        assert_eq!(due(f(Some(l), &none), &none, None, false, true, l + tf + 20_000, now).as_deref(), Some("有新 K 线收盘"));
        // 参数改了 → 算
        assert_eq!(due(f(Some(l), &none), &p, None, false, true, l, now).as_deref(), Some("参数改了"));
        // 自动关着：只有手动才算
        assert!(due(None, &none, None, false, false, l, now).is_none());
        assert_eq!(due(f(Some(l), &none), &none, None, true, false, l, now).as_deref(), Some("手动"));
        // 在跑 / 刚算完 / 刚出错：不算
        let run = Job::Running { since: now, why: String::new() };
        assert!(due(None, &none, Some(&run), true, true, l, now).is_none());
        let done = Job::Done { at: now, secs: 1.0 };
        assert!(due(None, &none, Some(&done), false, true, l, now).is_none());
        assert!(due(None, &none, Some(&done), false, true, l, now + MIN_GAP).is_some());
        let bad = Job::Failed { at: now, msg: "x".into() };
        assert!(due(None, &none, Some(&bad), false, true, l, now + Duration::from_secs(60)).is_none());
        assert!(due(None, &none, Some(&bad), true, true, l, now).is_some(), "「现在算」不等出错的冷却");
    }
}
