//! 读 Redis 通道① `trader-{run}:stream:events.*`（OrderFilled / Position* 的 msgpack）→ 聚合成
//! 订单/PnL 状态，供 cockpit 叠加显示（docs/03 §B.5 wire 契约 / docs/08 F3）。
//!
//! 用 rmpv 动态解 msgpack（字段类型混合：价/量/ts 为字符串，avg_px/signed_qty 为浮点）。
//! 跟随 `ws:active_run` 的 run（回测/实盘态有 run_id），run 变更则重置。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use redis::streams::{StreamReadOptions, StreamReadReply};
use redis::{Client, Commands, Connection, RedisResult};

use super::active_run::ActiveRunWatcher;

/// 一笔成交标记。
#[derive(Clone, Copy, Debug)]
pub struct Fill {
    pub side: u8, // 1=买 2=卖
    pub px: f64,
    pub qty: f64,
    pub ts: u64,
}

/// 订单/PnL 聚合状态（cockpit 角标读数 + ▲▼ 列表）。
#[derive(Clone, Debug, Default)]
pub struct OrderState {
    pub run_id: String,
    pub fills: Vec<Fill>, // 近窗（上限裁剪）
    pub n_buy: usize,
    pub n_sell: usize,
    pub pos_side: String, // LONG/SHORT/FLAT
    pub net_qty: f64,
    pub avg_px: f64,
    pub realized: f64,
    pub unrealized: Option<f64>,
    pub has_summary: bool,
}

const FILL_CAP: usize = 500;

/// 进程级图上成交标记缓存（docs/08 F3b）：main.rs 的 `WsOrders` 处理写入，
/// kline.rs 的 canvas draw 读取，在蜡烛图上画 ▲（买）/▼（卖）。
/// GPUI/iced 不共享进程，这里只在 cockpit 进程内做 App↔图表的轻量旁路（不改 FS 的 71 处 ContentKind）。
static CHART_FILLS: OnceLock<Mutex<Vec<Fill>>> = OnceLock::new();

/// 覆盖图上成交标记（run 切换时 `fills` 会被上游重置 → 这里整体替换即可）。
pub fn publish_chart_fills(fills: &[Fill]) {
    let lock = CHART_FILLS.get_or_init(|| Mutex::new(Vec::new()));
    if let Ok(mut g) = lock.lock() {
        g.clear();
        g.extend_from_slice(fills);
    }
}

/// 读当前图上成交标记快照（每帧调用，≤500 条，clone 成本可忽略）。
pub fn chart_fills_snapshot() -> Vec<Fill> {
    CHART_FILLS
        .get()
        .and_then(|m| m.lock().ok().map(|g| g.clone()))
        .unwrap_or_default()
}

/// 图上当前仓位/费后 PnL（§11.1）：在主图 avg_px 处画持仓线 + 读数。
#[derive(Clone, Debug, Default)]
pub struct ChartPosition {
    pub side: String, // LONG/SHORT/FLAT
    pub net_qty: f64,
    pub avg_px: f64,
    pub realized: f64,           // 费后已实现 PnL（引擎含手续费）
    pub unrealized: Option<f64>, // 费后未实现 PnL（盯市）
}

static CHART_POSITION: OnceLock<Mutex<ChartPosition>> = OnceLock::new();

/// 覆盖图上仓位读数（每次 `WsOrders` 聚合后调用）。
pub fn publish_chart_position(st: &OrderState) {
    let lock = CHART_POSITION.get_or_init(|| Mutex::new(ChartPosition::default()));
    if let Ok(mut g) = lock.lock() {
        *g = ChartPosition {
            side: st.pos_side.clone(),
            net_qty: st.net_qty,
            avg_px: st.avg_px,
            realized: st.realized,
            unrealized: st.unrealized,
        };
    }
}

/// 读图上仓位快照（每帧调用）。
pub fn chart_position_snapshot() -> ChartPosition {
    CHART_POSITION
        .get()
        .and_then(|m| m.lock().ok().map(|g| g.clone()))
        .unwrap_or_default()
}

// ── rmpv 取字段助手 ──
fn map_get<'a>(m: &'a [(rmpv::Value, rmpv::Value)], key: &str) -> Option<&'a rmpv::Value> {
    m.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v)
}
fn as_f64(v: &rmpv::Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_i64().map(|i| i as f64)).or_else(|| {
        // 字符串数值（如 "65000.10" 或 "1170.00 USDT"）取首段。
        v.as_str().and_then(|s| s.split_whitespace().next()).and_then(|s| s.parse::<f64>().ok())
    })
}
fn as_str(v: &rmpv::Value) -> Option<String> {
    v.as_str().map(|s| s.to_string())
}

