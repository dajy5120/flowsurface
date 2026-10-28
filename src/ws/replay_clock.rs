//! 统一回放时钟（docs/51 P2）：特征回放的「现在」只有一个——播放头 `head`。
//!
//! 后端（`replay_source --timeline-only`）全速算、边算边写时间线；这里每帧推进播放头，四张图、特征矩阵、
//! 图表参数都按它取「这一刻」。暂停、步进、拖时间轴只改播放头，不再给进程发 SIGSTOP。
//!
//! - 播放：`head += 墙钟流逝 × 倍速`，到 frontier（后端已经算到哪）停住等；「最快」= 一直贴着 frontier。
//! - 往回：时间轴图只会累积、没有「截到某一刻」，所以要清图重灌——这里只**请求**（`epoch_req` 加一），
//!   主循环下一帧清完图再把 `epoch_live` 对齐（[`take_reset`]）；图表订阅按 `epoch_live` 建，清图之后才换新订阅，
//!   新订阅的数据不会被清图冲掉。
//! - 单位：纳秒，接收时刻 `ts_init` 时钟（与时间线同一个）。

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use iced::widget::{row, slider};
use iced::{Alignment, Element};

use super::timeline::Timeline;

/// 回放速度（按事件时间的倍数）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pace {
    X1,
    X10,
    X60,
    Max,
}

impl Pace {
    pub const ALL: [Self; 4] = [Self::X1, Self::X10, Self::X60, Self::Max];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::X1 => "1×（实时）",
            Self::X10 => "10×",
            Self::X60 => "60×",
            Self::Max => "最快",
        }
    }

    const fn short(self) -> &'static str {
        match self {
            Self::X1 => "1×",
            Self::X10 => "10×",
            Self::X60 => "60×",
            Self::Max => "最快",
        }
    }

    /// 倍数；`None` = 贴着 frontier。
    const fn rate(self) -> Option<f64> {
        match self {
            Self::X1 => Some(1.0),
            Self::X10 => Some(10.0),
            Self::X60 => Some(60.0),
            Self::Max => None,
        }
    }

    const fn next(self) -> Self {
        match self {
            Self::X1 => Self::X10,
            Self::X10 => Self::X60,
            Self::X60 => Self::Max,
            Self::Max => Self::X1,
        }
    }
}

/// 一步走多远。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Ms100,
    S1,
    /// 1 根 K 线（图表页签的 K 线是 M1）
    Bar,
}

impl Step {
    const fn ns(self) -> i64 {
        match self {
            Self::Ms100 => 100_000_000,
            Self::S1 => 1_000_000_000,
            Self::Bar => 60_000_000_000,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Ms100 => "100ms",
            Self::S1 => "1 秒",
            Self::Bar => "1 根 K 线",
        }
    }

