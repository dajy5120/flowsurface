use std::env;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    register_drawing_tools();
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs");
    println!("cargo:rerun-if-changed=.git/packed-refs");

    let Some(git_sha) = git_output(&["rev-parse", "--short=7", "HEAD"]) else {
        return;
    };

    println!("cargo:rustc-env=FLOWSURFACE_GIT_SHA={git_sha}");

    let pkg_version = env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let is_release_tag = git_head_has_release_tag(&pkg_version);
    println!(
        "cargo:rustc-env=FLOWSURFACE_IS_RELEASE_TAG={}",
        if is_release_tag { "true" } else { "false" }
    );

    let is_official_release = is_official_release_build();
    println!(
        "cargo:rustc-env=FLOWSURFACE_IS_OFFICIAL_RELEASE={}",
        if is_official_release { "true" } else { "false" }
    );
}

fn is_official_release_build() -> bool {
    if !env_var_is_truthy("FLOWSURFACE_OFFICIAL_RELEASE") {
        return false;
    }

    if !env_var_is_truthy("GITHUB_ACTIONS") {
        return false;
    }

    let workflow = env::var("GITHUB_WORKFLOW").unwrap_or_default();
    let event = env::var("GITHUB_EVENT_NAME").unwrap_or_default();

    workflow == "Release" && event == "workflow_dispatch"
}

fn env_var_is_truthy(name: &str) -> bool {
    let Ok(value) = env::var(name) else {
        return false;
    };

    value == "1"
        || value.eq_ignore_ascii_case("true")
        || value.eq_ignore_ascii_case("yes")
        || value.eq_ignore_ascii_case("on")
}

fn git_output(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8(output.stdout).ok()?;
    let value = stdout.trim().to_string();
    if value.is_empty() { None } else { Some(value) }
}

fn git_head_has_release_tag(pkg_version: &str) -> bool {
    let Some(tags) = git_output(&["tag", "--points-at", "HEAD"]) else {
        return false;
    };

    let prefixed = format!("v{pkg_version}");
    tags.lines().any(|tag| {
        let value = tag.trim();
        value == pkg_version || value == prefixed
    })
}

/// 图表绘图的工具注册（docs/46 G5）：`src/ws/drawing/tools/` 里每个文件（`mod.rs` 除外）是一种工具，
/// 生成「每个文件一个模块 + 登记表」——加工具只要新增一个文件。
fn register_drawing_tools() {
    let dir = std::path::Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap_or_default()).join("src/ws/drawing/tools");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    files.retain(|p| p.extension().is_some_and(|x| x == "rs") && p.file_name().is_some_and(|n| n != "mod.rs"));
    files.sort();
    let mut out = String::from("// 由 build.rs 生成（docs/46 G5）：不要手改\n");
    let mut names = Vec::new();
    for p in &files {
        println!("cargo:rerun-if-changed={}", p.display());
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or_default().replace(['-', ' '], "_");
        out.push_str(&format!("#[path = {:?}]\npub mod {stem};\n", p.display().to_string()));
        names.push(format!("&{stem}::TOOL"));
    }
    out.push_str(&format!("/// 登记了的工具。\npub static REGISTERED: &[&dyn ToolDef] = &[{}];\n", names.join(", ")));
    let dst = std::path::Path::new(&env::var("OUT_DIR").unwrap_or_default()).join("drawing_tools.rs");
    let _ = std::fs::write(dst, out);
}
