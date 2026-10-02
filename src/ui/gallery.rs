//! 组件样张页（docs/35 批 4 / §13.2）：全部组件的全部状态铺在一页上，截图回归用。
//!
//! 只在样张模式里出现：`WS_UI_SPECIMEN_COMPONENTS=1`（行数用 `WS_UI_SPECIMEN_ROWS`，缺省 10⁶）。
//! 网格初始就定位在中部，并把**每帧构建网格视图的耗时**显示出来——
//! 这是「10⁶ 行照样流畅」的实测证据，不是口头保证（UPDS V5 §39）。

use iced::widget::{column, container, row, scrollable};
use iced::{Element, Length};

use super::fmt::{self, Absence, Provenance, Rounding};
use super::grid::{self, Cell, Column, GridMsg, GridState};
use super::widgets::{self, Kind, Tone};
use super::{metrics, text as t};

#[derive(Debug, Clone)]
pub enum GalleryMsg {
    Grid(GridMsg),
    Tab(u8),
    Seg(u8),
    Noop,
}

pub struct Gallery {
    cols: Vec<Column>,
    rows: Vec<Vec<Cell>>,
    grid: GridState,
    tab: u8,
    seg: u8,
}

impl Gallery {
    pub fn from_env() -> Option<Self> {
        std::env::var_os("WS_UI_SPECIMEN_COMPONENTS")?;
        let n: usize = std::env::var("WS_UI_SPECIMEN_ROWS").ok().and_then(|s| s.parse().ok()).unwrap_or(1_000_000);
        let cols = vec![
            Column::text("合约", 120.0).key(),
            Column::num("最新价", Some("USD"), 110.0),
            Column::num("24h 涨跌", Some("%"), 90.0),
            Column::num("成交额", Some("USD"), 110.0),
            Column::text("状态", 90.0).groupable(),
        ];
        let d = super::domain();
        let mut rows: Vec<Vec<Cell>> = (0..n)
            .map(|i| {
                let px = 100.0 + (i as f64 * 0.37).sin() * 50.0 + i as f64 * 1e-3;
                let chg = ((i as f64) * 0.013).cos() * 0.05;
                let vol = (i as f64 * 7919.0) % 9.0e7;
                vec![
                    Cell::Id(format!("SYM-{i}")),
                    if i % 97 == 0 {
                        Cell::Absent(Absence::Missing)
                    } else {
                        Cell::Num { v: Some(px), s: fmt::number(px, 2, Rounding::Money), prov: if i % 13 == 0 { Provenance::Derived } else { Provenance::Measured } }
                    },
                    Cell::Colored(
                        format!("{} {}", if chg >= 0.0 { "▲" } else { "▼" }, fmt::pct(chg, 2)),
                        super::color(if chg >= 0.0 { d.up } else { d.down }),
                    ),
                    Cell::num(vol, fmt::compact(vol, 0)),
                    if i % 11 == 0 { Cell::Badge("降级".into(), Tone::Warning) } else { Cell::Badge("良好".into(), Tone::Success) },
                ]
            })
            .collect();
        // WS_UI_SPECIMEN_LONG=1：第一行放一个超长值（docs/35 §13.1「数据规模」维度）——超长的合约名
        // （中英混合）与一个极大的价格，看标识符中间省略、数字列不折行不撑破
        if std::env::var_os("WS_UI_SPECIMEN_LONG").is_some() && !rows.is_empty() {
            let huge = 9_876_543_210_987_654.32;
            rows[0][0] = Cell::Id(format!("超长合约名-PERP-{}-季度交割-永续掉期-{}", "X".repeat(60), "尾巴"));
            rows[0][1] = Cell::Num { v: Some(huge), s: fmt::number(huge, 2, Rounding::Money), prov: Provenance::Measured };
        }
        let mut grid = GridState::new(&cols);
        // WS_UI_SPECIMEN_GROUP=1：样张按「状态」分组并折叠「良好」，证明分组 + 折叠 + 虚拟滚动一起工作
        if std::env::var_os("WS_UI_SPECIMEN_GROUP").is_some() {
            grid.group_by = Some(4);
            grid.collapsed.insert("良好".into());
        }
        // WS_UI_SPECIMEN_FILTER=1：展开过滤器，预置一条「24h 涨跌 > 0」
        if std::env::var_os("WS_UI_SPECIMEN_FILTER").is_some() {
            grid.filters.push(grid::Filter { col: 2, op: grid::FilterOp::Gt, value: "0".into() });
            grid.panel = Some(grid::GridPanel::Filters);
        }
        grid.resort(&cols, &rows);
        Some(Self { cols, rows, grid, tab: 0, seg: 1 })
    }

