//! 检查器里的面板设置（docs/35 §16.5 第 3 项，docs/42）。
//!
//! **主区始终减负**（docs/42，用户定）：面板的设置、数据选择、守护控制与长说明一律只在检查器里——
//! 「属性」[`view`]、「数据」[`data_view`]、「说明」[`about`]；检查器关着时，顶部工具栏有「⚙ 设置」入口，
//! 改过设置的写「⚙ 已改 n 项」（[`changed`]）。
//!
//! # 消息怎么回去
//!
//! 检查器里的控件发的是**和面板里一模一样的 pane 事件**（`Event::FeatureMatrixInteraction`、
//! `Event::BacktestLaunchInteraction` ……），由 main 包成 `Dashboard → Pane → PaneEvent(聚焦的
//! pane, 事件)` 送回原来的处理函数。所以副作用（清图、横滚表头、写配置重启引擎）完全不变，
//! 这里不另写一套处理逻辑。

use iced::widget::{column, text};
use iced::{Element, Length};

use crate::screen::dashboard::pane::{Content, Event, State};

// ── 渲染部位（docs/42 第 4 期）──────────────────────────────────────
//
// 大多数面板的视图是一个函数从上往下推：标题与长说明 → 守护 / 运行控制与数据状态 → 筛选与显示设置 → 内容。
// 同一个函数按「部位」各画一遍：主区只画内容与状态，检查器「属性」页只画设置、「数据」页只画
// 数据与守护——**不复制任何视图逻辑**，状态规范化（如资产类换了回退口径）只有一份。
// 部位放在线程局部里（视图在 UI 线程里同步构建），面板视图用 [`part`] 读，不用改每个函数的签名。

/// 面板视图这一次画的是哪一部分。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// 全部内联（原来的样子）。主区始终减负之后，只是构建视图之前的缺省值
    Inline,
    /// 主区：内容、状态行、与钱和结论有关的警示
    Main,
    /// 检查器「属性」页：设置
    Props,
    /// 检查器「数据」页：数据源、守护、数据状态
    Data,
}

impl Part {
    pub fn of(hosted: bool) -> Self {
        if hosted { Part::Main } else { Part::Inline }
    }
    /// 标题与长说明（托管时进「说明」页）
    pub fn intro(self) -> bool {
        self == Part::Inline
    }
    /// 内容与状态
    pub fn main(self) -> bool {
        matches!(self, Part::Inline | Part::Main)
    }
    pub fn props(self) -> bool {
        matches!(self, Part::Inline | Part::Props)
    }
    pub fn data(self) -> bool {
        matches!(self, Part::Inline | Part::Data)
    }
    /// 正在画进检查器：画完设置 / 数据就该返回，不画内容
    pub fn side(self) -> bool {
        matches!(self, Part::Props | Part::Data)
    }
}

thread_local! {
    static PART: std::cell::Cell<Part> = const { std::cell::Cell::new(Part::Inline) };
}

/// 当前在画的部位（面板视图里读）。
pub fn part() -> Part {
    PART.with(std::cell::Cell::get)
}

/// 以部位 `p` 构建视图（构建完恢复原值）。
pub fn with_part<T>(p: Part, f: impl FnOnce() -> T) -> T {
    let old = PART.with(|c| c.replace(p));
    let r = f();
    PART.with(|c| c.set(old));
    r
}

/// 在作用域内把部位设成 `p`，离开作用域恢复（面板视图函数开头用）。
#[must_use]
pub fn scope(p: Part) -> PartScope {
    PartScope(PART.with(|c| c.replace(p)))
}

pub struct PartScope(Part);

impl Drop for PartScope {
    fn drop(&mut self) {
        PART.with(|c| c.set(self.0));
    }
}

