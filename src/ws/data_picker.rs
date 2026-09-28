//! 数据选择组件（共用）：选**数据管线**（数据源）→ **市场** → **标的** →（可选）**日期**。
//!
//! 任何面板都可以嵌一个：宿主持有一份 [`DataPick`]（每个面板各自的选择，互不干扰），
//! 把 [`DataPickMsg`] 包进自己的消息、交给 [`DataPick::update`]，渲染用
//! [`super::data_picker_view::view`]，读结果用 [`DataPick::selection`]。
//!
//! ## 目录只有一份实现
//!
//! 「哪个数据源有哪些标的、各属于什么市场、哪几天有数据」全在主仓 `wealthspring-vendors`
//! （市场判定规则在 `data-vendors.toml`）。Cockpit 不链接它（子模块循环 + parquet 之类的重依赖），
//! 而是在**后台线程**里调它的命令行 `ws-data-source catalog`，结果进程级缓存——
//! 渲染线程从不起子进程、从不扫目录。「刷新目录」按钮重新拉一次。
//!
//! ## 实时
//!
//! 宿主可以在数据源里加一项「实时」（[`PickOpts::live`]）：选它时没有市场 / 标的 / 日期可选，
//! 宿主按自己的实时通道处理（例如特征矩阵读常驻引擎的快照）。

use std::sync::{Mutex, OnceLock};

/// 「实时」这一项在数据源里的键。
pub const LIVE: &str = "live";

/// 数据源的中文名。
#[must_use]
pub fn source_label(s: &str) -> String {
    match s {
        LIVE => "实时".into(),
        "tardis" => "Tardis（加密·已购）".into(),
        "databento" => "Databento（美股 / 期货·已购）".into(),
        "recorder" => "自录".into(),
        other => other.into(),
    }
}

/// 市场的中文名。
#[must_use]
pub fn market_label(m: &str) -> String {
    match m {
        "crypto" => "加密".into(),
        "equity" => "美股".into(),
        "etf" => "ETF".into(),
        "future" => "期货".into(),
        "option" => "期权".into(),
        "?" => "未归类".into(),
        other => other.into(),
    }
}

/// 目录里的一个标的。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub source: String,
    pub market: String,
    pub symbol: String,
    /// 有数据的天数、最早 / 最晚日期。
    pub days: u32,
    pub first: String,
    pub last: String,
}

/// 一个数据源（根目录在不在）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceInfo {
    pub name: String,
    pub available: bool,
    pub root: String,
}

/// 全部数据源的目录。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalog {
    pub sources: Vec<SourceInfo>,
    pub entries: Vec<Entry>,
}

impl Catalog {
    /// 解析 `ws-data-source catalog` 的输出（制表符分隔）。认不出的行跳过。
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut c = Self::default();
        for line in text.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            match f.as_slice() {
                ["source", name, avail, root] => c.sources.push(SourceInfo {
                    name: (*name).into(),
                    available: *avail == "1",
                    root: (*root).into(),
                }),
                ["symbol", source, market, symbol, days, first, last] => c.entries.push(Entry {
                    source: (*source).into(),
                    market: (*market).into(),
                    symbol: (*symbol).into(),
                    days: days.parse().unwrap_or(0),
                    first: (*first).into(),
                    last: (*last).into(),
                }),
                _ => {}
            }
        }
        c
    }

    /// 某数据源里出现过的市场（按目录顺序去重）。
    #[must_use]
    pub fn markets(&self, source: &str) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for e in self.entries.iter().filter(|e| e.source == source) {
            if !v.contains(&e.market) {
                v.push(e.market.clone());
            }
        }
        v
    }

    /// 某数据源某市场的标的（按代码排序）；`search` 按代码子串筛（不分大小写）。
    #[must_use]
    pub fn symbols(&self, source: &str, market: &str, search: &str) -> Vec<&Entry> {
        let q = search.trim().to_ascii_uppercase();
        let mut v: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|e| e.source == source && e.market == market)
            .filter(|e| q.is_empty() || e.symbol.to_ascii_uppercase().contains(&q))
            .collect();
        v.sort_by(|a, b| a.symbol.cmp(&b.symbol));
        v
    }

    #[must_use]
    pub fn entry(&self, source: &str, symbol: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.source == source && e.symbol == symbol)
    }
}

/// 目录的加载状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Load<T> {
    Loading,
    Ready(T),
    Failed(String),
}

static CATALOG: OnceLock<Mutex<Option<Load<Catalog>>>> = OnceLock::new();
/// (数据源, 标的) → 日期。
type DateCache = std::collections::HashMap<(String, String), Load<Vec<String>>>;

