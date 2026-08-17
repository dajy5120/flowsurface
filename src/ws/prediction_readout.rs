//! 预测市场（Polymarket）面板只读快照（docs/19）。
//!
//! **独立新增面板**（`Content::PredictionBoard`）——不改任何既有面板/readout。数据源（全只读）：
//! `~/ws-data/live/prediction_board.json`（`factory.prediction.run` 产出：市场列表 + 基线关注档 +
//! 可选 AI 决策支持）。沿用 c4_readout/options_readout 旁路 poller 模式（惰性 10s 刷新）。

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// AI 决策支持（可选·仅关注档且启用 AI 时有）。
#[derive(Default, Clone)]
pub struct AiView {
    pub ai_prob: f64,
    pub market_prob: Option<f64>,
    pub edge: Option<f64>,
    pub confidence: String,
    pub rationale: String,
}

/// 校准曲线一个分箱（AI 预测区间 vs 实际发生率）。
#[derive(Default, Clone)]
pub struct CalibBin {
    pub lo: f64,
    pub hi: f64,
    pub n: i64,
    pub mean_pred: f64,
    pub actual: f64,
}

/// AI 校准追踪摘要（docs/19 §5）：AI Brier vs 市场 Brier + 校准曲线。
#[derive(Default, Clone)]
pub struct CalibrationView {
    pub n_total: i64,
    pub n_resolved: i64,
    pub ai_brier: Option<f64>,
    pub market_brier: Option<f64>,
    pub ai_beats_market: bool,
    pub bins: Vec<CalibBin>,
    pub stamp: String,
}

/// 一个预测市场行。
#[derive(Default, Clone)]
pub struct MarketRow {
    pub question: String,
    pub yes_prob: Option<f64>,
    pub volume: f64,
    pub liquidity: f64,
    pub watch: bool,
    pub tags: Vec<String>,
    pub ai: Option<AiView>,
}

#[derive(Default, Clone)]
pub struct PredictionReadout {
    pub stamp: String,
    pub source: String,
    pub ai_enabled: bool,
    pub ai_analyzed: i64,
    pub rows: Vec<MarketRow>,
    pub calib: Option<CalibrationView>,
    pub refreshed: String,
    pub present: bool,
    /// 夜跑 service 状态（在跑/时长/上次结果）。后台 poller 每 10s 刷，或被面板
    /// 「刷新」按钮唤醒——**不能在 view 里查**，那是每帧一次 systemctl 子进程
    /// （docs/20 §19.2 的每帧开销教训）。
    ///
    /// 夜跑是 `Type=oneshot`，`restarts` 对它恒 0 无意义，看 `last_result` 才对。
    pub svc: super::svcctl::UnitState,
    pub timer_enabled: bool,
    pub timer_next: String,
}

static READOUT: OnceLock<Mutex<PredictionReadout>> = OnceLock::new();
static POLLER: OnceLock<()> = OnceLock::new();

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}
fn board_path() -> PathBuf {
    std::env::var("WS_PREDICTION_BOARD")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home().join("ws-data/live/prediction_board.json"))
}
fn calib_path() -> PathBuf {
    std::env::var("WS_PREDICTION_CALIB")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home().join("ws-data/live/prediction_calibration.json"))
}

pub fn snapshot() -> PredictionReadout {
    ensure_poller();
    READOUT
        .get()
        .and_then(|m| m.lock().ok().map(|g| g.clone()))
        .unwrap_or_default()
}

/// 预测市场夜跑的 systemd 单元名（oneshot service + 每日 timer）。
pub const NIGHTLY_SVC: &str = "ws-prediction-nightly.service";
pub const NIGHTLY_TIMER: &str = "ws-prediction-nightly.timer";

static WAKER: super::svcctl::Waker = super::svcctl::Waker::new();

/// 面板「刷新」按钮：叫醒 poller 立刻刷一轮（不阻塞 UI）。
pub fn request_refresh() {
    WAKER.request();
}

/// 查 service/timer 状态。**只在后台 poller 里调**（起子进程，不可进渲染线程）。
fn poll_nightly_units(st: &mut PredictionReadout) {
    st.svc = super::svcctl::query(NIGHTLY_SVC);
    st.timer_enabled = super::svcctl::timer_active(NIGHTLY_TIMER);
    st.timer_next = super::svcctl::next_elapse(NIGHTLY_TIMER);
}

