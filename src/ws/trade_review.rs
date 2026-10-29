//! 「交易核对」（docs/56）：一次回测的逐笔交易画在秒级 / 1 分钟 K 线上，人工确认开平仓位置。
//!
//! 数据全是文件（面板不算任何东西）：
//! - 交易清单 `~/ws-data/trade_review/<运行>/`：`run.json`（运行状态与进度）+ `trades.jsonl`（每平一笔追加一行）。
//!   完整引擎、网格宿主、列式、策略中心的运行都自动写；跑的时候这里边跑边出笔。
//! - K 线数据 `chart/<日期>.json`：`python -m strategies.trade_journal chart` 生成（Tardis 原始逐笔成交 + 特征仓库买一卖一），
//!   某天缺或笔数变了就起一次。
//! - 核对结果 `review.json`：本面板写，按「开仓纳秒|线」认笔。
//! - 别的交易文件用「导入」：`python -m strategies.trade_journal import <路径>`。

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

// ── 数据 ───────────────────────────────────────────────────────────────

#[derive(Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Progress {
    #[serde(default)]
    pub pct: Option<f64>,
    #[serde(default)]
    pub now_ns: Option<i64>,
    #[serde(default)]
    pub days_done: Option<u32>,
    #[serde(default)]
    pub days_total: Option<u32>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Deserialize, Debug, Clone, Default, PartialEq)]
pub struct RunMeta {
    #[serde(default)]
    pub run_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub progress: Progress,
    #[serde(default)]
    pub engine: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(default)]
    pub vendor: Option<String>,
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub end: Option<String>,
    #[serde(default)]
    pub strategy: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub started_at: f64,
    #[serde(default)]
    pub updated_at: f64,
}

impl RunMeta {
    pub fn running(&self) -> bool {
        self.state == "running"
    }

