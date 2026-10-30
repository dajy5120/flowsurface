//! 消息中心（docs/65）的 Cockpit 端：顶栏 🔔 的数、消息中心浮窗的状态与动作、系统消息的写入。
//!
//! 库与查询在 `wealthspring-inbox`（与 ws-news 共用）。**渲染线程不查库**：后台线程 `ws-inbox` 每秒看一眼最大 id
//! （游标），有新的、或界面改了筛选 / 做了操作才重查，结果放进快照；界面每帧只读快照。
//! 用户的操作（已读、确认、星标、归档、导出）排进队列交给同一个线程做。

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use wealthspring_inbox as ib;

/// 时间段筛选。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Range {
    Day,
    #[default]
    Week,
    Month,
    All,
}

impl Range {
    pub const ALL: [Range; 4] = [Range::Day, Range::Week, Range::Month, Range::All];
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Range::Day => "24 小时",
            Range::Week => "7 天",
            Range::Month => "30 天",
            Range::All => "全部",
        }
    }
    fn since(self, now_ms: i64) -> Option<i64> {
        let d = 86_400_000;
        match self {
            Range::Day => Some(now_ms - d),
            Range::Week => Some(now_ms - 7 * d),
            Range::Month => Some(now_ms - 30 * d),
            Range::All => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum InboxMsg {
    /// 顶栏 🔔：开 / 关消息中心
    Toggle,
    Cat(ib::Cat),
    Text(String),
    Symbol(String),
    Range(Range),
    /// 去掉「只看这条规则」
    ClearRule,
    /// 点一条：看详情（顺带标已读）
    Select(i64),
    Check(i64, bool),
    CheckNone,
    /// 勾上当前列表里的全部
    CheckShown,
    /// 下面五个作用在勾选的上（没勾就是当前选中的那条）
    Read,
    Unread,
    Ack,
    Archive,
    Unarchive,
    Star(i64, bool),
    AllRead,
    /// 展开 / 收起折叠的一组（按组里第一条的 id）
    Expand(i64),
    OpenUrl(String),
    /// 本页的 K 线图跳到这个时刻（毫秒）
    Goto(i64),
    /// 只看这条规则触发的
    RuleHistory(String),
    Export,
    ToggleCats,
    /// 打开提醒管理器
    Rules,
    /// 提醒声音设置页 ⇄ 列表（docs/68）
    SoundPage,
    Sound(super::alert_sound::SoundMsg),
}

/// 浮窗的界面状态（筛选、选中、勾选）。
#[derive(Debug, Clone, Default)]
pub struct Ui {
    pub open: bool,
    pub cat: ib::Cat,
    pub text: String,
    pub symbol: String,
    pub range: Range,
    pub rule_ref: Option<String>,
    pub sel: Option<i64>,
    pub checked: BTreeSet<i64>,
    pub expanded: HashSet<i64>,
    pub cats_hidden: bool,
    /// 显示的是提醒声音设置（docs/68）
    pub sound_page: bool,
    pub note: String,
    /// 筛选改了就加一：后台据此重查
    rev: u64,
}

impl Ui {
    fn filter(&self, now_ms: i64) -> ib::Filter {
        ib::Filter {
            cat: self.cat,
            text: self.text.clone(),
            symbol: self.symbol.clone(),
            since_ms: self.range.since(now_ms),
            rule_ref: self.rule_ref.clone(),
            limit: 500,
        }
    }
}

fn ui_lock() -> std::sync::MutexGuard<'static, Ui> {
    static U: OnceLock<Mutex<Ui>> = OnceLock::new();
    U.get_or_init(Default::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[must_use]
pub fn ui() -> Ui {
    ui_lock().clone()
}

#[must_use]
pub fn is_open() -> bool {
    ui_lock().open
}

/// 一路投递：（渠道, 状态, 错误）。
pub type Delivery = (String, String, Option<String>);

/// 后台算好的一份。
#[derive(Debug, Clone, Default)]
pub struct Snap {
    pub badge: ib::Badge,
    pub list: Vec<ib::Msg>,
    pub groups: Vec<ib::Group>,
    /// 选中那条 + 它的各渠道投递
    pub detail: Option<(ib::Msg, Vec<Delivery>)>,
    /// 规则 → （累计触发, 最近一次）
    pub stats: HashMap<String, (i64, i64)>,
    pub err: String,
    /// 后台在跑（没跑 = 样张 / 测试）
    pub live: bool,
}

fn snap_cell() -> &'static Mutex<Arc<Snap>> {
    static S: OnceLock<Mutex<Arc<Snap>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

#[must_use]
pub fn snapshot() -> Arc<Snap> {
    snap_cell().lock().map(|g| g.clone()).unwrap_or_default()
}

// ── 交给后台线程的事 ──────────────────────────────────────────────────

enum Op {
    Read(Vec<i64>),
    Unread(Vec<i64>),
    Ack(Vec<i64>),
    Archive(Vec<i64>),
    Unarchive(Vec<i64>),
    Star(i64, bool),
    AllRead,
    Post(Box<ib::Post>),
    Export(ib::Filter),
}

struct Queue {
    ops: Mutex<Vec<Op>>,
    wake: Condvar,
}

fn queue() -> &'static Queue {
    static Q: OnceLock<Queue> = OnceLock::new();
    Q.get_or_init(|| Queue { ops: Mutex::new(Vec::new()), wake: Condvar::new() })
}

fn push(op: Op) {
    let q = queue();
    let mut g = q.ops.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    // 后台没起（样张）时别无限攒
    if g.len() < 2000 {
        g.push(op);
    }
    q.wake.notify_one();
}

fn wake() {
    queue().wake.notify_one();
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// 写一条系统消息（弹出提示、行情断流、数据目录、自动暂停……）。测试里不写。
pub fn post_system(level: u8, title: impl Into<String>, body: impl Into<String>) {
    if cfg!(test) {
        return;
    }
    let now = now_ms();
    let title: String = title.into();
    let body: String = body.into();
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    push(Op::Post(Box::new(ib::Post {
        kind: ib::SYSTEM.into(),
        level,
        occurred_at: now,
        received_at: now,
        source: "Cockpit".into(),
        dedup_key: format!("sys:{}:{now}:{seq}", std::process::id()),
        title,
        body,
        ..Default::default()
    })));
}

/// 弹出提示入库（docs/65：底部「通知」页并进来）。错误 P2（计未读），其余 P3。
pub fn post_toast(title: &str, body: &str, danger: bool) {
    let t = if title.is_empty() { "提示" } else { title };
    post_system(if danger { 2 } else { 3 }, t.to_string(), body.to_string());
}

/// 环境里会断的东西（行情流、数据目录）：持续 10 秒的断开记一条（P2），恢复记一条（P3）。每帧调，自己判变化。
pub fn note_env(streams_down: &[(String, String)], data_dirs: Option<&(String, String)>) {
    struct Seen {
        since: Instant,
        why: String,
        posted: bool,
    }
    static S: OnceLock<Mutex<HashMap<String, Seen>>> = OnceLock::new();
    let mut g = S.get_or_init(Default::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut now_down: Vec<(String, String)> = streams_down.iter().map(|(k, w)| (format!("行情 {k}"), w.clone())).collect();
    if let Some((label, tip)) = data_dirs {
        now_down.push((format!("数据目录 {label}"), tip.clone()));
    }
    for (k, why) in &now_down {
        let e = g.entry(k.clone()).or_insert_with(|| Seen { since: Instant::now(), why: why.clone(), posted: false });
        if !e.posted && e.since.elapsed() >= Duration::from_secs(10) {
            e.posted = true;
            post_system(2, format!("{k} 中断"), e.why.clone());
        }
    }
    g.retain(|k, e| {
        let still = now_down.iter().any(|(x, _)| x == k);
        if !still && e.posted {
            post_system(3, format!("{k} 恢复"), format!("中断了 {} 秒", e.since.elapsed().as_secs()));
        }
        still
    });
}

// ── 后台线程 ──────────────────────────────────────────────────────────

static STARTED: OnceLock<()> = OnceLock::new();

/// Cockpit 启动时调一次（样张模式不调）。
pub fn start() {
    STARTED.get_or_init(|| {
        super::spawn_named("ws-inbox", run);
    });
}

fn run() {
    let path = super::calendar_alerts::db_path();
    let c = match super::calendar_alerts::open(&path) {
        Ok(c) => c,
        Err(e) => {
            set_snap(Snap { err: format!("打不开消息库 {}：{e}", path.display()), ..Default::default() });
            return;
        }
    };
    match ib::prune(&c, now_ms()) {
        Ok(n) if n > 0 => log::info!("[inbox] 清掉 {n} 条过期消息（新闻 / 低级别系统消息超过 {} 天）", ib::KEEP_DAYS),
        Err(e) => log::warn!("[inbox] 清理过期消息失败：{e}"),
        _ => {}
    }
    let mut last_max = -1;
    // 提醒声音（docs/68）的游标：启动时库里已有的积压不响
    super::alert_sound::init();
    let mut sound_from = ib::max_id(&c);
    let mut last_rev = u64::MAX;
    let mut last_full = Instant::now();
    loop {
        let ops: Vec<Op> = std::mem::take(&mut *queue().ops.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
        let acted = !ops.is_empty();
        let mut err = String::new();
        for op in ops {
            if let Err(e) = exec(&c, op) {
                err = format!("消息库写入失败：{e}");
            }
        }
        let max = ib::max_id(&c);
        if max > sound_from {
            let v: Vec<super::alert_sound::Arrival> = ib::arrivals(&c, sound_from)
                .into_iter()
                .map(|(id, level, silent)| super::alert_sound::Arrival { id, level, silent })
                .collect();
            sound_from = v.last().map_or(max, |a| a.id);
            super::alert_sound::on_arrivals(&c, &v);
        }
        super::alert_sound::tick(&c);
        let u = ui();
        if acted || max != last_max || u.rev != last_rev || last_full.elapsed() >= Duration::from_secs(30) {
            last_max = max;
            last_rev = u.rev;
            last_full = Instant::now();
            let now = now_ms();
            let mut s = Snap { badge: ib::badge(&c, now), live: true, err, ..Default::default() };
            if u.open || super::alert_rules::is_open() {
                s.stats = ib::rule_stats(&c);
            }
            if u.open {
                match ib::query(&c, &u.filter(now)) {
                    Ok(v) => s.list = v,
                    Err(e) => s.err = format!("查询失败：{e}"),
                }
                s.groups = ib::fold(&s.list);
                if let Some(id) = u.sel
                    && let Ok(Some(m)) = ib::get(&c, id)
                {
                    let d = m.job_id.map(|j| ib::deliveries(&c, j)).unwrap_or_default();
                    s.detail = Some((m, d));
                }
            }
            set_snap(s);
        }
        let q = queue();
        let g = q.ops.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = q.wake.wait_timeout_while(g, Duration::from_secs(1), |v| v.is_empty());
    }
}

fn set_snap(s: Snap) {
    if let Ok(mut g) = snap_cell().lock() {
        *g = Arc::new(s);
    }
}

fn exec(c: &ib::rusqlite::Connection, op: Op) -> ib::rusqlite::Result<()> {
    let now = now_ms();
    match op {
        Op::Read(ids) => ib::mark_read(c, &ids, now).map(|_| ()),
        Op::Unread(ids) => ib::mark_unread(c, &ids).map(|_| ()),
        Op::Ack(ids) => ib::ack(c, &ids, now).map(|_| ()),
        Op::Archive(ids) => ib::archive(c, &ids, now).map(|_| ()),
        Op::Unarchive(ids) => ib::unarchive(c, &ids).map(|_| ()),
        Op::Star(id, on) => ib::star(c, id, on).map(|_| ()),
        Op::AllRead => ib::mark_all_read(c, now).map(|_| ()),
        Op::Post(p) => ib::post(c, &p).map(|_| ()),
        Op::Export(mut f) => {
            f.limit = 1_000_000;
            let v = ib::query(c, &f)?;
            let dir = super::paths::hot_dir().join("exports");
            let file = dir.join(format!("消息-{}.csv", chrono::Local::now().format("%Y%m%d-%H%M%S")));
            let r = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&file, ib::csv(&v)));
            ui_lock().note = match r {
                Ok(()) => format!("✔ 导出 {} 条 → {}", v.len(), file.display()),
                Err(e) => format!("✗ 导出失败：{e}"),
            };
            Ok(())
        }
    }
}

// ── 动作 ──────────────────────────────────────────────────────────────

/// 勾选的；没勾就是选中的那条。
fn targets(u: &Ui) -> Vec<i64> {
    if u.checked.is_empty() { u.sel.into_iter().collect() } else { u.checked.iter().copied().collect() }
}

pub fn handle(m: InboxMsg) {
    let mut u = ui_lock();
    let mut refilter = true;
    match m {
        InboxMsg::Toggle => {
            u.open = !u.open;
            u.note.clear();
        }
        InboxMsg::Cat(c) => {
            u.cat = c;
            u.checked.clear();
        }
        InboxMsg::Text(t) => u.text = t,
        InboxMsg::Symbol(t) => u.symbol = t,
        InboxMsg::Range(r) => u.range = r,
        InboxMsg::ClearRule => u.rule_ref = None,
        InboxMsg::Select(id) => {
            u.sel = if u.sel == Some(id) { None } else { Some(id) };
            if u.sel.is_some() {
                push(Op::Read(vec![id]));
            }
        }
        InboxMsg::Check(id, on) => {
            refilter = false;
            if on {
                u.checked.insert(id);
            } else {
                u.checked.remove(&id);
            }
        }
        InboxMsg::CheckNone => {
            refilter = false;
            u.checked.clear();
        }
        InboxMsg::CheckShown => {
            refilter = false;
            u.checked = snapshot().list.iter().map(|m| m.id).collect();
        }
        InboxMsg::Read => push(Op::Read(targets(&u))),
        InboxMsg::Unread => push(Op::Unread(targets(&u))),
        InboxMsg::Ack => push(Op::Ack(targets(&u))),
        InboxMsg::Archive => {
            push(Op::Archive(targets(&u)));
            u.checked.clear();
        }
        InboxMsg::Unarchive => {
            push(Op::Unarchive(targets(&u)));
            u.checked.clear();
        }
        InboxMsg::Star(id, on) => push(Op::Star(id, on)),
        InboxMsg::AllRead => push(Op::AllRead),
        InboxMsg::Expand(id) => {
            refilter = false;
            if !u.expanded.remove(&id) {
                u.expanded.insert(id);
            }
        }
        InboxMsg::OpenUrl(url) => {
            refilter = false;
            // 外部链接是数据：只放行 http(s)
            if let Some(url) = ib::safe_url(&url) {
                super::news::open_in_browser(url);
            } else {
                u.note = "✗ 只打开 http(s) 链接".into();
            }
        }
        InboxMsg::Goto(ms) => {
            refilter = false;
            crate::chart::goto_time::request(ms.max(0) as u64);
            u.note = "已让本页的 K 线图跳到这个时刻（本页没有 K 线图时不动）".into();
        }
        InboxMsg::RuleHistory(r) => {
            u.open = true;
            u.rule_ref = Some(r);
            u.cat = ib::Cat::All;
            u.range = Range::All;
            u.checked.clear();
        }
        InboxMsg::Export => {
            refilter = false;
            push(Op::Export(u.filter(now_ms())));
            u.note = "导出中…".into();
        }
        InboxMsg::ToggleCats => {
            refilter = false;
            u.cats_hidden = !u.cats_hidden;
        }
        InboxMsg::SoundPage => {
            refilter = false;
            u.sound_page = !u.sound_page;
        }
        InboxMsg::Sound(m) => {
            drop(u);
            super::alert_sound::handle(m);
            return;
        }
        InboxMsg::Rules => {
            drop(u);
            if !super::alert_rules::is_open() {
                super::alert_rules::handle(super::alert_rules::RulesMsg::Toggle);
            }
            wake();
            return;
        }
    }
    if refilter {
        u.rev += 1;
    }
    drop(u);
    wake();
}

/// 提醒管理器里「触发历史」：打开消息中心并只看这条规则。
pub fn show_rule(rule_ref: &str) {
    handle(InboxMsg::RuleHistory(rule_ref.to_string()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 没勾选时动作落在选中的那条() {
        let mut u = Ui { sel: Some(7), ..Default::default() };
        assert_eq!(targets(&u), vec![7]);
        u.checked.insert(3);
        u.checked.insert(5);
        assert_eq!(targets(&u), vec![3, 5]);
    }

    #[test]
    fn 时间段() {
        let now = 10 * 86_400_000;
        assert_eq!(Range::Day.since(now), Some(9 * 86_400_000));
        assert_eq!(Range::All.since(now), None);
    }
}
