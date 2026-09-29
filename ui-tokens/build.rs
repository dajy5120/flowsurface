//! 读 `upds/upds-tokens.json`（UPDS 核心，只读）与 `pack/wealthspring-pack.json`（交易领域包），
//! 生成 `$OUT_DIR/tokens.rs`，并在**编译期**校验（任何一条不满足就编译失败）：
//!
//! 1. 四个主题的核心键、领域包键完全一致（缺一个 = 「第三个主题做不出来」，UPDS V1 §03）。
//! 2. 对比度：正文 ≥ 4.5:1（高对比 ≥ 7:1）；三级文字 ≥ 3:1（高对比 ≥ 4.5:1）；
//!    强调色、状态色、领域实色 ≥ 3:1（高对比 ≥ 4.5:1）。一律对 `surface.primary` 量。
//! 3. 色弱：涨 / 跌的色弱安全版在绿色弱模拟下仍要分得开（OKLab 距离 ≥ 0.10）。

#[path = "src/color.rs"]
#[allow(dead_code)]
mod color;

use color::Rgba;
use serde_json::{Map, Value};
use std::fmt::Write as _;

const THEMES: [(&str, &str); 4] = [
    ("dark", "DARK"),
    ("light", "LIGHT"),
    ("oledDark", "OLED_DARK"),
    ("highContrast", "HIGH_CONTRAST"),
];

