//! 数据选择组件的渲染（状态与目录在 [`super::data_picker`]）。
//!
//! 一行排开：数据管线 → 市场 → 标的（带搜索）→ 日期（宿主要求时）→ 刷新目录；下面一行小字说明当前选到了什么。
//! 宿主用 `.map(...)` 把 [`DataPickMsg`] 包进自己的消息。

use iced::widget::{button, column, pick_list, row, text, text_input};
use iced::{Color, Element, Length};

use super::data_picker::{
    self as dp, market_label, source_label, DataPick, DataPickMsg, Load, PickOpts,
};

const C_DIM: Color = Color::from_rgb(0.55, 0.58, 0.64);
const C_OK: Color = Color::from_rgb(0.35, 0.78, 0.50);
const C_BAD: Color = Color::from_rgb(0.90, 0.40, 0.40);

/// 下拉框的一项：键 + 显示名。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Choice {
    key: String,
    label: String,
}

impl std::fmt::Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

fn label<'a>(t: &str) -> Element<'a, DataPickMsg> {
    text(t.to_string()).size(11).color(C_DIM).into()
}

fn pick<'a>(
    options: Vec<Choice>,
    selected: Option<&str>,
    placeholder: &str,
    width: f32,
    on: impl Fn(String) -> DataPickMsg + 'a,
) -> Element<'a, DataPickMsg> {
    let sel = selected.and_then(|k| options.iter().find(|c| c.key == k).cloned());
    pick_list(options, sel, move |c: Choice| on(c.key))
        .placeholder(placeholder.to_string())
        .text_size(11)
        .padding([2, 6])
        .width(Length::Fixed(width))
        .into()
}

/// 组件本体。
pub fn view<'a>(p: &DataPick, opts: &PickOpts) -> Element<'a, DataPickMsg> {
    let cat = dp::catalog();
    let mut r = row![label("数据管线")].spacing(6).align_y(iced::Alignment::Center);
    let mut note: (String, Color) = (String::new(), C_DIM);

    match &cat {
        Load::Loading => {
            r = r.push(text("正在读目录…").size(11).color(C_DIM));
        }
        Load::Failed(e) => {
            note = (format!("读目录失败：{e}"), C_BAD);
            r = r.push(text("目录不可用").size(11).color(C_BAD));
        }
        Load::Ready(c) => {
            // 数据源
            let mut srcs: Vec<Choice> = Vec::new();
            if opts.live {
                srcs.push(Choice { key: dp::LIVE.into(), label: source_label(dp::LIVE) });
            }
            for s in &c.sources {
                if opts.sources.as_ref().is_some_and(|only| !only.contains(&s.name.as_str())) {
                    continue;
                }
                let n = c.entries.iter().filter(|e| e.source == s.name).count();
                srcs.push(Choice {
                    key: s.name.clone(),
                    label: if s.available {
                        format!("{}（{n} 个标的）", source_label(&s.name))
                    } else {
                        format!("{}（目录不在）", source_label(&s.name))
                    },
                });
            }
            r = r.push(pick(srcs, p.source.as_deref(), "选数据管线", 230.0, DataPickMsg::Source));

            if let Some(src) = p.source.as_deref().filter(|s| *s != dp::LIVE) {
                // 市场
                let markets: Vec<Choice> = c
                    .markets(src)
                    .into_iter()
                    .map(|m| Choice { label: market_label(&m), key: m })
                    .collect();
                r = r.push(label("市场")).push(pick(markets, p.market.as_deref(), "选市场", 100.0, DataPickMsg::Market));

                // 标的（带搜索）
                if let Some(mk) = p.market.as_deref() {
                    let all = c.symbols(src, mk, "").len();
                    let syms: Vec<Choice> = c
                        .symbols(src, mk, &p.search)
                        .into_iter()
                        .map(|e| Choice {
                            key: e.symbol.clone(),
                            label: if e.days > 1 {
                                format!("{} · {} 天", e.symbol, e.days)
                            } else {
                                e.symbol.clone()
                            },
                        })
                        .collect();
                    let shown = syms.len();
                    r = r.push(label("标的"));
                    if all > 8 {
                        r = r.push(
                            text_input("搜索代码", &p.search)
                                .on_input(DataPickMsg::Search)
                                .size(11)
                                .width(Length::Fixed(90.0)),
                        );
                    }
                    r = r.push(pick(syms, p.symbol.as_deref(), &format!("选标的（{shown}）"), 150.0, DataPickMsg::Symbol));

                    // 日期
                    if opts.date
                        && let Some(sym) = p.symbol.as_deref()
                    {
                        {
                            r = r.push(label("日期"));
                            match dp::dates(src, sym) {
                                Load::Loading => r = r.push(text("查日期…").size(11).color(C_DIM)),
                                Load::Failed(e) => {
                                    note = (format!("查日期失败：{e}"), C_BAD);
                                    r = r.push(text("—").size(11).color(C_BAD));
                                }
                                Load::Ready(ds) => {
                                    let latest = ds.last().cloned();
                                    let n = ds.len();
                                    let items: Vec<Choice> = ds
                                        .into_iter()
                                        .rev()
                                        .map(|d| Choice { key: d.clone(), label: d })
                                        .collect();
                                    r = r.push(pick(items, p.date.as_deref(), &format!("选日期（{n}）"), 120.0, DataPickMsg::Date));
                                    if let Some(d) = latest
                                        && p.date.as_deref() != Some(d.as_str())
                                    {
                                        r = r.push(
                                            button(text("最新").size(10))
                                                .padding([2, 6])
                                                .style(|t, st| crate::style::button::modifier(t, st, false))
                                                .on_press(DataPickMsg::Date(d)),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // 说明行
            if note.0.is_empty() {
                note = if p.is_live() {
                    ("实时：读常驻通道，不选市场 / 标的 / 日期".into(), C_OK)
                } else if let Some(s) = p.selection(opts) {
                    let e = c.entry(&s.source, &s.symbol);
                    (
                        format!(
                            "已选：{} · {} · {}{}{}",
                            source_label(&s.source),
                            market_label(&s.market),
                            s.symbol,
                            s.date.as_deref().map(|d| format!(" · {d}")).unwrap_or_default(),
                            e.map(|e| format!("（该标的共 {} 天：{} ~ {}）", e.days, e.first, e.last))
                                .unwrap_or_default()
                        ),
                        C_OK,
                    )
                } else {
                    ("依次选数据管线 → 市场 → 标的".to_string() + if opts.date { " → 日期" } else { "" }, C_DIM)
                };
            }
            if let Some(s) = c.sources.iter().find(|s| Some(s.name.as_str()) == p.source.as_deref())
                && !s.available
            {
                note = (format!("{} 的数据目录不在：{}", source_label(&s.name), s.root), C_BAD);
            }
        }
    }
    r = r.push(
        button(text("↻ 刷新目录").size(10))
            .padding([2, 6])
            .style(|t, st| crate::style::button::modifier(t, st, false))
            .on_press(DataPickMsg::Refresh),
    );
    let mut b = column![r.wrap()].spacing(3);
    if !note.0.is_empty() {
        b = b.push(text(note.0).size(10).color(note.1));
    }
    b.into()
}
