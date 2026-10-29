//! 订单流特征面板的数据源——共用数据选择组件（[`super::data_picker`]）的第二个宿主。
//!
//! 面板与四张图只读两份文件（旁路快照 `feature_matrix.json`、图表流 `feature_chart.jsonl`），
//! 谁写它们就是数据从哪来（docs/31 §8.2 的源无关）。这里只决定**读哪一组**：
//!
//! | 选择 | 谁写 | 读哪组 |
//! |---|---|---|
//! | 管线 B · B1 交易所实时 | 常驻引擎 `ws-features`（通道②） | 缺省那两份 |
//! | 管线 B · B2 购买数据 / B3 本地录制 | 主仓 `replay_source`（特征引擎直接读数据商接口） | `cockpit/feature_replay/` 下本次回放的那两份 |
//!
//! 回放写**自己的**文件，所以不用停常驻引擎，切回实时也不丢它这段时间的历史。
//! 回放用面板的启用集与时间窗口（`--config`），与常驻引擎同一份配置——表上的列就是它算的列。
//! 每次开始回放 / 切回实时，四张图清空重画（`Effect::ResetFeatureCharts`），不把两段数据画进同一张图。
//!
//! 管线 A 不可选：它只喂图表（docs/28 §1.2），特征要的是管线 B 的规范事件。
//!
//! ## 回放 = 先算后播（docs/51 P2）
//!
//! 回放进程（`replay_source --timeline-only`）全速算、边算边写一份**回放时间线**（`run_<时刻>/tl_*`），
//! 统一播放头（[`super::replay_clock`]）按倍速推进，四张图、特征矩阵、图表参数都按播放头取「这一刻」。
//! 暂停、步进、拖时间轴只改播放头；停止只停计算，已经算出来的那段照样能前后看。

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use super::data_picker::{
    BSource, DataPick, DataPickMsg, Pipeline, PickOpts, Purpose, Selection, TimeMode,
};

/// 面板对组件的要求：计算用途（管线 A 置灰）、B1–B3、时间选一段。
#[must_use]
pub fn pick_opts() -> PickOpts {
    PickOpts {
        purpose: Purpose::Compute,
        sources: Some(vec![BSource::Live, BSource::Purchased, BSource::Recorded]),
        vendors: None,
        time: TimeMode::Window,
        ..PickOpts::default()
    }
}

pub use super::replay_clock::Pace;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceMsg {
    Data(DataPickMsg),
    Pace(Pace),
    /// 展开 / 收起选择器。
    Toggle,
    Start,
    /// 暂停 / 继续（切换）：只改播放头，计算照跑。
    Pause,
    /// 停止计算（已经算出来的那段照样能看）。
    Stop,
    /// 不用缓存、重算正在看的这一段（原始数据换过时；docs/67 T1b）。
    Recompute,
}

/// 一次回放。
struct Run {
    /// 回放进程；用缓存时没有（docs/67 T1b）
    child: Option<Child>,
    /// 这次写到 / 读的缓存目录（算完了才算数）
    dir: PathBuf,
    desc: String,
    log: PathBuf,
    started: Instant,
    /// 结束后的一行结论（`None` = 还在算）。
    done: Option<String>,
    /// 这次回放选的数据（链路徽标逐跳显示用）。
    sel: Selection,
    /// 日志首行（引擎配置：启用集 / 窗口），读到一次就缓存。
    engine_line: Option<String>,
}

struct St {
    pick: DataPick,
    pace: Pace,
    open: bool,
    run: Option<Run>,
    /// 面板正在读的回放：`(时间线目录, 链路徽标盯的输出文件, 说明)`；`None` = 常驻引擎那两份。
    reading: Option<(PathBuf, PathBuf, String)>,
    note: String,
    /// 交易核对要开的那笔（docs/67 T1）：等本地目录扫描好（补市场与类型）再开
    pending_trade: Option<(TradeReq, Instant)>,
}

/// 从交易核对开一段回放看一笔（docs/67 T1）。
#[derive(Debug, Clone)]
pub struct TradeReq {
    pub vendor: String,
    pub symbol: String,
    pub date: String,
    pub hhmm: String,
    pub minutes: u32,
    pub mark: super::replay_clock::TradeMark,
}

static ST: OnceLock<Mutex<St>> = OnceLock::new();

fn cell() -> &'static Mutex<St> {
    ST.get_or_init(|| {
        // 缺省 = 现状：B1 · Binance U 本位永续 · BTCUSDT（常驻引擎在算的那一个）
        let mut pick = DataPick::default();
        for m in [
            DataPickMsg::Pipeline(Pipeline::B),
            DataPickMsg::Source(BSource::Live),
            DataPickMsg::Venue("Binance".into()),
            DataPickMsg::Market("linear".into()),
            DataPickMsg::Symbol("BTCUSDT".into()),
        ] {
            pick.update(m);
        }
        Mutex::new(St { pick, pace: Pace::X10, open: false, run: None, reading: None, note: String::new(), pending_trade: None })
    })
}

/// 回放输出目录。
#[must_use]
pub fn replay_dir() -> PathBuf {
    super::paths::hot_dir().join("cockpit").join("feature_replay")
}

/// `replay_source` 可执行文件（主仓 release 例子；`WS_REPLAY_SOURCE_BIN` 可覆盖）。
#[must_use]
pub fn replay_bin() -> PathBuf {
    std::env::var("WS_REPLAY_SOURCE_BIN").map_or_else(
        |_| super::paths::repo_root().join("target/release/examples/replay_source"),
        PathBuf::from,
    )
}

