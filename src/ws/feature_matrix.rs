//! 特征矩阵面板 — 视图状态与消息（docs/31 §8.1）。
//!
//! 状态放**进程级静态**而不是每个 pane 一份，与 `ws::radar` 同一形状：
//! 开两个矩阵 pane 时两边的筛选一致，「我筛过的」不会因为看的是另一个 pane 而失效。
//!
//! 筛选与折叠没有副作用。**有副作用的只有两件**（都在 [`handle`] 里、丢后台线程）：
//! 总开关启停 `ws-features`，以及启用集（默认 / 全开 / 自定义）写
//! [`config_path`] 再重启 `ws-features`——启用集决定引擎的缓冲与模块分配，不做热切换。

use std::sync::{Mutex, OnceLock};

/// 三个视图：特征矩阵 / 图表参数（docs/33）/ 引擎健康。
///
/// docs/31 §8.1 原设计还有一个「② 实时向量」（按阶段折叠、异常高亮）。做下来它与特征矩阵的
/// 列、行、筛选都一样，只差折叠与异常标黄——同一张表放两遍，徒增困惑。2026-09-29 合并：
/// 折叠与异常高亮并进特征矩阵，视图只剩两个。
///
/// 「研究纪律」不在这里：它是**另一个面板**（`ContentKind::FeatureLab`，docs/30）。
/// §8.1 要求它与 ①② 分开，而 docs/30 已经把它做完了——
/// 在这里再做一份会立刻产生第二套多重比较口径。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    /// 特征矩阵：七阶段纵向分节，阶段可折叠，有异常窗口的阶段标黄。
    #[default]
    Matrix,
    /// 图表参数（docs/33）：12 张经典图表卡片，参数取自特征与图表参数层。
    Chart,
    /// 引擎自身的健康（簿同步、缓冲容量、刷新率）。
    Engine,
}

impl View {
    pub const ALL: [Self; 3] = [Self::Matrix, Self::Chart, Self::Engine];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Matrix => "特征矩阵",
            Self::Chart => "图表参数",
            Self::Engine => "引擎健康",
        }
    }
}

/// 表头 / 表体两个 scrollable 的控件 ID（表体横滚时把表头滚到同一位置）。
pub const HEAD_ID: &str = "feature-matrix-table-head";
pub const BODY_ID: &str = "feature-matrix-table-body";

/// 表格的一类列。同一类列在各个窗口组里宽度相同（拖一处，所有窗口组一起变）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Col {
    Name,
    Key,
    Val,
    Z,
    Pct,
    /// 透视模式每个窗口的那一格。
    Cell,
    Unit,
    Layer,
    Status,
}

impl Col {
    pub const ALL: [Self; 9] = [
        Self::Name,
        Self::Key,
        Self::Val,
        Self::Z,
        Self::Pct,
        Self::Cell,
        Self::Unit,
        Self::Layer,
        Self::Status,
    ];

    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Key => "key",
            Self::Val => "val",
            Self::Z => "z",
            Self::Pct => "pct",
            Self::Cell => "cell",
            Self::Unit => "unit",
            Self::Layer => "layer",
            Self::Status => "status",
        }
    }
}

/// 列宽下限 / 上限（像素）。
pub const COL_MIN: f32 = 28.0;
pub const COL_MAX: f32 = 600.0;

/// 表格的展示方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TableMode {
    /// 每个窗口一组：质量色条 + 值 | z | 分位。
    #[default]
    Full,
    /// 每个窗口一列，只显示选中的一个指标（z / 分位按偏离着色，成热力图）。
    Pivot,
}

impl TableMode {
    pub const ALL: [Self; 2] = [Self::Full, Self::Pivot];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Full => "完整",
            Self::Pivot => "透视",
        }
    }
}

/// 透视视图显示的指标。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Metric {
    #[default]
    Z,
    Pct,
    Value,
    Quality,
    /// 与 N 秒前相比 z 的变化（涨跌箭头 + Δz）。
    Change,
}

impl Metric {
    pub const ALL: [Self; 5] = [Self::Z, Self::Pct, Self::Value, Self::Quality, Self::Change];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Z => "z",
            Self::Pct => "分位",
            Self::Value => "值",
            Self::Quality => "质量",
            Self::Change => "变化",
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
    /// 被折叠起来的阶段（点阶段标题切换）。
    pub collapsed: Vec<String>,
    /// 显示本部署没启用的特征（默认藏起来：默认集下三百多行「未启用」会淹没有值的行）。
    pub show_disabled: bool,
    /// 自定义启用集的选择页（打开时整块替换视图）。
    pub picker: Option<Picker>,
    /// 全局时间窗口的编辑草稿（模式栏下的「时间窗口」一行；`None` = 没在编辑）。
    pub win_edit: Option<WinEdit>,
    /// 表格展示方式（完整 / 透视）与透视的指标。
    pub table: TableMode,
    pub metric: Metric,
    /// 鼠标所在的特征行（整行高亮）。
    pub hover: Option<String>,
    /// 收起表格上方的控件（引擎 / 启用集 / 窗口 / 筛选），把高度让给表格。
    pub fold_controls: bool,
    /// 用户拖过的列宽（像素）；没拖过的列按内容自适应。持久化在 [`ui_path`]。
    pub col_w: std::collections::BTreeMap<Col, f32>,
    /// 正在拖的列：`(列, 起点鼠标 x, 起点宽度)`。
    pub drag: Option<(Col, f32, f32)>,
    /// 鼠标在表头里的最近 x（拖拽起点用）。
    pub mouse_x: f32,
    /// 涨跌箭头：与多久之前比（毫秒）、各档的 |Δz| 阈值（1~5 档，逐档递增；第 N 档画 N 个箭头）。
    /// 持久化在 [`ui_path`]。
    pub trend_ms: u32,
    pub ths: Vec<f64>,
    /// 输入框的文本（输入到一半时不合法，合法了才生效）：比较时长 + 五档阈值（空 = 这一档不用）。
    pub trend_text: String,
    pub th_text: [String; TREND_LEVELS],
    /// 上一次输入不合法的原因。
    pub trend_err: String,
}

