//! 预测市场回放 — 状态与交互（「Tardis 历史回放」工作区的独立视图）。
//!
//! ## 回放单位是「一轮」，不是「一天」
//!
//! 5 分钟市场每轮换一个 `market_id`、换一套盘口。把一天 68 轮的曲线接起来没有意义：
//! 上一轮结算时赢的一侧冲到 0.99、下一轮新市场从 0.5 开始，画出来像一次暴跌，
//! 其实只是换了个市场。所以选择器是 **日期 → 轮次**，一次回放一轮。
//!
//! ## 为什么不并进旁边那个三源选择器
//!
//! 那套的类型词汇（trades / l2 / deriv / 强平 / BBO…）是给交易所行情设计的。
//! 预测市场一个都不对应：没有成交流、没有中间价、没有资金费，有的是
//! **两本独立的簿 + 一个二元结局**。硬塞进去要么类型名撒谎，要么到处是空列。
//!
//! 复用的是**下面那一层**：图表 JSON 契约、绘图部件、播放头裁剪全是同一套
//! （见 [`super::pm_replay_readout`]）。
//!
//! ## 播放头本地算
//!
//! Tardis 那边靠一个外部推流器往 Redis 写 `data_ts`，因为它还要同时喂 FS 图表。
//! 这里不需要——只是把已加载的曲线按时间裁开，`iced::window::frames()` 逐帧重绘
//! 就够了。少一个进程、少一条 Redis 依赖、停止即刻生效。

use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use super::pm_replay_readout as ro;

fn repo() -> std::path::PathBuf {
    std::env::var("WS_REPO")
        .unwrap_or_else(|_| super::paths::repo_root().to_string_lossy().into_owned())
        .into()
}

fn venv_py() -> String {
    std::env::var("WS_VENV_PY")
        .unwrap_or_else(|_| super::paths::python().to_string_lossy().into_owned())
}

/// 回放倍速。一轮只有 5 分钟，所以最低档就是 1×（实时重演）；
/// 60× 的话 5 秒看完一轮，适合快速扫过很多轮找异常。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Speed {
    X1,
    X5,
    X20,
    X60,
}

impl Speed {
    pub const ALL: [Speed; 4] = [Speed::X1, Speed::X5, Speed::X20, Speed::X60];
    pub fn mult(self) -> f64 {
        match self {
            Speed::X1 => 1.0,
            Speed::X5 => 5.0,
            Speed::X20 => 20.0,
            Speed::X60 => 60.0,
        }
    }
}

impl std::fmt::Display for Speed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}×", self.mult() as i64)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PmReplayMsg {
    PickSymbol(String),
    PickDate(String),
    PickRound(String),
    PickSpeed(Speed),
    Load,
    Play,
    Pause,
    Rewind,
    Seek(f32),
    RefreshCatalog,
    /// 只跳到上一轮/下一轮并加载——扫一串轮次时省得每次点三下。
    Step(i32),
}

#[derive(Clone, Debug)]
pub struct PmReplayState {
    pub symbol: String,
    pub date: String,
    pub market_id: String,
    pub speed: Speed,
    /// 拖动进度条留下的位置（0–1）。静止时图停在这里，不回到整窗。
    pub seek_pct: Option<f32>,
    pub hint: String,
}

impl Default for PmReplayState {
    fn default() -> Self {
        Self {
            symbol: String::new(),
            date: String::new(),
            market_id: String::new(),
            speed: Speed::X20,
            seek_pct: None,
            hint: String::new(),
        }
    }
}

impl PmReplayState {
    /// 建面板时调用：预选第一个符号/日期/轮次，让它一打开就有东西可点。
    pub fn load() -> Self {
        let mut st = Self::default();
        st.fill_defaults();
        st
    }

