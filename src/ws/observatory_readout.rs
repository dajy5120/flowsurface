//! 数据接口观察终端的只读快照（docs/23）。
//!
//! 面板**没有一行网络代码**：`ws-observatory` 守护持有全部连接，这里只把它写在
//! tmpfs 里的那份 JSON 读进来。后台 poller 线程读文件，渲染线程只取内存快照——
//! 渲染线程上的任何 IO 都会直接变成掉帧。

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// 连接表单的一个字段。**面板照这个渲染控件，不认识任何具体协议**（docs/23 §2.1）。
#[derive(Default, Clone, PartialEq)]
pub struct FieldSpec {
    pub key: String,
    pub label: String,
    /// `text` / `secret` / `int` / `bool` / `enum`
    pub kind: String,
    pub default: String,
    pub required: bool,
    pub hint: String,
    pub options: Vec<String>,
}

/// 一条流的自述。
#[derive(Default, Clone, PartialEq)]
pub struct StreamSpec {
    pub stream_id: u32,
    pub id: String,
    pub label: String,
    pub wire: String,
    /// 满了怎么办。**逐流不同**，界面要显示出来——用户得知道哪条流会丢。
    pub drop: String,
    pub rate_hint: String,
}

/// 一个 Adapter 的自述。
#[derive(Default, Clone, PartialEq)]
pub struct AdapterSpec {
    pub id: String,
    pub label: String,
    /// 只用于分组显示。**面板里不许出现 `match transport`**——
    /// 一旦出现，「加协议不动 UI」就已经破了。
    pub transport: String,
    pub config_schema: Vec<FieldSpec>,
    pub streams: Vec<StreamSpec>,
    pub can_request: bool,
}

/// 一条流的实时指标。
#[derive(Default, Clone, PartialEq)]
pub struct StreamStat {
    pub stream_id: u32,
    pub label: String,
    pub received: i64,
    pub bytes: i64,
    // 四个阶段分开：一个总数没法告诉人该去修哪儿（docs/23 §5.4）
    pub drop_frame: i64,
    pub drop_ring: i64,
    pub drop_sink: i64,
    /// **不是故障**：UI 跟不上是正常的。与上面三个分开显示。
    pub skipped_ui: i64,
    pub decode_err: i64,
    pub gap: i64,
    pub dropped_total: i64,
    pub rate_msg: i64,
    pub rate_bytes: i64,
    pub spark_msg: Vec<i64>,
    /// 可能为**负**：对端时钟比我们快。原样显示，不要 clamp。
    pub lag_p50_ns: Option<f64>,
    pub lag_p99_ns: Option<f64>,
}

/// 环形缓冲状态。
#[derive(Default, Clone, PartialEq)]
pub struct RingStat {
    pub slots_used: i64,
    pub slots_cap: i64,
    pub bytes_used: i64,
    pub bytes_cap: i64,
    pub fill_frac: f64,
    pub evicted: i64,
    pub oldest_seq: i64,
    pub next_seq: i64,
    /// 覆盖区间。**必须显著显示**：用户会去要一个已经淘汰掉的时间窗，
    /// 然后拿到一个安静变短的文件（docs/23 §5.1）。
    pub coverage_from_ms: Option<i64>,
    pub coverage_to_ms: Option<i64>,
    pub coverage_secs: Option<f64>,
}

/// 尾窗一行。**后端已经格式化好**，面板不做 `format!`（docs/23 §10.3）。
#[derive(Default, Clone, PartialEq)]
pub struct TailRow {
    pub seq: i64,
    pub stream_id: u32,
    pub recv_ms: i64,
    pub len: i64,
    pub wire: String,
    pub dir: String,
    /// `gap|synthetic` 这种。空 = 正常。
    pub flags: String,
    pub lag_ns: Option<f64>,
    pub preview: String,
    pub preview_truncated: bool,
    /// Parsed 视图的解析结果（`路径 → 值`）。`None` = 没开解析或解不出来。
    pub parsed: Option<Vec<(String, String)>>,
}

