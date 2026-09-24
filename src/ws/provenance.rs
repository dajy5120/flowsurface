//! 图表面板的数据来源徽标（docs/28 §4.2）——**常驻显示，不藏进菜单**。
//!
//! ## 为什么必须常驻
//!
//! 最贵的一类误会不是「选错了源」，是**不知道自己在比两个源**：图上是管线 A 的 Binance 实时、
//! 旁边的订单流特征算的是管线 B 的 Tardis，两者在快照时机、序列、aggressor 口径上有细微差异，
//! 足以让人花几天追一个根本不存在的 bug。
//!
//! 这一类已经咬过两次：跨流比对 55%→100%（docs/20 §4）、CLEAR 缺失多出 811 个幽灵档
//! （docs/27 §2.1）。
//!
//! 所以**不禁止逐面板选源**（跨源对比本身有价值，docs/20 的双源重建对拍就是干这个的），
//! 但来源必须一眼可见。藏在菜单里等于没有。
//!
//! ## 谁有徽标
//!
//! 只有吃行情的面板（`stream_pair_kind()` 非空，即 docs/28 §4.1 数的那 7 个）。
//! 其余 17 个是零交易所流的读数面板，给它们加徽标是纯噪音。

use super::active_run::{self, ActiveRun};

/// 一枚徽标：短标签 + 悬停详情 + 语气。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Badge {
    /// 常驻显示的短标签，控制在 ~12 个字符内，否则会把标题栏挤爆。
    pub label: String,
    /// 详情（tooltip）。可以长，说清楚是哪一次运行、什么成色。
    pub detail: String,
    /// 语气：决定配色。`Live` 中性、`Replay` 强调、`Warn` 提醒。
    pub tone: Tone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// 管线 A：交易所实时流。
    Live,
    /// 管线 B：回放/回测，数据成色已知良好。
    Replay,
    /// 有需要注意的地方（来源未声明、体检判 C、等待运行）。
    Warn,
}

/// 计算当前应显示的徽标。
///
/// `replay_mode` 来自 [`super::workspace::replay_mode`]——它按**活动工作区**判定，
/// 而不是按「后台有没有在跑回测」。这个区分是有意的（见 `main.rs` 的订阅装配注释）：
/// 即便后台正跑回测，「实盘」工作区的图仍是实时的，切到回测工作区才看回测行情。
/// 徽标必须跟着同一个判据走，否则它说的和图上画的是两回事。
pub fn badge(replay_mode: bool) -> Badge {
    // 特征工作区优先判：它也是 `replay_mode`，但徽标要说的话不同（docs/31 §8.2）。
    if super::workspace::feature_feed() {
        return Badge {
            label: "特征引擎".into(),
            detail: "管线 C：订单流特征引擎的图表事件流（docs/31 §8.2）。\n                     逐笔成交与深度快照都取自引擎重建出来的簿——\n                     图上看到的那一笔，就是引擎算出那个特征值的那一笔。\n                     零交易所连接；数据来自 feature_chart.jsonl。"
                .into(),
            tone: Tone::Replay,
        };
    }
    if !replay_mode {
        return Badge {
            label: "实时".into(),
            detail: "管线 A：交易所 WS 直连 → 图表。\n\
                     不经 Nautilus 引擎与 Redis，只喂图表，不参与特征计算与策略。"
                .into(),
            tone: Tone::Live,
        };
    }
    match active_run::current() {
        Some(ar) if ar.mode == "backtest" => from_run(&ar),
        Some(ar) if ar.mode == "live" => Badge {
            label: "实盘 paper".into(),
            detail: format!("管线 B · Sandbox paper · run {}", ar.run_id),
            tone: Tone::Replay,
        },
        _ => Badge {
            label: "等待运行".into(),
            detail: "回放工作区，但当前没有活动的回测。\n\
                     点运行条上的按钮开始，图表会边跑边画。"
                .into(),
            tone: Tone::Warn,
        },
    }
}

fn from_run(ar: &ActiveRun) -> Badge {
    let src = source_label(&ar.source);
    // 来源缺省时**说「未声明」而不是猜一个**：control_server 拉起进程那次广播还不知道
    // 策略声明了什么源，runner 读完策略后会订正。猜错比不显示更误导。
    let tone = if ar.source.is_empty() || ar.grade == "C" { Tone::Warn } else { Tone::Replay };
    let label = if ar.grade.is_empty() {
        src.to_string()
    } else {
        format!("{src} {}", ar.grade)
    };
    let mut detail = format!("管线 B · 来源 {src} · run {}", ar.run_id);
    if !ar.symbol.is_empty() {
        detail.push_str(&format!("\n标的 {}", ar.symbol));
    }
    detail.push_str(match ar.grade.as_str() {
        "A" => "\n入口体检 A：本窗口各项检查全清。",
        "B" => "\n入口体检 B：可用，但有 warn 项——结论里须标注（见回测结果面板的溯源）。",
        "C" => "\n⚠ 入口体检 C：数据不可信，这次是 --force 强行跑的。",
        _ => "\n入口体检结果未知（旧格式广播或跳过了体检）。",
    });
    Badge { label, detail, tone }
}