static DATES: OnceLock<Mutex<DateCache>> = OnceLock::new();

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
    let out = std::process::Command::new(&bin).args(args).output().map_err(|e| format!("调用 {} 失败：{e}", bin.display()))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 当前目录（第一次调用时在后台加载；加载中返回 `Load::Loading`）。
#[must_use]
pub fn catalog() -> Load<Catalog> {
    let cell = CATALOG.get_or_init(|| Mutex::new(None));
    let Ok(mut g) = cell.lock() else {
        return Load::Failed("目录锁坏了".into());
    };
    match &*g {
        Some(l) => l.clone(),
        None => {
            *g = Some(Load::Loading);
            std::thread::spawn(|| {
                let r = run_cli(&["catalog"]).map(|t| Catalog::parse(&t));
                if let Ok(mut g) = CATALOG.get_or_init(|| Mutex::new(None)).lock() {
                    *g = Some(match r {
                        Ok(c) => Load::Ready(c),
                        Err(e) => Load::Failed(e),
                    });
                }
                if let Ok(mut d) = DATES.get_or_init(Default::default).lock() {
                    d.clear();
                }
            });
            Load::Loading
        }
    }
}

/// 重新拉一次目录（日期缓存一起清）。
pub fn refresh() {
    if let Ok(mut g) = CATALOG.get_or_init(|| Mutex::new(None)).lock() {
        *g = None;
    }
    let _ = catalog();
}

/// 某数据源某标的有数据的日期（升序；第一次问时在后台查）。
#[must_use]
pub fn dates(source: &str, symbol: &str) -> Load<Vec<String>> {
    let cell = DATES.get_or_init(Default::default);
    let key = (source.to_string(), symbol.to_string());
    let Ok(mut g) = cell.lock() else {
        return Load::Failed("日期缓存锁坏了".into());
    };
    if let Some(l) = g.get(&key) {
        return l.clone();
    }
    g.insert(key.clone(), Load::Loading);
    std::thread::spawn(move || {
        let r = run_cli(&["days", &key.1, "--source", &key.0])
            .map(|t| t.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect());
        if let Ok(mut g) = DATES.get_or_init(Default::default).lock() {
            g.insert(
                key,
                match r {
                    Ok(v) => Load::Ready(v),
                    Err(e) => Load::Failed(e),
                },
            );
        }
    });
    Load::Loading
}

/// 宿主对组件的要求。
#[derive(Debug, Clone, Default)]
pub struct PickOpts {
    /// 只列这些数据源（`None` = 目录里全部）。
    pub sources: Option<Vec<&'static str>>,
    /// 在数据源里加一项「实时」。
    pub live: bool,
    /// 要不要选日期。
    pub date: bool,
}

/// 一个面板的选择。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DataPick {
    pub source: Option<String>,
    pub market: Option<String>,
    pub symbol: Option<String>,
    pub date: Option<String>,
    /// 标的搜索框。
    pub search: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataPickMsg {
    Source(String),
    Market(String),
    Symbol(String),
    Date(String),
    Search(String),
    /// 重新拉目录。
    Refresh,
}

/// 选定的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub source: String,
    pub market: String,
    pub symbol: String,
    /// 没要求选日期时为 `None`。
    pub date: Option<String>,
}

impl DataPick {
    /// 预先选好（宿主恢复上次的选择用）。
    #[must_use]
    pub fn with(source: &str, symbol: &str, date: Option<&str>) -> Self {
        let market = match catalog() {
            Load::Ready(c) => c.entry(source, symbol).map(|e| e.market.clone()),
            _ => None,
        };
        Self {
            source: Some(source.into()),
            market,
            symbol: Some(symbol.into()),
            date: date.map(str::to_string),
            search: String::new(),
        }
    }

