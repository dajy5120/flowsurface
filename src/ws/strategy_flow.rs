//! 策略画布（docs/44 §21）——状态与逻辑。
//!
//! 画布上每个节点是一个模块，端口有类型，只有同类型的端口能连。图不是直接执行的：
//! 它被 Python 端**编译**成一份工作流（`*.swe.toml`），由现有的宿主运行。
//!
//! **面板不判断图对不对、不算任何东西**——各有唯一来源：
//!
//! | 什么 | 从哪来 |
//! |---|---|
//! | 模块库（端口、参数表、说明） | `python -m strategies.swe.graph palette` |
//! | 现有工作流 → 图 | `python -m strategies.swe.graph import <toml>` |
//! | 检查 / 编译（错误带节点 id、轨迹定位表） | `python -m strategies.swe.graph check -` / `compile <图>` |
//! | 运行 | `python -m strategies.swe.run_dukascopy <toml> --live --pace …` |
//! | 实时高亮 | Redis 流 `ws:swe:<运行号>:trace`（宿主推出来的决策轨迹） |
//!
//! 视图每帧都会调用，所以**视图里不做任何 IO**——只读这里的快照。

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

// ── 数据形状（与 strategies/swe/graph.py 的 JSON 一致）───────────────────

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct ParamDef {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    pub required: bool,
    pub default: Value,
    pub description: String,
    pub optimize: bool,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct ModDef {
    #[serde(rename = "ref")]
    pub refer: String,
    pub id: String,
    pub kind: String,
    pub kind_label: String,
    pub title: String,
    pub book: String,
    pub summary: String,
    pub ins: Vec<String>,
    pub outs: Vec<String>,
    pub params: Vec<ParamDef>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Palette {
    pub modules: Vec<ModDef>,
    pub lane_params: Vec<ParamDef>,
}

impl Palette {
    pub fn get(&self, refer: &str) -> Option<&ModDef> {
        self.modules.iter().find(|m| m.refer == refer)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Node {
    pub id: String,
    #[serde(rename = "use")]
    pub module: String,
    pub params: Map<String, Value>,
    pub x: f32,
    pub y: f32,
    /// 市场状态节点的名字（过滤 / 路由靠它引用）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 入场节点带着这条线的属性：name / entry_tf / exit_tf / entry_on
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lane: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Edge {
    pub from: String,
    pub out: String,
    pub to: String,
    #[serde(rename = "in")]
    pub inp: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Graph {
    pub id: String,
    pub name: String,
    pub version: String,
    pub strategy: Value,
    pub backtest: Value,
    pub settings: Value,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

pub const EXEC: &str = "kernel.exec";

impl Graph {
    /// 空画布：只有一个「执行」节点。
    pub fn blank() -> Self {
        Graph {
            id: "draft.untitled".into(),
            name: "未命名策略".into(),
            version: "0.1".into(),
            nodes: vec![Node { id: "n1".into(), module: EXEC.into(), x: 760.0, y: 120.0, ..Default::default() }],
            ..Default::default()
        }
    }

    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    fn node_mut(&mut self, id: &str) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    fn fresh_id(&self) -> String {
        let max = self.nodes.iter().filter_map(|n| n.id.strip_prefix('n').and_then(|s| s.parse::<u32>().ok())).max().unwrap_or(0);
        format!("n{}", max + 1)
    }

    /// 接一根线。单输入端口上原有的线被顶掉（风控与「执行」的 intent 输入可以接多根）。
    pub fn connect(&mut self, e: Edge, multi_in: bool) {
        if e.from == e.to || self.edges.contains(&e) {
            return;
        }
        if !multi_in {
            self.edges.retain(|x| !(x.to == e.to && x.inp == e.inp));
        }
        self.edges.push(e);
    }

    pub fn remove_node(&mut self, id: &str) {
        self.nodes.retain(|n| n.id != id);
        self.edges.retain(|e| e.from != id && e.to != id);
    }
}

/// 一个节点的运行态：最近一次决策是什么、什么时候（墙上时间）。
#[derive(Debug, Clone)]
pub struct NodeLive {
    pub at: Instant,
    pub kind: String,
    pub reason: String,
    pub n: u32,
    /// 被否决 / 拒绝 / 缩量的次数
    pub n_blocked: u32,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(default)]
pub struct Issue {
    pub node: String,
    pub msg: String,
}

/// 能打开的东西：画布存的图，或者现有的工作流（导入成副本）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileItem {
    pub path: String,
    pub is_graph: bool,
}

impl std::fmt::Display for FileItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = self.path.rsplit('/').next().unwrap_or(&self.path);
        write!(f, "{}{}", if self.is_graph { "画布 · " } else { "导入 · " }, name)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum FlMsg {
    Refresh,
    New,
    Open(FileItem),
    Save,
    Check,
    Run,
    Stop,
    Start(String),
    End(String),
    Pace(String),
    MetaId(String),
    MetaName(String),
    AddNode(String),
    Select(Option<String>),
    MoveNode(String, f32, f32),
    Delete(String),
    Connect(Edge),
    /// 拔掉接在某个输入端口上的线
    Disconnect(String, String),
    Swap(String, String),
    Param(String, String, String),
    LaneField(String, String, String),
    RegimeName(String, String),
    Pan(f32, f32),
    Zoom(f32),
    ResetView,
}

// ── 状态 ────────────────────────────────────────────────────────────────

struct St {
    palette: Option<Palette>,
    palette_loading: bool,
    files: Vec<FileItem>,
    graph: Graph,
    /// 图文件（仓库相对路径）；还没存过是 None
    path: Option<String>,
    dirty: bool,
    selected: Option<String>,
    /// 参数输入框里的原文：键 = 节点 id + '\u{1}' + 参数名（允许暂时填着不合法的东西）
    form: BTreeMap<String, String>,
    issues: Vec<Issue>,
    /// 轨迹定位表：`线名|模块 id` → 节点 id
    whence: HashMap<String, String>,
    checked: Option<bool>,
    busy: bool,
    msg: String,
    pan: (f32, f32),
    zoom: f32,
    start: String,
    end: String,
    pace: String,
    run_id: String,
    child: Option<Child>,
    running: bool,
    live: HashMap<String, NodeLive>,
    live_n: usize,
    /// 换一次运行加一：读轨迹的线程发现自己过时了就退出
    generation: u64,
}

impl Default for St {
    fn default() -> Self {
        St {
            palette: None,
            palette_loading: false,
            files: Vec::new(),
            graph: Graph::blank(),
            path: None,
            dirty: false,
            selected: None,
            form: BTreeMap::new(),
            issues: Vec::new(),
            whence: HashMap::new(),
            checked: None,
            busy: false,
            msg: String::new(),
            pan: (24.0, 16.0),
            zoom: 1.0,
            start: "2026-06-01".into(),
            end: "2026-06-05".into(),
            pace: "3600".into(),
            run_id: String::new(),
            child: None,
            running: false,
            live: HashMap::new(),
            live_n: 0,
            generation: 0,
        }
    }
}

fn cell() -> &'static Mutex<St> {
    static C: OnceLock<Mutex<St>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(St::default()))
}

fn with<R>(f: impl FnOnce(&mut St) -> R) -> Option<R> {
    cell().lock().ok().map(|mut g| f(&mut g))
}

/// 视图每帧拿的快照。
#[derive(Clone)]
pub struct View {
    pub palette: Option<Palette>,
    pub palette_loading: bool,
    pub files: Vec<FileItem>,
    pub graph: Graph,
    pub path: Option<String>,
    pub dirty: bool,
    pub selected: Option<String>,
    pub form: BTreeMap<String, String>,
    pub issues: Vec<Issue>,
    pub checked: Option<bool>,
    pub busy: bool,
    pub msg: String,
    pub pan: (f32, f32),
    pub zoom: f32,
    pub start: String,
    pub end: String,
    pub pace: String,
    pub run_id: String,
    pub running: bool,
    pub live: HashMap<String, NodeLive>,
    pub live_n: usize,
}

pub fn view() -> View {
    ensure_started();
    with(|g| View {
        palette: g.palette.clone(),
        palette_loading: g.palette_loading,
        files: g.files.clone(),
        graph: g.graph.clone(),
        path: g.path.clone(),
        dirty: g.dirty,
        selected: g.selected.clone(),
        form: g.form.clone(),
        issues: g.issues.clone(),
        checked: g.checked,
        busy: g.busy,
        msg: g.msg.clone(),
        pan: g.pan,
        zoom: g.zoom,
        start: g.start.clone(),
        end: g.end.clone(),
        pace: g.pace.clone(),
        run_id: g.run_id.clone(),
        running: g.running,
        live: g.live.clone(),
        live_n: g.live_n,
    })
    .expect("策略画布状态锁中毒")
}

pub fn form_key(node: &str, param: &str) -> String {
    format!("{node}\u{1}{param}")
}

/// 参数值 → 输入框里显示的文字。
pub fn value_text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// 输入框里的文字 → 参数值。按参数类型解析；解析不了就当字符串留着（检查时由模块的 Config 报错）。
pub fn parse_value(text: &str, ty: &str) -> Option<Value> {
    let t = text.trim();
    if t.is_empty() {
        return None; // 留空 = 用缺省值
    }
    match ty {
        "integer" => t.parse::<i64>().ok().map(Value::from).or_else(|| Some(Value::String(t.to_string()))),
        "number" => t.parse::<f64>().ok().and_then(|x| serde_json::Number::from_f64(x).map(Value::Number)).or_else(|| Some(Value::String(t.to_string()))),
        "boolean" => match t {
            "true" | "是" | "1" => Some(Value::Bool(true)),
            "false" | "否" | "0" => Some(Value::Bool(false)),
            _ => Some(Value::String(t.to_string())),
        },
        "string" => Some(Value::String(t.to_string())),
        // 数组 / 表：按 JSON 写
        _ => serde_json::from_str(t).ok().or_else(|| Some(Value::String(t.to_string()))),
    }
}

/// 这个输入端口能不能接多根线（与 graph.py 的 MULTI_IN 一致）。
pub fn multi_in(kind: &str, port: &str) -> bool {
    port == "intent" && (kind == "risk" || kind == "exec")
}

fn python(args: &[&str], stdin: Option<&[u8]>) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new(super::paths::python());
    cmd.args(args).current_dir(super::paths::repo_root()).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
    let mut child = cmd.spawn().map_err(|e| format!("起不来 Python：{e}"))?;
    if let (Some(data), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let _ = pipe.write_all(data);
    }
    let out = child.wait_with_output().map_err(|e| format!("Python 没跑完：{e}"))?;
    // check / compile 在图有问题时返回 1，但标准输出仍是完整的 JSON
    if out.stdout.is_empty() {
        return Err(String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("Python 出错").to_string());
    }
    Ok(out.stdout)
}

fn ensure_started() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        load_palette();
        scan_files();
        // 样张截图用：启动时直接打开一个文件；`WS_FLOW_DEMO=1` 再给几个节点摆上运行中的样子
        if let Ok(path) = std::env::var("WS_FLOW_OPEN") {
            let is_graph = path.ends_with(".json");
            open(FileItem { path, is_graph });
            // 端到端自测用：打开之后自动「保存 → 编译 → 运行」（全速、两天），截图时应能看到节点上的计数
            if std::env::var_os("WS_FLOW_RUN").is_some() {
                super::spawn_named("ws-flowauto", || {
                    std::thread::sleep(std::time::Duration::from_secs(5));
                    with(|g| {
                        g.pace = "0".into();
                        g.end = "2026-06-02".into();
                    });
                    handle(FlMsg::Run);
                });
            }
            if std::env::var_os("WS_FLOW_DEMO").is_some() {
                super::spawn_named("ws-flowdemo", || {
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    with(|g| {
                        let kinds = ["intent", "veto", "risk_resize", "fill", "regime", "risk_reject", "move_stop"];
                        let ids: Vec<String> = g.graph.nodes.iter().map(|n| n.id.clone()).collect();
                        for (i, id) in ids.iter().enumerate().filter(|(i, _)| i % 3 == 0) {
                            let kind = kinds[(i / 3) % kinds.len()].to_string();
                            g.live.insert(id.clone(), NodeLive { at: Instant::now() + std::time::Duration::from_secs(3600), kind, reason: "演示".into(), n: 3 + i as u32, n_blocked: (i % 2) as u32 });
                        }
                        g.live_n = 42;
                        g.running = true;
                    });
                });
            }
        }
    });
}