    /// 启动时滚到中部：证明虚拟滚动在深处也只渲染可见行。
    pub fn initial_scroll<M: 'static + Send>(&self) -> iced::Task<M> {
        let n = self.grid.len(self.rows.len());
        // 分组样张：滚到末尾附近，同时看到上一组的尾巴和折叠着的组头
        self.grid.scroll_to_row(if self.grid.group_by.is_some() { n.saturating_sub(12) } else { n / 2 })
    }

    pub fn update(&mut self, m: GalleryMsg) {
        match m {
            GalleryMsg::Grid(g) => self.grid.update(g, &self.cols, &self.rows),
            GalleryMsg::Tab(i) => self.tab = i,
            GalleryMsg::Seg(i) => self.seg = i,
            GalleryMsg::Noop => {}
        }
    }

    pub fn view(&self) -> Element<'_, GalleryMsg> {
        let sec = |title: &'static str, body: Element<'static, GalleryMsg>| -> Element<'static, GalleryMsg> {
            column![widgets::section(title), body].spacing(metrics::space(2)).into()
        };

        let buttons = row![
            widgets::btn("主要操作", Kind::Primary, Some(GalleryMsg::Noop)),
            widgets::btn("标准", Kind::Standard, Some(GalleryMsg::Noop)),
            widgets::btn("次要", Kind::Subtle, Some(GalleryMsg::Noop)),
            widgets::btn("幽灵", Kind::Ghost, Some(GalleryMsg::Noop)),
            widgets::btn("删除 2 个录制文件…", Kind::Destructive, Some(GalleryMsg::Noop)),
            widgets::btn("禁用", Kind::Standard, None::<GalleryMsg>),
        ]
        .spacing(metrics::space(3));

        let badges = row![
            widgets::badge("中性", Tone::Neutral),
            widgets::badge("强调", Tone::Accent),
            widgets::badge("良好", Tone::Success),
            widgets::badge("降级", Tone::Warning),
            widgets::badge("断流", Tone::Danger),
            widgets::badge("说明", Tone::Info),
        ]
        .spacing(metrics::space(3));

        let values = row![
            widgets::value("842 110.00", Provenance::Measured),
            widgets::value("512 006.75", Provenance::Derived),
            widgets::value("24.86", Provenance::Estimated),
            widgets::value("142 350.00", Provenance::Overridden),
            widgets::value("98 442.10", Provenance::Stale),
            widgets::value("248 860.25", Provenance::Simulated),
            widgets::absent(Absence::Missing),
            widgets::absent(Absence::NotApplicable),
            widgets::absent(Absence::Unknown),
            widgets::absent(Absence::Withheld),
            widgets::absent(Absence::Invalid),
        ]
        .spacing(metrics::space(5));

        let tabs = widgets::tabs(&[("矩阵", 0u8), ("图表参数", 1), ("引擎健康", 2)], &self.tab, GalleryMsg::Tab);
        let seg = widgets::segmented(&[("表格", 0u8), ("图表", 1), ("两者", 2)], &self.seg, GalleryMsg::Seg);

        let states = row![
            container(widgets::empty::<GalleryMsg>("没有选中的合约", "在网格里选一行查看它的数值，或按 Ctrl P 直接搜索。")).width(Length::FillPortion(1)),
            container(widgets::loading::<GalleryMsg>("正在读取 feature_matrix.json…")).width(Length::FillPortion(1)),
            container(widgets::skeleton::<GalleryMsg>(4)).width(Length::FillPortion(1)),
        ]
        .spacing(metrics::space(5));
        let states2 = row![
            container(widgets::error::<GalleryMsg>(
                "导出失败 —— strategies/backtest_results/x.csv",
                "目标目录只读（权限 0555）。",
                "换一个位置，或在「设置 → 存储」里改导出目录。结果已在内存中，没有丢失。",
                Some("EXP-0x2F04"),
            ))
            .width(Length::FillPortion(1)),
            container(widgets::stale_bar::<GalleryMsg>("14 秒", "ws-features 快照没有更新")).width(Length::FillPortion(1)),
            container(widgets::unavailable::<GalleryMsg>("实盘下单", "当前是回测环境；实盘下单需要布防两步提交（docs/35 §9.2）。")).width(Length::FillPortion(1)),
        ]
        .spacing(metrics::space(5));

        let grid_view = grid::view(
            &self.cols,
            &self.rows,
            &self.grid,
            Some(format!("上一帧构建可见行 {} µs（共 {} 行）", grid::last_build_us(), self.rows.len())),
            GalleryMsg::Grid,
        );

        let top = column![
            row![t::title("组件样张"), t::caption(format!("主题 {} · 密度 {}", super::theme_id().label(), super::density().label()))]
                .spacing(metrics::space(4))
                .align_y(iced::Alignment::End),
            sec("按钮（每个作用域只有一个主要按钮；危险操作不用强调色填充）", buttons.into()),
            sec("徽标（颜色配符号，灰度下也分得清）", badges.into()),
            sec("数值来源标记与六种「无」", values.into()),
            sec("标签页 · 分段按钮", row![tabs, seg].spacing(metrics::space(6)).into()),
            sec("状态：空 · 加载 · 骨架", states.into()),
            sec("状态：错误 · 过期 · 不可用", states2.into()),
            widgets::panel_header(
                format!("数据网格 · {} 行（虚拟滚动，初始定位在中部）", self.rows.len()),
                Some(widgets::badge("实时", Tone::Success)),
                vec![widgets::btn("复制为 TSV", Kind::Ghost, Some(GalleryMsg::Noop))],
            ),
        ]
        .spacing(metrics::space(5));

        column![scrollable(top.padding(metrics::space(5))).height(Length::Shrink), container(grid_view).width(Length::Fill).height(Length::Fill)]
            .spacing(0)
            .into()
    }
}
