//! 绘图对象模型（docs/46 §3）：锚点存（时间, 价格），不存像素。
//!
//! 对象 JSON 里认不得的字段原样留着（`extra`）：旧程序打开新格式写的对象，存回去时不丢东西
//! （docs/45 C8 的教训）。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// 对象格式版本（存在库的 `meta` 表里）。2：加了到价提醒 `alert`（G5）。升级时在 `store::MIGRATIONS` 加一步。
pub const FORMAT: u32 = 2;

/// 一个锚点：时间（毫秒，UTC）+ 价格（定点 `Price::units`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    pub t: u64,
    pub p: i64,
}

/// 工具 / 对象种类（G5 起是名字：内置的是下面的常量，**新工具放进 `tools/` 目录就自动登记**，不用改这里）。
/// 存盘、外部文件里写的就是这个名字（`h_line`、`trend`…）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Kind(pub &'static str);

#[allow(non_upper_case_globals)]
impl Kind {

    /// 水平线：一个锚点，只看价格，横贯整张图
    pub const HLine: Kind = Kind("h_line");
    /// 垂直线：一个锚点，只看时间
    pub const VLine: Kind = Kind("v_line");
    /// 趋势线：两个锚点之间的线段
    pub const Trend: Kind = Kind("trend");
    /// 射线：从第一个锚点出发、穿过第二个锚点、一直延到图边
    pub const Ray: Kind = Kind("ray");
    /// 矩形：两个对角
    pub const Rect: Kind = Kind("rect");
    /// 折线：任意多个顶点（至少两个；回车或在最后一个点上再点一下结束）
    pub const Polyline: Kind = Kind("polyline");
    /// 平行通道：前两个锚点是一条边，第三个锚点定另一条边（平行）；中线虚线
    pub const Channel: Kind = Kind("channel");
    /// 斐波那契回撤：两个锚点；只存锚点与比例，各条线由引擎算
    pub const FibRetrace: Kind = Kind("fib_retrace");
    /// 斐波那契延伸（按趋势）：A → B 是趋势，C 是回调点，比例从 C 起量
    pub const FibExt: Kind = Kind("fib_ext");
    /// 文字：一个锚点 + 文字
    pub const Text: Kind = Kind("text");
    /// 标注：指向的点 + 文字框的位置
    pub const Callout: Kind = Kind("callout");
    /// 价格区间：两个锚点之间的价差、百分比、tick 数、基点
    pub const PriceRange: Kind = Kind("price_range");
    /// 日期区间：两个锚点之间的 K 线根数与实际时长
    pub const DateRange: Kind = Kind("date_range");
    /// 多头仓位：入场、目标、止损三个价位（点一下放下，再拖改）；盈亏只是**估算**，不是委托
    pub const LongPos: Kind = Kind("long_pos");
    /// 空头仓位
    pub const ShortPos: Kind = Kind("short_pos");
    /// 标记：一个点上的 ▲ / ▼ / ● 加文字（成交、订单流事件、策略决策；G4）
    pub const Marker: Kind = Kind("marker");
    /// 成交量分布：时间区间里各价位的成交量，算出 POC / VAH / VAL（存参数，画的时候按数据重算；G4）
    pub const VolProfile: Kind = Kind("vol_profile");
}

impl Serialize for Kind {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.0)
    }
}

impl<'de> Deserialize<'de> for Kind {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let name = String::deserialize(d)?;
        Kind::from_name(&name).ok_or_else(|| serde::de::Error::custom(format!("不认识的绘图种类 {name:?}（新版本的工具？）")))
    }
}

impl Kind {
    /// 内置的种类（登记的工具见 [`Kind::all`]）。
    pub const ALL: [Self; 17] = [
        Self::HLine,
        Self::VLine,
        Self::Trend,
        Self::Ray,
        Self::Rect,
        Self::Polyline,
        Self::Channel,
        Self::FibRetrace,
        Self::FibExt,
        Self::Text,
        Self::Callout,
        Self::PriceRange,
        Self::DateRange,
        Self::LongPos,
        Self::ShortPos,
        Self::Marker,
        Self::VolProfile,
    ];

