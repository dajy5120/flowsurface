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
    /// **本次运行的 trader id**（如 BT-619605），与 `run` 那个结果目录时间戳不是一回事。
    ///
    /// 面板靠它判断「这份结果属不属于当前正在跑的这一次」。没有它的话，新回测跑完之前
    /// 面板会一直把**上一次**的结论当本次显示——数字看着正常，其实完全不相干
    /// （docs/27 §12）。旧结果没有这个字段 → 空串 → 一律视为「非本次」。
    #[serde(default)]
    pub run_id: String,
    #[serde(default)]
    pub finished_at: String,
}

/// 数据溯源（docs/27 S5）：这次回测用什么成色的数据、什么撮合假设跑出来的。
///
/// 没有它，一份 result.json 看不出是 A 级购买数据 + 队列位置撮合，还是覆盖不全的自录数据
/// + 一触价即成交——两者的数字长得一模一样。旧结果没有这一段，故整体 `Option`。
#[derive(Deserialize, Default, Clone)]
pub struct Provenance {
    /// `tardis`（已购高质量数据）/ `recorder`（自录，覆盖不完整）。
    #[serde(default)]
    pub source: String,
    /// 窗口体检等级 A/B/C；自录数据无体检故为空。
    #[serde(default)]
    pub grade: Option<String>,
    #[serde(default)]
    pub quality_flags: Vec<String>,
    #[serde(default)]
    pub window: ProvWindow,
    #[serde(default)]
    pub execution: ProvExec,
    #[serde(default)]
    pub strategy: ProvStrategy,
    #[serde(default)]
    pub versions: std::collections::BTreeMap<String, String>,
}

#[derive(Deserialize, Default, Clone)]
pub struct ProvWindow {
    #[serde(default)]
    pub symbol: String,
    #[serde(default)]
    pub date: String,
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub minutes: Option<i64>,
}

#[derive(Deserialize, Default, Clone)]
pub struct ProvExec {
    #[serde(default)]
    pub queue_position: bool,
    #[serde(default)]
    pub liquidity_consumption: bool,
    #[serde(default)]
    pub latency_ms: f64,
    #[serde(default)]
    pub maker_bp: Option<f64>,
    #[serde(default)]
    pub taker_bp: Option<f64>,
}

#[derive(Deserialize, Default, Clone)]
pub struct ProvStrategy {
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub sha256: String,
}

impl Provenance {
    /// 一行摘要：数据源 + 等级 + 窗口。
    pub fn summary(&self) -> String {
        let src = match self.source.as_str() {
            "tardis" => "自有数据(Tardis)",
            "recorder" => "录制数据(自录)",
            "" => "来源未标注",
            other => other,
        };
        let grade = match self.grade.as_deref() {
            Some(g) => format!(" · 体检 {g}"),
            None => String::new(),
        };
        let w = &self.window;
        let win = if w.date.is_empty() {
            String::new()
        } else {
            let span = match (&w.start, w.minutes) {
                (Some(s), Some(m)) => format!(" {s} +{m}min"),
                _ => String::new(),
            };
            format!(" · {} {}{span}", w.symbol, w.date)
        };
        format!("{src}{grade}{win}")
    }

    /// 撮合假设摘要——**结果能不能当真，一半看这一行**。
    pub fn execution_summary(&self) -> String {
        let q = if self.execution.queue_position {
            "队列位置✓"
        } else {
            "队列位置✗(一触价即成交，贴盘口报价偏乐观)"
        };
        let fees = match (self.execution.maker_bp, self.execution.taker_bp) {
            (Some(m), Some(t)) => format!(" · 费率 maker {m}bp/taker {t}bp"),
            _ => " · 费率用 instrument 缺省".to_string(),
        };
        format!("{q}{fees} · 延迟 {}ms", self.execution.latency_ms)
    }

