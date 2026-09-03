//! 全市场雷达面板的交互消息与视图状态（docs/22 P0b，表达形式对齐 TradingView）。
//!
//! 守护默认**不随开机自启**（同 recorder/C4/预测，见 c682944），由本面板按钮控制启停。
//!
//! 视图状态放**进程级静态**而不是 pane 状态：同一份口径对所有雷达 pane 生效，
//! 换 pane 不用重设一遍。

use std::sync::Mutex;

use super::radar_filter::{self, N_FILTERS};
use super::radar_readout::{RadarRow, N_WIN};

/// 一个下拉选项。`key` 是快照契约里的指标键（见 docs/22 §4.3），
/// `own:` 前缀表示雷达自有指标（TradingView 没有的）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Opt {
    pub key: &'static str,
    pub label: &'static str,
}

impl std::fmt::Display for Opt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label)
    }
}

const fn o(key: &'static str, label: &'static str) -> Opt {
    Opt { key, label }
}

/// 「大小来自」——对齐 TradingView 股票热图的 8 项。
pub const SIZE_OPTS: [Opt; 9] = [
    o("market_cap_basic", "市值"),
    o("volume", "成交量 1天"),
    o("average_volume_10d_calc", "成交量 ~1周"),
    o("average_volume_30d_calc", "成交量 ~1月"),
    o("Value.Traded", "价格×成交量 1天"),
    o("value_traded_10d", "价格×成交量 ~1周"),
    o("value_traded_30d", "价格×成交量 ~1月"),
    // 加密没有市值口径，用自有的 24h 成交额
    o("own:turnover", "24h 成交额（加密）"),
    o("equal", "相同大小"),
];

/// 「颜色来自」——TradingView 的 14 项 + 雷达自有的 3 项。
///
/// 自有那三项是 TradingView 没有的：波动归一化的涨跌速度、成交额异常度。
pub const COLOR_OPTS: [Opt; 17] = [
    o("own:speed_z", "涨跌速度 z（雷达）"),
    o("own:ret_pct", "涨跌幅·选定窗口（雷达）"),
    o("own:zvol", "量异常 z（雷达）"),
    o("change|60", "涨跌 1小时, %"),
    o("change|240", "涨跌 4小时, %"),
    o("change", "涨跌 1天, %"),
    o("Perf.W", "表现 1周, %"),
    o("Perf.1M", "表现 1月, %"),
    o("Perf.3M", "表现 3月, %"),
    o("Perf.6M", "表现 6月, %"),
    o("Perf.YTD", "表现 YTD, %"),
    o("Perf.Y", "表现 1年, %"),
    o("premarket_change", "盘前涨跌, %"),
    o("postmarket_change", "盘后涨跌, %"),
    o("relative_volume_10d_calc", "相对成交量"),
    o("Volatility.D", "波动率 1天, %"),
    o("gap", "跳空, %"),
];

/// 色阶形态：决定中性点在哪、以及是否双向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaleKind {
    /// 以 0 为中性的双向（涨跌类）。
    Diverging,
    /// 以 1 为中性的双向（相对成交量：1.0 = 常态）。
    AroundOne,
    /// 单向量级（波动率恒非负，双向上色是错的）。
    Magnitude,
}

pub fn scale_kind(key: &str) -> ScaleKind {
    match key {
        "relative_volume_10d_calc" => ScaleKind::AroundOne,
        "Volatility.D" => ScaleKind::Magnitude,
        _ => ScaleKind::Diverging,
    }
}

/// 树图分组（对应 TradingView 股票热图按板块分组）。加密没有板块，
/// 能用的是 venue——顺带解决同一标的在 spot/linear 各出一格、面积算重的问题。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupBy {
    None,
    Venue,
    /// 按国别（股票层才有；加密行归入「加密」一组）。
    Country,
    /// 按板块，同 TradingView 股票热图的默认分组。
    Sector,
}

/// 色板。TradingView 用绿涨红跌；但树图上格子多且小，红绿对红绿色盲不可分辨，
/// 故默认仍是蓝橙。A 股习惯又是红涨绿跌——三种都给，别替用户猜。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Palette {
    /// 蓝涨橙跌（默认，色盲安全）。
    BlueOrange,
    /// 绿涨红跌（TradingView / 欧美习惯）。
    GreenUp,
    /// 红涨绿跌（A 股习惯）。
    RedUp,
}

