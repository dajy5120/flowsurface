//! 录制驾驶舱 pane 的可编辑状态 + 交互逻辑（docs/08 F6 — P3）。
//!
//! 把独立 `wealthspring-recorder-gui`（24/7 守护录制控制中心）移植为 FlowSurface 原生
//! pane `Content::Recorder` 的**交互**部分:服务启停/重启、改保存位置 + 币种/档位、写
//! `recorder.toml`。只读的服务/数据状态见 [`super::recorder_readout`];渲染见 `recorder_view`。
//!
//! 不依赖主仓 `wealthspring-recorder`(避免子模块循环):recorder.toml 简单稳定,内联手解析/写出。
//! systemctl 动作内联同步执行(start/stop 很快,同 recorder-gui)。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

pub const SERVICE: &str = "wealthspring-recorder";
pub const GOAL_DAYS: i64 = 30;
pub const PRESETS: [&str; 10] = [
    "BTCUSDT", "ETHUSDT", "SOLUSDT", "BNBUSDT", "XRPUSDT", "DOGEUSDT", "ADAUSDT", "AVAXUSDT",
    "LINKUSDT", "LTCUSDT",
];

pub fn toml_path() -> String {
    std::env::var("WS_RECORDER_TOML").unwrap_or_else(|_| {
        "/home/dajy/dev/WealthSpring/crates/wealthspring-recorder/recorder.toml".to_string()
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TierOpt {
    Full,
    Light,
}
impl std::fmt::Display for TierOpt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            TierOpt::Full => "全 L2",
            TierOpt::Light => "轻量",
        })
    }
}
impl TierOpt {
    pub const ALL: [TierOpt; 2] = [TierOpt::Full, TierOpt::Light];
}

#[derive(Clone)]
pub struct SymSel {
    pub enabled: bool,
    pub tier: TierOpt,
}

/// pane 携带的可编辑状态(从 recorder.toml 初始化)。
#[derive(Clone)]
pub struct RecorderPaneState {
    pub data_dir: String,
    pub syms: BTreeMap<String, SymSel>,
    pub custom: String,
    pub hint: String,
}

impl Default for RecorderPaneState {
    fn default() -> Self {
        Self::load()
    }
}

impl RecorderPaneState {
    /// 从 recorder.toml 读现有配置(保存位置 + 币种/档位);读不到则给预设默认。
    pub fn load() -> Self {
        let mut data_dir = "~/ws-data".to_string();
        let mut syms: BTreeMap<String, SymSel> = PRESETS
            .iter()
            .map(|s| (s.to_string(), SymSel { enabled: false, tier: TierOpt::Light }))
            .collect();
        if let Ok(content) = std::fs::read_to_string(toml_path()) {
            let (dd, parsed) = parse_toml(&content);
            if let Some(dd) = dd {
                data_dir = dd;
            }
            for (name, tier) in parsed {
                syms.entry(name)
                    .and_modify(|e| {
                        e.enabled = true;
                        e.tier = tier;
                    })
                    .or_insert(SymSel { enabled: true, tier });
            }
        }
        Self {
            data_dir,
            syms,
            custom: String::new(),
            hint: "24/7 守护录制:启停服务、改保存位置/币种、看已录总览".into(),
        }
    }
}

/// 录制 pane 的交互消息(view 发出 → pane.update 路由到 [`handle`])。
#[derive(Debug, Clone)]
pub enum RecorderMsg {
    DataDir(String),
    ToggleSym(String, bool),
    TierPick(String, TierOpt),
    CustomInput(String),
    AddCustom,
    Start,
    Stop,
    Restart,
    ApplyConfig,
}

/// 处理一条交互:改状态 + 必要的副作用(systemctl / 写 toml)。
pub fn handle(st: &mut RecorderPaneState, msg: RecorderMsg) {
    match msg {
        RecorderMsg::DataDir(s) => st.data_dir = s,
        RecorderMsg::ToggleSym(s, on) => {
            if let Some(e) = st.syms.get_mut(&s) {
                e.enabled = on;
            }
        }
        RecorderMsg::TierPick(s, t) => {
            if let Some(e) = st.syms.get_mut(&s) {
                e.tier = t;
            }
        }
        RecorderMsg::CustomInput(s) => st.custom = s.to_uppercase(),
        RecorderMsg::AddCustom => {
            let s = st.custom.trim().to_uppercase();
            if !s.is_empty() && !st.syms.contains_key(&s) {
                st.syms.insert(s, SymSel { enabled: true, tier: TierOpt::Light });
            }
            st.custom.clear();
        }
        RecorderMsg::Start => st.hint = svc_action("start"),
        RecorderMsg::Stop => st.hint = svc_action("stop"),
        RecorderMsg::Restart => st.hint = svc_action("restart"),
        RecorderMsg::ApplyConfig => st.hint = apply_config(st),
    }
}