    /// 存几个锚点（折线不定，返回 0）。
    #[must_use]
    pub fn anchors(self) -> usize {
        match self {
            Self::HLine | Self::VLine | Self::Text | Self::Marker => 1,
            Self::Trend | Self::Ray | Self::Rect | Self::FibRetrace | Self::Callout | Self::PriceRange | Self::DateRange | Self::VolProfile => 2,
            Self::Channel | Self::FibExt | Self::LongPos | Self::ShortPos => 3,
            Self::Polyline => 0,
            k => super::tools::find(k).map_or(1, |t| t.anchors()),
        }
    }

    /// 内置的 + 登记的全部种类。
    #[must_use]
    pub fn all() -> Vec<Self> {
        Self::ALL.iter().copied().chain(super::tools::all().iter().map(|t| t.kind())).collect()
    }

    /// 名字 → 种类（内置的或登记的）。
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::all().into_iter().find(|k| k.0 == name)
    }

    /// 放下它要在图上点几下（折线不定，返回 0；仓位点一下，另两个价位按缺省比例摆好）。
    #[must_use]
    pub fn clicks(self) -> usize {
        match self {
            Self::LongPos | Self::ShortPos => 1,
            k => super::tools::find(k).map_or_else(|| k.anchors(), |t| t.clicks()),
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::HLine => "水平线",
            Self::VLine => "垂直线",
            Self::Trend => "趋势线",
            Self::Ray => "射线",
            Self::Rect => "矩形",
            Self::Polyline => "折线",
            Self::Channel => "平行通道",
            Self::FibRetrace => "斐波那契回撤",
            Self::FibExt => "斐波那契延伸",
            Self::Text => "文字",
            Self::Callout => "标注",
            Self::PriceRange => "价格区间",
            Self::DateRange => "日期区间",
            Self::LongPos => "多头仓位",
            Self::ShortPos => "空头仓位",
            Self::Marker => "标记",
            Self::VolProfile => "成交量分布",
            k => super::tools::find(k).map_or("?", |t| t.label()),
        }
    }

    /// 斐波那契的缺省比例。
    #[must_use]
    pub fn default_levels(self) -> Vec<f64> {
        match self {
            Self::FibRetrace => vec![0.0, 0.236, 0.382, 0.5, 0.618, 0.786, 1.0],
            Self::FibExt => vec![0.0, 0.618, 1.0, 1.272, 1.618, 2.618],
            _ => Vec::new(),
        }
    }
}

/// 线型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dash {
    #[default]
    Solid,
    Dashed,
    Dotted,
}

/// 样式。颜色是用户数据（RGBA 字节），画的时候经 `ui::chart::drawing_color` 换成界面颜色。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Style {
    pub color: [u8; 4],
    pub width: f32,
    pub dash: Dash,
    /// 矩形的填充（`None` = 用线色的 15%）
    pub fill: Option<[u8; 4]>,
}

impl Default for Style {
    fn default() -> Self {
        Self { color: [41, 98, 255, 255], width: 1.5, dash: Dash::Solid, fill: None }
    }
}

/// 对象从哪来（docs/46 §2 第 3 条）。只有手工的能拖、能删；其余只读，可以复制成手工对象（G4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    #[default]
    Manual,
    Backtest,
    Strategy,
    Calendar,
    Order,
    /// 订单流事件（docs/40 订单流层析；G4）
    OrderFlow,
    /// 研究脚本经 Python 接口写上来的（G4）
    Research,
}

impl Source {
    /// 系统来源（菜单里按它们分别显示 / 隐藏）。
    pub const SYSTEM: [Self; 6] = [Self::Order, Self::Calendar, Self::Strategy, Self::Backtest, Self::OrderFlow, Self::Research];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Manual => "手工",
            Self::Backtest => "回测",
            Self::Strategy => "策略",
            Self::Calendar => "日历",
            Self::Order => "订单",
            Self::OrderFlow => "订单流",
            Self::Research => "研究",
        }
    }
}

