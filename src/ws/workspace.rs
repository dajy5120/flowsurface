//! WealthSpring 工作区（docs/08 F6 — P1）。
//!
//! 把 cockpit 的功能按「左侧分栏」组织成 5 个固定工作区，每个 = 一个 FlowSurface layout
//! （独立 pane 树）。复用 FS 既有 `LayoutManager`；左侧工作区栏只是切 active_layout 的薄壳。
//!
//! - **官方原生**：干净 FS 热图（直连 Binance，无 WS 叠加）。
//! - **实盘**：实时图表 ∣ WealthSpring pane（mode=Live，仅实盘态显读数）。
//! - **回测**：replay 喂的 K 线 ∣ WealthSpring pane（mode=Backtest，仅回测态显读数）。
//! - **数据录制**：占位（Recorder pane 见后续 P3）。
//! - **Alpha Factory**：占位（Factory pane 见后续 P2）。
//!
//! 工作区按名幂等播种：缺哪个补哪个，不动用户已有 layout（达成对等前两者并存）。

use uuid::Uuid;

use crate::layout::{LayoutId, configuration};
use crate::modal::layout_manager::LayoutManager;
use crate::screen::dashboard::Dashboard;

/// 5 个工作区的固定名字（侧边栏顺序）。
pub const WS_OFFICIAL: &str = "官方原生";
pub const WS_LIVE: &str = "实盘";
pub const WS_BACKTEST: &str = "回测";
pub const WS_RECORDER: &str = "数据录制";
pub const WS_FACTORY: &str = "Alpha Factory";
pub const WORKSPACES: [&str; 5] = [WS_OFFICIAL, WS_LIVE, WS_BACKTEST, WS_RECORDER, WS_FACTORY];

/// 工作区在侧边栏的图标（合并进 FS 原生侧边栏，docs/08 F6 — P1）。
pub fn icon(name: &str) -> crate::style::Icon {
    use crate::style::Icon;
    match name {
        "官方原生" => Icon::BinanceLogo, // 官方直连 Binance
        "实盘" => Icon::ChartOutline,    // 实时图表 + 交易
        "回测" => Icon::Return,          // 回放/回测
        "数据录制" => Icon::Folder,      // 数据湖
        "Alpha Factory" => Icon::Star,   // alpha 因子
        _ => Icon::Layout,
    }
}

/// 每个工作区的 pane 树模板（`data::Pane` 的 JSON）。
/// 行情 pane 复用真实序列化形态（含 BinanceLinear:BTCUSDT 流），serde 负责解析→流解析由 FS 完成。
fn pane_template(name: &str) -> &'static str {
    match name {
        // 干净官方热图（无 WS 叠加）。
        "官方原生" => {
            r#"{"ShaderHeatmap":{"studies":[{"VolumeProfile":"VisibleRange"}],"stream_type":[{"Depth":{"ticker":"BinanceLinear:BTCUSDT","depth_aggr":"Client","push_freq":"ServerDefault"}},{"Trades":{"ticker":"BinanceLinear:BTCUSDT"}}],"settings":{"tick_multiply":5,"visual_config":null,"selected_basis":{"Time":"MS100"}},"indicators":["Volume"],"link_group":null}}"#
        }
        // 实盘：实时热图 ∣ WealthSpring(Live)。
        "实盘" => {
            r#"{"Split":{"axis":"Vertical","ratio":0.62,"a":{"ShaderHeatmap":{"studies":[{"VolumeProfile":"VisibleRange"}],"stream_type":[{"Depth":{"ticker":"BinanceLinear:BTCUSDT","depth_aggr":"Client","push_freq":"ServerDefault"}},{"Trades":{"ticker":"BinanceLinear:BTCUSDT"}}],"settings":{"tick_multiply":5,"visual_config":null,"selected_basis":{"Time":"MS100"}},"indicators":["Volume"],"link_group":null}},"b":{"WealthSpring":{"mode":"Live","settings":{},"link_group":null}}}}"#
        }
        // 回测：M1 K 线（回测态由 replay 喂）∣ 回测结果（收益曲线/回撤/统计）。
        "回测" => {
            r#"{"Split":{"axis":"Vertical","ratio":0.58,"a":{"KlineChart":{"layout":{"splits":[0.8],"autoscale":"CenterLatest"},"kind":"Candles","stream_type":[{"Kline":{"ticker":"BinanceLinear:BTCUSDT","timeframe":"M1"}}],"settings":{"tick_multiply":null,"visual_config":null,"selected_basis":{"Time":"M1"}},"indicators":["Volume"],"link_group":null}},"b":{"BacktestResult":{"settings":{},"link_group":null}}}}"#
        }
        // Alpha Factory 仪表盘（docs/08 F6-P2）。
        "Alpha Factory" => r#"{"Factory":{"settings":{},"link_group":null}}"#,
        // 录制驾驶舱（docs/08 F6-P3）。
        "数据录制" => r#"{"Recorder":{"settings":{},"link_group":null}}"#,
        _ => r#"{"Starter":{"link_group":null}}"#,
    }
}

/// 把模板 JSON 构成运行期 `Dashboard`（解析 `data::Pane` → configuration → from_config）。
fn dashboard_from_template(name: &str) -> Option<Dashboard> {
    let pane: data::Pane = serde_json::from_str(pane_template(name))
        .inspect_err(|e| log::error!("WS workspace `{name}` 模板解析失败: {e}"))
        .ok()?;
    let layout_id = Uuid::new_v4();
    Some(Dashboard::from_config(
        configuration(pane),
        vec![],
        layout_id,
    ))
}

/// 幂等播种：缺失的工作区按名补建（不动用户已有 layout）。返回新建数量。
pub fn ensure_seeded(manager: &mut LayoutManager) -> usize {
    let mut added = 0;
    for name in WORKSPACES {
        let exists = manager.layouts.iter().any(|l| l.id.name == name);
        if exists {
            continue;
        }
        let Some(dashboard) = dashboard_from_template(name) else {
            continue;
        };
        let id = LayoutId {
            unique: Uuid::new_v4(),
            name: name.to_string(),
        };
        manager.insert_layout(id, dashboard);
        added += 1;
    }
    if added > 0 {
        log::info!("WS workspaces: seeded {added} workspace(s)");
    }
    added
}
