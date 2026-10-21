//! AI 接口（docs/53 AI0 / AI0b）：Cockpit 各处共用的接口清单、下拉框与「AI 接口」管理卡。
//!
//! 清单、校验、密钥、测试都只在 Python（`python -m factory.ai …`），这里只跑它、读 JSON。
//! 下拉框选的接口全局生效（策略中心 AI 研究助理、画布 AI 页共用）：选了下一次调用就用它，不用重启。
//!
//! 管理卡里能增删改接口、填密钥并测试（AI0b，与 Telegram 同一种做法）：密钥存进 `ai_keys.json`（权限 600），
//! **从界面交给 Python 走标准输入**（不进命令行参数）；界面只拿回打码后的样子，输入框是密码框，存完就清空。

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::sync::Mutex;

use iced::widget::{column, container, pick_list, row, text_input};
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
    /// 沿用哪个接口的密钥（空 = 自己的）
    #[serde(default)]
    pub key_ref: String,
    #[serde(default)]
    pub structured: String,
    #[serde(default)]
    pub timeout: Option<f64>,
    #[serde(default)]
    pub price_in: Option<f64>,
    #[serde(default)]
    pub price_out: Option<f64>,
    #[serde(default)]
    pub ready: bool,
    #[serde(default)]
    pub note: String,
    /// 密钥从哪来：`env:变量名` / `file` / 空
    #[serde(default)]
    pub key_source: String,
    /// 打码后的密钥（`sk-…9f2a`）
    #[serde(default)]
    pub key_masked: String,
    #[serde(default)]
    pub needs_key: bool,
    #[serde(default)]
    pub last_test: Option<TestResult>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Listing {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub exists: bool,
    #[serde(default)]
    pub keys_file: String,
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
    /// 测试时间（秒）
    #[serde(default)]
    pub at: Option<i64>,
}

/// 新增 / 编辑接口的表单（文字原样留着，保存时交给 Python 校验）。
#[derive(Debug, Clone, Default)]
pub struct Form {
    /// 改的是哪个（`None` = 新增）
    pub orig: Option<String>,
    pub name: String,
    pub kind: String,
    pub model: String,
    pub base_url: String,
    pub structured: String,
    /// 空 = 用自己的密钥
    pub key_ref: String,
    pub key_env: String,
    pub price_in: String,
    pub price_out: String,
    pub timeout: String,
    pub err: String,
    pub saving: bool,
}

/// 表单里能改的项。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Name,
    Kind,
    Model,
    BaseUrl,
    Structured,
    KeyRef,
    KeyEnv,
    PriceIn,
    PriceOut,
    Timeout,
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
    /// 建接口清单文件
    Init,
    /// 在 Studio 里打开接口清单
    OpenFile(String),
    /// 密钥输入框（密钥名 = 接口名）
    KeyInput(String, String),
    /// 存密钥（密钥名, 存完测哪个接口）
    KeySave(String, Option<String>),
    /// 删密钥：点两下确认
    KeyDelete(String),
    /// 打开表单：`None` = 新增；`Some(名字)` = 编辑
    Edit(Option<String>),
    /// 新增时用常用服务商预填（服务商在 [`PRESETS`] 里的下标）
    Preset(usize),
    FormField(Field, String),
    FormSave,
    FormCancel,
    /// 删接口：点两下确认
    Delete(String),
}

/// 常用服务商（只是预填，地址与模型名以服务商当前文档为准）：(按钮名, 名字, 类型, 地址, 模型, 结构化方式)。
pub const PRESETS: [(&str, &str, &str, &str, &str, &str); 7] = [
    ("Claude 官方 API", "claude-api", "anthropic", "", "claude-opus-5-5", "json_schema"),
    ("OpenAI", "openai", "openai-compatible", "https://api.openai.com/v1", "gpt-4o", "json_schema"),
    ("DeepSeek", "deepseek", "openai-compatible", "https://api.deepseek.com/v1", "deepseek-chat", "json_object"),
    ("通义千问", "qwen", "openai-compatible", "https://dashscope.aliyuncs.com/compatible-mode/v1", "qwen-plus", "json_object"),
    ("Kimi", "kimi", "openai-compatible", "https://api.moonshot.cn/v1", "moonshot-v1-32k", "json_object"),
    ("智谱", "zhipu", "openai-compatible", "https://open.bigmodel.cn/api/paas/v4", "glm-4", "json_object"),
    ("本机 Ollama", "ollama", "openai-compatible", "http://localhost:11434/v1", "qwen2.5-coder:14b", "json_schema"),
];

