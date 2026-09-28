//! Tardis 历史面板 — 状态与交互（docs/20 §9）。
//!
//! 层次：**数据源(3) → 数据类型(8) → 图表(主图 + 该类型的衍生图)**。
//! **全程零交易所流**：不声明任何 ticker、不订阅任何实时连接；数据由主仓
//! `factory/replay/panels.py` 落成 JSON，本面板只读渲染（见 [`super::tardis_board_readout`]）。
//!
//! 四个数据接口（主仓 `factory/replay/sources.py`，产出同一套规范化列）：
//!   ① Tardis · DuckDB 仓   ② Tardis · 直读 Parquet   ③ Databento（经 wealthspring_py 统一数据源接口）   ④ 自录数据
//!
//! 选数据用共用数据选择组件（[`super::data_picker`]，docs/28）：管线 B → B2 购买数据（Tardis / Databento）/
//! B3 本地录制 → 根目录扫描 → 市场 → 标的 → 日期 + 起始时刻 + 时长。选定的根目录经
//! `WS_TARDIS_ROOT` / `WS_DATABENTO_ROOT` / `WS_DATA_DIR` 传给脚本。数据类型仍由本面板单选（它有自己的规范类型名与中文名）：
//! 可选的类型 = 扫描到的当天类型（数据商原名换成规范名，见 [`canonical_types`]）。

use std::process::{Command, Stdio};

use super::data_picker::{BSource, DataPick, DataPickMsg, Load, PickOpts, Pipeline, Purpose, TimeMode};
use super::tardis_board_readout as ro;

/// 本面板对共用组件的要求：只读本地（B2 Tardis / Databento、B3 录制），时间选一段，类型自己选。
#[must_use]
pub fn pick_opts() -> PickOpts {
    PickOpts {
        purpose: Purpose::Chart,
        sources: Some(vec![BSource::Purchased, BSource::Recorded]),
        vendors: None,
        time: TimeMode::Window,
        local_only: true,
        hide_types: true,
    }
}

/// 扫描给的类型名（数据商原名）→ 本面板（`sources.py`）能画的规范类型。
///
/// - Tardis：本来就是规范名。
/// - 录制器：按 `RecorderSource.MAP` 反查。
/// - Databento（`DatabentoSource`）：`mbo` 经 Rust 逐单重建出增量盘口与逐笔成交；`trades` 即逐笔成交；
///   `mbp-10` 是交易所发布的前 10 档，给 BBO 顶档与深簿 5 档。
///
/// 没有对应物的（录制器 `snap100ms`、Databento `definition` / `statistics` / `status`）给空。
#[must_use]
pub fn canonical_types(source: &str, t: &str) -> Vec<&'static str> {
    match (source, t) {
        ("tardis", _) => ro_type(t).into_iter().collect(),
        ("recorder", "trades") | ("databento", "trades") => vec!["trades"],
        ("recorder", "l2") => vec!["incremental_book_L2"],
        ("recorder", "mark") => vec!["derivative_ticker"],
        ("databento", "mbo") => vec!["incremental_book_L2", "trades"],
        ("databento", "mbp-10") => vec!["book_ticker", "book_snapshot_5"],
        _ => Vec::new(),
    }
}

/// 八种规范类型里的哪一个（Tardis 扫描名原样对上）。
fn ro_type(t: &str) -> Option<&'static str> {
    [
        "incremental_book_L2",
        "book_ticker",
        "quotes",
        "book_snapshot_25",
        "book_snapshot_5",
        "trades",
        "derivative_ticker",
        "liquidations",
    ]
    .into_iter()
    .find(|x| *x == t)
}

/// Tardis 的两种读法（同一份数据）：直读 Parquet（跟随所选根目录）/ DuckDB 仓（固定仓库文件）。
pub const TARDIS_ACCESS: [(&str, &str); 2] =
    [("tardis_parquet", "直读 Parquet"), ("tardis_duckdb", "DuckDB 仓")];

