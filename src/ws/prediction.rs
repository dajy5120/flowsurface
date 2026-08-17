//! 预测市场面板的交互消息（夜跑手动启停 + 每日定时开关）。
//!
//! 夜跑默认**不随开机自启**（timer 已 `systemctl --user disable`），由本面板按钮控制。
//! 定时器与手动运行相互独立：关了定时器仍可手动跑（同 [`super::factory`]）。

/// 预测市场面板的交互消息。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PredictionMsg {
    RunNightly,
    StopNightly,
    /// 把每日定时**设为**开/关（两个独立按钮，非切换——切换式按钮看不出当前是哪态）。
    SetTimer(bool),
    /// 手动刷新：叫醒 poller 立刻取一次状态（不必等下一轮 10s）。
    Refresh,
}

static ACTION_MSG: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

pub fn action_message() -> String {
    ACTION_MSG.lock().map(|m| m.clone()).unwrap_or_default()
}

pub fn handle(msg: PredictionMsg) {
    use super::prediction_readout as ro;
    let m = match msg {
        PredictionMsg::RunNightly => ro::nightly_start(),
        PredictionMsg::StopNightly => ro::nightly_stop(),
        PredictionMsg::SetTimer(on) => ro::nightly_toggle_timer(on),
        PredictionMsg::Refresh => {
            ro::request_refresh();
            String::new() // 刷新无需反馈文字，时间戳自己会跳
        }
    };
    if let Ok(mut g) = ACTION_MSG.lock() {
        *g = m;
    }
}
