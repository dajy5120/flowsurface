//! 数据目录（docs/47）：后台每 30 秒查一次大数据目录与小文件目录（挂载点挂着、存在、可写），
//! 不过就在界面里弹出提醒、状态栏常驻一个红色提示；设置窗口「数据目录…」改 `paths.toml`。
//!
//! 只查、只提醒，不替用户建目录、不挂盘：程序永远不创建数据根目录（免得盘没挂上时写到根分区上）。
//! 路径进程里只解析一次（`paths::resolved`），改了配置要重启——设置卡片里写明。

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::paths;

/// 一个目录的检查结果。
#[derive(Debug, Clone)]
pub struct Row {
    pub role: &'static str,
    pub path: PathBuf,
    /// 没问题 = `None`；否则一句「什么不对、怎么修」
    pub problem: Option<String>,
}

/// 设置卡片里正在编辑的三项。
#[derive(Debug, Clone, Default)]
pub struct Card {
    pub data: String,
    pub hot: String,
    pub mount: String,
    /// 「检查」的结果（角色, 路径, 问题）
    pub preview: Vec<(String, String, Option<String>)>,
    pub note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Data,
    Hot,
    Mount,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataDirsMsg {
    /// 提醒里的「重新检查」
    Recheck,
    /// 提醒里的「知道了」：同一个问题不再弹，状态变了再弹
    Dismiss,
    /// 打开设置卡片
    Open,
    Close,
    Edit(Field, String),
    /// 用卡片里填的值试查一遍（不保存）
    Test,
    Save,
}

#[derive(Default)]
struct State {
    rows: Vec<Row>,
    checked: Option<Instant>,
    /// 点「知道了」时的问题签名
    dismissed: String,
    card: Option<Card>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    f(g.get_or_insert_with(State::default))
}

/// 历史数据（Tardis / Databento）也在要求的挂载点上时，提醒里一并写上。
fn history_note(mount: &std::path::Path) -> Option<String> {
    let roots = [super::data_picker::default_root("tardis"), super::data_picker::default_root("databento")];
    roots.iter().any(|r| std::path::Path::new(r).starts_with(mount)).then(|| format!("历史数据（Tardis / Databento）也在 {} 上，同样读不到", mount.display()))
}

/// 现在查一遍（几个 stat + 一个探针文件；不读数据）。
pub fn refresh() {
    let rows: Vec<Row> = paths::check()
        .into_iter()
        .map(|(role, path, p)| {
            let problem = p.map(|p| {
                let mut s = p.describe(&path);
                if let (paths::Problem::NotMounted(m), true) = (&p, role == "大数据目录")
                    && let Some(h) = history_note(m)
                {
                    s = format!("{s}。{h}");
                }
                s
            });
            Row { role, path, problem }
        })
        .collect();
    with(|st| {
        let sig = signature(&rows);
        if sig.is_empty() {
            st.dismissed.clear(); // 修好了：下次再坏还要弹
        }
        st.rows = rows;
        st.checked = Some(Instant::now());
    });
}

fn signature(rows: &[Row]) -> String {
    rows.iter().filter_map(|r| r.problem.as_ref().map(|p| format!("{}|{}", r.role, p))).collect::<Vec<_>>().join("\n")
}

/// 启动时查一次，之后后台每 30 秒查一次。
pub fn start() {
    refresh();
    super::spawn_named("ws-datadirs", || {
        loop {
            std::thread::sleep(Duration::from_secs(30));
            refresh();
        }
    });
}

/// 有问题的目录（状态栏常驻提示用）：`None` = 都好。
#[must_use]
pub fn problem_summary() -> Option<(String, String)> {
    with(|st| {
        let bad: Vec<&Row> = st.rows.iter().filter(|r| r.problem.is_some()).collect();
        if bad.is_empty() {
            return None;
        }
        let label = if bad.iter().any(|r| r.problem.as_deref().is_some_and(|p| p.contains("没有挂上"))) { "▲ 数据盘未挂载" } else { "▲ 数据目录不可用" };
        let tip = bad.iter().map(|r| format!("{}：{}", r.role, r.problem.clone().unwrap_or_default())).collect::<Vec<_>>().join("\n");
        Some((label.to_string(), format!("{tip}\n点这里看详情")))
    })
}

/// 要弹的提醒：有问题、且不是已经点过「知道了」的同一个问题。
#[must_use]
pub fn alert() -> Option<Vec<Row>> {
    with(|st| {
        let sig = signature(&st.rows);
        (!sig.is_empty() && sig != st.dismissed && st.card.is_none()).then(|| st.rows.clone())
    })
}

/// 设置卡片（开着时）。
#[must_use]
pub fn card() -> Option<Card> {
    with(|st| st.card.clone())
}

/// 当前生效的三项（进程启动时解析的）。
#[must_use]
pub fn effective() -> (PathBuf, PathBuf, Option<PathBuf>) {
    paths::resolved().clone()
}

/// 最近一次检查的结果。
#[must_use]
pub fn rows() -> Vec<Row> {
    with(|st| st.rows.clone())
}

fn opt(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

fn test_card(c: &Card) -> Vec<(String, String, Option<String>)> {
    let home = std::env::var("HOME").ok();
    let cfg = paths::PathsConfig { data_dir: opt(&c.data), hot_dir: opt(&c.hot), require_mount: opt(&c.mount) };
    // 试查不看环境变量：看的是「写进 paths.toml 之后」会怎样
    let d = paths::data_dir_from(None, &cfg, std::env::var("XDG_DATA_HOME").ok().as_deref(), home.as_deref());
    let h = paths::hot_dir_from(None, &cfg, &d, home.as_deref());
    let m = paths::require_mount_from(None, &cfg, home.as_deref());
    let mut out = vec![("大数据目录".to_string(), d.display().to_string(), paths::check_dir(&d, m.as_deref()).map(|p| p.describe(&d)))];
    if h != d {
        let hm = m.as_deref().filter(|m| h.starts_with(m));
        out.push(("小文件目录".to_string(), h.display().to_string(), paths::check_dir(&h, hm).map(|p| p.describe(&h))));
    }
    out
}

/// 把卡片写进 `paths.toml`（原子写：先写临时文件再改名）。
fn save(c: &Card) -> Result<PathBuf, String> {
    let cfg = paths::PathsConfig { data_dir: opt(&c.data), hot_dir: opt(&c.hot), require_mount: opt(&c.mount) };
    let file = paths::paths_file();
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建配置目录 {} 失败：{e}", dir.display()))?;
    }
    let tmp = file.with_extension("toml.tmp");
    std::fs::write(&tmp, paths::render_paths_toml(&cfg)).map_err(|e| format!("写 {} 失败：{e}", tmp.display()))?;
    std::fs::rename(&tmp, &file).map_err(|e| format!("改名到 {} 失败：{e}", file.display()))?;
    Ok(file)
}

pub fn handle(m: DataDirsMsg) {
    match m {
        DataDirsMsg::Recheck => refresh(),
        DataDirsMsg::Dismiss => with(|st| st.dismissed = signature(&st.rows)),
        DataDirsMsg::Open => {
            let cfg = paths::paths_config();
            with(|st| {
                st.card = Some(Card {
                    data: cfg.data_dir.unwrap_or_default(),
                    hot: cfg.hot_dir.unwrap_or_default(),
                    mount: cfg.require_mount.unwrap_or_default(),
                    preview: Vec::new(),
                    note: String::new(),
                });
            });
        }
        DataDirsMsg::Close => with(|st| st.card = None),
        DataDirsMsg::Edit(f, v) => with(|st| {
            if let Some(c) = st.card.as_mut() {
                match f {
                    Field::Data => c.data = v,
                    Field::Hot => c.hot = v,
                    Field::Mount => c.mount = v,
                }
                c.preview.clear();
                c.note.clear();
            }
        }),
        DataDirsMsg::Test => with(|st| {
            if let Some(c) = st.card.as_mut() {
                c.preview = test_card(c);
                c.note.clear();
            }
        }),
        DataDirsMsg::Save => with(|st| {
            if let Some(c) = st.card.as_mut() {
                c.preview = test_card(c);
                c.note = match save(c) {
                    Ok(f) => {
                        let bad = c.preview.iter().any(|(_, _, p)| p.is_some());
                        format!(
                            "已写入 {}。重启 Cockpit 后生效（守护跟着重启）{}",
                            f.display(),
                            if bad { "；上面还有没通过的检查，重启前先处理" } else { "" }
                        )
                    }
                    Err(e) => format!("✗ {e}"),
                };
            }
        }),
    }
}

/// 检查过了多久（卡片里写「N 秒前查过」）。
#[must_use]
pub fn checked_ago() -> Option<Duration> {
    with(|st| st.checked.map(|t| t.elapsed()))
}
