//! 策略画布（docs/44 §21，docs/45）——状态与逻辑。
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
//! | 打开图文件（旧格式由 Python 升级，迁移只有一份） | `python -m strategies.swe.graph upgrade <图>` |
//! | 检查 / 编译（错误带节点 id、轨迹定位表） | `python -m strategies.swe.graph check -` / `compile <图>` |
//! | 运行（按工作流的数据源挑回测脚本：黄金报价 / 已购逐笔数据） | `python -m strategies.swe.run <toml> --run-id …` |
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
    /// 只能取这几个值（下拉）
    #[serde(rename = "enum")]
    pub choices: Option<Vec<Value>>,
}

impl ParamDef {
    /// 用哪种控件（docs/45 §4.3）：开关 / 下拉 / 列表 / 表 / 文本。
    pub fn control(&self) -> Control {
        if self.choices.as_ref().is_some_and(|c| !c.is_empty()) {
            return Control::Choice;
        }
        match self.ty.as_str() {
            "boolean" => Control::Switch,
            "array" => Control::List,
            "object" => Control::Table,
            _ => Control::Text,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Text,
    Switch,
    Choice,
    /// 一行里用逗号隔开
    List,
    /// 两层的表（按来源覆盖、路由权重）：外层键 · 内层键 = 值
    Table,
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
    /// 强制风控层 `[envelope]` 的各项（docs/45 §3.6）：账户底线，不在图上
    pub envelope_params: Vec<ParamDef>,
    /// 运行配置 `[backtest]` 里画布上能改的项
    pub backtest_params: Vec<ParamDef>,
    /// `[workflow]` 里画布上能改的运行设置（采样栅格）
    pub settings_params: Vec<ParamDef>,
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
    /// 图文件的格式版本（docs/45 §3.7）；迁移在 Python 端（`graph.upgrade`）
    pub format: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    pub strategy: Value,
    pub backtest: Value,
    /// 强制风控层：没写的项用缺省的底线
    pub envelope: Value,
    pub settings: Value,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

pub const EXEC: &str = "kernel.exec";
/// 这份代码写出的图格式版本，与 `strategies/swe/graph.py` 的 FORMAT 一致（Python 端拒绝比它新的）。
pub const GRAPH_FORMAT: u32 = 2;
/// 撤销栈的深度。
const UNDO_DEPTH: usize = 100;
/// 停手多久之后自动检查。
const AUTO_CHECK_MS: u64 = 500;

impl Graph {
    /// 空画布：只有一个「执行」节点。
    pub fn blank() -> Self {
        Graph {
            format: GRAPH_FORMAT,
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

    /// 取出一组节点和它们**之间**的线（复制用）。「执行」节点不复制——每张图只能有一个。
    pub fn excerpt(&self, ids: &[String]) -> Clip {
        let nodes: Vec<Node> = self.nodes.iter().filter(|n| ids.contains(&n.id) && n.module != EXEC).cloned().collect();
        let has = |id: &str| nodes.iter().any(|n| n.id == id);
        let edges = self.edges.iter().filter(|e| has(&e.from) && has(&e.to)).cloned().collect();
        Clip { nodes, edges }
    }

    /// 贴进一组节点：换新 id、线名与状态名改成不重复的、整体挪到 `at`（左上角对齐）。返回新节点的 id。
    pub fn paste(&mut self, clip: &Clip, at: (f32, f32)) -> Vec<String> {
        let (x0, y0) = clip.nodes.iter().fold((f32::MAX, f32::MAX), |a, n| (a.0.min(n.x), a.1.min(n.y)));
        let mut map: HashMap<String, String> = HashMap::new();
        for n in &clip.nodes {
            let id = self.fresh_id();
            let mut n2 = n.clone();
            n2.id = id.clone();
            n2.x = n.x - x0 + at.0;
            n2.y = n.y - y0 + at.1;
            if let Some(name) = n2.lane.as_ref().and_then(|l| l.get("name")).and_then(Value::as_str).map(str::to_string) {
                let taken: Vec<String> = self.nodes.iter().filter_map(|x| x.lane.as_ref().and_then(|l| l.get("name")).and_then(Value::as_str).map(str::to_string)).collect();
                if let Some(l) = n2.lane.as_mut() {
                    l.insert("name".into(), Value::String(unique_name(&name, &taken)));
                }
            }
            if let Some(name) = n2.name.clone() {
                let taken: Vec<String> = self.nodes.iter().filter_map(|x| x.name.clone()).collect();
                n2.name = Some(unique_name(&name, &taken));
            }
            self.nodes.push(n2);
            map.insert(n.id.clone(), id);
        }
        for e in &clip.edges {
            if let (Some(a), Some(b)) = (map.get(&e.from), map.get(&e.to)) {
                self.edges.push(Edge { from: a.clone(), out: e.out.clone(), to: b.clone(), inp: e.inp.clone() });
            }
        }
        clip.nodes.iter().filter_map(|n| map.get(&n.id).cloned()).collect()
    }
}

/// 剪贴板里的一段图（整个进程共用一份，所以能跨图粘贴）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Clip {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

/// `S1` 被占了就给 `S1_2`、`S1_3`……
fn unique_name(base: &str, taken: &[String]) -> String {
    if !taken.iter().any(|t| t == base) {
        return base.to_string();
    }
    let stem = base.rsplit_once('_').filter(|(_, n)| n.parse::<u32>().is_ok()).map_or(base, |(s, _)| s);
    (2..).map(|i| format!("{stem}_{i}")).find(|c| !taken.contains(c)).unwrap_or_else(|| base.to_string())
}

/// 拖线拖到空白处弹出的「接什么」搜索框（docs/45 §2：只列能接的节点）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Quick {
    pub from: String,
    pub port: String,
    /// 新节点放在哪（世界坐标）
    pub x: f32,
    pub y: f32,
    pub query: String,
}

impl Quick {
    /// 能接在这根线后面的模块：有一个同类型的输入端口，且名字 / 环节里含搜索词。
    pub fn candidates<'a>(&self, pal: &'a Palette) -> Vec<&'a ModDef> {
        let q = self.query.trim().to_lowercase();
        pal.modules
            .iter()
            .filter(|m| m.ins.iter().any(|p| port_type(p) == port_type(&self.port)))
            .filter(|m| q.is_empty() || m.title.to_lowercase().contains(&q) || m.kind_label.contains(&q) || m.summary.to_lowercase().contains(&q))
            .collect()
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
    /// 运行结果目录里的留底图（绝对路径）
    pub snapshot: bool,
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
    /// 只选这一个（None = 全不选）
    Select(Option<String>),
    /// Ctrl / Shift 点：加进选择或从选择里去掉
    SelectToggle(String),
    /// 框选；true = 加到现有选择上
    SelectBox(Vec<String>, bool),
    SelectAll,
    /// 把选中的节点一起挪 (dx, dy)
    MoveSelected(f32, f32),
    Delete(String),
    DeleteSelected,
    Copy,
    Cut,
    /// 贴到这个世界坐标（None = 视野左上）
    Paste(Option<(f32, f32)>),
    Duplicate,
    Undo,
    Redo,
    /// 拖线拖到空白处：弹出「接什么」
    QuickOpen(Quick),
    QuickQuery(String),
    QuickPick(String),
    QuickClose,
    LibQuery(String),
    Connect(Edge),
    /// 拔掉接在某个输入端口上的线
    Disconnect(String, String),
    Swap(String, String),
    Param(String, String, String),
    /// 开关 / 下拉直接给值；None = 回到缺省
    ParamValue(String, String, Option<Value>),
    /// 表格参数：(节点, 参数, 行, 列 0..3, 文字)
    TableEdit(String, String, usize, usize, String),
    TableAdd(String, String),
    TableDel(String, String, usize),
    /// 表单（docs/45 §3.5）：(段 backtest / envelope / settings, 项, 文字)。不是节点——运行配置与账户底线
    Config(String, String, String),
    /// 从一次运行的结果目录打开当时的图
    OpenSnapshot(String),
    /// 「模块库」页签里的操作，转给策略中心的模块注册表
    Center(super::strategy_center::ScMsg),
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
    /// 选中的节点（最后一个是「主」选中：检查器显示它）
    selected: Vec<String>,
    undo: Vec<Graph>,
    redo: Vec<Graph>,
    /// 上一次改动的标签与时刻：同一个标签连着来（打字、拖动）只记一步撤销
    undo_tag: Option<(String, Instant)>,
    quick: Option<Quick>,
    lib_query: String,
    /// 表格参数正在编辑的行：键同 `form`。每行 [外层键, 内层键, 值]
    tables: BTreeMap<String, Vec<[String; 3]>>,
    /// 最近一次改动的时刻（自动检查用）；检查过了清掉
    touched: Option<Instant>,
    auto_on: bool,
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
    /// 这次运行里强制风控层最近一次出手：(拒绝 / 缩量 / 停手, 原因, 共几次)
    envelope_hit: Option<(String, String, u32)>,
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
            selected: Vec::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            undo_tag: None,
            quick: None,
            lib_query: String::new(),
            tables: BTreeMap::new(),
            touched: None,
            auto_on: false,
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
            envelope_hit: None,
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
    pub selected: Vec<String>,
    pub can_undo: bool,
    pub can_redo: bool,
    pub can_paste: bool,
    pub quick: Option<Quick>,
    pub lib_query: String,
    pub tables: BTreeMap<String, Vec<[String; 3]>>,
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
    pub envelope_hit: Option<(String, String, u32)>,
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
        can_undo: !g.undo.is_empty(),
        can_redo: !g.redo.is_empty(),
        can_paste: clip().lock().map(|c| !c.nodes.is_empty()).unwrap_or(false),
        quick: g.quick.clone(),
        lib_query: g.lib_query.clone(),
        tables: g.tables.clone(),
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
        envelope_hit: g.envelope_hit.clone(),
    })
    .expect("策略画布状态锁中毒")
}

impl View {
    /// 检查器显示的那个节点：只选了一个时才有。
    pub fn primary(&self) -> Option<&Node> {
        match self.selected.as_slice() {
            [one] => self.graph.node(one),
            _ => None,
        }
    }
}

fn clip() -> &'static Mutex<Clip> {
    static C: OnceLock<Mutex<Clip>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(Clip::default()))
}

