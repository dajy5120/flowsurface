//! 共用数据选择组件：**管线 → 来源 → 市场 → 标的 → 时间**（docs/28「两条管线 × N 个源」）。
//!
//! ```text
//! 管线 A（flowsurface 原生）  交易所 WS 直连图表。只喂图表（docs/28 §1.2）——计算用的宿主里不可选
//! 管线 B（Nautilus 规范）     经适配器进 Nautilus 模型，策略 / 特征 / 回测 / 结果只走这条
//!   B1 交易所实时
//!   B2 购买的高质量数据（本地目录）  Tardis / Databento 各走自己的接口，不互相转换格式
//!   B3 本地录制数据
//!   B4 第三方 API
//! ```
//!
//! 本地数据（B2 / B3）：选根目录（有缺省，可手填或弹目录选择框）→ **自动扫描**出其中的市场、标的、日期，
//! 以及每天有哪些数据类型（数据商原名：Tardis 的 `incremental_book_L2`、Databento 的 `mbo`、录制器的 `l2`…），
//! 类型可勾选。扫描只有主仓一份实现（`ws-data-source scan`，各数据商在 `wealthspring-vendors` 里各自一个 `scan`），
//! 这里在后台线程调它、按（来源, 根目录）缓存——渲染线程不起子进程、不扫目录。
//!
//! 实时来源（A / B1 / B4）的时间就是「实时」。A / B1 的标的取 flowsurface 已从交易所拉到的交易对列表
//! （行情面板的标的表拉取时经 [`publish_native_tickers`] 共享过来），这里不另外联网。
//!
//! 用法：宿主持有一份 [`DataPick`]（每个面板各自一份），把 [`DataPickMsg`] 包进自己的消息交给
//! [`DataPick::update`]，渲染用 [`super::data_picker_view::view`]，读结果用 [`DataPick::selection`]。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, OnceLock};

// ── 管线与来源 ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Pipeline {
    /// flowsurface 原生（交易所 WS → 图表）。
    A,
    /// Nautilus 规范。
    B,
}

impl Pipeline {
    pub const ALL: [Self; 2] = [Self::A, Self::B];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::A => "管线 A（flowsurface 原生）",
            Self::B => "管线 B（Nautilus 规范）",
        }
    }
}

/// 管线 B 的来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BSource {
    /// B1 交易所实时。
    Live,
    /// B2 购买的高质量数据（本地目录）。
    Purchased,
    /// B3 本地录制数据。
    Recorded,
    /// B4 第三方 API。
    ThirdParty,
}

impl BSource {
    pub const ALL: [Self; 4] = [Self::Live, Self::Purchased, Self::Recorded, Self::ThirdParty];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Live => "B1 交易所实时",
            Self::Purchased => "B2 购买数据（本地）",
            Self::Recorded => "B3 本地录制",
            Self::ThirdParty => "B4 第三方 API",
        }
    }

    /// 本地目录类来源。
    #[must_use]
    pub const fn is_local(self) -> bool {
        matches!(self, Self::Purchased | Self::Recorded)
    }
}

/// B2 的两家数据商（各走自己的接口）。
pub const VENDORS: [(&str, &str); 2] = [
    ("tardis", "Tardis（加密逐笔）"),
    ("databento", "Databento（美股 / 期货 / 期权）"),
];

/// B4 第三方 API：(键, 名称, 市场, 状态说明, 已接)。
pub const PROVIDERS: [(&str, &str, &str, &str, bool); 3] = [
    ("polymarket", "Polymarket Gamma", "prediction", "公开 API、无 key（docs/19）", true),
    ("polygon", "Polygon", "option", "美股期权，需 API key（docs/18）", true),
    ("databento_live", "Databento 实时", "future", "按用量计费，未接（docs/28 §6.2）", false),
];