/// 手动触发一次夜跑。
///
/// **必须 `--no-block`**：该 service 是 `Type=oneshot` 且 `TimeoutStartSec=30min`，
/// 不加的话 `systemctl start` 会一直等到跑完才返回，直接冻死 UI 线程
/// （同 factory nightly 踩过的坑，见 [`super::factory_readout::nightly_start`]）。
pub fn nightly_start() -> String {
    match std::process::Command::new("systemctl")
        .args(["--user", "start", "--no-block", NIGHTLY_SVC])
        .status()
    {
        Ok(s) if s.success() => {
            WAKER.request();
            "▶ 已触发预测夜跑（后台运行）".into()
        }
        Ok(s) => format!("✗ 触发失败，退出码 {:?}", s.code()),
        Err(e) => format!("✗ 触发失败：{e}"),
    }
}

/// 中止正在运行的夜跑。
pub fn nightly_stop() -> String {
    match std::process::Command::new("systemctl")
        .args(["--user", "stop", NIGHTLY_SVC])
        .status()
    {
        Ok(s) if s.success() => {
            WAKER.request();
            "■ 已中止预测夜跑".into()
        }
        Ok(s) => format!("✗ 中止失败，退出码 {:?}", s.code()),
        Err(e) => format!("✗ 中止失败：{e}"),
    }
}

/// 开/关每日定时器（只影响自动触发，不影响手动运行）。
pub fn nightly_toggle_timer(enable: bool) -> String {
    let act = if enable { "start" } else { "stop" };
    match std::process::Command::new("systemctl")
        .args(["--user", act, NIGHTLY_TIMER])
        .status()
    {
        Ok(s) if s.success() => {
            WAKER.request();
            if enable { "✔ 每日定时已开".into() } else { "✔ 每日定时已关（手动仍可运行）".into() }
        }
        Ok(s) => format!("✗ 操作失败，退出码 {:?}", s.code()),
        Err(e) => format!("✗ 操作失败：{e}"),
    }
}

fn ensure_poller() {
    POLLER.get_or_init(|| {
        std::thread::spawn(|| loop {
            let mut snap = poll_once();
            poll_nightly_units(&mut snap);
            let lock = READOUT.get_or_init(|| Mutex::new(PredictionReadout::default()));
            if let Ok(mut g) = lock.lock() {
                *g = snap;
            }
            WAKER.wait(Duration::from_secs(10));
        });
    });
}

fn poll_once() -> PredictionReadout {
    let mut st = PredictionReadout {
        refreshed: chrono::Local::now().format("%H:%M:%S").to_string(),
        ..Default::default()
    };
    if let Some(parsed) = read_board() {
        st = PredictionReadout {
            refreshed: st.refreshed,
            present: true,
            ..parsed
        };
    }
    st.calib = read_calib();  // 校准快照独立文件（可缺）
    st
}

fn read_calib() -> Option<CalibrationView> {
    let txt = std::fs::read_to_string(calib_path()).ok()?;
    let v: serde_json::Value = serde_json::from_str(&txt).ok()?;
    Some(parse_calibration(&v))
}

/// 纯解析（可单测）：prediction_calibration.json → CalibrationView。
fn parse_calibration(v: &serde_json::Value) -> CalibrationView {
    let bins = v
        .get("bins")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .map(|b| CalibBin {
                    lo: b.get("lo").and_then(|x| x.as_f64()).unwrap_or(0.0),
                    hi: b.get("hi").and_then(|x| x.as_f64()).unwrap_or(0.0),
                    n: b.get("n").and_then(|x| x.as_i64()).unwrap_or(0),
                    mean_pred: b.get("mean_pred").and_then(|x| x.as_f64()).unwrap_or(0.0),
                    actual: b.get("actual").and_then(|x| x.as_f64()).unwrap_or(0.0),
                })
                .collect()
        })
        .unwrap_or_default();
    CalibrationView {
        n_total: v.get("n_total").and_then(|x| x.as_i64()).unwrap_or(0),
        n_resolved: v.get("n_resolved").and_then(|x| x.as_i64()).unwrap_or(0),
        ai_brier: v.get("ai_brier").and_then(|x| x.as_f64()),
        market_brier: v.get("market_brier").and_then(|x| x.as_f64()),
        ai_beats_market: v
            .get("ai_beats_market")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
        bins,
        stamp: v.get("stamp").and_then(|x| x.as_str()).unwrap_or("").to_string(),
    }
}