/// 涨跌箭头的默认值：与 5 秒前比，|Δz| ≥ 0.5 一个箭头、≥ 1.5 两个箭头（第 3~5 档默认不用）。
pub const TREND_MS: u32 = 5_000;
pub const THS: [f64; 2] = [0.5, 1.5];
/// 最多几档（第 N 档画 N 个箭头）。
pub const TREND_LEVELS: usize = 5;
/// 比较时长的范围（上限 = 面板保留的快照历史长度）。
pub const TREND_MIN_MS: u32 = 1_000;
pub const TREND_MAX_MS: u32 = 600_000;

/// 某格与 N 秒前相比的 z 变化 → 箭头级别：±(|Δz| 达到了几档阈值)，0 = 不画。
#[must_use]
pub fn trend_level(dz: f64, ths: &[f64]) -> i8 {
    let n = ths.iter().take_while(|t| dz.abs() >= **t).count() as i8;
    if dz < 0.0 { -n } else { n }
}

/// 校验五个阈值输入框：按顺序填（中间不能空）、都大于 0、逐档递增；至少填一档。
pub fn parse_ths(texts: &[String]) -> Result<Vec<f64>, String> {
    let mut out: Vec<f64> = Vec::new();
    let mut ended = false;
    for (i, t) in texts.iter().enumerate() {
        let t = t.trim();
        if t.is_empty() {
            ended = true;
            continue;
        }
        if ended {
            return Err(format!("第 {} 档前面有空着的档——阈值要从第 1 档起按顺序填", i + 1));
        }
        let x: f64 = t.parse().map_err(|_| format!("第 {} 档「{t}」不是数", i + 1))?;
        if x <= 0.0 {
            return Err(format!("第 {} 档要大于 0", i + 1));
        }
        if let Some(prev) = out.last()
            && x <= *prev
        {
            return Err(format!("第 {} 档（{x}）要大于第 {} 档（{prev}）", i + 1, i));
        }
        out.push(x);
    }
    if out.is_empty() {
        return Err("至少填第 1 档".into());
    }
    Ok(out)
}

/// 阈值的说法，如 `0.5 / 1.5`。
#[must_use]
pub fn fmt_ths(ths: &[f64]) -> String {
    ths.iter().map(|x| format!("{x}")).collect::<Vec<_>>().join(" / ")
}

/// 阈值 → 五个输入框的文本（没用的档是空串）。
#[must_use]
pub fn ths_text(ths: &[f64]) -> [String; TREND_LEVELS] {
    std::array::from_fn(|i| ths.get(i).map_or_else(String::new, |x| format!("{x}")))
}

/// 全局时间窗口的编辑草稿。点「应用并重启」才写配置。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WinEdit {
    /// 草稿里的窗口（毫秒，升序）。空 = 恢复字典默认。
    pub list: Vec<u32>,
    /// 「添加窗口」输入框。
    pub input: String,
    /// 上一次添加失败的原因。
    pub err: String,
}

/// 常用的几组全局窗口（一键填进草稿）。
pub const WINDOW_PRESETS: [(&str, &[u32]); 3] = [
    ("高频", &[1_000, 5_000, 30_000]),
    ("日内", &[60_000, 300_000, 900_000, 3_600_000]),
    ("波段", &[3_600_000, 14_400_000, 86_400_000]),
];

/// 自定义启用集的草稿。点「应用并重启」才写配置；「取消」直接丢弃。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Picker {
    pub selected: std::collections::BTreeSet<String>,
    /// 按键名 / 中文名子串筛（四百多条，不给搜索等于没法选）。
    pub search: String,
    /// 正在编辑的已存选择集（`None` = 还没存过的新草稿）。
    pub editing: Option<String>,
    /// 名称输入框（新建时是新名字；编辑时改它 + 「更新」= 改名）。
    pub name: String,
    pub note: String,
    /// 点过一次「删除」、等第二次确认的选择集。
    pub confirm_delete: Option<String>,
    /// 全局时间窗口（文本，如 `1m 5m 15m`；空 = 字典默认）。随选择集保存。
    pub global_text: String,
    /// 单条特征的窗口覆盖（特征键 → 文本；空串 = 不覆盖）。随选择集保存。
    pub win_text: std::collections::BTreeMap<String, String>,
}

impl Picker {
    /// 草稿与正在编辑的选择集不一致（有未保存的修改）。
    #[must_use]
    pub fn dirty(&self) -> bool {
        let Some(n) = &self.editing else {
            return false;
        };
        match super::feature_presets::get(n) {
            Some(p) => {
                let saved: std::collections::BTreeSet<String> = p.keys.iter().cloned().collect();
                saved != self.selected
                    || p.note != self.note.trim()
                    || n != self.name.trim()
                    || self.windows().ok().as_ref() != Some(&p.windows)
            }
            None => true,
        }
    }

