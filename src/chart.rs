pub mod comparison;
pub mod heatmap;
pub mod indicator;
pub mod kline;
mod scale;

use crate::connector::fetcher::{FetchRange, FetchSpec, RequestHandler};
use crate::style;
use crate::widget::multi_split::{DRAG_SIZE, MultiSplit};
use crate::widget::tooltip;
use data::chart::{Autoscale, Basis, PlotData, ViewConfig, indicator::Indicator};
use exchange::TickerInfo;
use exchange::unit::{Price, PriceStep};
use scale::linear::PriceInfoLabel;
use scale::{AxisLabelsX, AxisLabelsY};

use iced::theme::palette::Extended;
use iced::widget::canvas::{self, Cache, Canvas, Event, Frame, LineDash, Path, Stroke};
use iced::{
    Alignment, Element, Length, Point, Rectangle, Size, Theme, Vector, keyboard, mouse, padding,
    widget::{button, center, column, container, mouse_area, row, rule, text},
};

const ZOOM_SENSITIVITY: f32 = 30.0;
const TEXT_SIZE: f32 = crate::style::text_size::BODY;

#[derive(Default, Debug, Clone, Copy)]
pub enum Interaction {
    #[default]
    None,
    Zoomin {
        last_position: Point,
    },
    Panning {
        translation: Vector,
        start: Point,
    },
    Ruler {
        start: Option<Point>,
    },
    /// 画线（docs/46）：两个锚点的工具放下了第一个点，等第二下（状态在 `ws::drawing`）
    DrawPlace,
    /// 画线：正在拖一个对象或它的锚点（状态在 `ws::drawing`）
    DrawDrag,
}

#[derive(Debug, Clone, Copy)]
pub enum AxisScaleClicked {
    X,
    Y,
}

#[derive(Debug, Clone, Copy)]
pub enum Message {
    Translated(Vector),
    Scaled(f32, Vector),
    AutoscaleToggled,
    CrosshairMoved,
    YScaling(f32, f32, bool),
    XScaling(f32, f32, bool),
    BoundsChanged(Rectangle),
    SplitDragged(usize, f32),
    DoubleClick(AxisScaleClicked),
}

pub trait Chart: PlotConstants + canvas::Program<Message> {
    type IndicatorKind: Indicator;

    fn state(&self) -> &ViewState;

    fn mut_state(&mut self) -> &mut ViewState;

    fn invalidate_all(&mut self);

    fn invalidate_crosshair(&mut self);

