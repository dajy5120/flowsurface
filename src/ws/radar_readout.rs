//! 全市场雷达面板只读快照（docs/22 §4.2）。
//!
//! **独立新增面板**（`Content::MarketMap`）——不改任何既有面板/readout。数据源（全只读）：
//! `~/ws-data/live/radar_board.json`（`ws-radar` 守护产出）。沿用
//! c4_readout / prediction_readout 的旁路 poller 模式，但节拍取 2s——守护 5s 出一版，
//! 10s 会让面板明显滞后于数据。

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// 窗口顺序必须与 `wealthspring_radar::metrics::Window::ALL` 一致。
pub const WINDOWS: [&str; 6] = ["1m", "5m", "15m", "1h", "4h", "24h"];
pub const N_WIN: usize = 6;

/// 一个标的一行。
#[derive(Default, Clone)]
pub struct RadarRow {
    pub symbol: String,
    pub venue: String,
    /// 数据等级 A/B/C/D（docs/22 §0）。**逐行**给——一张表里混着 A 档加密和
    /// C 档延迟股票，不标就会被当成同一等级。
    pub tier: String,
    /// 资产类（`crypto` / `equity` / …）。由数据源声明，面板据此过滤。
    pub asset: String,
    /// 全称（股票源有，加密源空）。
    pub name: String,
    pub sector: String,
    pub country: String,
    pub currency: String,
    pub mcap: f64,
    /// TradingView 口径的全量指标（键见 docs/22 §4.3）。缺的指标**不在表里**，
    /// 不是 0——面板据此显示「—」并把格子置中性。
    pub m: std::collections::HashMap<String, f64>,
    /// 加密分类（TradingView `crypto_common_categories`）。「来源」下拉据此过滤。
    pub cats: Vec<String>,
    pub price: f64,
    pub quote_vol_24h: f64,
    /// 各窗口对数收益。`None` = **该窗口还没热身**，不是「没涨没跌」。
    pub ret: [Option<f64>; N_WIN],
    /// 各窗口的波动归一化 z 值（docs/22 §3 的「涨跌速度」）。
    pub z_ret: [Option<f64>; N_WIN],
    pub z_vol: Option<f64>,
    pub z_cnt: Option<f64>,
    /// 自身样本足够、z 可信。false → 灰显。
    pub sigma_ok: bool,
    /// z 借了横截面中位数基线（暂且一看，不是结论）→ 同样降级渲染。
    pub z_provisional: bool,
}

impl RadarRow {
    /// 面板配色/排序用的主口径：5m 的 z，缺则退 1m。
    pub fn headline_z(&self) -> Option<f64> {
        self.z_ret[1].or(self.z_ret[0])
    }
    /// 读数是否可当真（两个降级标都干净）。
    pub fn trustworthy(&self) -> bool {
        self.sigma_ok && !self.z_provisional
    }
}

/// 回填进度（docs/22 P0c）。
#[derive(Default, Clone, PartialEq)]
pub struct BackfillView {
    pub done: i64,
    pub total: i64,
    pub failed: i64,
    pub running: bool,
    pub finished: bool,
}

impl BackfillView {
    pub fn pct(&self) -> f64 {
        if self.total <= 0 {
            0.0
        } else {
            self.done as f64 / self.total as f64
        }
    }
}

/// 一个市场的宽度（docs/22 §2 ③）。
#[derive(Default, Clone, PartialEq)]
pub struct BreadthRow {
    pub market: String,
    pub n: i64,
    pub adv: i64,
    pub dec: i64,
    pub unch: i64,
    pub new_high: i64,
    pub new_low: i64,
    pub net_new_high: i64,
    /// 比值一律 `Option`——分母为 0 时给 `None`，面板显示「—」而不是 inf。
    pub adv_pct: Option<f64>,
    pub ad_ratio: Option<f64>,
    pub above_ma200_pct: Option<f64>,
}

/// 一行全球总览（docs/22 §2 ②）。
#[derive(Default, Clone, PartialEq)]
pub struct OverviewRow {
    pub label: String,
    pub ticker: String,
    pub currency: String,
    /// 本币对数收益，键为「日/周/月/YTD」。
    pub local: [Option<f64>; 4],
    /// 美元对数收益。**缺席即缺席**——拿不到汇率时绝不退回本币值。
    pub usd: [Option<f64>; 4],
}

pub const OV_WINDOWS: [&str; 4] = ["日", "周", "月", "YTD"];