    /// 把一个选择集载入草稿。当前字典里没有的键跳过，返回跳过了几条。
    pub fn load(&mut self, p: &super::feature_presets::Preset, known: &std::collections::BTreeSet<String>) -> usize {
        self.selected = p.keys.iter().filter(|k| known.is_empty() || known.contains(*k)).cloned().collect();
        self.editing = Some(p.name.clone());
        self.name = p.name.clone();
        self.note = p.note.clone();
        self.confirm_delete = None;
        self.set_windows(&p.windows);
        p.keys.len() - self.selected.len()
    }

    /// 把窗口设置填进文本框。
    pub fn set_windows(&mut self, w: &super::feature_presets::PresetWindows) {
        use super::feature_matrix_readout::format_windows;
        self.global_text = w.global.as_deref().map(format_windows).unwrap_or_default();
        self.win_text = w.overrides.iter().map(|(k, v)| (k.clone(), format_windows(v))).collect();
    }

    /// 解析草稿里的窗口设置；写错的给出是哪一条。
    pub fn windows(&self) -> Result<super::feature_presets::PresetWindows, String> {
        use super::feature_matrix_readout::parse_windows;
        let g = parse_windows(&self.global_text).map_err(|e| format!("全局窗口：{e}"))?;
        let mut o = std::collections::BTreeMap::new();
        for (k, t) in &self.win_text {
            let v = parse_windows(t).map_err(|e| format!("{k} 的窗口：{e}"))?;
            if !v.is_empty() {
                o.insert(k.clone(), v);
            }
        }
        Ok(super::feature_presets::PresetWindows { global: (!g.is_empty()).then_some(g), overrides: o })
    }
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
    /// 把已存选择集载入选择页（编辑它）。
    PresetLoad(String),
    /// 从当前勾选开始一个新的（未保存）选择集。
    PresetNew,
    PresetName(String),
    PresetNote(String),
    /// 以名称框里的名字另存为新选择集。
    PresetSaveNew,
    /// 覆盖正在编辑的选择集（名称框改过 = 顺带改名）。
    PresetUpdate,
    /// 删除（第一次点只是要求确认）。
    PresetDelete(String),
    /// 导出到文件（系统「另存为」对话框）。
    PresetExport(String),
    /// 从文件导入（系统「打开」对话框），导入后直接载入编辑。
    PresetImport,
    /// 直接应用一个已存选择集并重启引擎（模式栏的下拉框）。
    PresetApply(String),
    /// 选择页：全局窗口文本。
    PickGlobalWindows(String),
    /// 选择页：`(特征键, 窗口文本)`。
    PickWindows(String, String),
    /// 模式栏「时间窗口」：打开 / 关闭编辑。
    WinOpen,
    WinClose,
    WinInput(String),
    /// 把输入框里的窗口加进草稿。
    WinAdd,
    WinRemove(u32),
    /// 用一组常用窗口替换草稿。
    WinQuick(usize),
    /// 清空草稿（= 字典默认窗口）。
    WinDefault,
    /// 写配置并重启引擎。
    WinApply,
    SetTable(TableMode),
    SetMetric(Metric),
    /// 鼠标进入某一行。
    HoverIn(String),
    /// 鼠标离开某一行（只清掉自己——进出事件的先后不保证）。
    HoverOut(String),
    ToggleFoldControls,
    /// 表体滚动（横向偏移）：返回给上层去同步表头。
    TableScrolled(f32),
    /// 鼠标在表头里移动（相对表头的 x）。
    HeaderMove(f32),
    /// 在某列右侧的分隔线上按下：`(列, 当前宽度)`。
    DragStart(Col, f32),
    DragEnd,
    /// 全部列回到自适应宽度。
    ResetWidths,
    /// 涨跌箭头：比较时长 / 两档阈值的输入框。
    TrendLookback(String),
    /// `(第几档，0 起, 文本)`。
    TrendTh(usize, String),
    /// 某一列回到自适应宽度（双击表头分隔线）。
    AutoCol(Col),
    /// 数据源（共用数据选择组件 + 回放控制，见 [`super::feature_source`]）。
    Source(super::feature_source::SourceMsg),
    /// 图表参数的口径设置（docs/33 批 4，见 [`super::chart_params::handle_edit`]）。
    ChartEdit(super::chart_params::ChartEditMsg),
    /// 图表参数卡片的列宽（表头拖分隔线）。
    ChartUi(super::chart_params::ChartUiMsg),
}

static STATE: OnceLock<Mutex<ViewState>> = OnceLock::new();

fn cell() -> &'static Mutex<ViewState> {
    STATE.get_or_init(|| {
        let p = load_ui();
        Mutex::new(ViewState {
            trend_text: super::feature_matrix_readout::format_window(p.trend_ms),
            th_text: ths_text(&p.ths),
            col_w: p.col_w,
            trend_ms: p.trend_ms,
            ths: p.ths,
            ..ViewState::default()
        })
    })
}

/// 面板自己的界面偏好（列宽）。与引擎配置分开：它不影响计算，也不该随选择集走。
#[must_use]
pub fn ui_path() -> std::path::PathBuf {
    super::paths::data_dir().join("cockpit").join("feature_matrix_ui.json")
}

/// 界面偏好（列宽 + 涨跌箭头设置）。
#[derive(Debug, Clone, PartialEq)]
pub struct UiPrefs {
    pub col_w: std::collections::BTreeMap<Col, f32>,
    pub trend_ms: u32,
    pub ths: Vec<f64>,
}

impl UiPrefs {
    fn of(st: &ViewState) -> Self {
        Self { col_w: st.col_w.clone(), trend_ms: st.trend_ms, ths: st.ths.clone() }
    }
}

