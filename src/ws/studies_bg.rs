//! 后台指标数据（docs/63 §9.5 续 + §10）：图自己没有的数据都从这里来——
//!
//! 1. **指标提醒**：挂了提醒的（品种, 周期）没有一张图在算它时，这里定时取 K 线、照常算、照常判
//!    （同一个 `studies::compute`，提醒的「看到过哪根」也是同一份状态，图和后台交接不重报、不漏报）；
//! 2. **跨品种指标**：BTC 的图上挂「ETHUSDT 的 RSI」——ETH 的 K 线从这里来（只用已收盘的，对齐不前视）；
//!    对照序列（相关 / 价差的另一个品种）同理；
//! 3. **K 线之外的序列**（§10 J3）：持仓、资金费、溢价指数、标记价、多空比、主动买卖比——指标在元数据里声明要哪些，
//!    图算的时候登记需要，这里去取；每个点带「能被知道的时刻」，图按它对齐（不前视）。
//!
//! 都走交易所 REST（`AdapterHandles::fetch_klines` / `fetch_open_interest` / `fetch_derivs`）：K 线第一次取最近 400 根，
//! 之后每根收盘后 15 秒（交易所那根定稿了）增量取一次；盘中提醒另外每 15 秒取一次。出口总闸关着时一律不取。
//! 「有图在算」= 最近 30 秒内图对这个（品种, 周期）调过 `compute`——图不显示时不画、不算，后台就接手。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use super::studies::Ohlcv;

/// 图算过之后多久内算「有图在管」。图在显示时每次收到行情都会重画，远小于这个间隔。
const CHART_FRESH_MS: u64 = 30_000;
/// 跨品种 / 序列的需求多久没人要就不再取。
const WANT_FRESH_MS: u64 = 120_000;
/// 收盘后等几秒再取（交易所那根要定稿）。实测（2026-10-27 币安永续 BTC 1m）：收盘后 2 秒取到的那根
/// 收盘价、成交量都还会变（80134.8 → 80148.0），10 秒时已定稿——留足余量取 15 秒。没定稿的根被当成已收盘，
/// 收盘确认的提醒可能按一个不存在的收盘价响，下一次取到定稿又触发「数据修正」重算
const AFTER_CLOSE_MS: u64 = 15_000;
/// 盘中提醒的轮询间隔。
const INTRABAR_EVERY_MS: u64 = 15_000;
const RETRY_MS: u64 = 30_000;
/// 一路最多留这么多根 / 点（超了砍到一半：砍会让指标整段重建，所以别常砍）。
const MAX_BARS: usize = 3_000;

/// 「K 线」这一种之外的序列（= `Bar` 上同名的字段，见指标核心 `ind::EXTRA_FIELDS`）。
pub const SERIES: [&str; 6] = ["oi", "funding", "premium", "mark", "ls_ratio", "taker_ratio"];
pub const BARS: &str = "bars";

/// 一路数据的键：（品种, 种类：`bars` 或 [`SERIES`] 之一, 周期）。
type Key = (String, &'static str, u64);

#[derive(Debug, Default)]
struct Feed {
    bars: Vec<Ohlcv>,
    /// 与 `bars` 一一对应的主动买入量（交易所 K 线给的；没有为 NaN）
    buy: Vec<f64>,
    /// 序列：（能被知道的时刻, 值），按时刻升序
    pts: Vec<(u64, f64)>,
    next_at: u64,
    inflight: bool,
    err: Option<String>,
    /// 这批数据是什么时候**发出请求**的：哪根算已收盘、哪个点算已定稿按它判，不按回来的时刻——
    /// 09:23:59 发出、09:24:00 回来的那批里，09:23 那根还是没收盘时的样子
    asked_at: u64,
}

#[derive(Default)]
struct Bg {
    feeds: HashMap<Key, Feed>,
    chart_seen: HashMap<(String, u64), u64>,
    wanted: HashMap<Key, u64>,
    last_scan: u64,
}

fn bg() -> &'static Mutex<Bg> {
    static B: OnceLock<Mutex<Bg>> = OnceLock::new();
    B.get_or_init(Default::default)
}

fn lock() -> std::sync::MutexGuard<'static, Bg> {
    bg().lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn kind_of(k: &str) -> Option<&'static str> {
    if k == BARS {
        return Some(BARS);
    }
    SERIES.iter().copied().find(|s| *s == k)
}

