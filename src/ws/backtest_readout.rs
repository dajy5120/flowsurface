//! 回测结果只读快照（docs/08 F6-P7）。
//!
//! cockpit「回测」工作区的「回测结果」pane 读取回测脚本导出的 `result.json`（收益曲线 / 回撤 /
//! 各维度统计），原生渲染（`backtest_view`）。每次回测存 `<out_dir>/<时间戳>/result.json`；
//! 这里读 `<out_dir>/latest.json` 指针定位最新一次（退路：扫子目录取最新名）。
//!
//! 保存目录与回测脚本约定一致：env `WS_BACKTEST_OUT`，否则 `strategies/backtest_results`。
//! 惰性起 poller（打开过「回测」工作区才轮询），每 3s 重读 → 新回测自动刷新。

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::Deserialize;

#[derive(Deserialize, Default, Clone)]
pub struct Meta {
    #[serde(default)]
    pub strategy: String,
    #[serde(default)]
    pub symbol: String,
    #[serde(default)]
    pub bars: i64,
    #[serde(default)]
    pub run: String,
    #[serde(default)]
    pub finished_at: String,
}

#[derive(Deserialize, Default, Clone)]
pub struct Series {
    #[serde(default)]
    pub t: Vec<i64>,
    #[serde(default)]
    pub v: Vec<f64>,
}

#[derive(Deserialize, Default, Clone)]
pub struct Monthly {
    #[serde(default)]
    pub months: Vec<String>,
    #[serde(default)]
    pub years: Vec<String>,
    /// 年×月 收益矩阵（%），缺失为 null。
    #[serde(default)]
    pub z: Vec<Vec<Option<f64>>>,
}

#[derive(Deserialize, Default, Clone)]
pub struct Yearly {
    #[serde(default)]
    pub years: Vec<String>,
    #[serde(default)]
    pub v: Vec<Option<f64>>,
}

#[derive(Deserialize, Default, Clone)]
pub struct Distribution {
    #[serde(default)]
    pub centers: Vec<f64>,
    #[serde(default)]
    pub counts: Vec<i64>,
}

#[derive(Deserialize, Default, Clone)]
pub struct BacktestResult {
    #[serde(default)]
    pub meta: Meta,
    #[serde(default)]
    pub equity: Series,
    #[serde(default)]
    pub drawdown: Series,
    /// 各维度统计：[[标签, 值], …]。
    #[serde(default)]
    pub stats: Vec<Vec<String>>,
    #[serde(default)]
    pub monthly: Monthly,
    #[serde(default)]
    pub yearly: Yearly,
    #[serde(default)]
    pub rolling_sharpe: Series,
    #[serde(default)]
    pub distribution: Distribution,
    /// 收盘价线（降采样）。
    #[serde(default)]
    pub price: Series,
    /// 成交点：[ts_ms, side(1买/2卖), px]。
    #[serde(default)]
    pub fills: Vec<[f64; 3]>,
    /// 运行期标志（非 JSON 字段）：poller 是否已起 / 是否读到文件。
    #[serde(skip)]
    pub loaded: bool,
    #[serde(skip)]
    pub dir: String,
}

static STATE: OnceLock<Mutex<BacktestResult>> = OnceLock::new();
static POLLER: OnceLock<()> = OnceLock::new();

fn out_dir() -> PathBuf {
    if let Ok(p) = std::env::var("WS_BACKTEST_OUT") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join("dev/WealthSpring/strategies/backtest_results")
}

/// pane 渲染时读快照（惰性起 poller）。
pub fn snapshot() -> BacktestResult {
    ensure_poller();
    STATE
        .get()
        .and_then(|m| m.lock().ok().map(|g| g.clone()))
        .unwrap_or_default()
}

pub fn out_dir_display() -> String {
    out_dir().display().to_string()
}

fn ensure_poller() {
    POLLER.get_or_init(|| {
        std::thread::spawn(|| loop {
            let snap = load_latest();
            let lock = STATE.get_or_init(|| Mutex::new(BacktestResult::default()));
            if let Ok(mut g) = lock.lock() {
                *g = snap;
            }
            std::thread::sleep(Duration::from_secs(3));
        });
    });
}

/// 读最新一次回测的 result.json：优先 latest.json 指针，退路扫子目录取最大名。
fn load_latest() -> BacktestResult {
    let base = out_dir();
    let mut run_dir: Option<PathBuf> = None;

    // 1) latest.json 指针 { "dir": "...", "run": "..." }
    if let Ok(txt) = std::fs::read_to_string(base.join("latest.json"))
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt)
        && let Some(d) = v.get("dir").and_then(|x| x.as_str())
    {
        run_dir = Some(PathBuf::from(d));
    }

    // 2) 退路：扫子目录，取名字最大（时间戳命名 → 字典序=时间序）
    if run_dir.as_ref().map(|d| !d.join("result.json").exists()).unwrap_or(true)
        && let Ok(rd) = std::fs::read_dir(&base)
    {
        let mut dirs: Vec<PathBuf> =
            rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        dirs.sort();
        run_dir = dirs.into_iter().rev().find(|d| d.join("result.json").exists());
    }

    let Some(dir) = run_dir else {
        return BacktestResult::default();
    };
    let Ok(txt) = std::fs::read_to_string(dir.join("result.json")) else {
        return BacktestResult::default();
    };
    match serde_json::from_str::<BacktestResult>(&txt) {
        Ok(mut r) => {
            r.loaded = true;
            r.dir = dir.display().to_string();
            r
        }
        Err(e) => {
            log::error!("回测结果 result.json 解析失败: {e}");
            BacktestResult::default()
        }
    }
}