fn repo() -> std::path::PathBuf {
    std::env::var("WS_REPO")
        .unwrap_or_else(|_| {
            super::paths::repo_root().to_string_lossy().into_owned()
        })
        .into()
}

fn venv_py() -> String {
    std::env::var("WS_VENV_PY").unwrap_or_else(|_| {
        super::paths::python().to_string_lossy().into_owned()
    })
}

/// 时长档（分钟）。窗口越长下载/重建越久，故给固定档位。
pub const MINUTES: [u32; 5] = [1, 5, 10, 30, 60];

/// 回放倍速档（∞ = 直接跳到末尾）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Speed {
    X1,
    X10,
    X60,
    X300,
    Max,
}

impl Speed {
    pub const ALL: [Speed; 5] = [Speed::X1, Speed::X10, Speed::X60, Speed::X300, Speed::Max];
    pub fn arg(self) -> &'static str {
        match self {
            Speed::X1 => "1",
            Speed::X10 => "10",
            Speed::X60 => "60",
            Speed::X300 => "300",
            Speed::Max => "0",
        }
    }
}

impl std::fmt::Display for Speed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Speed::X1 => "×1",
            Speed::X10 => "×10",
            Speed::X60 => "×60",
            Speed::X300 => "×300",
            Speed::Max => "跳到末尾",
        })
    }
}

#[derive(Clone)]
pub struct TardisBoardState {
    /// 共用数据选择组件的选择（下面的 source / symbol / date / start_hm / minutes 由它同步而来）。
    pub pick: DataPick,
    /// B2 Tardis 的读法（[`TARDIS_ACCESS`] 的键）。
    pub access: String,
    /// `sources.py` 的源键：`tardis_parquet` / `tardis_duckdb` / `databento` / `recorder`（空 = 还没选到本地来源）。
    pub source: String,
    pub symbol: String,
    pub date: String,
    pub start_hm: String,
    pub minutes: u32,
    pub dtype: String,
    pub hint: String,
    pub busy: bool,
    /// 时间步进回放倍速（docs/20 §10）。
    pub speed: Speed,
    /// 用户拖动的播放位置（0~100%）。None = 未拖过（回放从头起、静止时显示整窗）。
    pub seek_pct: Option<f32>,
    /// 跨符号对比：同窗口把该源全部符号并排画（docs/20 §16）。
    pub compare: bool,
    /// 选择变化后自动加载（docs/20 §22）。默认开。
    pub auto_load: bool,
    /// L2 盘口热图价格带 = mid ± N tick（docs/20 §22）。仅对增量盘口 L2 生效。
    pub band_ticks: u32,
}

impl Default for TardisBoardState {
    fn default() -> Self {
        Self::load()
    }
}

impl TardisBoardState {
    /// 缺省：管线 B · B2 购买数据 · Tardis · 加密 · BTCUSDT，09:00 起 10 分钟（日期取扫描到的最新一天）。
    pub fn load() -> Self {
        let mut pick = DataPick::default();
        for m in [
            DataPickMsg::Pipeline(Pipeline::B),
            DataPickMsg::Source(BSource::Purchased),
            DataPickMsg::Vendor("tardis".into()),
            DataPickMsg::Market("crypto".into()),
            DataPickMsg::Symbol("BTCUSDT".into()),
            DataPickMsg::Start("09:00".into()),
            DataPickMsg::Minutes(10),
        ] {
            pick.update(m);
        }
        let mut st = Self {
            pick,
            access: "tardis_parquet".into(),
            source: String::new(),
            symbol: String::new(),
            date: String::new(),
            start_hm: "09:00".into(),
            minutes: 10,
            dtype: "trades".into(),
            hint: "选 管线 → 来源 → 市场 → 标的 → 时间，再选数据类型，点「加载」出图；再点「▶ 回放」按时间步进播放".into(),
            busy: false,
            speed: Speed::X60,
            seek_pct: None,
            compare: false,
            auto_load: true,
            band_ticks: 200,
        };
        st.sync();
        st
    }

