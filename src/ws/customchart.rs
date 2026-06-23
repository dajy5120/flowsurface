//! 自有数据自适应图（「自有数据回测」工作区左侧，docs/08）。
//!
//! 读用户提供的 **CSV/JSON 数据文件** → 通用自适应图：多列折线 + 散点，横轴时间或数值
//! **按数据自动判定**，横/纵轴范围随数据缩放。与策略运行解耦——纯展示任意二维数据。
//!
//! 数据文件路径：env `WS_SELFDATA_CHART`，否则默认 `<repo>/strategies/data/selfdata.csv`。
//! - CSV：首行表头，第 1 列为 X（数值或时间），其余列各为一条 Y 序列。
//! - JSON：`{"x_label":"t","x_is_time":true,"x":[...],"series":[{"name":"a","v":[...]}]}`。
//! 文件按 mtime 缓存，改动即重载（每帧渲染读快照）。

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use iced::widget::canvas::{self, Cache, Frame, Geometry, Path, Stroke, Text};
use iced::widget::{canvas as canvas_widget, center, column, container, text};
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Size, Theme, mouse};

use crate::style;

const ML: f32 = 52.0; // 左边距（Y 轴标签）
const MR: f32 = 12.0;
const MT: f32 = 12.0;
const MB: f32 = 22.0; // 底边距（X 轴标签）
const C_AXIS: Color = Color { r: 0.5, g: 0.5, b: 0.55, a: 1.0 };
const C_GRID: Color = Color { r: 0.3, g: 0.3, b: 0.34, a: 0.5 };

/// 多列序列调色板。
const PALETTE: [Color; 8] = [
    Color { r: 0.30, g: 0.72, b: 0.47, a: 1.0 },
    Color { r: 0.36, g: 0.62, b: 0.95, a: 1.0 },
    Color { r: 0.92, g: 0.62, b: 0.28, a: 1.0 },
    Color { r: 0.86, g: 0.40, b: 0.45, a: 1.0 },
    Color { r: 0.66, g: 0.50, b: 0.92, a: 1.0 },
    Color { r: 0.30, g: 0.78, b: 0.78, a: 1.0 },
    Color { r: 0.85, g: 0.80, b: 0.35, a: 1.0 },
    Color { r: 0.70, g: 0.70, b: 0.74, a: 1.0 },
];

#[derive(Clone, Default)]
pub struct ChartData {
    pub x_is_time: bool,
    pub x_label: String,
    pub x: Vec<f64>,                     // 数值 X 或时间戳(ms)
    pub series: Vec<(String, Vec<f64>)>, // (列名, y 值；NaN=缺)
    pub source: String,
    pub error: Option<String>,
}

fn data_path() -> PathBuf {
    if let Ok(p) = std::env::var("WS_SELFDATA_CHART") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join("dev/WealthSpring/strategies/data/selfdata.csv")
}

fn parse_time_ms(s: &str) -> Option<f64> {
    use chrono::{DateTime, NaiveDate, NaiveDateTime};
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp_millis() as f64);
    }
    for fmt in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M", "%Y/%m/%d %H:%M:%S", "%Y/%m/%d %H:%M"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(s, fmt) {
            return Some(dt.and_utc().timestamp_millis() as f64);
        }
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return d.and_hms_opt(0, 0, 0).map(|t| t.and_utc().timestamp_millis() as f64);
    }
    None
}

fn parse_csv(text: &str, src: String) -> ChartData {
    let mut d = ChartData { source: src, ..Default::default() };
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let Some(header) = lines.next() else {
        d.error = Some("空文件".into());
        return d;
    };
    let cols: Vec<String> = header.split(',').map(|s| s.trim().to_string()).collect();
    if cols.len() < 2 {
        d.error = Some("至少需 2 列（X + ≥1 个 Y）".into());
        return d;
    }
    d.x_label = cols[0].clone();
    let mut series: Vec<(String, Vec<f64>)> =
        cols[1..].iter().map(|c| (c.clone(), Vec::new())).collect();
    let mut xs_raw: Vec<String> = Vec::new();
    for line in lines {
        let vals: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        if vals.is_empty() {
            continue;
        }
        xs_raw.push(vals.first().copied().unwrap_or("").to_string());
        for (i, s) in series.iter_mut().enumerate() {
            let v = vals.get(i + 1).and_then(|x| x.parse::<f64>().ok()).unwrap_or(f64::NAN);
            s.1.push(v);
        }
    }
    if xs_raw.is_empty() {
        d.error = Some("无数据行".into());
        return d;
    }
    // X 轴判定：纯数值 → 数值轴（列名暗示时间则当时间戳）；否则尝试时间解析；再不行用索引。
    let as_num: Option<Vec<f64>> = xs_raw.iter().map(|s| s.parse::<f64>().ok()).collect();
    let name_is_time = matches!(
        d.x_label.to_lowercase().as_str(),
        "t" | "time" | "ts" | "timestamp" | "date" | "datetime"
    );
    if let Some(nx) = as_num {
        d.x_is_time = name_is_time;
        d.x = nx;
    } else if let Some(t) = xs_raw.iter().map(|s| parse_time_ms(s)).collect::<Option<Vec<f64>>>() {
        d.x_is_time = true;
        d.x = t;
    } else {
        d.x_is_time = false;
        d.x = (0..xs_raw.len()).map(|i| i as f64).collect();
    }
    d.series = series;
    d
}

