//! 订单流特征（docs/31，S1–S7）与图表参数（docs/33）当指标的只读输入（docs/63 §10 J4「已有的接进来，不重算」）。
//!
//! 指标在「对照序列」里选 `@feature:<编号>`（「外部序列」指标把它画出来，别的指标再拿那个指标的输出当数据源）。
//! 特征引擎只把**当前**的快照写给 Cockpit（`feature_matrix.json`），面板自己的历史只留 10 分钟，所以这里另记：
//! 被引用的编号，每次快照前进就按引擎的事件时间记一个点（每秒最多一个），最多留 24 小时——
//! **只有 Cockpit 开着、引擎在跑的这段才有值**，更早的要看 Python 侧（特征仓库，`IND.feature_bars`）。
//! 值原样搬，不重算（同 `feature_matrix_readout` 的原则：面板里不能有第二条计算路径）。

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};

use super::feature_matrix_readout as fm;

const KEEP_MS: u64 = 24 * 3_600_000;
const WANT_FRESH_MS: u64 = 120_000;
/// 引擎快照多久没前进就算没在跑。
const STALE_MS: u64 = 60_000;

#[derive(Default)]
struct Acc {
    pts: HashMap<String, VecDeque<(u64, f64)>>,
    wanted: HashMap<String, u64>,
    last_tick: u64,
    last_as_of: u64,
    /// 引擎跟的品种（每秒在 `tick` 里取一次；`series` 每次重画都调，不能每次都去拼链路信息）
    engine_symbol: String,
}

fn acc() -> &'static Mutex<Acc> {
    static A: OnceLock<Mutex<Acc>> = OnceLock::new();
    A.get_or_init(Default::default)
}

/// 一个 slot 的编号：`<特征键>@<窗口毫秒>`（同一个特征有多个窗口）。图表参数就是它的键。
pub fn slot_id(s: &fm::Slot) -> String {
    format!("{}@{}", s.key, s.window_ms)
}

/// 当前快照里能选的：（编号, 显示名）——启用了的特征 slot 与图表参数。
pub fn catalog() -> Vec<(String, String)> {
    let m = fm::snapshot();
    let mut v: Vec<(String, String)> = m
        .slots
        .iter()
        .filter(|s| !s.disabled() && !s.not_implemented())
        .map(|s| (slot_id(s), format!("{} {}", s.name_cn, s.window_label())))
        .collect();
    let mut chart: Vec<(String, String)> = m.chart.values.keys().map(|k| (k.clone(), format!("图表参数 {k}"))).collect();
    chart.sort();
    v.extend(chart);
    v
}

fn value_of(m: &fm::Matrix, id: &str) -> Option<f64> {
    if let Some((key, w)) = id.rsplit_once('@')
        && let Ok(w) = w.parse::<u32>()
    {
        return m.slots.iter().find(|s| s.key == key && s.window_ms == w).and_then(|s| s.value);
    }
    m.chart.values.get(id).and_then(|v| v.v)
}

/// 每帧调（每秒最多真做一次）：快照前进了就给被引用的编号各记一个点。回放时不记（回放的快照不是实时的）。
pub fn tick(now_ms: u64) {
    let Ok(mut a) = acc().lock() else { return };
    if now_ms.saturating_sub(a.last_tick) < 1_000 {
        return;
    }
    a.last_tick = now_ms;
    a.wanted.retain(|_, t| now_ms.saturating_sub(*t) < WANT_FRESH_MS);
    if a.wanted.is_empty() || super::replay_clock::target().is_some() {
        return;
    }
    a.engine_symbol = super::feature_source::link_info().symbol;
    let m = fm::snapshot();
    let t = m.as_of / 1_000_000;
    if !m.present || t <= a.last_as_of || t / 1_000 == a.last_as_of / 1_000 {
        return;
    }
    a.last_as_of = t;
    let ids: Vec<String> = a.wanted.keys().cloned().collect();
    for id in ids {
        if let Some(v) = value_of(&m, &id).filter(|v| v.is_finite()) {
            let q = a.pts.entry(id).or_default();
            q.push_back((t, v));
            while q.front().is_some_and(|p| p.0 + KEEP_MS < t) {
                q.pop_front();
            }
        }
    }
}

/// 这个编号记下来的（引擎事件时间毫秒, 值）。`code` 是图的品种代码：引擎跟的不是它就不给（别的品种的特征叠上来是错的）。
pub fn series(id: &str, code: &str, now_ms: u64) -> Result<Vec<(u64, f64)>, String> {
    let Ok(mut a) = acc().lock() else { return Err("特征记录不可用".into()) };
    a.wanted.insert(id.to_string(), now_ms);
    let eng = a.engine_symbol.clone();
    if !eng.is_empty() && !eng.eq_ignore_ascii_case(code) {
        return Err(format!("特征引擎跟的是 {eng}，不是这张图的 {code}"));
    }
    let m = fm::snapshot();
    let pts: Vec<(u64, f64)> = a.pts.get(id).map(|q| q.iter().copied().collect()).unwrap_or_default();
    if pts.is_empty() {
        return Err(if !m.present || (m.as_of / 1_000_000) + STALE_MS < now_ms {
            "特征引擎没在跑（订单流特征工作区 / 守护 ws-features）".into()
        } else if value_of(&m, id).is_none() {
            format!("引擎快照里没有 {id}（没启用，或这个市场没有）")
        } else {
            "在记录特征（从现在起有值；更早的看 Python 侧特征仓库）…".into()
        });
    }
    Ok(pts)
}

#[cfg(test)]
pub fn push_for_test(id: &str, pts: &[(u64, f64)]) {
    if let Ok(mut a) = acc().lock() {
        a.pts.insert(id.to_string(), pts.iter().copied().collect());
    }
}