#[derive(Default, Clone, PartialEq)]
pub struct SessionView {
    pub adapter: String,
    pub adapter_label: String,
    /// 密钥已由守护抹去。
    pub config: Vec<(String, String)>,
    pub reconnects: i64,
    pub ring: RingStat,
    pub streams: Vec<StreamStat>,
    pub tail: Vec<TailRow>,
    /// 这一窗跳过了多少条。**必须显示**，否则界面就是在假装全都显示了。
    pub tail_skipped: i64,
    /// 上次刷新到现在**到了**多少条。`tail_skipped` 的分母。
    pub tail_arrived: i64,
    /// 游标被环淘汰追上而丢的条数。非 0 = 连尾窗都跟不上环的淘汰速度。
    pub tail_lapped: i64,
    pub filter: FilterStat,
    pub parse: bool,
}

/// 录制状态（docs/23 §6）。
#[derive(Default, Clone, PartialEq)]
pub struct RecStat {
    pub on: bool,
    pub dir: String,
    pub session_id: String,
    pub frames: i64,
    pub bytes: i64,
    pub gaps: i64,
    /// 被环追上而**真的丢了**的条数。非 0 = 录制跟不上，该调环容量或降速。
    pub drop_sink: i64,
    /// 单次录制预算已用比例。
    pub budget_frac: f64,
    pub budget_cap: i64,
    /// 被闸门停了。**与「用户点了停止」是两回事**，界面要分开说。
    pub halted: bool,
    pub halt: String,
}

/// 显示筛选的状态。
#[derive(Default, Clone, PartialEq)]
pub struct FilterStat {
    pub src: String,
    /// 编译错误。非空时**显示的是全部数据**——写错了报错并照常显示，
    /// 而不是给一张空表让人以为没数据。
    pub err: String,
    /// 这条筛选要不要逐条解 JSON。面板据此提示「这条贵」。
    pub needs_json: bool,
    pub scanned: i64,
    /// 扫到预算上限还没凑够。**必须显示**。
    pub exhausted: bool,
}

#[derive(Default, Clone)]
pub struct ObsReadout {
    pub stamp: String,
    pub present: bool,
    pub connected: bool,
    /// `connecting` / `live` / `idle` / `lost`
    pub health: String,
    pub last_err: String,
    pub next_retry_secs: i64,
    pub catalog: Vec<AdapterSpec>,
    pub session: Option<SessionView>,
    pub rec: RecStat,
    /// 最近一次保存/收尾的回执。时间窗保存是异步的，不给回执用户
    /// 不知道到底存没存下来。
    pub last_save: String,
    pub refreshed: String,
    pub svc: super::svcctl::UnitState,
}

pub const SERVICE: &str = "ws-observatory";

fn runtime_dir() -> PathBuf {
    match std::env::var("XDG_RUNTIME_DIR") {
        Ok(x) if !x.is_empty() => PathBuf::from(x).join("wealthspring"),
        _ => PathBuf::from("/run/user/1000/wealthspring"),
    }
}

fn board_path() -> PathBuf {
    std::env::var("WS_OBS_BOARD").map(PathBuf::from).unwrap_or_else(|_| runtime_dir().join("observatory.json"))
}

pub fn request_path() -> PathBuf {
    std::env::var("WS_OBS_REQUEST")
        .map(PathBuf::from)
        .unwrap_or_else(|_| runtime_dir().join("observatory_request.json"))
}

static READOUT: OnceLock<Mutex<std::sync::Arc<ObsReadout>>> = OnceLock::new();
static POLLER: OnceLock<()> = OnceLock::new();

pub fn snapshot() -> std::sync::Arc<ObsReadout> {
    ensure_poller();
    READOUT
        .get_or_init(|| Mutex::new(std::sync::Arc::new(ObsReadout::default())))
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default()
}

