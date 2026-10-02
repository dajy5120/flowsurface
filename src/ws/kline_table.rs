//! K 线的「数据表」替代视图（docs/35 §6.3，UPDS V6 §44）：Ctrl Shift D 在图与表之间切换。
//!
//! 图看趋势，表看确切数值——要核对某根 K 线的开高低收、或者复制一段数据出去时，
//! 不必再对着十字线一根根读。用 `ui::grid`：可排序、调宽、多选合计、复制。
//! 每个面板各一份网格状态（按面板 id）。

use std::cell::RefCell;
use std::collections::HashMap;

use exchange::Kline;
use exchange::unit::MinTicksize;
use iced::Element;

use crate::ui::grid::{self, Cell, Column, GridMsg, GridState};

thread_local! {
    static STATES: RefCell<HashMap<uuid::Uuid, GridState>> = RefCell::new(HashMap::new());
}

fn cols() -> Vec<Column> {
    vec![
        Column::text("时间", 150.0).key(),
        Column::num("开", None, 96.0),
        Column::num("高", None, 96.0),
        Column::num("低", None, 96.0),
        Column::num("收", None, 96.0),
        Column::num("涨跌", Some("%"), 70.0),
        Column::num("量", None, 96.0),
    ]
}

pub fn handle(pane: uuid::Uuid, m: GridMsg) {
    let cols = cols();
    STATES.with(|s| s.borrow_mut().entry(pane).or_insert_with(|| GridState::new(&cols)).update(m, &cols, &[]));
}

/// `klines` 新的在前。`precision` = 标的最小价位（价格按它的小数位显示）。
pub fn view<'a>(pane: uuid::Uuid, klines: &[Kline], precision: Option<MinTicksize>) -> Element<'a, GridMsg> {
    let cols = cols();
    let px = |p: exchange::unit::Price| match precision {
        Some(t) => p.to_string(t),
        None => format!("{}", p.to_f32()),
    };
    let rows: Vec<Vec<Cell>> = klines
        .iter()
        .map(|k| {
            let (o, c) = (k.open.to_f32() as f64, k.close.to_f32() as f64);
            let chg = if o != 0.0 { (c - o) / o * 100.0 } else { 0.0 };
            let t = chrono::DateTime::from_timestamp_millis(k.time.as_u64() as i64)
                .map(|d| d.format("%m-%d %H:%M:%S").to_string())
                .unwrap_or_default();
            let vol = k.volume.total().to_f32_lossy() as f64;
            vec![
                Cell::Text(t),
                Cell::num(o, px(k.open)),
                Cell::num(k.high.to_f32() as f64, px(k.high)),
                Cell::num(k.low.to_f32() as f64, px(k.low)),
                Cell::num(c, px(k.close)),
                Cell::Colored(
                    format!("{chg:+.2}"),
                    if chg >= 0.0 { crate::ui::chart::up() } else { crate::ui::chart::down() },
                ),
                Cell::num(vol, crate::ui::fmt::compact(vol, 2)),
            ]
        })
        .collect();
    let st = STATES.with(|s| {
        let mut s = s.borrow_mut();
        let st = s.entry(pane).or_insert_with(|| GridState::new(&cols));
        st.resort(&cols, &rows);
        st.clone()
    });
    let foot = format!("最新在上 · 最多 {} 根 · Ctrl Shift D 回到图", klines.len());
    grid::view(cols, rows, st, Some(foot), |m| m)
}
