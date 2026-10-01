//! Cockpit ↔ Studio 跨进程命令（docs/35 §16.5 第 4 项，§14「两个程序是一个产品」）。
//!
//! 两个界面进程各监听一个本地 Unix socket，协议与控制面（`wealthspring-ipc::control`）同款：
//! **一条连接一行 JSON 命令、回一行 JSON 应答**。放在这个 crate 里是因为它是两边唯一共同
//! 依赖的库（Cockpit 所在的子模块不依赖主仓的 ipc / paths crate）。
//!
//! ```text
//! → {"cmd":"show_run","run_id":"20260929-172619"}
//! ← {"ok":true,"message":"Cockpit 已切到回测工作区"}
//! ```
//!
//! socket 在 `$XDG_RUNTIME_DIR/wealthspring/`（没有就 `/tmp/wealthspring-$USER/`，目录 0700）。
//! 对端没开 → [`send`] 返回「Studio 没在运行」这样的一句话，调用方原样显示给人看。

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Peer {
    Cockpit,
    Studio,
}

impl Peer {
    pub fn label(self) -> &'static str {
        match self {
            Peer::Cockpit => "Cockpit",
            Peer::Studio => "Studio",
        }
    }
}

/// 跨进程命令。
#[derive(Debug, Clone, PartialEq)]
pub enum UiCommand {
    /// Studio → Cockpit：在 Cockpit 里看这次回测（切到回测工作区；`run_id` 空 = 当前运行）
    ShowRun { run_id: String },
    /// Cockpit → Studio：在 Studio 里打开这个文件（策略源码）
    OpenFile { path: String, line: Option<u32> },
    /// 探活
    Ping,
}

impl UiCommand {
    pub fn to_json(&self) -> Value {
        match self {
            UiCommand::ShowRun { run_id } => json!({"cmd": "show_run", "run_id": run_id}),
            UiCommand::OpenFile { path, line } => json!({"cmd": "open_file", "path": path, "line": line}),
            UiCommand::Ping => json!({"cmd": "ping"}),
        }
    }

    pub fn from_json(v: &Value) -> Result<Self, String> {
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        match v.get("cmd").and_then(Value::as_str) {
            Some("show_run") => Ok(UiCommand::ShowRun { run_id: s("run_id") }),
            Some("open_file") => {
                let path = s("path");
                if path.is_empty() {
                    return Err("open_file 缺 path".into());
                }
                let line = v.get("line").and_then(Value::as_u64).map(|l| l as u32);
                Ok(UiCommand::OpenFile { path, line })
            }
            Some("ping") => Ok(UiCommand::Ping),
            Some(other) => Err(format!("不认识的命令 {other}")),
            None => Err("缺 cmd 字段".into()),
        }
    }
}

/// socket 所在目录。
pub fn dir() -> PathBuf {
    match std::env::var("XDG_RUNTIME_DIR") {
        Ok(x) if !x.is_empty() => PathBuf::from(x).join("wealthspring"),
        _ => PathBuf::from(format!("/tmp/wealthspring-{}", std::env::var("USER").unwrap_or_else(|_| "ws".into()))),
    }
}

/// 某一方监听的 socket。`WS_UI_BRIDGE_DIR` 可改目录（测试、样张用，不碰真实实例）。
pub fn sock(peer: Peer) -> PathBuf {
    let d = std::env::var("WS_UI_BRIDGE_DIR").ok().filter(|s| !s.is_empty()).map_or_else(dir, PathBuf::from);
    d.join(match peer {
        Peer::Cockpit => "ui-cockpit.sock",
        Peer::Studio => "ui-studio.sock",
    })
}

