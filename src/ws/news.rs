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
        // 打字只改面板内存：守护照常收全部
        NewsMsg::FilterEdited(t) => ro::set_filter_text(&t),
        NewsMsg::WatchEdited(t) => ro::set_watch_input(&t),
        NewsMsg::WatchAdd => {
            // 一次可以贴多个：`AAPL MSFT,NVDA` 都认
            let raw = ro::watch_input();
            let add: Vec<String> = raw
                .split([' ', ',', ';', '\t'])
                .filter_map(ro::clean_symbol)
                .collect();
            if add.is_empty() {
                return;
            }
            let mut cur = ro::read_watch();
            for a in add {
                if !cur.iter().any(|c| c.eq_ignore_ascii_case(&a)) {
                    cur.push(a);
                }
            }
            ro::write_watch(&cur);
            ro::set_watch_input("");
        }
        NewsMsg::SetView(v) => ro::set_view(v),
        NewsMsg::ToggleSource(id, on) => ro::set_enabled(&id, on),
        NewsMsg::DeleteSource(id) => ro::remove_source(&id),
        NewsMsg::ProbeSource(id_or_url) => {
            // 表格里的按钮给的是源 id，加源表单给的是 URL。
            // 从当前快照里把 id 翻成 URL——**翻不到就按 URL 处理**，
            // 而不是静默什么都不做
            let cur = ro::snapshot();
            let url = cur
                .sources
                .iter()
                .find(|s| s.id == id_or_url)
                .map(|s| s.url.clone())
                .unwrap_or(id_or_url);
            if url.starts_with("http") {
                ro::request_probe(&url);
            } else {
                ro::set_add_note("要测的地址得是 http:// 或 https://");
            }
        }
        NewsMsg::AddEdited(f, t) => {
            ro::set_add_form(f, &t);
            ro::set_add_note("");
        }
        NewsMsg::AddSource => {
            let (id, label, url, tier) = ro::add_form();
            let e = ro::add_source(&id, &label, &url, &tier);
            if e.is_empty() {
                ro::clear_add_form();
                // 加完立刻测一下：加进去才发现连不上，不如加的时候就说
                ro::request_probe(&url);
                ro::set_add_note(&format!("✔ 已添加「{}」，正在测试…", id.trim()));
            } else {
                ro::set_add_note(&e);
            }
        }
        NewsMsg::SearchEdited(t) => ro::set_search_input(&t),
        NewsMsg::SearchRun => {
            let q = ro::search_input();
            if !q.trim().is_empty() {
                // 表格类型先固定 8-K：全文检索不限表格的话，
                // 一个常见词能命中上万条 Form 4，翻不完也没用
                ro::write_search(q.trim(), "8-K");
            }
        }
        NewsMsg::SearchClear => {
            ro::set_search_input("");
            ro::write_search("", "");
        }
        NewsMsg::WatchRemove(sym) => {
            let cur: Vec<String> =
                ro::read_watch().into_iter().filter(|c| !c.eq_ignore_ascii_case(&sym)).collect();
            ro::write_watch(&cur);
        }
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
/// 顺带挡住 `-` 开头（会被当成选项）——虽然「必须以 http(s):// 开头」
/// 这一条已经隐含了它，留着是为了让规则自己说清楚。
pub fn is_safe_url(url: &str) -> bool {
    let u = url.trim();
    (u.starts_with("https://") || u.starts_with("http://"))
        && !u.starts_with('-')
        // 控制字符（含换行）能把一条命令拆成两条
        && !u.chars().any(|c| c.is_control())
        && u.len() > 10
        && u.len() < 4096
}

/// 交给 `xdg-open` 的完整参数表。
///
/// 抽出来是为了能测：`--` 那个 bug 的表现是「点了没反应」，
/// 而它只要看一眼 argv 就能发现。
pub fn open_args(url: &str) -> Vec<String> {
    // **一个参数，不带任何选项。** 见下面的坑 ①
    vec![url.to_string()]
}

/// 用系统默认浏览器打开原文。
///
/// # 两处踩过的坑
///
/// **① 不能加 `--`。** 一度为了防参数注入写成 `xdg-open -- URL`，
/// 结果 `xdg-open` 根本不认这个约定，直接
/// `xdg-open: unexpected option '--'`——功能整个失效。
/// 它是个 shell 脚本，自己解析参数，不遵守 getopt 的惯例。
/// 安全性由 [`is_safe_url`] 保证就够了：它要求以 `http(s)://` 开头，
/// `-` 开头本来就进不来。
///
/// **② 失败不能吞。** 原来 stdout/stderr 都丢进 /dev/null 且不等退出码，
/// 于是上面那个 bug 的表现就只是「点了没反应」——一条日志都没有。
/// 现在起一个线程收退出码，失败就记下来。
fn open_in_browser(url: &str) {
    if !is_safe_url(url) {
        log::warn!("拒绝打开这个链接（只放行 http/https）：{url}");
        return;
    }
    let args = open_args(url);
    // 收尸放到后台线程：`xdg-open` 在某些桌面下会等浏览器起来，
    // 在 UI 线程里 wait 会卡住整个界面
    std::thread::spawn(move || {
        let url = args[0].clone();
        match std::process::Command::new("xdg-open").args(&args).output() {
            Ok(o) if o.status.success() => {}
            Ok(o) => log::warn!(
                "xdg-open 打不开 {url}：退出码 {:?} {}",
                o.status.code(),
                String::from_utf8_lossy(&o.stderr).trim()
            ),
            Err(e) => log::warn!("起不了 xdg-open：{e}"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::is_safe_url;

    #[test]
    fn xdg_open_gets_the_url_and_nothing_else() {
        // 一度写成 `xdg-open -- URL` 防参数注入，结果 xdg-open 根本不认
        // 这个约定：`xdg-open: unexpected option '--'`，功能整个失效，
        // 而失败又被 /dev/null 吞了 → 表现就是「点了没反应」。
        //
        // 它是个 shell 脚本，自己解析参数，不遵守 getopt 惯例
        let a = super::open_args("https://x.com/a");
        assert_eq!(a, vec!["https://x.com/a".to_string()]);
        assert!(!a.iter().any(|s| s.starts_with('-')), "不许带任何选项：{a:?}");
        assert_eq!(a.len(), 1, "多一个参数就可能被当成要打开的第二个东西");
    }

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
