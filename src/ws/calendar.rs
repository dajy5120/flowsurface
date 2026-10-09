//! 金融日历面板的**副作用**（docs/43 K2）：选日期 / 翻月 / 筛选只改面板内存；
//! 刷新是起一次 `ws-news --once calendar`；打开原文走新闻面板同一个安全检查。
//!
//! 与新闻面板同一模式：`_view` 只产生消息，副作用集中在这里。

use super::calendar_readout as ro;
use super::calendar_view::CalMsg;

pub fn handle(m: CalMsg) {
    match m {
        CalMsg::SetView(v) => ro::set_view(v),
        CalMsg::Select(d) => ro::select(d),
        CalMsg::PrevMonth => ro::shift_month(&ro::snapshot(), -1),
        CalMsg::NextMonth => ro::shift_month(&ro::snapshot(), 1),
        CalMsg::Today => ro::go_today(&ro::snapshot()),
        CalMsg::PickEvent(id) => ro::select_event(&id),
        CalMsg::Open(url) => super::news::open_in_browser(&url),
        CalMsg::Refresh => {
            // 守护常驻时 OnceJob 会拒绝并说明原因（数据本来就在持续更新）
            let active = super::svcctl::query(super::news_readout::SERVICE).active;
            let _ = super::once::CALENDAR.start(active);
        }
        CalMsg::MaxImportance(i) => ro::update_filter(|f| f.max_importance = i.min(3)),
        CalMsg::ToggleKind(k) => ro::update_filter(|f| toggle(&mut f.hidden_kinds, k)),
        CalMsg::ToggleCountry(c) => ro::update_filter(|f| toggle(&mut f.hidden_countries, c)),
        CalMsg::ShowUnlisted(on) => ro::update_filter(|f| f.show_unlisted = on),
        CalMsg::Query(q) => ro::update_filter(|f| f.query = q),
        CalMsg::Tz(t) => ro::update_filter(|f| f.tz = t),
        CalMsg::StreamDays(n) => ro::update_filter(|f| f.stream_days = n),
        CalMsg::YearMode(m) => ro::set_year_mode(m),
        CalMsg::PrevYear => ro::shift_year(&ro::snapshot(), -1),
        CalMsg::NextYear => ro::shift_year(&ro::snapshot(), 1),
        // 事件流表：操作格转成「详情」，其余交给网格
        CalMsg::Table(crate::ui::grid::GridMsg::Action(r, c)) => {
            if let Some(m) = super::calendar_view::stream_action(r, c) {
                handle(m);
            }
        }
        CalMsg::Table(g) => crate::ui::grid::named_update("calendar.stream", g),
    }
}

/// 在「隐藏」列表里加 / 去一项。
fn toggle(v: &mut Vec<String>, x: String) {
    if let Some(i) = v.iter().position(|y| *y == x) {
        v.remove(i);
    } else {
        v.push(x);
    }
}