    fn view_indicators(&'_ self, enabled: &[Self::IndicatorKind]) -> Vec<Element<'_, Message>>;

    fn visible_timerange(&self) -> Option<(u64, u64)>;

    fn interval_keys(&self) -> Option<Vec<u64>>;

    fn autoscaled_coords(&self) -> Vector;

    fn supports_fit_autoscaling(&self) -> bool;

    fn is_empty(&self) -> bool;

    /// 画线（docs/46）：这张图的品种键与此刻的坐标换算。不支持画线的图返回 `None`。
    fn drawing(&self) -> Option<(String, crate::ws::drawing::geometry::DrawView)> {
        None
    }
}

fn canvas_interaction<T: Chart>(
    chart: &T,
    interaction: &mut Interaction,
    event: &Event,
    bounds: Rectangle,
    cursor: mouse::Cursor,
) -> Option<canvas::Action<Message>> {
    if chart.state().bounds != bounds {
        return Some(canvas::Action::publish(Message::BoundsChanged(bounds)));
    }

    let shrunken_bounds = bounds.shrink(DRAG_SIZE * 4.0);
    let cursor_position = cursor.position_in(shrunken_bounds);
    // 画线（docs/46）：品种键与坐标换算；光标在画布里的位置（与绘制同一个坐标系）
    let drawing = chart.drawing();
    let local = cursor.position_in(bounds);

    if let Event::Mouse(mouse::Event::ButtonReleased(_)) = event {
        match interaction {
            Interaction::Panning { start, .. } => {
                // 特征回放（docs/51 P3）：按下松开、中间没拖动 = 单击 → 播放头跳到这一刻
                let clicked = cursor_position.is_some_and(|p| p.distance(*start) < 3.0);
                *interaction = Interaction::None;
                if clicked
                    && crate::ws::replay_clock::charts_on()
                    && let Some(t) = local.and_then(|p| chart.state().time_at_x(p.x, bounds.size()))
                {
                    crate::ws::replay_clock::seek_to_ms(t);
                    return Some(canvas::Action::request_redraw());
                }
            }
            Interaction::Zoomin { .. } => {
                *interaction = Interaction::None;
            }
            Interaction::DrawDrag => {
                crate::ws::drawing::drop_drag();
                *interaction = Interaction::None;
                return Some(canvas::Action::request_redraw().and_capture());
            }
            _ => {}
        }
    }

    if let Interaction::Ruler { .. } = interaction
        && cursor_position.is_none()
    {
        *interaction = Interaction::None;
    }

    match event {
        Event::Mouse(mouse_event) => {
            let state = chart.state();

            match mouse_event {
                mouse::Event::ButtonPressed(button) => {
                    // 画线（docs/46 §3.5）：选了工具 → 放锚点；没选工具 → 先看点没点中对象，没点中才平移
                    if let mouse::Button::Left = button
                        && let (Some((symbol, view)), Some(p)) = (&drawing, local)
                        && !matches!(interaction, Interaction::Ruler { .. })
                    {
                        if crate::ws::drawing::tool().is_some() {
                            crate::ws::drawing::place_at(symbol, view, p);
                            *interaction = if crate::ws::drawing::placing(symbol).is_some() {
                                Interaction::DrawPlace
                            } else {
                                Interaction::None
                            };
                            return Some(canvas::Action::publish(Message::CrosshairMoved).and_capture());
                        }
                        if crate::ws::drawing::press(symbol, view, p) {
                            *interaction = if crate::ws::drawing::dragging() { Interaction::DrawDrag } else { Interaction::None };
                            return Some(canvas::Action::request_redraw().and_capture());
                        }
                    }
                    let cursor_in_bounds = cursor_position?;

                    if let mouse::Button::Left = button {
                        match interaction {
                            Interaction::None
                            | Interaction::Panning { .. }
                            | Interaction::Zoomin { .. }
                            | Interaction::DrawPlace
                            | Interaction::DrawDrag => {
                                *interaction = Interaction::Panning {
                                    translation: state.translation,
                                    start: cursor_in_bounds,
                                };
                            }
                            Interaction::Ruler { start } if start.is_none() => {
                                *interaction = Interaction::Ruler {
                                    start: Some(cursor_in_bounds),
                                };
                            }
                            Interaction::Ruler { .. } => {
                                *interaction = Interaction::None;
                            }
                        }
                    }
                    Some(canvas::Action::request_redraw().and_capture())
                }
                mouse::Event::CursorMoved { .. } => match *interaction {
                    Interaction::Panning { translation, start } => {
                        let cursor_in_bounds = cursor_position?;
                        let msg = Message::Translated(
                            translation + (cursor_in_bounds - start) * (1.0 / state.scaling),
                        );
                        Some(canvas::Action::publish(msg).and_capture())
                    }
                    Interaction::None | Interaction::Ruler { .. } | Interaction::DrawPlace => {
                        Some(canvas::Action::publish(Message::CrosshairMoved))
                    }
                    Interaction::DrawDrag => {
                        if let (Some((_, view)), Some(p)) = (&drawing, local) {
                            crate::ws::drawing::drag_to(view, p);
                        }
                        Some(canvas::Action::publish(Message::CrosshairMoved).and_capture())
                    }
                    _ => None,
                },
                mouse::Event::WheelScrolled { delta } => {
                    cursor_position?;

                    let default_cell_width = T::default_cell_width(chart);
                    let min_cell_width = T::min_cell_width(chart);
                    let max_cell_width = T::max_cell_width(chart);
                    let max_scaling = T::max_scaling(chart);
                    let min_scaling = T::min_scaling(chart);

                    if matches!(interaction, Interaction::Panning { .. }) {
                        return Some(canvas::Action::capture());
                    }

                    let cursor_to_center = cursor.position_from(bounds.center())?;
                    let y = match delta {
                        mouse::ScrollDelta::Lines { y, .. }
                        | mouse::ScrollDelta::Pixels { y, .. } => y,
                    };

                    if let Some(Autoscale::FitToVisible) = state.layout.autoscale {
                        return Some(
                            canvas::Action::publish(Message::XScaling(
                                y / 2.0,
                                cursor_to_center.x,
                                false,
                            ))
                            .and_capture(),
                        );
                    }

                    let should_adjust_cell_width = match (y.signum(), state.scaling) {
                        (-1.0, scaling)
                            if scaling == max_scaling && state.cell_width > default_cell_width =>
                        {
                            true
                        }
                        (1.0, scaling)
                            if scaling == min_scaling && state.cell_width < default_cell_width =>
                        {
                            true
                        }
                        (1.0, scaling)
                            if scaling == max_scaling && state.cell_width < max_cell_width =>
                        {
                            true
                        }
                        (-1.0, scaling)
                            if scaling == min_scaling && state.cell_width > min_cell_width =>
                        {
                            true
                        }
                        _ => false,
                    };

                    if should_adjust_cell_width {
                        return Some(
                            canvas::Action::publish(Message::XScaling(
                                y / 2.0,
                                cursor_to_center.x,
                                true,
                            ))
                            .and_capture(),
                        );
                    }

                    // normal scaling cases
                    if (*y < 0.0 && state.scaling > min_scaling)
                        || (*y > 0.0 && state.scaling < max_scaling)
                    {
                        let old_scaling = state.scaling;
                        let scaling = (state.scaling * (1.0 + y / ZOOM_SENSITIVITY))
                            .clamp(min_scaling, max_scaling);

                        let denominator = old_scaling * scaling;
                        let vector_diff = if denominator.abs() > 0.0001 {
                            let factor = scaling - old_scaling;
                            Vector::new(
                                cursor_to_center.x * factor / denominator,
                                cursor_to_center.y * factor / denominator,
                            )
                        } else {
                            Vector::default()
                        };

                        let translation = state.translation - vector_diff;

                        return Some(
                            canvas::Action::publish(Message::Scaled(scaling, translation))
                                .and_capture(),
                        );
                    }

                    Some(canvas::Action::capture())
                }
                _ => None,
            }
        }
        Event::Keyboard(keyboard_event) => {
            cursor_position?;
            match keyboard_event {
                iced::keyboard::Event::KeyPressed { key, modifiers, .. } => {
                    // 画线（docs/46 G0）：Alt+H / V / T / R、Alt+Shift+R 选工具；Delete 删选中；Ctrl+Z / Ctrl+Shift+Z / Ctrl+Y 撤销重做
                    if let Some((symbol, _)) = &drawing {
                        let (ctrl, shift, alt) = (modifiers.control(), modifiers.shift(), modifiers.alt());
                        let handled = match key.as_ref() {
                            keyboard::Key::Character(c) if alt && !ctrl => {
                                crate::ws::drawing::tool_for_key(c, shift).map(|k| crate::ws::drawing::set_tool(Some(k))).is_some()
                            }
                            keyboard::Key::Character(c) if ctrl && !alt => match (c.to_ascii_lowercase().as_str(), shift) {
                                ("z", false) => {
                                    crate::ws::drawing::undo(symbol);
                                    true
                                }
                                ("z", true) | ("y", false) => {
                                    crate::ws::drawing::redo(symbol);
                                    true
                                }
                                _ => false,
                            },
                            keyboard::Key::Named(keyboard::key::Named::Delete | keyboard::key::Named::Backspace) if !ctrl && !alt => {
                                crate::ws::drawing::delete_selected(symbol)
                            }
                            keyboard::Key::Named(keyboard::key::Named::Enter) => crate::ws::drawing::finish(symbol).is_some(),
                            _ => false,
                        };
                        if handled {
                            *interaction = Interaction::None;
                            return Some(canvas::Action::publish(Message::CrosshairMoved).and_capture());
                        }
                    }
                    match key.as_ref() {
                    keyboard::Key::Named(keyboard::key::Named::Shift) => {
                        *interaction = Interaction::Ruler { start: None };
                        Some(canvas::Action::request_redraw().and_capture())
                    }
                    keyboard::Key::Named(keyboard::key::Named::Escape) => {
                        *interaction = Interaction::None;
                        if drawing.is_some() {
                            crate::ws::drawing::escape();
                            return Some(canvas::Action::publish(Message::CrosshairMoved).and_capture());
                        }
                        Some(canvas::Action::request_redraw().and_capture())
                    }
                    _ => None,
                    }
                }
                _ => None,
            }
        }
        _ => None,
    }
}

pub enum Action {
    ErrorOccurred(data::InternalError),
    RequestFetch(Vec<FetchSpec>),
    RequestPalette,
}

pub fn update<T: Chart>(chart: &mut T, message: &Message) {
    match message {
        Message::DoubleClick(scale) => {
            let default_chart_width = T::default_cell_width(chart);
            let autoscaled_coords = chart.autoscaled_coords();
            let supports_fit_autoscaling = chart.supports_fit_autoscaling();

            let state = chart.mut_state();

            match scale {
                AxisScaleClicked::X => {
                    state.cell_width = default_chart_width;
                    state.translation = autoscaled_coords;
                    // 双击时间轴回到最新：联动的图一起回去（特征回放，docs/51 P3）
                    crate::ws::replay_clock::publish_view(state.linked_id(), crate::ws::replay_clock::ViewSync::Live);
                }
                AxisScaleClicked::Y => {
                    if supports_fit_autoscaling {
                        state.layout.autoscale = Some(Autoscale::FitToVisible);
                        state.scaling = 1.0;
                    } else {
                        state.layout.autoscale = Some(Autoscale::CenterLatest);
                    }
                }
            }
        }
        Message::Translated(translation) => {
            let state = chart.mut_state();

            if let Some(Autoscale::FitToVisible) = state.layout.autoscale {
                state.translation.x = translation.x;
            } else {
                state.translation = *translation;
                state.layout.autoscale = None;
            }
            state.publish_view_sync();
        }
        Message::Scaled(scaling, translation) => {
            let state = chart.mut_state();
            state.scaling = *scaling;
            state.translation = *translation;

            state.layout.autoscale = None;
            state.publish_view_sync();
        }
        Message::AutoscaleToggled => {
            let supports_fit_autoscaling = chart.supports_fit_autoscaling();
            let state = chart.mut_state();

            let current_autoscale = state.layout.autoscale;
            state.layout.autoscale = {
                match current_autoscale {
                    None => Some(Autoscale::CenterLatest),
                    Some(Autoscale::CenterLatest) => {
                        if supports_fit_autoscaling {
                            Some(Autoscale::FitToVisible)
                        } else {
                            None
                        }
                    }
                    Some(Autoscale::FitToVisible) => None,
                }
            };

            if state.layout.autoscale.is_some() {
                state.scaling = 1.0;
            }
        }
        Message::XScaling(delta, cursor_to_center_x, is_wheel_scroll) => {
            let min_cell_width = T::min_cell_width(chart);
            let max_cell_width = T::max_cell_width(chart);

            let state = chart.mut_state();

            if !(*delta < 0.0 && state.cell_width > min_cell_width
                || *delta > 0.0 && state.cell_width < max_cell_width)
            {
                return;
            }

            let is_fit_to_visible_zoom =
                !is_wheel_scroll && matches!(state.layout.autoscale, Some(Autoscale::FitToVisible));

            let zoom_factor = if is_fit_to_visible_zoom {
                ZOOM_SENSITIVITY / 1.5
            } else if *is_wheel_scroll {
                ZOOM_SENSITIVITY
            } else {
                ZOOM_SENSITIVITY * 3.0
            };

            let new_width = (state.cell_width * (1.0 + delta / zoom_factor))
                .clamp(min_cell_width, max_cell_width);

            if is_fit_to_visible_zoom {
                let anchor_interval = {
                    let latest_x_coord = state.interval_to_x(state.latest_x);
                    if state.is_interval_x_visible(latest_x_coord) {
                        state.latest_x
                    } else {
                        let visible_region = state.visible_region(state.bounds.size());
                        state.x_to_interval(visible_region.x + visible_region.width)
                    }
                };

                let old_anchor_chart_x = state.interval_to_x(anchor_interval);

                state.cell_width = new_width;

                let new_anchor_chart_x = state.interval_to_x(anchor_interval);

                let shift = new_anchor_chart_x - old_anchor_chart_x;
                state.translation.x -= shift;
            } else {
                let (old_scaling, old_translation_x) = { (state.scaling, state.translation.x) };

                let latest_x = state.interval_to_x(state.latest_x);
                let is_interval_x_visible = state.is_interval_x_visible(latest_x);

                let cursor_chart_x = {
                    if *is_wheel_scroll || !is_interval_x_visible {
                        cursor_to_center_x / old_scaling - old_translation_x
                    } else {
                        latest_x / old_scaling - old_translation_x
                    }
                };

                let new_cursor_x = match state.basis {
                    Basis::Time(_) => {
                        let cursor_time = state.x_to_interval(cursor_chart_x);
                        state.cell_width = new_width;

                        state.interval_to_x(cursor_time)
                    }
                    Basis::Tick(_) => {
                        let tick_index = cursor_chart_x / state.cell_width;
                        state.cell_width = new_width;

                        tick_index * state.cell_width
                    }
                };

                if *is_wheel_scroll || !is_interval_x_visible {
                    if !new_cursor_x.is_nan() && !cursor_chart_x.is_nan() {
                        state.translation.x -= new_cursor_x - cursor_chart_x;
                    }

                    state.layout.autoscale = None;
                }
            }
        }
        Message::YScaling(delta, cursor_to_center_y, is_wheel_scroll) => {
            let min_cell_height = T::min_cell_height(chart);
            let max_cell_height = T::max_cell_height(chart);

            let state = chart.mut_state();

            if state.layout.autoscale == Some(Autoscale::FitToVisible) {
                state.layout.autoscale = None;
            }

            if *delta < 0.0 && state.cell_height > min_cell_height
                || *delta > 0.0 && state.cell_height < max_cell_height
            {
                let (old_scaling, old_translation_y) = { (state.scaling, state.translation.y) };

                let zoom_factor = if *is_wheel_scroll {
                    ZOOM_SENSITIVITY
                } else {
                    ZOOM_SENSITIVITY * 3.0
                };

                let new_height = (state.cell_height * (1.0 + delta / zoom_factor))
                    .clamp(min_cell_height, max_cell_height);

                let cursor_chart_y = cursor_to_center_y / old_scaling - old_translation_y;

                let cursor_price = state.y_to_price(cursor_chart_y);

                state.cell_height = new_height;

                let new_cursor_y = state.price_to_y(cursor_price);

                state.translation.y -= new_cursor_y - cursor_chart_y;

                if *is_wheel_scroll {
                    state.layout.autoscale = None;
                }
            }
        }
        Message::BoundsChanged(bounds) => {
            let state = chart.mut_state();

            // calculate how center shifted
            let old_center_x = state.bounds.width / 2.0;
            let new_center_x = bounds.width / 2.0;
            let center_delta_x = (new_center_x - old_center_x) / state.scaling;

            state.bounds = *bounds;

            if state.layout.autoscale != Some(Autoscale::CenterLatest) {
                state.translation.x += center_delta_x;
            }
        }
        Message::SplitDragged(split, size) => {
            let state = chart.mut_state();

            if let Some(split) = state.layout.splits.get_mut(*split) {
                *split = (size * 100.0).round() / 100.0;
            }
        }
        Message::CrosshairMoved => return chart.invalidate_crosshair(),
    }
    chart.invalidate_all();
}

pub fn view<'a, T: Chart>(
    chart: &'a T,
    indicators: &'a [T::IndicatorKind],
    timezone: data::UserTimezone,
) -> Element<'a, Message> {
    if chart.is_empty() {
        return center(text("Waiting for data...").size(crate::style::text_size::TITLE)).into();
    }

    let state = chart.state();
    let drawing = chart.drawing();

    let axis_labels_x = Canvas::new(AxisLabelsX {
        labels_cache: &state.cache.x_labels,
        scaling: state.scaling,
        translation_x: state.translation.x,
        max: state.latest_x,
        basis: state.basis,
        cell_width: state.cell_width,
        timezone,
        chart_bounds: state.bounds,
        interval_keys: chart.interval_keys(),
        autoscaling: state.layout.autoscale,
    })
    .width(Length::Fill)
    .height(Length::Fill);

    let buttons = {
        let (autoscale_btn_placeholder, autoscale_btn_tooltip) = match state.layout.autoscale {
            Some(Autoscale::CenterLatest) => (text("C"), Some("Center last price")),
            Some(Autoscale::FitToVisible) => (text("A"), Some("Auto")),
            None => (text("C"), Some("Toggle autoscaling")),
        };
        let is_active = state.layout.autoscale.is_some();

        let autoscale_button = button(
            autoscale_btn_placeholder
                .size(crate::style::text_size::TINY)
                .align_x(Alignment::Center)
                .align_y(Alignment::Center),
        )
        .height(Length::Fill)
        .on_press(Message::AutoscaleToggled)
        .style(move |theme: &Theme, status| style::button::transparent(theme, status, is_active));

        row![
            iced::widget::space::horizontal(),
            tooltip(
                autoscale_button,
                autoscale_btn_tooltip,
                iced::widget::tooltip::Position::Top
            ),
        ]
        .padding(crate::ui::metrics::space(0))
    };

    let y_labels_width = state.y_labels_width();

    let content = {
        let axis_labels_y = Canvas::new(AxisLabelsY {
            labels_cache: &state.cache.y_labels,
            translation_y: state.translation.y,
            scaling: state.scaling,
            decimals: state.decimals,
            min: state.base_price_y.to_f32_lossy(),
            last_price: state.last_price,
            tick_size: state.tick_size.to_f32_lossy(),
            cell_height: state.cell_height,
            basis: state.basis,
            chart_bounds: state.bounds,
            marks: drawing
                .as_ref()
                .map(|(symbol, v)| {
                    crate::ws::drawing::axis_marks(symbol, v.cat, &v.tf)
                        .into_iter()
                        .map(|(p, c)| (Price::from_units(p).to_f32_lossy(), c))
                        .collect()
                })
                .unwrap_or_default(),
        })
        .width(Length::Fill)
        .height(Length::Fill);

        let main_chart: Element<_> = row![
            container(Canvas::new(chart).width(Length::Fill).height(Length::Fill))
                .width(Length::FillPortion(10))
                .height(Length::FillPortion(120)),
            rule::vertical(1).style(style::split_ruler),
            container(
                mouse_area(axis_labels_y)
                    .on_double_click(Message::DoubleClick(AxisScaleClicked::Y))
            )
            .width(y_labels_width)
            .height(Length::FillPortion(120))
        ]
        .into();

        let indicators = chart.view_indicators(indicators);

        if indicators.is_empty() {
            main_chart
        } else {
            let panels = std::iter::once(main_chart)
                .chain(indicators)
                .collect::<Vec<_>>();

            MultiSplit::new(panels, &state.layout.splits, |index, position| {
                Message::SplitDragged(index, position)
            })
            .into()
        }
    };

    column![
        content,
        rule::horizontal(1).style(style::split_ruler),
        row![
            container(
                mouse_area(axis_labels_x)
                    .on_double_click(Message::DoubleClick(AxisScaleClicked::X))
            )
            .padding(padding::right(1))
            .width(Length::FillPortion(10))
            .height(Length::Fixed(26.0)),
            buttons.width(y_labels_width).height(Length::Fixed(26.0))
        ]
    ]
    .padding(padding::left(1).right(1).bottom(1))
    .into()
}

