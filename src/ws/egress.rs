//! **网络出口总闸**：这台机器上凡是往外发包的，都在这一页，都能手动启停。
//!
//! # 为什么需要这一页
//!
//! 出口散在四个地方：几个常驻服务、几个定时任务、还有 **Cockpit 自己**。
//! 一个只列服务的页面会**撒谎**——实测过一次：所有服务都停了，
//! flowsurface 本进程还挂着 6 条交易所行情连接。
//!
//! # 三类出口，停法不一样
//!
//! | 类 | 谁 | 怎么停 |
//! |---|---|---|
//! | 常驻服务 | 雷达、观察终端、录制器、做市影子 | `systemctl --user stop` |
//! | 定时任务 | 三个 nightly | 停 **timer**，不是停 service |
//! | 进程内 | Cockpit 的行情图 | systemctl 停不掉——只能在这儿关订阅 |
//!
//! 定时任务那一行尤其要说清：`systemctl stop` 一个 oneshot service **什么都没停**，
//! 它本来就没在跑；到点还是会被 timer 拉起来。要停的是 timer。
//!
//! # 「现在 0 条」不等于「不会再用流量」
//!
//! 一个 08:34 触发的 nightly，此刻连接数是 0，明早照样拉一遍数据。
//! 所以这一页除了当前连接数，还必须显示 **下次触发时刻**——
//! 只看连接数会得出「已经全停了」的错误结论。
//!
//! # 连接数怎么数
//!
//! 不起 `ss` 子进程（那是每次一个 fork）：从 `/proc/<pid>/fd` 拿到该进程的
//! socket inode，再到 `/proc/net/tcp{,6}` 里查这些 inode 的状态和对端地址。
//! 只数 ESTABLISHED——监听中、TIME_WAIT 都不算「正在用流量」。
//!
//! **回环连接不能一概丢掉。** 这台机器上跑着 xray（`127.0.0.1:10808`），
//! 走代理出网的话，进程这一侧的对端就是 `127.0.0.1`，真正的外网 socket
//! 挂在代理进程上。按「对端非回环」数，一个正在狂拉行情的进程会显示成 0。
//!
//! 所以分两个数：
//!
//! - **直连**：对端不是回环。
//! - **经本机**：对端是回环，且不是我们已知的本机服务（Redis 6379）。
//!   多半是走本机代理出网——**它一样在耗流量**。
//!
//! 从 socket 本身分不出「代理跳板」和「某个本机服务」，所以这一栏叫
//! 「经本机」而不是「经代理」：只说看得见的事实，不替用户下结论。
//!
//! # 速度怎么算——以及为什么两个数对不上
//!
//! **每一路**：`ss -tineH` 给出每个 socket 的 `bytes_sent` / `bytes_received`
//! 和 `ino:`，用已有的 inode→进程映射归到各路头上，两次采样相减除以间隔。
//!
//! **总速度**：`/proc/net/dev` 里**真实网卡**的收发字节差分。
//! 「真实」的判据是有没有 `/sys/class/net/<if>/device`——veth、docker0、
//! singbox_tun 都没有。把它们一起加起来会把同一份流量数两三遍。
//!
//! 这两个数**本来就对不上**，界面上必须写清，否则用户会以为哪儿算错了：
//!
//! 1. **走本机代理的流量被数了两次**：应用 → xray（回环）算一次，
//!    xray → 外网（网卡）又算一次。
//! 2. **口径不同**：每一路数的是 TCP 载荷，总速度数的是**网线上的字节**，
//!    含 IP/TCP 头和重传。
//! 3. **总速度是整台机器的**，浏览器、系统更新全在里面。
//! 4. 两次采样之间**关掉的连接**，它最后那段字节数丢了——所以每一路的
//!    速度是**下限**。
//!
//! # 「直连」这个分类有一个盲点：透明代理
//!
//! 这台机器上跑着 sing-box TUN（`singbox_tun` 网卡）。被它透明接管的连接，
//! socket 的对端是**真实远端 IP**，所以我们会把它算成「直连」——
//! 而实际路径是 TUN → sing-box → xray → 网卡。
//!
//! 实测的指纹长这样：
//!
//! ```text
//! 全市场雷达  直连   ↓213.8K/s
//! 本机代理    经本机 ↑215.0K/s   ← 几乎相同：同一份字节被数了两遍
//! 本机代理    直连   ↓277.3K/s
//! 整机网卡           ↓291.8K/s   ≈ 代理直连 + 协议开销
//! ```
//!
//! 从 socket 层面分不出透明代理，只能这样。**整机那个数才是「真的过了网卡
//! 多少」**，逐行的数回答的是「这一路自己传了多少」——两个问题，两个数。

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use super::svcctl::{self, Waker};

/// 出口的种类。停法不同，所以必须分开。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// 常驻服务：停了就不再连。
    Service,
    /// 定时任务：现在没在跑，到点会跑。要停的是 timer。
    Timer,
    /// Cockpit 进程内：systemctl 管不着。
    InProcess,
    /// **不是本项目的东西**，但它持着外网连接。只列不动。
    ///
    /// 不列的话这一页的总数对不上机器的实际情况，用户会以为「都停了」；
    /// 给它一个停止按钮又是越界——停掉本机代理，机器上别的东西全断。
    Foreign,
}