/// 管线 A 的交易所（flowsurface 原生适配器，`exchange::Venue`）与市场。
pub const NATIVE_VENUES: [&str; 5] = ["Binance", "Bybit", "OKX", "Hyperliquid", "MEXC"];
/// 管线 B1：本仓接了实时的交易所（通道② feed + Nautilus live）；其余 Nautilus 有适配器、本仓未接。
pub const LIVE_VENUES: [(&str, bool); 3] = [("Binance", true), ("Bybit", false), ("OKX", false)];

/// 市场的中文名（本地扫描用 `data-vendors.toml` 的市场名；交易所用现货 / 永续）。
#[must_use]
pub fn market_label(m: &str) -> String {
    match m {
        "crypto" => "加密".into(),
        "equity" => "美股".into(),
        "etf" => "ETF".into(),
        "future" => "期货".into(),
        "option" => "期权".into(),
        "prediction" => "预测市场".into(),
        "spot" => "现货".into(),
        "linear" => "U 本位永续".into(),
        "inverse" => "币本位永续".into(),
        "?" => "未归类".into(),
        other => other.into(),
    }
}

// ── 管线 A / B1：交易所交易对（flowsurface 拉到时共享过来）─────────────────────

/// (交易所, 市场) → 交易对。
type NativeMap = BTreeMap<(String, String), BTreeSet<String>>;
static NATIVE: OnceLock<Mutex<NativeMap>> = OnceLock::new();

/// 行情面板的标的表拉到某交易所的交易对后调用（`venue` 用 [`NATIVE_VENUES`] 的写法，`market` 用 spot / linear / inverse）。
pub fn publish_native_tickers(venue: &str, market: &str, symbols: impl IntoIterator<Item = String>) {
    if let Ok(mut g) = NATIVE.get_or_init(Default::default).lock() {
        g.entry((venue.to_string(), market.to_string())).or_default().extend(symbols);
    }
}

/// 某交易所已知的市场（有交易对的）。
#[must_use]
pub fn native_markets(venue: &str) -> Vec<String> {
    let g = NATIVE.get_or_init(Default::default).lock();
    let mut v: Vec<String> = g
        .map(|g| g.keys().filter(|(v, _)| v == venue).map(|(_, m)| m.clone()).collect())
        .unwrap_or_default();
    v.sort_by_key(|m| ["linear", "spot", "inverse"].iter().position(|x| x == m).unwrap_or(9));
    v
}

/// 某交易所某市场的交易对（按代码排序，`search` 子串筛）。
#[must_use]
pub fn native_symbols(venue: &str, market: &str, search: &str) -> Vec<String> {
    let q = search.trim().to_ascii_uppercase();
    NATIVE
        .get_or_init(Default::default)
        .lock()
        .map(|g| {
            g.get(&(venue.to_string(), market.to_string()))
                .map(|s| s.iter().filter(|x| q.is_empty() || x.contains(&q)).cloned().collect())
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

// ── B2 / B3：本地目录扫描 ──────────────────────────────────────────────────────

/// 扫描结果的一项：某标的某日有哪些数据类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanItem {
    pub market: String,
    pub symbol: String,
    pub date: String,
    pub types: Vec<String>,
}

/// 一个根目录的扫描结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    pub root: String,
    pub exists: bool,
    pub items: Vec<ScanItem>,
}

impl Scan {
    /// 解析 `ws-data-source scan` 的输出（制表符分隔）。认不出的行跳过。
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut s = Self::default();
        for line in text.lines() {
            match line.split('\t').collect::<Vec<_>>().as_slice() {
                ["root", root, ex] => {
                    s.root = (*root).into();
                    s.exists = *ex == "1";
                }
                ["item", market, symbol, date, types] => s.items.push(ScanItem {
                    market: (*market).into(),
                    symbol: (*symbol).into(),
                    date: (*date).into(),
                    types: types.split(',').filter(|t| !t.is_empty()).map(str::to_string).collect(),
                }),
                _ => {}
            }
        }
        s
    }

    /// 出现过的市场（按出现先后去重）。
    #[must_use]
    pub fn markets(&self) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for it in &self.items {
            if !v.contains(&it.market) {
                v.push(it.market.clone());
            }
        }
        v
    }

    /// 某市场的标的与天数（按代码排序；`search` 子串筛，不分大小写）。
    #[must_use]
    pub fn symbols(&self, market: &str, search: &str) -> Vec<(String, usize)> {
        let q = search.trim().to_ascii_uppercase();
        let mut m: BTreeMap<String, usize> = BTreeMap::new();
        for it in self.items.iter().filter(|i| i.market == market) {
            if q.is_empty() || it.symbol.to_ascii_uppercase().contains(&q) {
                *m.entry(it.symbol.clone()).or_default() += 1;
            }
        }
        m.into_iter().collect()
    }

    /// 某标的有数据的日期（升序）。
    #[must_use]
    pub fn dates(&self, symbol: &str) -> Vec<String> {
        let mut v: Vec<String> = self.items.iter().filter(|i| i.symbol == symbol).map(|i| i.date.clone()).collect();
        v.sort();
        v.dedup();
        v
    }

    /// 某标的某日有的数据类型。
    #[must_use]
    pub fn types(&self, symbol: &str, date: &str) -> Vec<String> {
        self.items
            .iter()
            .find(|i| i.symbol == symbol && i.date == date)
            .map(|i| i.types.clone())
            .unwrap_or_default()
    }

    /// 某标的全部日期里出现过的数据类型（各类型出现的天数）。
    #[must_use]
    pub fn type_days(&self, symbol: &str) -> Vec<(String, usize)> {
        let mut m: BTreeMap<String, usize> = BTreeMap::new();
        for it in self.items.iter().filter(|i| i.symbol == symbol) {
            for t in &it.types {
                *m.entry(t.clone()).or_default() += 1;
            }
        }
        m.into_iter().collect()
    }
}

