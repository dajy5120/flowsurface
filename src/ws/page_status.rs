//! 页签与侧栏工作区的状态角标（docs/41 §3.4）：后台在跑 ●、出了问题 ⚠——切到别处也看得见。
//!
//! 只看**页面里有哪些面板**，再问各面板模块已有的状态，不另起轮询（面板状态本来就在进程级旁路里）。

use crate::screen::dashboard::Dashboard;
use crate::screen::dashboard::pane::Content;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Badge {
    /// 后台在跑（回测、优化、生成窗口、实时 tail …）
    Running,
    /// 有需要看一眼的问题（例如常驻单元上次失败）
    Problem,
}

impl Badge {
    pub fn glyph(self) -> &'static str {
        match self {
            Badge::Running => "●",
            Badge::Problem => "⚠",
        }
    }
}

/// `backtest_running`：主程序知道的活动回测 run（`ws_active.mode == "backtest"`）。
pub fn of(d: &Dashboard, backtest_running: bool) -> Option<Badge> {
    let mut out: Option<Badge> = None;
    let mut bump = |b: Badge| out = Some(out.map_or(b, |o| o.max(b)));
    for (_, st) in d.panes.iter() {
        match &st.content {
            Content::StrategyCenter if super::strategy_center::any_active() => bump(Badge::Running),
            Content::OfmsLab if super::ofms_lab::busy() => bump(Badge::Running),
            Content::BacktestResult | Content::WealthSpring(_) if backtest_running => bump(Badge::Running),
            Content::Procs if procs_problem() => bump(Badge::Problem),
            _ => {}
        }
    }
    out
}

/// 常驻单元里有「上次跑失败、现在也没在跑」的。
fn procs_problem() -> bool {
    super::procs::rows_cached().iter().any(|r| !r.st.active && r.st.ever_ran() && !r.st.last_ok())
}