fn main() {
    println!("cargo:rerun-if-changed=upds/upds-tokens.json");
    println!("cargo:rerun-if-changed=pack/wealthspring-pack.json");
    println!("cargo:rerun-if-changed=src/color.rs");

    let upds: Value = read("upds/upds-tokens.json");
    let pack: Value = read("pack/wealthspring-pack.json");
    let mut errors: Vec<String> = Vec::new();
    let mut out = String::from("// 由 build.rs 从 upds-tokens.json + wealthspring-pack.json 生成。不要手改。\n\n");

    // ── 核心颜色：四个主题 ──
    let t = &upds["color"]["theme"];
    let dark = obj(&t["dark"]);
    let core_keys: Vec<String> = dark.keys().cloned().collect();
    let mut cores: Vec<(&str, Map<String, Value>)> = Vec::new();
    for (name, _) in THEMES {
        let m = match name {
            "oledDark" => merge(dark, obj(&t["oledDark"])),
            "highContrast" => strip_note(obj(&pack["highContrastCore"])),
            _ => obj(&t[name]).clone(),
        };
        cores.push((name, m));
    }
    for (name, m) in &cores {
        same_keys(&core_keys, m, &format!("核心主题 {name}"), &mut errors);
    }
    writeln!(out, "/// UPDS 核心语义色（V1 §03）。组件只认这些名字，不认具体颜色。").ok();
    writeln!(out, "#[derive(Clone, Copy, Debug)]\npub struct Core {{").ok();
    for k in &core_keys {
        writeln!(out, "    pub {}: Rgba,", ident(k)).ok();
    }
    out.push_str("}\n\n");
    let mut core_rgba = Vec::new();
    for ((name, konst), (_, m)) in THEMES.iter().zip(&cores) {
        let _ = name;
        writeln!(out, "pub const CORE_{konst}: Core = Core {{").ok();
        let mut map = std::collections::HashMap::new();
        for k in &core_keys {
            let c = color_of(m.get(k), &format!("{name}.{k}"), &mut errors);
            map.insert(k.clone(), c);
            writeln!(out, "    {}: {},", ident(k), lit(c)).ok();
        }
        out.push_str("};\n\n");
        core_rgba.push((*name, map));
    }

    // ── 领域包颜色：四个主题 ──
    let pt = &pack["theme"];
    let pdark = obj(&pt["dark"]);
    let pack_keys: Vec<String> = pdark.keys().filter(|k| *k != "chart.series").cloned().collect();
    writeln!(out, "/// 交易领域包颜色（docs/35 §4.2）。").ok();
    writeln!(out, "#[derive(Clone, Copy, Debug)]\npub struct Pack {{").ok();
    for k in &pack_keys {
        writeln!(out, "    pub {}: Rgba,", ident(k)).ok();
    }
    out.push_str("    pub chart_series: [Rgba; 8],\n}\n\n");
    for ((name, konst), (_, core)) in THEMES.iter().zip(&core_rgba) {
        let raw = obj(&pt[*name]);
        let m = match raw.get("extends").and_then(Value::as_str) {
            Some(base) => merge(obj(&pt[base]), raw),
            None => raw.clone(),
        };
        let m = strip_note(&m);
        let mut keys: Vec<String> = m.keys().filter(|k| *k != "chart.series" && *k != "extends").cloned().collect();
        keys.sort();
        let mut want = pack_keys.clone();
        want.sort();
        if keys != want {
            errors.push(format!("领域包主题 {name} 的键与 dark 不一致"));
        }
        let bg = core["surface.primary"];
        let (min_solid, _) = thresholds(name);
        writeln!(out, "pub const PACK_{konst}: Pack = Pack {{").ok();
        let mut cols = std::collections::HashMap::new();
        for k in &pack_keys {
            let c = color_of(m.get(k), &format!("pack.{name}.{k}"), &mut errors);
            // 实色（不透明）的领域色要看得清：≥ 3:1（高对比 4.5:1）
            if c.a >= 1.0 && k != "trade.flat" {
                let r = color::contrast(c, bg);
                if r < min_solid {
                    errors.push(format!("{name}：{k} 对背景对比度 {r:.2} < {min_solid}"));
                }
            }
            cols.insert(k.clone(), c);
            writeln!(out, "    {}: {},", ident(k), lit(c)).ok();
        }
        let series = m.get("chart.series").and_then(Value::as_array).cloned().unwrap_or_default();
        if series.len() != 8 {
            errors.push(format!("{name}：chart.series 要 8 个颜色，给了 {}", series.len()));
        }
        out.push_str("    chart_series: [");
        for (i, v) in series.iter().enumerate() {
            let c = color_of(Some(v), &format!("{name}.chart.series[{i}]"), &mut errors);
            write!(out, "{}, ", lit(c)).ok();
        }
        out.push_str("],\n};\n\n");

        // 色弱：色弱安全的涨 / 跌在绿色弱模拟下要分得开
        let (g, r) = (cols["trade.green.cvd"], cols["trade.red.cvd"]);
        let d = color::distance(color::deuteranopia(g), color::deuteranopia(r));
        if d < 0.10 {
            errors.push(format!("{name}：色弱安全涨跌色在绿色弱模拟下距离 {d:.3} < 0.10"));
        }
    }

    // ── 核心对比度 ──
    for (name, c) in &core_rgba {
        let (min_solid, min_text) = thresholds(name);
        let bg = c["surface.primary"];
        let min_tertiary = if *name == "highContrast" { 4.5 } else { 3.0 };
        let checks: [(&str, f32); 8] = [
            ("text.primary", min_text),
            ("text.secondary", min_text),
            ("text.tertiary", min_tertiary),
            ("accent.primary", min_solid),
            ("status.success", min_solid),
            ("status.warning", min_solid),
            ("status.danger", min_solid),
            ("status.info", min_solid),
        ];
        for (k, min) in checks {
            let r = color::contrast(c[k], bg);
            if r < min {
                errors.push(format!("{name}：{k} 对 surface.primary 对比度 {r:.2} < {min}"));
            }
        }
    }

    // ── 热图色阶（与主题无关）──
    for (k, v) in obj(&pack["heatmapScale"]) {
        let Some(a) = v.as_array() else { continue };
        write!(out, "pub const HEATMAP_{}: [Rgba; {}] = [", k.to_uppercase(), a.len()).ok();
        for (i, c) in a.iter().enumerate() {
            write!(out, "{}, ", lit(color_of(Some(c), &format!("heatmap.{k}[{i}]"), &mut errors))).ok();
        }
        out.push_str("];\n");
    }
    out.push('\n');

    // ── 字体 ──
    for k in ["ui", "mono"] {
        let list: Vec<String> = pack["font"][k]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| format!("{s:?}"))).collect())
            .unwrap_or_default();
        writeln!(out, "pub const FONT_{}: &[&str] = &[{}];", k.to_uppercase(), list.join(", ")).ok();
    }

    // ── 度量：间距、圆角、边框、不透明度、动效 ──
    let space: Vec<String> = (0..9).map(|i| num(&upds["space"][i.to_string()])).collect();
    writeln!(out, "\n/// 4px 基准的间距阶梯 space.0..space.8（V1 §05）。\npub const SPACE: [f32; 9] = [{}];", space.join(", ")).ok();
    for k in ["none", "sm", "md", "lg"] {
        writeln!(out, "pub const RADIUS_{}: f32 = {};", k.to_uppercase(), num(&upds["radius"][k])).ok();
    }
    for (k, n) in [("hairline", "HAIRLINE"), ("strong", "STRONG"), ("focus", "FOCUS"), ("focusOffset", "FOCUS_OFFSET")] {
        writeln!(out, "pub const BORDER_{n}: f32 = {};", num(&upds["border"][k])).ok();
    }
    for k in ["disabled", "ghost", "hover", "scrim"] {
        writeln!(out, "pub const OPACITY_{}: f32 = {};", k.to_uppercase(), num(&upds["opacity"][k])).ok();
    }
    for k in ["instant", "micro", "transient", "structural", "modal"] {
        writeln!(out, "pub const MOTION_{}_MS: u32 = {};", k.to_uppercase(), upds["motion"][k]["duration"].as_u64().unwrap_or(0)).ok();
    }

    // ── 密度（V1 §06）──
    out.push('\n');
    for (k, n) in [("compact", "COMPACT"), ("comfortable", "COMFORTABLE"), ("spacious", "SPACIOUS")] {
        let d = &upds["density"][k];
        writeln!(
            out,
            "pub const DENSITY_{n}: DensitySpec = DensitySpec {{ row: {}, control: {}, toolbar: {}, panel_header: {}, padding: {}, font_size: {}, icon: {} }};",
            num(&d["row"]), num(&d["control"]), num(&d["toolbar"]), num(&d["panelHeader"]),
            num(&d["padding"]), num(&d["fontSize"]), num(&d["icon"])
        ).ok();
    }

    // ── 字号角色（V1 §04）──
    out.push('\n');
    for (k, v) in obj(&upds["typography"]["role"]) {
        writeln!(
            out,
            "pub const ROLE_{}: TypeSpec = TypeSpec {{ size: {}, line_height: {}, weight: {}, tracking: {}, mono: {}, uppercase: {} }};",
            k.to_uppercase(),
            num(&v["size"]), num(&v["lineHeight"]), v["weight"].as_u64().unwrap_or(400),
            num(&v["tracking"]),
            v["family"].as_str() == Some("mono"),
            v["transform"].as_str() == Some("uppercase"),
        ).ok();
    }

    // ── 布局红线（V6 §53）──
    let l = &upds["layout"];
    let pairs = [
        ("SPLITTER_LINE", &l["splitter"]["line"]), ("SPLITTER_HIT", &l["splitter"]["hitArea"]),
        ("SPLITTER_KEY_STEP", &l["splitter"]["keyStep"]),
        ("PANEL_MIN_W", &l["panelMin"]["width"]), ("PANEL_MIN_H", &l["panelMin"]["height"]),
        ("SIDEBAR_RAIL", &l["sidebar"]["rail"]), ("SIDEBAR_MIN", &l["sidebar"]["min"]), ("SIDEBAR_MAX", &l["sidebar"]["max"]),
        ("SCROLLBAR_IDLE", &l["scrollbar"]["idle"]), ("SCROLLBAR_HOVER", &l["scrollbar"]["hover"]),
        ("POINTER_MIN", &l["pointerTarget"]["min"]), ("POINTER_TOUCH", &l["pointerTarget"]["touch"]),
        ("ICON_LABEL_GAP", &l["iconLabelGap"]["default"]), ("ICON_LABEL_GAP_COMPACT", &l["iconLabelGap"]["compact"]),
        ("TREE_INDENT", &l["treeIndent"]), ("TEXT_MEASURE_MAX_CH", &l["textMeasure"]["max"]),
    ];
    out.push('\n');
    for (n, v) in pairs {
        writeln!(out, "pub const {n}: f32 = {};", num(v)).ok();
    }
    let bps: Vec<String> = l["breakpoint"].as_array().map(|a| a.iter().map(num).collect()).unwrap_or_default();
    writeln!(out, "pub const BREAKPOINTS: [f32; {}] = [{}];", bps.len(), bps.join(", ")).ok();

    // ── 性能预算（V5 §39）──
    out.push('\n');
    for (k, v) in obj(&upds["performance"]) {
        if let Some(n) = v.as_u64() {
            writeln!(out, "pub const BUDGET_{}: u32 = {n};", snake(k).to_uppercase()).ok();
        }
    }

    // ── 数据语义（V8）──
    out.push('\n');
    for (k, v) in obj(&upds["dataSemantics"]["absence"]) {
        writeln!(out, "pub const ABSENT_{}: &str = {:?};", snake(k).to_uppercase(), v.as_str().unwrap_or("")).ok();
    }

    // ── 缺省偏好 ──
    let d = &pack["defaults"];
    writeln!(out, "\npub const DEFAULT_THEME: &str = {:?};", d["theme"].as_str().unwrap_or("dark")).ok();
    writeln!(out, "pub const DEFAULT_DENSITY: &str = {:?};", d["density"].as_str().unwrap_or("compact")).ok();
    writeln!(out, "pub const DEFAULT_UP_DOWN: &str = {:?};", d["upDown"].as_str().unwrap_or("international")).ok();
    writeln!(out, "pub const DEFAULT_HEATMAP_SCALE: &str = {:?};", d["heatmapScale"].as_str().unwrap_or("inferno")).ok();

    if !errors.is_empty() {
        panic!("\n设计 token 校验失败（docs/35 §4.1）：\n  {}\n", errors.join("\n  "));
    }
    let dst = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("tokens.rs");
    std::fs::write(dst, out).expect("写 tokens.rs 失败");
}

