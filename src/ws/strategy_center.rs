//! 策略中心（docs/37 P1）：策略库 · 参数表单 · 一键回测 · 运行记录与概况。
//!
//! **面板不跑回测、不算指标**——三样东西各有唯一来源：
//!
//! | 什么 | 从哪来 | 怎么来 |
//! |---|---|---|
//! | 策略目录 + 参数表 | `python -m factory.lab catalog --json --with-params` | 打开 / 点「刷新」时后台跑一次 |
//! | 运行记录 + 汇总 | `~/ws-data/research.sqlite`（只读） | 后台线程轮询：有运行在跑 2 秒，否则 10 秒 |
//! | 发起 / 停止回测 | ws-control 的 `run_spec` / `stop` | 控制 socket 一行 JSON |
//!
//! 视图每帧都会调用，所以**视图里不做任何 IO**——只读这里的快照。

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

use super::strategy_center_opt::{self as opt, OptForm, StudyRow};

// ── 数据形状（与 factory/lab 的 JSON 一致）─────────────────────────────

#[derive(Deserialize, Clone, Default, Debug)]
pub struct Param {
    pub name: String,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub default: Value,
    #[serde(default)]
    pub minimum: Option<f64>,
    #[serde(default)]
    pub maximum: Option<f64>,
    #[serde(default)]
    pub step: Option<f64>,
    #[serde(rename = "enum", default)]
    pub choices: Option<Vec<Value>>,
    #[serde(default)]
    pub optimize: bool,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub nullable: bool,
}

#[derive(Deserialize, Clone, Default, Debug)]
pub struct Meta {
    pub id: String,
    pub name: String,
    pub version: String,
    pub family: String,
    #[serde(default)]
    pub engines: Vec<String>,
    pub verdict: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub data: Vec<String>,
    #[serde(default)]
    pub verdict_note: String,
    #[serde(default)]
    pub docs: Vec<String>,
}

#[derive(Deserialize, Clone, Default, Debug)]
pub struct Entry {
    pub id: String,
    pub path: String,
    #[serde(default)]
    pub meta: Option<Meta>,
    #[serde(default)]
    pub backtest: Option<Value>,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub params: Vec<Param>,
}

impl Entry {
    /// 策略 `BACKTEST` 里声明的某个字段（字符串）。
    pub fn bt(&self, key: &str) -> String {
        self.backtest
            .as_ref()
            .and_then(|b| b.get(key))
            .map(|v| match v {
                Value::String(s) => s.clone(),
                Value::Array(a) => a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("+"),
                other => other.to_string(),
            })
            .unwrap_or_default()
    }
}

/// 研究库里一次运行的汇总（字段与 `factory.lab.run.summarize` 一致；多出来的字段忽略）。
#[derive(Deserialize, Clone, Default, Debug)]
pub struct Summary {
    pub pnl: Option<f64>,
    pub pnl_pct: Option<f64>,
    pub max_dd_pct: Option<f64>,
    #[serde(default)]
    pub dd_basis: Option<String>,
    pub sharpe: Option<f64>,
    pub sortino: Option<f64>,
    #[serde(default)]
    pub returns_basis: Option<String>,
    pub win_rate: Option<f64>,
    pub profit_factor: Option<f64>,
    pub fills: Option<f64>,
    pub start_balance: Option<f64>,
    pub end_balance: Option<f64>,
    pub elapsed_s: Option<f64>,
    #[serde(default)]
    pub grade: Option<String>,
    #[serde(default)]
    pub quality_flags: Vec<String>,
    pub stop_outs: Option<f64>,
    pub min_margin_level_pct: Option<f64>,
}

