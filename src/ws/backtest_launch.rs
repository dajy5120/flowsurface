//! 回测结果面板顶上的「发起回测」——共用数据选择组件（[`super::data_picker`]）的第三个宿主。
//!
//! 选 策略文件 + 数据（管线 B · B2 Tardis / Databento，或 B3 本地录制 → 市场 → 标的 → 日期 + 时段）→ ▶ 运行。
//! 跑的是主仓现成的 runner，与 Studio 终端里跑的是同一条命令：
//!
//! | 来源 | runner | 覆盖参数 |
//! |---|---|---|
//! | B2 Tardis / Databento | `run_tardis_backtest.py` | `--source --symbol --date --start --minutes` |
//! | B3 本地录制 | `run_user_strategy.py` | `--symbol --date`（整天，按 `--max-events` 截断；不支持时段） |
//!
//! 所选根目录经 `WS_TARDIS_ROOT` / `WS_DATABENTO_ROOT` / `WS_DATA_DIR` 传入。
//! 选择是对策略 `BACKTEST` 声明的**覆盖**（docs/27 §8）：面板上没选的项（费率等）仍按策略声明。
//!
//! 结果怎么回到 Cockpit 不用这里管：runner 发布 `ws:active_run` → K 线 / ▲▼ / 订单面板 / 进度条自动跟随，
//! 跑完写 result.json → 下面的 tearsheet 自动读到。这里只起进程、显示它的最后一行输出。

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use super::data_picker::{BSource, DataPick, DataPickMsg, PickOpts, Pipeline, Purpose, Selection, TimeMode};