fn parse_file(path: &PathBuf) -> ChartData {
    let src = path.display().to_string();
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            return ChartData {
                source: src,
                error: Some(format!("读不到数据文件：{e}")),
                ..Default::default()
            };
        }
    };
    if text.trim_start().starts_with('{') {
        match serde_json::from_str::<JsonData>(&text) {
            Ok(j) => ChartData {
                x_is_time: j.x_is_time,
                x_label: j.x_label,
                x: j.x,
                series: j.series.into_iter().map(|s| (s.name, s.v)).collect(),
                source: src,
                error: None,
            },
            Err(e) => ChartData {
                source: src,
                error: Some(format!("JSON 解析失败：{e}")),
                ..Default::default()
            },
        }
    } else {
        parse_csv(&text, src)
    }
}

#[derive(serde::Deserialize)]
struct JsonSeries {
    name: String,
    v: Vec<f64>,
}
#[derive(serde::Deserialize)]
struct JsonData {
    #[serde(default)]
    x_is_time: bool,
    #[serde(default)]
    x_label: String,
    #[serde(default)]
    x: Vec<f64>,
    #[serde(default)]
    series: Vec<JsonSeries>,
}

/// 读数据快照（按 mtime + 路径缓存，改动即重载）。
pub fn snapshot() -> ChartData {
    let path = data_path();
    let mtime =
        std::fs::metadata(&path).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
    let lock = CACHE.get_or_init(|| Mutex::new((SystemTime::UNIX_EPOCH, String::new(), ChartData::default())));
    let mut g = lock.lock().unwrap();
    let p = path.display().to_string();
    if g.0 != mtime || g.1 != p {
        g.2 = parse_file(&path);
        g.0 = mtime;
        g.1 = p;
    }
    g.2.clone()
}

static CACHE: OnceLock<Mutex<(SystemTime, String, ChartData)>> = OnceLock::new();

// ───────────────────────── 画布 ─────────────────────────

struct Chart {
    data: ChartData,
    cache: Cache,
}

fn fmt_x(v: f64, is_time: bool) -> String {
    if is_time {
        chrono::DateTime::from_timestamp_millis(v as i64)
            .map(|dt| dt.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string())
            .unwrap_or_default()
    } else if v.abs() >= 1000.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.3}")
    }
}

