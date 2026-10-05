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
    let root = super::paths::repo_root();
    let prov = r.data_provenance.map(|p| p.strategy).unwrap_or_default();
    // ① 结果里记了仓库相对路径（新结果）→ 直接用
    if !prov.path.is_empty() {
        let p = root.join(&prov.path);
        if p.is_file() {
            return Some(p);
        }
    }
    // ② 只有文件名（旧结果 / 没写 path 的运行器）→ 在 strategies/ 下按名找。
    //    strategies/ 分了主题子目录，只拼「strategies/<文件名>」会找不到。
    let file = Some(prov.file)
        .filter(|f| !f.is_empty())
        .or_else(|| (!r.meta.strategy.is_empty()).then(|| format!("{}.py", r.meta.strategy)))?;
    if std::path::Path::new(&file).is_absolute() {
        let p = PathBuf::from(&file);
        return p.is_file().then_some(p);
    }
    find_named(&root.join("strategies"), &file)
}

/// 在目录树里按文件名找第一个匹配（跳过结果 / 数据目录）。
fn find_named(dir: &std::path::Path, name: &str) -> Option<PathBuf> {
    let direct = dir.join(name);
    if direct.is_file() {
        return Some(direct);
    }
    let mut subs: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter(|p| {
            let n = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            !matches!(n.as_str(), "backtest_results" | "data" | "__pycache__" | "pm_flow_log" | "mt5")
                && !n.starts_with('.')
        })
        .collect();
    subs.sort();
    subs.iter().find_map(|d| find_named(d, name))
}

/// 把策略发给 Studio 打开。返回给人看的一句话（成功或失败原因）。
pub fn open_in_studio() -> Result<String, String> {
    let p = strategy_to_open().ok_or("没有可打开的策略：在「回测」面板里选一个，或先跑一次回测")?;
    bridge::send(Peer::Studio, &UiCommand::OpenFile { path: p.display().to_string(), line: None })
}

#[cfg(test)]
mod find_named_tests {
    use super::find_named;

    #[test]
    fn finds_strategy_in_topic_subdir_and_skips_results() {
        let root = std::env::temp_dir().join(format!("ws-find-named-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("research/quantum_queen")).unwrap();
        std::fs::create_dir_all(root.join("backtest_results/x")).unwrap();
        std::fs::write(root.join("backtest_results/x/qq.py"), "").unwrap();
        std::fs::write(root.join("research/quantum_queen/qq.py"), "").unwrap();
        assert_eq!(find_named(&root, "qq.py"), Some(root.join("research/quantum_queen/qq.py")));
        assert_eq!(find_named(&root, "none.py"), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