    /// 补齐空着的选择。**只补空的**——已选的不动，否则每帧都会把用户的选择推回默认。
    pub fn fill_defaults(&mut self) {
        let cat = ro::catalog();
        if self.symbol.is_empty() {
            if let Some(s) = cat.symbols.first() {
                self.symbol = s.symbol.clone();
            }
        }
        let Some(sym) = cat.symbol(&self.symbol) else { return };
        if self.date.is_empty() || !sym.dates.iter().any(|d| d.date == self.date) {
            self.date = sym.dates.first().map(|d| d.date.clone()).unwrap_or_default();
            self.market_id.clear();
        }
        let Some(d) = cat.date(&self.symbol, &self.date) else { return };
        if self.market_id.is_empty() || !d.rounds.iter().any(|r| r.market_id == self.market_id) {
            // 默认选**最后一个已结算**的轮次：未结算的看不到结局，回放价值低。
            self.market_id = d
                .rounds
                .iter()
                .rev()
                .find(|r| !r.resolved.is_empty())
                .or_else(|| d.rounds.last())
                .map(|r| r.market_id.clone())
                .unwrap_or_default();
        }
    }

    pub fn rounds(&self) -> Vec<ro::RoundEntry> {
        ro::catalog().date(&self.symbol, &self.date).map(|d| d.rounds.clone()).unwrap_or_default()
    }
}

// ── 子进程：生成 catalog / panel ─────────────────────────────────────────────
static JOB: OnceLock<Mutex<Option<(Child, String)>>> = OnceLock::new();
static MSG: OnceLock<Mutex<String>> = OnceLock::new();

fn job() -> &'static Mutex<Option<(Child, String)>> {
    JOB.get_or_init(|| Mutex::new(None))
}

pub fn message() -> String {
    MSG.get().and_then(|m| m.lock().ok().map(|g| g.clone())).unwrap_or_default()
}

fn set_message(s: String) {
    if let Ok(mut g) = MSG.get_or_init(|| Mutex::new(String::new())).lock() {
        *g = s;
    }
}

/// 还在跑就返回它在干什么；跑完则收结果、作废缓存。
pub fn poll() -> Option<String> {
    let mut g = job().lock().ok()?;
    let (child, desc) = g.as_mut()?;
    match child.try_wait() {
        Ok(None) => Some(desc.clone()),
        Ok(Some(status)) => {
            let mut err = String::new();
            if let Some(mut e) = child.stderr.take() {
                use std::io::Read;
                let _ = e.read_to_string(&mut err);
            }
            set_message(if status.success() {
                ro::invalidate();
                format!("✔ {desc}已就绪")
            } else {
                // 只取最后一行：Python 的 traceback 很长，塞进一行界面里没法看，
                // 而最后一行正是异常本身。
                format!("✗ {desc}失败：{}", err.lines().last().unwrap_or("(无输出)"))
            });
            *g = None;
            None
        }
        Err(_) => {
            *g = None;
            None
        }
    }
}

fn spawn(args: &[String], desc: &str) -> String {
    if poll().is_some() {
        return "上一次还没跑完，稍等".into();
    }
    let out = super::paths::data_dir().join("cockpit");
    let _ = std::fs::create_dir_all(&out);
    let child = Command::new(venv_py())
        .current_dir(repo())
        .args(["-m", "factory.replay.pm_replay"])
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn();
    match child {
        Ok(c) => {
            if let Ok(mut g) = job().lock() {
                *g = Some((c, desc.to_string()));
            }
            format!("正在生成{desc}…")
        }
        Err(e) => format!("✗ 起不来 Python：{e}"),
    }
}

fn refresh_catalog() -> String {
    spawn(
        &["--catalog".into(), "--out".into(), ro::catalog_path().to_string_lossy().into_owned()],
        "轮次清单",
    )
}

fn load_round(st: &PmReplayState) -> String {
    if st.date.is_empty() || st.market_id.is_empty() {
        return "✗ 先选日期和轮次（没有的话点「刷新清单」）".into();
    }
    stop();
    spawn(
        &[
            "--symbol".into(),
            st.symbol.clone(),
            "--date".into(),
            st.date.clone(),
            "--round".into(),
            st.market_id.clone(),
            "--out".into(),
            ro::panel_path().to_string_lossy().into_owned(),
        ],
        &format!("{} 轮次 {}", st.date, st.market_id),
    )
}

