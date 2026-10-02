//! 自绘图的「数据表」替代视图（docs/35 §6.3 / §16.15 第 9 项）：Ctrl Shift D 在图与表之间切换。
//!
//! K 线有专门的 [`super::kline_table`]；其余自绘图（自有数据图、Tardis 历史面板）都是
//! 「横轴一列 + 若干数值序列」，共用这一份：`ui::grid`，可排序、调宽、多选合计、复制。
//! 每个面板各一份网格状态（按面板 id）。

use std::cell::RefCell;
use std::collections::HashMap;

use iced::Element;
use iced::widget::{column, text};

use crate::ui::grid::{self, Cell, Column, GridMsg, GridState};

thread_local! {
    /// 面板 id → （上一次的列, 网格状态）。列存下来，交互时按真实的列处理（排序类型、对齐）
    static STATES: RefCell<HashMap<uuid::Uuid, (Vec<Column>, GridState)>> = RefCell::new(HashMap::new());
}

/// 一张表的数据：横轴 + 若干列。
pub struct Table {
    pub title: String,
    pub x_label: String,
    pub x_is_time: bool,
    pub x: Vec<f64>,
    pub cols: Vec<(String, Vec<f64>)>,
}

fn columns(t: &Table) -> Vec<Column> {
    let mut v = vec![Column::text(if t.x_label.is_empty() { "X" } else { &t.x_label }, 170.0).key()];
    v.extend(t.cols.iter().map(|(n, _)| Column::num(n.clone(), None, 110.0)));
    v
}

fn fmt_x(x: f64, is_time: bool) -> String {
    if is_time {
        chrono::DateTime::from_timestamp_millis(x as i64)
            .map(|d| d.format("%Y-%m-%d %H:%M:%S%.3f").to_string())
            .unwrap_or_else(|| crate::ui::fmt::invalid_at(&format!("数据表时间戳无法换算（{x}）")))
    } else {
        format!("{x}")
    }
}

fn fmt_v(v: f64) -> String {
    let a = v.abs();
    if a >= 1e6 {
        crate::ui::fmt::compact(v, 2)
    } else if a >= 1.0 || a == 0.0 {
        crate::ui::fmt::number(v, 4, crate::ui::fmt::Rounding::Measurement)
    } else {
        format!("{v:.6}")
    }
}

pub fn handle(pane: uuid::Uuid, m: GridMsg) {
    STATES.with(|s| {
        if let Some((cols, st)) = s.borrow_mut().get_mut(&pane) {
            st.update(m, cols, &[]);
        }
    });
}

pub fn view<'a>(pane: uuid::Uuid, t: &Table) -> Element<'a, GridMsg> {
    let cols = columns(t);
    // 新的在上（与 K 线数据表一致）
    let rows: Vec<Vec<Cell>> = (0..t.x.len())
        .rev()
        .map(|i| {
            let mut r = vec![Cell::Text(fmt_x(t.x[i], t.x_is_time))];
            r.extend(t.cols.iter().map(|(_, v)| match v.get(i).copied() {
                Some(x) if x.is_finite() => Cell::num(x, fmt_v(x)),
                Some(_) => Cell::Absent(crate::ui::fmt::Absence::Invalid),
                None => Cell::Absent(crate::ui::fmt::Absence::Missing),
            }));
            r
        })
        .collect();
    let st = STATES.with(|s| {
        let mut s = s.borrow_mut();
        let e = s.entry(pane).or_insert_with(|| (cols.clone(), GridState::new(&cols)));
        // 换了列（换了数据类型 / 文件）就换一份状态，旧的列宽、排序对不上
        let titles = |c: &[Column]| c.iter().map(|x| x.title.clone()).collect::<Vec<_>>();
        if titles(&e.0) != titles(&cols) {
            *e = (cols.clone(), GridState::new(&cols));
        }
        e.1.resort(&cols, &rows);
        e.1.clone()
    });
    let foot = format!("{} 行 · 最新在上 · Ctrl Shift D 回到图", rows.len());
    column![
        text(format!("数据表 · {}", t.title)).size(crate::ui::text::s_small()).color(crate::ui::pal::head()),
        grid::view(cols, rows, st, Some(foot), |m| m),
    ]
    .spacing(crate::ui::metrics::space(2))
    .padding(crate::ui::metrics::space(3))
    .into()
}

/// 自有数据图的表。
pub fn from_selfdata() -> Table {
    let d = super::customchart::snapshot();
    Table {
        title: format!("自有数据图 · {}", d.source.rsplit('/').next().unwrap_or_default()),
        x_label: d.x_label.clone(),
        x_is_time: d.x_is_time,
        x: d.x.clone(),
        cols: d.series.clone(),
    }
}

/// Tardis 历史面板当前那张主图的表（剖面图没有时间轴，跳过）。
pub fn from_tardis() -> Option<Table> {
    let p = super::tardis_board_readout::panel();
    let ch = p.charts.iter().find(|c| c.kind != "profile")?;
    let cols = match ch.kind.as_str() {
        "candle" => vec![
            ("开".to_string(), ch.o.clone()),
            ("高".into(), ch.h.clone()),
            ("低".into(), ch.l.clone()),
            ("收".into(), ch.c.clone()),
            ("量".into(), ch.v.clone()),
        ],
        "scatter" => {
            let mut v = vec![(if ch.y_label.is_empty() { "Y".to_string() } else { ch.y_label.clone() }, ch.y.clone())];
            if !ch.size.is_empty() {
                v.push(("大小".into(), ch.size.clone()));
            }
            v
        }
        "heatmap" => vec![("中价".to_string(), ch.mid.clone())],
        _ => ch.series.clone(),
    };
    Some(Table {
        title: format!("{} · {} {}", ch.title, p.symbol, p.date),
        x_label: if ch.x_is_time { "时间（UTC）".into() } else { "X".into() },
        x_is_time: ch.x_is_time,
        x: ch.x.clone(),
        cols,
    })
}
