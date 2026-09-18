//! **进程页**：WealthSpring 名下所有常驻单元的状态与启停。
//!
//! # 和「网络出口」页的分工
//!
//! 出口页回答「谁在往外发包、怎么让它停」，所以它只列有外部连接的东西
//! （外加 Cockpit 自己这个进程内出口）。这一页回答的是另一个问题：
//! **「整套系统现在有哪些部件活着」**——包括不连外网的
//! `ws-control` / `ws-signals` / `ws-factory-bridge`。
//!
//! 两页会重叠：有外连的单元两边都能启停。这不是重复，是两个不同的问题
//! 恰好共用一个动作。真正要避免的是**漏**：`super::egress` 里的清点测试
//! 要求每个已安装单元要么在出口清单里、要么在 `NO_EGRESS` 里被显式判定过。
//!
//! # 为什么不显示 Cockpit（Studio 现在显示）
//!
//! Cockpit 是你正在看的这个窗口：给它配一个「停止」按钮，点下去窗口就没了——
//! 那是「关闭」，不是进程管理。出口页把它单列为「进程内出口」是因为它确实在
//! 发包、必须给人一个关法；这一页没有那个必要。
//!
//! **Studio 原先也不列，理由是「它不是 systemd 单元，systemctl 停不掉」。
//! 那条理由已经作废**：docs/26 S4b 补了 `ws-studio.service`（S4 建 target 时
//! 漏了 P3，症状是重启后「后台全回来了、P3 面板没了」，且看不出是故障还是
//! 没人管——是后者）。它现在受 systemd 管，于是这一页能真的启停它，
//! 列出来就不再是「一行按不动的按钮」。
//!
//! # 编排从 P0 交给 systemd（docs/26 S4）
//!
//! 原先 `supervisor.rs` 一条命令拉起 P1→通道②→P2 并盯着它们。它已退役——
//! 退避重启、依赖次序、日志这些 systemd 本来就做得更好，而它自己挂了没人管。

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::svcctl::{self, UnitState, Waker};

/// 一个受管单元。**加了新的常驻服务就要往这里加一行**——
/// 漏一行，这一页就从「系统全貌」退化成「一份不完整的清单」。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 常驻守护。**跟随本窗口**：Cockpit 关掉时一并停止（`PartOf=ws-stack.target`）。
    Daemon,
    /// 按点触发的研究任务。**不跟随窗口**——绑进来等于「Cockpit 没开着就永远不跑」，
    /// 那不是关掉后台，是把夜跑废掉。列在这里是为了让它「已知」：
    /// 用户要的是不留**未知**的后台进程，不是不留后台进程。
    Timer,
}

pub struct Proc {
    pub key: &'static str,
    pub label: &'static str,
    /// 干什么。写清楚才能让人判断该不该停。
    pub what: &'static str,
    pub unit: &'static str,
    pub kind: Kind,
    /// 停了会怎样。**「进程还活着」不等于「功能还在」**——
    /// 有些服务停掉是静默降级，不报错，所以必须写出来。
    pub if_stopped: &'static str,
}

