//! 订单流信号（docs/08 F4a）：从 cockpit 已有的逐笔成交（FS 实时 / 回测 replay）算 CVD +
//! 滚动成交不平衡 + 价/CVD 背离 —— 自包含（无需外部发布器），叠在悬浮读数里。

use std::collections::VecDeque;

use exchange::depth::Depth;

#[derive(Clone, Debug, Default)]
pub struct FlowState {
    pub cvd: f64,        // 累计主动买 − 卖 量
    pub imbalance: f64,  // 滚动窗(≤100)成交不平衡 ∈[-1,1]
    pub last_price: f64,
    pub divergence: i8,  // 0 无 / 1 看涨背离 / -1 看跌背离
    // F4b：盘口侧（从 Depth 精确算）：spread + 盘口不平衡
    pub best_bid: f64,
    pub best_ask: f64,
    pub spread: f64,
    pub book_imb: f64, // 盘口前 N 档量不平衡 ∈[-1,1]
    // F4b：最优档吸收/撤补（cockpit 侧近似——仅最优档；精确全档版需 orderbook 引擎，docs/08）
    pub absorbed_bid: f64, // 当前最优买价持稳期间被主动卖「吃」掉的量
    pub absorbed_ask: f64,
    pub pulled_bid: f64, // 同期被撤/抽走的量（= 减少量 − 成交量）
    pub pulled_ask: f64,
    prev_bid_px: f64,
    prev_bid_sz: f64,
    prev_ask_px: f64,
    prev_ask_sz: f64,
    pend_sell: f64, // 自上次盘口更新起，命中 bid 的主动卖量
    pend_buy: f64,  // 命中 ask 的主动买量
    roll: VecDeque<(f64, bool)>,  // (qty, is_sell)
    hist: VecDeque<(f64, f64)>,   // (price, cvd) 背离检测
}

impl FlowState {
    pub fn apply(&mut self, price: f64, qty: f64, is_sell: bool) {
        self.cvd += if is_sell { -qty } else { qty };
        self.last_price = price;
        // F4b：主动卖命中 bid、主动买命中 ask；攒到下次盘口更新做归因。
        if is_sell {
            self.pend_sell += qty;
        } else {
            self.pend_buy += qty;
        }

        self.roll.push_back((qty, is_sell));
        if self.roll.len() > 100 {
            self.roll.pop_front();
        }
        let (mut b, mut s) = (0.0f64, 0.0f64);
        for &(q, sell) in &self.roll {
            if sell {
                s += q;
            } else {
                b += q;
            }
        }
        self.imbalance = if b + s > 0.0 { (b - s) / (b + s) } else { 0.0 };

        self.hist.push_back((price, self.cvd));
        if self.hist.len() > 300 {
            self.hist.pop_front();
        }
        self.divergence = detect_divergence(&self.hist);
    }

    /// F4b：从盘口快照算 spread + 盘口不平衡（精确）；并做最优档吸收/撤补归因（近似）。
    pub fn apply_depth(&mut self, depth: &Depth) {
        const N: usize = 10;
        // BTreeMap 按价升序：最高买 = 末，最低卖 = 首。
        let bb = depth.bids.iter().next_back().map(|(p, q)| (p.to_f32() as f64, f32::from(*q) as f64));
        let ba = depth.asks.iter().next().map(|(p, q)| (p.to_f32() as f64, f32::from(*q) as f64));
        if let Some((bp, _)) = bb {
            self.best_bid = bp;
        }
        if let Some((ap, _)) = ba {
            self.best_ask = ap;
        }
        if self.best_ask > 0.0 && self.best_bid > 0.0 {
            self.spread = (self.best_ask - self.best_bid).max(0.0);
        }
        let bid_vol: f64 = depth.bids.iter().rev().take(N).map(|(_, q)| f32::from(*q) as f64).sum();
        let ask_vol: f64 = depth.asks.iter().take(N).map(|(_, q)| f32::from(*q) as f64).sum();
        self.book_imb = if bid_vol + ask_vol > 0.0 {
            (bid_vol - ask_vol) / (bid_vol + ask_vol)
        } else {
            0.0
        };

        // ── 最优档吸收/撤补归因（近似）──
        // 价格持稳期间，最优档减少量 = 成交(被吃,吸收) + 撤单(pull)；价格变动则该档结束、重置累计。
        if let Some((bp, bs)) = bb {
            if (bp - self.prev_bid_px).abs() < f64::EPSILON {
                let removed = (self.prev_bid_sz - bs).max(0.0);
                let traded = self.pend_sell.min(removed);
                self.absorbed_bid += traded;
                self.pulled_bid += (removed - traded).max(0.0);
            } else {
                self.absorbed_bid = 0.0;
                self.pulled_bid = 0.0;
            }
            self.prev_bid_px = bp;
            self.prev_bid_sz = bs;
        }
        if let Some((ap, as_)) = ba {
            if (ap - self.prev_ask_px).abs() < f64::EPSILON {
                let removed = (self.prev_ask_sz - as_).max(0.0);
                let traded = self.pend_buy.min(removed);
                self.absorbed_ask += traded;
                self.pulled_ask += (removed - traded).max(0.0);
            } else {
                self.absorbed_ask = 0.0;
                self.pulled_ask = 0.0;
            }
            self.prev_ask_px = ap;
            self.prev_ask_sz = as_;
        }
        self.pend_sell = 0.0;
        self.pend_buy = 0.0;
    }
}

/// 价创新高而 CVD 不配合 = 看跌(-1)；价创新低而 CVD 走高 = 看涨(1)；否则 0。
fn detect_divergence(hist: &VecDeque<(f64, f64)>) -> i8 {
    let v: Vec<(f64, f64)> = hist.iter().copied().collect();
    let n = v.len();
    if n < 40 {
        return 0;
    }
    let split = n * 6 / 10;
    let (early, recent) = (&v[..split], &v[split..]);
    let by_price = |a: &&(f64, f64), b: &&(f64, f64)| a.0.partial_cmp(&b.0).unwrap();
    let (e_hi_p, e_hi_cvd) = *early.iter().max_by(by_price).unwrap();
    let (e_lo_p, e_lo_cvd) = *early.iter().min_by(by_price).unwrap();
    let (r_hi_p, r_hi_cvd) = *recent.iter().max_by(by_price).unwrap();
    let (r_lo_p, r_lo_cvd) = *recent.iter().min_by(by_price).unwrap();

    if r_hi_p > e_hi_p && r_hi_cvd < e_hi_cvd {
        -1 // 价新高、CVD 没跟 → 看跌背离
    } else if r_lo_p < e_lo_p && r_lo_cvd > e_lo_cvd {
        1 // 价新低、CVD 没跟 → 看涨背离
    } else {
        0
    }
}
