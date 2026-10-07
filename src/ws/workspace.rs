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
/// 策略中心（docs/37 P1）：策略库 · 参数表单 · 一键回测 · 运行记录与概况 ∣ 回测结果。
/// 放在「回测」组首位：`Ctrl Shift 2` 直达。
pub const WS_STRATEGY: &str = "策略中心";
/// 订单流层析（docs/40）：OFMS-10 分层时间轴 · 事件因果链 · 价格响应 · 特征字典（按层）。
pub const WS_OFMS: &str = "订单流层析";
pub const WS_RECORDER: &str = "数据录制";
pub const WS_FACTORY: &str = "Alpha Factory";
pub const WS_C4: &str = "C4 影子"; // maker 影子守护实时/影子日/活体vs重放（docs/14 §2）
pub const WS_OPTIONS: &str = "期权/0DTE"; // 期权回测·探针面板（docs/18）
pub const WS_PREDICTION: &str = "预测市场"; // Polymarket 决策支持面板（docs/19）
pub const WS_TARDIS: &str = "Tardis 历史回放"; // 已购 30 天逐笔变速回放（docs/20 Phase 5）
pub const WS_FEATURES: &str = "订单流特征"; // 特征矩阵 + 引擎同源的 Footprint/热图/Ladder/Tape（docs/31）
pub const WS_GLOBAL: &str = "全球市场"; // 全市场雷达 + 树图（docs/22）
pub const WS_OBSERVATORY: &str = "接口观察终端"; // REST/WS/TCP/FIX 统一观察与录制（docs/23）
pub const WS_EGRESS: &str = "网络出口"; // 谁在往外发包 + 手动启停（一页看全）
pub const WS_NEWS: &str = "新闻资讯"; // 交易所/监管/媒体统一时间线（docs/25）
pub const WS_PROCS: &str = "进程"; // 常驻单元状态与启停（docs/26 S4）——已并入「资源」
/// 资源（docs/35 批 7，UPDS V7 §60 资源可见性）：「进程」与「网络出口」合成一个工作区，
/// 左右两栏——常驻单元的状态与启停、谁在往外发包与流量。两页原本各占一个工作区，
/// 看「这台机器在花什么」要来回切；UPDS 要求资源一处看全。
pub const WS_RESOURCES: &str = "资源";
/// 侧栏的**六组**（docs/41 §2，2026-10-07 用户指定顺序）。
///
/// 此前是五组（docs/35 §5.3，按 UPDS 应用模式分组）。docs/41 引入「工作区内的多个页面」后，
/// 用户重新指定了分组与顺序；组名显示在侧栏里，组首工作区有 Ctrl Shift 1–6 直达：
///
/// | 组 | 工作区 |
/// |---|---|
/// | 图表 | 官方原生 |
/// | 资讯 | 新闻资讯 · 全球市场 |
/// | 数据 | 数据录制 · Tardis 历史回放 · 接口观察终端 |
/// | 研究 | 订单流特征 · 订单流层析 · 策略中心 · Alpha Factory · C4 影子 · 期权/0DTE · 预测市场 |
/// | 回测 | 回测 · 实时数据回测 |
/// | 系统 | 资源 |
///
/// 改分组只改这里；[`WORKSPACES`] 必须是它按顺序摊平的结果（有测试钉住）。
pub const GROUPS: [(&str, &[&str]); 6] = [
    ("图表", &[WS_OFFICIAL]),
    ("资讯", &[WS_NEWS, WS_GLOBAL]),
    ("数据", &[WS_RECORDER, WS_TARDIS, WS_OBSERVATORY]),
    ("研究", &[WS_FEATURES, WS_OFMS, WS_STRATEGY, WS_FACTORY, WS_C4, WS_OPTIONS, WS_PREDICTION]),
    ("回测", &[WS_BACKTEST, WS_LIVE]),
    ("系统", &[WS_RESOURCES]),
];

