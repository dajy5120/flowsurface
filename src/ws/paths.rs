//! 路径定义。**守护侧 `crates/wealthspring-paths/src/lib.rs` 有一份逐字相同的实现。**
//!
//! 为什么抄一份而不是依赖它：flowsurface 是 vendored fork、独立 workspace，
//! 刻意不依赖任何 wealthspring crate——改动全部是新增文件，`Cargo.toml`
//! 这种冲突高发的文件一行不碰，上游 rebase 才不会痛。代价就是这份孪生体。
//!
//! 靠两边**同名的单测**钉住同样的行为。一边改了另一边没改，表现是面板永远
//! 「等待数据」而守护日志一切正常——两边都不报错，最难查的那种。
//!
//! 统一之前（docs/26 S3）这里散着三套不同的兜底：radar_readout 走 `/dev/shm`、
//! observatory_readout 写死 `/run/user/1000`、tardis_* 六处各自
//! `env::var("HOME").unwrap_or("/home/dajy")`。

use std::path::PathBuf;

/// 空串按「没设」算：systemd 里未设的变量常常是空串而不是不存在。
fn ok(s: Option<&str>) -> Option<&str> {
    s.filter(|v| !v.is_empty())
}

/// 快照与控制文件的落点：**内存文件系统，不写硬盘**。
///
///   1. `$XDG_RUNTIME_DIR/wealthspring`——systemd 挂的 tmpfs，0700、登出即清。
///   2. `/dev/shm/wealthspring-$USER`——没有 XDG_RUNTIME_DIR 时的 tmpfs 兜底。
///   3. `$HOME/ws-data/live`——**会写盘**，只在系统完全没有 tmpfs 时走到。
pub fn runtime_dir_from(
    xdg: Option<&str>,
    user: Option<&str>,
    home: Option<&str>,
    dev_shm_exists: bool,
) -> PathBuf {
    if let Some(x) = ok(xdg) {
        return PathBuf::from(x).join("wealthspring");
    }
    if dev_shm_exists {
        return PathBuf::from(format!("/dev/shm/wealthspring-{}", ok(user).unwrap_or("ws")));
    }
    PathBuf::from(ok(home).unwrap_or("/tmp")).join("ws-data/live")
}

pub fn runtime_dir() -> PathBuf {
    runtime_dir_from(
        std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
        std::env::var("USER").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
        std::path::Path::new("/dev/shm").is_dir(),
    )
}

/// 用户配置：**该留在盘上**——用户手编的源列表、订阅规则，放 tmpfs 就是登出即丢。
pub fn config_dir_from(xdg_config: Option<&str>, home: Option<&str>) -> PathBuf {
    let base = match ok(xdg_config) {
        Some(x) => PathBuf::from(x),
        None => PathBuf::from(ok(home).unwrap_or(".")).join(".config"),
    };
    base.join("wealthspring")
}