/// 回放的时间线目录（回放中 / 回放过；B1 = `None`）。
#[must_use]
pub fn matrix_override() -> Option<PathBuf> {
    cell().lock().ok()?.reading.as_ref().map(|r| r.0.clone())
}

/// 链路徽标盯的回放输出（时间线的成交文件；同上）。非 `None` = 正在看回放。
#[must_use]
pub fn chart_override() -> Option<PathBuf> {
    cell().lock().ok()?.reading.as_ref().map(|r| r.1.clone())
}

/// 给视图用的只读副本。
#[derive(Debug, Clone)]
pub struct View {
    pub pick: DataPick,
    pub pace: Pace,
    pub open: bool,
    /// 当前读的是什么（一行）。
    pub reading: String,
    pub is_replay: bool,
    /// 回放状态一行（没有回放 = 空）。
    pub status: String,
    pub running: bool,
    /// 有回放可看（在算或已算完 / 已停）。
    pub replay_on: bool,
    /// 播放头停着。
    pub paused: bool,
    /// 正在看的是缓存（没起回放程序；docs/67 T1b）
    pub cached: bool,
    pub note: String,
}

/// 视图快照；顺带收回放进程的结束状态、目录选择框的结果。
#[must_use]
pub fn view() -> View {
    let Ok(mut g) = cell().lock() else {
        return View {
            pick: DataPick::default(),
            pace: Pace::X10,
            open: false,
            reading: String::new(),
            is_replay: false,
            status: String::new(),
            running: false,
            replay_on: false,
            paused: false,
            cached: false,
            note: String::new(),
        };
    };
    g.pick.poll();
    let status = g.run.as_mut().map(poll_run).unwrap_or_default();
    View {
        pick: g.pick.clone(),
        pace: g.pace,
        open: g.open,
        reading: g.reading.as_ref().map_or_else(
            || "B1 · 交易所实时（常驻引擎 ws-features，Binance U 本位永续 BTCUSDT）".to_string(),
            |r| r.2.clone(),
        ),
        is_replay: g.reading.is_some(),
        status,
        running: g.run.as_ref().is_some_and(|r| r.done.is_none()),
        replay_on: g.reading.is_some() && super::replay_clock::active(),
        paused: super::replay_clock::view().is_some_and(|v| !v.playing),
        cached: g.reading.is_some() && g.run.as_ref().is_some_and(|r| r.child.is_none()),
        note: g.note.clone(),
    }
}

/// 回放状态一行：计算进程（结束时从日志里取结论）+ 播放头。
fn poll_run(r: &mut Run) -> String {
    if r.done.is_none()
        && let Some(Ok(Some(st))) = r.child.as_mut().map(Child::try_wait)
    {
        let log = std::fs::read_to_string(&r.log).unwrap_or_default();
        r.done = Some(if st.success() {
            let n = log.lines().find_map(|l| l.strip_prefix("重放 ")).unwrap_or("").trim();
            // 算完的一段留作缓存；超了上限从最久没用的删起（正在看的这段不删）
            let keep = r.dir.clone();
            super::spawn_named("ws-replaycache", move || evict(&cache_dir(), cache_budget(), &keep));
            format!("✔ 计算完成：{n}（用时 {}）", fmt_s(r.started.elapsed().as_secs()))
        } else {
            let last = log.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("（无输出）");
            format!("✗ 回放失败：{last}")
        });
    }
    let clock = super::replay_clock::view().map(|v| v.status()).unwrap_or_default();
    match &r.done {
        Some(d) if d.starts_with('✗') => format!("{d} · {}", r.desc),
        Some(d) => format!("{clock} · {d} · {}", r.desc),
        None => format!("{clock} · 计算中 {} · {}", fmt_s(r.started.elapsed().as_secs()), r.desc),
    }
}

fn fmt_s(s: u64) -> String {
    format!("{}:{:02}", s / 60, s % 60)
}

// ── 图的外壳（标的 / 刻度）跟着数据走 ─────────────────────────────────────────
//
// 四张图是 flowsurface 的行情图，外壳是一个 `TickerInfo`（交易所:标的 + 最小刻度 + 最小量），
// 价格轴与聚合粒度都按它来。回放美股 / 期货时沿用 BinanceLinear:BTCUSDT 的外壳，
// 价格照画但聚合粒度不对（ESU6 的 0.25 被按 0.1 × 倍数聚），标题也写着 BTCUSDT。
//
// 所以回放写出图表流的第一行（`meta`：标的、刻度、最小量）后，把外壳换成它：
// 后台线程等到这一行 → 记为「待换外壳」→ 主循环下一帧（`Message::Tick`）按它重建特征工作区的图。
// 交易所一栏仍写 BinanceLinear——flowsurface 的交易所枚举里没有 Databento，
// 而这些图的数据只来自图表流、从不连交易所（回放类工作区禁止补拉，`workspace::replay_mode()`）。

/// 图的外壳。
#[derive(Debug, Clone, PartialEq)]
pub struct Shell {
    pub symbol: String,
    pub tick: f32,
    pub min_qty: f32,
}

impl Shell {
    /// 常驻引擎那一个（Binance U 本位永续 BTCUSDT）。
    #[must_use]
    pub fn live() -> Self {
        Self { symbol: "BTCUSDT".into(), tick: 0.1, min_qty: 0.001 }
    }
}

/// `(待换的外壳, 现在的外壳)`。
static SHELL: OnceLock<Mutex<(Option<Shell>, Shell)>> = OnceLock::new();

fn shell_cell() -> &'static Mutex<(Option<Shell>, Shell)> {
    SHELL.get_or_init(|| Mutex::new((None, Shell::live())))
}

fn set_pending_shell(s: Shell) {
    if let Ok(mut g) = shell_cell().lock() {
        g.0 = Some(s);
    }
}

