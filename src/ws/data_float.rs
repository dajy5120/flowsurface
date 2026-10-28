//! 数据浮窗（docs/64 §4，W3）：工具栏「数据 ▾」点开的「管线 → 来源 → 市场 → 标的 → 时间」。
//!
//! 浮窗里改的是一份**草稿**：层级深，中间几步没选完时状态是无效的，边选边生效会让面板反复重载。
//! 点「应用」才经宿主原来的数据消息整份交过去（[`DataPickMsg::Replace`]），「取消」丢掉草稿。
//! 草稿跟着打开它的那个面板（`pane`）：工具栏换到别的面板时浮窗不显示，换回来还在。

use std::sync::Mutex;

use iced::widget::{column, row, text, Space};
use iced::{Alignment, Element, Length};

use super::data_picker::{DataPick, DataPickMsg, PickOpts};
use crate::ui::{metrics, pal, widgets as w};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataFloatMsg {
    /// 打开（面板, 它现在的选择）；已开着同一个面板 = 关
    Open(uuid::Uuid, Box<DataPick>),
    Edit(DataPickMsg),
    Apply,
    Cancel,
}

struct Draft {
    pane: uuid::Uuid,
    pick: DataPick,
}

static DRAFT: Mutex<Option<Draft>> = Mutex::new(None);

fn draft() -> std::sync::MutexGuard<'static, Option<Draft>> {
    DRAFT.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// 浮窗开给了哪个面板。
#[must_use]
pub fn open_for() -> Option<uuid::Uuid> {
    draft().as_ref().map(|d| d.pane)
}

/// 处理消息；点「应用」时返回（面板, 新的选择），由宿主交给那个面板。
pub fn handle(m: DataFloatMsg) -> Option<(uuid::Uuid, DataPick)> {
    let mut g = draft();
    match m {
        DataFloatMsg::Open(pane, pick) => {
            if g.as_ref().is_some_and(|d| d.pane == pane) {
                *g = None;
            } else {
                *g = Some(Draft { pane, pick: *pick });
            }
        }
        DataFloatMsg::Edit(e) => {
            if let Some(d) = g.as_mut() {
                d.pick.update(e);
            }
        }
        DataFloatMsg::Apply => return g.take().map(|d| (d.pane, d.pick)),
        DataFloatMsg::Cancel => *g = None,
    }
    None
}

/// 浮窗内容：选择器 + 选到了什么 + 应用 / 取消。`current` = 面板现在用着的选择（对照用）。
pub fn view<'a>(opts: &PickOpts, current: &DataPick) -> Option<Element<'a, DataFloatMsg>> {
    let g = draft();
    let d = g.as_ref()?;
    let sel = d.pick.selection(opts);
    let ready = sel.is_some();
    // 比选到的结果，不比整份（搜索框里打的字不算改动）
    let changed = sel != current.selection(opts);
    let now = super::data_picker_view::short(current, opts);
    Some(
        column![
            super::data_picker_view::view(&d.pick, opts).map(DataFloatMsg::Edit),
            text(format!("现在用着：{now}")).size(crate::ui::text::s_meta()).color(pal::dim()),
            row![
                Space::new().width(Length::Fill),
                w::btn("取消", w::Kind::Standard, Some(DataFloatMsg::Cancel)),
                w::btn_why(
                    "应用",
                    w::Kind::Primary,
                    (ready && changed).then_some(DataFloatMsg::Apply),
                    if ready { "和现在用着的一样" } else { "还没选完：选到最后一级才能应用" },
                ),
            ]
            .spacing(metrics::space(2))
            .align_y(Alignment::Center),
        ]
        .spacing(metrics::space(3))
        .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 草稿_应用才交出去_取消丢掉_再点同一个面板是关() {
        let a = uuid::Uuid::new_v4();
        let mut p = DataPick::default();
        p.start = "08:00".into();
        handle(DataFloatMsg::Open(a, Box::new(p.clone())));
        assert_eq!(open_for(), Some(a));
        handle(DataFloatMsg::Edit(DataPickMsg::Start("09:30".into())));
        let (pane, got) = handle(DataFloatMsg::Apply).expect("应用交出草稿");
        assert_eq!((pane, got.start.as_str()), (a, "09:30"));
        assert_eq!(open_for(), None);
        // 宿主收到整份替换
        let mut host = p.clone();
        host.update(DataPickMsg::Replace(Box::new(got)));
        assert_eq!(host.start, "09:30");
        // 取消：草稿丢掉；再点同一个面板：关
        handle(DataFloatMsg::Open(a, Box::new(p.clone())));
        handle(DataFloatMsg::Cancel);
        assert_eq!(open_for(), None);
        handle(DataFloatMsg::Open(a, Box::new(p.clone())));
        handle(DataFloatMsg::Open(a, Box::new(p)));
        assert_eq!(open_for(), None);
    }
}
