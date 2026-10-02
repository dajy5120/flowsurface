//! 数据语义（UPDS V8，docs/35 §7）：屏幕上一个数字到底是什么意思。
//!
//! - **六种「无」**，六个符号，永远不留空格子：零 `0.00`、缺失 `—`、不适用 `n/a`、
//!   未知 `···`、屏蔽 `•••`、错误 `!`。缺失绝不显示成 0。
//! - **舍入**：金额「远离零」，测量值「银行家舍入」（半数取偶）。一列一种小数位。
//! - **大数**只在 ≥ 10⁶ 时缩写（`7.99 M`），完整值留给悬停与复制。
//! - **来源标记**是排版上的，不只靠颜色：派生前导「·」、人工覆盖前导「^」、模拟前导「~」……
//! - **标识符自然排序**：`BTC-2` 在 `BTC-10` 前；缺失值永远排最后。

use std::cmp::Ordering;

use super::tokens;

/// 六种「无」（UPDS V8 §68）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Absence {
    /// 还没有值（窗口未满、数据未到）
    Missing,
    /// 这一行不适用（现货没有资金费率）
    NotApplicable,
    /// 还没取到（快照没读到、守护没起）
    Unknown,
    /// 有值但被屏蔽（隐藏数值模式）
    Withheld,
    /// 算出错 / 值违反自身类型——同时要进底部「问题」
    Invalid,
}

impl Absence {
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Missing => tokens::ABSENT_MISSING,
            Self::NotApplicable => tokens::ABSENT_NOT_APPLICABLE,
            Self::Unknown => tokens::ABSENT_UNKNOWN,
            Self::Withheld => tokens::ABSENT_WITHHELD,
            Self::Invalid => tokens::ABSENT_INVALID,
        }
    }
}

/// 六种「无」的符号，面板里**不再手写**（docs/35 §16.15 第 5 项）：
/// 缺失 `—`（窗口未满、数据没到）· 不适用 `n/a`（这一行本来就没有这个量）·
/// 未取到 `···`（快照还没读到、守护没起）· 出错 `!`（解析失败、非有限值）。
pub const MISSING: &str = tokens::ABSENT_MISSING;
pub const NOT_APPLICABLE: &str = tokens::ABSENT_NOT_APPLICABLE;
pub const UNKNOWN: &str = tokens::ABSENT_UNKNOWN;
pub const INVALID: &str = tokens::ABSENT_INVALID;

pub fn missing() -> String {
    MISSING.to_string()
}
pub fn na() -> String {
    NOT_APPLICABLE.to_string()
}
pub fn unknown() -> String {
    UNKNOWN.to_string()
}
pub fn invalid() -> String {
    INVALID.to_string()
}

/// 出错值：显示 `!`，并**进底部「问题」页**（UPDS V8：错误要同时出现在问题列表里）。
/// 问题页读的是日志里的警告；同一个 `what` 只记一次，面板每帧重画不会刷屏。
pub fn invalid_at(what: &str) -> String {
    static SEEN: std::sync::Mutex<Option<std::collections::HashSet<String>>> = std::sync::Mutex::new(None);
    if let Ok(mut g) = SEEN.lock() {
        let set = g.get_or_insert_with(Default::default);
        if set.len() < 500 && set.insert(what.to_string()) {
            log::warn!("[数据] 出错值（界面显示 {INVALID}）：{what}");
        }
    }
    invalid()
}

/// 单元格级来源（UPDS V8 §72）。标记是排版上的：灰度打印、色弱都看得出来。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Provenance {
    /// 实测：直接来自数据源，无标记
    #[default]
    Measured,
    /// 派生：本地算出来的（特征引擎、图表参数层）
    Derived,
    /// 估算：模型或插值、质量降级
    Estimated,
    /// 人工覆盖（手改口径、强制跑）
    Overridden,
    /// 过期：最后已知值，显示时变暗并带年龄
    Stale,
    /// 模拟：回测、模拟盘、回放——**永远不和实盘数值无标记并列**
    Simulated,
}

impl Provenance {
    /// 前导标记。过期靠变暗 + 年龄、估算靠下划线，这里不加字符。
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Measured | Self::Stale | Self::Estimated => "",
            Self::Derived => "· ",
            Self::Overridden => "^ ",
            Self::Simulated => "~ ",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Measured => "实测",
            Self::Derived => "派生",
            Self::Estimated => "估算",
            Self::Overridden => "人工覆盖",
            Self::Stale => "过期",
            Self::Simulated => "模拟",
        }
    }

    /// 合计继承输入里最弱的那个（UPDS：来源会传递）。枚举顺序即强弱顺序。
    pub fn weakest(it: impl IntoIterator<Item = Self>) -> Self {
        it.into_iter().max().unwrap_or_default()
    }
}

/// 舍入规则（UPDS V8 §69）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rounding {
    /// 金额：半数远离零
    Money,
    /// 测量值：半数取偶
    Measurement,
}