pub static ALL: &[Proc] = &[
    Proc {
        key: "control",
        label: "控制服务端",
        what: "绑本地 UDS 收 Studio 命令 → 拉起回测/实盘，回 run_id",
        unit: "ws-control",
        kind: Kind::Daemon,
        if_stopped: "Studio 的「运行 / 跑回测」按钮会回「连不上 control_server」",
    },
    Proc {
        key: "signals",
        label: "信号发布器",
        what: "通道② ring → 微结构信号 + Factory combo → Redis ws:signals",
        unit: "ws-signals",
        kind: Kind::Daemon,
        if_stopped: "Cockpit「实盘」读数的「引擎」行不再更新",
    },
    Proc {
        key: "l2-feed",
        label: "L2 增量 feed",
        what: "Binance @depth → 通道② ring（有外连，需 VPN）",
        unit: "ws-l2-feed",
        kind: Kind::Daemon,
        if_stopped: "signals 收不到盘口增量，信号全线停更",
    },
    Proc {
        key: "trades-feed",
        label: "成交 feed",
        what: "Binance @aggTrade → 通道② ring（有外连，需 VPN）",
        unit: "ws-trades-feed",
        kind: Kind::Daemon,
        if_stopped: "**静默降级**：撤补退化成「总移除量」近似，combo 缺成交类特征",
    },
    Proc {
        key: "factory-bridge",
        label: "Factory 池桥",
        what: "本地 Factory 现役池 → Redis ws:factory:pool（每 5s）",
        unit: "ws-factory-bridge",
        kind: Kind::Daemon,
        if_stopped: "combo 实时值用的是最后一次同步的池，会悄悄过期",
    },
    Proc {
        key: "redis",
        label: "Redis（通道①）",
        what: "MessageBus 总线。无持久化，容器随服务启停",
        unit: "ws-redis",
        kind: Kind::Daemon,
        if_stopped: "上面全部服务的写入端都会开始报连接失败",
    },
    Proc {
        key: "radar",
        label: "全市场雷达",
        what: "5s 一轮全市场扫描 → radar_board.json",
        unit: "ws-radar",
        kind: Kind::Daemon,
        if_stopped: "雷达面板停在最后一张快照上",
    },
    Proc {
        key: "news",
        label: "新闻聚合",
        what: "RSS/Atom/JSON 多源抓取 → news_board.json",
        unit: "ws-news",
        kind: Kind::Daemon,
        if_stopped: "新闻面板停更；源健康页的陈旧判定会开始变红",
    },
    Proc {
        key: "observatory",
        label: "接口观察终端",
        what: "REST/WS/TCP/FIX 探针 → observatory.json",
        unit: "ws-observatory",
        kind: Kind::Daemon,
        if_stopped: "观察终端面板停更，正在录的帧会中断",
    },
    Proc {
        key: "recorder",
        label: "行情录制器",
        what: "Binance USDS-M L2/成交/标记价 → ~/ws-data parquet",
        unit: "wealthspring-recorder",
        kind: Kind::Daemon,
        if_stopped: "**录制出现空洞**，且事后无法补——研究数据是一次性的",
    },
    Proc {
        key: "pm-recorder",
        label: "预测市场录制器",
        what: "币安钱包 BTC 5 分钟涨跌：WS 盘口(Up) + REST(Down) → ~/ws-data/raw/pm_book",
        unit: "ws-pm-recorder",
        kind: Kind::Daemon,
        if_stopped: "**这份数据没有第三方历史源**——5 分钟市场结束即消失，不录就永远没有，\
                     事后一秒都补不回来。也是「预测市场」页 Binance 视图的唯一数据来源",
    },
    Proc {
        key: "factory-nightly",
        label: "Alpha 工厂夜跑",
        what: "每日跑数据管线 + 因子挖掘（按点触发，不跟随窗口）",
        unit: "ws-factory-nightly.timer",
        kind: Kind::Timer,
        if_stopped: "停 timer 才真的停；停 service 什么都没停，到点照样被拉起",
    },
    Proc {
        key: "prediction-nightly",
        label: "预测市场夜跑",
        what: "Polymarket Gamma API 拉取 + AI 决策支持（按点触发）",
        unit: "ws-prediction-nightly.timer",
        kind: Kind::Timer,
        if_stopped: "同上：要停的是 timer，不是 service",
    },
    Proc {
        key: "f0-accept",
        label: "F0 验收检查",
        what: "读本地录制做验收（一般不出网，按点触发）",
        unit: "ws-f0-accept.timer",
        kind: Kind::Timer,
        if_stopped: "录制质量的每日体检停掉；问题会攒到你手动看的时候才发现",
    },
    Proc {
        key: "obs-check",
        label: "观察终端日检",
        what: "接口探针的每日健康检查（按点触发）",
        unit: "ws-observatory-check.timer",
        kind: Kind::Timer,
        if_stopped: "接口失效不会主动告诉你",
    },
    Proc {
        key: "maker-shadow",
        label: "做市影子",
        what: "SOLUSDT 实盘排队仿真（不下真单）",
        unit: "wealthspring-maker-shadow",
        kind: Kind::Daemon,
        if_stopped: "C4 合格日的连续统计会断档",
    },
    Proc {
        key: "studio",
        label: "P3 Studio",
        what: "GPUI IDE：文件树 / 编辑器 / 终端 / 监控，也是发起回测与实盘 run 的入口",
        unit: "ws-studio",
        kind: Kind::Daemon,
        if_stopped: "写码与发起 run 的入口没了；**已经在跑的 run 不受影响**——\
                     它们是 ws-control 的子进程，不在 Studio 名下",
    },
];