    const fn next(self) -> Self {
        match self {
            Self::Ms100 => Self::S1,
            Self::S1 => Self::Bar,
            Self::Bar => Self::Ms100,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    TogglePlay,
    /// 前进 / 后退几个步长（负 = 后退）。步进会暂停。
    Step(i64),
    /// 前进 / 后退几秒（Shift + ← →）。
    Secs(i64),
    /// 拖时间轴：拖动中只显示位置（距起点的秒数），松手才跳。
    Drag(f64),
    Release,
    /// 跳到某一刻（纳秒；在 K 线 / Footprint 上单击）。会暂停。
    Seek(i64),
    CyclePace,
    CycleStep,
    /// 上一个 / 下一个事件（筛选里勾了的类；`[` `]`）
    PrevEvent,
    NextEvent,
    /// 事件筛选：展开 / 收起、勾一类、全选、全不选
    EventMenu,
    ToggleKind(String),
    AllKinds(bool),
    /// 条件跳转（docs/51 P5）：改条件、上一处 / 下一处（`{` `}`）
    CondInput(String),
    CondPrev,
    CondNext,
    /// 书签：在播放头加一个（`b`）、改笔记、跳过去、删掉
    AddBookmark,
    BookmarkNote(usize, String),
    BookmarkGo(usize),
    BookmarkDel(usize),
}

/// 一个书签（按「数据商 | 标的」存盘，下次回放同一段时还在）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Bookmark {
    /// 时刻（纳秒，接收时刻时钟）
    pub t: i64,
    #[serde(default)]
    pub note: String,
}

/// 书签文件（`WS_REPLAY_BOOKMARKS` 可覆盖）：`{"数据商|标的": [书签…]}`。
fn bookmarks_path() -> PathBuf {
    std::env::var_os("WS_REPLAY_BOOKMARKS")
        .map(PathBuf::from)
        .unwrap_or_else(|| super::paths::hot_dir().join("cockpit").join("replay_bookmarks.json"))
}

fn load_bookmarks(key: &str) -> Vec<Bookmark> {
    let all: std::collections::BTreeMap<String, Vec<Bookmark>> =
        std::fs::read(bookmarks_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let mut v = all.get(key).cloned().unwrap_or_default();
    v.sort_by_key(|b| b.t);
    v
}

fn save_bookmarks(key: &str, v: &[Bookmark]) {
    let p = bookmarks_path();
    let mut all: std::collections::BTreeMap<String, Vec<Bookmark>> =
        std::fs::read(&p).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    if v.is_empty() {
        all.remove(key);
    } else {
        all.insert(key.to_string(), v.to_vec());
    }
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    // 先写临时文件再改名：写到一半被打断也不会把别的标的的书签弄坏
    let tmp = p.with_extension("json.tmp");
    if let Ok(b) = serde_json::to_vec_pretty(&all)
        && std::fs::write(&tmp, b).is_ok()
    {
        let _ = std::fs::rename(&tmp, &p);
    }
}

/// 一组书签（导出画线分析时带上；docs/52 A3）。键见 [`Clock`] 的书签说明：`数据商|标的[#分析 id]`。
#[must_use]
pub fn bookmarks_of(key: &str) -> Vec<Bookmark> {
    load_bookmarks(key)
}

/// 导入画线分析时把它的书签并进来（同一时刻的不重复加）；回放正好在看这组就一起刷新。
pub fn merge_bookmarks(key: &str, add: &[Bookmark]) {
    let mut v = load_bookmarks(key);
    for b in add {
        if !v.iter().any(|x| x.t == b.t) {
            v.push(b.clone());
        }
    }
    v.sort_by_key(|b| b.t);
    save_bookmarks(key, &v);
    if let Ok(mut g) = CLOCK.lock()
        && let Some(c) = g.as_mut()
        && c.bm_key.as_deref() == Some(key)
    {
        c.bookmarks = v;
        c.marks_ver += 1;
    }
}

/// 条件命中在「聚焦」里的类型名（与事件共用聚焦那一套：虚线、工具栏读数、`[` `]` 从它接着找）。
pub const COND_KIND: &str = "条件";

/// 往前跳超过这么远就按往回跳处理：清图（保留视图）后在主循环里直接灌到播放头。
const FAR_JUMP_NS: i64 = 30_000_000_000;

/// 跳到事件时，播放头落在事件前多久（看清它是怎么形成的）。
pub const EVENT_LEAD_NS: i64 = 2_000_000_000;

/// 缺省勾选的事件：订单流里最常拿来看的几类（REPLENISH 一类十分钟上千次，默认不勾）。
const DEFAULT_KINDS: [&str; 8] =
    ["SWEEP", "RESP_ABSORPTION", "WALL_PULLED", "WALL_CONSUMED", "VACUUM", "EXHAUSTION", "FLOW_REVERSAL", "CVD_DIVERGENCE"];

/// 聚焦的事件（上一个 / 下一个跳到的那个）。
#[derive(Debug, Clone, PartialEq)]
pub struct Focus {
    pub kind: String,
    /// 检出的接收时刻 / 事件自己的时刻（纳秒）
    pub ti: i64,
    pub ts: i64,
    pub dir: i8,
    pub px: Option<f64>,
}

struct Clock {
    dir: PathBuf,
    t0: i64,
    t1: i64,
    head: i64,
    frontier: Option<i64>,
    done: bool,
    playing: bool,
    pace: Pace,
    step: Step,
    epoch_req: u64,
    epoch_live: u64,
    /// 请求清图的时刻：等在途的旧数据处理完再清（见 [`take_reset`]）
    reset_since: Option<Instant>,
    last_tick: Option<Instant>,
    last_poll: Option<Instant>,
    /// 拖时间轴中（距起点的秒数）
    drag: Option<f64>,
    tl: Option<Timeline>,
    /// 事件筛选：`None` = 还没定（时间线打开后按缺省勾）
    kinds: Option<std::collections::BTreeSet<String>>,
    focus: Option<Focus>,
    /// 聚焦时播放头放在哪：播放头没动过，上一个 / 下一个就从聚焦的事件接着找，否则从播放头找
    focus_head: i64,
    menu: bool,
    /// 事件标记的版本：聚焦或筛选变了加一（图按它重画）
    marks_ver: u64,
    cond_text: String,
    /// 条件写错了 / 没找到时的一行说明
    cond_note: String,
    /// 书签：`(数据商|标的, 书签)`；时间线打开时读
    bm_key: Option<String>,
    bookmarks: Vec<Bookmark>,
    /// 书签跟着哪份画线分析（docs/52 A2；`None` = 默认画板，键不带后缀）
    bm_scope: Option<String>,
    /// 打开分析时要回到的播放头：还没算到就等算到了再跳
    pending_head: Option<i64>,
}

static CLOCK: Mutex<Option<Clock>> = Mutex::new(None);

/// 上一次回放的视野同步作废（新开的图不能跳到那一刻去）。
fn clear_view_sync() {
    if let Ok(mut g) = VIEW.lock() {
        *g = (g.0 + 1, 0, None);
    }
}

/// 时间线的抬头读出来之前（回放进程还在读数据）用调用方给的时段。
pub fn begin(dir: PathBuf, t0: i64, t1: i64, pace: Pace) {
    clear_view_sync();
    if let Ok(mut g) = CLOCK.lock() {
        let epoch = g.as_ref().map_or(0, |c| c.epoch_req + 1);
        *g = Some(Clock {
            dir,
            t0,
            t1,
            head: t0,
            frontier: None,
            done: false,
            playing: true,
            pace,
            step: Step::S1,
            epoch_req: epoch,
            epoch_live: epoch,
            reset_since: None,
            last_tick: None,
            last_poll: None,
            drag: None,
            tl: None,
            kinds: None,
            focus: None,
            focus_head: i64::MIN,
            menu: false,
            marks_ver: 0,
            cond_text: String::new(),
            cond_note: String::new(),
            bm_key: None,
            bookmarks: Vec::new(),
            bm_scope: None,
            pending_head: None,
        });
    }
}

/// 回到实时（B1）：时钟停用。
pub fn end() {
    clear_view_sync();
    if let Ok(mut g) = CLOCK.lock() {
        *g = None;
    }
}

#[must_use]
pub fn active() -> bool {
    CLOCK.lock().is_ok_and(|g| g.is_some())
}

/// 播放头：`(时间线目录, head, epoch_live)`。图表订阅与矩阵按它取。
#[must_use]
pub fn target() -> Option<(PathBuf, i64, u64)> {
    let g = CLOCK.lock().ok()?;
    let c = g.as_ref()?;
    Some((c.dir.clone(), c.head, c.epoch_live))
}

/// 图表联动在不在（特征回放开着、看的是「订单流特征」）：拖动同步、单击跳转、播放头竖线、热图的「现在」都按它开关，
/// 别的工作区的图照旧。
#[must_use]
pub fn charts_on() -> bool {
    super::workspace::feature_feed() && active()
}

/// 在图上单击：播放头跳到那一刻（毫秒）。
pub fn seek_to_ms(ms: u64) {
    if charts_on() {
        handle(Cmd::Seek(ms as i64 * 1_000_000));
    }
}

/// 时间轴图之间的视野同步（docs/51 P3）：一张图被拖动 / 缩放，别的图把视野中心移到同一时刻（各自保留缩放）；
/// 双击时间轴回到最新 = 大家一起回到跟随最新。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewSync {
    /// 视野中心的时刻（毫秒）
    Center(u64),
    /// 跟随最新（播放头）
    Live,
}

/// `(版本, 来源图, 内容)`
static VIEW: Mutex<(u64, u64, Option<ViewSync>)> = Mutex::new((0, 0, None));

/// 用户在图 `src` 上拖动 / 缩放了。
pub fn publish_view(src: u64, v: ViewSync) {
    if !charts_on() {
        return;
    }
    if let Ok(mut g) = VIEW.lock()
        && g.2 != Some(v)
    {
        *g = (g.0 + 1, src, Some(v));
    }
}

/// 比 `seen` 新的视野同步：`(版本, 来源图, 内容)`。图在刷新时看一眼，来源是自己就只记版本。
#[must_use]
pub fn view_sync(seen: u64) -> Option<(u64, u64, ViewSync)> {
    if !charts_on() {
        return None;
    }
    let g = VIEW.lock().ok()?;
    let v = g.2?;
    (g.0 > seen).then_some((g.0, g.1, v))
}

/// 时间轴图（热图）的「现在」（毫秒）：特征回放开着、看的是「订单流特征」时 = 播放头；否则 `None`（照旧按墙钟外推）。
#[must_use]
pub fn chart_now_ms() -> Option<u64> {
    if !charts_on() {
        return None;
    }
    target().map(|(_, h, _)| (h.max(0) / 1_000_000) as u64)
}

/// 热图「从历史整体重画」的请求：`(代, 灌到的时刻 ms)`。时间线订阅从头灌到播放头时、停下来看的时候各发一次；
/// 热图收到的数据到了这一刻才重画（往前步进时数据还在路上）。
static REBUILD: Mutex<(u64, u64)> = Mutex::new((0, 0));

/// 时间线订阅：从头灌到播放头了（`upto_ms` = 灌到哪）。
pub fn request_rebuild(upto_ms: u64) {
    if let Ok(mut g) = REBUILD.lock() {
        *g = (g.0 + 1, upto_ms);
    }
}

/// 热图每帧看一眼：有没有新的重画请求（特征回放、看的是「订单流特征」时才有）。
#[must_use]
pub fn rebuild_req() -> Option<(u64, u64)> {
    if !charts_on() {
        return None;
    }
    REBUILD.lock().ok().map(|g| *g).filter(|g| g.0 > 0)
}

/// 往回跳之后等多久再清图（毫秒）。
///
/// 旧订阅一看到 `epoch_req` 变了就不再投递，但已经送进 iced 消息队列的那些还会被处理——
/// 立刻清图的话它们会落进新图，和新订阅从头灌的数据交错，时间倒退、热图卡死（连按几次 ← 时实测到）。
/// 等一小会儿让它们先落进旧图（随后被清掉）；这期间又往回跳了就重新计时，几次合成一次清图。
const RESET_SETTLE_MS: u128 = 80;

/// 正在一口气重灌（主循环里，往回跳之后）：热图只记历史、不逐列更新实时缓冲——灌完会从历史整体重画一次。
static BULK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_bulk(on: bool) {
    BULK.store(on, std::sync::atomic::Ordering::Relaxed);
}

#[must_use]
pub fn bulk() -> bool {
    BULK.load(std::sync::atomic::Ordering::Relaxed)
}

/// 主循环每帧调：要清图就返回 `true`（并把 `epoch_live` 对齐）。
pub fn take_reset() -> bool {
    let Ok(mut g) = CLOCK.lock() else { return false };
    let Some(c) = g.as_mut() else { return false };
    if c.epoch_live == c.epoch_req {
        return false;
    }
    match c.reset_since {
        Some(t) if t.elapsed().as_millis() >= RESET_SETTLE_MS => {
            c.reset_since = None;
            c.epoch_live = c.epoch_req;
            true
        }
        _ => false,
    }
}

/// 时间线订阅还该不该投递：时钟还在、目录没换、没有新的往回跳。
#[must_use]
pub fn feed_alive(dir: &std::path::Path, epoch: u64) -> bool {
    CLOCK.lock().is_ok_and(|g| g.as_ref().is_some_and(|c| c.dir == dir && c.epoch_req == epoch))
}

impl Clock {
    fn poll(&mut self, now: Instant) {
        if self.last_poll.is_some_and(|t| now.duration_since(t).as_millis() < 100) {
            return;
        }
        self.last_poll = Some(now);
        if self.tl.is_none()
            && let Ok(tl) = Timeline::open(&self.dir)
        {
            // 时段以时间线为准（选择器给的是「起始时刻 + 分钟」，回放按 ts_init 切）
            self.t0 = tl.meta.t0;
            self.t1 = tl.meta.t1;
            self.head = self.head.clamp(self.t0, self.t1);
            self.tl = Some(tl);
            self.load_bm();
        }
        if let Some(tl) = self.tl.as_mut() {
            tl.refresh_done();
            self.done = tl.meta.done;
            self.frontier = tl.frontier();
        }
    }

