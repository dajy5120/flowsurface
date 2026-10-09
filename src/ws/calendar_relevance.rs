//! 金融日历「与我的相关度」（docs/43 §4 第二个数字）：**规则写死、可解释，不做模型**。
//!
//! 「我的标的」从四处来，每一个都标着来源：
//! - 手填：检查器「属性」里的「我关注的标的」（存在事件库 `alert_settings.watch`，和提醒设置在一起）；
//! - 收藏：侧栏行情表里点了星的；
//! - 图表：所有布局里 K 线 / 足迹图的品种（读 Cockpit 存盘，存盘每分钟写一次）；
//! - 运行：当前活动 run 的品种（`ws:active_run`），有仓位时记「持仓」。
//!
//! 事件这一边的标的 = 系列目录给的关联标的 + 标题里的代码（财报「（AAPL）」、交割「BTCUSDT_261225」、上新「AUSDT」）。
//!
//! - **高**：同一个标的（等价写法算同一个：ES / SPY / SPX、BTCUSDT / BTCUSD / BTC……）；
//! - **中**：同一类资产（只关注 SOLUSDT 时，关联 BTCUSDT 的 CPI 是「同类：加密」）。同类只看系列目录给的
//!   关联标的与加密交易所事件——别家公司的财报不算你个股的同类；
//! - 其余：无。

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::calendar_readout::Ev;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    None,
    Mid,
    High,
}

impl Level {
    pub fn label(self) -> &'static str {
        match self {
            Level::High => "高",
            Level::Mid => "中",
            Level::None => "—",
        }
    }
}

/// 我的一个标的：规范化后的代码、来源（手填 / 收藏 / 图表 / 运行 / 持仓）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mine {
    pub symbol: String,
    pub source: &'static str,
}

/// 等价写法：同一组里任一个都规范成第一个。
const ALIASES: &[&[&str]] = &[
    &["ES", "MES", "SPX", "SPY", "US500", "SPX500"],
    &["NQ", "MNQ", "NDX", "QQQ", "US100", "NAS100"],
    &["YM", "MYM", "DJI", "DIA", "US30"],
    &["RTY", "M2K", "RUT", "IWM"],
    &["GC", "MGC", "XAUUSD", "XAU", "GLD"],
    &["SI", "SIL", "XAGUSD", "XAG", "SLV"],
    &["CL", "MCL", "WTI", "USOIL", "USO"],
    &["ZN", "TY", "IEF"],
    &["ZB", "US", "TLT"],
    &["6E", "EURUSD", "EUR", "FXE"],
    &["6J", "USDJPY", "JPY", "FXY"],
    &["6B", "GBPUSD", "GBP", "FXB"],
    &["NKD", "NIY", "N225", "NIKKEI", "JP225"],
    &["DXY", "DX", "UUP"],
    &["VIX", "VX", "UVXY", "VXX"],
    &["FESX", "SX5E", "STOXX50"],
];

/// 资产大类（「中」用）。
fn class(sym: &str) -> Option<&'static str> {
    const CLASSES: &[(&str, &[&str])] = &[
        ("美股", &["ES", "NQ", "YM", "RTY", "VIX"]),
        ("美债", &["ZN", "ZB", "ZF", "ZT"]),
        ("贵金属", &["GC", "SI"]),
        ("能源", &["CL", "NG"]),
        ("外汇", &["6E", "6J", "6B", "DXY", "EURUSD"]),
        ("欧股", &["FESX"]),
        ("日股", &["NKD"]),
    ];
    if let Some((c, _)) = CLASSES.iter().find(|(_, xs)| xs.contains(&sym)) {
        return Some(c);
    }
    if is_crypto(sym) {
        return Some("加密");
    }
    // 1–5 个字母的其余代码：当美股个股（财报、除息的代码都是这样）
    if (1..=5).contains(&sym.len()) && sym.chars().all(|c| c.is_ascii_uppercase()) {
        return Some("美股");
    }
    None
}

