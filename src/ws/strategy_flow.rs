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

use super::data_picker::{self as dp, BSource, DataPick, DataPickMsg, PickOpts, Purpose, Selection, TimeMode};

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
    /// 端口类型，与 `ins` / `outs` 一一对应（连线只看类型）。数值层：series = 数，flag = 条件
    pub in_types: Vec<String>,
    pub out_types: Vec<String>,
    pub params: Vec<ParamDef>,
    /// 子图资产（kind = sub）才有：版本、文件、里面有哪些模块、验证记录
    pub version: String,
    pub path: String,
    pub inner: Vec<String>,
    pub validations: Vec<Value>,
}

impl ModDef {
    pub fn in_type<'a>(&'a self, port: &'a str) -> &'a str {
        self.ins.iter().position(|p| p == port).and_then(|i| self.in_types.get(i)).map_or(port_type(port), String::as_str)
    }

    pub fn out_type<'a>(&'a self, port: &'a str) -> &'a str {
        self.outs.iter().position(|p| p == port).and_then(|i| self.out_types.get(i)).map_or(port_type(port), String::as_str)
    }

    /// 第一个这种类型的输入端口。
    pub fn first_in(&self, ty: &str) -> Option<&String> {
        self.ins.iter().find(|p| self.in_type(p) == ty)
    }
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
    /// 新建时可选的起步图（docs/45 §4.6）
    pub templates: Vec<Template>,
    /// 抽象数据的名字（price / book / trades → 中文），数据选择器置灰用（docs/45 §21 D2）
    pub data_kinds: BTreeMap<String, String>,
    /// 数据商 → 类型 → 满足的抽象数据（`datareq.VENDOR_TYPES`）
    pub vendor_types: BTreeMap<String, BTreeMap<String, Vec<String>>>,
}

/// 一份模板：只是起点，新建出来的图与它不保持关联。
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Template {
    pub path: String,
    pub title: String,
    pub doc: String,
}

impl std::fmt::Display for Template {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.title)
    }
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
    /// 属于哪个子图（docs/45 C4）。只影响显示
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// 无线引用：它的输出不画线，用它的节点在端口旁标一个名字。只影响显示
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub wireless: bool,
    /// 存成子图资产时要暴露的参数：{参数名: 暴露名}（docs/45 §4.3 第 4 条）
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub expose: Map<String, Value>,
    /// 旁路（docs/45 §4.4）：当它不存在。过滤 → 全部放行；出场 / 止损 / 缩放 → 不调整；风控 → 不审
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub bypass: bool,
    /// 待比较（docs/45 C7）：`{"use": [模块…]}` 或 `{参数: [取值…]}`；「跑变体」取它们的笛卡尔积
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub vary: Map<String, Value>,
    /// 由连线驱动的参数（docs/45 C8）：每个变成一个「数」输入端口，接上的数值链在运行时给它取值
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub driven: Vec<String>,
    /// 这份代码不认识的字段：原样留着、原样写回（新版画布加的东西不会被旧版存丢）
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// 子图 / 分组框（docs/45 §4.2）：一组节点。展开时画一个框，折叠时顶层只占一个节点，可以进去看。
/// **只影响显示**——编译时不看分组。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Group {
    pub id: String,
    pub title: String,
    pub collapsed: bool,
    /// 从哪个子图资产「解除关联」展开来的（`sub.<id>@<大版本>`）：改完可以存回去
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset: Option<String>,
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
    pub groups: Vec<Group>,
    /// 不认识的字段（运行快照里冻结的子图 `assets`、新版加的东西）：原样留着、原样写回
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

pub const EXEC: &str = "kernel.exec";
/// 这份代码写出的图格式版本，与 `strategies/swe/graph.py` 的 FORMAT 一致（Python 端拒绝比它新的）。
pub const GRAPH_FORMAT: u32 = 6;
/// 最小缩放：上千个节点的图要能整张看全（低于 0.3 只画色块，见视图的细节层级）。
pub const MIN_ZOOM: f32 = 0.1;
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
        self.prune_groups();
    }

    /// 没有成员的子图去掉。
    fn prune_groups(&mut self) {
        let used: Vec<String> = self.nodes.iter().filter_map(|n| n.group.clone()).collect();
        self.groups.retain(|g| used.contains(&g.id));
    }

    pub fn group(&self, id: &str) -> Option<&Group> {
        self.groups.iter().find(|g| g.id == id)
    }

    pub fn members(&self, gid: &str) -> Vec<String> {
        self.nodes.iter().filter(|n| n.group.as_deref() == Some(gid)).map(|n| n.id.clone()).collect()
    }

    /// 把这些节点编成一个新的子图（原来各自的分组作废）。返回新子图的 id。
    pub fn make_group(&mut self, ids: &[String], title: &str) -> Option<String> {
        if !self.nodes.iter().any(|n| ids.contains(&n.id)) {
            return None;
        }
        let max = self.groups.iter().filter_map(|g| g.id.strip_prefix('g').and_then(|s| s.parse::<u32>().ok())).max().unwrap_or(0);
        let gid = format!("g{}", max + 1);
        for n in self.nodes.iter_mut().filter(|n| ids.contains(&n.id)) {
            n.group = Some(gid.clone());
        }
        self.groups.push(Group { id: gid.clone(), title: title.to_string(), collapsed: false, asset: None });
        self.prune_groups();
        Some(gid)
    }

    /// 解散子图：节点留着，只是不再属于它。
    pub fn dissolve(&mut self, gid: &str) {
        for n in self.nodes.iter_mut().filter(|n| n.group.as_deref() == Some(gid)) {
            n.group = None;
        }
        self.groups.retain(|g| g.id != gid);
    }

    /// 搜索（docs/45 §4.2）：模块名、标题、线名、状态名、参数的值里含这段文字的节点。
    pub fn find(&self, query: &str, palette: Option<&Palette>) -> Vec<String> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        self.nodes
            .iter()
            .filter(|n| {
                let def = palette.and_then(|p| p.get(&n.module));
                let mut hay = vec![n.module.to_lowercase(), n.name.clone().unwrap_or_default().to_lowercase()];
                if let Some(d) = def {
                    hay.push(d.title.to_lowercase());
                    hay.push(d.kind_label.to_lowercase());
                }
                hay.extend(n.params.iter().map(|(k, v)| format!("{k}={}", value_text(v)).to_lowercase()));
                if let Some(l) = &n.lane {
                    hay.extend(l.values().map(|v| value_text(v).to_lowercase()));
                }
                hay.iter().any(|h| h.contains(&q))
            })
            .map(|n| n.id.clone())
            .collect()
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
            n2.group = None; // 贴出来的是散着的节点，要分组自己再编
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

/// 调试面板的页（docs/45 C5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DebugTab {
    #[default]
    Trades,
    WhyNot,
    Breaks,
    /// 运行队列与变体对比（docs/45 C7）
    Queue,
}

/// 运行队列里的一项：一组变体（没标待比较 = 一组只有一次）。经策略中心跑，每次都计入全局试验数。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Job {
    pub title: String,
    pub path: String,
    pub start: String,
    pub end: String,
    /// 数据根目录不是缺省时传给子进程的环境变量（`WS_DATABENTO_ROOT` …）
    pub roots: Vec<(String, String)>,
    /// waiting / running / done / failed / cancelled
    pub status: String,
    pub study_id: String,
    pub labels: Vec<String>,
    /// 旁路 / 静音了什么（跑的时候图上的状态）
    pub marks: Vec<String>,
    /// 每组一条：done / failed 事件（带 metrics / error）
    pub rows: Vec<Value>,
    pub curves: Option<Value>,
    pub global_trials: Option<i64>,
    pub error: String,
}

impl Job {
    pub fn n_finished(&self) -> usize {
        self.rows.len()
    }
}

/// 一笔成交的来龙去脉里的一步。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PathStep {
    pub ts: i64,
    pub kind: String,
    pub module: String,
    pub reason: String,
    /// 依据：`why` 里的键值（规则模块还带着条件里各段的值）
    pub why: String,
    pub node: Option<String>,
}

/// 决策轨迹里一个时间戳（纳秒）→ 「月-日 时:分:秒」（UTC）。
pub fn ts_text(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts.div_euclid(1_000_000_000), ts.rem_euclid(1_000_000_000) as u32).map(|d| d.format("%m-%d %H:%M:%S").to_string()).unwrap_or_default()
}

fn why_text(v: &Value) -> String {
    let mut out = Vec::new();
    if let Some(m) = v.as_object() {
        for (k, x) in m {
            match x {
                Value::Object(inner) => out.extend(inner.iter().map(|(a, b)| format!("{a} = {}", value_text(b)))),
                other => out.push(format!("{k} = {}", value_text(other))),
            }
        }
    }
    out.join("；")
}

/// 「按成交反查」（docs/45 §4.5）：第 `i` 条事件（一笔结束）→ 这一笔从提出意图到平仓经过的每一步，以及它们落在哪些节点上。
///
/// 从这一笔往回找同一条线上的事件，找到上一笔结束为止（不含）；意图里带着的来龙去脉（信号、过滤……）也算进去。
pub fn trade_path(events: &[Value], i: usize, whence: &HashMap<String, String>) -> (Vec<PathStep>, Vec<String>) {
    const KINDS: [&str; 9] = ["intent", "fill", "close", "move_stop", "move_target", "add", "veto", "trade", "why_not"];
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let Some(end) = events.get(i) else { return (Vec::new(), Vec::new()) };
    let lane = s(end, "lane");
    let mut steps = Vec::new();
    let mut nodes: Vec<String> = Vec::new();
    for (j, ev) in events[..=i].iter().enumerate().rev() {
        if s(ev, "lane") != lane {
            continue;
        }
        let kind = s(ev, "kind");
        if j < i && kind == "trade" {
            break; // 上一笔
        }
        let wanted = KINDS.contains(&kind.as_str()) || kind.starts_with("risk_") || kind.starts_with("envelope_");
        if !wanted || kind == "why_not" {
            continue;
        }
        let module = s(ev, "module");
        let node = locate(whence, &lane, &module);
        // 往回走的，最后整体倒过来：先放这一步，再放它的来龙去脉，倒过来之后依据在前
        steps.push(PathStep { ts: ev.get("ts").and_then(Value::as_i64).unwrap_or(0), kind, module: module.clone(), reason: s(ev, "reason"), why: ev.get("why").map(why_text).unwrap_or_default(), node });
        if let Some(trail) = ev.get("trail").and_then(Value::as_array) {
            // 意图的来龙去脉：信号模块（带着它的依据）、入场模块
            for o in trail.iter().rev() {
                let m = o.get(0).and_then(Value::as_str).unwrap_or("");
                if m.is_empty() || m == module {
                    continue;
                }
                let n = locate(whence, &lane, m);
                steps.push(PathStep { ts: ev.get("ts").and_then(Value::as_i64).unwrap_or(0), kind: "origin".into(), module: m.to_string(), reason: String::new(), why: o.get(2).map(why_text).unwrap_or_default(), node: n });
            }
        }
        if steps.len() > 60 {
            break;
        }
    }
    steps.reverse();
    for st in &steps {
        if let Some(n) = &st.node
            && !nodes.contains(n)
        {
            nodes.push(n.clone());
        }
    }
    (steps, nodes)
}

/// 拖线拖到空白处弹出的「接什么」搜索框（docs/45 §2：只列能接的节点）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Quick {
    pub from: String,
    pub port: String,
    /// 拖出来的那个端口的类型
    pub ty: String,
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
            .filter(|m| m.first_in(&self.ty).is_some())
            .filter(|m| q.is_empty() || m.title.to_lowercase().contains(&q) || m.kind_label.contains(&q) || m.summary.to_lowercase().contains(&q))
            .collect()
    }
}

/// 节点实时数值（docs/49 M2）：编译结果里这个节点在运行时按什么取值——数值节点按表达式文本，特征节点按特征名。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(default)]
pub struct Probe {
    pub expr: Option<String>,
    pub feature: Option<String>,
}

/// 节点上显示的一个值。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Val {
    Num(f64),
    Flag(bool),
    /// 还没算好（预热中、这一格没有数据）
    Missing,
}

