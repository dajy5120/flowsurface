//! 「抓取一次」（2026-10-01，用户要求耗网络 / 耗资源的任务人工触发）：
//! 以子进程跑守护的 `--once` 模式（各源请求一轮、写快照、退出），不常驻、不定时。
//!
//! 子进程留在 Cockpit 的 cgroup 里：`ws-cockpit` 停止（关窗）时 systemd 会把它一起结束，
//! 「关窗即停」不破例。输出写到运行目录下的 `<名字>-once.log`，失败时看得到原因。

use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Instant;

pub struct OnceJob {
    /// 守护的二进制名（`target/release/` 下）
    bin: &'static str,
    /// 配置文件（相对仓库根）
    config: &'static str,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    child: Option<(Child, Instant)>,
    /// 上一次的结果（给面板显示）
    last: String,
}

impl OnceJob {
    pub const fn new(bin: &'static str, config: &'static str) -> Self {
        Self { bin, config, state: Mutex::new(State { child: None, last: String::new() }) }
    }

    /// 起一次。已在跑 / 常驻守护开着（`daemon_active`）→ 不起，返回原因。
    /// 返回的一句话同时记进 [`status`](Self::status)，面板按钮旁边看得到。
    pub fn start(&self, daemon_active: bool) -> String {
        let Ok(mut g) = self.state.lock() else { return "✗ 状态锁异常".into() };
        let m = self.start_locked(&mut g, daemon_active);
        if g.child.is_none() {
            g.last = m.clone();
        }
        m
    }

    fn start_locked(&self, g: &mut State, daemon_active: bool) -> String {
        if daemon_active {
            return "守护正在常驻运行，数据本来就在持续更新，不需要再抓一次".into();
        }
        poll(g);
        if g.child.is_some() {
            return "正在抓取中，请等这一轮跑完".into();
        }
        let root = super::paths::repo_root();
        let exe = root.join("target/release").join(self.bin);
        if !exe.is_file() {
            return format!("✗ 找不到 {}（先 cargo build --release -p 对应 crate）", exe.display());
        }
        let log = super::paths::runtime_dir().join(format!("{}-once.log", self.bin));
        let out = std::fs::File::create(&log).ok();
        let mut cmd = Command::new(&exe);
        cmd.arg(root.join(self.config)).arg("--once").current_dir(&root).stdin(Stdio::null());
        match out.and_then(|f| f.try_clone().ok().map(|f2| (f, f2))) {
            Some((a, b)) => {
                cmd.stdout(a).stderr(b);
            }
            None => {
                cmd.stdout(Stdio::null()).stderr(Stdio::null());
            }
        }
        match cmd.spawn() {
            Ok(c) => {
                g.child = Some((c, Instant::now()));
                g.last.clear();
                "⟳ 已开始抓取一次（跑完自动退出）".into()
            }
            Err(e) => format!("✗ 启动失败：{e}"),
        }
    }

    /// 面板显示的一行：「抓取中 37s」/「上次抓取 19:01 完成，用时 60s」/ 空。
    pub fn status(&self) -> String {
        let Ok(mut g) = self.state.lock() else { return String::new() };
        poll(&mut g);
        match &g.child {
            Some((_, t0)) => format!("抓取中 {}s", t0.elapsed().as_secs()),
            None => g.last.clone(),
        }
    }

    pub fn running(&self) -> bool {
        self.state.lock().map(|mut g| {
            poll(&mut g);
            g.child.is_some()
        })
        .unwrap_or(false)
    }
}

/// 子进程结束了就收尸并记下结果。
fn poll(g: &mut State) {
    let Some((c, t0)) = g.child.as_mut() else { return };
    if let Ok(Some(st)) = c.try_wait() {
        let secs = t0.elapsed().as_secs();
        let at = chrono::Local::now().format("%H:%M");
        g.last = if st.success() {
            format!("上次抓取 {at} 完成，用时 {secs}s")
        } else {
            format!("上次抓取 {at} 失败（退出码 {:?}），见运行目录下的 *-once.log", st.code())
        };
        g.child = None;
    }
}

pub static RADAR: OnceJob = OnceJob::new("ws-radar", "crates/wealthspring-radar/radar.toml");
pub static NEWS: OnceJob = OnceJob::new("ws-news", "crates/wealthspring-news/news.toml");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 拒绝的原因进状态行() {
        let j = OnceJob::new("ws-no-such-daemon", "x.toml");
        assert!(j.start(true).contains("常驻运行"));
        assert!(j.status().contains("常驻运行"), "守护开着时点按钮，要看得到为什么没动");
        assert!(j.start(false).starts_with('✗'));
        assert!(j.status().starts_with('✗'));
        assert!(!j.running());
    }
}