/// 后台加载状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Load<T> {
    Loading,
    Ready(T),
    Failed(String),
}

/// (来源键, 根目录) → 扫描结果。来源键：`tardis` / `databento` / `recorder`。
type ScanCache = BTreeMap<(String, String), Load<Scan>>;
static SCANS: OnceLock<Mutex<ScanCache>> = OnceLock::new();

/// `ws-data-source` 可执行文件（主仓 release 构建产物；`WS_DATA_SOURCE_BIN` 可覆盖）。
#[must_use]
pub fn cli() -> std::path::PathBuf {
    std::env::var("WS_DATA_SOURCE_BIN").map_or_else(
        |_| super::paths::repo_root().join("target/release/ws-data-source"),
        std::path::PathBuf::from,
    )
}

fn run_cli(args: &[&str]) -> Result<String, String> {
    let bin = cli();
    if !bin.exists() {
        return Err(format!(
            "找不到 {}——先在主仓 `cargo build --release -p wealthspring-vendors --bin ws-data-source`",
            bin.display()
        ));
    }
    let out = std::process::Command::new(&bin)
        .args(args)
        .output()
        .map_err(|e| format!("调用 {} 失败：{e}", bin.display()))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 某来源的缺省根目录（与主仓 `Roots::default()` 同一组缺省；环境变量同名覆盖）。
#[must_use]
pub fn default_root(src: &str) -> String {
    let env = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
    match src {
        "tardis" => env("WS_TARDIS_ROOT", "/data/ubuntu/HistoricalData/data/bronze/tardis/binance-futures"),
        "databento" => env("WS_DATABENTO_ROOT", "/data/ubuntu/HistoricalData/data/bronze/Databento"),
        _ => std::env::var("WS_DATA_DIR").unwrap_or_else(|_| super::paths::data_dir().to_string_lossy().into_owned()),
    }
}

/// 扫描结果（第一次问时后台扫；`force` = 重扫）。
#[must_use]
pub fn scan(src: &str, root: &str, force: bool) -> Load<Scan> {
    let cell = SCANS.get_or_init(Default::default);
    let key = (src.to_string(), root.trim().to_string());
    let Ok(mut g) = cell.lock() else {
        return Load::Failed("扫描缓存锁坏了".into());
    };
    if !force && let Some(l) = g.get(&key) {
        return l.clone();
    }
    g.insert(key.clone(), Load::Loading);
    std::thread::spawn(move || {
        let r = run_cli(&["scan", &key.0, "--root", &key.1]).map(|t| Scan::parse(&t));
        if let Ok(mut g) = SCANS.get_or_init(Default::default).lock() {
            g.insert(
                key,
                match r {
                    Ok(s) => Load::Ready(s),
                    Err(e) => Load::Failed(e),
                },
            );
        }
    });
    Load::Loading
}

/// 系统「选择目录」对话框（zenity，阻塞——调用方放后台线程）。取消返回 `None`。
#[must_use]
pub fn ask_dir(start: &str) -> Option<String> {
    let out = std::process::Command::new("zenity")
        .args(["--file-selection", "--directory", "--title=选择数据根目录", &format!("--filename={start}/")])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// 目录选择框选完的结果（后台线程写，宿主下一次 update 时取走）。
static PICKED_DIR: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn take_picked_dir() -> Option<String> {
    PICKED_DIR.get_or_init(|| Mutex::new(None)).lock().ok()?.take()
}

// ── 宿主接口 ─────────────────────────────────────────────────────────────────

/// 宿主的用途：计算用（策略 / 特征 / 回测）不允许管线 A（docs/28 §1.2「管线 A 只喂图表」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Purpose {
    /// 图表：两条管线都可选。
    Chart,
    /// 计算：只能管线 B。
    #[default]
    Compute,
}

/// 时间怎么选。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimeMode {
    /// 只选日期。
    Date,
    /// 日期 + 起始时刻（UTC）+ 时长。
    #[default]
    Window,
}