#[derive(Clone, Debug, Default)]
pub struct RunRow {
    pub run_id: String,
    pub strategy_id: String,
    pub status: String,
    pub created_ts: i64,
    pub finished_ts: Option<i64>,
    pub params: Value,
    pub data: Value,
    pub note: String,
    pub dirty: bool,
    pub error: String,
    pub result_dir: Option<String>,
    pub summary: Option<Summary>,
    /// 优化试验带 study_id（运行记录页不列它们，在「优化」页看）
    pub study_id: Option<String>,
}

impl RunRow {
    pub fn active(&self) -> bool {
        matches!(self.status.as_str(), "queued" | "running")
    }
}

// ── 消息 ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum ScMsg {
    Refresh,
    Select(String),
    Search(String),
    Verdict(Option<String>),
    Field(String, String),
    Start(String),
    End(String),
    Note(String),
    ResetParams,
    RunFull,
    Stop(String),
    PickRun(String),
    FollowLatest,
    OpenSource,
    // ── 右栏页签 / 参数优化 ──
    Tab(Tab),
    OptToggle(String),
    OptLow(String, String),
    OptHigh(String, String),
    OptStep(String, String),
    OptSampler(String),
    OptTrials(String),
    OptWorkers(String),
    OptObjective(String),
    StartStudy,
    StopStudy(String),
    PickStudy(String),
}

/// 右栏页签。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Runs,
    Studies,
}

// ── 状态 ────────────────────────────────────────────────────────────────

#[derive(Default)]
struct St {
    catalog: Vec<Entry>,
    catalog_loading: bool,
    catalog_err: String,
    catalog_at: Option<Instant>,
    selected: Option<String>,
    search: String,
    verdict: Option<String>,
    form: BTreeMap<String, String>,
    start: String,
    end: String,
    note: String,
    /// 当前所选策略的运行记录（新 → 旧）
    runs: Vec<RunRow>,
    /// 每个策略最近一次**完成**的运行（策略库列表里显示）
    latest: HashMap<String, RunRow>,
    db_err: String,
    picked_run: Option<String>,
    msg: String,
    sending: bool,
    tab: Tab,
    opt: OptForm,
    studies: Vec<StudyRow>,
    picked_study: Option<String>,
}

static ST: OnceLock<Mutex<St>> = OnceLock::new();
static POLLER: OnceLock<()> = OnceLock::new();
/// 选择变了 / 刚发起运行 → 让轮询线程马上读一次，不等满周期。
static KICK: AtomicBool = AtomicBool::new(false);

fn cell() -> &'static Mutex<St> {
    ST.get_or_init(|| Mutex::new(St::default()))
}

fn with<R>(f: impl FnOnce(&mut St) -> R) -> Option<R> {
    cell().lock().ok().map(|mut g| f(&mut g))
}

