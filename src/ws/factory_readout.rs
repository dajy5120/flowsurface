//! Alpha Factory 只读快照（docs/08 F6 — P2）。
//!
//! 把独立 `wealthspring-factory-gui` 的后台 poller 移植进 cockpit 进程:每 4s 轮询 Registry
//! （`~/ws-data/registry.sqlite`,WAL 与 Python 写端共存,只读打开）+ nightly 报告 + 数据湖扫描,
//! 发布到进程级快照,供 `Content::Factory` pane 渲染(`ws/factory_view.rs`)。
//!
//! 沿用 F6 旁路模式:poller 在后台线程跑 IO,pane 视图只读快照、不阻塞渲染。首次 [`snapshot`]
//! 时惰性起 poller(只有打开过 Factory 工作区才轮询)。

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

#[derive(Default, Clone)]
pub struct AlphaRow {
    pub gen_src: String,
    pub expr: String,
    pub horizon: String,
    pub ic_mean: f64,
    pub ic_t: f64,
    pub folds_same: i64,
    pub n_folds: i64,
    pub net_bp: f64,
    pub leakage: bool,
}

#[derive(Default, Clone)]
pub struct PoolRow {
    pub expr: String,
    pub weight: f64,
    pub cluster: i64,
    pub ic_t: f64,
}

#[derive(Default, Clone)]
pub struct ComboRow {
    pub method: String,
    pub ic_mean: f64,
    pub ic_t: f64,
    pub net_bp: f64,
    pub status: String,
}

#[derive(Default, Clone)]
pub struct StageBRow {
    pub expr: String,
    pub n_entries: i64,
    pub pnl: f64,
    pub net_bp: f64,
}

#[derive(Default, Clone)]
pub struct LiveRow {
    pub symbol: String,
    pub realized_ic_1s: f64,
    pub pnl: f64,
    pub n_trades: i64,
    pub age: String,
}

#[derive(Default, Clone)]
pub struct LakeRow {
    pub symbol: String,
    pub raw_days: i64,
    pub feat_frames: i64,
    pub sig_frames: i64,
}

#[derive(Default, Clone)]
pub struct FactoryReadout {
    pub db_ok: bool,
    pub status_counts: Vec<(String, i64)>,
    pub gensrc_counts: Vec<(String, i64)>,
    pub n_evals: i64,
    pub n_trials: i64,
    pub n_combos: i64,
    pub thresholds: Vec<(String, String)>,
    pub stage_a: Vec<AlphaRow>,
    pub stage_b: Vec<StageBRow>,
    pub pool: Vec<PoolRow>,
    pub combos: Vec<ComboRow>,
    pub live: Vec<LiveRow>,
    pub lake: Vec<LakeRow>,
    pub nightly_title: String,
    pub nightly_lines: Vec<String>,
    pub refreshed: String,
    pub started: bool,
}

static READOUT: OnceLock<Mutex<FactoryReadout>> = OnceLock::new();
static POLLER: OnceLock<()> = OnceLock::new();

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}
fn db_path() -> PathBuf {
    std::env::var("WS_REGISTRY")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home().join("ws-data/registry.sqlite"))
}

/// pane 渲染时读快照(惰性起 poller:首次调用才开后台线程)。
pub fn snapshot() -> FactoryReadout {
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
            let lock = READOUT.get_or_init(|| Mutex::new(FactoryReadout::default()));
            if let Ok(mut g) = lock.lock() {
                *g = snap;
            }
            std::thread::sleep(Duration::from_secs(4));
        });
    });
}

