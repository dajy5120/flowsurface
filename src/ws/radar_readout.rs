//! 全市场雷达面板只读快照（docs/22 §4.2）。
//!
//! **独立新增面板**（`Content::MarketMap`）——不改任何既有面板/readout。数据源（全只读）：
//! `$XDG_RUNTIME_DIR/wealthspring/radar_board.json`（`ws-radar` 守护产出，
//! 落在 tmpfs 上、不写硬盘，见 `runtime_dir_from`）。沿用
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
    /// 文本值的指标列（评级 / 财务期间 / K 线形态）。与 `m` 分开是因为它们
    /// 不是数字——混进 `m` 会让排序和着色拿到没法比较的值。
    pub t: std::collections::HashMap<String, String>,
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

/// 一个列组标签。
#[derive(Default, Clone, PartialEq)]
pub struct ColumnTab {
    pub key: String,
    pub label: String,
    pub cols: Vec<String>,
}

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
    /// Screener 的列组（对齐 TV 的 Overview/Performance/…）。随快照下发，
    /// 面板不硬编码列名——加列只改守护侧一处。
    pub tabs: Vec<ColumnTab>,
    /// 百分数口径的指标键。
    pub pct_keys: std::collections::HashSet<String>,
    /// 本币金额口径的指标键（已换算成美元）。
    /// 值是 Unix 秒的指标（财报日期）——不单列一类会显示成十位整数。
    pub date_keys: std::collections::HashSet<String>,
    pub money_keys: std::collections::HashSet<String>,
    pub markets: Vec<CatalogItem>,
    /// 指数成分股来源。`code` 是来源 id（`america@SPX`），`region` 借用为所属市场。
    pub indices: Vec<CatalogItem>,
    pub types: Vec<CatalogItem>,
    pub crypto_cats: Vec<CatalogItem>,
}

#[derive(Default, Clone)]
pub struct RadarReadout {
    /// 数据版本号，每轮轮询递增。**只用来判断「数据变没变」**——
    /// 树图的 canvas 缓存据此决定要不要重画（见 radar_view 的 TREEMAP_CACHE）。
    pub generation: u64,
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
    /// 慢层快照的时间戳（股票 60s 一刷，与热层不同步——面板要分别标注，
    /// 否则会拿热层的时间当成股票数据的时间）。
    pub slow_stamp: String,
    pub present: bool,
    /// 守护 service 状态。后台 poller 刷——**不能在 view 里查**，那是每帧一次
    /// systemctl 子进程（docs/20 §19.2 的每帧开销教训）。
    pub svc: super::svcctl::UnitState,
}

static READOUT: OnceLock<Mutex<std::sync::Arc<RadarReadout>>> = OnceLock::new();
static POLLER: OnceLock<()> = OnceLock::new();
static WAKER: super::svcctl::Waker = super::svcctl::Waker::new();

pub const RADAR_SVC: &str = "ws-radar.service";

/// 快照落点：**内存文件系统，不写硬盘**。
///
/// 快照是纯易失数据，重启就该重新生成，没有一份值得留在盘上；写盘的唯一
/// 后果是磨损 SSD（实测 0.23 MB/s、一天 20GB）。按顺序取第一个可用的：
///   1. `$XDG_RUNTIME_DIR/wealthspring`（systemd 给每个用户挂的 tmpfs，0700、登出即清）
///   2. `/dev/shm/wealthspring-$USER`（tmpfs 兜底）
///   3. `$HOME/ws-data/live`（**会写盘**，只在系统没有 tmpfs 时走到）
///
/// ⚠ 守护侧 `wealthspring-radar/src/runtime.rs` 有一份**逐字相同**的实现。
/// 两个 crate 互不依赖，只能各写一份，靠两边同名的单测钉住同样的行为——
/// 一边改了另一边没改，表现是面板永远「等待数据」而守护日志一切正常。
fn runtime_dir_from(xdg: Option<&str>, user: Option<&str>, home: Option<&str>) -> PathBuf {
    // 空字符串按「没设」处理：systemd 里未设的变量常常是空串而不是不存在
    fn ok(s: Option<&str>) -> Option<&str> {
        s.filter(|v| !v.is_empty())
    }
    if let Some(x) = ok(xdg) {
        return PathBuf::from(x).join("wealthspring");
    }
    if std::path::Path::new("/dev/shm").is_dir() {
        return PathBuf::from(format!("/dev/shm/wealthspring-{}", ok(user).unwrap_or("ws")));
    }
    PathBuf::from(ok(home).unwrap_or("/tmp")).join("ws-data/live")
}