/// **侧边栏从上到下的顺序**（`main.rs` 按它取 layout）= [`GROUPS`] 按顺序摊平。
///
/// **加/删项要同时改 `GROUPS`、`icon()` 与 `pane_template()` 的 match 臂**，
/// 漏了会落到 `_ => Starter` 兜底（有测试钉住）。
pub const WORKSPACES: [&str; 16] = [
    WS_OFFICIAL,
    WS_NEWS,
    WS_GLOBAL,
    WS_RECORDER,
    WS_TARDIS,
    WS_OBSERVATORY,
    WS_FEATURES,
    WS_OFMS,
    WS_STRATEGY,
    WS_FACTORY,
    WS_C4,
    WS_OPTIONS,
    WS_PREDICTION,
    WS_BACKTEST,
    WS_LIVE,
    WS_RESOURCES,
];

// ── 工作区内的页面（docs/41 §3）──────────────────────────────────────

/// 页面布局名的分隔符：第一页的布局名就是工作区名（旧存档原样是第一页），其余页是「工作区｜页名」。
pub const PAGE_SEP: char = '｜';

/// 每个工作区的默认页面（模板）。第一项是**第一页的页签名**（布局名仍是工作区名）。
/// 没列的工作区只有一页（不显示页签栏）。
///
/// 2026-10-07 用户逐个指定（docs/41 §3.2）：面板内部原有的视图 / 分节拆成页面，去掉「总览」页。
/// 同一面板出现在同一工作区的几页里时，靠 `Settings.view` 锁定各自显示哪个视图（§3.3）；
/// 几页不会同时显示，共用面板的其余状态正好一致。
pub const PAGES: [(&str, &[&str]); 11] = [
    (WS_NEWS, &["新闻", "订阅与检索", "源管理"]),
    (WS_GLOBAL, &["热图", "筛选器", "全球总览", "市场宽度", "加密全景", "预测市场", "股票全景", "宏观新闻"]),
    (WS_RECORDER, &["行情录制", "录制明细", "预测市场录制"]),
    (WS_TARDIS, &["历史面板", "预测市场回放", "自有数据图"]),
    (WS_FEATURES, &["特征矩阵", "图表参数", "引擎健康", "图表"]),
    (WS_OFMS, &["层析时间轴", "响应表", "入场形态矩阵", "特征字典"]),
    (WS_STRATEGY, &["策略库", "七层"]),
    (WS_FACTORY, &["Alpha Factory", "特征库"]),
    (WS_PREDICTION, &["币安钱包", "Polymarket"]),
    (WS_BACKTEST, &["运行", "结果报告", "订单明细"]),
    (WS_RESOURCES, &["进程", "网络出口", "检查更新"]),
];