fn load_ui() -> UiPrefs {
    let v: serde_json::Value = std::fs::read_to_string(ui_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let col_w = Col::ALL
        .iter()
        .filter_map(|c| {
            v["col_w"][c.key()]
                .as_f64()
                .map(|w| (*c, (w as f32).clamp(COL_MIN, COL_MAX)))
        })
        .collect();
    let t = &v["trend"];
    // 新格式 `ths`；旧格式 `th1` / `th2` 照读
    let saved: Vec<String> = match t["ths"].as_array() {
        Some(a) => a.iter().filter_map(serde_json::Value::as_f64).map(|x| format!("{x}")).collect(),
        None => [t["th1"].as_f64(), t["th2"].as_f64()].into_iter().flatten().map(|x| format!("{x}")).collect(),
    };
    let ths = parse_ths(&saved).unwrap_or_else(|_| THS.to_vec());
    UiPrefs {
        col_w,
        trend_ms: t["ms"]
            .as_u64()
            .map_or(TREND_MS, |x| (x as u32).clamp(TREND_MIN_MS, TREND_MAX_MS)),
        ths: ths.into_iter().take(TREND_LEVELS).collect(),
    }
}

/// 把自适应算出的宽度**固定下来**：只补还没有宽度的列（第一次打开、双击恢复、全部重新自适应之后），
/// 已有的一概不动——之后数据怎么变列宽都不跟着跳，只由用户拖。补过才写盘。
pub fn freeze_missing(auto: &[(Col, f32)]) {
    let w = {
        let Ok(mut g) = cell().lock() else { return };
        let mut added = false;
        for (c, x) in auto {
            if !g.col_w.contains_key(c) {
                g.col_w.insert(*c, x.clamp(COL_MIN, COL_MAX));
                added = true;
            }
        }
        if !added {
            return;
        }
        UiPrefs::of(&g)
    };
    save_ui(&w);
}

fn save_ui(p: &UiPrefs) {
    let o: serde_json::Map<String, serde_json::Value> = p
        .col_w
        .iter()
        .map(|(c, x)| (c.key().to_string(), serde_json::json!(x.round())))
        .collect();
    let path = ui_path();
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new(".")));
    let v = serde_json::json!({
        "col_w": o,
        "trend": { "ms": p.trend_ms, "ths": p.ths },
    });
    let _ = std::fs::write(path, v.to_string());
}

/// 当前视图状态的副本。
#[must_use]
pub fn state() -> ViewState {
    cell().lock().map(|g| g.clone()).unwrap_or_default()
}

/// 处理一条面板消息。返回 `Some(x)` = 请上层把表头横向滚到 `x`（与表体同步）。
pub fn handle(m: FeatureMatrixMsg) -> Option<f32> {
    match m {
        FeatureMatrixMsg::TableScrolled(x) => return Some(x),
        FeatureMatrixMsg::DragEnd | FeatureMatrixMsg::ResetWidths | FeatureMatrixMsg::AutoCol(_) => {
            let (w, changed) = {
                let Ok(mut g) = cell().lock() else { return None };
                // 鼠标每次离开表头都会发 DragEnd：没在拖就什么都不做，别每次都写文件
                let changed = !matches!(m, FeatureMatrixMsg::DragEnd) || g.drag.is_some();
                apply(&mut g, m);
                (UiPrefs::of(&g), changed)
            };
            if changed {
                save_ui(&w);
            }
        }
        FeatureMatrixMsg::TrendLookback(_) | FeatureMatrixMsg::TrendTh(..) => {
            let (p, changed) = {
                let Ok(mut g) = cell().lock() else { return None };
                let before = UiPrefs::of(&g);
                apply(&mut g, m);
                let now = UiPrefs::of(&g);
                let changed = now != before;
                (now, changed)
            };
            if changed {
                save_ui(&p);
            }
        }
        FeatureMatrixMsg::Engine(act) => engine_action(act),
        FeatureMatrixMsg::ChartUi(u) => super::chart_params::handle_ui(u),
        FeatureMatrixMsg::ChartEdit(e) => {
            let snap = super::feature_matrix_readout::snapshot();
            super::chart_params::handle_edit(e, &snap.chart, |label, edit| write_and_restart(label, edit));
        }
        FeatureMatrixMsg::Source(s) => {
            // pane 那一层先截走了（要按返回值清图）；走到这里的只是兜底
            super::feature_source::handle(s);
        }
        // 切到默认 / 全开不动窗口，也不丢上一次的自定义键表（再切回自定义时还在）
        FeatureMatrixMsg::SetMode(mode) => apply_mode(mode, None, None, None),
        FeatureMatrixMsg::WinApply => {
            let w = state().win_edit?;
            let mut pw = config_windows();
            pw.global = (!w.list.is_empty()).then_some(w.list.clone());
            let label = pw.global.as_deref().map_or_else(
                || "默认".to_string(),
                super::feature_matrix_readout::format_windows,
            );
            write_and_restart(format!("全局时间窗口 → {label}"), |v| set_windows(v, &pw));
            if let Ok(mut g) = cell().lock() {
                g.win_edit = None;
            }
        }
        FeatureMatrixMsg::ApplyPicker => {
            let p = state().picker?;
            if p.selected.is_empty() {
                set_engine_note("✗ 一条特征都没选——引擎会退回默认集，没有应用".into());
                return None;
            }
            let windows = match p.windows() {
                Ok(w) => w,
                Err(e) => {
                    set_preset_note(format!("✗ {e}——没有应用"));
                    return None;
                }
            };
            // 草稿与所编辑的选择集一致时，配置里记下它的名字（模式栏据此显示）
            let preset = p.editing.clone().filter(|_| !p.dirty());
            apply_mode("custom", Some(p.selected.into_iter().collect()), preset, Some(windows));
            if let Ok(mut g) = cell().lock() {
                g.picker = None;
            }
        }
        FeatureMatrixMsg::PresetApply(name) => match super::feature_presets::get(&name) {
            Some(p) => apply_mode("custom", Some(p.keys), Some(name), Some(p.windows)),
            None => set_engine_note(format!("✗ 没有选择集「{name}」")),
        },
        FeatureMatrixMsg::PresetExport(name) => {
            set_preset_note(String::new());
            std::thread::spawn(move || {
                let Some(path) = super::feature_presets::ask_save_path(&name) else {
                    return;
                };
                set_preset_note(match super::feature_presets::export_to(&name, &path) {
                    Ok(()) => format!("✔ 已导出「{name}」→ {}", path.display()),
                    Err(e) => format!("✗ {e}"),
                });
            });
        }
        FeatureMatrixMsg::PresetImport => {
            set_preset_note(String::new());
            std::thread::spawn(|| {
                let Some(path) = super::feature_presets::ask_open_path() else {
                    return;
                };
                match super::feature_presets::import_from(&path) {
                    Ok(pr) => {
                        let known = known_keys(&super::feature_matrix_readout::snapshot());
                        let mut skipped = 0;
                        if let Ok(mut g) = cell().lock()
                            && let Some(p) = g.picker.as_mut()
                        {
                            skipped = p.load(&pr, &known);
                        }
                        set_preset_note(format!(
                            "✔ 已导入「{}」（{} 条特征）{}",
                            pr.name,
                            pr.keys.len(),
                            if skipped > 0 { format!("，其中 {skipped} 条当前字典里没有，已跳过") } else { String::new() }
                        ));
                    }
                    Err(e) => set_preset_note(format!("✗ 导入 {} 失败：{e}", path.display())),
                }
            });
        }
        m => {
            // 选择页的批量「恢复默认」与初始勾选要读快照（默认集、当前启用集）
            let snap = super::feature_matrix_readout::snapshot();
            let Ok(mut g) = cell().lock() else { return None };
            apply_with(&mut g, m, &snap);
        }
    }
    None
}

