//! C4 活体影子面板的交互消息（maker 影子守护启停）。
//!
//! 守护默认**不随开机自启**（unit 已 `systemctl --user disable`），由本面板按钮控制
//! 起停——同录制驾驶舱的 24/7 服务控制（[`super::recorder`]）。
//!
//! 面板本身无可编辑状态，故 handle 不带状态参数（同 [`super::factory`]）；
//! 操作反馈存全局，由 view 读出显示。

/// C4 面板的交互消息。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum C4Msg {
    StartShadow,
    StopShadow,
    RestartShadow,
    /// 手动刷新：叫醒 poller 立刻取一次状态（不必等下一轮 10s）。
    Refresh,
}

static ACTION_MSG: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

pub fn action_message() -> String {
    ACTION_MSG.lock().map(|m| m.clone()).unwrap_or_default()
}

pub fn handle(msg: C4Msg) {
    use super::c4_readout as ro;
    let m = match msg {
        C4Msg::StartShadow => ro::svc_action("start"),
        C4Msg::StopShadow => ro::svc_action("stop"),
        C4Msg::RestartShadow => ro::svc_action("restart"),
        C4Msg::Refresh => {
            ro::request_refresh();
            String::new() // 刷新无需反馈文字，时间戳自己会跳
        }
    };
    if let Ok(mut g) = ACTION_MSG.lock() {
        *g = m;
    }
}