    /// 播放头能到的最远处。
    fn limit(&self) -> i64 {
        self.frontier.unwrap_or(self.t0).min(self.t1)
    }

    fn seek(&mut self, t: i64) {
        // 栅格是 100ms：播放头对齐到格上，同一刻前进与后退取到的是同一行
        let t = (t.clamp(self.t0, self.limit().max(self.t0)) / 100_000_000) * 100_000_000;
        // 往回跳要清图重灌；往前一下跳很远（打开分析回到 5 分钟、拖时间轴、跳到远处的事件）也一样——
        // 不然订阅要在一两帧里连发几千条消息，实测 K 线与 Footprint 的价格轴被压扁（只剩 0.3 美元高）
        if t < self.head || t - self.head > FAR_JUMP_NS {
            self.epoch_req += 1;
            self.reset_since = Some(Instant::now());
        }
        self.head = t;
    }

    fn counts(&self) -> Vec<(String, usize)> {
        self.tl.as_ref().map(Timeline::event_counts).unwrap_or_default()
    }

    /// 筛选里勾了的类。用户还没动过筛选就按缺省那几类——按名字，不看此刻出现过没有：
    /// 后台还在算的时候，有的类要过一阵才出现第一个（实测 RESP_ABSORPTION 因此一直没被勾上）。
    fn kinds(&mut self) -> std::collections::BTreeSet<String> {
        self.kinds.clone().unwrap_or_else(|| DEFAULT_KINDS.iter().map(|k| (*k).to_string()).collect())
    }

    /// 上一个 / 下一个事件：播放头落在事件前 [`EVENT_LEAD_NS`]（按事件自己的时刻与检出时刻取早的），暂停。
    fn jump_event(&mut self, forward: bool) {
        let kinds = self.kinds();
        let Some(tl) = self.tl.as_ref() else { return };
        let lim = self.limit();
        // 从哪找：刚跳过来、播放头没动 → 从聚焦的那个事件；否则从播放头（往前找的是播放头之后才检出的）
        let anchor = match &self.focus {
            Some(f) if self.head == self.focus_head => f.ti,
            // 跳过去时播放头放在事件前 2 秒：以「播放头 + 2 秒」为界，前后都不会落回原地
            _ => self.head + EVENT_LEAD_NS,
        };
        let pick = |e: &super::timeline::EventRec| kinds.contains(&e.kind);
        let found = if forward {
            tl.events(anchor + 1, lim + 1, &[]).into_iter().find(|e| pick(e))
        } else {
            tl.events(self.t0, anchor, &[]).into_iter().rev().find(|e| pick(e))
        };
        let Some(e) = found else { return };
        self.playing = false;
        self.seek(e.ts.min(e.ti) - EVENT_LEAD_NS);
        self.focus_head = self.head;
        self.focus = Some(Focus { kind: e.kind, ti: e.ti, ts: e.ts, dir: e.dir, px: e.px });
        self.marks_ver += 1;
    }

    /// 书签的键：`数据商|标的`，跟着某份画线分析时加 `#分析 id`。
    fn load_bm(&mut self) {
        let Some(tl) = self.tl.as_ref() else { return };
        let mut key = format!("{}|{}", tl.meta.vendor, tl.meta.symbol);
        if let Some(a) = &self.bm_scope {
            key.push('#');
            key.push_str(a);
        }
        if self.bm_key.as_deref() == Some(key.as_str()) {
            return;
        }
        self.bookmarks = load_bookmarks(&key);
        self.bm_key = Some(key);
        self.marks_ver += 1;
    }

    fn save_bm(&mut self) {
        if let Some(k) = &self.bm_key {
            save_bookmarks(k, &self.bookmarks);
        }
        self.marks_ver += 1;
    }

    /// 条件跳转：条件由不成立变成立的上一处 / 下一处。落点与事件一样在命中前 2 秒，命中那一刻当成聚焦（虚线）。
    fn jump_cond(&mut self, forward: bool) {
        let Some(tl) = self.tl.as_ref() else {
            self.cond_note = "时间线还没打开".into();
            return;
        };
        let cond = match super::replay_cond::Cond::parse(&self.cond_text, &tl.meta.columns) {
            Ok(c) => c,
            Err(e) => {
                self.cond_note = e.0;
                return;
            }
        };
        let anchor = match &self.focus {
            Some(f) if self.head == self.focus_head => f.ti,
            _ => self.head + EVENT_LEAD_NS,
        };
        let Some(g) = tl.find_edge(&cond, anchor, forward) else {
            self.cond_note = if forward { "往后（已算出的部分里）没有了" } else { "往前没有了" }.into();
            return;
        };
        self.cond_note.clear();
        self.playing = false;
        self.seek(g - EVENT_LEAD_NS);
        self.focus_head = self.head;
        self.focus = Some(Focus { kind: COND_KIND.into(), ti: g, ts: g, dir: 0, px: None });
        self.marks_ver += 1;
    }

