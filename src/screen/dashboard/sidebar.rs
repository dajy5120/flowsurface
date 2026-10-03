use super::tickers_table::{self, TickersTable};
use crate::{
    TooltipPosition,
    layout::SavedState,
    style::{Icon, icon_text},
    widget::button_with_tooltip,
};
use data::sidebar;

use iced::{
    Alignment, Element, Subscription, Task,
    widget::responsive,
    widget::{column, row, scrollable, space},
};
use rustc_hash::FxHashMap;

#[derive(Debug, Clone)]
pub enum Message {
    ToggleSidebarMenu(Option<sidebar::Menu>),
    SetSidebarPosition(sidebar::Position),
    TickersTable(super::tickers_table::Message),
    /// WealthSpring 工作区切换（docs/08 F6 — P1）：合并进侧边栏的工作区图标按钮。
    SelectWorkspace(uuid::Uuid),
}

pub struct Sidebar {
    pub state: data::Sidebar,
    pub tickers_table: TickersTable,
}

pub enum Action {
    TickerSelected(
        exchange::TickerInfo,
        Option<data::layout::pane::ContentKind>,
    ),
    ErrorOccurred(data::InternalError),
    /// 工作区切换 → main.rs 复用 LayoutManager 的 SelectActive 流程（docs/08 F6 — P1）。
    SelectWorkspace(uuid::Uuid),
}

