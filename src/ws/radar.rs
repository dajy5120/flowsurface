//! 全市场雷达面板的交互消息（docs/22 P0b）。
//!
//! 守护默认**不随开机自启**（同 recorder/C4/预测，见 c682944），由本面板按钮控制启停。

/// 面板可切换的排序口径。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortBy {
    /// |z_ret| 降序——「涨跌速度」，跨标的可比（docs/22 §3）。
    Speed,
    /// 裸涨跌幅降序。**留着是为了对照**：它永远把小市值垃圾顶到榜首，
    /// 切一次就能看出为什么默认口径不是它。
    Change,
    /// 成交额异常度降序——常领先价格。
    Volume,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadarMsg {
    Start,
    Stop,
    Refresh,
    SetSort(SortBy),
    /// 切换观察窗口（[`super::radar_readout::WINDOWS`] 的下标）。
    SetWindow(usize),
}

static ACTION_MSG: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
static SORT: std::sync::Mutex<SortBy> = std::sync::Mutex::new(SortBy::Speed);
static WINDOW: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1); // 默认 5m

pub fn action_message() -> String {
    ACTION_MSG.lock().map(|m| m.clone()).unwrap_or_default()
}

pub fn sort_by() -> SortBy {
    SORT.lock().map(|g| *g).unwrap_or(SortBy::Speed)
}

pub fn window_idx() -> usize {
    WINDOW
        .load(std::sync::atomic::Ordering::Relaxed)
        .min(super::radar_readout::N_WIN - 1)
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
        RadarMsg::SetSort(s) => {
            if let Ok(mut g) = SORT.lock() {
                *g = s;
            }
            String::new()
        }
        RadarMsg::SetWindow(i) => {
            WINDOW.store(i, std::sync::atomic::Ordering::Relaxed);
            String::new()
        }
    };
    if let Ok(mut g) = ACTION_MSG.lock() {
        *g = m;
    }
}