/// 一路出口的静态描述。
pub struct Source {
    pub key: &'static str,
    pub label: &'static str,
    /// 连谁。写清楚才能让人判断该不该停。
    pub what: &'static str,
    pub kind: Kind,
    /// systemd 单元名（`InProcess` 为空）。
    pub unit: &'static str,
}

/// 全部出口。**加了新的对外连接就要往这里加一行**——
/// 漏一行，这一页就从「总闸」退化成「一份不完整的清单」，
/// 而后者比没有更糟：它会让人以为已经全停了。
pub static ALL: &[Source] = &[
    Source {
        key: "proxy",
        label: "本机代理 xray",
        what: "不是本项目的东西——只列出来让总数对得上，这一页不动它",
        kind: Kind::Foreign,
        unit: "xray",
    },
    Source {
        key: "cockpit",
        label: "Cockpit 行情图",
        what: "各交易所行情 WS（币安 / Bybit / OKX / MEXC / Hyperliquid）",
        kind: Kind::InProcess,
        unit: "",
    },
    Source {
        key: "radar",
        label: "全市场雷达",
        what: "各交易所公开 REST（全symbol 扫描）",
        kind: Kind::Service,
        unit: "ws-radar",
    },
    Source {
        key: "observatory",
        label: "接口观察终端",
        what: "你在面板里指定的那一个接口",
        kind: Kind::Service,
        unit: "ws-observatory",
    },
    Source {
        key: "recorder",
        label: "录制器",
        what: "交易所行情 WS（落盘）",
        kind: Kind::Service,
        unit: "wealthspring-recorder",
    },
    Source {
        key: "maker-shadow",
        label: "做市影子",
        what: "交易所 WS + REST（SOLUSDT）",
        kind: Kind::Service,
        unit: "wealthspring-maker-shadow",
    },
    Source {
        key: "factory-nightly",
        label: "Alpha 工厂夜间流水线",
        what: "跑数据管线（是否出网看当日任务）",
        kind: Kind::Timer,
        unit: "ws-factory-nightly.timer",
    },
    Source {
        key: "prediction-nightly",
        label: "预测市场夜间任务",
        what: "Polymarket Gamma API",
        kind: Kind::Timer,
        unit: "ws-prediction-nightly.timer",
    },
    Source {
        key: "f0-accept",
        label: "F0 验收检查",
        what: "读本地录制（一般不出网）",
        kind: Kind::Timer,
        unit: "ws-f0-accept.timer",
    },
    Source {
        key: "observatory-check",
        label: "录制日检",
        what: "只读本地文件，不出网",
        kind: Kind::Timer,
        unit: "ws-observatory-check.timer",
    },
];

/// 一路出口的当前状态。
#[derive(Clone, Default)]
pub struct Row {
    pub key: String,
    /// 在跑（服务：active；timer：会触发；进程内：订阅开着）。
    pub on: bool,
    /// 开机自启。
    pub enabled: bool,
    /// 当前 TCP 连接数。`None` = 数不出来（进程没跑）。
    pub conns: Option<Conns>,
    /// 实时速度。`None` = 还没有两次采样，或这一路没在跑。
    ///
    /// **是下限**：两次采样之间关掉的连接，它最后那段字节数丢了。
    pub bps: Option<Rate>,
    pub uptime_secs: i64,
    /// 下次触发（timer 用）。**必须显示**：连接数 0 不代表不会再用流量。
    pub next: String,
}

/// Cockpit 自己的行情订阅开关。
///
/// `main.rs` 的 `subscription()` 读它：关掉时交易所流那一支返回
/// `Subscription::none()`，iced 会把订阅连同底下的 WS 一起丢掉——
/// 连接是真的断，不是「界面不显示了」。
static STREAMS_ON: Mutex<bool> = Mutex::new(true);

pub fn streams_enabled() -> bool {
    STREAMS_ON.lock().map(|g| *g).unwrap_or(true)
}

pub fn set_streams_enabled(on: bool) {
    if let Ok(mut g) = STREAMS_ON.lock() {
        *g = on;
    }
    waker().request();
}

// ── 连接计数（纯 /proc，不起子进程）────────────────────────────

/// 一个进程持有的 socket inode。
fn socket_inodes(pid: u32) -> HashSet<u64> {
    let mut out = HashSet::new();
    let Ok(rd) = std::fs::read_dir(format!("/proc/{pid}/fd")) else { return out };
    for e in rd.flatten() {
        if let Ok(t) = std::fs::read_link(e.path()) {
            // `socket:[12345]`
            let s = t.to_string_lossy();
            if let Some(n) = s.strip_prefix("socket:[").and_then(|r| r.strip_suffix(']')) {
                if let Ok(i) = n.parse() {
                    out.insert(i);
                }
            }
        }
    }
    out
}

/// 已知的本机服务端口：连它们不算出网。
///
/// 目前只有 Redis（回放总线）。**这份名单要短**——多写一个，就可能把
/// 一条真的代理出网连接算成「本机自己人」，那正是这个计数器最怕的错。
const LOCAL_SERVICE_PORTS: [u16; 1] = [6379];

/// `/proc/net/tcp` 里一行的对端是不是回环。
///
/// 地址是小端十六进制。IPv4 回环 `127.0.0.1` 写成 `0100007F`；
/// IPv6 的 `::1` 是 15 个 0 加一个 1。
fn is_loopback(hex_addr: &str) -> bool {
    match hex_addr.len() {
        8 => hex_addr.eq_ignore_ascii_case("0100007F"),
        32 => {
            // v4 映射地址（::ffff:127.0.0.1）也要认出来
            hex_addr.eq_ignore_ascii_case("00000000000000000000000001000000")
                || hex_addr[24..].eq_ignore_ascii_case("0100007F")
        }
        _ => false,
    }
}