    /// 本地来源的扫描结果（还没选到本地来源 / 还在扫 = `None`）。
    fn scan(&self) -> Option<super::data_picker::Scan> {
        match self.pick.local_scan()? {
            Load::Ready(s) => Some(s),
            _ => None,
        }
    }

    /// 当前 标的 × 日 可选的数据类型（规范名，按扫描顺序去重）。
    #[must_use]
    pub fn avail_types(&self) -> Vec<String> {
        let (Some(sc), Some(key)) = (self.scan(), self.pick.local_key()) else {
            return Vec::new();
        };
        if self.symbol.is_empty() || self.date.is_empty() {
            return Vec::new();
        }
        let mut v: Vec<String> = Vec::new();
        for t in sc.types(&self.symbol, &self.date) {
            for c in canonical_types(key, &t) {
                if !v.iter().any(|x| x == c) {
                    v.push(c.to_string());
                }
            }
        }
        v
    }

    /// 同市场的标的数（跨符号对比要 ≥ 2）。
    #[must_use]
    pub fn n_symbols(&self) -> usize {
        match (self.scan(), self.pick.market.as_deref()) {
            (Some(sc), Some(mk)) => sc.symbols(mk, "").len(),
            _ => 0,
        }
    }

    /// 所选根目录要经哪个环境变量传给脚本：`(变量名, 目录)`。
    #[must_use]
    pub fn root_env(&self) -> Option<(&'static str, String)> {
        match self.pick.local_key()? {
            "tardis" => Some(("WS_TARDIS_ROOT", self.pick.root_or_default())),
            "databento" => Some(("WS_DATABENTO_ROOT", self.pick.root_or_default())),
            "recorder" => Some(("WS_DATA_DIR", self.pick.root_or_default())),
            _ => None,
        }
    }

    /// 从组件的选择同步出脚本要的参数；日期没选时取扫描到的最新一天；类型不在当天可选范围内就拉回第一个。
    fn sync(&mut self) {
        self.pick.poll();
        self.source = match self.pick.local_key() {
            Some("tardis") => self.access.clone(),
            Some("databento") => "databento".into(),
            Some("recorder") => "recorder".into(),
            _ => String::new(),
        };
        self.symbol = self.pick.symbol.clone().unwrap_or_default();
        self.date = match (&self.pick.date, self.scan()) {
            (Some(d), _) => d.clone(),
            (None, Some(sc)) if !self.symbol.is_empty() => sc.dates(&self.symbol).last().cloned().unwrap_or_default(),
            _ => String::new(),
        };
        self.start_hm = self.pick.start.clone();
        self.minutes = self.pick.minutes;
        let types = self.avail_types();
        if !types.is_empty() && !types.contains(&self.dtype) {
            self.dtype = types[0].clone();
        }
    }
}

#[derive(Debug, Clone)]
pub enum TardisBoardMsg {
    /// 共用数据选择组件的消息。
    Data(DataPickMsg),
    /// B2 Tardis 的读法（[`TARDIS_ACCESS`]）。
    Access(String),
    TypePick(String),
    Load,
    RefreshCatalog,
    SpeedPick(Speed),
    Play,
    StopPlay,
    /// 拖动进度条到 0~100%（docs/20 §15）。
    Seek(f32),
    /// 切换跨符号对比（docs/20 §16）。
    ToggleCompare,
    /// 切换「改完自动加载」（docs/20 §22）。
    ToggleAuto,
    /// L2 热图价格带（tick 数）。
    BandPick(u32),
    /// 导出当前面板为 CSV / 窗口截图（docs/20 §17）。
    ExportCsv,
    ExportPng,
}

pub fn hours() -> Vec<String> {
    (0..24).map(|h| format!("{h:02}:00")).collect()
}