fn poll_once() -> FactoryReadout {
    let mut st = FactoryReadout {
        refreshed: chrono::Local::now().format("%H:%M:%S").to_string(),
        started: true,
        ..Default::default()
    };
    let conn = match rusqlite::Connection::open_with_flags(
        db_path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(c) => c,
        Err(_) => {
            scan_lake(&mut st);
            read_nightly(&mut st);
            return st;
        }
    };
    st.db_ok = true;

    st.status_counts = q_pairs(
        &conn,
        "SELECT status, COUNT(*) FROM alphas GROUP BY status ORDER BY 2 DESC",
    );
    st.gensrc_counts = q_pairs(
        &conn,
        "SELECT gen_src, COUNT(*) FROM alphas GROUP BY gen_src ORDER BY 2 DESC",
    );
    st.n_evals = q_i64(&conn, "SELECT COUNT(*) FROM evals");
    st.n_trials = q_i64(&conn, "SELECT COALESCE(SUM(n_evaluated),0) FROM trials");
    st.n_combos = q_i64(&conn, "SELECT COUNT(*) FROM combos");
    st.thresholds = q_pairs_s(&conn, "SELECT key, value FROM config ORDER BY key");

    if let Ok(mut stmt) = conn.prepare(
        "SELECT a.gen_src, a.expr, e.horizon, e.ic_mean, e.ic_t, e.folds_same_sign, e.n_folds,
                e.net_bp, e.leakage
         FROM evals e JOIN alphas a ON a.id=e.alpha_id
         WHERE e.stage='A' AND e.ic_t IS NOT NULL
         ORDER BY ABS(e.ic_t) DESC LIMIT 16",
    ) {
        st.stage_a = stmt
            .query_map([], |r| {
                Ok(AlphaRow {
                    gen_src: r.get(0)?,
                    expr: r.get(1)?,
                    horizon: r.get(2)?,
                    ic_mean: r.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
                    ic_t: r.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
                    folds_same: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                    n_folds: r.get::<_, Option<i64>>(6)?.unwrap_or(0),
                    net_bp: r.get::<_, Option<f64>>(7)?.unwrap_or(f64::NAN),
                    leakage: r.get::<_, Option<i64>>(8)?.unwrap_or(0) != 0,
                })
            })
            .map(|it| it.flatten().collect())
            .unwrap_or_default();
    }

    if let Ok(mut stmt) = conn.prepare(
        "SELECT a.expr, json_extract(e.detail_json,'$.n_entries'),
                json_extract(e.detail_json,'$.pnl_usdt'), e.net_bp
         FROM evals e JOIN alphas a ON a.id=e.alpha_id
         WHERE e.stage='B' ORDER BY e.ts DESC LIMIT 8",
    ) {
        st.stage_b = stmt
            .query_map([], |r| {
                Ok(StageBRow {
                    expr: r.get(0)?,
                    n_entries: r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    pnl: r.get::<_, Option<f64>>(2)?.unwrap_or(f64::NAN),
                    net_bp: r.get::<_, Option<f64>>(3)?.unwrap_or(f64::NAN),
                })
            })
            .map(|it| it.flatten().collect())
            .unwrap_or_default();
    }

    if let Ok(mut stmt) = conn.prepare(
        "SELECT a.expr, COALESCE(p.weight,0.0), p.cluster_id,
                COALESCE((SELECT MAX(ABS(e.ic_t)) FROM evals e WHERE e.alpha_id=a.id AND e.stage='A'),0.0)
         FROM pool_membership p JOIN alphas a ON a.id=p.alpha_id
         WHERE p.until_ts IS NULL ORDER BY ABS(COALESCE(p.weight,0.0)) DESC, 4 DESC LIMIT 16",
    ) {
        st.pool = stmt
            .query_map([], |r| {
                Ok(PoolRow {
                    expr: r.get(0)?,
                    weight: r.get(1)?,
                    cluster: r.get(2)?,
                    ic_t: r.get(3)?,
                })
            })
            .map(|it| it.flatten().collect())
            .unwrap_or_default();
    }

    if let Ok(mut stmt) = conn.prepare(
        "SELECT method, json_extract(eval_json,'$.ic_mean'), json_extract(eval_json,'$.ic_t'),
                json_extract(eval_json,'$.net_bp'), status
         FROM combos ORDER BY id DESC LIMIT 8",
    ) {
        st.combos = stmt
            .query_map([], |r| {
                Ok(ComboRow {
                    method: r.get(0)?,
                    ic_mean: r.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
                    ic_t: r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                    net_bp: r.get::<_, Option<f64>>(3)?.unwrap_or(f64::NAN),
                    status: r.get(4)?,
                })
            })
            .map(|it| it.flatten().collect())
            .unwrap_or_default();
    }

    if table_exists(&conn, "live_metrics") {
        let now = chrono::Utc::now().timestamp();
        if let Ok(mut stmt) = conn.prepare(
            "SELECT symbol, ts, json_extract(metrics_json,'$.realized_ic_1s'),
                    json_extract(metrics_json,'$.pnl_usdt'), json_extract(metrics_json,'$.n_trades')
             FROM live_metrics ORDER BY ts DESC LIMIT 6",
        ) {
            st.live = stmt
                .query_map([], |r| {
                    let ts: i64 = r.get(1)?;
                    let mins = (now - ts) / 60;
                    Ok(LiveRow {
                        symbol: r.get(0)?,
                        realized_ic_1s: r.get::<_, Option<f64>>(2)?.unwrap_or(f64::NAN),
                        pnl: r.get::<_, Option<f64>>(3)?.unwrap_or(f64::NAN),
                        n_trades: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                        age: if mins < 60 {
                            format!("{mins}分前")
                        } else {
                            format!("{}时前", mins / 60)
                        },
                    })
                })
                .map(|it| it.flatten().collect())
                .unwrap_or_default();
        }
    }

    scan_lake(&mut st);
    read_nightly(&mut st);
    st
}