/// 主循环每帧取一次：有待换的外壳就返回它（并记为当前）。
pub fn take_shell() -> Option<Shell> {
    let mut g = shell_cell().lock().ok()?;
    let s = g.0.take()?;
    g.1 = s.clone();
    Some(s)
}

/// 外壳标的的写法约束（flowsurface `Ticker`：≤ 28 字节、ASCII、不含 `|`）。
pub(crate) fn shell_symbol(s: &str) -> Option<String> {
    let t: String = s.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')).collect();
    (!t.is_empty() && t.len() <= 28).then_some(t)
}

/// 等回放写出时间线的抬头（`tl_meta.json`：标的、刻度、最小量；最多 2 分钟：Databento 逐单窗口起点要先建簿）。
fn watch_meta(dir: PathBuf) {
    super::spawn_named("ws-featuresourc", move || {
        for _ in 0..600 {
            // 期间又换了一次回放 / 切回实时：这一份作废
            if matrix_override().as_ref() != Some(&dir) {
                return;
            }
            if let Ok(t) = std::fs::read_to_string(dir.join("tl_meta.json"))
                && let Ok(v) = serde_json::from_str::<serde_json::Value>(&t)
                && let Some(symbol) = v["symbol"].as_str().and_then(shell_symbol)
                && let Some(tick) = v["tick"].as_f64().filter(|x| *x > 0.0)
            {
                let lot = v["lot"].as_f64().unwrap_or(0.0);
                set_pending_shell(Shell { symbol, tick: tick as f32, min_qty: if lot > 0.0 { lot as f32 } else { 1.0 } });
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    });
}

/// 存盘前把布局里的回放外壳换回 BTCUSDT。
///
/// 布局按「交易所:标的」存图的流；下次启动时 flowsurface 按交易所的标的表去解析——
/// `BinanceLinear:ESU6` 解析不到，那几张图会一直停在「等待就绪」，连重置都做不了
/// （重置要先拿到就绪的流）。币安本来就有的标的（从 Tardis 回放 ETHUSDT）照存。
#[must_use]
pub fn scrub_saved(json: String) -> String {
    let cur = shell_cell().lock().map(|g| g.1.clone()).unwrap_or_else(|_| Shell::live());
    let native = super::data_picker::native_symbols("Binance", "linear", "");
    if cur.symbol == "BTCUSDT" || native.contains(&cur.symbol) {
        return json;
    }
    json.replace(&format!("\"BinanceLinear:{}\"", cur.symbol), "\"BinanceLinear:BTCUSDT\"")
}

/// 处理一条消息。返回 `true` = 图表流换了，请上层把四张图清空重画。
pub fn handle(m: SourceMsg) -> bool {
    let Ok(mut g) = cell().lock() else { return false };
    g.note.clear();
    match m {
        SourceMsg::Toggle => g.open = !g.open,
        SourceMsg::Pace(p) => {
            g.pace = p;
            super::replay_clock::set_pace(p);
        }
        SourceMsg::Data(d) => {
            g.pick.update(d);
            // 选回 B1 = 回到常驻引擎那两份（正在跑的回放停掉）
            if g.pick.source == Some(BSource::Live) && g.reading.is_some() {
                stop(&mut g);
                g.reading = None;
                super::replay_clock::end();
                // 图的外壳换回常驻引擎的那一个
                set_pending_shell(Shell::live());
                return true;
            }
        }
        SourceMsg::Stop => {
            stop(&mut g);
            g.note = "已停止计算；已经算出来的那段照样能前后看".into();
        }
        SourceMsg::Start => return start(&mut g),
        SourceMsg::Recompute => {
            let Some(sel) = g.run.as_ref().map(|r| r.sel.clone()) else {
                g.note = "没有在看的回放".into();
                return false;
            };
            return launch_with(&mut g, sel, true);
        }
        SourceMsg::Pause => {
            if !super::replay_clock::active() {
                g.note = "没有回放".into();
                return false;
            }
            super::replay_clock::handle(super::replay_clock::Cmd::TogglePlay);
        }
    }
    false
}

fn stop(g: &mut St) {
    if let Some(r) = g.run.as_mut()
        && r.done.is_none()
    {
        if let Some(c) = r.child.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
        r.done = Some("■ 已停止".into());
    }
}

fn start(g: &mut St) -> bool {
    let Some(sel) = g.pick.selection(&pick_opts()) else {
        g.note = "还没选完：本地数据要选到 标的 + 日期（并至少留一类数据）".into();
        return false;
    };
    launch(g, sel)
}

/// 样张 / 验收用：`WS_FEATURE_REPLAY=数据商|根目录|市场|标的|日期|HH:MM|分钟` 启动时直接开一次回放
/// （与画布的 `WS_FLOW_OPEN` 同一类钩子；不走选择器）。
fn auto_start(g: &mut St) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let Ok(spec) = std::env::var("WS_FEATURE_REPLAY") else { return };
        let p: Vec<&str> = spec.split('|').collect();
        let [source, root, market, symbol, date, hhmm, minutes] = p.as_slice() else {
            g.note = format!("WS_FEATURE_REPLAY 要写成 数据商|根目录|市场|标的|日期|HH:MM|分钟：{spec}");
            return;
        };
        let sel = Selection::Local {
            source: (*source).into(),
            root: (*root).into(),
            market: (*market).into(),
            symbol: (*symbol).into(),
            date: (*date).into(),
            window: Some(((*hhmm).into(), minutes.parse().unwrap_or(10))),
            types: Vec::new(),
            end: (*date).into(),
            channel: String::new(),
        };
        launch(g, sel);
    });
}