/// 检查器「属性」页：选中面板的可编辑设置。没有的面板返回 `None`。
///
/// 返回的元素发 pane 事件，调用方负责包成送回那个 pane 的消息。
pub fn view<'a>(st: &'a State) -> Option<Element<'a, Event>> {
    let section = |title: &str| crate::ui::text::metadata(title.to_string());
    let content = &st.content;
    match content {
        Content::FeatureMatrix => {
            let mut b = column![super::feature_matrix_view::inspector_settings().map(Event::FeatureMatrixInteraction)]
                .spacing(crate::ui::metrics::space(2));
            let m = super::feature_matrix_readout::snapshot();
            // 特征定义（点矩阵里的特征名选中）
            let kv = |k: &str, v: String| {
                iced::widget::row![
                    text(k.to_string()).size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()).width(Length::Fixed(72.0)),
                    text(v).size(crate::ui::text::s_small()).color(crate::ui::pal::txt()),
                ]
                .spacing(crate::ui::metrics::space(2))
            };
            match super::feature_matrix::inspected().and_then(|k| m.slots.iter().find(|s| s.key == k).cloned()) {
                Some(s) => {
                    let list = |v: &[String]| if v.is_empty() { crate::ui::fmt::missing() } else { v.join(" · ") };
                    b = b
                        .push(section("特征定义"))
                        .push(kv("名称", s.name_cn.clone()))
                        .push(kv("键", s.key.clone()))
                        .push(kv("阶段 / 族", format!("{} · {}", s.stage, s.family)))
                        .push(kv("单位", if s.unit.is_empty() { crate::ui::fmt::na() } else { s.unit.clone() }))
                        .push(kv("窗口", s.window_label()))
                        .push(kv("所需数据", list(&s.inputs)))
                        .push(kv("市场", list(&s.markets)))
                        .push(kv("实现", format!("{} · 第 {} 批", s.status, s.wave)))
                        .push(kv("先验", if s.prior.is_empty() { crate::ui::fmt::unknown() } else { s.prior.clone() }))
                        .push(kv("延迟档", s.latency.clone()))
                        .push(kv("默认启用", if s.default_on { "是".into() } else { "否".into() }))
                        .push(kv("当前质量", if s.reason.is_empty() { s.quality.clone() } else { format!("{} · {}", s.quality, s.reason) }));
                }
                None => {
                    b = b.push(section("特征定义")).push(
                        text("点矩阵里的特征名，这里显示它的定义").size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()),
                    );
                }
            }
            if m.chart.present && !m.chart.editable.is_empty() {
                b = b.push(section("图表参数口径")).push(
                    super::chart_params_view::edit_block(&m).map(Event::FeatureMatrixInteraction),
                );
            }
            Some(b.width(Length::Fill).into())
        }
        Content::OfmsLab => Some(super::ofms_lab_view::inspector_settings(st.unique_id()).map(Event::OfmsLabInteraction)),
        Content::BacktestResult => Some(super::backtest_launch_view::inspector_form().map(Event::BacktestLaunchInteraction)),
        Content::TardisBoard(tb) => {
            Some(with_part(Part::Props, || super::tardis_board_view::pane_body(tb, true)).map(Event::TardisBoardInteraction))
        }
        Content::MarketMap => Some(
            with_part(Part::Props, || super::radar_view::pane_body(st.settings.view.as_deref())).map(Event::RadarInteraction),
        ),
        // 订单：选中一行 → 检查器列出它的全部字段（docs/35 §16.15 第 10 项）；没选中不占检查器
        Content::Orders => super::orders_view::selected_detail().map(|(title, fields)| {
            let mut b = column![section(title)].spacing(crate::ui::metrics::space(1));
            for (k, v) in fields {
                b = b.push(
                    iced::widget::row![
                        text(k).size(crate::ui::text::s_meta()).color(crate::ui::pal::dim()).width(Length::Fixed(80.0)),
                        text(v).size(crate::ui::text::s_small()).color(crate::ui::pal::txt()),
                    ]
                    .spacing(crate::ui::metrics::space(2)),
                );
            }
            b.width(Length::Fill).into()
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 没有可编辑属性的面板不占检查器() {
        assert!(!has_props(&Content::Procs));
        assert!(has_props(&Content::FeatureMatrix) && has_props(&Content::OfmsLab) && has_props(&Content::BacktestResult));
    }
}

/// 检查器「数据」页（docs/42 第 3 期）：选中面板的数据选择——共用数据选择组件等。
pub fn data_view<'a>(st: &'a State) -> Option<Element<'a, Event>> {
    let section = |title: &str| crate::ui::text::metadata(title.to_string());
    match &st.content {
        Content::FeatureMatrix => Some(
            super::feature_matrix_view::inspector_source().map(Event::FeatureMatrixInteraction),
        ),
        Content::OfmsLab => Some(super::ofms_lab_view::inspector_data(st.unique_id()).map(Event::OfmsLabInteraction)),
        Content::BacktestResult => {
            let v = super::backtest_launch::view();
            Some(
                column![
                    section("回测数据"),
                    super::data_picker_view::view(&v.pick, &super::backtest_launch::pick_opts())
                        .map(|m| Event::BacktestLaunchInteraction(super::backtest_launch::LaunchMsg::Data(m))),
                ]
                .spacing(crate::ui::metrics::space(2))
                .into(),
            )
        }
        Content::TardisBoard(tb) => Some(super::tardis_board_view::inspector_data(tb).map(Event::TardisBoardInteraction)),
        Content::MarketMap => Some(
            with_part(Part::Data, || super::radar_view::pane_body(st.settings.view.as_deref())).map(Event::RadarInteraction),
        ),
        // 以下面板没有「属性」，只把守护 / 运行控制与数据状态搬进「数据」页（docs/42 第 4 期）。
        // 它们不自动展开检查器——只有在用户打开或钉住检查器时，主区才减负
        Content::News => Some(with_part(Part::Data, || super::news_view::pane_body(st.settings.view.as_deref())).map(Event::NewsInteraction)),
        Content::Factory => Some(with_part(Part::Data, super::factory_view::pane_body).map(Event::FactoryInteraction)),
        Content::C4Shadow => Some(with_part(Part::Data, super::c4_view::pane_body).map(Event::C4Interaction)),
        Content::PredictionBoard => Some(with_part(Part::Data, super::prediction_view::pane_body).map(Event::PredictionInteraction)),
        Content::PmBinance => Some(with_part(Part::Data, super::pm_binance_view::pane_body)),
        Content::WealthSpring(data::layout::pane::WsPaneMode::SelfChart) => {
            Some(super::customchart::inspector().map(Event::SelfChartInteraction))
        }
        Content::PmReplay(pr) => Some(with_part(Part::Data, || super::pm_replay_view::pane_body(pr)).map(Event::PmReplayInteraction)),
        Content::TardisReplay(tr) => Some(
            column![
                section("数据"),
                super::data_picker_view::view(&tr.pick, &super::tardis_replay::pick_opts())
                    .map(|m| Event::TardisReplayInteraction(super::tardis_replay::TardisReplayMsg::Data(m))),
            ]
            .spacing(crate::ui::metrics::space(2))
            .into(),
        ),
        _ => None,
    }
}