/// 画线（docs/46 G1）：选中面板上，选中对象旁浮出的小操作条，叠在 [`view`] 上方（坐标与画布同一个原点）。
/// 没有选中对象、对象在这张图上不显示、不是选中面板时为 `None`。
pub fn drawing_overlay<'a, T: Chart>(chart: &T, focused: bool) -> Option<Element<'a, crate::ws::drawing::DrawMsg>> {
    if !focused || chart.is_empty() {
        return None;
    }
    let (symbol, v) = chart.drawing()?;
    let d = crate::ws::drawing::selected_drawing(&symbol).filter(|d| !d.hidden && d.visible_in(v.cat, &v.tf))?;
    // 放在整个对象的上方（不盖住它自己的标签，如仓位工具的风险收益）；上面放不下就放在下方
    let bb = crate::ws::drawing::geometry::bbox(&v, &d)?;
    let x = bb.x.clamp(4.0, (v.size.width - 340.0).max(4.0));
    let y = if bb.y > 44.0 { bb.y - 40.0 } else { (bb.y + bb.height + 8.0).min((v.size.height - 34.0).max(0.0)) };
    let bar = crate::ws::drawing_view::floating(&symbol, &d);
    Some(
        column![
            iced::widget::Space::new().height(Length::Fixed(y.max(0.0))),
            row![iced::widget::Space::new().width(Length::Fixed(x)), iced::widget::opaque(bar)]
        ]
        .into(),
    )
}