/// 一个进程当前的连接构成。
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Conns {
    /// 对端是外网地址。
    pub direct: u32,
    /// 对端是回环、且不是已知本机服务。**多半是走本机代理出网，一样在耗流量。**
    pub via_local: u32,
}

impl Conns {
    pub fn total(self) -> u32 {
        self.direct + self.via_local
    }
    /// 给人看的一行。
    pub fn label(self) -> String {
        match (self.direct, self.via_local) {
            (0, 0) => "0".into(),
            (d, 0) => d.to_string(),
            (0, v) => format!("{v}（经本机）"),
            (d, v) => format!("{d} + {v} 经本机"),
        }
    }
}

/// 数一个进程的 ESTABLISHED 连接。
///
/// 只是 [`stats`] 的窄门面——两份实现迟早会各走各的，而它们要是给出
/// 不同的连接数，这一页就开始自相矛盾了。
pub fn established(pid: u32) -> Conns {
    stats(pid).0
}


/// 按进程名找 pid（本项目之外的东西用）。
fn pid_of(name: &str) -> u32 {
    // `-x` 精确匹配进程名：`-f` 会把发起查询的这条命令自己也匹配上，
    // 这个坑在别处踩过（一次 pgrep -f 把调用它的 shell 也杀了）
    std::process::Command::new("pgrep")
        .args(["-x", name])
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).lines().next()?.trim().parse().ok())
        .unwrap_or(0)
}

/// 单元的主进程号。
fn main_pid(unit: &str) -> u32 {
    std::process::Command::new("systemctl")
        .args(["--user", "show", unit, "-p", "MainPID"])
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .split('=')
                .nth(1)
                .and_then(|v| v.trim().parse().ok())
        })
        .unwrap_or(0)
}

fn is_enabled(unit: &str) -> bool {
    std::process::Command::new("systemctl")
        .args(["--user", "is-enabled", unit])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "enabled")
        .unwrap_or(false)
}

/// 启/停一路出口。返回一句给人看的回执。
pub fn action(key: &str, act: &str) -> String {
    let Some(s) = ALL.iter().find(|s| s.key == key) else {
        return format!("✗ 不认识的出口：{key}");
    };
    if s.kind == Kind::Foreign {
        // 停掉本机代理，机器上别的东西全断。这不是本项目该管的
        return format!("✗ {} 不归这一页管——它是本项目之外的东西", s.label);
    }
    if s.kind == Kind::InProcess {
        set_streams_enabled(act == "start" || act == "enable");
        return format!(
            "✔ Cockpit 行情订阅已{}",
            if streams_enabled() { "打开" } else { "关闭（连接会断开）" }
        );
    }
    // **必须 --no-block**：`systemctl start` 会等 unit 就绪才返回，
    // 足以冻死 UI 线程（观察终端/预测/工厂三处都踩过）
    let mut args = vec!["--user", act];
    if act == "start" || act == "stop" || act == "restart" {
        args.push("--no-block");
    }
    args.push(s.unit);
    let r = match std::process::Command::new("systemctl").args(&args).status() {
        Ok(st) if st.success() => format!("✔ {} 已{}", s.label, verb(act)),
        Ok(st) => format!("✗ {} systemctl {act} 退出码 {:?}", s.label, st.code()),
        Err(e) => format!("✗ {} systemctl 失败：{e}", s.label),
    };
    waker().request();
    r
}

fn verb(act: &str) -> &'static str {
    match act {
        "start" => "启动",
        "stop" => "停止",
        "enable" => "设为开机自启",
        "disable" => "取消开机自启",
        _ => "操作",
    }
}

/// 全部停止。**不动开机自启**——那是另一件事，要用户自己点。
///
/// 返回 `(停了几路, 回执)`。
pub fn stop_all() -> (usize, String) {
    let mut n = 0;
    for s in ALL {
        match s.kind {
            // 本项目之外的东西不碰
            Kind::Foreign => {}
            Kind::InProcess => {
                if streams_enabled() {
                    set_streams_enabled(false);
                    n += 1;
                }
            }
            _ => {
                // timer 停的是 timer 本身；停它对应的 oneshot service 什么都没停
                let _ = std::process::Command::new("systemctl")
                    .args(["--user", "stop", "--no-block", s.unit])
                    .status();
                n += 1;
            }
        }
    }
    waker().request();
    (n, format!("已停 {n} 路。开机自启没动——那是另一件事，要单独关"))
}

/// 最近一次操作的回执。**每个按钮都要给回执**：`systemctl --no-block`
/// 是异步返回的，不给一句话的话，点了停止但没停下来（比如权限不对）
/// 在界面上和「停下来了」分不出来。
static NOTE: Mutex<String> = Mutex::new(String::new());

pub fn note() -> String {
    NOTE.lock().map(|g| g.clone()).unwrap_or_default()
}

pub fn set_note(t: &str) {
    if !t.is_empty()
        && let Ok(mut g) = NOTE.lock()
    {
        *g = t.to_string();
    }
}

// ── 轮询 ──────────────────────────────────────────────────────