fn runtime_dir() -> PathBuf {
    runtime_dir_from(
        std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
        std::env::var("USER").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

fn board_path() -> PathBuf {
    std::env::var("WS_RADAR_BOARD")
        .map(PathBuf::from)
        .unwrap_or_else(|_| runtime_dir().join("radar_board.json"))
}

/// 慢层快照（股票/ETF + 宽度 + 总览），与热层同目录、文件名加 `_slow`。
///
/// 拆两个文件是为写盘量：加密 5s 一变、股票 60s 才变，合成一个的话股票那
/// 五十多列基本面指标会跟着每 5s 重写（实测 1.94 MB/s、一天 167GB）。
fn slow_path() -> PathBuf {
    let p = board_path();
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("radar_board");
    p.with_file_name(format!("{stem}_slow.json"))
}

/// 当前读数。**返回 `Arc`，不深拷贝**。
///
/// 视图每帧都调它一次。原来返回的是 `RadarReadout` 的深拷贝——4189 行、每行
/// 一个上百项的 `HashMap<String, f64>` 外加文本段和七八个 `String`，一帧要
/// 重新分配几千万字节。列数从 72 涨到 114 之后这笔开销翻倍，主线程直接
/// 打满一个核（实测 99.9%）。换成 `Arc` 后每帧只是一次引用计数加一。
pub fn snapshot() -> std::sync::Arc<RadarReadout> {
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
        .unwrap_or_else(|_| runtime_dir().join("radar_request.json"))
}

/// 纯构造（可单测）：选中的来源 → 请求 JSON。
///
/// 只带**当前选中项**，不累积：累积会让守护的请求数随用户点击单调增长，
/// 最后把非官方接口打爆。配置里的默认市场恒在，选中的额外加一个。
pub fn request_body(
    source: &str,
    security_types: &[&str],
    filters: &[super::radar_filter::Wire],
) -> String {
    let sources: Vec<serde_json::Value> = if source.is_empty() {
        Vec::new()
    } else {
        security_types
            .iter()
            .map(|t| serde_json::json!({"market": source, "security_type": t}))
            .collect()
    };
    // 筛选必须**下推到服务端**：只在守护抓回的样本里筛，等于只在按市值前 N 只
    // 里找。实测全美「P/E<10 且 息>4%」命中 87 只，样本里只有 2 只。
    let f: Vec<serde_json::Value> = filters
        .iter()
        .map(|w| match w.text {
            Some(t) => serde_json::json!({"field": w.field, "op": w.op, "text": t}),
            None => serde_json::json!({"field": w.field, "op": w.op, "right": w.right}),
        })
        .collect();
    serde_json::json!({ "sources": sources, "filters": f }).to_string()
}

/// 把选中的来源写给守护（原子写，同快照）。
///
/// 内容没变就不写：面板每帧都会走到这里，每帧重写文件既浪费也会让守护看到抖动。
pub fn write_request(
    source: &str,
    security_types: &[&str],
    filters: &[super::radar_filter::Wire],
) {
    static LAST: Mutex<String> = Mutex::new(String::new());
    let body = request_body(source, security_types, filters);
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
        std::thread::spawn(|| {
            let mut svc = super::svcctl::UnitState::default();
            let mut tick = 0u32;
            loop {
                let mut snap = poll_once();
                snap.generation = tick as u64;
                // `systemctl show` 是一次 **fork+exec**。每 2s 一次的话，光它就贡献了
                // 每秒近百次读系统调用（实测），而服务状态几乎不变。降到 10s 一次，
                // 中间沿用上次结果；面板按钮启停后会立刻 WAKER 唤醒，不必靠轮询看到。
                if tick % 5 == 0 {
                    svc = super::svcctl::query(RADAR_SVC);
                }
                tick = tick.wrapping_add(1);
                snap.svc = svc.clone();
                let lock =
                    READOUT.get_or_init(|| Mutex::new(std::sync::Arc::new(RadarReadout::default())));
                if let Ok(mut g) = lock.lock() {
                    *g = std::sync::Arc::new(snap);
                }
                WAKER.wait(Duration::from_secs(2));
            }
        });
    });
}

fn read_json(p: PathBuf) -> Option<serde_json::Value> {
    std::fs::read_to_string(p)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
}

