//! **后台守护跟随 Cockpit 的生命周期**（docs/26 S4b，用户要求）。
//!
//! 要解决的问题原话是：「不要出现界面已经关闭了但是还有很多**未知的**后台程序还在运行」。
//! 重点在「未知」——不是「机器上不许有后台进程」，是「别有我不知道、也关不掉的」。
//!
//! # 为什么走 target 而不是在这里循环 stop 十一次
//!
//! 各守护的 unit 里写了 `PartOf=ws-stack.target`，于是**级联停止由 systemd 负责**：
//!
//! | 做法 | 面板正常退出 | 面板被 SIGKILL / 崩溃 |
//! |---|---|---|
//! | 循环 `systemctl stop` ×11 | 能停 | **停不掉**——那段代码根本没机会跑 |
//! | `stop ws-stack.target` | 能停 | 同样停不掉，但至少是一条命令、不会停一半 |
//! | 上面 + `PartOf=` | 能停 | 见下 |
//!
//! `PartOf=` 让 systemd 记住这层从属关系，所以停 target 一定是**原子**的、
//! 也不需要面板懂依赖次序（`ws-signals` 要先于两个 feed）。
//!
//! **崩溃这一档没有被完全解决**，说清楚比假装解决了强：Cockpit 被 `kill -9`
//! 时没有任何用户态代码能跑。真正堵死这一档要让 Cockpit 自己也是个 unit
//! （`ws-cockpit.service` + 守护 `BindsTo=` 它），那是 S5 打包时的启动方式。
//! 在此之前，崩溃后留下的守护会在**下次启动 Cockpit 时被接管**（同一个 target，
//! 不会变成孤儿），面板上也一眼看得到它们在跑。
//!
//! # 四个 .timer 不在 target 里
//!
//! 工厂夜跑 / 预测夜跑 / F0 验收 / 观察终端日检是**按点触发的研究任务**。
//! 绑进来等于「Cockpit 没开着就永远不跑」——那不是关掉后台，是把夜跑废掉。
//! 它们在「进程」页上单列并显示下次触发时刻，于是它们是**已知**的，
//! 满足用户那句话的实际要求。
//!
//! # 停掉的代价不是对称的
//!
//! 大部分守护停了只是「停更」，重启就补回来。两个不是：
//! 录制器停多久、行情 parquet 就少多久（**事后无法补录**），
//! 做市影子停一次、C4 合格日的连续统计就断一次。
//! 用户明确要求「所有」，所以它们照绑；代价标在面板上，不藏着。

use std::process::Command;

pub const TARGET: &str = "ws-stack.target";

/// 装 panic 钩子：崩溃时也把守护停掉。
///
/// **这只是补丁，不是解法。** 真正的解法是让 Cockpit 由 systemd 拉起
/// （`ws-cockpit.service`），target 上的 `BindsTo=` 覆盖**任何**结束方式。
/// 本钩子只覆盖 panic——`kill -9`、OOM、段错误都轮不到它跑。
///
/// 为什么还是要装：开发时直接跑 target/release 下的二进制是常态，
/// 而实测就撞到过一次 wgpu panic 导致十一个守护全留着。
/// 覆盖 90% 比覆盖 0% 强，前提是别把它当成覆盖了 100%。
pub fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // 先让原钩子打印回溯——停服务失败也不该吃掉崩溃信息
        prev(info);
        let _ = stop_all();
    }));
}

/// Cockpit 启动时拉起全部守护。
///
/// `--no-block`：不等它们真的起来。一个 GUI 卡在启动画面上等十一个服务
/// 依次拉起（其中还有要连网的）是很糟的手感，而各面板本来就能处理
/// 「守护还没起」——它们读快照，读不到就显示「等待数据」。
pub fn start_all() -> std::io::Result<()> {
    Command::new("systemctl").args(["--user", "start", "--no-block", TARGET]).status()?;
    Ok(())
}

/// Cockpit 退出时停掉全部守护。
///
/// 这里**不能**用 `--no-block`：进程马上就要没了，得让 systemd 先把请求收下。
/// `stop` 收下请求即返回（真正的停止是 systemd 异步做的），所以不会拖慢退出。
pub fn stop_all() -> std::io::Result<()> {
    Command::new("systemctl").args(["--user", "stop", TARGET]).status()?;
    Ok(())
}

/// target 现在是不是活的（用于面板显示「守护跟随本窗口」的状态）。
pub fn target_active() -> bool {
    Command::new("systemctl")
        .args(["--user", "is-active", TARGET])
        .output()
        .map(|o| o.stdout.starts_with(b"active"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_target_name_matches_the_unit_file() {
        // 名字对不上的症状是「点了没反应」——systemctl 对不存在的单元
        // 返回非零，而我们没在 UI 上显示这个错。
        assert_eq!(TARGET, "ws-stack.target");
        let Ok(h) = std::env::var("HOME") else { return };
        let p = std::path::PathBuf::from(h).join(".config/systemd/user").join(TARGET);
        if p.exists() {
            let s = std::fs::read_to_string(&p).unwrap_or_default();
            assert!(s.contains("[Unit]"), "{TARGET} 不像一个 unit 文件");
        }
    }

    #[test]
    fn every_managed_service_is_in_the_target() {
        // 「进程」页上列着的常驻服务，必须都真的被 target 管着——
        // 否则关掉界面时会**漏掉几个**，而这正是用户要消掉的那件事。
        let Ok(h) = std::env::var("HOME") else { return };
        let p = std::path::PathBuf::from(h).join(".config/systemd/user").join(TARGET);
        let Ok(target) = std::fs::read_to_string(&p) else { return };
        for proc in super::super::procs::ALL {
            // timer 有意不绑（绑了等于把夜跑废掉），只检查常驻守护
            if proc.kind == super::super::procs::Kind::Timer {
                continue;
            }
            assert!(
                target.contains(&format!("{}.service", proc.unit)),
                "{} 在「进程」页上但不在 {TARGET} 里——关掉界面时它会被落下",
                proc.unit
            );
        }
    }
}
