//! 进程级 WealthSpring 读数快照（docs/08）。
//!
//! App 的 `ws_*` 状态在 `view()` 顶部 [`publish`] 到此，供原生 WealthSpring dockable pane
//! （`Content::WealthSpring`）渲染——pane 视图深嵌在 dashboard 内、拿不到 `&App`，
//! 沿用 `orders::CHART_FILLS` 的旁路模式（GPUI/iced 不共享进程，仅 cockpit 进程内旁路）。

use std::sync::{Mutex, OnceLock};

use super::factory::PoolMember;

/// 给 pane 渲染用的合并快照（镜像原悬浮框的全部字段）。
#[derive(Clone, Default)]
pub struct Readout {
    pub mode: String,
    pub run_id: String,

    // 订单 / PnL（F3a，orders）
    pub has_orders: bool,
    pub pos_side: String,
    pub net_qty: f64,
    pub avg_px: f64,
    pub realized: f64,
    pub unrealized: Option<f64>,
    pub n_fills: usize,
    pub n_buy: usize,
    pub n_sell: usize,

    // 订单流（F4a，flow）
    pub cvd: f64,
    pub imbalance: f64,
    pub divergence: i8,
    pub book_imb: f64,
    pub spread: f64,
    pub absorbed_bid: f64,
    pub absorbed_ask: f64,
    pub pulled_bid: f64,
    pub pulled_ask: f64,

    // Factory 现役池（F4c，factory）
    pub fac_alphas: u64,
    pub fac_n_pool: u64,
    pub fac_evals: u64,
    pub pool: Vec<PoolMember>,

    // 引擎信号（F4b–d 精确版，signals）
    pub has_signals: bool,
    pub sess_traded_bid: f64,
    pub sess_traded_ask: f64,
    pub sess_pulled_bid: f64,
    pub sess_pulled_ask: f64,
    pub iceberg_bid: f64,
    pub iceberg_ask: f64,
    pub depth_bid: u64,
    pub depth_ask: u64,
    pub sig_combo: f64,
    pub sig_n_combo: u64,
    pub sig_n_pool: u64,
}

static READOUT: OnceLock<Mutex<Readout>> = OnceLock::new();

/// 由 `App::view()` 每帧调用：把当前 ws_* 状态整体替换进快照。
pub fn publish(r: Readout) {
    let lock = READOUT.get_or_init(|| Mutex::new(Readout::default()));
    if let Ok(mut g) = lock.lock() {
        *g = r;
    }
}

/// pane 渲染时读快照（clone 成本可忽略：池 ≤14 行）。
pub fn snapshot() -> Readout {
    READOUT
        .get()
        .and_then(|m| m.lock().ok().map(|g| g.clone()))
        .unwrap_or_default()
}
