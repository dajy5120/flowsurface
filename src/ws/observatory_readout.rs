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
    /// 密钥已由守护抹去；`${环境变量名}` 是引用，会原样留着。
    pub config: Vec<(String, String)>,
    /// Adapter 报上来的链路事实：WS 有没有开 permessage-deflate 之类。
    /// 从数据本身看不出来（帧里存的是解压后的内容），只能由 Adapter 说。
    pub transport: Vec<(String, String)>,
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
    pub capture: CaptureStat,
    pub triggers: Vec<TrigStat>,
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

/// 一条触发规则的状态（docs/23 §7）。
#[derive(Default, Clone, PartialEq)]
pub struct TrigStat {
    pub name: String,
    /// 条件串。守护原样回发——面板删掉其中一条时要把其余的重发。
    pub cond: String,
    pub pre_roll_ms: i64,
    pub post_roll_ms: i64,
    pub cooldown_ms: i64,
    pub cap_per_hour: i64,
    pub fired: i64,
    /// **被闸门挡下来的次数**。它说明条件写得太宽——不显示的话，
    /// 用户看着条件明明命中却没录，会以为是条件写错了。
    pub blocked: i64,
    pub blocked_why: String,
    pub used_this_hour: i64,
    /// 正在录后半段。
    pub capturing: bool,
}

/// 捕获筛选的状态（docs/23 §8.1）。
#[derive(Default, Clone, PartialEq)]
pub struct CaptureStat {
    pub src: String,
    /// 被它筛掉的条数。**必须显示**——不显示的话用户看到条数少了会以为是丢包。
    pub dropped: i64,
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
    /// 密钥是明文填的还是写成 `${环境变量名}`。明文不阻止连接，但必须说——
    /// 请求文件是 tmpfs 上的普通文件，明文密钥就一直躺在那儿。
    pub secret_warn: String,
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
    // 测试改写。**必须有**：不改的话跑一遍测试就会把用户正在跑的那份
    // 请求文件冲掉——守护会立刻照着测试写的内容去重连
    #[cfg(test)]
    if let Some(p) = tests::req_override() {
        return p;
    }
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
        transport: sv
            .get("transport")
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
        capture: {
            let c = sv.get("capture").cloned().unwrap_or_default();
            CaptureStat { src: s(&c, "src"), dropped: i(&c, "dropped") }
        },
        triggers: arr(sv, "triggers")
            .iter()
            .map(|t| TrigStat {
                name: s(t, "name"),
                cond: s(t, "cond"),
                pre_roll_ms: i(t, "pre_roll_ms"),
                post_roll_ms: i(t, "post_roll_ms"),
                cooldown_ms: i(t, "cooldown_ms"),
                cap_per_hour: i(t, "cap_per_hour"),
                fired: i(t, "fired"),
                blocked: i(t, "blocked"),
                blocked_why: t
                    .get("blocked_why")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                used_this_hour: i(t, "used_this_hour"),
                capturing: t.get("capturing").and_then(|b| b.as_bool()).unwrap_or(false),
            })
            .collect(),
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
        secret_warn: s(v, "secret_warn"),
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
        // **基线从磁盘上的现状取，不是从空对象**。
        //
        // 面板启动时并不知道请求文件里已经有什么——那可能是上一次会话留下的，
        // 也可能是别人手写进去的。从空对象开始的话，用户在面板上做的第一个
        // 操作就会把所有它不知道的键冲掉。实测：手写了一条 `capture`，
        // 在面板上点一下「删规则」，请求文件就只剩 `{"triggers":[]}` 了
        let v = g.get_or_insert_with(|| {
            std::fs::read_to_string(request_path())
                .ok()
                .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                .filter(|x| x.is_object())
                .unwrap_or_else(|| serde_json::json!({}))
        });
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

/// 界面模式（docs/23 §9）。**三种模式是同一个后端会话上的三种界面**，
/// 不是三个程序——切模式不碰连接、不碰录制。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// 我要试一个接口。
    Api,
    /// 我要盯着这条流。
    #[default]
    Observe,
    /// 让它录着。
    Record,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Api => "API 调试",
            Mode::Observe => "数据观察",
            Mode::Record => "录制",
        }
    }
    pub const ALL: [Mode; 3] = [Mode::Api, Mode::Observe, Mode::Record];
}

