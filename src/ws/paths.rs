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

// ── 数据目录（docs/47）：大数据 `data_dir` + 经常写的小文件 `hot_dir` ─────────────────
//
// 用户在 `<配置目录>/paths.toml` 里自定义（Cockpit 设置窗口「数据目录…」也写这个文件）：
//
//     data_dir = "/data/ubuntu/ws-data"   # 大数据：raw / features / signals / labels / zoo / ofms / bars / studies…
//     hot_dir = "~/ws-data"               # 经常写的小文件：*.sqlite、live/、cockpit/、reports/、chart_drawings/
//     require_mount = "/data"             # 可不写；写了就要求它真的挂着
//
// 取值顺序：环境变量（`WS_DATA_DIR` / `WS_HOT_DIR` / `WS_REQUIRE_MOUNT`）→ paths.toml →
// `$XDG_DATA_HOME/wealthspring` → `~/ws-data`。`hot_dir` 没配时等于 `data_dir`。
// **两个目录缺省相同**：不写配置时与 docs/47 之前一字不差（改缺省等于让已录的数据凭空消失）。
//
// 调用方自己选冷热，不按名字自动分流（自动分流选错了不显眼）。
// **永远不创建根目录**：挂载盘没挂上时根目录不存在，`create_dir_all` 会在根分区上造一个空目录、
// 然后悄悄写进去——这正是 docs/47 要杜绝的。只在根目录存在时建它下面的子目录。

/// `paths.toml` 里的三项（都可不写）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathsConfig {
    pub data_dir: Option<String>,
    pub hot_dir: Option<String>,
    pub require_mount: Option<String>,
}

/// 读 `paths.toml`：只认 `键 = "值"` 这一种行（`#` 起是注释），不拉 toml 依赖。认不得的行忽略。
pub fn parse_paths_toml(s: &str) -> PathsConfig {
    let mut c = PathsConfig::default();
    for line in s.lines() {
        // `#` 起是注释——引号里的 `#` 不算
        let line = match line.split_once('#') {
            Some((code, _)) if code.matches('"').count() % 2 == 0 => code,
            _ => line,
        }
        .trim();
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim();
        let Some(v) = v.strip_prefix('"').and_then(|v| v.strip_suffix('"')) else { continue };
        let v = (!v.trim().is_empty()).then(|| v.trim().to_string());
        match k.trim() {
            "data_dir" => c.data_dir = v,
            "hot_dir" => c.hot_dir = v,
            "require_mount" => c.require_mount = v,
            _ => {}
        }
    }
    c
}

/// 写回 `paths.toml` 的文本（Cockpit 设置窗口用）。
pub fn render_paths_toml(c: &PathsConfig) -> String {
    let mut s = String::from("# WealthSpring 数据目录（docs/47）。改了要重启 Cockpit 与守护才生效\n");
    let line = |k: &str, v: &Option<String>, note: &str| match v {
        Some(v) => format!("# {note}\n{k} = \"{v}\"\n"),
        None => format!("# {note}\n# {k} = \"\"\n"),
    };
    s += &line("data_dir", &c.data_dir, "大数据：录制的行情、特征、信号、标签、订单流层析……可以放在挂载盘上");
    s += &line("hot_dir", &c.hot_dir, "经常写的小文件：SQLite 库、实时断点、面板快照、报告。放本机盘");
    s += &line("require_mount", &c.require_mount, "大数据所在的挂载点（可不写）。写了就要求它真的挂着，否则拒绝读写、界面弹提醒");
    s
}

/// 展开前导 `~`。
pub fn expand_home(p: &str, home: Option<&str>) -> PathBuf {
    match (p.strip_prefix('~'), ok(home)) {
        (Some(rest), Some(h)) => PathBuf::from(h).join(rest.trim_start_matches('/')),
        _ => PathBuf::from(p),
    }
}

/// 纯解析：大数据目录。
pub fn data_dir_from(env: Option<&str>, cfg: &PathsConfig, xdg_data: Option<&str>, home: Option<&str>) -> PathBuf {
    if let Some(e) = ok(env) {
        return expand_home(e, home);
    }
    if let Some(c) = &cfg.data_dir {
        return expand_home(c, home);
    }
    match ok(xdg_data) {
        Some(x) => PathBuf::from(x).join("wealthspring"),
        None => PathBuf::from(ok(home).unwrap_or(".")).join("ws-data"),
    }
}

/// 纯解析：经常写的小文件的目录；没配就是 `data_dir`。
pub fn hot_dir_from(env: Option<&str>, cfg: &PathsConfig, data_dir: &std::path::Path, home: Option<&str>) -> PathBuf {
    if let Some(e) = ok(env) {
        return expand_home(e, home);
    }
    match &cfg.hot_dir {
        Some(c) => expand_home(c, home),
        None => data_dir.to_path_buf(),
    }
}

