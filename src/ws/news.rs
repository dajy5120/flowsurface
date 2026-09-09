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

/// 这个 URL 能不能交给浏览器。
///
/// **只放行 http/https**：URL 是从新闻源来的。源被人接管、或者哪天
/// 某个源开始返回 `file:///etc/...`、`javascript:` 这类东西，
/// 直接丢给 `xdg-open` 就是一次本地执行。
///
/// 还要挡住参数注入：`-` 开头的会被 `xdg-open` 当成选项。
pub fn is_safe_url(url: &str) -> bool {
    let u = url.trim();
    (u.starts_with("https://") || u.starts_with("http://"))
        && !u.starts_with('-')
        // 控制字符（含换行）能把一条命令拆成两条
        && !u.chars().any(|c| c.is_control())
        && u.len() > 10
        && u.len() < 4096
}

/// 用系统默认浏览器打开原文。
fn open_in_browser(url: &str) {
    if !is_safe_url(url) {
        log::warn!("拒绝打开这个链接（只放行 http/https）：{url}");
        return;
    }
    // `--` 之后的一律当参数不当选项
    let _ = std::process::Command::new("xdg-open")
        .arg("--")
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::is_safe_url;

    #[test]
    fn only_http_urls_are_handed_to_the_browser() {
        // URL 来自新闻源。源被接管、或哪天开始返回别的 scheme，
        // 直接丢给 xdg-open 就是一次本地执行
        assert!(is_safe_url("https://www.federalreserve.gov/newsevents/a.htm"));
        assert!(is_safe_url("http://example.com/a"));
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,<script>",
            "ftp://x.com/a",
            "",
            "https://",
            // 换行能把一条命令拆成两条
            "https://x.com/a\nrm -rf /",
            // `-` 开头会被当成选项
            "-https://x.com",
        ] {
            assert!(!is_safe_url(bad), "{bad:?} 不该放行");
        }
    }
}