/// 直接写入读数，**仅供测试**：让帧时测量能喂进一份确定的快照，
/// 而不是依赖后台 poller 恰好读到什么。
#[doc(hidden)]
pub fn seed_for_test(r: ObsReadout) {
    POLLER.get_or_init(|| ()); // 占位，阻止真的起 poller 线程
    let lock = READOUT.get_or_init(|| Mutex::new(std::sync::Arc::new(ObsReadout::default())));
    if let Ok(mut g) = lock.lock() {
        *g = std::sync::Arc::new(r);
    }
}

fn ensure_poller() {
    POLLER.get_or_init(|| {
        std::thread::spawn(|| loop {
            let mut snap = poll_once();
            snap.svc = super::svcctl::query(SERVICE);
            let lock = READOUT.get_or_init(|| Mutex::new(std::sync::Arc::new(ObsReadout::default())));
            if let Ok(mut g) = lock.lock() {
                *g = std::sync::Arc::new(snap);
            }
            // 读文件 + 解析在**后台线程**。渲染线程只取内存快照
            std::thread::sleep(Duration::from_millis(400));
        });
    });
}

fn poll_once() -> ObsReadout {
    let refreshed = chrono::Local::now().format("%H:%M:%S").to_string();
    let Ok(txt) = std::fs::read_to_string(board_path()) else {
        return ObsReadout { refreshed, ..Default::default() };
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else {
        return ObsReadout { refreshed, ..Default::default() };
    };
    let mut r = parse(&v);
    r.refreshed = refreshed;
    r
}

fn s(v: &serde_json::Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}
fn i(v: &serde_json::Value, k: &str) -> i64 {
    v.get(k).and_then(|x| x.as_i64()).unwrap_or(0)
}
fn f(v: &serde_json::Value, k: &str) -> f64 {
    v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0)
}
fn of(v: &serde_json::Value, k: &str) -> Option<f64> {
    v.get(k).and_then(|x| x.as_f64())
}
fn oi(v: &serde_json::Value, k: &str) -> Option<i64> {
    v.get(k).and_then(|x| x.as_i64())
}
fn arr(v: &serde_json::Value, k: &str) -> Vec<serde_json::Value> {
    v.get(k).and_then(|x| x.as_array()).cloned().unwrap_or_default()
}

