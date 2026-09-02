//! 全市场雷达面板的交互消息与视图状态（docs/22 P0b，表达形式对齐 TradingView）。
//!
//! 守护默认**不随开机自启**（同 recorder/C4/预测，见 c682944），由本面板按钮控制启停。
//!
//! 视图状态放**进程级静态**而不是 pane 状态：同一份口径对所有雷达 pane 生效，
//! 换 pane 不用重设一遍。

use std::sync::Mutex;

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

/// 资产类过滤。股票的成交额远大于加密，同图时加密会被挤到看不见——
/// 想专看某一类就用这个，而不是靠分组去找。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetFilter {
    All,
    Crypto,
    Equity,
    Etf,
}

impl AssetFilter {
    /// 该行是否通过。`All` 恒通过。
    pub fn keeps(self, asset: &str) -> bool {
        match self {
            AssetFilter::All => true,
            AssetFilter::Crypto => asset == "crypto",
            AssetFilter::Equity => asset == "equity",
            AssetFilter::Etf => asset == "etf",
        }
    }
}

/// 面板视图。三块回答的是不同问题（docs/22 §2）：
/// 热图=「谁在动」、总览=「哪个国家最强」、宽度=「整个市场什么状态」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Heatmap,
    Overview,
    Breadth,
}

/// Screener 的列组（对应 TradingView 筛选器顶部的 Overview/Performance/… 标签）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnSet {
    Overview,
    Performance,
    Speed,
    Volume,
    /// 参考数据（国别/板块/市值）——股票层进来后才有意义。
    Reference,
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
    /// 来源选择。股票/ETF 时是市场代码（如 `america`），加密时是分类
    /// （如 `defi`）。空串 = 全部。对应 TV 的「来源」下拉。
    pub source: &'static str,
    pub sort: SortKey,
    /// true = 降序。点同一列再点一次翻向。
    pub desc: bool,
}

impl ViewState {
    pub const DEFAULT: Self = Self {
        win: 1, // 5m
        size: SIZE_OPTS[0],
        color: COLOR_OPTS[0],
        group_by: GroupBy::None,
        cols: ColumnSet::Overview,
        palette: Palette::BlueOrange,
        mode: ViewMode::Heatmap,
        asset: AssetFilter::All,
        source: "",
        sort: SortKey::Z(1),
        desc: true,
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
    SetSource(&'static str),
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
        RadarMsg::SetMode(m) => v.mode = m,
        RadarMsg::SetAsset(a) => {
            v.asset = a;
            // 换资产类时清掉来源：加密选的是分类、股票选的是市场，
            // 留着旧值会得到一张空表，看起来像数据没了
            v.source = "";
        }
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

pub fn visible(rows: &[RadarRow], a: AssetFilter, source: &str) -> Vec<usize> {
    (0..rows.len())
        .filter(|&i| a.keeps(&rows[i].asset) && matches_source(&rows[i], source))
        .collect()
}

/// 按当前口径排序，返回行下标。
///
/// 缺值恒沉底：把「还没热身」排在榜首等于用空数据占据最显眼的位置。
pub fn order(rows: &[RadarRow], v: ViewState) -> Vec<usize> {
    order_within(rows, &visible(rows, v.asset, v.source), v)
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
        assert!(AssetFilter::Crypto.keeps("crypto"));
        assert!(!AssetFilter::Crypto.keeps("equity"));
        assert!(AssetFilter::Equity.keeps("equity"));
        assert!(!AssetFilter::Equity.keeps("crypto"));
        // 未声明资产类的行不该被任何具体过滤器意外收进来
        assert!(!AssetFilter::Crypto.keeps(""));
        assert!(!AssetFilter::Equity.keeps("other"));
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
        v.asset = AssetFilter::Equity;
        let o = order(&rows, v);
        assert_eq!(o.len(), 1, "过滤后只该剩股票");
        assert_eq!(rows[o[0]].symbol, "NVDA");
        v.asset = AssetFilter::Crypto;
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
        let v = apply(v, RadarMsg::SetAsset(AssetFilter::Crypto));
        assert_eq!(v.source, "");
    }

    #[test]
    fn source_matches_market_for_equities() {
        let mut a = row("NVDA", "tv:america:stock", Some(1.0), None, 1.0);
        a.asset = "equity".into();
        let mut b = row("7203", "tv:japan:stock", Some(1.0), None, 1.0);
        b.asset = "equity".into();
        let rows = vec![a, b];
        assert_eq!(visible(&rows, AssetFilter::All, "").len(), 2);
        let v = visible(&rows, AssetFilter::All, "japan");
        assert_eq!(v.len(), 1);
        assert_eq!(rows[v[0]].symbol, "7203");
        // 同一市场的股票与 ETF 都该匹配
        let mut c = row("SPY", "tv:america:fund", Some(1.0), None, 1.0);
        c.asset = "etf".into();
        let rows = vec![rows[0].clone(), c];
        assert_eq!(visible(&rows, AssetFilter::All, "america").len(), 2);
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
        let v = visible(&rows, AssetFilter::Crypto, "defi");
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
        assert!(!AssetFilter::Equity.keeps("etf"), "ETF 不该混进股票");
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