/// 「已改 n 项」（docs/42 §5.2）：选中面板与缺省值不同的设置，逐项一句。
///
/// 只列**会改变读数或可见范围**的设置（筛选、启用集、窗口、隐藏的泳道……），
/// 列宽、悬停这类纯外观不算——用户要回答的是「为什么我看到的和别人不一样」。
pub fn changed(st: &State) -> Vec<String> {
    let mut out = Vec::new();
    match &st.content {
        Content::FeatureMatrix => {
            use super::feature_matrix::{QualityFilter, StatusFilter, TableMode};
            let (mode, keys) = super::feature_matrix::read_config();
            match mode.as_str() {
                "default" => {}
                "all" => out.push("启用集：全开".into()),
                _ => out.push(format!("启用集：自定义 {} 条", keys.len())),
            }
            let w = super::feature_matrix::config_windows();
            if let Some(g) = &w.global {
                out.push(format!("全局时间窗口：{}", super::feature_matrix_readout::format_windows(g)));
            }
            if !w.overrides.is_empty() {
                out.push(format!("单条窗口覆盖 {} 条", w.overrides.len()));
            }
            let v = super::feature_matrix::state();
            if v.quality != QualityFilter::All {
                out.push(format!("质量筛选：{}", v.quality.label()));
            }
            if v.status != StatusFilter::All {
                out.push(format!("进度筛选：{}", v.status.label()));
            }
            if let Some(x) = v.stage {
                out.push(format!("阶段筛选：{x}"));
            }
            if let Some(x) = &v.family {
                out.push(format!("族筛选：{x}"));
            }
            if let Some(x) = &v.market {
                out.push(format!("市场筛选：{x}"));
            }
            if v.show_disabled {
                out.push("显示未启用的特征".into());
            }
            if v.table != TableMode::Full {
                out.push(format!("展示：{}", v.table.label()));
            }
        }
        Content::OfmsLab => {
            let v = super::ofms_lab::view(st.unique_id());
            if !v.hidden.is_empty() {
                out.push(format!("隐藏泳道 {} 条", v.hidden.len()));
            }
        }
        Content::MarketMap => {
            let v = super::radar::view();
            let d = super::radar::ViewState::DEFAULT;
            if v.asset != d.asset {
                out.push(format!("资产：{}", v.asset.label()));
            }
            if !v.source.is_empty() {
                out.push("来源：已选".into());
            }
            let n = super::radar_filter::active_count(&v);
            if n > 0 {
                out.push(format!("筛选 {n} 条"));
            }
            if v.size.key != d.size.key || v.color.key != d.color.key {
                out.push("大小 / 颜色口径".into());
            }
            if v.palette != d.palette {
                out.push("色板".into());
            }
        }
        _ => {}
    }
    out
}

