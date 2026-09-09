//! 接口观察终端的**副作用**（docs/23）：守护启停 + 连/断 + 显示口径。
//!
//! 与 `c4` / `prediction` / `radar` 同一模式：`_view` 只产生消息，副作用集中在这里。
//! 请求文件的写入全部走 `observatory_readout` 的局部修改路径——
//! 重建整份请求会把 nonce/adapter 冲掉，改个筛选就变成重连或断线。

use super::observatory_hist as hist;
use super::observatory_lib as lib;
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
            // 用**表单里的**值，不是快照里的：快照里的密钥已被守护抹成
            // «已抹去»，回读等于把真密钥换成那四个字，下一次连接必然失败
            let cur = ro::snapshot();
            if ro::form_ready(&cur.catalog).is_err() {
                return;
            }
            let (id, vals) = ro::form(&cur.catalog);
            ro::request_connect(&id, &vals);
        }
        ObsMsg::PickAdapter(id) => {
            let cur = ro::snapshot();
            ro::form_pick(&cur.catalog, &id);
        }
        ObsMsg::FieldEdited(k, v) => ro::form_set(&k, &v),
        ObsMsg::Disconnect => ro::request_disconnect(),
        // 打字**只改面板内存**：每次按键都写文件的话，守护会在你打到一半时
        // 反复重编译一条写错的表达式，日志里全是「筛选写错了」
        ObsMsg::FilterEdited(t) => ro::set_filter_text(&t),
        ObsMsg::SetParse(on) => {
            ro::set_parse(on);
            ro::request_view();
        }
        ObsMsg::ApplyFilter => ro::request_view(),
        ObsMsg::SetRecord(on) => ro::request_record(on),
        ObsMsg::SetMode(m) => ro::set_mode(m),
        // 暂停**只改面板**：不发请求文件，后端照常收、照常录、照常触发
        ObsMsg::TogglePause => {
            let cur = ro::snapshot();
            ro::toggle_pause(&cur);
        }
        ObsMsg::SendEdited(t) => ro::set_send_body(&t),
        ObsMsg::SendNow => {
            // 发到**当前 Adapter 的控制流**：它是双向那条。
            // 写死 0 的话，在响应流是 0 的 Adapter 上会把请求发到数据流里
            let cur = ro::snapshot();
            let sid = cur
                .session
                .as_ref()
                .and_then(|s| cur.catalog.iter().find(|a| a.id == s.adapter))
                .and_then(|a| a.streams.iter().find(|st| st.id == "control"))
                .map(|st| st.stream_id)
                .unwrap_or(0);
            ro::request_send(sid);
        }
        ObsMsg::HistConnect(i) | ObsMsg::HistFill(i) => {
            let cur = ro::snapshot();
            let Some(e) = hist::all().into_iter().nth(i) else { return };
            ro::form_from(&cur.catalog, &e.adapter, &e.cfg);
            // 密钥被抹掉的那条只填表单不发连接——发了必然失败在一条
            // 难懂的鉴权错误上，而真正的原因是密钥没了
            if matches!(m, ObsMsg::HistConnect(_)) && ro::form_ready(&cur.catalog).is_ok() {
                let (id, vals) = ro::form(&cur.catalog);
                ro::request_connect(&id, &vals);
            }
        }
        ObsMsg::HistDelete(i) => {
            if let Some(e) = hist::all().into_iter().nth(i) {
                hist::remove(&e.adapter, &e.cfg);
            }
        }
        ObsMsg::LibNameEdited(t) => ro::set_lib_name(&t),
        ObsMsg::LibParamEdited(k, v) => ro::set_lib_param(&k, &v),
        ObsMsg::LibSave => {
            let name = ro::lib_name();
            if !name.trim().is_empty() {
                let cur = ro::snapshot();
                lib::save(lib::Saved {
                    name: name.trim().to_string(),
                    // 记下这条是给谁写的。**不是用来禁止跨 Adapter 用**——
                    // 同一段 JSON 在两个交易所的 WS 上都能发
                    adapter: cur.session.as_ref().map(|s| s.adapter.clone()).unwrap_or_default(),
                    body: ro::send_body(),
                    // 存的是**默认值**：下次载入出来还是这一份
                    params: ro::lib_params(),
                });
            }
        }
        ObsMsg::LibLoad(name) => {
            if let Some(e) = lib::get(&name) {
                ro::load_saved(&e);
            }
        }
        ObsMsg::LibDelete(name) => lib::remove(&name),
        ObsMsg::CaptureEdited(t) => ro::set_capture_text(&t),
        ObsMsg::ApplyCapture => ro::request_capture(),
        ObsMsg::TrigEdited(k, v) => ro::set_new_trig(|f| match k {
            "name" => f.name = v,
            "cond" => f.cond = v,
            "pre" => f.pre_roll_ms = v,
            "post" => f.post_roll_ms = v,
            "cool" => f.cooldown_ms = v,
            _ => f.max_per_hour = v,
        }),
        ObsMsg::AddTrigger => {
            let cur = ro::snapshot();
            let existing = cur.session.as_ref().map(|s| s.triggers.as_slice()).unwrap_or(&[]);
            // 加失败（空条件、重名）时**什么都不发**——发一份坏的规则表
            // 会把守护那边已有规则的冷却状态一起清掉
            let _ = ro::request_add_trigger(existing, &ro::new_trig());
        }
        ObsMsg::DelTrigger(i) => {
            let cur = ro::snapshot();
            let Some(s) = cur.session.as_ref() else { return };
            if i >= s.triggers.len() {
                return;
            }
            // 整份重发：守护那边是整表重建
            let rest: Vec<ro::TrigStat> = s
                .triggers
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, t)| t.clone())
                .collect();
            ro::request_triggers(&rest);
        }
        ObsMsg::SaveLast(secs) => {
            // 区间的右端取**环覆盖的终点**而不是「现在」：两者差着一次快照
            // 的时间，用「现在」会让区间右端落在还没进环的位置上
            let cur = ro::snapshot();
            let Some(to) = cur.session.as_ref().and_then(|s| s.ring.coverage_to_ms) else {
                return;
            };
            ro::request_save_window(to - secs as i64 * 1000, to);
        }
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