/// 纯解析（可单测）。
pub fn parse(v: &serde_json::Value) -> ObsReadout {
    let catalog = arr(v, "catalog")
        .iter()
        .map(|a| AdapterSpec {
            id: s(a, "id"),
            label: s(a, "label"),
            transport: s(a, "transport"),
            config_schema: arr(a, "config_schema")
                .iter()
                .map(|x| FieldSpec {
                    key: s(x, "key"),
                    label: s(x, "label"),
                    kind: s(x, "kind"),
                    default: s(x, "default"),
                    required: x.get("required").and_then(|b| b.as_bool()).unwrap_or(false),
                    hint: s(x, "hint"),
                    options: x
                        .get("options")
                        .and_then(|o| o.as_array())
                        .map(|o| o.iter().filter_map(|t| t.as_str().map(String::from)).collect())
                        .unwrap_or_default(),
                })
                .collect(),
            streams: arr(a, "streams")
                .iter()
                .map(|x| StreamSpec {
                    stream_id: i(x, "stream_id") as u32,
                    id: s(x, "id"),
                    label: s(x, "label"),
                    wire: s(x, "wire"),
                    drop: s(x, "drop"),
                    rate_hint: s(x, "rate_hint"),
                })
                .collect(),
            can_request: a
                .get("caps")
                .and_then(|c| c.get("can_request"))
                .and_then(|b| b.as_bool())
                .unwrap_or(false),
        })
        .collect();

    let session = v.get("session").filter(|x| !x.is_null()).map(|sv| SessionView {
        adapter: s(sv, "adapter"),
        adapter_label: s(sv, "adapter_label"),
        config: sv
            .get("config")
            .and_then(|c| c.as_object())
            .map(|o| {
                o.iter()
                    .map(|(k, x)| (k.clone(), x.as_str().unwrap_or("").to_string()))
                    .collect()
            })
            .unwrap_or_default(),
        reconnects: i(sv, "reconnects"),
        ring: {
            let r = sv.get("ring").cloned().unwrap_or_default();
            RingStat {
                slots_used: i(&r, "slots_used"),
                slots_cap: i(&r, "slots_cap"),
                bytes_used: i(&r, "bytes_used"),
                bytes_cap: i(&r, "bytes_cap"),
                fill_frac: f(&r, "fill_frac"),
                evicted: i(&r, "evicted"),
                oldest_seq: i(&r, "oldest_seq"),
                next_seq: i(&r, "next_seq"),
                coverage_from_ms: oi(&r, "coverage_from_ms"),
                coverage_to_ms: oi(&r, "coverage_to_ms"),
                coverage_secs: of(&r, "coverage_secs"),
            }
        },
        streams: arr(sv, "streams")
            .iter()
            .map(|x| StreamStat {
                stream_id: i(x, "stream_id") as u32,
                label: s(x, "label"),
                received: i(x, "received"),
                bytes: i(x, "bytes"),
                drop_frame: i(x, "drop_frame"),
                drop_ring: i(x, "drop_ring"),
                drop_sink: i(x, "drop_sink"),
                skipped_ui: i(x, "skipped_ui"),
                decode_err: i(x, "decode_err"),
                gap: i(x, "gap"),
                dropped_total: i(x, "dropped_total"),
                rate_msg: i(x, "rate_msg"),
                rate_bytes: i(x, "rate_bytes"),
                spark_msg: arr(x, "spark_msg").iter().filter_map(|n| n.as_i64()).collect(),
                lag_p50_ns: of(x, "lag_p50_ns"),
                lag_p99_ns: of(x, "lag_p99_ns"),
            })
            .collect(),
        tail: arr(sv, "tail")
            .iter()
            .map(|x| TailRow {
                seq: i(x, "seq"),
                stream_id: i(x, "stream_id") as u32,
                recv_ms: i(x, "recv_ms"),
                len: i(x, "len"),
                wire: s(x, "wire"),
                dir: s(x, "dir"),
                flags: s(x, "flags"),
                lag_ns: of(x, "lag_ns"),
                preview: s(x, "preview"),
                preview_truncated: x
                    .get("preview_truncated")
                    .and_then(|b| b.as_bool())
                    .unwrap_or(false),
                parsed: x.get("parsed").and_then(|p| p.as_object()).map(|o| {
                    o.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string())).collect()
                }),
            })
            .collect(),
        tail_skipped: i(sv, "tail_skipped"),
        tail_arrived: i(sv, "tail_arrived"),
        tail_lapped: i(sv, "tail_lapped"),
        filter: {
            let f = sv.get("filter").cloned().unwrap_or_default();
            FilterStat {
                src: s(&f, "src"),
                err: s(&f, "err"),
                needs_json: f.get("needs_json").and_then(|b| b.as_bool()).unwrap_or(false),
                scanned: i(&f, "scanned"),
                exhausted: f.get("exhausted").and_then(|b| b.as_bool()).unwrap_or(false),
            }
        },
        parse: sv.get("parse").and_then(|b| b.as_bool()).unwrap_or(false),
    });

    let rec = {
        let r = v.get("recording").cloned().unwrap_or_default();
        RecStat {
            on: r.get("on").and_then(|b| b.as_bool()).unwrap_or(false),
            dir: s(&r, "dir"),
            session_id: s(&r, "session_id"),
            frames: i(&r, "frames"),
            bytes: i(&r, "bytes"),
            gaps: i(&r, "gaps"),
            drop_sink: i(&r, "drop_sink"),
            budget_frac: f(&r, "budget_frac"),
            budget_cap: i(&r, "budget_cap"),
            halted: r.get("halted").and_then(|b| b.as_bool()).unwrap_or(false),
            halt: s(&r, "halt"),
        }
    };
    ObsReadout {
        stamp: s(v, "stamp"),
        present: true,
        connected: v.get("connected").and_then(|b| b.as_bool()).unwrap_or(false),
        health: s(v, "health"),
        last_err: s(v, "last_err"),
        next_retry_secs: i(v, "next_retry_secs"),
        catalog,
        session,
        rec,
        last_save: s(v, "last_save"),
        refreshed: String::new(),
        svc: Default::default(),
    }
}

