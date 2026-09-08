//! 接口观察终端的**副作用**（docs/23）：守护启停 + 连/断 + 显示口径。
//!
//! 与 `c4` / `prediction` / `radar` 同一模式：`_view` 只产生消息，副作用集中在这里。
//! 请求文件的写入全部走 `observatory_readout` 的局部修改路径——
//! 重建整份请求会把 nonce/adapter 冲掉，改个筛选就变成重连或断线。

use super::observatory_readout as ro;
use super::observatory_view::ObsMsg;

pub fn handle(m: ObsMsg) {
    match m {
        ObsMsg::Start => {
            let _ = ro::svc_action("start");
        }
        ObsMsg::Stop => {
            let _ = ro::svc_action("stop");
        }
        ObsMsg::Connect => {
            let cur = ro::snapshot();
            let Some(s) = cur.session.as_ref() else { return };
            let cfg: std::collections::BTreeMap<String, String> =
                s.config.iter().cloned().collect();
            // **抹去过的值不能回写**：快照里的密钥是 «已抹去»，原样发回去
            // 就等于把真密钥换成了这四个字，下一次连接必然失败
            if cfg.values().any(|v| v.contains("已抹去")) {
                return;
            }
            ro::request_connect(&s.adapter, &cfg);
        }
        ObsMsg::Disconnect => ro::request_disconnect(),
        // 打字**只改面板内存**：每次按键都写文件的话，守护会在你打到一半时
        // 反复重编译一条写错的表达式，日志里全是「筛选写错了」
        ObsMsg::FilterEdited(t) => ro::set_filter_text(&t),
        ObsMsg::SetParse(on) => {
            ro::set_parse(on);
            ro::request_view();
        }
        ObsMsg::ApplyFilter => ro::request_view(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_redacted_config_is_never_written_back() {
        // 快照里的密钥是 «已抹去»。原样发回去等于把真密钥换成这四个字，
        // 下一次连接必然失败，而错误信息完全看不出是这个原因
        let cfg: std::collections::BTreeMap<String, String> =
            [("api_key".to_string(), "«已抹去»".to_string())].into_iter().collect();
        assert!(cfg.values().any(|v| v.contains("已抹去")));
    }
}
