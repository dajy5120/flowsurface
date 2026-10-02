//! systemd 用户单元的统一状态查询 + poller 唤醒器（各服务面板共用）。
//!
//! 面板约定（用户要求）：凡是有启/停按钮的服务，都要同时给出
//! **运行状态 + 运行时长 + 重启次数 + 可点的「刷新」按钮**。
//!
//! ## 为什么需要 [`Waker`]
//!
//! 各 readout 的后台 poller 是「干一轮 → sleep N 秒」的循环，N 最长 10s。
//! 点了启停按钮却要等下一轮才看到状态翻转，手感很差；而「刷新」按钮若在 UI 线程
//! 里同步查 systemctl / 扫数据湖，又会卡住渲染（docs/20 §19.2 的每帧开销教训）。
//!
//! 解法：poller 用 [`Waker::wait`] 取代 `thread::sleep`，面板只需调 [`Waker::request`]
//! ——立即返回，后台线程被唤醒马上刷一轮。启停动作后也自动请求一次，
//! 于是按钮按下去状态立刻跟着变。

use std::sync::{Condvar, Mutex};
use std::time::Duration;

/// poller 的「睡到超时，或被叫醒」原语。
pub struct Waker {
    flag: Mutex<bool>,
    cv: Condvar,
}

impl Waker {
    pub const fn new() -> Self {
        Self { flag: Mutex::new(false), cv: Condvar::new() }
    }

    /// 请求 poller 立刻刷一轮（面板「刷新」按钮、启停动作后调用）。不阻塞。
    pub fn request(&self) {
        if let Ok(mut g) = self.flag.lock() {
            *g = true;
        }
        self.cv.notify_all();
    }

    /// poller 专用：等到 `d` 超时或被 [`request`](Self::request) 唤醒。
    ///
    /// 虚假唤醒无害——最坏结果只是多刷一轮。
    pub fn wait(&self, d: Duration) {
        let _ = self.wait_requested(d);
    }

    /// 同 [`wait`](Self::wait)，返回这次是不是被 [`request`](Self::request) 叫醒的
    /// （面板「刷新」按钮 → 该做的重活这一轮要做）。
    pub fn wait_requested(&self, d: Duration) -> bool {
        let Ok(g) = self.flag.lock() else { return false };
        let g = if *g { g } else { self.cv.wait_timeout(g, d).map(|(g, _)| g).unwrap_or_else(|e| e.into_inner().0) };
        let mut g = g;
        let was = *g;
        *g = false;
        was
    }
}

/// 「有人在看」闸门（docs/35 §16.10）：读数线程只在对应面板显示时工作。
///
/// 面板取快照时 [`touch`](Self::touch)（每帧一次，一次原子交换）；读数线程每轮开头
/// [`wait_viewed`](Self::wait_viewed)——超过 [`DEMAND_IDLE`] 没人看就挂起，面板再出现时立刻唤醒。
///
/// 不加闸门时，这些线程从第一次打开对应工作区起就一直跑：实测雷达每 2s 整份重解析 16MB、
/// 录制每 3s 扫整个数据湖 + 多次 systemctl / journalctl、工厂每 4s 三次 systemctl + 查库，
/// 离开那个工作区也照跑，合计约 15% 一个核。
pub struct Demand {
    last_ms: std::sync::atomic::AtomicU64,
    lock: Mutex<()>,
    cv: Condvar,
}

/// 多久没人看就挂起。
pub const DEMAND_IDLE: Duration = Duration::from_secs(10);

/// 多久没人看就释放快照（2026-10-01，用户确认 10 分钟）。各面板的数据快照原先离开后一直留在
/// 内存里——实测依次打开几个面板后 RSS 从 233 MB 涨到 474 MB。`WS_RELEASE_AFTER_SECS` 可改（验证用）。
pub fn release_after() -> Duration {
    static D: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *D.get_or_init(|| {
        std::env::var("WS_RELEASE_AFTER_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .map_or(Duration::from_secs(600), Duration::from_secs)
    })
}

/// 把 glibc 手里空着的堆还给系统。快照里大多是小块分配（字符串、Vec），
/// 不调这个的话 free 了 RSS 也不降。
pub fn trim_heap() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        unsafe extern "C" {
            fn malloc_trim(pad: usize) -> i32;
        }
        // SAFETY: glibc 的 malloc_trim 线程安全，只整理空闲块
        unsafe {
            malloc_trim(0);
        }
    }
}