fn load_palette() {
    if with(|g| std::mem::replace(&mut g.palette_loading, true)).unwrap_or(true) {
        return;
    }
    super::spawn_named("ws-flowpal", || {
        let r = python(&["-m", "strategies.swe.graph", "palette"], None)
            .and_then(|b| serde_json::from_slice::<Palette>(&b).map_err(|e| format!("模块库 JSON 解析失败：{e}")));
        with(|g| {
            g.palette_loading = false;
            match r {
                Ok(p) => g.palette = Some(p),
                Err(e) => g.msg = e,
            }
        });
    });
}

/// 找出能打开的文件：`strategies/research/` 下的图（`*.swe.graph.json`）与工作流（`*.swe.toml`）。
fn scan_files() {
    super::spawn_named("ws-flowscan", || {
        let root = super::paths::repo_root();
        let mut out = Vec::new();
        let mut stack = vec![root.join("strategies/research")];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                let name = e.file_name().to_string_lossy().to_string();
                if p.is_dir() {
                    if !name.starts_with('.') && name != "__pycache__" {
                        stack.push(p);
                    }
                } else if name.ends_with(".swe.graph.json") || name.ends_with(".swe.toml") {
                    if let Ok(rel) = p.strip_prefix(&root) {
                        out.push(FileItem { path: rel.to_string_lossy().to_string(), is_graph: name.ends_with(".json") });
                    }
                }
            }
        }
        // 已经有图的工作流不再列成「导入」（它就是那张图编译出来的）
        let graphs: Vec<String> = out.iter().filter(|f| f.is_graph).map(|f| f.path.replace(".swe.graph.json", ".swe.toml")).collect();
        out.retain(|f| f.is_graph || !graphs.contains(&f.path));
        out.sort_by(|a, b| (!a.is_graph, &a.path).cmp(&(!b.is_graph, &b.path)));
        with(|g| g.files = out);
    });
}

