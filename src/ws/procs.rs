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
//! # 为什么不显示 Cockpit 和 Studio
//!
//! 它们不是 systemd 单元，systemctl 停不掉。出口页把 Cockpit 单列为
//! 「进程内出口」是因为它确实在发包、必须给人一个关法；这一页没有那个必要，
//! 列一行「无法操作」只会让人以为按钮坏了。
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
pub struct Proc {
    pub key: &'static str,
    pub label: &'static str,
    /// 干什么。写清楚才能让人判断该不该停。
    pub what: &'static str,
    pub unit: &'static str,
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
        if_stopped: "Studio 的「运行 / 跑回测」按钮会回「连不上 control_server」",
    },
    Proc {
        key: "signals",
        label: "信号发布器",
        what: "通道② ring → 微结构信号 + Factory combo → Redis ws:signals",
        unit: "ws-signals",
        if_stopped: "Cockpit「实盘」读数的「引擎」行不再更新",
    },
    Proc {
        key: "l2-feed",
        label: "L2 增量 feed",
        what: "Binance @depth → 通道② ring（有外连，需 VPN）",
        unit: "ws-l2-feed",
        if_stopped: "signals 收不到盘口增量，信号全线停更",
    },
    Proc {
        key: "trades-feed",
        label: "成交 feed",
        what: "Binance @aggTrade → 通道② ring（有外连，需 VPN）",
        unit: "ws-trades-feed",
        if_stopped: "**静默降级**：撤补退化成「总移除量」近似，combo 缺成交类特征",
    },
    Proc {
        key: "factory-bridge",
        label: "Factory 池桥",
        what: "本地 Factory 现役池 → Redis ws:factory:pool（每 5s）",
        unit: "ws-factory-bridge",
        if_stopped: "combo 实时值用的是最后一次同步的池，会悄悄过期",
    },
    Proc {
        key: "redis",
        label: "Redis（通道①）",
        what: "MessageBus 总线。无持久化，容器随服务启停",
        unit: "ws-redis",
        if_stopped: "上面全部服务的写入端都会开始报连接失败",
    },
    Proc {
        key: "radar",
        label: "全市场雷达",
        what: "5s 一轮全市场扫描 → radar_board.json",
        unit: "ws-radar",
        if_stopped: "雷达面板停在最后一张快照上",
    },
    Proc {
        key: "news",
        label: "新闻聚合",
        what: "RSS/Atom/JSON 多源抓取 → news_board.json",
        unit: "ws-news",
        if_stopped: "新闻面板停更；源健康页的陈旧判定会开始变红",
    },
    Proc {
        key: "observatory",
        label: "接口观察终端",
        what: "REST/WS/TCP/FIX 探针 → observatory.json",
        unit: "ws-observatory",
        if_stopped: "观察终端面板停更，正在录的帧会中断",
    },
    Proc {
        key: "recorder",
        label: "行情录制器",
        what: "Binance USDS-M L2/成交/标记价 → ~/ws-data parquet",
        unit: "wealthspring-recorder",
        if_stopped: "**录制出现空洞**，且事后无法补——研究数据是一次性的",
    },
    Proc {
        key: "maker-shadow",
        label: "做市影子",
        what: "SOLUSDT 实盘排队仿真（不下真单）",
        unit: "wealthspring-maker-shadow",
        if_stopped: "C4 合格日的连续统计会断档",
    },
];

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
            .map(|p| Row {
                key: p.key.into(),
                label: p.label.into(),
                what: p.what.into(),
                unit: p.unit.into(),
                if_stopped: p.if_stopped.into(),
                st: svcctl::query(p.unit),
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
    fn every_installed_unit_shows_up_on_this_page() {
        // 这一页的价值全在「全」。漏一行，它就从系统全貌退化成
        // 一份不完整的清单——而后者比没有更糟，会让人以为已经全停了。
        let Ok(h) = std::env::var("HOME") else { return };
        let dir = std::path::PathBuf::from(h).join(".config/systemd/user");
        let Ok(rd) = std::fs::read_dir(&dir) else { return };
        let known: Vec<&str> = ALL.iter().map(|p| p.unit).collect();
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if !(n.starts_with("ws-") || n.starts_with("wealthspring-")) || n.ends_with(".timer") {
                continue;
            }
            let stem = n.trim_end_matches(".service");
            // 定时任务的 oneshot service 不列：它本来就不常驻，
            // 「停止」它什么都没停，到点还是会被 timer 拉起来。
            if dir.join(format!("{stem}.timer")).exists() {
                continue;
            }
            assert!(known.contains(&stem), "常驻单元 {stem} 没在进程页上——加一行到 procs::ALL");
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