/// 一个面板锁定在某个视图（`Settings.view`）的模板。
fn locked(kind: &str, view: &str) -> String {
    format!(r#"{{"{kind}":{{"settings":{{"view":"{view}"}},"link_group":null}}}}"#)
}

/// 页面模板（含各工作区的第一页）。不在这里的布局名回落到 [`pane_template_base`]。
fn page_template(layout: &str) -> Option<String> {
    let ws = workspace_of(layout);
    let title = page_title(layout);
    let t = title.as_str();
    Some(match ws {
        WS_NEWS => locked("News", t),
        WS_GLOBAL => locked("MarketMap", t),
        WS_RECORDER => locked("Recorder", t),
        WS_OFMS => locked("OfmsLab", t),
        WS_FEATURES if t != "图表" => locked("FeatureMatrix", t),
        WS_STRATEGY if t == "七层" => locked("StrategyLayers", t),
        // 策略库：左策略库 / 详情 / 运行记录 ∣ 右回测结果（七层是单独一页）
        WS_STRATEGY => r#"{"Split":{"axis":"Vertical","ratio":0.64,"a":{"StrategyCenter":{"settings":{},"link_group":null}},"b":{"BacktestResult":{"settings":{},"link_group":null}}}}"#.to_string(),
        WS_FACTORY if t == "特征库" => locked("FeatureLab", t),
        WS_FACTORY => locked("Factory", t),
        WS_PREDICTION if t == "Polymarket" => locked("PredictionBoard", t),
        WS_PREDICTION => locked("PmBinance", t),
        WS_TARDIS if t == "历史面板" => locked("TardisBoard", t),
        WS_TARDIS if t == "预测市场回放" => locked("PmReplay", t),
        WS_TARDIS => format!(r#"{{"WealthSpring":{{"mode":"SelfChart","settings":{{"view":"{t}"}},"link_group":null}}}}"#),
        WS_BACKTEST if t == "结果报告" => locked("BacktestResult", t),
        WS_BACKTEST if t == "订单明细" => format!(r#"{{"WealthSpring":{{"mode":"Backtest","settings":{{"view":"{t}"}},"link_group":null}}}}"#),
        WS_RESOURCES if t == "网络出口" => locked("NetEgress", t),
        WS_RESOURCES => locked("Procs", t),
        _ => return None,
    })
}

/// 布局名属于哪个工作区（「回测｜结果报告」→「回测」）。
pub fn workspace_of(layout_name: &str) -> &str {
    layout_name.split(PAGE_SEP).next().unwrap_or(layout_name)
}

/// 布局在页签上显示的名字。
pub fn page_title(layout_name: &str) -> String {
    match layout_name.split_once(PAGE_SEP) {
        Some((_, page)) => page.to_string(),
        None => PAGES
            .iter()
            .find(|(ws, _)| *ws == layout_name)
            .and_then(|(_, pages)| pages.first())
            .map_or_else(|| layout_name.to_string(), |p| (*p).to_string()),
    }
}

/// 一个工作区的全部页面布局名（模板顺序）。
pub fn page_layouts(ws: &str) -> Vec<String> {
    match PAGES.iter().find(|(w, _)| *w == ws) {
        None => vec![ws.to_string()],
        Some((_, pages)) => pages
            .iter()
            .enumerate()
            .map(|(i, p)| if i == 0 { ws.to_string() } else { format!("{ws}{PAGE_SEP}{p}") })
            .collect(),
    }
}

/// 全部托管布局名（工作区 × 页面），播种与测试用。
pub fn all_layouts() -> Vec<String> {
    WORKSPACES.iter().flat_map(|w| page_layouts(w)).collect()
}

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
const MERGES: [(&str, &str); 4] = [
    ("录制数据回测", WS_BACKTEST),
    ("自有数据回测", WS_BACKTEST),
    // docs/35 批 7：进程 + 网络出口 → 资源（模板在启动时刷新成左右两栏）
    (WS_PROCS, WS_RESOURCES),
    (WS_EGRESS, WS_RESOURCES),
];

/// 工作区在侧边栏的图标（合并进 FS 原生侧边栏，docs/08 F6 — P1）。
pub fn icon(name: &str) -> crate::style::Icon {
    use crate::style::Icon;
    match name {
        WS_OFFICIAL => Icon::BinanceLogo, // 官方直连 Binance
        WS_LIVE => Icon::ChartOutline,    // 实时图表 + 交易
        WS_STRATEGY => Icon::Layout,      // 策略中心（docs/37）
        WS_BACKTEST => Icon::Return,      // 回放/重放（两个数据源合一）
        WS_RECORDER => Icon::Folder,      // 数据湖
        WS_FACTORY => Icon::Star,         // alpha 因子
        WS_OFMS => Icon::ChartOutline,    // 订单流层析（docs/40）
        WS_C4 => Icon::Checkmark,         // C4 判定进度（合格影子日）
        WS_OPTIONS => Icon::Layout,       // 期权/0DTE 回测面板
        WS_PREDICTION => Icon::Layout,    // 预测市场 Polymarket 面板
        WS_TARDIS => Icon::Return,        // 历史回放（同「录制数据回测」语义）
        WS_GLOBAL => Icon::Search,        // 全市场扫描
        WS_FEATURES => Icon::ChartOutline, // 订单流特征：矩阵 + 同源图表（docs/31）
        WS_OBSERVATORY => Icon::Search,   // 接口观察（docs/23）
        WS_RESOURCES => Icon::Cog,        // 资源：进程 + 网络出口
        WS_NEWS => Icon::Star,            // 新闻资讯（docs/25）
        _ => Icon::Layout,
    }
}

/// 每个工作区的 pane 树模板（`data::Pane` 的 JSON）。
/// 行情 pane 复用真实序列化形态（含 BinanceLinear:BTCUSDT 流），serde 负责解析→流解析由 FS 完成。
fn pane_template(name: &str) -> String {
    page_template(name).unwrap_or_else(|| pane_template_base(name).to_string())
}

/// 各工作区原来的单页模板（页面表没覆盖到的工作区与页仍用它）。
fn pane_template_base(name: &str) -> &'static str {
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
        // 策略中心：上 = 左 策略库/详情/运行记录 · 右 回测结果（点运行记录会把它钉在那次运行上）；
        // 下 = 七层（docs/39，跟随选中的策略与运行）
        WS_STRATEGY => {
            r#"{"Split":{"axis":"Horizontal","ratio":0.62,"a":{"Split":{"axis":"Vertical","ratio":0.64,"a":{"StrategyCenter":{"settings":{},"link_group":null}},"b":{"BacktestResult":{"settings":{},"link_group":null}}}},"b":{"StrategyLayers":{"settings":{},"link_group":null}}}}"#
        }
        // 订单流层析（docs/40）：单面板——数据选择、层析时间轴与因果链都在里面。
        WS_OFMS => r#"{"OfmsLab":{"settings":{},"link_group":null}}"#,
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
        // 订单流特征（docs/31）：左特征矩阵，右四张图——**全部由特征引擎的
        // 图表事件流驱动**（`ws::feature_feed` 读 feature_chart.jsonl），
        // **零交易所连接**。图上看到的那一笔，就是引擎算出那个值的那一笔（§8.2）。
        //
        // 四张图各答一个问题：Footprint「这一格的买卖是怎么成的」、
        // Ladder「此刻簿长什么样」、热图（含 Volume Profile）「流动性怎么迁移的」、
        // Tape「逐笔顺序」。它们都是**人用来理解特征的**，不是特征本身。
        //
        // ticker 写 BinanceLinear:BTCUSDT 是因为录制流就是它；换标的要连模板一起改
        // （feed 按面板声明的 StreamKind 原样回传，ticker 对不上则一格都不画）。
        WS_FEATURES => {
            r#"{"Split":{"axis":"Vertical","ratio":0.38,"a":{"FeatureMatrix":{"settings":{},"link_group":null}},"b":{"Split":{"axis":"Horizontal","ratio":0.58,"a":{"Split":{"axis":"Vertical","ratio":0.62,"a":{"KlineChart":{"layout":{"splits":[0.8],"autoscale":"CenterLatest"},"kind":{"Footprint":{"clusters":"BidAsk","scaling":"VisibleRange","studies":[{"Imbalance":{"threshold":200,"color_scale":null,"ignore_zeros":true}}]}},"stream_type":[{"Kline":{"ticker":"BinanceLinear:BTCUSDT","timeframe":"M1"}},{"Trades":{"ticker":"BinanceLinear:BTCUSDT"}}],"settings":{"tick_multiply":5,"visual_config":null,"selected_basis":{"Time":"M1"}},"indicators":["Volume"],"link_group":null}},"b":{"Ladder":{"stream_type":[{"Depth":{"ticker":"BinanceLinear:BTCUSDT","depth_aggr":"Client","push_freq":"ServerDefault"}}],"settings":{"tick_multiply":null,"visual_config":null,"selected_basis":null},"link_group":null}}}},"b":{"Split":{"axis":"Vertical","ratio":0.68,"a":{"ShaderHeatmap":{"studies":[{"VolumeProfile":"VisibleRange"}],"stream_type":[{"Depth":{"ticker":"BinanceLinear:BTCUSDT","depth_aggr":"Client","push_freq":"ServerDefault"}},{"Trades":{"ticker":"BinanceLinear:BTCUSDT"}}],"settings":{"tick_multiply":5,"visual_config":null,"selected_basis":{"Time":"MS100"}},"indicators":["Volume"],"link_group":null}},"b":{"TimeAndSales":{"stream_type":[{"Trades":{"ticker":"BinanceLinear:BTCUSDT"}}],"settings":{"tick_multiply":null,"visual_config":null,"selected_basis":null},"link_group":null}}}}}}}}"#
        }
        WS_GLOBAL => r#"{"MarketMap":{"settings":{},"link_group":null}}"#,
        // 接口观察终端（docs/23 P0）：**零交易所连接**——全部连接在
        // ws-observatory 守护里，这个 pane 只读快照
        WS_OBSERVATORY => r#"{"Observatory":{"settings":{},"link_group":null}}"#,
        // 网络出口总闸：**零交易所连接**——它只数 /proc 和调 systemctl。
        // 这个工作区本身要是也拉行情，那就荒唐了
        // 左：进程（常驻单元状态与启停）；右：网络出口（对外 / 内部连接与流量）
        WS_RESOURCES => r#"{"Split":{"axis":"Vertical","ratio":0.46,"a":{"Procs":{"settings":{},"link_group":null}},"b":{"NetEgress":{"settings":{},"link_group":null}}}}"#,
        // 进程（docs/26 S4）：**零交易所连接**——只调 systemctl。
        // 和「网络出口」是邻居：一个管进程、一个管出口。
        // 新闻资讯：**零交易所连接**——全部在 ws-news 守护里，这个 pane 只读快照
        WS_NEWS => r#"{"News":{"settings":{},"link_group":null}}"#,
        // ── 页面（docs/41 §3.2）：第一页是上面的组合布局，以下是单项放大 ──
        // 四张图（与第一页同一组：Footprint ∣ Ladder 在上，热图 ∣ Tape 在下）
        "订单流特征｜图表" => {
            r#"{"Split":{"axis":"Horizontal","ratio":0.58,"a":{"Split":{"axis":"Vertical","ratio":0.62,"a":{"KlineChart":{"layout":{"splits":[0.8],"autoscale":"CenterLatest"},"kind":{"Footprint":{"clusters":"BidAsk","scaling":"VisibleRange","studies":[{"Imbalance":{"threshold":200,"color_scale":null,"ignore_zeros":true}}]}},"stream_type":[{"Kline":{"ticker":"BinanceLinear:BTCUSDT","timeframe":"M1"}},{"Trades":{"ticker":"BinanceLinear:BTCUSDT"}}],"settings":{"tick_multiply":5,"visual_config":null,"selected_basis":{"Time":"M1"}},"indicators":["Volume"],"link_group":null}},"b":{"Ladder":{"stream_type":[{"Depth":{"ticker":"BinanceLinear:BTCUSDT","depth_aggr":"Client","push_freq":"ServerDefault"}}],"settings":{"tick_multiply":null,"visual_config":null,"selected_basis":null},"link_group":null}}}},"b":{"Split":{"axis":"Vertical","ratio":0.68,"a":{"ShaderHeatmap":{"studies":[{"VolumeProfile":"VisibleRange"}],"stream_type":[{"Depth":{"ticker":"BinanceLinear:BTCUSDT","depth_aggr":"Client","push_freq":"ServerDefault"}},{"Trades":{"ticker":"BinanceLinear:BTCUSDT"}}],"settings":{"tick_multiply":5,"visual_config":null,"selected_basis":{"Time":"MS100"}},"indicators":["Volume"],"link_group":null}},"b":{"TimeAndSales":{"stream_type":[{"Trades":{"ticker":"BinanceLinear:BTCUSDT"}}],"settings":{"tick_multiply":null,"visual_config":null,"selected_basis":null},"link_group":null}}}}}}"#
        }
        _ => r#"{"Starter":{"link_group":null}}"#,
    }
}

/// 把模板 JSON 构成运行期 `Dashboard`（解析 `data::Pane` → configuration → from_config）。
fn dashboard_from_template(name: &str) -> Option<Dashboard> {
    let pane: data::Pane = serde_json::from_str(&pane_template(name))
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
/// 上游缺省布局「Layout 1」（五个空 Starter 面板）是否可以清掉（docs/35 §5.3）：
/// 只有**全是空面板**、没有弹出窗口时才算遗留——用户在里面放过任何东西都保留。
fn is_leftover_default(name: &str, d: &Dashboard) -> bool {
    name == "Layout 1"
        && d.popout.is_empty()
        && d.panes.iter().all(|(_, st)| matches!(st.content, crate::screen::dashboard::pane::Content::Starter))
}

pub fn ensure_seeded(manager: &mut LayoutManager) -> usize {
    // 遗留的上游缺省布局（docs/35 §5.3）：空的就删；正在用的不删（删了会没有活动工作区）
    let active = manager.active_layout_id().map(|l| l.unique);
    let before = manager.layouts.len();
    manager
        .layouts
        .retain(|l| Some(l.id.unique) == active || !is_leftover_default(&l.id.name, &l.dashboard));
    if manager.layouts.len() < before {
        log::info!("WS workspaces: 清掉遗留的空布局「Layout 1」");
    }
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
    // 页面表改了（如去掉「总览」页）：旧的页面布局删掉，不留在页签栏上（docs/41）。
    // 正在看的那一页被删时，活动布局改到它所属工作区的第一页
    let keep = all_layouts();
    let stale: Vec<(Uuid, String)> = manager
        .layouts
        .iter()
        .filter(|l| l.id.name.contains(PAGE_SEP) && !keep.contains(&l.id.name))
        .map(|l| (l.id.unique, l.id.name.clone()))
        .collect();
    if !stale.is_empty() {
        let active = manager.active_layout_id().map(|l| l.unique);
        manager.layouts.retain(|l| !stale.iter().any(|(u, _)| *u == l.id.unique));
        if let Some((_, n)) = stale.iter().find(|(u, _)| Some(*u) == active) {
            let base = workspace_of(n).to_string();
            if let Some(u) = manager.layouts.iter().find(|l| l.id.name == base).map(|l| l.id.unique) {
                let _ = manager.set_active_layout(u);
            }
        }
        log::info!("WS workspaces: 去掉旧页面 {:?}", stale.iter().map(|(_, n)| n).collect::<Vec<_>>());
    }
    let mut added = 0;
    for name in all_layouts() {
        let name = name.as_str();
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

    #[test]
    fn 只清空的缺省布局() {
        // 未知名字的模板兜底是一个 Starter 空面板（见 pane_template 的 `_` 分支）
        let empty = dashboard_from_template("Layout 1").expect("兜底模板");
        assert!(is_leftover_default("Layout 1", &empty));
        assert!(!is_leftover_default("我的布局", &empty), "只认上游缺省名");
        let real = dashboard_from_template(WS_BACKTEST).expect("回测模板");
        assert!(!is_leftover_default("Layout 1", &real), "里面放过真面板的要保留");
    }

    /// docs/41 §2：用户指定的分组与顺序，钉死防手滑。
    #[test]
    fn 侧栏按用户指定的六组排序() {
        let names: Vec<&str> = GROUPS.iter().map(|(g, _)| *g).collect();
        assert_eq!(names, ["图表", "资讯", "数据", "研究", "回测", "系统"]);
        assert_eq!(GROUPS[3].1, [WS_FEATURES, WS_OFMS, WS_STRATEGY, WS_FACTORY, WS_C4, WS_OPTIONS, WS_PREDICTION]);
    }

    /// 页面：第一页的布局名就是工作区名（旧存档 = 第一页），其余页「工作区｜页名」；名字能双向还原。
    #[test]
    fn 页面命名与归属() {
        assert_eq!(page_layouts(WS_RESOURCES), ["资源", "资源｜网络出口", "资源｜检查更新"]);
        assert_eq!(page_title(WS_RESOURCES), "进程");
        assert!(PAGES.iter().all(|(_, p)| !p.contains(&"总览")), "用户要求去掉「总览」页");
        // 锁定视图的页面：模板里写着 view，且与面板的视图标签一致
        assert!(pane_template("新闻资讯｜源管理").contains(r#""view":"源管理""#));
        assert!(pane_template(WS_GLOBAL).contains(r#""view":"热图""#));
        assert!(pane_template("订单流层析｜特征字典").contains(r#""view":"特征字典""#));
        assert!(pane_template("资源｜检查更新").contains(r#""view":"检查更新""#));
        // 单面板的页：面板配置里都写着页名（标题栏显示页名）；多面板组合页不写（各面板保留自己的名字）
        for (ws, pages) in PAGES {
            for (i, p) in pages.iter().enumerate() {
                let name = if i == 0 { ws.to_string() } else { format!("{ws}{PAGE_SEP}{p}") };
                let tpl = pane_template(&name);
                if !tpl.contains("\"Split\"") {
                    assert!(tpl.contains(&format!(r#""view":"{p}""#)), "`{name}` 的面板没写页名：{tpl}");
                }
            }
        }
        assert_eq!(page_layouts(WS_C4), [WS_C4]);
        assert_eq!(workspace_of("回测｜结果报告"), WS_BACKTEST);
        assert_eq!(workspace_of(WS_BACKTEST), WS_BACKTEST);
        assert_eq!(page_title(WS_BACKTEST), "运行");
        assert_eq!(page_title("回测｜结果报告"), "结果报告");
        assert_eq!(page_title(WS_C4), WS_C4);
        for (ws, pages) in PAGES {
            assert!(WORKSPACES.contains(&ws), "页面表里的 `{ws}` 不是现役工作区");
            assert!(pages.len() >= 2, "`{ws}` 只有一页就不必写进 PAGES");
            assert!(!ws.contains(PAGE_SEP), "工作区名里不能有页面分隔符");
        }
        let all = all_layouts();
        let mut dedup = all.clone();
        dedup.sort();
        dedup.dedup();
        assert_eq!(all.len(), dedup.len(), "页面布局名重复");
    }

    /// 页面表改动后，旧的页面布局被清掉（「资源｜总览」之类不会残留在页签栏上）。
    #[test]
    fn 旧页面布局被清掉() {
        let mut m = LayoutManager::new();
        let dash = dashboard_from_template(WS_NEWS).expect("模板");
        m.insert_layout(LayoutId { unique: Uuid::new_v4(), name: "资源｜总览".to_string() }, dash);
        ensure_seeded(&mut m);
        assert!(m.layouts.iter().all(|l| l.id.name != "资源｜总览"));
        assert!(m.layouts.iter().any(|l| l.id.name == "资源｜检查更新"));
    }

    #[test]
    fn 侧栏顺序等于分组摊平() {
        let flat: Vec<&str> = GROUPS.iter().flat_map(|(_, ws)| ws.iter().copied()).collect();
        assert_eq!(flat, WORKSPACES.to_vec(), "WORKSPACES 必须是 GROUPS 按顺序摊平");
    }

    /// 每个工作区模板都必须是合法 `data::Pane` JSON——否则该工作区在运行期静默变空
    /// （`dashboard_from_template` 只记 error 后跳过）。加 pane 时容易写错，这里锁死。
    #[test]
    fn every_workspace_template_parses() {
        for name in all_layouts() {
            let name = name.as_str();
            let raw = pane_template(name);
            assert!(
                serde_json::from_str::<data::Pane>(&raw).is_ok(),
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
        let templates: String = all_layouts().iter().map(|n| pane_template(n)).collect();
        // 只读面板类：没有 ticker、不吃行情流，各自是一个独立用途的页面。
        for kind in [
            "Factory", "C4Shadow", "Recorder", "OptionsBoard", "PredictionBoard", "PmBinance", "PmReplay", "FeatureLab", "FeatureMatrix", "StrategyCenter", "StrategyLayers", "OfmsLab", "TardisBoard",
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
        for name in all_layouts() {
            let name = name.as_str();
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

/// 当前工作区的图表是否由**特征引擎的图表事件流**驱动（docs/31 §8.2）。
///
/// 与 [`replay_mode`] 分开而不是复用它：两者都意味着「图上的数据不来自交易所」，
/// 但来源徽标要说的话完全不同。回放态说的是「等待运行，点按钮开始跑回测」——
/// 在特征工作区里那句话是错的：这里没有回测，也没有按钮可点，
/// 数据来自引擎写的 `feature_chart.jsonl`。
///
/// 徽标模块的立身之本就是「来源一眼可见」，它自己说错来源是最不能接受的。
static FEATURE_FEED: AtomicBool = AtomicBool::new(false);

pub fn set_feature_feed(on: bool) {
    FEATURE_FEED.store(on, Ordering::Relaxed);
}

pub fn feature_feed() -> bool {
    FEATURE_FEED.load(Ordering::Relaxed)
}

pub fn set_replay_mode(on: bool) {
    REPLAY_MODE.store(on, Ordering::Relaxed);
}

pub fn replay_mode() -> bool {
    REPLAY_MODE.load(Ordering::Relaxed)
}

/// 串行化所有**碰 `REPLAY_MODE` 的测试**。
///
/// 它是进程级全局，而 `cargo test` 默认并行跑。本模块自己的测试早就靠「合成一个
/// 测试」绕开了互相踩，但那挡不住**别的模块**：`timeandsales` 的保留窗口也按
/// 这个标志分实时/回放，它的测试与这里的测试并行时会互相把标志翻掉——
/// 现象是随机失败，而两边的代码都是对的（W2 的全局分配器计数踩过同一个坑）。
///
/// 凡是要 `set_replay_mode` 的测试，先取这把锁。
#[cfg(test)]
pub(crate) fn replay_mode_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    // 锁中毒（前一个持有者 panic）不影响正确性：标志本身是原子量，
    // 这把锁只用来排队。
    LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod replay_mode_tests {
    use super::*;

    /// 合在一个测试里：`REPLAY_MODE` 是进程级全局，拆开会并行互相踩。
    #[test]
    fn 回放模式开关() {
        let _g = replay_mode_test_lock();
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

    /// docs/35 批 7：「进程」「网络出口」并入「资源」，且资源工作区里两个面板都在。
    #[test]
    fn 进程与网络出口并入资源() {
        let mut m = LayoutManager::new();
        for old in [WS_PROCS, WS_EGRESS] {
            let dash = dashboard_from_template(WS_NEWS).expect("任一模板即可");
            m.insert_layout(LayoutId { unique: Uuid::new_v4(), name: old.to_string() }, dash);
        }
        ensure_seeded(&mut m);
        let names: Vec<&str> = m.layouts.iter().map(|l| l.id.name.as_str()).collect();
        assert_eq!(names.iter().filter(|n| **n == WS_RESOURCES).count(), 1, "{names:?}");
        assert!(!names.contains(&WS_PROCS) && !names.contains(&WS_EGRESS), "不留孤儿：{names:?}");
        // docs/41：资源拆成三页——进程、网络出口、检查更新都要能到
        let tpl: String = page_layouts(WS_RESOURCES).iter().map(|n| pane_template(n)).collect();
        assert!(tpl.contains("\"Procs\"") && tpl.contains("\"NetEgress\""), "两个面板都要在");
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
        let templates: String = all_layouts().iter().map(|n| pane_template(n)).collect();
        assert!(
            templates.contains("\"SelfChart\""),
            "SelfChart 没有任何工作区模板引用它——用户加了也活不过重启"
        );
    }
}
