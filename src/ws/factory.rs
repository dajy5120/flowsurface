//! 读 Redis `ws:factory:pool`（Factory 现役池，由 factory_pool_bridge.py 发布）→ cockpit 展示（docs/08 F4c）。
//! 解耦：cockpit 只读 Redis（不直连 sqlite/不跑工厂）。

use redis::{Client, Commands};
use serde::Deserialize;

pub const FACTORY_POOL_KEY: &str = "ws:factory:pool";

#[derive(Deserialize, Clone, Debug, Default)]
pub struct PoolMember {
    pub expr: String,
    pub weight: f64,
    pub ic_t: f64,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct FactoryPool {
    #[serde(default)]
    pub pool: Vec<PoolMember>,
    #[serde(default)]
    pub alphas: u64,
    #[serde(default)]
    pub n_pool: u64,
    #[serde(default)]
    pub evals: u64,
}

/// 轮询 `ws:factory:pool`（2s）→ 变化时发出现役池快照。
pub fn subscription(redis_url: String) -> iced::Subscription<FactoryPool> {
    use iced::futures::SinkExt;
    iced::Subscription::run_with(("ws-factory", redis_url), |(_, redis_url): &(&str, String)| {
        let redis_url = redis_url.clone();
        iced::stream::channel(
            8,
            move |mut output: iced::futures::channel::mpsc::Sender<FactoryPool>| async move {
                let (tx, mut rx) = tokio::sync::mpsc::channel::<FactoryPool>(8);
                std::thread::spawn(move || {
                    let mut conn = match Client::open(redis_url.as_str())
                        .and_then(|c| c.get_connection())
                    {
                        Ok(c) => c,
                        Err(_) => return,
                    };
                    let mut last = String::new();
                    loop {
                        let v: Option<String> = conn.get(FACTORY_POOL_KEY).ok().flatten();
                        if let Some(s) = v
                            && s != last
                        {
                            last = s.clone();
                            if let Ok(p) = serde_json::from_str::<FactoryPool>(&s)
                                && tx.blocking_send(p).is_err()
                            {
                                break;
                            }
                        }
                        std::thread::sleep(std::time::Duration::from_millis(2000));
                    }
                });
                while let Some(p) = rx.recv().await {
                    if output.send(p).await.is_err() {
                        break;
                    }
                }
            },
        )
    })
}

/// Factory 面板的交互（docs/20 §26）：目前只有 nightly 手动启停。
#[derive(Debug, Clone)]
pub enum FactoryMsg {
    RunNightly,
    StopNightly,
    /// 把每日定时**设为**开/关（两个独立按钮，非切换——切换式按钮看不出当前是哪态）。
    SetTimer(bool),
    /// 手动刷新：叫醒 poller 立刻取一次状态（不必等下一轮 4s）。
    Refresh,
}

/// 最近一次操作结果（面板无自有状态，用进程级静态承载，同 tardis_board 的做法）。
static ACTION_MSG: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

pub fn action_message() -> String {
    ACTION_MSG.lock().map(|m| m.clone()).unwrap_or_default()
}

pub fn handle(msg: FactoryMsg) {
    use super::factory_readout as ro;
    let m = match msg {
        FactoryMsg::RunNightly => ro::nightly_start(),
        FactoryMsg::StopNightly => ro::nightly_stop(),
        FactoryMsg::SetTimer(on) => ro::nightly_toggle_timer(on),
        FactoryMsg::Refresh => {
            ro::request_refresh();
            String::new() // 刷新无需反馈文字，时间戳自己会跳
        }
    };
    if let Ok(mut g) = ACTION_MSG.lock() {
        *g = m;
    }
}