/// 主题色（系统对象用：跟着主题与「红涨绿跌」走，不写死颜色；G4）。设了就盖过 `style.color`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    Up,
    Down,
    Warn,
    Danger,
    Accent,
    Dim,
}

/// 一个绘图对象。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Drawing {
    pub id: String,
    pub kind: Kind,
    pub anchors: Vec<Anchor>,
    #[serde(default)]
    pub style: Style,
    #[serde(default)]
    pub locked: bool,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub source: Source,
    /// 分组（对象树里按它归类，整组隐藏 / 锁定；G1）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// 在哪几类周期上**不**显示（[`TF_CATS`] 的键；空 = 处处显示；G1）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hide_on: Vec<String>,
    /// 文字 / 标注的内容（G2）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// 斐波那契的比例（空 = 缺省那一组；G2）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub levels: Vec<f64>,
    /// 仓位工具：数量（张 / 个）与单边手续费（基点）——只用于估算（G2）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qty: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fee_bp: Option<f64>,
    /// 画它时那张图的周期（如 `15m`、`50T`；G3）：到别的周期看时线头标出来
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_tf: Option<String>,
    /// 只在画它的那个周期显示（G3）
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub only_origin: bool,
    /// 主题色（G4）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<Tone>,
    /// 标记的方向：1 = ▲（在价位下方）、−1 = ▼（上方）、0 = ●（G4）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<i8>,
    /// 成交量分布：价位分几档、价值区占比（缺省 24 档、0.7；G4）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub va: Option<f64>,
    /// 到价提醒（G5）：价格穿过这条线时提醒——**只提醒，不下单**
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alert: Option<Alert>,
    /// 认不得的字段（新版本加的）原样保留
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Drawing {
    /// 新的手工对象。锚点数不对时补齐 / 截断到该种类要的个数。
    #[must_use]
    pub fn new(kind: Kind, mut anchors: Vec<Anchor>) -> Self {
        let n = kind.anchors();
        if n > 0
            && let Some(&last) = anchors.last()
        {
            anchors.resize(n, last);
        }
        Self {
            id: new_id(),
            kind,
            anchors,
            style: Style::default(),
            locked: false,
            hidden: false,
            source: Source::Manual,
            group: None,
            hide_on: Vec::new(),
            text: matches!(kind, Kind::Text | Kind::Callout).then(|| "文字".to_string()),
            levels: Vec::new(),
            qty: None,
            fee_bp: None,
            origin_tf: None,
            only_origin: false,
            tone: None,
            dir: None,
            rows: None,
            va: None,
            alert: None,
            extra: Map::new(),
        }
    }

    /// 在这张图上显不显示：这一类周期没关、没设「只在来源周期显示」或者正是来源周期。`tf`：这张图的周期。
    #[must_use]
    pub fn visible_in(&self, cat: &str, tf: &str) -> bool {
        self.shown_on(cat) && (!self.only_origin || self.origin_tf.as_deref().is_none_or(|o| o == tf))
    }

    /// 到 `tf` 这张图上看时要不要标来源周期：画它的周期不是这一个。
    #[must_use]
    pub fn origin_tag(&self, tf: &str) -> Option<&str> {
        self.origin_tf.as_deref().filter(|o| *o != tf)
    }

    /// 斐波那契用的比例（没设就是缺省那一组）。
    #[must_use]
    pub fn fib_levels(&self) -> Vec<f64> {
        if self.levels.is_empty() { self.kind.default_levels() } else { self.levels.clone() }
    }

    /// 拖了第 `i` 个锚点之后保持形状的约束：仓位工具的目标与止损在同一个右边界上。
    pub fn constrain(&mut self, moved: usize) {
        if matches!(self.kind, Kind::LongPos | Kind::ShortPos) && self.anchors.len() == 3 && moved >= 1 {
            let t = self.anchors[moved].t;
            self.anchors[1].t = t;
            self.anchors[2].t = t;
        }
    }

    /// 在这一类周期上显示吗（[`tf_cat`]）。
    #[must_use]
    pub fn shown_on(&self, cat: &str) -> bool {
        !self.hide_on.iter().any(|c| c == cat)
    }

    /// 能不能拖、能不能删（锁定的、系统生成的都不行）。
    #[must_use]
    pub fn editable(&self) -> bool {
        !self.locked && self.source == Source::Manual
    }
}