static MODE: Mutex<Mode> = Mutex::new(Mode::Observe);

pub fn mode() -> Mode {
    MODE.lock().map(|g| *g).unwrap_or_default()
}

pub fn set_mode(m: Mode) {
    if let Ok(mut g) = MODE.lock() {
        *g = m;
    }
}

/// 暂停时冻结的尾窗（docs/23 §14 坑 7）。
///
/// **暂停不是断连**：后端照常收、照常录、照常触发。停的只是这一屏的刷新。
/// 界面上必须写清楚，否则用户会以为暂停期间的数据没了。
///
/// 冻结整份而不是「停止追加」：后者在环淘汰之后会露出空洞，
/// 而暂停恰恰是为了盯住某一屏不动。
static PAUSED: Mutex<Option<Frozen>> = Mutex::new(None);

#[derive(Clone)]
pub struct Frozen {
    pub rows: Vec<TailRow>,
    /// 暂停那一刻的 seq。用来算「暂停期间后端又收了多少」。
    pub at_seq: i64,
    pub at: String,
}

pub fn paused() -> Option<Frozen> {
    PAUSED.lock().ok().and_then(|g| g.clone())
}

pub fn is_paused() -> bool {
    PAUSED.lock().map(|g| g.is_some()).unwrap_or(false)
}

/// 暂停 / 继续。
pub fn toggle_pause(cur: &ObsReadout) {
    let Ok(mut g) = PAUSED.lock() else { return };
    if g.is_some() {
        *g = None;
        return;
    }
    let Some(s) = cur.session.as_ref() else { return };
    *g = Some(Frozen {
        rows: s.tail.clone(),
        at_seq: s.ring.next_seq,
        at: chrono::Local::now().format("%H:%M:%S").to_string(),
    });
}

/// API 调试的请求体编辑框。
static SEND_BODY: Mutex<String> = Mutex::new(String::new());

pub fn send_body() -> String {
    SEND_BODY.lock().map(|g| g.clone()).unwrap_or_default()
}

pub fn set_send_body(t: &str) {
    if let Ok(mut g) = SEND_BODY.lock() {
        *g = t.to_string();
    }
}

/// 请求库的编辑区状态：名称框 + 当前这些 `{{参数}}` 的值。
///
/// 参数值**不跟着库走**：库里存的是默认值，这里是「这一次要发的值」。
/// 两者混在一起的话，改一次参数就把默认值改了，下次载入出来的不是原来那条。
static LIB_NAME: Mutex<String> = Mutex::new(String::new());
static LIB_PARAMS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

pub fn lib_name() -> String {
    LIB_NAME.lock().map(|g| g.clone()).unwrap_or_default()
}

pub fn set_lib_name(t: &str) {
    if let Ok(mut g) = LIB_NAME.lock() {
        *g = t.to_string();
    }
}

/// 当前参数值。请求体里新出现的 `{{名字}}` 会自动补一个空项，
/// 删掉的会被丢弃——否则参数框会越攒越多，全是早就不用的名字。
pub fn lib_params() -> Vec<(String, String)> {
    let want = crate::ws::observatory_lib::placeholders(&send_body());
    let Ok(mut g) = LIB_PARAMS.lock() else { return Vec::new() };
    g.retain(|(k, _)| want.iter().any(|w| w == k));
    for w in &want {
        if !g.iter().any(|(k, _)| k == w) {
            g.push((w.clone(), String::new()));
        }
    }
    // 按请求体里的出现顺序排，界面上才对得上
    let mut out = g.clone();
    out.sort_by_key(|(k, _)| want.iter().position(|w| w == k).unwrap_or(usize::MAX));
    out
}