static ROWS: OnceLock<Mutex<Vec<Row>>> = OnceLock::new();
/// 整机速度（下行, 上行）字节/秒。`None` = 还没有两次采样。
static WIRE: Mutex<Option<(f64, f64)>> = Mutex::new(None);

/// 整台机器现在的网速（真实网卡上的字节，含 IP/TCP 头）。
pub fn wire_rate() -> Option<(f64, f64)> {
    WIRE.lock().ok().and_then(|g| *g)
}
static WAKER: OnceLock<Waker> = OnceLock::new();
static POLLER: OnceLock<()> = OnceLock::new();

pub fn waker() -> &'static Waker {
    WAKER.get_or_init(Waker::new)
}

/// 当前状态。第一次调用时把后台 poller 拉起来。
pub fn rows() -> Vec<Row> {
    POLLER.get_or_init(|| {
        std::thread::spawn(|| {
            let mut prev = Prev::default();
            loop {
                let v = collect(&mut prev);
                if let Ok(mut g) = ROWS.get_or_init(|| Mutex::new(Vec::new())).lock() {
                    *g = v;
                }
                // 2 秒够了：这一页是给人点按钮用的，不是监控图
                waker().wait(std::time::Duration::from_secs(2));
            }
        });
    });
    ROWS.get_or_init(|| Mutex::new(Vec::new())).lock().map(|g| g.clone()).unwrap_or_default()
}

/// 扫一轮。**只在后台线程里调**：起 systemctl / ss 子进程，不可进渲染线程。
fn collect(prev: &mut Prev) -> Vec<Row> {
    let b = sample_bytes();
    let now = std::time::Instant::now();
    let dt = prev.at.map(|t| now.duration_since(t).as_secs_f64()).unwrap_or(0.0);

    // 整机速度：只算真实网卡
    let w = wire_bytes();
    if let Ok(mut g) = WIRE.lock() {
        *g = rate(w, prev.wire, dt);
    }
    prev.wire = w;

    let out = ALL
        .iter()
        .map(|s| {
            let pid = match s.kind {
                Kind::Foreign => pid_of(s.unit),
                Kind::InProcess => std::process::id(),
                Kind::Service => {
                    if svcctl::query(s.unit).active { main_pid(s.unit) } else { 0 }
                }
                // 定时任务平时没在跑，数连接和速度都没有意义——看下次触发
                Kind::Timer => 0,
            };
            let (conns, sk) = stats(pid);
            // 定时任务没有进程，给 None 而不是 0——「没在跑」和「跑着但没传」
            // 是两回事
            let bps = (pid != 0)
                .then(|| {
                    let d = as_rate(delta_bytes(&sk.direct, &b, &prev.per_inode), dt)?;
                    let v = as_rate(delta_bytes(&sk.via_local, &b, &prev.per_inode), dt)?;
                    Some(Rate { direct: d, via_local: v })
                })
                .flatten();
            match s.kind {
                Kind::Foreign => Row {
                    key: s.key.into(),
                    on: pid != 0,
                    enabled: false,
                    conns: (pid != 0).then_some(conns),
                    bps,
                    ..Default::default()
                },
                Kind::InProcess => Row {
                    key: s.key.into(),
                    on: streams_enabled(),
                    // Cockpit 是随面板起的，「开机自启」对它没意义
                    enabled: false,
                    conns: Some(conns),
                    bps,
                    ..Default::default()
                },
                Kind::Service => {
                    let st = svcctl::query(s.unit);
                    Row {
                        key: s.key.into(),
                        on: st.active,
                        enabled: is_enabled(s.unit),
                        conns: st.active.then_some(conns),
                        bps,
                        uptime_secs: st.uptime_secs,
                        next: String::new(),
                    }
                }
                Kind::Timer => Row {
                    key: s.key.into(),
                    on: svcctl::timer_active(s.unit),
                    enabled: is_enabled(s.unit),
                    conns: None,
                    bps: None,
                    uptime_secs: 0,
                    next: svcctl::fmt_stamp(&svcctl::next_elapse(s.unit)),
                },
            }
        })
        .collect();
    prev.per_inode = b;
    prev.at = Some(now);
    out
}

// ── 速度 ──────────────────────────────────────────────────────

/// 一次 `ss` 采样：inode → （已发, 已收）字节。
type Bytes = std::collections::HashMap<u64, (u64, u64)>;

/// 扫一遍所有 socket 的字节计数。
///
/// `/proc/net/tcp` 里**没有**字节数——那是内核 TCP_INFO 里的东西，
/// 只能走 netlink（`ss` 就是干这个的）。所以这一处认了一次 fork，
/// 放在后台线程、两秒一次。
fn sample_bytes() -> Bytes {
    let mut out = Bytes::new();
    let Ok(o) = std::process::Command::new("ss").args(["-tineH"]).output() else { return out };
    let t = String::from_utf8_lossy(&o.stdout);
    // 一条记录两行：头一行带 `ino:`，紧跟的缩进行带 `bytes_sent:` / `bytes_received:`
    let mut ino = 0u64;
    for line in t.lines() {
        if let Some(v) = field(line, "ino:") {
            ino = v;
        }
        let sent = field(line, "bytes_sent:");
        let recv = field(line, "bytes_received:");
        if (sent.is_some() || recv.is_some()) && ino != 0 {
            let e = out.entry(ino).or_default();
            // 同一个 inode 只该出现一次；真出现两次就取大的，别把速度算成负
            e.0 = e.0.max(sent.unwrap_or(0));
            e.1 = e.1.max(recv.unwrap_or(0));
            ino = 0;
        }
    }
    out
}