fn db_path() -> PathBuf {
    std::env::var("WS_RESEARCH_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("ws-data/research.sqlite"))
}

fn control_sock() -> PathBuf {
    super::paths::runtime_dir().join("control.sock")
}

// ── 视图读的快照 ────────────────────────────────────────────────────────

#[derive(Clone, Default)]
pub struct View {
    pub catalog: Vec<Entry>,
    pub catalog_loading: bool,
    pub catalog_err: String,
    pub selected: Option<Entry>,
    pub search: String,
    pub verdict: Option<String>,
    pub form: BTreeMap<String, String>,
    pub field_errors: BTreeMap<String, String>,
    pub changed: Vec<String>,
    pub start: String,
    pub end: String,
    pub note: String,
    pub runs: Vec<RunRow>,
    pub latest: HashMap<String, RunRow>,
    pub db_err: String,
    pub picked_run: Option<String>,
    pub msg: String,
    pub sending: bool,
    pub tab: Tab,
    pub opt: OptForm,
    pub grid_estimate: Option<usize>,
    pub studies: Vec<StudyRow>,
    pub picked_study: Option<String>,
}

pub fn view() -> View {
    ensure_started();
    let Ok(g) = cell().lock() else { return View::default() };
    let selected = g.selected.as_ref().and_then(|id| g.catalog.iter().find(|e| &e.id == id).cloned());
    let (field_errors, changed) = match &selected {
        Some(e) => {
            let (_, errs, changed) = build_params(&e.params, &g.form);
            (errs, changed)
        }
        None => (BTreeMap::new(), vec![]),
    };
    View {
        catalog: g.catalog.clone(),
        catalog_loading: g.catalog_loading,
        catalog_err: g.catalog_err.clone(),
        selected,
        search: g.search.clone(),
        verdict: g.verdict.clone(),
        form: g.form.clone(),
        field_errors,
        changed,
        start: g.start.clone(),
        end: g.end.clone(),
        note: g.note.clone(),
        runs: g.runs.clone(),
        latest: g.latest.clone(),
        db_err: g.db_err.clone(),
        picked_run: g.picked_run.clone(),
        msg: g.msg.clone(),
        sending: g.sending,
        tab: g.tab,
        grid_estimate: g
            .selected
            .as_ref()
            .and_then(|id| g.catalog.iter().find(|e| &e.id == id))
            .and_then(|e| opt::grid_estimate(e, &g.opt)),
        opt: g.opt.clone(),
        studies: g.studies.clone(),
        picked_study: g.picked_study.clone(),
    }
}

fn ensure_started() {
    POLLER.get_or_init(|| {
        load_catalog();
        super::spawn_named("ws-strategyctr", || {
            let mut last = Instant::now() - Duration::from_secs(3600);
            loop {
                let busy = with(|g| g.runs.iter().any(RunRow::active) || g.studies.iter().any(StudyRow::active)).unwrap_or(false);
                let period = Duration::from_secs(if busy { 2 } else { 10 });
                if KICK.swap(false, Ordering::Relaxed) || last.elapsed() >= period {
                    poll_db();
                    last = Instant::now();
                }
                std::thread::sleep(Duration::from_millis(300));
            }
        });
    });
}

// ── 目录：python -m factory.lab catalog ────────────────────────────────

fn load_catalog() {
    let started = with(|g| {
        if g.catalog_loading {
            return false;
        }
        g.catalog_loading = true;
        g.catalog_err.clear();
        true
    })
    .unwrap_or(false);
    if !started {
        return;
    }
    super::spawn_named("ws-stratcatalog", || {
        let out = std::process::Command::new(super::paths::python())
            .args(["-m", "factory.lab", "catalog", "--json", "--with-params"])
            .current_dir(super::paths::repo_root())
            .output();
        let parsed: Result<Vec<Entry>, String> = match out {
            Ok(o) if o.status.success() || !o.stdout.is_empty() => {
                serde_json::from_slice(&o.stdout).map_err(|e| format!("目录 JSON 解析失败：{e}"))
            }
            Ok(o) => Err(format!(
                "factory.lab catalog 失败（{}）：{}",
                o.status,
                String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or_default()
            )),
            Err(e) => Err(format!("起不来 Python：{e}")),
        };
        with(|g| {
            g.catalog_loading = false;
            g.catalog_at = Some(Instant::now());
            match parsed {
                Ok(v) => {
                    g.catalog = v;
                    // 原来选中的策略还在就保留；否则选第一个合法的
                    let keep = g.selected.as_ref().is_some_and(|id| g.catalog.iter().any(|e| &e.id == id));
                    if !keep {
                        let first = g.catalog.iter().find(|e| e.meta.is_some() && e.error.is_empty()).map(|e| e.id.clone());
                        if let Some(id) = first {
                            select_in(g, &id);
                        }
                    }
                }
                Err(e) => g.catalog_err = e,
            }
        });
        KICK.store(true, Ordering::Relaxed);
    });
}

// ── 研究库：只读 ───────────────────────────────────────────────────────

fn parse_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<RunRow> {
    let spec: String = r.get("spec_json")?;
    let spec: Value = serde_json::from_str(&spec).unwrap_or(Value::Null);
    let summary: Option<String> = r.get("summary_json")?;
    Ok(RunRow {
        run_id: r.get("run_id")?,
        strategy_id: r.get("strategy_id")?,
        status: r.get("status")?,
        created_ts: r.get("created_ts")?,
        finished_ts: r.get("finished_ts")?,
        params: spec.get("params").cloned().unwrap_or(Value::Null),
        data: spec.get("data").cloned().unwrap_or(Value::Null),
        note: r.get::<_, Option<String>>("note")?.unwrap_or_default(),
        dirty: r.get::<_, Option<i64>>("dirty")?.unwrap_or(0) != 0,
        error: r.get::<_, Option<String>>("error")?.unwrap_or_default(),
        result_dir: r.get("result_dir")?,
        summary: summary.and_then(|s| serde_json::from_str(&s).ok()),
        study_id: r.get("study_id")?,
    })
}

const COLS: &str =
    "run_id, strategy_id, status, created_ts, finished_ts, spec_json, note, dirty, error, result_dir, summary_json, study_id";

fn poll_db() {
    let selected = with(|g| g.selected.clone()).flatten();
    let path = db_path();
    if !path.exists() {
        with(|g| {
            g.db_err = "研究库还没有记录（第一次运行后会出现）".into();
            g.runs.clear();
        });
        return;
    }
    let conn = match rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(c) => c,
        Err(e) => {
            with(|g| g.db_err = format!("打不开研究库：{e}"));
            return;
        }
    };
    let runs: Vec<RunRow> = match &selected {
        Some(id) => conn
            .prepare(&format!(
                "SELECT {COLS} FROM runs WHERE strategy_id=? AND study_id IS NULL ORDER BY created_ts DESC LIMIT 50"
            ))
            .and_then(|mut st| st.query_map([id], parse_row)?.collect())
            .unwrap_or_default(),
        None => vec![],
    };
    // 每个策略最近一次完成的运行（策略库列表用）
    let latest: HashMap<String, RunRow> = conn
        .prepare(&format!(
            "SELECT {COLS} FROM runs r WHERE status='done' AND study_id IS NULL AND created_ts = \
             (SELECT MAX(created_ts) FROM runs x WHERE x.strategy_id=r.strategy_id AND x.status='done' AND x.study_id IS NULL)"
        ))
        .and_then(|mut st| st.query_map([], parse_row)?.collect::<rusqlite::Result<Vec<_>>>())
        .unwrap_or_default()
        .into_iter()
        .map(|r| (r.strategy_id.clone(), r))
        .collect();
    // 优化任务（studies 表在第一次优化之前不存在：查不到就当空）
    let studies: Vec<StudyRow> = match &selected {
        Some(id) => conn
            .prepare("SELECT * FROM studies WHERE strategy_id=? ORDER BY created_ts DESC LIMIT 30")
            .and_then(|mut st| st.query_map([id], opt::parse_study)?.collect())
            .unwrap_or_default(),
        None => vec![],
    };
    with(|g| {
        // 用户在轮询期间换了策略：这批结果作废
        if g.selected == selected {
            g.runs = runs;
            g.studies = studies;
        }
        g.latest = latest;
        g.db_err.clear();
    });
}