fn read_board() -> Option<PredictionReadout> {
    let txt = std::fs::read_to_string(board_path()).ok()?;
    let v: serde_json::Value = serde_json::from_str(&txt).ok()?;
    Some(parse_board(&v))
}

/// 纯解析（可单测）：prediction_board.json → PredictionReadout。
fn parse_board(v: &serde_json::Value) -> PredictionReadout {
    let s = |o: &serde_json::Value, k: &str| {
        o.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
    };
    let optf = |o: &serde_json::Value, k: &str| o.get(k).and_then(|x| x.as_f64());
    let f = |o: &serde_json::Value, k: &str| optf(o, k).unwrap_or(0.0);
    let rows = v
        .get("markets")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .map(|o| MarketRow {
                    question: s(o, "question"),
                    yes_prob: optf(o, "yes_prob"),
                    volume: f(o, "volume"),
                    liquidity: f(o, "liquidity"),
                    watch: o.get("watch").and_then(|x| x.as_bool()).unwrap_or(false),
                    tags: o
                        .get("tags")
                        .and_then(|x| x.as_array())
                        .map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect())
                        .unwrap_or_default(),
                    ai: o.get("ai").map(|a| AiView {
                        ai_prob: f(a, "ai_prob"),
                        market_prob: optf(a, "market_prob"),
                        edge: optf(a, "edge"),
                        confidence: s(a, "confidence"),
                        rationale: s(a, "rationale"),
                    }),
                })
                .collect()
        })
        .unwrap_or_default();
    PredictionReadout {
        stamp: s(v, "stamp"),
        source: s(v, "source"),
        ai_enabled: v.get("ai_enabled").and_then(|x| x.as_bool()).unwrap_or(false),
        ai_analyzed: v.get("ai_analyzed").and_then(|x| x.as_i64()).unwrap_or(0),
        rows,
        calib: None,
        refreshed: String::new(),
        present: true,
        // 单元状态不来自 board JSON，由 poller 的 poll_nightly_units 覆盖填入。
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_board_reads_market_and_ai() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"stamp":"t","source":"Polymarket","ai_enabled":true,"ai_analyzed":1,
                "markets":[{"question":"Will X?","yes_prob":0.62,"volume":50000,
                 "liquidity":8000,"watch":true,"tags":["临近结算(<3d)"],
                 "ai":{"ai_prob":0.55,"market_prob":0.62,"edge":-0.07,"confidence":"low","rationale":"r"}}]}"#,
        )
        .unwrap();
        let r = parse_board(&v);
        assert_eq!(r.source, "Polymarket");
        assert!(r.ai_enabled);
        assert_eq!(r.rows.len(), 1);
        assert!(r.rows[0].watch);
        assert!((r.rows[0].yes_prob.unwrap() - 0.62).abs() < 1e-9);
        let ai = r.rows[0].ai.as_ref().unwrap();
        assert!((ai.edge.unwrap() - (-0.07)).abs() < 1e-9);
    }

    #[test]
    fn parse_board_no_markets() {
        let v: serde_json::Value = serde_json::from_str(r#"{"stamp":"t"}"#).unwrap();
        assert_eq!(parse_board(&v).rows.len(), 0);
    }

    #[test]
    fn parse_calibration_reads_summary_and_bins() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"n_total":8,"n_resolved":5,"ai_brier":0.12,"market_brier":0.18,
                "ai_beats_market":true,"stamp":"t",
                "bins":[{"lo":0.8,"hi":0.9,"n":3,"mean_pred":0.85,"actual":0.667}]}"#,
        )
        .unwrap();
        let c = parse_calibration(&v);
        assert_eq!(c.n_resolved, 5);
        assert!(c.ai_beats_market);
        assert!((c.ai_brier.unwrap() - 0.12).abs() < 1e-9);
        assert_eq!(c.bins.len(), 1);
        assert!((c.bins[0].actual - 0.667).abs() < 1e-9);
    }

    #[test]
    fn parse_calibration_null_briers() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{"n_total":8,"n_resolved":0,"ai_brier":null,"market_brier":null}"#)
                .unwrap();
        let c = parse_calibration(&v);
        assert_eq!(c.n_resolved, 0);
        assert!(c.ai_brier.is_none());
    }
}
