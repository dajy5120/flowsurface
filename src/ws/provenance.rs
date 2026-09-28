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

/// 性质徽标：图上这是**什么数据**、能不能拿来下结论（实时 / 回放 / 回测 / 模拟盘 + 成色）。
///
/// 数据**从哪来、流到哪、通不通**不归它管，归 [`link`]（链路徽标）——两枚各说一件事，
/// 成色有问题看这枚，链路断了看那枚。
///
/// `replay_mode` 来自 [`super::workspace::replay_mode`]——它按**活动工作区**判定，
/// 而不是按「后台有没有在跑回测」。这个区分是有意的（见 `main.rs` 的订阅装配注释）：
/// 即便后台正跑回测，「实盘」工作区的图仍是实时的，切到回测工作区才看回测行情。
/// 徽标必须跟着同一个判据走，否则它说的和图上画的是两回事。
pub fn badge(replay_mode: bool) -> Badge {
    // 特征工作区优先判：它也是 `replay_mode`，但要说的话不同（docs/31 §8.2）。
    if super::workspace::feature_feed() {
        let replay = super::feature_source::chart_override().is_some();
        return Badge {
            label: if replay { "回放·引擎同源" } else { "实时·引擎同源" }.into(),
            detail: format!(
                "{}\n\
                 图上的逐笔成交与深度快照都取自特征引擎重建出来的簿——\n\
                 图上看到的那一笔，就是引擎算出那个特征值的那一笔，可以和特征矩阵逐笔对照。\n\
                 {}",
                if replay {
                    "管线 B 的历史数据（购买 / 自录）经特征引擎回放。"
                } else {
                    "管线 B1：交易所实时经常驻特征引擎。"
                },
                if replay {
                    "回放入口不跑数据体检：没有 A/B/C 成色，结论要自己看数据有没有断流。"
                } else {
                    "实时流会断线：断线期间的空洞在引擎健康页里看。"
                }
            ),
            tone: if replay { Tone::Replay } else { Tone::Live },
        };
    }
    if !replay_mode {
        return Badge {
            label: "实时·仅看图".into(),
            detail: "管线 A：交易所 WS 直连 → 图表。\n\
                     不经 Nautilus 引擎与 Redis，只喂图表，不参与特征计算与策略；\n\
                     与特征矩阵的数值不是同一份数据，不能逐笔对照。"
                .into(),
            tone: Tone::Live,
        };
    }
    match active_run::current() {
        Some(ar) if ar.mode == "backtest" || ar.mode == "stopped" => from_run(&ar),
        Some(ar) if ar.mode == "live" => Badge {
            label: "模拟盘".into(),
            detail: format!("管线 B · Sandbox paper（不是真金）· run {}", ar.run_id),
            tone: Tone::Replay,
        },
        _ => Badge {
            label: "等待运行".into(),
            detail: "回放工作区，但当前没有活动的回测。\n\
                     在回测结果面板顶上的「发起回测」选策略与数据后运行，图表会边跑边画。"
                .into(),
            tone: Tone::Warn,
        },
    }
}

fn from_run(ar: &ActiveRun) -> Badge {
    let tone = if ar.grade == "C" {
        Tone::Warn
    } else {
        Tone::Replay
    };
    let label = match ar.grade.as_str() {
        "" => "回测".to_string(),
        "C" => "回测 C!".to_string(),
        g => format!("回测 {g}"),
    };
    let mut detail = format!("管线 B · Nautilus 事件驱动回测 · run {}", ar.run_id);
    detail.push_str(match ar.grade.as_str() {
        "A" => "\n入口体检 A：本窗口各项检查全清。",
        "B" => "\n入口体检 B：可用，但有 warn 项——结论里须标注（见回测结果面板的溯源）。",
        "C" => "\n⚠ 入口体检 C：数据不可信，这次是 --force 强行跑的。不要用它得结论。",
        _ => "\n入口体检结果未知（该数据源没有体检、旧格式广播或跳过了体检）。",
    });
    detail.push_str("\n撮合：队列位置 + 流动性消耗默认开（--optimistic 才关）；费率按策略声明 / docs/10 档位。");
    Badge { label, detail, tone }
}

/// 数据源 key → 给人看的名字。未知 key 原样显示，不要静默吞掉。
fn source_label(key: &str) -> &str {
    match key {
        "tardis" => "Tardis",
        "databento" => "Databento",
        "recorder" => "录制",
        "" => "来源未声明",
        other => other,
    }
}

// ── 链路徽标 ────────────────────────────────────────────────────────────────

