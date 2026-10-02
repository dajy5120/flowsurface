//! 实时数据的五种状态（docs/35 §8，UPDS V5 §30–31）：实时 / 降级 / 过期 / 离线 / 对账中。
//!
//! 各面板原先各写各的「运行中」「已陈」「未运行」，词汇和颜色都不统一。这里统一成五态，
//! 面板标题栏的状态位按它显示。每一态都带符号，不只靠颜色区分。

use std::time::Duration;

use iced::Color;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveState {
    /// 数据在按预期节拍更新
    Live,
    /// 在更新，但有部分来源出问题（如若干新闻源已陈）
    Degraded,
    /// 超过预期节拍很久没有新数据
    Stale,
    /// 来源服务没在跑 / 连接断开
    Offline,
    /// 正在补数据 / 重同步（回填、重建），读数还不完整
    Reconciling,
}

impl LiveState {
    /// 由「数据多久没更新」与来源状态推出五态。
    ///
    /// `age` 超过 `expected` 的 3 倍算过期（给守护写盘、读盘的抖动留余量）；
    /// 来源停了一律离线，哪怕文件还在——那是上一次留下的。
    pub fn classify(age: Option<Duration>, expected: Duration, source_up: bool, degraded: bool, reconciling: bool) -> Self {
        if !source_up {
            return Self::Offline;
        }
        if reconciling {
            return Self::Reconciling;
        }
        match age {
            Some(a) if a > expected * 3 => Self::Stale,
            None => Self::Stale,
            _ if degraded => Self::Degraded,
            _ => Self::Live,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Live => "● 实时",
            Self::Degraded => "◐ 降级",
            Self::Stale => "◌ 过期",
            Self::Offline => "○ 离线",
            Self::Reconciling => "⟳ 对账中",
        }
    }

    pub fn color(self) -> Color {
        use super::pal;
        match self {
            Self::Live => pal::ok(),
            Self::Degraded => pal::warn(),
            Self::Stale => pal::warn(),
            Self::Offline => pal::dim(),
            Self::Reconciling => pal::info(),
        }
    }
}

/// 文件多久没改了（守护写的快照）。读不到 = None。
pub fn file_age(p: &std::path::Path) -> Option<Duration> {
    std::fs::metadata(p).ok()?.modified().ok()?.elapsed().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 五态判定() {
        let s = Duration::from_secs;
        assert_eq!(LiveState::classify(Some(s(1)), s(1), true, false, false), LiveState::Live);
        assert_eq!(LiveState::classify(Some(s(10)), s(1), true, false, false), LiveState::Stale, "超过 3 倍节拍");
        assert_eq!(LiveState::classify(Some(s(1)), s(1), true, true, false), LiveState::Degraded);
        assert_eq!(LiveState::classify(Some(s(1)), s(1), false, false, false), LiveState::Offline, "来源停了：文件新也算离线");
        assert_eq!(LiveState::classify(Some(s(1)), s(1), true, false, true), LiveState::Reconciling);
        assert_eq!(LiveState::classify(None, s(1), true, false, false), LiveState::Stale, "读不到快照");
        for st in [LiveState::Live, LiveState::Degraded, LiveState::Stale, LiveState::Offline, LiveState::Reconciling] {
            assert!(!st.label().chars().next().unwrap().is_alphanumeric(), "每态都带符号，不只靠颜色");
        }
    }
}
