//! 新闻面板的**副作用**（docs/25）：守护启停 + 打开原文。
//!
//! 与 `observatory` / `radar` 同一模式：`_view` 只产生消息，副作用集中在这里。

use super::news_readout as ro;
use super::news_view::NewsMsg;

pub fn handle(m: NewsMsg) {
    match m {
        NewsMsg::Start => {
            let _ = ro::svc_action("start");
        }
        NewsMsg::Stop => {
            let _ = ro::svc_action("stop");
        }
        NewsMsg::Open(url) => open_in_browser(&url),
        // N2 只有两区，折叠留给条目多起来之后
        NewsMsg::ToggleHealth => {}
    }
}

/// 用系统默认浏览器打开原文。
///
/// **只放行 http/https**：URL 是从新闻源来的，源被人接管的话，
/// 一个 `file://` 或 `javascript:` 就能变成一次本地执行。
fn open_in_browser(url: &str) {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        log::warn!("拒绝打开非 http(s) 链接：{url}");
        return;
    }
    let _ = std::process::Command::new("xdg-open")
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}
