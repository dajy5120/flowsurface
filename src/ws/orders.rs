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

/// 一笔订单明细（成交回报）：右侧面板「订单明细」表用（时间/方向/类型/金额/收益/手续费/净收益）。
#[derive(Clone, Debug)]
pub struct Trade {
    pub ts: u64,
    pub side: u8,            // 1=买 2=卖
    pub order_type: String,  // LIMIT / MARKET …
    pub amount: f64,         // 成交额 = px*qty（USDT）
    pub gross: f64,          // 本笔毛收益（开仓=0）
    pub fee: f64,            // 手续费
    pub net: f64,            // 净收益 = gross - fee
}

/// 一笔活动挂单（resting limit order）：在主图挂单价位画线标注（§11.1）。
#[derive(Clone, Debug)]
pub struct WorkingOrder {
    pub order_id: String,
    pub side: u8, // 1=买 2=卖
    pub price: f64,
    pub qty: f64,
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
    pub realized: f64,    // 累计毛收益（策略发）
    pub unrealized: Option<f64>,
    pub working: HashMap<String, WorkingOrder>, // 活动挂单（OrderAccepted 入，成交/撤单出）
    pub has_summary: bool,
    pub trades: Vec<Trade>, // 逐笔订单明细（近窗裁剪，供面板表）
    pub capital: f64,       // 本金（策略 capital 字段）
    pub realized_net: f64,  // 累计净收益 = Σ(本笔毛 - 手续费)
    pub fee_total: f64,     // 累计手续费
}

const FILL_CAP: usize = 500;
const TRADE_CAP: usize = 200;

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

/// 图上活动挂单（§11.1）：kline 画布在挂单价位画虚线 + 标注。
static CHART_WORKING: OnceLock<Mutex<Vec<WorkingOrder>>> = OnceLock::new();

/// 覆盖图上活动挂单（每次 `WsOrders` 聚合后调用）。
pub fn publish_chart_working(working: &HashMap<String, WorkingOrder>) {
    let lock = CHART_WORKING.get_or_init(|| Mutex::new(Vec::new()));
    if let Ok(mut g) = lock.lock() {
        g.clear();
        g.extend(working.values().cloned());
    }
}

