//! 「七层」面板（docs/39）：大师策略的七层流水线（体制 → 形态 → 确认 → 入场 → 止损 → 仓位 → 出场）
//! 与共用件矩阵。独立面板，跟随策略中心当前选中的策略与运行。
//!
//! 数据只有两个来源（面板不算任何东西）：
//! - 一次运行的 `layers.json`（`strategies/research/masters/core/layers_report.py` 写，快速回测 `bars` 适配器才有）；
//! - 共用件目录 `python -m strategies.research.masters.core.components --json`（第 3 期）。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use serde::Deserialize;

use super::strategy_center::{self as sc, LayersTarget};

#[derive(Deserialize, Clone, Debug, Default)]
pub struct FunnelBox {
    pub layer: String,
    pub label: String,
    #[serde(default)]
    pub rows: Vec<(String, i64)>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct SymData {
    pub t: Vec<i64>,
    pub o: Vec<Option<f64>>,
    pub h: Vec<Option<f64>>,
    pub l: Vec<Option<f64>>,
    pub c: Vec<Option<f64>>,
    #[serde(default)]
    pub regime: Vec<(i64, i64, i64)>,
    #[serde(default)]
    pub stops: Vec<(i64, i64, Option<f64>)>,
    /// (时间, 层, 事件, 数值, 标签)
    #[serde(default)]
    pub events: Vec<(i64, String, String, Option<f64>, String)>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct LayersData {
    /// orders = 挂单型；target = 目标仓位型
    pub kind: String,
    pub funnel: Vec<FunnelBox>,
    pub symbols: BTreeMap<String, SymData>,
}

/// 共用件（第 3 期）：`core/components.py` 的登记表 + 各策略的声明。
#[derive(Deserialize, Clone, Debug, Default)]
pub struct Component {
    pub key: String,
    pub layer: String,
    pub label: String,
    #[serde(default)]
    pub implementation: String,
    #[serde(default)]
    pub description: String,
    /// 用到它的策略 id
    #[serde(default)]
    pub used_by: Vec<String>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Components {
    pub components: Vec<Component>,
    /// 策略 id → 名称（矩阵的列头）
    #[serde(default)]
    pub strategies: BTreeMap<String, String>,
}

/// 页签。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LyTab {
    #[default]
    Pipeline,
    Matrix,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LyMsg {
    Tab(LyTab),
    /// None = 全部层
    Layer(Option<String>),
    Symbol(String),
    Component(Option<String>),
    Reload,
}

#[derive(Default)]
struct St {
    loaded: Option<PathBuf>,
    loading: bool,
    data: Option<Arc<LayersData>>,
    err: String,
    layer: Option<String>,
    symbol: Option<String>,
    tab: LyTab,
    comps: Option<Arc<Components>>,
    comps_loading: bool,
    comps_err: String,
    component: Option<String>,
}

static ST: OnceLock<Mutex<St>> = OnceLock::new();

fn with<R>(f: impl FnOnce(&mut St) -> R) -> Option<R> {
    ST.get_or_init(|| Mutex::new(St::default())).lock().ok().map(|mut g| f(&mut g))
}

/// 视图读的快照。
#[derive(Clone, Default)]
pub struct View {
    pub target: Option<LayersTarget>,
    pub data: Option<Arc<LayersData>>,
    pub loading: bool,
    pub err: String,
    pub layer: Option<String>,
    pub symbol: Option<String>,
    pub tab: LyTab,
    pub comps: Option<Arc<Components>>,
    pub comps_loading: bool,
    pub comps_err: String,
    pub component: Option<String>,
}

pub fn view() -> View {
    let target = sc::layers_target();
    let want = target.as_ref().and_then(|t| t.dir.clone()).map(|d| d.join("layers.json"));
    let need = with(|g| {
        if g.loaded != want && !g.loading {
            g.loaded = want.clone();
            g.data = None;
            g.err.clear();
            g.loading = want.is_some();
            return want.clone();
        }
        None
    })
    .flatten();
    if let Some(p) = need {
        load(p);
    }
    let need_comps = with(|g| {
        // 流水线方块也要共用件的中文名：两页都加载
        let go = g.comps.is_none() && !g.comps_loading && g.comps_err.is_empty();
        if go {
            g.comps_loading = true;
        }
        go
    })
    .unwrap_or(false);
    if need_comps {
        load_components();
    }
    with(|g| View {
        target,
        data: g.data.clone(),
        loading: g.loading,
        err: g.err.clone(),
        layer: g.layer.clone(),
        symbol: g.symbol.clone(),
        tab: g.tab,
        comps: g.comps.clone(),
        comps_loading: g.comps_loading,
        comps_err: g.comps_err.clone(),
        component: g.component.clone(),
    })
    .unwrap_or_default()
}

fn load(p: PathBuf) {
    super::spawn_named("ws-layers", move || {
        let r: Result<LayersData, String> = match std::fs::read(&p) {
            Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("layers.json 解析失败：{e}")),
            Err(_) => Err("这次运行没有七层轨迹（只有大师日线引擎的快速回测才写 layers.json；旧运行请重跑）".into()),
        };
        with(|g| {
            if g.loaded.as_ref() != Some(&p) {
                return;
            }
            g.loading = false;
            match r {
                Ok(d) => {
                    if g.symbol.as_ref().is_none_or(|s| !d.symbols.contains_key(s)) {
                        g.symbol = d.symbols.keys().next().cloned();
                    }
                    g.data = Some(Arc::new(d));
                }
                Err(e) => g.err = e,
            }
        });
    });
}

fn load_components() {
    super::spawn_named("ws-layers-comp", move || {
        let out = std::process::Command::new(super::paths::python())
            .args(["-m", "strategies.research.masters.core.components", "--json"])
            .current_dir(super::paths::repo_root())
            .output();
        let r: Result<Components, String> = match out {
            Ok(o) if o.status.success() => serde_json::from_slice(&o.stdout).map_err(|e| format!("共用件目录解析失败：{e}")),
            Ok(o) => Err(String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("共用件目录读取失败").to_string()),
            Err(e) => Err(format!("起不来 Python：{e}")),
        };
        with(|g| {
            g.comps_loading = false;
            match r {
                Ok(c) => g.comps = Some(Arc::new(c)),
                Err(e) => g.comps_err = e,
            }
        });
    });
}

pub fn handle(m: LyMsg) {
    with(|g| match m {
        LyMsg::Tab(t) => g.tab = t,
        LyMsg::Layer(l) => g.layer = l,
        LyMsg::Symbol(s) => g.symbol = Some(s),
        LyMsg::Component(c) => g.component = c,
        LyMsg::Reload => {
            g.loaded = None;
            g.comps = None;
            g.comps_err.clear();
        }
    });
}

/// 事件名 → 中文。
pub fn kind_label(k: &str) -> &'static str {
    match k {
        "signal" => "形态信号",
        "blocked" => "体制挡掉",
        "pass" => "确认通过",
        "fail" => "确认拒绝",
        "qty" => "定仓",
        "zero" => "数量为 0",
        "order" => "挂单",
        "fill_entry" => "入场成交",
        "fill_add" => "加仓成交",
        "init" => "初始止损",
        "move" => "止损移动",
        "fill" => "出场成交",
        "target" => "目标仓位",
        "state" => "体制状态",
        _ => "事件",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_layers_json() {
        let j = r#"{"kind":"orders","funnel":[{"layer":"regime","label":"体制","rows":[["评估",10],["放行",8]]}],
                   "symbols":{"ES":{"t":[1,2],"o":[1.0,2.0],"h":[2.0,3.0],"l":[0.5,1.5],"c":[1.5,null],
                   "regime":[[1,2,2]],"stops":[[1,2,0.9]],"events":[[2,"entry","fill_entry",1.5,"breakout20"]]}}}"#;
        let d: LayersData = serde_json::from_str(j).unwrap();
        assert_eq!(d.funnel[0].rows[1], ("放行".to_string(), 8));
        let s = &d.symbols["ES"];
        assert_eq!(s.c[1], None);
        assert_eq!(s.events[0].2, "fill_entry");
        assert_eq!(kind_label(&s.events[0].2), "入场成交");
    }
}