/// 一段回放数据（画线分析记下它，打开分析时重开这段；docs/52 A2）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DataSeg {
    pub source: String,
    pub root: String,
    pub market: String,
    pub symbol: String,
    pub date: String,
    pub hhmm: String,
    pub minutes: u32,
    #[serde(default)]
    pub types: Vec<String>,
}

impl DataSeg {
    /// 一行说明（分析卡片里）。
    #[must_use]
    pub fn describe(&self) -> String {
        format!("{} {} {} {} 起 {} 分钟", vendor_label(&self.source), self.symbol, self.date, self.hhmm, self.minutes)
    }

    /// 同一段数据（根目录与类型不算：同一段换个挂载点还是同一段）。
    #[must_use]
    pub fn same_as(&self, o: &Self) -> bool {
        (self.source.as_str(), self.symbol.as_str(), self.date.as_str(), self.hhmm.as_str(), self.minutes)
            == (o.source.as_str(), o.symbol.as_str(), o.date.as_str(), o.hhmm.as_str(), o.minutes)
    }
}

/// 正在看的回放是哪段数据（没在回放 = `None`）。
#[must_use]
pub fn current_seg() -> Option<DataSeg> {
    let g = cell().lock().ok()?;
    g.reading.as_ref()?;
    match &g.run.as_ref()?.sel {
        Selection::Local { source, root, market, symbol, date, window, types, .. } => {
            let (hhmm, minutes) = window.clone().unwrap_or_else(|| ("00:00".into(), 30));
            Some(DataSeg {
                source: source.clone(),
                root: root.clone(),
                market: market.clone(),
                symbol: symbol.clone(),
                date: date.clone(),
                hhmm,
                minutes,
                types: types.clone(),
            })
        }
        _ => None,
    }
}

/// 开这段数据的回放（画线分析打开时；已经在看同一段就不重开）。返回是否新开了。
pub fn open_seg(seg: &DataSeg) -> bool {
    if current_seg().is_some_and(|c| c.same_as(seg)) {
        return false;
    }
    let Ok(mut g) = cell().lock() else { return false };
    let sel = Selection::Local {
        source: seg.source.clone(),
        root: seg.root.clone(),
        market: seg.market.clone(),
        symbol: seg.symbol.clone(),
        date: seg.date.clone(),
        window: Some((seg.hhmm.clone(), seg.minutes)),
        types: seg.types.clone(),
        end: seg.date.clone(),
        channel: String::new(),
    };
    launch(&mut g, sel)
}

/// 主循环每帧调：处理 `WS_FEATURE_REPLAY`（只第一次有事）与交易核对要开的那笔。
pub fn tick() {
    if let Ok(mut g) = cell().lock() {
        auto_start(&mut g);
        try_trade(&mut g);
    }
}

/// 开一段回放看这一笔（docs/67 T1）：数据选择器预置成 B2 · 这个数据商 · 标的 · 日期 · 时段，扫描好了就开，
/// 开了以后播放头停在开仓那一刻。返回一行说明（给交易核对显示）。
pub fn open_trade(req: TradeReq) -> String {
    let Ok(mut g) = cell().lock() else { return "回放状态锁坏了".into() };
    g.pick.preset_local(BSource::Purchased, Some(&req.vendor), &req.symbol, Some(&req.date), None, Some((&req.hhmm, req.minutes)));
    g.note = format!("准备回放 {} {} {} 起 {} 分钟（扫描本地目录…）", req.symbol, req.date, req.hhmm, req.minutes);
    g.pending_trade = Some((req, Instant::now()));
    try_trade(&mut g);
    g.note.clone()
}

fn try_trade(g: &mut St) {
    let Some((req, since)) = g.pending_trade.clone() else { return };
    if let Some(sel) = g.pick.selection(&pick_opts()) {
        g.pending_trade = None;
        let ok = launch(g, sel);
        if ok {
            super::replay_clock::set_trade(req.mark);
            let cached = g.run.as_ref().is_some_and(|r| r.child.is_none());
            g.note = format!(
                "回放 {} {} {} 起 {} 分钟：{}",
                req.symbol,
                req.date,
                req.hhmm,
                req.minutes,
                if cached { "用上次算好的结果（不重算），停在开仓那一刻" } else { "算到开仓那一刻就停在那里" }
            );
        }
        crate::ui::command::post(crate::ui::command::Cmd::Notice(
            if ok { g.note.clone() } else { format!("在回放里看这笔：{}", g.note) },
            !ok,
        ));
        return;
    }
    let failed = match g.pick.local_scan() {
        Some(super::data_picker::Load::Failed(e)) => Some(format!("扫描本地目录失败：{e}")),
        Some(super::data_picker::Load::Ready(_)) => Some(format!("本地没有 {} {} 的数据（B2 · {}，缺省根目录）", req.symbol, req.date, req.vendor)),
        _ if since.elapsed().as_secs() > 60 => Some("扫描本地目录超过 1 分钟还没好".into()),
        _ => None,
    };
    if let Some(e) = failed {
        g.pending_trade = None;
        crate::ui::command::post(crate::ui::command::Cmd::Notice(format!("在回放里看这笔：{e}"), true));
        g.note = e;
    }
}

/// 回放面板的一行说明（交易核对开回放后显示进度 / 出错）。
#[must_use]
pub fn note() -> String {
    cell().lock().map(|g| g.note.clone()).unwrap_or_default()
}

fn launch(g: &mut St, sel: Selection) -> bool {
    launch_with(g, sel, false)
}

// ── 回放结果缓存（docs/67 T1b）─────────────────────────────────────────────
//
// 同一段数据 + 同一份特征配置 + 同一个回放程序 = 同一个目录；算完了（`tl_meta.json` 的 done）下次直接读，
// 不起回放程序。总量超过上限按最近使用从旧删起。