fn round_to(v: f64, dp: usize, mode: Rounding) -> f64 {
    let k = 10f64.powi(dp as i32);
    let x = v * k;
    let r = match mode {
        Rounding::Money => x.round(), // f64::round 本身就是远离零
        Rounding::Measurement => {
            let f = x.floor();
            let diff = x - f;
            if (diff - 0.5).abs() < 1e-9 {
                if f % 2.0 == 0.0 { f } else { f + 1.0 }
            } else {
                x.round()
            }
        }
    };
    r / k
}

/// 千分位分组（空格分组，UPDS 的写法 `1 284 993.50`；等宽字体下列对得齐）。
fn group(int_part: &str) -> String {
    let (sign, digits) = match int_part.strip_prefix('-') {
        Some(d) => ("−", d),
        None => ("", int_part),
    };
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(' ');
        }
        out.push(ch);
    }
    format!("{sign}{out}")
}

/// 按固定小数位格式化一个数；非有限值显示错误符号。负号用真正的减号「−」。
pub fn number(v: f64, dp: usize, mode: Rounding) -> String {
    if !v.is_finite() {
        return Absence::Invalid.glyph().to_string();
    }
    let r = round_to(v, dp, mode);
    let s = format!("{:.*}", dp, r.abs());
    let (i, f) = s.split_once('.').map(|(a, b)| (a, Some(b))).unwrap_or((&s, None));
    let signed = if r < 0.0 { format!("-{i}") } else { i.to_string() };
    let mut out = group(&signed);
    if let Some(f) = f {
        out.push('.');
        out.push_str(f);
    }
    out
}

/// 可能缺失的数：`None` 显示 `—`。
pub fn opt(v: Option<f64>, dp: usize, mode: Rounding) -> String {
    match v {
        Some(x) => number(x, dp, mode),
        None => Absence::Missing.glyph().to_string(),
    }
}

/// 百分比：符号总显示（`+0.49%` / `−1.91%`），输入是比例（0.0049）。
pub fn pct(ratio: f64, dp: usize) -> String {
    if !ratio.is_finite() {
        return Absence::Invalid.glyph().to_string();
    }
    let s = number(ratio * 100.0, dp, Rounding::Measurement);
    if ratio > 0.0 { format!("+{s}%") } else { format!("{s}%") }
}

/// 大数缩写：≥ 10⁶ 才缩（`7.99 M`），以下照常分组。完整值留给悬停与复制。
pub fn compact(v: f64, dp: usize) -> String {
    let a = v.abs();
    let (d, u) = if a >= 1e12 {
        (1e12, " T")
    } else if a >= 1e9 {
        (1e9, " B")
    } else if a >= 1e6 {
        (1e6, " M")
    } else {
        return number(v, dp, Rounding::Measurement);
    };
    format!("{}{u}", number(v / d, 2, Rounding::Measurement))
}

/// 隐藏数值模式下金额 / 持仓的显示（UPDS V7 §57）。
pub fn withheld_if(hide: bool, s: String) -> String {
    if hide { Absence::Withheld.glyph().to_string() } else { s }
}

/// 私密数值（金额、持仓、账户标识）：隐藏数值模式下显示 `•••`（docs/35 §9.3）。
pub fn private(s: impl Into<String>) -> String {
    withheld_if(super::hide_values(), s.into())
}

/// 模拟出来的金额 / 持仓（回测、模拟盘、回放、影子）：前缀 `~`（docs/35 §7.2，UPDS V7 §57——
/// 与实盘数值永远不能无标记地并列）；隐藏数值模式下显示 `•••`。
pub fn sim(s: impl Into<String>) -> String {
    let s = s.into();
    withheld_if(super::hide_values(), format!("{}{s}", Provenance::Simulated.prefix()))
}

/// 自然排序比较（UPDS V8 §71）：数字段按数值比，`CC-2 < CC-10`、`v1.9 < v1.10`；
/// 大小写不敏感，平局再按原文比保证稳定。
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    fn chunks(s: &str) -> Vec<(bool, String)> {
        let mut v: Vec<(bool, String)> = Vec::new();
        for ch in s.chars() {
            let d = ch.is_ascii_digit();
            match v.last_mut() {
                Some((isd, buf)) if *isd == d => buf.push(ch),
                _ => v.push((d, ch.to_string())),
            }
        }
        v
    }
    let (ca, cb) = (chunks(a), chunks(b));
    for (x, y) in ca.iter().zip(cb.iter()) {
        let o = match (x.0, y.0) {
            (true, true) => {
                let (xa, ya) = (x.1.trim_start_matches('0'), y.1.trim_start_matches('0'));
                xa.len().cmp(&ya.len()).then_with(|| xa.cmp(ya))
            }
            _ => x.1.to_lowercase().cmp(&y.1.to_lowercase()),
        };
        if o != Ordering::Equal {
            return o;
        }
    }
    ca.len().cmp(&cb.len()).then_with(|| a.cmp(b))
}