/// 优化试验不在运行记录列表里：点它时直接查研究库（只读、单行）。
fn result_dir_of(run_id: &str) -> Option<String> {
    let conn = rusqlite::Connection::open_with_flags(db_path(), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    conn.query_row("SELECT result_dir FROM runs WHERE run_id=?", [run_id], |r| r.get::<_, Option<String>>(0)).ok().flatten()
}

// ── 参数表单 → RunSpec.params ──────────────────────────────────────────

/// 表单里的一个缺省值怎么显示。
pub fn default_text(p: &Param) -> String {
    match &p.default {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Number(n) => {
            // 1.0 显示成 1.0（不是 1），让人一眼看出是浮点
            if p.kind == "number" && n.is_i64() { format!("{}.0", n) } else { n.to_string() }
        }
        v => v.to_string(),
    }
}

/// 表单 → 参数对象。返回（**和缺省不同的**参数、逐字段错误、改动过的字段名）。
/// 只下发改过的参数：研究库里一眼看出这次跑的是哪几个参数的改动。
pub fn build_params(params: &[Param], form: &BTreeMap<String, String>) -> (serde_json::Map<String, Value>, BTreeMap<String, String>, Vec<String>) {
    let mut out = serde_json::Map::new();
    let mut errs = BTreeMap::new();
    let mut changed = vec![];
    for p in params {
        let Some(raw) = form.get(&p.name) else { continue };
        let raw = raw.trim();
        let v: Result<Value, String> = if raw.is_empty() {
            if p.nullable { Ok(Value::Null) } else { Err("不能为空".into()) }
        } else {
            match p.kind.as_str() {
                "integer" => raw.parse::<i64>().map(Value::from).map_err(|_| "要整数".into()),
                "number" => raw
                    .parse::<f64>()
                    .ok()
                    .and_then(|f| serde_json::Number::from_f64(f).map(Value::Number))
                    .ok_or_else(|| "要数字".into()),
                "boolean" => match raw {
                    "true" => Ok(Value::Bool(true)),
                    "false" => Ok(Value::Bool(false)),
                    _ => Err("要 true / false".into()),
                },
                "array" | "object" => serde_json::from_str(raw).map_err(|_| "要 JSON（如 [0, 1, 2]）".into()),
                _ => Ok(Value::String(raw.to_string())),
            }
        };
        let v = match v {
            Ok(v) => v,
            Err(e) => {
                errs.insert(p.name.clone(), e);
                continue;
            }
        };
        if let Some(x) = v.as_f64() {
            if let Some(lo) = p.minimum
                && x < lo
            {
                errs.insert(p.name.clone(), format!("不能小于 {lo}"));
                continue;
            }
            if let Some(hi) = p.maximum
                && x > hi
            {
                errs.insert(p.name.clone(), format!("不能大于 {hi}"));
                continue;
            }
        }
        if let Some(ch) = &p.choices
            && !v.is_null()
            && !ch.contains(&v)
        {
            errs.insert(p.name.clone(), "不在可选值里".into());
            continue;
        }
        if !same_value(&v, &p.default) {
            changed.push(p.name.clone());
            out.insert(p.name.clone(), v);
        }
    }
    (out, errs, changed)
}

/// 缺省值比较：数字按数值比（`1` 与 `1.0` 相同），数组与元组的 JSON 形态一致。
fn same_value(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => (x - y).abs() < 1e-12,
        _ => a == b,
    }
}

