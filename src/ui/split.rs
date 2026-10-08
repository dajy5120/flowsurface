//! 可拖拽的分区（面板内部的左右 / 上下分栏）。
//!
//! 用法：面板视图里 `split::row(key, items, on)` / `split::column(..)`，每一项给**缺省占比**与**最小像素**。
//! 相邻两项之间画一根分隔条，拖它只在这两项之间挪占比（总和不变）；双击分隔条全部复位成缺省。
//! 按比例而不是定宽：面板宽度会变（同页并排别的面板、拖检查器），定宽的几栏加起来一超，中间那栏就被挤没了。
//!
//! 占比按 `key` 记在 Cockpit 数据目录的 `splits.json`，重启照旧。消息 [`SplitMsg`] 由面板包成自己的消息、
//! 交给 [`handle`]——面板不用自己存状态。拖动时整个分区接住鼠标移动（光标离开分隔条照样跟），按位移改占比，
//! 按下的位置不准也不跳（同检查器拖宽，docs/42 第 2 期）。像素与占比的换算用上一帧量到的分区尺寸（`ui::mark::width`）。

use std::collections::BTreeMap;
use std::sync::Mutex;

use iced::widget::{Space, container, mouse_area};
use iced::{Background, Element, Length, Point};

use super::{color, core};

const FILE: &str = "splits.json";
/// 分隔条的可抓宽度（像素）；中间画 1px 的线
const GRIP: f32 = 6.0;

#[derive(Debug, Clone, PartialEq)]
pub enum SplitMsg {
    /// 按下第 `i` 根分隔条（第 i 项与第 i+1 项之间）
    Start { key: String, i: usize, shares: Vec<f32>, mins: Vec<f32> },
    /// 拖动中：光标位置（相对分区）
    Move(Point),
    End,
    /// 双击分隔条：全部回到缺省
    Reset { key: String },
}

struct Drag {
    key: String,
    i: usize,
    shares: Vec<f32>,
    mins: Vec<f32>,
    anchor: Option<f32>,
}

static SHARES: Mutex<Option<BTreeMap<String, Vec<f32>>>> = Mutex::new(None);
static DRAG: Mutex<Option<Drag>> = Mutex::new(None);

fn with_shares<T>(f: impl FnOnce(&mut BTreeMap<String, Vec<f32>>) -> T) -> T {
    let mut g = SHARES.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let m = g.get_or_insert_with(|| {
        std::fs::read_to_string(data::data_path(Some(FILE)))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    });
    f(m)
}

fn save() {
    let j = with_shares(|m| serde_json::to_string_pretty(m).unwrap_or_default());
    if let Err(e) = data::write_json_to_file(&j, FILE) {
        log::warn!("写 {FILE} 失败：{e}");
    }
}

/// 这一组现在的占比（存的不合用——项数变了——就用缺省），归一化到和为 1。
fn shares_of(key: &str, defaults: &[f32]) -> Vec<f32> {
    let v = with_shares(|m| m.get(key).cloned()).filter(|v| v.len() == defaults.len()).unwrap_or_else(|| defaults.to_vec());
    let sum: f32 = v.iter().sum::<f32>().max(f32::EPSILON);
    v.iter().map(|x| x / sum).collect()
}

/// 分区的长度（像素）：横向分栏是宽，纵向是高。上一帧量到的；还没量到返回 None。
fn extent(key: &str) -> Option<f32> {
    let k = format!("split:{key}");
    if key.ends_with("|v") { super::mark::height_of(&k) } else { super::mark::width_of(&k) }
}

/// 处理分区消息。
pub fn handle(m: SplitMsg) {
    match m {
        SplitMsg::Start { key, i, shares, mins } => {
            if let Ok(mut g) = DRAG.lock() {
                *g = Some(Drag { key, i, shares, mins, anchor: None });
            }
        }
        SplitMsg::Move(p) => {
            let Ok(mut g) = DRAG.lock() else { return };
            let Some(d) = g.as_mut() else { return };
            let vertical = d.key.ends_with("|v");
            let pos = if vertical { p.y } else { p.x };
            let anchor = *d.anchor.get_or_insert(pos);
            let Some(total) = extent(&d.key).filter(|t| *t > 1.0) else { return };
            let (i, j) = (d.i, d.i + 1);
            let pair = d.shares[i] + d.shares[j];
            // 两项各自的下限（像素换成占比）；两项加起来都放不下下限时不动
            let (lo_i, lo_j) = (d.mins[i] / total, d.mins[j] / total);
            if lo_i + lo_j >= pair {
                return;
            }
            let a = (d.shares[i] + (pos - anchor) / total).clamp(lo_i, pair - lo_j);
            let mut v = d.shares.clone();
            v[i] = a;
            v[j] = pair - a;
            let key = d.key.clone();
            drop(g);
            with_shares(|m| {
                m.insert(key, v);
            });
        }
        SplitMsg::End => {
            let was = DRAG.lock().ok().and_then(|mut g| g.take()).is_some();
            if was {
                save();
            }
        }
        SplitMsg::Reset { key } => {
            with_shares(|m| {
                m.remove(&key);
            });
            save();
        }
    }
}

fn dragging(key: &str) -> bool {
    DRAG.lock().ok().is_some_and(|g| g.as_ref().is_some_and(|d| d.key == key))
}

