//! Tardis 历史回放控制 pane（docs/20 Phase 5）——**独立新增面板**，不改任何既有面板。
//!
//! 已购 Tardis 30 天逐笔成交按真实节奏（可变速）回放进 Cockpit：本 pane 只负责**控制**
//! （选符号/日期/时段/倍速、起停），实际推流由主仓脚本
//! `factory/replay/tardis_cockpit_feed.py` 完成 → Redis `ws:bt:{run}:trades`
//! → 既有 [`super::replay`] 订阅喂图。**不新增 wire 契约、不改图表层**。
//!
//! 不依赖主仓 crate（子模块循环，见 `super`）：以子进程方式调脚本，路径可经
//! `WS_REPO` / `WS_VENV_PY` 覆盖。只读状态见 [`super::tardis_replay_readout`]，渲染见
//! `tardis_replay_view`。

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

/// Tardis 数据根（与 docs/20 §1 一致）；可经 `WS_TARDIS_ROOT` 覆盖。
pub fn tardis_root() -> PathBuf {
    std::env::var("WS_TARDIS_ROOT")
        .unwrap_or_else(|_| {
            "/data/ubuntu/HistoricalData/data/bronze/tardis/binance-futures".to_string()
        })
        .into()
}

fn repo() -> PathBuf {
    std::env::var("WS_REPO")
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/home/dajy".into());
            format!("{home}/dev/WealthSpring")
        })
        .into()
}

fn venv_py() -> String {
    std::env::var("WS_VENV_PY").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/dajy".into());
        format!("{home}/ws-venv/bin/python")
    })
}

/// 回放倍速档（∞ = 尽可能快，用于快速灌满一段行情）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Speed {
    X1,
    X10,
    X60,
    X300,
    Max,
}

impl Speed {
    pub const ALL: [Speed; 5] = [Speed::X1, Speed::X10, Speed::X60, Speed::X300, Speed::Max];
    /// 传给脚本的 `--speed`（0 = 不节流）。
    pub fn arg(self) -> &'static str {
        match self {
            Speed::X1 => "1",
            Speed::X10 => "10",
            Speed::X60 => "60",
            Speed::X300 => "300",
            Speed::Max => "0",
        }
    }
}

impl std::fmt::Display for Speed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Speed::X1 => "×1 实时",
            Speed::X10 => "×10",
            Speed::X60 => "×60",
            Speed::X300 => "×300",
            Speed::Max => "最快",
        })
    }
}

/// 时长档（分钟）。
pub const MINUTES: [u32; 5] = [5, 15, 30, 60, 240];

/// pane 携带的可编辑状态。
#[derive(Clone)]
pub struct TardisReplayState {
    pub symbol: String,
    pub date: String,
    pub start_hm: String,
    pub minutes: u32,
    pub speed: Speed,
    pub hint: String,
}

impl Default for TardisReplayState {
    fn default() -> Self {
        Self::load()
    }
}

impl TardisReplayState {
    /// 默认落在数据集里**实际存在**的首个符号/日期，避免开箱即错。
    pub fn load() -> Self {
        let syms = available_symbols();
        let symbol = syms
            .iter()
            .find(|s| *s == "BTCUSDT")
            .or_else(|| syms.first())
            .cloned()
            .unwrap_or_else(|| "BTCUSDT".into());
        let date = available_dates(&symbol).first().cloned().unwrap_or_default();
        Self {
            symbol,
            date,
            start_hm: "09:00".into(),
            minutes: 30,
            speed: Speed::X60,
            hint: "选符号/日期/时段 → 开始回放；行情进左侧图表（K 线/足迹/CVD）".into(),
        }
    }
}

/// 扫 `trades/{年}/{月}/{日}/{符号}.parquet` 得可用符号（取首个存在的日目录）。
pub fn available_symbols() -> Vec<String> {
    let root = tardis_root().join("trades");
    let mut out = BTreeSet::new();
    // trades/YYYY/MM/DD/SYM.parquet —— 逐层取第一个，够列出符号集。
    for y in read_dir_sorted(&root) {
        for m in read_dir_sorted(&y) {
            for d in read_dir_sorted(&m) {
                if let Ok(rd) = std::fs::read_dir(&d) {
                    for f in rd.flatten() {
                        let p = f.path();
                        if p.extension().is_some_and(|e| e == "parquet")
                            && let Some(stem) = p.file_stem().and_then(|s| s.to_str())
                        {
                            out.insert(stem.to_string());
                        }
                    }
                }
                if !out.is_empty() {
                    return out.into_iter().collect();
                }
            }
        }
    }
    out.into_iter().collect()
}