/// 两层的表 → 编辑用的行（[外层键, 内层键, 值]）。一层的表内层键留空。
pub fn table_rows(v: Option<&Value>) -> Vec<[String; 3]> {
    let mut rows = Vec::new();
    if let Some(Value::Object(outer)) = v {
        for (k, inner) in outer {
            match inner {
                Value::Object(m) => rows.extend(m.iter().map(|(k2, x)| [k.clone(), k2.clone(), value_text(x)])),
                x => rows.push([k.clone(), String::new(), value_text(x)]),
            }
        }
    }
    rows
}

/// 编辑用的行 → 表。键没填的行不算；值按「数 / 真假 / 文字」认。全空 = None（用缺省值）。
pub fn rows_table(rows: &[[String; 3]]) -> Option<Value> {
    let scalar = |s: &str| -> Value {
        let s = s.trim();
        if let Ok(i) = s.parse::<i64>() {
            return Value::from(i);
        }
        if let Some(n) = s.parse::<f64>().ok().and_then(serde_json::Number::from_f64) {
            return Value::Number(n);
        }
        match s {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => Value::String(s.to_string()),
        }
    };
    let mut out = Map::new();
    for [a, b, v] in rows {
        let (a, b) = (a.trim(), b.trim());
        if a.is_empty() || v.trim().is_empty() {
            continue;
        }
        if b.is_empty() {
            out.insert(a.to_string(), scalar(v));
        } else if let Value::Object(m) = out.entry(a.to_string()).or_insert_with(|| Value::Object(Map::new())) {
            m.insert(b.to_string(), scalar(v));
        }
    }
    (!out.is_empty()).then_some(Value::Object(out))
}