/// 一个节点的实时数值：当前值与最近一段（小图用）。小图里条件记 1 / 0，没算好记 NaN。
#[derive(Debug, Clone, PartialEq)]
pub struct NodeValue {
    pub v: Val,
    pub hist: std::collections::VecDeque<f32>,
    /// 回测时刻（纳秒）
    pub ts: i64,
}

/// 小图留多少个点。宿主每 0.25 秒（墙上时间）推一次，约半分钟。
pub const VALUE_HIST: usize = 120;
/// 节点实时数值只在不小于这个缩放时显示（再小就不画底栏了，见视图的细节层级）。
pub const VALUE_MIN_ZOOM: f32 = 0.5;

/// 要宿主推哪些：屏幕上看得见的、有取值方式的节点（docs/49 M2「按需」）。缩得太小、底栏不画时一个都不要。
/// 返回（表达式文本, 特征名），各自去重排序。
#[must_use]
pub fn watch_of(graph: &Graph, probes: &HashMap<String, Probe>, focus: Option<&str>, pan: (f32, f32), zoom: f32, size: (f32, f32)) -> (Vec<String>, Vec<String>) {
    let (mut exprs, mut feats) = (Vec::new(), Vec::new());
    if zoom < VALUE_MIN_ZOOM {
        return (exprs, feats);
    }
    let folded: std::collections::HashSet<&str> = graph.groups.iter().filter(|g| g.collapsed).map(|g| g.id.as_str()).collect();
    // 节点的大小在视图里按端口数算；这里只判断在不在屏幕上，取一个宽松的高度
    let (w, h) = (168.0 * zoom, 120.0 * zoom);
    for n in &graph.nodes {
        let shown = match focus {
            Some(f) => n.group.as_deref() == Some(f),
            None => !n.group.as_deref().is_some_and(|g| folded.contains(g)),
        };
        let (x, y) = (n.x * zoom + pan.0, n.y * zoom + pan.1);
        if !shown || x + w < 0.0 || y + h < 0.0 || x > size.0 || y > size.1 {
            continue;
        }
        match probes.get(&n.id) {
            Some(Probe { expr: Some(e), .. }) => exprs.push(e.clone()),
            Some(Probe { feature: Some(f), .. }) => feats.push(f.clone()),
            _ => {}
        }
    }
    exprs.sort();
    exprs.dedup();
    feats.sort();
    feats.dedup();
    (exprs, feats)
}

/// 宿主推来的一份值（`ws:swe:<运行号>:values`）落到节点上。这一份里没有的节点（不在屏幕上）不动。
pub fn apply_values(values: &mut HashMap<String, NodeValue>, probes: &HashMap<String, Probe>, got: &Value) {
    let ts = got.get("ts").and_then(Value::as_i64).unwrap_or(0);
    for (nid, p) in probes {
        let raw = match p {
            Probe { expr: Some(e), .. } => got.get("e").and_then(|m| m.get(e)),
            Probe { feature: Some(f), .. } => got.get("f").and_then(|m| m.get(f)),
            _ => None,
        };
        let Some(raw) = raw else { continue };
        let v = match raw {
            Value::Bool(b) => Val::Flag(*b),
            Value::Number(x) => x.as_f64().map_or(Val::Missing, Val::Num),
            _ => Val::Missing,
        };
        let e = values.entry(nid.clone()).or_insert_with(|| NodeValue { v, hist: std::collections::VecDeque::new(), ts });
        e.v = v;
        e.ts = ts;
        e.hist.push_back(match v {
            Val::Num(x) => x as f32,
            Val::Flag(b) => f32::from(u8::from(b)),
            Val::Missing => f32::NAN,
        });
        while e.hist.len() > VALUE_HIST {
            e.hist.pop_front();
        }
    }
}

/// 成本线的一句话：平掉的那些笔平均毛赚多少 bp、手续费吃掉多少 bp、净多少，开仓 / 平仓里被动成交各占几成。
#[must_use]
pub fn cost_line(c: &Map<String, Value>) -> String {
    let num = |k: &str| c.get(k).and_then(Value::as_f64);
    let bp = |k: &str| num(k).map_or("—".to_string(), |x| format!("{x:+.2}"));
    let pct = |k: &str| num(k).map_or("—".to_string(), |x| format!("{:.0}%", x * 100.0));
    format!(
        "成本线：{} 笔，平均毛 {} bp − 手续费 {} bp = 净 {} bp（按开仓名义额）；被动成交 开仓 {} · 平仓 {}",
        num("trades").unwrap_or(0.0),
        bp("gross_bp"),
        num("fee_bp").map_or("—".to_string(), |x| format!("{x:.2}")),
        bp("net_bp"),
        pct("maker_open"),
        pct("maker_close"),
    )
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

/// 这张图要什么数据（`strategies/swe/datareq.py`，docs/45 §21）：抽象类 price / book / trades 与原因。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(default)]
pub struct Needs {
    pub kinds: Vec<String>,
    pub why: BTreeMap<String, String>,
    pub grid_ms: u64,
}

impl Needs {
    /// 抽象类的中文名（与 `datareq.KINDS` 同一组）。
    #[must_use]
    pub fn label(kind: &str) -> &str {
        match kind {
            "price" => "价格（买一 / 卖一）",
            "book" => "盘口增量",
            "trades" => "逐笔成交",
            "l3" => "逐单（L3）",
            other => other,
        }
    }
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
    /// 从模板新建（docs/45 §4.6）
    NewFrom(Template),
    /// 子图资产（docs/45 C6）：存成资产时用的 id、要不要升大版本
    AssetId(String),
    AssetMajor(bool),
    /// 把这个分组存成子图资产（图里换成一个引用它的节点）
    SaveAsset(String),
    /// 解除关联：把子图实例展开成一个普通分组
    Unlink(String),
    /// 存成资产时暴露这个参数：(节点, 参数, 暴露与否)
    Expose(String, String, bool),
    /// 代码节点 = 生成模块（docs/45 §3.4）：环节、名字、生成、契约测试
    CodeKind(String),
    CodeName(String),
    CodeNew,
    CodeTest,
    /// 旁路这个节点 / 静音这条线（入场节点）
    Bypass(String, bool),
    LaneMute(String, bool),
    /// 待比较：换模块的候选（节点, 模块, 加 / 去）
    VaryUse(String, String, bool),
    /// 待比较：参数的候选取值（节点, 参数, 逗号隔开的文字；空 = 不比较）
    VaryParam(String, String, String),
    /// 把当前的图（先保存）按当前日期排进运行队列
    Enqueue,
    QueueCancel(usize),
    QueuePick(usize),
    /// 这个参数改为由连线驱动 / 取消（取消时拔掉接在上面的线）
    Drive(String, String, bool),
    /// 节点上叠显示上次运行各模块的耗时
    ProfileToggle,
    Open(FileItem),
    Save,
    Check,
    Run,
    Stop,
    /// 共用数据选择组件（docs/45 §21）：选完写回 `[backtest]`
    Data(DataPickMsg),
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
    /// Ctrl / Shift 点一个折叠的子图：成员一起加选 / 减选
    SelectToggleMany(Vec<String>),
    /// 把选中的节点编成一个子图
    GroupSelected,
    Ungroup(String),
    ToggleCollapse(String),
    GroupTitle(String, String),
    /// 进入一个子图只看它里面（None = 回顶层）
    Focus(Option<String>),
    Wireless(String, bool),
    /// 画布告诉这边它现在多大（搜索跳转、看全要用）
    Viewport(f32, f32),
    SetPan(f32, f32),
    FindQuery(String),
    FindNext,
    /// 自动整理（选中了就只整理选中的）
    Tidy,
    /// 模块库左栏：展开 / 收起一个分类
    LibToggle(String),
    /// 调试面板（docs/45 C5）
    DebugToggle,
    DebugTab(DebugTab),
    /// 点一笔成交（事件序号）→ 亮出它的来龙去脉
    PickTrade(usize),
    /// 在这个节点上设 / 取消断点
    Breakpoint(String, bool),
    /// 运行控制：pause / run / step
    Ctl(String),
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
    /// 模块库里展开着的分类
    lib_open: Vec<String>,
    /// 正在看哪个子图里面（None = 顶层）
    focus: Option<String>,
    view_size: (f32, f32),
    find: String,
    find_i: usize,
    /// 表格参数正在编辑的行：键同 `form`。每行 [外层键, 内层键, 值]
    tables: BTreeMap<String, Vec<[String; 3]>>,
    /// 最近一次改动的时刻（自动检查用）；检查过了清掉
    touched: Option<Instant>,
    auto_on: bool,
    /// 参数输入框里的原文：键 = 节点 id + '\u{1}' + 参数名（允许暂时填着不合法的东西）
    form: BTreeMap<String, String>,
    issues: Vec<Issue>,
    warnings: Vec<String>,
    needs: Needs,
    /// 轨迹定位表：`线名|模块 id` → 节点 id
    whence: HashMap<String, String>,
    checked: Option<bool>,
    busy: bool,
    msg: String,
    pan: (f32, f32),
    zoom: f32,
    /// 数据选择器的状态（根目录等与机器有关的东西只在这里，不进图）
    pick: DataPick,
    /// 端到端自测钩子（`WS_FLOW_RUN`）用：这次运行把结束日改成它，不改图
    auto_end: Option<String>,
    pace: String,
    run_id: String,
    child: Option<Child>,
    running: bool,
    live: HashMap<String, NodeLive>,
    live_n: usize,
    /// 节点实时数值（docs/49 M2）：节点 → 怎么取值；节点 → 最近收到的值
    probes: HashMap<String, Probe>,
    values: HashMap<String, NodeValue>,
    /// 这次运行的成本线（docs/49 M4，宿主结束时推的 `costs`）
    costs: Option<Map<String, Value>>,
    /// 这次运行里强制风控层最近一次出手：(拒绝 / 缩量 / 停手, 原因, 共几次)
    envelope_hit: Option<(String, String, u32)>,
    /// 这次运行收到的决策轨迹（调试面板用；最多留 5 万条）
    events: Vec<Value>,
    /// 其中「一笔结束」的那些的序号（视图每帧要，别每帧去五万条里筛）
    trade_idx: Vec<usize>,
    /// 各条线最新的「为什么没做」计数
    why_not: BTreeMap<String, Map<String, Value>>,
    debug_open: bool,
    debug_tab: DebugTab,
    picked_trade: Option<usize>,
    trace_path: Vec<PathStep>,
    path_nodes: Vec<String>,
    breakpoints: Vec<String>,
    /// 回测停在哪（调试器写的那条事件）
    paused: Option<Value>,
    /// 换一次运行加一：读轨迹的线程发现自己过时了就退出
    generation: u64,
    asset_id: String,
    asset_major: bool,
    code_kind: String,
    code_name: String,
    /// 最近生成的代码节点：(模块引用, 文件)
    code_made: Option<(String, String)>,
    /// 它的契约测试结果（None = 还没测）
    code_problems: Option<Vec<String>>,
    queue: Vec<Job>,
    queue_pick: Option<usize>,
    queue_child: Option<Child>,
    /// 上次运行各节点的耗时：节点 → (毫秒, 调用次数)
    profile: HashMap<String, (f64, u64)>,
    profile_on: bool,
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
            lib_open: Vec::new(),
            focus: None,
            view_size: (1280.0, 800.0),
            find: String::new(),
            find_i: 0,
            tables: BTreeMap::new(),
            touched: None,
            auto_on: false,
            form: BTreeMap::new(),
            issues: Vec::new(),
            warnings: Vec::new(),
            needs: Needs::default(),
            whence: HashMap::new(),
            checked: None,
            busy: false,
            msg: String::new(),
            pan: (24.0, 16.0),
            zoom: 1.0,
            pick: DataPick::default(),
            auto_end: None,
            pace: "3600".into(),
            run_id: String::new(),
            child: None,
            running: false,
            live: HashMap::new(),
            live_n: 0,
            probes: HashMap::new(),
            values: HashMap::new(),
            costs: None,
            envelope_hit: None,
            events: Vec::new(),
            trade_idx: Vec::new(),
            why_not: BTreeMap::new(),
            debug_open: false,
            debug_tab: DebugTab::Trades,
            picked_trade: None,
            trace_path: Vec::new(),
            path_nodes: Vec::new(),
            breakpoints: Vec::new(),
            paused: None,
            generation: 0,
            asset_id: String::new(),
            asset_major: false,
            code_kind: "setup".into(),
            code_name: String::new(),
            code_made: None,
            code_problems: None,
            queue: Vec::new(),
            queue_pick: None,
            queue_child: None,
            profile: HashMap::new(),
            profile_on: false,
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
    pub lib_open: Vec<String>,
    pub focus: Option<String>,
    pub find: String,
    /// 搜索命中了几个、现在是第几个
    pub find_hits: (usize, usize),
    pub tables: BTreeMap<String, Vec<[String; 3]>>,
    pub form: BTreeMap<String, String>,
    pub issues: Vec<Issue>,
    pub warnings: Vec<String>,
    pub needs: Needs,
    pub checked: Option<bool>,
    pub busy: bool,
    pub msg: String,
    pub pan: (f32, f32),
    pub zoom: f32,
    pub pick: DataPick,
    pub pace: String,
    pub run_id: String,
    pub running: bool,
    pub live: HashMap<String, NodeLive>,
    /// 节点实时数值（docs/49 M2）
    pub values: HashMap<String, NodeValue>,
    /// 成本线（docs/49 M4）
    pub costs: Option<Map<String, Value>>,
    pub live_n: usize,
    pub envelope_hit: Option<(String, String, u32)>,
    /// 这次运行的成交（一笔结束）：(事件序号, 事件)
    pub trades: Vec<(usize, Value)>,
    pub why_not: BTreeMap<String, Map<String, Value>>,
    pub debug_open: bool,
    pub debug_tab: DebugTab,
    pub picked_trade: Option<usize>,
    pub trace_path: Vec<PathStep>,
    pub path_nodes: Vec<String>,
    pub breakpoints: Vec<String>,
    pub paused: Option<Value>,
    pub asset_id: String,
    pub asset_major: bool,
    pub code_kind: String,
    pub code_name: String,
    pub code_made: Option<(String, String)>,
    pub code_problems: Option<Vec<String>>,
    pub queue: Vec<Job>,
    pub queue_pick: Option<usize>,
    pub profile: HashMap<String, (f64, u64)>,
    pub profile_on: bool,
}

/// 「新建代码节点」能生成哪些环节（与 `strategies/swe/codegen.py` 的模板一致）。
pub const CODE_KINDS: [&str; 5] = ["features", "setup", "confirm", "exit", "sizing"];

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
        lib_open: g.lib_open.clone(),
        focus: g.focus.clone(),
        find: g.find.clone(),
        find_hits: (g.graph.find(&g.find, g.palette.as_ref()).len(), g.find_i),
        tables: g.tables.clone(),
        form: g.form.clone(),
        issues: g.issues.clone(),
        warnings: g.warnings.clone(),
        needs: g.needs.clone(),
        checked: g.checked,
        busy: g.busy,
        msg: g.msg.clone(),
        pan: g.pan,
        zoom: g.zoom,
        pick: g.pick.clone(),
        pace: g.pace.clone(),
        run_id: g.run_id.clone(),
        running: g.running,
        live: g.live.clone(),
        values: g.values.clone(),
        costs: g.costs.clone(),
        live_n: g.live_n,
        envelope_hit: g.envelope_hit.clone(),
        trades: if g.debug_open { g.trade_idx.iter().filter_map(|&i| g.events.get(i).map(|e| (i, e.clone()))).collect() } else { Vec::new() },
        why_not: g.why_not.clone(),
        debug_open: g.debug_open,
        debug_tab: g.debug_tab,
        picked_trade: g.picked_trade,
        trace_path: g.trace_path.clone(),
        path_nodes: g.path_nodes.clone(),
        breakpoints: g.breakpoints.clone(),
        paused: g.paused.clone(),
        asset_id: g.asset_id.clone(),
        asset_major: g.asset_major,
        code_kind: g.code_kind.clone(),
        code_name: g.code_name.clone(),
        code_made: g.code_made.clone(),
        code_problems: g.code_problems.clone(),
        queue: g.queue.clone(),
        queue_pick: g.queue_pick,
        profile: if g.profile_on { g.profile.clone() } else { HashMap::new() },
        profile_on: g.profile_on,
    })
    .expect("策略画布状态锁中毒")
}

