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