pub fn config_dir() -> PathBuf {
    config_dir_from(
        std::env::var("XDG_CONFIG_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

/// 长期数据（录制的行情等）。回落**不是** `~/.local/share/wealthspring`：
/// 录制器早就在往 `~/ws-data` 写，改默认值等于让已录的历史数据凭空消失。
pub fn data_dir_from(xdg_data: Option<&str>, home: Option<&str>) -> PathBuf {
    match ok(xdg_data) {
        Some(x) => PathBuf::from(x).join("wealthspring"),
        None => PathBuf::from(ok(home).unwrap_or(".")).join("ws-data"),
    }
}

pub fn data_dir() -> PathBuf {
    data_dir_from(
        std::env::var("XDG_DATA_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

/// 仓库/安装根：脚本、Python 包、`recorder.toml` 之类**跟代码一起走**的东西。
///
/// `WS_REPO` → 打包布局 `/usr/lib/wealthspring` → `$HOME/dev/WealthSpring`，
/// **后两档要求目录真的存在**。原先这里是六处写死的 `{home}/dev/WealthSpring`。
pub fn repo_root_from(ws_repo: Option<&str>, home: Option<&str>, exists: &dyn Fn(&str) -> bool) -> PathBuf {
    if let Some(p) = ok(ws_repo) {
        return PathBuf::from(p);
    }
    let mut cands = vec!["/usr/lib/wealthspring".to_string()];
    if let Some(h) = ok(home) {
        cands.push(format!("{h}/dev/WealthSpring"));
    }
    for c in &cands {
        if exists(c) {
            return PathBuf::from(c);
        }
    }
    PathBuf::from(&cands[0])
}

pub fn repo_root() -> PathBuf {
    repo_root_from(
        std::env::var("WS_REPO").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
        &|p| std::path::Path::new(p).is_dir(),
    )
}
/// 装了 Nautilus/factory 的 venv python。
///
/// 阶梯里**只有第一档是声明，后面几档要求文件真的存在**：历史默认值
/// `/tmp/ws-venv/bin/python` 指向的东西根本不存在（`/tmp` 重启即失）——
/// 返回一个不存在的解释器，错误要到子进程 spawn 失败时才浮出来。
///
/// **`~/dev/ws-venv` 这一档已删除（2026-09-11，整改 E-01）**：机器上原本并存两个
/// venv，numpy 分别是 2.2.6 与 2.4.6，而它们共享同一份计算代码。现已合并为唯一的
/// `~/ws-venv`；留着那一档等于给「哪天有人重建了那个目录、代码又悄悄选错」留一扇门。
///
/// ⚠ 本文件是 `crates/wealthspring-paths` 的**逐字孪生**（docs/26 S3）——
/// 那边改了这里必须跟着改，两边同名的单测就是钉住这件事的。
pub fn python_from(ws_python: Option<&str>, home: Option<&str>, exists: &dyn Fn(&str) -> bool) -> PathBuf {
    if let Some(p) = ok(ws_python) {
        return PathBuf::from(p);
    }
    let mut cands = vec!["/usr/lib/wealthspring/venv/bin/python".to_string()];
    if let Some(h) = ok(home) {
        cands.push(format!("{h}/ws-venv/bin/python"));
    }
    for c in &cands {
        if exists(c) {
            return PathBuf::from(c);
        }
    }
    PathBuf::from(&cands[0])
}

pub fn python() -> PathBuf {
    python_from(
        std::env::var("WS_PYTHON").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
        &|p| std::path::Path::new(p).is_file(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // 以下五条与守护侧 `wealthspring-paths` 同名同断言。改一边必须改另一边。

    #[test]
    fn every_component_agrees_under_systemd() {
        assert_eq!(
            runtime_dir_from(Some("/run/user/1000"), Some("dajy"), Some("/home/dajy"), true),
            PathBuf::from("/run/user/1000/wealthspring")
        );
    }

    #[test]
    fn an_empty_env_var_is_not_a_value() {
        let p = runtime_dir_from(Some(""), Some("dajy"), Some("/home/dajy"), true);
        assert_eq!(p, PathBuf::from("/dev/shm/wealthspring-dajy"));
    }

    #[test]
    fn without_xdg_it_still_lands_on_tmpfs() {
        assert_eq!(
            runtime_dir_from(None, Some("dajy"), Some("/home/dajy"), true),
            PathBuf::from("/dev/shm/wealthspring-dajy")
        );
    }

    #[test]
    fn only_a_machine_with_no_tmpfs_at_all_touches_the_disk() {
        assert_eq!(
            runtime_dir_from(None, None, Some("/home/dajy"), false),
            PathBuf::from("/home/dajy/ws-data/live")
        );
    }

    #[test]
    fn config_goes_to_disk_not_tmpfs() {
        assert_eq!(
            config_dir_from(None, Some("/home/x")),
            PathBuf::from("/home/x/.config/wealthspring")
        );
        assert_eq!(config_dir_from(Some("/cfg"), Some("/home/x")), PathBuf::from("/cfg/wealthspring"));
    }

    #[test]
    fn recorded_data_keeps_its_historical_home() {
        assert_eq!(data_dir_from(None, Some("/home/x")), PathBuf::from("/home/x/ws-data"));
    }

#[test]
    fn a_declared_repo_root_is_taken_on_faith() {
        let never = |_: &str| false;
        assert_eq!(repo_root_from(Some("/srv/ws"), Some("/home/x"), &never), PathBuf::from("/srv/ws"));
    }

    #[test]
    fn an_unset_repo_root_picks_one_that_actually_exists() {
        let only_dev = |p: &str| p == "/home/x/dev/WealthSpring";
        assert_eq!(
            repo_root_from(None, Some("/home/x"), &only_dev),
            PathBuf::from("/home/x/dev/WealthSpring")
        );
    }

    #[test]
    fn the_packaged_repo_layout_wins_over_a_dev_checkout() {
        let both = |_: &str| true;
        assert_eq!(repo_root_from(None, Some("/home/x"), &both), PathBuf::from("/usr/lib/wealthspring"));
    }

    #[test]
    fn an_unset_python_picks_one_that_actually_exists() {
        let only_home = |p: &str| p == "/home/x/ws-venv/bin/python";
        assert_eq!(
            python_from(None, Some("/home/x"), &only_home),
            PathBuf::from("/home/x/ws-venv/bin/python")
        );
    }

    /// E-01：合并成唯一 venv 之后，`~/dev/ws-venv` 不再是候选。
    /// 与 `crates/wealthspring-paths` 同名测试成对——两边行为必须一致。
    #[test]
    fn the_retired_dev_venv_is_never_picked() {
        let only_dev = |p: &str| p == "/home/x/dev/ws-venv/bin/python";
        assert_ne!(
            python_from(None, Some("/home/x"), &only_dev),
            PathBuf::from("/home/x/dev/ws-venv/bin/python")
        );
    }

    #[test]
    fn no_path_here_names_a_person() {
        // 统一之前这一层散落着 6 处 `unwrap_or("/home/dajy")`。
        for p in [runtime_dir(), config_dir(), data_dir()] {
            assert!(!p.to_string_lossy().contains("dajy") || std::env::var("HOME").unwrap_or_default().contains("dajy"));
        }
    }
}
