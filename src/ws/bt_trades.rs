//! 读 Redis `ws:bt:{run}:trades`（逐笔成交流，P1 TradeTap 发布）→ 喂 FS 图。契约见 docs/07/08。
//!
//! ## 价量走定点（docs/28 §5.2）
//!
//! 生产端发两套字段，本模块**优先读 `px_raw`/`qty_raw`**（i64 @ 1e9 定点，本仓规范标度），
//! 读不到才退回十进制字符串 `px`/`qty`——流里可能还躺着旧格式的条目。
//!
//! 为什么非改不可：旧链路是
//! `Price(i128@1e16) → as_double() → 十进制字符串 → parse f64 → Price::from_f32(1e11)`。
//! 末尾那步是 **f32**（24 位尾数 ≈ 7 位有效数字）——实测 BTC 报价 63123.45 经它还原后
//! 偏 0.00078。BTC 的 tick 是 0.1 尚可容忍，换个更细 tick 或更高价的标的就不行了。
//! 三次表示转换、两次精度损失，一次都不报错。
//!
//! 定点直传只在本模块做一次整数换算，零损失。

use exchange::unit::{Price, Qty};
use redis::streams::{StreamReadOptions, StreamReadReply};
use redis::{Client, Commands, Connection, RedisResult};

/// 本仓规范定点标度（`wealthspring-ipc::FIXED_SCALAR`，docs/28 §2.2）。
pub const IPC_SCALE: i64 = 1_000_000_000;

pub fn bt_trades_key(run_id: &str) -> String {
    format!("ws:bt:{run_id}:trades")
}

/// 一笔成交。价量是 **i64 @ 1e9 定点**，不是浮点。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BtTrade {
    /// 价，1e9 定点。
    pub px_raw: i64,
    /// 量，1e9 定点。
    pub qty_raw: i64,
    pub side: u8, // 1=主动买 2=主动卖 0=未知
    pub ts: u64,  // 毫秒
}

impl BtTrade {
    /// → FlowSurface `Price`（1e11 定点）。1e9 → 1e11 就是 ×100，纯整数。
    ///
    /// `saturating_mul` 兜底：i64 在 1e11 标度下能表示到约 9200 万，
    /// 任何加密报价都远在其内；真溢出了宁可钉在边界，也别静默回绕成负价。
    #[inline]
    pub fn price(&self) -> Price {
        Price { units: self.px_raw.saturating_mul(100) }
    }

    /// → FlowSurface `Qty`（1e8 定点）。1e9 → 1e8 要**除**，故四舍五入而非截断。
    #[inline]
    pub fn qty(&self) -> Qty {
        let r = self.qty_raw;
        Qty { units: if r >= 0 { (r + 5) / 10 } else { (r - 5) / 10 } }
    }
}

/// 十进制字符串 → 1e9 定点。仅用于兼容旧条目（新生产端直接发 `*_raw`）。
fn decimal_to_raw(s: &str) -> Option<i64> {
    let v: f64 = s.parse().ok()?;
    Some((v * IPC_SCALE as f64).round() as i64)
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
                // 优先定点；退回十进制字符串只为兼容流里的旧条目。
                let px = entry
                    .get::<String>("px_raw")
                    .and_then(|s| s.parse::<i64>().ok())
                    .or_else(|| entry.get::<String>("px").as_deref().and_then(decimal_to_raw));
                let qty = entry
                    .get::<String>("qty_raw")
                    .and_then(|s| s.parse::<i64>().ok())
                    .or_else(|| entry.get::<String>("qty").as_deref().and_then(decimal_to_raw));
                let side = entry.get::<String>("side").and_then(|s| s.parse::<u8>().ok());
                let ts = entry.get::<String>("ts").and_then(|s| s.parse::<u64>().ok());
                if let (Some(px), Some(qty), Some(side), Some(ts)) = (px, qty, side, ts) {
                    out.push(BtTrade { px_raw: px, qty_raw: qty, side, ts });
                }
            }
        }
        Ok(out)
    }
}

/// 回测进度（docs/27 §12）：`ws:bt:{run}:progress`，由 `RunTap` 按回测时钟定期 SET。
#[derive(serde::Deserialize, Clone, Default, Debug)]
pub struct BtProgress {
    #[serde(default)]
    pub run_id: String,
    /// 0~100。
    #[serde(default)]
    pub pct: f32,
    /// 回测时钟（毫秒 epoch）——不是墙钟。
    #[serde(default)]
    pub clock_ms: u64,
    #[serde(default)]
    pub ticks: u64,
}

pub fn progress_key(run_id: &str) -> String {
    format!("ws:bt:{run_id}:progress")
}

/// 读某 run 的进度。读不到（还没开始 / 已过期）返回 None。
///
/// **用 GET 而非 stream**：面板只关心当前进度，不需要历史；key 带 1 小时过期，跑完也不留垃圾。
pub fn fetch_progress(redis_url: &str, run_id: &str) -> Option<BtProgress> {
    let client = Client::open(redis_url).ok()?;
    let mut conn = client.get_connection().ok()?;
    let raw: Option<String> = conn.get(progress_key(run_id)).ok()?;
    serde_json::from_str(&raw?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(px_raw: i64, qty_raw: i64) -> BtTrade {
        BtTrade { px_raw, qty_raw, side: 1, ts: 0 }
    }

    /// 1e9 → 1e11 是纯整数 ×100，**零损失**。
    ///
    /// 对照旧链路：`as_double() → 字符串 → f64 → Price::from_f32` 的末尾是 f32
    /// （24 位尾数 ≈ 7 位有效数字），实测 BTC 报价 63123.45 经它还原后偏 0.00078。
    #[test]
    fn 价格换算零损失() {
        // 63123.45 在 1e9 定点下
        let bt = t(63_123_450_000_000, 0);
        assert_eq!(bt.price().units, 6_312_345_000_000_000);
        // 反算回真实值必须完全相等
        assert_eq!(bt.price().units as f64 / 1e11, 63123.45);
    }

    /// 1e9 → 1e8 要**除**，故四舍五入而非截断——截断会让量系统性偏小。
    #[test]
    fn 数量换算四舍五入() {
        assert_eq!(t(0, 125_000_000).qty().units, 12_500_000); // 0.125 整除
        assert_eq!(t(0, 14).qty().units, 1); // 1.4 → 1
        assert_eq!(t(0, 15).qty().units, 2); // 1.5 → 2（不是截断的 1）
        assert_eq!(t(0, -15).qty().units, -2); // 负数对称
    }

    /// 溢出宁可钉边界也不静默回绕——回绕会把高价变成负价，图上完全看不出来。
    #[test]
    fn 价格溢出不回绕() {
        assert_eq!(t(i64::MAX, 0).price().units, i64::MAX);
        assert_eq!(t(i64::MIN, 0).price().units, i64::MIN);
    }

    /// 旧条目（只有十进制字符串）仍要能读——流里躺着的历史数据不会重发。
    #[test]
    fn 兼容旧的十进制字符串() {
        assert_eq!(decimal_to_raw("63123.45"), Some(63_123_450_000_000));
        assert_eq!(decimal_to_raw("0.125"), Some(125_000_000));
        assert_eq!(decimal_to_raw("坏值"), None);
    }

    #[test]
    fn 流键按_run_分开() {
        assert_eq!(bt_trades_key("BT-1"), "ws:bt:BT-1:trades");
    }
}