/// 宿主对组件的要求。
#[derive(Debug, Clone, Default)]
pub struct PickOpts {
    pub purpose: Purpose,
    /// 只允许这些管线 B 来源（`None` = 全部）。
    pub sources: Option<Vec<BSource>>,
    /// 只允许 B2 的这些数据商（`None` = 全部）。
    pub vendors: Option<Vec<&'static str>>,
    pub time: TimeMode,
    /// 宿主只读本地历史文件（管线 A / B1 / B4 都没有意义）：管线 A 置灰。
    pub local_only: bool,
    /// 宿主自己选数据类型（单选、有自己的类型名）：组件不列类型勾选。
    pub hide_types: bool,
}

/// 一个面板的选择。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataPick {
    pub pipeline: Option<Pipeline>,
    pub source: Option<BSource>,
    /// B2：`tardis` / `databento`；B4：第三方键。
    pub vendor: Option<String>,
    /// A / B1：交易所。
    pub venue: Option<String>,
    /// B2 / B3：根目录（输入框文本）。
    pub root: String,
    pub market: Option<String>,
    pub symbol: Option<String>,
    pub date: Option<String>,
    /// 起始时刻（UTC，`HH:MM`）与时长（分钟）。
    pub start: String,
    pub minutes: u32,
    /// 取消勾选的数据类型（空 = 当天有的全部要）。
    pub excluded: BTreeSet<String>,
    /// 标的搜索框。
    pub search: String,
    /// 上一条提示（目录选择失败之类）。
    pub note: String,
}

