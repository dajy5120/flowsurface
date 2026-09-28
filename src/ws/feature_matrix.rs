//! 特征矩阵面板 — 视图状态与消息（docs/31 §8.1）。
//!
//! 状态放**进程级静态**而不是每个 pane 一份，与 `ws::radar` 同一形状：
//! 开两个矩阵 pane 时两边的筛选一致，「我筛过的」不会因为看的是另一个 pane 而失效。
//!
//! 筛选与折叠没有副作用。**有副作用的只有两件**（都在 [`handle`] 里、丢后台线程）：
//! 总开关启停 `ws-features`，以及启用集（默认 / 全开 / 自定义）写
//! [`config_path`] 再重启 `ws-features`——启用集决定引擎的缓冲与模块分配，不做热切换。

use std::sync::{Mutex, OnceLock};

/// 三个视图（docs/31 §8.1）。
///
/// 「研究纪律」不在这里：它是**另一个面板**（`ContentKind::FeatureLab`，docs/30）。
/// §8.1 要求它与 ①② 分开，而 docs/30 已经把它做完了——
/// 在这里再做一份会立刻产生第二套多重比较口径。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    /// ① 特征矩阵：七阶段纵向分节的主视图。
    #[default]
    Matrix,
    /// ② 实时向量：按阶段折叠，异常质量高亮。
    Vector,
    /// 引擎自身的健康（簿同步、缓冲容量、刷新率）。
    Engine,
}

impl View {
    pub const ALL: [Self; 3] = [Self::Matrix, Self::Vector, Self::Engine];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Matrix => "特征矩阵",
            Self::Vector => "实时向量",
            Self::Engine => "引擎健康",
        }
    }
}

/// 质量筛选。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QualityFilter {
    #[default]
    All,
    /// 只看可下单质量（`GOOD`）。
    UsableOnly,
    /// 只看有问题的——**这是排障时唯一想看的那一档**。
    ProblemsOnly,
}

impl QualityFilter {
    pub const ALL: [Self; 3] = [Self::All, Self::UsableOnly, Self::ProblemsOnly];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "全部",
            Self::UsableOnly => "仅可用",
            Self::ProblemsOnly => "仅异常",
        }
    }
}

/// 实现进度筛选。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StatusFilter {
    #[default]
    All,
    /// 只看已实现。
    Implemented,
    /// 只看已登记·待实现。
    Pending,
}

impl StatusFilter {
    pub const ALL: [Self; 3] = [Self::All, Self::Implemented, Self::Pending];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "全部",
            Self::Implemented => "已实现",
            Self::Pending => "待实现",
        }
    }
}

/// 面板的视图状态。全是筛选与折叠，**没有一个字段参与计算**。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ViewState {
    pub view: View,
    pub quality: QualityFilter,
    pub status: StatusFilter,
    /// 阶段筛选。`None` = 全部阶段。
    pub stage: Option<&'static str>,
    /// 族（类别）筛选。`None` = 全部。串是从快照里取的，故为 `String`。
    pub family: Option<String>,
    /// 市场筛选。`None` = 全部。
    pub market: Option<String>,
    /// 实时向量视图里被折叠起来的阶段。
    pub collapsed: Vec<String>,
    /// 显示本部署没启用的特征（默认藏起来：默认集下三百多行「未启用」会淹没有值的行）。
    pub show_disabled: bool,
    /// 自定义启用集的选择页（打开时整块替换视图）。
    pub picker: Option<Picker>,
}

/// 自定义启用集的草稿。点「应用并重启」才写配置；「取消」直接丢弃。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Picker {
    pub selected: std::collections::BTreeSet<String>,
    /// 按键名 / 中文名子串筛（四百多条，不给搜索等于没法选）。
    pub search: String,
}

impl Picker {
    #[must_use]
    pub fn matches(&self, s: &super::feature_matrix_readout::Slot) -> bool {
        let q = self.search.trim();
        q.is_empty() || s.key.contains(q) || s.name_cn.contains(q)
    }
}

/// 选择页的批量动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bulk {
    All,
    None,
    /// 按引擎的默认启用集勾选。
    Default,
}