/// (实色下限, 正文下限)
fn thresholds(theme: &str) -> (f32, f32) {
    if theme == "highContrast" { (4.5, 7.0) } else { (3.0, 4.5) }
}

fn read(p: &str) -> Value {
    let t = std::fs::read_to_string(p).unwrap_or_else(|e| panic!("读 {p} 失败：{e}"));
    serde_json::from_str(&t).unwrap_or_else(|e| panic!("{p} 不是合法 JSON：{e}"))
}

fn obj(v: &Value) -> &Map<String, Value> {
    v.as_object().unwrap_or_else(|| panic!("期望 JSON 对象：{v}"))
}

fn merge(base: &Map<String, Value>, over: &Map<String, Value>) -> Map<String, Value> {
    let mut m = base.clone();
    for (k, v) in over {
        if k != "extends" {
            m.insert(k.clone(), v.clone());
        }
    }
    m
}

fn strip_note(m: &Map<String, Value>) -> Map<String, Value> {
    m.iter().filter(|(k, _)| *k != "note").map(|(k, v)| (k.clone(), v.clone())).collect()
}

fn same_keys(want: &[String], m: &Map<String, Value>, what: &str, errors: &mut Vec<String>) {
    for k in want {
        if !m.contains_key(k) {
            errors.push(format!("{what} 缺 {k}"));
        }
    }
    for k in m.keys() {
        if !want.contains(k) {
            errors.push(format!("{what} 多了 {k}"));
        }
    }
}

fn color_of(v: Option<&Value>, what: &str, errors: &mut Vec<String>) -> Rgba {
    match v.and_then(Value::as_str).map(color::parse) {
        Some(Ok(c)) => c,
        Some(Err(e)) => {
            errors.push(format!("{what}：{e}"));
            Rgba::new(1.0, 0.0, 1.0, 1.0)
        }
        None => {
            errors.push(format!("{what}：缺失"));
            Rgba::new(1.0, 0.0, 1.0, 1.0)
        }
    }
}

fn lit(c: Rgba) -> String {
    format!("Rgba::new({:.6}, {:.6}, {:.6}, {:.4})", c.r, c.g, c.b, c.a)
}

fn num(v: &Value) -> String {
    let f = v.as_f64().unwrap_or(0.0);
    format!("{f:?}_f32").replace("_f32", "")
}

fn ident(k: &str) -> String {
    k.replace(['.', '-'], "_")
}

fn snake(k: &str) -> String {
    let mut s = String::new();
    for c in k.chars() {
        if c.is_uppercase() {
            s.push('_');
            s.push(c.to_ascii_lowercase());
        } else {
            s.push(c);
        }
    }
    s
}
