//! 交易所元数据（交易对清单 + 最小价位 / 数量 / 合约面值）的本地缓存（2026-10-01）。
//!
//! 官方 flowsurface 每次启动都向每个选中的交易所拉一遍元数据：实测每家 1.4–3 MB，五家合计
//! 约 10 MB，而且不受「对外连接」总闸控制。这些数据几天都不变（新上的币除外），
//! 用户要求耗网络的部分人工触发，所以改成：
//!
//! - 启动 / 勾选交易所：有缓存就用缓存，**不联网**；某家从没拉过才拉一次。
//! - 交易对列表顶部「⇩」：手动重新拉（选中的交易所），拉完写回缓存。
//!
//! 每家一个文件 `<数据目录>/ticker_metadata/<交易所>.json`，刷新一家不重写别家。
//! 写入用临时文件 + 改名，写到一半退出不会留下半个 JSON。

use std::collections::HashMap;
use std::path::PathBuf;

use exchange::adapter::Venue;
use exchange::{Ticker, TickerInfo};
use serde::{Deserialize, Serialize};

pub type Metadata = HashMap<Ticker, Option<TickerInfo>>;

#[derive(Serialize, Deserialize)]
struct File {
    /// 拉取时刻（unix 秒），给界面显示「清单拉取于」
    fetched_at: i64,
    items: Vec<(Ticker, Option<TickerInfo>)>,
}

/// 各交易所缓存的拉取时刻（读 / 写缓存时记下；按钮提示每帧读它，不碰磁盘）。
static FETCHED_AT: std::sync::Mutex<Vec<(Venue, i64)>> = std::sync::Mutex::new(Vec::new());

fn remember(venue: Venue, at: i64) {
    if let Ok(mut g) = FETCHED_AT.lock() {
        g.retain(|(v, _)| *v != venue);
        g.push((venue, at));
    }
}

fn dir() -> PathBuf {
    data::data_path(Some("ticker_metadata"))
}

fn path(venue: Venue) -> PathBuf {
    dir().join(format!("{venue:?}.json"))
}

/// 读缓存。没有 / 读不懂 / 是空表 → `None`（调用方去联网拉）。
pub fn load(venue: Venue) -> Option<(Metadata, i64)> {
    let t = std::fs::read_to_string(path(venue)).ok()?;
    let f: File = serde_json::from_str(&t).ok()?;
    if f.items.is_empty() {
        return None;
    }
    remember(venue, f.fetched_at);
    Some((f.items.into_iter().collect(), f.fetched_at))
}

/// 写缓存（联网拉成功后调）。失败只记日志——缓存写不进去不影响这一次使用。
pub fn save(venue: Venue, m: &Metadata) {
    if m.is_empty() {
        return;
    }
    let f = File {
        fetched_at: chrono::Utc::now().timestamp(),
        items: m.iter().map(|(k, v)| (*k, *v)).collect(),
    };
    remember(venue, f.fetched_at);
    let r = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(dir())?;
        let p = path(venue);
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(&f).map_err(std::io::Error::other)?)?;
        std::fs::rename(tmp, p)
    })();
    if let Err(e) = r {
        log::warn!("[ticker_cache] 写 {venue:?} 元数据缓存失败：{e}");
    }
}

/// 选中交易所里最旧的一份缓存是什么时候拉的（给按钮提示用；只读内存，每帧调也没事）。
pub fn oldest_label(venues: impl IntoIterator<Item = Venue>) -> String {
    let known = FETCHED_AT.lock().map(|g| g.clone()).unwrap_or_default();
    let oldest = venues
        .into_iter()
        .filter_map(|v| known.iter().find(|(k, _)| *k == v).map(|(_, t)| *t))
        .min();
    match oldest.and_then(|t| chrono::DateTime::from_timestamp(t, 0)) {
        Some(t) => format!(
            "交易对清单拉取于 {}",
            t.with_timezone(&chrono::Local).format("%m-%d %H:%M")
        ),
        None => "交易对清单还没缓存".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 写了能读回来_空表不算缓存() {
        let d = std::env::temp_dir().join(format!("ws-ticker-cache-{}", std::process::id()));
        // SAFETY: 测试进程里只有这一个测试改这个变量
        unsafe { std::env::set_var("FLOWSURFACE_DATA_PATH", &d) };
        assert!(load(Venue::Mexc).is_none());
        save(Venue::Mexc, &Metadata::new());
        assert!(load(Venue::Mexc).is_none(), "空表不写，也不当缓存");

        let t = Ticker::new("BTCUSDT", exchange::adapter::Exchange::MexcSpot);
        let mut m = Metadata::new();
        m.insert(t, Some(TickerInfo::new(t, 0.01, 0.0001, None)));
        save(Venue::Mexc, &m);
        let (back, at) = load(Venue::Mexc).expect("读回");
        assert_eq!(back.len(), 1);
        assert_eq!(back.get(&t).copied().flatten().map(|i| i.ticker), Some(t));
        assert!(at > 0);
        assert!(oldest_label([Venue::Mexc]).starts_with("交易对清单拉取于"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