impl ViewState {
    /// 一个 slot 是否通过当前全部筛选。
    #[must_use]
    pub fn passes(&self, s: &super::feature_matrix_readout::Slot) -> bool {
        match self.quality {
            QualityFilter::All => {}
            // 待实现的永远不是「可用」，但它也不是「异常」——它只是还没做。
            // 把它混进「仅异常」会让排障时的那张表里一半是与故障无关的行。
            QualityFilter::UsableOnly => {
                if s.quality != "GOOD" {
                    return false;
                }
            }
            QualityFilter::ProblemsOnly => {
                if !s.abnormal() || s.not_implemented() {
                    return false;
                }
            }
        }
        match self.status {
            StatusFilter::All => {}
            StatusFilter::Implemented => {
                if s.not_implemented() {
                    return false;
                }
            }
            StatusFilter::Pending => {
                if !s.not_implemented() {
                    return false;
                }
            }
        }
        if let Some(st) = self.stage
            && s.stage != st
        {
            return false;
        }
        if let Some(f) = &self.family
            && &s.family != f
        {
            return false;
        }
        if let Some(m) = &self.market
            && !s.markets.iter().any(|x| x == m)
        {
            return false;
        }
        true
    }

    /// 是否设了任何筛选。面板据此显示「已筛掉 N 条」——
    /// **不显示这句话是个坑**：筛过之后忘了筛，会把「矩阵里只有 3 条」当成引擎的问题。
    #[must_use]
    pub fn any_filter(&self) -> bool {
        self.quality != QualityFilter::All
            || self.status != StatusFilter::All
            || self.stage.is_some()
            || self.family.is_some()
            || self.market.is_some()
    }

