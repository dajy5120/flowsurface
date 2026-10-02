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
    match content {
        Content::FeatureMatrix => super::feature_matrix::engine_state().map(|s| {
            if s.active {
                ("● 引擎运行中".to_string(), pal::ok())
            } else {
                ("○ 引擎未运行".to_string(), pal::dim())
            }
        }),
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
