//! 浮窗（docs/64 §2）：工具栏下拉卡片与深层设置的共用外壳。
//!
//! **开没开由各自的模块管**（指标浏览器、分析、事件……各有自己的状态），这里只管它们开着时
//! 摆在哪、谁在上面、怎么拖：
//! - 每种浮窗（[`WinKey`]）记自己的位置（本次运行内；重启回默认位置）；第一次打开时比已开着的
//!   往右下错开 [`CASCADE`]，不正好叠住。
//! - 可以同时开多个；叠放次序 = 出现的先后，按住标题栏或点卡片空白处就到最上层；Esc 只关最上面那个（[`top`]）。
//! - 横向整张卡夹在区域里；竖向**只要标题栏留在区域里**，上下随便拖（拖过头停在边上、往回拖立刻跟手；
//!   窗口变小了也拉得回来）。卡的高度封顶在每种浮窗的 `h`，再不超过它下面剩的空间——摆得靠下时卡变矮、
//!   内容在卡里滚动，不会被压扁看不见（docs/63 §14 踩过）。
//!   以前竖向按封顶高夹整张卡：封顶高往往比卡实际高、窗口又不够高，结果几乎只能横着拖（10-28 用户报）。
//! - 浮窗以外的地方照常可拖可画。

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use iced::widget::{column, container, mouse_area, opaque, pin, row, scrollable, Space};
use iced::{Alignment, Element, Length, Size};

use super::{metrics, pal, text as t, widgets as w};

/// 浮窗的种类（同一种同时只开一个）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WinKey {
    /// 指标浏览器（工具栏「指标 ▾」）
    StudyMenu,
    /// 单个指标的设置
    StudyEdit,
    /// 分析（工具栏「分析 ▾」）
    Analysis,
    /// 回放事件筛选（工具栏「事件 ▾」）
    Events,
    /// 「⋯ 页面」
    Page,
    /// 工具栏「数据 ▾」：管线 → 来源 → 市场 → 标的 → 时间
    Data,
    /// 发起回测（回测结果面板）
    Backtest,
    /// 特征矩阵的口径设置
    Caliber,
    /// AI 接口与密钥
    AiKeys,
    /// Telegram 配置
    Telegram,
    /// 消息中心（顶栏 🔔，docs/65）
    Inbox,
    /// 提醒管理器（docs/65）
    AlertRules,
    /// 市场状态设置（图例齿轮 / 工具栏「状态」，docs/70 C）
    Regime,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FloatMsg {
    /// 按住标题栏：开始拖（也到最上层）
    DragStart(WinKey),
    /// 点了卡片：到最上层
    Raise(WinKey),
    /// ✕：由宿主转给各自的模块去关
    Close(WinKey),
    Move(f32, f32),
    Up,
}

/// 第一次打开时离顶的距离、与已开着的错开多少。
pub const TOP: f32 = 72.0;
pub const CASCADE: f32 = 24.0;
/// 夹边时离区域边缘留的空。
const MARGIN: f32 = 4.0;

struct Drag {
    key: WinKey,
    /// 起点等第一次移动时记（按下时拿不到窗口坐标）
    start: Option<(f32, f32)>,
    base: (f32, f32),
}