    #[must_use]
    pub fn is_collapsed(&self, stage: &str) -> bool {
        self.collapsed.iter().any(|s| s == stage)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum FeatureMatrixMsg {
    SetView(View),
    SetQuality(QualityFilter),
    SetStatus(StatusFilter),
    /// `None` = 全部阶段。
    SetStage(Option<&'static str>),
    SetFamily(Option<String>),
    SetMarket(Option<String>),
    ToggleStage(String),
    ClearFilters,
    /// 特征引擎总开关：`"start"` / `"stop"`（systemctl，作用于 `ws-features`）。
    Engine(&'static str),
    /// 启用集切到 `"default"` / `"all"`：写配置并重启引擎。
    SetMode(&'static str),
    ToggleShowDisabled,
    /// 打开自定义选择页（初始勾选 = 配置里的自定义表，否则 = 当前启用的特征）。
    OpenPicker,
    ClosePicker,
    PickToggle(String, bool),
    /// `(阶段, 动作)`：阶段为 `None` 时作用于搜索结果里的全部特征。
    PickBulk(Option<&'static str>, Bulk),
    PickSearch(String),
    /// 写自定义配置并重启引擎。
    ApplyPicker,
}

static STATE: OnceLock<Mutex<ViewState>> = OnceLock::new();

fn cell() -> &'static Mutex<ViewState> {
    STATE.get_or_init(|| Mutex::new(ViewState::default()))
}

/// 当前视图状态的副本。
#[must_use]
pub fn state() -> ViewState {
    cell().lock().map(|g| g.clone()).unwrap_or_default()
}

pub fn handle(m: FeatureMatrixMsg) {
    match m {
        FeatureMatrixMsg::Engine(act) => engine_action(act),
        FeatureMatrixMsg::SetMode(mode) => apply_mode(mode, Vec::new()),
        FeatureMatrixMsg::ApplyPicker => {
            let keys: Vec<String> = state()
                .picker
                .map(|p| p.selected.into_iter().collect())
                .unwrap_or_default();
            if keys.is_empty() {
                set_engine_note("✗ 一条特征都没选——引擎会退回默认集，没有应用".into());
                return;
            }
            apply_mode("custom", keys);
            if let Ok(mut g) = cell().lock() {
                g.picker = None;
            }
        }
        m => {
            // 选择页的批量「恢复默认」与初始勾选要读快照（默认集、当前启用集）
            let snap = super::feature_matrix_readout::snapshot();
            let Ok(mut g) = cell().lock() else { return };
            apply_with(&mut g, m, &snap);
        }
    }
}

/// 启用集配置文件。**必须与主仓 `feature_config::config_path()` 算出同一个路径**。
#[must_use]
pub fn config_path() -> std::path::PathBuf {
    std::env::var("WS_FEATURE_CONFIG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| super::paths::data_dir().join("cockpit").join("feature_config.json"))
}

/// 配置文件里的 `(模式, 自定义键表)`。没有文件 = 默认集（与引擎同一约定）。
#[must_use]
pub fn read_config() -> (String, Vec<String>) {
    let Ok(t) = std::fs::read_to_string(config_path()) else {
        return ("default".into(), Vec::new());
    };
    let v: serde_json::Value = serde_json::from_str(&t).unwrap_or_default();
    let mode = v["mode"].as_str().unwrap_or("default").to_string();
    let keys = v["keys"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    (mode, keys)
}

/// 配置 JSON（纯函数，测试用）。
#[must_use]
pub fn config_json(mode: &str, keys: &[String]) -> String {
    serde_json::json!({ "mode": mode, "keys": keys }).to_string()
}

/// 写配置（临时文件 + rename：引擎恰好在读时不会读到半个文件）并重启引擎。
fn apply_mode(mode: &'static str, keys: Vec<String>) {
    let p = config_path();
    let tmp = p.with_extension("json.tmp");
    let r = std::fs::create_dir_all(p.parent().unwrap_or(std::path::Path::new(".")))
        .and_then(|()| std::fs::write(&tmp, config_json(mode, &keys)))
        .and_then(|()| std::fs::rename(&tmp, &p));
    if let Err(e) = r {
        set_engine_note(format!("✗ 写 {} 失败：{e}", p.display()));
        return;
    }
    let label = match mode {
        "all" => "全开".to_string(),
        "custom" => format!("自定义（{} 条）", keys.len()),
        _ => "默认".to_string(),
    };
    set_engine_note(format!("启用集 → {label}，正在重启引擎…"));
    std::thread::spawn(move || {
        let r = super::procs::action(ENGINE_KEY, "restart");
        set_engine_note(format!("启用集 → {label}：{r}（窗口从头累计，几分钟后才有长窗口的值）"));
    });
}

static ENGINE_NOTE: OnceLock<Mutex<String>> = OnceLock::new();

/// 总开关上一次动作的回执（给人看的一行字）。
#[must_use]
pub fn engine_note() -> String {
    ENGINE_NOTE
        .get_or_init(|| Mutex::new(String::new()))
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default()
}

fn set_engine_note(t: String) {
    if let Ok(mut g) = ENGINE_NOTE.get_or_init(|| Mutex::new(String::new())).lock() {
        *g = t;
    }
}

/// 启停特征引擎。与进程页同一个动作（[`super::procs::action`]）、同一个单元，
/// 丢后台线程——`systemctl stop` 要等进程退出，放在更新循环里会卡帧。
fn engine_action(act: &'static str) {
    set_engine_note(format!("正在{}…", if act == "start" { "启动" } else { "停止" }));
    std::thread::spawn(move || set_engine_note(super::procs::action(ENGINE_KEY, act)));
}

/// 特征引擎在进程清单（[`super::procs::ALL`]）里的 key。
pub const ENGINE_KEY: &str = "features";

/// 特征引擎此刻的状态（进程页的 5 秒轮询；轮询还没出第一轮时是 `None`）。
#[must_use]
pub fn engine_state() -> Option<super::svcctl::UnitState> {
    super::procs::rows()
        .into_iter()
        .find(|r| r.key == ENGINE_KEY)
        .map(|r| r.st)
}

/// 要读快照的状态转移（选择页的初始勾选与批量动作）。仍是纯函数：快照作参数传进来。
pub fn apply_with(st: &mut ViewState, m: FeatureMatrixMsg, snap: &super::feature_matrix_readout::Matrix) {
    match m {
        FeatureMatrixMsg::OpenPicker => {
            let (mode, keys) = read_config();
            let selected = if mode == "custom" && !keys.is_empty() {
                keys.into_iter().collect()
            } else {
                snap.enabled_keys().into_iter().collect()
            };
            st.picker = Some(Picker { selected, search: String::new() });
        }
        FeatureMatrixMsg::PickBulk(stage, bulk) => {
            let Some(p) = st.picker.as_mut() else { return };
            for r in snap.features() {
                let h = r.head();
                if stage.is_some_and(|x| h.stage != x) || !p.matches(h) {
                    continue;
                }
                let on = match bulk {
                    Bulk::All => true,
                    Bulk::None => false,
                    Bulk::Default => h.default_on,
                };
                if on {
                    p.selected.insert(h.key.clone());
                } else {
                    p.selected.remove(&h.key);
                }
            }
        }
        other => apply(st, other),
    }
}

/// 纯函数形式的状态转移。测试直接测它——不需要碰全局锁。
pub fn apply(st: &mut ViewState, m: FeatureMatrixMsg) {
    match m {
        FeatureMatrixMsg::SetView(v) => st.view = v,
        FeatureMatrixMsg::SetQuality(q) => st.quality = q,
        FeatureMatrixMsg::SetStatus(s) => st.status = s,
        FeatureMatrixMsg::SetStage(s) => st.stage = s,
        FeatureMatrixMsg::SetFamily(f) => st.family = f,
        FeatureMatrixMsg::SetMarket(m) => st.market = m,
        FeatureMatrixMsg::ToggleStage(s) => {
            if let Some(i) = st.collapsed.iter().position(|x| *x == s) {
                st.collapsed.remove(i);
            } else {
                st.collapsed.push(s);
            }
        }
        FeatureMatrixMsg::ToggleShowDisabled => st.show_disabled = !st.show_disabled,
        FeatureMatrixMsg::ClosePicker => st.picker = None,
        FeatureMatrixMsg::PickToggle(k, on) => {
            if let Some(p) = st.picker.as_mut() {
                if on {
                    p.selected.insert(k);
                } else {
                    p.selected.remove(&k);
                }
            }
        }
        FeatureMatrixMsg::PickSearch(q) => {
            if let Some(p) = st.picker.as_mut() {
                p.search = q;
            }
        }
        // 有副作用或要读快照的，不在这里（见 `handle` / `apply_with`）
        FeatureMatrixMsg::Engine(_)
        | FeatureMatrixMsg::SetMode(_)
        | FeatureMatrixMsg::ApplyPicker
        | FeatureMatrixMsg::OpenPicker
        | FeatureMatrixMsg::PickBulk(..) => {}
        // 折叠状态**不清**：它是「我在看哪一段」，不是筛选。
        FeatureMatrixMsg::ClearFilters => {
            st.quality = QualityFilter::All;
            st.status = StatusFilter::All;
            st.stage = None;
            st.family = None;
            st.market = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::feature_matrix_readout::Slot;
    use super::*;

    fn slot(key: &str, stage: &str, family: &str, quality: &str, status: &str) -> Slot {
        Slot {
            key: key.into(),
            stage: stage.into(),
            family: family.into(),
            quality: quality.into(),
            status: status.into(),
            markets: vec!["crypto_perp".into()],
            ..Default::default()
        }
    }

    #[test]
    fn 默认不筛任何东西() {
        let st = ViewState::default();
        assert!(!st.any_filter());
        assert!(st.passes(&slot("a", "S1_price", "price", "GOOD", "impl")));
        assert!(st.passes(&slot("b", "S5_order_behavior", "queue", "UNAVAILABLE", "spec")));
    }

    #[test]
    fn 仅异常不该把待实现混进来() {
        // 排障时点「仅异常」，想看的是**坏了的**东西。待实现的 slot 质量也是
        // UNAVAILABLE，但它不是故障——混进来会让那张表一半是噪声。
        let st = ViewState {
            quality: QualityFilter::ProblemsOnly,
            ..Default::default()
        };
        assert!(st.passes(&slot("a", "S3_book", "obi", "DEGRADED", "impl")));
        assert!(!st.passes(&slot("b", "S5_order_behavior", "queue", "UNAVAILABLE", "spec")));
        assert!(!st.passes(&slot("c", "S1_price", "price", "GOOD", "impl")));
    }

    #[test]
    fn 仅可用只放good过() {
        let st = ViewState {
            quality: QualityFilter::UsableOnly,
            ..Default::default()
        };
        assert!(st.passes(&slot("a", "S1_price", "price", "GOOD", "impl")));
        // DEGRADED 是「能看但不能下单」，在「仅可用」里必须被筛掉。
        assert!(!st.passes(&slot("b", "S1_price", "price", "DEGRADED", "impl")));
    }

    #[test]
    fn 四个维度可以同时筛() {
        let st = ViewState {
            stage: Some("S3_book"),
            family: Some("obi".into()),
            market: Some("crypto_perp".into()),
            status: StatusFilter::Implemented,
            ..Default::default()
        };
        assert!(st.passes(&slot("a", "S3_book", "obi", "GOOD", "impl")));
        // 任一维度不匹配即筛掉。
        assert!(!st.passes(&slot("b", "S2_trade", "obi", "GOOD", "impl")));
        assert!(!st.passes(&slot("c", "S3_book", "depth", "GOOD", "impl")));
        assert!(!st.passes(&slot("d", "S3_book", "obi", "GOOD", "spec")));
        let mut other = slot("e", "S3_book", "obi", "GOOD", "impl");
        other.markets = vec!["equity".into()];
        assert!(!st.passes(&other));
    }

    #[test]
    fn 清筛选不清折叠() {
        // 折叠是「我在看哪一段」，清筛选把它也清掉会让视图跳回全展开——
        // 而用户按的是「清筛选」。
        let mut st = ViewState::default();
        apply(&mut st, FeatureMatrixMsg::ToggleStage("S1_price".into()));
        apply(&mut st, FeatureMatrixMsg::SetQuality(QualityFilter::ProblemsOnly));
        assert!(st.any_filter());
        apply(&mut st, FeatureMatrixMsg::ClearFilters);
        assert!(!st.any_filter());
        assert!(st.is_collapsed("S1_price"), "清筛选把折叠也清了");
    }

    #[test]
    fn 折叠是开关不是只能折() {
        let mut st = ViewState::default();
        apply(&mut st, FeatureMatrixMsg::ToggleStage("S2_trade".into()));
        assert!(st.is_collapsed("S2_trade"));
        apply(&mut st, FeatureMatrixMsg::ToggleStage("S2_trade".into()));
        assert!(!st.is_collapsed("S2_trade"), "再点一次必须展开");
        assert!(st.collapsed.is_empty(), "展开后不该留下空壳");
    }

    #[test]
    fn 视图切换不影响筛选() {
        let mut st = ViewState::default();
        apply(&mut st, FeatureMatrixMsg::SetStage(Some("S7_execution")));
        apply(&mut st, FeatureMatrixMsg::SetView(View::Vector));
        assert_eq!(st.view, View::Vector);
        assert_eq!(st.stage, Some("S7_execution"), "换视图把筛选也丢了");
    }

    fn snap() -> super::super::feature_matrix_readout::Matrix {
        super::super::feature_matrix_readout::parse(
            r#"{"slots":[
          {"key":"mid_price","name_cn":"中间价","stage":"S1_price","window_ms":0,"quality":"GOOD","reason":null,"default_on":true},
          {"key":"obi_l2","name_cn":"两档失衡","stage":"S3_book","window_ms":0,"quality":"GOOD","reason":null,"default_on":true},
          {"key":"obi_l2","name_cn":"两档失衡","stage":"S3_book","window_ms":1000,"quality":"GOOD","reason":null,"default_on":true},
          {"key":"variance_ratio_2","name_cn":"方差比","stage":"S6_regime","window_ms":30000,"quality":"UNAVAILABLE","reason":"disabled","default_on":false}]}"#,
        )
    }

    #[test]
    fn 选择页批量动作只作用于搜索结果与所选阶段() {
        let m = snap();
        let mut st = ViewState::default();
        st.picker = Some(Picker::default());
        apply_with(&mut st, FeatureMatrixMsg::PickBulk(None, Bulk::All), &m);
        assert_eq!(st.picker.as_ref().unwrap().selected.len(), 3, "按特征计，不按窗口");
        apply_with(&mut st, FeatureMatrixMsg::PickBulk(Some("S3_book"), Bulk::None), &m);
        assert!(!st.picker.as_ref().unwrap().selected.contains("obi_l2"));
        apply(&mut st, FeatureMatrixMsg::PickSearch("方差".into()));
        apply_with(&mut st, FeatureMatrixMsg::PickBulk(None, Bulk::None), &m);
        let sel = &st.picker.as_ref().unwrap().selected;
        assert!(sel.contains("mid_price"), "搜索之外的不该被动");
        assert!(!sel.contains("variance_ratio_2"));
        apply(&mut st, FeatureMatrixMsg::PickSearch(String::new()));
        apply_with(&mut st, FeatureMatrixMsg::PickBulk(None, Bulk::Default), &m);
        let sel = &st.picker.as_ref().unwrap().selected;
        assert!(sel.contains("mid_price") && sel.contains("obi_l2") && !sel.contains("variance_ratio_2"));
        apply(&mut st, FeatureMatrixMsg::ClosePicker);
        assert!(st.picker.is_none(), "取消 = 丢掉草稿");
    }

    #[test]
    fn 配置json与引擎约定一致() {
        // 引擎侧 `feature_config::FeatureConfig` 读的就是这两个字段
        let t = config_json("custom", &["mid_price".into(), "obi_l2".into()]);
        let v: serde_json::Value = serde_json::from_str(&t).unwrap();
        assert_eq!(v["mode"], "custom");
        assert_eq!(v["keys"].as_array().unwrap().len(), 2);
        assert!(config_path().ends_with("cockpit/feature_config.json") || std::env::var("WS_FEATURE_CONFIG").is_ok());
    }

    #[test]
    fn 总开关不动筛选且指向特征引擎() {
        // 启停是副作用，不该顺手改掉人正在看的筛选
        let mut st = ViewState::default();
        apply(&mut st, FeatureMatrixMsg::SetStage(Some("S3_book")));
        let before = st.clone();
        apply(&mut st, FeatureMatrixMsg::Engine("stop"));
        assert_eq!(st.stage, before.stage);
        assert_eq!(st.view, before.view);
        // key 改名而这里没跟 = 按钮启停的是别的服务（或回「未知条目」）
        let p = super::super::procs::ALL.iter().find(|p| p.key == ENGINE_KEY).expect("进程清单里有特征引擎");
        assert_eq!(p.unit, "ws-features");
    }
}