/// 周期类别（可见周期按类设，与 TradingView 的「可见性」同一个思路）：(键, 名称)。
pub const TF_CATS: [(&str, &str); 5] = [("tick", "逐笔"), ("sec", "秒级"), ("min", "分钟"), ("hour", "小时"), ("day", "日线")];

/// 一张图的周期 → 类别。按时间的图看周期毫秒，按逐笔的图是 `tick`。
#[must_use]
pub fn tf_cat(interval_ms: Option<u64>) -> &'static str {
    match interval_ms {
        None => "tick",
        Some(ms) if ms < 60_000 => "sec",
        Some(ms) if ms < 3_600_000 => "min",
        Some(ms) if ms < 86_400_000 => "hour",
        Some(_) => "day",
    }
}

/// 价格定点（`exchange::unit::Price::units`）的刻度：1 = 10⁻¹¹。
pub const PRICE_SCALE: i64 = 100_000_000_000;

/// 定点价格 → 文字（`decimals` 位小数，四舍五入）。
#[must_use]
pub fn price_text(units: i64, decimals: usize) -> String {
    let d = decimals.min(11);
    let step = 10_i64.pow(11 - d as u32);
    let r = (units as i128 + if units >= 0 { step as i128 / 2 } else { -(step as i128) / 2 }) / step as i128;
    let neg = r < 0;
    let r = r.unsigned_abs();
    let base = 10_u128.pow(d as u32);
    let (int, frac) = (r / base, r % base);
    let sign = if neg { "-" } else { "" };
    if d == 0 { format!("{sign}{int}") } else { format!("{sign}{int}.{frac:0d$}") }
}

/// 文字 → 定点价格（不经浮点，不丢精度）。认不得返回 `None`。
#[must_use]
pub fn parse_price(s: &str) -> Option<i64> {
    let s = s.trim();
    let (neg, s) = s.strip_prefix('-').map_or((false, s), |r| (true, r));
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    if int.is_empty() && frac.is_empty() || frac.len() > 11 || !int.chars().chain(frac.chars()).all(|c| c.is_ascii_digit()) {
        return None;
    }
    let int: i64 = if int.is_empty() { 0 } else { int.parse().ok()? };
    let frac_units: i64 = if frac.is_empty() { 0 } else { format!("{frac:0<11}").parse().ok()? };
    let v = int.checked_mul(PRICE_SCALE)?.checked_add(frac_units)?;
    Some(if neg { -v } else { v })
}

/// 毫秒 → `YYYY-MM-DD HH:MM:SS`（UTC）。
#[must_use]
pub fn time_text(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64).map_or_else(|| ms.to_string(), |t| t.format("%Y-%m-%d %H:%M:%S").to_string())
}

/// `YYYY-MM-DD HH:MM[:SS]`（UTC）→ 毫秒。
#[must_use]
pub fn parse_time(s: &str) -> Option<u64> {
    let s = s.trim();
    let t = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M"))
        .ok()?;
    u64::try_from(t.and_utc().timestamp_millis()).ok()
}

/// 到价提醒的条件。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertWhen {
    /// 穿过（上下都算）
    Cross,
    /// 从下往上穿
    Up,
    /// 从上往下穿
    Down,
}

impl AlertWhen {
    pub const ALL: [Self; 3] = [Self::Cross, Self::Up, Self::Down];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Cross => "穿越",
            Self::Up => "上穿",
            Self::Down => "下穿",
        }
    }

    /// 价格从 `prev` 走到 `cur`，算不算碰到了 `level`。
    #[must_use]
    pub fn hit(self, prev: i64, cur: i64, level: i64) -> bool {
        let up = prev < level && cur >= level;
        let down = prev > level && cur <= level;
        match self {
            Self::Cross => up || down,
            Self::Up => up,
            Self::Down => down,
        }
    }
}