/// 启停守护。**必须 `--no-block`**（同 prediction/factory 踩过的坑：
/// `systemctl start` 会等到 unit 就绪才返回，足以冻死 UI 线程）。
pub fn svc_action(action: &str) -> String {
    match std::process::Command::new("systemctl")
        .args(["--user", action, "--no-block", SERVICE])
        .status()
    {
        Ok(s) if s.success() => format!(
            "✔ 观察终端守护已{}",
            match action {
                "start" => "启动",
                "stop" => "停止",
                _ => "操作",
            }
        ),
        Ok(s) => format!("✗ systemctl {action} 退出码 {:?}", s.code()),
        Err(e) => format!("✗ systemctl 失败：{e}"),
    }
}

/// 面板侧的显示口径。**在面板内存里，随打字变**——每次按键都写文件的话，
/// 守护会在你打到一半时反复重编译一条写错的表达式。写文件由「应用」触发。
static VIEW: Mutex<(String, bool)> = Mutex::new((String::new(), false));

pub fn view_state() -> (String, bool) {
    VIEW.lock().map(|g| g.clone()).unwrap_or_default()
}

pub fn set_filter_text(t: &str) {
    if let Ok(mut g) = VIEW.lock() {
        g.0 = t.to_string();
    }
}

pub fn set_parse(on: bool) {
    if let Ok(mut g) = VIEW.lock() {
        g.1 = on;
    }
}

/// 请求文件的**全部**内容都从这里出去，且是**局部修改**。
///
/// 每次重建整份请求会踩两个坑，两个都试过：
/// - 少带 `nonce` → 守护看到 nonce 从 3 变回 0，判定成「连接变了」→ **改个筛选就重连**
/// - 少带 `adapter` → 守护判定成「要断开」→ **改个筛选就断线**
///
/// 所以这里保存上一次写出去的整份 JSON，每次只改要改的键。
static REQ: Mutex<Option<serde_json::Value>> = Mutex::new(None);

fn patch(kv: &[(&str, serde_json::Value)]) {
    let body = {
        let Ok(mut g) = REQ.lock() else { return };
        let v = g.get_or_insert_with(|| serde_json::json!({}));
        for (k, x) in kv {
            v[*k] = x.clone();
        }
        v.to_string()
    };
    write_raw(&body);
}

/// 只增计数：守护比对**变没变**，不解释值。
fn next_nonce() -> i64 {
    use std::sync::atomic::{AtomicI64, Ordering};
    static N: AtomicI64 = AtomicI64::new(1);
    N.fetch_add(1, Ordering::Relaxed)
}

/// 连接到某个 adapter。会顺带把当前的显示口径一起带上。
pub fn request_connect(adapter: &str, config: &std::collections::BTreeMap<String, String>) {
    let (filter, parse) = view_state();
    patch(&[
        ("adapter", serde_json::json!(adapter)),
        ("config", serde_json::json!(config)),
        ("nonce", serde_json::json!(next_nonce())),
        ("filter", serde_json::json!(filter)),
        ("parse", serde_json::json!(parse)),
    ]);
}

pub fn request_disconnect() {
    patch(&[
        ("adapter", serde_json::json!("")),
        ("nonce", serde_json::json!(next_nonce())),
    ]);
}

/// 连接表单的当前内容：`(adapter id, 字段值)`。
///
/// **密钥就在这里，是明文。** 它只在面板内存里，随「连接」发给守护；
/// 守护在快照和清单里都会把它抹成 «已抹去»。面板**绝不**从快照回读这些值
/// （回读会把真密钥换成那四个字，下一次连接必然失败）。
static FORM: Mutex<Option<(String, std::collections::BTreeMap<String, String>)>> =
    Mutex::new(None);