    fn tick(&mut self, now: Instant) {
        self.poll(now);
        if let Some(h) = self.pending_head
            && self.tl.is_some()
            && (self.done || self.limit() >= h)
        {
            self.pending_head = None;
            self.playing = false;
            self.seek(h);
            // 跳到一个已经灌过的地方以后：往前走的话不清图，往回的 seek 自己会请求清图
        }
        let dt = self.last_tick.map_or(0, |t| now.duration_since(t).as_nanos() as i64);
        self.last_tick = Some(now);
        if !self.playing {
            return;
        }
        let lim = self.limit();
        self.head = match self.pace.rate() {
            Some(r) => (self.head + (dt as f64 * r) as i64).min(lim),
            None => lim,
        }
        .max(self.head);
        if self.done && self.head >= self.t1 {
            self.playing = false;
        }
    }

    fn handle(&mut self, m: Cmd) {
        match m {
            Cmd::TogglePlay => {
                self.playing = !self.playing;
                // 在终点按播放：从头再放
                if self.playing && self.done && self.head >= self.t1 {
                    self.seek(self.t0);
                }
            }
            Cmd::Step(n) => {
                self.playing = false;
                self.seek(self.head + n * self.step.ns());
            }
            Cmd::Secs(n) => {
                self.playing = false;
                self.seek(self.head + n * 1_000_000_000);
            }
            Cmd::Drag(s) => self.drag = Some(s),
            Cmd::Release => {
                if let Some(s) = self.drag.take() {
                    self.seek(self.t0 + (s * 1e9) as i64);
                }
            }
            Cmd::Seek(t) => {
                self.playing = false;
                self.seek(t);
            }
            Cmd::PrevEvent => self.jump_event(false),
            Cmd::NextEvent => self.jump_event(true),
            Cmd::EventMenu => self.menu = !self.menu,
            Cmd::ToggleKind(k) => {
                let cur = self.kinds();
                let set = self.kinds.get_or_insert(cur);
                if !set.remove(&k) {
                    set.insert(k);
                }
                self.marks_ver += 1;
            }
            Cmd::AllKinds(on) => {
                self.kinds = Some(if on { self.counts().into_iter().map(|(k, _)| k).collect() } else { Default::default() });
                self.marks_ver += 1;
            }
            Cmd::CondInput(t) => {
                self.cond_text = t;
                self.cond_note.clear();
            }
            Cmd::CondPrev => self.jump_cond(false),
            Cmd::CondNext => self.jump_cond(true),
            Cmd::AddBookmark => {
                if !self.bookmarks.iter().any(|b| b.t == self.head) {
                    self.bookmarks.push(Bookmark { t: self.head, note: String::new() });
                    self.bookmarks.sort_by_key(|b| b.t);
                    self.save_bm();
                }
                self.menu = true;
            }
            Cmd::BookmarkNote(i, n) => {
                if let Some(b) = self.bookmarks.get_mut(i) {
                    b.note = n;
                    self.save_bm();
                }
            }
            Cmd::BookmarkGo(i) => {
                if let Some(t) = self.bookmarks.get(i).map(|b| b.t) {
                    self.playing = false;
                    self.seek(t);
                }
            }
            Cmd::BookmarkDel(i) => {
                if i < self.bookmarks.len() {
                    self.bookmarks.remove(i);
                    self.save_bm();
                }
            }
            Cmd::CyclePace => self.pace = self.pace.next(),
            Cmd::CycleStep => self.step = self.step.next(),
        }
    }

    /// 播放头追上了还在算的 frontier。
    fn waiting(&self) -> bool {
        self.playing && !self.done && self.head >= self.limit()
    }
}

/// 主循环每帧调（`Message::Tick`）。
pub fn tick(now: Instant) {
    if let Ok(mut g) = CLOCK.lock()
        && let Some(c) = g.as_mut()
    {
        c.tick(now);
    }
}

pub fn handle(m: Cmd) {
    let head = {
        let Ok(mut g) = CLOCK.lock() else { return };
        let Some(c) = g.as_mut() else { return };
        c.handle(m);
        // 停下来看的时候（暂停 / 步进 / 拖动之后）热图从历史整体重画：一路播过来与往回跳之后重灌，画出来是同一张
        (!c.playing).then_some(c.head)
    };
    if let Some(h) = head {
        request_rebuild((h.max(0) / 1_000_000) as u64);
    }
}

/// 聚焦的事件（订单流层析按它选中同一个事件）与它的版本。
#[must_use]
pub fn focus() -> Option<(Focus, u64)> {
    let g = CLOCK.lock().ok()?;
    let c = g.as_ref()?;
    c.focus.clone().map(|f| (f, c.marks_ver))
}

/// 图上要标的事件：`[lo, hi]`（毫秒，按事件自己的时刻）里勾了的类，`(时刻 ms, 方向, 是不是聚焦的那个)`，至多 `cap` 个。
/// 只标播放头之前检出的（不偷看未来）；聚焦的那个即使在播放头之后也标（那是要去看的目标）。
#[must_use]
pub fn marks(lo_ms: u64, hi_ms: u64, cap: usize) -> Vec<(u64, i8, bool)> {
    if !charts_on() {
        return Vec::new();
    }
    let Ok(mut g) = CLOCK.lock() else { return Vec::new() };
    let Some(c) = g.as_mut() else { return Vec::new() };
    let kinds = c.kinds();
    let (head, focus) = (c.head, c.focus.clone());
    let Some(tl) = c.tl.as_ref() else { return Vec::new() };
    let mut out: Vec<(u64, i8, bool)> = tl
        .events(i64::MIN, head, &[])
        .into_iter()
        .filter(|e| kinds.contains(&e.kind))
        .map(|e| ((e.ts / 1_000_000) as u64, e.dir, false))
        .filter(|(t, _, _)| *t >= lo_ms && *t <= hi_ms)
        .collect();
    if out.len() > cap {
        out.drain(..out.len() - cap);
    }
    if let Some(f) = focus {
        let t = (f.ts / 1_000_000) as u64;
        out.retain(|m| m.0 != t);
        if t >= lo_ms && t <= hi_ms {
            out.push((t, f.dir, true));
        }
    }
    out
}

/// 图上要标的书签：`[lo, hi]`（毫秒）里的书签时刻（书签是人记的，不是数据，播放头之后的也标）。
#[must_use]
pub fn bookmark_marks(lo_ms: u64, hi_ms: u64) -> Vec<u64> {
    if !charts_on() {
        return Vec::new();
    }
    let Ok(g) = CLOCK.lock() else { return Vec::new() };
    g.as_ref().map_or_else(Vec::new, |c| {
        c.bookmarks.iter().map(|b| (b.t.max(0) / 1_000_000) as u64).filter(|t| *t >= lo_ms && *t <= hi_ms).collect()
    })
}

/// 事件标记的版本（聚焦 / 筛选变了就变）：图按它判断要不要重画。
#[must_use]
pub fn marks_ver() -> u64 {
    CLOCK.lock().ok().and_then(|g| g.as_ref().map(|c| c.marks_ver)).unwrap_or(0)
}

/// 事件筛选菜单开着吗。
#[must_use]
pub fn event_menu_open() -> bool {
    CLOCK.lock().is_ok_and(|g| g.as_ref().is_some_and(|c| c.menu))
}

/// 回放的标的（时间线抬头里的）。
#[must_use]
pub fn symbol() -> Option<String> {
    let g = CLOCK.lock().ok()?;
    g.as_ref()?.tl.as_ref().map(|t| t.meta.symbol.clone())
}

/// 回放状态（画线分析记下它，docs/52 A2）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct State {
    /// 播放头（纳秒）
    pub head: i64,
    pub pace: String,
    pub step: String,
    /// 事件筛选（`None` = 缺省那几类）
    #[serde(default)]
    pub kinds: Option<Vec<String>>,
    #[serde(default)]
    pub cond: String,
}

