//! 回放的条件跳转（docs/51 P5）：`obi_l5@0 > 0.3 and spread_bps@0 < 1` 这样的条件，按回放时间线的特征栅格逐格求值，
//! 找它「由不成立变成立」的那一格。
//!
//! 写法：
//!
//! | 写法 | 意思 |
//! |---|---|
//! | `obi_l5@0`、`trade_imbalance@5000` | 特征列：字典 key + 窗口毫秒（与时间线的列名一字不差） |
//! | `obi_l5` | 只写 key：取这条特征最小的窗口 |
//! | `ofms:residual_z`、`ofms:regime` | OFMS 状态列（编码见时间线抬头的 `ofms_codes`） |
//! | `0.3`、`-1e-3` | 数 |
//! | `<` `<=` `>` `>=` `==` `!=` | 比较（两边是列或数） |
//! | `and` `or` `not`、`(` `)` | 逻辑（也认 `&&` `||` `!`） |
//!
//! 质量不可用（STALE / INVALID / UNAVAILABLE 等，编码 ≥ 2）的格当 NaN；**NaN 参与比较一律不成立**——预热期间条件不会意外成立。

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
enum Operand {
    Col(usize),
    Num(f64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
}

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Or(Vec<Node>),
    And(Vec<Node>),
    Not(Box<Node>),
    Cmp(Operand, Op, Operand),
}

/// 解析好的条件。
#[derive(Debug, Clone, PartialEq)]
pub struct Cond {
    root: Node,
    /// 用到的列（时间线栅格里的下标，去重、升序）
    pub cols: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CondError(pub String);

impl fmt::Display for CondError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Num(f64),
    Op(Op),
    And,
    Or,
    Not,
    LParen,
    RParen,
}

fn ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '@' | ':' | '.')
}

fn lex(s: &str) -> Result<Vec<Tok>, CondError> {
    let cs: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        let two: String = cs[i..(i + 2).min(cs.len())].iter().collect();
        let (tok, n) = match two.as_str() {
            "<=" => (Tok::Op(Op::Le), 2),
            ">=" => (Tok::Op(Op::Ge), 2),
            "==" => (Tok::Op(Op::Eq), 2),
            "!=" => (Tok::Op(Op::Ne), 2),
            "&&" => (Tok::And, 2),
            "||" => (Tok::Or, 2),
            _ => match c {
                '<' => (Tok::Op(Op::Lt), 1),
                '>' => (Tok::Op(Op::Gt), 1),
                '=' => (Tok::Op(Op::Eq), 1),
                '!' => (Tok::Not, 1),
                '(' => (Tok::LParen, 1),
                ')' => (Tok::RParen, 1),
                _ => {
                    // 数（含负号与科学计数法）或名字
                    let num_start = c.is_ascii_digit() || (c == '-' || c == '.') && cs.get(i + 1).is_some_and(|d| d.is_ascii_digit() || *d == '.');
                    let mut j = i + 1;
                    if num_start {
                        while j < cs.len() && (cs[j].is_ascii_digit() || matches!(cs[j], '.' | 'e' | 'E') || (matches!(cs[j], '-' | '+') && matches!(cs[j - 1], 'e' | 'E'))) {
                            j += 1;
                        }
                        let t: String = cs[i..j].iter().collect();
                        let v: f64 = t.parse().map_err(|_| CondError(format!("「{t}」不是一个数")))?;
                        out.push(Tok::Num(v));
                        i = j;
                        continue;
                    }
                    if !ident_char(c) {
                        return Err(CondError(format!("认不出「{c}」")));
                    }
                    while j < cs.len() && ident_char(cs[j]) {
                        j += 1;
                    }
                    let t: String = cs[i..j].iter().collect();
                    out.push(match t.to_ascii_lowercase().as_str() {
                        "and" => Tok::And,
                        "or" => Tok::Or,
                        "not" => Tok::Not,
                        _ => Tok::Ident(t),
                    });
                    i = j;
                    continue;
                }
            },
        };
        out.push(tok);
        i += n;
    }
    Ok(out)
}