fn graph_path_for(id: &str) -> String {
    // id = 主题.名字 → strategies/research/<主题>/<名字>.swe.graph.json
    let (family, name) = id.split_once('.').unwrap_or(("drafts", id));
    let safe = |s: &str| s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' { c } else { '_' }).collect::<String>();
    format!("strategies/research/{}/{}.swe.graph.json", safe(family), safe(name))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CheckOut {
    ok: bool,
    errors: Vec<Issue>,
    #[serde(rename = "where")]
    whence: HashMap<String, String>,
    toml: String,
}

fn apply_check(g: &mut St, r: Result<CheckOut, String>, what: &str) -> Option<String> {
    g.busy = false;
    match r {
        Ok(c) => {
            g.checked = Some(c.ok);
            g.issues = c.errors;
            if c.ok {
                g.whence = c.whence;
                g.msg = format!("{what}通过");
                return Some(c.toml);
            }
            g.msg = format!("{what}：{} 个问题（标在节点上）", g.issues.len());
        }
        Err(e) => {
            g.checked = Some(false);
            g.msg = format!("{what}失败：{e}");
        }
    }
    None
}

fn check_async() {
    let Some(graph) = with(|g| {
        if g.busy {
            return None;
        }
        g.busy = true;
        Some(g.graph.clone())
    })
    .flatten() else {
        return;
    };
    super::spawn_named("ws-flowcheck", move || {
        let body = serde_json::to_vec(&graph).unwrap_or_default();
        let r = python(&["-m", "strategies.swe.graph", "check", "-"], Some(&body))
            .and_then(|b| serde_json::from_slice::<CheckOut>(&b).map_err(|e| format!("结果解析失败：{e}")));
        with(|g| apply_check(g, r, "检查"));
    });
}

/// 存图并编译出工作流。成功返回工作流的路径。
fn save_blocking() -> Result<String, String> {
    let (graph, path) = with(|g| {
        let path = g.path.clone().unwrap_or_else(|| graph_path_for(&g.graph.id));
        (g.graph.clone(), path)
    })
    .ok_or("状态锁中毒")?;
    let abs = super::paths::repo_root().join(&path);
    if let Some(dir) = abs.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建目录失败：{e}"))?;
    }
    let text = serde_json::to_string_pretty(&graph).map_err(|e| e.to_string())?;
    std::fs::write(&abs, text).map_err(|e| format!("写图文件失败：{e}"))?;
    let r = python(&["-m", "strategies.swe.graph", "compile", &path], None)
        .and_then(|b| serde_json::from_slice::<CheckOut>(&b).map_err(|e| format!("结果解析失败：{e}")));
    let toml = with(|g| {
        g.path = Some(path.clone());
        g.dirty = false;
        apply_check(g, r, "保存并编译")
    })
    .flatten();
    scan_files();
    match toml {
        Some(t) if !t.is_empty() => Ok(t),
        _ => Err("图还有问题，工作流没有生成（图本身已保存）".into()),
    }
}