/// 启用集配置文件。**必须与主仓 `feature_config::config_path()` 算出同一个路径**。
#[must_use]
pub fn config_path() -> std::path::PathBuf {
    std::env::var("WS_FEATURE_CONFIG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| super::paths::data_dir().join("cockpit").join("feature_config.json"))
}

/// 整份配置（没有文件 / 写坏了 = 空对象，与引擎「退回默认」同一约定）。
#[must_use]
pub fn config_value() -> serde_json::Value {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}))
}

/// 配置文件里的 `(模式, 自定义键表)`。
#[must_use]
pub fn read_config() -> (String, Vec<String>) {
    let v = config_value();
    let mode = v["mode"].as_str().unwrap_or("default").to_string();
    let keys = v["keys"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    (mode, keys)
}

/// 当前配置记着的选择集名（应用的是某个已存选择集时才有）。
#[must_use]
pub fn config_preset() -> Option<String> {
    config_value()["preset"].as_str().map(str::to_string)
}

/// 当前配置的时间窗口（全局一组 + 单条覆盖）。
#[must_use]
pub fn config_windows() -> super::feature_presets::PresetWindows {
    super::feature_presets::PresetWindows::from_value(&config_value()["windows"])
}

/// 在配置对象上改启用集（纯函数，测试用）。`keys` / `preset` / `windows` 为 `None` 的保持原样。
pub fn edit_config(
    v: &mut serde_json::Value,
    mode: &str,
    keys: Option<&[String]>,
    preset: Option<Option<&str>>,
    windows: Option<&super::feature_presets::PresetWindows>,
) {
    if !v.is_object() {
        *v = serde_json::json!({});
    }
    v["mode"] = serde_json::json!(mode);
    if let Some(k) = keys {
        v["keys"] = serde_json::json!(k);
    }
    match preset {
        Some(Some(p)) => v["preset"] = serde_json::json!(p),
        Some(None) => {
            if let Some(o) = v.as_object_mut() {
                o.remove("preset");
            }
        }
        None => {}
    }
    if let Some(w) = windows {
        set_windows(v, w);
    }
}

/// 在配置对象上写窗口段（默认窗口 = 删掉这一段）。
pub fn set_windows(v: &mut serde_json::Value, w: &super::feature_presets::PresetWindows) {
    if !v.is_object() {
        *v = serde_json::json!({});
    }
    if w.is_default() {
        if let Some(o) = v.as_object_mut() {
            o.remove("windows");
        }
    } else {
        v["windows"] = w.to_value();
    }
}

/// 读整份配置、按 `edit` 改、原子写回（临时文件 + rename：引擎恰好在读时不会读到半个文件），再重启引擎。
fn write_and_restart(label: String, edit: impl FnOnce(&mut serde_json::Value)) {
    let p = config_path();
    let mut v = config_value();
    edit(&mut v);
    let tmp = p.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(&v).unwrap_or_default();
    let r = std::fs::create_dir_all(p.parent().unwrap_or(std::path::Path::new(".")))
        .and_then(|()| std::fs::write(&tmp, text))
        .and_then(|()| std::fs::rename(&tmp, &p));
    if let Err(e) = r {
        set_engine_note(format!("✗ 写 {} 失败：{e}", p.display()));
        return;
    }
    set_engine_note(format!("{label}，正在重启引擎…"));
    std::thread::spawn(move || {
        let r = super::procs::action(ENGINE_KEY, "restart");
        set_engine_note(format!("{label}：{r}（窗口从头累计，几分钟后才有长窗口的值）"));
    });
}

static PRESET_NOTE: OnceLock<Mutex<String>> = OnceLock::new();

/// 选择集操作的回执（显示在选择页的选择集栏里）。
#[must_use]
pub fn preset_note() -> String {
    PRESET_NOTE.get_or_init(|| Mutex::new(String::new())).lock().map(|g| g.clone()).unwrap_or_default()
}

fn set_preset_note(t: String) {
    if let Ok(mut g) = PRESET_NOTE.get_or_init(|| Mutex::new(String::new())).lock() {
        *g = t;
    }
}

fn known_keys(m: &super::feature_matrix_readout::Matrix) -> std::collections::BTreeSet<String> {
    m.slots.iter().map(|s| s.key.clone()).collect()
}

/// 切启用集并重启引擎。`keys` / `windows` 为 `None` 时保留配置里原有的。
fn apply_mode(
    mode: &'static str,
    keys: Option<Vec<String>>,
    preset: Option<String>,
    windows: Option<super::feature_presets::PresetWindows>,
) {
    let n = keys.as_ref().map_or_else(|| read_config().1.len(), Vec::len);
    let label = match (mode, &preset) {
        ("all", _) => "全开".to_string(),
        ("custom", Some(p)) => format!("选择集「{p}」（{n} 条）"),
        ("custom", None) => format!("自定义（{n} 条）"),
        _ => "默认".to_string(),
    };
    write_and_restart(format!("启用集 → {label}"), |v| {
        edit_config(v, mode, keys.as_deref(), Some(preset.as_deref()), windows.as_ref());
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
            super::feature_presets::invalidate();
            set_preset_note(String::new());
            let (mode, keys) = read_config();
            let mut p = Picker::default();
            // 当前生效的是某个已存选择集 → 直接进入编辑它
            match config_preset().and_then(|n| super::feature_presets::get(&n)).filter(|_| mode == "custom") {
                Some(pr) => {
                    p.load(&pr, &known_keys(snap));
                }
                None => {
                    p.selected = if mode == "custom" && !keys.is_empty() {
                        keys.into_iter().collect()
                    } else {
                        snap.enabled_keys().into_iter().collect()
                    };
                    p.set_windows(&config_windows());
                }
            }
            st.picker = Some(p);
        }
        FeatureMatrixMsg::WinOpen => {
            st.win_edit = Some(WinEdit {
                list: config_windows().global.unwrap_or_default(),
                ..WinEdit::default()
            });
        }
        FeatureMatrixMsg::PresetLoad(name) => {
            let Some(p) = st.picker.as_mut() else { return };
            match super::feature_presets::get(&name) {
                Some(pr) => {
                    let skipped = p.load(&pr, &known_keys(snap));
                    set_preset_note(if skipped > 0 {
                        format!("已载入「{name}」；其中 {skipped} 条当前字典里没有，已跳过（点「更新」会把它们从选择集里去掉）")
                    } else {
                        format!("已载入「{name}」，改完点「更新」保存")
                    });
                }
                None => set_preset_note(format!("✗ 没有选择集「{name}」")),
            }
        }
        FeatureMatrixMsg::PresetSaveNew => {
            let Some(p) = st.picker.as_mut() else { return };
            let keys: Vec<String> = p.selected.iter().cloned().collect();
            let windows = match p.windows() {
                Ok(w) => w,
                Err(e) => {
                    set_preset_note(format!("✗ {e}"));
                    return;
                }
            };
            match super::feature_presets::save(&p.name, &p.note, &keys, &windows, false) {
                Ok(pr) => {
                    p.editing = Some(pr.name.clone());
                    p.name = pr.name.clone();
                    set_preset_note(format!("✔ 已保存选择集「{}」（{} 条）", pr.name, pr.keys.len()));
                }
                Err(e) => set_preset_note(format!("✗ {e}")),
            }
        }
        FeatureMatrixMsg::PresetUpdate => {
            let Some(p) = st.picker.as_mut() else { return };
            let Some(old) = p.editing.clone() else {
                set_preset_note("✗ 还没保存过——先「另存为新选择集」".into());
                return;
            };
            let keys: Vec<String> = p.selected.iter().cloned().collect();
            let windows = match p.windows() {
                Ok(w) => w,
                Err(e) => {
                    set_preset_note(format!("✗ {e}"));
                    return;
                }
            };
            match super::feature_presets::rename_and_update(&old, &p.name, &p.note, &keys, &windows) {
                Ok(pr) => {
                    let renamed = pr.name != old;
                    p.editing = Some(pr.name.clone());
                    p.name = pr.name.clone();
                    set_preset_note(if renamed {
                        format!("✔ 已更新并改名「{old}」→「{}」（{} 条）", pr.name, pr.keys.len())
                    } else {
                        format!("✔ 已更新「{}」（{} 条）", pr.name, pr.keys.len())
                    });
                }
                Err(e) => set_preset_note(format!("✗ {e}")),
            }
        }
        FeatureMatrixMsg::PresetDelete(name) => {
            let Some(p) = st.picker.as_mut() else { return };
            if p.confirm_delete.as_deref() != Some(name.as_str()) {
                p.confirm_delete = Some(name.clone());
                set_preset_note(format!("再点一次「确认删除」删掉「{name}」（不可恢复；导出过的文件不受影响）"));
                return;
            }
            p.confirm_delete = None;
            match super::feature_presets::delete(&name) {
                Ok(()) => {
                    if p.editing.as_deref() == Some(name.as_str()) {
                        p.editing = None;
                    }
                    set_preset_note(format!("✔ 已删除「{name}」"));
                }
                Err(e) => set_preset_note(format!("✗ {e}")),
            }
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
        FeatureMatrixMsg::PresetName(n) => {
            if let Some(p) = st.picker.as_mut() {
                p.name = n;
            }
        }
        FeatureMatrixMsg::PresetNote(n) => {
            if let Some(p) = st.picker.as_mut() {
                p.note = n;
            }
        }
        FeatureMatrixMsg::PickGlobalWindows(t) => {
            if let Some(p) = st.picker.as_mut() {
                p.global_text = t;
            }
        }
        FeatureMatrixMsg::PickWindows(k, t) => {
            if let Some(p) = st.picker.as_mut() {
                if t.trim().is_empty() {
                    p.win_text.remove(&k);
                } else {
                    p.win_text.insert(k, t);
                }
            }
        }
        FeatureMatrixMsg::WinClose => st.win_edit = None,
        FeatureMatrixMsg::SetTable(t) => st.table = t,
        FeatureMatrixMsg::SetMetric(m) => st.metric = m,
        FeatureMatrixMsg::HoverIn(k) => st.hover = Some(k),
        FeatureMatrixMsg::HoverOut(k) => {
            if st.hover.as_deref() == Some(k.as_str()) {
                st.hover = None;
            }
        }
        FeatureMatrixMsg::ToggleFoldControls => st.fold_controls = !st.fold_controls,
        FeatureMatrixMsg::HeaderMove(x) => {
            st.mouse_x = x;
            if let Some((c, x0, w0)) = st.drag {
                st.col_w.insert(c, (w0 + x - x0).clamp(COL_MIN, COL_MAX));
            }
        }
        FeatureMatrixMsg::DragStart(c, w) => st.drag = Some((c, st.mouse_x, w)),
        FeatureMatrixMsg::DragEnd => st.drag = None,
        FeatureMatrixMsg::ResetWidths => {
            st.col_w.clear();
            st.drag = None;
        }
        FeatureMatrixMsg::AutoCol(c) => {
            st.col_w.remove(&c);
            st.drag = None;
        }
        FeatureMatrixMsg::TrendLookback(t) => {
            match super::feature_matrix_readout::parse_window(&t) {
                Ok(ms) if (TREND_MIN_MS..=TREND_MAX_MS).contains(&ms) => {
                    st.trend_ms = ms;
                    st.trend_err.clear();
                }
                Ok(_) => st.trend_err = "比较时长要在 1s ~ 10m 之间".into(),
                Err(e) => st.trend_err = e,
            }
            st.trend_text = t;
        }
        FeatureMatrixMsg::TrendTh(i, t) => {
            if let Some(slot) = st.th_text.get_mut(i) {
                *slot = t;
            }
            match parse_ths(&st.th_text) {
                Ok(v) => {
                    st.ths = v;
                    st.trend_err.clear();
                }
                Err(e) => st.trend_err = format!("{e}（没有生效，仍按 {} 显示）", fmt_ths(&st.ths)),
            }
        }
        FeatureMatrixMsg::WinInput(t) => {
            if let Some(w) = st.win_edit.as_mut() {
                w.input = t;
                w.err.clear();
            }
        }
        FeatureMatrixMsg::WinAdd => {
            if let Some(w) = st.win_edit.as_mut() {
                match super::feature_matrix_readout::parse_windows(&w.input) {
                    Ok(v) => {
                        w.list.extend(v);
                        w.list.sort_unstable();
                        w.list.dedup();
                        if w.list.len() > super::feature_matrix_readout::MAX_WINDOWS {
                            w.list.truncate(super::feature_matrix_readout::MAX_WINDOWS);
                            w.err = format!("至多 {} 个窗口，多出的已丢掉", super::feature_matrix_readout::MAX_WINDOWS);
                        }
                        w.input.clear();
                    }
                    Err(e) => w.err = e,
                }
            }
        }
        FeatureMatrixMsg::WinRemove(x) => {
            if let Some(w) = st.win_edit.as_mut() {
                w.list.retain(|y| *y != x);
            }
        }
        FeatureMatrixMsg::WinQuick(i) => {
            if let (Some(w), Some((_, ws))) = (st.win_edit.as_mut(), WINDOW_PRESETS.get(i)) {
                w.list = ws.to_vec();
                w.err.clear();
            }
        }
        FeatureMatrixMsg::WinDefault => {
            if let Some(w) = st.win_edit.as_mut() {
                w.list.clear();
                w.err.clear();
            }
        }
        FeatureMatrixMsg::PresetNew => {
            if let Some(p) = st.picker.as_mut() {
                p.editing = None;
                p.name.clear();
                p.note.clear();
                p.confirm_delete = None;
            }
        }
        // 有副作用或要读快照的，不在这里（见 `handle` / `apply_with`）
        FeatureMatrixMsg::Engine(_)
        | FeatureMatrixMsg::Source(_)
        | FeatureMatrixMsg::ChartEdit(_)
        | FeatureMatrixMsg::ChartUi(_)
        | FeatureMatrixMsg::SetMode(_)
        | FeatureMatrixMsg::ApplyPicker
        | FeatureMatrixMsg::OpenPicker
        | FeatureMatrixMsg::PickBulk(..)
        | FeatureMatrixMsg::PresetLoad(_)
        | FeatureMatrixMsg::PresetSaveNew
        | FeatureMatrixMsg::PresetUpdate
        | FeatureMatrixMsg::PresetDelete(_)
        | FeatureMatrixMsg::PresetExport(_)
        | FeatureMatrixMsg::PresetImport
        | FeatureMatrixMsg::PresetApply(_)
        | FeatureMatrixMsg::WinOpen
        | FeatureMatrixMsg::WinApply
        | FeatureMatrixMsg::TableScrolled(_) => {}
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
        apply(&mut st, FeatureMatrixMsg::SetView(View::Engine));
        assert_eq!(st.view, View::Engine);
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
    fn 载入选择集跳过字典里没有的键并进入编辑() {
        let pr = super::super::feature_presets::Preset {
            name: "盘口".into(),
            note: "n".into(),
            keys: vec!["mid_price".into(), "改名前的旧键".into()],
            updated: String::new(),
            windows: {
                let mut w = super::super::feature_presets::PresetWindows {
                    global: Some(vec![60_000, 900_000]),
                    ..Default::default()
                };
                w.overrides.insert("mid_price".into(), vec![10_000]);
                w
            },
        };
        let known = known_keys(&snap());
        let mut p = Picker::default();
        assert_eq!(p.load(&pr, &known), 1);
        // 窗口随选择集载入，文本能原样解析回去
        assert_eq!(p.global_text, "1m · 15m");
        assert_eq!(p.windows().unwrap(), pr.windows);
        p.win_text.insert("obi_l2".into(), "abc".into());
        assert!(p.windows().is_err(), "写错的窗口要报出来，不能悄悄丢掉");
        assert_eq!(p.selected.len(), 1);
        assert_eq!(p.editing.as_deref(), Some("盘口"));
        assert_eq!(p.name, "盘口");
        assert_eq!(p.note, "n");
        // 新建 = 保留勾选、脱离已存选择集
        let mut st = ViewState { picker: Some(p), ..ViewState::default() };
        apply(&mut st, FeatureMatrixMsg::PresetNew);
        let p = st.picker.unwrap();
        assert!(p.editing.is_none() && p.name.is_empty());
        assert_eq!(p.selected.len(), 1, "新建不清勾选：从当前勾选开始");
    }

    #[test]
    fn 涨跌箭头分级与设置校验() {
        let ths = [0.5, 1.0, 1.5, 2.0, 3.0];
        assert_eq!(trend_level(0.49, &ths), 0);
        assert_eq!(trend_level(0.5, &ths), 1);
        assert_eq!(trend_level(1.6, &ths), 3);
        assert_eq!(trend_level(9.0, &ths), 5, "封顶 5 档");
        assert_eq!(trend_level(-2.2, &ths), -4);
        assert_eq!(trend_level(2.2, &THS), 2, "只设两档时封顶两个箭头");
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_string()).collect::<Vec<_>>();
        assert_eq!(parse_ths(&s(&["0.5", "1.5", "", "", ""])).unwrap(), vec![0.5, 1.5]);
        assert!(parse_ths(&s(&["0.5", "", "2", "", ""])).is_err(), "中间不能空");
        assert!(parse_ths(&s(&["1", "0.8", "", "", ""])).is_err(), "要逐档递增");
        assert!(parse_ths(&s(&["", "", "", "", ""])).is_err(), "至少一档");
        let mut st = ViewState { trend_ms: TREND_MS, ths: THS.to_vec(), th_text: ths_text(&THS), ..ViewState::default() };
        apply(&mut st, FeatureMatrixMsg::TrendLookback("30s".into()));
        assert_eq!(st.trend_ms, 30_000);
        apply(&mut st, FeatureMatrixMsg::TrendLookback("20m".into()));
        assert_eq!(st.trend_ms, 30_000, "超出历史长度的不生效");
        assert!(!st.trend_err.is_empty());
        apply(&mut st, FeatureMatrixMsg::TrendTh(3, "3".into()));
        assert_eq!(st.ths, THS.to_vec(), "跳过第 3 档填第 4 档不生效");
        apply(&mut st, FeatureMatrixMsg::TrendTh(2, "2".into()));
        assert_eq!(st.ths, vec![0.5, 1.5, 2.0, 3.0]);
        assert!(st.trend_err.is_empty());
    }

    #[test]
    fn 配置json与引擎约定一致() {
        // 引擎侧 `feature_config::FeatureConfig` 读的就是这两个字段
        // 改启用集不丢窗口段，改窗口不丢启用集
        let mut v = serde_json::json!({"mode": "all", "windows": {"global": [60000], "overrides": {}}});
        edit_config(&mut v, "custom", Some(&["mid_price".into(), "obi_l2".into()]), Some(Some("盘口")), None);
        assert_eq!(v["mode"], "custom");
        assert_eq!(v["keys"].as_array().unwrap().len(), 2);
        assert_eq!(v["preset"], "盘口");
        assert_eq!(v["windows"]["global"][0], 60000, "改启用集把窗口段丢了");
        edit_config(&mut v, "all", None, Some(None), None);
        assert!(v["preset"].is_null());
        assert_eq!(v["keys"].as_array().unwrap().len(), 2, "切到全开不该丢自定义键表");
        set_windows(&mut v, &super::super::feature_presets::PresetWindows::default());
        assert!(v["windows"].is_null(), "默认窗口 = 删掉窗口段");
        assert_eq!(v["mode"], "all");
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