pub fn set_lib_param(name: &str, val: &str) {
    if let Ok(mut g) = LIB_PARAMS.lock() {
        match g.iter_mut().find(|(k, _)| k == name) {
            Some(e) => e.1 = val.to_string(),
            None => g.push((name.to_string(), val.to_string())),
        }
    }
}

/// 载入一条：请求体和**默认参数值**一起进编辑区。
pub fn load_saved(e: &crate::ws::observatory_lib::Saved) {
    set_send_body(&e.body);
    set_lib_name(&e.name);
    if let Ok(mut g) = LIB_PARAMS.lock() {
        *g = e.params.clone();
    }
}

/// 发一条出站请求。走 `send` 段的独立 nonce——与连接、录制都分开。
///
/// 发出去的是**填好参数**的那份。填不上的由调用方先拦下——
/// 把 `{{x}}` 字面量发出去，对端只会回一条看不懂的错误。
pub fn request_send(stream_id: u32) {
    use std::sync::atomic::{AtomicI64, Ordering};
    static N: AtomicI64 = AtomicI64::new(1);
    let f = crate::ws::observatory_lib::fill(&send_body(), &lib_params());
    patch(&[(
        "send",
        serde_json::json!({
            "stream_id": stream_id,
            "body": f.body,
            "nonce": N.fetch_add(1, Ordering::Relaxed),
        }),
    )]);
}

/// 捕获筛选的编辑框（面板内存，随打字变）。
static CAPTURE: Mutex<String> = Mutex::new(String::new());

pub fn capture_text() -> String {
    CAPTURE.lock().map(|g| g.clone()).unwrap_or_default()
}

pub fn set_capture_text(t: &str) {
    if let Ok(mut g) = CAPTURE.lock() {
        *g = t.to_string();
    }
}

/// 把捕获筛选发给守护。
///
/// **与显示筛选走不同的键**：共用一个的话，改个显示条件会把数据也筛掉。
pub fn request_capture() {
    patch(&[("capture", serde_json::json!(capture_text()))]);
}

/// 新规则的编辑框：`(名称, 条件, pre_ms, post_ms, cooldown_ms, 每小时)`。
static NEW_TRIG: Mutex<Option<TrigForm>> = Mutex::new(None);

#[derive(Clone, PartialEq, Debug)]
pub struct TrigForm {
    pub name: String,
    pub cond: String,
    pub pre_roll_ms: String,
    pub post_roll_ms: String,
    pub cooldown_ms: String,
    pub max_per_hour: String,
}

impl Default for TrigForm {
    /// 缺省值**保守**：一个每帧都命中的条件在 19000 条/秒下，
    /// 配 0 会一秒生成两万个录制目录。
    fn default() -> Self {
        Self {
            name: "规则1".into(),
            cond: String::new(),
            pre_roll_ms: "10000".into(),
            post_roll_ms: "5000".into(),
            cooldown_ms: "30000".into(),
            max_per_hour: "10".into(),
        }
    }
}

pub fn new_trig() -> TrigForm {
    NEW_TRIG.lock().map(|g| g.clone().unwrap_or_default()).unwrap_or_default()
}

pub fn set_new_trig(f: impl FnOnce(&mut TrigForm)) {
    if let Ok(mut g) = NEW_TRIG.lock() {
        f(g.get_or_insert_with(TrigForm::default));
    }
}

fn trig_json(name: &str, cond: &str, pre: i64, post: i64, cool: i64, cap: i64) -> serde_json::Value {
    serde_json::json!({
        "name": name, "cond": cond,
        "pre_roll_ms": pre, "post_roll_ms": post,
        "cooldown_ms": cool, "max_per_hour": cap,
    })
}

/// 把整份规则表发给守护。
///
/// **整份重发**而不是增量：守护那边重建规则会把冷却状态和本小时计数清掉，
/// 所以只在真的变了时才调这个（面板侧由「添加/删除」触发，不随打字发）。
pub fn request_triggers(all: &[TrigStat]) {
    let list: Vec<serde_json::Value> = all
        .iter()
        .map(|t| {
            trig_json(
                &t.name,
                &t.cond,
                t.pre_roll_ms,
                t.post_roll_ms,
                t.cooldown_ms,
                t.cap_per_hour,
            )
        })
        .collect();
    patch(&[("triggers", serde_json::json!(list))]);
}

