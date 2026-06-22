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
    pub id: i64,
    pub gen_src: String,
    pub expr: String,
    pub horizon: String,
    pub ic_mean: f64,
    pub ic_t: f64,
    pub folds_same: i64,
    pub n_folds: i64,
    pub net_bp: f64,
    pub leakage: bool,
    pub hypothesis: String, // 机理假设（seed/llm 有；gp 子代多为空）→ 悬浮显示
}

/// 预测视界（由短到长）——IC 衰减曲线的 x 轴顺序。与 factory 评估口径一致。
pub const HORIZONS: [&str; 6] = ["500ms", "1s", "5s", "30s", "2m", "5m"];

/// 一条 alpha 的 IC 跨视界廓线（IC 衰减曲线的一条线）。
#[derive(Default, Clone)]
pub struct IcDecay {
    pub gen_src: String,
    pub expr: String,
    pub pts: Vec<(usize, f64)>, // (HORIZONS 下标, ic_mean)，按视界升序
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

/// nightly 实时进度（读 Redis 流 `factory:progress`，nightly.py 发布）。
/// 报告 md 只在 run **结束后**落盘；本结构给出运行**中**的逐步进度。
#[derive(Default, Clone)]
pub struct NightlyLive {
    pub seen: bool,        // 流里有任何数据
    pub running: bool,     // 已 start、未 finish
    pub date: String,      // 最新一轮 run 的日期
    pub header: String,    // 人读汇总行（运行中 / ✅ 完成 / ❌ 停机于 X）
    pub steps: Vec<(String, i64, f64)>, // 最近若干步 (step, rc, secs)，oldest→newest
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
    pub ic_decay: Vec<IcDecay>,
    pub stage_b: Vec<StageBRow>,
    pub pool: Vec<PoolRow>,
    pub combos: Vec<ComboRow>,
    pub live: Vec<LiveRow>,
    pub lake: Vec<LakeRow>,
    pub nightly_title: String,
    pub nightly_lines: Vec<String>,
    pub nightly_live: NightlyLive,
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
            read_progress(&mut st);
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
        "SELECT a.id, a.gen_src, a.expr, e.horizon, e.ic_mean, e.ic_t, e.folds_same_sign, e.n_folds,
                e.net_bp, e.leakage, COALESCE(a.hypothesis,'')
         FROM evals e JOIN alphas a ON a.id=e.alpha_id
         WHERE e.stage='A' AND e.ic_t IS NOT NULL
         ORDER BY ABS(e.ic_t) DESC LIMIT 16",
    ) {
        st.stage_a = stmt
            .query_map([], |r| {
                Ok(AlphaRow {
                    id: r.get(0)?,
                    gen_src: r.get(1)?,
                    expr: r.get(2)?,
                    horizon: r.get(3)?,
                    ic_mean: r.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
                    ic_t: r.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
                    folds_same: r.get::<_, Option<i64>>(6)?.unwrap_or(0),
                    n_folds: r.get::<_, Option<i64>>(7)?.unwrap_or(0),
                    net_bp: r.get::<_, Option<f64>>(8)?.unwrap_or(f64::NAN),
                    leakage: r.get::<_, Option<i64>>(9)?.unwrap_or(0) != 0,
                    hypothesis: r.get(10)?,
                })
            })
            .map(|it| it.flatten().collect())
            .unwrap_or_default();
    }

    read_ic_decay(&conn, &mut st);

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
    read_progress(&mut st);
    st
}