fn select_in(g: &mut St, id: &str) {
    g.selected = Some(id.to_string());
    g.runs.clear();
    g.studies.clear();
    g.picked_study = None;
    g.picked_run = None;
    g.msg.clear();
    reset_form_in(g);
}

fn reset_form_in(g: &mut St) {
    let Some(e) = g.selected.as_ref().and_then(|id| g.catalog.iter().find(|e| &e.id == id)).cloned() else {
        return;
    };
    g.form = e.params.iter().map(|p| (p.name.clone(), default_text(p))).collect();
    g.opt = opt::init_form(&e.params);
    g.start = e.bt("start");
    g.end = e.bt("end");
    g.note.clear();
}

/// 当前表单 → RunSpec（与 `factory.lab.spec.RunSpec` 同形）。窗口与策略声明相同就不下发。
pub fn spec_from(e: &Entry, form: &BTreeMap<String, String>, start: &str, end: &str, note: &str) -> Result<Value, String> {
    let (params, errs, _) = build_params(&e.params, form);
    if !errs.is_empty() {
        return Err(format!("参数有误：{}", errs.iter().map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join("；")));
    }
    let data = window_override(e, start, end)?;
    let mut spec = json!({ "strategy": e.id, "params": params });
    if let Some(d) = data {
        spec["data"] = d;
    }
    if !note.trim().is_empty() {
        spec["note"] = Value::String(note.trim().into());
    }
    Ok(spec)
}