#[derive(Default)]
struct State {
    /// 开着的，从下到上
    z: Vec<WinKey>,
    /// 左上角（区域坐标）
    pos: HashMap<WinKey, (f32, f32)>,
    /// 宽、封顶高（画的时候记下，拖动时按它夹）
    size: HashMap<WinKey, (f32, f32)>,
    area: (f32, f32),
    drag: Option<Drag>,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// 最上面那个（Esc 关它）。
#[must_use]
pub fn top() -> Option<WinKey> {
    state().z.last().copied()
}

/// 正在拖（宿主据此铺一层整窗接鼠标的层）。
#[must_use]
pub fn dragging() -> bool {
    state().drag.is_some()
}

/// 处理浮窗消息；返回要关掉的那个（宿主转给它的模块）。
pub fn update(m: FloatMsg) -> Option<WinKey> {
    let mut s = state();
    match m {
        FloatMsg::DragStart(k) => {
            raise(&mut s, k);
            let base = s.pos.get(&k).copied().unwrap_or_default();
            s.drag = Some(Drag { key: k, start: None, base });
        }
        FloatMsg::Raise(k) => raise(&mut s, k),
        FloatMsg::Close(k) => {
            s.drag = None;
            return Some(k);
        }
        FloatMsg::Move(x, y) => {
            let d = s.drag.as_mut()?;
            match d.start {
                None => d.start = Some((x, y)),
                Some((sx, sy)) => {
                    let (k, p) = (d.key, (d.base.0 + x - sx, d.base.1 + y - sy));
                    let p = clamp(&s, k, p);
                    s.pos.insert(k, p);
                }
            }
        }
        FloatMsg::Up => s.drag = None,
    }
    None
}

fn raise(s: &mut State, k: WinKey) {
    if let Some(i) = s.z.iter().position(|x| *x == k) {
        let k = s.z.remove(i);
        s.z.push(k);
    }
}

/// 竖向至少留在区域里的高度（标题栏 + 一点内容）：拖到最低也抓得回来。
const KEEP: f32 = 72.0;

/// 卡最高多高：封顶高，再不超过左上角在 `y` 时下面剩的空间。
fn card_h(area: (f32, f32), h: f32, y: f32) -> f32 {
    h.min((area.1 - y - MARGIN).max(KEEP))
}

/// 左上角夹到区域里：横向整张卡在里面，竖向标题栏在里面（[`KEEP`]）；区域还不知道时不夹。
fn clamp(s: &State, k: WinKey, p: (f32, f32)) -> (f32, f32) {
    let (aw, ah) = s.area;
    if aw <= 0.0 || ah <= 0.0 {
        return p;
    }
    let w = s.size.get(&k).map_or(0.0, |x| x.0);
    let x = p.0.clamp(MARGIN.min(aw), (aw - w - MARGIN).max(MARGIN.min(aw)));
    let y = p.1.clamp(MARGIN.min(ah), (ah - KEEP - MARGIN).max(MARGIN.min(ah)));
    (x, y)
}

/// 记下这一帧开着哪些（顺序 = 宿主给的顺序），新出现的放到最上层、没记过位置的给默认位置；
/// 关掉的从次序里拿掉（位置留着，下次打开回到原处）。返回从下到上的次序。
fn sync(area: (f32, f32), shown: &[(WinKey, f32, f32)]) -> Vec<WinKey> {
    let mut s = state();
    s.area = area;
    s.z.retain(|k| shown.iter().any(|(x, ..)| x == k));
    for &(k, w, h) in shown {
        s.size.insert(k, (w, h));
        if !s.z.contains(&k) {
            if !s.pos.contains_key(&k) {
                let n = s.z.len() as f32;
                let p = ((area.0 - w) / 2.0 + CASCADE * n, TOP + CASCADE * n);
                let p = clamp(&s, k, p);
                s.pos.insert(k, p);
            }
            s.z.push(k);
        }
    }
    if s.drag.as_ref().is_some_and(|d| !s.z.contains(&d.key)) {
        s.drag = None;
    }
    s.z.clone()
}

/// 一个浮窗：种类、标题、内容、宽、封顶高。
pub struct Win<'a, M> {
    pub key: WinKey,
    pub title: String,
    pub body: Element<'a, M>,
    pub w: f32,
    pub h: f32,
}

/// 把开着的浮窗按次序摞在 `area` 里（由宿主放在内容上方，`area` 来自 `responsive`）。
pub fn layer<'a, M: Clone + 'a>(area: Size, wins: Vec<Win<'a, M>>, wrap: impl Fn(FloatMsg) -> M + Copy + 'a) -> Element<'a, M> {
    let a = (area.width, area.height);
    // 宽不超过区域
    let shown: Vec<(WinKey, f32, f32)> = wins.iter().map(|x| (x.key, x.w.min((a.0 - 2.0 * MARGIN).max(120.0)), x.h)).collect();
    let order = sync(a, &shown);
    let (pos, dragging) = {
        let s = state();
        (order.iter().map(|k| clamp(&s, *k, s.pos.get(k).copied().unwrap_or_default())).collect::<Vec<_>>(), s.drag.as_ref().map(|d| d.key))
    };
    let mut wins: Vec<Option<Win<'a, M>>> = wins.into_iter().map(Some).collect();
    let mut st = iced::widget::Stack::new().width(Length::Fill).height(Length::Fill);
    for (k, (x, y)) in order.iter().zip(pos) {
        let Some(i) = shown.iter().position(|(s, ..)| s == k) else { continue };
        let Some(win) = wins[i].take() else { continue };
        let (_, w, h) = shown[i];
        let head = mouse_area(
            row![
                t::section(win.title),
                t::metadata("按住这里拖动").color(pal::dim()),
                Space::new().width(Length::Fill),
                w::btn("✕", w::Kind::Ghost, Some(wrap(FloatMsg::Close(*k)))),
            ]
            .spacing(metrics::space(2))
            .align_y(Alignment::Center),
        )
        .on_press(wrap(FloatMsg::DragStart(*k)))
        .interaction(if dragging == Some(*k) { iced::mouse::Interaction::Grabbing } else { iced::mouse::Interaction::Grab });
        let card = column![head, scrollable(win.body).width(Length::Fill)].spacing(metrics::space(2));
        let card = mouse_area(
            container(card)
                .padding(metrics::space(4))
                .width(Length::Fixed(w))
                .max_height(card_h(a, h, y))
                .style(w::float_card),
        )
        .on_press(wrap(FloatMsg::Raise(*k)));
        st = st.push(pin(opaque(card)).x(x).y(y));
    }
    st.into()
}