/// 数据源 key → 给人看的名字。未知 key 原样显示，不要静默吞掉。
fn source_label(key: &str) -> &str {
    match key {
        "tardis" => "Tardis",
        "recorder" => "自录",
        "" => "来源未声明",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(mode: &str, source: &str, grade: &str) -> ActiveRun {
        ActiveRun {
            run_id: "BT-1".into(),
            mode: mode.into(),
            symbol: "BTCUSDT".into(),
            source: source.into(),
            grade: grade.into(),
        }
    }

    /// 全部断言合在一个测试里**是有意的**：`active_run::CURRENT` 是进程级全局，
    /// 拆成多个测试会并行互相踩（`active_run` 那边踩过同一个坑，见其测试注释）。
    #[test]
    fn 徽标随来源与成色变化() {
        // 这个测试改**两份进程级全局**（active_run 与 FEATURE_FEED 的读端），
        // 与同模块另一个测试并行时会互相把状态翻掉。取同一把锁排队。
        let _g = crate::ws::workspace::replay_mode_test_lock();
        crate::ws::workspace::set_feature_feed(false);
        // 非回放工作区：恒为实时，与后台有没有在跑回测无关。
        active_run::publish_current(Some(run("backtest", "tardis", "A")));
        let b = badge(false);
        assert_eq!(b.label, "实时");
        assert_eq!(b.tone, Tone::Live);

        // 回放工作区 + 正常回测
        let b = badge(true);
        assert_eq!(b.label, "Tardis A");
        assert_eq!(b.tone, Tone::Replay);
        assert!(b.detail.contains("BT-1") && b.detail.contains("BTCUSDT"));

        // 判 C：必须变成警示语气，且详情点明是强行跑的
        active_run::publish_current(Some(run("backtest", "tardis", "C")));
        let b = badge(true);
        assert_eq!(b.tone, Tone::Warn);
        assert!(b.detail.contains("--force"));

        // 来源缺省：说「未声明」而不是猜
        active_run::publish_current(Some(run("backtest", "", "")));
        let b = badge(true);
        assert_eq!(b.label, "来源未声明");
        assert_eq!(b.tone, Tone::Warn);

        // 未知来源原样显示，不静默吞掉
        active_run::publish_current(Some(run("backtest", "databento", "A")));
        assert_eq!(badge(true).label, "databento A");

        // 自录
        active_run::publish_current(Some(run("backtest", "recorder", "B")));
        let b = badge(true);
        assert_eq!(b.label, "自录 B");
        assert!(b.detail.contains("标注"));

        // 实盘 paper
        active_run::publish_current(Some(run("live", "tardis", "")));
        assert_eq!(badge(true).label, "实盘 paper");

        // 回放工作区但没有活动 run
        active_run::publish_current(None);
        let b = badge(true);
        assert_eq!(b.label, "等待运行");
        assert_eq!(b.tone, Tone::Warn);
    }

    /// 特征工作区的徽标不能说「等待运行」（docs/31 §8.2）。
    ///
    /// 那句话在这里是错的：没有回测、没有按钮可点，数据来自引擎写的
    /// `feature_chart.jsonl`。徽标模块的立身之本是「来源一眼可见」，
    /// 它自己说错来源最不能接受。
    #[test]
    fn 特征工作区说特征引擎而不是等待运行() {
        let _g = crate::ws::workspace::replay_mode_test_lock();
        active_run::publish_current(None);
        crate::ws::workspace::set_feature_feed(true);
        let b = badge(true);
        assert_eq!(b.label, "特征引擎");
        assert_ne!(b.tone, Tone::Warn, "它不是一个「有问题」的状态");
        assert!(
            !b.detail.contains("点运行条"),
            "详情里仍然叫人去点一个这里不存在的按钮"
        );
        assert!(b.detail.contains("feature_chart.jsonl"), "详情要说清数据从哪来");

        // 关掉之后回到原来的判定——这个标志不能粘住。
        crate::ws::workspace::set_feature_feed(false);
        assert_eq!(badge(true).label, "等待运行");
        assert_eq!(badge(false).label, "实时");
    }
}