/// 来源目录的一项。
#[derive(Default, Clone, PartialEq)]
pub struct CatalogItem {
    pub code: String,
    pub label: String,
    /// 市场才有；类型/分类为空。
    pub region: String,
}

/// 来源目录（docs/22 §6.5）：面板「来源」下拉的**全部可选项**，随快照下发。
/// 只列已加载的 venue 的话，用户永远只能在守护恰好在拉的那几个市场里打转。
#[derive(Default, Clone)]
pub struct Catalog {
    pub markets: Vec<CatalogItem>,
    /// 指数成分股来源。`code` 是来源 id（`america@SPX`），`region` 借用为所属市场。
    pub indices: Vec<CatalogItem>,
    pub types: Vec<CatalogItem>,
    pub crypto_cats: Vec<CatalogItem>,
}

#[derive(Default, Clone)]
pub struct RadarReadout {
    pub stamp: String,
    pub source: String,
    /// 数据等级（docs/22 §0）：加密直连 = "A"。面板**必须**把它显示出来。
    pub tier: String,
    /// 资产类（`crypto` / `equity` / …）。由数据源声明，面板据此过滤。
    pub asset: String,
    pub n_symbols: i64,
    pub refreshed_ms: i64,
    pub rows: Vec<RadarRow>,
    pub backfill: BackfillView,
    pub catalog: Catalog,
    pub breadth: Vec<BreadthRow>,
    pub overview: Vec<OverviewRow>,
    pub refreshed: String,
    pub present: bool,
    /// 守护 service 状态。后台 poller 刷——**不能在 view 里查**，那是每帧一次
    /// systemctl 子进程（docs/20 §19.2 的每帧开销教训）。
    pub svc: super::svcctl::UnitState,
}

static READOUT: OnceLock<Mutex<RadarReadout>> = OnceLock::new();
static POLLER: OnceLock<()> = OnceLock::new();
static WAKER: super::svcctl::Waker = super::svcctl::Waker::new();

pub const RADAR_SVC: &str = "ws-radar.service";

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

fn board_path() -> PathBuf {
    std::env::var("WS_RADAR_BOARD")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home().join("ws-data/live/radar_board.json"))
}

pub fn snapshot() -> RadarReadout {
    ensure_poller();
    READOUT
        .get()
        .and_then(|m| m.lock().ok().map(|g| g.clone()))
        .unwrap_or_default()
}

pub fn request_refresh() {
    WAKER.request();
}

fn request_path() -> PathBuf {
    std::env::var("WS_RADAR_REQUEST")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home().join("ws-data/live/radar_request.json"))
}

/// 纯构造（可单测）：选中的来源 → 请求 JSON。
///
/// 只带**当前选中项**，不累积：累积会让守护的请求数随用户点击单调增长，
/// 最后把非官方接口打爆。配置里的默认市场恒在，选中的额外加一个。
pub fn request_body(source: &str, security_types: &[&str]) -> String {
    let sources: Vec<serde_json::Value> = if source.is_empty() {
        Vec::new()
    } else {
        security_types
            .iter()
            .map(|t| serde_json::json!({"market": source, "security_type": t}))
            .collect()
    };
    serde_json::json!({ "sources": sources }).to_string()
}

/// 把选中的来源写给守护（原子写，同快照）。
///
/// 内容没变就不写：面板每帧都会走到这里，每帧重写文件既浪费也会让守护看到抖动。
pub fn write_request(source: &str, security_types: &[&str]) {
    static LAST: Mutex<String> = Mutex::new(String::new());
    let body = request_body(source, security_types);
    if let Ok(mut g) = LAST.lock() {
        if *g == body {
            return;
        }
        *g = body.clone();
    }
    let p = request_path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = p.with_extension("json.tmp");
    if std::fs::write(&tmp, body.as_bytes()).is_ok() {
        let _ = std::fs::rename(&tmp, &p);
    }
}

/// 启动守护。**必须 `--no-block`**（同 prediction/factory 踩过的坑：`systemctl start`
/// 会等到 unit 就绪才返回，足以冻死 UI 线程）。
pub fn radar_start() -> String {
    match std::process::Command::new("systemctl")
        .args(["--user", "start", "--no-block", RADAR_SVC])
        .status()
    {
        Ok(s) if s.success() => {
            WAKER.request();
            "▶ 已启动雷达守护（各窗口需热身，见 docs/22 §8.1）".into()
        }
        Ok(s) => format!("✗ 启动失败，退出码 {:?}", s.code()),
        Err(e) => format!("✗ 启动失败：{e}"),
    }
}