/// 回测窗口覆盖：与策略 BACKTEST 声明相同的不下发；全相同返回 None。
pub fn window_override(e: &Entry, start: &str, end: &str) -> Result<Option<Value>, String> {
    let mut data = serde_json::Map::new();
    for (k, v) in [("start", start.trim()), ("end", end.trim())] {
        if v.is_empty() {
            continue;
        }
        if chrono::NaiveDate::parse_from_str(v, "%Y-%m-%d").is_err() {
            return Err(format!("{k} 要 YYYY-MM-DD，收到 {v}"));
        }
        if v != e.bt(k) {
            data.insert(k.into(), Value::String(v.into()));
        }
    }
    if !start.trim().is_empty() && !end.trim().is_empty() && end.trim() < start.trim() {
        return Err("结束日期早于开始日期".into());
    }
    Ok((!data.is_empty()).then_some(Value::Object(data)))
}

// ── 控制通道：一行 JSON 进、一行 JSON 出 ───────────────────────────────

fn control(cmd: &Value) -> Result<Value, String> {
    control_at(&control_sock(), cmd)
}

fn control_at(sock: &std::path::Path, cmd: &Value) -> Result<Value, String> {
    let mut s = std::os::unix::net::UnixStream::connect(sock)
        .map_err(|e| format!("连不上 ws-control（{}）：{e}——在「资源」页确认它在运行", sock.display()))?;
    s.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let line = format!("{}\n", cmd);
    s.write_all(line.as_bytes()).map_err(|e| format!("发送失败：{e}"))?;
    let mut resp = String::new();
    BufReader::new(s).read_line(&mut resp).map_err(|e| format!("读应答失败：{e}"))?;
    serde_json::from_str(&resp).map_err(|e| format!("应答不是 JSON：{e}"))
}

fn send_async(cmd: Value, what: &'static str) {
    with(|g| {
        g.sending = true;
        g.msg = format!("{what}…");
    });
    super::spawn_named("ws-stratsend", move || {
        let r = control(&cmd);
        with(|g| {
            g.sending = false;
            g.msg = match r {
                Ok(v) if v.get("status").and_then(Value::as_str) == Some("ok") => {
                    let id = v.get("run_id").and_then(Value::as_str).unwrap_or_default();
                    if id.is_empty() { format!("{what}：完成") } else { format!("{what}：{id}") }
                }
                Ok(v) => format!("{what}失败：{}", v.get("message").and_then(Value::as_str).unwrap_or("未知错误")),
                Err(e) => format!("{what}失败：{e}"),
            };
        });
        KICK.store(true, Ordering::Relaxed);
    });
}

// ── 消息处理 ───────────────────────────────────────────────────────────