/// 图算过这个（品种, 周期）——提醒交给图判，后台不重复。
pub fn mark_chart(symbol: &str, tf: u64, now_ms: u64) {
    lock().chart_seen.insert((symbol.to_string(), tf), now_ms);
}

/// 这根在 `asked_at` 发出的那次请求里是不是已经定稿（收盘超过 [`AFTER_CLOSE_MS`]）。
fn settled(k: &Ohlcv, tf: u64, asked_at: u64) -> bool {
    k.0 + tf + AFTER_CLOSE_MS <= asked_at
}

fn chart_covers(b: &Bg, key: &(String, u64), now_ms: u64) -> bool {
    b.chart_seen.get(key).is_some_and(|t| now_ms.saturating_sub(*t) < CHART_FRESH_MS)
}

fn unavailable(symbol: &str, what: &str, f: Option<&Feed>) -> String {
    match f {
        Some(Feed { err: Some(e), .. }) => format!("{} 的{what}取不到：{e}", code(symbol)),
        _ if !super::egress::streams_enabled() => format!("对外连接关着（网络出口 → Cockpit 行情图），取不了 {} 的{what}", code(symbol)),
        _ => format!("在取 {} 的{what}…", code(symbol)),
    }
}

/// 跨品种指标 / 对照序列要这一路的 K 线：返回**已收盘、已定稿**的（K 线, 主动买量），还没取到返回 `Err(说明)`。
pub fn closed_klines(symbol: &str, tf: u64, now_ms: u64) -> Result<(Vec<Ohlcv>, Vec<f64>), String> {
    let mut b = lock();
    let key = (symbol.to_string(), BARS, tf);
    b.wanted.insert(key.clone(), now_ms);
    match b.feeds.get(&key) {
        Some(f) if !f.bars.is_empty() => {
            let n = f.bars.iter().take_while(|k| settled(k, tf, f.asked_at)).count();
            Ok((f.bars[..n].to_vec(), f.buy[..n].to_vec()))
        }
        f => Err(unavailable(symbol, " K 线", f)),
    }
}

/// K 线之外的一条序列（[`SERIES`]）：返回已定稿的（能被知道的时刻, 值），还没取到返回 `Err(说明)`。
/// `tf` 是要它的那组指标的计算周期（溢价 / 标记价按这个周期取 K 线；统计类取不大于它的最近一档）。
pub fn series(symbol: &str, kind: &str, tf: u64, now_ms: u64) -> Result<Vec<(u64, f64)>, String> {
    let Some(kind) = kind_of(kind).filter(|k| *k != BARS) else { return Err(format!("没有序列 {kind}")) };
    let mut b = lock();
    let key = (symbol.to_string(), kind, tf);
    b.wanted.insert(key.clone(), now_ms);
    let what = match kind {
        "oi" => "持仓",
        "funding" => "资金费",
        "premium" => "溢价指数",
        "mark" => "标记价",
        "ls_ratio" => "多空比",
        _ => "主动买卖比",
    };
    match b.feeds.get(&key) {
        Some(f) if !f.pts.is_empty() || (f.err.is_none() && f.asked_at > 0 && !f.inflight) => {
            Ok(f.pts.iter().copied().filter(|p| p.0 + AFTER_CLOSE_MS <= f.asked_at).collect())
        }
        f => Err(unavailable(symbol, what, f)),
    }
}

fn code(symbol: &str) -> &str {
    symbol.rsplit(':').next().unwrap_or(symbol)
}

/// 一次要取的：（品种, 种类, 周期, 从哪起；None = 取最近一段）。
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub symbol: String,
    pub kind: &'static str,
    pub tf: u64,
    pub since: Option<u64>,
}