pub trait PlotConstants {
    fn min_scaling(&self) -> f32;
    fn max_scaling(&self) -> f32;
    fn max_cell_width(&self) -> f32;
    fn min_cell_width(&self) -> f32;
    fn max_cell_height(&self) -> f32;
    fn min_cell_height(&self) -> f32;
    fn default_cell_width(&self) -> f32;
}

#[derive(Default)]
pub struct Caches {
    main: Cache,
    /// 实时层（K 线图，见 `KlineChart::live_split`）：只画随每笔成交变的东西——最后一根 K 线、指标的最后一段、
    /// 副图读数、最新价线。视野没动时实时数据只重画这一层，主图层原样复用
    live: Cache,
    x_labels: Cache,
    y_labels: Cache,
    crosshair: Cache,
    /// 画线（docs/46）：单独一层，移动光标只重画十字线、不重画所有画线
    drawings: Cache,
}

impl Caches {
    fn clear_all(&self) {
        self.drawings.clear();
        self.main.clear();
        self.live.clear();
        self.x_labels.clear();
        self.y_labels.clear();
        self.crosshair.clear();
    }

    fn clear_crosshair(&self) {
        self.crosshair.clear();
        self.y_labels.clear();
        self.x_labels.clear();
    }

    /// 实时数据到了、视野没动：只重画实时层、价格轴（最新价标签）与十字线读数；
    /// 主图层、时间轴与画线层的坐标都没变，不重画。
    fn clear_live(&self) {
        self.live.clear();
        self.y_labels.clear();
        self.crosshair.clear();
    }

    /// 同 [`Self::clear_live`]，但主图层也重画（这张图没拆实时层时）。
    fn clear_main_live(&self) {
        self.main.clear();
        self.clear_live();
    }
}

pub struct ViewState {
    cache: Caches,
    bounds: Rectangle,
    translation: Vector,
    scaling: f32,
    cell_width: f32,
    cell_height: f32,
    basis: Basis,
    last_price: Option<PriceInfoLabel>,
    base_price_y: Price,
    latest_x: u64,
    tick_size: PriceStep,
    decimals: usize,
    ticker_info: TickerInfo,
    layout: ViewConfig,
    /// 上次看到的联动时间版本（docs/41 E 期）；变了就重画十字线
    linked_seen: std::sync::atomic::AtomicU64,
    /// 上次画绘图层时的画线版本（docs/46）：变了就重画绘图层与十字线层
    drawings_seen: std::sync::atomic::AtomicU64,
    /// 上次画主图时的市场状态叠加层版本（docs/62 R2）：变了就重画主图
    pub regime_seen: std::sync::atomic::AtomicU64,
    /// 图上指标（docs/63 I3）设置的版本
    pub studies_seen: std::sync::atomic::AtomicU64,
    /// 价格轴宽度的高水位（f32 位）：只增不减。见 [`ViewState::y_labels_width`]。
    y_labels_max: std::sync::atomic::AtomicU32,
    /// 见过的「在图上看」请求版本（[`goto_time`]）。
    goto_seen: u64,
    /// 见过的视野同步版本（特征回放，docs/51 P3）
    view_seen: u64,
    /// 上次画十字线层时的播放头（毫秒）：变了就重画（播放头竖线在那一层）
    playhead_seen: std::sync::atomic::AtomicU64,
}

/// 「在图上看」（docs/43 §8 第 1 条）：日历请求跳到某个时刻，**当前页**按时间的 K 线图把视野中心移过去。
///
/// 请求带版本号与发出时刻：图表在每次刷新（`invalidate`）时看一眼，比自己见过的新、且发出不到 5 秒才执行——
/// 别的页的图不刷新，过几分钟切过去时不会莫名其妙跳走。执行了就记一次「接住」，日历据此说清本页有没有图。
pub mod goto_time {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    static REQ: Mutex<Option<(u64, u64, Instant)>> = Mutex::new(None); // (版本, 时刻毫秒, 发出)
    static VERSION: AtomicU64 = AtomicU64::new(0);
    static TAKEN: Mutex<(u64, u32)> = Mutex::new((0, 0)); // (版本, 接住的图数)
    const FRESH: Duration = Duration::from_secs(5);

    /// 发一个跳转请求，返回版本号。
    pub fn request(at_ms: u64) -> u64 {
        let v = VERSION.fetch_add(1, Ordering::Relaxed) + 1;
        if let Ok(mut g) = REQ.lock() {
            *g = Some((v, at_ms, Instant::now()));
        }
        v
    }

    /// 比 `seen` 新、还新鲜的请求：(版本, 时刻)。
    pub fn pending(seen: u64) -> Option<(u64, u64)> {
        let g = REQ.lock().ok()?;
        let (v, at, t) = (*g)?;
        (v > seen && t.elapsed() < FRESH).then_some((v, at))
    }

    pub fn ack(v: u64) {
        if let Ok(mut g) = TAKEN.lock() {
            if g.0 == v {
                g.1 += 1;
            } else {
                *g = (v, 1);
            }
        }
    }

    /// 最近一次请求：(版本, 发出多久了, 接住的图数)。
    pub fn last() -> Option<(u64, Duration, u32)> {
        let (v, _, t) = (*REQ.lock().ok()?)?;
        let n = TAKEN.lock().ok().map_or(0, |g| if g.0 == v { g.1 } else { 0 });
        Some((v, t.elapsed(), n))
    }
    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_request_is_taken_once_per_chart_and_counted() {
            let v = request(1_000);
            assert_eq!(pending(v - 1), Some((v, 1_000)), "没见过的图要执行");
            assert_eq!(pending(v), None, "执行过的图不再执行");
            ack(v);
            ack(v);
            assert_eq!(last().map(|x| (x.0, x.2)), Some((v, 2)));
            let w = request(2_000);
            assert_eq!(last().map(|x| (x.0, x.2)), Some((w, 0)), "新请求从零数");
        }
    }
}