/// 会改变「该加载什么」的消息——自动加载只对这些触发，
/// 回放/导出/开关自身等不该触发（否则点个倍速也要重跑一遍数据）。
fn changes_selection(m: &TardisBoardMsg) -> bool {
    matches!(
        m,
        TardisBoardMsg::Data(
            DataPickMsg::Symbol(_) | DataPickMsg::Date(_) | DataPickMsg::Minutes(_)
        ) | TardisBoardMsg::Access(_)
            | TardisBoardMsg::TypePick(_)
            | TardisBoardMsg::ToggleCompare
            | TardisBoardMsg::BandPick(_)
    )
}

pub fn handle(st: &mut TardisBoardState, msg: TardisBoardMsg) {
    if !matches!(msg, TardisBoardMsg::Load) {
        clear_load_message(); // 其它交互后让各自的 hint 显示，不被上次加载结果盖住
    }
    // 每条消息前先把目录选择框的结果 / 刚扫完的目录收进来（日期缺省取最新一天要等扫描）
    st.sync();
    let auto = st.auto_load && changes_selection(&msg);
    match msg {
        TardisBoardMsg::Data(m) => {
            st.pick.update(m);
            st.sync();
        }
        TardisBoardMsg::Access(a) => {
            st.access = a;
            st.sync();
            st.hint = if st.access == "tardis_duckdb" {
                "DuckDB 仓读的是固定的仓库文件（WS_DUCKDB_PATH），不跟随上面选的根目录".into()
            } else {
                "直读 Parquet：读上面选的根目录".into()
            };
        }
        TardisBoardMsg::TypePick(t) => {
            st.dtype = t;
            st.hint = "类型已切换，点「加载」出该类型的主图与衍生图".into();
        }
        TardisBoardMsg::Load => st.hint = load(st),
        TardisBoardMsg::RefreshCatalog => st.hint = refresh_catalog(),
        TardisBoardMsg::ToggleAuto => {
            st.auto_load = !st.auto_load;
            st.hint = if st.auto_load {
                "已开自动加载：改选择即刷新".into()
            } else {
                "已关自动加载：改完需手点「加载」".into()
            };
        }
        TardisBoardMsg::BandPick(b) => st.band_ticks = b,
        TardisBoardMsg::SpeedPick(sp) => st.speed = sp,
        TardisBoardMsg::Play => st.hint = play(st),
        TardisBoardMsg::StopPlay => st.hint = stop_play(),
        TardisBoardMsg::ExportCsv => st.hint = export_csv(),
        TardisBoardMsg::ExportPng => st.hint = export_png(),
        TardisBoardMsg::ToggleCompare => {
            st.compare = !st.compare;
            st.hint = if st.compare {
                "已开对比：同窗口并排画该源全部符号，只出无量纲/同单位量。点「加载」".into()
            } else {
                "已回到单符号视图。点「加载」".into()
            };
        }
        TardisBoardMsg::Seek(pct) => {
            let p = pct.clamp(0.0, 100.0);
            st.seek_pct = Some(p);
            // 正在放 → 带新起点重起（feeder 无状态，重起即跳转）；静止 → 只挪游标。
            if is_playing() {
                st.hint = play(st);
            } else {
                st.hint = format!("⏱ 已定位到 {p:.0}%（点「▶ 回放」从此处播）");
            }
        }
    }
    // 还没选完（没到标的 / 日期）就不自动加载——否则选的过程中一直冒「没有可用的…」
    if auto && !st.source.is_empty() && !st.symbol.is_empty() && !st.date.is_empty() {
        // 选择变了就直接重载。加载是异步的，界面不会卡；正忙时 load() 自己会拒并提示。
        let m = load(st);
        if !m.is_empty() {
            st.hint = m;
        }
    }
}