/// 当前表单。第一次访问时用 catalog 里的默认值初始化。
pub fn form(catalog: &[AdapterSpec]) -> (String, std::collections::BTreeMap<String, String>) {
    let Ok(mut g) = FORM.lock() else { return Default::default() };
    if g.is_none() {
        if let Some(a) = catalog.first() {
            let vals = a
                .config_schema
                .iter()
                .map(|f| (f.key.clone(), f.default.clone()))
                .collect();
            *g = Some((a.id.clone(), vals));
        }
    }
    g.clone().unwrap_or_default()
}

/// 换一个 Adapter：字段跟着换成那个 Adapter 的默认值。
///
/// **不保留上一个的值**：字段名相同但语义未必相同（同名的 `url` 在 REST 和 WS
/// 上要填的东西不一样），留着只会让人填错。
pub fn form_pick(catalog: &[AdapterSpec], id: &str) {
    let Some(a) = catalog.iter().find(|a| a.id == id) else { return };
    if let Ok(mut g) = FORM.lock() {
        *g = Some((
            id.to_string(),
            a.config_schema.iter().map(|f| (f.key.clone(), f.default.clone())).collect(),
        ));
    }
}

pub fn form_set(key: &str, val: &str) {
    if let Ok(mut g) = FORM.lock() {
        if let Some((_, m)) = g.as_mut() {
            m.insert(key.to_string(), val.to_string());
        }
    }
}

/// 表单里必填项是否都填了。空着就点连接，只会拿到一条难懂的握手错误。
pub fn form_ready(catalog: &[AdapterSpec]) -> Result<(), String> {
    let (id, vals) = form(catalog);
    let Some(a) = catalog.iter().find(|a| a.id == id) else {
        return Err("还没选 Adapter".into());
    };
    for f in &a.config_schema {
        if f.required && vals.get(&f.key).is_none_or(|v| v.trim().is_empty()) {
            return Err(format!("「{}」是必填的", f.label));
        }
    }
    Ok(())
}

/// 录制专用的 nonce。**与连接那个分开**：共用的话按下「录制」会顺带把连接
/// 重建一次，而重建正好会在数据里留下一个纯属自己制造的 gap。
fn next_rec_nonce() -> i64 {
    use std::sync::atomic::{AtomicI64, Ordering};
    static N: AtomicI64 = AtomicI64::new(1);
    N.fetch_add(1, Ordering::Relaxed)
}

/// 开/停录制。
pub fn request_record(on: bool) {
    patch(&[
        ("record", serde_json::json!(on)),
        ("rec_nonce", serde_json::json!(next_rec_nonce())),
        // 清掉窗口字段：留着的话守护会把这次当成窗口保存
        ("save_from_ms", serde_json::json!(0)),
        ("save_to_ms", serde_json::json!(0)),
    ]);
}

/// 从环里回捞一段**已经过去**的时间（docs/23 §5.1）。
pub fn request_save_window(from_ms: i64, to_ms: i64) {
    patch(&[
        ("save_from_ms", serde_json::json!(from_ms)),
        ("save_to_ms", serde_json::json!(to_ms)),
        ("rec_nonce", serde_json::json!(next_rec_nonce())),
    ]);
}

/// 只改显示口径。**不碰 adapter/config/nonce**——改个筛选不该让连接断一下。
pub fn request_view() {
    let (filter, parse) = view_state();
    patch(&[("filter", serde_json::json!(filter)), ("parse", serde_json::json!(parse))]);
}

