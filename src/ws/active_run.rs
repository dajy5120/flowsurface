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
