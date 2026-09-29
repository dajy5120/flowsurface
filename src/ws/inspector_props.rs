//! 检查器里的「面板可编辑属性」（docs/35 §16.5 第 3 项，UPDS V2 §11：属性在检查器里改）。
//!
//! 检查器打开时，聚焦面板的可编辑属性（数据选择器、口径设置）搬到检查器里显示，面板原处
//! 收成一行提示；检查器关着时仍在面板里内联显示——关掉检查器不会让这些设置找不到。
//!
//! # 消息怎么回去
//!
//! 检查器里的控件发的是**和面板里一模一样的 pane 事件**（`Event::FeatureMatrixInteraction`、
//! `Event::BacktestLaunchInteraction` ……），由 main 包成 `Dashboard → Pane → PaneEvent(聚焦的
//! pane, 事件)` 送回原来的处理函数。所以副作用（清图、横滚表头、写配置重启引擎）完全不变，
//! 这里不另写一套处理逻辑。
//!
//! # 谁托管着
//!
//! [`set_host`] 由 main 在每次画界面之前写入：检查器开着 → 聚焦的 pane；否则 `None`。
//! 面板视图用 [`hosted`] 判断自己的可编辑属性是不是正被检查器托管。

use std::sync::Mutex;

use iced::widget::{column, pane_grid, text};
use iced::{Element, Length};

use crate::screen::dashboard::pane::{Content, Event};

static HOST: Mutex<Option<pane_grid::Pane>> = Mutex::new(None);

/// 检查器正在托管哪个 pane 的可编辑属性（检查器关着 = `None`）。
pub fn set_host(p: Option<pane_grid::Pane>) {
    if let Ok(mut g) = HOST.lock() {
        *g = p;
    }
}

/// 这个 pane 的可编辑属性正显示在检查器里吗。
pub fn hosted(p: pane_grid::Pane) -> bool {
    HOST.lock().is_ok_and(|g| *g == Some(p))
}

/// 面板原处的一行提示（可编辑属性已搬进检查器）。
pub fn hint<'a, M: 'a>(what: &str) -> Element<'a, M> {
    text(format!("{what}在右侧检查器中编辑（Ctrl I 收起检查器后回到这里）"))
        .size(crate::ui::text::s_meta())
        .color(crate::ui::pal::dim())
        .into()
}

/// 聚焦面板的可编辑属性。没有可编辑属性的面板返回 `None`（检查器照常只显示信息）。
///
/// 返回的元素发 pane 事件，调用方负责包成送回聚焦 pane 的消息。
pub fn view<'a>(content: &'a Content) -> Option<Element<'a, Event>> {
    let section = |title: &str| crate::ui::text::metadata(title.to_string());
    match content {
        Content::FeatureMatrix => {
            use super::feature_matrix::FeatureMatrixMsg as Fm;
            let src = super::feature_source::view();
            let mut b = column![
                section("数据源"),
                text(src.reading.clone()).size(crate::ui::text::s_small()).color(crate::ui::pal::txt()),
                super::data_picker_view::view(&src.pick, &super::feature_source::pick_opts())
                    .map(|m| Event::FeatureMatrixInteraction(Fm::Source(super::feature_source::SourceMsg::Data(m)))),
            ]
            .spacing(crate::ui::metrics::space(2));
            let m = super::feature_matrix_readout::snapshot();
            if m.chart.present && !m.chart.editable.is_empty() {
                b = b.push(section("图表参数口径")).push(
                    super::chart_params_view::edit_block(&m).map(Event::FeatureMatrixInteraction),
                );
            }
            Some(b.width(Length::Fill).into())
        }
        Content::BacktestResult => {
            let v = super::backtest_launch::view();
            Some(
                column![
                    section("回测数据"),
                    super::data_picker_view::view(&v.pick, &super::backtest_launch::pick_opts())
                        .map(|m| Event::BacktestLaunchInteraction(super::backtest_launch::LaunchMsg::Data(m))),
                ]
                .spacing(crate::ui::metrics::space(2))
                .into(),
            )
        }
        Content::TardisBoard(tb) => Some(
            column![
                section("数据"),
                super::data_picker_view::view(&tb.pick, &super::tardis_board::pick_opts())
                    .map(|m| Event::TardisBoardInteraction(super::tardis_board::TardisBoardMsg::Data(m))),
            ]
            .spacing(crate::ui::metrics::space(2))
            .into(),
        ),
        Content::TardisReplay(tr) => Some(
            column![
                section("数据"),
                super::data_picker_view::view(&tr.pick, &super::tardis_replay::pick_opts())
                    .map(|m| Event::TardisReplayInteraction(super::tardis_replay::TardisReplayMsg::Data(m))),
            ]
            .spacing(crate::ui::metrics::space(2))
            .into(),
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 托管只认一个_pane() {
        let mut ids = Vec::new();
        let (mut st, a) = pane_grid::State::new(());
        let (b, _) = st.split(pane_grid::Axis::Vertical, a, ()).expect("split");
        ids.push(a);
        ids.push(b);
        set_host(Some(a));
        assert!(hosted(a));
        assert!(!hosted(b));
        set_host(None);
        assert!(!hosted(a), "检查器关了：面板收回内联显示");
    }

    #[test]
    fn 没有可编辑属性的面板不占检查器() {
        assert!(view(&Content::Orders).is_none());
        assert!(view(&Content::Procs).is_none());
    }
}