/// 一个对象上的到价提醒（G5）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Alert {
    pub when: AlertWhen,
    /// 响一次就停
    #[serde(default)]
    pub once: bool,
    /// 也发 Telegram（还要看 Telegram 配好、开着；与日历提醒同一个通道）
    #[serde(default)]
    pub telegram: bool,
    /// 还在等（「只提醒一次」响过之后是 false）
    #[serde(default = "yes")]
    pub armed: bool,
    /// 上次响的时刻（毫秒）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fired: Option<u64>,
}

fn yes() -> bool {
    true
}

impl Kind {
    /// 能不能设到价提醒：水平线、趋势线、射线。
    #[must_use]
    pub fn alertable(self) -> bool {
        matches!(self, Kind::HLine | Kind::Trend | Kind::Ray)
    }
}

impl Drawing {
    /// 这条线在 `t`（毫秒）时的价位：水平线就是它的价；趋势线在两个锚点之间按时间插值（之外没有）；
    /// 射线从第一个锚点起往后延伸。
    #[must_use]
    pub fn level_at(&self, t: u64) -> Option<i64> {
        let a = *self.anchors.first()?;
        match self.kind {
            Kind::HLine => Some(a.p),
            Kind::Trend | Kind::Ray => {
                let b = *self.anchors.get(1)?;
                let (lo, hi) = (a.t.min(b.t), a.t.max(b.t));
                if a.t == b.t || t < lo || (self.kind == Kind::Trend && t > hi) || (self.kind == Kind::Ray && t < a.t.min(b.t)) {
                    return None;
                }
                let f = (t as f64 - a.t as f64) / (b.t as f64 - a.t as f64);
                Some(a.p + ((b.p - a.p) as f64 * f).round() as i64)
            }
            _ => None,
        }
    }
}

/// 成交量分布的结果（G4）：各档 (价位下沿, 上沿, 成交量)、POC 那档、价值区的上下沿。
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub bins: Vec<(i64, i64, f64)>,
    pub poc: usize,
    pub vah: i64,
    pub val: i64,
}

/// 把一组 K 线（开始时间, 低, 高, 成交量）按价位分档：每根的量**均摊到它高低之间**（K 线没有逐价位成交时的近似；
/// 有足迹数据时更准，G4 先用均摊）。价值区从 POC 往两边、每次加量大的那一侧，直到够 `va` 占比。
#[must_use]
pub fn volume_profile(bars: &[(u64, i64, i64, f64)], rows: u32, va: f64) -> Option<Profile> {
    let lo = bars.iter().map(|b| b.1).min()?;
    let hi = bars.iter().map(|b| b.2).max()?;
    let rows = rows.clamp(4, 200) as usize;
    let step = ((hi - lo) as f64 / rows as f64).max(1.0);
    let mut vol = vec![0.0; rows];
    for &(_, l, h, v) in bars {
        if v <= 0.0 {
            continue;
        }
        let span = (h - l).max(1) as f64;
        for (i, slot) in vol.iter_mut().enumerate() {
            let (b0, b1) = (lo as f64 + i as f64 * step, lo as f64 + (i + 1) as f64 * step);
            let overlap = (b1.min(h as f64) - b0.max(l as f64)).max(0.0);
            if h == l {
                if (l as f64) >= b0 && ((l as f64) < b1 || i == rows - 1) {
                    *slot += v;
                }
            } else {
                *slot += v * overlap / span;
            }
        }
    }
    let total: f64 = vol.iter().sum();
    if total <= 0.0 {
        return None;
    }
    let poc = vol.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).map_or(0, |(i, _)| i);
    let (mut a, mut b, mut got) = (poc, poc, vol[poc]);
    while got < total * va.clamp(0.0, 1.0) && (a > 0 || b + 1 < rows) {
        let up = if b + 1 < rows { vol[b + 1] } else { -1.0 };
        let down = if a > 0 { vol[a - 1] } else { -1.0 };
        if up >= down {
            b += 1;
            got += vol[b];
        } else {
            a -= 1;
            got += vol[a];
        }
    }
    let edge = |i: usize| (lo as f64 + i as f64 * step).round() as i64;
    Some(Profile { bins: (0..rows).map(|i| (edge(i), edge(i + 1), vol[i])).collect(), poc, vah: edge(b + 1), val: edge(a) })
}