/// **有意不列在这一页上**的单元，附理由。
///
/// 下面那条清点测试要求每个已安装的常驻单元要么在 [`ALL`] 里、要么在这里——
/// 新加一个服务时**必须有人分类一次**，不能靠沉默混过去。
pub static NOT_LISTED: &[(&str, &str)] = &[(
    "ws-cockpit",
    "面板自己。给你正在看的这个窗口配一个「停止」按钮，点下去窗口就没了——\
     那是「关闭」，不是进程管理；关窗口本来就会停掉全部守护（BindsTo）。",
)];

static ROWS: OnceLock<Mutex<Vec<Row>>> = OnceLock::new();
static WAKER: Waker = Waker::new();
static STARTED: OnceLock<()> = OnceLock::new();
static LAST_VIEW: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();

#[derive(Clone, Default)]
pub struct Row {
    pub key: String,
    pub label: String,
    pub what: String,
    pub unit: String,
    pub if_stopped: String,
    pub timer: bool,
    /// 下次触发（仅 timer）。**这一列不能省**：一个此刻没在跑的 timer
    /// 不等于「不会再跑」，只看状态会得出「已经全停了」的错误结论。
    pub next: String,
    pub st: UnitState,
}

static NOTE: OnceLock<Mutex<String>> = OnceLock::new();

/// 上一次动作的回执（给人看的一行字）。
pub fn note() -> String {
    NOTE.get_or_init(|| Mutex::new(String::new())).lock().map(|g| g.clone()).unwrap_or_default()
}

/// 空串不覆盖：刷新这类动作没有回执，别把上一条有用的信息擦掉。
pub fn set_note(t: &str) {
    if !t.is_empty()
        && let Ok(mut g) = NOTE.get_or_init(|| Mutex::new(String::new())).lock()
    {
        *g = t.to_string();
    }
}

pub fn waker() -> &'static Waker {
    &WAKER
}

/// 后台 poller：每 5s（或被叫醒）查一轮全部单元。
///
/// **只在这里起子进程**——`systemctl show` 进渲染线程会卡帧（docs/20 §19.2）。
pub fn start() {
    if STARTED.set(()).is_err() {
        return;
    }
    std::thread::spawn(|| loop {
        let rows: Vec<Row> = ALL
            .iter()
            .map(|p| {
                let timer = p.kind == Kind::Timer;
                Row {
                    key: p.key.into(),
                    label: p.label.into(),
                    what: p.what.into(),
                    unit: p.unit.into(),
                    if_stopped: p.if_stopped.into(),
                    timer,
                    // timer 的「已运行」没有意义（它本来就不在跑），要的是下次触发
                    next: if timer { svcctl::fmt_stamp(&svcctl::next_elapse(p.unit)) } else { String::new() },
                    st: svcctl::query(p.unit),
                }
            })
            .collect();
        if let Ok(mut g) = ROWS.get_or_init(|| Mutex::new(Vec::new())).lock() {
            *g = rows;
        }
        WAKER.wait(Duration::from_secs(5));
    });
}

pub fn rows() -> Vec<Row> {
    start();
    if let Ok(mut g) = LAST_VIEW.get_or_init(|| Mutex::new(None)).lock() {
        let first = g.is_none();
        *g = Some(Instant::now());
        // 刚打开这一页时立刻刷一轮，别让人对着 5 秒前的数
        if first {
            WAKER.request();
        }
    }
    ROWS.get_or_init(|| Mutex::new(Vec::new())).lock().map(|g| g.clone()).unwrap_or_default()
}

/// 对一个单元执行 systemctl 动作，返回给人看的回执。
pub fn action(key: &str, act: &str) -> String {
    let Some(p) = ALL.iter().find(|p| p.key == key) else {
        return format!("✗ 未知条目 {key}");
    };
    let r = match std::process::Command::new("systemctl").args(["--user", act, p.unit]).status() {
        Ok(s) if s.success() => format!("✔ {} 已{}", p.label, zh(act)),
        Ok(s) => format!("✗ systemctl {act} {} 退出码 {:?}", p.unit, s.code()),
        Err(e) => format!("✗ systemctl 失败: {e}"),
    };
    WAKER.request();
    r
}

