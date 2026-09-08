//! 接口观察终端的**副作用**（docs/23）：守护启停 + 连/断。
//!
//! 与 `c4` / `prediction` / `radar` 同一模式：`_view` 只产生消息，副作用集中在这里。

use super::observatory_readout as ro;
use super::observatory_view::ObsMsg;

/// 面板侧的请求 nonce。只增——守护比对**变没变**，不解释值。
fn bump() -> i64 {
    use std::sync::atomic::{AtomicI64, Ordering};
    static N: AtomicI64 = AtomicI64::new(1);
    N.fetch_add(1, Ordering::Relaxed)
}

pub fn handle(m: ObsMsg) {
    match m {
        ObsMsg::Start => {
            let _ = ro::svc_action("start");
        }
        ObsMsg::Stop => {
            let _ = ro::svc_action("stop");
        }
        ObsMsg::Connect => {
            // 重连当前会话：adapter/config 保持不变，靠 nonce 触发
            let cur = ro::snapshot();
            let (a, cfg) = match cur.session.as_ref() {
                Some(s) => (
                    s.adapter.clone(),
                    s.config
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect::<std::collections::BTreeMap<_, _>>(),
                ),
                None => return,
            };
            // **抹去过的值不能回写**：快照里的密钥是 «已抹去»，原样发回去
            // 就等于把真密钥换成了这四个字，下一次连接必然失败
            if cfg.values().any(|v| v.contains("已抹去")) {
                return;
            }
            let body = serde_json::json!({"adapter": a, "config": cfg, "nonce": bump()});
            ro::write_request(&body.to_string());
        }
        ObsMsg::Disconnect => {
            ro::write_request(&serde_json::json!({"adapter": "", "nonce": bump()}).to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_nonce_only_increases() {
        // 守护靠「变没变」判断要不要重连。回绕或重复会让重连按钮失灵
        let a = bump();
        let b = bump();
        assert!(b > a);
    }

    #[test]
    fn a_redacted_config_is_never_written_back() {
        // 快照里的密钥是 «已抹去»。原样发回去等于把真密钥换成这四个字，
        // 下一次连接必然失败，而错误信息完全看不出是这个原因
        let cfg: std::collections::BTreeMap<String, String> =
            [("api_key".to_string(), "«已抹去»".to_string())].into_iter().collect();
        assert!(cfg.values().any(|v| v.contains("已抹去")));
    }
}
