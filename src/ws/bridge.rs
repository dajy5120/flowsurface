//! Cockpit 这一端的跨进程命令（docs/35 §16.5 第 4 项；协议见 `wealthspring_ui_tokens::bridge`）。
//!
//! - 收：Studio 发来「在 Cockpit 中查看本次回测」→ 进收件箱，下一次 Tick 取出执行
//!   （切到回测工作区并把窗口提到前面）。监听线程只入队，不碰界面状态。
//! - 发：命令面板「在 Studio 中打开策略」→ 把当前策略文件发给 Studio。

use std::path::PathBuf;
use std::sync::Mutex;

pub use wealthspring_ui_tokens::bridge::{Peer, UiCommand};
use wealthspring_ui_tokens::bridge;

static INBOX: Mutex<Vec<UiCommand>> = Mutex::new(Vec::new());

/// 开始监听。另一个 Cockpit 已在监听 / 绑不上 → 记一行日志，不影响启动。
pub fn start() {
    let r = bridge::listen(Peer::Cockpit, |cmd| match cmd {
        UiCommand::Ping => Ok("Cockpit 在".into()),
        UiCommand::ShowRun { .. } => {
            if let Ok(mut g) = INBOX.lock() {
                g.push(cmd);
            }
            Ok("Cockpit 已收到，正在切到回测工作区".into())
        }
        UiCommand::OpenFile { .. } => Err("Cockpit 不打开文件（发给 Studio）".into()),
    });
    if let Err(e) = r {
        log::warn!("[bridge] 跨进程命令监听没起来：{e}");
    }
}

/// 取出收件箱里的全部命令（Tick 里调）。
pub fn take() -> Vec<UiCommand> {
    INBOX.lock().map(|mut g| std::mem::take(&mut *g)).unwrap_or_default()
}

/// 「在 Studio 中打开策略」打开哪个文件：回测面板里选的策略 → 最近一次回测结果记录的策略文件。
pub fn strategy_to_open() -> Option<PathBuf> {
    if let Some(p) = super::backtest_launch::view().strategy.map(PathBuf::from)
        && p.is_file()
    {
        return Some(p);
    }
    let r = super::backtest_readout::snapshot();
    let dir = super::paths::repo_root().join("strategies");
    let file = r
        .data_provenance
        .map(|p| p.strategy.file)
        .filter(|f| !f.is_empty())
        .or_else(|| (!r.meta.strategy.is_empty()).then(|| format!("{}.py", r.meta.strategy)))?;
    let p = if std::path::Path::new(&file).is_absolute() { PathBuf::from(&file) } else { dir.join(&file) };
    p.is_file().then_some(p)
}

/// 把策略发给 Studio 打开。返回给人看的一句话（成功或失败原因）。
pub fn open_in_studio() -> Result<String, String> {
    let p = strategy_to_open().ok_or("没有可打开的策略：在「回测」面板里选一个，或先跑一次回测")?;
    bridge::send(Peer::Studio, &UiCommand::OpenFile { path: p.display().to_string(), line: None })
}
