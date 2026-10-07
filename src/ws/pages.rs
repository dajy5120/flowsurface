//! 工作区页面的元数据（docs/41 §3、§5，B 期）：顺序、用户自建页、关闭的模板页、锁定、模板指纹，以及页面模板库。
//!
//! 页面本身是 `LayoutManager` 里的布局（A 期）；布局内容随 `saved-state.json` 存。这里只存布局之外的东西，
//! 放在同一个数据目录（`data::data_path`）的 `pages.json`——样张实例有自己的数据目录，不会碰到用户的。
//!
//! **模板只是初始值**（B 期）：启动时缺页才按模板建，已有的页一律不动；模板与上次应用时不同，
//! 只在页签上提示「↻ 模板有更新」，用户点「恢复默认」才覆盖。指纹存在 `applied` 里。

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const FILE: &str = "pages.json";
const TEMPLATE_DIR: &str = "page_templates";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PagesState {
    /// 工作区 → 页面布局名的顺序（用户排过序的才有）
    pub order: BTreeMap<String, Vec<String>>,
    /// 用户自建的页（布局名）。启动清理旧页面时不能删它们
    pub user: BTreeSet<String>,
    /// 用户关掉的模板页：启动时不再按模板补建；「＋」里可以找回
    pub closed: BTreeSet<String>,
    /// 锁定布局的页：不能拖动、改大小、关闭面板
    pub locked: BTreeSet<String>,
    /// 模板页上次应用时的模板指纹
    pub applied: BTreeMap<String, String>,
}