/// 现在该取哪几路（每秒最多扫一次）。`alerts`：挂着提醒的（品种, 周期, 盘中?）。
pub fn due(now_ms: u64, alerts: &[(String, u64, bool)]) -> Vec<Job> {
    let mut b = lock();
    if now_ms.saturating_sub(b.last_scan) < 1_000 {
        return Vec::new();
    }
    b.last_scan = now_ms;
    // 要的：没有图在管的提醒（K 线）+ 最近有人要的（跨品种 K 线、序列）
    let mut need: HashMap<Key, bool> = HashMap::new();
    for (s, tf, intrabar) in alerts {
        if *tf > 0 && !chart_covers(&b, &(s.clone(), *tf), now_ms) {
            *need.entry((s.clone(), BARS, *tf)).or_default() |= *intrabar;
        }
    }
    b.wanted.retain(|_, t| now_ms.saturating_sub(*t) < WANT_FRESH_MS);
    for key in b.wanted.keys() {
        need.entry(key.clone()).or_default();
    }
    // 没人要了的不留（下次要从头取，免得用一段过时的历史）
    b.feeds.retain(|k, _| need.contains_key(k));
    let mut jobs = Vec::new();
    for (key, intrabar) in need {
        let f = b.feeds.entry(key.clone()).or_default();
        if f.inflight || now_ms < f.next_at {
            continue;
        }
        f.inflight = true;
        let (tf, asked) = (key.2, f.asked_at);
        // 增量：K 线从上次取时已定稿的最后一根起（它和之后的一起换掉）；序列从最后一个点往前留一个周期的余量
        let since = if key.1 == BARS {
            f.bars.iter().rev().find(|k| settled(k, tf, asked)).map(|k| k.0)
        } else {
            f.pts.last().map(|p| p.0.saturating_sub(2 * tf.max(300_000)))
        };
        f.asked_at = now_ms;
        jobs.push(Job { symbol: key.0.clone(), kind: key.1, tf, since });
        // 下一次：下一根收盘后；盘中提醒再加一道 15 秒。序列至少隔 5 分钟（统计类最细 5 分钟一档）
        let step = if key.1 == BARS { tf } else { tf.max(300_000) };
        let next_close = (now_ms / step + 1) * step + AFTER_CLOSE_MS;
        f.next_at = if intrabar { next_close.min(now_ms + INTRABAR_EVERY_MS) } else { next_close };
    }
    jobs
}

fn failed(symbol: &str, kind: &str, tf: u64, f: &mut Feed, e: String, now_ms: u64) {
    log::warn!("[指标后台] {symbol} {kind} {tf}ms 取不到：{e}");
    f.err = Some(e);
    f.next_at = now_ms + RETRY_MS;
}

/// K 线取回来了：并进这一路，返回给提醒用的（K 线, 当作「现在」的时刻）：已定稿的全部 + 紧跟的一根当暂定
/// （它可能还在走、也可能刚收盘还没定稿——都只给盘中提醒用，收盘确认要等它定稿）。
/// `got` 里每根带主动买入量（没有为 NaN）。出错记下、30 秒后重试。
pub fn ingest(symbol: &str, tf: u64, got: Result<Vec<(Ohlcv, f64)>, String>, now_ms: u64) -> Option<(Vec<Ohlcv>, Vec<f64>, u64)> {
    let mut b = lock();
    let f = b.feeds.get_mut(&(symbol.to_string(), BARS, tf))?;
    f.inflight = false;
    match got {
        Err(e) => {
            failed(symbol, BARS, tf, f, e, now_ms);
            None
        }
        Ok(mut new) => {
            f.err = None;
            new.sort_by_key(|k| k.0 .0);
            new.dedup_by_key(|k| k.0 .0);
            merge(&mut f.bars, &mut f.buy, new);
            if f.bars.len() > MAX_BARS {
                let cut = f.bars.len() - MAX_BARS / 2;
                f.bars.drain(..cut);
                f.buy.drain(..cut);
            }
            let n = f.bars.iter().take_while(|k| settled(k, tf, f.asked_at)).count();
            let m = (n + 1).min(f.bars.len());
            let out: Vec<Ohlcv> = f.bars[..m].to_vec();
            // 「现在」取在最后一根已定稿的收盘之后、暂定那根收盘之前：`compute` 据此把最后一根当暂定
            let as_now = out.get(n).map_or(f.asked_at, |k| k.0 + tf - 1).max(out.get(n.wrapping_sub(1)).map_or(0, |k| k.0 + tf));
            Some((out, f.buy[..m].to_vec(), as_now))
        }
    }
}

