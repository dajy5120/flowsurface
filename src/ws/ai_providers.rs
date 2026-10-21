//! AI 接口（docs/53 AI0）：Cockpit 各处共用的接口清单、下拉框与「AI 接口」管理卡。
//!
//! 清单、校验、测试都只在 Python（`python -m factory.ai providers / test / default / init`），这里只跑它、读 JSON。
//! 下拉框选的接口全局生效（策略中心 AI 研究助理、画布 AI 页共用）：选了下一次调用就用它，不用重启；
//! 「设为默认」才写回配置文件。密钥只放环境变量，这里只显示「设没设」，从不读出内容。

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use iced::widget::{column, container, pick_list, row};
use iced::{Alignment, Element, Length};
use serde::Deserialize;

use crate::ui::metrics::space;
use crate::ui::pal;
use crate::ui::text as t;
use crate::ui::widgets::{self as w, Kind};

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Provider {
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub key_env: String,
    #[serde(default)]
    pub ready: bool,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Listing {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub exists: bool,
    #[serde(default)]
    pub env_file: String,
    #[serde(default)]
    pub env_file_exists: bool,
    #[serde(default)]
    pub default: String,
    #[serde(default)]
    pub providers: Vec<Provider>,
    #[serde(default)]
    pub problems: Vec<String>,
    #[serde(default)]
    pub error: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TestResult {
    pub ok: bool,
    #[serde(default)]
    pub seconds: f64,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub error: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AiMsg {
    /// 下拉框选接口（本次会话生效）
    Pick(String),
    /// 「AI 接口」管理卡开 / 关
    Manage,
    Refresh,
    /// 测一下（真的发一个很小的问题）
    Test(String),
    SetDefault(String),
    /// 建配置模板与密钥文件模板
    Init,
    /// 在 Studio 里打开配置文件 / 密钥文件
    OpenFile(String),
}

#[derive(Default)]
struct St {
    listing: Option<Listing>,
    loading: bool,
    err: String,
    /// 本次会话选的接口（`None` = 用配置的缺省）
    chosen: Option<String>,
    open: bool,
    tests: HashMap<String, TestResult>,
    testing: HashSet<String>,
    msg: String,
}

static ST: Mutex<Option<St>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut St) -> R) -> R {
    let mut g = ST.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    f(g.get_or_insert_with(St::default))
}

fn python(args: &[&str]) -> Result<serde_json::Value, String> {
    let out = std::process::Command::new(super::paths::python())
        .args(["-m", "factory.ai"])
        .args(args)
        .current_dir(super::paths::repo_root())
        .output()
        .map_err(|e| format!("起不来 Python：{e}"))?;
    serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .map_err(|_| String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("factory.ai 失败").trim_start_matches("✗ ").to_string())
}

fn load() {
    if !with(|g| !std::mem::replace(&mut g.loading, true)) {
        return;
    }
    super::spawn_named("ws-aiprov", || {
        let r = python(&["providers", "--json"]).and_then(|v| serde_json::from_value::<Listing>(v).map_err(|e| format!("接口清单解析失败：{e}")));
        with(|g| {
            g.loading = false;
            match r {
                Ok(l) => {
                    // 选的接口从清单里没了（改了配置）：回到缺省
                    if g.chosen.as_ref().is_some_and(|c| !l.providers.iter().any(|p| &p.name == c)) {
                        g.chosen = None;
                    }
                    g.listing = Some(l);
                    g.err.clear();
                }
                Err(e) => g.err = e,
            }
        });
    });
}

/// 第一次用到时读一次清单（之后靠「刷新」或改了配置）。
pub fn ensure() {
    if with(|g| g.listing.is_none() && !g.loading && g.err.is_empty()) {
        load();
    }
}

/// 这次调用用哪个接口：`Some(名字)` 就给 `--provider`；`None` = 不给（Python 用配置的缺省）。
#[must_use]
pub fn chosen() -> Option<String> {
    with(|g| g.chosen.clone())
}

/// 现在显示的接口名（选的，或缺省）。
#[must_use]
pub fn current() -> String {
    with(|g| g.chosen.clone().or_else(|| g.listing.as_ref().map(|l| l.default.clone())).unwrap_or_else(|| "claude-cli".into()))
}

pub fn handle(m: AiMsg) {
    match m {
        AiMsg::Pick(name) => with(|g| {
            let is_default = g.listing.as_ref().is_some_and(|l| l.default == name);
            g.chosen = (!is_default).then_some(name);
        }),
        AiMsg::Manage => {
            let open = with(|g| {
                g.open = !g.open;
                g.open
            });
            if open {
                load();
            }
        }
        AiMsg::Refresh => load(),
        AiMsg::Test(name) => {
            if !with(|g| g.testing.insert(name.clone())) {
                return;
            }
            super::spawn_named("ws-aitest", move || {
                let r = python(&["test", &name, "--json"]).and_then(|v| {
                    if v.get("ok") == Some(&serde_json::Value::Bool(false)) && v.get("seconds").is_none() {
                        return Err(v["error"].as_str().unwrap_or("测试失败").to_string());
                    }
                    serde_json::from_value::<TestResult>(v).map_err(|e| format!("测试结果解析失败：{e}"))
                });
                with(|g| {
                    g.testing.remove(&name);
                    let t = r.unwrap_or_else(|e| TestResult { ok: false, error: e, ..TestResult::default() });
                    g.tests.insert(name, t);
                });
            });
        }
        AiMsg::SetDefault(name) => {
            super::spawn_named("ws-aiprov", move || {
                let r = python(&["default", &name, "--json"]);
                with(|g| {
                    g.msg = match r {
                        Ok(v) if v["ok"] == true => {
                            g.chosen = None;
                            format!("缺省接口改成了 {name}")
                        }
                        Ok(v) => v["error"].as_str().unwrap_or("没改成").to_string(),
                        Err(e) => e,
                    };
                });
                load();
            });
        }
        AiMsg::Init => {
            super::spawn_named("ws-aiprov", || {
                let r = python(&["init", "--json"]);
                with(|g| {
                    g.msg = match r {
                        Ok(v) => match v["made"].as_array() {
                            Some(a) if !a.is_empty() => format!("已建：{}", a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join("、")),
                            _ => "配置文件已经有了，没动".into(),
                        },
                        Err(e) => e,
                    };
                });
                load();
            });
        }
        AiMsg::OpenFile(path) => {
            let r = wealthspring_ui_tokens::bridge::send(
                wealthspring_ui_tokens::bridge::Peer::Studio,
                &wealthspring_ui_tokens::bridge::UiCommand::OpenFile { path, line: None },
            );
            with(|g| g.msg = r.unwrap_or_else(|e| format!("打开失败：{e}")));
        }
    }
}

/// 下拉框 + 「AI 接口」按钮（放在每个用 AI 的地方）。
pub fn picker<'a>() -> Element<'a, AiMsg> {
    ensure();
    let (names, cur, open, ready) = with(|g| {
        let names: Vec<String> = g.listing.as_ref().map(|l| l.providers.iter().map(|p| p.name.clone()).collect()).unwrap_or_default();
        let cur = g.chosen.clone().or_else(|| g.listing.as_ref().map(|l| l.default.clone())).unwrap_or_else(|| "claude-cli".into());
        let ready = g.listing.as_ref().and_then(|l| l.providers.iter().find(|p| p.name == cur)).is_none_or(|p| p.ready);
        (names, cur, g.open, ready)
    });
    let mut r = row![
        t::metadata("接口").color(pal::dim()),
        pick_list(names, Some(cur), AiMsg::Pick).text_size(t::s_small()),
        w::btn(if open { "AI 接口 ▴" } else { "AI 接口 ⚙" }, if open { Kind::Standard } else { Kind::Ghost }, Some(AiMsg::Manage)),
    ]
    .spacing(space(2))
    .align_y(Alignment::Center);
    if !ready {
        r = r.push(t::metadata("这个接口还不能用（见「AI 接口」）").color(pal::warn()));
    }
    r.into()
}

/// 管理卡开着吗（开着才画 [`manager`]）。
#[must_use]
pub fn manager_open() -> bool {
    with(|g| g.open)
}

/// 「AI 接口」管理卡：每个接口一行（类型、模型、密钥状态、测一下、设为默认），配置文件在哪、怎么放密钥。
pub fn manager<'a>() -> Element<'a, AiMsg> {
    let (l, loading, err, tests, testing, msg) =
        with(|g| (g.listing.clone(), g.loading, g.err.clone(), g.tests.clone(), g.testing.clone(), g.msg.clone()));
    let mut col = column![row![
        w::section("AI 接口"),
        iced::widget::Space::new().width(Length::Fill),
        w::btn_busy("刷新", Kind::Ghost, Some(AiMsg::Refresh), loading),
    ]
    .align_y(Alignment::Center)]
    .spacing(space(2));
    if !err.is_empty() {
        col = col.push(t::caption(err).color(pal::bad()));
    }
    let Some(l) = l else {
        return container(col).padding(space(3)).into();
    };
    if !l.error.is_empty() {
        col = col.push(t::caption(format!("配置没读：{}（现在只用内置的 claude-cli）", l.error)).color(pal::bad()));
    }
    for p in &l.problems {
        col = col.push(t::metadata(format!("⚠ {p}")).color(pal::warn()));
    }
    for p in &l.providers {
        let is_default = p.name == l.default;
        // 两行：名字 · 类型 · 模型 · 缺省 / 设为默认 · 测一下；下面是状态与测试结果（窄栏里也排得开）
        let mut head = row![t::label(p.name.clone()), t::metadata(format!("{} · {}", p.kind, p.model)).color(pal::dim())]
            .spacing(space(2))
            .align_y(Alignment::Center);
        head = head.push(if is_default {
            Element::from(t::metadata("缺省").color(pal::accent()))
        } else {
            w::btn("设为默认", Kind::Ghost, Some(AiMsg::SetDefault(p.name.clone())))
        });
        head = head.push(w::btn_busy("测一下", Kind::Ghost, Some(AiMsg::Test(p.name.clone())), testing.contains(&p.name)));
        let mut line = column![head.wrap(), t::metadata(format!("{} {}", if p.ready { "✓" } else { "✗" }, p.note)).color(if p.ready { pal::ok() } else { pal::warn() })]
            .spacing(0);
        if let Some(r) = tests.get(&p.name) {
            let cost = r.cost_usd.map(|c| format!(" · ${c:.4}")).unwrap_or_default();
            line = line.push(if r.ok {
                t::metadata(format!("测试：通了 · {:.1}s{cost}", r.seconds)).color(pal::ok())
            } else {
                t::metadata(format!("测试：不通——{}", r.error)).color(pal::bad())
            });
        }
        col = col.push(line);
    }
    let mut files = row![t::metadata(format!("配置：{}{}", l.path, if l.exists { "" } else { "（还没有：现在只有内置的 claude-cli）" })).color(pal::dim())]
        .spacing(space(2))
        .align_y(Alignment::Center);
    if l.exists {
        files = files.push(w::btn("在 Studio 打开", Kind::Ghost, Some(AiMsg::OpenFile(l.path.clone()))));
    } else {
        files = files.push(w::btn("建配置文件", Kind::Standard, Some(AiMsg::Init)));
    }
    col = col.push(files.wrap());
    let mut keys = row![t::metadata(format!("密钥：{}（{}）", l.env_file, if l.env_file_exists { "有" } else { "还没有" })).color(pal::dim())]
        .spacing(space(2))
        .align_y(Alignment::Center);
    if l.env_file_exists {
        keys = keys.push(w::btn("在 Studio 打开", Kind::Ghost, Some(AiMsg::OpenFile(l.env_file.clone()))));
    }
    col = col.push(keys.wrap());
    col = col.push(
        t::metadata("改配置不用重启：点「刷新」，下一次调用就用新的。密钥只放环境变量：写进上面的密钥文件（一行一个「变量名=密钥」），改了要重启 Cockpit。「测一下」是一次真实调用，会花一点额度。")
            .color(pal::dim()),
    );
    if !msg.is_empty() {
        col = col.push(t::caption(msg).color(pal::dim()));
    }
    container(col)
        .padding(space(3))
        .style(|_| iced::widget::container::Style { border: crate::ui::metrics::hairline(crate::ui::metrics::radius::SM), ..Default::default() })
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_parses_python_output_and_ignores_extra_fields() {
        // `python -m factory.ai providers --json` 的样子（多出来的字段忽略）
        let raw = r#"{"path": "/x/ai_providers.toml", "exists": true, "env_file": "/x/ai.env", "env_file_exists": false,
            "default": "ds", "providers": [
              {"name": "claude-cli", "kind": "claude-cli", "model": "claude-opus-5-5", "base_url": "", "key_env": "",
               "structured": "json_schema", "timeout": 300.0, "max_tokens": 8192, "price_in": null, "price_out": null,
               "ready": true, "note": "claude 命令：/bin/claude"},
              {"name": "ds", "kind": "openai-compatible", "model": "m", "base_url": "http://a/v1", "key_env": "DS_KEY",
               "ready": false, "note": "DS_KEY 没设置"}],
            "problems": ["接口名 'x' 重复"], "error": ""}"#;
        let l: Listing = serde_json::from_str(raw).unwrap();
        assert_eq!((l.default.as_str(), l.providers.len(), l.problems.len()), ("ds", 2, 1));
        assert!(l.providers[0].ready && !l.providers[1].ready && l.providers[1].key_env == "DS_KEY");
        let t: TestResult = serde_json::from_str(r#"{"name": "ds", "ok": false, "seconds": 0.0, "cost_usd": null, "error": "HTTP 401", "answer": null}"#).unwrap();
        assert!(!t.ok && t.error == "HTTP 401" && t.cost_usd.is_none());
    }
}