fn pace_key(p: Pace) -> &'static str {
    match p {
        Pace::X1 => "1",
        Pace::X10 => "10",
        Pace::X60 => "60",
        Pace::Max => "max",
    }
}

fn step_key(s: Step) -> &'static str {
    match s {
        Step::Ms100 => "100ms",
        Step::S1 => "1s",
        Step::Bar => "bar",
    }
}

/// 现在的回放状态（没在回放 = `None`）。
#[must_use]
pub fn snapshot() -> Option<State> {
    let g = CLOCK.lock().ok()?;
    let c = g.as_ref()?;
    Some(State {
        head: c.pending_head.unwrap_or(c.head),
        pace: pace_key(c.pace).into(),
        step: step_key(c.step).into(),
        kinds: c.kinds.as_ref().map(|k| k.iter().cloned().collect()),
        cond: c.cond_text.clone(),
    })
}

/// 回到记下的状态：播放头（还没算到就等）、倍速、步长、筛选、条件。暂停着。
pub fn restore(st: &State) {
    if let Ok(mut g) = CLOCK.lock()
        && let Some(c) = g.as_mut()
    {
        c.pace = Pace::ALL.into_iter().find(|p| pace_key(*p) == st.pace).unwrap_or(c.pace);
        c.step = [Step::Ms100, Step::S1, Step::Bar].into_iter().find(|s| step_key(*s) == st.step).unwrap_or(c.step);
        c.kinds = st.kinds.as_ref().map(|k| k.iter().cloned().collect());
        c.cond_text = st.cond.clone();
        c.playing = false;
        c.pending_head = Some(st.head);
        c.focus = None;
        c.marks_ver += 1;
    }
}

/// 书签跟着哪份画线分析（`None` = 默认画板）；变了就换一组书签。
pub fn set_scope(scope: Option<String>) {
    if let Ok(mut g) = CLOCK.lock()
        && let Some(c) = g.as_mut()
        && c.bm_scope != scope
    {
        c.bm_scope = scope;
        c.load_bm();
    }
}

/// 回放稳住了：没有等着回去的播放头、没有等着的清图（打开分析后，图灌好了才把视野移回去）。
#[must_use]
pub fn settled() -> bool {
    CLOCK.lock().is_ok_and(|g| g.as_ref().is_some_and(|c| c.pending_head.is_none() && c.epoch_live == c.epoch_req && c.tl.is_some()))
}

/// 视野同步的最后一份（画线分析记下视野中心用）。
#[must_use]
pub fn last_view() -> Option<ViewSync> {
    VIEW.lock().ok().and_then(|g| g.2)
}

/// 设置当前倍速（数据源那一栏选的）。
pub fn set_pace(p: Pace) {
    if let Ok(mut g) = CLOCK.lock()
        && let Some(c) = g.as_mut()
    {
        c.pace = p;
    }
}

/// 给视图 / 链路徽标的只读副本。
#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub t0: i64,
    pub t1: i64,
    pub head: i64,
    pub frontier: Option<i64>,
    pub done: bool,
    pub playing: bool,
    pub waiting: bool,
    pub pace: Pace,
    pub step: Step,
    pub drag: Option<f64>,
    pub opened: bool,
    pub focus: Option<Focus>,
    /// 勾了几类 / 一共几类
    pub kinds_on: usize,
    pub menu: bool,
}

#[must_use]
pub fn view() -> Option<View> {
    let g = CLOCK.lock().ok()?;
    let c = g.as_ref()?;
    Some(View {
        t0: c.t0,
        t1: c.t1,
        head: c.head,
        frontier: c.frontier,
        done: c.done,
        playing: c.playing,
        waiting: c.waiting(),
        pace: c.pace,
        step: c.step,
        drag: c.drag,
        opened: c.tl.is_some(),
        focus: c.focus.clone(),
        kinds_on: c.kinds.as_ref().map_or(DEFAULT_KINDS.len(), std::collections::BTreeSet::len),
        menu: c.menu,
    })
}

/// 时刻的读法（UTC，到 0.1 秒）。
#[must_use]
pub fn fmt_t(ns: i64) -> String {
    // chrono 只认 %.3f / %.6f / %.9f，十分位自己补
    let tenth = ns.rem_euclid(1_000_000_000) / 100_000_000;
    format!("{}.{tenth}", chrono::DateTime::from_timestamp_nanos(ns).format("%H:%M:%S"))
}

impl View {
    /// 已经算出来的比例（0–1）。
    #[must_use]
    pub fn computed(&self) -> f64 {
        if self.done {
            return 1.0;
        }
        let span = (self.t1 - self.t0).max(1) as f64;
        self.frontier.map_or(0.0, |f| ((f - self.t0) as f64 / span).clamp(0.0, 1.0))
    }

    /// 状态一行。
    #[must_use]
    pub fn status(&self) -> String {
        let state = if self.frontier.is_none() {
            // 时间线还没打开 / 还没算出第一行
            "准备中（回放在读数据）".to_string()
        } else if self.waiting {
            format!("◌ 等计算（已算到 {:.0}%）", self.computed() * 100.0)
        } else if self.playing {
            format!("▶ {}", self.pace.short())
        } else {
            "⏸ 已暂停".into()
        };
        let calc = if self.done { "已算完".to_string() } else { format!("已算 {:.0}%", self.computed() * 100.0) };
        format!("{state} · {} / {} · {calc}", fmt_t(self.head), fmt_t(self.t1))
    }
}

/// 快捷键：空格暂停 / 继续；← → 一个步长；Shift + ← → 10 秒；`[` `]` 上一个 / 下一个事件；
/// `{` `}`（Shift + `[` `]`）条件的上一处 / 下一处；`b` 在播放头加书签。只在特征回放开着、且看的是「订单流特征」时有效。
#[must_use]
pub fn key_cmd(key: &iced::keyboard::Key, shift: bool) -> Option<Cmd> {
    use iced::keyboard::Key;
    use iced::keyboard::key::Named;
    if !active() || !super::workspace::feature_feed() {
        return None;
    }
    match key.as_ref() {
        Key::Named(Named::Space) => Some(Cmd::TogglePlay),
        Key::Named(Named::ArrowLeft) => Some(if shift { Cmd::Secs(-10) } else { Cmd::Step(-1) }),
        Key::Named(Named::ArrowRight) => Some(if shift { Cmd::Secs(10) } else { Cmd::Step(1) }),
        Key::Character("{") => Some(Cmd::CondPrev),
        Key::Character("}") => Some(Cmd::CondNext),
        Key::Character("[") if shift => Some(Cmd::CondPrev),
        Key::Character("]") if shift => Some(Cmd::CondNext),
        Key::Character("[") => Some(Cmd::PrevEvent),
        Key::Character("]") => Some(Cmd::NextEvent),
        Key::Character("b" | "B") => Some(Cmd::AddBookmark),
        _ => None,
    }
}