/// 从 `ss` 的一行里抠一个 `名字:数字`。
fn field(line: &str, key: &str) -> Option<u64> {
    let i = line.find(key)? + key.len();
    let rest = &line[i..];
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// 一路的连接数，外加它持有的 socket inode。
///
/// 连接数和 inode 一起给：两者都要遍历 `/proc/<pid>/fd`，分开做就是读两遍。
fn stats(pid: u32) -> (Conns, Sockets) {
    let mut c = Conns::default();
    let mut sk = Sockets::default();
    if pid == 0 {
        return (c, sk);
    }
    let mine = socket_inodes(pid);
    if mine.is_empty() {
        return (c, sk);
    }
    for f in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(t) = std::fs::read_to_string(f) else { continue };
        for line in t.lines().skip(1) {
            let c2: Vec<&str> = line.split_whitespace().collect();
            let (Some(rem), Some(st), Some(ino)) = (c2.get(2), c2.get(3), c2.get(9)) else {
                continue;
            };
            if *st != "01" {
                continue;
            }
            if !ino.parse::<u64>().is_ok_and(|i| mine.contains(&i)) {
                continue;
            }
            let Some((addr, port)) = rem.split_once(':') else { continue };
            let port = u16::from_str_radix(port, 16).unwrap_or(0);
            let ino: u64 = ino.parse().unwrap_or(0);
            if !is_loopback(addr) {
                c.direct += 1;
                sk.direct.insert(ino);
            } else if !LOCAL_SERVICE_PORTS.contains(&port) {
                c.via_local += 1;
                sk.via_local.insert(ino);
            }
        }
    }
    (c, sk)
}

/// 一个进程的 socket，**按「直连」和「经本机」分开**。
///
/// 分开的理由是本机代理：它两侧都持着 socket——一侧收客户端、一侧发外网，
/// 走的是同一份字节。合在一起算，它的速度会是真实值的两倍。
/// 分开之后界面上看到的是「↓170K + 170K 经本机」，一眼就知道是同一份数两遍。
#[derive(Default)]
struct Sockets {
    direct: HashSet<u64>,
    via_local: HashSet<u64>,
}

/// 这一轮里，这些 socket 上新走了多少字节。
///
/// # 为什么不是「两次总和相减」
///
/// 第一版是把当前所有 socket 的累计字节加起来，两轮相减。**在连接频繁
/// 开关的进程上这个数会往回跳**——旧 socket 关掉，它那份累计从总和里
/// 消失了，于是差出负数，界面上就是一格「—」。本机代理正是这种进程：
/// 实测它的连接数一分钟内在 33 和 48 之间来回，速度那一格几乎永远是空的。
///
/// 改成**逐 socket 求差**：
///
/// - 上一轮见过的 socket：只算它这一轮多走的。
/// - 这一轮**新出现**的 socket：它是上次采样之后才建的，所以它身上
///   的字节本来就都发生在这个窗口里，全算。
/// - 关掉的 socket：它最后那一小段没人报了，丢掉——所以这个数是**下限**。
fn delta_bytes(inodes: &HashSet<u64>, now: &Bytes, prev: &Bytes) -> (u64, u64) {
    let mut d = (0u64, 0u64);
    for i in inodes {
        let Some((s, r)) = now.get(i) else { continue };
        let (ps, pr) = prev.get(i).copied().unwrap_or((0, 0));
        // 单个 socket 的累计只会增；真倒退（inode 被复用）就按 0 算，
        // 别让一路的速度被一个复用的 inode 拉成负数
        d.0 += s.saturating_sub(ps);
        d.1 += r.saturating_sub(pr);
    }
    d
}

/// 上一轮采样，用来做差分。
#[derive(Default)]
pub struct Prev {
    at: Option<std::time::Instant>,
    /// **逐 socket** 的累计字节。按进程总和存的话，连接一开一关就会
    /// 差出负数（见 [`delta_bytes`]）。
    per_inode: Bytes,
    wire: (u64, u64),
}

/// 一路的实时速度，按连接类别分开。
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct Rate {
    /// 直连外网的：（下行, 上行）字节/秒。
    pub direct: (f64, f64),
    /// 经本机（多半是代理）的。
    pub via_local: (f64, f64),
}

impl Rate {
    pub fn total_down(self) -> f64 {
        self.direct.0 + self.via_local.0
    }
    /// 给人看的一行。
    ///
    /// 两类都有时**必须都显示**：那通常意味着这是个代理，两边是同一份字节，
    /// 只给一个合计数会让它看起来在跑两倍的量。
    pub fn label(self) -> String {
        let d = human_rate(self.direct.0, self.direct.1);
        if self.via_local == (0.0, 0.0) {
            return d;
        }
        let v = human_rate(self.via_local.0, self.via_local.1);
        if self.direct == (0.0, 0.0) { format!("{v} 经本机") } else { format!("{d} + {v} 经本机") }
    }
}

/// 一段增量换算成速率。对进程来说 received 是下行。
fn as_rate(d: (u64, u64), dt: f64) -> Option<(f64, f64)> {
    (dt > 0.0).then(|| (d.1 as f64 / dt, d.0 as f64 / dt))
}