/// 列名 → 下标。只写 key 时取最小窗口；找不到时报出相近的列。
fn resolve(name: &str, columns: &[String]) -> Result<usize, CondError> {
    if let Some(i) = columns.iter().position(|c| c == name) {
        return Ok(i);
    }
    if !name.contains('@') && !name.contains(':') {
        let prefix = format!("{name}@");
        let best = columns
            .iter()
            .enumerate()
            .filter_map(|(i, c)| c.strip_prefix(&prefix).and_then(|w| w.parse::<u64>().ok()).map(|w| (w, i)))
            .min();
        if let Some((_, i)) = best {
            return Ok(i);
        }
    }
    let key = name.split('@').next().unwrap_or(name);
    let near: Vec<&str> = columns.iter().filter(|c| c.contains(key)).take(6).map(String::as_str).collect();
    Err(CondError(if near.is_empty() {
        format!("这次回放的启用集里没有「{name}」")
    } else {
        format!("没有「{name}」，相近的有：{}", near.join("、"))
    }))
}

struct Parser<'a> {
    t: Vec<Tok>,
    i: usize,
    cols: &'a [String],
    used: Vec<usize>,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }

    fn or(&mut self) -> Result<Node, CondError> {
        let mut v = vec![self.and()?];
        while self.peek() == Some(&Tok::Or) {
            self.i += 1;
            v.push(self.and()?);
        }
        Ok(if v.len() == 1 { v.remove(0) } else { Node::Or(v) })
    }

    fn and(&mut self) -> Result<Node, CondError> {
        let mut v = vec![self.not()?];
        while self.peek() == Some(&Tok::And) {
            self.i += 1;
            v.push(self.not()?);
        }
        Ok(if v.len() == 1 { v.remove(0) } else { Node::And(v) })
    }

    fn not(&mut self) -> Result<Node, CondError> {
        if self.peek() == Some(&Tok::Not) {
            self.i += 1;
            return Ok(Node::Not(Box::new(self.not()?)));
        }
        if self.peek() == Some(&Tok::LParen) {
            self.i += 1;
            let n = self.or()?;
            if self.peek() != Some(&Tok::RParen) {
                return Err(CondError("括号没有配对".into()));
            }
            self.i += 1;
            return Ok(n);
        }
        let a = self.operand()?;
        let Some(Tok::Op(op)) = self.peek().cloned() else {
            return Err(CondError("每一段都要是比较：「列 比较 数」或「列 比较 列」".into()));
        };
        self.i += 1;
        let b = self.operand()?;
        if matches!((&a, &b), (Operand::Num(_), Operand::Num(_))) {
            return Err(CondError("两边都是数的比较没有意义".into()));
        }
        Ok(Node::Cmp(a, op, b))
    }

    fn operand(&mut self) -> Result<Operand, CondError> {
        match self.peek().cloned() {
            Some(Tok::Num(v)) => {
                self.i += 1;
                Ok(Operand::Num(v))
            }
            Some(Tok::Ident(name)) => {
                self.i += 1;
                let c = resolve(&name, self.cols)?;
                self.used.push(c);
                Ok(Operand::Col(c))
            }
            Some(t) => Err(CondError(format!("这里应该是列名或数，却是 {t:?}"))),
            None => Err(CondError("条件没写完".into())),
        }
    }
}

impl Cond {
    /// 按这次回放时间线的列解析。
    ///
    /// # Errors
    /// 写法不对、列不存在（报出相近的列）。
    pub fn parse(text: &str, columns: &[String]) -> Result<Self, CondError> {
        if text.trim().is_empty() {
            return Err(CondError("条件是空的".into()));
        }
        let mut p = Parser { t: lex(text)?, i: 0, cols: columns, used: Vec::new() };
        let root = p.or()?;
        if p.i != p.t.len() {
            return Err(CondError(format!("多出来的部分：{:?}", &p.t[p.i..])));
        }
        let mut cols = p.used;
        cols.sort_unstable();
        cols.dedup();
        Ok(Self { root, cols })
    }

