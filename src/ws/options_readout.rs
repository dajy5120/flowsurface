//! 期权/0DTE 回测面板只读快照（docs/18 P2）。
//!
//! **独立新增面板**（`Content::OptionsBoard`）——不改任何既有面板/readout。数据源（全只读）：
//! `~/ws-data/live/options_board.json`（`factory.options.run_backtest` 产出：逐策略净 PnL +
//! 摩擦分解 + 探针 go/no-go）。沿用 c4_readout 的旁路 poller 模式（惰性 10s 刷新，pane 只读快照）。

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// 一条策略的回测结果（摩擦分解 + 探针）。
#[derive(Default, Clone)]
pub struct StrategyRow {
    pub code: String,
    pub role: String,
    pub desc: String,
    pub net: f64,
    pub premium: f64,
    pub fees: f64,
    pub spread_cost: f64,
    pub hedge_cost: f64,
    pub settle_pnl: f64,
    pub hedge_mkt_pnl: f64,
    pub n_fills: i64,
    pub gate_ok: bool,
}

#[derive(Default, Clone)]
pub struct OptionsReadout {
    pub stamp: String,
    pub data_source: String, // "合成" / "真实"
    pub rows: Vec<StrategyRow>,
    pub refreshed: String,
    pub present: bool, // 快照文件是否存在
}

static READOUT: OnceLock<Mutex<OptionsReadout>> = OnceLock::new();
static POLLER: OnceLock<()> = OnceLock::new();

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}
fn board_path() -> PathBuf {
    std::env::var("WS_OPTIONS_BOARD")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home().join("ws-data/live/options_board.json"))
}

/// pane 渲染时读快照（惰性起 poller）。
pub fn snapshot() -> OptionsReadout {
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
            let lock = READOUT.get_or_init(|| Mutex::new(OptionsReadout::default()));
            if let Ok(mut g) = lock.lock() {
                *g = snap;
            }
            std::thread::sleep(Duration::from_secs(10));
        });
    });
}

fn poll_once() -> OptionsReadout {
    let mut st = OptionsReadout {
        refreshed: chrono::Local::now().format("%H:%M:%S").to_string(),
        ..Default::default()
    };
    if let Some(parsed) = read_board() {
        st = OptionsReadout {
            refreshed: st.refreshed,
            present: true,
            ..parsed
        };
    }
    st
}

fn read_board() -> Option<OptionsReadout> {
    let txt = std::fs::read_to_string(board_path()).ok()?;
    let v: serde_json::Value = serde_json::from_str(&txt).ok()?;
    Some(parse_board(&v))
}

/// 纯解析（可单测）：options_board.json → OptionsReadout。
fn parse_board(v: &serde_json::Value) -> OptionsReadout {
    let s = |o: &serde_json::Value, k: &str| {
        o.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
    };
    let f = |o: &serde_json::Value, k: &str| o.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0);
    let rows = v
        .get("strategies")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .map(|o| StrategyRow {
                    code: s(o, "code"),
                    role: s(o, "role"),
                    desc: s(o, "desc"),
                    net: f(o, "net"),
                    premium: f(o, "premium"),
                    fees: f(o, "fees"),
                    spread_cost: f(o, "spread_cost"),
                    hedge_cost: f(o, "hedge_cost"),
                    settle_pnl: f(o, "settle_pnl"),
                    hedge_mkt_pnl: f(o, "hedge_mkt_pnl"),
                    n_fills: o.get("n_fills").and_then(|x| x.as_i64()).unwrap_or(0),
                    gate_ok: o.get("gate_ok").and_then(|x| x.as_bool()).unwrap_or(false),
                })
                .collect()
        })
        .unwrap_or_default();
    OptionsReadout {
        stamp: s(v, "stamp"),
        data_source: s(v, "data_source"),
        rows,
        refreshed: String::new(),
        present: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_board_reads_rows() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"stamp":"t","data_source":"合成","strategies":[
                {"code":"OS1","role":"试点","desc":"d","net":2.1,"premium":4.0,
                 "fees":-0.1,"spread_cost":-0.06,"hedge_cost":-0.05,"settle_pnl":-0.6,
                 "hedge_mkt_pnl":-1.0,"n_fills":2,"gate_ok":true}]}"#,
        )
        .unwrap();
        let r = parse_board(&v);
        assert_eq!(r.data_source, "合成");
        assert_eq!(r.rows.len(), 1);
        assert_eq!(r.rows[0].code, "OS1");
        assert!(r.rows[0].gate_ok);
        assert!((r.rows[0].net - 2.1).abs() < 1e-9);
    }

    #[test]
    fn parse_board_empty_strategies() {
        let v: serde_json::Value = serde_json::from_str(r#"{"stamp":"t"}"#).unwrap();
        assert_eq!(parse_board(&v).rows.len(), 0);
    }
}