impl View {
    /// 选中的恰好是某个子图的全部成员 → 那个子图（检查器显示它的操作）。
    pub fn selected_group(&self) -> Option<&Group> {
        let gid = self.graph.node(self.selected.first()?)?.group.as_deref()?;
        let members = self.graph.members(gid);
        (members.len() == self.selected.len() && members.iter().all(|m| self.selected.contains(m))).then(|| self.graph.group(gid)).flatten()
    }

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
    if let Some(r) = param_ref(v) {
        return format!("={r}");
    }
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
    // 参数引用（docs/45 §4.3 第 5 条）：`=节点.参数` = 与那个参数相同，改一处全跟着变
    if let Some(r) = t.strip_prefix('=').map(str::trim)
        && r.split_once('.').is_some_and(|(a, b)| !a.is_empty() && !b.is_empty())
    {
        return Some(serde_json::json!({ "$ref": r }));
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

/// 参数值是引用（`{"$ref": "节点.参数"}`）→ 引用的目标。
pub fn param_ref(v: &Value) -> Option<&str> {
    let m = v.as_object()?;
    (m.len() == 1).then(|| m.get("$ref").and_then(Value::as_str)).flatten()
}

/// 端口名 → 类型的兜底（模块库没给类型时用；与 graph.py 的 `ptype` 一致）。
/// 正常情况下类型来自模块库的 `in_types` / `out_types`（见 [`ModDef::in_type`]）。
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
                        g.auto_end = Some("2026-06-02".into());
                    });
                    handle(FlMsg::Run);
                });
            }
            // 样张截图用：`WS_FLOW_PICK=<秒>` 过这么久打开调试面板、点最后一笔成交（配合 WS_FLOW_RUN 看反查路径）
            if let Some(secs) = std::env::var("WS_FLOW_PICK").ok().and_then(|s| s.parse::<u64>().ok()) {
                super::spawn_named("ws-flowpick", move || {
                    std::thread::sleep(std::time::Duration::from_secs(secs));
                    handle(FlMsg::DebugToggle);
                    let last = with(|g| g.events.iter().rposition(|e| e.get("kind").and_then(Value::as_str) == Some("trade"))).flatten();
                    if let Some(i) = last {
                        handle(FlMsg::PickTrade(i));
                    }
                });
            }
            // 样张截图用：`WS_FLOW_FOCUS=<子图 id>` 进入那个子图；`WS_FLOW_EXPAND=<子图 id>` 把它展开
            if let Ok(gid) = std::env::var("WS_FLOW_FOCUS") {
                super::spawn_named("ws-flowfocus", move || {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    handle(FlMsg::Focus(Some(gid)));
                });
            }
            if let Ok(gid) = std::env::var("WS_FLOW_EXPAND") {
                super::spawn_named("ws-flowexp", move || {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    handle(FlMsg::ToggleCollapse(gid));
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
                            let ty = g.palette.as_ref().and_then(|p| p.get(&n.module)).map(|d| d.out_type(&port).to_string()).unwrap_or_default();
                            g.quick = Some(Quick { from: n.id.clone(), port, ty, x: n.x + 260.0, y: n.y + 60.0, query: String::new() });
                        }
                    });
                });
            }
            // 性能自测（docs/45 C8）：`WS_FLOW_BENCH=1` 打开后连着平移 / 缩放 240 帧，把视图构建与绘制的耗时打到标准错误
            if std::env::var_os("WS_FLOW_BENCH").is_some() {
                super::spawn_named("ws-flowbench", || {
                    std::thread::sleep(std::time::Duration::from_secs(6));
                    // 最坏的情况：缩到最小、视野里塞满节点（WS_FLOW_BENCH=near 则保持打开时的视野）
                    if std::env::var("WS_FLOW_BENCH").ok().as_deref() != Some("near") {
                        with(fit);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    bench_reset();
                    for i in 0..240 {
                        if i % 60 == 30 {
                            handle(FlMsg::Zoom(if i < 120 { 1.25 } else { 0.8 }));
                        } else {
                            handle(FlMsg::Pan(if (i / 60) % 2 == 0 { 6.0 } else { -6.0 }, 3.0));
                        }
                        std::thread::sleep(std::time::Duration::from_millis(16));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    eprintln!("[flow-bench] {}", bench_report());
                });
            }
            // 样张截图用：`WS_FLOW_QUEUE_LOG=<变体运行的事件日志>` 把一次跑过的变体放进队列（不重跑、不再计试验数）
            if let Ok(log) = std::env::var("WS_FLOW_QUEUE_LOG") {
                let mut job = Job { title: "（回放）".into(), status: "running".into(), ..Default::default() };
                for line in std::fs::read_to_string(&log).unwrap_or_default().lines() {
                    if let Ok(ev) = serde_json::from_str::<Value>(line) {
                        apply_queue_event(&mut job, &ev);
                    }
                }
                with(|g| {
                    g.queue.push(job);
                    g.queue_pick = Some(0);
                    g.debug_open = true;
                    g.debug_tab = DebugTab::Queue;
                });
            }
            // 样张截图用：`WS_FLOW_PROFILE=1` 一开始就叠显示耗时（配 WS_FLOW_RUN，跑完才有数）
            if std::env::var_os("WS_FLOW_PROFILE").is_some() {
                with(|g| g.profile_on = true);
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
    /// 不挡编译的提醒（表达式两边量纲不同之类）
    warnings: Vec<String>,
    needs: Needs,
    #[serde(rename = "where")]
    whence: HashMap<String, String>,
    toml: String,
    /// 节点实时数值的取值方式（docs/49 M2）
    #[serde(default)]
    probes: HashMap<String, Probe>,
}

fn apply_check(g: &mut St, r: Result<CheckOut, String>, what: &str) -> Option<String> {
    g.busy = false;
    match r {
        Ok(c) => {
            g.checked = Some(c.ok);
            g.issues = c.errors;
            g.warnings = c.warnings;
            g.needs = c.needs;
            if c.ok {
                g.whence = c.whence;
                g.probes = c.probes;
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
        write_breakpoints(&run_id); // 回测一起来就要读到，所以在起进程之前写
        let (args, roots, pace, auto_end) = with(|g| (run_overrides(&g.graph.backtest), root_env(&g.pick), g.pace.clone(), g.auto_end.take())).unwrap_or_default();
        let log = std::env::temp_dir().join(format!("ws-{run_id}.log"));
        let out = std::fs::File::create(&log).ok();
        let mut cmd = Command::new(super::paths::python());
        cmd.args(["-m", "strategies.swe.run", &toml, "--run-id", &run_id]);
        // 数据：图里 `[backtest]` 的每一项都作为覆盖传进去——界面上看到的就是实际跑的（docs/45 §21）
        cmd.args(&args);
        if let Some(e) = &auto_end {
            cmd.args(["--end", e]);
        }
        cmd.envs(roots);
        if pace.trim().parse::<f64>().map(|p| p > 0.0).unwrap_or(false) {
            cmd.args(["--pace", pace.trim()]);
        }
        let (start, end) = with(|g| data_range(&g.graph.backtest)).unwrap_or_default();
        let end = auto_end.unwrap_or(end);
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
                    g.values.clear();
                    g.costs = None;
                    g.envelope_hit = None;
                    g.events.clear();
                    g.trade_idx.clear();
                    g.why_not.clear();
                    g.picked_trade = None;
                    g.trace_path.clear();
                    g.path_nodes.clear();
                    g.paused = None;
                    g.generation += 1;
                    g.profile.clear();
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
        let (mut last_watch, mut last_seq) = ((Vec::new(), Vec::new()), -1i64);
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
            // 节点实时数值（docs/49 M2）：告诉宿主屏幕上有哪些节点，取回它推来的最新一份
            let watch = with(|g| watch_of(&g.graph, &g.probes, g.focus.as_deref(), g.pan, g.zoom, g.view_size)).unwrap_or_default();
            if watch != last_watch {
                let body = serde_json::json!({"exprs": watch.0, "features": watch.1}).to_string();
                let _: redis::RedisResult<()> = conn.set_ex(format!("ws:swe:{run_id}:watch"), body, 3600);
                last_watch = watch;
            }
            let vals: Option<String> = conn.get(format!("ws:swe:{run_id}:values")).ok().flatten();
            if let Some(got) = vals.and_then(|s| serde_json::from_str::<Value>(&s).ok()) {
                let seq = got.get("seq").and_then(Value::as_i64).unwrap_or(0);
                if seq != last_seq {
                    last_seq = seq;
                    with(|g| {
                        if g.generation == generation {
                            let probes = std::mem::take(&mut g.probes);
                            apply_values(&mut g.values, &probes, &got);
                            g.probes = probes;
                        }
                    });
                }
            }
            // 调试器停住时写的「停在哪」
            let state: Option<String> = conn.get(format!("ws:swe:{run_id}:state")).ok().flatten();
            let paused = state.and_then(|s| serde_json::from_str::<Value>(&s).ok());
            with(|g| {
                if g.generation == generation {
                    g.paused = paused;
                }
            });
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
                    if kind == "profile" {
                        // 运行结束时宿主报的各模块耗时（docs/45 C8）：按「线|模块」落到节点上，同一个节点的相加
                        g.profile = profile_by_node(&v, &g.whence);
                        let worst = g.profile.iter().max_by(|a, b| a.1.0.total_cmp(&b.1.0)).map(|(n, (ms, _))| (n.clone(), *ms));
                        if let Some((n, ms)) = worst {
                            g.msg = format!("耗时最多：{n}（{ms:.0} ms）；工具条「耗时」叠在节点上");
                        }
                        continue;
                    }
                    if kind == "costs" {
                        // 宿主结束时报的成本线（docs/49 M4）：毛 / 手续费 / 净（bp）与被动成交占比
                        g.costs = v.get("why").and_then(Value::as_object).cloned();
                        if let Some(c) = &g.costs {
                            g.msg = cost_line(c);
                        }
                        continue;
                    }
                    if kind == "why_not" {
                        if let Some(m) = v.get("why").and_then(Value::as_object) {
                            g.why_not.insert(lane.clone(), m.clone());
                        }
                        continue;
                    }
                    if g.events.len() < 50_000 {
                        if kind == "trade" {
                            g.trade_idx.push(g.events.len());
                        }
                        g.events.push(v.clone());
                    }
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
            g.paused = None;
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
        // 比这份代码新的图：打开了也认不全，存回去会丢东西——不开（docs/45 §3.7）
        let r = r.and_then(|gr| if gr.format > GRAPH_FORMAT { Err(format!("这张图是第 {} 版格式，这个 Cockpit 只认到第 {GRAPH_FORMAT} 版：先更新 Cockpit 再打开", gr.format)) } else { Ok(gr) });
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
    const NODE: (f32, f32) = (168.0, 120.0);
    let view = g.view_size;
    #[allow(non_snake_case)]
    let VIEW = (view.0.max(200.0), view.1.max(200.0));
    // 进了子图就只看它的成员
    let shown: Vec<&Node> = g.graph.nodes.iter().filter(|n| g.focus.is_none() || n.group == g.focus).collect();
    if shown.is_empty() {
        return;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for n in shown {
        x0 = x0.min(n.x);
        y0 = y0.min(n.y);
        x1 = x1.max(n.x + NODE.0);
        y1 = y1.max(n.y + NODE.1);
    }
    let zoom = (VIEW.0 / (x1 - x0).max(1.0)).min(VIEW.1 / (y1 - y0).max(1.0)).clamp(MIN_ZOOM, 1.0);
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
// ── 数据（docs/45 §21）：共用数据选择组件 ⇄ 图的 `[backtest]` ──────────────────────

/// 画布对数据选择组件的要求：计算用途、B2 已购数据 + B3 本地录制（单日）+ B5 本地自有数据、一段日期 + 每日时段（自有数据按整日）、列出数据类型；
/// 检查过的图带上它要的数据（D2）——给不了的数据商置灰、缺数据的日子标出、必需的类型锁住。
#[must_use]
pub fn pick_opts(needs: &Needs, pal: Option<&Palette>) -> PickOpts {
    let require = pal.filter(|_| !needs.kinds.is_empty()).map(|p| dp::Require {
        kinds: needs.kinds.iter().map(|k| (k.clone(), p.data_kinds.get(k).cloned().unwrap_or_else(|| Needs::label(k).to_string()))).collect(),
        provides: p.vendor_types.clone(),
    });
    PickOpts {
        purpose: Purpose::Compute,
        sources: Some(vec![BSource::Purchased, BSource::Recorded, BSource::Own]),
        vendors: None,
        time: TimeMode::Window,
        local_only: true,
        hide_types: false,
        range: true,
        require,
    }
}

/// 已购数据的数据源名（B2）；`[backtest] source` 是别的名字 = B5 本地自有数据的渠道（如 `dukascopy`）。
const PURCHASED: [&str; 3] = ["databento", "tardis", "auto"];

/// `[backtest]` 的数据源（没写 = `dukascopy`，与 `strategies/swe/run.py` 一致）。
fn bt_source(bt: &Value) -> &str {
    bt.get("source").and_then(Value::as_str).unwrap_or("dukascopy")
}

/// `[backtest]` 里写了的标的（字符串或列表取第一个）。
fn bt_symbol_written(bt: &Value) -> Option<String> {
    match bt.get("symbols") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Array(a)) => a.first().and_then(Value::as_str).map(str::to_string),
        _ => None,
    }
}

/// `[backtest]` 的标的；Dukascopy 没写标的 = XAUUSD（它只有这一个）。
fn bt_symbol(bt: &Value) -> Option<String> {
    bt_symbol_written(bt).or_else(|| (bt_source(bt) == "dukascopy").then(|| "XAUUSD".to_string()))
}

/// 图上画策略决策用（docs/46 G4）：这次运行的品种代码与带价格的成交事件（最近 500 条）。没跑过返回 `None`。
#[must_use]
pub fn chart_events() -> Option<(String, Vec<Value>)> {
    with(|g| {
        if g.events.is_empty() {
            return None;
        }
        let sym = bt_symbol(&g.graph.backtest)?;
        let fills: Vec<Value> = g
            .events
            .iter()
            .filter(|e| e.get("kind").and_then(Value::as_str) == Some("fill") && e.get("why").and_then(|w| w.get("price")).is_some())
            .cloned()
            .collect();
        let skip = fills.len().saturating_sub(500);
        Some((sym, fills.into_iter().skip(skip).collect()))
    })
    .flatten()
}

/// `[backtest]` 的起止日（没写结束日 = 起始那一天）。
#[must_use]
pub fn data_range(bt: &Value) -> (String, String) {
    let s = |k: &str| bt.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let start = s("start");
    let end = Some(s("end")).filter(|e| !e.is_empty()).unwrap_or_else(|| start.clone());
    (start, end)
}

/// 画布左上角数据牌上的一行：数据商 · 标的 · 日期（· 时段）（· 类型）。
#[must_use]
pub fn data_label(bt: &Value) -> String {
    let src = bt_source(bt);
    let (start, end) = data_range(bt);
    let vendor = if PURCHASED.contains(&src) || src == "recorder" { dp::source_label(src) } else { format!("自有数据 {src}") };
    let mut out = vec![vendor, bt_symbol(bt).unwrap_or_else(|| "?".into())];
    out.push(match (start.is_empty(), start == end) {
        (true, _) => "最新一天".into(),
        (false, true) => start,
        (false, false) => format!("{start} → {end}"),
    });
    if let Some(sess) = bt.get("session") {
        let st = sess.get("start").and_then(Value::as_str).unwrap_or("?");
        let m = sess.get("minutes").and_then(Value::as_u64).unwrap_or(0);
        out.push(format!("{st} 起 {m} 分钟"));
    }
    if let Some(t) = bt.get("types").and_then(Value::as_array).filter(|t| !t.is_empty()) {
        out.push(t.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("+"));
    }
    out.join(" · ")
}

/// `[backtest]` → `strategies.swe.run` 的覆盖参数（给了才传）。
#[must_use]
pub fn run_overrides(bt: &Value) -> Vec<String> {
    let s = |k: &str| bt.get(k).and_then(Value::as_str).filter(|v| !v.is_empty()).map(str::to_string);
    let sess = bt.get("session");
    let mut out = Vec::new();
    for (flag, v) in [
        ("--source", s("source")),
        ("--symbol", bt_symbol_written(bt)),
        ("--start", s("start")),
        ("--end", s("end")),
        ("--session-start", sess.and_then(|x| x.get("start")).and_then(Value::as_str).map(str::to_string)),
        ("--minutes", sess.and_then(|x| x.get("minutes")).and_then(Value::as_u64).map(|m| m.to_string())),
    ] {
        if let Some(v) = v {
            out.push(flag.to_string());
            out.push(v);
        }
    }
    out
}

/// 根目录不是缺省时，子进程要的环境变量（根目录与机器有关，不写进图）。
fn root_env(p: &DataPick) -> Vec<(String, String)> {
    let Some(k) = p.local_key() else { return Vec::new() };
    let root = p.root_or_default();
    let var = match k {
        "databento" => "WS_DATABENTO_ROOT",
        "tardis" => "WS_TARDIS_ROOT",
        "own" => "WS_OWN_ROOT", // 运行入口据此找到渠道目录（`datareq.own_channel_dir`）
        "recorder" => "WS_DATA_DIR",
        _ => return Vec::new(),
    };
    if root == dp::default_root(k) { Vec::new() } else { vec![(var.to_string(), root)] }
}

/// 图的 `[backtest]` → 选择器（打开图、撤销重做之后）。Tardis / Databento → B2；recorder → B3；其余数据源名
/// （没写 = dukascopy）是 B5 本地自有数据的渠道。`auto`：选择器从头选。同一个来源 / 数据商时保留手填的根目录。
fn sync_pick(g: &mut St) {
    let bt = &g.graph.backtest;
    let s = |k: &str| bt.get(k).and_then(Value::as_str);
    let mut p = DataPick::default();
    let src = bt_source(bt);
    if src != "auto"
        && let Some(sym) = bt_symbol(bt)
    {
        let sess = bt.get("session");
        let window = sess
            .and_then(|x| Some((x.get("start")?.as_str()?, u32::try_from(x.get("minutes")?.as_u64()?).ok()?)));
        if PURCHASED.contains(&src) {
            p.preset_local(BSource::Purchased, Some(src), &sym, s("start"), s("end"), window);
        } else if src == "recorder" {
            p.preset_local(BSource::Recorded, None, &sym, s("start"), s("end"), window);
        } else {
            p.preset_local(BSource::Own, None, &sym, s("start"), s("end"), None);
        }
        if g.pick.source == p.source && g.pick.vendor == p.vendor && !g.pick.root.trim().is_empty() {
            p.root = g.pick.root.clone();
        }
    }
    g.pick = p;
}

/// 选择器选完了 → 写回 `[backtest]`（其余项如手续费照留）。没变就不记撤销、不算改动。
fn write_pick(g: &mut St) {
    let Some(Selection::Local { source, symbol, date, end, window, types, channel, .. }) = g.pick.selection(&pick_opts(&g.needs, g.palette.as_ref())) else {
        return;
    };
    let mut bt = g.graph.backtest.as_object().cloned().unwrap_or_default();
    // B5 本地自有数据：数据源写渠道名（`dukascopy` …），运行入口按它挑回测脚本
    let source = if source == "own" { if channel.is_empty() { source } else { channel } } else { source };
    bt.insert("source".into(), source.into());
    bt.insert("symbols".into(), symbol.into());
    bt.insert("start".into(), date.into());
    bt.insert("end".into(), end.into());
    match window {
        Some((s, m)) => bt.insert("session".into(), serde_json::json!({"start": s, "minutes": m})),
        None => bt.remove("session"),
    };
    bt.insert("types".into(), types.into());
    let bt = Value::Object(bt);
    if bt != g.graph.backtest {
        edit(g, "c.backtest.data");
        g.graph.backtest = bt;
        g.form.retain(|k, _| !k.starts_with(&form_key("backtest", "")));
        touch(g);
    }
}

fn reset_editing(g: &mut St) {
    sync_pick(g);
    g.selected.clear();
    g.undo.clear();
    g.redo.clear();
    g.undo_tag = None;
    g.quick = None;
    g.tables.clear();
    g.envelope_hit = None;
    g.focus = None;
    g.find_i = 0;
    g.needs = Needs::default();
    // 打开就检查一次（不算改动）：问题清单、这张图要的数据（数据选择器按它置灰，docs/45 §21 D2）都要现成的
    g.touched = (g.graph.nodes.len() > 1).then(Instant::now);
    if g.touched.is_some() && !std::mem::replace(&mut g.auto_on, true) {
        auto_check();
    }
}

/// 撤销 / 重做之后：输入框里的原文作废（照图上的值重显），选择里去掉已经不在的节点。
fn after_swap(g: &mut St) {
    sync_pick(g);
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
        FlMsg::NewFrom(t) => {
            stop();
            open_template(t);
        }
        FlMsg::AssetId(s) => {
            with(|g| g.asset_id = s);
        }
        FlMsg::AssetMajor(b) => {
            with(|g| g.asset_major = b);
        }
        FlMsg::SaveAsset(gid) => save_asset(gid),
        FlMsg::Unlink(id) => unlink(id),
        FlMsg::Expose(id, param, on) => {
            with(|g| {
                record(g, "");
                g.dirty = true;
                if let Some(n) = g.graph.node_mut(&id) {
                    if on {
                        n.expose.insert(param.clone(), Value::String(param));
                    } else {
                        n.expose.remove(&param);
                    }
                }
            });
        }
        FlMsg::CodeKind(k) => {
            with(|g| g.code_kind = k);
        }
        FlMsg::CodeName(s) => {
            with(|g| g.code_name = s);
        }
        FlMsg::CodeNew => code_new(),
        FlMsg::Bypass(id, on) => {
            with(|g| {
                edit(g, "");
                if let Some(n) = g.graph.node_mut(&id) {
                    n.bypass = on;
                }
            });
        }
        FlMsg::LaneMute(id, on) => {
            with(|g| {
                edit(g, "");
                if let Some(l) = g.graph.node_mut(&id).and_then(|n| n.lane.as_mut()) {
                    if on {
                        l.insert("mute".into(), Value::Bool(true));
                    } else {
                        l.remove("mute");
                    }
                }
            });
        }
        FlMsg::VaryUse(id, refer, on) => {
            with(|g| {
                edit(g, "");
                let Some(n) = g.graph.node_mut(&id) else { return };
                let cur = n.module.clone();
                let mut list: Vec<Value> = n.vary.get("use").and_then(Value::as_array).cloned().unwrap_or_default();
                if list.is_empty() {
                    list.push(Value::String(cur)); // 现在用的那个总是候选之一
                }
                list.retain(|v| v.as_str() != Some(refer.as_str()));
                if on {
                    list.push(Value::String(refer));
                }
                n.vary.retain(|k, _| k == "use"); // 换模块与改参数不能标在同一个节点上
                if list.len() >= 2 {
                    n.vary.insert("use".into(), Value::Array(list));
                } else {
                    n.vary.remove("use");
                }
            });
        }
        FlMsg::VaryParam(id, param, text) => {
            with(|g| {
                let ty = g.graph.node(&id).and_then(|n| g.palette.as_ref().and_then(|p| p.get(&n.module))).and_then(|d| d.params.iter().find(|p| p.name == param)).map(|p| p.ty.clone()).unwrap_or_default();
                edit(g, &format!("v.{id}.{param}"));
                g.form.insert(form_key(&id, &format!("vary.{param}")), text.clone());
                let values: Vec<Value> = text.split([',', '，']).filter(|s| !s.trim().is_empty()).filter_map(|s| parse_value(s, &ty)).collect();
                if let Some(n) = g.graph.node_mut(&id) {
                    n.vary.remove("use");
                    if values.is_empty() {
                        n.vary.remove(&param);
                    } else {
                        n.vary.insert(param, Value::Array(values));
                    }
                }
            });
        }
        FlMsg::Enqueue => enqueue(),
        FlMsg::Drive(id, param, on) => {
            with(|g| {
                edit(g, "");
                if !on {
                    g.graph.edges.retain(|e| !(e.to == id && e.inp == param));
                }
                if let Some(n) = g.graph.node_mut(&id) {
                    n.driven.retain(|p| *p != param);
                    if on {
                        n.driven.push(param);
                    }
                }
            });
        }
        FlMsg::ProfileToggle => {
            with(|g| {
                g.profile_on = !g.profile_on;
                if g.profile_on && g.profile.is_empty() {
                    g.msg = "还没有耗时：在画布上运行一次（跑完时宿主报各模块的耗时）".into();
                }
            });
        }
        FlMsg::QueueCancel(i) => {
            with(|g| {
                let Some(j) = g.queue.get_mut(i) else { return };
                match j.status.as_str() {
                    "waiting" => j.status = "cancelled".into(),
                    "running" => {
                        if let Some(mut c) = g.queue_child.take() {
                            let _ = c.kill();
                            let _ = c.wait();
                        }
                        j.status = "cancelled".into();
                    }
                    _ => {}
                }
            });
            queue_kick();
        }
        FlMsg::QueuePick(i) => {
            with(|g| g.queue_pick = Some(i));
        }
        FlMsg::CodeTest => code_test(),
        FlMsg::SelectToggleMany(ids) => {
            with(|g| {
                if ids.iter().all(|i| g.selected.contains(i)) {
                    g.selected.retain(|s| !ids.contains(s));
                } else {
                    for id in ids {
                        if !g.selected.contains(&id) {
                            g.selected.push(id);
                        }
                    }
                }
            });
        }
        FlMsg::GroupSelected => {
            with(|g| {
                // 「执行」节点不进子图：它是全图唯一的出口
                let ids: Vec<String> = g.selected.iter().filter(|id| g.graph.node(id).is_some_and(|n| n.module != EXEC)).cloned().collect();
                if ids.len() < 2 {
                    g.msg = "先选中至少两个节点（Shift 拖空白框选），再编成子图".to_string();
                    return;
                }
                record(g, "");
                g.dirty = true;
                let n = g.graph.groups.len() + 1;
                g.graph.make_group(&ids, &format!("子图 {n}"));
                g.selected = ids;
            });
        }
        FlMsg::Ungroup(gid) => {
            with(|g| {
                record(g, "");
                g.dirty = true;
                g.graph.dissolve(&gid);
                if g.focus.as_deref() == Some(gid.as_str()) {
                    g.focus = None;
                }
            });
        }
        FlMsg::ToggleCollapse(gid) => {
            with(|g| {
                record(g, "");
                g.dirty = true;
                if let Some(x) = g.graph.groups.iter_mut().find(|x| x.id == gid) {
                    x.collapsed = !x.collapsed;
                }
            });
        }
        FlMsg::GroupTitle(gid, text) => {
            with(|g| {
                record(g, &format!("g.{gid}"));
                g.dirty = true;
                if let Some(x) = g.graph.groups.iter_mut().find(|x| x.id == gid) {
                    x.title = text;
                }
            });
        }
        FlMsg::Focus(gid) => {
            with(|g| {
                g.focus = gid.filter(|id| g.graph.group(id).is_some());
                g.quick = None;
                fit(g);
            });
        }
        FlMsg::Wireless(id, on) => {
            with(|g| {
                record(g, "");
                g.dirty = true;
                if let Some(n) = g.graph.node_mut(&id) {
                    n.wireless = on;
                }
            });
        }
        FlMsg::Viewport(w, h) => {
            with(|g| g.view_size = (w, h));
        }
        FlMsg::SetPan(x, y) => {
            with(|g| g.pan = (x, y));
        }
        FlMsg::FindQuery(s) => {
            with(|g| {
                g.find = s;
                g.find_i = 0;
            });
        }
        FlMsg::FindNext => {
            with(|g| {
                let hits = g.graph.find(&g.find, g.palette.as_ref());
                if hits.is_empty() {
                    g.msg = if g.find.trim().is_empty() { String::new() } else { format!("没找到「{}」", g.find.trim()) };
                    return;
                }
                g.find_i = g.find_i % hits.len() + 1;
                let id = hits[g.find_i - 1].clone();
                let Some(n) = g.graph.node(&id).cloned() else { return };
                // 在折叠的子图里：进去看；在别的子图里看着：换过去
                let collapsed = n.group.as_ref().is_some_and(|gid| g.graph.group(gid).is_some_and(|x| x.collapsed));
                if g.focus != n.group && (g.focus.is_some() || collapsed) {
                    g.focus = if collapsed || g.focus.is_some() { n.group.clone() } else { None };
                }
                g.selected = vec![id];
                g.zoom = g.zoom.max(0.8);
                g.pan = (g.view_size.0 / 2.0 - (n.x + 84.0) * g.zoom, g.view_size.1 / 2.0 - (n.y + 40.0) * g.zoom);
            });
        }
        FlMsg::Tidy => tidy_async(),
        FlMsg::LibToggle(label) => {
            with(|g| {
                if let Some(i) = g.lib_open.iter().position(|x| *x == label) {
                    g.lib_open.remove(i);
                } else {
                    g.lib_open.push(label);
                }
            });
        }
        FlMsg::DebugToggle => {
            with(|g| g.debug_open = !g.debug_open);
        }
        FlMsg::DebugTab(t) => {
            with(|g| g.debug_tab = t);
        }
        FlMsg::PickTrade(i) => {
            with(|g| {
                if g.picked_trade == Some(i) {
                    g.picked_trade = None;
                    g.trace_path.clear();
                    g.path_nodes.clear();
                    return;
                }
                let (steps, nodes) = trade_path(&g.events, i, &g.whence);
                g.picked_trade = Some(i);
                g.trace_path = steps;
                g.path_nodes = nodes;
            });
        }
        FlMsg::Breakpoint(id, on) => {
            let run = with(|g| {
                g.breakpoints.retain(|b| *b != id);
                if on {
                    g.breakpoints.push(id);
                }
                g.running.then(|| g.run_id.clone())
            })
            .flatten();
            if let Some(run_id) = run {
                write_breakpoints(&run_id);
            }
        }
        FlMsg::Ctl(what) => {
            let Some(run_id) = with(|g| g.running.then(|| g.run_id.clone())).flatten() else { return };
            super::spawn_named("ws-flowctl", move || {
                use redis::Commands;
                let r = redis::Client::open(redis_url()).and_then(|c| c.get_connection()).and_then(|mut c| c.set::<_, _, ()>(format!("ws:swe:{run_id}:ctl"), &what));
                if let Err(e) = r {
                    with(|g| g.msg = format!("发不出运行控制：{e}"));
                }
            });
        }
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
        FlMsg::Data(m) => {
            with(|g| {
                g.pick.update(m);
                write_pick(g);
            });
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
                let inp = g.palette.as_ref().and_then(|p| p.get(&refer)).and_then(|d| d.first_in(&q.ty).cloned()).unwrap_or_else(|| q.port.clone());
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
            with(|g| g.zoom = (g.zoom * f).clamp(MIN_ZOOM, 2.0));
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

/// 从模板新建：Python 把模板升到当前格式、换成草稿 id（`graph template <文件>`）。
fn open_template(t: Template) {
    if with(|g| std::mem::replace(&mut g.busy, true)).unwrap_or(true) {
        return;
    }
    super::spawn_named("ws-flowtpl", move || {
        let r: Result<Graph, String> = python(&["-m", "strategies.swe.graph", "template", &t.path], None).and_then(|b| serde_json::from_slice(&b).map_err(|e| format!("模板解析失败：{e}")));
        with(|g| {
            g.busy = false;
            match r {
                Ok(graph) => {
                    g.graph = graph;
                    g.path = None;
                    g.dirty = true;
                    fit(g);
                    reset_editing(g);
                    g.form.clear();
                    g.issues.clear();
                    g.whence.clear();
                    g.checked = None;
                    g.live.clear();
                    g.live_n = 0;
                    g.msg = format!("从模板「{}」新建（只是起点，与模板不保持关联；改个 id 再保存）", t.title);
                    touch(g);
                }
                Err(e) => g.msg = e,
            }
        });
    });
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SubOut {
    ok: bool,
    error: String,
    graph: Option<Graph>,
    #[serde(rename = "ref")]
    refer: String,
    version: String,
    affected: Vec<String>,
}

/// 把一个分组存成子图资产（`subgraph extract -`）。图里那组节点换成一个引用它的节点；
/// 同 id 的资产已经有了：接口没变就升小版本（所有引用它的图一起变），接口变了要勾「升大版本」。
fn save_asset(gid: String) {
    let Some(req) = with(|g| {
        if g.busy {
            return None;
        }
        let grp = g.graph.group(&gid)?.clone();
        let from_asset = grp.asset.as_deref().and_then(|a| a.strip_prefix("sub.")).and_then(|a| a.split_once('@')).map(|(id, _)| id.to_string());
        let id = if g.asset_id.trim().is_empty() { from_asset.unwrap_or_default() } else { g.asset_id.trim().to_string() };
        if id.is_empty() {
            g.msg = "先给子图资产起个 id（英文字母、数字、下划线）".into();
            return None;
        }
        g.busy = true;
        Some(serde_json::json!({"graph": g.graph, "group": gid, "id": id, "name": grp.title, "major": g.asset_major, "path": g.path}))
    })
    .flatten() else {
        return;
    };
    super::spawn_named("ws-flowasset", move || {
        let body = serde_json::to_vec(&req).unwrap_or_default();
        let r = python(&["-m", "strategies.swe.subgraph", "extract", "-"], Some(&body)).and_then(|b| serde_json::from_slice::<SubOut>(&b).map_err(|e| format!("结果解析失败：{e}")));
        with(|g| {
            g.busy = false;
            match r {
                Ok(o) if o.ok && o.graph.is_some() => {
                    record(g, "");
                    g.graph = o.graph.unwrap_or_default();
                    g.selected.clear();
                    g.focus = None;
                    g.asset_id.clear();
                    g.asset_major = false;
                    let also = if o.affected.is_empty() { String::new() } else { format!("；同时生效于：{}", o.affected.join("、")) };
                    g.msg = format!("已存成子图资产 {}（版本 {}）{also}", o.refer, o.version);
                    touch(g);
                }
                Ok(o) => g.msg = format!("没存成：{}", o.error),
                Err(e) => g.msg = format!("没存成：{e}"),
            }
        });
        load_palette();
    });
}

/// 解除关联（`subgraph unlink -`）：实例展开成一个普通分组，以后不再跟着资产变。
fn unlink(id: String) {
    let Some(req) = with(|g| {
        if g.busy {
            return None;
        }
        g.busy = true;
        Some(serde_json::json!({"graph": g.graph, "node": id}))
    })
    .flatten() else {
        return;
    };
    super::spawn_named("ws-flowunlink", move || {
        let body = serde_json::to_vec(&req).unwrap_or_default();
        let r = python(&["-m", "strategies.swe.subgraph", "unlink", "-"], Some(&body)).and_then(|b| serde_json::from_slice::<SubOut>(&b).map_err(|e| format!("结果解析失败：{e}")));
        with(|g| {
            g.busy = false;
            match r {
                Ok(o) if o.ok && o.graph.is_some() => {
                    edit(g, "");
                    g.graph = o.graph.unwrap_or_default();
                    g.selected.clear();
                    g.msg = "已解除关联：展开成了一个分组，以后各改各的；改完可以在分组上「存回资产」".into();
                }
                Ok(o) => g.msg = format!("解除关联失败：{}", o.error),
                Err(e) => g.msg = format!("解除关联失败：{e}"),
            }
        });
    });
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CodeOut {
    ok: bool,
    error: String,
    #[serde(rename = "ref")]
    refer: String,
    path: String,
    abs: String,
    problems: Vec<String>,
}

/// 代码节点：生成一个带标准接口的模块文件（`codegen new`），发给 Studio 打开。
fn code_new() {
    let Some((kind, name)) = with(|g| (g.code_kind.clone(), g.code_name.trim().to_string())) else { return };
    super::spawn_named("ws-flowcode", move || {
        let r = python(&["-m", "strategies.swe.codegen", "new", &kind, &name], None).and_then(|b| serde_json::from_slice::<CodeOut>(&b).map_err(|e| format!("结果解析失败：{e}")));
        match r {
            Ok(o) if o.ok => {
                let sent = wealthspring_ui_tokens::bridge::send(
                    wealthspring_ui_tokens::bridge::Peer::Studio,
                    &wealthspring_ui_tokens::bridge::UiCommand::OpenFile { path: o.abs.clone(), line: None },
                );
                with(|g| {
                    g.code_made = Some((o.refer.clone(), o.path.clone()));
                    g.code_problems = None;
                    g.code_name.clear();
                    g.msg = match sent {
                        Ok(_) => format!("已生成 {}，在 Studio 里打开了；改完点「契约测试」", o.path),
                        Err(e) => format!("已生成 {}（Studio 没打开：{e}）；改完点「契约测试」", o.path),
                    };
                });
            }
            Ok(o) => {
                with(|g| g.msg = format!("没生成：{}", o.error));
            }
            Err(e) => {
                with(|g| g.msg = format!("没生成：{e}"));
            }
        }
    });
}

/// 代码节点的契约测试（`codegen test`，与仓库测试里对全部模块跑的是同一份）；过了就刷新模块库。
fn code_test() {
    let Some(refer) = with(|g| g.code_made.as_ref().map(|c| c.0.clone())).flatten() else { return };
    super::spawn_named("ws-flowcodetest", move || {
        let r = python(&["-m", "strategies.swe.codegen", "test", &refer], None).and_then(|b| serde_json::from_slice::<CodeOut>(&b).map_err(|e| format!("结果解析失败：{e}")));
        let ok = matches!(&r, Ok(o) if o.ok);
        with(|g| {
            match r {
                Ok(o) => {
                    g.code_problems = Some(o.problems);
                    g.msg = if o.ok { format!("{refer} 过了契约测试，已进模块库") } else { format!("{refer} 没过契约测试（问题列在模块库下方）") };
                }
                Err(e) => g.msg = format!("契约测试没跑起来：{e}"),
            }
        });
        if ok {
            load_palette();
        }
    });
}

/// 排进运行队列：先存图（变体按存下的工作流跑），再记一项；队列空着就马上开始。
fn enqueue() {
    super::spawn_named("ws-flowenq", || {
        if with(|g| g.busy).unwrap_or(true) {
            return;
        }
        if let Err(e) = save_blocking() {
            with(|g| g.msg = format!("没排进队列：{e}"));
            return;
        }
        with(|g| {
            let n_vary: usize = g.graph.nodes.iter().filter(|n| !n.vary.is_empty()).count();
            let (start, end) = data_range(&g.graph.backtest);
            let job = Job {
                title: format!("{}（{start} → {end}）", g.graph.name),
                path: g.path.clone().unwrap_or_default(),
                start,
                end,
                roots: root_env(&g.pick),
                status: "waiting".into(),
                ..Default::default()
            };
            g.queue.push(job);
            g.queue_pick = Some(g.queue.len() - 1);
            g.debug_open = true;
            g.debug_tab = DebugTab::Queue;
            g.msg = if n_vary == 0 { "排进队列：没标待比较，就跑一次（也计入全局试验数）".into() } else { format!("排进队列：{n_vary} 个节点标了待比较") };
        });
        queue_kick();
    });
}

/// 队列空闲时开始下一项（`python -m strategies.swe.variants run`，一行一个 JSON 事件）。
fn queue_kick() {
    let next = with(|g| {
        if g.queue.iter().any(|j| j.status == "running") {
            return None;
        }
        let i = g.queue.iter().position(|j| j.status == "waiting")?;
        g.queue[i].status = "running".into();
        Some((i, g.queue[i].clone()))
    })
    .flatten();
    let Some((i, job)) = next else { return };
    super::spawn_named("ws-flowqueue", move || {
        use std::io::BufRead;
        let mut cmd = Command::new(super::paths::python());
        cmd.args(["-m", "strategies.swe.variants", "run", &job.path, "--workers", "2"]); // 起止日用图里 `[backtest]` 的
        cmd.envs(job.roots.clone());
        cmd.current_dir(super::paths::repo_root()).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                with(|g| {
                    if let Some(j) = g.queue.get_mut(i) {
                        j.status = "failed".into();
                        j.error = format!("起不来：{e}");
                    }
                });
                return queue_kick();
            }
        };
        let out = child.stdout.take();
        with(|g| g.queue_child = Some(child));
        if let Some(out) = out {
            for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                let Ok(ev) = serde_json::from_str::<Value>(&line) else { continue };
                with(|g| {
                    if let Some(j) = g.queue.get_mut(i) {
                        apply_queue_event(j, &ev);
                    }
                });
            }
        }
        with(|g| {
            if let Some(mut c) = g.queue_child.take() {
                let _ = c.wait();
            }
            if let Some(j) = g.queue.get_mut(i)
                && j.status == "running"
            {
                j.status = "failed".into();
                if j.error.is_empty() {
                    j.error = "进程没跑完就退出了".into();
                }
            }
        });
        queue_kick();
    });
}

/// 宿主报的耗时（{"线|模块": {"ms", "calls"}}）→ 每个节点的（毫秒, 次数）。
pub fn profile_by_node(ev: &Value, whence: &HashMap<String, String>) -> HashMap<String, (f64, u64)> {
    let mut out: HashMap<String, (f64, u64)> = HashMap::new();
    if let Some(m) = ev.get("why").and_then(Value::as_object) {
        for (key, x) in m {
            let (lane, module) = key.split_once('|').unwrap_or(("", key));
            if let Some(node) = locate(whence, lane, module) {
                let e = out.entry(node).or_default();
                e.0 += x.get("ms").and_then(Value::as_f64).unwrap_or(0.0);
                e.1 += x.get("calls").and_then(Value::as_u64).unwrap_or(0);
            }
        }
    }
    out
}

/// 一行事件 → 队列项。
pub fn apply_queue_event(j: &mut Job, ev: &Value) {
    let s = |k: &str| ev.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    match s("event").as_str() {
        "start" => {
            j.study_id = s("study_id");
            j.labels = ev.get("labels").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
            j.marks = ev.get("marks").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
        }
        "done" | "failed" => j.rows.push(ev.clone()),
        "end" => {
            if let Some(rows) = ev.get("rows").and_then(Value::as_array) {
                j.rows = rows.clone(); // 按组的顺序（跑的时候是谁先完谁先到）
            }
            j.curves = ev.get("curves").cloned().filter(|c| !c.is_null());
            j.global_trials = ev.get("global_trials").and_then(Value::as_i64);
            j.status = "done".into();
        }
        "error" => {
            j.status = "failed".into();
            j.error = s("error");
        }
        _ => {}
    }
}

/// 性能自测的计时（视图构建 / 绘制，毫秒）。只在 `WS_FLOW_BENCH` 时有人看。
fn bench_cell() -> &'static Mutex<(Vec<f32>, Vec<f32>)> {
    static C: OnceLock<Mutex<(Vec<f32>, Vec<f32>)>> = OnceLock::new();
    C.get_or_init(|| Mutex::new((Vec::new(), Vec::new())))
}

pub fn bench_note(view_ms: Option<f32>, draw_ms: Option<f32>) {
    if let Ok(mut c) = bench_cell().lock() {
        if c.0.len() < 100_000 {
            c.0.extend(view_ms);
            c.1.extend(draw_ms);
        }
    }
}

fn bench_reset() {
    if let Ok(mut c) = bench_cell().lock() {
        c.0.clear();
        c.1.clear();
    }
}

fn bench_report() -> String {
    let q = |v: &mut Vec<f32>| -> String {
        if v.is_empty() {
            return "无".into();
        }
        v.sort_by(f32::total_cmp);
        let at = |p: f32| v[((v.len() - 1) as f32 * p) as usize];
        format!("{} 次 中位 {:.2} / p95 {:.2} / 最大 {:.2} ms", v.len(), at(0.5), at(0.95), v[v.len() - 1])
    };
    let n = with(|g| g.graph.nodes.len()).unwrap_or(0);
    bench_cell().lock().map(|mut c| format!("{n} 个节点；视图构建 {}；绘制 {}", q(&mut c.0), q(&mut c.1))).unwrap_or_default()
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

/// 断点（节点）→ 调试器认的 `[{lane, module}]`，写到 `ws:swe:<运行号>:bp`。节点经轨迹定位表反查出线名与模块 id。
fn breakpoint_specs(g: &St) -> Vec<Value> {
    let mut out = Vec::new();
    for (key, nid) in &g.whence {
        if !g.breakpoints.contains(nid) {
            continue;
        }
        let (lane, module) = key.split_once('|').unwrap_or(("", key));
        if module.is_empty() {
            continue; // 「线|」这个兜底键指向入场节点；入场模块本身另有一条
        }
        out.push(serde_json::json!({"lane": lane, "module": module}));
    }
    out
}

fn write_breakpoints(run_id: &str) {
    let Some(specs) = with(|g| breakpoint_specs(g)) else { return };
    let key = format!("ws:swe:{run_id}:bp");
    super::spawn_named("ws-flowbp", move || {
        use redis::Commands;
        if let Ok(mut c) = redis::Client::open(redis_url()).and_then(|c| c.get_connection()) {
            let _: Result<(), _> = c.set(&key, serde_json::to_string(&specs).unwrap_or_default());
            let _: Result<(), _> = c.expire(&key, 86_400);
        }
    });
}

/// 自动整理：位置由 Python 算（`graph layout`，排版只有一份），这里只是把结果放回去。
fn tidy_async() {
    let Some((graph, ids)) = with(|g| {
        if g.busy {
            return None;
        }
        g.busy = true;
        // 选中了就只整理选中的；进了子图就整理它的成员；否则全图
        let ids: Option<Vec<String>> = if g.selected.len() > 1 { Some(g.selected.clone()) } else { g.focus.as_ref().map(|f| g.graph.members(f)) };
        Some((g.graph.clone(), ids))
    })
    .flatten() else {
        return;
    };
    super::spawn_named("ws-flowtidy", move || {
        let body = serde_json::to_vec(&serde_json::json!({"graph": graph, "ids": ids})).unwrap_or_default();
        let r = python(&["-m", "strategies.swe.graph", "layout", "-"], Some(&body))
            .and_then(|b| serde_json::from_slice::<HashMap<String, (f32, f32)>>(&b).map_err(|e| format!("整理结果解析失败：{e}")));
        with(|g| {
            g.busy = false;
            match r {
                Ok(pos) => {
                    record(g, "");
                    g.dirty = true;
                    for n in g.graph.nodes.iter_mut() {
                        if let Some((x, y)) = pos.get(&n.id) {
                            n.x = *x;
                            n.y = *y;
                        }
                    }
                    if ids.is_none() {
                        fit(g);
                    }
                    g.msg = format!("整理了 {} 个节点（Ctrl Z 撤销）", pos.len());
                }
                Err(e) => g.msg = format!("整理失败：{e}"),
            }
        });
    });
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
    fn parameter_references_are_typed_as_eq_node_dot_param() {
        // docs/45 §4.3 第 5 条：`=节点.参数` 存成 {"$ref": …}，显示时还原；不像引用的「=」照常当文字
        let r = parse_value("= n5.sl", "number").unwrap();
        assert_eq!(r, serde_json::json!({"$ref": "n5.sl"}));
        assert_eq!(param_ref(&r), Some("n5.sl"));
        assert_eq!(value_text(&r), "=n5.sl");
        assert_eq!(parse_value("=oops", "string"), Some(Value::String("=oops".into())));
        assert_eq!(param_ref(&serde_json::json!({"$ref": "a.b", "x": 1})), None);
    }

    #[test]
    fn format_4_fields_survive_a_save_and_stay_out_of_old_graphs() {
        let mut g = Graph::blank();
        assert_eq!(g.format, GRAPH_FORMAT);
        g.nodes.push(Node { id: "n2".into(), module: "sub.reaper_trail@1".into(), ..Default::default() });
        let plain = serde_json::to_value(&g).unwrap();
        assert!(plain["nodes"][1].get("expose").is_none()); // 空的不写
        g.nodes[1].expose.insert("distance".into(), Value::String("trail_distance".into()));
        g.groups.push(Group { id: "g1".into(), title: "t".into(), collapsed: false, asset: Some("sub.reaper_trail@1".into()) });
        let back: Graph = serde_json::from_value(serde_json::to_value(&g).unwrap()).unwrap();
        assert_eq!(back, g);
    }

    #[test]
    fn bypass_and_vary_are_saved_only_when_set() {
        let mut n = Node { id: "n1".into(), module: "stop.fixed@1.0".into(), ..Default::default() };
        let v = serde_json::to_value(&n).unwrap();
        assert!(v.get("bypass").is_none() && v.get("vary").is_none());
        n.bypass = true;
        n.vary.insert("sl".into(), serde_json::json!([20, 30]));
        let v = serde_json::to_value(&n).unwrap();
        assert_eq!(v["bypass"], Value::Bool(true));
        assert_eq!(v["vary"]["sl"], serde_json::json!([20, 30]));
    }

    #[test]
    fn profile_lands_on_nodes_and_sums_modules_sharing_one() {
        let mut whence = HashMap::new();
        whence.insert("S1|stop.fixed".to_string(), "n5".to_string());
        whence.insert("S1|exit.trailing_stop".to_string(), "n9".to_string());
        whence.insert("S1|exit.breakeven".to_string(), "n9".to_string()); // 子图实例：里面几个模块都落在它上
        let ev = serde_json::json!({"kind": "profile", "why": {"S1|stop.fixed": {"ms": 1.5, "calls": 3}, "S1|exit.trailing_stop": {"ms": 2.0, "calls": 10}, "S1|exit.breakeven": {"ms": 1.0, "calls": 10}}});
        let p = profile_by_node(&ev, &whence);
        assert_eq!(p.get("n5"), Some(&(1.5, 3)));
        assert_eq!(p.get("n9"), Some(&(3.0, 20)));
    }

    #[test]
    fn unknown_fields_survive_a_round_trip() {
        let src = serde_json::json!({"format": 6, "id": "a.b", "assets": {"sub.x@1": {"id": "x"}}, "future": 1,
            "nodes": [{"id": "n1", "use": "kernel.exec", "x": 0.0, "y": 0.0, "params": {}, "later_field": [1, 2]}]});
        let g: Graph = serde_json::from_value(src).unwrap();
        let back = serde_json::to_value(&g).unwrap();
        assert_eq!(back["assets"]["sub.x@1"]["id"], "x");
        assert_eq!(back["future"], 1);
        assert_eq!(back["nodes"][0]["later_field"], serde_json::json!([1, 2]));
    }

    #[test]
    fn driven_params_are_saved_only_when_set() {
        let mut n = Node { id: "n1".into(), module: "stop.fixed@1.0".into(), ..Default::default() };
        assert!(serde_json::to_value(&n).unwrap().get("driven").is_none());
        n.driven.push("sl".into());
        assert_eq!(serde_json::to_value(&n).unwrap()["driven"], serde_json::json!(["sl"]));
    }

    #[test]
    fn queue_events_fill_a_job_and_end_in_order() {
        let mut j = Job { status: "running".into(), ..Default::default() };
        apply_queue_event(&mut j, &serde_json::json!({"event": "start", "study_id": "S-1", "n": 2, "labels": ["a", "b"], "marks": ["旁路 x"]}));
        assert_eq!((j.study_id.as_str(), j.labels.len(), j.marks.len()), ("S-1", 2, 1));
        apply_queue_event(&mut j, &serde_json::json!({"event": "done", "i": 1, "label": "b", "metrics": {"pnl": 2.0}}));
        assert_eq!((j.n_finished(), j.status.as_str()), (1, "running"));
        let rows = serde_json::json!([{"event": "done", "i": 0, "label": "a", "metrics": {"pnl": 1.0}}, {"event": "done", "i": 1, "label": "b", "metrics": {"pnl": 2.0}}]);
        apply_queue_event(&mut j, &serde_json::json!({"event": "end", "rows": rows, "curves": null, "global_trials": 7}));
        assert_eq!((j.rows.len(), j.status.as_str(), j.global_trials, j.curves.is_none()), (2, "done", Some(7), true));
        let mut k = Job::default();
        apply_queue_event(&mut k, &serde_json::json!({"event": "error", "error": "先保存"}));
        assert_eq!((k.status.as_str(), k.error.as_str()), ("failed", "先保存"));
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
        let m = |title: &str, kind: &str, ins: &[&str]| ModDef { refer: format!("{kind}.{title}@1"), title: title.into(), kind: kind.into(), kind_label: kind.into(), ins: ins.iter().map(|s| s.to_string()).collect(), in_types: ins.iter().map(|s| port_type(s).to_string()).collect(), ..Default::default() };
        let pal = Palette { modules: vec![m("calendar", "confirm", &["signal"]), m("stop_ladder", "entry", &["signal", "stop"]), m("breakeven", "exit", &["position"])], ..Default::default() };
        let mut q = Quick { port: "signal".into(), ty: "signal".into(), ..Default::default() };
        assert_eq!(q.candidates(&pal).iter().map(|m| m.title.as_str()).collect::<Vec<_>>(), ["calendar", "stop_ladder"]);
        q.query = "LADDER".into();
        assert_eq!(q.candidates(&pal).len(), 1);
        q.ty = "regime".into();
        q.query.clear();
        assert!(q.candidates(&pal).is_empty());
        // 特征输入端口以参数名为名，按类型匹配
        let pal = Palette { modules: vec![m("vol_unit", "sizing", &["n_feature"]), m("threshold", "setup", &["feature"])], ..Default::default() };
        q.ty = "feature".into();
        assert_eq!(q.candidates(&pal).len(), 2);
        // 数值节点：端口叫 a / b，类型由模块库给
        let v = ModDef { ins: vec!["a".into(), "b".into()], in_types: vec!["flag".into(), "series".into()], outs: vec!["out".into()], out_types: vec!["flag".into()], ..Default::default() };
        assert_eq!((v.in_type("a"), v.in_type("b"), v.out_type("out")), ("flag", "series", "flag"));
        assert_eq!(v.first_in("series").map(String::as_str), Some("b"));
        assert_eq!((port_type("n_feature"), port_type("feature"), port_type("signal")), ("feature", "feature", "signal"));
    }

    #[test]
    fn a_trade_traces_back_to_its_signal_through_the_same_lane_only() {
        let w: HashMap<String, String> = [("S1|setup.rule", "n2"), ("S1|entry.market_single", "n3"), ("S1|", "n3"), ("S1|exit.rule", "n4"), ("S2|", "n9")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let ev: Vec<Value> = [
            r#"{"ts": 1, "lane": "S1", "kind": "trade", "module": "", "reason": "old"}"#,
            r#"{"ts": 2, "lane": "S1", "kind": "intent", "module": "entry.market_single", "reason": "买 市价", "trail": [["setup.rule", "1.0", {"rule": "z > 1", "values": {"z": 1.7}}], ["entry.market_single", "1.0", {}]]}"#,
            r#"{"ts": 3, "lane": "S2", "kind": "fill", "module": "", "reason": "开仓"}"#,
            r#"{"ts": 4, "lane": "S1", "kind": "fill", "module": "", "reason": "第 1 笔", "why": {"price": 100.5}}"#,
            r#"{"ts": 5, "lane": "S1", "kind": "why_not", "module": "", "why": {"检查了": 9}}"#,
            r#"{"ts": 6, "lane": "S1", "kind": "close", "module": "exit.rule", "reason": "rule", "why": {"held_secs": 31.0}}"#,
            r#"{"ts": 7, "lane": "S1", "kind": "trade", "module": "", "reason": "rule", "why": {"gross": 12.5}}"#,
        ]
        .iter()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
        let (steps, nodes) = trade_path(&ev, 6, &w);
        // 从信号（带着它的依据）到平仓；别的线、上一笔、计数事件都不在里面
        assert_eq!(steps.iter().map(|s| s.kind.as_str()).collect::<Vec<_>>(), ["origin", "intent", "fill", "close", "trade"]);
        assert_eq!(steps[0].module, "setup.rule");
        assert!(steps[0].why.contains("z = 1.7") && steps[0].why.contains("rule = z > 1"));
        assert_eq!(nodes, ["n2", "n3", "n4"]);
        assert_eq!(ts_text(1_788_270_111_200_000_000), "09-01 13:41:51");
        assert!(trade_path(&ev, 99, &w).0.is_empty());
    }

    #[test]
    fn groups_are_display_only_bookkeeping() {
        let mut g = Graph::blank();
        for (i, m) in ["setup.a@1", "entry.b@1", "exit.c@1"].iter().enumerate() {
            g.nodes.push(Node { id: format!("n{}", i + 2), module: m.to_string(), ..Default::default() });
        }
        let gid = g.make_group(&["n2".into(), "n3".into()], "线 S1").unwrap();
        assert_eq!((gid.as_str(), g.members(&gid)), ("g1", vec!["n2".to_string(), "n3".to_string()]));
        // 重新编组：节点换到新的子图，空了的旧子图自动去掉
        let g2 = g.make_group(&["n2".into(), "n3".into(), "n4".into()], "大的").unwrap();
        assert_eq!(g.groups.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), [g2.as_str()]);
        g.remove_node("n4");
        assert_eq!(g.members(&g2).len(), 2);
        // 贴出来的节点不带分组
        let clip = g.excerpt(&["n2".into(), "n3".into()]);
        let new = g.paste(&clip, (0.0, 0.0));
        assert!(new.iter().all(|id| g.node(id).unwrap().group.is_none()));
        g.dissolve(&g2);
        assert!(g.groups.is_empty() && g.nodes.iter().all(|n| n.group.is_none()));
        // 没有成员的子图不落盘；分组与无线标记不写进没用到它们的节点
        let v = serde_json::to_value(&g).unwrap();
        assert!(v["nodes"][0].get("group").is_none() && v["nodes"][0].get("wireless").is_none());
    }

    #[test]
    fn search_matches_module_title_lane_and_parameter_values() {
        let mut g = Graph::blank();
        let lane: Map<String, Value> = [("name".to_string(), Value::String("S7".into()))].into_iter().collect();
        g.nodes.push(Node { id: "n2".into(), module: "entry.stop_ladder@1.0".into(), lane: Some(lane), ..Default::default() });
        g.nodes.push(Node { id: "n3".into(), module: "value.ema".into(), params: [("n".to_string(), Value::from(200))].into_iter().collect(), ..Default::default() });
        g.nodes.push(Node { id: "n4".into(), module: "regime.er@1".into(), name: Some("Trend".into()), ..Default::default() });
        assert_eq!(g.find("LADDER", None), ["n2"]);
        assert_eq!(g.find("s7", None), ["n2"]);
        assert_eq!(g.find("n=200", None), ["n3"]);
        assert_eq!(g.find("trend", None), ["n4"]);
        assert!(g.find("  ", None).is_empty() && g.find("zzz", None).is_empty());
    }

    #[test]
    fn backtest_section_becomes_run_overrides_and_badge() {
        let bt = serde_json::json!({"source": "databento", "symbols": "ESU6", "start": "2026-09-01", "end": "2026-09-01",
            "session": {"start": "13:30", "minutes": 30}, "types": ["mbo", "trades"], "maker_bp": 1.0});
        assert_eq!(
            run_overrides(&bt),
            ["--source", "databento", "--symbol", "ESU6", "--start", "2026-09-01", "--end", "2026-09-01", "--session-start", "13:30", "--minutes", "30"]
        );
        assert_eq!(data_label(&bt), "Databento · ESU6 · 2026-09-01 · 13:30 起 30 分钟 · mbo+trades");
        let gold = serde_json::json!({"source": "dukascopy", "symbols": ["XAUUSD"], "start": "2026-06-01", "end": "2026-06-30"});
        assert_eq!(data_label(&gold), "自有数据 dukascopy · XAUUSD · 2026-06-01 → 2026-06-30");
        assert_eq!(data_label(&serde_json::json!({})), "自有数据 dukascopy · XAUUSD · 最新一天", "没写数据源 = Dukascopy 黄金");
        assert_eq!(data_range(&serde_json::json!({"start": "2026-06-01"})), ("2026-06-01".into(), "2026-06-01".into()));
        assert!(run_overrides(&serde_json::json!({})).is_empty(), "没写的项不传，按工作流里的");
    }

    #[test]
    fn picker_round_trips_through_the_graph_backtest() {
        let mut g = St::default();
        g.graph.backtest = serde_json::json!({"source": "databento", "symbols": "ESU6", "start": "2026-09-01", "session": {"start": "13:30", "minutes": 30}, "maker_bp": 1.0});
        sync_pick(&mut g);
        assert_eq!((g.pick.vendor.as_deref(), g.pick.symbol.as_deref(), g.pick.date.as_deref()), (Some("databento"), Some("ESU6"), Some("2026-09-01")));
        assert_eq!((g.pick.start.as_str(), g.pick.minutes), ("13:30", 30));
        // 自己的根目录 + 注入扫描结果 → 换时长写回 `[backtest]`，其余项（手续费）照留，可撤销
        g.pick.root = "/画布往返测试".into();
        dp::inject_scan("databento", "/画布往返测试", "root\t/x\t1\nitem\tfuture\tESU6\t2026-09-01\tmbo,trades\n");
        g.pick.update(DataPickMsg::Minutes(60));
        write_pick(&mut g);
        assert_eq!(g.graph.backtest["session"]["minutes"], 60);
        assert_eq!(g.graph.backtest["types"], serde_json::json!(["mbo", "trades"]));
        assert_eq!((g.graph.backtest["end"].as_str(), g.graph.backtest["maker_bp"].as_f64()), (Some("2026-09-01"), Some(1.0)));
        assert_eq!(g.undo.len(), 1);
        assert_eq!(root_env(&g.pick), [("WS_DATABENTO_ROOT".to_string(), "/画布往返测试".to_string())]);
        write_pick(&mut g);
        assert_eq!(g.undo.len(), 1, "没变就不记撤销");
        // Dukascopy 的图（没写数据源也算）：预置成 B5 本地自有数据；选完写回渠道名、整日（不写时段）
        g.graph.backtest = serde_json::json!({"start": "2026-06-01", "end": "2026-06-05"});
        sync_pick(&mut g);
        assert_eq!((g.pick.source, g.pick.symbol.as_deref(), g.pick.end.as_deref()), (Some(BSource::Own), Some("XAUUSD"), Some("2026-06-05")));
        g.pick.root = "/自有数据测试".into();
        dp::inject_scan("own", "/自有数据测试", "root\t/x\t1\nitem\tcfd\tXAUUSD\t2026-06-01\tquotes\tdukascopy\nitem\tcfd\tXAUUSD\t2026-06-02\tquotes\tdukascopy\n");
        g.pick.update(DataPickMsg::End("2026-06-02".into()));
        write_pick(&mut g);
        let bt = &g.graph.backtest;
        assert_eq!((bt["source"].as_str(), bt["symbols"].as_str(), bt["end"].as_str()), (Some("dukascopy"), Some("XAUUSD"), Some("2026-06-02")));
        assert!(bt.get("session").is_none(), "自有数据按整日");
        assert_eq!(bt["types"], serde_json::json!(["quotes"]));
        assert_eq!(root_env(&g.pick), [("WS_OWN_ROOT".to_string(), "/自有数据测试".to_string())]);
        // 本地录制（B3）：预置、写回 source = recorder 与时段，根目录走 WS_DATA_DIR
        g.graph.backtest = serde_json::json!({"source": "recorder", "symbols": "BTCUSDT", "start": "2026-10-09", "session": {"start": "08:00", "minutes": 60}});
        sync_pick(&mut g);
        assert_eq!((g.pick.source, g.pick.local_key()), (Some(BSource::Recorded), Some("recorder")));
        g.pick.root = "/录制测试".into();
        dp::inject_scan("recorder", "/录制测试", "root\t/x\t1\nitem\tcrypto\tBTCUSDT\t2026-10-09\tl2,mark,trades\n");
        g.pick.update(DataPickMsg::Minutes(30));
        write_pick(&mut g);
        let bt = &g.graph.backtest;
        assert_eq!((bt["source"].as_str(), bt["end"].as_str(), bt["session"]["minutes"].as_u64()), (Some("recorder"), Some("2026-10-09"), Some(30)));
        assert_eq!(data_label(bt), "本地录制 · BTCUSDT · 2026-10-09 · 08:00 起 30 分钟 · l2+mark+trades");
        assert_eq!(root_env(&g.pick), [("WS_DATA_DIR".to_string(), "/录制测试".to_string())]);
        // auto：选择器从头选
        g.graph.backtest = serde_json::json!({"source": "auto", "symbols": "ESU6"});
        sync_pick(&mut g);
        assert_eq!(g.pick.pipeline, None);
    }

    #[test]
    fn check_output_carries_data_needs() {
        let c: CheckOut = serde_json::from_str(r#"{"ok":true,"needs":{"kinds":["price","book","trades"],"why":{"book":"订单流特征 z"},"grid_ms":100}}"#).unwrap();
        assert_eq!(c.needs.kinds, ["price", "book", "trades"]);
        assert_eq!((c.needs.grid_ms, Needs::label("book")), (100, "盘口增量"));
        let old: CheckOut = serde_json::from_str(r#"{"ok":true}"#).unwrap();
        assert!(old.needs.kinds.is_empty(), "旧的检查输出没有 needs 也能读");
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

    #[test]
    fn 实时数值只要屏幕上看得见的节点() {
        let g: Graph = serde_json::from_value(serde_json::json!({
            "nodes": [
                {"id": "a", "use": "value.rsi@1.0", "x": 10.0, "y": 10.0},
                {"id": "b", "use": "value.gt@1.0", "x": 5000.0, "y": 10.0},
                {"id": "c", "use": "features.ext@1.0", "x": 300.0, "y": 10.0},
                {"id": "d", "use": "value.lt@1.0", "x": 20.0, "y": 200.0, "group": "G"},
                {"id": "e", "use": "value.const@1.0", "x": 40.0, "y": 40.0}
            ],
            "groups": [{"id": "G", "title": "子图", "collapsed": true}]
        })).unwrap();
        let p = |e: Option<&str>, f: Option<&str>| Probe { expr: e.map(str::to_string), feature: f.map(str::to_string) };
        let probes: HashMap<String, Probe> = [("a", p(Some("rsi(close, 14)"), None)), ("b", p(Some("x > 1"), None)),
            ("c", p(None, Some("obi"))), ("d", p(Some("y < 2"), None))].into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        let (e, f) = watch_of(&g, &probes, None, (0.0, 0.0), 1.0, (1280.0, 800.0));
        assert_eq!((e, f), (vec!["rsi(close, 14)".to_string()], vec!["obi".to_string()]));   // b 在屏幕外，d 折叠着
        assert_eq!(watch_of(&g, &probes, Some("G"), (0.0, 0.0), 1.0, (1280.0, 800.0)).0, vec!["y < 2".to_string()]);
        assert_eq!(watch_of(&g, &probes, None, (0.0, 0.0), 0.3, (1280.0, 800.0)), (vec![], vec![]));   // 太小不画底栏
    }

    #[test]
    fn 实时数值落到节点上并留一段小图() {
        let p = |e: Option<&str>, f: Option<&str>| Probe { expr: e.map(str::to_string), feature: f.map(str::to_string) };
        let probes: HashMap<String, Probe> = [("a", p(Some("z > 1"), None)), ("b", p(None, Some("obi"))), ("c", p(Some("w"), None))]
            .into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        let mut vals = HashMap::new();
        apply_values(&mut vals, &probes, &serde_json::json!({"ts": 5, "seq": 1, "e": {"z > 1": true}, "f": {"obi": 0.25}}));
        apply_values(&mut vals, &probes, &serde_json::json!({"ts": 6, "seq": 2, "e": {"z > 1": false}, "f": {"obi": null}}));
        assert_eq!(vals["a"].v, Val::Flag(false));
        assert_eq!(vals["a"].hist.iter().copied().collect::<Vec<_>>(), vec![1.0, 0.0]);
        assert_eq!((vals["b"].v, vals["b"].ts), (Val::Missing, 6));
        assert!(vals["b"].hist[1].is_nan());
        assert!(!vals.contains_key("c"));                 // 这一份里没有它（不在屏幕上）：不动
        for i in 0..(VALUE_HIST + 5) {
            apply_values(&mut vals, &probes, &serde_json::json!({"ts": i, "e": {"z > 1": true}}));
        }
        assert_eq!(vals["a"].hist.len(), VALUE_HIST);
    }

    #[test]
    fn 成本线一句话() {
        let c: Map<String, Value> = serde_json::from_value(serde_json::json!({
            "trades": 75, "gross_bp": -0.245, "fee_bp": 6.441, "net_bp": -6.686, "maker_open": 1.0, "maker_close": 0.0
        })).unwrap();
        assert_eq!(cost_line(&c), "成本线：75 笔，平均毛 -0.24 bp − 手续费 6.44 bp = 净 -6.69 bp（按开仓名义额）；被动成交 开仓 100% · 平仓 0%");
        let empty = Map::new();
        assert!(cost_line(&empty).contains("平均毛 — bp"));
    }
}
