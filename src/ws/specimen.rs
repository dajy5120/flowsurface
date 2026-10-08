//! 界面**样张模式**（docs/35 批 0 / 批 9）：自动轮换全部工作区并自截图，用于界面重构的截图对比。
//!
//! # 怎么用
//!
//! ```text
//! mkdir -p /tmp/spec && cp ~/.local/share/flowsurface/saved-state.json /tmp/spec/
//! FLOWSURFACE_DATA_PATH=/tmp/spec WS_UI_SPECIMEN=/tmp/spec/shots flowsurface
//! ```
//!
//! 每个工作区等 `WS_UI_SPECIMEN_SETTLE` 秒（缺省 6）让面板把数据读进来，再截主窗口，
//! 存成 `NN-工作区名.ppm`（iced 给的是原始 RGBA，本 crate 没有 PNG 编码器，PPM 零依赖；
//! 要 PNG 用 `convert x.ppm x.png`）。全部截完自动退出。
//!
//! # 为什么必须配 `FLOWSURFACE_DATA_PATH`
//!
//! 样张实例和日常开着的 Cockpit 是**两个进程**。它若读写同一份 saved-state，
//! 退出时会把轮换到的最后一个工作区写回去；所以要给它一份副本。
//!
//! # 样张实例不碰守护
//!
//! `main.rs` 在样张模式下**跳过** `lifecycle::start_all/stop_all` 与
//! `egress::apply_startup`：否则它退出时会停掉 `ws-stack.target`（日常那个 Cockpit
//! 的全部守护跟着停），启动时还会按「启动时关闭对外连接」把对外服务停掉。

use std::path::PathBuf;
use std::time::{Duration, Instant};

/// 样张模式开着吗（读一次环境变量）。
pub fn enabled() -> bool {
    std::env::var_os("WS_UI_SPECIMEN").is_some()
}

pub struct Specimen {
    pub dir: PathBuf,
    settle: Duration,
    /// 待截的工作区（id, 名字），按侧栏顺序。
    pub queue: Vec<(uuid::Uuid, String)>,
    pub idx: usize,
    started: Option<Instant>,
    requested: bool,
}

/// 这一拍该做什么。
pub enum Step {
    Nothing,
    /// 切到第 `idx` 个工作区。
    Load(uuid::Uuid),
    /// 截图。
    Shoot,
}

impl Specimen {
    pub fn from_env() -> Option<Self> {
        let dir = PathBuf::from(std::env::var_os("WS_UI_SPECIMEN")?);
        let settle = std::env::var("WS_UI_SPECIMEN_SETTLE")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(6.0);
        let _ = std::fs::create_dir_all(&dir);
        Some(Self {
            dir,
            settle: Duration::from_secs_f64(settle),
            queue: Vec::new(),
            idx: 0,
            started: None,
            requested: false,
        })
    }

    /// 每 500ms 调一次。`queue` 由调用方在第一拍填好。
    pub fn step(&mut self, now: Instant) -> Step {
        let Some((id, _)) = self.queue.get(self.idx) else { return Step::Nothing };
        match self.started {
            None => {
                self.started = Some(now);
                self.requested = false;
                Step::Load(*id)
            }
            Some(t) if !self.requested && now.duration_since(t) >= self.settle => {
                self.requested = true;
                Step::Shoot
            }
            _ => Step::Nothing,
        }
    }

    /// 截图回来了：落盘，进下一个。返回 true = 全部截完。
    pub fn save(&mut self, rgba: &[u8], w: u32, h: u32) -> bool {
        if let Some((_, name)) = self.queue.get(self.idx) {
            let file = self.dir.join(format!("{:02}-{}.ppm", self.idx, sanitize(name)));
            match write_ppm(&file, rgba, w, h) {
                Ok(()) => log::info!("[specimen] {} → {}", name, file.display()),
                Err(e) => log::error!("[specimen] 写 {} 失败：{e}", file.display()),
            }
        }
        self.idx += 1;
        self.started = None;
        self.idx >= self.queue.len()
    }
}

/// 文件名里不能有 `/`（「期权/0DTE」）。
fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c == '/' || c == ' ' { '_' } else { c }).collect()
}

/// RGBA → PPM（P6，丢掉 alpha）。
/// 追加一个工作区的焦点顺序（F6 依次经过的面板）到 `focus-order.md`。
pub fn append_focus_order(dir: &std::path::Path, workspace: &str, panes: &[String]) {
    use std::io::Write;
    let p = dir.join("focus-order.md");
    let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) else { return };
    let order: Vec<String> = panes.iter().enumerate().map(|(i, n)| format!("{}. {n}", i + 1)).collect();
    let _ = writeln!(f, "| {workspace} | {} | {} |", panes.len(), order.join(" → "));
}

/// 追加一页的非内容高度到 `chrome.json`（一行一个 JSON；`scripts/ui_chrome_gate.py` 判定）。
/// 本页没有被托管的面板时 `m` 是 `None`，照样写一行，闸门据此区分「没测」与「没标记」。
pub fn append_chrome(dir: &std::path::Path, page: &str, m: Option<crate::ui::mark::Measure>) {
    use std::io::Write;
    let p = dir.join("chrome.json");
    let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) else { return };
    let v = match m {
        Some(m) => serde_json::json!({ "page": page, "panel": m.panel, "px": m.px }),
        None => serde_json::json!({ "page": page, "panel": null, "px": null }),
    };
    let _ = writeln!(f, "{v}");
}

fn write_ppm(path: &std::path::Path, rgba: &[u8], w: u32, h: u32) -> std::io::Result<()> {
    let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
    out.reserve((w * h * 3) as usize);
    for px in rgba.chunks_exact(4) {
        out.extend_from_slice(&px[..3]);
    }
    std::fs::write(path, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 轮换节奏_先切再等再截() {
        let mut s = Specimen {
            dir: std::env::temp_dir(),
            settle: Duration::from_secs(2),
            queue: vec![(uuid::Uuid::nil(), "a".into()), (uuid::Uuid::nil(), "b".into())],
            idx: 0,
            started: None,
            requested: false,
        };
        let t0 = Instant::now();
        assert!(matches!(s.step(t0), Step::Load(_)));
        assert!(matches!(s.step(t0 + Duration::from_secs(1)), Step::Nothing));
        assert!(matches!(s.step(t0 + Duration::from_secs(2)), Step::Shoot));
        // 截图还没回来，不重复要
        assert!(matches!(s.step(t0 + Duration::from_secs(3)), Step::Nothing));
        assert!(!s.save(&[0; 4], 1, 1));
        assert!(matches!(s.step(t0 + Duration::from_secs(3)), Step::Load(_)));
        assert!(s.save(&[0; 4], 1, 1), "两个都截完");
    }

    #[test]
    fn 文件名去掉斜杠() {
        assert_eq!(sanitize("期权/0DTE"), "期权_0DTE");
    }
}