    /// 结果可信度的粗档：用于着色。C 或无体检 → 低。
    pub fn is_low_confidence(&self) -> bool {
        self.source != "tardis"
            || matches!(self.grade.as_deref(), Some("C") | None)
            || !self.execution.queue_position
    }
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
pub struct StatsSection {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub rows: Vec<Vec<String>>,
}

#[derive(Deserialize, Default, Clone)]
pub struct BacktestResult {
    #[serde(default)]
    pub meta: Meta,
    #[serde(default)]
    pub equity: Series,
    #[serde(default)]
    pub drawdown: Series,
    /// 各维度统计（旧扁平表，保留兼容）：[[标签, 值], …]。
    #[serde(default)]
    pub stats: Vec<Vec<String>>,
    /// Run Information（与报告同字段顺序）。
    #[serde(default)]
    pub run_info: Vec<Vec<String>>,
    /// Account Summary（起始/结束余额）。
    #[serde(default)]
    pub account: Vec<Vec<String>>,
    /// Performance Statistics 分节（PnL / Returns / General）。
    #[serde(default)]
    pub stats_sections: Vec<StatsSection>,
    #[serde(default)]
    pub monthly: Monthly,
    #[serde(default)]
    pub yearly: Yearly,
    #[serde(default)]
    pub rolling_sharpe: Series,
    #[serde(default)]
    pub distribution: Distribution,
    /// 数据溯源（docs/27 S5）。旧结果没有这一段 → None，面板显示「来源未标注」。
    #[serde(default)]
    pub data_provenance: Option<Provenance>,
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
    load_latest_from(&out_dir())
}

/// 同上，但 base 由调用方给——把读环境变量和找结果分开，后者才好测（S5 改了退路扫描，
/// 那段逻辑不测就只能靠「跑起来看看」）。
fn load_latest_from(base: &std::path::Path) -> BacktestResult {
    let base = base.to_path_buf();
    let mut run_dir: Option<PathBuf> = None;

    // 1) latest.json 指针 { "dir": "...", "run": "..." }
    if let Ok(txt) = std::fs::read_to_string(base.join("latest.json"))
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt)
        && let Some(d) = v.get("dir").and_then(|x| x.as_str())
    {
        run_dir = Some(PathBuf::from(d));
    }

    // 2) 退路：扫子目录，取名字最大（时间戳命名 → 字典序=时间序）。
    //    结果按数据源分目录后（`backtest_results/{tardis,recorder}/`），base 的直接子目录
    //    不再含 result.json，**必须再下探一层**，否则指针缺失时这条退路整个失效。
    if run_dir.as_ref().map(|d| !d.join("result.json").exists()).unwrap_or(true) {
        let mut dirs: Vec<PathBuf> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&base) {
            for p in rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
                if p.join("result.json").exists() {
                    dirs.push(p);
                } else if let Ok(sub) = std::fs::read_dir(&p) {
                    dirs.extend(
                        sub.flatten()
                            .map(|e| e.path())
                            .filter(|q| q.join("result.json").exists()),
                    );
                }
            }
        }
        // 按目录名（时间戳）排序，跨数据源取最新那次。
        dirs.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        run_dir = dirs.pop();
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

#[cfg(test)]
mod discover_tests {
    use super::*;

