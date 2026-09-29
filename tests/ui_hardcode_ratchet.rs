//! 界面硬编码**棘轮**（docs/35 批 0）：写死的颜色 / 字号 / 内边距，每个文件的计数只许降、不许升。
//!
//! # 为什么要有它
//!
//! 界面重构要分 10 批做完，中间几个月里还会继续加新面板。没有这道闸，
//! 迁移掉一处、新面板又写回两处——重构永远做不完。
//!
//! # 规则
//!
//! - `src/ui/` 是适配层，token 在那里变成 iced 的颜色与字号，**不计入**。
//! - 每个文件、每一类的计数 ≤ 基线（`ui-hardcode-baseline.tsv`）。
//! - 基线里没有的文件，计数必须是 0（新文件从第一天起就只用 `ui::`）。
//! - 计数下降了：测试照样通过，但会提示收紧基线——
//!   `UPDATE_UI_BASELINE=1 cargo test --test ui_hardcode_ratchet`。
//!   **只能在计数下降时更新**；更新时如果发现有项上升，拒绝写入。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 统计的四类硬编码。顺序即基线文件的列序。
const KINDS: [&str; 4] = ["color", "const_color", "size", "padding"];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn baseline_path() -> PathBuf {
    root().join("ui-hardcode-baseline.tsv")
}

/// 一行源码里各类硬编码的个数。注释行不算（说明文字里提到 `Color::from_rgb` 不是硬编码）。
fn count_line(line: &str, acc: &mut [usize; 4]) {
    let t = line.trim_start();
    if t.starts_with("//") {
        return;
    }
    // 颜色字面量：from_rgb / from_rgb8 / from_rgba / from_rgba8，以及结构体字面量 Color { r: … }
    acc[0] += t.matches("Color::from_rgb").count() + t.matches("Color { r:").count();
    // 面板里各自定义的颜色常量（const C_XXX: Color）
    if t.contains("const C_") && t.contains(": Color") || t.contains(": iced::Color") && t.contains("const C_") {
        acc[1] += 1;
    }
    acc[2] += count_literal_call(t, ".size(");
    acc[3] += count_literal_call(t, ".padding(");
}

/// `.size(12)` / `.padding(8)` / `.padding([2, 6])` 这类**字面量**调用；`.size(ui::text::LABEL)` 不算。
fn count_literal_call(t: &str, pat: &str) -> usize {
    let mut n = 0;
    let mut rest = t;
    while let Some(i) = rest.find(pat) {
        let after = &rest[i + pat.len()..];
        let c = after.chars().next().unwrap_or(' ');
        if c.is_ascii_digit() || c == '[' {
            n += 1;
        }
        rest = after;
    }
    n
}

fn scan() -> BTreeMap<String, [usize; 4]> {
    let mut out = BTreeMap::new();
    let src = root().join("src");
    let mut stack = vec![src.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                // 适配层：token → iced 的唯一合法落点
                if p == src.join("ui") {
                    continue;
                }
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                let mut acc = [0usize; 4];
                for line in std::fs::read_to_string(&p).unwrap_or_default().lines() {
                    count_line(line, &mut acc);
                }
                if acc.iter().any(|&n| n > 0) {
                    out.insert(rel(&p), acc);
                }
            }
        }
    }
    out
}

fn rel(p: &Path) -> String {
    p.strip_prefix(root()).unwrap_or(p).to_string_lossy().replace('\\', "/")
}

fn load_baseline() -> BTreeMap<String, [usize; 4]> {
    let mut out = BTreeMap::new();
    let Ok(t) = std::fs::read_to_string(baseline_path()) else { return out };
    for line in t.lines().skip(1) {
        let c: Vec<&str> = line.split('\t').collect();
        if c.len() != 5 {
            continue;
        }
        let n = |i: usize| c[i].parse().unwrap_or(0);
        out.insert(c[0].to_string(), [n(1), n(2), n(3), n(4)]);
    }
    out
}

fn write_baseline(m: &BTreeMap<String, [usize; 4]>) {
    let mut s = format!("file\t{}\n", KINDS.join("\t"));
    for (f, a) in m {
        s.push_str(&format!("{f}\t{}\t{}\t{}\t{}\n", a[0], a[1], a[2], a[3]));
    }
    std::fs::write(baseline_path(), s).expect("写基线失败");
}

#[test]
fn 硬编码只许减少() {
    let now = scan();
    let update = std::env::var("UPDATE_UI_BASELINE").is_ok();
    // 第一次：没有基线文件时，更新模式把现状写成基线（批 0 盘点）
    if update && !baseline_path().exists() {
        write_baseline(&now);
        eprintln!("已建立基线：{} 个文件", now.len());
        return;
    }
    let base = load_baseline();

    let mut worse = Vec::new();
    let mut better = 0usize;
    for (f, a) in &now {
        let b = base.get(f).copied().unwrap_or([0; 4]);
        for k in 0..4 {
            if a[k] > b[k] {
                worse.push(format!("  {f}：{} {} → {}", KINDS[k], b[k], a[k]));
            } else if a[k] < b[k] {
                better += 1;
            }
        }
    }
    better += base.keys().filter(|f| !now.contains_key(*f)).count();

    assert!(
        worse.is_empty(),
        "界面硬编码增加了（docs/35）——新代码请用 `crate::ui::` 的 token 与字号角色：\n{}",
        worse.join("\n")
    );
    if update {
        write_baseline(&now);
        eprintln!("已收紧基线（{better} 项下降）");
    } else if better > 0 {
        eprintln!(
            "有 {better} 项硬编码已减少——收紧基线：UPDATE_UI_BASELINE=1 cargo test --test ui_hardcode_ratchet"
        );
    }
}

#[test]
fn 计数规则() {
    let mut a = [0; 4];
    count_line("text(s).size(12).color(Color::from_rgb(0.1, 0.2, 0.3))", &mut a);
    count_line("// Color::from_rgb 只是注释", &mut a);
    count_line("const C_DIM: Color = Color::from_rgb(0.5, 0.5, 0.5);", &mut a);
    count_line(".padding([2, 6]).size(crate::ui::text::LABEL)", &mut a);
    assert_eq!(a, [2, 1, 1, 1]);
}