fn is_crypto(sym: &str) -> bool {
    ["USDT", "USDC", "BUSD", "FDUSD"].iter().any(|q| sym.len() > q.len() && sym.ends_with(q))
        || ["BTC", "ETH", "SOL", "BNB", "XRP", "DOGE"].contains(&sym)
}

/// 规范化：去交易所前缀（`BinanceSpot:`）、永续 / 交割后缀（`_PERP`、`_261225`、`.P`、`-SWAP`），
/// 大写，等价写法收成一个；加密的 `BTCUSD` / `BTC` 收成 `BTCUSDT`。
pub fn normalize(raw: &str) -> String {
    let mut s = raw.trim().to_ascii_uppercase();
    // 存盘里的品种是「交易所:内部代码|显示代码」
    if let Some(i) = s.find('|') {
        s.truncate(i);
    }
    if let Some(i) = s.rfind(':') {
        s = s[i + 1..].to_string();
    }
    for suf in ["_PERP", ".P", "-SWAP", "-PERP", "PERP", ".FUT"] {
        if s.len() > suf.len() && s.ends_with(suf) {
            s.truncate(s.len() - suf.len());
        }
    }
    // 交割合约 BTCUSDT_261225 → BTCUSDT
    if let Some((a, b)) = s.split_once('_')
        && b.chars().all(|c| c.is_ascii_digit())
    {
        s = a.to_string();
    }
    s.retain(|c| !c.is_whitespace());
    for g in ALIASES {
        if g.contains(&s.as_str()) {
            return g[0].to_string();
        }
    }
    for base in ["BTC", "ETH", "SOL", "BNB", "XRP", "DOGE"] {
        if s == base || s == format!("{base}USD") || s == format!("{base}USDC") {
            return format!("{base}USDT");
        }
    }
    s
}

/// 事件的标的：关联标的 + 标题里的代码。
pub fn event_symbols(e: &Ev) -> Vec<String> {
    let mut out: Vec<String> = e.assets.iter().map(|a| normalize(a)).collect();
    // 财报 / 分红 / 新股：「Delta Air Lines, Inc.（DAL）财报」
    for (open, close) in [('（', '）'), ('(', ')')] {
        let mut rest = e.title.as_str();
        while let Some(i) = rest.find(open) {
            let after = &rest[i + open.len_utf8()..];
            let Some(j) = after.find(close) else { break };
            let inner = after[..j].trim();
            if (1..=6).contains(&inner.len()) && inner.chars().all(|c| c.is_ascii_uppercase() || c == '.') {
                out.push(normalize(inner));
            }
            // 交割「（BTCUSDT_261225、ETHUSDT_261225）」
            for tok in inner.split(['、', ',', ' ']) {
                if tok.contains('_') && tok.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    out.push(normalize(tok));
                }
            }
            rest = &after[j..];
        }
    }
    // 上新 / 下架：「Listing of AUSDT」
    for w in e.title.split(|c: char| !c.is_ascii_alphanumeric() && c != '_') {
        if w.len() > 4 && w.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') && is_crypto(&normalize(w)) {
            out.push(normalize(w));
        }
    }
    out.sort();
    out.dedup();
    out
}

/// 一个事件对我的相关度，和一句为什么。
pub fn rate(e: &Ev, mine: &[Mine]) -> (Level, String) {
    if mine.is_empty() {
        return (Level::None, String::new());
    }
    let syms = event_symbols(e);
    let hits: Vec<&Mine> = mine.iter().filter(|m| syms.contains(&m.symbol)).collect();
    if !hits.is_empty() {
        let why: Vec<String> = hits.iter().map(|m| format!("{}（{}）", m.symbol, m.source)).collect();
        return (Level::High, format!("关联你的 {}", dedup(why).join("、")));
    }
    // 「同类」只看系列目录给的关联标的（指数、美债、美元……）：别家公司的财报和你的个股不算同类，
    // 否则关注一只股票，几百条财报全成了「中」
    let ev_classes: Vec<&str> = e.assets.iter().filter_map(|s| class(&normalize(s))).collect();
    // 加密交易所自己的事件（上新、下架、交割、维护）没有标的时也算加密
    let ev_classes = if ev_classes.is_empty() && e.country == "CRYPTO" { vec!["加密"] } else { ev_classes };
    for m in mine {
        if let Some(c) = class(&m.symbol)
            && ev_classes.contains(&c)
        {
            return (Level::Mid, format!("同类：{c}（你的 {}）", m.symbol));
        }
    }
    (Level::None, String::new())
}

