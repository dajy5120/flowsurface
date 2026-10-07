//! 「订单流层析」（docs/40 P2）：OFMS-10 的独立面板——选一个数据窗口，看 L0–L10 在同一时间轴上的泳道、
//! 事件的因果链、以及同类事件之后价格怎么走。
//!
//! 数据只有两个来源（面板不算任何东西）：
//! - 窗口输出：`python -m factory.ofms.run … --depth --json`（Rust 特征引擎回放 + OFMS 检测器，按窗口缓存）→
//!   `ofms_events.csv` / `ofms_states.csv` / `ofms_depth.csv`；
//! - 登记表与特征字典：`target/release/ws-features-dict`（层、事件定义、观测 / 推断、458 条特征的归层）。

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use serde::Deserialize;

use super::data_picker::{BSource, DataPick, DataPickMsg, Pipeline, PickOpts, Purpose, Selection, TimeMode};

// ── 数据 ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct OfEvent {
    pub id: u64,
    pub parent: Option<u64>,
    pub chain: u64,
    /// 毫秒
    pub t: i64,
    pub layer: u8,
    pub key: String,
    pub dir: i8,
    pub price: f64,
    pub size: f64,
    pub strength: f64,
    pub nature: String,
    pub detail: String,
}

#[derive(Debug, Clone, Default)]
pub struct OfStates {
    pub t: Vec<i64>,
    pub mid: Vec<f64>,
    pub spread_bps: Vec<f64>,
    pub depth_bid: Vec<f64>,
    pub depth_ask: Vec<f64>,
    pub buy: Vec<f64>,
    pub sell: Vec<f64>,
    pub cvd: Vec<f64>,
    pub aggr_z: Vec<f64>,
    pub residual_z: Vec<f64>,
    pub response: Vec<String>,
    pub regime: Vec<String>,
    pub rv_ratio: Vec<f64>,
    pub vwap: Vec<f64>,
    pub level_hi: Vec<f64>,
    pub level_lo: Vec<f64>,
    pub synced: Vec<bool>,
}

/// 每秒的深度快照：(毫秒, 卖侧?, 价, 量)
#[derive(Debug, Clone, Default)]
pub struct OfDepth {
    pub rows: Vec<(i64, bool, f64, f64)>,
    pub max_qty: f64,
}

#[derive(Debug, Clone, Default)]
pub struct OfData {
    pub dir: PathBuf,
    pub title: String,
    pub events: Vec<OfEvent>,
    pub by_id: HashMap<u64, usize>,
    pub states: OfStates,
    pub depth: Option<OfDepth>,
}

