//! Cockpit 的 **UPDS 适配层**（docs/35 批 2）：设计 token → iced。
//!
//! 界面上的一切颜色、字号、间距最终都应从这里取——`tests/ui_hardcode_ratchet.rs`
//! 盯着其余文件里的硬编码只减不增，本目录是唯一的合法落点。
//!
//! # 状态
//!
//! 偏好（主题、密度、涨跌约定……）存在 `~/.config/wealthspring/ui.json`，Studio 也读同一份。
//! 这里缓存一份，[`poll`] 每秒看一次文件修改时间，变了就重读——在 Studio 里切主题，
//! Cockpit 一秒内跟着变。
//!
//! 取值函数（[`core`]、[`domain`]、[`density`]）都是读全局缓存，渲染线程里随便调，
//! 不做 IO。

pub mod chart;
pub mod command;
pub mod fmt;
pub mod gallery;
pub mod grid;
pub mod metrics;
pub mod pal;
pub mod shell;
pub mod text;
pub mod theme;
pub mod widgets;

use std::sync::{LazyLock, RwLock};
use std::time::{Duration, Instant, SystemTime};

pub use wealthspring_ui_tokens as tokens;
#[allow(unused_imports)] // UpDown 给批 3 的设置入口用
pub use wealthspring_ui_tokens::{Density, Domain, Prefs, Role, ThemeId, UpDown};

struct State {
    prefs: Prefs,
    domain: Domain,
    iced: iced::Theme,
    mtime: Option<SystemTime>,
    polled: Instant,
}

impl State {
    fn new(prefs: Prefs) -> Self {
        Self {
            domain: Domain::resolve(prefs.theme, &prefs),
            iced: theme::iced_theme(prefs.theme),
            mtime: Prefs::mtime(),
            polled: Instant::now(),
            prefs,
        }
    }
}

static STATE: LazyLock<RwLock<State>> = LazyLock::new(|| RwLock::new(State::new(Prefs::load())));

fn read<T>(f: impl FnOnce(&State) -> T) -> T {
    match STATE.read() {
        Ok(g) => f(&g),
        Err(e) => f(&e.into_inner()),
    }
}

/// 当前偏好（克隆一份）。
pub fn prefs() -> Prefs {
    read(|s| s.prefs.clone())
}

pub fn theme_id() -> ThemeId {
    read(|s| s.prefs.theme)
}

pub fn density() -> Density {
    read(|s| s.prefs.density)
}

/// 当前主题的 UPDS 核心语义色。
pub fn core() -> &'static tokens::Core {
    theme_id().core()
}

/// 按主题 + 涨跌约定 + 色弱开关解析好的领域色。
pub fn domain() -> Domain {
    read(|s| s.domain)
}

/// 给 iced 的主题（`Flowsurface::theme` 每帧调；克隆只是 Arc 计数）。
pub fn iced_theme() -> iced::Theme {
    read(|s| s.iced.clone())
}

/// 看一眼 `ui.json` 有没有被改过（Studio 或手工）。节流到每秒一次，返回是否重读了。
pub fn poll() -> bool {
    let due = read(|s| s.polled.elapsed() >= Duration::from_secs(1));
    if !due {
        return false;
    }
    let now = Prefs::mtime();
    let Ok(mut g) = STATE.write() else { return false };
    g.polled = Instant::now();
    if now == g.mtime {
        return false;
    }
    *g = State::new(Prefs::load());
    g.mtime = now;
    log::info!("[ui] 偏好已更新：主题 {} · 密度 {}", g.prefs.theme.label(), g.prefs.density.label());
    true
}

/// 改偏好并落盘（Studio 会跟着变）。
pub fn update(f: impl FnOnce(&mut Prefs)) {
    let mut p = prefs();
    f(&mut p);
    if let Err(e) = p.save() {
        log::error!("[ui] 写 {} 失败：{e}", Prefs::path().display());
    }
    if let Ok(mut g) = STATE.write() {
        *g = State::new(p);
    }
}

/// Ctrl Alt T：循环主题。
pub fn cycle_theme() {
    update(|p| p.theme = p.theme.next());
}

/// Ctrl Alt D：循环密度。
pub fn cycle_density() {
    update(|p| p.density = p.density.next());
}

/// token 颜色 → iced 颜色。
pub fn color(c: tokens::Rgba) -> iced::Color {
    iced::Color { r: c.r, g: c.g, b: c.b, a: c.a }
}