    /// 运行中但 run.json 超过 10 分钟没更新：多半是进程没了（被杀 / 崩了）却没来得及写状态。
    pub fn stale(&self) -> bool {
        let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
        self.running() && now - self.updated_at > 600.0
    }
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct Trade {
    #[serde(default)]
    pub lane: String,
    #[serde(default)]
    pub dir: String,
    pub open_ns: i64,
    pub close_ns: i64,
    #[serde(default)]
    pub entry: Option<f64>,
    #[serde(default)]
    pub exit: Option<f64>,
    #[serde(default)]
    pub qty: Option<f64>,
    #[serde(default)]
    pub gross: Option<f64>,
    #[serde(default)]
    pub fee: Option<f64>,
    #[serde(default)]
    pub net: Option<f64>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub day: String,
}

impl Trade {
    pub fn long(&self) -> bool {
        self.dir == "long"
    }
    pub fn key(&self) -> String {
        format!("{}|{}", self.open_ns, self.lane)
    }
    pub fn hold_s(&self) -> f64 {
        (self.close_ns - self.open_ns) as f64 / 1e9
    }
    pub fn reason_or_dash(&self) -> String {
        self.reason.clone().unwrap_or_else(|| "—".into())
    }
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct Bars {
    #[serde(default)]
    pub t: Vec<i64>,
    #[serde(default)]
    pub o: Vec<f64>,
    #[serde(default)]
    pub h: Vec<f64>,
    #[serde(default)]
    pub l: Vec<f64>,
    #[serde(default)]
    pub c: Vec<f64>,
    #[serde(default)]
    pub v: Vec<f64>,
    #[serde(default)]
    pub bv: Vec<f64>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct Window {
    pub key: String,
    #[serde(flatten)]
    pub bars: Bars,
    /// 每秒最后的买一 / 卖一
    #[serde(default)]
    pub qt: Vec<i64>,
    #[serde(default)]
    pub qb: Vec<f64>,
    #[serde(default)]
    pub qa: Vec<f64>,
    /// 开 / 平那一刻的 [买一, 卖一]
    #[serde(default)]
    pub open_touch: Option<[f64; 2]>,
    #[serde(default)]
    pub close_touch: Option<[f64; 2]>,
    /// 微观曲线（docs/67 T2）：特征仓库里每秒最后一个值
    #[serde(default)]
    pub micro: Option<Micro>,
}

/// 订单详情的微观曲线（docs/67 T2）。`None` = 那一秒没有有效值。
#[derive(Deserialize, Debug, Clone, Default)]
pub struct Micro {
    #[serde(default)]
    pub t: Vec<i64>,
    /// 价差（bp）
    #[serde(default)]
    pub spread: Vec<Option<f64>>,
    /// 前 5 档盘口失衡（−1…1）
    #[serde(default)]
    pub obi: Vec<Option<f64>>,
    /// 5 秒成交失衡（−1…1）
    #[serde(default)]
    pub timb: Vec<Option<f64>>,
    /// 累计成交量差（窗口起点归零）
    #[serde(default)]
    pub cvd: Vec<Option<f64>>,
}

/// 图表数据的版本（与 `strategies/trade_journal.py` 的 `CHART_VER` 一致）：旧的笔数没变也要重生成。
pub const CHART_VER: u32 = 2;

#[derive(Deserialize, Debug, Clone, Default)]
pub struct ChartDay {
    #[serde(default)]
    pub day: String,
    /// 图表数据版本（旧文件没有 = 1）
    #[serde(default = "ver1")]
    pub ver: u32,
    #[serde(default)]
    pub n: usize,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub quotes: Option<String>,
    #[serde(default)]
    pub minute: Bars,
    #[serde(default)]
    pub windows: Vec<Window>,
}

fn ver1() -> u32 {
    1
}

impl ChartDay {
    pub fn window(&self, key: &str) -> Option<&Window> {
        self.windows.iter().find(|w| w.key == key)
    }
}

/// 整段运行的 K 线（`chart/overview.json`）：全景图用。
#[derive(Deserialize, Debug, Clone, Default)]
pub struct Overview {
    #[serde(default)]
    pub start: String,
    #[serde(default)]
    pub end: String,
    /// 一根几秒（60 或 300）
    #[serde(default)]
    pub gran: f64,
    #[serde(default)]
    pub bars: Bars,
    #[serde(default)]
    pub missing: Vec<String>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Rev {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

#[derive(Debug, Clone)]
pub struct RunItem {
    pub dir: String,
    pub meta: RunMeta,
    pub n: usize,
}

// ── 状态 ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Status {
    #[default]
    All,
    None,
    Ok,
    Doubt,
}

#[derive(Default)]
struct St {
    runs: Vec<RunItem>,
    runs_at: Option<Instant>,
    /// 选中的运行（目录名）
    sel: Option<String>,
    /// 跟随最新运行：有新的运行开始就切过去
    follow: bool,
    meta: Option<RunMeta>,
    meta_mtime: Option<SystemTime>,
    trades: Arc<Vec<Trade>>,
    /// trades.jsonl 读到哪个字节（只追加，增量读）
    offset: u64,
    charts: HashMap<String, Arc<ChartDay>>,
    chart_mtime: HashMap<String, SystemTime>,
    overview: Option<Arc<Overview>>,
    overview_mtime: Option<SystemTime>,
    chart_busy: bool,
    chart_at: Option<Instant>,
    chart_err: String,
    review: Arc<HashMap<String, Rev>>,
    // 筛选
    f_lane: Option<String>,
    f_dir: Option<String>,
    f_reason: Option<String>,
    f_status: Status,
    /// 选中的笔（trades 里的下标）
    cur: Option<usize>,
    import_path: String,
    import_busy: bool,
    note: String,
}

static ST: OnceLock<Mutex<St>> = OnceLock::new();
static POLLER: OnceLock<()> = OnceLock::new();
/// 只在面板显示时轮询（docs/35 §16.10）。
static DEMAND: super::svcctl::Demand = super::svcctl::Demand::new();

fn st() -> &'static Mutex<St> {
    ST.get_or_init(|| Mutex::new(St { follow: true, ..Default::default() }))
}

fn with<R>(f: impl FnOnce(&mut St) -> R) -> R {
    let mut g = st().lock().unwrap_or_else(|e| e.into_inner());
    f(&mut g)
}

pub fn root() -> PathBuf {
    std::env::var_os("WS_TRADE_REVIEW_DIR").map(PathBuf::from).unwrap_or_else(|| super::paths::hot_dir().join("trade_review"))
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

fn read_meta(dir: &Path) -> Option<RunMeta> {
    let s = std::fs::read_to_string(dir.join("run.json")).ok()?;
    serde_json::from_str(&s).ok()
}

fn count_lines(p: &Path) -> usize {
    std::fs::read(p).map(|b| b.iter().filter(|c| **c == b'\n').count()).unwrap_or(0)
}

/// 清单目录列表：运行中的在前，其余按最后更新时间新的在前。
pub fn list_runs(root: &Path) -> Vec<RunItem> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else { return out };
    for e in rd.flatten() {
        let d = e.path();
        if let Some(meta) = read_meta(&d) {
            let n = count_lines(&d.join("trades.jsonl"));
            out.push(RunItem { dir: e.file_name().to_string_lossy().into_owned(), meta, n });
        }
    }
    out.sort_by(|a, b| {
        let ra = a.meta.running() && !a.meta.stale();
        let rb = b.meta.running() && !b.meta.stale();
        rb.cmp(&ra).then(b.meta.updated_at.total_cmp(&a.meta.updated_at))
    });
    out
}

/// 增量读 trades.jsonl：从 `offset` 起读到最后一个完整行，返回新的笔与新的 offset。
pub fn read_new(p: &Path, offset: u64) -> (Vec<Trade>, u64) {
    let Ok(mut f) = std::fs::File::open(p) else { return (Vec::new(), offset) };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len < offset {
        return read_new(p, 0); // 文件被重写（同名重跑）：从头读
    }
    if f.seek(SeekFrom::Start(offset)).is_err() {
        return (Vec::new(), offset);
    }
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return (Vec::new(), offset);
    }
    let Some(last_nl) = buf.iter().rposition(|c| *c == b'\n') else { return (Vec::new(), offset) };
    let mut out = Vec::new();
    for line in buf[..last_nl].split(|c| *c == b'\n') {
        if let Ok(t) = serde_json::from_slice::<Trade>(line) {
            out.push(t);
        }
    }
    (out, offset + last_nl as u64 + 1)
}

fn load_review(dir: &Path) -> HashMap<String, Rev> {
    std::fs::read_to_string(dir.join("review.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn save_review(dir: &Path, r: &HashMap<String, Rev>) {
    let keep: BTreeMap<&String, &Rev> = r.iter().filter(|(_, v)| v.s.is_some() || !v.note.is_empty()).collect();
    if let Ok(s) = serde_json::to_string_pretty(&keep) {
        let tmp = dir.join(".review.json.tmp");
        if std::fs::write(&tmp, s).is_ok() {
            let _ = std::fs::rename(tmp, dir.join("review.json"));
        }
    }
}

fn switch_to(g: &mut St, dir: Option<String>) {
    if g.sel == dir {
        return;
    }
    g.sel = dir;
    g.meta = None;
    g.meta_mtime = None;
    g.trades = Arc::new(Vec::new());
    g.offset = 0;
    g.charts.clear();
    g.chart_mtime.clear();
    g.overview = None;
    g.overview_mtime = None;
    g.chart_err.clear();
    g.cur = None;
    g.f_lane = None;
    g.f_dir = None;
    g.f_reason = None;
    g.f_status = Status::All;
    g.review = Arc::new(g.sel.as_ref().map(|d| load_review(&root().join(d))).unwrap_or_default());
}

/// 轮询一轮（后台线程每秒一次）。
fn poll() {
    let r = root();
    let refresh_list = with(|g| g.runs_at.is_none_or(|t| t.elapsed() > Duration::from_secs(3)));
    if refresh_list {
        let runs = list_runs(&r);
        with(|g| {
            // 跟随：有运行中的就跟它；没有就停在当前（第一次打开选最新的）
            let newest = runs.first().map(|x| x.dir.clone());
            let live = runs.iter().find(|x| x.meta.running() && !x.meta.stale()).map(|x| x.dir.clone());
            let target = if g.follow { live.or_else(|| g.sel.clone()).or(newest) } else { g.sel.clone().or(newest) };
            switch_to(g, target);
            g.runs = runs;
            g.runs_at = Some(Instant::now());
        });
    }
    let Some(sel) = with(|g| g.sel.clone()) else { return };
    let d = r.join(&sel);
    // run.json
    let mt = mtime(&d.join("run.json"));
    if mt.is_some() && with(|g| g.meta_mtime != mt) {
        let m = read_meta(&d);
        with(|g| {
            if g.sel.as_deref() == Some(sel.as_str()) {
                g.meta = m;
                g.meta_mtime = mt;
            }
        });
    }
    // 新的笔
    let off = with(|g| g.offset);
    let (new, off2) = read_new(&d.join("trades.jsonl"), off);
    if off2 != off {
        with(|g| {
            if g.sel.as_deref() != Some(sel.as_str()) {
                return;
            }
            let mut v: Vec<Trade> = if off2 < off || off == 0 { Vec::new() } else { (*g.trades).clone() };
            v.extend(new);
            // 按开仓时间排（快速档各天并行，先跑完的那天先写）；# 就是时间顺序。选中的按笔认、不按下标
            let keep = g.cur.and_then(|i| g.trades.get(i)).map(Trade::key);
            v.sort_by(|a, b| a.open_ns.cmp(&b.open_ns).then_with(|| a.lane.cmp(&b.lane)));
            g.cur = keep.and_then(|k| v.iter().position(|t| t.key() == k)).or(g.cur);
            g.trades = Arc::new(v);
            g.offset = off2;
            if g.cur.is_none() && !g.trades.is_empty() {
                g.cur = Some(0);
            }
        });
    }
    // 图表数据：读变了的；缺的 / 笔数不对的起一次 Python
    let (trades, running) = with(|g| (g.trades.clone(), g.meta.as_ref().is_some_and(|m| m.running())));
    let mut per_day: BTreeMap<&str, usize> = BTreeMap::new();
    for t in trades.iter() {
        *per_day.entry(t.day.as_str()).or_default() += 1;
    }
    let mut need = false;
    for (day, n) in &per_day {
        let p = d.join("chart").join(format!("{day}.json"));
        let mt = mtime(&p);
        let known = with(|g| g.chart_mtime.get(*day).copied());
        if mt.is_some()
            && mt != known
            && let Some(c) = std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str::<ChartDay>(&s).ok())
        {
            let c = Arc::new(c);
            with(|g| {
                if g.sel.as_deref() == Some(sel.as_str()) {
                    g.charts.insert(day.to_string(), c);
                    g.chart_mtime.insert(day.to_string(), mt.unwrap_or(SystemTime::UNIX_EPOCH));
                }
            });
        }
        let have = with(|g| g.charts.get(*day).map(|c| (c.n, c.ver)));
        if have.is_none_or(|(k, v)| k != *n || v < CHART_VER) {
            need = true;
        }
    }
    // 全景图：文件变了就读；没有 / 范围与这次运行对不上就要生成
    let ov_path = d.join("chart").join("overview.json");
    let ov_mt = mtime(&ov_path);
    if ov_mt.is_some()
        && with(|g| g.overview_mtime != ov_mt)
        && let Some(o) = std::fs::read_to_string(&ov_path).ok().and_then(|s| serde_json::from_str::<Overview>(&s).ok())
    {
        let o = Arc::new(o);
        with(|g| {
            if g.sel.as_deref() == Some(sel.as_str()) {
                g.overview = Some(o);
                g.overview_mtime = ov_mt;
            }
        });
    }
    let want = with(|g| {
        let m = g.meta.as_ref();
        let a = m.and_then(|m| m.start.clone()).or_else(|| per_day.keys().next().map(|s| s.to_string()));
        let z = m.and_then(|m| m.end.clone()).or_else(|| per_day.keys().last().map(|s| s.to_string()));
        a.zip(z)
    });
    if let Some((a, z)) = want
        && with(|g| g.overview.as_ref().is_none_or(|o| o.start != a || o.end != z))
    {
        need = true;
    }
    // 运行中每 5 秒最多起一次（一天的 K 线约 0.5 秒）；生成失败的天不反复重试（文件里记着 n，笔数不变就不再起）
    let due = with(|g| !g.chart_busy && g.chart_at.is_none_or(|t| t.elapsed() > Duration::from_secs(if running { 5 } else { 1 })));
    if need && due {
        start_chart(d);
    }
}

fn start_chart(dir: PathBuf) {
    with(|g| {
        g.chart_busy = true;
        g.chart_at = Some(Instant::now());
    });
    super::spawn_named("ws-trade-chart", move || {
        let out = std::process::Command::new(super::paths::python())
            .args(["-m", "strategies.trade_journal", "chart"])
            .arg(&dir)
            .current_dir(super::paths::repo_root())
            .output();
        let err = match out {
            Ok(o) if o.status.success() => String::new(),
            Ok(o) => String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("生成 K 线数据失败").to_string(),
            Err(e) => format!("起不来 Python：{e}"),
        };
        with(|g| {
            g.chart_busy = false;
            g.chart_at = Some(Instant::now());
            g.chart_err = err;
        });
    });
}

fn ensure_poller() {
    POLLER.get_or_init(|| {
        super::spawn_named("ws-trade-review", || loop {
            DEMAND.wait(|| {});
            poll();
            std::thread::sleep(Duration::from_millis(800));
        });
    });
}

// ── 视图快照 ───────────────────────────────────────────────────────────

pub struct View {
    pub runs: Vec<RunItem>,
    pub sel: Option<String>,
    pub follow: bool,
    pub meta: Option<RunMeta>,
    pub trades: Arc<Vec<Trade>>,
    /// 筛选后的下标（trades 里）
    pub shown: Vec<usize>,
    pub cur: Option<usize>,
    pub charts: HashMap<String, Arc<ChartDay>>,
    pub overview: Option<Arc<Overview>>,
    pub chart_busy: bool,
    pub chart_err: String,
    pub review: Arc<HashMap<String, Rev>>,
    pub f_lane: Option<String>,
    pub f_dir: Option<String>,
    pub f_reason: Option<String>,
    pub f_status: Status,
    pub lanes: Vec<String>,
    pub reasons: Vec<String>,
    pub import_path: String,
    pub import_busy: bool,
    pub note: String,
}

impl View {
    pub fn status_of(&self, t: &Trade) -> Option<&str> {
        self.review.get(&t.key()).and_then(|r| r.s.as_deref())
    }
}

fn shown(g: &St) -> Vec<usize> {
    g.trades
        .iter()
        .enumerate()
        .filter(|(_, t)| g.f_lane.as_ref().is_none_or(|l| &t.lane == l))
        .filter(|(_, t)| g.f_dir.as_ref().is_none_or(|d| &t.dir == d))
        .filter(|(_, t)| g.f_reason.as_ref().is_none_or(|r| &t.reason_or_dash() == r))
        .filter(|(_, t)| {
            let s = g.review.get(&t.key()).and_then(|r| r.s.as_deref());
            match g.f_status {
                Status::All => true,
                Status::None => s.is_none(),
                Status::Ok => s == Some("ok"),
                Status::Doubt => s == Some("doubt"),
            }
        })
        .map(|(i, _)| i)
        .collect()
}

fn uniq(it: impl Iterator<Item = String>) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for x in it {
        if !v.contains(&x) {
            v.push(x);
        }
    }
    v
}

pub fn view() -> View {
    DEMAND.touch();
    ensure_poller();
    with(|g| View {
        runs: g.runs.clone(),
        sel: g.sel.clone(),
        follow: g.follow,
        meta: g.meta.clone(),
        trades: g.trades.clone(),
        shown: shown(g),
        cur: g.cur,
        charts: g.charts.clone(),
        overview: g.overview.clone(),
        chart_busy: g.chart_busy,
        chart_err: g.chart_err.clone(),
        review: g.review.clone(),
        f_lane: g.f_lane.clone(),
        f_dir: g.f_dir.clone(),
        f_reason: g.f_reason.clone(),
        f_status: g.f_status,
        lanes: uniq(g.trades.iter().map(|t| t.lane.clone())),
        reasons: uniq(g.trades.iter().map(|t| t.reason_or_dash())),
        import_path: g.import_path.clone(),
        import_busy: g.import_busy,
        note: g.note.clone(),
    })
}

/// 页签上的「运行中」徽标。
pub fn busy() -> bool {
    ST.get().is_some_and(|_| with(|g| g.meta.as_ref().is_some_and(|m| m.running() && !m.stale())))
}

// ── 消息 ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum TrMsg {
    SelectRun(String),
    Follow(bool),
    Lane(Option<String>),
    Dir(Option<String>),
    Reason(Option<String>),
    Status(Status),
    Select(usize),
    Prev,
    Next,
    /// "ok" / "doubt"：再点一次取消
    Mark(&'static str),
    Note(String),
    Copy,
    ImportPath(String),
    Import,
    Delete,
    Split(crate::ui::split::SplitMsg),
    /// 逐笔表（`crate::ui::grid` 命名表）的消息
    Table(crate::ui::grid::Named),
    /// 在回放里看这笔（docs/67 T1）
    Replay,
}

/// 开仓前、平仓后各看多久（秒）；回放最长几分钟（再长只看开仓后这么久）。
const REPLAY_PRE_S: i64 = 5 * 60;
const REPLAY_POST_S: i64 = 3 * 60;
const REPLAY_MAX_MIN: i64 = 240;

/// 看这一笔要回放的时段：`(日期, 起始 HH:MM, 分钟, 截短了的说明)`。按分钟取整、限在开仓那一天
/// （回放只读一天的文件，docs/67 T0）。
#[must_use]
pub fn replay_window(open_ns: i64, close_ns: i64) -> (String, String, u32, Option<String>) {
    const DAY: i64 = 86_400;
    let (o, c) = (open_ns.div_euclid(1_000_000_000), close_ns.div_euclid(1_000_000_000).max(open_ns.div_euclid(1_000_000_000)));
    let day0 = o.div_euclid(DAY) * DAY;
    let a = ((o - REPLAY_PRE_S).max(day0) / 60) * 60;
    let mut z = ((c + REPLAY_POST_S + 59) / 60) * 60;
    let mut cut = None;
    if z > day0 + DAY {
        z = day0 + DAY;
        cut = Some("平仓在第二天：只回放到开仓那天 UTC 24:00（回放不跨天）".to_string());
    }
    if z - a > REPLAY_MAX_MIN * 60 {
        z = a + REPLAY_MAX_MIN * 60;
        cut = Some(format!("持仓太长：只回放开仓前后 {} 小时", REPLAY_MAX_MIN / 60));
    }
    let date = chrono::DateTime::from_timestamp(day0, 0).map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default();
    let hhmm = format!("{:02}:{:02}", (a - day0) / 3600, (a - day0) % 3600 / 60);
    (date, hhmm, ((z - a) / 60).max(1) as u32, cut)
}

/// 逐笔表在 `crate::ui::grid` 里的名字。表的行 = 筛选后的笔（`shown` 的顺序），行下标 = 在 `shown` 里的位置。
pub const TABLE: &str = "trade_review.trades";

/// 让表里的选中跟上 `cur`（图上点选、键盘换笔、换筛选之后）。
fn sync_table(g: &St) {
    let s = shown(g);
    crate::ui::grid::named_select(TABLE, g.cur.and_then(|c| s.iter().position(|x| *x == c)));
}

/// 上一笔 / 下一笔：按表里当前的显示顺序（表头排序、表内过滤之后）走。
fn step(g: &mut St, d: isize) {
    let s = shown(g);
    if s.is_empty() {
        return;
    }
    let seq: Vec<usize> = match crate::ui::grid::named_order(TABLE) {
        Some(o) if !o.is_empty() => o.into_iter().filter_map(|p| s.get(p).copied()).collect(),
        _ => s,
    };
    if seq.is_empty() {
        return;
    }
    let pos = g.cur.and_then(|c| seq.iter().position(|x| *x == c));
    let k = match pos {
        Some(p) => (p as isize + d).clamp(0, seq.len() as isize - 1) as usize,
        None => 0,
    };
    g.cur = Some(seq[k]);
    sync_table(g);
}

fn fmt_utc(ns: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_nanos(ns).format("%m-%d %H:%M:%S").to_string()
}

pub fn review_text(trades: &[Trade], review: &HashMap<String, Rev>, name: &str) -> String {
    let rows: Vec<String> = trades
        .iter()
        .enumerate()
        .filter_map(|(i, t)| {
            let r = review.get(&t.key())?;
            if r.s.is_none() && r.note.is_empty() {
                return None;
            }
            let s = match r.s.as_deref() {
                Some("ok") => "正确",
                Some("doubt") => "有疑问",
                _ => "未定",
            };
            let note = if r.note.is_empty() { String::new() } else { format!("：{}", r.note) };
            Some(format!("#{} {} {} {} {s}{note}", i + 1, fmt_utc(t.open_ns), t.lane, t.dir))
        })
        .collect();
    format!("{name} 核对（{}/{}）\n{}", rows.len(), trades.len(), rows.join("\n"))
}

pub fn handle(m: TrMsg) {
    match m {
        TrMsg::SelectRun(d) => with(|g| {
            g.follow = false;
            switch_to(g, Some(d));
        }),
        TrMsg::Follow(b) => with(|g| {
            g.follow = b;
            g.runs_at = None;
        }),
        TrMsg::Lane(v) => with(|g| {
            g.f_lane = v;
            sync_table(g);
        }),
        TrMsg::Dir(v) => with(|g| {
            g.f_dir = v;
            sync_table(g);
        }),
        TrMsg::Reason(v) => with(|g| {
            g.f_reason = v;
            sync_table(g);
        }),
        TrMsg::Status(v) => with(|g| {
            g.f_status = v;
            sync_table(g);
        }),
        TrMsg::Select(i) => with(|g| {
            g.cur = Some(i);
            sync_table(g);
        }),
        TrMsg::Table(crate::ui::grid::Named(k, m)) => {
            let pick = if let crate::ui::grid::GridMsg::Select(p) = &m { Some(*p) } else { None };
            crate::ui::grid::named_update(&k, m);
            if let Some(p) = pick {
                // 点行 = 选这一笔（再点一次不取消：右边总要有一笔可看）
                with(|g| {
                    if let Some(i) = shown(g).get(p).copied() {
                        g.cur = Some(i);
                    }
                    sync_table(g);
                });
            }
        }
        TrMsg::Prev => with(|g| step(g, -1)),
        TrMsg::Next => with(|g| step(g, 1)),
        TrMsg::Mark(s) => with(|g| {
            let (Some(i), Some(sel)) = (g.cur, g.sel.clone()) else { return };
            let Some(t) = g.trades.get(i) else { return };
            let k = t.key();
            let mut r = (*g.review).clone();
            let e = r.entry(k).or_default();
            e.s = if e.s.as_deref() == Some(s) { None } else { Some(s.to_string()) };
            save_review(&root().join(sel), &r);
            g.review = Arc::new(r);
        }),
        TrMsg::Note(s) => with(|g| {
            let (Some(i), Some(sel)) = (g.cur, g.sel.clone()) else { return };
            let Some(t) = g.trades.get(i) else { return };
            let mut r = (*g.review).clone();
            r.entry(t.key()).or_default().note = s;
            save_review(&root().join(sel), &r);
            g.review = Arc::new(r);
        }),
        TrMsg::Copy => with(|g| {
            let name = g.meta.as_ref().map(|m| m.name.clone()).unwrap_or_default();
            crate::ui::grid::post_clipboard(review_text(&g.trades, &g.review, &name));
        }),
        TrMsg::Replay => {
            let req = with(|g| {
                let i = g.cur?;
                let t = g.trades.get(i)?.clone();
                let m = g.meta.as_ref()?;
                Some((i, t, m.vendor.clone().unwrap_or_else(|| "tardis".into()), m.symbol.clone()?))
            });
            let Some((i, t, vendor, symbol)) = req else {
                with(|g| g.note = "这份清单没写标的：开不了回放".into());
                return;
            };
            let (date, hhmm, minutes, cut) = replay_window(t.open_ns, t.close_ns);
            let mark = super::replay_clock::TradeMark {
                open_ns: t.open_ns,
                close_ns: t.close_ns,
                entry: t.entry,
                exit: t.exit,
                long: t.long(),
                gross: t.gross,
                label: format!("#{} {}{}", i + 1, if t.long() { "多" } else { "空" }, t.reason.as_deref().map(|r| format!(" · {r}")).unwrap_or_default()),
            };
            let note = super::feature_source::open_trade(super::feature_source::TradeReq { vendor, symbol, date, hhmm, minutes, mark });
            with(|g| g.note = cut.map_or_else(|| note.clone(), |c| format!("{note}；{c}")));
            crate::ui::command::post(crate::ui::command::Cmd::Workspace(format!(
                "{}{}图表",
                super::workspace::WS_FEATURES,
                super::workspace::PAGE_SEP
            )));
        }
        TrMsg::ImportPath(s) => with(|g| g.import_path = s),
        TrMsg::Import => start_import(),
        TrMsg::Split(m) => crate::ui::split::handle(m),
        TrMsg::Delete => with(|g| {
            // 只删导入来的（运行写的清单留着，免得误删正在跑的）
            let Some(sel) = g.sel.clone() else { return };
            if !sel.starts_with("import-") {
                g.note = "只能删导入的清单；运行写的清单在 ~/ws-data/trade_review/ 里手动删".into();
                return;
            }
            let _ = std::fs::remove_dir_all(root().join(&sel));
            g.note = format!("已删除 {sel}");
            g.follow = true;
            g.sel = None;
            switch_to(g, None);
            g.runs_at = None;
        }),
    }
}

fn start_import() {
    let Some(path) = with(|g| {
        let p = g.import_path.trim().to_string();
        if p.is_empty() || g.import_busy {
            return None;
        }
        g.import_busy = true;
        g.note = format!("导入 {p} …");
        Some(p)
    }) else {
        return;
    };
    super::spawn_named("ws-trade-import", move || {
        let out = std::process::Command::new(super::paths::python())
            .args(["-m", "strategies.trade_journal", "import"])
            .arg(super::paths::expand_home(&path, std::env::var("HOME").ok().as_deref()))
            .current_dir(super::paths::repo_root())
            .output();
        let r: Result<String, String> = match out {
            Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).trim().to_string()),
            Ok(o) => Err(String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("导入失败").to_string()),
            Err(e) => Err(format!("起不来 Python：{e}")),
        };
        with(|g| {
            g.import_busy = false;
            match r {
                Ok(dir) => {
                    let name = Path::new(&dir).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    g.note = format!("已导入：{name}");
                    g.import_path.clear();
                    g.follow = false;
                    switch_to(g, Some(name));
                    g.runs_at = None;
                }
                Err(e) => g.note = e,
            }
        });
    });
}