impl<M> canvas::Program<M> for Chart {
    type State = ();
    fn draw(
        &self,
        _s: &(),
        r: &Renderer,
        _t: &Theme,
        b: Rectangle,
        _c: mouse::Cursor,
    ) -> Vec<Geometry> {
        let geo = self.cache.draw(r, b.size(), |frame: &mut Frame| {
            let (w, h) = (frame.width(), frame.height());
            let d = &self.data;
            // 数据有效性。
            let n = d.x.len();
            let has = n >= 2 && d.series.iter().any(|s| s.1.iter().any(|v| v.is_finite()));
            if !has {
                let msg = d.error.clone().unwrap_or_else(|| {
                    "无数据 —— 放 CSV 到 strategies/data/selfdata.csv（首行表头，第 1 列 X）".into()
                });
                frame.fill_text(Text {
                    content: msg,
                    position: Point::new(10.0, h / 2.0),
                    color: C_AXIS,
                    size: iced::Pixels(11.0),
                    ..Default::default()
                });
                return;
            }
            // X / Y 范围。
            let (mut xlo, mut xhi) = (f64::INFINITY, f64::NEG_INFINITY);
            for &x in &d.x {
                if x.is_finite() {
                    xlo = xlo.min(x);
                    xhi = xhi.max(x);
                }
            }
            let (mut ylo, mut yhi) = (f64::INFINITY, f64::NEG_INFINITY);
            for s in &d.series {
                for &v in &s.1 {
                    if v.is_finite() {
                        ylo = ylo.min(v);
                        yhi = yhi.max(v);
                    }
                }
            }
            if !(xlo.is_finite() && xhi.is_finite() && ylo.is_finite() && yhi.is_finite()) {
                return;
            }
            if (xhi - xlo).abs() < 1e-12 {
                xhi = xlo + 1.0;
            }
            if (yhi - ylo).abs() < 1e-12 {
                yhi = ylo + 1.0;
            }
            // 纵轴留 5% 余量。
            let pad = (yhi - ylo) * 0.05;
            ylo -= pad;
            yhi += pad;

            let pw = (w - ML - MR).max(1.0);
            let ph = (h - MT - MB).max(1.0);
            let mx = |x: f64| ML + ((x - xlo) / (xhi - xlo)) as f32 * pw;
            let my = |v: f64| MT + ((yhi - v) / (yhi - ylo)) as f32 * ph;

            // 网格 + 轴标签（Y 5 档、X 4 档）。
            for k in 0..=5 {
                let y = MT + ph * k as f32 / 5.0;
                frame.stroke(
                    &Path::line(Point::new(ML, y), Point::new(ML + pw, y)),
                    Stroke::default().with_width(1.0).with_color(C_GRID),
                );
                let val = yhi - (yhi - ylo) * k as f64 / 5.0;
                frame.fill_text(Text {
                    content: if val.abs() >= 1000.0 { format!("{val:.0}") } else { format!("{val:.3}") },
                    position: Point::new(2.0, y - 5.0),
                    color: C_AXIS,
                    size: iced::Pixels(9.0),
                    ..Default::default()
                });
            }
            for k in 0..=4 {
                let i = (k * (n - 1) / 4).min(n - 1);
                let x = mx(d.x[i]);
                frame.fill_text(Text {
                    content: fmt_x(d.x[i], d.x_is_time),
                    position: Point::new((x - 20.0).max(0.0), h - MB + 4.0),
                    color: C_AXIS,
                    size: iced::Pixels(9.0),
                    ..Default::default()
                });
            }

            // 每条序列：折线 + 散点。
            for (si, s) in d.series.iter().enumerate() {
                let color = PALETTE[si % PALETTE.len()];
                let m = s.1.len().min(n);
                let mut started = false;
                let line = Path::new(|p| {
                    for i in 0..m {
                        let v = s.1[i];
                        if !v.is_finite() {
                            started = false;
                            continue;
                        }
                        let pt = Point::new(mx(d.x[i]), my(v));
                        if started {
                            p.line_to(pt);
                        } else {
                            p.move_to(pt);
                            started = true;
                        }
                    }
                });
                frame.stroke(&line, Stroke::default().with_width(1.5).with_color(color));
                // 散点（点数不太多时画，避免过密）。
                if m <= 400 {
                    for i in 0..m {
                        let v = s.1[i];
                        if v.is_finite() {
                            frame.fill(
                                &Path::circle(Point::new(mx(d.x[i]), my(v)), 1.8),
                                color,
                            );
                        }
                    }
                }
                // 图例。
                let ly = MT + 2.0 + si as f32 * 13.0;
                frame.fill_rectangle(Point::new(ML + 6.0, ly), Size::new(10.0, 3.0), color);
                frame.fill_text(Text {
                    content: s.0.clone(),
                    position: Point::new(ML + 20.0, ly - 4.0),
                    color,
                    size: iced::Pixels(10.0),
                    ..Default::default()
                });
            }
        });
        vec![geo]
    }
}

/// pane 渲染入口：标题（数据源/X 轴类型）+ 自适应图。
pub fn pane_body<'a, M: 'a>() -> Element<'a, M> {
    let d = snapshot();
    let src = d
        .source
        .rsplit('/')
        .next()
        .unwrap_or("selfdata.csv")
        .to_string();
    let axis = if d.x_is_time { "时间轴" } else { "数值轴" };
    let title = format!("自有数据图 · {src} · X={} ({axis})", d.x_label);
    let body = if d.error.is_some() || d.x.len() < 2 {
        center(
            text(d.error.unwrap_or_else(|| {
                "无数据 —— 放 CSV 到 strategies/data/selfdata.csv（或设 WS_SELFDATA_CHART）".into()
            }))
            .size(style::text_size::BODY),
        )
        .into()
    } else {
        let chart: Element<'a, M> = canvas_widget(Chart { data: d, cache: Cache::new() })
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        chart
    };
    container(
        column![
            text(title).size(style::text_size::BODY).font(style::AZERET_MONO),
            body,
        ]
        .spacing(6),
    )
    .padding(8)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}