fn poll_once() -> RadarReadout {
    let refreshed = chrono::Local::now().format("%H:%M:%S").to_string();
    let Some(hot) = read_json(board_path()) else {
        return RadarReadout { refreshed, ..Default::default() };
    };
    let mut st = RadarReadout { refreshed, ..parse_board(&hot) };
    // 慢层可缺（股票层没开、或还没写第一轮）——热层照常显示，不整个作废
    if let Some(slow) = read_json(slow_path()) {
        let s = parse_board(&slow);
        st.rows.extend(s.rows);
        st.breadth = s.breadth;
        st.overview = s.overview;
        // 标的总数是两层之和；单层的 n_symbols 只算自己那部分
        st.n_symbols += s.n_symbols;
        st.slow_stamp = s.stamp;
    }
    st
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
                    t: o
                        .get("t")
                        .and_then(|x| x.as_object())
                        .map(|mm| {
                            mm.iter()
                                .filter_map(|(k, v)| {
                                    v.as_str().filter(|s| !s.is_empty()).map(|s| (k.clone(), s.to_string()))
                                })
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
    let cat = |k: &str| v.get("catalog").and_then(|c| c.get(k));
    let tabs: Vec<ColumnTab> = cat("tabs")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .map(|o| ColumnTab {
                    key: s(o, "key"),
                    label: s(o, "label"),
                    cols: o
                        .get("cols")
                        .and_then(|x| x.as_array())
                        .map(|c| c.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                        .unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default();
    let keys = |k: &str| -> std::collections::HashSet<String> {
        cat("units")
            .and_then(|u| u.get(k))
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default()
    };
    let catalog = Catalog {
        tabs,
        pct_keys: keys("pct"),
        money_keys: keys("money"),
        date_keys: keys("date"),
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

    #[test]
    fn text_valued_columns_parse_into_their_own_map() {
        // 评级返回 "StrongBuy"、财务期间返回 "2026-Q2"；当数字解析会整列「—」
        let j = r#"{"stamp":"s","source":"tv","rows":[{"symbol":"NVDA","venue":"tv:america:stock",
          "m":{"close":217.4},"t":{"AnalystRating":"StrongBuy","fiscal_period_current":"2026-Q2","x":""}}]}"#;
        let r = parse_board(&serde_json::from_str(j).unwrap());
        let row = &r.rows[0];
        assert_eq!(row.t.get("AnalystRating").map(String::as_str), Some("StrongBuy"));
        assert_eq!(row.t.get("fiscal_period_current").map(String::as_str), Some("2026-Q2"));
        assert!(row.t.get("x").is_none(), "空串等于没有，不该占一格");
        assert_eq!(row.m.get("close").copied(), Some(217.4));
        assert!(row.m.get("AnalystRating").is_none());
    }

    #[test]
    fn a_snapshot_without_the_text_map_still_parses() {
        // 守护可能是旧版本，没有 `t` 段——不能因此整份快照解析失败
        let j = r#"{"stamp":"s","source":"tv","rows":[{"symbol":"A","venue":"v","m":{"close":1.0}}]}"#;
        let r = parse_board(&serde_json::from_str(j).unwrap());
        assert_eq!(r.rows.len(), 1);
        assert!(r.rows[0].t.is_empty());
    }

    #[test]
    fn request_body_carries_the_pushdown_filters() {
        use super::super::radar_filter::Wire;
        let f = vec![
            Wire { field: "price_earnings_ttm", op: "eless", right: vec![10.0], text: None },
            Wire { field: "sector", op: "equal", right: vec![], text: Some("Finance") },
        ];
        let v: serde_json::Value =
            serde_json::from_str(&request_body("america", &["stock"], &f)).unwrap();
        let a = v["filters"].as_array().unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(a[0]["field"], "price_earnings_ttm");
        assert_eq!(a[0]["right"][0], 10.0);
        // 文本条件走 text 字段，不塞进 right——守护按字段名分派算子
        assert_eq!(a[1]["text"], "Finance");
        assert!(a[1].get("right").is_none());
    }

    #[test]
    fn request_body_always_has_a_filters_key() {
        // 缺键的话守护那边 serde 用 default 也能过，但显式写出来才看得出「就是没筛」
        let v: serde_json::Value =
            serde_json::from_str(&request_body("america", &["stock"], &[])).unwrap();
        assert!(v["filters"].as_array().unwrap().is_empty());
    }

    // ── 快照落点（与守护侧 runtime.rs 逐字相同的行为，两边同名单测钉住）──

    #[test]
    fn xdg_runtime_dir_wins() {
        assert_eq!(
            runtime_dir_from(Some("/run/user/1000"), Some("dajy"), Some("/home/dajy")),
            PathBuf::from("/run/user/1000/wealthspring")
        );
    }

    #[test]
    fn empty_env_counts_as_unset() {
        // systemd 里未设的变量常常是空串；当成已设会得到 "/wealthspring"
        let p = runtime_dir_from(Some(""), Some("dajy"), Some("/home/dajy"));
        assert_ne!(p, PathBuf::from("/wealthspring"));
        assert!(p.is_absolute());
    }

    #[test]
    fn falls_back_to_shm_when_xdg_missing() {
        if std::path::Path::new("/dev/shm").is_dir() {
            assert_eq!(
                runtime_dir_from(None, Some("dajy"), Some("/home/dajy")),
                PathBuf::from("/dev/shm/wealthspring-dajy")
            );
            assert_eq!(
                runtime_dir_from(None, None, Some("/home/dajy")),
                PathBuf::from("/dev/shm/wealthspring-ws")
            );
        }
    }

    #[test]
    fn never_returns_a_relative_or_empty_path() {
        // 相对路径会让守护和面板按各自的 cwd 解析到不同文件，
        // 表现是面板一直「等待数据」而守护日志一切正常
        for c in [
            (Some("/run/user/1000"), Some("u"), Some("/home/u")),
            (None, Some("u"), Some("/home/u")),
            (None, None, None),
        ] {
            let p = runtime_dir_from(c.0, c.1, c.2);
            assert!(p.is_absolute() && p.as_os_str().len() > 1, "{p:?}");
        }
    }

    #[test]
    fn default_location_is_not_on_disk() {
        // 这几条测试存在的理由：默认落点必须在 tmpfs 上
        let d = runtime_dir();
        let tmpfs = d.starts_with("/run/user") || d.starts_with("/dev/shm");
        assert!(
            tmpfs || !std::path::Path::new("/dev/shm").is_dir(),
            "有 tmpfs 却把快照默认读写到了 {d:?}"
        );
    }

    #[test]
    fn hot_and_slow_and_request_share_one_directory() {
        // 三个文件必须同目录：慢层路径是从热层派生的，
        // 控制通道分家的话面板改的来源守护看不到
        assert_eq!(board_path().parent(), slow_path().parent());
        assert_eq!(board_path().parent(), request_path().parent());
    }
    use super::*;

    const BOARD: &str = r#"{
      "stamp":"2026-08-30 07:00:00","source":"binance:linear+binance:spot","tier":"A",
      "n_symbols":577,"n_rows":2,"refreshed_ms":1312,
      "backfill":{"done":37,"total":574,"failed":2,"running":true,"finished":false},
      "catalog":{"markets":[{"code":"america","label":"美国","region":"美洲"},
                            {"code":"japan","label":"日本","region":"亚太"}],
                 "types":[{"code":"stock","label":"股票"},{"code":"fund","label":"ETF"}],
                 "tabs":[{"key":"overview","label":"概览","cols":["close","change","volume"]},
                         {"key":"valuation","label":"估值","cols":["price_earnings_ttm","price_book_fq"]}],
                 "units":{"pct":["change","Perf.YTD"],"money":["close","market_cap_basic"]},
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
        let b = request_body("japan", &["stock", "fund"], &[]);
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
            serde_json::from_str(&request_body("america@SPX", &["stock"], &[])).unwrap();
        assert_eq!(v["sources"][0]["market"], "america@SPX");
    }

    #[test]
    fn empty_selection_requests_nothing() {
        // 选「全部市场」时不该请求任何额外来源——那等于要求守护拉全世界
        let v: serde_json::Value =
            serde_json::from_str(&request_body("", &["stock"], &[])).unwrap();
        assert!(v["sources"].as_array().unwrap().is_empty());
    }

    #[test]
    fn slow_layer_absence_does_not_void_the_hot_layer() {
        // 股票层没开、或慢层还没写第一轮时，加密照常显示
        let hot = parse_board(&serde_json::from_str(
            r#"{"stamp":"t","layer":"hot","rows":[{"symbol":"BTCUSDT","asset":"crypto"}]}"#,
        ).unwrap());
        assert_eq!(hot.rows.len(), 1);
        assert!(hot.breadth.is_empty());
        assert!(hot.slow_stamp.is_empty());
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
        assert_eq!(c.tabs.len(), 2);
        assert_eq!(c.tabs[0].key, "overview");
        assert_eq!(c.tabs[0].cols, vec!["close", "change", "volume"]);
        assert!(c.pct_keys.contains("Perf.YTD"));
        assert!(c.money_keys.contains("market_cap_basic"));
        assert!(!c.pct_keys.contains("market_cap_basic"), "金额不是百分数");
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