/// 纯解析：要求挂着的挂载点。
pub fn require_mount_from(env: Option<&str>, cfg: &PathsConfig, home: Option<&str>) -> Option<PathBuf> {
    ok(env).map(|e| expand_home(e, home)).or_else(|| cfg.require_mount.as_deref().map(|c| expand_home(c, home)))
}

/// `paths.toml` 的位置。
pub fn paths_file() -> PathBuf {
    config_dir().join("paths.toml")
}

/// 读本机的 `paths.toml`（没有 = 全缺省）。
pub fn paths_config() -> PathsConfig {
    std::fs::read_to_string(paths_file()).map(|s| parse_paths_toml(&s)).unwrap_or_default()
}

fn env_s(k: &str) -> Option<String> {
    std::env::var(k).ok()
}

/// 解析好的三项：进程里只读一次（改了要重启——守护与面板各自缓存了路径，热切换会一半写旧一半写新）。
pub fn resolved() -> &'static (PathBuf, PathBuf, Option<PathBuf>) {
    static R: std::sync::OnceLock<(PathBuf, PathBuf, Option<PathBuf>)> = std::sync::OnceLock::new();
    R.get_or_init(|| {
        let cfg = paths_config();
        let home = env_s("HOME");
        let d = data_dir_from(env_s("WS_DATA_DIR").as_deref(), &cfg, env_s("XDG_DATA_HOME").as_deref(), home.as_deref());
        let h = hot_dir_from(env_s("WS_HOT_DIR").as_deref(), &cfg, &d, home.as_deref());
        let m = require_mount_from(env_s("WS_REQUIRE_MOUNT").as_deref(), &cfg, home.as_deref());
        (d, h, m)
    })
}

/// 大数据目录（录制、特征、信号、标签、订单流层析、缓存的 K 线、研究产物）。
pub fn data_dir() -> PathBuf {
    resolved().0.clone()
}

/// 经常写的小文件的目录（SQLite 库、`live/`、`cockpit/`、`reports/`、`chart_drawings/`）。
pub fn hot_dir() -> PathBuf {
    resolved().1.clone()
}

/// 配置里要求挂着的挂载点。
pub fn require_mount() -> Option<PathBuf> {
    resolved().2.clone()
}

/// 一个数据目录哪里不对。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// 挂载点没挂上（它与上一级在同一个设备上）
    NotMounted(PathBuf),
    /// 目录不在要求的挂载点下面
    OutsideMount(PathBuf),
    /// 目录不存在（程序不会替你建根目录）
    Missing,
    /// 目录存在但写不进去（只读挂载、权限）
    ReadOnly(String),
}

impl Problem {
    /// 给人看的一句话：什么不对、怎么修。
    pub fn describe(&self, dir: &std::path::Path) -> String {
        match self {
            Self::NotMounted(m) => format!(
                "挂载点 {} 没有挂上，{} 不可用。挂上后再试：sudo mount {}（Windows 那块盘还要在 Windows 里关掉「快速启动」）",
                m.display(),
                dir.display(),
                m.display()
            ),
            Self::OutsideMount(m) => format!("{} 不在要求的挂载点 {} 下面——检查 paths.toml 里的 data_dir 与 require_mount", dir.display(), m.display()),
            Self::Missing => format!("{} 不存在。程序不会自动创建数据根目录（免得盘没挂上时写到根分区上）；确认路径无误后手动建好", dir.display()),
            Self::ReadOnly(e) => format!("{} 写不进去（{e}）。NTFS 盘被 Windows 快速启动 / 休眠锁住时会挂成只读", dir.display()),
        }
    }
}

/// `p` 是不是一个挂载点：它与上一级目录不在同一个设备上（根目录算）。
pub fn is_mountpoint(p: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(m) = std::fs::metadata(p) else { return false };
    match p.parent() {
        None => true,
        Some(parent) => std::fs::metadata(parent).is_ok_and(|pm| pm.dev() != m.dev()),
    }
}

/// 查一个数据根目录：挂载点（若要求）→ 存在 → 可写（写一个探针文件再删）。
pub fn check_dir(dir: &std::path::Path, mount: Option<&std::path::Path>) -> Option<Problem> {
    if let Some(m) = mount {
        if !dir.starts_with(m) {
            return Some(Problem::OutsideMount(m.to_path_buf()));
        }
        if !is_mountpoint(m) {
            return Some(Problem::NotMounted(m.to_path_buf()));
        }
    }
    if !dir.is_dir() {
        return Some(Problem::Missing);
    }
    let probe = dir.join(format!(".ws-write-probe-{}", std::process::id()));
    match std::fs::OpenOptions::new().write(true).create(true).truncate(true).open(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            None
        }
        Err(e) => Some(Problem::ReadOnly(e.to_string())),
    }
}