/// 进行中的加载任务：(子进程, 人读描述)。加载**异步**——同步 `output()` 会把 iced
/// 的 update 循环整个卡住（实测 L2 30/60min 窗口 2.3~3.5s，界面全程冻结）。
static LOAD: std::sync::Mutex<Option<(std::process::Child, String)>> =
    std::sync::Mutex::new(None);
/// 最近一次加载的结果（成功/失败），由 [`poll_load`] 在子进程退出时写入。
static LOAD_MSG: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// 每帧由 view 调用：仍在加载则返回描述；刚结束则落结果并刷新面板缓存。
pub fn poll_load() -> Option<String> {
    let mut g = LOAD.lock().ok()?;
    let (child, desc) = g.as_mut()?;
    match child.try_wait() {
        Ok(None) => Some(desc.clone()), // 还在跑
        Ok(Some(status)) => {
            let mut err = String::new();
            if let Some(mut e) = child.stderr.take() {
                use std::io::Read;
                let _ = e.read_to_string(&mut err);
            }
            let msg = if status.success() {
                if desc == "数据源清单" {
                    ro::invalidate_catalog();
                    "✔ 数据源清单已刷新".to_string()
                } else if desc == "CSV" {
                    format!("✔ 已导出 CSV → {}", export_root().display())
                } else if let Some(n) = desc.strip_prefix("PNG|") {
                    format!("✔ 已存图 {n}")
                } else {
                    ro::invalidate();
                    format!("✔ 已加载 {desc}")
                }
            } else {
                format!("✗ 生成失败：{}", err.lines().last().unwrap_or("(无输出)"))
            };
            if let Ok(mut m) = LOAD_MSG.lock() {
                *m = msg;
            }
            *g = None;
            None
        }
        Err(_) => {
            *g = None;
            None
        }
    }
}

pub fn load_message() -> String {
    LOAD_MSG.lock().map(|m| m.clone()).unwrap_or_default()
}

fn clear_load_message() {
    if let Ok(mut m) = LOAD_MSG.lock() {
        m.clear();
    }
}

/// `HH:MM`（UTC）。
fn valid_hm(s: &str) -> bool {
    let p: Vec<&str> = s.split(':').collect();
    p.len() == 2
        && p[0].parse::<u32>().is_ok_and(|h| h < 24)
        && p[1].len() == 2
        && p[1].parse::<u32>().is_ok_and(|m| m < 60)
}