const KINDS: [&str; 3] = ["claude-cli", "anthropic", "openai-compatible"];
const STRUCTURED: [&str; 3] = ["json_schema", "json_object", "none"];
const OWN_KEY: &str = "用自己的密钥";

#[derive(Default)]
struct St {
    listing: Option<Listing>,
    loading: bool,
    err: String,
    /// 本次会话选的接口（`None` = 用配置的缺省）
    chosen: Option<String>,
    open: bool,
    testing: HashSet<String>,
    /// 密钥输入框里还没存的（只在内存里，存完就清）
    keys: HashMap<String, String>,
    key_saving: HashSet<String>,
    /// 等第二下确认的删除：`key:名字` / `prov:名字`
    confirm: Option<String>,
    form: Option<Form>,
    msg: String,
}

static ST: Mutex<Option<St>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut St) -> R) -> R {
    let mut g = ST.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    f(g.get_or_insert_with(St::default))
}

/// 跑 `python -m factory.ai …`；`stdin` 给的话写进标准输入（密钥走这里，不进参数）。
fn python(args: &[&str], stdin: Option<&str>) -> Result<serde_json::Value, String> {
    let mut child = std::process::Command::new(super::paths::python())
        .args(["-m", "factory.ai"])
        .args(args)
        .current_dir(super::paths::repo_root())
        .stdin(if stdin.is_some() { std::process::Stdio::piped() } else { std::process::Stdio::null() })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("起不来 Python：{e}"))?;
    if let (Some(s), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let _ = pipe.write_all(s.as_bytes());
    }
    let out = child.wait_with_output().map_err(|e| format!("Python 没跑完：{e}"))?;
    serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .map_err(|_| String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("factory.ai 失败").trim_start_matches("✗ ").to_string())
}

/// `{"ok": false, "error": …}` → Err。
fn ok_or_err(r: Result<serde_json::Value, String>) -> Result<serde_json::Value, String> {
    let v = r?;
    if v.get("ok") == Some(&serde_json::Value::Bool(false)) && v.get("seconds").is_none() {
        return Err(v["error"].as_str().unwrap_or("失败").to_string());
    }
    Ok(v)
}

