//! 色板对照页：四个主题 × 全部核心色与领域色，标出对 `surface.primary` 的对比度，供目测。
//!
//! ```text
//! cargo run -p wealthspring-ui-tokens --example swatches -- /tmp/swatches.html
//! ```

use wealthspring_ui_tokens::{Domain, Prefs, Rgba, ThemeId, color, UpDown};

fn chip(name: &str, c: Rgba, bg: Rgba, fg: Rgba) -> String {
    let r = color::contrast(c, bg);
    format!(
        "<div class=c><div class=s style='background:{}'></div><div style='color:{}'>{name}<br><small>{} · {r:.1}:1</small></div></div>",
        css(c),
        css(fg),
        c.hex()
    )
}

fn css(c: Rgba) -> String {
    let [r, g, b, _] = c.to_u8();
    format!("rgba({r},{g},{b},{:.2})", c.a)
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "swatches.html".into());
    let mut h = String::from(
        "<!doctype html><meta charset=utf-8><title>WealthSpring 色板</title><style>\
         body{margin:0;font:13px 'Noto Sans CJK SC',sans-serif}section{padding:16px}\
         .g{display:grid;grid-template-columns:repeat(auto-fill,minmax(190px,1fr));gap:6px}\
         .c{display:flex;gap:8px;align-items:center}.s{width:36px;height:24px;border-radius:2px;border:1px solid #8884}\
         h2{font-size:15px;margin:4px 0 10px}h3{font-size:12px;margin:14px 0 6px;opacity:.8}</style>",
    );
    for t in ThemeId::ALL {
        let c = t.core();
        let (bg, fg) = (c.surface_primary, c.text_primary);
        h.push_str(&format!("<section style='background:{};color:{}'><h2>{} · {}</h2>", css(bg), css(fg), t.label(), t.key()));
        let core = [
            ("surface.base", c.surface_base), ("surface.primary", c.surface_primary),
            ("surface.secondary", c.surface_secondary), ("surface.elevated", c.surface_elevated),
            ("border.default", c.border_default), ("border.strong", c.border_strong),
            ("text.primary", c.text_primary), ("text.secondary", c.text_secondary),
            ("text.tertiary", c.text_tertiary), ("accent.primary", c.accent_primary),
            ("accent.soft", c.accent_soft), ("status.success", c.status_success),
            ("status.warning", c.status_warning), ("status.danger", c.status_danger),
            ("status.info", c.status_info),
        ];
        h.push_str("<h3>核心</h3><div class=g>");
        for (n, x) in core {
            h.push_str(&chip(n, x, bg, fg));
        }
        h.push_str("</div>");
        for (label, prefs) in [
            ("领域 · 绿涨红跌", Prefs::default()),
            ("领域 · 红涨绿跌", Prefs { up_down: UpDown::China, ..Prefs::default() }),
            ("领域 · 色弱安全", Prefs { cvd_safe: true, ..Prefs::default() }),
        ] {
            let d = Domain::resolve(t, &prefs);
            let list = [
                ("涨 up", d.up), ("跌 down", d.down), ("买 buy", d.buy), ("卖 sell", d.sell),
                ("买 soft", d.buy_soft), ("卖 soft", d.sell_soft), ("大单", d.large), ("POC", d.poc),
                ("挂单", d.order_working), ("持仓均价", d.position_avg), ("实盘", d.env_live),
                ("模拟盘", d.env_paper), ("回测", d.env_backtest), ("回放", d.env_replay),
            ];
            h.push_str(&format!("<h3>{label}</h3><div class=g>"));
            for (n, x) in list {
                h.push_str(&chip(n, x, bg, fg));
            }
            h.push_str("</div>");
        }
        let d = Domain::resolve(t, &Prefs::default());
        h.push_str("<h3>图表系列 · 热图色阶</h3><div class=g>");
        for (i, x) in d.chart_series.iter().enumerate() {
            h.push_str(&chip(&format!("series.{}", i + 1), *x, bg, fg));
        }
        h.push_str("</div><div style='display:flex;height:18px;margin-top:8px'>");
        for i in 0..60 {
            let x = wealthspring_ui_tokens::sample_scale(&d.heatmap, i as f32 / 59.0);
            h.push_str(&format!("<div style='flex:1;background:{}'></div>", css(x)));
        }
        h.push_str("</div></section>");
    }
    std::fs::write(&out, h).expect("写文件失败");
    println!("{out}");
}