/// 缓存目录。
#[must_use]
pub fn cache_dir() -> PathBuf {
    replay_dir().join("cache")
}

/// 缓存总量上限（字节；`WS_REPLAY_CACHE_GB`，缺省 8 GB）。
fn cache_budget() -> u64 {
    let gb = std::env::var("WS_REPLAY_CACHE_GB").ok().and_then(|v| v.trim().parse::<f64>().ok()).filter(|g| *g > 0.0).unwrap_or(8.0);
    (gb * 1e9) as u64
}

/// FNV-1a 64：固定算法（`DefaultHasher` 不保证跨版本一致，缓存键要稳定）。
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3))
}

/// 缓存键的原文与目录名。根目录不算（同一段换个挂载点还是同一段）；数据类型排序后算。
fn cache_key(source: &str, symbol: &str, date: &str, hhmm: &str, minutes: u32, types: &[String], config: &[u8], bin: &str) -> (String, String) {
    let mut ty = types.to_vec();
    ty.sort();
    let text = format!("{source}|{symbol}|{date}|{hhmm}|{minutes}|{}|config {:016x}|bin {bin}", ty.join(","), fnv(config));
    let safe: String = symbol.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')).collect();
    let name = format!("{safe}_{date}_{}_{minutes}m_{:016x}", hhmm.replace(':', ""), fnv(text.as_bytes()));
    (text, name)
}

/// 回放程序的「版本」：大小 + 修改时间（重编了就变）。
fn bin_stamp(bin: &std::path::Path) -> String {
    std::fs::metadata(bin)
        .map(|m| {
            let t = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
            format!("{}@{t}", m.len())
        })
        .unwrap_or_default()
}