fn load() {
    if !with(|g| !std::mem::replace(&mut g.loading, true)) {
        return;
    }
    super::spawn_named("ws-aiprov", || {
        let r = python(&["providers", "--json"], None).and_then(|v| serde_json::from_value::<Listing>(v).map_err(|e| format!("接口清单解析失败：{e}")));
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

fn run_test(name: String) {
    if !with(|g| g.testing.insert(name.clone())) {
        return;
    }
    super::spawn_named("ws-aitest", move || {
        let r = ok_or_err(python(&["test", &name, "--json"], None));
        with(|g| {
            g.testing.remove(&name);
            if let Err(e) = r {
                g.msg = format!("{name}：{e}");
            }
        });
        load(); // 测试结果 Python 记下了：重读清单就显示
    });
}

fn opt_num(s: &str) -> Result<serde_json::Value, String> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(serde_json::Value::Null);
    }
    s.parse::<f64>().map(serde_json::Value::from).map_err(|_| format!("「{s}」不是数"))
}

fn form_json(f: &Form) -> Result<serde_json::Value, String> {
    let mut m = serde_json::Map::new();
    m.insert("name".into(), f.name.trim().into());
    m.insert("kind".into(), f.kind.clone().into());
    m.insert("model".into(), f.model.trim().into());
    if f.kind != "claude-cli" {
        m.insert("base_url".into(), f.base_url.trim().into());
        m.insert("key_ref".into(), f.key_ref.clone().into());
        m.insert("key_env".into(), f.key_env.trim().into());
        for (k, v) in [("price_in", &f.price_in), ("price_out", &f.price_out)] {
            m.insert(k.into(), opt_num(v)?);
        }
    }
    if f.kind == "openai-compatible" {
        m.insert("structured".into(), f.structured.clone().into());
    }
    m.insert("timeout".into(), opt_num(&f.timeout)?);
    Ok(serde_json::Value::Object(m))
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
        AiMsg::Test(name) => run_test(name),
        AiMsg::SetDefault(name) => {
            super::spawn_named("ws-aiprov", move || {
                let r = ok_or_err(python(&["default", &name, "--json"], None));
                with(|g| {
                    g.msg = match r {
                        Ok(_) => {
                            g.chosen = None;
                            format!("缺省接口改成了 {name}")
                        }
                        Err(e) => e,
                    };
                });
                load();
            });
        }
        AiMsg::Init => {
            super::spawn_named("ws-aiprov", || {
                let r = ok_or_err(python(&["init", "--json"], None));
                with(|g| g.msg = r.map_or_else(|e| e, |_| "已建接口清单".into()));
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
        AiMsg::KeyInput(r, v) => with(|g| {
            g.keys.insert(r, v);
        }),
        AiMsg::KeySave(r, test) => {
            let Some(key) = with(|g| {
                let k = g.keys.get(&r).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())?;
                g.key_saving.insert(r.clone()).then_some(k)
            }) else {
                return;
            };
            super::spawn_named("ws-aikey", move || {
                let res = ok_or_err(python(&["set-key", &r, "--stdin", "--json"], Some(&key)));
                drop(key);
                with(|g| {
                    g.key_saving.remove(&r);
                    match &res {
                        Ok(v) => {
                            g.keys.remove(&r); // 存上了就把明文从内存里清掉
                            g.msg = format!("{r} 的密钥已保存（{}）", v["masked"].as_str().unwrap_or(""));
                        }
                        Err(e) => g.msg = format!("密钥没存上：{e}"),
                    }
                });
                load();
                if let (Ok(_), Some(t)) = (res, test) {
                    run_test(t);
                }
            });
        }
        AiMsg::KeyDelete(r) => {
            let go = with(|g| {
                let k = format!("key:{r}");
                if g.confirm.as_deref() == Some(k.as_str()) {
                    g.confirm = None;
                    true
                } else {
                    g.confirm = Some(k);
                    false
                }
            });
            if go {
                super::spawn_named("ws-aikey", move || {
                    let res = ok_or_err(python(&["delete-key", &r, "--json"], None));
                    with(|g| g.msg = res.map_or_else(|e| e, |_| format!("{r} 的密钥已删除")));
                    load();
                });
            }
        }
        AiMsg::Edit(name) => with(|g| {
            let p = name.as_ref().and_then(|n| g.listing.as_ref()?.providers.iter().find(|p| &p.name == n).cloned());
            let num = |x: Option<f64>| x.map(|v| v.to_string()).unwrap_or_default();
            g.form = Some(match p {
                Some(p) => Form {
                    orig: Some(p.name.clone()),
                    name: p.name,
                    kind: p.kind,
                    model: p.model,
                    base_url: p.base_url,
                    structured: if p.structured.is_empty() { "json_schema".into() } else { p.structured },
                    key_ref: p.key_ref,
                    key_env: if p.key_env == "ANTHROPIC_API_KEY" { String::new() } else { p.key_env },
                    price_in: num(p.price_in),
                    price_out: num(p.price_out),
                    timeout: num(p.timeout.filter(|t| (*t - 300.0).abs() > f64::EPSILON)),
                    ..Form::default()
                },
                None => Form { kind: "openai-compatible".into(), structured: "json_schema".into(), ..Form::default() },
            });
            g.confirm = None;
        }),
        AiMsg::Preset(i) => with(|g| {
            if let (Some(f), Some((_, name, kind, url, model, st))) = (g.form.as_mut(), PRESETS.get(i)) {
                let taken = g.listing.as_ref().is_some_and(|l| l.providers.iter().any(|p| p.name == *name));
                f.name = if taken { format!("{name}-2") } else { (*name).to_string() };
                f.kind = (*kind).to_string();
                f.base_url = (*url).to_string();
                f.model = (*model).to_string();
                f.structured = (*st).to_string();
                f.err.clear();
            }
        }),
        AiMsg::FormField(k, v) => with(|g| {
            if let Some(f) = g.form.as_mut() {
                let slot = match k {
                    Field::Name => &mut f.name,
                    Field::Kind => &mut f.kind,
                    Field::Model => &mut f.model,
                    Field::BaseUrl => &mut f.base_url,
                    Field::Structured => &mut f.structured,
                    Field::KeyRef => &mut f.key_ref,
                    Field::KeyEnv => &mut f.key_env,
                    Field::PriceIn => &mut f.price_in,
                    Field::PriceOut => &mut f.price_out,
                    Field::Timeout => &mut f.timeout,
                };
                *slot = if k == Field::KeyRef && v == OWN_KEY { String::new() } else { v.strip_prefix("沿用 ").map_or(v.clone(), str::to_string) };
            }
        }),
        AiMsg::FormCancel => with(|g| g.form = None),
        AiMsg::FormSave => {
            let Some((json, orig)) = with(|g| {
                let f = g.form.as_mut()?;
                if f.saving {
                    return None;
                }
                match form_json(f) {
                    Ok(j) => {
                        f.saving = true;
                        f.err.clear();
                        Some((j.to_string(), f.orig.clone()))
                    }
                    Err(e) => {
                        f.err = e;
                        None
                    }
                }
            }) else {
                return;
            };
            super::spawn_named("ws-aiprov", move || {
                let mut args = vec!["save-provider", "--stdin", "--json"];
                if let Some(o) = orig.as_deref() {
                    args.extend(["--orig", o]);
                }
                let r = ok_or_err(python(&args, Some(&json)));
                with(|g| match r {
                    Ok(v) => {
                        g.form = None;
                        g.msg = format!("已保存接口 {}", v["provider"]["name"].as_str().unwrap_or(""));
                    }
                    Err(e) => {
                        if let Some(f) = g.form.as_mut() {
                            f.saving = false;
                            f.err = e;
                        }
                    }
                });
                load();
            });
        }
        AiMsg::Delete(name) => {
            let go = with(|g| {
                let k = format!("prov:{name}");
                if g.confirm.as_deref() == Some(k.as_str()) {
                    g.confirm = None;
                    true
                } else {
                    g.confirm = Some(k);
                    false
                }
            });
            if go {
                super::spawn_named("ws-aiprov", move || {
                    let r = ok_or_err(python(&["delete-provider", &name, "--json"], None));
                    with(|g| g.msg = r.map_or_else(|e| e, |_| format!("已删除接口 {name}（连同它的密钥）")));
                    load();
                });
            }
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

fn when(at: Option<i64>) -> String {
    at.and_then(|s| chrono::DateTime::from_timestamp(s, 0))
        .map(|d| d.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

/// 「AI 接口」管理卡：每个接口——名字、类型、模型、缺省 / 设为默认、测一下、编辑、删除；密钥状态与密钥框；上次测试。
/// 下面是「＋ 添加接口」与表单。
pub fn manager<'a>() -> Element<'a, AiMsg> {
    let (l, loading, err, testing, keys, saving, confirm, form, msg) = with(|g| {
        (g.listing.clone(), g.loading, g.err.clone(), g.testing.clone(), g.keys.clone(), g.key_saving.clone(), g.confirm.clone(), g.form.clone(), g.msg.clone())
    });
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
        col = col.push(t::caption(format!("接口清单没读：{}（现在只用内置的 claude-cli）", l.error)).color(pal::bad()));
    }
    for p in &l.problems {
        col = col.push(t::metadata(format!("⚠ {p}")).color(pal::warn()));
    }
    let names: Vec<String> = l.providers.iter().map(|p| p.name.clone()).collect();
    for p in &l.providers {
        let is_default = p.name == l.default;
        let mut head = row![t::label(p.name.clone()), t::metadata(format!("{} · {}", p.kind, p.model)).color(pal::dim())]
            .spacing(space(2))
            .align_y(Alignment::Center);
        head = head.push(if is_default {
            Element::from(t::metadata("缺省").color(pal::accent()))
        } else {
            w::btn("设为默认", Kind::Ghost, Some(AiMsg::SetDefault(p.name.clone())))
        });
        head = head.push(w::btn_busy("测一下", Kind::Ghost, Some(AiMsg::Test(p.name.clone())), testing.contains(&p.name)));
        head = head.push(w::btn("编辑", Kind::Ghost, Some(AiMsg::Edit(Some(p.name.clone())))));
        let del = confirm.as_deref() == Some(format!("prov:{}", p.name).as_str());
        head = head.push(w::btn(if del { "确认删除（连同密钥）" } else { "删除" }, if del { Kind::Destructive } else { Kind::Ghost }, Some(AiMsg::Delete(p.name.clone()))));
        let mut item = column![head.wrap(), t::metadata(format!("{} {}", if p.ready { "✓" } else { "✗" }, p.note)).color(if p.ready { pal::ok() } else { pal::warn() })]
            .spacing(0);
        if let Some(r) = &p.last_test {
            let cost = r.cost_usd.map(|c| format!(" · ${c:.4}")).unwrap_or_default();
            item = item.push(if r.ok {
                t::metadata(format!("上次测试 {}：通了 · {:.1}s{cost}", when(r.at), r.seconds)).color(pal::ok())
            } else {
                t::metadata(format!("上次测试 {}：不通——{}", when(r.at), r.error)).color(pal::bad())
            });
        }
        // 密钥：claude-cli 不要；沿用别人的就不在这里填
        if p.kind != "claude-cli" && p.key_ref.is_empty() {
            let r = p.name.clone();
            let cur = keys.get(&r).cloned().unwrap_or_default();
            let ph = if p.key_source == "file" { format!("已保存 {}——要换就粘贴新的", p.key_masked) } else if p.needs_key { "粘贴密钥".to_string() } else { "可以不要；要的话粘贴".to_string() };
            let (r1, r2, r3) = (r.clone(), r.clone(), r.clone());
            let has = !cur.trim().is_empty();
            let mut keyrow = row![
                text_input(&ph, &cur).secure(true).on_input(move |v| AiMsg::KeyInput(r1.clone(), v)).on_submit(AiMsg::KeySave(r2.clone(), None)).size(t::s_small()).width(Length::Fill),
                w::btn_busy("保存", Kind::Ghost, has.then(|| AiMsg::KeySave(r3.clone(), None)), saving.contains(&r)),
                w::btn("保存并测试", Kind::Standard, has.then(|| AiMsg::KeySave(r.clone(), Some(r.clone())))),
            ]
            .spacing(space(1))
            .align_y(Alignment::Center);
            if p.key_source == "file" {
                let dk = confirm.as_deref() == Some(format!("key:{r}").as_str());
                keyrow = keyrow.push(w::btn(if dk { "确认删除密钥" } else { "删除密钥" }, if dk { Kind::Destructive } else { Kind::Ghost }, Some(AiMsg::KeyDelete(r.clone()))));
            }
            item = item.push(keyrow.wrap());
            if p.key_source.starts_with("env:") {
                item = item.push(t::metadata("环境变量优先：要用这里填的密钥，先去掉那个环境变量").color(pal::dim()));
            }
        }
        col = col.push(container(item).padding(iced::Padding::ZERO.bottom(space(2))));
    }
    match &form {
        Some(f) => col = col.push(form_view(f, &names)),
        None => col = col.push(w::btn("＋ 添加接口", Kind::Standard, Some(AiMsg::Edit(None)))),
    }
    let mut files = row![t::metadata(format!("接口清单：{}{}", l.path, if l.exists { "" } else { "（还没有：现在只有内置的 claude-cli）" })).color(pal::dim())]
        .spacing(space(2))
        .align_y(Alignment::Center);
    if l.exists {
        files = files.push(w::btn("在 Studio 打开", Kind::Ghost, Some(AiMsg::OpenFile(l.path.clone()))));
    } else {
        files = files.push(w::btn("建接口清单", Kind::Ghost, Some(AiMsg::Init)));
    }
    col = col.push(files.wrap());
    col = col.push(
        t::metadata(format!(
            "密钥存在 {}（只有你能读，权限 600；不进仓库），每次调用现读，改了不用重启。环境变量（接口里写了 key_env）优先。「测一下」是一次真实调用，会花一点额度。",
            l.keys_file
        ))
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

fn form_view<'a>(f: &Form, names: &[String]) -> Element<'a, AiMsg> {
    let input = |label: &str, hint: &str, v: &str, k: Field| -> Element<'a, AiMsg> {
        column![t::caption(label.to_string()), text_input(hint, v).on_input(move |s| AiMsg::FormField(k, s)).size(t::s_small())].spacing(0).into()
    };
    let mut col = column![w::section(if f.orig.is_some() { "编辑接口" } else { "添加接口" })].spacing(space(2));
    if f.orig.is_none() {
        let mut presets = row![t::metadata("常用：").color(pal::dim())].spacing(space(1)).align_y(Alignment::Center);
        for (i, (label, ..)) in PRESETS.iter().enumerate() {
            presets = presets.push(w::btn(*label, Kind::Ghost, Some(AiMsg::Preset(i))));
        }
        col = col.push(presets.wrap());
        col = col.push(t::metadata("预填的地址与模型名以服务商当前文档为准").color(pal::dim()));
    }
    col = col.push(input("名字（字母、数字、- _ .）", "deepseek", &f.name, Field::Name));
    col = col.push(column![
        t::caption("类型"),
        pick_list(KINDS.map(String::from).to_vec(), Some(f.kind.clone()), |s| AiMsg::FormField(Field::Kind, s)).text_size(t::s_small()),
        t::metadata(match f.kind.as_str() {
            "claude-cli" => "Claude Code CLI，走订阅额度，不要密钥",
            "anthropic" => "Claude 官方 API，按量计费，要密钥",
            _ => "OpenAI 兼容接口：OpenAI、DeepSeek、通义千问、Kimi、智谱、本机 Ollama / vLLM……",
        })
        .color(pal::dim()),
    ]);
    col = col.push(input("模型", "deepseek-chat", &f.model, Field::Model));
    if f.kind != "claude-cli" {
        col = col.push(input(
            if f.kind == "anthropic" { "地址（可不填）" } else { "地址（base_url）" },
            "https://api.deepseek.com/v1",
            &f.base_url,
            Field::BaseUrl,
        ));
    }
    if f.kind == "openai-compatible" {
        col = col.push(column![
            t::caption("结构化输出"),
            pick_list(STRUCTURED.map(String::from).to_vec(), Some(f.structured.clone()), |s| AiMsg::FormField(Field::Structured, s)).text_size(t::s_small()),
            t::metadata("json_schema = 支持按结构回答（OpenAI、新版 Ollama）；json_object = 只保证回 JSON（DeepSeek 等）；none = 都不支持").color(pal::dim()),
        ]);
    }
    if f.kind != "claude-cli" {
        let mut opts = vec![OWN_KEY.to_string()];
        opts.extend(names.iter().filter(|n| Some(*n) != f.orig.as_ref()).map(|n| format!("沿用 {n}")));
        let cur = if f.key_ref.is_empty() { OWN_KEY.to_string() } else { format!("沿用 {}", f.key_ref) };
        col = col.push(column![
            t::caption("密钥"),
            pick_list(opts, Some(cur), |s| AiMsg::FormField(Field::KeyRef, s)).text_size(t::s_small()),
            t::metadata("同一家的几个模型可以共用一个密钥；密钥本身保存接口后在列表里填").color(pal::dim()),
        ]);
        col = col.push(input("环境变量名（可不填；填了就优先用它）", if f.kind == "anthropic" { "ANTHROPIC_API_KEY" } else { "DEEPSEEK_API_KEY" }, &f.key_env, Field::KeyEnv));
        col = col.push(row![
            container(input("输入价（$/百万 token，可不填）", "0.27", &f.price_in, Field::PriceIn)).width(Length::FillPortion(1)),
            container(input("输出价", "1.10", &f.price_out, Field::PriceOut)).width(Length::FillPortion(1)),
        ].spacing(space(2)));
    }
    col = col.push(input("超时（秒，可不填 = 300）", "300", &f.timeout, Field::Timeout));
    col = col.push(
        row![
            w::btn_busy("保存接口", Kind::Primary, (!f.saving).then_some(AiMsg::FormSave), f.saving),
            w::btn("取消", Kind::Ghost, Some(AiMsg::FormCancel)),
        ]
        .spacing(space(2)),
    );
    if !f.err.is_empty() {
        col = col.push(t::caption(f.err.clone()).color(pal::bad()));
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
        let raw = r#"{"path": "/x/ai_providers.toml", "exists": true, "keys_file": "/x/ai_keys.json", "env_file": "/x/ai.env",
            "default": "ds", "kinds": [], "structured": [], "providers": [
              {"name": "claude-cli", "kind": "claude-cli", "model": "claude-opus-5-5", "base_url": "", "key_env": "", "key_ref": "",
               "structured": "json_schema", "timeout": 300.0, "max_tokens": 8192, "price_in": null, "price_out": null,
               "ready": true, "note": "走订阅额度", "key_source": "", "key_masked": "", "key_ref_of": "claude-cli", "needs_key": false},
              {"name": "ds", "kind": "openai-compatible", "model": "m", "base_url": "http://a/v1", "key_env": "", "key_ref": "",
               "ready": true, "note": "密钥 sk-…9f2a（已保存）", "key_source": "file", "key_masked": "sk-…9f2a", "needs_key": true,
               "price_in": 0.27, "last_test": {"name": "ds", "ok": false, "seconds": 0.4, "cost_usd": null, "error": "HTTP 401", "at": 1790000000}}],
            "problems": ["接口名 'x' 重复"], "error": ""}"#;
        let l: Listing = serde_json::from_str(raw).unwrap();
        assert_eq!((l.default.as_str(), l.providers.len(), l.problems.len()), ("ds", 2, 1));
        let ds = &l.providers[1];
        assert!(ds.ready && ds.key_source == "file" && ds.key_masked == "sk-…9f2a" && ds.price_in == Some(0.27));
        let t = ds.last_test.as_ref().unwrap();
        assert!(!t.ok && t.error == "HTTP 401" && t.at == Some(1_790_000_000));
    }

    #[test]
    fn form_json_only_sends_what_the_kind_uses() {
        let f = Form { name: " ds ".into(), kind: "openai-compatible".into(), model: "m".into(), base_url: "https://a/v1".into(),
                       structured: "json_object".into(), price_in: "0.27".into(), ..Form::default() };
        let j = form_json(&f).unwrap();
        assert_eq!(j["name"], "ds");
        assert_eq!(j["price_in"], 0.27);
        assert!(j["price_out"].is_null() && j["timeout"].is_null());
        assert_eq!(j["structured"], "json_object");
        let cli = Form { name: "c".into(), kind: "claude-cli".into(), ..Form::default() };
        let j = form_json(&cli).unwrap();
        assert!(j.get("base_url").is_none() && j.get("key_ref").is_none(), "claude-cli 不要地址与密钥");
        let bad = Form { name: "x".into(), kind: "anthropic".into(), price_in: "贵".into(), ..Form::default() };
        assert!(form_json(&bad).unwrap_err().contains("不是数"));
    }
}