fn dedup(mut v: Vec<String>) -> Vec<String> {
    v.dedup();
    v
}

// ── 我的标的：四个来源 ────────────────────────────────────────────

/// 手填的清单：逗号 / 空格 / 顿号分隔。
pub fn parse_watch(s: &str) -> Vec<String> {
    let mut out: Vec<String> = s.split([',', '，', '、', ' ', '\n']).filter(|x| !x.trim().is_empty()).map(normalize).collect();
    out.dedup();
    out
}

/// 存盘里的图表品种与侧栏收藏。读文件，按修改时间缓存（每帧都会问）。
fn from_saved_state() -> (Vec<String>, Vec<String>) {
    type Saved = (std::time::SystemTime, Vec<String>, Vec<String>);
    static CACHE: Mutex<Option<Saved>> = Mutex::new(None);
    let path = data::data_path(Some(data::SAVED_STATE_PATH));
    let Ok(mtime) = std::fs::metadata(&path).and_then(|m| m.modified()) else {
        return (Vec::new(), Vec::new());
    };
    let mut g = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((t, a, b)) = g.as_ref()
        && *t == mtime
    {
        return (a.clone(), b.clone());
    }
    let (charts, favs) = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .map(|v| parse_saved_state(&v))
        .unwrap_or_default();
    *g = Some((mtime, charts.clone(), favs.clone()));
    (charts, favs)
}

/// 存盘 JSON → （图表品种, 收藏）。图表 = 所有布局里 `KlineChart` 的 `stream_type[].Kline.ticker`。
pub fn parse_saved_state(v: &serde_json::Value) -> (Vec<String>, Vec<String>) {
    fn walk(v: &serde_json::Value, out: &mut Vec<String>) {
        match v {
            serde_json::Value::Object(m) => {
                if let Some(k) = m.get("KlineChart").and_then(|k| k.get("stream_type")).and_then(|s| s.as_array()) {
                    for s in k {
                        if let Some(t) = s.get("Kline").and_then(|x| x.get("ticker")).and_then(|x| x.as_str()) {
                            out.push(normalize(t));
                        }
                    }
                }
                for x in m.values() {
                    walk(x, out);
                }
            }
            serde_json::Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    let mut charts = Vec::new();
    if let Some(l) = v.get("layout_manager") {
        walk(l, &mut charts);
    }
    charts.sort();
    charts.dedup();
    let mut favs: Vec<String> = v
        .pointer("/sidebar/tickers_table/favorited_tickers")
        .and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|t| t.as_str().map(normalize)).collect())
        .unwrap_or_default();
    favs.sort();
    favs.dedup();
    (charts, favs)
}

/// 我的全部标的（同一个标的只留第一个来源：手填 > 持仓 > 运行 > 收藏 > 图表）。
pub fn mine() -> Vec<Mine> {
    // 单测不读这台机器的存盘 / Redis（否则筛选测试随用户开了什么图而变）
    if cfg!(test) {
        return Vec::new();
    }
    let mut out: Vec<Mine> = Vec::new();
    let mut push = |s: String, source: &'static str| {
        if !s.is_empty() && !out.iter().any(|m| m.symbol == s) {
            out.push(Mine { symbol: s, source });
        }
    };
    for s in &watch() {
        push(normalize(s), "手填");
    }
    if let Some(ar) = super::active_run::current()
        && ar.mode != "stopped"
        && !ar.symbol.is_empty()
    {
        let pos = super::orders::chart_position_snapshot();
        let held = !pos.side.is_empty() && pos.side != "FLAT" && pos.net_qty != 0.0;
        push(normalize(&ar.symbol), if held { "持仓" } else { "运行" });
    }
    let (charts, favs) = from_saved_state();
    for s in favs {
        push(s, "收藏");
    }
    for s in charts {
        push(s, "图表");
    }
    out
}