/// 某符号可用日期（`YYYY-MM-DD`，升序）。
pub fn available_dates(symbol: &str) -> Vec<String> {
    let root = tardis_root().join("trades");
    let mut out = Vec::new();
    for y in read_dir_sorted(&root) {
        for m in read_dir_sorted(&y) {
            for d in read_dir_sorted(&m) {
                if d.join(format!("{symbol}.parquet")).exists() {
                    let seg = |p: &PathBuf| {
                        p.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string()
                    };
                    out.push(format!("{}-{}-{}", seg(&y), seg(&m), seg(&d)));
                }
            }
        }
    }
    out
}

fn read_dir_sorted(p: &PathBuf) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(p)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    v.sort();
    v
}

/// 整点选项（00:00..23:00）。
pub fn hours() -> Vec<String> {
    (0..24).map(|h| format!("{h:02}:00")).collect()
}

/// pane 的交互消息（view 发出 → pane.update 路由到 [`handle`]）。
#[derive(Debug, Clone)]
pub enum TardisReplayMsg {
    SymbolPick(String),
    DatePick(String),
    StartPick(String),
    MinutesPick(u32),
    SpeedPick(Speed),
    Start,
    Stop,
}

/// 当前回放子进程（同一时刻只允许一个，起新的先杀旧的）。
static CHILD: std::sync::Mutex<Option<Child>> = std::sync::Mutex::new(None);

/// 回放子进程是否在跑（顺带回收已退出的僵尸）。
pub fn is_running() -> bool {
    let Ok(mut g) = CHILD.lock() else {
        return false;
    };
    match g.as_mut() {
        Some(c) => match c.try_wait() {
            Ok(Some(_)) => {
                *g = None; // 已退出
                false
            }
            Ok(None) => true,
            Err(_) => false,
        },
        None => false,
    }
}

pub fn handle(st: &mut TardisReplayState, msg: TardisReplayMsg) {
    match msg {
        TardisReplayMsg::SymbolPick(s) => {
            st.symbol = s;
            // 换符号后日期可能不再有效 → 落到该符号的首个可用日。
            let dates = available_dates(&st.symbol);
            if !dates.contains(&st.date) {
                st.date = dates.first().cloned().unwrap_or_default();
            }
        }
        TardisReplayMsg::DatePick(s) => st.date = s,
        TardisReplayMsg::StartPick(s) => st.start_hm = s,
        TardisReplayMsg::MinutesPick(m) => st.minutes = m,
        TardisReplayMsg::SpeedPick(s) => st.speed = s,
        TardisReplayMsg::Start => st.hint = start(st),
        TardisReplayMsg::Stop => st.hint = stop(),
    }
}

fn start(st: &TardisReplayState) -> String {
    if st.date.is_empty() {
        return "✗ 没有可用日期（检查 Tardis 数据根）".into();
    }
    let src = tardis_root()
        .join("trades")
        .join(st.date.replace('-', "/"))
        .join(format!("{}.parquet", st.symbol));
    if !src.exists() {
        return format!("✗ 无此数据: {}", src.display());
    }
    stop(); // 同一时刻只跑一个回放
    let mut cmd = Command::new(venv_py());
    cmd.current_dir(repo())
        .args([
            "-m",
            "factory.replay.tardis_cockpit_feed",
            &st.symbol,
            &st.date,
            "--from",
            &st.start_hm,
            "--minutes",
            &st.minutes.to_string(),
            "--speed",
            st.speed.arg(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Ok(url) = std::env::var("WS_REDIS_URL") {
        cmd.args(["--redis-url", &url]);
    }
    match cmd.spawn() {
        Ok(child) => {
            if let Ok(mut g) = CHILD.lock() {
                *g = Some(child);
            }
            format!(
                "▶ 回放中 {} {} {} +{}min（{}）",
                st.symbol, st.date, st.start_hm, st.minutes, st.speed
            )
        }
        Err(e) => format!("✗ 启动失败: {e}（检查 {} 与主仓路径）", venv_py()),
    }
}

fn stop() -> String {
    let Ok(mut g) = CHILD.lock() else {
        return "✗ 内部锁失败".into();
    };
    match g.take() {
        Some(mut c) => {
            let _ = c.kill();
            let _ = c.wait();
            "■ 已停止回放".into()
        }
        None => "（当前没有回放在跑）".into(),
    }
}