/// 网卡累计计数器的速率。
///
/// 网卡计数器只会增，减出负数说明它回绕或被重置了——那一轮不给数，
/// 别显示一个假的负值。
fn rate(now: (u64, u64), prev: (u64, u64), dt: f64) -> Option<(f64, f64)> {
    if dt <= 0.0 || now.0 < prev.0 || now.1 < prev.1 {
        return None;
    }
    // 网卡这边 (rx, tx) 本来就是 (下行, 上行)
    Some(((now.0 - prev.0) as f64 / dt, (now.1 - prev.1) as f64 / dt))
}

/// 真实网卡的收发累计字节。
///
/// 「真实」= 有 `/sys/class/net/<if>/device`。veth / docker0 / singbox_tun
/// 都没有——把它们加进来会把同一份流量数两三遍。
pub fn wire_bytes() -> (u64, u64) {
    let Ok(t) = std::fs::read_to_string("/proc/net/dev") else { return (0, 0) };
    let mut rx = 0u64;
    let mut tx = 0u64;
    for line in t.lines().skip(2) {
        let Some((name, rest)) = line.split_once(':') else { continue };
        let name = name.trim();
        if !std::path::Path::new(&format!("/sys/class/net/{name}/device")).exists() {
            continue;
        }
        let c: Vec<&str> = rest.split_whitespace().collect();
        rx += c.first().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
        tx += c.get(8).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    }
    (rx, tx)
}

/// 参与统计的网卡名（界面上要说清总速度算的是哪几张卡）。
pub fn wire_ifaces() -> Vec<String> {
    let Ok(rd) = std::fs::read_dir("/sys/class/net") else { return Vec::new() };
    let mut v: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| std::path::Path::new(&format!("/sys/class/net/{n}/device")).exists())
        .collect();
    v.sort();
    v
}