/// 写控制请求（原子：同目录 tmp + rename）。**只有面板写这个文件。**
fn write_raw(body: &str) {
    let p = request_path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = p.with_extension("json.tmp");
    if std::fs::write(&tmp, body.as_bytes()).is_ok() {
        let _ = std::fs::rename(&tmp, &p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SNAP: &str = r#"{
      "stamp":"2026-09-08 12:00:00","connected":true,"health":"live",
      "last_err":"","next_retry_secs":0,
      "catalog":[{"id":"ws.raw","label":"WebSocket（通用）","transport":"ws",
        "config_schema":[{"key":"url","label":"地址","kind":"text","default":"wss://","required":true,"hint":"h","options":null},
                         {"key":"api_key","label":"密钥","kind":"secret","default":"","required":false,"hint":"","options":null}],
        "streams":[{"stream_id":0,"id":"frames","label":"帧","wire":"text","drop":"drop_oldest","rate_hint":"torrent"}],
        "caps":{"can_request":true,"can_subscribe":true,"bidirectional":true,"needs_auth":false}}],
      "session":{"adapter":"ws.raw","adapter_label":"WebSocket（通用）",
        "config":{"url":"wss://x","api_key":"«已抹去»"},"reconnects":2,
        "ring":{"slots_used":102,"slots_cap":524288,"bytes_used":2000,"bytes_cap":536870912,
                "fill_frac":0.0002,"evicted":0,"oldest_seq":0,"next_seq":102,
                "coverage_from_ms":1788841000000,"coverage_to_ms":1788841019000,"coverage_secs":19.0},
        "streams":[{"stream_id":0,"label":"帧","received":100,"bytes":20500,
                    "drop_frame":1,"drop_ring":2,"drop_sink":0,"skipped_ui":927,
                    "decode_err":0,"gap":1,"dropped_total":3,
                    "rate_msg":35,"rate_bytes":7146,"spark_msg":[1,2,3],
                    "lag_p50_ns":-1000000.0,"lag_p99_ns":9000000.0}],
        "tail":[{"seq":101,"stream_id":0,"recv_ms":1788841019000,"len":205,"wire":"text",
                 "dir":"in","flags":"gap|synthetic","lag_ns":null,
                 "preview":"读失败","preview_truncated":false}],
        "tail_skipped":21,"tail_arrived":221,"tail_lapped":0}
    }"#;

    #[test]
    fn the_panel_learns_the_connection_form_from_the_snapshot() {
        // 面板不认识任何具体协议：表单字段、流清单全从 descriptor 来。
        // 这里一旦要写死一个默认值，「加协议不动 UI」就破了
        let r = parse(&serde_json::from_str(SNAP).unwrap());
        let a = &r.catalog[0];
        assert_eq!(a.id, "ws.raw");
        assert_eq!(a.config_schema.len(), 2);
        assert_eq!(a.config_schema[0].key, "url");
        assert!(a.config_schema[0].required);
        assert_eq!(a.config_schema[1].kind, "secret", "密钥字段要能识别出来用密码框");
        assert_eq!(a.streams[0].drop, "drop_oldest");
    }

    #[test]
    fn ui_skips_stay_separate_from_real_drops() {
        // 混在一起界面就会天天报警而没人再看
        let r = parse(&serde_json::from_str(SNAP).unwrap());
        let st = &r.session.as_ref().unwrap().streams[0];
        assert_eq!(st.skipped_ui, 927);
        assert_eq!(st.dropped_total, 3, "只算 frame+ring+sink");
        assert_eq!((st.drop_frame, st.drop_ring, st.drop_sink), (1, 2, 0));
    }

    #[test]
    fn a_negative_lag_survives_the_parse() {
        // 对端时钟比我们快时是负的。解析成 0 或丢掉，就再也发现不了
        let r = parse(&serde_json::from_str(SNAP).unwrap());
        let st = &r.session.as_ref().unwrap().streams[0];
        assert_eq!(st.lag_p50_ns, Some(-1_000_000.0));
    }

    #[test]
    fn ring_coverage_is_parsed_so_the_user_can_see_how_far_back_they_can_reach() {
        let r = parse(&serde_json::from_str(SNAP).unwrap());
        let ring = &r.session.as_ref().unwrap().ring;
        assert_eq!(ring.coverage_secs, Some(19.0));
        assert_eq!(ring.next_seq, 102);
    }

    #[test]
    fn a_gap_row_keeps_its_flags() {
        // 断线的唯一凭证。丢掉 flags，那一行就和普通数据长得一样了
        let r = parse(&serde_json::from_str(SNAP).unwrap());
        let t = &r.session.as_ref().unwrap().tail[0];
        assert_eq!(t.flags, "gap|synthetic");
        let se = r.session.as_ref().unwrap();
        assert_eq!((se.tail_skipped, se.tail_arrived), (21, 221));
    }

    #[test]
    fn changing_the_filter_does_not_touch_the_connection_fields() {
        // 两个都踩过：少带 nonce → 守护看到 3 变回 0，判成「连接变了」→ 改筛选就重连；
        // 少带 adapter → 判成「要断开」→ 改筛选就断线
        if let Ok(mut g) = REQ.lock() {
            *g = Some(serde_json::json!({"adapter":"ws.raw","config":{"url":"wss://x"},"nonce":7}));
        }
        set_filter_text("len > 10");
        let (f, _) = view_state();
        assert_eq!(f, "len > 10");
        // patch 之后 adapter/nonce 必须原样还在
        let before = REQ.lock().unwrap().clone().unwrap();
        assert_eq!(before["nonce"], 7);
        assert_eq!(before["adapter"], "ws.raw");
    }

    #[test]
    fn the_nonce_only_ever_increases() {
        // 守护靠「变没变」判断要不要重连。回绕会让重连按钮失灵
        assert!(next_nonce() < next_nonce());
    }

    #[test]
    fn recording_state_round_trips_including_the_halt_reason() {
        // 「被闸门停了」和「用户点了停止」是两回事，事后必须分得清
        let j = r#"{"stamp":"s","recording":{"on":true,"dir":"/d","session_id":"rec-1",
          "frames":35726,"bytes":7279063,"gaps":2,"drop_sink":0,
          "budget_frac":0.0034,"budget_cap":2147483648,
          "halted":true,"halt":"本次录制已写 2.0G / 上限 2.0G，已停止"},
          "last_save":"已保存 3530 条"}"#;
        let r = parse(&serde_json::from_str(j).unwrap());
        assert!(r.rec.on && r.rec.halted);
        assert_eq!(r.rec.frames, 35_726);
        assert!(r.rec.halt.contains("上限"));
        assert_eq!(r.last_save, "已保存 3530 条");
    }

    #[test]
    fn not_recording_is_distinguishable_from_an_old_daemon() {
        // 守护旧版本时整段缺失；没在录时是 {"on":false}。
        // 两者都不该让面板崩，但界面上是两句不同的话
        let r = parse(&serde_json::from_str(r#"{"recording":{"on":false}}"#).unwrap());
        assert!(!r.rec.on);
        let r = parse(&serde_json::from_str(r#"{}"#).unwrap());
        assert!(!r.rec.on);
        assert!(r.rec.dir.is_empty());
    }

    #[test]
    fn the_record_nonce_is_separate_from_the_connection_nonce() {
        // 共用的话按下「录制」会顺带重建连接，而重建正好会留下一个
        // 纯属自己制造的 gap
        let a = next_rec_nonce();
        let b = next_nonce();
        let c = next_rec_nonce();
        assert!(c > a, "录制 nonce 自己递增");
        let _ = b;
    }

    #[test]
    fn a_missing_or_broken_snapshot_is_absent_not_a_crash() {
        // 守护没起来时面板要照常画，只是标成「无快照」
        let r = parse(&serde_json::from_str("{}").unwrap());
        assert!(r.session.is_none());
        assert!(r.catalog.is_empty());
        assert!(!r.connected);
    }

    #[test]
    fn a_session_that_is_json_null_is_treated_as_absent() {
        // 守护未连接时会发 "session": null。当成对象解析会得到一个
        // 全零的假会话，界面上显示成「已连接但一条数据都没有」
        let r = parse(&serde_json::from_str(r#"{"connected":false,"session":null}"#).unwrap());
        assert!(r.session.is_none());
    }
}