/// 在现有规则表上加一条。
pub fn request_add_trigger(existing: &[TrigStat], f: &TrigForm) -> Result<(), String> {
    if f.cond.trim().is_empty() {
        return Err("条件不能为空".into());
    }
    if existing.iter().any(|t| t.name == f.name) {
        // 同名规则会让「删掉哪一条」变得没法表达
        return Err(format!("已经有一条叫「{}」的规则了", f.name));
    }
    let num = |s: &str, d: i64| s.trim().parse::<i64>().unwrap_or(d);
    let mut list: Vec<serde_json::Value> = existing
        .iter()
        .map(|t| {
            trig_json(&t.name, &t.cond, t.pre_roll_ms, t.post_roll_ms, t.cooldown_ms, t.cap_per_hour)
        })
        .collect();
    list.push(trig_json(
        &f.name,
        &f.cond,
        num(&f.pre_roll_ms, 10_000),
        num(&f.post_roll_ms, 5_000),
        num(&f.cooldown_ms, 30_000),
        num(&f.max_per_hour, 10),
    ));
    patch(&[("triggers", serde_json::json!(list))]);
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

    /// 测试期间请求文件一律指到临时目录。
    ///
    /// **默认就改，不是可选项**：不改的话，跑一遍测试就会写进
    /// `$XDG_RUNTIME_DIR` 里那份**用户正在用**的请求文件，守护会立刻照着
    /// 测试内容去重连。实测发生过——跑完测试之后，守护那边真的多出了
    /// 两条叫「规则1」「规则2」的触发规则。
    ///
    /// 只有需要自己读回文件内容的测试才显式指定路径。
    static REQ_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

    pub(super) fn req_override() -> Option<PathBuf> {
        Some(REQ_PATH.lock().ok().and_then(|g| g.clone()).unwrap_or_else(|| {
            std::env::temp_dir()
                .join(format!("ws-obs-test-{}", std::process::id()))
                .join("observatory_request.json")
        }))
    }

    /// 请求文件和 `REQ` 都是**进程级**的，两个测试并行跑就会互相踩。
    /// 合成一条按顺序走——这是第三次栽在同一个坑上了（前两次是 IPO 月份
    /// 和连接表单），并行测试碰进程级状态就得这么办。
    #[test]
    fn patching_the_request_file_preserves_every_key_it_did_not_write() {
        // ① 面板启动时不知道请求文件里已经有什么。从空对象开始的话，
        //    用户在面板上做的第一个操作就会把所有它不知道的键冲掉——
        //    实测：手写一条 capture，点一下「删规则」，文件就只剩 triggers 了
        let p = std::env::temp_dir()
            .join(format!("ws-obs-req-{}", std::process::id()))
            .join("observatory_request.json");
        if let Ok(mut g) = REQ_PATH.lock() {
            *g = Some(p.clone());
        }
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(
            &p,
            r#"{"adapter":"ws.raw","nonce":42,"capture":"len > 40","别人写的":1}"#,
        );
        if let Ok(mut g) = REQ.lock() {
            *g = None; // 模拟面板刚启动
        }
        patch(&[("triggers", serde_json::json!([]))]);
        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(after["capture"], "len > 40", "别的键要保住");
        assert_eq!(after["nonce"], 42, "连接那一路更要保住");
        assert_eq!(after["别人写的"], 1, "连不认识的键也保住");
        assert!(after["triggers"].as_array().unwrap().is_empty());

        // ② 两个都踩过：少带 nonce → 守护看到 3 变回 0，判成「连接变了」→
        //    改筛选就重连；少带 adapter → 判成「要断开」→ 改筛选就断线
        if let Ok(mut g) = REQ.lock() {
            *g = Some(serde_json::json!({"adapter":"ws.raw","config":{"url":"wss://x"},"nonce":7}));
        }
        set_filter_text("len > 10");
        let (f, _) = view_state();
        assert_eq!(f, "len > 10");
        let before = REQ.lock().unwrap().clone().unwrap();
        assert_eq!(before["nonce"], 7);
        assert_eq!(before["adapter"], "ws.raw");

        // ③ 请求库：发出去的必须是**填好参数**的那份。发模板的话，
        //    对端收到 `{{sym}}@depth` 这段字面量，然后回一条看不懂的错误
        if let Ok(mut g) = REQ.lock() {
            *g = Some(serde_json::json!({"adapter":"ws.raw","nonce":9}));
        }
        crate::ws::observatory_lib::reset_for_test(Some(&p.with_file_name("lib.json")));
        load_saved(&crate::ws::observatory_lib::Saved {
            name: "深度".into(),
            adapter: "ws.raw".into(),
            body: r#"{"params":["{{sym}}@depth"]}"#.into(),
            params: vec![("sym".into(), "btcusdt".into())],
        });
        assert_eq!(send_body(), r#"{"params":["{{sym}}@depth"]}"#);
        assert_eq!(lib_params(), vec![("sym".to_string(), "btcusdt".to_string())]);
        request_send(1);
        let sent: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(sent["send"]["body"], r#"{"params":["btcusdt@depth"]}"#);
        assert_eq!(sent["nonce"], 9, "发请求不该动连接那一路");

        // 请求体里换掉占位符之后，旧参数要消失、新的要补上——
        // 不然参数框会越攒越多，全是早就不用的名字
        set_send_body("{{lvl}} 档");
        assert_eq!(lib_params(), vec![("lvl".to_string(), String::new())]);
        crate::ws::observatory_lib::reset_for_test(None);

        let _ = std::fs::remove_dir_all(p.parent().unwrap());
        if let Ok(mut g) = REQ.lock() {
            *g = None;
        }
        if let Ok(mut g) = REQ_PATH.lock() {
            *g = None;
        }
    }

    #[test]
    fn the_nonce_only_ever_increases() {
        // 守护靠「变没变」判断要不要重连。回绕会让重连按钮失灵
        assert!(next_nonce() < next_nonce());
    }

    #[test]
    fn trigger_state_carries_enough_to_rebuild_the_list() {
        // 面板删掉其中一条时要把其余的原样重发。只回发名字的话就重发不了
        let j = r#"{"stamp":"s","session":{"triggers":[
          {"name":"大单","cond":"$.q > 10","pre_roll_ms":10000,"post_roll_ms":5000,
           "cooldown_ms":30000,"cap_per_hour":10,"fired":3,"blocked":1724415,
           "blocked_why":"本小时已触发 10/10 次","used_this_hour":10,"capturing":false}],
          "capture":{"src":"$.s == BTCUSDT","dropped":385024}}}"#;
        let r = parse(&serde_json::from_str(j).unwrap());
        let t = &r.session.as_ref().unwrap().triggers[0];
        assert_eq!(t.cond, "$.q > 10", "条件要回发，否则重发不了");
        assert_eq!((t.pre_roll_ms, t.post_roll_ms), (10_000, 5_000));
        assert_eq!(t.blocked, 1_724_415, "被挡次数说明条件写得太宽");
        assert!(t.blocked_why.contains("10/10"));
        assert_eq!(r.session.as_ref().unwrap().capture.dropped, 385_024);
    }

    #[test]
    fn the_new_trigger_form_defaults_are_conservative() {
        // 一个每帧都命中的条件在 19000 条/秒下，配 0 会一秒生成两万个录制目录
        let f = TrigForm::default();
        assert!(f.cooldown_ms.parse::<i64>().unwrap() >= 1_000);
        assert!(f.max_per_hour.parse::<i64>().unwrap() <= 60);
        assert!(f.pre_roll_ms.parse::<i64>().unwrap() > 0, "pre-roll 是触发录制的全部意义");
        assert!(f.cond.is_empty(), "条件要用户自己写");
    }

    #[test]
    fn adding_a_trigger_rejects_an_empty_condition_and_a_duplicate_name() {
        // 空条件会命中一切；同名规则会让「删掉哪一条」没法表达
        let mut f = TrigForm::default();
        assert!(request_add_trigger(&[], &f).is_err(), "空条件要挡住");
        f.cond = "len > 100".into();
        let existing = vec![TrigStat { name: "规则1".into(), ..Default::default() }];
        let e = request_add_trigger(&existing, &f).unwrap_err();
        assert!(e.contains("规则1"), "{e}");
        f.name = "规则2".into();
        assert!(request_add_trigger(&existing, &f).is_ok());
    }

    #[test]
    fn the_link_facts_the_bytes_cannot_tell_you_reach_the_panel() {
        // docs/23 §14 坑 4：帧里存的是解压后的内容，事后看不出压没压。
        // 只有 Adapter 在建连时说得清，所以这条必须一路走到界面
        let j = r#"{"stamp":"s","session":{"adapter":"ws.raw",
          "transport":{"ws.deflate":"开","ws.extensions":"permessage-deflate; client_max_window_bits"}}}"#;
        let se = parse(&serde_json::from_str(j).unwrap()).session.unwrap();
        assert!(se.transport.iter().any(|(k, v)| k == "ws.deflate" && v == "开"));
        // 旧版守护不发这个字段，那就是空，不是假数据
        let old = r#"{"stamp":"s","session":{"adapter":"ws.raw"}}"#;
        assert!(parse(&serde_json::from_str(old).unwrap()).session.unwrap().transport.is_empty());
    }

    #[test]
    fn a_plaintext_secret_warning_reaches_the_panel_and_an_absent_one_is_empty() {
        // 守护那边判明文/引用，面板只负责把话说出来。旧版守护不发这个字段，
        // 那也不能变成一条假警告
        let with = r#"{"stamp":"s","secret_warn":"明文密钥：headers（建议改成 ${环境变量名}）"}"#;
        assert!(parse(&serde_json::from_str(with).unwrap()).secret_warn.contains("headers"));
        let without = r#"{"stamp":"s"}"#;
        assert!(parse(&serde_json::from_str(without).unwrap()).secret_warn.is_empty());
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

/// 两次响应的差异（docs/23 §9 Mode 1）。
///
/// **按字段比，不按文本比**：API 响应是结构化的，「哪些字段变了」才是
/// 要问的问题。文本 diff 会被键顺序、空白、数组重排搅得没法看。
#[derive(Default, Clone, PartialEq, Debug)]
pub struct Diff {
    /// 新出现的字段。
    pub added: Vec<(String, String)>,
    /// 消失的字段。**要单独列**——接口悄悄不再返回某个字段，
    /// 是升级时最容易漏掉的一类变化。
    pub removed: Vec<(String, String)>,
    /// 值变了的：`(路径, 旧值, 新值)`。
    pub changed: Vec<(String, String, String)>,
    /// 两边都有且相同的字段数。
    pub same: usize,
}

impl Diff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
    pub fn total_changes(&self) -> usize {
        self.added.len() + self.removed.len() + self.changed.len()
    }
}