fn q_i64(c: &rusqlite::Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).unwrap_or(0)
}
fn q_pairs(c: &rusqlite::Connection, sql: &str) -> Vec<(String, i64)> {
    c.prepare(sql)
        .and_then(|mut s| {
            s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map(|it| it.flatten().collect())
        })
        .unwrap_or_default()
}
fn q_pairs_s(c: &rusqlite::Connection, sql: &str) -> Vec<(String, String)> {
    c.prepare(sql)
        .and_then(|mut s| {
            s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map(|it| it.flatten().collect())
        })
        .unwrap_or_default()
}
fn table_exists(c: &rusqlite::Connection, t: &str) -> bool {
    c.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?",
        [t],
        |_| Ok(()),
    )
    .is_ok()
}

fn scan_lake(st: &mut FactoryReadout) {
    let dd = home().join("ws-data");
    let syms = ["BTCUSDT", "ETHUSDT", "SOLUSDT", "BNBUSDT", "XRPUSDT", "DOGEUSDT"];
    for sym in syms {
        let raw_days = count_dir(&dd.join("raw/l2").join(sym));
        let feat = count_glob(&dd.join("features").join(sym), ".parquet");
        let sig = count_glob(&dd.join("signals").join(sym), ".parquet");
        if raw_days + feat + sig > 0 {
            st.lake.push(LakeRow {
                symbol: sym.into(),
                raw_days,
                feat_frames: feat,
                sig_frames: sig,
            });
        }
    }
}
fn count_dir(p: &PathBuf) -> i64 {
    std::fs::read_dir(p)
        .map(|rd| rd.flatten().filter(|e| e.path().is_dir()).count() as i64)
        .unwrap_or(0)
}
fn count_glob(p: &PathBuf, suffix: &str) -> i64 {
    std::fs::read_dir(p)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_name().to_string_lossy().ends_with(suffix))
                .count() as i64
        })
        .unwrap_or(0)
}

fn read_nightly(st: &mut FactoryReadout) {
    let dir = home().join("ws-data/reports");
    let mut reports: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .map(|n| {
                            n.to_string_lossy().starts_with("nightly-")
                                && n.to_string_lossy().ends_with(".md")
                        })
                        .unwrap_or(false)
                })
                .collect()
        })
        .unwrap_or_default();
    reports.sort();
    if let Some(latest) = reports.last() {
        if let Ok(content) = std::fs::read_to_string(latest) {
            let mut lines = content.lines();
            st.nightly_title = lines.next().unwrap_or("").trim_start_matches("# ").to_string();
            st.nightly_lines = lines
                .filter(|l| !l.trim().is_empty() && !l.starts_with("|---"))
                .take(20)
                .map(|l| l.to_string())
                .collect();
        }
    } else {
        st.nightly_title = "（尚无 nightly 报告——首跑 08:34 CST）".into();
    }
}

pub fn db_path_display() -> String {
    db_path().display().to_string()
}