impl Default for DataPick {
    fn default() -> Self {
        Self {
            pipeline: None,
            source: None,
            vendor: None,
            venue: None,
            root: String::new(),
            market: None,
            symbol: None,
            date: None,
            start: "13:30".into(),
            minutes: 30,
            excluded: BTreeSet::new(),
            search: String::new(),
            note: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataPickMsg {
    Pipeline(Pipeline),
    Source(BSource),
    Vendor(String),
    Venue(String),
    Root(String),
    /// 弹目录选择框。
    BrowseRoot,
    /// 重扫当前根目录。
    Rescan,
    Market(String),
    Symbol(String),
    Date(String),
    Start(String),
    Minutes(u32),
    /// 勾 / 取消某数据类型。
    ToggleType(String),
    Search(String),
}

/// 选定的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// 管线 A：flowsurface 原生实时（只喂图表）。
    Native { venue: String, market: String, symbol: String },
    /// B1：交易所实时（经 Nautilus）。
    Live { venue: String, market: String, symbol: String },
    /// B2 / B3：本地数据。`source` = `tardis` / `databento` / `recorder`。
    Local {
        source: String,
        root: String,
        market: String,
        symbol: String,
        date: String,
        /// `TimeMode::Window` 时的起始时刻（UTC `HH:MM`）与时长（分钟）。
        window: Option<(String, u32)>,
        /// 选用的数据类型（数据商原名）。
        types: Vec<String>,
    },
    /// B4：第三方 API（实时）。
    ThirdParty { provider: String, market: String, symbol: String },
}

impl Selection {
    /// 一行说明（宿主可显示在标题栏，docs/28 §4.2「来源常驻可见」）。
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Native { venue, market, symbol } => format!("A · {venue} {} · {symbol} · 实时", market_label(market)),
            Self::Live { venue, market, symbol } => format!("B1 · {venue} {} · {symbol} · 实时", market_label(market)),
            Self::Local { source, market, symbol, date, window, types, .. } => format!(
                "{} · {} · {} · {symbol} · {date}{} · {} 类数据",
                if source == "recorder" { "B3" } else { "B2" },
                source_label(source),
                market_label(market),
                window.as_ref().map(|(s, m)| format!(" {s} 起 {m} 分钟")).unwrap_or_default(),
                types.len()
            ),
            Self::ThirdParty { provider, symbol, .. } => format!("B4 · {provider} · {symbol} · 实时"),
        }
    }
}

/// 本地来源键 → 中文名。
#[must_use]
pub fn source_label(s: &str) -> String {
    match s {
        "tardis" => "Tardis".into(),
        "databento" => "Databento".into(),
        "recorder" => "本地录制".into(),
        other => other.into(),
    }
}

impl DataPick {
    /// 当前本地来源键（B2 取所选数据商，B3 = `recorder`）。
    #[must_use]
    pub fn local_key(&self) -> Option<&str> {
        match (self.pipeline, self.source) {
            (Some(Pipeline::B), Some(BSource::Purchased)) => self.vendor.as_deref(),
            (Some(Pipeline::B), Some(BSource::Recorded)) => Some("recorder"),
            _ => None,
        }
    }

    /// 当前本地来源的扫描结果（根目录为空时用缺省根）。
    #[must_use]
    pub fn local_scan(&self) -> Option<Load<Scan>> {
        let k = self.local_key()?;
        Some(scan(k, &self.root_or_default(), false))
    }

    #[must_use]
    pub fn root_or_default(&self) -> String {
        match self.local_key() {
            Some(k) if self.root.trim().is_empty() => default_root(k),
            _ => self.root.trim().to_string(),
        }
    }

    fn clear_below_source(&mut self) {
        self.vendor = None;
        self.venue = None;
        self.root.clear();
        self.clear_below_root();
    }

    fn clear_below_root(&mut self) {
        self.market = None;
        self.clear_below_market();
    }

    fn clear_below_market(&mut self) {
        self.symbol = None;
        self.search.clear();
        self.clear_below_symbol();
    }

    fn clear_below_symbol(&mut self) {
        self.date = None;
        self.excluded.clear();
    }

