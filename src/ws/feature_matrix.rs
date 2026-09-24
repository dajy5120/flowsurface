//! 特征矩阵面板 — 视图状态与消息（docs/31 §8.1）。
//!
//! 状态放**进程级静态**而不是每个 pane 一份，与 `ws::radar` 同一形状：
//! 开两个矩阵 pane 时两边的筛选一致，「我筛过的」不会因为看的是另一个 pane 而失效。
//!
//! 这里没有任何副作用——面板只读旁路 JSON，不启停守护、不调 Python。
//! 所以 [`handle`] 只改筛选与折叠状态。

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
    let Ok(mut g) = cell().lock() else { return };
    apply(&mut g, m);
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
}