/// 序列取回来了：按「能被知道的时刻」并进去（新的一段盖掉旧的同一段）。返回有没有人在等它（要让图重画）。
pub fn ingest_series(symbol: &str, kind: &str, tf: u64, got: Result<Vec<(u64, f64)>, String>, now_ms: u64) -> bool {
    let Some(kind) = kind_of(kind) else { return false };
    let mut b = lock();
    let key = (symbol.to_string(), kind, tf);
    let wanted = b.wanted.contains_key(&key);
    let Some(f) = b.feeds.get_mut(&key) else { return false };
    f.inflight = false;
    match got {
        Err(e) => failed(symbol, kind, tf, f, e, now_ms),
        Ok(mut new) => {
            f.err = None;
            new.retain(|p| p.1.is_finite());
            new.sort_by_key(|p| p.0);
            new.dedup_by_key(|p| p.0);
            if let Some(first) = new.first().map(|p| p.0) {
                let keep = f.pts.partition_point(|p| p.0 < first);
                f.pts.truncate(keep);
                f.pts.extend(new);
            }
            if f.pts.len() > MAX_BARS {
                f.pts.drain(..f.pts.len() - MAX_BARS / 2);
            }
        }
    }
    wanted
}

/// 新取的一段盖掉旧的同一段（从它第一根起），再接上。
fn merge(bars: &mut Vec<Ohlcv>, buy: &mut Vec<f64>, new: Vec<(Ohlcv, f64)>) {
    let Some(first) = new.first().map(|k| k.0 .0) else { return };
    let keep = bars.partition_point(|k| k.0 < first);
    bars.truncate(keep);
    buy.truncate(keep);
    for (k, b) in new {
        bars.push(k);
        buy.push(b);
    }
}

/// 这一路是不是有人（跨品种指标 / 对照序列）在等——取到了要让图重画。
pub fn is_wanted(symbol: &str, tf: u64) -> bool {
    lock().wanted.contains_key(&(symbol.to_string(), BARS, tf))
}

/// 检查器里显示：后台在管哪几路（品种, 种类, 周期, 条数, 错误）。
pub fn status() -> Vec<(String, &'static str, u64, usize, Option<String>)> {
    let b = lock();
    let mut v: Vec<_> = b.feeds.iter().map(|((s, k, tf), f)| (s.clone(), *k, *tf, f.bars.len().max(f.pts.len()), f.err.clone())).collect();
    v.sort();
    v
}

#[cfg(test)]
pub fn reset() {
    *lock() = Bg::default();
}