/// 读图上活动挂单快照（每帧调用，挂单数极少）。
pub fn chart_working_snapshot() -> Vec<WorkingOrder> {
    CHART_WORKING
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
/// 买卖方向码：1=买 2=卖 0=未知（兼容 order_side / side 两种字段名）。
fn side_code(m: &[(rmpv::Value, rmpv::Value)]) -> u8 {
    match map_get(m, "order_side")
        .or_else(|| map_get(m, "side"))
        .and_then(as_str)
        .as_deref()
    {
        Some("BUY") => 1,
        Some("SELL") => 2,
        _ => 0,
    }
}
/// 订单标识（兼容 order_id / client_order_id）。
fn order_id(m: &[(rmpv::Value, rmpv::Value)]) -> Option<String> {
    map_get(m, "order_id")
        .or_else(|| map_get(m, "client_order_id"))
        .and_then(as_str)
        .filter(|s| !s.is_empty())
}

impl OrderState {
    /// 把一条解码后的 msgpack 事件并入状态。
    fn apply(&mut self, payload: &[u8]) {
        let Ok(val) = rmpv::decode::read_value(&mut &payload[..]) else { return };
        let rmpv::Value::Map(m) = val else { return };
        let typ = map_get(&m, "type").and_then(as_str).unwrap_or_default();
        match typ.as_str() {
            "OrderFilled" => {
                let side = side_code(&m);
                let px = map_get(&m, "last_px").and_then(as_f64).unwrap_or(0.0);
                let qty = map_get(&m, "last_qty").and_then(as_f64).unwrap_or(0.0);
                let ts = map_get(&m, "ts_event").and_then(as_f64).unwrap_or(0.0) as u64 / 1_000_000;
                if side == 1 {
                    self.n_buy += 1;
                } else if side == 2 {
                    self.n_sell += 1;
                }
                // 成交即离场：清掉对应活动挂单（若带 order_id）。
                if let Some(oid) = order_id(&m) {
                    self.working.remove(&oid);
                }
                self.fills.push(Fill { side, px, qty, ts });
                if self.fills.len() > FILL_CAP {
                    let drain = self.fills.len() - FILL_CAP;
                    self.fills.drain(0..drain);
                }
                // 逐笔订单明细：金额/收益/手续费/净收益。
                let fee = map_get(&m, "commission").and_then(as_f64).unwrap_or(0.0);
                let gross = map_get(&m, "trade_pnl").and_then(as_f64).unwrap_or(0.0);
                let order_type =
                    map_get(&m, "order_type").and_then(as_str).unwrap_or_else(|| "—".into());
                let net = gross - fee;
                self.fee_total += fee;
                self.realized_net += net;
                self.trades.push(Trade { ts, side, order_type, amount: px * qty, gross, fee, net });
                if self.trades.len() > TRADE_CAP {
                    let drain = self.trades.len() - TRADE_CAP;
                    self.trades.drain(0..drain);
                }
            }
            // 活动挂单生命周期（§11.1 实盘订单标注）。
            "OrderAccepted" | "OrderUpdated" | "OrderInitialized" => {
                let price = map_get(&m, "price").and_then(as_f64).unwrap_or(0.0);
                let qty = map_get(&m, "quantity")
                    .or_else(|| map_get(&m, "qty"))
                    .and_then(as_f64)
                    .unwrap_or(0.0);
                if let (Some(oid), true) = (order_id(&m), price > 0.0) {
                    self.working.insert(
                        oid.clone(),
                        WorkingOrder { order_id: oid, side: side_code(&m), price, qty },
                    );
                }
            }
            "OrderCanceled" | "OrderRejected" | "OrderExpired" | "OrderDenied" => {
                if let Some(oid) = order_id(&m) {
                    self.working.remove(&oid);
                }
            }
            "PositionOpened" | "PositionChanged" | "PositionClosed" => {
                self.has_summary = true;
                if let Some(c) = map_get(&m, "capital").and_then(as_f64) {
                    self.capital = c;
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 把 (key,val) 串编码成 apply() 所需的 msgpack payload。
    fn ev(pairs: &[(&str, &str)]) -> Vec<u8> {
        let m = rmpv::Value::Map(
            pairs
                .iter()
                .map(|(k, v)| (rmpv::Value::from(*k), rmpv::Value::from(*v)))
                .collect(),
        );
        let mut buf = Vec::new();
        rmpv::encode::write_value(&mut buf, &m).unwrap();
        buf
    }

    #[test]
    fn working_order_accept_then_fill_clears() {
        let mut st = OrderState::default();
        st.apply(&ev(&[
            ("type", "OrderAccepted"), ("order_id", "o1"),
            ("order_side", "BUY"), ("price", "64000.0"), ("quantity", "0.010"),
        ]));
        assert_eq!(st.working.len(), 1);
        let w = &st.working["o1"];
        assert_eq!(w.side, 1);
        assert_eq!(w.price, 64000.0);
        // 成交（带 order_id）→ 挂单清除 + 成交入列
        st.apply(&ev(&[
            ("type", "OrderFilled"), ("order_id", "o1"),
            ("order_side", "BUY"), ("last_px", "64000.0"), ("last_qty", "0.010"),
        ]));
        assert!(st.working.is_empty(), "成交后挂单应清除");
        assert_eq!(st.fills.len(), 1);
    }

    #[test]
    fn working_order_cancel_clears() {
        let mut st = OrderState::default();
        st.apply(&ev(&[
            ("type", "OrderAccepted"), ("order_id", "o2"),
            ("order_side", "SELL"), ("price", "65000.0"), ("quantity", "0.010"),
        ]));
        assert_eq!(st.working["o2"].side, 2);
        st.apply(&ev(&[("type", "OrderCanceled"), ("order_id", "o2")]));
        assert!(st.working.is_empty(), "撤单后挂单应清除");
    }

    #[test]
    fn working_order_chase_replaces() {
        // limit-chase：撤旧挂新（不同 oid）→ 只剩新挂单
        let mut st = OrderState::default();
        st.apply(&ev(&[
            ("type", "OrderAccepted"), ("order_id", "old"),
            ("order_side", "BUY"), ("price", "63990.0"), ("quantity", "0.010"),
        ]));
        st.apply(&ev(&[("type", "OrderCanceled"), ("order_id", "old")]));
        st.apply(&ev(&[
            ("type", "OrderAccepted"), ("order_id", "new"),
            ("order_side", "BUY"), ("price", "63999.0"), ("quantity", "0.010"),
        ]));
        assert_eq!(st.working.len(), 1);
        assert!(st.working.contains_key("new"));
        assert_eq!(st.working["new"].price, 63999.0);
    }
}