/// 资产类。**对齐 TradingView 的组织方式：先选资产类，下面的一切才跟着它走。**
///
/// 官方的热图只有 股票 / ETF / 加密 三种，筛选器有 股票 / ETF / 债券 /
/// 加密货币 / CEX / DEX 六种；每一类的字段集完全不同（股票 `market_cap_basic`
/// vs 加密 `market_cap_calc` vs DEX `dex_total_liquidity`），可选项也不同。
///
/// `All` 是雷达自有的额外项，不是官方分类——它是「一眼看整个市场」这个
/// 初衷所需，只在热图里给。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetFilter {
    All,
    Stock,
    Etf,
    Coin,
    Cex,
    Dex,
    Bond,
}

impl AssetFilter {
    /// 目录里的资产类 id（`assets[].kind`）。`All` 没有对应的类。
    pub fn kind(self) -> &'static str {
        match self {
            AssetFilter::All => "",
            AssetFilter::Stock => "stock",
            AssetFilter::Etf => "etf",
            AssetFilter::Coin => "coin",
            AssetFilter::Cex => "cex",
            AssetFilter::Dex => "dex",
            AssetFilter::Bond => "bond",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AssetFilter::All => "全部",
            AssetFilter::Stock => "股票",
            AssetFilter::Etf => "ETF",
            AssetFilter::Coin => "加密货币",
            AssetFilter::Cex => "CEX",
            AssetFilter::Dex => "DEX",
            AssetFilter::Bond => "债券",
        }
    }

    /// 该行是否通过。`All` 恒通过。
    ///
    /// 行上的 `asset` 是**数据源**声明的（加密源写 `crypto`、股票源写
    /// `equity`），与这里的 id 不是同一套字符串，必须显式映射——直接比
    /// `kind()` 的话「股票」会一行都匹配不到。
    pub fn keeps(self, asset: &str) -> bool {
        match self {
            AssetFilter::All => true,
            AssetFilter::Stock => asset == "equity",
            AssetFilter::Etf => asset == "etf",
            AssetFilter::Coin => asset == "crypto",
            AssetFilter::Cex => asset == "cex",
            AssetFilter::Dex => asset == "dex",
            AssetFilter::Bond => asset == "bond",
        }
    }

    /// 热图页可选的资产类（官方三种 + 雷达自有的「全部」）。
    pub const HEATMAP: [AssetFilter; 4] =
        [AssetFilter::All, AssetFilter::Stock, AssetFilter::Etf, AssetFilter::Coin];

    /// 筛选器页可选的资产类（官方六种）。
    pub const SCREENER: [AssetFilter; 6] = [
        AssetFilter::Stock,
        AssetFilter::Etf,
        AssetFilter::Coin,
        AssetFilter::Cex,
        AssetFilter::Dex,
        AssetFilter::Bond,
    ];
}

/// 面板视图。三块回答的是不同问题（docs/22 §2）：
/// 热图=「谁在动」、总览=「哪个国家最强」、宽度=「整个市场什么状态」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    /// 热图页（对齐 tradingview.com/heatmap/）。
    Heatmap,
    /// 筛选器页（对齐 tradingview.com/screener/）。**与热图分开**——
    /// 原来两者挤在一个视图里，控制条上一半的下拉对当前内容无效。
    Screener,
    Overview,
    Breadth,
}

/// 筛选器的表达形式。官方页面左上角那三个小图标就是这个。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    Table,
    Heatmap,
}

/// Screener 的列组（对应 TradingView 筛选器顶部的 Overview/Performance/… 标签）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnSet {
    /// 雷达自有：多窗口涨跌幅。
    Speed,
    /// 雷达自有：多窗口速度 z。
    SpeedZ,
    /// 参考数据（国别/板块/市值）。
    Reference,
    /// TradingView 的九个列组之一，按目录里的下标寻址。
    Tv(usize),
}

