//! 策略中心 · 对比与组合（docs/37 P5）：对比篮（可跨策略）→ 统计并排、收益曲线叠加、相关矩阵、组合曲线。
//!
//! 计算在 `python -m factory.lab compare … --json`（分桶、相关、等权 / 逆波动 / ERC 配权**只在 Python 一处**），
//! 这里只放数据结构、对比篮规则与一张多序列折线图。对比只读，不进闸门。

use iced::widget::canvas::{self, Cache, Frame, Geometry, Path, Stroke, Text};
use iced::{Color, Point, Rectangle, Renderer, Theme, mouse};
use serde::Deserialize;
use serde_json::Value;

/// 对比篮最多放几次运行（再多相关矩阵就看不清了）。
pub const MAX_BASKET: usize = 8;

/// 分桶（与 factory.lab.compare.FREQS 一致；auto = 共同覆盖够 20 天按日，否则按小时 / 5 分钟）。
pub const FREQS: [(&str, &str); 4] = [("auto", "自动"), ("D", "按日"), ("h", "按小时"), ("5min", "按 5 分钟")];

/// 对比篮里的一次运行。
#[derive(Debug, Clone, PartialEq)]
pub struct BasketItem {
    pub run_id: String,
    pub strategy: String,
    pub created_ts: i64,
    pub brief: String,
}

/// 放进 / 拿出对比篮。返回提示（满了、已有）——`None` 表示正常切换。
pub fn toggle(basket: &mut Vec<BasketItem>, item: BasketItem) -> Option<String> {
    if let Some(i) = basket.iter().position(|b| b.run_id == item.run_id) {
        basket.remove(i);
        return None;
    }
    if basket.len() >= MAX_BASKET {
        return Some(format!("对比篮最多 {MAX_BASKET} 次运行，先拿掉几个"));
    }
    basket.push(item);
    None
}

