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
}

/// 一次回放。
struct Run {
    child: Child,
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
        Mutex::new(St { pick, pace: Pace::X10, open: false, run: None, reading: None, note: String::new() })
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
        note: g.note.clone(),
    }
}

/// 回放状态一行：计算进程（结束时从日志里取结论）+ 播放头。
fn poll_run(r: &mut Run) -> String {
    if r.done.is_none()
        && let Ok(Some(st)) = r.child.try_wait()
    {
        let log = std::fs::read_to_string(&r.log).unwrap_or_default();
        r.done = Some(if st.success() {
            let n = log.lines().find_map(|l| l.strip_prefix("重放 ")).unwrap_or("").trim();
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
fn shell_symbol(s: &str) -> Option<String> {
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
        let _ = r.child.kill();
        let _ = r.child.wait();
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

/// 主循环每帧调：处理 `WS_FEATURE_REPLAY`（只第一次有事）。
pub fn tick() {
    if let Ok(mut g) = cell().lock() {
        auto_start(&mut g);
    }
}

fn launch(g: &mut St, sel: Selection) -> bool {
    let Selection::Local { source, root, symbol, date, window, types, .. } = &sel else {
        g.note = "实时数据不用回放：选 B1 就是常驻引擎在算的那一份".into();
        return false;
    };
    let (hhmm, minutes) = window.clone().unwrap_or_else(|| ("00:00".into(), 30));
    if !valid_hhmm(&hhmm) || minutes == 0 {
        g.note = format!("起始时刻要写成 HH:MM（UTC），时长要 > 0——现在是 {hhmm} / {minutes} 分钟");
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
    if let Err(e) = std::fs::create_dir_all(&dir) {
        g.note = format!("建 {} 失败：{e}", dir.display());
        return false;
    }
    // 每次回放一个新目录（时间线订阅按目录建）；旧的回放目录与旧版的那组文件删掉
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
    let tag = chrono::Local::now().format("%H%M%S").to_string();
    let run_dir = dir.join(format!("run_{tag}"));
    if let Err(e) = std::fs::create_dir_all(&run_dir) {
        g.note = format!("建 {} 失败：{e}", run_dir.display());
        return false;
    }
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
            let desc = sel.describe();
            g.run = Some(Run { child, desc: desc.clone(), log, started: Instant::now(), done: None, sel: sel.clone(), engine_line: None });
            g.reading = Some((run_dir.clone(), run_dir.join("tl_trades.bin"), desc));
            let (t0, t1) = window_ns(date, &hhmm, minutes);
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