/// [`Demand::wait`] 的结果。
#[derive(Debug, Clone, Copy, Default)]
pub struct Wake {
    /// 刚才挂起过、面板是重新打开的（该刷一轮全量）
    pub resumed: bool,
    /// 挂起期间超过 [`release_after`]，快照已释放（「文件没变不重读」的记录也要清掉，否则重开后是空的）
    pub released: bool,
}

fn mono_ms() -> u64 {
    static T0: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    // +1：让「从没被看过」（0）与「刚启动就被看」区分开
    T0.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64 + 1
}

impl Demand {
    pub const fn new() -> Self {
        Self { last_ms: std::sync::atomic::AtomicU64::new(0), lock: Mutex::new(()), cv: Condvar::new() }
    }

    fn viewed_at(&self, now: u64) -> bool {
        let last = self.last_ms.load(std::sync::atomic::Ordering::Relaxed);
        last != 0 && now.saturating_sub(last) <= DEMAND_IDLE.as_millis() as u64
    }

    /// 面板正在显示（取快照的入口里调）。之前没人看 → 唤醒挂起的线程。
    pub fn touch(&self) {
        let now = mono_ms();
        let prev = self.last_ms.swap(now, std::sync::atomic::Ordering::Relaxed);
        if prev == 0 || now.saturating_sub(prev) > DEMAND_IDLE.as_millis() as u64 {
            // 先拿锁再通知：线程「检查 → 等待」也在锁里，不会漏掉这次唤醒
            let _g = self.lock.lock();
            self.cv.notify_all();
        }
    }

    /// 读数线程每轮开头调：没人看就挂起，直到面板再出现。
    /// 返回 true = 刚才挂起过、面板是**重新打开**的（该刷一轮全量）。
    pub fn wait_viewed(&self) -> bool {
        self.wait(|| {}).resumed
    }

    /// 同 [`wait_viewed`](Self::wait_viewed)，另外：挂起满 [`release_after`] 时调一次 `release`
    /// （清掉本模块的快照），然后把空闲堆还给系统。
    pub fn wait(&self, release: impl Fn()) -> Wake {
        let Ok(mut g) = self.lock.lock() else { return Wake::default() };
        let mut w = Wake::default();
        while !self.viewed_at(mono_ms()) {
            w.resumed = true;
            let last = self.last_ms.load(std::sync::atomic::Ordering::Relaxed);
            if !w.released && last != 0 && mono_ms().saturating_sub(last) >= release_after().as_millis() as u64 {
                release();
                trim_heap();
                w.released = true;
                log::info!(
                    "[{}] {}s 没显示，已释放快照",
                    std::thread::current().name().unwrap_or("?"),
                    release_after().as_secs()
                );
            }
            match self.cv.wait_timeout(g, Duration::from_secs(5)) {
                Ok((ng, _)) => g = ng,
                Err(_) => return w,
            }
        }
        w
    }
}