impl Sidebar {
    pub fn new(
        state: &SavedState,
        handles: exchange::adapter::AdapterHandles,
    ) -> (Self, Task<Message>) {
        let (tickers_table, initial_fetch) =
            if let Some(settings) = state.sidebar.tickers_table.as_ref() {
                TickersTable::new_with_settings(settings, handles.clone())
            } else {
                TickersTable::new(handles)
            };

        (
            Self {
                state: state.sidebar.clone(),
                tickers_table,
            },
            initial_fetch.map(Message::TickersTable),
        )
    }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Action>) {
        match message {
            Message::ToggleSidebarMenu(menu) => {
                self.set_menu(menu.filter(|&m| !self.is_menu_active(m)));
            }
            Message::SetSidebarPosition(position) => {
                self.state.position = position;
            }
            Message::SelectWorkspace(id) => {
                return (Task::none(), Some(Action::SelectWorkspace(id)));
            }
            Message::TickersTable(msg) => {
                let action = self.tickers_table.update(msg);

                match action {
                    Some(tickers_table::Action::TickerSelected(ticker_info, content)) => {
                        return (
                            Task::none(),
                            Some(Action::TickerSelected(ticker_info, content)),
                        );
                    }
                    Some(tickers_table::Action::Fetch(task)) => {
                        return (task.map(Message::TickersTable), None);
                    }
                    Some(tickers_table::Action::ErrorOccurred(error)) => {
                        return (Task::none(), Some(Action::ErrorOccurred(error)));
                    }
                    Some(tickers_table::Action::FocusWidget(id)) => {
                        return (iced::widget::operation::focus(id), None);
                    }
                    None => {}
                }
            }
        }

        (Task::none(), None)
    }

    pub fn view<'a>(
        &'a self,
        workspaces: &[(uuid::Uuid, &'static str, bool)],
    ) -> Element<'a, Message> {
        let state = &self.state;

        let tooltip_position = if state.position == sidebar::Position::Left {
            TooltipPosition::Right
        } else {
            TooltipPosition::Left
        };

        let is_table_open = self.tickers_table.is_shown;

        let nav_buttons = self.nav_buttons(is_table_open, tooltip_position, workspaces);

        let tickers_table = if is_table_open {
            column![responsive(move |size| self
                .tickers_table
                .view(size)
                .map(Message::TickersTable))]
            .width(200)
        } else {
            column![]
        };

        match state.position {
            sidebar::Position::Left => row![nav_buttons, tickers_table],
            sidebar::Position::Right => row![tickers_table, nav_buttons],
        }
        .spacing(if is_table_open { 8 } else { 4 })
        .into()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        self.tickers_table.subscription().map(Message::TickersTable)
    }

    fn nav_buttons<'a>(
        &'a self,
        is_table_open: bool,
        tooltip_position: TooltipPosition,
        workspaces: &[(uuid::Uuid, &'static str, bool)],
    ) -> iced::widget::Column<'a, Message> {
        let settings_modal_button = {
            let is_active = self.is_menu_active(sidebar::Menu::Settings)
                || self.is_menu_active(sidebar::Menu::ThemeEditor)
                || self.is_menu_active(sidebar::Menu::Network)
                || self.is_menu_active(sidebar::Menu::Audio);

            button_with_tooltip(
                icon_text(Icon::Cog, 14)
                    .width(24)
                    .align_x(Alignment::Center),
                Message::ToggleSidebarMenu(Some(sidebar::Menu::Settings)),
                None,
                tooltip_position,
                move |theme, status| crate::style::button::transparent(theme, status, is_active),
            )
        };

        let layout_modal_button = {
            let is_active = self.is_menu_active(sidebar::Menu::Layout);

            button_with_tooltip(
                icon_text(Icon::Layout, 14)
                    .width(24)
                    .align_x(Alignment::Center),
                Message::ToggleSidebarMenu(Some(sidebar::Menu::Layout)),
                None,
                tooltip_position,
                move |theme, status| crate::style::button::transparent(theme, status, is_active),
            )
        };

        let ticker_search_button = {
            button_with_tooltip(
                icon_text(Icon::Search, 14)
                    .width(24)
                    .align_x(Alignment::Center),
                Message::TickersTable(super::tickers_table::Message::ToggleTable),
                None,
                tooltip_position,
                move |theme, status| {
                    crate::style::button::transparent(theme, status, is_table_open)
                },
            )
        };

        // WealthSpring 工作区切换（docs/08 F6 — P1）：合并进侧边栏顶部的图标按钮组。
        let mut col = column![].width(32).spacing(2).align_x(Alignment::Center);
        for &(uid, name, is_active) in workspaces {
            // 分组标题（docs/35 §5.3）：组首工作区前放组名，组与组之间留一点空
            if let Some((i, (group, _))) = crate::ws::workspace::GROUPS
                .iter()
                .enumerate()
                .find(|(_, (_, ws))| ws.first() == Some(&name))
            {
                if i > 0 {
                    col = col.push(space::vertical().height(2));
                }
                col = col.push(crate::ui::text::metadata(*group));
            }
            col = col.push(button_with_tooltip(
                icon_text(crate::ws::workspace::icon(name), 14)
                    .width(24)
                    .align_x(Alignment::Center),
                Message::SelectWorkspace(uid),
                Some(name),
                tooltip_position,
                move |theme, status| crate::style::button::transparent(theme, status, is_active),
            ));
        }
        // 工作区组放进可滚动区：小屏幕高度不够时滚轮翻动，不再把下方工具组挤出状态栏以上的可见区。
        // 不画滚动条（宽 0），滚轮直接翻；声音设置已移进设置窗口；搜索标的 / 布局 / 设置固定在底部、紧接滚动区。
        let workspaces_area = scrollable::Scrollable::with_direction(
            col,
            scrollable::Direction::Vertical(scrollable::Scrollbar::new().width(0).scroller_width(0)),
        )
        .height(iced::Length::Fill);

        column![
            workspaces_area,
            ticker_search_button,
            layout_modal_button,
            settings_modal_button,
        ]
        .width(32)
        .spacing(2)
        .align_x(Alignment::Center)
    }

    pub fn hide_tickers_table(&mut self) -> bool {
        let table = &mut self.tickers_table;

        if table.expand_ticker_card.is_some() {
            table.expand_ticker_card = None;
            return true;
        } else if table.is_shown {
            table.is_shown = false;
            return true;
        }

        false
    }

    pub fn is_menu_active(&self, menu: sidebar::Menu) -> bool {
        self.state.active_menu == Some(menu)
    }

    pub fn active_menu(&self) -> Option<sidebar::Menu> {
        self.state.active_menu
    }

    pub fn position(&self) -> sidebar::Position {
        self.state.position
    }

    pub fn set_menu(&mut self, menu: Option<sidebar::Menu>) {
        self.state.active_menu = menu;
    }

    pub fn sync_tickers_table_settings(&mut self) {
        let settings = &self.tickers_table.settings();
        self.state.tickers_table = Some(settings.clone());
    }

    pub fn tickers_info(&self) -> &FxHashMap<exchange::Ticker, Option<exchange::TickerInfo>> {
        &self.tickers_table.tickers_info
    }
}