/// 本面板对组件的要求：计算用途（管线 A 置灰）、只本地数据、时间选一段。
#[must_use]
pub fn pick_opts() -> PickOpts {
    PickOpts {
        purpose: Purpose::Compute,
        sources: Some(vec![BSource::Purchased, BSource::Recorded]),
        vendors: None,
        time: TimeMode::Window,
        local_only: true,
        hide_types: true,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchMsg {
    Toggle,
    Data(DataPickMsg),
    Strategy(String),
    BrowseStrategy,
    Run,
    Stop,
}

struct RunState {
    child: Child,
    trader_id: String,
    desc: String,
    started: Instant,
    done: Option<String>,
}

struct St {
    open: bool,
    pick: DataPick,
    strategy: Option<String>,
    run: Option<RunState>,
    note: String,
}

static ST: OnceLock<Mutex<St>> = OnceLock::new();
/// 文件对话框选到的策略（后台线程写，下一条消息 / 下一帧收）。
static PICKED: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn cell() -> &'static Mutex<St> {
    ST.get_or_init(|| {
        let mut pick = DataPick::default();
        for m in [DataPickMsg::Pipeline(Pipeline::B), DataPickMsg::Source(BSource::Purchased)] {
            pick.update(m);
        }
        Mutex::new(St { open: true, pick, strategy: None, run: None, note: String::new() })
    })
}

fn repo() -> PathBuf {
    super::paths::repo_root()
}

/// 主仓 `strategies/` 下可回测的策略（含按主题分的子目录；定义了 `build(` 的 `.py`）。
/// 结果、数据、日志与原件目录不扫。
#[must_use]
pub fn strategies() -> Vec<String> {
    fn walk(dir: &std::path::Path, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for p in rd.flatten().map(|e| e.path()) {
            if p.is_dir() {
                let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if !matches!(name.as_str(), "backtest_results" | "data" | "pm_flow_log" | "mt5" | "__pycache__")
                    && !name.starts_with('.')
                {
                    walk(&p, out);
                }
            } else if p.extension().is_some_and(|x| x == "py")
                && std::fs::read_to_string(&p).is_ok_and(|t| t.contains("def build("))
            {
                out.push(p.display().to_string());
            }
        }
    }
    let mut v = Vec::new();
    walk(&repo().join("strategies"), &mut v);
    v.sort();
    v
}

/// 只读副本。
#[derive(Debug, Clone)]
pub struct View {
    pub open: bool,
    pub pick: DataPick,
    pub strategy: Option<String>,
    pub running: bool,
    pub status: String,
    pub note: String,
}

#[must_use]
pub fn view() -> View {
    let Ok(mut g) = cell().lock() else {
        return View {
            open: false,
            pick: DataPick::default(),
            strategy: None,
            running: false,
            status: String::new(),
            note: String::new(),
        };
    };
    take_picked(&mut g);
    g.pick.poll();
    let status = g.run.as_mut().map(poll_run).unwrap_or_default();
    View {
        open: g.open,
        pick: g.pick.clone(),
        strategy: g.strategy.clone(),
        running: g.run.as_ref().is_some_and(|r| r.done.is_none()),
        status,
        note: g.note.clone(),
    }
}

fn take_picked(g: &mut St) {
    if let Some(p) = PICKED.get_or_init(|| Mutex::new(None)).lock().ok().and_then(|mut x| x.take()) {
        g.strategy = Some(p);
    }
}

/// 运行日志。
#[must_use]
pub fn log_path() -> PathBuf {
    super::paths::data_dir().join("cockpit").join("backtest_launch.log")
}

fn last_line() -> String {
    std::fs::read_to_string(log_path())
        .ok()
        .and_then(|t| t.lines().rev().find(|l| !l.trim().is_empty()).map(str::to_string))
        .unwrap_or_default()
}

fn poll_run(r: &mut RunState) -> String {
    if r.done.is_none()
        && let Ok(Some(st)) = r.child.try_wait()
    {
        r.done = Some(if st.success() {
            format!("✔ 回测跑完（用时 {}），结果见下方", fmt_s(r.started.elapsed().as_secs()))
        } else {
            format!("✗ 回测失败：{}", last_line())
        });
    }
    match &r.done {
        Some(d) => format!("{d} · {} · {}", r.trader_id, r.desc),
        None => format!("▶ 运行中 {} · {} · {}\n{}", fmt_s(r.started.elapsed().as_secs()), r.trader_id, r.desc, last_line()),
    }
}

fn fmt_s(s: u64) -> String {
    format!("{}:{:02}", s / 60, s % 60)
}

pub fn handle(m: LaunchMsg) {
    let Ok(mut g) = cell().lock() else { return };
    g.note.clear();
    take_picked(&mut g);
    match m {
        LaunchMsg::Toggle => g.open = !g.open,
        LaunchMsg::Data(d) => g.pick.update(d),
        LaunchMsg::Strategy(s) => g.strategy = Some(s),
        LaunchMsg::BrowseStrategy => {
            let start = repo().join("strategies");
            super::spawn_named("ws-backtestlaun", move || {
                let out = Command::new("zenity")
                    .args(["--file-selection", "--title=选策略文件", "--file-filter=*.py"])
                    .arg(format!("--filename={}/", start.display()))
                    .output();
                if let Ok(o) = out
                    && o.status.success()
                {
                    let p = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    if !p.is_empty()
                        && let Ok(mut x) = PICKED.get_or_init(|| Mutex::new(None)).lock()
                    {
                        *x = Some(p);
                    }
                }
            });
            g.note = "在弹出的对话框里选策略文件".into();
        }
        LaunchMsg::Stop => {
            if let Some(r) = g.run.as_mut()
                && r.done.is_none()
            {
                let _ = r.child.kill();
                let _ = r.child.wait();
                r.done = Some("■ 已停止".into());
            }
        }
        LaunchMsg::Run => run(&mut g),
    }
}

fn run(g: &mut St) {
    if g.run.as_ref().is_some_and(|r| r.done.is_none()) {
        g.note = "上一个回测还在跑：先停止它".into();
        return;
    }
    let Some(strat) = g.strategy.clone() else {
        g.note = "先选策略文件".into();
        return;
    };
    let Some(sel) = g.pick.selection(&pick_opts()) else {
        g.note = "数据还没选完：管线 B → 来源 → 标的 → 日期".into();
        return;
    };
    let Selection::Local { source, root, symbol, date, window, .. } = &sel else {
        return;
    };
    let (hhmm, minutes) = window.clone().unwrap_or_else(|| ("00:00".into(), 60));
    let recorder = source == "recorder";
    let script = repo().join("crates/wealthspring-py/scripts").join(if recorder {
        "run_user_strategy.py"
    } else {
        "run_tardis_backtest.py"
    });
    // trader_id：必须含连字符、纯 ASCII（Nautilus 在 Rust 侧否则 panic）
    let trader_id = format!("CKP-{}", chrono::Local::now().format("%H%M%S"));
    let log = log_path();
    let _ = std::fs::create_dir_all(log.parent().unwrap_or(std::path::Path::new(".")));
    let Ok(f) = std::fs::File::create(&log) else {
        g.note = format!("建 {} 失败", log.display());
        return;
    };
    let Ok(f2) = f.try_clone() else { return };

    let mut cmd = Command::new(super::paths::python());
    cmd.current_dir(repo())
        .arg(&script)
        .arg(&strat)
        .arg(&trader_id)
        .args(["--symbol", symbol.as_str(), "--date", date.as_str()]);
    if !recorder {
        cmd.args(["--source", source.as_str(), "--start", hhmm.as_str(), "--minutes", &minutes.to_string()]);
    }
    if let Ok(url) = std::env::var("WS_REDIS_URL") {
        cmd.args(["--redis-url", &url]);
    }
    let root_env = match source.as_str() {
        "tardis" => "WS_TARDIS_ROOT",
        "databento" => "WS_DATABENTO_ROOT",
        _ => "WS_DATA_DIR",
    };
    cmd.env(root_env, root)
        .env("PYTHONUNBUFFERED", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::from(f))
        .stderr(Stdio::from(f2));
    let name = std::path::Path::new(&strat)
        .file_name()
        .map_or_else(|| strat.clone(), |n| n.to_string_lossy().into_owned());
    match cmd.spawn() {
        Ok(child) => {
            g.run = Some(RunState {
                child,
                trader_id,
                desc: format!(
                    "{name} · {}{}",
                    sel.describe(),
                    if recorder { "（录制数据按整天跑，不支持时段）" } else { "" }
                ),
                started: Instant::now(),
                done: None,
            });
            g.open = false;
        }
        Err(e) => g.note = format!("启动失败：{e}"),
    }
}

/// 关窗时停掉（关窗即停）。
pub fn shutdown() {
    handle(LaunchMsg::Stop);
}

/// 点链路徽标：展开「发起回测」。
pub fn open_picker() {
    if let Ok(mut g) = cell().lock() {
        g.open = true;
    }
}
