//! 订单流信号（docs/08 F4a）：从 cockpit 已有的逐笔成交（FS 实时 / 回测 replay）算 CVD +
//! 滚动成交不平衡 + 价/CVD 背离 —— 自包含（无需外部发布器），叠在悬浮读数里。

use std::collections::VecDeque;

#[derive(Clone, Debug, Default)]
pub struct FlowState {
    pub cvd: f64,        // 累计主动买 − 卖 量
    pub imbalance: f64,  // 滚动窗(≤100)成交不平衡 ∈[-1,1]
    pub last_price: f64,
    pub divergence: i8,  // 0 无 / 1 看涨背离 / -1 看跌背离
    roll: VecDeque<(f64, bool)>,  // (qty, is_sell)
    hist: VecDeque<(f64, f64)>,   // (price, cvd) 背离检测
}

impl FlowState {
    pub fn apply(&mut self, price: f64, qty: f64, is_sell: bool) {
        self.cvd += if is_sell { -qty } else { qty };
        self.last_price = price;

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