/// 刚保存的手填清单：提醒线程下一轮才把它放进快照，这之前用这里的（保存即生效）。
static SAVED: Mutex<Option<Vec<String>>> = Mutex::new(None);

/// 手填清单（已保存的）。
pub fn watch() -> Vec<String> {
    let snap = super::calendar_alerts::snapshot().watch.clone();
    let mut g = SAVED.lock().unwrap_or_else(|e| e.into_inner());
    match g.as_ref() {
        Some(v) if *v != snap => v.clone(),
        _ => {
            *g = None; // 快照已经追上
            snap
        }
    }
}

/// 保存了新的手填清单：立即生效。
pub fn saved(list: Vec<String>) {
    if let Ok(mut g) = SAVED.lock() {
        *g = Some(list);
    }
    invalidate();
}

// ── 每帧用的缓存：事件 id → 相关度 ─────────────────────────────────

struct Cache {
    at: Instant,
    mine: Vec<Mine>,
    rated: HashMap<String, (Level, String)>,
}

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

/// 我的标的变了（手填保存后）：下一帧重算。
pub fn invalidate() {
    if let Ok(mut g) = CACHE.lock() {
        *g = None;
    }
}

fn with_cache<T>(f: impl FnOnce(&mut Cache) -> T) -> T {
    let mut g = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    // 我的标的每 5 秒重新收集一次（存盘每分钟写；运行状态随时变）
    if g.as_ref().is_none_or(|c| c.at.elapsed() > Duration::from_secs(5)) {
        let mine = mine();
        let keep = g.as_ref().is_some_and(|c| c.mine == mine);
        let rated = if keep { g.take().map(|c| c.rated).unwrap_or_default() } else { HashMap::new() };
        *g = Some(Cache { at: Instant::now(), mine, rated });
    }
    f(g.as_mut().expect("刚放进去"))
}

pub fn of(e: &Ev) -> (Level, String) {
    with_cache(|c| {
        if let Some(x) = c.rated.get(&e.id) {
            return x.clone();
        }
        let x = rate(e, &c.mine);
        c.rated.insert(e.id.clone(), x.clone());
        x
    })
}

pub fn level(e: &Ev) -> Level {
    of(e).0
}

/// 当前的我的标的（检查器里列出来）。
pub fn current() -> Vec<Mine> {
    with_cache(|c| c.mine.clone())
}

