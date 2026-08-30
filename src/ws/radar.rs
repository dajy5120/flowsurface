//! 全市场雷达面板的交互消息与视图状态（docs/22 P0b，表达形式对齐 TradingView）。
//!
//! 守护默认**不随开机自启**（同 recorder/C4/预测，见 c682944），由本面板按钮控制启停。
//!
//! 视图状态放**进程级静态**而不是 pane 状态：同一份口径对所有雷达 pane 生效，
//! 换 pane 不用重设一遍。

use std::sync::Mutex;

use super::radar_readout::{RadarRow, N_WIN};

/// 树图的面积口径（对应 TradingView 热图的 "Size by"）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeBy {
    /// 24h 成交额——加密没有市值口径可用，成交额是最接近「重要性」的量。
    Turnover,
    /// 等权：每格一样大。看结构不看体量时用。
    Equal,
}

/// 树图的颜色口径（对应 "Color by"）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorBy {
    /// 波动归一化的涨跌速度 z——跨标的可比（docs/22 §3）。
    SpeedZ,
    /// 裸涨跌幅。**跨标的不可比**，留作对照。
    Change,
    /// 成交额异常度。只有正尾可解读（docs/22 §3）。
    VolZ,
}

/// 树图分组（对应 TradingView 股票热图按板块分组）。加密没有板块，
/// 能用的是 venue——顺带解决同一标的在 spot/linear 各出一格、面积算重的问题。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupBy {
    None,
    Venue,
}

/// Screener 的列组（对应 TradingView 筛选器顶部的 Overview/Performance/… 标签）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnSet {
    Overview,
    Performance,
    Speed,
    Volume,
}

/// 排序键。`Ret`/`Z` 带窗口下标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Symbol,
    Venue,
    Price,
    Turnover,
    Ret(usize),
    Z(usize),
    VolZ,
    CntZ,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewState {
    pub win: usize,
    pub size_by: SizeBy,
    pub color_by: ColorBy,
    pub group_by: GroupBy,
    pub cols: ColumnSet,
    pub sort: SortKey,
    /// true = 降序。点同一列再点一次翻向。
    pub desc: bool,
}