fn run_async() {
    if with(|g| std::mem::replace(&mut g.busy, true)).unwrap_or(true) {
        return;
    }
    super::spawn_named("ws-flowrun", || {
        with(|g| g.busy = false);
        let toml = match save_blocking() {
            Ok(t) => t,
            Err(e) => {
                with(|g| g.msg = e);
                return;
            }
        };
        stop();
        let run_id = format!("flow-{}", chrono::Local::now().format("%H%M%S"));
        let (start, end, pace) = with(|g| (g.start.clone(), g.end.clone(), g.pace.clone())).unwrap_or_default();
        let log = std::env::temp_dir().join(format!("ws-{run_id}.log"));
        let out = std::fs::File::create(&log).ok();
        let mut cmd = Command::new(super::paths::python());
        cmd.args(["-m", "strategies.swe.run_dukascopy", &toml, "--live", "--run-id", &run_id, "--start", &start, "--end", &end]);
        if pace.trim().parse::<f64>().map(|p| p > 0.0).unwrap_or(false) {
            cmd.args(["--pace", pace.trim()]);
        }
        cmd.current_dir(super::paths::repo_root()).stdin(Stdio::null());
        match out {
            Some(f) => {
                cmd.stdout(f.try_clone().map(Stdio::from).unwrap_or_else(|_| Stdio::null())).stderr(Stdio::from(f));
            }
            None => {
                cmd.stdout(Stdio::null()).stderr(Stdio::null());
            }
        }
        match cmd.spawn() {
            Ok(child) => {
                let generation = with(|g| {
                    g.child = Some(child);
                    g.running = true;
                    g.run_id = run_id.clone();
                    g.live.clear();
                    g.live_n = 0;
                    g.generation += 1;
                    g.msg = format!("运行中：{run_id}（{start} → {end}）");
                    g.generation
                })
                .unwrap_or(0);
                follow_trace(run_id.clone(), generation);
                wait_child(run_id, generation, log);
            }
            Err(e) => {
                with(|g| g.msg = format!("起不来回测：{e}"));
            }
        }
    });
}