/// 可能缺失的数比较：缺失**永远排最后**，与升降序无关（UPDS V8 §71）。
/// `desc` 只作用于有值的部分。
pub fn cmp_opt(a: Option<f64>, b: Option<f64>, desc: bool) -> Ordering {
    match (a, b) {
        (Some(x), Some(y)) => {
            let o = x.partial_cmp(&y).unwrap_or(Ordering::Equal);
            if desc { o.reverse() } else { o }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// 长标识符中间省略（`0a435104…1b32918c`），保留首尾，永不截掉开头（UPDS V8 §71）。
pub fn elide_middle(s: &str, max_chars: usize) -> String {
    let n = s.chars().count();
    if n <= max_chars || max_chars < 5 {
        return s.to_string();
    }
    let keep = (max_chars - 1) / 2;
    let head: String = s.chars().take(keep).collect();
    let tail: String = s.chars().skip(n - keep).collect();
    format!("{head}…{tail}")
}

/// 按显示宽度省略中间：全角字（中文等）算 2 个单位、半角算 1 个。
/// 只数字符数时，中文多的标识符会被低估宽度而折行（docs/35 §16.15 第 20 项超长值场景实测）。
pub fn elide_middle_width(s: &str, max_units: usize) -> String {
    let unit = |c: char| if c.is_ascii() { 1 } else { 2 };
    let total: usize = s.chars().map(unit).sum();
    if total <= max_units || max_units < 5 {
        return s.to_string();
    }
    let half = (max_units - 1) / 2;
    let (mut head, mut used) = (String::new(), 0);
    for c in s.chars() {
        if used + unit(c) > half {
            break;
        }
        used += unit(c);
        head.push(c);
    }
    let (mut tail, mut used) = (Vec::new(), 0);
    for c in s.chars().rev() {
        if used + unit(c) > half {
            break;
        }
        used += unit(c);
        tail.push(c);
    }
    let tail: String = tail.into_iter().rev().collect();
    format!("{head}…{tail}")
}

#[cfg(test)]
mod tests {

    #[test]
    fn 按宽度省略中间_中文算两格() {
        let s = super::elide_middle_width("超长合约名-PERP-XXXXXXXXXX-尾巴", 16);
        let w: usize = s.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum();
        assert!(w <= 16, "{s} 宽 {w}");
        assert!(s.contains('…') && s.starts_with("超") && s.ends_with("巴"));
        assert_eq!(super::elide_middle_width("BTCUSDT", 16), "BTCUSDT");
    }

    #[test]
    fn 模拟金额带波浪号_隐藏时为三点() {
        // 测试里偏好固定为缺省（不隐藏）
        assert_eq!(sim("1 024.50"), "~ 1 024.50");
        assert_eq!(private("BTC 0.5"), "BTC 0.5");
        assert_eq!(withheld_if(true, sim("1.00")), "•••");
    }

    use super::*;

    #[test]
    fn 舍入_金额远离零_测量半数取偶() {
        assert_eq!(number(1_284_993.4972, 2, Rounding::Money), "1 284 993.50");
        assert_eq!(number(2.5, 0, Rounding::Money), "3");
        assert_eq!(number(-2.5, 0, Rounding::Money), "−3");
        assert_eq!(number(2.5, 0, Rounding::Measurement), "2");
        assert_eq!(number(3.5, 0, Rounding::Measurement), "4");
        assert_eq!(number(-0.0, 2, Rounding::Money), "0.00", "负零不显示减号");
    }

    #[test]
    fn 百分比与缩写() {
        assert_eq!(pct(-0.019137, 2), "−1.91%");
        assert_eq!(pct(0.0049, 2), "+0.49%");
        assert_eq!(compact(7_991_250.0, 0), "7.99 M");
        assert_eq!(compact(999_999.0, 0), "999 999", "10⁶ 以下不缩");
    }

    #[test]
    fn 缺失绝不显示成零() {
        assert_eq!(opt(None, 2, Rounding::Money), "—");
        assert_eq!(opt(Some(0.0), 2, Rounding::Money), "0.00");
        assert_eq!(number(f64::NAN, 2, Rounding::Money), "!");
    }

    #[test]
    fn 自然排序() {
        let mut v = vec!["CC-10", "CC-2", "cc-1", "v1.10", "v1.9"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["cc-1", "CC-2", "CC-10", "v1.9", "v1.10"]);
    }

    #[test]
    fn 缺失永远排最后_两个方向都是() {
        let mut v = vec![Some(2.0), None, Some(5.0), Some(1.0)];
        v.sort_by(|a, b| cmp_opt(*a, *b, false));
        assert_eq!(v, [Some(1.0), Some(2.0), Some(5.0), None]);
        v.sort_by(|a, b| cmp_opt(*a, *b, true));
        assert_eq!(v, [Some(5.0), Some(2.0), Some(1.0), None]);
    }

    #[test]
    fn 来源取最弱_中间省略() {
        assert_eq!(
            Provenance::weakest([Provenance::Measured, Provenance::Simulated, Provenance::Derived]),
            Provenance::Simulated
        );
        assert_eq!(elide_middle("0a435104-2f6e-4cb3-826b-31941b32918c", 17), "0a435104…1b32918c");
        assert_eq!(elide_middle("short", 17), "short");
    }
}