    /// 处理一条消息。上一级变了，下一级若不再成立就清掉（换数据源后市场不在了、换市场后标的不在了……）。
    pub fn update(&mut self, m: DataPickMsg) {
        let cat = match catalog() {
            Load::Ready(c) => Some(c),
            _ => None,
        };
        match m {
            DataPickMsg::Source(s) => {
                self.source = Some(s.clone());
                if s == LIVE {
                    self.market = None;
                    self.symbol = None;
                    self.date = None;
                    return;
                }
                if let Some(c) = &cat {
                    let ms = c.markets(&s);
                    if self.market.as_ref().is_none_or(|m| !ms.contains(m)) {
                        // 这个源只有一个市场时直接选上
                        self.market = (ms.len() == 1).then(|| ms[0].clone());
                    }
                    if let (Some(m), Some(sym)) = (&self.market, &self.symbol) {
                        if c.entry(&s, sym).is_none_or(|e| &e.market != m) {
                            self.symbol = None;
                        }
                    } else {
                        self.symbol = None;
                    }
                }
                self.date = None;
            }
            DataPickMsg::Market(mk) => {
                if self.market.as_deref() != Some(mk.as_str()) {
                    self.market = Some(mk);
                    self.symbol = None;
                    self.date = None;
                }
            }
            DataPickMsg::Symbol(sym) => {
                if self.symbol.as_deref() != Some(sym.as_str()) {
                    self.symbol = Some(sym);
                    self.date = None;
                }
            }
            DataPickMsg::Date(d) => self.date = Some(d),
            DataPickMsg::Search(q) => self.search = q,
            DataPickMsg::Refresh => refresh(),
        }
    }

    #[must_use]
    pub fn is_live(&self) -> bool {
        self.source.as_deref() == Some(LIVE)
    }

    /// 选完了：数据源 + 市场 + 标的（+ 要求时的日期）。实时 / 没选完返回 `None`。
    #[must_use]
    pub fn selection(&self, opts: &PickOpts) -> Option<Selection> {
        if self.is_live() {
            return None;
        }
        let date = if opts.date { Some(self.date.clone()?) } else { None };
        Some(Selection {
            source: self.source.clone()?,
            market: self.market.clone()?,
            symbol: self.symbol.clone()?,
            date,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "source\ttardis\t1\t/t\n\
source\tdatabento\t1\t/d\n\
source\trecorder\t0\t/r\n\
symbol\ttardis\tcrypto\tBTCUSDT\t30\t2026-06-01\t2026-06-30\n\
symbol\ttardis\tcrypto\tETHUSDT\t30\t2026-06-01\t2026-06-30\n\
symbol\tdatabento\tequity\tAAPL\t1\t2026-09-01\t2026-09-01\n\
symbol\tdatabento\tfuture\tESU6\t1\t2026-09-01\t2026-09-01\n\
symbol\tdatabento\tfuture\tGCZ6\t1\t2026-09-01\t2026-09-01\n\
garbage line\n";

    #[test]
    fn 目录解析与按源按市场筛() {
        let c = Catalog::parse(SAMPLE);
        assert_eq!(c.sources.len(), 3);
        assert!(!c.sources[2].available);
        assert_eq!(c.entries.len(), 5);
        assert_eq!(c.markets("databento"), vec!["equity".to_string(), "future".into()]);
        assert_eq!(c.markets("tardis"), vec!["crypto".to_string()]);
        let f: Vec<&str> = c.symbols("databento", "future", "").iter().map(|e| e.symbol.as_str()).collect();
        assert_eq!(f, vec!["ESU6", "GCZ6"]);
        let s: Vec<&str> = c.symbols("databento", "future", "gc").iter().map(|e| e.symbol.as_str()).collect();
        assert_eq!(s, vec!["GCZ6"], "搜索不分大小写");
        assert_eq!(c.entry("tardis", "ETHUSDT").unwrap().days, 30);
    }

    #[test]
    fn 实时没有下级选择也不算选完() {
        let mut p = DataPick::default();
        p.update(DataPickMsg::Source(LIVE.into()));
        assert!(p.is_live());
        assert!(p.selection(&PickOpts { date: true, ..PickOpts::default() }).is_none());
    }

    #[test]
    fn 换市场清掉标的和日期_选完才有结果() {
        let mut p = DataPick {
            source: Some("databento".into()),
            market: Some("future".into()),
            symbol: Some("ESU6".into()),
            date: Some("2026-09-01".into()),
            search: String::new(),
        };
        let opts = PickOpts { date: true, ..PickOpts::default() };
        assert_eq!(p.selection(&opts).unwrap().symbol, "ESU6");
        p.update(DataPickMsg::Market("equity".into()));
        assert!(p.symbol.is_none() && p.date.is_none());
        assert!(p.selection(&opts).is_none());
        p.update(DataPickMsg::Symbol("AAPL".into()));
        assert!(p.selection(&opts).is_none(), "要求选日期时没选日期不算完");
        assert!(p.selection(&PickOpts::default()).is_some(), "不要求日期时选到标的即可");
        p.update(DataPickMsg::Date("2026-09-01".into()));
        let s = p.selection(&opts).unwrap();
        assert_eq!((s.source.as_str(), s.market.as_str(), s.symbol.as_str()), ("databento", "equity", "AAPL"));
    }
}