/// 两个目录的检查结果：`[("大数据目录", 路径, 问题), ("小文件目录", …)]`（两者相同时只一项）。
pub fn check() -> Vec<(&'static str, PathBuf, Option<Problem>)> {
    let (d, h, m) = resolved();
    let mut out = Vec::new();
    // 挂载要求只管大数据目录；小文件目录若也在那个挂载点下面，一样要查
    out.push(("大数据目录", d.clone(), check_dir(d, m.as_deref())));
    if h != d {
        let hm = m.as_deref().filter(|m| h.starts_with(m));
        out.push(("小文件目录", h.clone(), check_dir(h, hm)));
    }
    out
}

/// 守护开头调用：大数据目录不能用就带说明退出（systemd 里显示 failed），不悄悄写到别处。
pub fn require_data_dir() -> PathBuf {
    for (role, dir, p) in check() {
        if let Some(p) = p {
            eprintln!("[wealthspring] {role}不可用：{}", p.describe(&dir));
            std::process::exit(78); // EX_CONFIG
        }
    }
    data_dir()
}

/// 守护开头调用：只用小文件目录的（不碰大数据）。
pub fn require_hot_dir() -> PathBuf {
    let (d, h, m) = resolved();
    let hm = m.as_deref().filter(|m| h.starts_with(m));
    if let Some(p) = check_dir(h, hm) {
        eprintln!("[wealthspring] 小文件目录不可用：{}", p.describe(h));
        std::process::exit(78);
    }
    let _ = d;
    hot_dir()
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

    // ── 数据目录（docs/47）：以下与孪生文件同名同断言 ──

    #[test]
    fn recorded_data_keeps_its_historical_home() {
        // 回落成 ~/.local/share/wealthspring 的话，已录的历史会凭空消失。
        let none = PathsConfig::default();
        assert_eq!(data_dir_from(None, &none, None, Some("/home/x")), PathBuf::from("/home/x/ws-data"));
        let d = data_dir_from(None, &none, None, Some("/home/x"));
        assert_eq!(hot_dir_from(None, &none, &d, Some("/home/x")), d, "不写配置时两个目录相同");
    }

    #[test]
    fn paths_toml_beats_xdg_and_env_beats_paths_toml() {
        let cfg = parse_paths_toml("# 注释\ndata_dir = \"/data/ubuntu/ws-data\"\nhot_dir = \"~/ws-hot\"  # 行尾注释\nrequire_mount = \"/data\"\nother = 1\n");
        assert_eq!(cfg.data_dir.as_deref(), Some("/data/ubuntu/ws-data"));
        assert_eq!(data_dir_from(None, &cfg, Some("/xdg"), Some("/home/x")), PathBuf::from("/data/ubuntu/ws-data"));
        assert_eq!(data_dir_from(Some("/env"), &cfg, Some("/xdg"), Some("/home/x")), PathBuf::from("/env"));
        assert_eq!(data_dir_from(Some(""), &cfg, None, Some("/home/x")), PathBuf::from("/data/ubuntu/ws-data"), "空环境变量 = 没设");
        let d = PathBuf::from("/data/ubuntu/ws-data");
        assert_eq!(hot_dir_from(None, &cfg, &d, Some("/home/x")), PathBuf::from("/home/x/ws-hot"), "~ 展开");
        assert_eq!(require_mount_from(None, &cfg, Some("/home/x")), Some(PathBuf::from("/data")));
        assert_eq!(parse_paths_toml(&render_paths_toml(&cfg)), cfg, "写回去再读一样");
    }

    #[test]
    fn a_missing_root_is_reported_and_never_created() {
        let base = std::env::temp_dir().join(format!("ws-paths-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("data");
        assert_eq!(check_dir(&dir, None), Some(Problem::Missing));
        assert!(!dir.exists(), "检查不建目录");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(check_dir(&dir, None), None);
        assert!(std::fs::read_dir(&dir).unwrap().next().is_none(), "探针文件删干净");
        // 要求的挂载点：临时目录与上一级同一个设备 → 没挂上
        assert_eq!(check_dir(&dir, Some(&base)), Some(Problem::NotMounted(base.clone())));
        assert_eq!(check_dir(&dir, Some(std::path::Path::new("/elsewhere"))), Some(Problem::OutsideMount(PathBuf::from("/elsewhere"))));
        assert!(is_mountpoint(std::path::Path::new("/")));
        let _ = std::fs::remove_dir_all(&base);
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
