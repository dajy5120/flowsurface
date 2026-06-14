//! 读 Redis `ws:bt:{run}:trades`（回测逐笔成交流，P1 TradeTap 发布）→ 喂 FS 图。契约见 docs/07/08。

use redis::streams::{StreamReadOptions, StreamReadReply};
use redis::{Client, Commands, Connection, RedisResult};

pub fn bt_trades_key(run_id: &str) -> String {
    format!("ws:bt:{run_id}:trades")
}

/// 一笔回测成交。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BtTrade {
    pub px: f64,
    pub qty: f64,
    pub side: u8, // 1=主动买 2=主动卖 0=未知
    pub ts: u64,  // 毫秒
}

/// 跟读某 run 的回测成交流（从 "0" 读全程，可回放）。
pub struct BtTradeConsumer {
    conn: Connection,
    key: String,
    last_id: String,
    block_ms: usize,
}

impl BtTradeConsumer {
    pub fn connect(redis_url: &str, run_id: &str) -> RedisResult<Self> {
        Ok(Self {
            conn: Client::open(redis_url)?.get_connection()?,
            key: bt_trades_key(run_id),
            last_id: "0".to_string(),
            block_ms: 300,
        })
    }

    pub fn with_block_ms(mut self, ms: usize) -> Self {
        self.block_ms = ms;
        self
    }

    /// 阻塞最多 block_ms 拉一批；推进游标；超时返回空。
    pub fn poll(&mut self) -> RedisResult<Vec<BtTrade>> {
        let opts = StreamReadOptions::default().block(self.block_ms).count(2000);
        let reply: StreamReadReply = self.conn.xread_options(
            std::slice::from_ref(&self.key),
            std::slice::from_ref(&self.last_id),
            &opts,
        )?;
        let mut out = Vec::new();
        for skey in reply.keys {
            if let Some(last) = skey.ids.last() {
                self.last_id = last.id.clone();
            }
            for entry in skey.ids {
                let px = entry.get::<String>("px").and_then(|s| s.parse::<f64>().ok());
                let qty = entry.get::<String>("qty").and_then(|s| s.parse::<f64>().ok());
                let side = entry.get::<String>("side").and_then(|s| s.parse::<u8>().ok());
                let ts = entry.get::<String>("ts").and_then(|s| s.parse::<u64>().ok());
                if let (Some(px), Some(qty), Some(side), Some(ts)) = (px, qty, side, ts) {
                    out.push(BtTrade { px, qty, side, ts });
                }
            }
        }
        Ok(out)
    }
}
