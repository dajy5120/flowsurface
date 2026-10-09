//! 面板标题栏的统一标题与状态位（docs/35 §5.1 / §10 第 1 条，UPDS V2 §11 Panel）。
//!
//! 上游的面板标题栏只给吃行情的面板显示标的；WS 自研面板的标题各自画在内容区里，
//! 字号、写法、位置各不相同。这里给所有非行情面板一个统一的标题栏标题，外加一个状态位。
//!
//! **状态位每帧都画**，只取不做 IO、不复制大快照的轻量状态：取不到的就不显示，
//! 不为了一个状态位去拉起读数线程（那会破坏「面板显示时才工作」的闸门，见 svcctl::Demand）。

use iced::Color;

use crate::screen::dashboard::pane::Content;

/// (状态文字, 颜色)。文字里带符号（● ○ ⚠），不只靠颜色区分。
pub fn status(content: &Content) -> Option<(String, Color)> {
    use crate::ui::pal;
    use crate::ui::live::{LiveState, file_age};
    use std::time::Duration;
    let live = |st: LiveState, extra: &str| Some((format!("{}{extra}", st.label()), st.color()));
    match content {
        // 引擎每 0.5s 写一次快照；读的是本地文件的修改时间（一次 stat，不解析）
        Content::FeatureMatrix => {
            let up = super::feature_matrix::engine_state().is_some_and(|s| s.active);
            let age = file_age(&super::feature_matrix_readout::board_path());
            live(LiveState::classify(age, Duration::from_millis(500), up, false, false), if up { " · 引擎运行中" } else { " · 引擎未运行" })
        }
        // 新闻 / 雷达的快照是 Arc，取一次只复制指针（面板正显示，读数线程本来就在跑）
        Content::News => {
            let r = super::news_readout::snapshot();
            let degraded = r.stale_sources > 0;
            let age = file_age(&super::paths::runtime_dir().join("news_board.json"));
            let extra = if degraded { format!(" · {} 个源已陈", r.stale_sources) } else { String::new() };
            live(LiveState::classify(age, Duration::from_secs(60), r.svc.active, degraded, false), &extra)
        }
        // 日历不是实时流：按「数据多旧」说话，超过 12 小时（会自动刷新的阈值）标黄
        Content::Calendar => {
            let r = super::calendar_readout::snapshot();
            if super::once::CALENDAR.running() {
                return Some(("⟳ 日历刷新中".to_string(), pal::info()));
            }
            if !r.present {
                return Some(("○ 还没有日历数据".to_string(), pal::warn()));
            }
            let now = chrono::Local::now().timestamp_millis();
            let h = (now - r.generated_ms) / 3_600_000;
            let stale = super::calendar_readout::needs_refresh(true, r.generated_ms, now);
            // 错过的提醒（Cockpit 关着时到点的）：在状态里说出来，通知中心里点「知道了」消掉
            // 此刻在风控窗口里（K5a）：最该被看见的一句，放最前
            let snap = super::calendar_alerts::snapshot();
            if let Some(w) = snap.active.first() {
                let left = (w.end_ms - now) / 60_000;
                return Some((format!("⚠ 事件窗口中：{}（还有 {left} 分钟）", w.title), pal::warn()));
            }
            let missed = super::calendar_alerts::status().missed;
            let tail = if missed > 0 { format!(" · 错过 {missed} 条提醒") } else { String::new() };
            Some((
                format!("{} {} 个事件 · 数据 {h} 小时前{tail}", if stale { "▲" } else { "●" }, r.events.len()),
                if stale || missed > 0 { pal::warn() } else { pal::ok() },
            ))
        }
        Content::MarketMap => {
            let r = super::radar_readout::snapshot();
            let filling = !r.progress.cur.is_empty();
            let age = file_age(&super::paths::runtime_dir().join("radar_board.json"));
            live(LiveState::classify(age, Duration::from_secs(30), r.svc.active, false, filling), if filling { " · 抓取中" } else { "" })
        }
        Content::BacktestResult | Content::Orders => match super::active_run::current() {
            Some(r) if r.mode == "backtest" => Some((format!("● 回测运行中 · {}", r.run_id), pal::ok())),
            Some(r) if r.mode == "live" => Some((format!("● 模拟盘运行中 · {}", r.run_id), pal::ok())),
            _ => Some(("○ 没有运行".to_string(), pal::dim())),
        },
        Content::NetEgress => {
            let n = super::egress::external_conns();
            Some((format!("对外 {n} 条"), if n > 0 { pal::warn() } else { pal::dim() }))
        }
        _ => None,
    }
}
