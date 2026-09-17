//! 读 Redis `ws:active_run`（P0 起/停 run 时广播的活动 run）→ P2 三态切换。契约见 docs/03/07/08。

use redis::{Client, Commands, Connection, RedisResult};
use serde::Deserialize;

pub const ACTIVE_RUN_KEY: &str = "ws:active_run";

/// 活动 run（P0 → P2）。`mode`：`"backtest"` | `"live"` | `"stopped"`。
#[derive(Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ActiveRun {
    pub run_id: String,
    pub mode: String,
    #[serde(default)]
    pub symbol: String,
}

/// 持久连接轮询活动 run。
pub struct ActiveRunWatcher {
    conn: Connection,
}

impl ActiveRunWatcher {
    pub fn connect(redis_url: &str) -> RedisResult<Self> {
        Ok(Self { conn: Client::open(redis_url)?.get_connection()? })
    }

    /// 读当前活动 run（key 不存在 / 解析失败 → None）。
    pub fn poll(&mut self) -> RedisResult<Option<ActiveRun>> {
        let v: Option<String> = self.conn.get(ACTIVE_RUN_KEY)?;
        Ok(v.and_then(|s| serde_json::from_str(&s).ok()))
    }
}

/// 轮询 `ws:active_run`（500ms）→ 变化时发出当前活动 run（驱动 P2 三态切换）。
pub fn subscription(redis_url: String) -> iced::Subscription<Option<ActiveRun>> {
    use iced::futures::SinkExt;
    iced::Subscription::run_with(("ws-active-run", redis_url), |(_, redis_url): &(&str, String)| {
        let redis_url = redis_url.clone();
        iced::stream::channel(
            8,
            move |mut output: iced::futures::channel::mpsc::Sender<Option<ActiveRun>>| async move {
                let (tx, mut rx) = tokio::sync::mpsc::channel::<Option<ActiveRun>>(8);
                std::thread::spawn(move || {
                    let mut watcher = ActiveRunWatcher::connect(&redis_url).ok();
                    let mut last: Option<ActiveRun> = None;
                    loop {
                        // 对端存活探测：**每轮无条件做一次**。
                        // 原先唯一的检测点是 `tx.blocking_send(..).is_err()`，而它嵌在
                        // 「值发生变化」的分支里——值冻住时（恰恰是上游守护挂掉的表现）
                        // 这个线程永远发现不了订阅已被销毁，以固定间隔永久空转。
                        if tx.is_closed() {
                            break;
                        }
                        let cur = watcher.as_mut().and_then(|w| w.poll().ok().flatten());
                        if cur != last {
                            last = cur.clone();
                            if tx.blocking_send(cur).is_err() {
                                break;
                            }
                        }
                        std::thread::sleep(std::time::Duration::from_millis(500));
                    }
                });
                while let Some(ar) = rx.recv().await {
                    if output.send(ar).await.is_err() {
                        break;
                    }
                }
            },
        )
    })
}

use std::sync::{Mutex, OnceLock};

/// 进程级「当前活动 run」。App 收到 `ws:active_run` 变化时发布到这里。
///
/// pane 视图深嵌在 dashboard 内、拿不到 `&App`，沿用 `orders::CHART_FILLS` 的旁路模式。
/// **存在的理由**：多个面板要判断「手上这份数据属不属于当前正在跑的这一次」——
/// 没有这个判据，新回测跑完之前，面板会把上一次的结论当本次显示（docs/27 §12）。
static CURRENT: OnceLock<Mutex<Option<ActiveRun>>> = OnceLock::new();

pub fn publish_current(ar: Option<ActiveRun>) {
    let lock = CURRENT.get_or_init(|| Mutex::new(None));
    if let Ok(mut g) = lock.lock() {
        *g = ar;
    }
}

pub fn current() -> Option<ActiveRun> {
    CURRENT.get().and_then(|m| m.lock().ok().map(|g| g.clone()))?
}

/// 当前是否正跑着回测。
pub fn backtest_running() -> bool {
    current().is_some_and(|a| a.mode == "backtest")
}

/// `run_id` 是否就是当前活动的那次运行。空串（旧结果没有该字段）一律判否。
pub fn is_current_run(run_id: &str) -> bool {
    !run_id.is_empty() && current().is_some_and(|a| a.run_id == run_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ar(run: &str, mode: &str) -> ActiveRun {
        ActiveRun { run_id: run.into(), mode: mode.into(), symbol: "BTCUSDT".into() }
    }

    /// 全部断言合在一个测试里**是有意的**：`CURRENT` 是进程级全局，拆成多个测试会并行
    /// 互相踩（第一版就这么挂的——一个测试刚 publish，另一个把它清成 None）。
    #[test]
    fn 本次运行的判定() {
        // 旧结果没有 run_id → 空串 → 一律判非本次。判成「是」的话，运行期间面板会把
        // 上一次的结论当本次显示，数字看着正常却完全不相干。
        publish_current(Some(ar("BT-1", "backtest")));
        assert!(!is_current_run(""), "空 run_id 不该判为本次");
        assert!(is_current_run("BT-1"));
        assert!(!is_current_run("BT-2"));
        assert!(backtest_running());

        // 实盘/停止态都不算「回测进行中」——那个判据只用来决定回测报告面板是否显示占位。
        publish_current(Some(ar("LIVE-1", "live")));
        assert!(!backtest_running());
        publish_current(Some(ar("BT-5", "stopped")));
        assert!(!backtest_running());

        // 没有活动 run 时一律判非本次。
        publish_current(None);
        assert!(!is_current_run("BT-1"));
        assert!(!backtest_running());
    }
}