/// 顶部工具栏里的播放控件（docs/42：工具栏 = 看什么）。
#[must_use]
pub fn toolbar<'a>() -> Option<Element<'a, Cmd>> {
    use crate::ui::shell::tool_pick;
    use crate::style::Icon;
    use crate::ui::widgets::{Kind, btn, ibtn_tip};
    let v = view()?;
    let span = ((v.t1 - v.t0).max(1) as f64) / 1e9;
    let pos = v.drag.unwrap_or(((v.head - v.t0) as f64 / 1e9).clamp(0.0, span));
    let r = row![
        btn("◀", Kind::Ghost, Some(Cmd::Step(-1))),
        ibtn_tip(if v.playing { Icon::UiPause } else { Icon::UiPlay }, "", Kind::Standard, Some(Cmd::TogglePlay), if v.playing { "暂停（空格）" } else { "播放（空格）" }),
        btn("▶︎|", Kind::Ghost, Some(Cmd::Step(1))),
        ibtn_tip(Icon::UiPrev, "", Kind::Ghost, Some(Cmd::PrevEvent), "上一个事件（[）"),
        ibtn_tip(Icon::UiNext, "", Kind::Ghost, Some(Cmd::NextEvent), "下一个事件（]）"),
        tool_pick(
            "跳转",
            match &v.focus {
                Some(f) => format!("{} {} · {}{}", f.kind, dir_glyph(f.dir), fmt_t(f.ts), if v.menu { " ▴" } else { " ▾" }),
                None => format!("{} 类{}", v.kinds_on, if v.menu { " ▴" } else { " ▾" }),
            },
            Some(Cmd::EventMenu),
            v.menu,
        ),
        tool_pick("步长", v.step.label().into(), Some(Cmd::CycleStep), false),
        tool_pick("倍速", v.pace.short().into(), Some(Cmd::CyclePace), false),
        slider(0.0..=span, pos, Cmd::Drag).on_release(Cmd::Release).step(0.1).width(iced::Length::Fixed(220.0)),
        crate::ui::text::metadata(v.status()).color(if v.waiting { crate::ui::pal::warn() } else { crate::ui::pal::dim() }),
    ]
    .spacing(crate::ui::metrics::space(1))
    .align_y(Alignment::Center);
    Some(r.into())
}

fn dir_glyph(d: i8) -> &'static str {
    match d {
        1 => "↑",
        -1 => "↓",
        _ => "·",
    }
}

/// 「跳转 ▾」卡片（盖在内容上方，与「⋯ 页面」同一种做法）：事件筛选、条件跳转、书签与笔记。
#[must_use]
pub fn event_menu<'a>() -> Element<'a, Cmd> {
    use crate::ui::widgets::{Kind, btn};
    use iced::widget::{column, text_input};
    let (counts, on, cond_text, cond_note, columns, bookmarks, head) = {
        let Ok(mut g) = CLOCK.lock() else { return row![].into() };
        let Some(c) = g.as_mut() else { return row![].into() };
        let cols = c.tl.as_ref().map(|t| t.meta.columns.clone()).unwrap_or_default();
        (c.counts(), c.kinds(), c.cond_text.clone(), c.cond_note.clone(), cols, c.bookmarks.clone(), c.head)
    };
    let sp = crate::ui::metrics::space;
    // ① 事件
    let mut chips = row![].spacing(sp(1));
    for (k, n) in counts {
        let label = format!("{} {k} {n}", if on.contains(&k) { "☑" } else { "☐" });
        chips = chips.push(btn(label, if on.contains(&k) { Kind::Standard } else { Kind::Ghost }, Some(Cmd::ToggleKind(k))));
    }
    // ② 条件
    let hints = super::replay_cond::suggest(super::replay_cond::last_ident(&cond_text), &columns, 8);
    let mut cond_row = row![
        crate::ui::text::label("条件"),
        text_input("例如 obi_l5@0 > 0.3 and spread_bps@0 < 1", &cond_text).on_input(Cmd::CondInput).on_submit(Cmd::CondNext).width(iced::Length::Fixed(520.0)),
        crate::ui::widgets::ibtn(crate::style::Icon::UiPrev, "上一处", Kind::Ghost, Some(Cmd::CondPrev)),
        crate::ui::widgets::ibtn(crate::style::Icon::UiNext, "下一处", Kind::Standard, Some(Cmd::CondNext)),
    ]
    .spacing(sp(2))
    .align_y(Alignment::Center);
    if !cond_note.is_empty() {
        cond_row = cond_row.push(crate::ui::text::metadata(cond_note).color(crate::ui::pal::warn()));
    }
    let hint_text = if hints.is_empty() {
        "列名写成 key@窗口毫秒（只写 key 取最小窗口），可用 and / or / not、括号、< <= > >= == !=；质量不够的格不算成立。回车 = 下一处，快捷键 { }".to_string()
    } else {
        format!("可选的列：{}", hints.join("  "))
    };
    // ③ 书签
    let mut bm = column![row![
        crate::ui::text::label("书签"),
        btn("＋ 在播放头加书签（b）", Kind::Ghost, Some(Cmd::AddBookmark)),
        crate::ui::text::metadata("按数据商 + 标的存盘，下次回放同一段还在；也标在 K 线、Footprint、热图底边").color(crate::ui::pal::dim()),
    ]
    .spacing(sp(2))
    .align_y(Alignment::Center)]
    .spacing(sp(1));
    for (i, b) in bookmarks.iter().enumerate() {
        let here = b.t == head;
        bm = bm.push(
            row![
                btn(format!("{}{}", if here { "▶ " } else { "" }, fmt_t(b.t)), if here { Kind::Standard } else { Kind::Ghost }, Some(Cmd::BookmarkGo(i))),
                text_input("笔记…", &b.note).on_input(move |n| Cmd::BookmarkNote(i, n)).width(iced::Length::Fixed(520.0)),
                btn("删除", Kind::Ghost, Some(Cmd::BookmarkDel(i))),
            ]
            .spacing(sp(2))
            .align_y(Alignment::Center),
        );
    }
    column![
        row![
            crate::ui::text::label("事件：勾了的类参与「上一个 / 下一个事件」（快捷键 [ ]），也标在 K 线、Footprint 与热图上；播放头落在事件前 2 秒"),
            btn("全选", Kind::Ghost, Some(Cmd::AllKinds(true))),
            btn("全不选", Kind::Ghost, Some(Cmd::AllKinds(false))),
        ]
        .spacing(sp(2))
        .align_y(Alignment::Center),
        chips.wrap(),
        cond_row,
        crate::ui::text::metadata(hint_text).color(crate::ui::pal::dim()),
        bm,
    ]
    .spacing(sp(2))
    .padding(sp(3))
    .into()
}

/// 测试用：直接摆一个时钟（不读文件）。
#[cfg(test)]
fn with_clock(dir: &std::path::Path, t0: i64, t1: i64, frontier: i64, done: bool) {
    begin(dir.to_path_buf(), t0, t1, Pace::X10);
    let mut g = CLOCK.lock().unwrap();
    let c = g.as_mut().unwrap();
    c.frontier = Some(frontier);
    c.done = done;
    c.playing = false;
    // 别去读盘
    c.last_poll = Some(Instant::now() + std::time::Duration::from_secs(3600));
}