/// 等回测进程结束，把结局写进状态条。
fn wait_child(run_id: String, generation: u64, log: PathBuf) {
    super::spawn_named("ws-flowwait", move || {
        loop {
            std::thread::sleep(std::time::Duration::from_millis(400));
            let done = with(|g| {
                if g.generation != generation {
                    return Some(None); // 已经换了一次运行
                }
                match g.child.as_mut().map(Child::try_wait) {
                    Some(Ok(Some(status))) => Some(Some(status.success())),
                    Some(Ok(None)) => None,
                    _ => Some(None),
                }
            })
            .flatten();
            let Some(result) = done else { continue };
            if let Some(ok) = result {
                let tail = std::fs::read_to_string(&log).ok().and_then(|s| s.lines().rev().find(|l| !l.trim().is_empty()).map(str::to_string)).unwrap_or_default();
                with(|g| {
                    g.running = false;
                    g.child = None;
                    g.msg = if ok { format!("跑完了：{run_id}（{} 个决策）", g.live_n) } else { format!("运行失败：{tail}") };
                });
            }
            return;
        }
    });
}

fn redis_url() -> String {
    std::env::var("WS_REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string())
}

/// 一条决策轨迹落在哪个节点上：先找「线|模块」，再找工作流级的「|模块」，最后退到这条线的入场节点。
pub fn locate(whence: &HashMap<String, String>, lane: &str, module: &str) -> Option<String> {
    whence
        .get(&format!("{lane}|{module}"))
        .or_else(|| if module.is_empty() { None } else { whence.get(&format!("|{module}")) })
        .or_else(|| whence.get(&format!("{lane}|")))
        .cloned()
}

fn follow_trace(run_id: String, generation: u64) {
    super::spawn_named("ws-flowlive", move || {
        use redis::Commands;
        use redis::streams::{StreamReadOptions, StreamReadReply};
        let Ok(client) = redis::Client::open(redis_url()) else { return };
        let Ok(mut conn) = client.get_connection() else {
            with(|g| g.msg = "连不上 Redis，看不到实时轨迹".to_string());
            return;
        };
        let key = format!("ws:swe:{run_id}:trace");
        let mut last = "0".to_string();
        let opts = StreamReadOptions::default().block(300).count(400);
        loop {
            let (alive, running) = with(|g| (g.generation == generation, g.running)).unwrap_or((false, false));
            if !alive {
                return;
            }
            let reply: StreamReadReply = match conn.xread_options(&[&key], &[&last], &opts) {
                Ok(r) => r,
                Err(_) => return,
            };
            let mut got = Vec::new();
            for k in reply.keys {
                for id in k.ids {
                    last = id.id.clone();
                    if let Some(redis::Value::BulkString(b)) = id.map.get("j")
                        && let Ok(v) = serde_json::from_slice::<Value>(b)
                    {
                        got.push(v);
                    }
                }
            }
            if got.is_empty() {
                if !running {
                    return; // 进程已结束，流也读空了
                }
                continue;
            }
            with(|g| {
                if g.generation != generation {
                    return;
                }
                for v in got {
                    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                    let (lane, module, kind, reason) = (s("lane"), s("module"), s("kind"), s("reason"));
                    g.live_n += 1;
                    let Some(node) = locate(&g.whence, &lane, &module) else { continue };
                    let blocked = matches!(kind.as_str(), "veto" | "risk_reject" | "risk_resize" | "risk_halt");
                    let e = g.live.entry(node).or_insert(NodeLive { at: Instant::now(), kind: String::new(), reason: String::new(), n: 0, n_blocked: 0 });
                    e.at = Instant::now();
                    e.kind = kind;
                    e.reason = reason;
                    e.n += 1;
                    e.n_blocked += blocked as u32;
                }
            });
        }
    });
}

fn stop() {
    with(|g| {
        if let Some(mut c) = g.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        if g.running {
            g.running = false;
            g.msg = "已停止".to_string();
        }
    });
}

fn open(item: FileItem) {
    if with(|g| std::mem::replace(&mut g.busy, true)).unwrap_or(true) {
        return;
    }
    super::spawn_named("ws-flowopen", move || {
        let r: Result<Graph, String> = if item.is_graph {
            std::fs::read(super::paths::repo_root().join(&item.path))
                .map_err(|e| format!("读不了 {}：{e}", item.path))
                .and_then(|b| serde_json::from_slice(&b).map_err(|e| format!("图文件解析失败：{e}")))
        } else {
            python(&["-m", "strategies.swe.graph", "import", &item.path], None)
                .and_then(|b| serde_json::from_slice(&b).map_err(|e| format!("导入失败：{e}")))
        };
        with(|g| {
            g.busy = false;
            match r {
                Ok(mut graph) => {
                    if item.is_graph {
                        g.path = Some(item.path.clone());
                        g.dirty = false;
                        g.msg = format!("已打开 {}", item.path);
                    } else {
                        // 导入的是手写的工作流：做成副本，免得保存时顶掉原文件、id 也撞车
                        graph.id = format!("{}.copy", graph.id);
                        graph.name = format!("{}（副本）", graph.name);
                        g.path = None;
                        g.dirty = true;
                        g.msg = format!("已导入 {} 的副本（改个名字再保存）", item.path);
                    }
                    g.graph = graph;
                    fit(g);
                    g.selected = None;
                    g.form.clear();
                    g.issues.clear();
                    g.whence.clear();
                    g.checked = None;
                    g.live.clear();
                    g.live_n = 0;
                }
                Err(e) => g.msg = e,
            }
        });
    });
}

/// 把视野调到能看见全部节点（画布的实际大小这里不知道，按常见的面板大小估）。
fn fit(g: &mut St) {
    const VIEW: (f32, f32) = (1280.0, 800.0);
    const NODE: (f32, f32) = (168.0, 120.0);
    if g.graph.nodes.is_empty() {
        return;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for n in &g.graph.nodes {
        x0 = x0.min(n.x);
        y0 = y0.min(n.y);
        x1 = x1.max(n.x + NODE.0);
        y1 = y1.max(n.y + NODE.1);
    }
    let zoom = (VIEW.0 / (x1 - x0).max(1.0)).min(VIEW.1 / (y1 - y0).max(1.0)).clamp(0.35, 1.0);
    g.zoom = zoom;
    g.pan = (20.0 - x0 * zoom, 16.0 - y0 * zoom);
}

fn touch(g: &mut St) {
    g.dirty = true;
    g.checked = None;
}

pub fn handle(m: FlMsg) {
    match m {
        FlMsg::Refresh => {
            load_palette();
            scan_files();
        }
        FlMsg::New => {
            stop();
            with(|g| {
                g.graph = Graph::blank();
                g.path = None;
                g.dirty = false;
                g.selected = None;
                g.form.clear();
                g.issues.clear();
                g.whence.clear();
                g.checked = None;
                g.live.clear();
                g.live_n = 0;
                g.msg = "新画布：从左边的模块库点模块加节点，拖端口连线".to_string();
            });
        }
        FlMsg::Open(item) => open(item),
        FlMsg::Check => check_async(),
        FlMsg::Save => {
            if with(|g| std::mem::replace(&mut g.busy, true)).unwrap_or(true) {
                return;
            }
            super::spawn_named("ws-flowsave", || {
                with(|g| g.busy = false);
                if let Err(e) = save_blocking() {
                    with(|g| g.msg = e);
                }
            });
        }
        FlMsg::Run => run_async(),
        FlMsg::Stop => stop(),
        FlMsg::Start(v) => {
            with(|g| g.start = v);
        }
        FlMsg::End(v) => {
            with(|g| g.end = v);
        }
        FlMsg::Pace(v) => {
            with(|g| g.pace = v);
        }
        FlMsg::MetaId(v) => {
            with(|g| {
                g.graph.id = v;
                g.path = None; // id 变了 = 另存为
                touch(g);
            });
        }
        FlMsg::MetaName(v) => {
            with(|g| {
                g.graph.name = v;
                touch(g);
            });
        }
        FlMsg::AddNode(refer) => {
            with(|g| {
                let Some(def) = g.palette.as_ref().and_then(|p| p.get(&refer)).cloned() else { return };
                if def.kind == "exec" && g.graph.nodes.iter().any(|n| n.module == EXEC) {
                    g.msg = "「执行」节点已经有一个了".to_string();
                    return;
                }
                let id = g.graph.fresh_id();
                // 放在当前视野的左上附近，逐个错开
                let k = g.graph.nodes.len() as f32;
                let (x, y) = ((40.0 - g.pan.0) / g.zoom + (k % 5.0) * 24.0, (40.0 - g.pan.1) / g.zoom + (k % 5.0) * 24.0);
                let mut node = Node { id: id.clone(), module: refer, x, y, ..Default::default() };
                if def.kind == "entry" {
                    let n_lanes = g.graph.nodes.iter().filter(|n| n.lane.is_some()).count();
                    let mut lane = Map::new();
                    lane.insert("name".into(), Value::String(format!("L{}", n_lanes + 1)));
                    lane.insert("entry_tf".into(), Value::from(15));
                    lane.insert("exit_tf".into(), Value::from(1));
                    lane.insert("entry_on".into(), Value::String(if def.book == "basket" { "tick" } else { "bar" }.into()));
                    node.lane = Some(lane);
                }
                if def.kind == "regime" {
                    node.name = Some(def.title.clone());
                }
                g.graph.nodes.push(node);
                g.selected = Some(id);
                touch(g);
            });
        }
        FlMsg::Select(id) => {
            with(|g| g.selected = id);
        }
        FlMsg::MoveNode(id, x, y) => {
            with(|g| {
                if let Some(n) = g.graph.node_mut(&id) {
                    n.x = x;
                    n.y = y;
                    g.dirty = true;
                }
            });
        }
        FlMsg::Delete(id) => {
            with(|g| {
                g.graph.remove_node(&id);
                g.selected = None;
                g.live.remove(&id);
                touch(g);
            });
        }
        FlMsg::Connect(e) => {
            with(|g| {
                let kind = g.graph.node(&e.to).and_then(|n| g.palette.as_ref().and_then(|p| p.get(&n.module))).map(|d| d.kind.clone()).unwrap_or_default();
                let multi = multi_in(&kind, &e.inp);
                g.graph.connect(e, multi);
                touch(g);
            });
        }
        FlMsg::Disconnect(to, inp) => {
            with(|g| {
                g.graph.edges.retain(|e| !(e.to == to && e.inp == inp));
                touch(g);
            });
        }
        FlMsg::Swap(id, refer) => {
            with(|g| {
                // 换成同环节的另一个模块：名字相同的参数留着，其余丢掉
                let keep: Vec<String> = g.palette.as_ref().and_then(|p| p.get(&refer)).map(|d| d.params.iter().map(|p| p.name.clone()).collect()).unwrap_or_default();
                if let Some(n) = g.graph.node_mut(&id) {
                    n.module = refer;
                    n.params.retain(|k, _| keep.contains(k));
                }
                g.form.retain(|k, _| !k.starts_with(&format!("{id}\u{1}")));
                touch(g);
            });
        }
        FlMsg::Param(id, name, text) => {
            with(|g| {
                let ty = g
                    .graph
                    .node(&id)
                    .and_then(|n| g.palette.as_ref().and_then(|p| p.get(&n.module)))
                    .and_then(|d| d.params.iter().find(|p| p.name == name))
                    .map(|p| p.ty.clone())
                    .unwrap_or_default();
                let parsed = parse_value(&text, &ty);
                g.form.insert(form_key(&id, &name), text);
                if let Some(n) = g.graph.node_mut(&id) {
                    match parsed {
                        Some(v) => {
                            n.params.insert(name, v);
                        }
                        None => {
                            n.params.remove(&name);
                        }
                    }
                }
                touch(g);
            });
        }
        FlMsg::LaneField(id, name, text) => {
            with(|g| {
                let ty = if name == "entry_tf" || name == "exit_tf" { "integer" } else { "string" };
                let parsed = parse_value(&text, ty);
                g.form.insert(form_key(&id, &format!("lane.{name}")), text);
                if let Some(lane) = g.graph.node_mut(&id).and_then(|n| n.lane.as_mut()) {
                    match parsed {
                        Some(v) => {
                            lane.insert(name, v);
                        }
                        None => {
                            lane.remove(&name);
                        }
                    }
                }
                touch(g);
            });
        }
        FlMsg::RegimeName(id, text) => {
            with(|g| {
                if let Some(n) = g.graph.node_mut(&id) {
                    n.name = if text.trim().is_empty() { None } else { Some(text) };
                }
                touch(g);
            });
        }
        FlMsg::Pan(dx, dy) => {
            with(|g| g.pan = (g.pan.0 + dx, g.pan.1 + dy));
        }
        FlMsg::Zoom(f) => {
            with(|g| g.zoom = (g.zoom * f).clamp(0.35, 2.0));
        }
        FlMsg::ResetView => {
            // 第一下：看全；已经是看全的样子再点：回到 100%
            with(|g| {
                let before = (g.zoom, g.pan);
                fit(g);
                if before == (g.zoom, g.pan) {
                    g.zoom = 1.0;
                    g.pan = (24.0, 16.0);
                }
            });
        }
    }
}

/// 有回测在跑（给页签上的状态点用）。
pub fn busy() -> bool {
    with(|g| g.running).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_json_roundtrips_with_python_field_names() {
        let raw = r#"{"id":"a.b","name":"x","version":"0.1","strategy":{},"backtest":{},"settings":{"pip":0.01},
            "nodes":[{"id":"n1","use":"regime.ema_trend@1.0","params":{"fast":21},"x":0.0,"y":10.5,"name":"trend"},
                     {"id":"n2","use":"entry.stop_ladder@1.0","params":{},"x":1.0,"y":2.0,"lane":{"name":"S1","entry_tf":15}}],
            "edges":[{"from":"n1","out":"regime","to":"n2","in":"regime"}]}"#;
        let g: Graph = serde_json::from_str(raw).unwrap();
        assert_eq!(g.nodes[0].module, "regime.ema_trend@1.0");
        assert_eq!(g.nodes[1].lane.as_ref().unwrap()["name"], "S1");
        assert_eq!(g.edges[0].inp, "regime");
        let back: Value = serde_json::to_value(&g).unwrap();
        assert_eq!(back["nodes"][0]["use"], "regime.ema_trend@1.0"); // 写回去仍是 Python 认的键名
        assert_eq!(back["edges"][0]["in"], "regime");
        assert!(back["nodes"][0].get("lane").is_none()); // 没有的可选字段不写出去
    }

    #[test]
    fn connect_replaces_single_inputs_but_stacks_on_multi_inputs() {
        let mut g = Graph::blank();
        let e = |from: &str, to: &str, port: &str| Edge { from: from.into(), out: port.into(), to: to.into(), inp: port.into() };
        g.connect(e("a", "x", "signal"), false);
        g.connect(e("b", "x", "signal"), false); // 单输入：顶掉旧的
        assert_eq!(g.edges.len(), 1);
        assert_eq!(g.edges[0].from, "b");
        g.connect(e("a", "r", "intent"), true);
        g.connect(e("b", "r", "intent"), true); // 风控 / 执行的 intent：并上
        g.connect(e("b", "r", "intent"), true); // 重复的线不加
        assert_eq!(g.edges.len(), 3);
        g.connect(e("x", "x", "signal"), false); // 自己连自己：不理
        assert_eq!(g.edges.len(), 3);
        g.remove_node("b");
        assert_eq!(g.edges.len(), 1);
        assert!(multi_in("risk", "intent") && multi_in("exec", "intent") && !multi_in("entry", "signal"));
    }

    #[test]
    fn param_text_is_parsed_by_declared_type() {
        assert_eq!(parse_value(" 15 ", "integer"), Some(Value::from(15)));
        assert_eq!(parse_value("2.5", "number").unwrap().as_f64(), Some(2.5));
        assert_eq!(parse_value("true", "boolean"), Some(Value::Bool(true)));
        assert_eq!(parse_value("[1, 15]", "array"), Some(serde_json::json!([1, 15])));
        assert_eq!(parse_value("", "number"), None); // 留空 = 用缺省值
        assert_eq!(parse_value("abc", "integer"), Some(Value::String("abc".into()))); // 填错了也留着，检查时报
        assert_eq!(value_text(&Value::String("both".into())), "both");
        assert_eq!(value_text(&serde_json::json!(0.35)), "0.35");
    }

    #[test]
    fn trace_events_find_their_node_with_fallbacks() {
        let w: HashMap<String, String> = [("S1|exit.breakeven", "n7"), ("|risk.exposure_cap", "n3"), ("S1|", "n5"), ("|regime.ema_trend", "n1")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        assert_eq!(locate(&w, "S1", "exit.breakeven").as_deref(), Some("n7"));
        assert_eq!(locate(&w, "S1", "risk.exposure_cap").as_deref(), Some("n3")); // 风控是工作流级的
        assert_eq!(locate(&w, "", "regime.ema_trend").as_deref(), Some("n1"));
        assert_eq!(locate(&w, "S1", "").as_deref(), Some("n5")); // 成交 / 平仓 → 这条线的入场节点
        assert_eq!(locate(&w, "S9", "exit.breakeven"), None);
        assert_eq!(graph_path_for("ea.my_breakout"), "strategies/research/ea/my_breakout.swe.graph.json");
    }
}