pub fn form_key(node: &str, param: &str) -> String {
    format!("{node}\u{1}{param}")
}

/// 参数值 → 输入框里显示的文字。
pub fn value_text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        // 列表：一行里用逗号隔开（不是 JSON）
        Value::Array(a) if a.iter().all(|x| !x.is_array() && !x.is_object()) => a.iter().map(value_text).collect::<Vec<_>>().join(", "),
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
        // 列表：逗号隔开，每一项是数就按数、否则按文字（也认方括号写法）
        "array" => {
            if t.starts_with('[')
                && let Ok(v) = serde_json::from_str::<Value>(t)
            {
                return Some(v);
            }
            let item = |s: &str| -> Value {
                let s = s.trim().trim_matches('"');
                s.parse::<i64>().map(Value::from).ok().or_else(|| s.parse::<f64>().ok().and_then(serde_json::Number::from_f64).map(Value::Number)).unwrap_or_else(|| Value::String(s.to_string()))
            };
            Some(Value::Array(t.split([',', '，']).filter(|s| !s.trim().is_empty()).map(item).collect()))
        }
        _ => serde_json::from_str(t).ok().or_else(|| Some(Value::String(t.to_string()))),
    }
}

/// 端口名 → 类型（与 graph.py 的 `ptype` 一致）。大多数端口名字就是类型；
/// 特征输入端口以参数名为名（`feature`、`n_feature`），类型都是 feature。连线只看类型。
pub fn port_type(port: &str) -> &str {
    if port == "feature" || port.ends_with("_feature") { "feature" } else { port }
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
            open(FileItem { path, is_graph, snapshot: false });
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
            // 样张截图用：`WS_FLOW_SELECT=<模块 id 的一段>` 选中第一个匹配的节点（看检查器里的参数控件）；
            // `WS_FLOW_QUICK=1` 再从它的第一个输出端口弹出「接什么」
            if let Ok(pat) = std::env::var("WS_FLOW_SELECT") {
                super::spawn_named("ws-flowsel", move || {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    with(|g| {
                        let Some(n) = g.graph.nodes.iter().find(|n| n.module.contains(&pat)).cloned() else { return };
                        g.selected = vec![n.id.clone()];
                        if std::env::var_os("WS_FLOW_QUICK").is_some()
                            && let Some(port) = g.palette.as_ref().and_then(|p| p.get(&n.module)).and_then(|d| d.outs.first().cloned())
                        {
                            g.quick = Some(Quick { from: n.id.clone(), port, x: n.x + 260.0, y: n.y + 60.0, query: String::new() });
                        }
                    });
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
                        out.push(FileItem { path: rel.to_string_lossy().to_string(), is_graph: name.ends_with(".json"), snapshot: false });
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
        cmd.args(["-m", "strategies.swe.run", &toml, "--run-id", &run_id, "--start", &start, "--end", &end]);
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
                    g.envelope_hit = None;
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
                    // 强制风控层不是节点：它出手时记在横幅上
                    if let Some(what) = kind.strip_prefix("envelope_") {
                        if what != "resume" {
                            let n = g.envelope_hit.as_ref().map_or(0, |h| h.2) + 1;
                            g.envelope_hit = Some((what.to_string(), reason.clone(), n));
                        }
                        continue;
                    }
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
            // 旧格式的图由 Python 升到当前版（迁移只有一份）；比代码新的版本它会拒绝并说明
            let abs = super::paths::repo_root().join(&item.path);
            python(&["-m", "strategies.swe.graph", "upgrade", &abs.to_string_lossy()], None)
                .and_then(|b| serde_json::from_slice(&b).map_err(|e| format!("图文件解析失败：{e}")))
        } else {
            python(&["-m", "strategies.swe.graph", "import", &item.path], None)
                .and_then(|b| serde_json::from_slice(&b).map_err(|e| format!("导入失败：{e}")))
        };
        with(|g| {
            g.busy = false;
            match r {
                Ok(mut graph) => {
                    if item.snapshot {
                        // 运行记录里存的图：是那次运行的留底，打开成副本，不指向任何文件
                        graph.id = format!("{}.rerun", graph.id);
                        g.path = None;
                        g.dirty = true;
                        g.msg = "已打开那次运行用的图（副本；改个 id 再保存）".to_string();
                    } else if item.is_graph {
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
                    reset_editing(g);
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
    g.touched = Some(Instant::now());
    if !std::mem::replace(&mut g.auto_on, true) {
        auto_check();
    }
}

/// 边改边检查（docs/45 §4.4）：停手 0.5 秒自动检查一次。按需起线程，检查完就退出。
fn auto_check() {
    super::spawn_named("ws-flowidle", || {
        loop {
            std::thread::sleep(std::time::Duration::from_millis(120));
            let go = with(|g| match g.touched {
                None => Some(false),
                Some(at) if at.elapsed().as_millis() as u64 >= AUTO_CHECK_MS && !g.busy && g.palette.is_some() => {
                    g.touched = None;
                    Some(true)
                }
                Some(_) => None,
            })
            .unwrap_or(Some(false));
            match go {
                None => continue,
                Some(run) => {
                    with(|g| g.auto_on = false);
                    if run {
                        check_async();
                    }
                    return;
                }
            }
        }
    });
}

/// 改图之前调用：记一步撤销。`tag` 非空时，一秒内连着来的同标签改动（打字、拖动）并成一步。
fn edit(g: &mut St, tag: &str) {
    record(g, tag);
    touch(g);
}

/// 只记撤销，不触发检查（挪位置用：不改变策略）。
fn record(g: &mut St, tag: &str) {
    let now = Instant::now();
    let merge = !tag.is_empty() && g.undo_tag.as_ref().is_some_and(|(t, at)| t == tag && now.duration_since(*at).as_millis() < 1000);
    if !merge {
        g.undo.push(g.graph.clone());
        if g.undo.len() > UNDO_DEPTH {
            g.undo.remove(0);
        }
    }
    g.redo.clear();
    g.undo_tag = (!tag.is_empty()).then(|| (tag.to_string(), now));
}

/// 换了一张图：选择、撤销栈、弹窗都清掉。
fn reset_editing(g: &mut St) {
    g.selected.clear();
    g.undo.clear();
    g.redo.clear();
    g.undo_tag = None;
    g.quick = None;
    g.tables.clear();
    g.touched = None;
    g.envelope_hit = None;
}

/// 撤销 / 重做之后：输入框里的原文作废（照图上的值重显），选择里去掉已经不在的节点。
fn after_swap(g: &mut St) {
    g.form.clear();
    g.tables.clear();
    g.undo_tag = None;
    let ids: Vec<String> = g.graph.nodes.iter().map(|n| n.id.clone()).collect();
    g.selected.retain(|s| ids.contains(s));
    g.quick = None;
    touch(g);
}

fn kind_of(g: &St, node: &str) -> String {
    g.graph.node(node).and_then(|n| g.palette.as_ref().and_then(|p| p.get(&n.module))).map(|d| d.kind.clone()).unwrap_or_default()
}

/// 加一个节点（入场带一条线的缺省属性，市场状态带名字）。返回新节点的 id。
fn add_node(g: &mut St, refer: &str, x: f32, y: f32) -> Option<String> {
    let def = g.palette.as_ref().and_then(|p| p.get(refer)).cloned()?;
    if def.kind == "exec" && g.graph.nodes.iter().any(|n| n.module == EXEC) {
        g.msg = "「执行」节点已经有一个了".to_string();
        return None;
    }
    let id = g.graph.fresh_id();
    let mut node = Node { id: id.clone(), module: refer.to_string(), x, y, ..Default::default() };
    if def.kind == "entry" {
        let taken: Vec<String> = g.graph.nodes.iter().filter_map(|n| n.lane.as_ref().and_then(|l| l.get("name")).and_then(Value::as_str).map(str::to_string)).collect();
        let mut lane = Map::new();
        lane.insert("name".into(), Value::String(unique_name(&format!("L{}", taken.len() + 1), &taken)));
        lane.insert("entry_tf".into(), Value::from(15));
        lane.insert("exit_tf".into(), Value::from(1));
        lane.insert("entry_on".into(), Value::String(if def.book == "basket" { "tick" } else { "bar" }.into()));
        node.lane = Some(lane);
    }
    if def.kind == "regime" {
        let taken: Vec<String> = g.graph.nodes.iter().filter_map(|n| n.name.clone()).collect();
        node.name = Some(unique_name(&def.title, &taken));
    }
    g.graph.nodes.push(node);
    Some(id)
}

fn delete_nodes(g: &mut St, ids: &[String]) {
    // 「执行」节点删不掉：每张图必须有一个
    let ids: Vec<&String> = ids.iter().filter(|id| g.graph.node(id).is_some_and(|n| n.module != EXEC)).collect();
    if ids.is_empty() {
        return;
    }
    edit(g, "");
    for id in ids {
        g.graph.remove_node(id);
        g.live.remove(id);
    }
    g.selected.clear();
}

fn copy_selected(g: &mut St) -> usize {
    let c = g.graph.excerpt(&g.selected);
    let n = c.nodes.len();
    if n > 0
        && let Ok(mut slot) = clip().lock()
    {
        *slot = c;
    }
    n
}

fn paste_at(g: &mut St, at: (f32, f32)) {
    let Some(c) = clip().lock().ok().map(|c| c.clone()).filter(|c| !c.nodes.is_empty()) else { return };
    edit(g, "");
    g.selected = g.graph.paste(&c, at);
    g.msg = format!("贴了 {} 个节点", g.selected.len());
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
                reset_editing(g);
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
        FlMsg::Config(section, name, text) => {
            with(|g| {
                let ty = g
                    .palette
                    .as_ref()
                    .and_then(|p| match section.as_str() {
                        "backtest" => Some(&p.backtest_params),
                        "envelope" => Some(&p.envelope_params),
                        "settings" => Some(&p.settings_params),
                        _ => None,
                    })
                    .and_then(|ps| ps.iter().find(|p| p.name == name))
                    .map(|p| p.ty.clone())
                    .unwrap_or_default();
                let parsed = parse_value(&text, &ty);
                edit(g, &format!("c.{section}.{name}"));
                g.form.insert(form_key(&section, &name), text);
                let slot = match section.as_str() {
                    "backtest" => &mut g.graph.backtest,
                    "envelope" => &mut g.graph.envelope,
                    _ => &mut g.graph.settings,
                };
                set_path(slot, &name, parsed);
            });
        }
        FlMsg::OpenSnapshot(dir) => {
            let path = std::path::Path::new(&dir).join("graph.json");
            if path.exists() {
                open(FileItem { path: path.to_string_lossy().to_string(), is_graph: true, snapshot: true });
            } else {
                with(|g| g.msg = "那次运行没有存图（这个功能之前跑的，或者不是工作流）".to_string());
            }
        }
        FlMsg::Center(m) => super::strategy_center::handle(m),
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
                edit(g, "meta.id");
                g.graph.id = v;
                g.path = None; // id 变了 = 另存为
            });
        }
        FlMsg::MetaName(v) => {
            with(|g| {
                edit(g, "meta.name");
                g.graph.name = v;
            });
        }
        FlMsg::AddNode(refer) => {
            with(|g| {
                // 放在当前视野的左上附近，逐个错开
                let k = g.graph.nodes.len() as f32;
                let (x, y) = ((40.0 - g.pan.0) / g.zoom + (k % 5.0) * 24.0, (40.0 - g.pan.1) / g.zoom + (k % 5.0) * 24.0);
                let before = g.graph.clone();
                if let Some(id) = add_node(g, &refer, x, y) {
                    let after = std::mem::replace(&mut g.graph, before);
                    edit(g, "");
                    g.graph = after;
                    g.selected = vec![id];
                }
            });
        }
        FlMsg::Select(id) => {
            with(|g| {
                g.selected = id.into_iter().collect();
                g.quick = None;
            });
        }
        FlMsg::SelectToggle(id) => {
            with(|g| {
                if let Some(i) = g.selected.iter().position(|s| *s == id) {
                    g.selected.remove(i);
                } else {
                    g.selected.push(id);
                }
            });
        }
        FlMsg::SelectBox(ids, add) => {
            with(|g| {
                if !add {
                    g.selected.clear();
                }
                for id in ids {
                    if !g.selected.contains(&id) {
                        g.selected.push(id);
                    }
                }
            });
        }
        FlMsg::SelectAll => {
            with(|g| g.selected = g.graph.nodes.iter().map(|n| n.id.clone()).collect());
        }
        FlMsg::MoveSelected(dx, dy) => {
            with(|g| {
                if g.selected.is_empty() {
                    return;
                }
                // 挪位置不改变策略：记撤销，但不触发检查、不把「检查通过」打回未检查
                record(g, "move");
                g.dirty = true;
                let sel = g.selected.clone();
                for n in g.graph.nodes.iter_mut().filter(|n| sel.contains(&n.id)) {
                    n.x += dx;
                    n.y += dy;
                }
            });
        }
        FlMsg::Delete(id) => {
            with(|g| delete_nodes(g, &[id]));
        }
        FlMsg::DeleteSelected => {
            with(|g| {
                let ids = g.selected.clone();
                delete_nodes(g, &ids);
            });
        }
        FlMsg::Copy => {
            with(|g| {
                let n = copy_selected(g);
                if n > 0 {
                    g.msg = format!("复制了 {n} 个节点");
                }
            });
        }
        FlMsg::Cut => {
            with(|g| {
                if copy_selected(g) > 0 {
                    let ids = g.selected.clone();
                    delete_nodes(g, &ids);
                }
            });
        }
        FlMsg::Paste(at) => {
            with(|g| {
                let at = at.unwrap_or(((60.0 - g.pan.0) / g.zoom, (60.0 - g.pan.1) / g.zoom));
                paste_at(g, at);
            });
        }
        FlMsg::Duplicate => {
            with(|g| {
                let c = g.graph.excerpt(&g.selected);
                if c.nodes.is_empty() {
                    return;
                }
                let (x0, y0) = c.nodes.iter().fold((f32::MAX, f32::MAX), |a, n| (a.0.min(n.x), a.1.min(n.y)));
                edit(g, "");
                g.selected = g.graph.paste(&c, (x0 + 32.0, y0 + 32.0));
            });
        }
        FlMsg::Undo => {
            with(|g| {
                if let Some(prev) = g.undo.pop() {
                    g.redo.push(std::mem::replace(&mut g.graph, prev));
                    after_swap(g);
                }
            });
        }
        FlMsg::Redo => {
            with(|g| {
                if let Some(next) = g.redo.pop() {
                    g.undo.push(std::mem::replace(&mut g.graph, next));
                    after_swap(g);
                }
            });
        }
        FlMsg::QuickOpen(q) => {
            with(|g| g.quick = Some(q));
        }
        FlMsg::QuickQuery(s) => {
            with(|g| {
                if let Some(q) = g.quick.as_mut() {
                    q.query = s;
                }
            });
        }
        FlMsg::QuickClose => {
            with(|g| g.quick = None);
        }
        FlMsg::QuickPick(refer) => {
            with(|g| {
                let Some(q) = g.quick.take() else { return };
                let before = g.graph.clone();
                let Some(id) = add_node(g, &refer, q.x, q.y) else { return };
                let after = std::mem::replace(&mut g.graph, before);
                edit(g, "");
                g.graph = after;
                // 接到新节点上第一个同类型的输入端口（特征输入的名字是参数名，不等于类型）
                let inp = g.palette.as_ref().and_then(|p| p.get(&refer)).and_then(|d| d.ins.iter().find(|p| port_type(p) == port_type(&q.port)).cloned()).unwrap_or_else(|| q.port.clone());
                let multi = multi_in(&kind_of(g, &id), &inp);
                g.graph.connect(Edge { from: q.from, out: q.port, to: id.clone(), inp }, multi);
                g.selected = vec![id];
            });
        }
        FlMsg::LibQuery(s) => {
            with(|g| g.lib_query = s);
        }
        FlMsg::Connect(e) => {
            with(|g| {
                let multi = multi_in(&kind_of(g, &e.to), &e.inp);
                edit(g, "");
                g.graph.connect(e, multi);
            });
        }
        FlMsg::Disconnect(to, inp) => {
            with(|g| {
                edit(g, "");
                g.graph.edges.retain(|e| !(e.to == to && e.inp == inp));
            });
        }
        FlMsg::Swap(id, refer) => {
            with(|g| {
                // 换成同环节的另一个模块：名字相同的参数留着，其余丢掉
                let keep: Vec<String> = g.palette.as_ref().and_then(|p| p.get(&refer)).map(|d| d.params.iter().map(|p| p.name.clone()).collect()).unwrap_or_default();
                edit(g, "");
                if let Some(n) = g.graph.node_mut(&id) {
                    n.module = refer;
                    n.params.retain(|k, _| keep.contains(k));
                }
                g.form.retain(|k, _| !k.starts_with(&format!("{id}\u{1}")));
                g.tables.retain(|k, _| !k.starts_with(&format!("{id}\u{1}")));
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
                edit(g, &format!("p.{id}.{name}"));
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
            });
        }
        FlMsg::ParamValue(id, name, value) => {
            with(|g| {
                edit(g, "");
                g.form.remove(&form_key(&id, &name));
                if let Some(n) = g.graph.node_mut(&id) {
                    match value {
                        Some(v) => {
                            n.params.insert(name, v);
                        }
                        None => {
                            n.params.remove(&name);
                        }
                    }
                }
            });
        }
        FlMsg::TableEdit(id, name, row, col, text) => {
            with(|g| {
                let key = form_key(&id, &name);
                let mut rows = g.tables.get(&key).cloned().unwrap_or_else(|| table_rows(g.graph.node(&id).and_then(|n| n.params.get(&name))));
                let Some(cell) = rows.get_mut(row).and_then(|r| r.get_mut(col)) else { return };
                *cell = text;
                set_table(g, &id, &name, rows, &format!("t.{id}.{name}.{row}.{col}"));
            });
        }
        FlMsg::TableAdd(id, name) => {
            with(|g| {
                let key = form_key(&id, &name);
                let mut rows = g.tables.get(&key).cloned().unwrap_or_else(|| table_rows(g.graph.node(&id).and_then(|n| n.params.get(&name))));
                // 新行沿用上一行的外层键：多半是给同一个来源再加一项
                let outer = rows.last().map(|r| r[0].clone()).unwrap_or_default();
                rows.push([outer, String::new(), String::new()]);
                g.tables.insert(key, rows); // 空行还不算改动
            });
        }
        FlMsg::TableDel(id, name, row) => {
            with(|g| {
                let key = form_key(&id, &name);
                let mut rows = g.tables.get(&key).cloned().unwrap_or_else(|| table_rows(g.graph.node(&id).and_then(|n| n.params.get(&name))));
                if row < rows.len() {
                    rows.remove(row);
                    set_table(g, &id, &name, rows, "");
                }
            });
        }
        FlMsg::LaneField(id, name, text) => {
            with(|g| {
                let ty = if name == "entry_tf" || name == "exit_tf" { "integer" } else { "string" };
                let parsed = parse_value(&text, ty);
                edit(g, &format!("l.{id}.{name}"));
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
            });
        }
        FlMsg::RegimeName(id, text) => {
            with(|g| {
                edit(g, &format!("r.{id}"));
                if let Some(n) = g.graph.node_mut(&id) {
                    n.name = if text.trim().is_empty() { None } else { Some(text) };
                }
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

/// 在一个表里按「a.b」的路径写值（None = 删掉；删空了的子表一并删掉）。表单里 `session.start` 这类项用。
pub fn set_path(root: &mut Value, path: &str, value: Option<Value>) {
    if !root.is_object() {
        *root = Value::Object(Map::new());
    }
    let Some(map) = root.as_object_mut() else { return };
    match path.split_once('.') {
        None => match value {
            Some(v) => {
                map.insert(path.to_string(), v);
            }
            None => {
                map.remove(path);
            }
        },
        Some((head, rest)) => {
            let child = map.entry(head.to_string()).or_insert_with(|| Value::Object(Map::new()));
            set_path(child, rest, value);
            if child.as_object().is_some_and(Map::is_empty) {
                map.remove(head);
            }
        }
    }
}

/// 按「a.b」的路径读值。
pub fn get_path<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(root, |v, k| v.get(k))
}

fn set_table(g: &mut St, id: &str, name: &str, rows: Vec<[String; 3]>, tag: &str) {
    edit(g, tag);
    let value = rows_table(&rows);
    g.tables.insert(form_key(id, name), rows);
    if let Some(n) = g.graph.node_mut(id) {
        match value {
            Some(v) => {
                n.params.insert(name.to_string(), v);
            }
            None => {
                n.params.remove(name);
            }
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
        // 列表不用写成 JSON：逗号隔开，数按数、文字按文字；显示时也是逗号隔开
        assert_eq!(parse_value("1, 15，60", "array"), Some(serde_json::json!([1, 15, 60])));
        assert_eq!(parse_value("trend_up, range", "array"), Some(serde_json::json!(["trend_up", "range"])));
        assert_eq!(value_text(&serde_json::json!(["trend_up", "range"])), "trend_up, range");
        assert_eq!(value_text(&serde_json::json!([1, 15])), "1, 15");
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

    #[test]
    fn two_level_tables_edit_as_rows_and_back() {
        let v = serde_json::json!({"trend_up": {"S1": 1.0, "S2": 0}, "range": {"S1": 0.5}});
        let rows = table_rows(Some(&v));
        assert_eq!(rows.len(), 3);
        assert_eq!(rows_table(&rows), Some(v));
        // 键没填、值没填的行不算；全空 = 用缺省值
        assert_eq!(rows_table(&[["a".into(), "".into(), "2".into()], ["".into(), "x".into(), "1".into()], ["b".into(), "k".into(), "".into()]]), Some(serde_json::json!({"a": 2})));
        assert_eq!(rows_table(&[["".into(), "".into(), "".into()]]), None);
        assert_eq!(table_rows(None), Vec::<[String; 3]>::new());
    }

    #[test]
    fn paste_gives_fresh_ids_unique_names_and_keeps_inner_wires() {
        let mut g = Graph::blank();
        let lane = |name: &str| Some([("name".to_string(), Value::String(name.into()))].into_iter().collect::<Map<String, Value>>());
        g.nodes.push(Node { id: "n2".into(), module: "setup.x@1".into(), x: 10.0, y: 20.0, ..Default::default() });
        g.nodes.push(Node { id: "n3".into(), module: "entry.y@1".into(), x: 210.0, y: 20.0, lane: lane("S1"), ..Default::default() });
        g.nodes.push(Node { id: "n4".into(), module: "regime.z@1".into(), name: Some("trend".into()), ..Default::default() });
        g.edges.push(Edge { from: "n2".into(), out: "signal".into(), to: "n3".into(), inp: "signal".into() });
        g.edges.push(Edge { from: "n3".into(), out: "intent".into(), to: "n1".into(), inp: "intent".into() });
        // 「执行」不复制；只带走选中节点之间的线
        let clip = g.excerpt(&["n1".into(), "n2".into(), "n3".into(), "n4".into()]);
        assert_eq!((clip.nodes.len(), clip.edges.len()), (3, 1));
        let new = g.paste(&clip, (100.0, 100.0));
        assert_eq!(new, ["n5", "n6", "n7"]);
        assert_eq!(g.edges.len(), 3);
        assert!(g.edges.contains(&Edge { from: "n5".into(), out: "signal".into(), to: "n6".into(), inp: "signal".into() }));
        assert_eq!(g.node("n6").unwrap().lane.as_ref().unwrap()["name"], "S1_2"); // 线名不能重复
        assert_eq!(g.node("n7").unwrap().name.as_deref(), Some("trend_2"));
        // 左上角对齐到粘贴点，相对位置不变（n4 在 (0,0) 是这一组的左上角）
        assert_eq!((g.node("n5").unwrap().x, g.node("n6").unwrap().x), (110.0, 310.0));
        assert_eq!(unique_name("S1_2", &["S1".into(), "S1_2".into()]), "S1_3");
    }

    #[test]
    fn quick_search_lists_only_modules_that_accept_the_dragged_port() {
        let m = |title: &str, kind: &str, ins: &[&str]| ModDef { refer: format!("{kind}.{title}@1"), title: title.into(), kind: kind.into(), kind_label: kind.into(), ins: ins.iter().map(|s| s.to_string()).collect(), ..Default::default() };
        let pal = Palette { modules: vec![m("calendar", "confirm", &["signal"]), m("stop_ladder", "entry", &["signal", "stop"]), m("breakeven", "exit", &["position"])], ..Default::default() };
        let mut q = Quick { port: "signal".into(), ..Default::default() };
        assert_eq!(q.candidates(&pal).iter().map(|m| m.title.as_str()).collect::<Vec<_>>(), ["calendar", "stop_ladder"]);
        q.query = "LADDER".into();
        assert_eq!(q.candidates(&pal).len(), 1);
        q.port = "regime".into();
        q.query.clear();
        assert!(q.candidates(&pal).is_empty());
        // 特征输入端口以参数名为名，按类型匹配
        let pal = Palette { modules: vec![m("vol_unit", "sizing", &["n_feature"]), m("threshold", "setup", &["feature"])], ..Default::default() };
        q.port = "feature".into();
        assert_eq!(q.candidates(&pal).len(), 2);
        assert_eq!((port_type("n_feature"), port_type("feature"), port_type("signal")), ("feature", "feature", "signal"));
    }

    #[test]
    fn config_form_writes_nested_paths_and_cleans_up_empty_tables() {
        let mut v = Value::Null;
        set_path(&mut v, "source", Some("databento".into()));
        set_path(&mut v, "session.start", Some("13:30".into()));
        set_path(&mut v, "session.minutes", Some(30.into()));
        assert_eq!(v, serde_json::json!({"source": "databento", "session": {"start": "13:30", "minutes": 30}}));
        assert_eq!(get_path(&v, "session.minutes"), Some(&Value::from(30)));
        assert_eq!(get_path(&v, "session.nope"), None);
        set_path(&mut v, "session.start", None);
        set_path(&mut v, "session.minutes", None); // 子表删空了就整个去掉，不留一个空的 session = {}
        assert_eq!(v, serde_json::json!({"source": "databento"}));
    }

    #[test]
    fn param_controls_follow_declared_type() {
        let p = |ty: &str, choices: Option<Vec<Value>>| ParamDef { ty: ty.into(), choices, ..Default::default() };
        assert_eq!(p("boolean", None).control(), Control::Switch);
        assert_eq!(p("string", Some(vec!["a".into(), "b".into()])).control(), Control::Choice);
        assert_eq!(p("array", None).control(), Control::List);
        assert_eq!(p("object", None).control(), Control::Table);
        assert_eq!(p("number", None).control(), Control::Text);
    }
}
