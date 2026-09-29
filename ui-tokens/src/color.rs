//! 颜色数学：解析、oklch → sRGB、对比度、色弱模拟。
//!
//! **这个文件同时被 `build.rs` 用 `#[path]` 引入**，所以不能依赖任何 crate、不能引用本 crate
//! 的其他模块——编译期校验与运行时计算必须是同一份代码，否则「构建时查过对比度」没有意义。

/// sRGB 颜色，分量 0..=1（已做 gamma），`a` 为不透明度。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Rgba {
    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    pub const fn with_alpha(self, a: f32) -> Self {
        Self { a, ..self }
    }

    /// 0..=255 的四元组。
    pub fn to_u8(self) -> [u8; 4] {
        let q = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
        [q(self.r), q(self.g), q(self.b), q(self.a)]
    }

    /// `#rrggbb` 或 `#rrggbbaa`（a < 1 时）。
    pub fn hex(self) -> String {
        let [r, g, b, a] = self.to_u8();
        if a == 255 {
            format!("#{r:02x}{g:02x}{b:02x}")
        } else {
            format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
        }
    }

    /// 把自己按不透明度叠到 `bg` 上，得到实际显示的颜色（`bg` 视为不透明）。
    pub fn over(self, bg: Rgba) -> Rgba {
        let a = self.a;
        Rgba::new(
            self.r * a + bg.r * (1.0 - a),
            self.g * a + bg.g * (1.0 - a),
            self.b * a + bg.b * (1.0 - a),
            1.0,
        )
    }
}

/// 解析 `oklch(L C H)` / `oklch(L C H / a)` / `#rrggbb` / `#rrggbbaa`。
pub fn parse(s: &str) -> Result<Rgba, String> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix('#') {
        let n = |i: usize| {
            u8::from_str_radix(h.get(i..i + 2).ok_or("位数不对")?, 16)
                .map(|v| v as f32 / 255.0)
                .map_err(|e| e.to_string())
        };
        return match h.len() {
            6 => Ok(Rgba::new(n(0)?, n(2)?, n(4)?, 1.0)),
            8 => Ok(Rgba::new(n(0)?, n(2)?, n(4)?, n(6)?)),
            _ => Err(format!("不认识的十六进制颜色：{s}")),
        };
    }
    let inner = s
        .strip_prefix("oklch(")
        .and_then(|r| r.strip_suffix(')'))
        .ok_or_else(|| format!("只认 oklch(...) 或 #rrggbb：{s}"))?;
    let (lch, alpha) = match inner.split_once('/') {
        Some((a, b)) => (a, b.trim().parse::<f32>().map_err(|e| format!("{s}: {e}"))?),
        None => (inner, 1.0),
    };
    let v: Vec<f32> = lch
        .split_whitespace()
        .map(|x| x.parse::<f32>().map_err(|e| format!("{s}: {e}")))
        .collect::<Result<_, _>>()?;
    if v.len() != 3 {
        return Err(format!("oklch 要三个数：{s}"));
    }
    let (r, g, b) = oklch_to_srgb(v[0], v[1], v[2]);
    Ok(Rgba::new(r, g, b, alpha))
}

// ── oklch / oklab / 线性 sRGB ────────────────────────────────────────

fn oklab_to_linear(l: f32, a: f32, b: f32) -> (f32, f32, f32) {
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    (
        4.076_741_7 * l3 - 3.307_711_6 * m3 + 0.230_969_94 * s3,
        -1.268_438 * l3 + 2.609_757_4 * m3 - 0.341_319_38 * s3,
        -0.004_196_086_3 * l3 - 0.703_418_6 * m3 + 1.707_614_7 * s3,
    )
}

fn linear_to_oklab(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let l = 0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b;
    let m = 0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b;
    let s = 0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b;
    let (l_, m_, s_) = (l.cbrt(), m.cbrt(), s.cbrt());
    (
        0.210_454_26 * l_ + 0.793_617_8 * m_ - 0.004_072_047 * s_,
        1.977_998_5 * l_ - 2.428_592_2 * m_ + 0.450_593_7 * s_,
        0.025_904_037 * l_ + 0.782_771_77 * m_ - 0.808_675_77 * s_,
    )
}

fn gamma(x: f32) -> f32 {
    if x <= 0.003_130_8 { 12.92 * x } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 }
}

fn degamma(x: f32) -> f32 {
    if x <= 0.040_45 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) }
}

fn in_gamut((r, g, b): (f32, f32, f32)) -> bool {
    let e = 1e-4;
    [r, g, b].iter().all(|v| *v >= -e && *v <= 1.0 + e)
}

/// oklch → sRGB（0..=1）。超出 sRGB 色域时**保持明度与色相、降低彩度**直到落进色域
/// （CSS Color 4 的做法）——直接裁剪分量会让色相漂移，那样「同一个色相的明度阶梯」就不成立了。
pub fn oklch_to_srgb(l: f32, c: f32, h_deg: f32) -> (f32, f32, f32) {
    let h = h_deg.to_radians();
    let lin = |c: f32| oklab_to_linear(l, c * h.cos(), c * h.sin());
    let mut rgb = lin(c);
    if !in_gamut(rgb) {
        let (mut lo, mut hi) = (0.0f32, c);
        for _ in 0..24 {
            let mid = (lo + hi) / 2.0;
            if in_gamut(lin(mid)) { lo = mid } else { hi = mid }
        }
        rgb = lin(lo);
    }
    let f = |x: f32| gamma(x.clamp(0.0, 1.0));
    (f(rgb.0), f(rgb.1), f(rgb.2))
}

// ── 对比度与色弱 ─────────────────────────────────────────────────────

/// WCAG 相对亮度。
pub fn luminance(c: Rgba) -> f32 {
    0.2126 * degamma(c.r) + 0.7152 * degamma(c.g) + 0.0722 * degamma(c.b)
}

/// WCAG 对比度（1..=21）。前景带透明度时先叠到背景上。
pub fn contrast(fg: Rgba, bg: Rgba) -> f32 {
    let fg = if fg.a < 1.0 { fg.over(bg) } else { fg };
    let (a, b) = (luminance(fg), luminance(bg));
    let (hi, lo) = if a > b { (a, b) } else { (b, a) };
    (hi + 0.05) / (lo + 0.05)
}

/// 绿色弱（deuteranopia，最常见）模拟：Machado 2009，严重度 1.0，在线性 RGB 上做。
pub fn deuteranopia(c: Rgba) -> Rgba {
    let (r, g, b) = (degamma(c.r), degamma(c.g), degamma(c.b));
    let m = |x: f32, y: f32, z: f32| gamma((x * r + y * g + z * b).clamp(0.0, 1.0));
    Rgba::new(
        m(0.367_322, 0.860_646, -0.227_968),
        m(0.280_085, 0.672_501, 0.047_413),
        m(-0.011_820, 0.042_940, 0.968_881),
        c.a,
    )
}

/// 两色在 OKLab 空间的距离（≈ 感知差异；0.1 左右是一眼能分开的量级）。
pub fn distance(a: Rgba, b: Rgba) -> f32 {
    let p = linear_to_oklab(degamma(a.r), degamma(a.g), degamma(a.b));
    let q = linear_to_oklab(degamma(b.r), degamma(b.g), degamma(b.b));
    ((p.0 - q.0).powi(2) + (p.1 - q.1).powi(2) + (p.2 - q.2).powi(2)).sqrt()
}