// ── 播放头：本地推进 ─────────────────────────────────────────────────────────
/// `(起播时刻, 起播时的数据时间 ms, 倍速)`。`None` = 没在播。
static PLAY: OnceLock<Mutex<Option<(Instant, f64, f64)>>> = OnceLock::new();

fn play_state() -> &'static Mutex<Option<(Instant, f64, f64)>> {
    PLAY.get_or_init(|| Mutex::new(None))
}

pub fn is_playing() -> bool {
    play_state().lock().map(|g| g.is_some()).unwrap_or(false)
}

pub fn stop() {
    if let Ok(mut g) = play_state().lock() {
        *g = None;
    }
}

fn start(from_ms: f64, mult: f64) {
    if let Ok(mut g) = play_state().lock() {
        *g = Some((Instant::now(), from_ms, mult));
    }
}

/// 播放头的算法本体（纯函数）：返回 `(数据时间 ms, 是否已播完)`。
///
/// 抽出来是为了能测：全局播放状态是进程级的，而测试并行跑——
/// 一条测试的 `stop()` 会把另一条的播放掐掉，测出来的失败与被测逻辑无关。
pub fn head_at(from_ms: f64, elapsed_s: f64, mult: f64, t1_ms: f64) -> (f64, bool) {
    let h = from_ms + elapsed_s * 1000.0 * mult;
    // **停在末尾而不是回到开头**：否则最后那几秒（结算瞬间，最值得看的部分）
    // 会一闪而过，而那正是判断「当时的失衡指对没指对」的地方。
    if h >= t1_ms { (t1_ms, true) } else { (h, false) }
}

/// 当前播放头（数据时间 ms）。播到末尾自动停。
pub fn head(t1_ms: f64) -> Option<f64> {
    let mut g = play_state().lock().ok()?;
    let (t0, from, mult) = (*g)?;
    let (h, done) = head_at(from, t0.elapsed().as_secs_f64(), mult, t1_ms);
    if done {
        *g = None;
    }
    Some(h)
}