    /// 处理一条消息：上一级变了，下一级全部清掉（重选），不留下与新上级对不上的旧选择。
    pub fn update(&mut self, m: DataPickMsg) {
        self.note.clear();
        if let Some(d) = take_picked_dir() {
            self.root = d;
            self.clear_below_root();
        }
        match m {
            DataPickMsg::Pipeline(p) => {
                if self.pipeline != Some(p) {
                    self.pipeline = Some(p);
                    self.source = None;
                    self.clear_below_source();
                }
            }
            DataPickMsg::Source(s) => {
                if self.source != Some(s) {
                    self.source = Some(s);
                    self.clear_below_source();
                    if s == BSource::Recorded {
                        self.root = default_root("recorder");
                    }
                }
            }
            DataPickMsg::Vendor(v) => {
                if self.vendor.as_deref() != Some(v.as_str()) {
                    let local = self.source == Some(BSource::Purchased);
                    self.clear_below_source();
                    if local {
                        self.root = default_root(&v);
                    } else if let Some(p) = PROVIDERS.iter().find(|p| p.0 == v) {
                        // 第三方：市场由提供方决定
                        self.market = Some(p.2.to_string());
                    }
                    self.vendor = Some(v);
                }
            }
            DataPickMsg::Venue(v) => {
                if self.venue.as_deref() != Some(v.as_str()) {
                    self.venue = Some(v);
                    self.clear_below_root();
                }
            }
            DataPickMsg::Root(r) => {
                self.root = r;
                self.clear_below_root();
            }
            DataPickMsg::BrowseRoot => {
                let start = self.root_or_default();
                std::thread::spawn(move || {
                    if let Some(d) = ask_dir(&start)
                        && let Ok(mut g) = PICKED_DIR.get_or_init(|| Mutex::new(None)).lock()
                    {
                        *g = Some(d);
                    }
                });
                self.note = "在弹出的对话框里选目录；选完点任意处刷新".into();
            }
            DataPickMsg::Rescan => {
                if let Some(k) = self.local_key() {
                    let _ = scan(k, &self.root_or_default(), true);
                }
            }
            DataPickMsg::Market(mk) => {
                if self.market.as_deref() != Some(mk.as_str()) {
                    self.market = Some(mk);
                    self.clear_below_market();
                }
            }
            DataPickMsg::Symbol(sym) => {
                if self.symbol.as_deref() != Some(sym.as_str()) {
                    self.symbol = Some(sym);
                    self.clear_below_symbol();
                }
            }
            DataPickMsg::Date(d) => self.date = Some(d),
            DataPickMsg::Start(s) => self.start = s,
            DataPickMsg::Minutes(n) => self.minutes = n,
            DataPickMsg::ToggleType(t) => {
                if !self.excluded.remove(&t) {
                    self.excluded.insert(t);
                }
            }
            DataPickMsg::Search(q) => self.search = q,
        }
    }

    /// 宿主每帧也可以调一次：把目录选择框的结果收进来（不需要等下一条消息）。
    pub fn poll(&mut self) {
        if let Some(d) = take_picked_dir() {
            self.root = d;
            self.clear_below_root();
        }
    }