/// 选中面板在检查器里有没有可设置的东西（docs/42：有才自动展开检查器）。
pub fn has_props(content: &Content) -> bool {
    match content {
        Content::FeatureMatrix
        | Content::BacktestResult
        | Content::TardisBoard(_)
        | Content::TardisReplay(_)
        | Content::OfmsLab
        | Content::MarketMap => true,
        Content::Orders => super::orders_view::selected_detail().is_some(),
        _ => false,
    }
}

/// 检查器「说明」页（docs/42 §3）：面板是干什么的、读数的口径、要注意什么。段落之间空一行。
pub fn about(content: &Content) -> Option<String> {
    let s = match content {
        Content::OfmsLab => "订单流层析（docs/40）：把订单流与微观结构事件按 OFMS-10 分层（L0 数据完整性 … L10 交易后学习）画在同一条时间轴上，点事件看因果链。\n\n数据由 Rust 特征引擎回放 + OFMS 检测器生成，按窗口缓存；实时模式读常驻引擎的输出。数据选择与生成 / 实时在「数据」页，泳道开关在「属性」页。\n\n注意：时间轴上的「同类事件之后」只是本窗口的描述，不是结论；跨窗口、扣成本、去重叠的正式统计在「响应表」页——目前 0 格越过成本。L2 的「墙被吃 / 被撤」是推断（Databento MBO 校验准确率 95–100%）。",
        Content::FeatureMatrix => "特征矩阵（docs/31）：常驻特征引擎算出的订单流特征（7 个阶段、458 条），逐条显示数值、z 与分位、数据质量。\n\n感知层，不是交易信号：特征有信息不等于能赚钱（docs/20）。引擎启停、启用集、时间窗口、筛选与展示、图表参数口径在「属性」页改；数据源在「数据」页。",
        Content::TardisBoard(_) => "Tardis 历史面板（docs/20 §9）：已购历史数据的查看器——数据 → 数据类型 → 图表（Tardis / Databento / 本地录制）。\n\n零交易所流：全部数据来自本地历史文件，不建立任何实时连接。\n\n数据、读法、数据类型在「数据」页选；出图（单符号 / 对比、自动、热图带宽）与导出在「属性」页；回放留在面板里，「加载」在顶部工具栏。",
        Content::TardisReplay(_) => "Tardis 回放：按速度回放已购的逐笔数据，图表边放边画。",
        Content::BacktestResult => "回测结果（docs/37）：最近一次（或钉住的那次）回测的 tearsheet——收益、回撤、成交、数据来源与体检等级。\n\n选策略在「属性」页、选数据在「数据」页，运行 / 停止在顶部工具栏。回测 PnL 不可直接外推实盘：队列位置已建模，但逆向选择、他人撤单、自身冲击不在内（docs/27 §9）。",
        Content::StrategyCenter => "策略中心（docs/37）：策略库、参数表单、完整 / 快速回测、运行记录、优化与验证、闸门（G1–G8）、对比。\n\n只跑观察模式与回测；真金下单必须逐次确认。",
        Content::StrategyLayers => "七层（docs/39）：交易大师策略的七层流水线，跟随策略中心选中的策略与运行。",
        Content::News => "新闻资讯（docs/25）：交易所 / 监管 / 媒体的统一时间线；连接全在 ws-news 守护里，面板只读快照。\n\n守护启停、条数与源新鲜度在「数据」页；`~` = 源没给发布时间，显示的是我们抓到的时刻。",
        Content::MarketMap => "全球市场雷达（docs/22）：全市场热图、筛选器、宽度等视图。发现工具，不是交易信号；加密是实时价，股票是延迟报价，徽章分档标出。\n\n资产 / 来源 / 筛选 / 大小 / 颜色 / 色板在「属性」页；守护启停、刷新、标的数、z 可信度与回填在「数据」页。",
        Content::Recorder(_) => "数据录制（docs/08）：行情录制守护的启停、实况、落盘总览、按日覆盖与录制明细。",
        Content::Procs => "进程（docs/26）：常驻单元的状态与启停。守护跟随 Cockpit 窗口：开窗拉起、关窗一并停止。",
        Content::NetEgress => "网络出口：谁在往外发包、连到哪、流量多少；行情订阅总闸。",
        Content::C4Shadow => "C4 活体影子（docs/14）：SOL 被动做市在实时行情上本地排队仿真（不下真单），按 UTC 日落账；7 个合格日判定。守护启停在「数据」页。\n\n成交是模拟的；真实成交可能更差（C1 / C6），只有小额实盘（C5）能回答。",
        Content::Factory => "Alpha Factory（docs/08）：因子流水线与现役池；多重比较账在这里。\n\n状态计数、生成来源与晋级阈值在「数据」页。",
        Content::FeatureLab => "特征库（docs/30）：特征的研究纪律记录与检验结果。",
        Content::OptionsBoard => "期权 / 0DTE（docs/18）：期权回测与探针。",
        Content::PredictionBoard | Content::PmBinance | Content::PmReplay(_) => "预测市场（docs/19）：Polymarket 决策支持与币安钱包 5 分钟盘口；只读旁路快照。\n\nPolymarket 夜跑启停与每日定时、币安钱包的录制状态、回放的符号与日期在「数据」页。",
        Content::WealthSpring(data::layout::pane::WsPaneMode::SelfChart) => "自有数据图：把任意 CSV / JSON 数据画成自适应折线（横轴时间或数值按数据自动判定），与策略运行无关，纯展示。\n\n数据文件在「数据」页选（选择文件… 或直接填路径），选过的会记住；文件改了自动重读。",
        Content::Orders => "订单：活动挂单与成交明细；选中一行，「属性」页列出它的全部字段。",
        Content::Kline { .. } | Content::Heatmap { .. } | Content::ShaderHeatmap { .. } | Content::TimeAndSales(_) | Content::Ladder(_) | Content::Comparison(_) => {
            "行情图表：标的与周期在顶部工具栏改（作用于选中的这张图，开了 ⛓ 联动时同组一起改）。指标与显示设置在面板标题栏的齿轮里。\n\n同页按时间轴的图会联动十字线：光标放在一张上，其他图在同一时刻画竖线。"
        }
        _ => return None,
    };
    // 工具类面板的长说明：面板里与这里共用同一份文字（检查器托管时面板里不再显示）
    let extra: Vec<String> = match content {
        Content::Procs => super::procs_view::NOTES.iter().map(|n| n.to_string()).collect(),
        Content::NetEgress => super::egress_view::notes(),
        _ => vec![],
    };
    Some(std::iter::once(s.to_string()).chain(extra).collect::<Vec<_>>().join("\n\n"))
}