/// 按来源分组：来源 → 标的（检查器里一行一个来源）。
pub fn by_source(mine: &[Mine]) -> BTreeMap<&'static str, Vec<String>> {
    let mut m: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    for x in mine {
        m.entry(x.source).or_default().push(x.symbol.clone());
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(title: &str, assets: &[&str], country: &str) -> Ev {
        Ev {
            id: title.into(),
            title: title.into(),
            country: country.into(),
            assets: assets.iter().map(|s| s.to_string()).collect(),
            kind: "macro".into(),
            ..Default::default()
        }
    }

    fn m(s: &str, src: &'static str) -> Mine {
        Mine { symbol: normalize(s), source: src }
    }

    #[test]
    fn symbols_normalize_across_venues_and_aliases() {
        assert_eq!(normalize("BinanceSpot:BTCUSDT"), "BTCUSDT");
        assert_eq!(normalize("BinanceLinear:btcusdt"), "BTCUSDT");
        assert_eq!(normalize("HyperliquidLinear:BTC|BTC-PERP"), "BTCUSDT");
        assert_eq!(normalize("BTCUSDT_PERP"), "BTCUSDT");
        assert_eq!(normalize("BTCUSDT_261225"), "BTCUSDT");
        assert_eq!(normalize("BTC"), "BTCUSDT");
        assert_eq!(normalize("SPY"), "ES");
        assert_eq!(normalize("ES.FUT"), "ES");
        assert_eq!(normalize("xauusd"), "GC");
        assert_eq!(normalize("AAPL"), "AAPL");
    }

    #[test]
    fn same_symbol_is_high_same_class_is_mid_and_both_say_why() {
        let cpi = ev("Consumer Price Index", &["ES", "NQ", "DXY", "GC", "ZN", "BTCUSDT"], "US");
        let (l, why) = rate(&cpi, &[m("BinanceSpot:BTCUSDT", "图表")]);
        assert_eq!(l, Level::High);
        assert_eq!(why, "关联你的 BTCUSDT（图表）");
        let (l, why) = rate(&cpi, &[m("SOLUSDT", "手填")]);
        assert_eq!((l, why.as_str()), (Level::Mid, "同类：加密（你的 SOLUSDT）"));
        let (l, _) = rate(&cpi, &[m("SPY", "收藏")]);
        assert_eq!(l, Level::High, "SPY 与 ES 是同一个");
        let (l, why) = rate(&cpi, &[m("AAPL", "手填")]);
        assert_eq!((l, why.as_str()), (Level::Mid, "同类：美股（你的 AAPL）"), "个股与关联股指的宏观事件同类");
        let ecb = ev("ECB", &["EURUSD", "DXY", "FESX"], "EU");
        assert_eq!(rate(&ecb, &[m("BTCUSDT", "图表")]).0, Level::None);
        assert_eq!(rate(&cpi, &[]).0, Level::None, "没有我的标的：一律无");
    }

    #[test]
    fn tickers_in_titles_count() {
        let er = ev("Delta Air Lines, Inc.（DAL）财报 · 盘前", &[], "US");
        assert_eq!(event_symbols(&er), ["DAL"]);
        assert_eq!(rate(&er, &[m("DAL", "手填")]).0, Level::High);
        assert_eq!(rate(&er, &[m("AAPL", "手填")]).0, Level::None, "别家公司的财报和你的个股不算同类");
        let dlv = ev("币安季度合约交割（BTCUSDT_261225、ETHUSDT_261225）", &[], "CRYPTO");
        assert_eq!(event_symbols(&dlv), ["BTCUSDT", "ETHUSDT"]);
        let ls = ev("New listing: Listing of AUSDT on Oct 15, 2026, 8:00AM UTC", &[], "CRYPTO");
        assert_eq!(event_symbols(&ls), ["AUSDT"]);
        assert_eq!(rate(&ls, &[m("SOLUSDT", "图表")]).0, Level::Mid, "加密交易所的事件对加密是同类");
        assert_eq!(rate(&ls, &[m("AUSDT", "图表")]).0, Level::High);
        let maint = ev("Binance 系统维护", &[], "CRYPTO");
        assert_eq!(rate(&maint, &[m("SOLUSDT", "图表")]).0, Level::Mid, "加密交易所的事件没有标的也算加密");
    }

    #[test]
    fn saved_state_gives_chart_tickers_and_favorites() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"layout_manager":{"layouts":[{"name":"x","dashboard":{"pane":{"Split":{"a":{"Calendar":{}},
               "b":{"KlineChart":{"stream_type":[{"Kline":{"ticker":"BinanceSpot:BTCUSDT","timeframe":"D1"}}]}}}}}},
               {"name":"y","dashboard":{"pane":{"KlineChart":{"stream_type":[]}}}}]},
               "sidebar":{"tickers_table":{"favorited_tickers":["BinanceLinear:SOLUSDT"]}}}"#,
        )
        .unwrap();
        assert_eq!(parse_saved_state(&v), (vec!["BTCUSDT".to_string()], vec!["SOLUSDT".to_string()]));
    }

    #[test]
    fn the_watch_list_accepts_chinese_and_ascii_separators() {
        assert_eq!(parse_watch("btcusdt, SPY、aapl  "), ["BTCUSDT", "ES", "AAPL"]);
    }
}
