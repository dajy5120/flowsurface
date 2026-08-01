//! Tardis 历史面板 — 状态与交互（docs/20 §9）。
//!
//! 层次：**数据源(3) → 数据类型(8) → 图表(主图 + 该类型的衍生图)**。
//! **全程零交易所流**：不声明任何 ticker、不订阅任何实时连接；数据由主仓
//! `factory/replay/panels.py` 落成 JSON，本面板只读渲染（见 [`super::tardis_board_readout`]）。
//!
//! 三个数据接口（主仓 `factory/replay/sources.py`，产出同一套规范化列）：
//!   ① Tardis · DuckDB 仓   ② Tardis · 直读 Parquet   ③ 自录数据 · Recorder
//! 类型清单/符号/日期不在 Rust 侧重复实现扫描——读 Python 生成的 catalog JSON。

use std::process::{Command, Stdio};

use super::tardis_board_readout as ro;

fn repo() -> std::path::PathBuf {
    std::env::var("WS_REPO")
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/home/dajy".into());
            format!("{home}/dev/WealthSpring")
        })
        .into()
}

fn venv_py() -> String {
    std::env::var("WS_VENV_PY").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/dajy".into());
        format!("{home}/ws-venv/bin/python")
    })
}

/// 时长档（分钟）。窗口越长下载/重建越久，故给固定档位。
pub const MINUTES: [u32; 5] = [1, 5, 10, 30, 60];

#[derive(Clone)]
pub struct TardisBoardState {
    pub source: String,
    pub symbol: String,
    pub date: String,
    pub start_hm: String,
    pub minutes: u32,
    pub dtype: String,
    pub hint: String,
    pub busy: bool,
}

impl Default for TardisBoardState {
    fn default() -> Self {
        Self::load()
    }
}

impl TardisBoardState {
    /// 默认落在 catalog 里**真实存在**的第一组组合，开箱即可出图。
    pub fn load() -> Self {
        let cat = ro::catalog();
        let src = cat
            .sources
            .iter()
            .find(|s| s.available)
            .cloned()
            .unwrap_or_default();
        let symbol = src
            .symbols
            .iter()
            .find(|s| *s == "BTCUSDT")
            .cloned()
            .or_else(|| src.symbols.first().cloned())
            .unwrap_or_default();
        let date = src
            .dates
            .get(&symbol)
            .and_then(|d| d.first().cloned())
            .unwrap_or_default();
        let dtype = src.types.first().cloned().unwrap_or_default();
        Self {
            source: src.key.clone(),
            symbol,
            date,
            start_hm: "09:00".into(),
            minutes: 10,
            dtype,
            hint: "选 数据源 → 数据类型 → 时段，点「加载」出图（主图 + 该类型的衍生图）".into(),
            busy: false,
        }
    }

    /// 当前源在 catalog 里的条目。
    pub fn src_entry(&self) -> ro::SourceEntry {
        ro::catalog()
            .sources
            .into_iter()
            .find(|s| s.key == self.source)
            .unwrap_or_default()
    }

    /// 切源/切符号后把不合法的选择拉回该源真实有的值（避免出现空面板）。
    fn reconcile(&mut self) {
        let e = self.src_entry();
        if !e.symbols.contains(&self.symbol) {
            self.symbol = e.symbols.first().cloned().unwrap_or_default();
        }
        let dates = e.dates.get(&self.symbol).cloned().unwrap_or_default();
        if !dates.contains(&self.date) {
            self.date = dates.first().cloned().unwrap_or_default();
        }
        if !e.types.contains(&self.dtype) {
            self.dtype = e.types.first().cloned().unwrap_or_default();
        }
    }
}

#[derive(Debug, Clone)]
pub enum TardisBoardMsg {
    SourcePick(String),
    SymbolPick(String),
    DatePick(String),
    StartPick(String),
    MinutesPick(u32),
    TypePick(String),
    Load,
    RefreshCatalog,
}

pub fn hours() -> Vec<String> {
    (0..24).map(|h| format!("{h:02}:00")).collect()
}

pub fn handle(st: &mut TardisBoardState, msg: TardisBoardMsg) {
    match msg {
        TardisBoardMsg::SourcePick(s) => {
            st.source = s;
            st.reconcile();
            st.hint = format!("已切到「{}」，点「加载」刷新", st.src_entry().label);
        }
        TardisBoardMsg::SymbolPick(s) => {
            st.symbol = s;
            st.reconcile();
        }
        TardisBoardMsg::DatePick(s) => st.date = s,
        TardisBoardMsg::StartPick(s) => st.start_hm = s,
        TardisBoardMsg::MinutesPick(m) => st.minutes = m,
        TardisBoardMsg::TypePick(t) => {
            st.dtype = t;
            st.hint = "类型已切换，点「加载」出该类型的主图与衍生图".into();
        }
        TardisBoardMsg::Load => st.hint = load(st),
        TardisBoardMsg::RefreshCatalog => st.hint = refresh_catalog(),
    }
}

/// 同步调 panels.py 生成面板 JSON（窗口通常几秒内；大窗口 L2 会久一些）。
fn load(st: &TardisBoardState) -> String {
    if st.symbol.is_empty() || st.date.is_empty() || st.dtype.is_empty() {
        return "✗ 该数据源没有可用的符号/日期/类型".into();
    }
    let out = ro::panel_path();
    let r = Command::new(venv_py())
        .current_dir(repo())
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
            "--out",
            &out.display().to_string(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output();
    match r {
        Ok(o) if o.status.success() => {
            ro::invalidate();
            format!("✔ 已加载 {} {} {} +{}min", st.symbol, st.date, st.start_hm, st.minutes)
        }
        Ok(o) => {
            let e = String::from_utf8_lossy(&o.stderr);
            format!("✗ 生成失败：{}", e.lines().last().unwrap_or("(无输出)"))
        }
        Err(e) => format!("✗ 启动失败：{e}（检查 {}）", venv_py()),
    }
}

fn refresh_catalog() -> String {
    let out = ro::catalog_path();
    let r = Command::new(venv_py())
        .current_dir(repo())
        .args([
            "-m",
            "factory.replay.sources",
            "--catalog",
            "--out",
            &out.display().to_string(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match r {
        Ok(s) if s.success() => {
            ro::invalidate_catalog();
            "✔ 数据源清单已刷新".into()
        }
        _ => "✗ 刷新清单失败".into(),
    }
}