/// 排序键。`Ret`/`Z` 带窗口下标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Symbol,
    Tier,
    Venue,
    Country,
    Sector,
    Mcap,
    Price,
    Turnover,
    Ret(usize),
    Z(usize),
    VolZ,
    CntZ,
    /// TradingView 口径的任意指标（键见 docs/22 §4.3）。
    Metric(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewState {
    pub win: usize,
    pub size: Opt,
    pub color: Opt,
    pub group_by: GroupBy,
    pub cols: ColumnSet,
    pub palette: Palette,
    pub mode: ViewMode,
    pub asset: AssetFilter,
    /// 筛选器页的表达形式（表格/热图）。热图页不用这个。
    pub form: Form,
    /// 来源选择。股票/ETF 时是市场代码（如 `america`），加密时是分类
    /// （如 `defi`）。空串 = 全部。对应 TV 的「来源」下拉。
    pub source: &'static str,
    pub sort: SortKey,
    /// true = 降序。点同一列再点一次翻向。
    pub desc: bool,
    /// 筛选栏是否展开。默认收起——19 个下拉一直占着屏幕，热图就没地方了。
    pub show_filters: bool,
    /// 各筛选器选中的档位下标，`0` = 不限（见 `radar_filter::FILTERS`）。
    /// 用定长数组而不是 Map，好让整个视图状态保持 `Copy`。
    pub filters: [u8; N_FILTERS],
}

impl ViewState {
    pub const DEFAULT: Self = Self {
        win: 1, // 5m
        size: SIZE_OPTS[0],
        color: COLOR_OPTS[0],
        group_by: GroupBy::None,
        cols: ColumnSet::Tv(0),
        palette: Palette::BlueOrange,
        mode: ViewMode::Heatmap,
        asset: AssetFilter::All,
        form: Form::Table,
        source: "",
        sort: SortKey::Z(1),
        desc: true,
        show_filters: false,
        filters: [0; N_FILTERS],
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadarMsg {
    Start,
    Stop,
    Refresh,
    SetWindow(usize),
    SetSize(Opt),
    SetColor(Opt),
    SetGroupBy(GroupBy),
    SetColumns(ColumnSet),
    SetPalette(Palette),
    SetMode(ViewMode),
    SetAsset(AssetFilter),
    SetForm(Form),
    SetSource(&'static str),
    /// 设某个筛选器的档位。`pi = 0` 为不限。
    SetFilter { fi: usize, pi: u8 },
    /// 一键清空全部筛选。
    ClearFilters,
    ToggleFilters,
    /// 点列头：同列则翻向，异列则换列并回到降序（数值列降序更符合「看榜」的直觉）。
    SortBy(SortKey),
}

static ACTION_MSG: Mutex<String> = Mutex::new(String::new());
static VIEW: Mutex<ViewState> = Mutex::new(ViewState::DEFAULT);

pub fn action_message() -> String {
    ACTION_MSG.lock().map(|m| m.clone()).unwrap_or_default()
}

pub fn view() -> ViewState {
    VIEW.lock().map(|v| *v).unwrap_or(ViewState::DEFAULT)
}

/// 纯状态迁移（可单测）：不碰 systemd、不碰全局锁。
pub fn apply(v: ViewState, msg: RadarMsg) -> ViewState {
    let mut v = v;
    match msg {
        RadarMsg::SetWindow(i) => {
            let i = i.min(N_WIN - 1);
            // 排序键跟着窗口走：换窗口后还按旧窗口排，榜单和高亮的列对不上
            v.sort = match v.sort {
                SortKey::Ret(_) => SortKey::Ret(i),
                SortKey::Z(_) => SortKey::Z(i),
                other => other,
            };
            v.win = i;
        }
        RadarMsg::SetSize(x) => v.size = x,
        RadarMsg::SetColor(x) => v.color = x,
        RadarMsg::SetGroupBy(g) => v.group_by = g,
        RadarMsg::SetColumns(c) => v.cols = c,
        RadarMsg::SetPalette(p) => v.palette = p,
        RadarMsg::SetMode(m) => {
            v.mode = m;
            // 每个视图允许的资产类不同：热图没有 CEX/DEX/债券，筛选器没有「全部」。
            // 不收敛的话切过去会是一张空表，看起来像数据没了
            let allowed: &[AssetFilter] = match m {
                ViewMode::Heatmap => &AssetFilter::HEATMAP,
                ViewMode::Screener => &AssetFilter::SCREENER,
                _ => &[],
            };
            if !allowed.is_empty() && !allowed.contains(&v.asset) {
                v.asset = allowed[0];
                v.source = "";
                v.filters = [0; N_FILTERS];
            }
        }
        RadarMsg::SetForm(f) => v.form = f,
        RadarMsg::SetAsset(a) => {
            v.asset = a;
            // 换资产类时清掉来源：加密选的是分类、股票选的是市场，
            // 留着旧值会得到一张空表，看起来像数据没了
            v.source = "";
            // 筛选同理，而且更隐蔽：市盈率/股息这些基本面指标加密根本没有，
            // 带着「市盈率 10–15」切到加密，会得到一张空表且**没有任何提示**
            v.filters = [0; N_FILTERS];
        }
        RadarMsg::SetFilter { fi, pi } => {
            if fi < N_FILTERS && (pi as usize) <= radar_filter::n_presets(fi) {
                v.filters[fi] = pi;
            }
        }
        RadarMsg::ClearFilters => v.filters = [0; N_FILTERS],
        RadarMsg::ToggleFilters => v.show_filters = !v.show_filters,
        RadarMsg::SetSource(m) => v.source = m,
        RadarMsg::SortBy(k) => {
            if v.sort == k {
                v.desc = !v.desc;
            } else {
                v.sort = k;
                v.desc = !matches!(
                k,
                SortKey::Symbol | SortKey::Venue | SortKey::Tier | SortKey::Country | SortKey::Sector
            );
            }
        }
        RadarMsg::Start | RadarMsg::Stop | RadarMsg::Refresh => {}
    }
    v
}

pub fn handle(msg: RadarMsg) {
    use super::radar_readout as ro;
    let m = match msg {
        RadarMsg::Start => ro::radar_start(),
        RadarMsg::Stop => ro::radar_stop(),
        RadarMsg::Refresh => {
            ro::request_refresh();
            String::new()
        }
        other => {
            if let Ok(mut g) = VIEW.lock() {
                *g = apply(*g, other);
            }
            String::new()
        }
    };
    if let Ok(mut g) = ACTION_MSG.lock() {
        *g = m;
    }
}

/// 取某行在某个排序键上的数值。`None` 一律沉底（不论升降序）。
pub fn key_value(r: &RadarRow, k: SortKey) -> Option<f64> {
    match k {
        SortKey::Symbol | SortKey::Venue | SortKey::Tier | SortKey::Country | SortKey::Sector => {
            None
        }
        SortKey::Mcap => Some(r.mcap),
        SortKey::Price => Some(r.price),
        SortKey::Turnover => Some(r.quote_vol_24h),
        SortKey::Ret(i) => r.ret[i],
        SortKey::Z(i) => r.z_ret[i],
        SortKey::VolZ => r.z_vol,
        SortKey::CntZ => r.z_cnt,
        SortKey::Metric(k) => r.m.get(k).copied(),
    }
}

/// 通过资产类过滤的行下标。
/// 该行是否匹配来源选择。
///
/// 股票/ETF 按**市场代码**匹配 venue（`tv:america:stock` 含 `america`）；
/// 加密按**分类**匹配。两者语义不同，不能共用一个字符串相等判断。
pub fn matches_source(r: &RadarRow, source: &str) -> bool {
    if source.is_empty() {
        return true;
    }
    if r.asset == "crypto" {
        r.cats.iter().any(|c| c == source)
    } else {
        // venue 形如 `tv:{market}:{type}`
        r.venue.split(':').nth(1).is_some_and(|m| m == source)
    }
}

pub fn visible(rows: &[RadarRow], v: ViewState) -> Vec<usize> {
    visible_at(rows, v, radar_filter::now_s())
}

/// 同 `visible`，但「现在」由调用方给——日期类筛选要用，单测也要用。
pub fn visible_at(rows: &[RadarRow], v: ViewState, now_s: i64) -> Vec<usize> {
    (0..rows.len())
        .filter(|&i| {
            let r = &rows[i];
            v.asset.keeps(&r.asset)
                && matches_source(r, v.source)
                && radar_filter::passes(r, &v, now_s)
        })
        .collect()
}

/// 按当前口径排序，返回行下标。
///
/// 缺值恒沉底：把「还没热身」排在榜首等于用空数据占据最显眼的位置。
pub fn order(rows: &[RadarRow], v: ViewState) -> Vec<usize> {
    order_within(rows, &visible(rows, v), v)
}

/// 只对给定子集排序（资产类过滤后用）。
pub fn order_within(rows: &[RadarRow], subset: &[usize], v: ViewState) -> Vec<usize> {
    let mut idx: Vec<usize> = subset.to_vec();
    idx.sort_by(|&a, &b| {
        let (ra, rb) = (&rows[a], &rows[b]);
        let ord = match v.sort {
            SortKey::Symbol => ra.symbol.cmp(&rb.symbol),
            SortKey::Venue => ra.venue.cmp(&rb.venue).then(ra.symbol.cmp(&rb.symbol)),
            SortKey::Tier => ra.tier.cmp(&rb.tier).then(ra.symbol.cmp(&rb.symbol)),
            SortKey::Country => ra.country.cmp(&rb.country).then(ra.symbol.cmp(&rb.symbol)),
            SortKey::Sector => ra.sector.cmp(&rb.sector).then(ra.symbol.cmp(&rb.symbol)),
            k => {
                let (va, vb) = (key_value(ra, k), key_value(rb, k));
                match (va, vb) {
                    (Some(x), Some(y)) => {
                        x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal)
                    }
                    // 缺值沉底：升序时也不能让它冒到最前
                    (Some(_), None) => return std::cmp::Ordering::Less,
                    (None, Some(_)) => return std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                }
            }
        };
        let ord = if v.desc { ord.reverse() } else { ord };
        // 同分按标的定序，否则每次刷新表格顺序会抖
        ord.then_with(|| ra.symbol.cmp(&rb.symbol).then(ra.venue.cmp(&rb.venue)))
    });
    idx
}

#[cfg(test)]
mod tests {

    #[test]
    fn each_view_only_offers_the_asset_kinds_it_supports() {
        // 官方热图只有 股票/ETF/加密；筛选器六类且没有「全部」。
        // 混成一套是原来「下拉一半无效」的根源。
        assert!(!AssetFilter::HEATMAP.contains(&AssetFilter::Bond));
        assert!(!AssetFilter::HEATMAP.contains(&AssetFilter::Cex));
        assert!(!AssetFilter::SCREENER.contains(&AssetFilter::All));
        assert_eq!(AssetFilter::SCREENER.len(), 6);
    }

    #[test]
    fn switching_view_pulls_the_asset_kind_into_range() {
        // 在热图选了「全部」再切到筛选器：筛选器没有「全部」，
        // 不收敛的话会是一张空表，看起来像数据没了
        let mut v = ViewState::DEFAULT;
        v.asset = AssetFilter::All;
        let v = apply(v, RadarMsg::SetMode(ViewMode::Screener));
        assert!(AssetFilter::SCREENER.contains(&v.asset));

        // 反向：筛选器选了债券再切回热图，热图没有债券
        let mut v = ViewState::DEFAULT;
        v.mode = ViewMode::Screener;
        v.asset = AssetFilter::Bond;
        let v = apply(v, RadarMsg::SetMode(ViewMode::Heatmap));
        assert!(AssetFilter::HEATMAP.contains(&v.asset));
    }

    #[test]
    fn switching_view_keeps_a_still_valid_asset_kind() {
        // 两个视图都支持股票，切过去不该被重置
        let mut v = ViewState::DEFAULT;
        v.asset = AssetFilter::Stock;
        v.source = "japan";
        let v = apply(v, RadarMsg::SetMode(ViewMode::Screener));
        assert_eq!(v.asset, AssetFilter::Stock);
        assert_eq!(v.source, "japan", "还合法就不该顺手清掉来源");
    }

    #[test]
    fn asset_kind_ids_match_the_catalog_and_row_labels() {
        // kind() 是目录里的 id，keeps() 比的是数据源写在行上的 asset——
        // 两套字符串不同（stock vs equity、coin vs crypto），直接比 kind 会一行都匹配不到
        assert_eq!(AssetFilter::Stock.kind(), "stock");
        assert!(AssetFilter::Stock.keeps("equity") && !AssetFilter::Stock.keeps("stock"));
        assert_eq!(AssetFilter::Coin.kind(), "coin");
        assert!(AssetFilter::Coin.keeps("crypto") && !AssetFilter::Coin.keeps("coin"));
        // 每个类的 id 唯一
        let mut k: Vec<_> = AssetFilter::SCREENER.iter().map(|a| a.kind()).collect();
        let n = k.len();
        k.sort();
        k.dedup();
        assert_eq!(k.len(), n);
    }

    #[test]
    fn form_toggle_is_independent_of_the_view() {
        let v = apply(ViewState::DEFAULT, RadarMsg::SetForm(Form::Heatmap));
        assert_eq!(v.form, Form::Heatmap);
        assert_eq!(v.mode, ViewState::DEFAULT.mode, "换表达形式不该动视图");
        let v = apply(v, RadarMsg::SetMode(ViewMode::Screener));
        assert_eq!(v.form, Form::Heatmap, "切视图不该重置表达形式");
    }
    use super::*;

    fn row(sym: &str, venue: &str, z5: Option<f64>, ret5: Option<f64>, tv: f64) -> RadarRow {
        let mut r = RadarRow {
            symbol: sym.into(),
            venue: venue.into(),
            quote_vol_24h: tv,
            sigma_ok: z5.is_some(),
            ..Default::default()
        };
        r.z_ret[1] = z5;
        r.ret[1] = ret5;
        r
    }

    #[test]
    fn asset_filter_keeps_only_the_selected_class() {
        assert!(AssetFilter::All.keeps("crypto") && AssetFilter::All.keeps("equity"));
        assert!(AssetFilter::Coin.keeps("crypto"));
        assert!(!AssetFilter::Coin.keeps("equity"));
        assert!(AssetFilter::Stock.keeps("equity"));
        assert!(!AssetFilter::Stock.keeps("crypto"));
        // 未声明资产类的行不该被任何具体过滤器意外收进来
        assert!(!AssetFilter::Coin.keeps(""));
        assert!(!AssetFilter::Stock.keeps("other"));
    }

    #[test]
    fn order_respects_the_asset_filter() {
        let mut c = row("BTCUSDT", "binance:linear", Some(9.0), None, 1.0);
        c.asset = "crypto".into();
        let mut e = row("NVDA", "tv:america", Some(1.0), None, 2.0);
        e.asset = "equity".into();
        let rows = vec![c, e];

        let mut v = ViewState::DEFAULT;
        assert_eq!(order(&rows, v).len(), 2);
        v.asset = AssetFilter::Stock;
        let o = order(&rows, v);
        assert_eq!(o.len(), 1, "过滤后只该剩股票");
        assert_eq!(rows[o[0]].symbol, "NVDA");
        v.asset = AssetFilter::Coin;
        assert_eq!(rows[order(&rows, v)[0]].symbol, "BTCUSDT");
    }

    #[test]
    fn mode_switch_preserves_the_other_knobs() {
        let v = apply(ViewState::DEFAULT, RadarMsg::SetWindow(3));
        let v = apply(v, RadarMsg::SetMode(ViewMode::Breadth));
        assert_eq!(v.mode, ViewMode::Breadth);
        assert_eq!(v.win, 3, "换视图不该顺手重置窗口");
        assert_eq!(v.palette, ViewState::DEFAULT.palette);
    }

    #[test]
    fn group_by_covers_reference_data_dimensions() {
        // 股票层进来后，venue 分组不够用了——板块/国别才是股票的自然维度
        for g in [GroupBy::None, GroupBy::Venue, GroupBy::Country, GroupBy::Sector] {
            let v = apply(ViewState::DEFAULT, RadarMsg::SetGroupBy(g));
            assert_eq!(v.group_by, g);
        }
    }

    #[test]
    fn option_lists_match_tradingview_counts() {
        // 对齐 TV 股票热图：大小 8 项（我们多一项加密成交额）、颜色 14 项（我们多 3 项自有）
        assert_eq!(SIZE_OPTS.len(), 9);
        assert_eq!(COLOR_OPTS.len(), 17);
        let tv_color = COLOR_OPTS.iter().filter(|o| !o.key.starts_with("own:")).count();
        assert_eq!(tv_color, 14, "TradingView 的 14 个颜色口径不能漏");
        let tv_size = SIZE_OPTS
            .iter()
            .filter(|o| !o.key.starts_with("own:") && o.key != "equal")
            .count();
        assert_eq!(tv_size, 7);
    }

    #[test]
    fn option_keys_are_unique() {
        // 键重复会让下拉选中项与实际取值对不上
        for list in [&SIZE_OPTS[..], &COLOR_OPTS[..]] {
            let mut k: Vec<_> = list.iter().map(|o| o.key).collect();
            let n = k.len();
            k.sort();
            k.dedup();
            assert_eq!(k.len(), n);
        }
    }

    #[test]
    fn scale_kind_is_not_diverging_for_magnitudes() {
        // 波动率恒非负，双向上色是错的；相对成交量的中性点是 1.0 不是 0
        assert_eq!(scale_kind("Volatility.D"), ScaleKind::Magnitude);
        assert_eq!(scale_kind("relative_volume_10d_calc"), ScaleKind::AroundOne);
        assert_eq!(scale_kind("change"), ScaleKind::Diverging);
        assert_eq!(scale_kind("own:speed_z"), ScaleKind::Diverging);
    }

    #[test]
    fn size_and_color_are_settable_independently() {
        // 按 key 找，不写死下标——加一项就会让下标断言假失败
        let by = |list: &'static [Opt], k: &str| *list.iter().find(|o| o.key == k).unwrap();
        let v = apply(ViewState::DEFAULT, RadarMsg::SetSize(by(&SIZE_OPTS, "Value.Traded")));
        assert_eq!(v.size.key, "Value.Traded");
        assert_eq!(v.color, ViewState::DEFAULT.color, "换大小不该动颜色");
        let v = apply(v, RadarMsg::SetColor(by(&COLOR_OPTS, "Perf.YTD")));
        assert_eq!(v.color.key, "Perf.YTD");
        assert_eq!(v.size.key, "Value.Traded");
    }

    #[test]
    fn switching_asset_clears_the_source() {
        // 加密选分类、股票选市场；留着旧值会得到一张空表，看起来像数据没了
        let mut v = ViewState::DEFAULT;
        v.source = "america";
        let v = apply(v, RadarMsg::SetAsset(AssetFilter::Coin));
        assert_eq!(v.source, "");
    }

    #[test]
    fn source_matches_market_for_equities() {
        let mut a = row("NVDA", "tv:america:stock", Some(1.0), None, 1.0);
        a.asset = "equity".into();
        let mut b = row("7203", "tv:japan:stock", Some(1.0), None, 1.0);
        b.asset = "equity".into();
        let rows = vec![a, b];
        assert_eq!(visible(&rows, { let mut v = ViewState::DEFAULT; v.asset = AssetFilter::All; v.source = ""; v }).len(), 2);
        let v = visible(&rows, { let mut v = ViewState::DEFAULT; v.asset = AssetFilter::All; v.source = "japan"; v });
        assert_eq!(v.len(), 1);
        assert_eq!(rows[v[0]].symbol, "7203");
        // 同一市场的股票与 ETF 都该匹配
        let mut c = row("SPY", "tv:america:fund", Some(1.0), None, 1.0);
        c.asset = "etf".into();
        let rows = vec![rows[0].clone(), c];
        assert_eq!(visible(&rows, { let mut v = ViewState::DEFAULT; v.asset = AssetFilter::All; v.source = "america"; v }).len(), 2);
    }

    #[test]
    fn source_matches_category_for_crypto() {
        let mut a = row("BTCUSDT", "binance:linear", Some(1.0), None, 1.0);
        a.asset = "crypto".into();
        a.cats = vec!["layer-1".into(), "cryptocurrencies".into()];
        let mut b = row("UNIUSDT", "binance:linear", Some(1.0), None, 1.0);
        b.asset = "crypto".into();
        b.cats = vec!["defi".into()];
        let rows = vec![a, b];
        let v = visible(&rows, { let mut v = ViewState::DEFAULT; v.asset = AssetFilter::Coin; v.source = "defi"; v });
        assert_eq!(v.len(), 1);
        assert_eq!(rows[v[0]].symbol, "UNIUSDT");
        // 没打上分类的不该被任何具体分类收进来
        let mut c = row("XUSDT", "binance:linear", Some(1.0), None, 1.0);
        c.asset = "crypto".into();
        assert!(!matches_source(&c, "defi"));
        assert!(matches_source(&c, ""), "「全部」应恒匹配");
    }

    #[test]
    fn asset_filter_covers_etf() {
        assert!(AssetFilter::Etf.keeps("etf"));
        assert!(!AssetFilter::Etf.keeps("equity"));
        assert!(!AssetFilter::Stock.keeps("etf"), "ETF 不该混进股票");
        assert!(AssetFilter::All.keeps("etf"));
    }

    #[test]
    fn palette_is_settable_and_defaults_to_colorblind_safe() {
        assert_eq!(ViewState::DEFAULT.palette, Palette::BlueOrange);
        let v = apply(ViewState::DEFAULT, RadarMsg::SetPalette(Palette::GreenUp));
        assert_eq!(v.palette, Palette::GreenUp);
        // 换色板不该顺手改掉别的口径
        assert_eq!(v.sort, ViewState::DEFAULT.sort);
        assert_eq!(v.win, ViewState::DEFAULT.win);
    }

    #[test]
    fn clicking_same_column_toggles_direction() {
        let v = ViewState::DEFAULT;
        assert!(v.desc);
        let v = apply(v, RadarMsg::SortBy(SortKey::Z(1)));
        assert!(!v.desc, "同列再点应翻向");
        let v = apply(v, RadarMsg::SortBy(SortKey::Z(1)));
        assert!(v.desc);
    }

    #[test]
    fn switching_column_resets_to_natural_direction() {
        let v = apply(ViewState::DEFAULT, RadarMsg::SortBy(SortKey::Z(1))); // 变升序
        let v = apply(v, RadarMsg::SortBy(SortKey::Turnover));
        assert_eq!(v.sort, SortKey::Turnover);
        assert!(v.desc, "数值列换列应回到降序");
        let v = apply(v, RadarMsg::SortBy(SortKey::Symbol));
        assert!(!v.desc, "文本列换列应升序（A→Z）");
    }

    #[test]
    fn changing_window_follows_the_sort_column() {
        let v = apply(ViewState::DEFAULT, RadarMsg::SetWindow(3));
        assert_eq!(v.win, 3);
        assert_eq!(v.sort, SortKey::Z(3), "按 z 排序时换窗口，排序键要跟着走");

        let v = apply(v, RadarMsg::SortBy(SortKey::Turnover));
        let v = apply(v, RadarMsg::SetWindow(0));
        assert_eq!(v.sort, SortKey::Turnover, "非窗口相关的排序键不该被改写");
    }

    #[test]
    fn window_index_is_clamped() {
        let v = apply(ViewState::DEFAULT, RadarMsg::SetWindow(99));
        assert_eq!(v.win, N_WIN - 1);
    }

    #[test]
    fn missing_values_sink_in_both_directions() {
        let rows = vec![
            row("A", "v", None, None, 1.0),
            row("B", "v", Some(-4.0), Some(0.01), 2.0),
            row("C", "v", Some(1.0), Some(0.02), 3.0),
        ];
        let mut v = ViewState::DEFAULT;
        v.sort = SortKey::Z(1);
        v.desc = true;
        assert_eq!(rows[order(&rows, v)[2]].symbol, "A", "降序时缺值该沉底");
        v.desc = false;
        assert_eq!(
            rows[order(&rows, v)[2]].symbol,
            "A",
            "升序时缺值也该沉底，不能冒到最前"
        );
    }

    #[test]
    fn descending_sort_is_signed_not_absolute() {
        // 列头排序按 TradingView 的语义：看的是带符号值，不是 |值|。
        // 想看跌得最狠的就切升序，而不是让涨跌混排。
        let rows = vec![
            row("UP", "v", Some(3.0), None, 1.0),
            row("DOWN", "v", Some(-9.0), None, 1.0),
        ];
        let mut v = ViewState::DEFAULT;
        v.sort = SortKey::Z(1);
        v.desc = true;
        assert_eq!(rows[order(&rows, v)[0]].symbol, "UP");
        v.desc = false;
        assert_eq!(rows[order(&rows, v)[0]].symbol, "DOWN");
    }

    #[test]
    fn ordering_is_stable_on_ties() {
        let rows = vec![
            row("ZZZ", "v", Some(1.0), None, 5.0),
            row("AAA", "v", Some(1.0), None, 5.0),
        ];
        let o = order(&rows, ViewState::DEFAULT);
        assert_eq!(rows[o[0]].symbol, "AAA", "同分应按标的定序，避免刷新抖动");
    }

    #[test]
    fn text_sort_uses_symbol_then_venue() {
        let rows = vec![
            row("BTCUSDT", "binance:spot", None, None, 1.0),
            row("BTCUSDT", "binance:linear", None, None, 1.0),
            row("AAAUSDT", "binance:spot", None, None, 1.0),
        ];
        let mut v = ViewState::DEFAULT;
        v.sort = SortKey::Symbol;
        v.desc = false;
        let o = order(&rows, v);
        assert_eq!(rows[o[0]].symbol, "AAAUSDT");
        assert_eq!(rows[o[1]].venue, "binance:linear", "同名按 venue 定序");
    }
}