    /// 选完了才有结果。
    #[must_use]
    pub fn selection(&self, opts: &PickOpts) -> Option<Selection> {
        match (self.pipeline?, self.source) {
            (Pipeline::A, _) => {
                if opts.purpose == Purpose::Compute {
                    return None;
                }
                Some(Selection::Native {
                    venue: self.venue.clone()?,
                    market: self.market.clone()?,
                    symbol: self.symbol.clone()?,
                })
            }
            (Pipeline::B, Some(BSource::Live)) => Some(Selection::Live {
                venue: self.venue.clone()?,
                market: self.market.clone()?,
                symbol: self.symbol.clone()?,
            }),
            (Pipeline::B, Some(BSource::ThirdParty)) => Some(Selection::ThirdParty {
                provider: self.vendor.clone()?,
                market: self.market.clone()?,
                symbol: self.symbol.clone().filter(|s| !s.trim().is_empty())?,
            }),
            (Pipeline::B, Some(s)) if s.is_local() => {
                let key = self.local_key()?.to_string();
                let symbol = self.symbol.clone()?;
                let date = self.date.clone()?;
                let types = match self.local_scan() {
                    Some(Load::Ready(sc)) => sc.types(&symbol, &date),
                    _ => Vec::new(),
                }
                .into_iter()
                .filter(|t| !self.excluded.contains(t))
                .collect::<Vec<_>>();
                if types.is_empty() {
                    return None;
                }
                Some(Selection::Local {
                    source: key,
                    root: self.root_or_default(),
                    market: self.market.clone()?,
                    symbol,
                    date,
                    window: (opts.time == TimeMode::Window).then(|| (self.start.clone(), self.minutes)),
                    types,
                })
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCAN: &str = "root\t/d\t1\n\
item\tequity\tAAPL\t2026-09-01\tdefinition,mbo,mbp-10,trades\n\
item\tfuture\tESU6\t2026-09-01\tmbo,trades\n\
item\tfuture\tGCZ6\t2026-09-01\tmbo\n\
item\tfuture\tGCZ6\t2026-09-02\tmbo,trades\n\
junk\n";

    #[test]
    fn 扫描解析与按市场标的日期类型取() {
        let s = Scan::parse(SCAN);
        assert!(s.exists);
        assert_eq!(s.root, "/d");
        assert_eq!(s.markets(), vec!["equity".to_string(), "future".into()]);
        assert_eq!(s.symbols("future", ""), vec![("ESU6".to_string(), 1), ("GCZ6".into(), 2)]);
        assert_eq!(s.symbols("future", "gc"), vec![("GCZ6".to_string(), 2)], "搜索不分大小写");
        assert_eq!(s.dates("GCZ6"), vec!["2026-09-01".to_string(), "2026-09-02".into()]);
        assert_eq!(s.types("GCZ6", "2026-09-02"), vec!["mbo".to_string(), "trades".into()]);
        assert_eq!(s.type_days("GCZ6"), vec![("mbo".to_string(), 2), ("trades".into(), 1)]);
    }

    #[test]
    fn 上级变了下级全清() {
        let mut p = DataPick::default();
        p.update(DataPickMsg::Pipeline(Pipeline::B));
        p.update(DataPickMsg::Source(BSource::Purchased));
        p.update(DataPickMsg::Vendor("databento".into()));
        assert_eq!(p.root, default_root("databento"), "选数据商填上缺省根");
        p.update(DataPickMsg::Market("future".into()));
        p.update(DataPickMsg::Symbol("ESU6".into()));
        p.update(DataPickMsg::Date("2026-09-01".into()));
        p.update(DataPickMsg::ToggleType("trades".into()));
        p.update(DataPickMsg::Symbol("GCZ6".into()));
        assert!(p.date.is_none() && p.excluded.is_empty(), "换标的清日期与类型勾选");
        p.update(DataPickMsg::Source(BSource::Recorded));
        assert!(p.vendor.is_none() && p.market.is_none() && p.symbol.is_none());
        assert_eq!(p.root, default_root("recorder"));
        p.update(DataPickMsg::Pipeline(Pipeline::A));
        assert!(p.source.is_none() && p.root.is_empty());
    }

    #[test]
    fn 计算用途不接受管线a_第三方要手填标的() {
        let mut p = DataPick::default();
        p.update(DataPickMsg::Pipeline(Pipeline::A));
        p.update(DataPickMsg::Venue("Binance".into()));
        p.update(DataPickMsg::Market("linear".into()));
        p.update(DataPickMsg::Symbol("BTCUSDT".into()));
        let chart = PickOpts { purpose: Purpose::Chart, ..PickOpts::default() };
        assert!(matches!(p.selection(&chart), Some(Selection::Native { .. })));
        assert!(p.selection(&PickOpts::default()).is_none(), "计算用途：管线 A 只喂图表");

        let mut q = DataPick::default();
        q.update(DataPickMsg::Pipeline(Pipeline::B));
        q.update(DataPickMsg::Source(BSource::ThirdParty));
        q.update(DataPickMsg::Vendor("polymarket".into()));
        assert_eq!(q.market.as_deref(), Some("prediction"), "第三方的市场由提供方决定");
        assert!(q.selection(&PickOpts::default()).is_none());
        q.update(DataPickMsg::Symbol("btc-updown-5m".into()));
        assert!(matches!(q.selection(&PickOpts::default()), Some(Selection::ThirdParty { .. })));
    }
}