/// 测试用：清空。
#[cfg(test)]
pub fn reset() {
    *state() = State::default();
}

/// 浮窗的状态是进程级的，测试要串行。
#[cfg(test)]
pub static TEST_GUARD: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 多个浮窗_错开_点哪个哪个到上面_关掉的位置留着() {
        let _g = TEST_GUARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        reset();
        let area = (1600.0, 1000.0);
        let z = sync(area, &[(WinKey::StudyMenu, 600.0, 500.0)]);
        assert_eq!(z, vec![WinKey::StudyMenu]);
        let z = sync(area, &[(WinKey::StudyMenu, 600.0, 500.0), (WinKey::Analysis, 600.0, 500.0)]);
        assert_eq!(z, vec![WinKey::StudyMenu, WinKey::Analysis]);
        let p = |k| state().pos[&k];
        assert_eq!(p(WinKey::StudyMenu), (500.0, TOP));
        assert_eq!(p(WinKey::Analysis), (500.0 + CASCADE, TOP + CASCADE));
        // 点下面那个：到上面
        update(FloatMsg::Raise(WinKey::StudyMenu));
        assert_eq!(top(), Some(WinKey::StudyMenu));
        // ✕ 交给宿主去关
        assert_eq!(update(FloatMsg::Close(WinKey::StudyMenu)), Some(WinKey::StudyMenu));
        let z = sync(area, &[(WinKey::Analysis, 600.0, 500.0)]);
        assert_eq!(z, vec![WinKey::Analysis]);
        // 再打开：回到原处、在最上面
        let z = sync(area, &[(WinKey::Analysis, 600.0, 500.0), (WinKey::StudyMenu, 600.0, 500.0)]);
        assert_eq!(z, vec![WinKey::Analysis, WinKey::StudyMenu]);
        assert_eq!(p(WinKey::StudyMenu), (500.0, TOP));
    }

    #[test]
    fn 拖动_夹在区域里_光标回来就跟手() {
        let _g = TEST_GUARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        reset();
        let area = (1600.0, 1000.0);
        sync(area, &[(WinKey::StudyEdit, 560.0, 540.0)]);
        update(FloatMsg::DragStart(WinKey::StudyEdit));
        assert!(dragging());
        update(FloatMsg::Move(100.0, 100.0)); // 第一次只记起点
        update(FloatMsg::Move(100.0, 400.0)); // 竖着拖：不受卡高限制
        assert_eq!(state().pos[&WinKey::StudyEdit], (520.0, TOP + 300.0));
        update(FloatMsg::Move(5100.0, 5100.0)); // 拖过头：横向整卡在里面，竖向标题栏留在里面
        assert_eq!(state().pos[&WinKey::StudyEdit], (1600.0 - 560.0 - MARGIN, 1000.0 - KEEP - MARGIN));
        assert_eq!(card_h((1600.0, 1000.0), 540.0, 1000.0 - KEEP - MARGIN), KEEP, "摆到最低：卡变矮、内容在卡里滚动");
        update(FloatMsg::Move(200.0, 200.0)); // 光标回到区域里：卡跟着光标（按下时的位置 + 位移）
        assert_eq!(state().pos[&WinKey::StudyEdit], (520.0 + 100.0, TOP + 100.0));
        update(FloatMsg::Up);
        assert!(!dragging());
        // 窗口变小：横向拉回区域里；竖向标题栏还在里面就不动，变得更矮才往上拉
        sync((800.0, 300.0), &[(WinKey::StudyEdit, 560.0, 540.0)]);
        let s = state();
        assert_eq!(clamp(&s, WinKey::StudyEdit, s.pos[&WinKey::StudyEdit]), (800.0 - 560.0 - MARGIN, TOP + 100.0));
        drop(s);
        sync((800.0, 150.0), &[(WinKey::StudyEdit, 560.0, 540.0)]);
        let s = state();
        assert_eq!(clamp(&s, WinKey::StudyEdit, s.pos[&WinKey::StudyEdit]).1, 150.0 - KEEP - MARGIN);
    }
}
