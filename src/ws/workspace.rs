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
/// 回测（已购 Tardis 与自录数据**合一**，docs/28 §4.3）。
///
/// 合并前是「自有数据回测」「录制数据回测」两个。管线统一之后它们已经是同一件事的两个源：
/// 同一个 `BacktestSource` 契约、同一套判级口径、同一条流式喂法、同样的 result.json 与
/// tearsheet，布局也几乎一样。当前用的是哪个源由图表上的**来源徽标**显示（docs/28 §4.2），
/// 比用工作区名字区分可信——徽标说的是本次运行**实际用的**源与体检等级。
pub const WS_BACKTEST: &str = "回测";
pub const WS_RECORDER: &str = "数据录制";
pub const WS_FACTORY: &str = "Alpha Factory";
pub const WS_C4: &str = "C4 影子"; // maker 影子守护实时/影子日/活体vs重放（docs/14 §2）
pub const WS_OPTIONS: &str = "期权/0DTE"; // 期权回测·探针面板（docs/18）
pub const WS_PREDICTION: &str = "预测市场"; // Polymarket 决策支持面板（docs/19）
pub const WS_TARDIS: &str = "Tardis 历史回放"; // 已购 30 天逐笔变速回放（docs/20 Phase 5）
pub const WS_GLOBAL: &str = "全球市场"; // 全市场雷达 + 树图（docs/22）
pub const WS_OBSERVATORY: &str = "接口观察终端"; // REST/WS/TCP/FIX 统一观察与录制（docs/23）
pub const WS_EGRESS: &str = "网络出口"; // 谁在往外发包 + 手动启停（一页看全）
pub const WS_NEWS: &str = "新闻资讯"; // 交易所/监管/媒体统一时间线（docs/25）
pub const WS_PROCS: &str = "进程"; // 常驻单元状态与启停（docs/26 S4）——「网络出口」的邻居
/// **这个数组的顺序就是侧边栏从上到下的顺序**（`main.rs` 按它取 layout）。
///
/// 当前排法（2026-09-18 按用户指定）大致是「看世界 → 备数据 → 做研究 → 管机器」：
///
/// | 段 | 工作区 |
/// |---|---|
/// | 外部信息 | 新闻资讯 · 全球市场 · 官方原生 · 预测市场 · 期权/0DTE |
/// | 数据 | 数据录制 · Tardis 历史回放 · 接口观察终端 |
/// | 跑策略 | 回测 · 实时数据回测 |
/// | 研究产线 | Alpha Factory · C4 影子 |
/// | 机器自身 | 进程 · 网络出口 |
///
/// 改顺序只改这里。**加/删项要同时改 `icon()` 与 `pane_template()` 的 match 臂**，
/// 漏了会落到 `_ => Starter` 兜底（有测试钉住）。
pub const WORKSPACES: [&str; 14] = [
    // 外部信息
    WS_NEWS,
    WS_GLOBAL,
    WS_OFFICIAL,
    WS_PREDICTION,
    WS_OPTIONS,
    // 数据
    WS_RECORDER,
    WS_TARDIS,
    WS_OBSERVATORY,
    // 跑策略
    WS_BACKTEST,
    WS_LIVE,
    // 研究产线
    WS_FACTORY,
    WS_C4,
    // 机器自身
    WS_PROCS,
    WS_EGRESS,
];

/// 旧工作区名 → 新名迁移表（重命名常量后，把用户已播种的旧 layout 就地改名，不残留孤儿）。
///
/// ⚠ 这里曾有一条 `("回测", WS_SELFDATA)`——很久以前把名为「回测」的工作区迁到
/// 「自有数据回测」。合并之后 [`WS_BACKTEST`] 就叫「回测」，那条规则会**把新工作区
/// 改名走**（旧名恰好等于新名，而目标又不存在）。删掉它是本次合并的必做项。
const RENAMES: [(&str, &str); 1] = [("实盘", WS_LIVE)];

/// 多个旧工作区 → 同一个新工作区（本次合并）。
///
/// 与 [`RENAMES`] 的区别：这里是**多对一**。第一个存在的旧名就地改名，
/// 其余的**删掉**而不是留着——留着会变成侧边栏看不见、配置文件里却一直躺着的孤儿
/// （侧边栏按 `WORKSPACES` 列，不在列里就不显示，但 layout 对象还在）。
const MERGES: [(&str, &str); 2] = [("录制数据回测", WS_BACKTEST), ("自有数据回测", WS_BACKTEST)];