/// 仓位工具点一下时的缺省摆法：目标 +2%、止损 −1%（空头反过来），右边界在 `right` 时刻。
#[must_use]
pub fn position_anchors(kind: Kind, entry: Anchor, right: u64, snap: i64) -> Vec<Anchor> {
    let snap = snap.max(1);
    let pct = |f: f64| ((entry.p as f64 * f) / snap as f64).round() as i64 * snap;
    let (up, down) = (entry.p + pct(0.02).max(snap), entry.p - pct(0.01).max(snap));
    let (target, stop) = if kind == Kind::ShortPos { (entry.p - pct(0.02).max(snap), entry.p + pct(0.01).max(snap)) } else { (up, down) };
    vec![entry, Anchor { t: right, p: target }, Anchor { t: right, p: stop }]
}

/// 仓位工具的估算（G2）：风险收益比与扣手续费后的盈亏。**只是估算，不是委托**。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PosEstimate {
    /// 目标 / 止损相对入场的幅度（%）
    pub target_pct: f64,
    pub stop_pct: f64,
    /// 风险收益比：目标幅度 ÷ 止损幅度
    pub rr: f64,
    /// 到目标、到止损的盈亏（报价币，已扣进出两边手续费）
    pub win: f64,
    pub loss: f64,
}

/// `multiplier`：合约乘数（现货 / U 本位按 1）。
#[must_use]
pub fn estimate(d: &Drawing, multiplier: f64) -> Option<PosEstimate> {
    let [e, t, s] = <[Anchor; 3]>::try_from(d.anchors.as_slice()).ok()?;
    let f = |u: i64| u as f64 / PRICE_SCALE as f64;
    let (pe, pt, ps) = (f(e.p), f(t.p), f(s.p));
    if pe <= 0.0 {
        return None;
    }
    let dir = if d.kind == Kind::ShortPos { -1.0 } else { 1.0 };
    let qty = d.qty.unwrap_or(1.0) * multiplier;
    let fee = d.fee_bp.unwrap_or(4.5) / 10_000.0;
    let pnl = |exit: f64| dir * (exit - pe) * qty - (pe + exit) * qty * fee;
    let (tp, sp) = (dir * (pt - pe) / pe * 100.0, dir * (ps - pe) / pe * 100.0);
    Some(PosEstimate { target_pct: tp, stop_pct: sp, rr: if sp.abs() > 0.0 { tp / -sp } else { f64::INFINITY }, win: pnl(pt), loss: pnl(ps) })
}

