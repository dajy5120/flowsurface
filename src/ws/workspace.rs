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

/// 工作区固定名字（侧边栏顺序）。三种「回测」按数据来源区分：自有 / 录制 / 实时。
pub const WS_OFFICIAL: &str = "官方原生";
pub const WS_LIVE: &str = "实时数据回测"; // 实时 Binance 行情 + Sandbox/live
pub const WS_SELFDATA: &str = "自有数据回测"; // 脚本自带数据（result.json）
pub const WS_RECORDED: &str = "录制数据回测"; // 录制历史 ~/ws-data（replay）
pub const WS_RECORDER: &str = "数据录制";
pub const WS_FACTORY: &str = "Alpha Factory";
pub const WS_C4: &str = "C4 影子"; // maker 影子守护实时/影子日/活体vs重放（docs/14 §2）
pub const WS_OPTIONS: &str = "期权/0DTE"; // 期权回测·探针面板（docs/18）
pub const WS_PREDICTION: &str = "预测市场"; // Polymarket 决策支持面板（docs/19）
pub const WS_TARDIS: &str = "Tardis 历史回放"; // 已购 30 天逐笔变速回放（docs/20 Phase 5）
pub const WS_GLOBAL: &str = "全球市场"; // 全市场雷达 + 树图（docs/22）
pub const WORKSPACES: [&str; 11] = [
    WS_OFFICIAL, WS_LIVE, WS_RECORDED, WS_SELFDATA, WS_RECORDER, WS_FACTORY, WS_C4, WS_OPTIONS,
    WS_PREDICTION, WS_TARDIS, WS_GLOBAL,
];

/// 旧工作区名 → 新名迁移表（重命名常量后，把用户已播种的旧 layout 就地改名，不残留孤儿）。
const RENAMES: [(&str, &str); 2] = [("实盘", WS_LIVE), ("回测", WS_SELFDATA)];

/// 工作区在侧边栏的图标（合并进 FS 原生侧边栏，docs/08 F6 — P1）。
pub fn icon(name: &str) -> crate::style::Icon {
    use crate::style::Icon;
    match name {
        WS_OFFICIAL => Icon::BinanceLogo, // 官方直连 Binance
        WS_LIVE => Icon::ChartOutline,    // 实时图表 + 交易
        WS_RECORDED => Icon::Return,      // 回放/重放
        WS_SELFDATA => Icon::Layout,      // 自有数据回测
        WS_RECORDER => Icon::Folder,      // 数据湖
        WS_FACTORY => Icon::Star,         // alpha 因子
        WS_C4 => Icon::Checkmark,         // C4 判定进度（合格影子日）
        WS_OPTIONS => Icon::Layout,       // 期权/0DTE 回测面板
        WS_PREDICTION => Icon::Layout,    // 预测市场 Polymarket 面板
        WS_TARDIS => Icon::Return,        // 历史回放（同「录制数据回测」语义）
        WS_GLOBAL => Icon::Search,        // 全市场扫描
        _ => Icon::Layout,
    }
}