    /// 一格上成立吗。`val(列下标)` 给这一格的值（质量不够的给 NaN）。
    pub fn eval(&self, val: &dyn Fn(usize) -> f64) -> bool {
        fn go(n: &Node, val: &dyn Fn(usize) -> f64) -> bool {
            match n {
                Node::Or(v) => v.iter().any(|x| go(x, val)),
                Node::And(v) => v.iter().all(|x| go(x, val)),
                Node::Not(x) => !go(x, val),
                Node::Cmp(a, op, b) => {
                    let g = |o: &Operand| match o {
                        Operand::Col(c) => val(*c),
                        Operand::Num(v) => *v,
                    };
                    let (x, y) = (g(a), g(b));
                    if x.is_nan() || y.is_nan() {
                        return false;
                    }
                    match op {
                        Op::Lt => x < y,
                        Op::Le => x <= y,
                        Op::Gt => x > y,
                        Op::Ge => x >= y,
                        Op::Eq => x == y,
                        Op::Ne => x != y,
                    }
                }
            }
        }
        go(&self.root, val)
    }
}

/// 在名字列表里找以 `partial` 打头 / 含它的列（输入时的提示），至多 `n` 个。
#[must_use]
pub fn suggest<'a>(partial: &str, columns: &'a [String], n: usize) -> Vec<&'a str> {
    if partial.len() < 2 {
        return Vec::new();
    }
    let mut v: Vec<&str> = columns.iter().filter(|c| c.starts_with(partial)).map(String::as_str).collect();
    v.extend(columns.iter().filter(|c| !c.starts_with(partial) && c.contains(partial)).map(String::as_str));
    v.truncate(n);
    v
}

/// 输入框里正在写的最后一个名字（给 [`suggest`]）。
#[must_use]
pub fn last_ident(text: &str) -> &str {
    let start = text.char_indices().rev().find(|(_, c)| !ident_char(*c)).map_or(0, |(i, c)| i + c.len_utf8());
    &text[start..]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols() -> Vec<String> {
        ["obi_l5@0", "obi_l5@1000", "spread_bps@1000", "spread_bps@0", "ofms:residual_z"].iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn 解析与求值() {
        let c = cols();
        let k = Cond::parse("obi_l5@0 > 0.3 and (spread_bps < 1 or not ofms:residual_z >= -2e0)", &c).unwrap();
        assert_eq!(k.cols, vec![0, 3, 4], "只写 key 取最小窗口：spread_bps → @0");
        let row = |v: [f64; 5]| move |i: usize| v[i];
        assert!(k.eval(&row([0.4, 0.0, 9.0, 0.5, 0.0])));
        assert!(!k.eval(&row([0.2, 0.0, 9.0, 0.5, 0.0])));
        assert!(k.eval(&row([0.4, 0.0, 9.0, 5.0, -3.0])), "not 那一支");
        assert!(!k.eval(&row([f64::NAN, 0.0, 9.0, 0.5, 0.0])), "NaN 不成立");
        let k = Cond::parse("obi_l5@0 > obi_l5@1000 && spread_bps@1000 != 0", &c).unwrap();
        assert!(k.eval(&row([0.5, 0.1, 1.0, 0.0, 0.0])));
        assert!(Cond::parse("-0.5 < obi_l5@0", &c).unwrap().eval(&row([0.0; 5])));
    }

    #[test]
    fn 写错了说清楚() {
        let c = cols();
        assert!(Cond::parse("obi_l9 > 0", &c).unwrap_err().0.contains("没有"));
        assert!(Cond::parse("obi > 0", &c).unwrap_err().0.contains("obi_l5@0"), "报出相近的列");
        assert!(Cond::parse("obi_l5@0", &c).unwrap_err().0.contains("比较"));
        assert!(Cond::parse("(obi_l5@0 > 1", &c).unwrap_err().0.contains("括号"));
        assert!(Cond::parse("1 < 2", &c).is_err());
        assert!(Cond::parse("", &c).is_err());
        assert!(Cond::parse("obi_l5@0 > 1 obi_l5@0", &c).unwrap_err().0.contains("多出来"));
    }

    #[test]
    fn 输入提示() {
        let c = cols();
        assert_eq!(last_ident("obi_l5@0 > 0.3 and spr"), "spr");
        assert_eq!(suggest("spr", &c, 5), vec!["spread_bps@1000", "spread_bps@0"]);
        assert!(suggest("s", &c, 5).is_empty());
    }
}