/// 人读速率。
pub fn human_bps(b: f64) -> String {
    const U: [&str; 4] = ["B/s", "K/s", "M/s", "G/s"];
    let mut x = b.max(0.0);
    let mut i = 0;
    while x >= 1024.0 && i + 1 < U.len() {
        x /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{x:.0}{}", U[i]) } else { format!("{x:.1}{}", U[i]) }
}

/// 收发两路的速度，给人看的一行。
pub fn human_rate(down: f64, up: f64) -> String {
    format!("↓{} ↑{}", human_bps(down), human_bps(up))
}

/// 现在一共几条连接。
pub fn total_conns(rows: &[Row]) -> Conns {
    rows.iter().filter_map(|r| r.conns).fold(Conns::default(), |a, c| Conns {
        direct: a.direct + c.direct,
        via_local: a.via_local + c.via_local,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_proxy_hop_is_counted_not_discarded() {
        // 这台机器上跑着 xray（127.0.0.1:10808）。走代理出网的话，
        // 进程这一侧的对端就是 127.0.0.1——按「非回环」数，一个正在
        // 狂拉行情的进程会显示成 0
        assert_eq!(Conns { direct: 0, via_local: 30 }.label(), "30（经本机）");
        assert_eq!(Conns { direct: 7, via_local: 0 }.label(), "7");
        assert_eq!(Conns { direct: 2, via_local: 5 }.label(), "2 + 5 经本机");
        assert_eq!(Conns::default().label(), "0");
        assert_eq!(Conns { direct: 2, via_local: 5 }.total(), 7);
        // Redis 是本机数据总线，不是出网
        assert!(LOCAL_SERVICE_PORTS.contains(&6379));
        assert_eq!(LOCAL_SERVICE_PORTS.len(), 1, "这份名单要短：多写一个就可能把真的出网算成自己人");
    }

    #[test]
    fn loopback_addresses_are_recognised_in_both_families() {
        assert!(is_loopback("0100007F"), "127.0.0.1（小端十六进制）");
        assert!(is_loopback("0100007f"), "大小写都要认");
        assert!(is_loopback("00000000000000000000000001000000"), "::1");
        assert!(is_loopback("0000000000000000FFFF00000100007F"), "::ffff:127.0.0.1");
        // 真外网地址不能被当成回环
        assert!(!is_loopback("9B3D0E12"));
        assert!(!is_loopback(""));
        assert!(!is_loopback("0100007F00"), "长度不对就不猜");
    }

    #[test]
    fn every_source_declares_how_it_is_stopped() {
        // 漏一行，这一页就从「总闸」退化成「一份不完整的清单」——
        // 而后者比没有更糟：它会让人以为已经全停了
        for s in ALL {
            assert!(!s.label.is_empty() && !s.what.is_empty(), "{} 缺说明", s.key);
            match s.kind {
                Kind::InProcess => assert!(s.unit.is_empty(), "{} 进程内的不该有单元名", s.key),
                // 只列不动：给它停止按钮就是越界，停掉本机代理机器上别的东西全断
                Kind::Foreign => {
                    assert!(!s.unit.is_empty(), "{} 要有进程名才数得出连接", s.key);
                    assert!(
                        action(s.key, "stop").starts_with('✗'),
                        "{} 不该能被这一页停掉",
                        s.key
                    );
                }
                Kind::Service => assert!(
                    !s.unit.is_empty() && !s.unit.ends_with(".timer"),
                    "{} 服务的单元名不该是 timer",
                    s.key
                ),
                // 停 oneshot service 什么都没停（它本来就没在跑），到点还是会被拉起来
                Kind::Timer => assert!(
                    s.unit.ends_with(".timer"),
                    "{} 定时任务必须指向 timer 而不是 service",
                    s.key
                ),
            }
        }
    }

    /// 这一页最大的失败模式不是「按钮点不动」，是**漏了一路**——
    /// 那会让人看着一张「全停了」的表，而机器还在往外发包。
    ///
    /// 所以拿磁盘上真实存在的 unit 文件对一遍：新加了服务却忘了往
    /// [`ALL`] 里加一行，这条会红。
    #[test]
    fn no_installed_unit_is_missing_from_the_inventory() {
        let dir = match std::env::var("HOME") {
            Ok(h) => std::path::PathBuf::from(h).join(".config/systemd/user"),
            Err(_) => return,
        };
        let Ok(rd) = std::fs::read_dir(&dir) else { return }; // 别的机器上没有就跳过
        let known: Vec<&str> = ALL.iter().map(|s| s.unit).collect();
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if !(n.starts_with("ws-") || n.starts_with("wealthspring-")) {
                continue;
            }
            let stem = n.trim_end_matches(".service");
            // timer 由它的 .timer 那一行代表；oneshot service 不单独列
            // （停它什么都没停，到点还是会被 timer 拉起来）
            if n.ends_with(".timer") {
                assert!(known.contains(&n.as_str()), "定时任务 {n} 不在总闸清单里");
            } else if !std::path::Path::new(&dir).join(format!("{stem}.timer")).exists() {
                assert!(known.contains(&stem), "服务 {stem} 不在总闸清单里——加一行到 egress::ALL");
            }
        }
    }

    fn bytes(v: &[(u64, u64, u64)]) -> Bytes {
        v.iter().map(|(i, s, r)| (*i, (*s, *r))).collect()
    }

    #[test]
    fn a_closing_socket_does_not_make_the_rate_go_backwards() {
        // 第一版是「所有 socket 的累计加起来，两轮相减」。旧 socket 一关，
        // 它那份累计就从总和里消失，差出负数 → 界面上一格「—」。
        // 实测本机代理的连接数一分钟内在 33↔48 之间来回，那一格几乎永远空着
        let ino: HashSet<u64> = [2u64].into_iter().collect();
        let prev = bytes(&[(1, 900, 900), (2, 100, 100)]); // 1 号这一轮关了
        let now = bytes(&[(2, 150, 180)]);
        // 逐 socket 求差：只看还在的 2 号，多走了 50/80
        assert_eq!(delta_bytes(&ino, &now, &prev), (50, 80));
    }

    #[test]
    fn a_socket_born_in_this_window_contributes_all_of_its_bytes() {
        // 新 socket 是上次采样之后才建的，它身上的字节本来就都发生在
        // 这个窗口里。算 0 的话，短命连接（REST 轮询）的流量会全部漏掉
        let ino: HashSet<u64> = [7u64].into_iter().collect();
        assert_eq!(delta_bytes(&ino, &bytes(&[(7, 300, 40)]), &Bytes::new()), (300, 40));
    }

    #[test]
    fn a_reused_inode_cannot_drag_a_rate_negative() {
        // inode 会被复用。新 socket 的累计比旧的小，硬减就是负数
        let ino: HashSet<u64> = [3u64].into_iter().collect();
        let prev = bytes(&[(3, 9999, 9999)]);
        assert_eq!(delta_bytes(&ino, &bytes(&[(3, 10, 20)]), &prev), (0, 0));
    }

    #[test]
    fn the_rate_is_reported_as_down_then_up_from_the_processs_point_of_view() {
        // 对进程来说 received 是下行。搞反的话，一个在狂拉行情的进程
        // 会显示成在狂发数据
        let r = as_rate((10, 1000), 1.0).unwrap();
        assert_eq!(r.0, 1000.0, "下行 = received");
        assert_eq!(r.1, 10.0, "上行 = sent");
        assert_eq!(as_rate((10, 10), 0.0), None, "第一轮没有间隔，算不出来");
        // 没变化就是 0，不是 None——「连着但没在传」和「不知道」是两回事
        assert_eq!(as_rate((0, 0), 2.0), Some((0.0, 0.0)));
    }

    #[test]
    fn the_nic_counter_is_rx_then_tx_and_a_reset_yields_no_rate() {
        // 网卡这边 (rx, tx) 本来就是 (下行, 上行)——和进程那边反过来，
        // 两处用同一个函数的话必然搞错一处
        assert_eq!(rate((1000, 10), (0, 0), 1.0), Some((1000.0, 10.0)));
        assert_eq!(rate((5, 5), (100, 100), 2.0), None, "计数器被重置");
    }

    #[test]
    fn ss_output_is_parsed_into_inode_and_bytes() {
        // 一条记录两行：头行带 ino:，紧跟的缩进行带 bytes_*
        assert_eq!(field("uid:1000 ino:211024579 sk:263c47", "ino:"), Some(211_024_579));
        assert_eq!(field(" cwnd:10 bytes_sent:4111 bytes_acked:4111", "bytes_sent:"), Some(4111));
        assert_eq!(field("bytes_received:4630 segs_out:33", "bytes_received:"), Some(4630));
        // 没有就是没有，不猜 0——猜 0 会让速度算出一个假的下跌
        assert_eq!(field("ESTAB 0 0", "ino:"), None);
        // `ino:0` 是内核给的「无 inode」，不是一个真 socket
        assert_eq!(field("ino:0 sk:1", "ino:"), Some(0));
    }

    #[test]
    fn only_real_nics_count_toward_the_machine_total() {
        // veth / docker0 / singbox_tun 上跑的是同一份流量的另一段。
        // 全加起来会把它数两三遍——这台机器上有 30 多个 veth
        let v = wire_ifaces();
        for n in &v {
            assert!(
                std::path::Path::new(&format!("/sys/class/net/{n}/device")).exists(),
                "{n} 不是真实网卡"
            );
            assert!(!n.starts_with("veth") && !n.starts_with("docker") && n != "lo");
        }
        let (rx, tx) = wire_bytes();
        // 这台机器一定有网卡；累计字节数只会增
        assert!(!v.is_empty(), "一张真实网卡都没找到？");
        assert!(rx > 0 || tx > 0, "网卡累计字节是 0，不像真的");
    }

    #[test]
    fn a_proxy_shows_both_sides_instead_of_one_doubled_number() {
        // 代理两侧都持着 socket——一侧收客户端、一侧发外网，走的是同一份
        // 字节。合成一个数的话它看起来在跑两倍的量（实测 xray 曾显示 ↓338K/s，
        // 而实际过网卡的只有一半）
        let p = Rate { direct: (170_000.0, 5000.0), via_local: (168_000.0, 5000.0) };
        assert!(p.label().contains('+') && p.label().contains("经本机"), "{}", p.label());
        // 普通进程只有一侧，就别拿括号去烦人
        let plain = Rate { direct: (1024.0, 0.0), ..Default::default() };
        assert_eq!(plain.label(), "↓1.0K/s ↑0B/s");
        // 完全走代理的进程：只显示那一侧，并标明是经本机
        let behind = Rate { via_local: (2048.0, 0.0), ..Default::default() };
        assert_eq!(behind.label(), "↓2.0K/s ↑0B/s 经本机");
        assert_eq!(Rate::default().label(), "↓0B/s ↑0B/s");
    }

    #[test]
    fn speeds_are_readable_at_every_scale() {
        assert_eq!(human_bps(0.0), "0B/s");
        assert_eq!(human_bps(999.0), "999B/s");
        assert_eq!(human_bps(1536.0), "1.5K/s");
        assert_eq!(human_bps(5.0 * 1024.0 * 1024.0), "5.0M/s");
        // 负数不该出现（rate 已经挡了），万一漏进来也别显示成 -0
        assert_eq!(human_bps(-5.0), "0B/s");
        assert_eq!(human_rate(1024.0, 512.0), "↓1.0K/s ↑512B/s");
    }

    #[test]
    fn source_keys_are_unique() {
        let mut k: Vec<&str> = ALL.iter().map(|s| s.key).collect();
        let n = k.len();
        k.sort_unstable();
        k.dedup();
        assert_eq!(k.len(), n, "key 重复会让按钮点到别人身上");
    }

    #[test]
    fn counting_our_own_process_does_not_panic_and_excludes_loopback() {
        // 真在这台机器上跑一遍：数得出来、不炸
        let n = established(std::process::id());
        assert!(n.total() < 10_000, "{n:?} 看起来不像真的");
        // 不存在的 pid 是 0 而不是 panic
        assert_eq!(established(0), Conns::default());
        assert_eq!(established(u32::MAX), Conns::default());
    }
}

#[cfg(test)]
mod live_check {
    /// 只在本机手动跑：`cargo test -- --ignored egress_live`
    /// 拿真进程和 `ss` 对一次口径。手动跑：
    /// `cargo test -- --ignored count_matches_ss --nocapture <pid>`
    #[test]
    #[ignore]
    fn count_matches_ss() {
        for name in ["flowsurface", "ws-radar", "ws-observatory", "xray"] {
            let out = std::process::Command::new("pgrep").args(["-x", name]).output().unwrap();
            for pid in String::from_utf8_lossy(&out.stdout).lines() {
                let pid: u32 = pid.trim().parse().unwrap();
                let c = super::established(pid);
                let ss = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(format!("ss -tnp 2>/dev/null | grep -c 'pid={pid},'"))
                    .output()
                    .unwrap();
                let n: u32 = String::from_utf8_lossy(&ss.stdout).trim().parse().unwrap_or(0);
                println!("{name:<16} pid {pid:<8} 我们 {:<16} ss {n}", c.label());
            }
        }
    }

    #[test]
    #[ignore]
    fn egress_live() {
        super::rows(); // 拉起 poller
        // 要两轮才有速度：第一轮只是基线
        std::thread::sleep(std::time::Duration::from_millis(5000));
        for (s, r) in super::ALL.iter().zip(super::rows()) {
            println!(
                "{:<22} {:<8} 连接 {:<18} {:<34} {}",
                s.label,
                if r.on { "在跑" } else { "停着" },
                r.conns.map(|c| c.label()).unwrap_or_else(|| "—".into()),
                r.bps.map(|r| r.label()).unwrap_or_else(|| "—".into()),
                r.next
            );
        }
        match super::wire_rate() {
            Some((d, u)) => println!("\n整机（真实网卡 {:?}）：{}", super::wire_ifaces(), super::human_rate(d, u)),
            None => println!("\n整机：还没算出来"),
        }
    }
}