pub fn load() -> PagesState {
    std::fs::read_to_string(data::data_path(Some(FILE)))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save(p: &PagesState) {
    match serde_json::to_string_pretty(p) {
        Ok(j) => {
            if let Err(e) = data::write_json_to_file(&j, FILE) {
                log::warn!("写 {FILE} 失败：{e}");
            }
        }
        Err(e) => log::warn!("序列化页面元数据失败：{e}"),
    }
}

/// 模板指纹（FNV-1a 64；只用来判断「变没变」）。
pub fn fingerprint(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

impl PagesState {
    /// 改名：把各处记录里的旧名换成新名。
    pub fn rename(&mut self, old: &str, new: &str) {
        for v in self.order.values_mut() {
            for n in v.iter_mut() {
                if n == old {
                    *n = new.to_string();
                }
            }
        }
        for set in [&mut self.user, &mut self.closed, &mut self.locked] {
            if set.remove(old) {
                set.insert(new.to_string());
            }
        }
        if let Some(f) = self.applied.remove(old) {
            self.applied.insert(new.to_string(), f);
        }
    }

    pub fn forget(&mut self, name: &str) {
        for v in self.order.values_mut() {
            v.retain(|n| n != name);
        }
        self.user.remove(name);
        self.locked.remove(name);
        self.applied.remove(name);
    }
}

// ── 页面操作消息（页签栏的「⋯ 页面」工具条与命令面板共用）──────────

#[derive(Debug, Clone)]
pub enum PageMsg {
    ToggleMenu,
    NewBlank,
    Duplicate,
    /// 找回一个关掉的模板页（布局名）
    Reopen(String),
    /// 从模板库文件新建
    FromFile(PathBuf),
    RenameInput(String),
    RenameApply,
    /// 当前页左移（-1）/ 右移（+1）
    Move(i8),
    Close,
    /// 切到相邻页之后真正删掉（内部用）
    Remove(uuid::Uuid),
    /// 找回本次运行里最近关掉的页（Ctrl Shift T）
    Undo,
    ResetDefault,
    SaveTemplate,
    ToggleLock,
    /// 面板库：搜索框
    LibrarySearch(String),
    /// 面板库：加到浮动层（只对图表类）还是平铺层
    LibraryFloat(bool),
    /// 面板库：加一个面板到当前页
    AddPanel(ContentKind),
    /// 本页图表联动品种（docs/41 E 期）
    LinkSymbols(bool),
}

static ACTIVE_LOCKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 当前页是否锁定布局（面板标题栏与 pane 网格据此禁拖动 / 改大小 / 关闭；只有当前页在画）。
pub fn active_locked() -> bool {
    ACTIVE_LOCKED.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn set_active_locked(v: bool) {
    ACTIVE_LOCKED.store(v, std::sync::atomic::Ordering::Relaxed);
}

// ── 面板库（docs/41 D 期）──────────────────────────────────────────

use data::layout::pane::ContentKind;

/// 单实例面板：交互状态（选中、表单、列宽、筛选）只有一份，同一页里放两个会互相牵动——
/// 面板库加它时，本页已有就跳过去（别的页里有不算，几页不会同时显示）。
pub const SINGLE_INSTANCE: [ContentKind; 4] =
    [ContentKind::StrategyCenter, ContentKind::StrategyLayers, ContentKind::FeatureMatrix, ContentKind::Orders];

pub fn single_instance(k: ContentKind) -> bool {
    SINGLE_INSTANCE.contains(&k)
}

/// 能进浮动层的面板类型（与 `pane::State::is_chart_kind` 对应）。
pub fn floatable(k: ContentKind) -> bool {
    matches!(
        k,
        ContentKind::HeatmapChart
            | ContentKind::ShaderHeatmap
            | ContentKind::FootprintChart
            | ContentKind::CandlestickChart
            | ContentKind::ComparisonChart
            | ContentKind::TimeAndSales
            | ContentKind::Ladder
            | ContentKind::SelfChart
            | ContentKind::OfmsLab
    )
}

/// 面板库的分类。
pub fn category(k: ContentKind) -> &'static str {
    use ContentKind as K;
    match k {
        K::HeatmapChart | K::ShaderHeatmap | K::FootprintChart | K::CandlestickChart | K::ComparisonChart | K::TimeAndSales | K::Ladder => "行情",
        K::FeatureMatrix | K::OfmsLab | K::FeatureLab => "订单流",
        K::Factory | K::C4Shadow | K::OptionsBoard | K::PredictionBoard | K::PmBinance | K::PmReplay | K::StrategyCenter | K::StrategyLayers => "研究",
        K::WealthSpring | K::SelfChart | K::BacktestResult | K::Orders => "策略",
        K::MarketMap | K::News | K::Recorder | K::TardisReplay | K::TardisBoard | K::Observatory => "数据与资讯",
        K::Procs | K::NetEgress => "系统",
        K::Starter => "其他",
    }
}

// ── 页面模板库（存为模板 / 导出 / 导入）──────────────────────────────

/// 模板库目录：每个模板一份 JSON（`data::Dashboard` 外加名字与来源工作区）。
/// 导出 = 存进这个目录；另一台机器把文件拷进它的同名目录就是导入。
pub fn template_dir() -> PathBuf {
    data::data_path(Some(TEMPLATE_DIR))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageTemplate {
    pub title: String,
    pub workspace: String,
    pub dashboard: data::Dashboard,
}

pub fn save_template(t: &PageTemplate) -> Result<PathBuf, String> {
    let dir = template_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let safe: String = format!("{}-{}", t.workspace, t.title)
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let p = dir.join(format!("{safe}.json"));
    let j = serde_json::to_string_pretty(t).map_err(|e| e.to_string())?;
    std::fs::write(&p, j).map_err(|e| e.to_string())?;
    Ok(p)
}

/// 模板库里的全部模板（读不了的文件跳过）。
pub fn templates() -> Vec<(PathBuf, PageTemplate)> {
    let mut out: Vec<(PathBuf, PageTemplate)> = std::fs::read_dir(template_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| {
            let t = std::fs::read_to_string(e.path()).ok()?;
            Some((e.path(), serde_json::from_str::<PageTemplate>(&t).ok()?))
        })
        .collect();
    out.sort_by(|a, b| (&a.1.workspace, &a.1.title).cmp(&(&b.1.workspace, &b.1.title)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_moves_every_record() {
        let mut p = PagesState::default();
        p.order.insert("回测".into(), vec!["回测".into(), "回测｜甲".into()]);
        p.user.insert("回测｜甲".into());
        p.locked.insert("回测｜甲".into());
        p.applied.insert("回测｜甲".into(), "x".into());
        p.rename("回测｜甲", "回测｜乙");
        assert_eq!(p.order["回测"], ["回测", "回测｜乙"]);
        assert!(p.user.contains("回测｜乙") && p.locked.contains("回测｜乙") && p.applied.contains_key("回测｜乙"));
        p.forget("回测｜乙");
        assert_eq!(p.order["回测"], ["回测"]);
        assert!(p.user.is_empty() && p.locked.is_empty());
    }

    #[test]
    fn library_classification_covers_every_kind() {
        for k in ContentKind::ALL {
            assert!(!category(k).is_empty());
        }
        // 单实例的都不是能浮动的图表（浮动层里不会冒出第二个）
        assert!(SINGLE_INSTANCE.iter().all(|k| !floatable(*k)));
        // 订单流层析已改成每个面板各自的视图状态（D 期），可以多开、可以浮动
        assert!(!single_instance(ContentKind::OfmsLab) && floatable(ContentKind::OfmsLab));
    }

    #[test]
    fn fingerprint_changes_with_content() {
        assert_eq!(fingerprint("a"), fingerprint("a"));
        assert_ne!(fingerprint("a"), fingerprint("b"));
    }

    #[test]
    fn old_file_without_fields_still_loads() {
        let p: PagesState = serde_json::from_str("{}").unwrap();
        assert_eq!(p, PagesState::default());
    }
}