/// 每个工作区的 pane 树模板（`data::Pane` 的 JSON）。
/// 行情 pane 复用真实序列化形态（含 BinanceLinear:BTCUSDT 流），serde 负责解析→流解析由 FS 完成。
fn pane_template(name: &str) -> &'static str {
    match name {
        // 干净官方热图（无 WS 叠加）。
        WS_OFFICIAL => {
            r#"{"ShaderHeatmap":{"studies":[{"VolumeProfile":"VisibleRange"}],"stream_type":[{"Depth":{"ticker":"BinanceLinear:BTCUSDT","depth_aggr":"Client","push_freq":"ServerDefault"}},{"Trades":{"ticker":"BinanceLinear:BTCUSDT"}}],"settings":{"tick_multiply":5,"visual_config":null,"selected_basis":{"Time":"MS100"}},"indicators":["Volume"],"link_group":null}}"#
        }
        // 实时数据回测：实时热图 ∣ WealthSpring(Live)。
        WS_LIVE => {
            r#"{"Split":{"axis":"Vertical","ratio":0.62,"a":{"ShaderHeatmap":{"studies":[{"VolumeProfile":"VisibleRange"}],"stream_type":[{"Depth":{"ticker":"BinanceLinear:BTCUSDT","depth_aggr":"Client","push_freq":"ServerDefault"}},{"Trades":{"ticker":"BinanceLinear:BTCUSDT"}}],"settings":{"tick_multiply":5,"visual_config":null,"selected_basis":{"Time":"MS100"}},"indicators":["Volume"],"link_group":null}},"b":{"WealthSpring":{"mode":"Live","settings":{},"link_group":null}}}}"#
        }
        // 录制数据回测：M1 K 线（录制 replay 喂，含成交 ▲▼）∣ 右侧上下：订单明细(Backtest) + 回测结果 tearsheet。
        WS_RECORDED => {
            r#"{"Split":{"axis":"Vertical","ratio":0.58,"a":{"KlineChart":{"layout":{"splits":[0.8],"autoscale":"CenterLatest"},"kind":"Candles","stream_type":[{"Kline":{"ticker":"BinanceLinear:BTCUSDT","timeframe":"M1"}}],"settings":{"tick_multiply":null,"visual_config":null,"selected_basis":{"Time":"M1"}},"indicators":["Volume"],"link_group":null}},"b":{"Split":{"axis":"Horizontal","ratio":0.5,"a":{"WealthSpring":{"mode":"Backtest","settings":{},"link_group":null}},"b":{"BacktestResult":{"settings":{},"link_group":null}}}}}}"#
        }
        // 自有数据回测：左侧上下两面板——上 自有数据自适应图（CSV/JSON）+ 下 result.json→K 线
        // （selfdata 桥，含成交 ▲▼），两种测试方式并存；右侧 回测结果 tearsheet。
        WS_SELFDATA => {
            r#"{"Split":{"axis":"Vertical","ratio":0.58,"a":{"Split":{"axis":"Horizontal","ratio":0.5,"a":{"WealthSpring":{"mode":"SelfChart","settings":{},"link_group":null}},"b":{"KlineChart":{"layout":{"splits":[0.8],"autoscale":"CenterLatest"},"kind":"Candles","stream_type":[{"Kline":{"ticker":"BinanceLinear:BTCUSDT","timeframe":"M1"}}],"settings":{"tick_multiply":null,"visual_config":null,"selected_basis":{"Time":"M1"}},"indicators":["Volume"],"link_group":null}}}},"b":{"BacktestResult":{"settings":{},"link_group":null}}}}"#
        }
        // Alpha Factory 仪表盘（docs/08 F6-P2）。
        WS_FACTORY => r#"{"Factory":{"settings":{},"link_group":null}}"#,
        // C4 活体影子（docs/14 §2）：守护实时 + 影子日 + 活体vs重放 + 判定进度。
        WS_C4 => r#"{"C4Shadow":{"settings":{},"link_group":null}}"#,
        // 录制驾驶舱（docs/08 F6-P3）。
        WS_RECORDER => r#"{"Recorder":{"settings":{},"link_group":null}}"#,
        // 期权/0DTE 回测·探针（docs/18）。
        WS_OPTIONS => r#"{"OptionsBoard":{"settings":{},"link_group":null}}"#,
        // 预测市场 Polymarket（docs/19）。
        WS_PREDICTION => r#"{"PredictionBoard":{"settings":{},"link_group":null}}"#,
        // Tardis 历史回放（docs/20 §9）：单一历史面板 —— 数据源(3) → 数据类型(8) → 图表。
        // **不声明任何 ticker/stream**，故本工作区零交易所连接（用户明确要求不接实时流）。
        WS_TARDIS => r#"{"TardisBoard":{"settings":{},"link_group":null}}"#,
        WS_GLOBAL => r#"{"MarketMap":{"settings":{},"link_group":null}}"#,
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

/// 播种 + 刷新管理工作区：这 6 个是固定用途工作区（其 pane 树由模板定义），每次启动按当前
/// 模板**就地刷新内容**（保留 LayoutId.unique，激活态不丢），使模板更新重启即生效。
/// 缺失则新建；非管理的用户 layout 一律不动。返回新建数量。
pub fn ensure_seeded(manager: &mut LayoutManager) -> usize {
    // 迁移：把旧名工作区改名到新名（避免新旧并存）。
    for (old, new) in RENAMES {
        let has_new = manager.layouts.iter().any(|l| l.id.name == new);
        if has_new {
            continue;
        }
        if let Some(l) = manager.layouts.iter_mut().find(|l| l.id.name == old) {
            l.id.name = new.to_string();
            log::info!("WS workspaces: 迁移 `{old}` → `{new}`");
        }
    }
    let mut added = 0;
    for name in WORKSPACES {
        let Some(dashboard) = dashboard_from_template(name) else {
            continue;
        };
        if let Some(l) = manager.layouts.iter_mut().find(|l| l.id.name == name) {
            l.dashboard = dashboard; // 刷新到当前模板（覆盖旧内容）
        } else {
            let id = LayoutId { unique: Uuid::new_v4(), name: name.to_string() };
            manager.insert_layout(id, dashboard);
            added += 1;
        }
    }
    log::info!("WS workspaces: 已刷新模板（新建 {added}）");
    added
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个工作区模板都必须是合法 `data::Pane` JSON——否则该工作区在运行期静默变空
    /// （`dashboard_from_template` 只记 error 后跳过）。加 pane 时容易写错，这里锁死。
    #[test]
    fn every_workspace_template_parses() {
        for name in WORKSPACES {
            let raw = pane_template(name);
            assert!(
                serde_json::from_str::<data::Pane>(raw).is_ok(),
                "工作区 `{name}` 模板不是合法 data::Pane: {raw}"
            );
        }
    }

    /// 模板不得落到 `_ => Starter` 兜底（写错常量名/漏加 match 臂的典型症状）。
    #[test]
    fn no_workspace_falls_back_to_starter() {
        for name in WORKSPACES {
            assert!(
                !pane_template(name).contains("Starter"),
                "工作区 `{name}` 落到了 Starter 兜底模板"
            );
        }
    }
}