/// 工作区在侧边栏的图标（合并进 FS 原生侧边栏，docs/08 F6 — P1）。
pub fn icon(name: &str) -> crate::style::Icon {
    use crate::style::Icon;
    match name {
        WS_OFFICIAL => Icon::BinanceLogo, // 官方直连 Binance
        WS_LIVE => Icon::ChartOutline,    // 实时图表 + 交易
        WS_BACKTEST => Icon::Return,      // 回放/重放（两个数据源合一）
        WS_RECORDER => Icon::Folder,      // 数据湖
        WS_FACTORY => Icon::Star,         // alpha 因子
        WS_C4 => Icon::Checkmark,         // C4 判定进度（合格影子日）
        WS_OPTIONS => Icon::Layout,       // 期权/0DTE 回测面板
        WS_PREDICTION => Icon::Layout,    // 预测市场 Polymarket 面板
        WS_TARDIS => Icon::Return,        // 历史回放（同「录制数据回测」语义）
        WS_GLOBAL => Icon::Search,        // 全市场扫描
        WS_OBSERVATORY => Icon::Search,   // 接口观察（docs/23）
        WS_EGRESS => Icon::Link,          // 网络出口总闸
        WS_PROCS => Icon::Cog,            // 进程：常驻单元状态与启停
        WS_NEWS => Icon::Star,            // 新闻资讯（docs/25）
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
        // 回测：M1 K 线（replay 边跑边画，含成交 ▲▼）∣ 右侧上下：订单明细 + 回测结果 tearsheet。
        //
        // 取的是原「录制数据回测」的布局——原「自有数据回测」那边多一个 SelfChart
        // （读 CSV/JSON 的通用图），与回测无关，已挪去「Tardis 历史回放」（都是看数据的）。
        // 反过来原「录制数据回测」有订单明细而「自有数据回测」没有，合并后都有。
        WS_BACKTEST => {
            r#"{"Split":{"axis":"Vertical","ratio":0.58,"a":{"KlineChart":{"layout":{"splits":[0.8],"autoscale":"CenterLatest"},"kind":"Candles","stream_type":[{"Kline":{"ticker":"BinanceLinear:BTCUSDT","timeframe":"M1"}}],"settings":{"tick_multiply":null,"visual_config":null,"selected_basis":{"Time":"M1"}},"indicators":["Volume"],"link_group":null}},"b":{"Split":{"axis":"Horizontal","ratio":0.5,"a":{"WealthSpring":{"mode":"Backtest","settings":{},"link_group":null}},"b":{"BacktestResult":{"settings":{},"link_group":null}}}}}}"#
        }
        // Alpha Factory 仪表盘（docs/08 F6-P2）∣ 特征库（docs/30）。
        //
        // 特征库放这里而不是「预测市场」：它是**研究仪器**，与 Alpha 工厂同属
        // 「找信号」这件事，而预测市场那两页是在看市场本身。放一起也让
        // 「多重比较账」这类纪律显示紧挨着因子流水线——那正是最容易忘记它的地方。
        WS_FACTORY => {
            r#"{"Split":{"axis":"Vertical","ratio":0.42,"a":{"Factory":{"settings":{},"link_group":null}},"b":{"FeatureLab":{"settings":{},"link_group":null}}}}"#
        }
        // C4 活体影子（docs/14 §2）：守护实时 + 影子日 + 活体vs重放 + 判定进度。
        WS_C4 => r#"{"C4Shadow":{"settings":{},"link_group":null}}"#,
        // 录制驾驶舱（docs/08 F6-P3）。
        WS_RECORDER => r#"{"Recorder":{"settings":{},"link_group":null}}"#,
        // 期权/0DTE 回测·探针（docs/18）。
        WS_OPTIONS => r#"{"OptionsBoard":{"settings":{},"link_group":null}}"#,
        // 预测市场：**两个独立视图并排**，不是一个面板切换数据源。
        //
        // 左边币安钱包（BTC 5 分钟，秒级逐笔盘口），右边 Polymarket（天级决策支持）。
        // 它们只是名字里都有「预测市场」——节奏差三个数量级，信息形态也不同：
        // 盘口要竖着排档位，市场列表要横着排行。塞进同一个 pane 的结果是两边都只
        // 剩几行。左边给稍大的份额，因为两本簿的档位是这一页最占竖向空间的东西。
        //
        // **零交易所连接**：两个 pane 都只读旁路快照（pm_binance.json /
        // prediction_board.json），网络连接分别在 ws-pm-recorder 与 ws-prediction 守护里。
        WS_PREDICTION => {
            r#"{"Split":{"axis":"Vertical","ratio":0.54,"a":{"PmBinance":{"settings":{},"link_group":null}},"b":{"PredictionBoard":{"settings":{},"link_group":null}}}}"#
        }
        // Tardis 历史回放（docs/20 §9）：单一历史面板 —— 数据源(3) → 数据类型(8) → 图表。
        // **不声明任何 ticker/stream**，故本工作区零交易所连接（用户明确要求不接实时流）。
        // Tardis 历史面板 ∣ 自有数据自适应图（读 CSV/JSON）。
        // SelfChart 原在「自有数据回测」，但它与回测无关——纯展示任意二维数据。
        // 挪到这里是因为两者同属「看数据」，而托管工作区的布局每次启动按模板重置，
        // 不进模板就等于用户实际用不到（加了也活不过重启）。
        //
        // 预测市场回放是**第三个独立 pane**，不是 TardisBoard 的第四个数据源：
        // 那套的类型词汇（trades/l2/deriv/强平/BBO）是给交易所行情设计的，预测市场
        // 一个都不对应——没有成交流、没有中间价、没有资金费，有的是两本独立的簿
        // 加一个二元结局。并进去要么类型名撒谎，要么到处是空列。
        // 复用发生在下面一层：图表 JSON 契约、绘图部件、播放头裁剪都是同一套。
        //
        // 布局：左 Tardis 历史面板，右上预测市场回放，右下自有数据图。
        WS_TARDIS => {
            r#"{"Split":{"axis":"Vertical","ratio":0.56,"a":{"TardisBoard":{"settings":{},"link_group":null}},"b":{"Split":{"axis":"Horizontal","ratio":0.62,"a":{"PmReplay":{"settings":{},"link_group":null}},"b":{"WealthSpring":{"mode":"SelfChart","settings":{},"link_group":null}}}}}}"#
        }
        WS_GLOBAL => r#"{"MarketMap":{"settings":{},"link_group":null}}"#,
        // 接口观察终端（docs/23 P0）：**零交易所连接**——全部连接在
        // ws-observatory 守护里，这个 pane 只读快照
        WS_OBSERVATORY => r#"{"Observatory":{"settings":{},"link_group":null}}"#,
        // 网络出口总闸：**零交易所连接**——它只数 /proc 和调 systemctl。
        // 这个工作区本身要是也拉行情，那就荒唐了
        WS_EGRESS => r#"{"NetEgress":{"settings":{},"link_group":null}}"#,
        // 进程（docs/26 S4）：**零交易所连接**——只调 systemctl。
        // 和「网络出口」是邻居：一个管进程、一个管出口。
        WS_PROCS => r#"{"Procs":{"settings":{},"link_group":null}}"#,
        // 新闻资讯：**零交易所连接**——全部在 ws-news 守护里，这个 pane 只读快照
        WS_NEWS => r#"{"News":{"settings":{},"link_group":null}}"#,
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
    // 合并迁移：多个旧工作区 → 同一个新工作区。第一个就地改名，其余删掉。
    for (old, new) in MERGES {
        let has_new = manager.layouts.iter().any(|l| l.id.name == new);
        if let Some(l) = manager.layouts.iter_mut().find(|l| l.id.name == old) {
            if has_new {
                // 新名已存在（前一条 MERGES 已改过名）→ 这个是多余的，删掉不留孤儿。
                manager.layouts.retain(|l| l.id.name != old);
                log::info!("WS workspaces: 合并移除 `{old}`（已有 `{new}`）");
            } else {
                l.id.name = new.to_string();
                log::info!("WS workspaces: 合并 `{old}` → `{new}`");
            }
        }
    }
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

    /// **每个只读面板类的 pane 都必须有工作区入口。**
    ///
    /// 这条是补出来的：docs/26 S4 做完「进程」页——后端、渲染、pane 接线、
    /// 持久化往返测试全绿——却漏了在这里注册工作区。后果是页面**没有任何入口**：
    /// 编译过、测试过、`ContentKind::ALL` 里也有它，但侧边栏没有图标，
    /// 而 pane 选择器只在空白 pane 上出现，用户根本走不到。
    ///
    /// 「做完了但用户到不了」不会有任何一处报错——正是本项目反复栽的那类
    /// 「什么都没有 被判成 什么问题都没有」。
    ///
    /// 判据用的是 pane 模板里出现的 `ContentKind` 名字：面板类 pane（无 ticker、
    /// 只读快照）必须至少被一个工作区模板引用。行情/图表类不在此列——
    /// 它们是工作区的组成部分，不各自独占一个。
    #[test]
    fn every_panel_kind_has_a_workspace_to_reach_it() {
        let templates: String = WORKSPACES.iter().map(|n| pane_template(n)).collect();
        // 只读面板类：没有 ticker、不吃行情流，各自是一个独立用途的页面。
        for kind in [
            "Factory", "C4Shadow", "Recorder", "OptionsBoard", "PredictionBoard", "PmBinance", "PmReplay", "FeatureLab", "TardisBoard",
            "MarketMap", "Observatory", "NetEgress", "Procs", "News",
        ] {
            assert!(
                templates.contains(&format!("\"{kind}\"")),
                "面板 `{kind}` 没有任何工作区模板引用它——用户没有入口能打开这一页。\n\
                 加一个 WS_* 常量 + WORKSPACES 一项 + icon() 一臂 + pane_template() 一臂。"
            );
        }
    }

    /// 侧边栏顺序里不得有重复项。
    ///
    /// 手改顺序时最容易的两种错：漏一项、复制粘贴多一项。多的那项会在侧边栏出现两个
    /// 一样的图标（点哪个都是同一个 layout），而 `[&str; 14]` 的长度约束看起来还是满的
    /// ——因为漏和重复往往同时发生，正好抵消。
    #[test]
    fn 侧边栏顺序无重复() {
        let mut seen = std::collections::BTreeSet::new();
        for n in WORKSPACES {
            assert!(seen.insert(n), "`{n}` 在 WORKSPACES 里出现了两次");
        }
        assert_eq!(seen.len(), WORKSPACES.len());
    }

    /// 三个图表类工作区不得在重排中丢失。
    ///
    /// 只读面板类由 [`every_panel_kind_has_a_workspace_to_reach_it`] 守着，但那条判据看的是
    /// pane 模板里的 `ContentKind` 名字——图表类（`KlineChart`/`ShaderHeatmap`）不在它的
    /// 名单里（它们是工作区的组成部分，不各自独占一个），所以漏掉这三个不会被它发现。
    #[test]
    fn 图表类工作区不得在重排中丢失() {
        for n in [WS_OFFICIAL, WS_LIVE, WS_BACKTEST] {
            assert!(WORKSPACES.contains(&n), "`{n}` 不在侧边栏里——用户没有入口");
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

// ── 回放模式开关（docs/27 §12）─────────────────────────────────────────────
use std::sync::atomic::{AtomicBool, Ordering};

/// 当前工作区是否是「回放/回测」类（录制数据 / Tardis 回放 / 自有数据回测）。
///
/// **用途：禁止图表向交易所补拉历史 K 线。** 图表发现视窗里有空缺就会去 fetch，
/// 拉回来的是真实市场历史——在回测工作区里那些蜡烛与本次回测毫无关系，
/// 混在回测数据旁边看着却一模一样（docs/27 §12）。
///
/// 放进程级原子量而不是层层传参：判定点在 `dashboard` 深处，那里拿不到 layout 名字。
static REPLAY_MODE: AtomicBool = AtomicBool::new(false);

pub fn set_replay_mode(on: bool) {
    REPLAY_MODE.store(on, Ordering::Relaxed);
}

pub fn replay_mode() -> bool {
    REPLAY_MODE.load(Ordering::Relaxed)
}

#[cfg(test)]
mod replay_mode_tests {
    use super::*;

    /// 合在一个测试里：`REPLAY_MODE` 是进程级全局，拆开会并行互相踩。
    #[test]
    fn 回放模式开关() {
        // 默认关：普通看盘工作区要能正常补拉历史。
        set_replay_mode(false);
        assert!(!replay_mode());

        // 三个回放类工作区都该开——开着时图表不去交易所拉历史 K 线，
        // 否则真实市场蜡烛会混进回测数据里，长得一模一样却毫不相干。
        set_replay_mode(true);
        assert!(replay_mode());

        set_replay_mode(false);
        assert!(!replay_mode());
    }

    #[test]
    fn 回放类工作区常量齐全() {
        // 漏掉任何一个，那个工作区就会偷偷补拉历史。
        for n in [WS_BACKTEST, WS_TARDIS] {
            assert!(WORKSPACES.contains(&n), "{n} 不在 WORKSPACES 里");
        }
    }

    /// **迁移表不得把活着的工作区改名走。**
    ///
    /// 合并时真踩到过：`RENAMES` 里躺着一条 `("回测", WS_SELFDATA)`——很久以前把名为
    /// 「回测」的工作区迁去「自有数据回测」。合并后新工作区恰好就叫「回测」，
    /// 那条规则会在下次启动把它改名走，而且**不报任何错**：侧边栏少一个图标，
    /// 用户只会觉得「怎么没了」。
    #[test]
    fn 迁移表不得以现役工作区名作为源() {
        for (old, new) in RENAMES {
            assert!(
                !WORKSPACES.contains(&old),
                "RENAMES 的源 `{old}` 是现役工作区名——下次启动它会被改名成 `{new}` 而消失"
            );
        }
        for (old, _) in MERGES {
            assert!(
                !WORKSPACES.contains(&old),
                "MERGES 的源 `{old}` 是现役工作区名——它会被就地改名或删掉"
            );
        }
    }

    /// 迁移的目标必须是真实存在的工作区，否则旧 layout 被改成一个侧边栏列不出的名字，
    /// 等于**消失且带不回来**（`WORKSPACES` 里没有 = 不显示，也不会被模板刷新）。
    #[test]
    fn 迁移目标必须是现役工作区() {
        for (old, new) in RENAMES.iter().chain(MERGES.iter()) {
            assert!(
                WORKSPACES.contains(new),
                "迁移 `{old}` → `{new}`，但 `{new}` 不在 WORKSPACES 里"
            );
        }
    }

    /// 合并后不该再有「自有数据回测」「录制数据回测」这两个名字出现在现役列表里。
    #[test]
    fn 两个旧回测工作区已合并() {
        for gone in ["自有数据回测", "录制数据回测"] {
            assert!(!WORKSPACES.contains(&gone), "`{gone}` 应已并入 `{WS_BACKTEST}`");
            assert!(
                MERGES.iter().any(|(o, _)| *o == gone),
                "`{gone}` 没有迁移规则——用户机器上那个 layout 会变成看不见的孤儿"
            );
        }
    }

    /// **合并迁移真的把两个旧工作区收拢成一个，且不留孤儿。**
    ///
    /// 这条走的是 `ensure_seeded` 本身，不是对着常量表做同义反复——迁移的坑
    /// （改名 vs 删除的先后、`has_new` 的判定时机）只有跑一遍才看得出来。
    #[test]
    fn 合并迁移收拢两个旧工作区且不留孤儿() {
        let mut m = LayoutManager::new();
        for old in ["录制数据回测", "自有数据回测"] {
            let dash = dashboard_from_template(WS_BACKTEST).expect("模板应可解析");
            m.insert_layout(LayoutId { unique: Uuid::new_v4(), name: old.to_string() }, dash);
        }
        let before = m.layouts.len();
        ensure_seeded(&mut m);

        let names: Vec<&str> = m.layouts.iter().map(|l| l.id.name.as_str()).collect();
        assert_eq!(
            names.iter().filter(|n| **n == WS_BACKTEST).count(),
            1,
            "应恰好一个 `{WS_BACKTEST}`，实得 {names:?}"
        );
        for gone in ["录制数据回测", "自有数据回测"] {
            assert!(!names.contains(&gone), "`{gone}` 应已被收拢，实得 {names:?}");
        }
        // 两个旧的收拢成一个 → 净减一；其余工作区按缺补齐。
        assert!(m.layouts.len() >= before - 1);
    }

    /// 只有一个旧工作区时也要正确改名（用户可能只播种过其中一个）。
    #[test]
    fn 只存在一个旧工作区时就地改名() {
        for only in ["录制数据回测", "自有数据回测"] {
            let mut m = LayoutManager::new();
            let dash = dashboard_from_template(WS_BACKTEST).expect("模板应可解析");
            m.insert_layout(LayoutId { unique: Uuid::new_v4(), name: only.to_string() }, dash);
            ensure_seeded(&mut m);
            let names: Vec<&str> = m.layouts.iter().map(|l| l.id.name.as_str()).collect();
            assert!(!names.contains(&only), "`{only}` 应已改名");
            assert_eq!(names.iter().filter(|n| **n == WS_BACKTEST).count(), 1);
        }
    }

    /// SelfChart 与回测无关，但也不能顺手删掉——它是读 CSV/JSON 的通用图。
    /// 托管工作区的布局每次启动按模板重置，所以「不进模板」= 用户实际用不到
    /// （手动加了也活不过重启）。
    #[test]
    fn selfchart_仍有工作区容身() {
        let templates: String = WORKSPACES.iter().map(|n| pane_template(n)).collect();
        assert!(
            templates.contains("\"SelfChart\""),
            "SelfChart 没有任何工作区模板引用它——用户加了也活不过重启"
        );
    }
}
