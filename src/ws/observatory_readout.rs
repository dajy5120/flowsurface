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
            })
            .collect(),
        tail_skipped: i(sv, "tail_skipped"),
    });

    ObsReadout {
        stamp: s(v, "stamp"),
        present: true,
        connected: v.get("connected").and_then(|b| b.as_bool()).unwrap_or(false),
        health: s(v, "health"),
        last_err: s(v, "last_err"),
        next_retry_secs: i(v, "next_retry_secs"),
        catalog,
        session,
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

/// 写控制请求（原子：同目录 tmp + rename）。**只有面板写这个文件。**
pub fn write_request(body: &str) {
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
        "tail_skipped":21}
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
        assert_eq!(r.session.as_ref().unwrap().tail_skipped, 21);
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