/// 测试共用全局状态（这里和 `studies` 的跨品种 / 后台提醒测试）：串行跑。
#[cfg(test)]
pub fn test_guard() -> std::sync::MutexGuard<'static, ()> {
    static G: Mutex<()> = Mutex::new(());
    G.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: u64 = 60_000;

    fn k(t: u64, c: f64) -> Ohlcv {
        (t, c, c, c, c, 1.0)
    }

    use super::test_guard as guard;

    fn kb(t: u64, c: f64) -> (Ohlcv, f64) {
        (k(t, c), 0.5)
    }

    #[test]
    fn merge_replaces_overlap_and_appends() {
        let mut b = vec![k(0, 1.0), k(M, 2.0), k(2 * M, 3.0)];
        let mut buy = vec![0.1, 0.2, 0.3];
        merge(&mut b, &mut buy, vec![kb(2 * M, 3.5), kb(3 * M, 4.0)]);
        assert_eq!(b.iter().map(|x| x.4).collect::<Vec<_>>(), vec![1.0, 2.0, 3.5, 4.0]);
        assert_eq!(buy, vec![0.1, 0.2, 0.5, 0.5]);
    }

    #[test]
    fn alert_without_chart_is_fetched_then_waits_for_next_close() {
        let _g = guard();
        reset();
        let now = 100 * M + 10_000;
        let alerts = vec![("Binance Linear:BTCUSDT".to_string(), M, false)];
        let j = due(now, &alerts);
        assert_eq!(j, vec![Job { symbol: "Binance Linear:BTCUSDT".into(), kind: BARS, tf: M, since: None }]);
        // 取回来之前不重复派
        assert!(due(now + 1_500, &alerts).is_empty());
        let got: Vec<(Ohlcv, f64)> = (0..=100).map(|i| kb(i * M, i as f64)).collect();
        // 100 分那根还在走：99 根定稿（0..=98，99 分那根才收盘 10 秒）+ 99 分那根当暂定
        let (bars, _, as_now) = ingest("Binance Linear:BTCUSDT", M, Ok(got), now + 2_000).unwrap();
        assert_eq!(bars.len(), 100);
        assert!(bars[98].0 + M <= as_now && as_now < bars[99].0 + M, "最后一根当暂定");
        // 下一根收盘（101 分整）+ 15 秒之前不取
        assert!(due(101 * M + 10_000, &alerts).is_empty());
        let j = due(101 * M + 15_500, &alerts);
        // 上一批是 100 分 10 秒发出的：那时已定稿的最后一根是 98 分（99 分那根才收盘 10 秒），从它起取
        assert_eq!(j[0].since, Some(98 * M), "从上次取时最后一根已定稿的起增量取");
    }

    #[test]
    fn chart_covering_the_pair_stops_background_fetch() {
        let _g = guard();
        reset();
        let now = 100 * M;
        mark_chart("Binance Linear:BTCUSDT", M, now - 5_000);
        assert!(due(now, &[("Binance Linear:BTCUSDT".into(), M, false)]).is_empty());
        // 图 30 秒没算了（切走了 / 关了）→ 后台接手
        assert_eq!(due(now + 40_000, &[("Binance Linear:BTCUSDT".into(), M, false)]).len(), 1);
    }

    #[test]
    fn cross_symbol_want_gets_closed_bars_only() {
        let _g = guard();
        reset();
        let now = 10 * M + 20_000;
        assert!(closed_klines("Binance Linear:ETHUSDT", M, now).is_err());
        let j = due(now, &[]);
        assert_eq!(j.len(), 1);
        ingest("Binance Linear:ETHUSDT", M, Ok((0..=10).map(|i| kb(i * M, i as f64)).collect()), now);
        let (c, buy) = closed_klines("Binance Linear:ETHUSDT", M, now).unwrap();
        assert_eq!((c.len(), buy.len()), (10, 10), "10 分那根还没收盘，不给");
        // 过了 11 分：那批是 10 分 20 秒发出的，10 分那根在里面还是没收盘时的样子，照样不给（等下一批）
        assert_eq!(closed_klines("Binance Linear:ETHUSDT", M, 11 * M + 5_000).unwrap().0.len(), 10);
    }

    #[test]
    fn series_are_fetched_when_wanted_and_merged_by_avail_time() {
        let _g = guard();
        reset();
        let now = 100 * M;
        assert!(series("Binance Linear:BTCUSDT", "funding", M, now).is_err(), "还没取");
        let j = due(now, &[]);
        assert_eq!(j, vec![Job { symbol: "Binance Linear:BTCUSDT".into(), kind: "funding", tf: M, since: None }]);
        assert!(ingest_series("Binance Linear:BTCUSDT", "funding", M, Ok(vec![(10 * M, 1e-4), (50 * M, 2e-4)]), now));
        assert_eq!(series("Binance Linear:BTCUSDT", "funding", M, now).unwrap(), vec![(10 * M, 1e-4), (50 * M, 2e-4)]);
        // 下一次增量至少隔 5 分钟；新的一段盖掉旧的同一段（图每次重画都会再登记一次需要）
        assert!(due(now + 60_000, &[]).is_empty());
        let _ = series("Binance Linear:BTCUSDT", "funding", M, 105 * M);
        let j = due(105 * M + AFTER_CLOSE_MS + 1, &[]);
        assert_eq!(j[0].since, Some(50 * M - 600_000));
        ingest_series("Binance Linear:BTCUSDT", "funding", M, Ok(vec![(50 * M, 2.5e-4), (90 * M, 3e-4)]), 106 * M);
        assert_eq!(series("Binance Linear:BTCUSDT", "funding", M, 107 * M).unwrap(), vec![(10 * M, 1e-4), (50 * M, 2.5e-4), (90 * M, 3e-4)]);
        // 取不到（比如不是币安永续）：旧的还在就照用
        let _ = series("Binance Linear:BTCUSDT", "funding", M, 199 * M);
        due(200 * M, &[]);
        ingest_series("Binance Linear:BTCUSDT", "funding", M, Err("没有这类数据".into()), 200 * M);
        assert!(series("Binance Linear:BTCUSDT", "funding", M, 200 * M).unwrap().len() == 3, "旧的还在就照用");
    }

    #[test]
    fn errors_retry_later() {
        let _g = guard();
        reset();
        let a = vec![("Binance Linear:XUSDT".to_string(), M, false)];
        assert_eq!(due(M, &a).len(), 1);
        ingest("Binance Linear:XUSDT", M, Err("找不到".into()), M);
        assert!(due(M + 10_000, &a).is_empty());
        assert_eq!(due(M + RETRY_MS + 1, &a).len(), 1);
    }
}