/// 给对端发一条命令。成功返回对端的一句回执；失败返回给人看的原因。
pub fn send(peer: Peer, cmd: &UiCommand) -> Result<String, String> {
    let p = sock(peer);
    let mut s = UnixStream::connect(&p).map_err(|_| format!("{} 没在运行（{} 连不上）", peer.label(), p.display()))?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = s.set_write_timeout(Some(Duration::from_secs(2)));
    let line = cmd.to_json().to_string() + "\n";
    s.write_all(line.as_bytes()).map_err(|e| format!("发给 {} 失败：{e}", peer.label()))?;
    let mut resp = String::new();
    BufReader::new(s).read_line(&mut resp).map_err(|e| format!("{} 没有应答：{e}", peer.label()))?;
    let v: Value = serde_json::from_str(resp.trim()).map_err(|e| format!("{} 的应答读不懂：{e}", peer.label()))?;
    let msg = v.get("message").and_then(Value::as_str).unwrap_or_default().to_string();
    if v.get("ok").and_then(Value::as_bool) == Some(true) { Ok(msg) } else { Err(msg) }
}

/// 在后台线程上监听自己的 socket；每条命令交给 `handler`（在监听线程上调用，应尽快返回——
/// 界面进程一般只是把命令放进队列、唤醒界面）。
///
/// 已有同名实例在监听（能连上）→ 不抢，返回错误；残留的死 socket → 删掉重绑。
pub fn listen<F>(me: Peer, handler: F) -> std::io::Result<()>
where
    F: Fn(UiCommand) -> Result<String, String> + Send + 'static,
{
    let p = sock(me);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700));
    }
    if UnixStream::connect(&p).is_ok() {
        return Err(std::io::Error::new(std::io::ErrorKind::AddrInUse, format!("另一个 {} 已在监听 {}", me.label(), p.display())));
    }
    let _ = std::fs::remove_file(&p);
    let listener = UnixListener::bind(&p)?;
    std::thread::Builder::new().name(format!("ui-bridge-{}", me.label())).spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let mut line = String::new();
            let Ok(clone) = stream.try_clone() else { continue };
            if BufReader::new(clone).read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
            let res = serde_json::from_str::<Value>(line.trim())
                .map_err(|e| format!("无法解析：{e}"))
                .and_then(|v| UiCommand::from_json(&v))
                .and_then(&handler);
            let out = match res {
                Ok(m) => json!({"ok": true, "message": m}),
                Err(m) => json!({"ok": false, "message": m}),
            };
            let _ = stream.write_all((out.to_string() + "\n").as_bytes());
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 命令来回() {
        for c in [
            UiCommand::ShowRun { run_id: "r1".into() },
            UiCommand::OpenFile { path: "/a/b.py".into(), line: Some(12) },
            UiCommand::OpenFile { path: "/a/b.py".into(), line: None },
            UiCommand::Ping,
        ] {
            assert_eq!(UiCommand::from_json(&c.to_json()), Ok(c));
        }
        assert!(UiCommand::from_json(&json!({"cmd": "rm_rf"})).is_err());
        assert!(UiCommand::from_json(&json!({"cmd": "open_file"})).is_err(), "没有路径的打开不接");
    }

    #[test]
    fn 监听_发送_对端不在() {
        let d = std::env::temp_dir().join(format!("ws-bridge-test-{}", std::process::id()));
        // SAFETY: 测试进程里只有这一个测试改这个变量
        unsafe { std::env::set_var("WS_UI_BRIDGE_DIR", &d) };
        assert!(send(Peer::Studio, &UiCommand::Ping).unwrap_err().contains("Studio 没在运行"));

        listen(Peer::Studio, |c| match c {
            UiCommand::Ping => Ok("pong".into()),
            UiCommand::OpenFile { path, .. } => Ok(format!("打开 {path}")),
            UiCommand::ShowRun { .. } => Err("Studio 不看回测".into()),
        })
        .expect("listen");
        assert_eq!(send(Peer::Studio, &UiCommand::Ping), Ok("pong".into()));
        assert_eq!(send(Peer::Studio, &UiCommand::OpenFile { path: "x.py".into(), line: None }), Ok("打开 x.py".into()));
        assert_eq!(send(Peer::Studio, &UiCommand::ShowRun { run_id: String::new() }), Err("Studio 不看回测".into()));
        assert!(listen(Peer::Studio, |_| Ok(String::new())).is_err(), "已有实例在听：不抢");
        let _ = std::fs::remove_dir_all(&d);
    }
}
