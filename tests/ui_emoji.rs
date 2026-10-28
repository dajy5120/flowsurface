//! emoji 闸门（docs/66）：界面字符串里不许出现 emoji。
//!
//! emoji 字体里才有，系统拿**彩色 emoji 字体**补：自带颜色、不跟主题、大小与基线和文字对不齐，和侧栏的线性图标两种风格。
//! 图标一律用图标字体（`ui::widgets::glyph` / `ilabel` / `ibtn`，字形在 `scripts/ws_icons.py` 里画）；
//! 状态用文字符号 + 状态色（✓ ✗ ● ▲，docs/35「文字 + 符号」）。
//!
//! 只看字符串字面量（双引号之间），注释行不算——说明文字里提到 emoji 不是界面上显示它。

use std::path::{Path, PathBuf};

/// 默认按 emoji 显示的 BMP 字符（Unicode `Emoji_Presentation=Yes` 里界面上可能碰到的那些）。
const BMP_EMOJI: &str = "⌚⌛⏩⏪⏫⏬⏰⏳◽◾☔☕♿⚓⚡⚪⚫⚽⚾⛄⛅⛎⛔⛪⛲⛳⛵⛺⛽✅✊✋✨❌❎❓❔❕❗➕➖➗➰➿⬛⬜⭐⭕";

fn is_emoji(c: char) -> bool {
    let o = c as u32;
    o >= 0x1F000 || o == 0xFE0F || BMP_EMOJI.contains(c)
}

fn scan(dir: &Path, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            scan(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            let text = std::fs::read_to_string(&p).unwrap_or_default();
            for (n, line) in text.lines().enumerate() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                // 双引号之间的部分（第奇数段）
                for (k, seg) in line.split('"').enumerate() {
                    if k % 2 == 1
                        && let Some(c) = seg.chars().find(|c| is_emoji(*c))
                    {
                        out.push(format!("{}:{}  「{c}」  {}", p.display(), n + 1, line.trim()));
                    }
                }
            }
        }
    }
}

#[test]
fn 界面字符串里没有_emoji() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut hits = Vec::new();
    scan(&src, &mut hits);
    assert!(
        hits.is_empty(),
        "界面字符串里有 emoji（换成图标字体的字形：ui::widgets::glyph / ilabel / ibtn；状态用 ✓ ✗ ● ▲ + 状态色）：\n{}",
        hits.join("\n")
    );
}

#[test]
fn 判定() {
    assert!(is_emoji('🔔') && is_emoji('✅') && is_emoji('⚡') && is_emoji('⏳'));
    assert!(!is_emoji('●') && !is_emoji('▾') && !is_emoji('✓') && !is_emoji('✗') && !is_emoji('→') && !is_emoji('中'));
}