/// 链路徽标：数据从哪来、流到哪、现在通不通。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// 短标签：`来源代号 · 数据商/交易所 标的 状态`。
    pub label: String,
    /// 只有状态符号的短写（▶ ⏸ ✔ ✗ …），给放不下完整标签的地方用。
    pub short: String,
    /// 逐跳详情。
    pub detail: String,
    pub tone: LinkTone,
    /// 点它能不能跳到数据源选择（管线 A 的图换标的走标的列表，不跳）。
    pub clickable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkTone {
    /// 通：实时在流。
    Ok,
    /// 回放 / 回测在跑。
    Running,
    /// 暂停、等待、外壳不符、引擎停了。
    Warn,
    /// 断了：失败、图收不到数据。
    Bad,
    /// 跑完了，图已静止。
    Idle,
}

fn hops_text(hops: &[super::feature_source::Hop]) -> String {
    hops.iter()
        .map(|(ok, name, v)| format!("{} {name}  {v}", if *ok { "✔" } else { "✗" }))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 计算链路徽标。`venue` / `symbol` 是这张图自己的交易所与标的（管线 A 用）。
pub fn link(replay_mode: bool, venue: &str, symbol: &str) -> Link {
    use super::feature_source::LinkState as S;
    if super::workspace::feature_feed() {
        let li = super::feature_source::link_info();
        let (st, short, mut tone) = match &li.state {
            S::Live => ("实时".to_string(), "●", LinkTone::Ok),
            S::EngineStopped => ("引擎已停".into(), "■", LinkTone::Warn),
            S::Running(p) => (format!("▶{}", p.split('（').next().unwrap_or(p)), "▶", LinkTone::Running),
            S::Paused => ("⏸".into(), "⏸", LinkTone::Warn),
            S::Done => ("✔".into(), "✔", LinkTone::Idle),
            S::Failed => ("✗".into(), "✗", LinkTone::Bad),
            S::WaitingFile => ("等待文件…".into(), "…", LinkTone::Warn),
        };
        let mut detail = hops_text(&li.hops);
        if let Some((bad, msg)) = &li.problem {
            tone = if *bad { LinkTone::Bad } else if tone == LinkTone::Bad { tone } else { LinkTone::Warn };
            detail = format!("{} {msg}\n\n{detail}", if *bad { "✗" } else { "⚠" });
        }
        detail.push_str("\n\n点击：展开特征面板的「数据源」选择");
        let warn_mark = if li.problem.is_some() { " ⚠" } else { "" };
        return Link {
            label: format!("{} · {} {} {st}{warn_mark}", li.code, li.vendor, li.symbol),
            short: format!("{short}{warn_mark}"),
            detail,
            tone,
            clickable: true,
        };
    }
    if !replay_mode {
        return Link {
            label: format!("A · {venue} {symbol} 直连"),
            short: "●".into(),
            detail: format!(
                "✔ 选择  管线 A · {venue} · {symbol}\n\
                 ✔ 接入  flowsurface 原生适配器 · 交易所 WS 直连\n\
                 ✔ 图表  直接画，不经 Nautilus / 特征引擎\n\n\
                 换标的：点这张图左上角的标的名（标的列表），不经数据源选择组件"
            ),
            tone: LinkTone::Ok,
            clickable: false,
        };
    }
    match active_run::current() {
        Some(ar) if ar.mode == "backtest" || ar.mode == "stopped" || ar.mode == "live" => {
            let code = match ar.source.as_str() {
                "recorder" => "B3",
                "" => "B?",
                _ => "B2",
            };
            let running = ar.mode != "stopped";
            let (st, short, tone) = if ar.source.is_empty() {
                ("来源未声明".to_string(), "?", LinkTone::Warn)
            } else if running {
                ("▶".to_string(), "▶", LinkTone::Running)
            } else {
                ("✔".to_string(), "✔", LinkTone::Idle)
            };
            let runner = if ar.mode == "live" {
                "run_user_strategy_live.py（Sandbox）"
            } else if ar.source == "recorder" {
                "run_user_strategy.py（自录 L2 + trades）"
            } else {
                "run_tardis_backtest.py（数据商接口流式装载，不转格式）"
            };
            Link {
                label: format!("{code} · {} {} {st}", source_label(&ar.source), ar.symbol),
                short: short.into(),
                detail: format!(
                    "✔ 选择  管线 B · {code} · {} · {}\n\
                     ✔ 接入  {runner}\n\
                     {} 计算  NautilusTrader BacktestEngine · run {}\n\
                     ✔ 输出  Redis ws:bt:{}:trades → 图（K 线 / ▲▼ / 订单面板）\n\
                     \x20 状态  {}\n\n\
                     点击：展开回测结果面板顶上的「发起回测」",
                    source_label(&ar.source),
                    ar.symbol,
                    if running { "✔" } else { "·" },
                    ar.run_id,
                    ar.run_id,
                    if running { "运行中" } else { "已结束（图已静止）" },
                ),
                tone,
                clickable: true,
            }
        }
        _ => Link {
            label: "B · 未运行".into(),
            short: "○".into(),
            detail: "回放工作区，没有活动的运行：图上没有数据在流。\n\n点击：展开「发起回测」".into(),
            tone: LinkTone::Warn,
            clickable: true,
        },
    }
}

/// 点链路徽标：跳到对应面板的数据源选择。
pub fn on_link_click() {
    if super::workspace::feature_feed() {
        super::feature_source::open_picker();
    } else {
        super::backtest_launch::open_picker();
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
    fn 性质徽标随运行与成色变化() {
        let _g = crate::ws::workspace::replay_mode_test_lock();
        crate::ws::workspace::set_feature_feed(false);
        // 非回放工作区：恒为实时，与后台有没有在跑回测无关。
        active_run::publish_current(Some(run("backtest", "tardis", "A")));
        let b = badge(false);
        assert_eq!(b.label, "实时·仅看图");
        assert_eq!(b.tone, Tone::Live);

        // 回放工作区 + 正常回测：性质只说成色，不说数据商（那归链路徽标）
        let b = badge(true);
        assert_eq!(b.label, "回测 A");
        assert_eq!(b.tone, Tone::Replay);
        assert!(b.detail.contains("BT-1"));
        assert!(!b.label.contains("Tardis"));

        // 判 C：警示语气，详情点明是强行跑的
        active_run::publish_current(Some(run("backtest", "tardis", "C")));
        let b = badge(true);
        assert_eq!(b.label, "回测 C!");
        assert_eq!(b.tone, Tone::Warn);
        assert!(b.detail.contains("--force"));

        // 没有体检等级
        active_run::publish_current(Some(run("backtest", "databento", "")));
        assert_eq!(badge(true).label, "回测");

        // 实盘 paper
        active_run::publish_current(Some(run("live", "tardis", "")));
        assert_eq!(badge(true).label, "模拟盘");

        // 回放工作区但没有活动 run
        active_run::publish_current(None);
        let b = badge(true);
        assert_eq!(b.label, "等待运行");
        assert_eq!(b.tone, Tone::Warn);
    }

    #[test]
    fn 链路徽标说来源标的与状态() {
        let _g = crate::ws::workspace::replay_mode_test_lock();
        crate::ws::workspace::set_feature_feed(false);

        // 管线 A：图自己的交易所与标的，不可点（换标的走标的列表）
        let l = link(false, "Binance", "BTCUSDT");
        assert_eq!(l.label, "A · Binance BTCUSDT 直连");
        assert!(!l.clickable);

        // 回测在跑：来源代号 + 数据商 + 标的
        active_run::publish_current(Some(run("backtest", "databento", "A")));
        let l = link(true, "", "");
        assert_eq!(l.label, "B2 · Databento BTCUSDT ▶");
        assert_eq!(l.tone, LinkTone::Running);
        assert!(l.clickable && l.detail.contains("run_tardis_backtest"));

        // 跑完
        active_run::publish_current(Some(run("stopped", "recorder", "")));
        let l = link(true, "", "");
        assert_eq!(l.label, "B3 · 录制 BTCUSDT ✔");
        assert_eq!(l.tone, LinkTone::Idle);

        // 来源未声明：说出来，不猜
        active_run::publish_current(Some(run("backtest", "", "")));
        assert_eq!(link(true, "", "").tone, LinkTone::Warn);

        active_run::publish_current(None);
        assert_eq!(link(true, "", "").label, "B · 未运行");
    }

    /// 特征工作区：性质说「引擎同源」、链路说 B1/B2/B3（docs/31 §8.2）。
    ///
    /// 不能说「等待运行」：没有回测、没有按钮可点，数据来自特征引擎的图表流。
    #[test]
    fn 特征工作区说引擎同源与所选链路() {
        let _g = crate::ws::workspace::replay_mode_test_lock();
        active_run::publish_current(None);
        crate::ws::workspace::set_feature_feed(true);
        let b = badge(true);
        assert_eq!(b.label, "实时·引擎同源");
        assert_ne!(b.tone, Tone::Warn, "它不是一个「有问题」的状态");
        assert!(!b.detail.contains("点运行条"));
        let l = link(true, "", "");
        assert!(l.label.starts_with("B1 · Binance BTCUSDT"), "{}", l.label);
        assert!(l.clickable);
        assert!(l.detail.contains("ws-features"));

        // 关掉之后回到原来的判定——这个标志不能粘住。
        crate::ws::workspace::set_feature_feed(false);
        assert_eq!(badge(true).label, "等待运行");
        assert_eq!(badge(false).label, "实时·仅看图");
    }
}