impl ViewState {
    pub const DEFAULT: Self = Self {
        win: 1, // 5m
        size_by: SizeBy::Turnover,
        color_by: ColorBy::SpeedZ,
        group_by: GroupBy::None,
        cols: ColumnSet::Overview,
        sort: SortKey::Z(1),
        desc: true,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadarMsg {
    Start,
    Stop,
    Refresh,
    SetWindow(usize),
    SetSizeBy(SizeBy),
    SetColorBy(ColorBy),
    SetGroupBy(GroupBy),
    SetColumns(ColumnSet),
    /// 点列头：同列则翻向，异列则换列并回到降序（数值列降序更符合「看榜」的直觉）。
    SortBy(SortKey),
}

static ACTION_MSG: Mutex<String> = Mutex::new(String::new());
static VIEW: Mutex<ViewState> = Mutex::new(ViewState::DEFAULT);

pub fn action_message() -> String {
    ACTION_MSG.lock().map(|m| m.clone()).unwrap_or_default()
}

pub fn view() -> ViewState {
    VIEW.lock().map(|v| *v).unwrap_or(ViewState::DEFAULT)
}

/// 纯状态迁移（可单测）：不碰 systemd、不碰全局锁。
pub fn apply(v: ViewState, msg: RadarMsg) -> ViewState {
    let mut v = v;
    match msg {
        RadarMsg::SetWindow(i) => {
            let i = i.min(N_WIN - 1);
            // 排序键跟着窗口走：换窗口后还按旧窗口排，榜单和高亮的列对不上
            v.sort = match v.sort {
                SortKey::Ret(_) => SortKey::Ret(i),
                SortKey::Z(_) => SortKey::Z(i),
                other => other,
            };
            v.win = i;
        }
        RadarMsg::SetSizeBy(s) => v.size_by = s,
        RadarMsg::SetColorBy(c) => v.color_by = c,
        RadarMsg::SetGroupBy(g) => v.group_by = g,
        RadarMsg::SetColumns(c) => v.cols = c,
        RadarMsg::SortBy(k) => {
            if v.sort == k {
                v.desc = !v.desc;
            } else {
                v.sort = k;
                v.desc = !matches!(k, SortKey::Symbol | SortKey::Venue);
            }
        }
        RadarMsg::Start | RadarMsg::Stop | RadarMsg::Refresh => {}
    }
    v
}

pub fn handle(msg: RadarMsg) {
    use super::radar_readout as ro;
    let m = match msg {
        RadarMsg::Start => ro::radar_start(),
        RadarMsg::Stop => ro::radar_stop(),
        RadarMsg::Refresh => {
            ro::request_refresh();
            String::new()
        }
        other => {
            if let Ok(mut g) = VIEW.lock() {
                *g = apply(*g, other);
            }
            String::new()
        }
    };
    if let Ok(mut g) = ACTION_MSG.lock() {
        *g = m;
    }
}

/// 取某行在某个排序键上的数值。`None` 一律沉底（不论升降序）。
pub fn key_value(r: &RadarRow, k: SortKey) -> Option<f64> {
    match k {
        SortKey::Symbol | SortKey::Venue => None,
        SortKey::Price => Some(r.price),
        SortKey::Turnover => Some(r.quote_vol_24h),
        SortKey::Ret(i) => r.ret[i],
        SortKey::Z(i) => r.z_ret[i],
        SortKey::VolZ => r.z_vol,
        SortKey::CntZ => r.z_cnt,
    }
}

/// 按当前口径排序，返回行下标。
///
/// 缺值恒沉底：把「还没热身」排在榜首等于用空数据占据最显眼的位置。
pub fn order(rows: &[RadarRow], v: ViewState) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..rows.len()).collect();
    idx.sort_by(|&a, &b| {
        let (ra, rb) = (&rows[a], &rows[b]);
        let ord = match v.sort {
            SortKey::Symbol => ra.symbol.cmp(&rb.symbol),
            SortKey::Venue => ra.venue.cmp(&rb.venue).then(ra.symbol.cmp(&rb.symbol)),
            k => {
                let (va, vb) = (key_value(ra, k), key_value(rb, k));
                match (va, vb) {
                    (Some(x), Some(y)) => {
                        x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal)
                    }
                    // 缺值沉底：升序时也不能让它冒到最前
                    (Some(_), None) => return std::cmp::Ordering::Less,
                    (None, Some(_)) => return std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                }
            }
        };
        let ord = if v.desc { ord.reverse() } else { ord };
        // 同分按标的定序，否则每次刷新表格顺序会抖
        ord.then_with(|| ra.symbol.cmp(&rb.symbol).then(ra.venue.cmp(&rb.venue)))
    });
    idx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(sym: &str, venue: &str, z5: Option<f64>, ret5: Option<f64>, tv: f64) -> RadarRow {
        let mut r = RadarRow {
            symbol: sym.into(),
            venue: venue.into(),
            quote_vol_24h: tv,
            sigma_ok: z5.is_some(),
            ..Default::default()
        };
        r.z_ret[1] = z5;
        r.ret[1] = ret5;
        r
    }

    #[test]
    fn clicking_same_column_toggles_direction() {
        let v = ViewState::DEFAULT;
        assert!(v.desc);
        let v = apply(v, RadarMsg::SortBy(SortKey::Z(1)));
        assert!(!v.desc, "同列再点应翻向");
        let v = apply(v, RadarMsg::SortBy(SortKey::Z(1)));
        assert!(v.desc);
    }

    #[test]
    fn switching_column_resets_to_natural_direction() {
        let v = apply(ViewState::DEFAULT, RadarMsg::SortBy(SortKey::Z(1))); // 变升序
        let v = apply(v, RadarMsg::SortBy(SortKey::Turnover));
        assert_eq!(v.sort, SortKey::Turnover);
        assert!(v.desc, "数值列换列应回到降序");
        let v = apply(v, RadarMsg::SortBy(SortKey::Symbol));
        assert!(!v.desc, "文本列换列应升序（A→Z）");
    }

    #[test]
    fn changing_window_follows_the_sort_column() {
        let v = apply(ViewState::DEFAULT, RadarMsg::SetWindow(3));
        assert_eq!(v.win, 3);
        assert_eq!(v.sort, SortKey::Z(3), "按 z 排序时换窗口，排序键要跟着走");

        let v = apply(v, RadarMsg::SortBy(SortKey::Turnover));
        let v = apply(v, RadarMsg::SetWindow(0));
        assert_eq!(v.sort, SortKey::Turnover, "非窗口相关的排序键不该被改写");
    }

    #[test]
    fn window_index_is_clamped() {
        let v = apply(ViewState::DEFAULT, RadarMsg::SetWindow(99));
        assert_eq!(v.win, N_WIN - 1);
    }

    #[test]
    fn missing_values_sink_in_both_directions() {
        let rows = vec![
            row("A", "v", None, None, 1.0),
            row("B", "v", Some(-4.0), Some(0.01), 2.0),
            row("C", "v", Some(1.0), Some(0.02), 3.0),
        ];
        let mut v = ViewState::DEFAULT;
        v.sort = SortKey::Z(1);
        v.desc = true;
        assert_eq!(rows[order(&rows, v)[2]].symbol, "A", "降序时缺值该沉底");
        v.desc = false;
        assert_eq!(
            rows[order(&rows, v)[2]].symbol,
            "A",
            "升序时缺值也该沉底，不能冒到最前"
        );
    }

    #[test]
    fn descending_sort_is_signed_not_absolute() {
        // 列头排序按 TradingView 的语义：看的是带符号值，不是 |值|。
        // 想看跌得最狠的就切升序，而不是让涨跌混排。
        let rows = vec![
            row("UP", "v", Some(3.0), None, 1.0),
            row("DOWN", "v", Some(-9.0), None, 1.0),
        ];
        let mut v = ViewState::DEFAULT;
        v.sort = SortKey::Z(1);
        v.desc = true;
        assert_eq!(rows[order(&rows, v)[0]].symbol, "UP");
        v.desc = false;
        assert_eq!(rows[order(&rows, v)[0]].symbol, "DOWN");
    }

    #[test]
    fn ordering_is_stable_on_ties() {
        let rows = vec![
            row("ZZZ", "v", Some(1.0), None, 5.0),
            row("AAA", "v", Some(1.0), None, 5.0),
        ];
        let o = order(&rows, ViewState::DEFAULT);
        assert_eq!(rows[o[0]].symbol, "AAA", "同分应按标的定序，避免刷新抖动");
    }

    #[test]
    fn text_sort_uses_symbol_then_venue() {
        let rows = vec![
            row("BTCUSDT", "binance:spot", None, None, 1.0),
            row("BTCUSDT", "binance:linear", None, None, 1.0),
            row("AAAUSDT", "binance:spot", None, None, 1.0),
        ];
        let mut v = ViewState::DEFAULT;
        v.sort = SortKey::Symbol;
        v.desc = false;
        let o = order(&rows, v);
        assert_eq!(rows[o[0]].symbol, "AAAUSDT");
        assert_eq!(rows[o[1]].venue, "binance:linear", "同名按 venue 定序");
    }
}