/// 对象 id：毫秒时间戳 + 进程内序号（同一毫秒里连建几个也不撞）。
fn new_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    format!("d{ms:x}-{}", SEQ.fetch_add(1, Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_fields_survive_and_defaults_fill_in() {
        let j = r#"{"id":"x","kind":"trend","anchors":[{"t":1,"p":2},{"t":3,"p":4}],"visible_tfs":[60],"style":{"width":3.0}}"#;
        let d: Drawing = serde_json::from_str(j).unwrap();
        assert_eq!((d.kind, d.style.width, d.style.color, d.source), (Kind::Trend, 3.0, Style::default().color, Source::Manual));
        let back = serde_json::to_value(&d).unwrap();
        assert_eq!(back["visible_tfs"], serde_json::json!([60]), "新版本的字段存回去不能丢");
    }

    #[test]
    fn price_and_time_text_round_trip_without_floats() {
        assert_eq!(exchange::unit::Price::from_units(PRICE_SCALE).to_f32_lossy(), 1.0, "刻度与 Price 一致");
        assert_eq!(parse_price("0.3640"), Some(36_400_000_000));
        assert_eq!(parse_price("63445.1"), Some(6_344_510_000_000_000));
        assert_eq!(parse_price("-2.5"), Some(-250_000_000_000));
        assert_eq!((parse_price("abc"), parse_price("1.2.3"), parse_price("")), (None, None, None));
        assert_eq!(price_text(36_400_000_000, 4), "0.3640");
        assert_eq!(price_text(36_405_000_000, 3), "0.364", "四舍五入到 3 位：0.36405 → 0.364（第 4 位是 0）");
        assert_eq!(price_text(36_450_000_000, 3), "0.365");
        assert_eq!(price_text(6_344_510_000_000_000, 1), "63445.1");
        assert_eq!(price_text(-250_000_000_000, 0), "-3");
        assert_eq!(parse_time("2026-10-15 04:30"), Some(1_792_038_600_000));
        assert_eq!(time_text(1_792_038_600_000), "2026-10-15 04:30:00");
        assert_eq!(parse_time("10/15"), None);
    }

    #[test]
    fn timeframe_categories_and_visibility() {
        assert_eq!((tf_cat(None), tf_cat(Some(100)), tf_cat(Some(60_000)), tf_cat(Some(14_400_000)), tf_cat(Some(86_400_000))), ("tick", "sec", "min", "hour", "day"));
        let mut d = Drawing::new(Kind::HLine, vec![Anchor { t: 1, p: 1 }]);
        assert!(d.shown_on("min"));
        d.hide_on.push("min".into());
        assert!(!d.shown_on("min") && d.shown_on("hour"));
        let j = serde_json::to_value(&Drawing::new(Kind::HLine, vec![Anchor { t: 1, p: 1 }])).unwrap();
        assert!(j.get("group").is_none() && j.get("hide_on").is_none(), "没设的不写进库，G0 的对象原样");
    }

    #[test]
    fn position_tool_defaults_constraint_and_estimate() {
        let e = Anchor { t: 1_000, p: 100 * PRICE_SCALE };
        let a = position_anchors(Kind::LongPos, e, 5_000, PRICE_SCALE / 100);
        assert_eq!((a[1].p, a[2].p, a[1].t, a[2].t), (102 * PRICE_SCALE, 99 * PRICE_SCALE, 5_000, 5_000), "目标 +2%、止损 −1%");
        let s = position_anchors(Kind::ShortPos, e, 5_000, PRICE_SCALE / 100);
        assert_eq!((s[1].p, s[2].p), (98 * PRICE_SCALE, 101 * PRICE_SCALE));
        let mut d = Drawing::new(Kind::LongPos, a);
        d.anchors[2].t = 9_000;
        d.constrain(2);
        assert_eq!(d.anchors[1].t, 9_000, "目标与止损同一个右边界");
        d.qty = Some(2.0);
        d.fee_bp = Some(5.0);
        let est = estimate(&d, 1.0).unwrap();
        assert!((est.target_pct - 2.0).abs() < 1e-9 && (est.stop_pct + 1.0).abs() < 1e-9 && (est.rr - 2.0).abs() < 1e-9);
        // 到目标：(102 − 100) × 2 − (100 + 102) × 2 × 0.0005 = 4 − 0.202
        assert!((est.win - 3.798).abs() < 1e-9, "{}", est.win);
        assert!((est.loss - (-2.0 - 0.199)).abs() < 1e-9, "到止损：−1 × 2 − (100 + 99) × 2 × 0.0005；{}", est.loss);
        let mut short = Drawing::new(Kind::ShortPos, s);
        short.fee_bp = Some(0.0);
        let es = estimate(&short, 10.0).unwrap();
        assert!((es.win - 20.0).abs() < 1e-9 && (es.loss + 10.0).abs() < 1e-9, "空头按合约乘数 10：目标赚 2×10，止损亏 1×10");
        assert_eq!(Drawing::new(Kind::FibRetrace, vec![e, e]).fib_levels().len(), 7);
        assert_eq!(Drawing::new(Kind::Text, vec![e]).text.as_deref(), Some("文字"));
    }

    #[test]
    fn origin_timeframe_tag_and_only_origin() {
        let mut d = Drawing::new(Kind::HLine, vec![Anchor { t: 1, p: 1 }]);
        assert!(d.visible_in("min", "1m") && d.origin_tag("1m").is_none(), "没记来源周期（G2 之前画的）：处处显示、不标");
        d.origin_tf = Some("15m".into());
        assert_eq!((d.origin_tag("1m"), d.origin_tag("15m")), (Some("15m"), None));
        d.only_origin = true;
        assert!(d.visible_in("min", "15m") && !d.visible_in("min", "1m") && !d.visible_in("tick", "50T"));
        let j = serde_json::to_value(&Drawing::new(Kind::HLine, vec![Anchor { t: 1, p: 1 }])).unwrap();
        assert!(j.get("only_origin").is_none() && j.get("origin_tf").is_none());
    }

    #[test]
    fn volume_profile_poc_and_value_area() {
        // 三根 K 线：两根在 100–110（各 10），一根在 110–120（量 30）→ POC 在上半段
        let bars = [(1, 100, 110, 10.0), (2, 100, 110, 10.0), (3, 110, 120, 30.0)];
        let p = volume_profile(&bars, 4, 0.7).unwrap();
        assert_eq!(p.bins.len(), 4);
        let total: f64 = p.bins.iter().map(|b| b.2).sum();
        assert!((total - 50.0).abs() < 1e-9, "均摊后总量不变");
        assert!(p.poc >= 2, "量大的那根在上半段：{p:?}");
        assert!(p.vah == 120 && p.val >= 100 && p.val < 115, "{p:?}");
        assert!(volume_profile(&[], 4, 0.7).is_none());
        assert!(volume_profile(&[(1, 5, 5, 0.0)], 4, 0.7).is_none(), "没有量");
        // 单价位的 K 线（高 = 低）落进它所在的那一档
        assert!(volume_profile(&[(1, 100, 100, 3.0), (2, 100, 104, 1.0)], 4, 0.7).is_some());
    }

    #[test]
    fn alert_levels_and_crossing() {
        let h = Drawing::new(Kind::HLine, vec![Anchor { t: 0, p: 100 }]);
        assert_eq!(h.level_at(123), Some(100));
        let t = Drawing::new(Kind::Trend, vec![Anchor { t: 1_000, p: 100 }, Anchor { t: 3_000, p: 300 }]);
        assert_eq!((t.level_at(2_000), t.level_at(500), t.level_at(4_000)), (Some(200), None, None), "趋势线只在两点之间");
        let r = Drawing { kind: Kind::Ray, ..t.clone() };
        assert_eq!((r.level_at(4_000), r.level_at(500)), (Some(400), None), "射线往后延伸");
        assert_eq!(Drawing::new(Kind::Rect, vec![Anchor { t: 0, p: 1 }]).level_at(0), None);
        assert!(AlertWhen::Up.hit(99, 100, 100) && !AlertWhen::Up.hit(101, 99, 100));
        assert!(AlertWhen::Down.hit(101, 100, 100) && !AlertWhen::Down.hit(99, 101, 100));
        assert!(AlertWhen::Cross.hit(99, 101, 100) && AlertWhen::Cross.hit(101, 99, 100) && !AlertWhen::Cross.hit(100, 100, 100));
        let j: Alert = serde_json::from_str(r#"{"when":"up"}"#).unwrap();
        assert!(j.armed && !j.once, "缺省：开着、不止一次");
        assert!(serde_json::from_str::<Drawing>(r#"{"id":"x","kind":"tool_from_the_future","anchors":[]}"#).is_err(), "不认识的种类读不进来（整条跳过，库里不动）");
        assert_eq!(serde_json::from_str::<Drawing>(r#"{"id":"x","kind":"arrow","anchors":[]}"#).unwrap().kind, Kind("arrow"));
    }

    #[test]
    fn new_drawing_has_the_right_number_of_anchors() {
        let a = Anchor { t: 1, p: 2 };
        assert_eq!(Drawing::new(Kind::Rect, vec![a]).anchors, vec![a, a]);
        assert_eq!(Drawing::new(Kind::HLine, vec![a, Anchor { t: 5, p: 6 }]).anchors, vec![a]);
        assert_ne!(Drawing::new(Kind::HLine, vec![a]).id, Drawing::new(Kind::HLine, vec![a]).id);
    }
}