pub fn handle(m: ScMsg) {
    match m {
        ScMsg::Refresh => {
            load_catalog();
            KICK.store(true, Ordering::Relaxed);
        }
        ScMsg::Select(id) => {
            with(|g| select_in(g, &id));
            KICK.store(true, Ordering::Relaxed);
        }
        ScMsg::Search(s) => {
            with(|g| g.search = s);
        }
        ScMsg::Verdict(v) => {
            with(|g| g.verdict = v);
        }
        ScMsg::Field(k, v) => {
            with(|g| {
                g.form.insert(k, v);
            });
        }
        ScMsg::Start(v) => {
            with(|g| g.start = v);
        }
        ScMsg::End(v) => {
            with(|g| g.end = v);
        }
        ScMsg::Note(v) => {
            with(|g| g.note = v);
        }
        ScMsg::ResetParams => {
            with(reset_form_in);
        }
        ScMsg::RunFull => {
            let spec = with(|g| {
                let e = g.selected.as_ref().and_then(|id| g.catalog.iter().find(|e| &e.id == id)).cloned()?;
                Some(spec_from(&e, &g.form, &g.start, &g.end, &g.note))
            })
            .flatten();
            match spec {
                Some(Ok(spec)) => send_async(json!({"cmd": "run_spec", "spec": spec}), "发起回测"),
                Some(Err(e)) => {
                    with(|g| g.msg = e);
                }
                None => {}
            }
        }
        ScMsg::Stop(run_id) => send_async(json!({"cmd": "stop", "run_id": run_id}), "停止"),
        ScMsg::PickRun(run_id) => {
            let dir = with(|g| {
                g.picked_run = Some(run_id.clone());
                g.runs.iter().find(|r| r.run_id == run_id).and_then(|r| r.result_dir.clone())
            })
            .flatten()
            .or_else(|| result_dir_of(&run_id));
            // 右侧「回测结果」面板钉在这次运行上（没有结果目录的运行——失败 / 进行中——不钉）
            if let Some(d) = dir {
                super::backtest_readout::pin(Some(PathBuf::from(d)));
            }
        }
        ScMsg::FollowLatest => {
            with(|g| g.picked_run = None);
            super::backtest_readout::pin(None);
        }
        ScMsg::Tab(t) => {
            with(|g| g.tab = t);
        }
        ScMsg::OptToggle(k) => {
            with(|g| {
                if let Some(f) = g.opt.fields.get_mut(&k) {
                    f.on = !f.on;
                }
            });
        }
        ScMsg::OptLow(k, v) => {
            with(|g| {
                if let Some(f) = g.opt.fields.get_mut(&k) {
                    f.low = v;
                }
            });
        }
        ScMsg::OptHigh(k, v) => {
            with(|g| {
                if let Some(f) = g.opt.fields.get_mut(&k) {
                    f.high = v;
                }
            });
        }
        ScMsg::OptStep(k, v) => {
            with(|g| {
                if let Some(f) = g.opt.fields.get_mut(&k) {
                    f.step = v;
                }
            });
        }
        ScMsg::OptSampler(v) => {
            with(|g| g.opt.sampler = v);
        }
        ScMsg::OptTrials(v) => {
            with(|g| g.opt.trials = v);
        }
        ScMsg::OptWorkers(v) => {
            with(|g| g.opt.workers = v);
        }
        ScMsg::OptObjective(v) => {
            with(|g| g.opt.objective = v);
        }
        ScMsg::StartStudy => {
            let spec = with(|g| {
                let e = g.selected.as_ref().and_then(|id| g.catalog.iter().find(|e| &e.id == id)).cloned()?;
                let (fixed, errs, _) = build_params(&e.params, &g.form);
                if !errs.is_empty() {
                    return Some(Err("先改正标红的参数".to_string()));
                }
                Some(window_override(&e, &g.start, &g.end).and_then(|d| opt::study_spec(&e, &g.opt, &fixed, d, &g.note)))
            })
            .flatten();
            match spec {
                Some(Ok(spec)) => {
                    with(|g| g.tab = Tab::Studies);
                    send_async(json!({"cmd": "run_study", "spec": spec}), "发起优化");
                }
                Some(Err(e)) => {
                    with(|g| g.msg = e);
                }
                None => {}
            }
        }
        ScMsg::StopStudy(id) => send_async(json!({"cmd": "stop", "run_id": id}), "停止优化"),
        ScMsg::PickStudy(id) => {
            with(|g| g.picked_study = Some(id));
        }
        ScMsg::OpenSource => {
            let path = with(|g| {
                g.selected.as_ref().and_then(|id| g.catalog.iter().find(|e| &e.id == id)).map(|e| e.path.clone())
            })
            .flatten();
            if let Some(p) = path {
                let full = super::paths::repo_root().join(p);
                let r = wealthspring_ui_tokens::bridge::send(
                    wealthspring_ui_tokens::bridge::Peer::Studio,
                    &wealthspring_ui_tokens::bridge::UiCommand::OpenFile { path: full.display().to_string(), line: None },
                );
                with(|g| g.msg = r.unwrap_or_else(|e| format!("打开失败：{e}")));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(name: &str, kind: &str, default: Value) -> Param {
        Param { name: name.into(), kind: kind.into(), default, ..Default::default() }
    }

    #[test]
    fn only_changed_params_are_sent_and_numbers_compare_by_value() {
        let ps = vec![
            Param { minimum: Some(0.0), maximum: Some(6.0), ..p("risk_level", "integer", json!(3)) },
            p("leg_z", "number", json!(1.0)),
            p("preset", "string", json!("icvt_high")),
        ];
        let form: BTreeMap<String, String> =
            [("risk_level", "1"), ("leg_z", "1"), ("preset", "icvt_high")].iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        let (out, errs, changed) = build_params(&ps, &form);
        assert!(errs.is_empty());
        assert_eq!(Value::Object(out), json!({"risk_level": 1}), "1 与 1.0 相同、未改的 preset 不下发");
        assert_eq!(changed, vec!["risk_level".to_string()]);
    }

    #[test]
    fn bad_values_are_reported_per_field() {
        let ps = vec![
            Param { minimum: Some(0.0), maximum: Some(6.0), ..p("risk_level", "integer", json!(3)) },
            Param { choices: Some(vec![json!("a"), json!("b")]), ..p("mode", "string", json!("a")) },
            p("slots", "array", json!([0, 1])),
            Param { nullable: true, ..p("offset", "integer", Value::Null) },
        ];
        let form: BTreeMap<String, String> = [("risk_level", "9"), ("mode", "c"), ("slots", "[0,"), ("offset", "")]
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        let (_, errs, _) = build_params(&ps, &form);
        assert_eq!(errs.len(), 3, "{errs:?}");
        assert!(errs["risk_level"].contains("不能大于"));
        assert!(!errs.contains_key("offset"), "可空字段留空 = null，合法");
    }

    #[test]
    fn control_round_trip_is_one_json_line_each_way() {
        let dir = std::env::temp_dir().join(format!("ws-sc-ctl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("control.sock");
        let l = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let srv = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let mut line = String::new();
            BufReader::new(&s).read_line(&mut line).unwrap();
            let got: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(got["cmd"], "run_spec");
            (&s).write_all(b"{\"status\":\"ok\",\"run_id\":\"R-1-1\",\"message\":\"x\"}\n").unwrap();
        });
        let r = control_at(&sock, &json!({"cmd": "run_spec", "spec": {"strategy": "a.b"}})).unwrap();
        assert_eq!(r["run_id"], "R-1-1");
        srv.join().unwrap();
        assert!(control_at(&dir.join("none.sock"), &json!({})).unwrap_err().contains("连不上 ws-control"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spec_omits_window_equal_to_declaration() {
        let e = Entry {
            id: "quantum_queen.v43".into(),
            backtest: Some(json!({"start": "2026-09-01", "end": "2026-09-30"})),
            params: vec![p("risk_level", "integer", json!(3))],
            ..Default::default()
        };
        let form: BTreeMap<String, String> = [("risk_level".to_string(), "1".to_string())].into();
        let s = spec_from(&e, &form, "2026-09-01", "2026-09-10", " 降风险 ").unwrap();
        assert_eq!(s, json!({"strategy": "quantum_queen.v43", "params": {"risk_level": 1},
                              "data": {"end": "2026-09-10"}, "note": "降风险"}));
        assert!(spec_from(&e, &form, "2026-09-10", "2026-09-01", "").is_err(), "结束早于开始");
        assert!(spec_from(&e, &form, "2026/09/10", "", "").is_err(), "日期格式");
    }
}