impl Default for Demand {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for Waker {
    fn default() -> Self {
        Self::new()
    }
}

/// 一个用户单元的运行状态（常驻服务与 oneshot 共用；字段按需取用）。
#[derive(Default, Clone, PartialEq)]
pub struct UnitState {
    /// 正在跑（`active`，oneshot 跑到一半是 `activating`，这里也算在跑）。
    pub active: bool,
    /// 已运行时长（秒）——仅 `active` 时有意义。
    pub uptime_secs: i64,
    /// 自动重启次数（`Restart=always` 的常驻服务才有意义；oneshot 恒 0）。
    pub restarts: i64,
    /// 上次退出结果：`success` / `exit-code` / `timeout` …（oneshot 看这个）。
    pub last_result: String,
    /// 上次跑完的时刻（人读，空表示没跑过）。
    pub last_finish: String,
    /// 整个单元（含子进程）当前占用的内存（字节；systemd 记账，没开或没在跑为 `None`）
    pub mem_bytes: Option<u64>,
    /// 整个单元累计用掉的 CPU 时间（纳秒）。两次之差除以间隔 = CPU%（由调用方算）
    pub cpu_ns: Option<u64>,
}

impl UnitState {
    /// 上次是否跑成功（没跑过也算 false）。
    pub fn last_ok(&self) -> bool {
        self.last_result == "success"
    }
    /// 跑过没有。
    pub fn ever_ran(&self) -> bool {
        !self.last_finish.is_empty()
    }
}

/// 查一个用户单元的状态。**只在后台 poller 里调**（起子进程，不可进渲染线程）。
///
/// 一次 `systemctl show` 取全部字段——比一个属性一个子进程省得多。
pub fn query(unit: &str) -> UnitState {
    let out = std::process::Command::new("systemctl")
        .args([
            "--user",
            "show",
            unit,
            "-p",
            "ActiveState",
            "-p",
            "NRestarts",
            "-p",
            "ActiveEnterTimestampMonotonic",
            "-p",
            "Result",
            "-p",
            "ExecMainExitTimestamp",
            "-p",
            "MemoryCurrent",
            "-p",
            "CPUUsageNSec",
        ])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();

    let get = |k: &str| -> String {
        out.lines()
            .find_map(|l| l.strip_prefix(k).and_then(|r| r.strip_prefix('=')))
            .unwrap_or("")
            .trim()
            .to_string()
    };

    let state = get("ActiveState");
    let mut st = UnitState {
        active: state == "active" || state == "activating",
        restarts: get("NRestarts").parse().unwrap_or(0),
        last_result: get("Result"),
        last_finish: get("ExecMainExitTimestamp"),
        // 「[not set]」或很大的哨兵值都当没有
        mem_bytes: get("MemoryCurrent").parse::<u64>().ok().filter(|v| *v < u64::MAX / 2),
        cpu_ns: get("CPUUsageNSec").parse::<u64>().ok().filter(|v| *v < u64::MAX / 2),
        ..Default::default()
    };
    if st.active {
        // systemd 给的是单调时钟起点，和 /proc/uptime 相减才是「已运行多久」。
        let mono = get("ActiveEnterTimestampMonotonic").parse::<f64>().unwrap_or(0.0) / 1e6;
        let up: f64 = std::fs::read_to_string("/proc/uptime")
            .ok()
            .and_then(|s| s.split_whitespace().next().and_then(|x| x.parse().ok()))
            .unwrap_or(0.0);
        st.uptime_secs = (up - mono).max(0.0) as i64;
    }
    st
}

/// 下次定时触发时刻（timer 单元；空表示不会再触发）。
pub fn next_elapse(timer: &str) -> String {
    let v = std::process::Command::new("systemctl")
        .args(["--user", "show", timer, "-p", "NextElapseUSecRealtime"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    v.split('=').nth(1).unwrap_or("").trim().to_string()
}

/// timer 当前是否会自动触发。
pub fn timer_active(timer: &str) -> bool {
    std::process::Command::new("systemctl")
        .args(["--user", "is-active", timer])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "active")
        .unwrap_or(false)
}

/// 人读时长：`2天3小时` / `4小时12分` / `5分36秒` / `42秒`。
pub fn fmt_dur(secs: i64) -> String {
    let s = secs.max(0);
    let (d, h, m, sec) = (s / 86400, s % 86400 / 3600, s % 3600 / 60, s % 60);
    if d > 0 {
        format!("{d}天{h}小时")
    } else if h > 0 {
        format!("{h}小时{m}分")
    } else if m > 0 {
        format!("{m}分{sec}秒")
    } else {
        format!("{sec}秒")
    }
}

/// 把 systemd 的时间戳裁短成人读（`Mon 2026-08-17 09:17:40 CST` → `08-17 09:17`）。
pub fn fmt_stamp(s: &str) -> String {
    let p: Vec<&str> = s.split_whitespace().collect();
    match (p.get(1), p.get(2)) {
        (Some(d), Some(t)) => {
            let d = d.split('-').skip(1).collect::<Vec<_>>().join("-");
            let t = t.rsplit_once(':').map(|(hm, _)| hm).unwrap_or(t);
            format!("{d} {t}")
        }
        _ => s.to_string(),
    }
}