pub fn radar_stop() -> String {
    match std::process::Command::new("systemctl")
        .args(["--user", "stop", RADAR_SVC])
        .status()
    {
        Ok(s) if s.success() => {
            WAKER.request();
            "■ 已停止雷达守护（内存中的 EWMA 状态会丢，重启需重新热身）".into()
        }
        Ok(s) => format!("✗ 停止失败，退出码 {:?}", s.code()),
        Err(e) => format!("✗ 停止失败：{e}"),
    }
}

fn ensure_poller() {
    POLLER.get_or_init(|| {
        std::thread::spawn(|| loop {
            let mut snap = poll_once();
            snap.svc = super::svcctl::query(RADAR_SVC);
            let lock = READOUT.get_or_init(|| Mutex::new(RadarReadout::default()));
            if let Ok(mut g) = lock.lock() {
                *g = snap;
            }
            WAKER.wait(Duration::from_secs(2));
        });
    });
}

fn poll_once() -> RadarReadout {
    let refreshed = chrono::Local::now().format("%H:%M:%S").to_string();
    match std::fs::read_to_string(board_path())
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
    {
        Some(v) => RadarReadout {
            refreshed,
            ..parse_board(&v)
        },
        None => RadarReadout {
            refreshed,
            ..Default::default()
        },
    }
}

/// 从窗口字典里按 [`WINDOWS`] 顺序取值。**缺席即 `None`，不补 0**——
/// 补 0 会把「还没热身」显示成「没涨没跌」，是这个面板最容易犯的谎。
fn win_arr(o: Option<&serde_json::Value>) -> [Option<f64>; N_WIN] {
    let mut out = [None; N_WIN];
    if let Some(m) = o {
        for (i, k) in WINDOWS.iter().enumerate() {
            out[i] = m.get(*k).and_then(|x| x.as_f64());
        }
    }
    out
}