#[cfg(test)]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static L: Mutex<()> = Mutex::new(());
    L.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: i64 = 1_000_000_000;

    #[test]
    fn 播放不越过frontier_后退才请求清图() {
        let _l = test_lock();
        let d = std::env::temp_dir();
        with_clock(&d, 0, 600 * S, 30 * S, false);
        handle(Cmd::TogglePlay);
        let t = Instant::now();
        tick(t);
        tick(t + std::time::Duration::from_secs(10)); // 10× × 10 秒 = 100 秒，但只算到 30 秒
        let v = view().unwrap();
        assert_eq!(v.head, 30 * S);
        assert!(v.waiting && v.status().contains("等计算"));
        assert!(!take_reset());
        handle(Cmd::Step(1)); // 步进会暂停；已经在 frontier，不动
        assert_eq!(view().unwrap().head, 30 * S);
        assert!(!view().unwrap().playing);
        handle(Cmd::Secs(-10));
        assert_eq!(view().unwrap().head, 20 * S);
        assert!(!take_reset(), "等在途的旧数据处理完再清");
        assert!(!feed_alive(&d, 0), "旧订阅马上停止投递");
        std::thread::sleep(std::time::Duration::from_millis(RESET_SETTLE_MS as u64 + 5));
        assert!(take_reset(), "往回要清图");
        assert!(!take_reset(), "只清一次");
        handle(Cmd::Step(1));
        assert_eq!(view().unwrap().head, 21 * S);
        assert!(!take_reset(), "往前不清图");
        CLOCK.lock().unwrap().as_mut().unwrap().frontier = Some(600 * S);
        handle(Cmd::Secs(10));
        assert!(!take_reset(), "往前 10 秒不清图");
        handle(Cmd::Seek(100 * S));
        std::thread::sleep(std::time::Duration::from_millis(RESET_SETTLE_MS as u64 + 5));
        assert!(take_reset(), "往前一下跳很远也清图重灌");
        end();
    }

    #[test]
    fn 视野同步只在回放看图时有_自己发的自己不跟() {
        let _l = test_lock();
        let _w = super::super::workspace::replay_mode_test_lock();
        let d = std::env::temp_dir();
        super::super::workspace::set_feature_feed(false);
        with_clock(&d, 0, 600 * S, 600 * S, true);
        publish_view(1, ViewSync::Center(5_000));
        assert!(view_sync(0).is_none(), "不在「订单流特征」里：不联动");
        super::super::workspace::set_feature_feed(true);
        publish_view(1, ViewSync::Center(5_000));
        let (v, src, x) = view_sync(0).unwrap();
        assert_eq!((src, x), (1, ViewSync::Center(5_000)));
        assert!(view_sync(v).is_none(), "看过的不再给");
        publish_view(1, ViewSync::Center(5_000));
        assert!(view_sync(v).is_none(), "同样的内容不算新的一版");
        publish_view(2, ViewSync::Live);
        assert_eq!(view_sync(v).map(|x| (x.1, x.2)), Some((2, ViewSync::Live)));
        seek_to_ms(42_123);
        assert_eq!(view().unwrap().head, 42_100_000_000, "单击跳转");
        end();
        assert!(view_sync(0).is_none(), "回放结束，同步作废");
        super::super::workspace::set_feature_feed(false);
    }

    /// 一份只有事件的时间线（算完了：frontier = 终点）。
    fn tl_with_events(tag: &str, events: &[(&str, i64, i64, i8)]) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ws_rc_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let meta = serde_json::json!({"symbol": "X", "t0": 0, "t1": 60 * S, "columns": [], "depth_levels": 2, "done": true,
            "layout": {"trade": {"rec": 40}, "depth": {"rec": 88}, "grid": {"rec": 24}}});
        std::fs::write(d.join("tl_meta.json"), meta.to_string()).unwrap();
        for n in ["tl_grid.bin", "tl_trades.bin", "tl_depth.bin"] {
            std::fs::write(d.join(n), b"").unwrap();
        }
        let lines: String = events
            .iter()
            .map(|(k, ti, ts, dir)| format!("{}\n", serde_json::json!({"ti": ti, "ts": ts, "kind": k, "dir": dir})))
            .collect();
        std::fs::write(d.join("tl_events.jsonl"), lines).unwrap();
        d
    }

    #[test]
    fn 上一个下一个事件_播放头落在事件前两秒_只走勾了的类() {
        let _l = test_lock();
        let _w = super::super::workspace::replay_mode_test_lock();
        super::super::workspace::set_feature_feed(true);
        let d = tl_with_events(
            "ev",
            &[("SWEEP", 10 * S, 9 * S + S / 2, 1), ("REPLENISH", 12 * S, 12 * S, 0), ("SWEEP", 20 * S, 20 * S, -1), ("RESP_ABSORPTION", 30 * S, 27 * S, 0)],
        );
        begin(d.clone(), 0, 60 * S, Pace::X10);
        tick(Instant::now()); // 打开时间线
        handle(Cmd::TogglePlay);
        handle(Cmd::NextEvent);
        let v = view().unwrap();
        assert_eq!(v.head, 7_500_000_000, "SWEEP 事件时刻 9.5 秒 − 2 秒");
        assert!(!v.playing, "跳到事件会暂停");
        assert_eq!(v.focus.as_ref().map(|f| f.kind.as_str()), Some("SWEEP"));
        handle(Cmd::NextEvent);
        assert_eq!(view().unwrap().head, 18 * S, "REPLENISH 缺省不勾，跳过");
        handle(Cmd::NextEvent);
        assert_eq!(view().unwrap().head, 25 * S, "确认窗口：事件时刻 27 秒比检出 30 秒早，按早的");
        handle(Cmd::NextEvent);
        assert_eq!(view().unwrap().head, 25 * S, "后面没有了：不动");
        handle(Cmd::PrevEvent);
        assert_eq!(view().unwrap().head, 18 * S);
        // 图上的标记：播放头 18 秒之前检出的 + 聚焦的那个（20 秒，在播放头之后也标）
        assert_eq!(marks(0, 60_000, 100), vec![(9_500, 1, false), (20_000, -1, true)]);
        handle(Cmd::ToggleKind("REPLENISH".into()));
        handle(Cmd::Step(-1)); // 动过播放头：从播放头找
        handle(Cmd::PrevEvent);
        assert_eq!(view().unwrap().head, 10 * S, "勾上 REPLENISH 后：播放头 17 秒往回，最近的是 12 秒检出的 REPLENISH");
        end();
        super::super::workspace::set_feature_feed(false);
        let _ = std::fs::remove_dir_all(d);
    }

    /// 一份一列（`obi_l5@0`）、每秒一行的时间线。
    fn tl_with_grid(tag: &str, vals: &[f64]) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ws_rc_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let rec = super::super::timeline::grid_rec(1) as usize;
        let meta = serde_json::json!({"symbol": "BTCUSDT", "vendor": "tardis", "t0": 0, "t1": 60 * S, "columns": ["obi_l5@0"], "depth_levels": 2, "done": true,
            "layout": {"trade": {"rec": 40}, "depth": {"rec": 88}, "grid": {"rec": rec}}});
        std::fs::write(d.join("tl_meta.json"), meta.to_string()).unwrap();
        let mut g = Vec::new();
        for (i, v) in vals.iter().enumerate() {
            let mut r = vec![0u8; rec];
            r[0..8].copy_from_slice(&((i as i64 + 1) * S).to_le_bytes());
            r[24..32].copy_from_slice(&v.to_le_bytes());
            g.extend(r);
        }
        std::fs::write(d.join("tl_grid.bin"), g).unwrap();
        for n in ["tl_trades.bin", "tl_depth.bin", "tl_events.jsonl"] {
            std::fs::write(d.join(n), b"").unwrap();
        }
        d
    }

    #[test]
    fn 条件跳转与书签() {
        let _l = test_lock();
        let _w = super::super::workspace::replay_mode_test_lock();
        super::super::workspace::set_feature_feed(true);
        let bm = std::env::temp_dir().join(format!("ws_rc_bm_{}.json", std::process::id()));
        let _ = std::fs::remove_file(&bm);
        // SAFETY: 测试串行（test_lock）；只改这个测试自己用的变量
        unsafe { std::env::set_var("WS_REPLAY_BOOKMARKS", &bm) };
        //                       1s   2s   3s   4s   5s   6s   7s   8s   9s   10s
        let d = tl_with_grid("cond", &[0.0, 0.1, 0.5, 0.6, 0.1, 0.1, 0.1, 0.4, 0.4, 0.0]);
        begin(d.clone(), 0, 60 * S, Pace::X10);
        tick(Instant::now());
        handle(Cmd::TogglePlay);
        handle(Cmd::CondInput("obi_l5 > 0.3".into()));
        handle(Cmd::CondNext);
        let v = view().unwrap();
        assert_eq!(v.head, S, "3 秒那一格变成立，落在前 2 秒");
        assert_eq!(v.focus.as_ref().map(|f| (f.kind.as_str(), f.ts)), Some((COND_KIND, 3 * S)));
        handle(Cmd::CondNext);
        assert_eq!(view().unwrap().head, 6 * S, "4 秒仍成立不算，下一次变成立是 8 秒");
        handle(Cmd::CondNext);
        assert_eq!(view().unwrap().head, 6 * S, "后面没有了：不动");
        handle(Cmd::CondPrev);
        assert_eq!(view().unwrap().head, S);
        handle(Cmd::CondInput("obi_l9 > 0".into()));
        handle(Cmd::CondNext);
        assert!(CLOCK.lock().unwrap().as_ref().unwrap().cond_note.contains("没有"), "写错了有说明");
        // 书签：加、写笔记、存盘；重新开一次回放还在
        handle(Cmd::AddBookmark);
        handle(Cmd::BookmarkNote(0, "吸收之后反弹".into()));
        assert_eq!(bookmark_marks(0, 60_000), vec![1_000]);
        end();
        begin(d.clone(), 0, 60 * S, Pace::X10);
        tick(Instant::now());
        handle(Cmd::Secs(5));
        handle(Cmd::BookmarkGo(0));
        assert_eq!(view().unwrap().head, S, "跳回书签");
        let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&bm).unwrap()).unwrap();
        assert_eq!(saved["tardis|BTCUSDT"][0]["note"], "吸收之后反弹");
        handle(Cmd::BookmarkDel(0));
        assert!(bookmark_marks(0, 60_000).is_empty());
        end();
        super::super::workspace::set_feature_feed(false);
        let _ = std::fs::remove_dir_all(d);
        let _ = std::fs::remove_file(bm);
    }

    #[test]
    fn 记下与回到回放状态_书签跟着分析() {
        let _l = test_lock();
        let _w = super::super::workspace::replay_mode_test_lock();
        super::super::workspace::set_feature_feed(true);
        let bm = std::env::temp_dir().join(format!("ws_rc_bm2_{}.json", std::process::id()));
        let _ = std::fs::remove_file(&bm);
        // SAFETY: 测试串行（test_lock）
        unsafe { std::env::set_var("WS_REPLAY_BOOKMARKS", &bm) };
        let d = tl_with_grid("restore", &[0.0; 10]);
        begin(d.clone(), 0, 60 * S, Pace::X10);
        tick(Instant::now());
        let st = State { head: 5 * S, pace: "60".into(), step: "bar".into(), kinds: Some(vec!["SWEEP".into()]), cond: "obi_l5 > 0".into() };
        restore(&st);
        assert_eq!(snapshot().unwrap().head, 5 * S, "还没跳过去时记下的是要回去的那一刻");
        tick(Instant::now());
        assert!(settled());
        let v = view().unwrap();
        assert_eq!((v.head, v.pace, v.step, v.playing), (5 * S, Pace::X60, Step::Bar, false));
        assert_eq!(snapshot().unwrap(), st);
        // 书签：默认画板一组、分析一组
        handle(Cmd::AddBookmark);
        assert_eq!(bookmark_marks(0, 60_000), vec![5_000]);
        set_scope(Some("a1".into()));
        assert!(bookmark_marks(0, 60_000).is_empty(), "换到分析：它自己的一组");
        handle(Cmd::Secs(-2));
        handle(Cmd::AddBookmark);
        assert_eq!(bookmark_marks(0, 60_000), vec![3_000]);
        set_scope(None);
        assert_eq!(bookmark_marks(0, 60_000), vec![5_000]);
        let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&bm).unwrap()).unwrap();
        assert!(saved.get("|BTCUSDT").is_none() && saved.get("tardis|BTCUSDT").is_some() && saved.get("tardis|BTCUSDT#a1").is_some());
        end();
        super::super::workspace::set_feature_feed(false);
        let _ = std::fs::remove_dir_all(d);
        let _ = std::fs::remove_file(bm);
    }

    #[test]
    fn 时刻到十分之一秒() {
        assert_eq!(fmt_t(1_780_272_001_234_000_000), "00:00:01.2");
        assert_eq!(fmt_t(0), "00:00:00.0");
    }

    #[test]
    fn 拖时间轴松手才跳_播放头对齐到100ms() {
        let _l = test_lock();
        let d = std::env::temp_dir();
        with_clock(&d, 1_000 * S, 1_600 * S, 1_600 * S, true);
        handle(Cmd::Drag(123.456));
        assert_eq!(view().unwrap().head, 1_000 * S, "拖动中不跳");
        assert_eq!(view().unwrap().drag, Some(123.456));
        handle(Cmd::Release);
        assert_eq!(view().unwrap().head, 1_123_400_000_000);
        handle(Cmd::Drag(9_999.0));
        handle(Cmd::Release);
        assert_eq!(view().unwrap().head, 1_600 * S, "截到终点");
        handle(Cmd::Seek(1_300 * S + 55_000_000));
        assert_eq!(view().unwrap().head, 1_300 * S, "单击跳转也对齐到 100ms");
        handle(Cmd::Seek(1_600 * S));
        handle(Cmd::CycleStep);
        handle(Cmd::Step(-1));
        assert_eq!(view().unwrap().head, 1_540 * S, "1 根 K 线 = 60 秒");
        // 在终点按播放：从头再放
        handle(Cmd::Drag(600.0));
        handle(Cmd::Release);
        handle(Cmd::TogglePlay);
        assert_eq!(view().unwrap().head, 1_000 * S);
        end();
        assert!(target().is_none());
    }
}