/// 比较两份解析结果（旧 → 新）。
pub fn diff_parsed(old: &[(String, String)], new: &[(String, String)]) -> Diff {
    let om: std::collections::BTreeMap<&str, &str> =
        old.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let nm: std::collections::BTreeMap<&str, &str> =
        new.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let mut d = Diff::default();
    for (k, nv) in &nm {
        match om.get(k) {
            None => d.added.push((k.to_string(), nv.to_string())),
            Some(ov) if ov != nv => {
                d.changed.push((k.to_string(), ov.to_string(), nv.to_string()))
            }
            Some(_) => d.same += 1,
        }
    }
    for (k, ov) in &om {
        if !nm.contains_key(k) {
            d.removed.push((k.to_string(), ov.to_string()));
        }
    }
    d
}

/// 从尾窗里挑出某条流最近的两条，比一比。
///
/// 需要**开启解析**——没有解析结果时无从比起，返回 `None` 让界面去提示，
/// 而不是拿原始文本硬比出一堆噪声。
pub fn diff_last_two(sess: &SessionView, stream_id: u32) -> Option<(Diff, i64, i64)> {
    let mut it = sess
        .tail
        .iter()
        .rev()
        .filter(|r| r.stream_id == stream_id && r.parsed.as_ref().is_some_and(|p| !p.is_empty()));
    let new = it.next()?;
    let old = it.next()?;
    Some((
        diff_parsed(old.parsed.as_ref()?, new.parsed.as_ref()?),
        old.seq,
        new.seq,
    ))
}

