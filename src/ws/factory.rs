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