/// 对比篮的「指纹」：变了才需要重算。
pub fn basket_key(basket: &[BasketItem], freq: &str) -> String {
    let mut k: Vec<&str> = basket.iter().map(|b| b.run_id.as_str()).collect();
    k.push(freq);
    k.join("|")
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct CmpRun {
    pub run_id: String,
    pub strategy: String,
    pub engine: String,
    pub basis: String,
    /// `pct` 账户收益（复利 %）/ `bp` 快速回测每笔名义（累加 bp）
    pub unit: String,
    #[serde(default)]
    pub params: Value,
    pub start: String,
    pub end: String,
    #[serde(default)]
    pub summary: Value,
    pub periods: u64,
    pub total: Option<f64>,
    pub vol_ann: Option<f64>,
    pub sharpe: Option<f64>,
    pub max_dd: Option<f64>,
}

impl CmpRun {
    pub fn unit_label(&self) -> &'static str {
        if self.unit == "bp" { "bp" } else { "%" }
    }
    /// 运行自己汇总里的最大回撤（不分桶，%）——分桶回撤会抹掉桶内的回撤，两个都要看。
    pub fn run_dd_pct(&self) -> Option<f64> {
        self.summary.get("max_dd_pct").and_then(Value::as_f64)
    }
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Curves {
    pub t: Vec<i64>,
    pub series: Vec<Vec<Option<f64>>>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Corr {
    pub matrix: Vec<Vec<Option<f64>>>,
    pub overlap: Vec<Vec<u64>>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Method {
    pub key: String,
    pub label: String,
    pub weights: Vec<f64>,
    pub unit: String,
    pub total: Option<f64>,
    pub vol_ann: Option<f64>,
    pub sharpe: Option<f64>,
    pub max_dd: Option<f64>,
    pub diversification: Option<f64>,
    pub curve: Vec<f64>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Portfolio {
    pub periods: u64,
    #[serde(default)]
    pub methods: Vec<Method>,
    #[serde(default)]
    pub t: Vec<i64>,
    #[serde(default)]
    pub why: String,
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub end: Option<String>,
}

/// `factory.lab compare --json` 的结果。
#[derive(Deserialize, Clone, Debug, Default)]
pub struct Cmp {
    pub freq: String,
    pub freq_label: String,
    pub runs: Vec<CmpRun>,
    pub curves: Curves,
    pub corr: Corr,
    pub portfolio: Portfolio,
    #[serde(default)]
    pub warnings: Vec<String>,
}

// ── 多序列折线图 ───────────────────────────────────────────────────────

const ML: f32 = 52.0;
const MB: f32 = 16.0;
const MT: f32 = 14.0;
const MR: f32 = 8.0;

/// 几条序列共用一个时间轴（`None` = 这条在该时刻没有数据，断开不连线）。
pub struct MultiLine {
    pub t: Vec<i64>,
    pub series: Vec<(Vec<Option<f64>>, Color, String)>,
    pub unit: String,
    pub cache: Cache,
}

fn fmt_v(v: f64) -> String {
    if v.abs() >= 1000.0 { format!("{v:.0}") } else if v.abs() >= 10.0 { format!("{v:.1}") } else { format!("{v:.2}") }
}

fn fmt_t(ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms).map(|d| d.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default()
}

impl MultiLine {
    fn plot(&self, b: Rectangle) -> Option<super::chart_kit::Plot> {
        let vals = self.series.iter().flat_map(|(s, _, _)| s.iter().flatten().copied());
        let (mut lo, mut hi) = vals.fold((0.0f64, 0.0f64), |(a, b), v| (a.min(v), b.max(v)));
        if self.t.len() < 2 {
            return None;
        }
        if (hi - lo).abs() < 1e-12 {
            hi = lo + 1.0;
        }
        let pad = (hi - lo) * 0.05;
        lo -= pad;
        hi += pad;
        Some(super::chart_kit::Plot {
            x0: ML,
            y0: MT,
            w: (b.width - ML - MR).max(1.0),
            h: (b.height - MT - MB).max(1.0),
            xr: (self.t[0] as f64, *self.t.last()? as f64),
            yr: (lo, hi),
        })
    }
}

impl<M> canvas::Program<M> for MultiLine {
    type State = ();
    fn draw(&self, _s: &(), r: &Renderer, _t: &Theme, b: Rectangle, cursor: mouse::Cursor) -> Vec<Geometry> {
        let Some(plot) = self.plot(b) else {
            let mut f = Frame::new(r, b.size());
            f.fill_text(Text { content: "数据不足".into(), position: Point::new(ML, b.height / 2.0 - 6.0), color: crate::ui::pal::dim(), ..Default::default() });
            return vec![f.into_geometry()];
        };
        let sy = |v: f64| plot.y0 + ((plot.yr.1 - v) / (plot.yr.1 - plot.yr.0)) as f32 * plot.h;
        let geo = self.cache.draw(r, b.size(), |frame: &mut Frame| {
            let grid = Stroke::default().with_width(1.0).with_color(crate::ui::pal::grid());
            for i in 0..=4 {
                let v = plot.yr.0 + (plot.yr.1 - plot.yr.0) * f64::from(i) / 4.0;
                let y = sy(v);
                frame.stroke(&Path::line(Point::new(plot.x0, y), Point::new(plot.x0 + plot.w, y)), grid);
                frame.fill_text(Text { content: fmt_v(v), position: Point::new(2.0, y - 5.0), color: crate::ui::pal::axis(), size: iced::Pixels(9.0), ..Default::default() });
            }
            // 零线：比较「是否赚钱」的基准
            if plot.yr.0 < 0.0 && plot.yr.1 > 0.0 {
                frame.stroke(&Path::line(Point::new(plot.x0, sy(0.0)), Point::new(plot.x0 + plot.w, sy(0.0))),
                             Stroke::default().with_width(1.0).with_color(crate::ui::pal::reference()));
            }
            frame.fill_text(Text { content: self.unit.clone(), position: Point::new(2.0, 0.0), color: crate::ui::pal::axis(), size: iced::Pixels(9.0), ..Default::default() });
            for (k, ms) in [(0usize, self.t[0]), (1, *self.t.last().unwrap_or(&0))] {
                let s = fmt_t(ms);
                let x = if k == 0 { plot.x0 } else { plot.x0 + plot.w - s.len() as f32 * 5.2 };
                frame.fill_text(Text { content: s, position: Point::new(x, plot.y0 + plot.h + 3.0), color: crate::ui::pal::axis(), size: iced::Pixels(9.0), ..Default::default() });
            }
            for (s, c, _) in &self.series {
                let path = Path::new(|p| {
                    let mut pen = false;
                    for (i, v) in s.iter().enumerate() {
                        let Some(v) = v else {
                            pen = false;
                            continue;
                        };
                        let pt = Point::new(plot.sx(self.t[i] as f64), sy(*v));
                        if pen { p.line_to(pt) } else { p.move_to(pt) }
                        pen = true;
                    }
                });
                frame.stroke(&path, Stroke::default().with_width(1.5).with_color(*c));
            }
        });
        let mut out = vec![geo];
        if let Some(p) = cursor.position_in(b) {
            let mut hair = Frame::new(r, b.size());
            let x: Vec<f64> = self.t.iter().map(|t| *t as f64).collect();
            let mut extra = Vec::new();
            if let Some(i) = super::chart_kit::nearest(&x, plot.vx(p.x)) {
                for (s, _, name) in &self.series {
                    if let Some(Some(v)) = s.get(i) {
                        extra.push(format!("{name} {}", fmt_v(*v)));
                    }
                }
            }
            super::chart_kit::crosshair(&mut hair, &plot, p, |v| fmt_t(v as i64), fmt_v, &extra);
            out.push(hair.into_geometry());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn it(id: &str) -> BasketItem {
        BasketItem { run_id: id.into(), strategy: "x.y".into(), created_ts: 0, brief: String::new() }
    }

    #[test]
    fn basket_toggles_and_caps() {
        let mut b = Vec::new();
        assert!(toggle(&mut b, it("R-1")).is_none());
        assert!(toggle(&mut b, it("R-1")).is_none());
        assert!(b.is_empty(), "再点一次是拿出来");
        for i in 0..MAX_BASKET {
            toggle(&mut b, it(&format!("R-{i}")));
        }
        assert!(toggle(&mut b, it("R-x")).unwrap().contains("最多"));
        assert_eq!(b.len(), MAX_BASKET);
        assert_ne!(basket_key(&b, "auto"), basket_key(&b, "D"));
    }

    #[test]
    fn parses_compare_json() {
        let j = r#"{"freq":"D","freq_label":"按日","per_year":365,
          "runs":[{"run_id":"R-a","strategy":"x.y","engine":"quick","basis":"quick_bp","unit":"bp","params":{},
                   "start":"2026-06-01","end":"2026-06-20","summary":{"max_dd_pct":null},"periods":20,
                   "total":-120.5,"vol_ann":3.0,"sharpe":-2.1,"max_dd":-130.0}],
          "curves":{"t":[1,2],"series":[[null,1.5]]},
          "corr":{"matrix":[[1.0]],"overlap":[[20]]},
          "portfolio":{"periods":3,"methods":[],"t":[],"why":"x"},"warnings":["w"]}"#;
        let c: Cmp = serde_json::from_str(j).unwrap();
        assert_eq!(c.runs[0].unit_label(), "bp");
        assert_eq!(c.runs[0].run_dd_pct(), None);
        assert_eq!(c.curves.series[0], vec![None, Some(1.5)]);
    }
}