/// 这个目录是完整算完的一段吗。
fn cache_done(dir: &std::path::Path) -> bool {
    std::fs::read(dir.join("tl_meta.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .is_some_and(|v| v["done"].as_bool() == Some(true))
}

/// 用过一次：摸一下 `used`（按它的修改时间淘汰）。
fn touch_used(dir: &std::path::Path) {
    let _ = std::fs::write(dir.join("used"), b"");
}

fn dir_size(dir: &std::path::Path) -> u64 {
    std::fs::read_dir(dir).map_or(0, |rd| {
        rd.flatten()
            .map(|e| match e.metadata() {
                Ok(m) if m.is_dir() => dir_size(&e.path()),
                Ok(m) => m.len(),
                Err(_) => 0,
            })
            .sum()
    })
}

/// 清缓存：没算完的（不是 `keep`）删掉；算完的总量超过 `budget` 就按最近使用从旧删起。`keep` 永远不删。
fn evict(root: &std::path::Path, budget: u64, keep: &std::path::Path) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    // (最近使用, 大小, 目录)；读不到时间 = None = 最旧
    let mut done: Vec<(Option<std::time::SystemTime>, u64, PathBuf)> = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_dir() || p == keep {
            continue;
        }
        if !cache_done(&p) {
            let _ = std::fs::remove_dir_all(&p);
            continue;
        }
        let used = std::fs::metadata(p.join("used")).or_else(|_| std::fs::metadata(p.join("tl_meta.json"))).and_then(|m| m.modified()).ok();
        done.push((used, dir_size(&p), p));
    }
    let mut total: u64 = done.iter().map(|d| d.1).sum::<u64>() + if keep.starts_with(root) { dir_size(keep) } else { 0 };
    done.sort_by_key(|d| d.0);
    for (_, size, p) in done {
        if total <= budget {
            break;
        }
        if std::fs::remove_dir_all(&p).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

fn launch_with(g: &mut St, sel: Selection, force: bool) -> bool {
    let Selection::Local { source, root, symbol, date, window, types, .. } = &sel else {
        g.note = "实时数据不用回放：选 B1 就是常驻引擎在算的那一份".into();
        return false;
    };
    let (hhmm, minutes) = window.clone().unwrap_or_else(|| ("00:00".into(), 30));
    if !valid_hhmm(&hhmm) || minutes == 0 {
        g.note = format!("起始时刻要写成 HH:MM（UTC），时长要 > 0——现在是 {hhmm} / {minutes} 分钟");
        return false;
    }
    if let Some(over) = super::data_picker::past_midnight(&hhmm, minutes) {
        g.note = format!("{hhmm} 起 {minutes} 分钟过了 UTC 24:00 {over} 分钟：回放只读 {date} 这一天的文件，后面那段没有数据——改起始或时长（docs/67）");
        return false;
    }
    let bin = replay_bin();
    if !bin.exists() {
        g.note = format!(
            "找不到 {}——先在主仓 `cargo build --release -p wealthspring-features --example replay_source`",
            bin.display()
        );
        return false;
    }
    stop(g);

    let dir = replay_dir();
    if let Err(e) = std::fs::create_dir_all(dir.join("cache")) {
        g.note = format!("建 {} 失败：{e}", dir.display());
        return false;
    }
    // 旧版每次一个 `run_*` 目录、旧版的那组文件：删掉（迁移；缓存在 cache/ 里）
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.starts_with("run_") {
                let _ = std::fs::remove_dir_all(e.path());
            } else if n.starts_with("feature_chart_") || n.starts_with("feature_matrix_") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let config = std::fs::read(super::feature_matrix::config_path()).unwrap_or_default();
    let (key_text, name) = cache_key(source, symbol, date, &hhmm, minutes, types, &config, &bin_stamp(&bin));
    let run_dir = cache_dir().join(&name);
    let desc = sel.describe();
    let (t0, t1) = window_ns(date, &hhmm, minutes);
    // 命中：算完的同一段，直接读，不起回放程序
    if !force && cache_done(&run_dir) {
        touch_used(&run_dir);
        let log = dir.join("replay.log");
        g.run = Some(Run { child: None, dir: run_dir.clone(), desc: desc.clone(), log, started: Instant::now(), done: Some("✔ 用缓存（不重算）".into()), sel: sel.clone(), engine_line: None });
        g.reading = Some((run_dir.clone(), run_dir.join("tl_trades.bin"), desc));
        super::replay_clock::begin(run_dir.clone(), t0, t1, g.pace);
        watch_meta(run_dir);
        return true;
    }
    // 没算完的（上次停了 / 崩了）与这段的旧结果：删掉重算；顺带按上限清一遍
    let _ = std::fs::remove_dir_all(&run_dir);
    evict(&cache_dir(), cache_budget(), &run_dir);
    if let Err(e) = std::fs::create_dir_all(&run_dir) {
        g.note = format!("建 {} 失败：{e}", run_dir.display());
        return false;
    }
    let _ = std::fs::write(run_dir.join("key.txt"), &key_text);
    let log = dir.join("replay.log");
    let Ok(logf) = std::fs::File::create(&log) else {
        g.note = format!("建 {} 失败", log.display());
        return false;
    };

    let mut cmd = Command::new(&bin);
    // 全速算、只落时间线（docs/51）：倍速、暂停都在播放头那边
    cmd.args([symbol.as_str(), date.as_str(), hhmm.as_str(), &minutes.to_string()])
        .arg(&run_dir)
        .args(["--source", source.as_str(), "--config", "--timeline-only"]);
    // Databento 去掉 mbo = 不喂逐单（L3 特征如实标「不可用」）
    if source == "databento" && !types.iter().any(|t| t == "mbo") {
        cmd.arg("--no-l3");
    }
    let root_env = match source.as_str() {
        "tardis" => "WS_TARDIS_ROOT",
        "databento" => "WS_DATABENTO_ROOT",
        _ => "WS_DATA_DIR",
    };
    // 引擎原有的旁路快照 / 图表流写进它自己的临时目录（结束删掉）——**不能**继承 Cockpit 的环境变量，
    // 那会写进常驻引擎的那两份
    let scratch = run_dir.join("_engine");
    cmd.env(root_env, root)
        .env("WS_FEATURE_MATRIX", scratch.join("feature_matrix.json"))
        .env("WS_FEATURE_CHART", scratch.join("feature_chart.jsonl"))
        // 启用集与窗口跟面板走（数据根换了也读同一份配置）
        .env("WS_FEATURE_CONFIG", super::feature_matrix::config_path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(logf));
    match cmd.spawn() {
        Ok(child) => {
            touch_used(&run_dir);
            g.run = Some(Run { child: Some(child), dir: run_dir.clone(), desc: desc.clone(), log, started: Instant::now(), done: None, sel: sel.clone(), engine_line: None });
            g.reading = Some((run_dir.clone(), run_dir.join("tl_trades.bin"), desc));
            super::replay_clock::begin(run_dir.clone(), t0, t1, g.pace);
            watch_meta(run_dir);
            true
        }
        Err(e) => {
            g.note = format!("启动 {} 失败：{e}", bin.display());
            false
        }
    }
}

/// 选择器给的时段（UTC）→ 纳秒；时间线的抬头读出来之后以它为准。
fn window_ns(date: &str, hhmm: &str, minutes: u32) -> (i64, i64) {
    let t0 = chrono::NaiveDateTime::parse_from_str(&format!("{date} {hhmm}"), "%Y-%m-%d %H:%M")
        .map_or(0, |d| d.and_utc().timestamp_nanos_opt().unwrap_or(0));
    (t0, t0 + i64::from(minutes) * 60_000_000_000)
}

fn valid_hhmm(s: &str) -> bool {
    let p: Vec<&str> = s.split(':').collect();
    p.len() == 2
        && p[0].parse::<u32>().is_ok_and(|h| h < 24)
        && p[1].len() == 2
        && p[1].parse::<u32>().is_ok_and(|m| m < 60)
}

/// 退出时别把回放进程留下（关窗即停）。
pub fn shutdown() {
    if let Ok(mut g) = cell().lock() {
        stop(&mut g);
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn 缓存键_同一段同一配置同一程序才相同() {
        let ty = |v: &[&str]| v.iter().map(|x| (*x).to_string()).collect::<Vec<_>>();
        let (a, na) = super::cache_key("tardis", "BTCUSDT", "2026-06-01", "01:59", 9, &ty(&["trades", "l2"]), b"{}", "1@2");
        let (b, nb) = super::cache_key("tardis", "BTCUSDT", "2026-06-01", "01:59", 9, &ty(&["l2", "trades"]), b"{}", "1@2");
        assert_eq!((a.clone(), na.clone()), (b, nb), "数据类型的顺序不算");
        assert!(na.starts_with("BTCUSDT_2026-06-01_0159_9m_"));
        let (_, nc) = super::cache_key("tardis", "BTCUSDT", "2026-06-01", "01:59", 9, &ty(&["l2", "trades"]), b"{\"mode\":\"all\"}", "1@2");
        let (_, nd) = super::cache_key("tardis", "BTCUSDT", "2026-06-01", "01:59", 9, &ty(&["l2", "trades"]), b"{}", "1@3");
        let (_, ne) = super::cache_key("tardis", "BTCUSDT", "2026-06-01", "01:59", 10, &ty(&["l2", "trades"]), b"{}", "1@2");
        assert!(na != nc && na != nd && na != ne, "配置、程序、时段变了都要重算");
        assert_eq!(super::fnv(b"a"), 0xaf63_dc4c_8601_ec8c, "FNV-1a 固定算法");
    }

    #[test]
    fn 缓存淘汰_没算完的删_超上限从最旧删_正在看的不删() {
        let root = std::env::temp_dir().join(format!("ws_rcache_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mk = |n: &str, done: bool, bytes: usize, age_s: u64| {
            let d = root.join(n);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("tl_meta.json"), format!(r#"{{"done":{done}}}"#)).unwrap();
            std::fs::write(d.join("tl_grid.bin"), vec![0u8; bytes]).unwrap();
            let f = std::fs::File::create(d.join("used")).unwrap();
            f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(age_s)).unwrap();
            d
        };
        let old = mk("old", true, 1000, 300);
        let mid = mk("mid", true, 1000, 200);
        let new = mk("new", true, 1000, 100);
        let partial = mk("partial", false, 1000, 50);
        let keep = mk("keep", false, 1000, 400);
        assert!(super::cache_done(&old) && !super::cache_done(&partial));
        super::evict(&root, 2500, &keep); // 已完成 3 × ~1 KB + 正在看的 ~1 KB：删最旧两个才落到 2500 以下
        assert!(!partial.exists(), "没算完的删掉");
        assert!(keep.exists(), "正在算 / 看的不删");
        assert!(!old.exists() && !mid.exists() && new.exists(), "超上限从最久没用的删起");
        let _ = std::fs::remove_dir_all(&root);
    }

    use super::*;

    #[test]
    fn 时刻校验() {
        assert!(valid_hhmm("13:30"));
        assert!(valid_hhmm("00:05"));
        assert!(!valid_hhmm("24:00"));
        assert!(!valid_hhmm("9:5"));
        assert!(!valid_hhmm("abc"));
        assert_eq!(window_ns("2026-06-01", "00:00", 10), (1_780_272_000_000_000_000, 1_780_272_600_000_000_000));
    }

    #[test]
    fn 外壳标的清洗与存盘换回() {
        assert_eq!(shell_symbol("ESU6").as_deref(), Some("ESU6"));
        assert_eq!(shell_symbol("BRK.B").as_deref(), Some("BRK.B"));
        assert_eq!(shell_symbol("a|b c").as_deref(), Some("abc"));
        assert!(shell_symbol("").is_none());
        set_pending_shell(Shell { symbol: "ESU6".into(), tick: 0.25, min_qty: 1.0 });
        assert!(take_shell().is_some_and(|s| s.symbol == "ESU6"));
        let saved = scrub_saved(r#"{"t":"BinanceLinear:ESU6","u":"BinanceLinear:ETHUSDT"}"#.into());
        assert_eq!(saved, r#"{"t":"BinanceLinear:BTCUSDT","u":"BinanceLinear:ETHUSDT"}"#);
        set_pending_shell(Shell::live());
        let _ = take_shell();
    }

    #[test]
    fn 缺省是实时且读常驻引擎那两份() {
        let v = view();
        assert_eq!(v.pick.source, Some(BSource::Live));
        assert!(!v.is_replay);
        assert!(matrix_override().is_none() && chart_override().is_none());
    }
}

// ── 数据链路（链路徽标用，见 `super::provenance::link`）──────────────────────────

/// 链路状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkState {
    /// B1：常驻引擎在跑。
    Live,
    /// B1：常驻引擎停了（图停在最后一刻）。
    EngineStopped,
    /// 回放中（速度标签）。
    Running(&'static str),
    Paused,
    Done,
    Failed,
    /// 回放已起，但图表流文件还没建出来（数据还在加载）。
    WaitingFile,
}

/// 链路逐跳：`(这一跳是否正常, 名称, 内容)`。
pub type Hop = (bool, &'static str, String);

/// 特征工作区当前的数据链路。
#[derive(Debug, Clone)]
pub struct LinkInfo {
    /// `B1` / `B2` / `B3`。
    pub code: &'static str,
    /// 数据商 / 交易所的名字。
    pub vendor: String,
    pub symbol: String,
    pub state: LinkState,
    pub hops: Vec<Hop>,
    /// 自动检查发现的问题（有就把徽标转成警示 / 错误色）。
    pub problem: Option<(bool, String)>,
}

fn vendor_label(key: &str) -> &'static str {
    match key {
        "tardis" => "Tardis",
        "databento" => "Databento",
        "recorder" => "录制",
        _ => "?",
    }
}

fn file_size(p: &std::path::Path) -> Option<u64> {
    std::fs::metadata(p).ok().map(|m| m.len())
}

fn fmt_bytes(n: u64) -> String {
    if n >= 1 << 20 {
        format!("{:.1} MB", n as f64 / f64::from(1 << 20))
    } else {
        format!("{:.0} KB", n as f64 / 1024.0)
    }
}

fn fmt_ns(ns: u64) -> String {
    chrono::DateTime::from_timestamp_nanos(ns as i64).format("%H:%M:%S").to_string()
}

/// 图这一跳：外壳 + 最近收到的数据；并判「图表流在长、图却没收到」。
fn chart_hop(chart: &std::path::Path, flowing: bool) -> (Hop, Option<(bool, String)>) {
    let shell = shell_cell().lock().map(|g| g.1.clone()).unwrap_or_else(|_| Shell::live());
    let rx = super::feature_feed::last_received(chart);
    let recv = match &rx {
        Some((ts, at)) => format!("最近收到 {}（{} 秒前）", fmt_ns(*ts), at.elapsed().as_secs()),
        None => "还没收到数据".into(),
    };
    let stale = flowing && file_size(chart).is_some_and(|n| n > 4096)
        && rx.as_ref().is_none_or(|(_, at)| at.elapsed().as_secs() > 5);
    let hop = (!stale, "图表", format!("外壳 {} · tick {} · {recv}", shell.symbol, shell.tick));
    (hop, stale.then(|| (true, "图未收到数据：图表流在增长，但图 5 秒以上没收到新数据".to_string())))
}

/// 特征工作区的数据链路。
#[must_use]
pub fn link_info() -> LinkInfo {
    let matrix = super::feature_matrix_readout::snapshot();
    let engine_t = if matrix.as_of > 0 { format!(" · 事件时间 {}", fmt_ns(matrix.as_of)) } else { String::new() };
    let Ok(mut g) = cell().lock() else {
        return LinkInfo { code: "B?", vendor: String::new(), symbol: String::new(), state: LinkState::Failed, hops: vec![], problem: None };
    };
    // ── B1：常驻引擎 ──
    if g.reading.is_none() {
        drop(g);
        let active = super::feature_matrix::engine_state().is_none_or(|s| s.active);
        let chart = super::feature_feed::default_feed_path();
        let (chop, problem) = chart_hop(&chart, active);
        let mut hops = vec![
            (true, "选择", "管线 B · B1 交易所实时 · Binance U 本位永续 · BTCUSDT".to_string()),
            (true, "接入", "ws-l2-feed / ws-trades-feed → ws-signals → 通道② 共享内存环".to_string()),
            (active, "计算", format!("常驻特征引擎 ws-features（{}）{engine_t}", if active { "运行中" } else { "已停止" })),
            (file_size(&chart).is_some(), "输出", format!("{} · {}", chart.display(), file_size(&chart).map_or("不存在".into(), fmt_bytes))),
        ];
        hops.push(chop);
        return LinkInfo {
            code: "B1",
            vendor: "Binance".into(),
            symbol: "BTCUSDT".into(),
            state: if active { LinkState::Live } else { LinkState::EngineStopped },
            hops,
            problem: if active { problem } else { Some((false, "常驻引擎已停止：图停在停止前的最后一刻".into())) },
        };
    }
    // ── B2 / B3：回放 ──
    let (_, chart, _) = g.reading.clone().unwrap_or_default();
    let clock = super::replay_clock::view();
    let pace_label = clock.as_ref().map_or("", |v| v.pace.label());
    let Some(r) = g.run.as_mut() else {
        return LinkInfo { code: "B?", vendor: String::new(), symbol: String::new(), state: LinkState::Done, hops: vec![], problem: None };
    };
    let status = poll_run(r);
    if r.engine_line.is_none() {
        r.engine_line = std::fs::read_to_string(&r.log).ok().and_then(|t| t.lines().next().map(str::to_string));
    }
    let Selection::Local { source, root, symbol, date, window, types, .. } = r.sel.clone() else {
        return LinkInfo { code: "B?", vendor: String::new(), symbol: String::new(), state: LinkState::Failed, hops: vec![], problem: None };
    };
    let failed = r.done.as_deref().is_some_and(|d| d.starts_with('✗'));
    let exists = file_size(&chart).is_some();
    let state = if failed {
        LinkState::Failed
    } else if r.done.is_some() {
        LinkState::Done
    } else if clock.as_ref().is_some_and(|v| !v.playing) {
        LinkState::Paused
    } else if !exists {
        LinkState::WaitingFile
    } else {
        LinkState::Running(pace_label)
    };
    let flowing = matches!(state, LinkState::Running(_));
    let (w_hm, w_min) = window.unwrap_or_else(|| ("00:00".into(), 0));
    let code = if source == "recorder" { "B3" } else { "B2" };
    let adapter = match source.as_str() {
        "tardis" => "wealthspring-vendors::tardis（原始 parquet，不转格式）",
        "databento" => {
            if types.iter().any(|t| t == "mbo") {
                "wealthspring-vendors::databento（MBO 逐单重建，含 L3）"
            } else {
                "wealthspring-vendors::databento（不喂逐单）"
            }
        }
        _ => "wealthspring-vendors::recorder（自录 parquet）",
    };
    let engine = r.engine_line.clone().unwrap_or_default();
    let (chop, mut problem) = chart_hop(&chart, flowing);
    // 外壳与数据标的对不上（meta 行到了以后才会换）
    let shell = shell_cell().lock().map(|g| g.1.symbol.clone()).unwrap_or_default();
    if problem.is_none() && exists && shell != symbol && shell_symbol(&symbol).as_deref() != Some(shell.as_str()) {
        problem = Some((false, format!("图的外壳还是 {shell}，数据是 {symbol}（等回放写出 meta 行后会自动换）")));
    }
    if failed {
        problem = Some((true, r.done.clone().unwrap_or_default()));
    }
    let hops = vec![
        (true, "选择", format!("管线 B · {code} · {} · {symbol} · {date} {w_hm} 起 {w_min} 分钟", vendor_label(&source))),
        (true, "数据", format!("{root}（{} 类：{}）", types.len(), types.join(" · "))),
        (true, "接入", adapter.to_string()),
        (!failed, "计算", format!("特征引擎 replay_source（全速，只落时间线）· {engine}{engine_t}")),
        (true, "播放", clock.as_ref().map_or_else(|| "—".into(), |v| format!("统一播放头 · {pace_label} · {}", v.status()))),
        (exists, "输出", format!("{} · {}", chart.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned()), file_size(&chart).map_or("还没建出来".into(), fmt_bytes))),
        chop,
        (!failed, "状态", status),
    ];
    LinkInfo { code, vendor: vendor_label(&source).into(), symbol, state, hops, problem }
}

/// 点链路徽标：展开数据源选择栏。
pub fn open_picker() {
    if let Ok(mut g) = cell().lock() {
        g.open = true;
    }
}