    /// 造一个结果目录：`<base>/<sub>/<run>/result.json`（sub 为空则直接挂 base 下）。
    fn mk(base: &std::path::Path, sub: &str, run: &str, source: &str) {
        let d = if sub.is_empty() { base.join(run) } else { base.join(sub).join(run) };
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("result.json"),
            format!(r#"{{"meta":{{"run":"{run}"}},"data_provenance":{{"source":"{source}"}}}}"#),
        )
        .unwrap();
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ws-bt-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn 退路扫描能下探一层子目录() {
        // S5 把结果按数据源分了目录，base 的直接子目录不再含 result.json。
        // 不下探一层的话，指针一旦缺失，这条退路就整个失效——而它正是指针缺失时的兜底。
        let base = tmp("nested");
        mk(&base, "tardis", "20260101-000000", "tardis");
        let r = load_latest_from(&base);
        assert!(r.loaded);
        assert_eq!(r.data_provenance.unwrap().source, "tardis");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn 退路跨数据源取最新那次() {
        // 两个来源各有结果时，按时间戳取最新，而不是按目录名的字典序撞运气。
        let base = tmp("newest");
        mk(&base, "recorder", "20260101-000000", "recorder");
        mk(&base, "tardis", "20260601-120000", "tardis");
        let r = load_latest_from(&base);
        assert_eq!(r.data_provenance.unwrap().source, "tardis");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn 旧的平铺布局仍然认() {
        // S5 之前的结果直接挂在 base 下，不能因为改了布局就读不出历史结果。
        let base = tmp("flat");
        mk(&base, "", "20250101-000000", "");
        assert!(load_latest_from(&base).loaded);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn 指针优先于扫描() {
        let base = tmp("pointer");
        mk(&base, "recorder", "20260101-000000", "recorder");
        mk(&base, "tardis", "20260601-120000", "tardis");
        let target = base.join("recorder").join("20260101-000000");
        std::fs::write(
            base.join("latest.json"),
            format!(r#"{{"run":"x","dir":"{}"}}"#, target.display()),
        )
        .unwrap();
        // 指针指着较旧的那次 → 就该读那次（用户/脚本显式指定过）。
        assert_eq!(load_latest_from(&base).data_provenance.unwrap().source, "recorder");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn 空目录不炸() {
        let base = tmp("empty");
        assert!(!load_latest_from(&base).loaded);
        let _ = std::fs::remove_dir_all(&base);
    }
}

#[cfg(test)]
mod prov_tests {
    use super::*;

    fn parse(json: &str) -> BacktestResult {
        serde_json::from_str(json).expect("解析失败")
    }

    #[test]
    fn 旧结果没有溯源段也能解析() {
        // 落地 S5 之前跑出来的 result.json 里没有 data_provenance。解析不能因此失败——
        // 否则一次格式演进会让历史结果全部读不出来。
        let r = parse(r#"{"meta":{"strategy":"x"}}"#);
        assert!(r.data_provenance.is_none());
    }

    #[test]
    fn 自有数据体检甲级加队列位置算高可信() {
        let r = parse(
            r#"{"data_provenance":{"source":"tardis","grade":"A",
                "execution":{"queue_position":true,"liquidity_consumption":true,
                             "latency_ms":10.0,"maker_bp":2.0,"taker_bp":4.5}}}"#,
        );
        assert!(!r.data_provenance.unwrap().is_low_confidence());
    }

    #[test]
    fn 体检丙级一票降级() {
        let r = parse(
            r#"{"data_provenance":{"source":"tardis","grade":"C",
                "execution":{"queue_position":true}}}"#,
        );
        assert!(r.data_provenance.unwrap().is_low_confidence());
    }

    #[test]
    fn 关了队列位置一票降级() {
        // 数据再好，一触价即成交的撮合也撑不起「可信」——贴盘口报价的结果会系统性偏乐观
        // （实测高估 83%，docs/27 §5.2）。
        let r = parse(
            r#"{"data_provenance":{"source":"tardis","grade":"A",
                "execution":{"queue_position":false}}}"#,
        );
        assert!(r.data_provenance.unwrap().is_low_confidence());
    }

    #[test]
    fn 录制数据一律降级() {
        let r = parse(r#"{"data_provenance":{"source":"recorder","grade":null}}"#);
        assert!(r.data_provenance.unwrap().is_low_confidence());
    }

    #[test]
    fn 摘要含数据源与窗口() {
        let r = parse(
            r#"{"data_provenance":{"source":"tardis","grade":"B",
                "window":{"symbol":"BTCUSDT","date":"2026-06-02","start":"04:00","minutes":60}}}"#,
        );
        let p = r.data_provenance.unwrap();
        let s = p.summary();
        for want in ["自有数据", "体检 B", "BTCUSDT", "2026-06-02", "04:00", "+60min"] {
            assert!(s.contains(want), "摘要缺 {want}：{s}");
        }
    }