/// 纯解析（可单测）：radar_board.json → RadarReadout。
fn parse_board(v: &serde_json::Value) -> RadarReadout {
    let s = |o: &serde_json::Value, k: &str| {
        o.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
    };
    let rows = v
        .get("rows")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .map(|o| RadarRow {
                    symbol: s(o, "symbol"),
                    venue: s(o, "venue"),
                    tier: s(o, "tier"),
                    asset: s(o, "asset"),
                    name: s(o, "name"),
                    sector: s(o, "sector"),
                    country: s(o, "country"),
                    currency: s(o, "currency"),
                    mcap: o.get("mcap").and_then(|x| x.as_f64()).unwrap_or(0.0),
                    cats: o
                        .get("cats")
                        .and_then(|x| x.as_array())
                        .map(|a| {
                            a.iter().filter_map(|c| c.as_str().map(String::from)).collect()
                        })
                        .unwrap_or_default(),
                    m: o
                        .get("m")
                        .and_then(|x| x.as_object())
                        .map(|mm| {
                            mm.iter()
                                .filter_map(|(k, v)| v.as_f64().map(|f| (k.clone(), f)))
                                .collect()
                        })
                        .unwrap_or_default(),
                    price: o.get("price").and_then(|x| x.as_f64()).unwrap_or(0.0),
                    quote_vol_24h: o
                        .get("quote_vol_24h")
                        .and_then(|x| x.as_f64())
                        .unwrap_or(0.0),
                    ret: win_arr(o.get("ret")),
                    z_ret: win_arr(o.get("z_ret")),
                    z_vol: o.get("z_vol").and_then(|x| x.as_f64()),
                    z_cnt: o.get("z_cnt").and_then(|x| x.as_f64()),
                    sigma_ok: o.get("sigma_ok").and_then(|x| x.as_bool()).unwrap_or(false),
                    z_provisional: o
                        .get("z_provisional")
                        .and_then(|x| x.as_bool())
                        .unwrap_or(false),
                })
                .collect()
        })
        .unwrap_or_default();
    let bf = v.get("backfill");
    let bi = |k: &str| {
        bf.and_then(|o| o.get(k))
            .and_then(|x| x.as_i64())
            .unwrap_or(0)
    };
    let bb = |k: &str| {
        bf.and_then(|o| o.get(k))
            .and_then(|x| x.as_bool())
            .unwrap_or(false)
    };
    let i64_of = |o: &serde_json::Value, k: &str| o.get(k).and_then(|x| x.as_i64()).unwrap_or(0);
    let optf_of = |o: &serde_json::Value, k: &str| o.get(k).and_then(|x| x.as_f64());
    let items = |k: &str| -> Vec<CatalogItem> {
        v.get("catalog")
            .and_then(|c| c.get(k))
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .map(|o| CatalogItem {
                        code: s(o, "code"),
                        label: s(o, "label"),
                        region: s(o, "region"),
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let index_items: Vec<CatalogItem> = v
        .get("catalog")
        .and_then(|c| c.get("indices"))
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .map(|o| CatalogItem {
                    code: s(o, "id"),
                    label: s(o, "label"),
                    region: s(o, "market"),
                })
                .collect()
        })
        .unwrap_or_default();
    let catalog = Catalog {
        markets: items("markets"),
        indices: index_items,
        types: items("types"),
        crypto_cats: items("crypto_cats"),
    };
    let breadth = v
        .get("breadth")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .map(|o| BreadthRow {
                    market: s(o, "market"),
                    n: i64_of(o, "n"),
                    adv: i64_of(o, "adv"),
                    dec: i64_of(o, "dec"),
                    unch: i64_of(o, "unch"),
                    new_high: i64_of(o, "new_high"),
                    new_low: i64_of(o, "new_low"),
                    net_new_high: i64_of(o, "net_new_high"),
                    adv_pct: optf_of(o, "adv_pct"),
                    ad_ratio: optf_of(o, "ad_ratio"),
                    above_ma200_pct: optf_of(o, "above_ma200_pct"),
                })
                .collect()
        })
        .unwrap_or_default();
    let ov_arr = |o: Option<&serde_json::Value>| {
        let mut out = [None; 4];
        if let Some(m) = o {
            for (i, k) in OV_WINDOWS.iter().enumerate() {
                out[i] = m.get(*k).and_then(|x| x.as_f64());
            }
        }
        out
    };
    let overview = v
        .get("overview")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .map(|o| OverviewRow {
                    label: s(o, "label"),
                    ticker: s(o, "ticker"),
                    currency: s(o, "currency"),
                    local: ov_arr(o.get("local")),
                    usd: ov_arr(o.get("usd")),
                })
                .collect()
        })
        .unwrap_or_default();
    RadarReadout {
        stamp: s(v, "stamp"),
        source: s(v, "source"),
        tier: s(v, "tier"),
        n_symbols: v.get("n_symbols").and_then(|x| x.as_i64()).unwrap_or(0),
        refreshed_ms: v.get("refreshed_ms").and_then(|x| x.as_i64()).unwrap_or(0),
        rows,
        catalog,
        breadth,
        overview,
        backfill: BackfillView {
            done: bi("done"),
            total: bi("total"),
            failed: bi("failed"),
            running: bb("running"),
            finished: bb("finished"),
        },
        refreshed: String::new(),
        present: true,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOARD: &str = r#"{
      "stamp":"2026-08-30 07:00:00","source":"binance:linear+binance:spot","tier":"A",
      "n_symbols":577,"n_rows":2,"refreshed_ms":1312,
      "backfill":{"done":37,"total":574,"failed":2,"running":true,"finished":false},
      "catalog":{"markets":[{"code":"america","label":"美国","region":"美洲"},
                            {"code":"japan","label":"日本","region":"亚太"}],
                 "types":[{"code":"stock","label":"股票"},{"code":"fund","label":"ETF"}],
                 "indices":[{"market":"america","id":"america@SPX","label":"标准普尔500指数"},
                            {"market":"japan","id":"japan@NI225","label":"日经225指数"}],
                 "crypto_cats":[{"code":"","label":"全部加密货币"},{"code":"defi","label":"DeFi"}]},
      "breadth":[{"market":"japan","n":300,"adv":194,"dec":103,"unch":3,
                  "new_high":1,"new_low":0,"net_new_high":1,"above_ma200":231,"ma200_n":300,
                  "adv_pct":0.653,"ad_ratio":1.883,"above_ma200_pct":0.77},
                 {"market":"x","n":1,"adv":1,"dec":0,"unch":0,"new_high":0,"new_low":0,
                  "net_new_high":0,"above_ma200":0,"ma200_n":0,
                  "adv_pct":1.0,"ad_ratio":null,"above_ma200_pct":null}],
      "overview":[{"market":"india","ticker":"NSE:NIFTY","label":"印度 Nifty 50","currency":"INR",
                   "local":{"日":-0.0039,"YTD":-0.0834},"usd":{"日":-0.0018,"YTD":-0.1394}},
                  {"market":"japan","ticker":"TVC:NI225","label":"日本 日经 225","currency":"JPY",
                   "local":{"日":0.0008},"usd":{}}],
      "rows":[
        {"symbol":"KNCUSDT","venue":"binance:linear","tier":"A","asset":"crypto","cats":["layer-1","defi"],"price":0.42,"quote_vol_24h":5.1e6,
         "ret":{"1m":0.00165,"24h":0.042},"z_ret":{"1m":3.55},
         "z_vol":null,"z_cnt":null,"sigma_ok":true,"z_provisional":false},
        {"symbol":"NVDA","venue":"tv:america","tier":"C","asset":"equity","name":"NVIDIA Corporation",
         "sector":"Electronic Technology","country":"United States","currency":"USD","mcap":5.2e12,
         "price":1.0,"quote_vol_24h":2e6,
         "cats":[],
         "m":{"Perf.YTD":16.3,"gap":-0.6,"relative_volume_10d_calc":1.42,"market_cap_basic":5.2e12},
         "ret":{"1m":0.001},"z_ret":{"1m":1.2},
         "z_vol":4.4,"z_cnt":2.1,"sigma_ok":false,"z_provisional":true}
      ]}"#;

    fn board() -> RadarReadout {
        parse_board(&serde_json::from_str(BOARD).unwrap())
    }

    #[test]
    fn parses_header_and_rows() {
        let r = board();
        assert_eq!(r.tier, "A");
        assert_eq!(r.n_symbols, 577);
        assert_eq!(r.refreshed_ms, 1312);
        assert_eq!(r.rows.len(), 2);
        assert!(r.present);
    }

    #[test]
    fn absent_windows_stay_none_not_zero() {
        let r = board();
        let row = &r.rows[0];
        assert_eq!(row.ret[0], Some(0.00165)); // 1m
        assert!(row.ret[1].is_none(), "5m 缺席必须是 None，不能补 0");
        assert!(row.ret[2].is_none());
        assert_eq!(row.ret[5], Some(0.042)); // 24h
        assert!(row.z_ret[1].is_none());
    }

    #[test]
    fn degradation_flags_round_trip() {
        let r = board();
        assert!(r.rows[0].trustworthy());
        assert!(!r.rows[1].trustworthy(), "provisional 行不可当真");
        assert!(r.rows[1].z_provisional);
        assert!(!r.rows[1].sigma_ok);
    }

    #[test]
    fn headline_z_falls_back_to_1m() {
        let r = board();
        assert_eq!(r.rows[0].headline_z(), Some(3.55));
    }

    #[test]
    fn parses_per_row_tier_and_reference_data() {
        let r = board();
        assert_eq!(r.rows[0].tier, "A", "加密应是 A 档");
        let eq = &r.rows[1];
        assert_eq!(eq.tier, "C", "延迟股票必须标 C，不能被当成实时");
        assert_eq!(eq.asset, "equity");
        assert_eq!(r.rows[0].asset, "crypto");
        assert_eq!(eq.name, "NVIDIA Corporation");
        assert_eq!(eq.sector, "Electronic Technology");
        assert_eq!(eq.country, "United States");
        assert_eq!(eq.currency, "USD");
        assert!((eq.mcap - 5.2e12).abs() < 1.0);
        // 加密行没有这些参考数据，应是空而不是乱填
        assert_eq!(r.rows[0].country, "");
    }

    #[test]
    fn parses_breadth_with_null_ratios() {
        let b = &board().breadth;
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].market, "japan");
        assert_eq!((b[0].adv, b[0].dec, b[0].n), (194, 103, 300));
        assert!((b[0].above_ma200_pct.unwrap() - 0.77).abs() < 1e-9);
        // 分母为 0 的比值必须是 None，面板显示「—」而不是 inf
        assert!(b[1].ad_ratio.is_none());
        assert!(b[1].above_ma200_pct.is_none());
        assert_eq!(b[1].adv_pct, Some(1.0));
    }

    #[test]
    fn parses_overview_and_keeps_usd_absent_when_fx_missing() {
        let o = &board().overview;
        assert_eq!(o.len(), 2);
        let ind = &o[0];
        assert_eq!(ind.currency, "INR");
        assert!((ind.local[0].unwrap() - (-0.0039)).abs() < 1e-9);
        assert!((ind.usd[3].unwrap() - (-0.1394)).abs() < 1e-9);
        assert!(ind.local[1].is_none(), "缺的窗口应为 None");
        // 拿不到汇率时美元口径必须全空，绝不退回本币值
        assert!(o[1].usd.iter().all(Option::is_none));
        assert!(o[1].local[0].is_some());
    }

    #[test]
    fn request_body_carries_only_the_current_selection() {
        // 累积式请求会让守护的请求数随点击单调增长，最后把非官方接口打爆
        let b = request_body("japan", &["stock", "fund"]);
        let v: serde_json::Value = serde_json::from_str(&b).unwrap();
        let s = v["sources"].as_array().unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0]["market"], "japan");
        assert_eq!(s[0]["security_type"], "stock");
        assert_eq!(s[1]["security_type"], "fund");
    }

    #[test]
    fn request_body_carries_index_source_ids() {
        let v: serde_json::Value =
            serde_json::from_str(&request_body("america@SPX", &["stock"])).unwrap();
        assert_eq!(v["sources"][0]["market"], "america@SPX");
    }

    #[test]
    fn empty_selection_requests_nothing() {
        // 选「全部市场」时不该请求任何额外来源——那等于要求守护拉全世界
        let v: serde_json::Value =
            serde_json::from_str(&request_body("", &["stock"])).unwrap();
        assert!(v["sources"].as_array().unwrap().is_empty());
    }

    #[test]
    fn parses_the_source_catalog() {
        // 面板据此建「来源」下拉；漏了的话下拉只剩已加载的那几个市场
        let c = board().catalog;
        assert_eq!(c.markets.len(), 2);
        assert_eq!(c.markets[0].code, "america");
        assert_eq!(c.markets[0].label, "美国");
        assert_eq!(c.markets[0].region, "美洲");
        assert_eq!(c.types.len(), 2);
        assert_eq!(c.crypto_cats[0].code, "", "首项必须是「全部」");
        assert_eq!(c.indices.len(), 2);
        assert_eq!(c.indices[0].code, "america@SPX");
        assert_eq!(c.indices[0].label, "标准普尔500指数");
        assert_eq!(c.indices[0].region, "america", "指数要知道自己属于哪个市场");
    }

    #[test]
    fn parses_crypto_categories_per_row() {
        let b = board();
        assert_eq!(b.rows[0].cats, vec!["layer-1", "defi"]);
        assert!(b.rows[1].cats.is_empty(), "股票用板块，不该有加密分类");
    }

    #[test]
    fn missing_catalog_is_empty_not_a_parse_failure() {
        // 老版守护写的快照没有 catalog——面板必须照常工作
        let r = parse_board(&serde_json::from_str(r#"{"stamp":"t","rows":[]}"#).unwrap());
        assert!(r.catalog.markets.is_empty());
    }

    #[test]
    fn parses_tv_metric_map() {
        let eq = &board().rows[1];
        assert_eq!(eq.m.get("Perf.YTD"), Some(&16.3));
        assert_eq!(eq.m.get("gap"), Some(&-0.6));
        assert_eq!(eq.m.get("relative_volume_10d_calc"), Some(&1.42));
        assert!(
            eq.m.get("premarket_change").is_none(),
            "没给的指标必须缺席——补 0 会把「没有数据」显示成「值是 0」"
        );
        assert!(board().rows[0].m.is_empty(), "加密行没有 m 字段时应为空表");
    }

    #[test]
    fn parses_backfill_progress() {
        let b = board().backfill;
        assert_eq!((b.done, b.total, b.failed), (37, 574, 2));
        assert!(b.running && !b.finished);
        assert!((b.pct() - 37.0 / 574.0).abs() < 1e-9);
    }

    #[test]
    fn backfill_absent_is_zeroed_not_a_parse_failure() {
        // 老版守护写的快照没有 backfill 字段——面板必须照常工作
        let v: serde_json::Value =
            serde_json::from_str(r#"{"stamp":"t","rows":[]}"#).unwrap();
        let r = parse_board(&v);
        assert_eq!(r.backfill.total, 0);
        assert!(!r.backfill.running);
        assert!((r.backfill.pct() - 0.0).abs() < 1e-12, "total=0 时不该除零");
    }

    #[test]
    fn missing_board_is_not_present() {
        let r = parse_board(&serde_json::json!({}));
        assert_eq!(r.rows.len(), 0);
        assert_eq!(r.n_symbols, 0);
    }
}