fn zh(a: &str) -> &'static str {
    match a {
        "start" => "启动",
        "stop" => "停止",
        "restart" => "重启",
        _ => "操作",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_irreversible_ones_are_the_ones_we_think() {
        // 渲染层按 key 给这两行加「停掉就是永久缺口」的警示。
        // key 改名而警示没跟着改 = 警示挂在错的行上，比没有更糟。
        for k in ["recorder", "maker-shadow"] {
            assert!(ALL.iter().any(|p| p.key == k), "key `{k}` 没了——渲染层的不可逆警示会落空");
        }
    }

    #[test]
    fn a_timer_is_never_bound_to_the_window() {
        // 把 timer 绑进 ws-stack.target 等于「Cockpit 没开着夜跑就不跑」。
        // 这条钉住分类：timer 的 unit 名必须以 .timer 结尾，
        // 而 lifecycle 的那条测试只检查 Daemon 是否都在 target 里。
        for p in ALL {
            match p.kind {
                Kind::Timer => assert!(p.unit.ends_with(".timer"), "{} 标成 Timer 但不是 .timer", p.key),
                Kind::Daemon => assert!(!p.unit.ends_with(".timer"), "{} 标成 Daemon 但是个 .timer", p.key),
            }
        }
    }

    #[test]
    fn every_installed_unit_shows_up_on_this_page() {
        // 这一页的价值全在「全」。漏一行，它就从系统全貌退化成
        // 一份不完整的清单——而后者比没有更糟，会让人以为已经全停了。
        let Ok(h) = std::env::var("HOME") else { return };
        let dir = std::path::PathBuf::from(h).join(".config/systemd/user");
        let Ok(rd) = std::fs::read_dir(&dir) else { return };
        let known: Vec<&str> = ALL.iter().map(|p| p.unit).collect();
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            // `.timer` 由 procs::ALL 里的 Timer 行代表（下面用 known 一起比对）；
            // `.target` 不是进程，是分组（ws-stack.target），不该出现在进程清单上。
            if !(n.starts_with("ws-") || n.starts_with("wealthspring-")) || n.ends_with(".target") {
                continue;
            }
            if n.ends_with(".timer") {
                assert!(known.contains(&n.as_str()), "定时任务 {n} 没在进程页上");
                continue;
            }
            let stem = n.trim_end_matches(".service");
            // 定时任务的 oneshot service 不列：它本来就不常驻，
            // 「停止」它什么都没停，到点还是会被 timer 拉起来。
            if dir.join(format!("{stem}.timer")).exists() {
                continue;
            }
            let classified = known.contains(&stem) || NOT_LISTED.iter().any(|(u, _)| *u == stem);
            assert!(
                classified,
                "常驻单元 {stem} 没被分类——要么加一行到 procs::ALL，\
                 要么加进 NOT_LISTED（附不列的理由）"
            );
        }
    }

    #[test]
    fn every_row_says_what_breaks_if_you_stop_it() {
        // 「进程还活着」不等于「功能还在」。成交 feed 停掉是静默降级，
        // 不报错——这种事必须写在按钮旁边，不能指望人记得。
        for p in ALL {
            assert!(!p.if_stopped.is_empty(), "{} 没写停了会怎样", p.key);
            assert!(!p.what.is_empty(), "{} 没写干什么", p.key);
        }
    }

    #[test]
    fn keys_are_unique() {
        // key 重复 → action() 的 find 命中第一个，按钮会操作错的服务。
        let mut ks: Vec<&str> = ALL.iter().map(|p| p.key).collect();
        ks.sort_unstable();
        let n = ks.len();
        ks.dedup();
        assert_eq!(ks.len(), n, "procs::ALL 有重复 key");
    }

    #[test]
    fn an_unknown_key_does_not_silently_do_nothing() {
        // 打字打错时要说出来，不能静悄悄什么也不发生。
        assert!(action("没有这个", "start").starts_with("✗"));
    }
}