/// `ws-features-dict` 的 OFMS 部分 + 特征归层。
#[derive(Deserialize, Debug, Clone, Default)]
pub struct DictLayer {
    pub id: u8,
    pub key: String,
    pub name: String,
    pub question: String,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct DictEvent {
    pub key: String,
    pub layer: u8,
    pub name: String,
    pub definition: String,
    pub nature: String,
    #[serde(default)]
    pub params: BTreeMap<String, f64>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct DictOfms {
    pub layers: Vec<DictLayer>,
    pub events: Vec<DictEvent>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct DictFeature {
    pub key: String,
    pub name_cn: String,
    pub stage: String,
    pub family: String,
    pub formula: String,
    pub hypothesis: String,
    pub prior: String,
    pub ofms_layer: u8,
    #[serde(default)]
    pub inputs: Vec<String>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct Dict {
    pub features: Vec<DictFeature>,
    pub ofms: DictOfms,
}

impl Dict {
    pub fn event(&self, key: &str) -> Option<&DictEvent> {
        self.ofms.events.iter().find(|e| e.key == key)
    }
    pub fn layer_name(&self, id: u8) -> String {
        self.ofms.layers.iter().find(|l| l.id == id).map(|l| l.name.clone()).unwrap_or_else(|| format!("L{id}"))
    }
}

/// P3 响应表（`factory.ofms.response` 的 `response.json`）。
#[derive(Deserialize, Debug, Clone, Default)]
pub struct RespRow {
    pub kind: String,
    pub cell: String,
    pub h: u32,
    pub n: u64,
    pub clusters: u64,
    pub cost_bp: f64,
    #[serde(default)]
    pub mean_bp: Option<f64>,
    #[serde(default)]
    pub ci_lo: Option<f64>,
    #[serde(default)]
    pub ci_hi: Option<f64>,
    #[serde(default)]
    pub t_mid: Option<f64>,
    #[serde(default)]
    pub net_bp: Option<f64>,
    #[serde(default)]
    pub side: Option<String>,
    pub verdict: String,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct RespTrans {
    pub from: String,
    pub to: String,
    pub n: u64,
    pub mean30_bp: f64,
    #[serde(default)]
    pub se30_bp: Option<f64>,
}

/// 按周滚动的 30s 效应（docs/40 §7 alpha 衰减）。
#[derive(Deserialize, Debug, Clone, Default)]
pub struct RespWeek {
    pub week: String,
    pub mean_bp: f64,
    pub n: u64,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct RespRolling {
    pub cell: String,
    pub full_bp: f64,
    pub decayed: bool,
    pub weeks: Vec<RespWeek>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct Resp {
    pub summary: serde_json::Value,
    pub rows: Vec<RespRow>,
    pub transitions: Vec<RespTrans>,
    #[serde(default)]
    pub rolling: Vec<RespRolling>,
}

/// 描述语言条目（`python -m factory.ofms.dsl` 的导出）。
#[derive(Deserialize, Debug, Clone, Default)]
pub struct Setup {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub verdict: String,
    #[serde(default)]
    pub signal: String,
    #[serde(default)]
    pub replaces: Option<String>,
    #[serde(default)]
    pub triggers: Vec<String>,
    #[serde(default)]
    pub layers: BTreeMap<String, String>,
    #[serde(default)]
    pub error: Option<String>,
}

/// 一个描述文件在当前窗口上的逐笔（`python -m factory.ofms.quick` 的输出）。
#[derive(Deserialize, Debug, Clone, Default)]
pub struct DslTrade {
    pub entry_ts: i64,
    pub exit_ts: i64,
    pub side: i8,
    pub net_bp: f64,
    pub gross_bp: f64,
    #[serde(default)]
    pub exit_kind: String,
}

#[derive(Debug, Clone, Default)]
pub struct Overlay {
    pub spec: String,
    pub trades: Vec<DslTrade>,
    pub fee_rt_bp: f64,
}

// ── 状态 ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OfTab {
    #[default]
    Timeline,
    Response,
    Setups,
    Dictionary,
}

#[derive(Debug, Clone, PartialEq)]
pub enum OfMsg {
    Pick(DataPickMsg),
    Run,
    Tab(OfTab),
    ToggleLane(u8),
    Select(Option<u64>),
    DictLayer(Option<u8>),
    DictSearch(String),
    /// 响应表：event / state / trans
    RespKind(String),
    /// 响应表：只看有信息（越过成本 + 有信息未越过成本）
    RespInformative(bool),
    RespReload,
    /// 入场形态矩阵：把这个描述文件在当前窗口上跑一遍，叠加到 L6–L10 泳道
    Overlay(String),
    ClearOverlay,
    /// P6 实时观察：开 / 关（只读常驻引擎落的 `ofms/live/<品种>/<日>/`）
    Live(bool),
    /// 实时模式显示最近多少分钟
    LiveMinutes(u32),
}

#[derive(Default)]
struct St {
    pick: Option<DataPick>,
    running: bool,
    note: String,
    data: Option<Arc<OfData>>,
    tab: OfTab,
    hidden: BTreeSet<u8>,
    selected: Option<u64>,
    dict: Option<Arc<Dict>>,
    dict_loading: bool,
    /// 打开时自动读最近一次生成的窗口（只试一次）。
    restored: bool,
    resp: Option<Arc<Resp>>,
    resp_err: String,
    resp_loading: bool,
    resp_kind: String,
    resp_informative: bool,
    setups: Option<Arc<Vec<Setup>>>,
    setups_err: String,
    setups_loading: bool,
    overlay: Option<Arc<Overlay>>,
    overlay_running: bool,
    live: bool,
    live_minutes: u32,
    /// 实时 tail 线程的代号：关掉 / 重开时换代，旧线程看到不是自己的代号就退出
    live_gen: u64,
    dict_err: String,
    dict_layer: Option<u8>,
    dict_search: String,
}

static ST: OnceLock<Mutex<St>> = OnceLock::new();

fn with<R>(f: impl FnOnce(&mut St) -> R) -> Option<R> {
    ST.get_or_init(|| {
        // 样张截图（docs/35）用：WS_OFMS_TAB=response / setups / dictionary 指定开页
        let tab = match std::env::var("WS_OFMS_TAB").as_deref() {
            Ok("response") => OfTab::Response,
            Ok("setups") => OfTab::Setups,
            Ok("dictionary") => OfTab::Dictionary,
            _ => OfTab::Timeline,
        };
        Mutex::new(St { tab, ..St::default() })
    })
    .lock()
    .ok()
    .map(|mut g| f(&mut g))
}

pub fn opts() -> PickOpts {
    PickOpts {
        purpose: Purpose::Compute,
        sources: Some(vec![BSource::Purchased]),
        vendors: Some(vec!["tardis", "databento"]),
        time: TimeMode::Window,
        local_only: true,
        hide_types: true,
    }
}

fn default_pick() -> DataPick {
    let mut p = DataPick::default();
    for m in [DataPickMsg::Pipeline(Pipeline::B), DataPickMsg::Source(BSource::Purchased)] {
        p.update(m);
    }
    p.minutes = 60;
    p
}

#[derive(Clone, Default)]
pub struct View {
    pub pick: DataPick,
    pub running: bool,
    pub note: String,
    pub data: Option<Arc<OfData>>,
    pub tab: OfTab,
    pub hidden: BTreeSet<u8>,
    pub selected: Option<u64>,
    pub dict: Option<Arc<Dict>>,
    pub dict_err: String,
    pub dict_layer: Option<u8>,
    pub dict_search: String,
    pub resp: Option<Arc<Resp>>,
    pub resp_err: String,
    pub resp_kind: String,
    pub resp_informative: bool,
    pub setups: Option<Arc<Vec<Setup>>>,
    pub setups_err: String,
    pub overlay: Option<Arc<Overlay>>,
    pub overlay_running: bool,
    pub live: bool,
    pub live_minutes: u32,
}

/// 页签状态角标（docs/41 §3.4）：在生成窗口 / 叠加评估 / 实时 tail。
pub fn busy() -> bool {
    ST.get().and_then(|m| m.lock().ok()).is_some_and(|g| g.running || g.overlay_running || g.live)
}

/// 页面锁定视图（docs/41 §3.3）：订单流层析的四个视图各是一页。
pub const TABS: [(OfTab, &str); 4] =
    [(OfTab::Timeline, "层析时间轴"), (OfTab::Response, "响应表"), (OfTab::Setups, "入场形态矩阵"), (OfTab::Dictionary, "特征字典")];

pub fn lock_tab(label: &str) {
    if let Some((t, _)) = TABS.iter().find(|(_, n)| *n == label) {
        with(|g| {
            if g.tab != *t {
                g.tab = *t;
            }
        });
    }
}

pub fn view() -> View {
    let need_dict = with(|g| {
        let go = g.dict.is_none() && !g.dict_loading && g.dict_err.is_empty();
        if go {
            g.dict_loading = true;
        }
        go
    })
    .unwrap_or(false);
    if need_dict {
        load_dict();
    }
    if with(|g| !std::mem::replace(&mut g.restored, true)).unwrap_or(false) {
        if std::env::var_os("WS_OFMS_LIVE").is_some() {
            // 样张截图（docs/35）用：开页即进实时模式
            handle(OfMsg::Live(true));
        } else {
            restore_latest();
        }
    }
    let need_resp = with(|g| {
        let go = matches!(g.tab, OfTab::Response | OfTab::Setups) && g.resp.is_none() && !g.resp_loading && g.resp_err.is_empty();
        if go {
            g.resp_loading = true;
        }
        go
    })
    .unwrap_or(false);
    if need_resp {
        load_resp();
    }
    let need_setups = with(|g| {
        let go = g.tab == OfTab::Setups && g.setups.is_none() && !g.setups_loading && g.setups_err.is_empty();
        if go {
            g.setups_loading = true;
        }
        go
    })
    .unwrap_or(false);
    if need_setups {
        load_setups();
    }
    with(|g| {
        let p = g.pick.get_or_insert_with(default_pick);
        p.poll();
        View {
            pick: p.clone(),
            running: g.running,
            note: g.note.clone(),
            data: g.data.clone(),
            tab: g.tab,
            hidden: g.hidden.clone(),
            selected: g.selected,
            dict: g.dict.clone(),
            dict_err: g.dict_err.clone(),
            dict_layer: g.dict_layer,
            dict_search: g.dict_search.clone(),
            resp: g.resp.clone(),
            resp_err: g.resp_err.clone(),
            resp_kind: if g.resp_kind.is_empty() { "event".into() } else { g.resp_kind.clone() },
            resp_informative: g.resp_informative,
            setups: g.setups.clone(),
            setups_err: g.setups_err.clone(),
            overlay: g.overlay.clone(),
            overlay_running: g.overlay_running,
            live: g.live,
            live_minutes: if g.live_minutes == 0 { 30 } else { g.live_minutes },
        }
    })
    .unwrap_or_default()
}

pub fn handle(m: OfMsg) {
    match m {
        OfMsg::Pick(pm) => {
            with(|g| g.pick.get_or_insert_with(default_pick).update(pm));
        }
        OfMsg::Run => start_run(),
        OfMsg::Tab(t) => {
            with(|g| g.tab = t);
        }
        OfMsg::ToggleLane(l) => {
            with(|g| {
                if !g.hidden.remove(&l) {
                    g.hidden.insert(l);
                }
            });
        }
        OfMsg::Select(id) => {
            with(|g| g.selected = id);
        }
        OfMsg::DictLayer(l) => {
            with(|g| g.dict_layer = l);
        }
        OfMsg::DictSearch(s) => {
            with(|g| g.dict_search = s);
        }
        OfMsg::RespKind(k) => {
            with(|g| g.resp_kind = k);
        }
        OfMsg::RespInformative(b) => {
            with(|g| g.resp_informative = b);
        }
        OfMsg::RespReload => {
            with(|g| {
                g.resp = None;
                g.resp_err.clear();
                g.setups = None;
                g.setups_err.clear();
            });
        }
        OfMsg::Overlay(id) => start_overlay(id),
        OfMsg::Live(on) => {
            let ticket = with(|g| {
                g.live = on;
                g.live_gen += 1;
                if g.live_minutes == 0 {
                    g.live_minutes = 30;
                }
                if on {
                    g.overlay = None;
                    g.selected = None;
                    g.note = "实时：等常驻引擎的 OFMS 输出…".into();
                }
                g.live_gen
            })
            .unwrap_or(0);
            if on {
                start_live(ticket);
            }
        }
        OfMsg::LiveMinutes(m) => {
            with(|g| g.live_minutes = m.clamp(5, 240));
        }
        OfMsg::ClearOverlay => {
            with(|g| g.overlay = None);
        }
    }
}

fn start_run() {
    let Some(sel) = with(|g| {
        if g.running {
            return None;
        }
        if g.live {
            // 读历史窗口 = 离开实时模式
            g.live = false;
            g.live_gen += 1;
        }
        let p = g.pick.get_or_insert_with(default_pick);
        let sel = p.selection(&opts());
        if sel.is_none() {
            g.note = "先选好：来源 → 市场 → 标的 → 日期 → 时段".into();
        }
        sel
    })
    .flatten() else {
        return;
    };
    let Selection::Local { source, symbol, date, window: Some((start, minutes)), .. } = sel else {
        with(|g| g.note = "只支持本地购买数据的时间窗口".into());
        return;
    };
    with(|g| {
        g.running = true;
        g.note = format!("回放 {symbol} {date} {start} 起 {minutes} 分钟（首次约每小时 2 分钟；之后读缓存）…");
    });
    super::spawn_named("ws-ofms-run", move || {
        let out = std::process::Command::new(super::paths::python())
            .args(["-m", "factory.ofms.run", &symbol, &date, &start, &minutes.to_string(), "--source", &source, "--depth", "--json"])
            .current_dir(super::paths::repo_root())
            .output();
        let r: Result<OfData, String> = match out {
            Ok(o) if o.status.success() => {
                let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap_or_default();
                let dir = PathBuf::from(v["dir"].as_str().unwrap_or_default());
                load_dir(&dir, format!("{symbol} · {date} {start} 起 {minutes} 分钟 · {source}"))
            }
            Ok(o) => Err(String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("回放失败").trim_start_matches("✗ ").to_string()),
            Err(e) => Err(format!("起不来 Python：{e}")),
        };
        with(|g| {
            g.running = false;
            match r {
                Ok(d) => {
                    g.note = format!("{} 个事件、{} 秒状态", d.events.len(), d.states.t.len());
                    g.selected = None;
                    g.data = Some(Arc::new(d));
                    g.overlay = None;
                }
                Err(e) => g.note = e,
            }
        });
    });
}

/// 缓存根：与 `factory/ofms/run.py` 的 ROOT 同一处。
fn cache_root() -> PathBuf {
    std::env::var_os("WS_OFMS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join("ws-data/ofms"))
}

/// 打开面板时读最近一次生成完成的窗口（`meta.json` 里 status=done、修改时间最新的那个）。
fn restore_latest() {
    super::spawn_named("ws-ofms-restore", move || {
        let mut best: Option<(std::time::SystemTime, PathBuf, serde_json::Value)> = None;
        let root = cache_root();
        for src in std::fs::read_dir(&root).into_iter().flatten().flatten() {
            for sym in std::fs::read_dir(src.path()).into_iter().flatten().flatten() {
                for win in std::fs::read_dir(sym.path()).into_iter().flatten().flatten() {
                    let meta = win.path().join("meta.json");
                    let Ok(m) = std::fs::metadata(&meta).and_then(|m| m.modified()) else { continue };
                    if best.as_ref().is_some_and(|b| b.0 >= m) {
                        continue;
                    }
                    let v: serde_json::Value =
                        std::fs::read(&meta).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
                    if v["status"] == "done" {
                        best = Some((m, win.path(), v));
                    }
                }
            }
        }
        let Some((_, dir, v)) = best else { return };
        let title = format!(
            "{} · {} {} 起 {} 分钟 · {}（上次生成）",
            v["symbol"].as_str().unwrap_or("?"),
            v["date"].as_str().unwrap_or("?"),
            v["start"].as_str().unwrap_or("?"),
            v["minutes"],
            v["source"].as_str().unwrap_or("?")
        );
        if let Ok(d) = load_dir(&dir, title) {
            with(|g| {
                if g.data.is_none() && !g.running {
                    g.note = format!("{} 个事件、{} 秒状态", d.events.len(), d.states.t.len());
                    g.data = Some(Arc::new(d));
                }
            });
        }
    });
}

/// 读最新的一份响应表：`<缓存根>/research/*/response.json` 里修改时间最新的（冒烟用的 `smoke` 除外）。
fn load_resp() {
    super::spawn_named("ws-ofms-resp", move || {
        let root = cache_root().join("research");
        let best = std::fs::read_dir(&root)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_name() != "smoke")
            .map(|e| e.path().join("response.json"))
            .filter_map(|p| std::fs::metadata(&p).and_then(|m| m.modified()).ok().map(|m| (m, p)))
            .max_by_key(|x| x.0);
        let r: Result<Resp, String> = match best {
            None => Err(format!("还没有响应表：先跑 python -m factory.ofms.batch，再 python -m factory.ofms.response（输出在 {}）", root.display())),
            Some((_, p)) => std::fs::read(&p)
                .map_err(|e| format!("读不到 {}：{e}", p.display()))
                .and_then(|b| serde_json::from_slice(&b).map_err(|e| format!("响应表解析失败：{e}"))),
        };
        with(|g| {
            g.resp_loading = false;
            match r {
                Ok(x) => g.resp = Some(Arc::new(x)),
                Err(e) => g.resp_err = e,
            }
        });
    });
}

// ── P6 实时观察：tail 常驻引擎的按日输出 ─────────────────────────────

/// 增量读一个只追加的 CSV：从上次的位置读到最后一个完整行；第一次读跳过表头。
#[derive(Default)]
struct Tail {
    path: PathBuf,
    off: u64,
}

impl Tail {
    fn read_new(&mut self) -> String {
        use std::io::{Read, Seek, SeekFrom};
        let Ok(mut f) = std::fs::File::open(&self.path) else { return String::new() };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.off {
            self.off = 0; // 文件被换掉了（不该发生；当作从头来）
        }
        if len == self.off || f.seek(SeekFrom::Start(self.off)).is_err() {
            return String::new();
        }
        let mut buf = Vec::new();
        let _ = f.take(64 << 20).read_to_end(&mut buf);
        let Some(end) = buf.iter().rposition(|b| *b == b'\n') else { return String::new() };
        let mut start = 0;
        if self.off == 0 {
            start = buf.iter().position(|b| *b == b'\n').map_or(end + 1, |i| i + 1); // 表头
        }
        self.off += end as u64 + 1;
        // 解析器都会跳过第一行：补一个空行
        format!("\n{}", String::from_utf8_lossy(&buf[start.min(end + 1)..=end]))
    }
}

/// 常驻引擎 OFMS 输出的根：`<数据根>/ofms/live/`（与 `ws_features` 同一处）。
pub fn live_root() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(|x| PathBuf::from(x).join("wealthspring"))
        .unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join("ws-data"))
        .join("ofms/live")
}

fn utc_day_dir(sym_dir: &Path) -> PathBuf {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let d = chrono::DateTime::<chrono::Utc>::from_timestamp(secs as i64, 0).map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default();
    sym_dir.join(d)
}

/// 只保留最近 `keep_ms` 毫秒。
fn trim(d: &mut OfData, keep_ms: i64) {
    let Some(last) = d.states.t.last().copied() else { return };
    let lo = last - keep_ms;
    let k = d.states.t.partition_point(|t| *t < lo);
    if k > 0 {
        let s = &mut d.states;
        for v in [&mut s.mid, &mut s.spread_bps, &mut s.depth_bid, &mut s.depth_ask, &mut s.buy, &mut s.sell, &mut s.cvd,
                  &mut s.aggr_z, &mut s.residual_z, &mut s.rv_ratio, &mut s.vwap, &mut s.level_hi, &mut s.level_lo] {
            v.drain(..k.min(v.len()));
        }
        s.t.drain(..k);
        s.response.drain(..k.min(s.response.len()));
        s.regime.drain(..k.min(s.regime.len()));
        s.synced.drain(..k.min(s.synced.len()));
    }
    d.events.retain(|e| e.t >= lo);
    if let Some(dp) = d.depth.as_mut() {
        dp.rows.retain(|r| r.0 >= lo);
        dp.max_qty = dp.rows.iter().map(|r| r.3).fold(0.0, f64::max);
    }
    d.by_id = d.events.iter().enumerate().map(|(i, e)| (e.id, i)).collect();
}

fn extend_states(a: &mut OfStates, b: OfStates) {
    a.t.extend(b.t);
    a.mid.extend(b.mid);
    a.spread_bps.extend(b.spread_bps);
    a.depth_bid.extend(b.depth_bid);
    a.depth_ask.extend(b.depth_ask);
    a.buy.extend(b.buy);
    a.sell.extend(b.sell);
    a.cvd.extend(b.cvd);
    a.aggr_z.extend(b.aggr_z);
    a.residual_z.extend(b.residual_z);
    a.response.extend(b.response);
    a.regime.extend(b.regime);
    a.rv_ratio.extend(b.rv_ratio);
    a.vwap.extend(b.vwap);
    a.level_hi.extend(b.level_hi);
    a.level_lo.extend(b.level_lo);
    a.synced.extend(b.synced);
}

/// 每秒 tail 一次今天的三份文件，最近 N 分钟发布成面板数据。代号变了（关掉 / 重开）就退出。
fn start_live(ticket: u64) {
    super::spawn_named("ws-ofms-live", move || {
        let root = live_root();
        let mut day_dir = PathBuf::new();
        let mut tails: [Tail; 3] = Default::default();
        let mut buf = OfData::default();
        loop {
            let Some((on, minutes)) = with(|g| (g.live && g.live_gen == ticket, g.live_minutes.max(5))) else { return };
            if !on {
                return;
            }
            // 品种：live 下的第一个目录（常驻引擎只跑一个品种）
            let sym = std::fs::read_dir(&root).ok().and_then(|it| it.flatten().map(|e| e.path()).filter(|p| p.is_dir()).min());
            let note = match sym {
                None => Some(format!("没有实时输出：{} 不存在——常驻特征引擎（ws-features）没开，或两个行情 feed 没开", root.display())),
                Some(sym_dir) => {
                    let dd = utc_day_dir(&sym_dir);
                    if dd != day_dir {
                        day_dir = dd.clone();
                        tails = [
                            Tail { path: dd.join("ofms_events.csv"), off: 0 },
                            Tail { path: dd.join("ofms_states.csv"), off: 0 },
                            Tail { path: dd.join("ofms_depth.csv"), off: 0 },
                        ];
                        buf = OfData {
                            dir: dd.clone(),
                            title: format!("{} · 实时（常驻引擎）", sym_dir.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()),
                            depth: Some(OfDepth::default()),
                            ..OfData::default()
                        };
                    }
                    let ev = tails[0].read_new();
                    let st = tails[1].read_new();
                    let dp = tails[2].read_new();
                    buf.events.extend(parse_events(&ev));
                    extend_states(&mut buf.states, parse_states(&st));
                    if let Some(d) = buf.depth.as_mut() {
                        d.rows.extend(parse_depth(&dp).rows);
                    }
                    trim(&mut buf, i64::from(minutes) * 60_000);
                    match buf.states.t.last() {
                        None => Some(format!("{} 还没有今天的数据（feed 停着，或刚开）", day_dir.display())),
                        Some(t) => {
                            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
                            let lag = (now - t) as f64 / 1000.0;
                            Some(if lag > 15.0 {
                                format!("实时：最新一秒是 {lag:.0} 秒前——数据停了（feed 断了？）")
                            } else {
                                format!("实时 · 最近 {minutes} 分钟 · {} 个事件 · 延迟 {lag:.1} 秒", buf.events.len())
                            })
                        }
                    }
                }
            };
            let snap = Arc::new(buf.clone());
            let alive = with(|g| {
                if !(g.live && g.live_gen == ticket) {
                    return false;
                }
                if !snap.states.t.is_empty() {
                    g.data = Some(snap);
                }
                if let Some(n) = note {
                    g.note = n;
                }
                true
            })
            .unwrap_or(false);
            if !alive {
                return;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    });
}

fn load_setups() {
    super::spawn_named("ws-ofms-setups", move || {
        let out = std::process::Command::new(super::paths::python())
            .args(["-m", "factory.ofms.dsl"])
            .current_dir(super::paths::repo_root())
            .output();
        let r: Result<Vec<Setup>, String> = match out {
            Ok(o) if o.status.success() => serde_json::from_slice(&o.stdout).map_err(|e| format!("描述文件导出解析失败：{e}")),
            Ok(o) => Err(String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("导出失败").to_string()),
            Err(e) => Err(format!("起不来 Python：{e}")),
        };
        with(|g| {
            g.setups_loading = false;
            match r {
                Ok(x) => g.setups = Some(Arc::new(x)),
                Err(e) => g.setups_err = e,
            }
        });
    });
}

/// 把一个事件轨描述文件在当前窗口上跑一遍（快速档，逐秒），结果叠加到 L6–L10 泳道。
fn start_overlay(id: String) {
    let Some(dir) = with(|g| {
        if g.overlay_running {
            return None;
        }
        let d = g.data.as_ref().map(|d| d.dir.clone());
        if d.is_none() {
            g.note = "先在「层析时间轴」生成 / 读取一个窗口".into();
        } else {
            g.overlay_running = true;
            g.note = format!("在当前窗口上评估 {id} …");
        }
        d
    })
    .flatten() else {
        return;
    };
    super::spawn_named("ws-ofms-overlay", move || {
        let out = std::process::Command::new(super::paths::python())
            .args(["-m", "factory.ofms.quick", &id, &dir.to_string_lossy()])
            .current_dir(super::paths::repo_root())
            .output();
        #[derive(Deserialize)]
        struct F {
            fee_rt_bp: f64,
            trades: Vec<DslTrade>,
        }
        let r: Result<Overlay, String> = match out {
            Ok(o) if o.status.success() => std::fs::read(dir.join(format!("dsl_{id}.json")))
                .map_err(|e| e.to_string())
                .and_then(|b| serde_json::from_slice::<F>(&b).map_err(|e| e.to_string()))
                .map(|f| Overlay { spec: id.clone(), trades: f.trades, fee_rt_bp: f.fee_rt_bp }),
            Ok(o) => Err(String::from_utf8_lossy(&o.stdout).lines().last().unwrap_or("评估失败").trim_start_matches("✗ ").to_string()),
            Err(e) => Err(format!("起不来 Python：{e}")),
        };
        with(|g| {
            g.overlay_running = false;
            match r {
                Ok(o) => {
                    let net: f64 = o.trades.iter().map(|t| t.net_bp).sum();
                    g.note = format!("{}：本窗口 {} 笔，合计净 {net:+.1} bp（往返成本 {:.1} bp）", o.spec, o.trades.len(), o.fee_rt_bp);
                    g.overlay = Some(Arc::new(o));
                    g.tab = OfTab::Timeline;
                }
                Err(e) => g.note = e,
            }
        });
    });
}

fn load_dict() {
    super::spawn_named("ws-ofms-dict", move || {
        let bin = super::paths::repo_root().join("target/release/ws-features-dict");
        let r: Result<Dict, String> = match std::process::Command::new(&bin).output() {
            Ok(o) if o.status.success() => serde_json::from_slice(&o.stdout).map_err(|e| format!("字典解析失败：{e}")),
            Ok(_) => Err("ws-features-dict 运行失败".into()),
            Err(e) => Err(format!("找不到 {}（先 cargo build --release -p wealthspring-features）：{e}", bin.display())),
        };
        with(|g| {
            g.dict_loading = false;
            match r {
                Ok(d) => g.dict = Some(Arc::new(d)),
                Err(e) => g.dict_err = e,
            }
        });
    });
}

// ── 读 CSV ─────────────────────────────────────────────────────────────

fn num(s: &str) -> f64 {
    s.parse::<f64>().unwrap_or(f64::NAN)
}

fn ms(ns: &str) -> i64 {
    ns.parse::<i64>().map(|x| x / 1_000_000).unwrap_or(0)
}

pub fn parse_events(text: &str) -> Vec<OfEvent> {
    text.lines()
        .skip(1)
        .filter_map(|l| {
            let c: Vec<&str> = l.splitn(12, ',').collect();
            if c.len() < 12 {
                return None;
            }
            Some(OfEvent {
                id: c[0].parse().ok()?,
                parent: c[1].parse().ok(),
                chain: c[2].parse().ok()?,
                t: ms(c[3]),
                layer: c[4].parse().ok()?,
                key: c[5].to_string(),
                dir: c[6].parse().unwrap_or(0),
                price: num(c[7]),
                size: num(c[8]),
                strength: num(c[9]),
                nature: c[10].to_string(),
                detail: c[11].to_string(),
            })
        })
        .collect()
}

pub fn parse_states(text: &str) -> OfStates {
    let mut s = OfStates::default();
    for l in text.lines().skip(1) {
        let c: Vec<&str> = l.split(',').collect();
        if c.len() < 22 {
            continue;
        }
        s.t.push(ms(c[0]));
        s.mid.push(num(c[1]));
        s.spread_bps.push(num(c[4]));
        s.depth_bid.push(num(c[5]));
        s.depth_ask.push(num(c[6]));
        s.buy.push(num(c[7]));
        s.sell.push(num(c[8]));
        s.cvd.push(num(c[10]));
        s.aggr_z.push(num(c[11]));
        s.residual_z.push(num(c[14]));
        s.response.push(c[15].to_string());
        s.regime.push(c[16].to_string());
        s.rv_ratio.push(num(c[17]));
        s.vwap.push(num(c[18]));
        s.level_hi.push(num(c[19]));
        s.level_lo.push(num(c[20]));
        s.synced.push(c[21] == "1");
    }
    s
}

pub fn parse_depth(text: &str) -> OfDepth {
    let mut d = OfDepth::default();
    for l in text.lines().skip(1) {
        let c: Vec<&str> = l.split(',').collect();
        if c.len() < 4 {
            continue;
        }
        let q = num(c[3]);
        d.max_qty = d.max_qty.max(q);
        d.rows.push((ms(c[0]), c[1] == "a", num(c[2]), q));
    }
    d
}

fn load_dir(dir: &Path, title: String) -> Result<OfData, String> {
    let rd = |n: &str| std::fs::read_to_string(dir.join(n)).map_err(|e| format!("读不到 {n}：{e}"));
    let events = parse_events(&rd("ofms_events.csv")?);
    let states = parse_states(&rd("ofms_states.csv")?);
    let depth = rd("ofms_depth.csv").ok().map(|t| parse_depth(&t));
    let by_id = events.iter().enumerate().map(|(i, e)| (e.id, i)).collect();
    Ok(OfData { dir: dir.to_path_buf(), title, events, by_id, states, depth })
}

// ── 统计：本窗口里同类事件之后价格怎么走（P3 的研究表是跨窗口、扣成本、去重叠的正式版）──

/// 事件之后 h 秒的中间价变动（bp，按事件方向对齐：dir=+1 时上涨为正）。
pub fn forward_bps(s: &OfStates, t_ms: i64, dir: i8, h_s: i64) -> Option<f64> {
    let i = s.t.partition_point(|x| *x < t_ms);
    let j = s.t.partition_point(|x| *x < t_ms + h_s * 1000);
    if i >= s.t.len() || j >= s.t.len() || j <= i {
        return None;
    }
    let (a, b) = (s.mid[i], s.mid[j]);
    if !(a > 0.0 && b > 0.0) || dir == 0 {
        return None;
    }
    Some((b - a) / a * 1e4 * f64::from(dir))
}

/// 同类事件的前瞻统计：(样本数, 各视界的均值 bp)。
pub fn same_kind_stats(d: &OfData, key: &str, horizons: &[i64]) -> (usize, Vec<Option<f64>>) {
    let evs: Vec<&OfEvent> = d.events.iter().filter(|e| e.key == key && e.dir != 0).collect();
    let means = horizons
        .iter()
        .map(|h| {
            let v: Vec<f64> = evs.iter().filter_map(|e| forward_bps(&d.states, e.t, e.dir, *h)).collect();
            (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
        })
        .collect();
    (evs.len(), means)
}

/// 一条链上的全部事件（时间顺序）。
pub fn chain_of(d: &OfData, chain: u64) -> Vec<&OfEvent> {
    let mut v: Vec<&OfEvent> = d.events.iter().filter(|e| e.chain == chain).collect();
    v.sort_by_key(|e| (e.t, e.id));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_outputs_and_forward_returns() {
        let ev = "id,parent,chain,ts,layer,event,dir,price,size,strength,nature,detail\n\
                  1,,1,1000000000,4,AGGRESSION,1,100.0,5,3.1,observed,imb 0.6\n\
                  2,1,1,1000000000,5,RESP_ABSORPTION,1,100.0,5,-0.1,derived,aggr_z 3, extra\n";
        let e = parse_events(ev);
        assert_eq!(e.len(), 2);
        assert_eq!(e[1].parent, Some(1));
        assert_eq!(e[1].detail, "aggr_z 3, extra");
        assert_eq!(e[0].t, 1000);
        let mut st = String::from("ts,mid,bid,ask,spread_bps,depth_bid,depth_ask,buy_vol,sell_vol,imbalance,cvd,aggr_z,beta,expected,residual_z,response,regime,rv_ratio,vwap,level_hi,level_lo,synced\n");
        for k in 0..20 {
            st.push_str(&format!("{},{},,,1,2,3,1,1,0,0,0,,,0.5,,quiet,1,100,,,1\n", (k + 1) * 1_000_000_000_i64, 100.0 + k as f64 * 0.01));
        }
        let s = parse_states(&st);
        assert_eq!(s.t.len(), 20);
        let f = forward_bps(&s, 1000, 1, 5).unwrap();
        assert!((f - 0.05 / 100.0 * 1e4).abs() < 1e-6, "{f}");
        assert!(forward_bps(&s, 1000, -1, 5).unwrap() < 0.0);
        let d = OfData { events: e, states: s, ..Default::default() };
        let (n, m) = same_kind_stats(&d, "AGGRESSION", &[5, 60]);
        assert_eq!(n, 1);
        assert!(m[0].is_some() && m[1].is_none(), "超出窗口的视界没有值");
        assert_eq!(chain_of(&d, 1).len(), 2);
    }

    /// Python 侧三份输出的形状（`factory.ofms.response` / `factory.ofms.dsl` / `factory.ofms.quick`）：字段一漂移，这里先红。
    #[test]
    fn parses_python_exports() {
        let resp = r#"{"summary":{"windows":3,"n_trials":4},
            "rows":[{"kind":"event","cell":"AGGRESSION","h":30,"n":465,"clusters":5,"cost_bp":9.3,"mean_bp":0.47,
                     "se_bp":0.09,"ci_lo":0.3,"ci_hi":0.6,"t_mid":5.5,"side":"顺","net_bp":-8.86,"t_net":-98.0,"verdict":"有信息未越过成本"},
                    {"kind":"event","cell":"LEVEL_BREAK","h":300,"n":3,"clusters":2,"cost_bp":9.1,"verdict":"样本不足"}],
            "transitions":[{"from":"NORMAL","to":"RESP_VACUUM","n":781,"mean30_bp":0.5,"se30_bp":0.16}],
            "rolling":[{"cell":"AGGRESSION","h":30,"full_bp":0.44,"decayed":true,
                        "weeks":[{"week":"2026-W23","mean_bp":0.62,"n":2542}]}]}"#;
        let r: Resp = serde_json::from_str(resp).expect("响应表");
        assert_eq!(r.rows.len(), 2);
        assert!(r.rows[1].mean_bp.is_none());
        assert!(r.rolling[0].decayed);
        let setups = r#"[{"id":"ofms.x","name":"X","path":"p.toml","verdict":"untested","signal":"events","replaces":null,
                          "triggers":["EXHAUSTION"],"events":["EXHAUSTION"],"layers":{"L4":"EXHAUSTION（15s 内）"}},
                         {"path":"bad.toml","error":"写错了"}]"#;
        let ss: Vec<Setup> = serde_json::from_str(setups).expect("描述文件导出");
        assert_eq!(ss[0].layers["L4"], "EXHAUSTION（15s 内）");
        assert_eq!(ss[1].error.as_deref(), Some("写错了"));
        let tr: Vec<DslTrade> = serde_json::from_str(
            r#"[{"entry_ts":1,"exit_ts":2,"side":-1,"hold_secs":1.0,"gross_bp":0.5,"cost_bp":9.0,"net_bp":-8.5,"exit_kind":"event"}]"#,
        )
        .expect("逐笔");
        assert_eq!(tr[0].side, -1);
    }

    /// 实时 tail：第一次跳过表头；半行留到下次；只读新增部分。
    #[test]
    fn tail_reads_only_complete_new_lines() {
        let dir = std::env::temp_dir().join(format!("ofms_tail_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("ofms_events.csv");
        std::fs::write(&p, "id,parent,chain,ts,layer,event,dir,price,size,strength,nature,detail\n1,,1,1000000000,4,SWEEP,1,1,1,1,observed,x\n2,,2,2000").unwrap();
        let mut t = Tail { path: p.clone(), off: 0 };
        let a = parse_events(&t.read_new());
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].key, "SWEEP");
        assert!(parse_events(&t.read_new()).is_empty(), "半行不读");
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(b"000000,4,AGGRESSION,-1,1,1,1,observed,y\n").unwrap();
        let b = parse_events(&t.read_new());
        assert_eq!(b.len(), 1);
        assert_eq!((b[0].id, b[0].t, b[0].dir), (2, 2000, -1));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