fn redis_url() -> String {
    std::env::var("WS_REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string())
}

/// 读 Redis 流 `factory:progress` 末尾若干条 → 重建最新一轮 nightly 的实时进度。
/// Redis 不可用/流为空时静默跳过（保持 `seen=false`，面板回落到 md 报告）。
fn read_progress(st: &mut FactoryReadout) {
    use redis::Commands;
    use redis::streams::StreamRangeReply;

    let Ok(client) = redis::Client::open(redis_url()) else {
        return;
    };
    let Ok(mut conn) = client.get_connection() else {
        return;
    };
    // newest→oldest 取末尾 80 条（够覆盖单轮 13~40 步 + start/finish）
    let reply: StreamRangeReply =
        match conn.xrevrange_count("factory:progress", "+", "-", 80) {
            Ok(r) => r,
            Err(_) => return,
        };
    // 把 reply 摊平成 newest→oldest 的 (字段→值) 列表，再交给纯函数解析（可单测）。
    let events: Vec<std::collections::HashMap<String, String>> = reply
        .ids
        .iter()
        .map(|id| {
            id.map
                .keys()
                .filter_map(|k| id.get::<String>(k).map(|v| (k.clone(), v)))
                .collect()
        })
        .collect();
    if let Some(live) = build_live(&events) {
        st.nightly_live = live;
    }
}

/// 从 `factory:progress` 事件（newest→oldest）重建最新一轮 nightly 进度。无数据返回 None。
fn build_live(events: &[std::collections::HashMap<String, String>]) -> Option<NightlyLive> {
    let g = |e: &std::collections::HashMap<String, String>, k: &str| -> String {
        e.get(k).cloned().unwrap_or_default()
    };
    // 最新一轮的日期 = 最新一条带 date 字段的事件
    let date = events.iter().find_map(|e| {
        let d = g(e, "date");
        (!d.is_empty()).then_some(d)
    })?;

    let mut live = NightlyLive { seen: true, date: date.clone(), ..Default::default() };
    let mut finish: Option<(i64, i64, String, f64)> = None; // done,total,failed_at,secs
    let mut started = false;
    for e in events {
        if g(e, "date") != date {
            continue; // 只看最新一轮
        }
        match g(e, "kind").as_str() {
            "finish" if finish.is_none() => {
                finish = Some((
                    g(e, "done").parse().unwrap_or(0),
                    g(e, "total").parse().unwrap_or(0),
                    g(e, "failed_at"),
                    g(e, "secs").parse().unwrap_or(0.0),
                ));
            }
            "start" => started = true,
            "step" if live.steps.len() < 14 => {
                live.steps.push((
                    g(e, "step"),
                    g(e, "rc").parse().unwrap_or(0),
                    g(e, "secs").parse().unwrap_or(0.0),
                ));
            }
            _ => {}
        }
    }
    live.steps.reverse(); // newest-first 收集 → 翻回 oldest→newest 顺读

    if let Some((done, total, failed_at, secs)) = finish {
        live.running = false;
        live.header = if failed_at.is_empty() {
            format!("✅ 完成 {done}/{total} · {secs:.0}s")
        } else {
            format!("❌ 停机于 {failed_at}（{done}/{total} · {secs:.0}s）")
        };
    } else {
        live.running = started || !live.steps.is_empty();
        let n = live.steps.len();
        let tot: f64 = live.steps.iter().map(|s| s.2).sum();
        live.header = format!("🟢 运行中 · 已 {n} 步 · {tot:.0}s");
    }
    Some(live)
}


/// IC 衰减曲线：取 |IC t| 最强的前 6 个 alpha，各取其跨视界 ic_mean 廓线。
/// 健康的微观结构 alpha 应短视界 IC 高、随视界拉长衰减；平/反增即可疑（人工抽查排雷）。
fn read_ic_decay(conn: &rusqlite::Connection, st: &mut FactoryReadout) {
    // 强度排序的 top alpha_id（去重）
    let top_ids: Vec<i64> = conn
        .prepare(
            "SELECT alpha_id FROM evals WHERE stage='A' AND ic_t IS NOT NULL
             GROUP BY alpha_id ORDER BY MAX(ABS(ic_t)) DESC LIMIT 6",
        )
        .and_then(|mut s| {
            s.query_map([], |r| r.get::<_, i64>(0)).map(|it| it.flatten().collect())
        })
        .unwrap_or_default();
    if top_ids.is_empty() {
        return;
    }
    let hidx = |h: &str| HORIZONS.iter().position(|x| *x == h);
    for id in top_ids {
        let mut row = IcDecay::default();
        if let Ok((src, expr)) = conn.query_row(
            "SELECT gen_src, expr FROM alphas WHERE id=?",
            [id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        ) {
            row.gen_src = src;
            row.expr = expr;
        }
        if let Ok(mut stmt) = conn.prepare(
            "SELECT horizon, ic_mean FROM evals
             WHERE stage='A' AND alpha_id=? AND ic_mean IS NOT NULL",
        ) {
            let pairs: Vec<(String, f64)> = stmt
                .query_map([id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))
                .map(|it| it.flatten().collect())
                .unwrap_or_default();
            for (h, ic) in pairs {
                if let Some(i) = hidx(&h) {
                    row.pts.push((i, ic));
                }
            }
            row.pts.sort_by_key(|p| p.0);
        }
        if !row.pts.is_empty() {
            st.ic_decay.push(row);
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn ev(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn empty_stream_yields_none() {
        assert!(build_live(&[]).is_none());
    }

    #[test]
    fn running_run_reports_progress() {
        // newest→oldest：labels, features, record-check, start
        let events = vec![
            ev(&[("kind", "step"), ("date", "2026-06-22"), ("step", "labels:BTCUSDT"), ("rc", "0"), ("secs", "1.2")]),
            ev(&[("kind", "step"), ("date", "2026-06-22"), ("step", "features:BTCUSDT"), ("rc", "0"), ("secs", "5.4")]),
            ev(&[("kind", "step"), ("date", "2026-06-22"), ("step", "record-check"), ("rc", "0"), ("secs", "2.1")]),
            ev(&[("kind", "start"), ("date", "2026-06-22")]),
        ];
        let live = build_live(&events).unwrap();
        assert!(live.running);
        assert_eq!(live.date, "2026-06-22");
        assert_eq!(live.steps.len(), 3);
        assert_eq!(live.steps[0].0, "record-check"); // oldest→newest
        assert_eq!(live.steps[2].0, "labels:BTCUSDT");
        assert!(live.header.contains("运行中"));
    }

    #[test]
    fn finished_run_reports_done() {
        let events = vec![
            ev(&[("kind", "finish"), ("date", "2026-06-22"), ("done", "13"), ("total", "13"), ("failed_at", ""), ("secs", "240")]),
            ev(&[("kind", "step"), ("date", "2026-06-22"), ("step", "stage-b:BTCUSDT"), ("rc", "0"), ("secs", "30")]),
            ev(&[("kind", "start"), ("date", "2026-06-22")]),
        ];
        let live = build_live(&events).unwrap();
        assert!(!live.running);
        assert!(live.header.starts_with("✅ 完成 13/13"));
    }

    #[test]
    fn failed_run_reports_halt() {
        let events = vec![
            ev(&[("kind", "finish"), ("date", "2026-06-22"), ("done", "2"), ("total", "3"), ("failed_at", "labels:BTCUSDT"), ("secs", "9")]),
            ev(&[("kind", "step"), ("date", "2026-06-22"), ("step", "labels:BTCUSDT"), ("rc", "1"), ("secs", "0.3")]),
        ];
        let live = build_live(&events).unwrap();
        assert!(!live.running);
        assert!(live.header.starts_with("❌ 停机于 labels:BTCUSDT"));
    }

    /// 真机集成（需 WS_REDIS_URL 指向活的 redis，流里已有 nightly.py 发的事件）：
    /// `cargo test --bin flowsurface reads_live_stream -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn reads_live_stream() {
        let mut st = FactoryReadout::default();
        read_progress(&mut st);
        let lv = &st.nightly_live;
        eprintln!("seen={} running={} date={} header={}", lv.seen, lv.running, lv.date, lv.header);
        for s in &lv.steps {
            eprintln!("  {} rc={} {}s", s.0, s.1, s.2);
        }
        assert!(lv.seen, "应从 factory:progress 流读到事件");
    }

    #[test]
    fn only_latest_run_is_shown() {
        // 流里同时有 06-21（旧、已完成）与 06-22（新、运行中）→ 只取 06-22
        let events = vec![
            ev(&[("kind", "step"), ("date", "2026-06-22"), ("step", "record-check"), ("rc", "0"), ("secs", "2")]),
            ev(&[("kind", "start"), ("date", "2026-06-22")]),
            ev(&[("kind", "finish"), ("date", "2026-06-21"), ("done", "13"), ("total", "13"), ("failed_at", ""), ("secs", "200")]),
        ];
        let live = build_live(&events).unwrap();
        assert_eq!(live.date, "2026-06-22");
        assert!(live.running);
        assert_eq!(live.steps.len(), 1);
    }
}