#[cfg(test)]
mod diff_tests {
    use super::*;

    fn kv(p: &[(&str, &str)]) -> Vec<(String, String)> {
        p.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn a_field_that_stopped_being_returned_is_listed_separately() {
        // 接口悄悄不再返回某个字段，是升级时最容易漏掉的一类变化。
        // 只报「值变了」的话它根本不会出现
        let old = kv(&[("$.a", "1"), ("$.gone", "x")]);
        let new = kv(&[("$.a", "2"), ("$.new", "y")]);
        let d = diff_parsed(&old, &new);
        assert_eq!(d.removed, vec![("$.gone".to_string(), "x".to_string())]);
        assert_eq!(d.added, vec![("$.new".to_string(), "y".to_string())]);
        assert_eq!(d.changed, vec![("$.a".to_string(), "1".to_string(), "2".to_string())]);
        assert_eq!(d.same, 0);
    }

    #[test]
    fn two_identical_responses_diff_to_nothing() {
        let a = kv(&[("$.x", "1"), ("$.y", "2")]);
        let d = diff_parsed(&a, &a);
        assert!(d.is_empty());
        assert_eq!(d.same, 2);
        assert_eq!(d.total_changes(), 0);
    }

    #[test]
    fn comparing_by_field_not_by_text_ignores_key_order() {
        // 文本 diff 会被键顺序搅得没法看，而键顺序在 JSON 里没有意义
        let a = kv(&[("$.a", "1"), ("$.b", "2")]);
        let b = kv(&[("$.b", "2"), ("$.a", "1")]);
        assert!(diff_parsed(&a, &b).is_empty());
    }

    #[test]
    fn a_diff_needs_two_parsed_responses() {
        // 没有解析结果时无从比起。拿原始文本硬比会得到一堆噪声，
        // 不如让界面提示「开启解析」
        let mut s = SessionView::default();
        s.tail = vec![TailRow { stream_id: 0, parsed: None, ..Default::default() }];
        assert!(diff_last_two(&s, 0).is_none());
        s.tail = vec![
            TailRow { seq: 1, stream_id: 0, parsed: Some(kv(&[("$.p", "1")])), ..Default::default() },
        ];
        assert!(diff_last_two(&s, 0).is_none(), "只有一条也比不了");
        s.tail.push(TailRow {
            seq: 2,
            stream_id: 0,
            parsed: Some(kv(&[("$.p", "2")])),
            ..Default::default()
        });
        let (d, a, b) = diff_last_two(&s, 0).unwrap();
        assert_eq!((a, b), (1, 2), "旧 → 新");
        assert_eq!(d.changed.len(), 1);
    }

    #[test]
    fn the_diff_only_looks_at_the_stream_it_was_asked_about() {
        // 控制流里混着「已连接」之类的合成帧，比进去只会是噪声
        let mut s = SessionView::default();
        s.tail = vec![
            TailRow { seq: 1, stream_id: 1, parsed: Some(kv(&[("$.z", "9")])), ..Default::default() },
            TailRow { seq: 2, stream_id: 0, parsed: Some(kv(&[("$.p", "1")])), ..Default::default() },
            TailRow { seq: 3, stream_id: 0, parsed: Some(kv(&[("$.p", "2")])), ..Default::default() },
        ];
        let (d, a, b) = diff_last_two(&s, 0).unwrap();
        assert_eq!((a, b), (2, 3));
        assert_eq!(d.changed[0].0, "$.p");
    }
}