fn expand(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix('~')
        && let Ok(home) = std::env::var("HOME")
    {
        return PathBuf::from(home).join(rest.trim_start_matches('/'));
    }
    PathBuf::from(s)
}

/// 当前 recorder.toml 里的 data_dir(poller 扫数据湖用);读不到给默认。
pub fn config_data_dir() -> String {
    std::fs::read_to_string(toml_path())
        .ok()
        .and_then(|c| parse_toml(&c).0)
        .unwrap_or_else(|| "~/ws-data".into())
}

/// 展开 `~` 前缀为绝对路径(供 poller 扫数据湖)。
pub fn expand_dir(s: &str) -> PathBuf {
    expand(s)
}

pub fn is_active() -> bool {
    Command::new("systemctl")
        .args(["--user", "is-active", SERVICE])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "active")
        .unwrap_or(false)
}

fn svc_action(action: &str) -> String {
    match Command::new("systemctl").args(["--user", action, SERVICE]).status() {
        Ok(s) if s.success() => format!("✔ 24/7 服务已{}", zh(action)),
        Ok(s) => format!("✗ systemctl {action} 退出码 {:?}", s.code()),
        Err(e) => format!("✗ systemctl 失败: {e}"),
    }
}
fn zh(a: &str) -> &'static str {
    match a {
        "start" => "启动",
        "stop" => "停止",
        "restart" => "重启",
        _ => "操作",
    }
}

/// 改写 recorder.toml(保存位置 + 选中币种/档位),运行中则重启生效。
fn apply_config(st: &RecorderPaneState) -> String {
    if st.data_dir.trim().is_empty() || expand(&st.data_dir).starts_with("/tmp") {
        return "✗ 保存位置非法(空 / /tmp 易失目录)".into();
    }
    let chosen: Vec<(&String, &SymSel)> = st.syms.iter().filter(|(_, v)| v.enabled).collect();
    if chosen.is_empty() {
        return "✗ 至少选一个币种".into();
    }
    let mut toml = format!(
        "# WealthSpring Recorder 配置(录制驾驶舱写入)\ndata_dir = \"{}\"\nrotate_secs = 600\nflush_secs = 2\n",
        st.data_dir
    );
    for (name, sel) in chosen {
        let tier = if sel.tier == TierOpt::Full { "full" } else { "light" };
        toml.push_str(&format!("\n[[symbols]]\nname = \"{name}\"\ntier = \"{tier}\"\n"));
    }
    if let Err(e) = std::fs::write(toml_path(), toml) {
        return format!("✗ 写 recorder.toml 失败: {e}");
    }
    if is_active() {
        let _ = Command::new("systemctl").args(["--user", "restart", SERVICE]).status();
        "✔ 配置已写入 recorder.toml,服务已重启生效".into()
    } else {
        "✔ 配置已写入 recorder.toml(服务未运行,点「启动」生效)".into()
    }
}

/// 极简手解析 recorder.toml:取 `data_dir` + 各 `[[symbols]]` 的 name/tier。
fn parse_toml(content: &str) -> (Option<String>, Vec<(String, TierOpt)>) {
    let mut data_dir = None;
    let mut syms = Vec::new();
    let mut cur_name: Option<String> = None;
    let mut cur_tier = TierOpt::Light;
    let mut in_sym = false;
    let flush = |syms: &mut Vec<(String, TierOpt)>, name: &mut Option<String>, tier: TierOpt| {
        if let Some(n) = name.take() {
            syms.push((n.to_uppercase(), tier));
        }
    };
    for raw in content.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line == "[[symbols]]" {
            flush(&mut syms, &mut cur_name, cur_tier);
            in_sym = true;
            cur_tier = TierOpt::Light;
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let key = k.trim();
            let val = v.trim().trim_matches('"').to_string();
            match key {
                "data_dir" if !in_sym => data_dir = Some(val),
                "name" if in_sym => cur_name = Some(val),
                "tier" if in_sym => {
                    cur_tier = if val == "full" { TierOpt::Full } else { TierOpt::Light };
                }
                _ => {}
            }
        }
    }
    flush(&mut syms, &mut cur_name, cur_tier);
    (data_dir, syms)
}