#[cfg(test)]
mod tests {

    #[test]
    fn 回放时段按分钟取整且不跨天() {
        let ns = |h: i64, m: i64, sec: i64| ((20_605 * 86_400) + h * 3600 + m * 60 + sec) * 1_000_000_000; // 2026-06-01
        let (d, s, n, cut) = super::replay_window(ns(10, 7, 30), ns(10, 9, 10));
        assert_eq!((d.as_str(), s.as_str(), n, cut), ("2026-06-01", "10:02", 11, None));
        // 开在 00:02：起点不早于当天 00:00
        let (_, s, _, _) = super::replay_window(ns(0, 2, 0), ns(0, 3, 0));
        assert_eq!(s, "00:00");
        // 平仓在第二天：截到 24:00
        let (_, s, n, cut) = super::replay_window(ns(23, 50, 0), ns(24, 10, 0));
        assert_eq!((s.as_str(), n), ("23:45", 15));
        assert!(cut.is_some());
        // 持仓 6 小时：只看 4 小时
        let (_, _, n, cut) = super::replay_window(ns(1, 0, 0), ns(7, 0, 0));
        assert_eq!(n, 240);
        assert!(cut.is_some());
    }

    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ws-trade-review-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn 增量读只取完整行() {
        let d = tmp("incr");
        let p = d.join("trades.jsonl");
        let a = r#"{"lane":"A","dir":"long","open_ns":1,"close_ns":2,"day":"2026-06-01"}"#;
        std::fs::write(&p, format!("{a}\n{{\"lane\":\"B\"")).unwrap();
        let (v, off) = read_new(&p, 0);
        assert_eq!(v.len(), 1);
        assert_eq!(off as usize, a.len() + 1);
        // 半行写完
        std::fs::write(&p, format!("{a}\n{}\n", a.replace("\"A\"", "\"B\""))).unwrap();
        let (v, off2) = read_new(&p, off);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].lane, "B");
        assert!(off2 > off);
        // 文件被重写变短：从头读
        std::fs::write(&p, "").unwrap();
        let (v, off3) = read_new(&p, off2);
        assert!(v.is_empty());
        assert_eq!(off3, 0);
    }

    #[test]
    fn 运行中的排在前面() {
        let d = tmp("list");
        let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs_f64();
        for (name, state, at) in [("a", "done", now - 10.0), ("b", "running", now - 20.0), ("c", "done", now - 5.0)] {
            std::fs::create_dir_all(d.join(name)).unwrap();
            std::fs::write(d.join(name).join("run.json"), format!(r#"{{"run_id":"{name}","state":"{state}","updated_at":{at}}}"#)).unwrap();
        }
        let v: Vec<String> = list_runs(&d).into_iter().map(|r| r.dir).collect();
        assert_eq!(v, ["b", "c", "a"]);
    }

    #[test]
    fn 核对文字() {
        let t = Trade { lane: "回踩吸收".into(), dir: "short".into(), open_ns: 1_780_279_464_000_000_000, close_ns: 1_780_279_469_000_000_000, ..Default::default() };
        let mut r = HashMap::new();
        r.insert(t.key(), Rev { s: Some("doubt".into()), note: "入场偏早".into() });
        let s = review_text(&[t], &r, "6 月");
        assert!(s.starts_with("6 月 核对（1/1）"));
        assert!(s.contains("#1 06-01 02:04:24 回踩吸收 short 有疑问：入场偏早"), "{s}");
    }
}