/// 同页图表的时间联动（docs/41 §4.3，E 期）：光标所在的图把它指着的时间放在这里，
/// 同页其他按时间轴的图在那个时间画一条竖线。只有当前页（含它的浮窗与弹出窗口）在画，所以不分页存；
/// 切页时清空。
pub mod linked_cursor {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};

    static CUR: Mutex<Option<(u64, u64)>> = Mutex::new(None); // (来源图, 时间毫秒)
    static VERSION: AtomicU64 = AtomicU64::new(1);

    pub fn publish(src: u64, ts: Option<u64>) {
        if let Ok(mut g) = CUR.lock() {
            let new = ts.map(|t| (src, t));
            let changed = match (&*g, &new) {
                (Some(a), Some(b)) => a != b,
                (None, None) => false,
                // 别的图的光标离开时不能清掉当前来源的
                (Some((s0, _)), None) => *s0 == src,
                (None, Some(_)) => true,
            };
            if changed {
                *g = new;
                VERSION.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub fn clear() {
        if let Ok(mut g) = CUR.lock() {
            if g.take().is_some() {
                VERSION.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub fn get() -> Option<(u64, u64)> {
        CUR.lock().ok().and_then(|g| *g)
    }

    pub fn version() -> u64 {
        VERSION.load(Ordering::Relaxed)
    }
}

impl ViewState {
    /// 决定画面坐标的那几个量（平移、缩放、格宽、格高、基准价、最新 K 线位置）：实时更新前后一样 = 视野没动。
    pub fn view_key(&self) -> (u32, u32, u32, u32, u32, i64, u64) {
        (self.translation.x.to_bits(), self.translation.y.to_bits(), self.scaling.to_bits(), self.cell_width.to_bits(),
         self.cell_height.to_bits(), self.base_price_y.units, self.latest_x)
    }

    pub(crate) fn clear_live(&self) {
        self.cache.clear_live();
    }

    pub(crate) fn clear_main_live(&self) {
        self.cache.clear_main_live();
    }

    pub(crate) fn clear_all(&self) {
        self.cache.clear_all();
    }

    pub(crate) fn linked_id(&self) -> u64 {
        std::ptr::from_ref(self) as usize as u64
    }

    /// 联动时间变了（别的图在动光标）→ 清掉自己的十字线缓存（在 `draw` 开头调；缓存清理只要 `&self`）。
    /// 播放头动了也一样（播放头竖线画在十字线那一层）。
    pub fn sync_linked_cursor(&self) {
        let v = linked_cursor::version();
        if self.linked_seen.swap(v, std::sync::atomic::Ordering::Relaxed) != v {
            self.cache.clear_crosshair();
        }
        // 播放头与事件标记（聚焦 / 筛选）任一变了就重画
        let h = crate::ws::replay_clock::chart_now_ms()
            .unwrap_or(0)
            .wrapping_mul(1_000_003)
            .wrapping_add(crate::ws::replay_clock::marks_ver());
        if self.playhead_seen.swap(h, std::sync::atomic::Ordering::Relaxed) != h {
            self.cache.clear_crosshair();
        }
    }

    /// 可见范围内画布横坐标 `x`（0 = 左边）对应的时刻（毫秒）；与 [`Self::draw_linked_cursor`] 同一个换算。
    fn time_at_x(&self, x: f32, bounds: Size) -> Option<u64> {
        let Basis::Time(_) = self.basis else { return None };
        let region = self.visible_region(bounds);
        let earliest = self.x_to_interval(region.x) as f64;
        let latest = self.x_to_interval(region.x + region.width) as f64;
        (latest > earliest && bounds.width > 0.0).then(|| (earliest + f64::from(x / bounds.width) * (latest - earliest)) as u64)
    }

    /// 视野中心的时刻（毫秒；`goto_time` 把某一刻放到中心用的是 `translation.x = -interval_to_x(t)`，这里是它的反函数）。
    fn center_time(&self) -> Option<u64> {
        matches!(self.basis, Basis::Time(_)).then(|| self.x_to_interval(-self.translation.x))
    }

    /// 特征回放（docs/51 P3）：用户拖动 / 缩放了这张图 → 告诉同页别的时间轴图。
    pub fn publish_view_sync(&self) {
        if let Some(t) = self.center_time() {
            crate::ws::replay_clock::publish_view(self.linked_id(), crate::ws::replay_clock::ViewSync::Center(t));
        }
    }

    /// 别的图拖动 / 缩放了：把视野中心移到同一时刻（保留自己的缩放）；`Live` = 回到跟随最新。`fit` = 这张图能「适配可见」；
    /// `live` = 跟随最新时的平移（图自己的 `autoscaled_coords`）。
    pub fn apply_view_sync(&mut self, fit: bool, live: Vector) {
        let Some((ver, src, v)) = crate::ws::replay_clock::view_sync(self.view_seen) else { return };
        self.view_seen = ver;
        if src == self.linked_id() || !matches!(self.basis, Basis::Time(_)) {
            return;
        }
        match v {
            crate::ws::replay_clock::ViewSync::Center(t) => {
                self.translation.x = -self.interval_to_x(t);
                if self.layout.autoscale == Some(Autoscale::CenterLatest) {
                    self.layout.autoscale = fit.then_some(Autoscale::FitToVisible);
                }
            }
            // 正在「适配可见」的图保留适配（纵轴靠它定格高；改成「居中最新」的话新图那份没纠正过的格高就留下了——
            // 打开分析后价格轴只剩 0.3 美元高，实测），只把横向移到最新
            crate::ws::replay_clock::ViewSync::Live => match self.layout.autoscale {
                Some(Autoscale::FitToVisible) => self.translation.x = live.x,
                _ => self.layout.autoscale = Some(Autoscale::CenterLatest),
            },
        }
    }

    /// 特征回放的播放头竖线（实线、强调色）。
    pub fn draw_playhead(&self, frame: &mut canvas::Frame, theme: &Theme, bounds: Size) {
        let Basis::Time(_) = self.basis else { return };
        let Some(ts) = crate::ws::replay_clock::chart_now_ms() else { return };
        let region = self.visible_region(bounds);
        let earliest = self.x_to_interval(region.x) as f64;
        let latest = self.x_to_interval(region.x + region.width) as f64;
        if latest <= earliest || (ts as f64) < earliest || (ts as f64) > latest {
            return;
        }
        let x = ((ts as f64 - earliest) / (latest - earliest)) as f32 * bounds.width;
        frame.stroke(
            &Path::line(Point::new(x, 0.0), Point::new(x, bounds.height)),
            canvas::Stroke::default().with_color(theme.extended_palette().primary.base.color).with_width(1.5),
        );
    }

    /// 特征回放的事件标记（docs/51 P4）：勾了的类在顶边画一个小三角（向上涨色、向下跌色、无方向灰）；
    /// 聚焦的那个（⏮ ⏭ 跳到的）画一条贯穿的虚线。
    pub fn draw_event_marks(&self, frame: &mut canvas::Frame, theme: &Theme, bounds: Size) {
        let Basis::Time(_) = self.basis else { return };
        let region = self.visible_region(bounds);
        let earliest = self.x_to_interval(region.x);
        let latest = self.x_to_interval(region.x + region.width);
        if latest <= earliest {
            return;
        }
        let span = (latest - earliest) as f64;
        let pal = theme.extended_palette();
        for (t, dir, focused) in crate::ws::replay_clock::marks(earliest, latest, 400) {
            let x = ((t - earliest) as f64 / span) as f32 * bounds.width;
            let color = match dir {
                1 => pal.success.base.color,
                -1 => pal.danger.base.color,
                _ => pal.background.strong.color,
            };
            if focused {
                frame.stroke(
                    &Path::line(Point::new(x, 0.0), Point::new(x, bounds.height)),
                    canvas::Stroke {
                        width: 1.5,
                        line_dash: canvas::LineDash { segments: &[6.0, 4.0], offset: 0 },
                        ..Default::default()
                    }
                    .with_color(pal.primary.strong.color),
                );
            }
            let r = if focused { 6.0 } else { 4.0 };
            let tri = Path::new(|b| {
                b.move_to(Point::new(x, r * 1.6));
                b.line_to(Point::new(x - r, 0.0));
                b.line_to(Point::new(x + r, 0.0));
                b.close();
            });
            frame.fill(&tri, color);
        }
        // 书签（docs/51 P5）：底边一个小菱形
        for t in crate::ws::replay_clock::bookmark_marks(earliest, latest) {
            let x = ((t - earliest) as f64 / span) as f32 * bounds.width;
            let y = bounds.height - 7.0;
            let dia = Path::new(|b| {
                b.move_to(Point::new(x, y - 6.0));
                b.line_to(Point::new(x + 5.0, y));
                b.line_to(Point::new(x, y + 6.0));
                b.line_to(Point::new(x - 5.0, y));
                b.close();
            });
            frame.fill(&dia, pal.primary.strong.color);
        }
    }

    /// 自己有光标：把指着的时间发出去（只有时间轴的图参与）。
    pub fn publish_linked_cursor(&self, ts: Option<u64>) {
        if matches!(self.basis, Basis::Time(_)) {
            linked_cursor::publish(self.linked_id(), ts);
            self.linked_seen.store(linked_cursor::version(), std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// 自己没光标：画别的图指着的时间（竖线 + 时间不在可见范围就不画）。
    pub fn draw_linked_cursor(&self, frame: &mut canvas::Frame, theme: &Theme, bounds: Size) {
        let Basis::Time(_) = self.basis else { return };
        let Some((src, ts)) = linked_cursor::get() else { return };
        if src == self.linked_id() {
            return;
        }
        let region = self.visible_region(bounds);
        let earliest = self.x_to_interval(region.x) as f64;
        let latest = self.x_to_interval(region.x + region.width) as f64;
        if latest <= earliest || (ts as f64) < earliest || (ts as f64) > latest {
            return;
        }
        let x = ((ts as f64 - earliest) / (latest - earliest)) as f32 * bounds.width;
        frame.stroke(&Path::line(Point::new(x, 0.0), Point::new(x, bounds.height)), style::dashed_line(theme));
    }
    /// 复位「最新位置」与平移量，让图表重新锚定到下一批到来的数据。
    ///
    /// **清空数据时必须一起做**（docs/27 §12）：`update_latest_kline` 只在新 K 线时间
    /// **大于** `latest_x` 时才更新它。清空前若 `latest_x` 停在实时时间（当下），
    /// 而新数据是几个月前的回测区间——K 线进得去，视窗却不动，蜡烛全在屏幕外。
    /// tick 基图表按笔数索引、不看绝对时间，所以只有时间基会中招。
    pub fn reset_anchor(&mut self) {
        self.latest_x = 0;
        self.translation = Vector::default();
        self.scaling = 1.0;
        self.y_labels_max.store(0, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn new(
        basis: Basis,
        tick_size: PriceStep,
        decimals: usize,
        ticker_info: TickerInfo,
        layout: ViewConfig,
        cell_width: f32,
        cell_height: f32,
    ) -> Self {
        ViewState {
            cache: Caches::default(),
            bounds: Rectangle::default(),
            translation: Vector::default(),
            scaling: 1.0,
            cell_width,
            cell_height,
            basis,
            last_price: None,
            base_price_y: Price::from_f32_lossy(0.0),
            latest_x: 0,
            tick_size,
            decimals,
            ticker_info,
            layout,
            linked_seen: std::sync::atomic::AtomicU64::new(0),
            drawings_seen: std::sync::atomic::AtomicU64::new(0),
            regime_seen: std::sync::atomic::AtomicU64::new(0),
            studies_seen: std::sync::atomic::AtomicU64::new(0),
            y_labels_max: std::sync::atomic::AtomicU32::new(0),
            goto_seen: 0,
            view_seen: 0,
            playhead_seen: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// 画线的坐标换算（docs/46 §3.2）：与本图绘制同一个变换。按逐笔的图要给各根 K 线的开始时间（按先后），
    /// 一根都没有时不能画线。`bars`：看得见的 K 线（开始时间, [开高低收] 定点），磁吸用；没有就空。
    pub fn draw_view(&self, tick_times: Option<Vec<u64>>, bars: Vec<(u64, [i64; 4])>) -> Option<crate::ws::drawing::geometry::DrawView> {
        use crate::ws::drawing::geometry::{DrawView, XMode};
        let (x, cat) = match self.basis {
            Basis::Time(tf) => (
                XMode::Time { latest: self.latest_x, interval_ms: tf.to_milliseconds(), cell_w: self.cell_width },
                crate::ws::drawing::model::tf_cat(Some(tf.to_milliseconds())),
            ),
            Basis::Tick(_) => {
                let times = tick_times.filter(|t| !t.is_empty())?;
                (XMode::Tick { times, cell_w: self.cell_width }, crate::ws::drawing::model::tf_cat(None))
            }
        };
        let min_step: PriceStep = self.ticker_info.min_ticksize.into();
        Some(DrawView {
            size: self.bounds.size(),
            scaling: self.scaling,
            translation: self.translation,
            x,
            base: self.base_price_y.units,
            tick: self.effective_tick_units(),
            cell_h: self.cell_height,
            snap: min_step.units.max(1),
            cat,
            tf: self.basis.to_string(),
            bars,
            decimals: self.decimals,
            multiplier: self.contract_multiplier(),
            vol: Vec::new(),
        })
    }

    /// 看得见的时间范围（磁吸取 K 线用）：按时间的图是毫秒，按逐笔的图是根序号。
    pub fn visible_interval_range(&self) -> (u64, u64) {
        self.interval_range(&self.visible_region(self.bounds.size()))
    }

    /// 画线的品种键（docs/46）：同一品种的所有图共用一套画线。
    pub fn drawing_symbol(&self) -> String {
        crate::ws::drawing::symbol_key(&self.ticker_info.ticker)
    }

    /// 合约乘数（画线的仓位估算；没有就按 1）。
    pub fn contract_multiplier(&self) -> f64 {
        self.ticker_info.contract_size.map_or(1.0, |c| f64::from(c.as_f32()))
    }

    /// 价格的显示小数位。
    pub fn price_decimals(&self) -> usize {
        self.decimals
    }

    /// 绘图层（docs/46）：画线版本变了就把这一层与十字线层清掉重画。
    pub fn draw_drawings_layer(
        &self,
        renderer: &iced::Renderer,
        size: Size,
        drawing: Option<&(String, crate::ws::drawing::geometry::DrawView)>,
    ) -> Option<canvas::Geometry> {
        let (symbol, view) = drawing?;
        if crate::ws::drawing::changed_since(&self.drawings_seen) {
            self.cache.drawings.clear();
            self.cache.crosshair.clear();
            self.cache.y_labels.clear(); // 价格轴上的画线价位
        }
        Some(self.cache.drawings.draw(renderer, size, |frame| crate::ws::drawing::draw_layer(frame, view, symbol)))
    }

    fn effective_tick_units(&self) -> i64 {
        if self.tick_size.units > 0 {
            self.tick_size.units
        } else {
            let min_step: PriceStep = self.ticker_info.min_ticksize.into();
            min_step.units.max(1)
        }
    }

    fn visible_region(&self, size: Size) -> Rectangle {
        let width = size.width / self.scaling;
        let height = size.height / self.scaling;

        Rectangle {
            x: -self.translation.x - width / 2.0,
            y: -self.translation.y - height / 2.0,
            width,
            height,
        }
    }

    fn is_interval_x_visible(&self, interval_x: f32) -> bool {
        let region = self.visible_region(self.bounds.size());

        interval_x >= region.x && interval_x <= region.x + region.width
    }

    fn interval_range(&self, region: &Rectangle) -> (u64, u64) {
        match self.basis {
            Basis::Tick(_) => (
                self.x_to_interval(region.x + region.width),
                self.x_to_interval(region.x),
            ),
            Basis::Time(timeframe) => {
                let interval = timeframe.to_milliseconds();
                (
                    self.x_to_interval(region.x).saturating_sub(interval / 2),
                    self.x_to_interval(region.x + region.width)
                        .saturating_add(interval / 2),
                )
            }
        }
    }

    fn price_range(&self, region: &Rectangle) -> (Price, Price) {
        let highest = self.y_to_price(region.y);
        let lowest = self.y_to_price(region.y + region.height);

        (highest, lowest)
    }

    fn interval_to_x(&self, value: u64) -> f32 {
        match self.basis {
            Basis::Time(timeframe) => {
                let interval = timeframe.to_milliseconds() as f64;
                let cell_width = f64::from(self.cell_width);

                let diff = value as f64 - self.latest_x as f64;
                (diff / interval * cell_width) as f32
            }
            Basis::Tick(_) => -((value as f32) * self.cell_width),
        }
    }

    fn x_to_interval(&self, x: f32) -> u64 {
        match self.basis {
            Basis::Time(timeframe) => {
                let interval = timeframe.to_milliseconds();

                if x <= 0.0 {
                    let diff = (-x / self.cell_width * interval as f32) as u64;
                    self.latest_x.saturating_sub(diff)
                } else {
                    let diff = (x / self.cell_width * interval as f32) as u64;
                    self.latest_x.saturating_add(diff)
                }
            }
            Basis::Tick(_) => {
                let tick = -(x / self.cell_width);
                tick.round() as u64
            }
        }
    }

    fn price_to_y(&self, price: Price) -> f32 {
        let delta_units = self.base_price_y.units - price.units;
        let ticks = (delta_units as f32) / (self.effective_tick_units() as f32);
        ticks * self.cell_height
    }

    fn y_to_price(&self, y: f32) -> Price {
        let ticks = y / self.cell_height;
        let delta_units = (ticks * self.effective_tick_units() as f32).round() as i64;
        Price::from_units(self.base_price_y.units - delta_units)
    }

    fn draw_crosshair(
        &self,
        frame: &mut Frame,
        theme: &Theme,
        bounds: Size,
        cursor_position: Point,
        interaction: &Interaction,
    ) -> (f32, u64) {
        let region = self.visible_region(bounds);
        let dashed_line = style::dashed_line(theme);

        let highest_p: Price = self.y_to_price(region.y);
        let lowest_p: Price = self.y_to_price(region.y + region.height);
        let highest: f32 = highest_p.to_f32_lossy();
        let lowest: f32 = lowest_p.to_f32_lossy();

        let effective_step = if self.tick_size.units > 0 {
            self.tick_size
        } else {
            self.ticker_info.min_ticksize.into()
        };

        if let Interaction::Ruler { start: Some(start) } = interaction {
            let p1 = *start;
            let p2 = cursor_position;

            let snap_y = |y: f32| {
                let ratio = y / bounds.height;
                let price = highest + ratio * (lowest - highest);

                let p = Price::from_f32_lossy(price);
                let tick_units = effective_step.units;
                let tick_index = p.units.div_euclid(tick_units);
                let rounded_price_p = Price::from_units(tick_index * tick_units);
                let rounded_price = rounded_price_p.to_f32_lossy();
                let snap_ratio = (rounded_price - highest) / (lowest - highest);
                snap_ratio * bounds.height
            };

            let snap_x = |x: f32| {
                let (_, snap_ratio) = self.snap_x_to_index(x, bounds, region);
                snap_ratio * bounds.width
            };

            let snapped_p1_x = snap_x(p1.x);
            let snapped_p1_y = snap_y(p1.y);
            let snapped_p2_x = snap_x(p2.x);
            let snapped_p2_y = snap_y(p2.y);

            let price1 = self.y_to_price(snapped_p1_y);
            let price2 = self.y_to_price(snapped_p2_y);

            let pct = if price1.to_f32_lossy() == 0.0 {
                0.0
            } else {
                ((price2.to_f32_lossy() - price1.to_f32_lossy()) / price1.to_f32_lossy()) * 100.0
            };
            let pct_text = format!("{:.2}%", pct);

            let interval_diff: String = match self.basis {
                Basis::Time(_) => {
                    let (timestamp1, _) = self.snap_x_to_index(p1.x, bounds, region);
                    let (timestamp2, _) = self.snap_x_to_index(p2.x, bounds, region);

                    let diff_ms: u64 = timestamp1.abs_diff(timestamp2);
                    data::util::format_duration_ms(diff_ms)
                }
                Basis::Tick(_) => {
                    let (tick1, _) = self.snap_x_to_index(p1.x, bounds, region);
                    let (tick2, _) = self.snap_x_to_index(p2.x, bounds, region);

                    let tick_diff = tick1.abs_diff(tick2);
                    format!("{} ticks", tick_diff)
                }
            };

            let rect_x = snapped_p1_x.min(snapped_p2_x);
            let rect_y = snapped_p1_y.min(snapped_p2_y);
            let rect_w = (snapped_p1_x - snapped_p2_x).abs();
            let rect_h = (snapped_p1_y - snapped_p2_y).abs();

            let palette = theme.extended_palette();

            frame.fill_rectangle(
                Point::new(rect_x, rect_y),
                Size::new(rect_w, rect_h),
                palette.primary.base.color.scale_alpha(0.08),
            );
            let corners = [
                Point::new(rect_x, rect_y),
                Point::new(rect_x + rect_w, rect_y),
                Point::new(rect_x, rect_y + rect_h),
                Point::new(rect_x + rect_w, rect_y + rect_h),
            ];

            let (text_corner, idx) = corners
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    let da = (a.x - p2.x).hypot(a.y - p2.y);
                    let db = (b.x - p2.x).hypot(b.y - p2.y);
                    da.total_cmp(&db)
                })
                .map(|(i, &c)| (c, i))
                .unwrap_or((corners[0], 0));

            let text_padding = 8.0;
            let text_pos = match idx {
                0 => Point::new(text_corner.x + text_padding, text_corner.y + text_padding),
                1 => Point::new(text_corner.x - text_padding, text_corner.y + text_padding),
                2 => Point::new(text_corner.x + text_padding, text_corner.y - text_padding),
                3 => Point::new(text_corner.x - text_padding, text_corner.y - text_padding),
                _ => text_corner,
            };

            let datapoints_text = match self.basis {
                Basis::Time(timeframe) => {
                    let interval_ms = timeframe.to_milliseconds();
                    let (timestamp1, _) = self.snap_x_to_index(p1.x, bounds, region);
                    let (timestamp2, _) = self.snap_x_to_index(p2.x, bounds, region);

                    let diff_ms = timestamp1.abs_diff(timestamp2);
                    let datapoints = (diff_ms / interval_ms).max(1);
                    format!("{} bars", datapoints)
                }
                Basis::Tick(aggregation) => {
                    let (tick1, _) = self.snap_x_to_index(p1.x, bounds, region);
                    let (tick2, _) = self.snap_x_to_index(p2.x, bounds, region);

                    let tick_diff = tick1.abs_diff(tick2);
                    let datapoints = (tick_diff / u64::from(aggregation.0)).max(1);
                    format!("{} bars", datapoints)
                }
            };

            let label_text = format!("{}, {} | {}", datapoints_text, interval_diff, pct_text);

            let text_width = (label_text.len() as f32) * TEXT_SIZE * 0.6;
            let text_height = TEXT_SIZE * 1.2;
            let rect_padding = 4.0;

            let (bg_x, bg_y) = match idx {
                0 => (text_pos.x - rect_padding, text_pos.y - rect_padding),
                1 => (
                    text_pos.x - text_width - rect_padding,
                    text_pos.y - rect_padding,
                ),
                2 => (
                    text_pos.x - rect_padding,
                    text_pos.y - text_height - rect_padding,
                ),
                3 => (
                    text_pos.x - text_width - rect_padding,
                    text_pos.y - text_height - rect_padding,
                ),
                _ => (
                    text_pos.x - text_width / 2.0 - rect_padding,
                    text_pos.y - text_height / 2.0 - rect_padding,
                ),
            };

            frame.fill_rectangle(
                Point::new(bg_x, bg_y),
                Size::new(
                    text_width + rect_padding * 2.0,
                    text_height + rect_padding * 2.0,
                ),
                palette.background.weakest.color.scale_alpha(0.9),
            );

            frame.fill_text(iced::widget::canvas::Text {
                content: label_text,
                position: text_pos,
                color: palette.background.base.text,
                size: iced::Pixels(crate::style::text_size::SMALL),
                align_x: match idx {
                    0 | 2 => Alignment::Start.into(),
                    1 | 3 => Alignment::End.into(),
                    _ => Alignment::Center.into(),
                },
                align_y: match idx {
                    0 | 1 => Alignment::Start.into(),
                    2 | 3 => Alignment::End.into(),
                    _ => Alignment::Center.into(),
                },
                font: style::AZERET_MONO,
                ..Default::default()
            });
        }

        // Horizontal price line
        let crosshair_ratio = cursor_position.y / bounds.height;
        let crosshair_price = highest + crosshair_ratio * (lowest - highest);

        let rounded_price = Price::from_f32_lossy(crosshair_price)
            .round_to_step(effective_step)
            .to_f32_lossy();
        let snap_ratio = (rounded_price - highest) / (lowest - highest);

        frame.stroke(
            &Path::line(
                Point::new(0.0, snap_ratio * bounds.height),
                Point::new(bounds.width, snap_ratio * bounds.height),
            ),
            dashed_line,
        );

        // Vertical time/tick line
        match self.basis {
            Basis::Time(_) => {
                let (rounded_timestamp, snap_ratio) =
                    self.snap_x_to_index(cursor_position.x, bounds, region);

                frame.stroke(
                    &Path::line(
                        Point::new(snap_ratio * bounds.width, 0.0),
                        Point::new(snap_ratio * bounds.width, bounds.height),
                    ),
                    dashed_line,
                );
                (rounded_price, rounded_timestamp)
            }
            Basis::Tick(aggregation) => {
                let (chart_x_min, chart_x_max) = (region.x, region.x + region.width);
                let crosshair_pos = chart_x_min + (cursor_position.x / bounds.width) * region.width;

                let cell_index = (crosshair_pos / self.cell_width).round();

                let snapped_crosshair = cell_index * self.cell_width;
                let snap_ratio = (snapped_crosshair - chart_x_min) / (chart_x_max - chart_x_min);

                let rounded_tick = (-cell_index as u64) * (u64::from(aggregation.0));

                frame.stroke(
                    &Path::line(
                        Point::new(snap_ratio * bounds.width, 0.0),
                        Point::new(snap_ratio * bounds.width, bounds.height),
                    ),
                    dashed_line,
                );
                (rounded_price, rounded_tick)
            }
        }
    }

    fn draw_last_price_line(
        &self,
        frame: &mut canvas::Frame,
        palette: &Extended,
        region: Rectangle,
    ) {
        if let Some(price) = &self.last_price {
            let (last_price, line_color) = price.get_with_color(palette);
            let y_pos = self.price_to_y(last_price);

            let marker_line = Stroke::with_color(
                Stroke {
                    width: 1.0,
                    line_dash: LineDash {
                        segments: &[2.0, 2.0],
                        offset: 4,
                    },
                    ..Default::default()
                },
                line_color.scale_alpha(0.5),
            );

            frame.stroke(
                &Path::line(
                    Point::new(0.0, y_pos),
                    Point::new(region.x + region.width, y_pos),
                ),
                marker_line,
            );
        }
    }

    fn layout(&self) -> ViewConfig {
        let layout = &self.layout;
        ViewConfig {
            splits: layout.splits.clone(),
            autoscale: layout.autoscale,
        }
    }

    fn y_labels_width(&self) -> Length {
        let precision = self.ticker_info.min_ticksize;

        let value = self.base_price_y.to_string(precision);
        let width = (value.len() as f32 * TEXT_SIZE * 0.8).max(72.0).ceil();

        // 只增不减（2026-10-09 用户报「拖到尽头图表抖动」）：宽度按视野最高价的字数算，自动适配下
        // 最高价跨过 100000 时轴变宽 → 图变窄 → 视野左边缘把那根 K 线挤出去 → 最高价回到 9xxxx → 轴变窄
        // → 那根又露回来……每帧来回跳。轴宽只随最宽的标签长，循环就断了；换品种是新的 ViewState。
        let prev = f32::from_bits(self.y_labels_max.load(std::sync::atomic::Ordering::Relaxed));
        let width = width.max(prev);
        self.y_labels_max.store(width.to_bits(), std::sync::atomic::Ordering::Relaxed);

        Length::Fixed(width)
    }

    fn snap_x_to_index(&self, x: f32, bounds: Size, region: Rectangle) -> (u64, f32) {
        let x_ratio = x / bounds.width;

        match self.basis {
            Basis::Time(timeframe) => {
                let interval = timeframe.to_milliseconds();
                let earliest = self.x_to_interval(region.x) as f64;
                let latest = self.x_to_interval(region.x + region.width) as f64;

                let millis_at_x = earliest + f64::from(x_ratio) * (latest - earliest);

                let rounded_timestamp = (millis_at_x / (interval as f64)).round() as u64 * interval;

                let snap_ratio = if latest - earliest > 0.0 {
                    ((rounded_timestamp as f64 - earliest) / (latest - earliest)) as f32
                } else {
                    0.5
                };

                (rounded_timestamp, snap_ratio)
            }
            Basis::Tick(aggregation) => {
                let (chart_x_min, chart_x_max) = (region.x, region.x + region.width);
                let chart_x = chart_x_min + x_ratio * (chart_x_max - chart_x_min);

                let cell_index = (chart_x / self.cell_width).round();
                let snapped_x = cell_index * self.cell_width;

                let snap_ratio = if chart_x_max - chart_x_min > 0.0 {
                    (snapped_x - chart_x_min) / (chart_x_max - chart_x_min)
                } else {
                    0.5
                };

                let rounded_tick = (-cell_index as u64) * u64::from(aggregation.0);

                (rounded_tick, snap_ratio)
            }
        }
    }
}

fn request_fetch(handler: &mut RequestHandler, range: FetchRange) -> Option<Action> {
    match handler.add_request(range) {
        Ok(Some(req_id)) => {
            let fetch_spec = FetchSpec {
                req_id,
                fetch: range,
                stream: None,
            };
            Some(Action::RequestFetch(vec![fetch_spec]))
        }
        Ok(None) => None,
        Err(reason) => {
            log::error!("Failed to request {:?}: {}", range, reason);
            // TODO: handle this more explicitly, maybe by returning Action::ErrorOccurred
            None
        }
    }
}

fn draw_volume_bar(
    frame: &mut canvas::Frame,
    start_x: f32,
    start_y: f32,
    buy_qty: f32,
    sell_qty: f32,
    max_qty: f32,
    bar_length: f32,
    thickness: f32,
    buy_color: iced::Color,
    sell_color: iced::Color,
    bar_color_alpha: f32,
    horizontal: bool,
) {
    let total_qty = buy_qty + sell_qty;
    if total_qty <= 0.0 || max_qty <= 0.0 {
        return;
    }

    let total_bar_length = (total_qty / max_qty) * bar_length;

    let buy_proportion = buy_qty / total_qty;
    let sell_proportion = sell_qty / total_qty;

    let buy_bar_length = buy_proportion * total_bar_length;
    let sell_bar_length = sell_proportion * total_bar_length;

    if horizontal {
        let start_y = start_y - (thickness / 2.0);

        if sell_qty > 0.0 {
            frame.fill_rectangle(
                Point::new(start_x, start_y),
                Size::new(sell_bar_length, thickness),
                sell_color.scale_alpha(bar_color_alpha),
            );
        }

        if buy_qty > 0.0 {
            frame.fill_rectangle(
                Point::new(start_x + sell_bar_length, start_y),
                Size::new(buy_bar_length, thickness),
                buy_color.scale_alpha(bar_color_alpha),
            );
        }
    } else {
        let start_x = start_x - (thickness / 2.0);

        if sell_qty > 0.0 {
            frame.fill_rectangle(
                Point::new(start_x, start_y + (bar_length - sell_bar_length)),
                Size::new(thickness, sell_bar_length),
                sell_color.scale_alpha(bar_color_alpha),
            );
        }

        if buy_qty > 0.0 {
            frame.fill_rectangle(
                Point::new(
                    start_x,
                    start_y + (bar_length - sell_bar_length - buy_bar_length),
                ),
                Size::new(thickness, buy_bar_length),
                buy_color.scale_alpha(bar_color_alpha),
            );
        }
    }
}

#[cfg(test)]
mod linked_cursor_tests {
    use super::linked_cursor as lc;

    /// 同页时间联动（docs/41 E 期）：别的图光标离开不能清掉当前来源；来源自己离开才清；版本只在变化时加。
    #[test]
    fn publish_semantics() {
        lc::clear();
        let v0 = lc::version();
        lc::publish(1, Some(1000));
        assert_eq!(lc::get(), Some((1, 1000)));
        let v1 = lc::version();
        assert!(v1 > v0);
        lc::publish(1, Some(1000));
        assert_eq!(lc::version(), v1, "没变化不加版本（不然每帧都重画）");
        lc::publish(2, None);
        assert_eq!(lc::get(), Some((1, 1000)), "别的图光标离开不清当前来源");
        lc::publish(1, None);
        assert_eq!(lc::get(), None);
        lc::publish(2, Some(5));
        lc::clear();
        assert_eq!(lc::get(), None);
    }
}