/// 异步起 panels.py 生成面板 JSON：只 spawn 不等待，结果由 [`poll_load`] 收。
fn load(st: &TardisBoardState) -> String {
    if st.source.is_empty() || st.symbol.is_empty() || st.date.is_empty() {
        return "✗ 还没选完：管线 B → 购买数据（Tardis / Databento）或本地录制 → 标的 → 日期".into();
    }
    let types = st.avail_types();
    if !types.is_empty() && !types.contains(&st.dtype) {
        return format!("✗ {} {} 没有「{}」这类数据", st.symbol, st.date, ro::type_label(&st.dtype));
    }
    if !valid_hm(&st.start_hm) {
        return format!("✗ 起始时刻要写成 HH:MM（UTC）——现在是「{}」", st.start_hm);
    }
    if poll_load().is_some() {
        return "（上一次加载还在跑，稍候）".into();
    }
    clear_load_message();
    let out = ro::panel_path();
    let mut cmd = Command::new(venv_py());
    if let Some((k, v)) = st.root_env() {
        cmd.env(k, v);
    }
    cmd.current_dir(repo())
        .args([
            "-m",
            "factory.replay.panels",
            &st.symbol,
            &st.date,
            &st.dtype,
            "--source",
            &st.source,
            "--from",
            &st.start_hm,
            "--minutes",
            &st.minutes.to_string(),
            "--band-ticks",
            &st.band_ticks.to_string(),
            "--out",
            &out.display().to_string(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if st.compare {
        cmd.arg("--compare");
    }
    let r = cmd.spawn();
    match r {
        Ok(child) => {
            let desc = format!(
                "{} {} {} +{}min",
                if st.compare { "跨符号对比" } else { st.symbol.as_str() },
                st.date,
                st.start_hm,
                st.minutes
            );
            if let Ok(mut g) = LOAD.lock() {
                *g = Some((child, desc.clone()));
            }
            String::new() // 加载中的提示由 view 按 poll_load() 渲染
        }
        Err(e) => format!("✗ 启动失败：{e}（检查 {}）", venv_py()),
    }
}

/// 当前回放子进程（同一时刻只允许一个）。
static CHILD: std::sync::Mutex<Option<std::process::Child>> = std::sync::Mutex::new(None);

/// 回放中？（顺带回收已退出的子进程）
pub fn is_playing() -> bool {
    let Ok(mut g) = CHILD.lock() else {
        return false;
    };
    match g.as_mut() {
        Some(c) => match c.try_wait() {
            Ok(Some(_)) => {
                *g = None;
                false
            }
            Ok(None) => true,
            Err(_) => false,
        },
        None => false,
    }
}

/// 起 §8 的推流器（`--mode panel`）驱动播放头：它不喂 FS 图表、不动 ws:active_run，
/// 只按墙钟推进 `data_ts`，本面板据此裁剪图表 → 时间步进回放（docs/20 §10）。
/// 时间范围取**已加载图表的真实 x 范围**，故播放头与图上时间轴精确对齐、起播零延迟。
fn play(st: &TardisBoardState) -> String {
    let p = ro::panel();
    if !p.loaded || p.charts.is_empty() {
        return "✗ 先点「加载」出图，再回放".into();
    }
    let Some((t0, t1)) = ro::panel_time_span(&p) else {
        return "✗ 本类型的图无时间轴（如深度剖面），不支持时间步进".into();
    };
    // 从拖动位置起播；已在末尾（>99%）则回到开头，避免点了没反应。
    let frac = st.seek_pct.unwrap_or(0.0).clamp(0.0, 100.0);
    let frac = if frac > 99.0 { 0.0 } else { frac };
    let seek_ms = t0 + (t1 - t0) * (frac as f64) / 100.0;
    stop_play();
    let mut cmd = Command::new(venv_py());
    cmd.current_dir(repo())
        .args([
            "-m",
            "factory.replay.tardis_cockpit_feed",
            &p.symbol,
            &p.date,
            "--mode",
            "panel",
            "--from",
            &p.start,
            "--minutes",
            &p.minutes.to_string(),
            "--speed",
            st.speed.arg(),
            "--t0-ms",
            &format!("{}", t0 as i64),
            "--t1-ms",
            &format!("{}", t1 as i64),
            "--seek-ms",
            &format!("{}", seek_ms as i64),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Ok(url) = std::env::var("WS_REDIS_URL") {
        cmd.args(["--redis-url", &url]);
    }
    match cmd.spawn() {
        Ok(child) => {
            if let Ok(mut g) = CHILD.lock() {
                *g = Some(child);
            }
            if frac > 0.0 {
                format!("▶ 从 {frac:.0}% 回放中（{}）", st.speed)
            } else {
                format!("▶ 回放中（{}）—— 图表按时间步进显示", st.speed)
            }
        }
        Err(e) => format!("✗ 回放启动失败：{e}"),
    }
}

fn stop_play() -> String {
    let Ok(mut g) = CHILD.lock() else {
        return "✗ 内部锁失败".into();
    };
    match g.take() {
        Some(mut c) => {
            let _ = c.kill();
            let _ = c.wait();
            "■ 已停止回放（图表停在当前播放头）".into()
        }
        None => "（当前没有回放在跑）".into(),
    }
}

/// 刷新数据源清单——同样**异步**（扫盘约 0.25s，同步会顿一下）。
/// 与加载共用后台槽位：两者都改面板输入，同时跑没有意义。
/// 导出目录（与 Python 侧默认一致）。
fn export_root() -> std::path::PathBuf {
    super::paths::data_dir().join("cockpit/export")
}

/// 导 CSV：交给 `factory.replay.export` 从已加载的面板 JSON 生成（每图一表）。
fn export_csv() -> String {
    if !ro::panel().loaded {
        return "✗ 先加载面板再导出".into();
    }
    if poll_load().is_some() {
        return "（有后台任务在跑，稍候）".into();
    }
    clear_load_message();
    match Command::new(venv_py())
        .current_dir(repo())
        .args([
            "-m",
            "factory.replay.export",
            "--panel",
            &ro::panel_path().display().to_string(),
            "--out",
            &export_root().display().to_string(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => {
            if let Ok(mut g) = LOAD.lock() {
                *g = Some((child, "CSV".to_string()));
            }
            String::new()
        }
        Err(e) => format!("✗ 导出失败：{e}"),
    }
}

/// 存图：**窗口截图**（所见即所得）。
/// 不另实现一套离屏渲染器——那等于把 6 种图元的绘制逻辑写第二遍，必然与面板发散。
///
/// ⚠️ 两个坑（都踩过）：
/// 1. **不能用 `import -window <窗口名>`**：名字匹配不上时 ImageMagick 会**退化成交互式选窗**
///    并永久阻塞（实测挂死，进程一直在等鼠标点击）。必须先用 xdotool 解析出窗口 id。
/// 2. **不能同步 `.output()`**：截图挂住会连带冻死 UI（正是 §12 刚消除的问题）。
///    故走后台槽位 + `timeout` 兜底，任何情况下都不会把界面拖住。
fn export_png() -> String {
    let p = ro::panel();
    if !p.loaded {
        return "✗ 先加载面板再导出".into();
    }
    if poll_load().is_some() {
        return "（有后台任务在跑，稍候）".into();
    }
    // 解析自身窗口 id（本进程拿不到自己的 X window id；xdotool search 立即返回，不阻塞）
    let wid = Command::new("xdotool")
        .args(["search", "--name", "Flowsurface"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .last()
                .map(str::to_string)
        });
    let Some(wid) = wid.filter(|w| !w.is_empty()) else {
        return "✗ 找不到窗口（需 xdotool）".into();
    };
    let dir = export_root();
    if std::fs::create_dir_all(&dir).is_err() {
        return format!("✗ 建目录失败：{}", dir.display());
    }
    let stamp = p
        .charts
        .first()
        .and_then(|c| c.x.first().copied())
        .map(|t| t as i64)
        .unwrap_or(0);
    let name = format!(
        "{}_{}_{}_{}min_{}{}_{stamp}.png",
        p.symbol.replace([' ', '/'], ""),
        p.date,
        p.start.replace(':', ""),
        p.minutes,
        p.dtype,
        if p.compare { "_compare" } else { "" }
    );
    let out = dir.join(&name);
    clear_load_message();
    match Command::new("timeout")
        .args(["15", "import", "-window", &wid, &out.display().to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => {
            if let Ok(mut g) = LOAD.lock() {
                *g = Some((child, format!("PNG|{name}")));
            }
            String::new()
        }
        Err(e) => format!("✗ 存图失败：{e}（需 ImageMagick 的 import）"),
    }
}

fn refresh_catalog() -> String {
    if poll_load().is_some() {
        return "（有后台任务在跑，稍候）".into();
    }
    clear_load_message();
    let out = ro::catalog_path();
    match Command::new(venv_py())
        .current_dir(repo())
        .args([
            "-m",
            "factory.replay.sources",
            "--catalog",
            "--out",
            &out.display().to_string(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => {
            if let Ok(mut g) = LOAD.lock() {
                *g = Some((child, "数据源清单".to_string()));
            }
            String::new()
        }
        Err(e) => format!("✗ 刷新清单失败：{e}"),
    }
}