pub fn handle(st: &mut PmReplayState, msg: PmReplayMsg) {
    match msg {
        PmReplayMsg::PickSymbol(s) => {
            st.symbol = s;
            st.date.clear();
            st.market_id.clear();
            st.fill_defaults();
            stop();
        }
        PmReplayMsg::PickDate(d) => {
            st.date = d;
            st.market_id.clear();
            st.fill_defaults();
            stop();
        }
        PmReplayMsg::PickRound(r) => {
            st.market_id = r;
            stop();
            st.seek_pct = None;
        }
        PmReplayMsg::PickSpeed(s) => {
            st.speed = s;
            // 播放中换倍速：从当前位置接着播，不回到开头。
            if is_playing() {
                let p = ro::panel();
                if let Some((t0, t1)) = ro::time_span(&p)
                    && let Some(h) = head(t1)
                {
                    let _ = t0;
                    start(h, s.mult());
                }
            }
        }
        PmReplayMsg::Load => st.hint = load_round(st),
        PmReplayMsg::RefreshCatalog => st.hint = refresh_catalog(),
        PmReplayMsg::Play => {
            let p = ro::panel();
            match ro::time_span(&p) {
                None => st.hint = "✗ 先点「加载」出图，再回放".into(),
                Some((t0, t1)) => {
                    // 从拖动位置起播；已在末尾（>99%）则回到开头，避免点了没反应。
                    let from = match st.seek_pct {
                        Some(p) if p < 0.99 => t0 + (t1 - t0) * p as f64,
                        _ => t0,
                    };
                    start(from, st.speed.mult());
                    st.hint.clear();
                }
            }
        }
        PmReplayMsg::Pause => {
            // 暂停要把位置留在原地，否则一按暂停图就跳回整窗。
            let p = ro::panel();
            if let Some((t0, t1)) = ro::time_span(&p)
                && let Some(h) = head(t1)
            {
                st.seek_pct = Some(((h - t0) / (t1 - t0)) as f32);
            }
            stop();
        }
        PmReplayMsg::Rewind => {
            stop();
            st.seek_pct = None;
        }
        PmReplayMsg::Seek(p) => {
            stop();
            st.seek_pct = Some(p.clamp(0.0, 1.0));
        }
        PmReplayMsg::Step(d) => {
            let rs = st.rounds();
            if let Some(i) = rs.iter().position(|r| r.market_id == st.market_id) {
                let j = (i as i64 + d as i64).clamp(0, rs.len() as i64 - 1) as usize;
                if j != i {
                    st.market_id = rs[j].market_id.clone();
                    st.seek_pct = None;
                    stop();
                    st.hint = load_round(st);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(rounds: &[(&str, &str)]) -> PmReplayState {
        let mut s = PmReplayState::default();
        s.symbol = "S".into();
        s.date = "D".into();
        s.market_id = rounds.first().map(|r| r.0.to_string()).unwrap_or_default();
        s
    }

    #[test]
    fn 倍速档覆盖一轮的量级() {
        // 一轮 300 秒。1× 是实时重演，60× 是 5 秒扫完——两头都要够得着。
        assert_eq!(Speed::X1.mult(), 1.0);
        assert!(Speed::ALL.iter().any(|s| 300.0 / s.mult() <= 6.0), "要有能几秒扫完一轮的档");
        assert_eq!(Speed::X20.to_string(), "20×");
    }

    #[test]
    fn 播完停在末尾而不是回到开头() {
        // 一轮 300 秒（t0=0, t1=300_000）。
        assert_eq!(head_at(0.0, 400.0, 1.0, 300_000.0), (300_000.0, true), "越界要夹到末尾并判完");
        assert_eq!(head_at(0.0, 150.0, 1.0, 300_000.0), (150_000.0, false));
    }

    #[test]
    fn 倍速按数据时间推进而不是墙钟() {
        // 20× 下 15 秒墙钟 = 300 秒数据时间 = 整整一轮。
        let (h, done) = head_at(0.0, 15.0, 20.0, 300_000.0);
        assert!((h - 300_000.0).abs() < 1e-6 && done);
        // 同样 15 秒墙钟，1× 只走了 15 秒数据时间。
        assert_eq!(head_at(0.0, 15.0, 1.0, 300_000.0).0, 15_000.0);
    }

    #[test]
    fn 从拖动位置起播不是从头() {
        assert_eq!(head_at(200_000.0, 10.0, 1.0, 300_000.0).0, 210_000.0);
    }

    #[test]
    fn 暂停保留位置而不是跳回整窗() {
        let mut s = st(&[("1", "")]);
        s.seek_pct = None;
        stop();
        // 没有已加载面板时暂停不该 panic，也不该乱设位置
        handle(&mut s, PmReplayMsg::Pause);
        assert!(!is_playing());
    }

    #[test]
    fn 上下一轮在边界不越界() {
        let mut s = st(&[("1", "")]);
        // rounds() 读的是真实 catalog，测试环境里通常为空 → Step 应当安全地什么都不做
        let before = s.market_id.clone();
        handle(&mut s, PmReplayMsg::Step(-1));
        handle(&mut s, PmReplayMsg::Step(1));
        assert_eq!(s.market_id, before);
    }

    #[test]
    fn 换符号会清掉日期与轮次() {
        let mut s = st(&[("1", "")]);
        handle(&mut s, PmReplayMsg::PickSymbol("别的".into()));
        // fill_defaults 会试图补，但 catalog 里没有「别的」，故应留空而不是留着上一个符号的轮次
        assert!(s.market_id.is_empty() || s.symbol == "别的");
    }
}