impl OrderState {
    /// 把一条解码后的 msgpack 事件并入状态。
    fn apply(&mut self, payload: &[u8]) {
        let Ok(val) = rmpv::decode::read_value(&mut &payload[..]) else { return };
        let rmpv::Value::Map(m) = val else { return };
        let typ = map_get(&m, "type").and_then(as_str).unwrap_or_default();
        match typ.as_str() {
            "OrderFilled" => {
                let side = match map_get(&m, "order_side").and_then(as_str).as_deref() {
                    Some("BUY") => 1,
                    Some("SELL") => 2,
                    _ => 0,
                };
                let px = map_get(&m, "last_px").and_then(as_f64).unwrap_or(0.0);
                let qty = map_get(&m, "last_qty").and_then(as_f64).unwrap_or(0.0);
                let ts = map_get(&m, "ts_event").and_then(as_f64).unwrap_or(0.0) as u64 / 1_000_000;
                if side == 1 {
                    self.n_buy += 1;
                } else if side == 2 {
                    self.n_sell += 1;
                }
                self.fills.push(Fill { side, px, qty, ts });
                if self.fills.len() > FILL_CAP {
                    let drain = self.fills.len() - FILL_CAP;
                    self.fills.drain(0..drain);
                }
            }
            "PositionOpened" | "PositionChanged" | "PositionClosed" => {
                self.has_summary = true;
                self.pos_side = map_get(&m, "side").and_then(as_str).unwrap_or_else(|| "FLAT".into());
                self.net_qty = map_get(&m, "signed_qty").and_then(as_f64).unwrap_or(0.0);
                self.avg_px = map_get(&m, "avg_px_open").and_then(as_f64).unwrap_or(0.0);
                self.realized = map_get(&m, "realized_pnl").and_then(as_f64).unwrap_or(0.0);
                self.unrealized = map_get(&m, "unrealized_pnl").and_then(as_f64);
                if typ == "PositionClosed" {
                    self.pos_side = "FLAT".into();
                    self.net_qty = 0.0;
                }
            }
            _ => {}
        }
    }
}

/// 跟读某 run 的 events.* 流（XREAD 所有 events.* key，从 "0" 起，可回放）。
pub struct EventsConsumer {
    conn: Connection,
    pattern: String,
    last_ids: HashMap<String, String>,
    block_ms: usize,
}

impl EventsConsumer {
    pub fn connect(redis_url: &str, run_id: &str) -> RedisResult<Self> {
        Ok(Self {
            conn: Client::open(redis_url)?.get_connection()?,
            pattern: format!("trader-{run_id}:stream:events.*"),
            last_ids: HashMap::new(),
            block_ms: 300,
        })
    }

    /// 拉一批所有 events.* 的新条目（payload 字节）。
    pub fn poll(&mut self) -> RedisResult<Vec<Vec<u8>>> {
        let keys: Vec<String> = self.conn.keys(&self.pattern)?;
        if keys.is_empty() {
            std::thread::sleep(Duration::from_millis(200));
            return Ok(vec![]);
        }
        for k in &keys {
            self.last_ids.entry(k.clone()).or_insert_with(|| "0".to_string());
        }
        let ids: Vec<String> = keys.iter().map(|k| self.last_ids[k].clone()).collect();
        let opts = StreamReadOptions::default().block(self.block_ms).count(2000);
        let reply: StreamReadReply = self.conn.xread_options(&keys, &ids, &opts)?;
        let mut out = Vec::new();
        for skey in reply.keys {
            for entry in skey.ids {
                if let Some(payload) = entry.get::<Vec<u8>>("payload") {
                    out.push(payload);
                }
                self.last_ids.insert(skey.key.clone(), entry.id);
            }
        }
        Ok(out)
    }
}

/// 订阅：跟随活动 run 读 events.*，聚合 OrderState，变化时发出快照（run 变更则重置）。
pub fn subscription(redis_url: String) -> iced::Subscription<OrderState> {
    use iced::futures::SinkExt;
    iced::Subscription::run_with(("ws-orders", redis_url), |(_, redis_url): &(&str, String)| {
        let redis_url = redis_url.clone();
        iced::stream::channel(
            64,
            move |mut output: iced::futures::channel::mpsc::Sender<OrderState>| async move {
                let (tx, mut rx) = tokio::sync::mpsc::channel::<OrderState>(16);
                std::thread::spawn(move || {
                    let mut watcher = ActiveRunWatcher::connect(&redis_url).ok();
                    let mut active: Option<String> = None;
                    let mut consumer: Option<EventsConsumer> = None;
                    let mut state = OrderState::default();
                    loop {
                        let want = watcher
                            .as_mut()
                            .and_then(|w| w.poll().ok().flatten())
                            .filter(|ar| ar.mode == "backtest" || ar.mode == "live")
                            .map(|ar| ar.run_id);
                        if want != active {
                            active = want.clone();
                            state = OrderState::default();
                            state.run_id = active.clone().unwrap_or_default();
                            consumer = active
                                .as_ref()
                                .and_then(|r| EventsConsumer::connect(&redis_url, r).ok());
                            let _ = tx.blocking_send(state.clone());
                        }
                        let Some(c) = consumer.as_mut() else {
                            std::thread::sleep(Duration::from_millis(300));
                            continue;
                        };
                        match c.poll() {
                            Ok(batch) if !batch.is_empty() => {
                                for p in &batch {
                                    state.apply(p);
                                }
                                if tx.blocking_send(state.clone()).is_err() {
                                    break;
                                }
                            }
                            Ok(_) => {}
                            Err(_) => {
                                consumer = None;
                                active = None;
                                std::thread::sleep(Duration::from_millis(500));
                            }
                        }
                    }
                });
                while let Some(st) = rx.recv().await {
                    if output.send(st).await.is_err() {
                        break;
                    }
                }
            },
        )
    })
}
