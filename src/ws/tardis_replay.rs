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
            super::paths::repo_root().to_string_lossy().into_owned()
        })
        .into()
}

fn venv_py() -> String {
    std::env::var("WS_VENV_PY").unwrap_or_else(|_| {
        super::paths::python().to_string_lossy().into_owned()
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

/// 本面板的选择范围：管线 B · B2 购买数据 · Tardis（回放脚本读 Tardis 原始文件）；时间只选日期
/// （起始时刻 / 时长用本面板自己的整点与时长档）。
pub fn pick_opts() -> super::data_picker::PickOpts {
    use super::data_picker::{BSource, PickOpts, Purpose, TimeMode};
    PickOpts {
        purpose: Purpose::Chart,
        sources: Some(vec![BSource::Purchased]),
        vendors: Some(vec!["tardis"]),
        time: TimeMode::Date,
    }
}

/// pane 携带的可编辑状态。
#[derive(Clone)]
pub struct TardisReplayState {
    /// 数据管线 / 市场 / 标的 / 日期（共用组件 [`super::data_picker`]；目录只有主仓一份实现）。
    pub pick: super::data_picker::DataPick,
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
    /// 默认选 Tardis · 加密 · BTCUSDT；日期不选时开始回放用该标的最新有数据的一天。
    pub fn load() -> Self {
        Self {
            pick: {
                use super::data_picker::{BSource, DataPick, DataPickMsg as M, Pipeline};
                let mut p = DataPick::default();
                p.update(M::Pipeline(Pipeline::B));
                p.update(M::Source(BSource::Purchased));
                p.update(M::Vendor("tardis".into()));
                p.update(M::Market("crypto".into()));
                p.update(M::Symbol("BTCUSDT".into()));
                p
            },
            start_hm: "09:00".into(),
            minutes: 30,
            speed: Speed::X60,
            hint: "选标的 / 日期 / 时段 → 开始回放；行情进左侧图表（K 线/足迹/CVD）".into(),
        }
    }
}

/// 整点选项（00:00..23:00）。
pub fn hours() -> Vec<String> {
    (0..24).map(|h| format!("{h:02}:00")).collect()
}

/// pane 的交互消息（view 发出 → pane.update 路由到 [`handle`]）。
#[derive(Debug, Clone)]
pub enum TardisReplayMsg {
    /// 共用数据选择组件的消息。
    Data(super::data_picker::DataPickMsg),
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
        TardisReplayMsg::Data(m) => st.pick.update(m),
        TardisReplayMsg::StartPick(s) => st.start_hm = s,
        TardisReplayMsg::MinutesPick(m) => st.minutes = m,
        TardisReplayMsg::SpeedPick(s) => st.speed = s,
        TardisReplayMsg::Start => st.hint = start(st),
        TardisReplayMsg::Stop => st.hint = stop(),
    }
}

fn start(st: &TardisReplayState) -> String {
    use super::data_picker::Load;
    let Some(symbol) = st.pick.symbol.clone() else {
        return "✗ 先选标的".into();
    };
    // 没选日期：用该标的最新有数据的一天
    let date = match &st.pick.date {
        Some(d) => d.clone(),
        None => match st.pick.local_scan() {
            Some(Load::Ready(sc)) => match sc.dates(&symbol).last() {
                Some(d) => d.clone(),
                None => return format!("✗ {symbol} 没有可用日期（检查 Tardis 根目录）"),
            },
            Some(Load::Loading) => return "还在扫描目录，稍后再点".into(),
            Some(Load::Failed(e)) => return format!("✗ 扫描失败：{e}"),
            None => return "✗ 先选数据来源".into(),
        },
    };
    let (symbol, date) = (&symbol, &date);
    // 组件里选的根目录（缺省即 Tardis 默认根）；回放脚本经 WS_TARDIS_ROOT 读同一个
    let root = PathBuf::from(st.pick.root_or_default());
    let src = root
        .join("trades")
        .join(date.replace('-', "/"))
        .join(format!("{symbol}.parquet"));
    if !src.exists() {
        return format!("✗ 无此数据: {}", src.display());
    }
    stop(); // 同一时刻只跑一个回放
    let mut cmd = Command::new(venv_py());
    cmd.env("WS_TARDIS_ROOT", &root);
    cmd.current_dir(repo())
        .args([
            "-m",
            "factory.replay.tardis_cockpit_feed",
            symbol,
            date,
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
                symbol, date, st.start_hm, st.minutes, st.speed
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
