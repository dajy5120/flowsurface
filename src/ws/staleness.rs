//! 「距今多久」标注（docs/20 §27）。
//!
//! 面板上只印**绝对时间戳**时，陈旧数据看起来和新鲜数据一模一样。
//! 本轮实测代价：Factory nightly 连挂 10 天没人发现（docs/20 §25）、回测结果面板一直显示
//! 6 周前的 run、期权快照 19 天前——三者都只印了裸时间戳，读的人无从判断新旧。
//!
//! 统一在时间戳后面补「· N 天前」，超阈值转琥珀色。

use iced::Color;

/// 超过这个天数就示警（面板数据多为日更，3 天没动基本意味着上游停了）。
const WARN_DAYS: i64 = 3;

/// 琥珀警示色（与面板其它「需注意」用色一致）。
pub const C_STALE: Color = Color { r: 0.90, g: 0.70, b: 0.35, a: 1.0 };

/// 解析两种实际出现过的时间戳形态：
/// - RFC3339 / ISO：`2026-07-15T17:31:34Z`（期权、预测面板的 `stamp`）
/// - run id：`20260623-102926`（回测 result.json 只有这个，日期编在 id 里）
fn parse(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    use chrono::{NaiveDateTime, TimeZone, Utc};
    let s = s.trim();
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    // run id：按本机时区解释（只用于算「多久以前」，小时级偏差不影响判读）
    if let Ok(n) = NaiveDateTime::parse_from_str(s, "%Y%m%d-%H%M%S") {
        return chrono::Local.from_local_datetime(&n).single().map(|d| d.with_timezone(&Utc));
    }
    None
}

/// 返回 (人读的「N 天前」, 是否超阈值)。解析不出来则 None（调用方保持原样显示）。
pub fn age(stamp: &str) -> Option<(String, bool)> {
    let t = parse(stamp)?;
    let secs = (chrono::Utc::now() - t).num_seconds();
    if secs < 0 {
        return None; // 未来时间：不猜，交给调用方原样显示
    }
    let days = secs / 86400;
    let label = if secs < 3600 {
        "刚刚".to_string()
    } else if secs < 86400 {
        format!("{} 小时前", secs / 3600)
    } else if days < 14 {
        format!("{days} 天前")
    } else {
        format!("{} 周前", days / 7)
    };
    Some((label, days >= WARN_DAYS))
}

/// 直接给出可拼接的后缀，如 `" · 6 周前"`；解析失败返回空串。
pub fn suffix(stamp: &str) -> String {
    age(stamp).map(|(l, _)| format!(" · {l}")).unwrap_or_default()
}

/// 该时间戳是否应该以警示色显示。
pub fn is_stale(stamp: &str) -> bool {
    age(stamp).map(|(_, w)| w).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rfc3339_and_run_id() {
        assert!(parse("2026-07-15T17:31:34Z").is_some());
        assert!(parse("20260623-102926").is_some(), "回测 run id 形态应能解析");
        assert!(parse("不是时间").is_none());
    }

    #[test]
    fn recent_is_not_stale_and_old_is() {
        let now = chrono::Utc::now();
        let fresh = (now - chrono::Duration::hours(2)).to_rfc3339();
        let old = (now - chrono::Duration::days(42)).to_rfc3339();
        assert!(!is_stale(&fresh), "2 小时前不该示警");
        assert!(is_stale(&old), "42 天前必须示警");
        assert!(age(&old).unwrap().0.contains('周'), "超过 14 天用周计");
    }

    #[test]
    fn unparseable_yields_empty_suffix() {
        assert_eq!(suffix("garbage"), "");
        assert!(!is_stale("garbage"), "解析不出来不应误报陈旧");
    }

    /// 未来时间不猜（时钟偏差/时区误解时，宁可不标也不要标出负数天）。
    #[test]
    fn future_timestamp_is_ignored() {
        let future = (chrono::Utc::now() + chrono::Duration::days(3)).to_rfc3339();
        assert!(age(&future).is_none());
    }
}
