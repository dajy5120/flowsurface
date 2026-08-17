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
        if let Ok(g) = self.flag.lock()
            && let Ok((mut g, _)) = self.cv.wait_timeout(g, d)
        {
            *g = false;
        }
    }
}

impl Default for Waker {
    fn default() -> Self {
        Self::new()
    }
}

/// 一个用户单元的运行状态（常驻服务与 oneshot 共用；字段按需取用）。
#[derive(Default, Clone)]
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