    #[test]
    fn 未标注来源不伪装成正常() {
        let p = parse(r#"{"data_provenance":{"source":""}}"#).data_provenance.unwrap();
        assert!(p.summary().contains("未标注"));
        assert!(p.is_low_confidence());
    }

    #[test]
    fn 撮合摘要点明一触价即成交() {
        let p = parse(r#"{"data_provenance":{"execution":{"queue_position":false}}}"#)
            .data_provenance
            .unwrap();
        assert!(p.execution_summary().contains("一触价即成交"));
    }
}

#[cfg(test)]
mod crossend_tests {
    use super::*;

    /// 跨端对拍：Rust 的结构体必须吃得下 Python 真实写出来的 result.json。
    ///
    /// 两端的字段名各写一遍，只靠人眼比对迟早会漂。这里钉住一份**真实产物的样本**
    /// （由 `run_tardis_backtest.py` 实跑生成后裁剪），字段改名会立刻红。
    const SAMPLE: &str = r#"{
      "meta": {"strategy": "smoke_maker", "symbol": "BTCUSDT-PERP.BINANCE", "bars": 126116,
               "run": "20260916-175058", "finished_at": "2026-09-16 17:50:58"},
      "data_provenance": {
        "source": "tardis", "grade": "B", "quality_flags": ["warn:l2_latency"],
        "window": {"symbol": "BTCUSDT", "date": "2026-06-02", "start": "00:00", "minutes": 2},
        "execution": {"queue_position": true, "liquidity_consumption": true,
                      "latency_ms": 10.0, "maker_bp": 2.0, "taker_bp": 4.5,
                      "book_type": "L2_MBP"},
        "strategy": {"file": "smoke_maker.py", "sha256": "a1c36e49c3fa"},
        "versions": {"python": "3.12.3", "nautilus": "1.228.0", "repo_commit": "e1fab7e"}
      }
    }"#;

    #[test]
    fn 吃得下python的真实产物() {
        let r: BacktestResult = serde_json::from_str(SAMPLE).expect("解析真实产物失败");
        let p = r.data_provenance.expect("溯源段丢了");
        assert_eq!(p.source, "tardis");
        assert_eq!(p.grade.as_deref(), Some("B"));
        assert_eq!(p.quality_flags, vec!["warn:l2_latency"]);
        assert_eq!(p.window.minutes, Some(2));
        assert!(p.execution.queue_position);
        assert_eq!(p.execution.taker_bp, Some(4.5));
        assert_eq!(p.strategy.sha256, "a1c36e49c3fa");
        assert_eq!(p.versions.get("nautilus").map(String::as_str), Some("1.228.0"));
        assert!(!p.is_low_confidence());
    }
}

// ── 回测进度旁路（docs/27 §12）─────────────────────────────────────────────
use super::bt_trades::BtProgress;

static PROGRESS: OnceLock<Mutex<BtProgress>> = OnceLock::new();
static PROGRESS_POLLER: std::sync::Once = std::sync::Once::new();

/// 进度快照，供「回测进行中」那一页画进度条。
///
/// **不在渲染里直接读 Redis**：那是阻塞 IO，每帧一次会把 UI 拖住。后台线程轮询，
/// 渲染只读内存快照——与本文件其余部分同一套路。
pub fn progress_snapshot() -> BtProgress {
    ensure_progress_poller();
    PROGRESS
        .get()
        .and_then(|m| m.lock().ok().map(|g| g.clone()))
        .unwrap_or_default()
}

fn ensure_progress_poller() {
    PROGRESS_POLLER.call_once(|| {
        let url = std::env::var("WS_REDIS_URL")
            .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
        std::thread::spawn(move || {
            loop {
                // 只在回测跑着时才去读——没在跑的话连 run_id 都没有，白跑一次 Redis 往返。
                let run = super::active_run::current()
                    .filter(|a| a.mode == "backtest")
                    .map(|a| a.run_id);
                if let Some(run) = run
                    && let Some(p) = super::bt_trades::fetch_progress(&url, &run)
                {
                    let lock = PROGRESS.get_or_init(|| Mutex::new(BtProgress::default()));
                    if let Ok(mut g) = lock.lock() {
                        *g = p;
                    }
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        });
    });
}