/// 左右分栏。`items` = (内容, 缺省占比, 最小宽度像素)。
pub fn row<'a, M: Clone + 'a>(key: &str, items: Vec<(Element<'a, M>, f32, f32)>, on: impl Fn(SplitMsg) -> M + Clone + 'a) -> Element<'a, M> {
    build(format!("{key}|h"), items, on)
}

/// 上下分栏。`items` = (内容, 缺省占比, 最小高度像素)。
pub fn column<'a, M: Clone + 'a>(key: &str, items: Vec<(Element<'a, M>, f32, f32)>, on: impl Fn(SplitMsg) -> M + Clone + 'a) -> Element<'a, M> {
    build(format!("{key}|v"), items, on)
}

/// 当前尺寸下低于下限的项（窗口缩窄了）按下限补足，从富余的项里按富余比例匀出来——谁都不被挤没。
fn fit_mins(shares: &[f32], need: &[f32]) -> Vec<f32> {
    if need.iter().sum::<f32>() >= 1.0 {
        return shares.to_vec();
    }
    let short: f32 = shares.iter().zip(need).map(|(s, n)| (n - s).max(0.0)).sum();
    if short <= 0.0 {
        return shares.to_vec();
    }
    let spare: f32 = shares.iter().zip(need).map(|(s, n)| (s - n).max(0.0)).sum::<f32>().max(f32::EPSILON);
    shares.iter().zip(need).map(|(s, n)| if s < n { *n } else { s - (s - n) / spare * short }).collect()
}

fn build<'a, M: Clone + 'a>(key: String, items: Vec<(Element<'a, M>, f32, f32)>, on: impl Fn(SplitMsg) -> M + Clone + 'a) -> Element<'a, M> {
    let vertical = key.ends_with("|v");
    let defaults: Vec<f32> = items.iter().map(|(_, s, _)| *s).collect();
    let mins: Vec<f32> = items.iter().map(|(_, _, m)| *m).collect();
    let mut shares = shares_of(&key, &defaults);
    if let Some(total) = extent(&key).filter(|t| *t > 1.0) {
        let need: Vec<f32> = mins.iter().map(|m| m / total).collect();
        shares = fit_mins(&shares, &need);
    }
    let n = items.len();
    let mut parts: Vec<Element<'a, M>> = Vec::with_capacity(n * 2);
    for (i, (el, _, _)) in items.into_iter().enumerate() {
        let len = Length::FillPortion(((shares[i] * 10_000.0).round() as u16).max(1));
        parts.push(if vertical {
            container(el).width(Length::Fill).height(len).into()
        } else {
            container(el).height(Length::Fill).width(len).into()
        });
        if i + 1 < n {
            parts.push(grip(&key, vertical, i, shares.clone(), mins.clone(), on.clone()));
        }
    }
    let body: Element<'a, M> = if vertical {
        iced::widget::Column::with_children(parts).width(Length::Fill).height(Length::Fill).into()
    } else {
        iced::widget::Row::with_children(parts).width(Length::Fill).height(Length::Fill).into()
    };
    let body = if dragging(&key) {
        let (on_m, on_e) = (on.clone(), on);
        mouse_area(body)
            .on_move(move |p| on_m(SplitMsg::Move(p)))
            .on_release(on_e(SplitMsg::End))
            .interaction(if vertical { iced::mouse::Interaction::ResizingVertically } else { iced::mouse::Interaction::ResizingHorizontally })
            .into()
    } else {
        body
    };
    super::mark::width(format!("split:{key}"), body)
}

fn grip<'a, M: Clone + 'a>(key: &str, vertical: bool, i: usize, shares: Vec<f32>, mins: Vec<f32>, on: impl Fn(SplitMsg) -> M + Clone + 'a) -> Element<'a, M> {
    let line = container(Space::new().width(if vertical { Length::Fill } else { Length::Fixed(1.0) }).height(if vertical { Length::Fixed(1.0) } else { Length::Fill }))
        .style(|_| container::Style { background: Some(Background::Color(color(core().border_default))), ..Default::default() });
    let pad = (GRIP - 1.0) / 2.0;
    let area = if vertical {
        container(line).padding(iced::Padding { top: pad, bottom: pad, left: 0.0, right: 0.0 }).width(Length::Fill)
    } else {
        container(line).padding(iced::Padding { left: pad, right: pad, top: 0.0, bottom: 0.0 }).height(Length::Fill)
    };
    let on2 = on.clone();
    mouse_area(area)
        .interaction(if vertical { iced::mouse::Interaction::ResizingVertically } else { iced::mouse::Interaction::ResizingHorizontally })
        .on_press(on(SplitMsg::Start { key: key.to_string(), i, shares, mins }))
        .on_double_click(on2(SplitMsg::Reset { key: key.to_string() }))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 缺省占比归一化() {
        let v = shares_of("test.none|h", &[1.0, 2.0, 1.0]);
        assert!((v.iter().sum::<f32>() - 1.0).abs() < 1e-6);
        assert!((v[1] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn 窗口缩窄时谁都不被挤没() {
        // 中间一栏只剩 5%，下限要 25%：从两边富余里匀出来，总和仍是 1
        let v = fit_mins(&[0.45, 0.05, 0.5], &[0.2, 0.25, 0.2]);
        assert!(v[1] >= 0.25 - 1e-6, "{v:?}");
        assert!(v[0] >= 0.2 && v[2] >= 0.2, "{v:?}");
        assert!((v.iter().sum::<f32>() - 1.0).abs() < 1e-4, "{v:?}");
    }
}
