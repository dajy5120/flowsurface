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
    pub refreshed: String,
    pub present: bool,
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

pub fn snapshot() -> PredictionReadout {
    ensure_poller();
    READOUT
        .get()
        .and_then(|m| m.lock().ok().map(|g| g.clone()))
        .unwrap_or_default()
}

fn ensure_poller() {
    POLLER.get_or_init(|| {
        std::thread::spawn(|| loop {
            let snap = poll_once();
            let lock = READOUT.get_or_init(|| Mutex::new(PredictionReadout::default()));
            if let Ok(mut g) = lock.lock() {
                *g = snap;
            }
            std::thread::sleep(Duration::from_secs(10));
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
    st
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
        refreshed: String::new(),
        present: true,
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
}
