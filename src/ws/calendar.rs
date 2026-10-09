//! 金融日历面板的**副作用**（docs/43 K2）：选日期 / 翻月 / 筛选只改面板内存；
//! 刷新是起一次 `ws-news --once calendar`；打开原文走新闻面板同一个安全检查。
//!
//! 与新闻面板同一模式：`_view` 只产生消息，副作用集中在这里。

use super::calendar_alerts as al;
use super::calendar_readout as ro;
use super::calendar_view::CalMsg;

pub fn handle(m: CalMsg) {
    match m {
        CalMsg::SetView(v) => ro::set_view(v),
        CalMsg::Select(d) => ro::select(d),
        CalMsg::PrevMonth => ro::shift_month(&ro::snapshot(), -1),
        CalMsg::NextMonth => ro::shift_month(&ro::snapshot(), 1),
        CalMsg::Today => ro::go_today(&ro::snapshot()),
        CalMsg::PickEvent(id) => ro::select_event(&id),
        CalMsg::Open(url) => super::news::open_in_browser(&url),
        CalMsg::Refresh => {
            // 守护常驻时 OnceJob 会拒绝并说明原因（数据本来就在持续更新）
            let active = super::svcctl::query(super::news_readout::SERVICE).active;
            let _ = super::once::CALENDAR.start(active);
        }
        CalMsg::MaxImportance(i) => ro::update_filter(|f| f.max_importance = i.min(3)),
        CalMsg::ToggleKind(k) => ro::update_filter(|f| toggle(&mut f.hidden_kinds, k)),
        CalMsg::ToggleCountry(c) => ro::update_filter(|f| toggle(&mut f.hidden_countries, c)),
        CalMsg::ShowUnlisted(on) => ro::update_filter(|f| f.show_unlisted = on),
        CalMsg::Query(q) => ro::update_filter(|f| f.query = q),
        CalMsg::Tz(t) => ro::update_filter(|f| f.tz = t),
        CalMsg::StreamDays(n) => ro::update_filter(|f| f.stream_days = n),
        CalMsg::YearMode(m) => ro::set_year_mode(m),
        CalMsg::PrevYear => ro::shift_year(&ro::snapshot(), -1),
        CalMsg::NextYear => ro::shift_year(&ro::snapshot(), 1),
        // 事件流表：操作格转成「详情」，其余交给网格
        CalMsg::Table(crate::ui::grid::GridMsg::Action(r, c)) => {
            if let Some(m) = super::calendar_view::stream_action(r, c) {
                handle(m);
            }
        }
        CalMsg::Table(g) => crate::ui::grid::named_update("calendar.stream", g),
        // ── 提醒（docs/43 K4）──
        CalMsg::AlertPreset(i) => {
            let Some((name, rules)) = al::presets().into_iter().nth(i) else { return };
            let existing = al::snapshot().rules.clone();
            let mut added = 0;
            let r = al::write(|c| {
                for r in &rules {
                    // 已经有一模一样的（范围、目标、提前量）就不重复加
                    if existing.iter().any(|x| x.scope == r.scope && x.target == r.target && x.offsets == r.offsets && x.max_importance == r.max_importance) {
                        continue;
                    }
                    al::add_rule(c, r)?;
                    added += 1;
                }
                Ok(())
            });
            ro::set_alert_note(match r {
                Ok(()) if added == 0 => format!("「{name}」已经启用过了"),
                Ok(()) => format!("✔ 已启用「{name}」（{added} 条规则）"),
                Err(e) => format!("✗ {e}"),
            });
        }
        CalMsg::AlertToggle(id, on) => {
            if let Err(e) = al::write(|c| al::set_rule_enabled(c, id, on)) {
                ro::set_alert_note(format!("✗ {e}"));
            }
        }
        CalMsg::AlertDelete(id) => {
            if let Err(e) = al::write(|c| al::delete_rule(c, id)) {
                ro::set_alert_note(format!("✗ {e}"));
            }
        }
        CalMsg::AlertForEvent(id) => {
            let snap = ro::snapshot();
            let Some(e) = snap.events.iter().find(|e| e.id == id) else { return };
            // 有时刻的提前 30 分钟；没有时刻的（仅日期 / 预计）只能提前一天
            let timed = e.scheduled_at.is_some() && !matches!(e.precision.as_str(), "date_only" | "estimated");
            let rule = al::Rule {
                id: 0,
                name: ro::display_title(e),
                scope: al::Scope::Event,
                target: e.id.clone(),
                max_importance: 3,
                offsets: if timed { vec![30] } else { vec![1440] },
                channels: vec![al::Channel::InApp, al::Channel::Desktop],
                on_change: true,
                on_release: !e.values.is_empty(),
                enabled: true,
            };
            ro::set_alert_note(match al::write(|c| al::add_rule(c, &rule).map(|_| ())) {
                Ok(()) => format!("✔ 已为「{}」设提醒（{}）——在「提醒」页可改", rule.name, al::offset_label(rule.offsets[0])),
                Err(e) => format!("✗ {e}"),
            });
        }
        CalMsg::DraftScope(sc) => ro::update_draft(|d| {
            d.scope = sc;
            d.target.clear();
        }),
        CalMsg::DraftTarget(t) => ro::update_draft(|d| d.target = t),
        CalMsg::DraftImportance(i) => ro::update_draft(|d| d.max_importance = i.min(3)),
        CalMsg::DraftOffset(m) => ro::update_draft(|d| {
            if let Some(i) = d.offsets.iter().position(|x| *x == m) {
                d.offsets.remove(i);
            } else {
                d.offsets.push(m);
                d.offsets.sort_unstable_by(|a, b| b.cmp(a));
            }
        }),
        CalMsg::DraftChannel(ch) => ro::update_draft(|d| {
            if let Some(i) = d.channels.iter().position(|x| *x == ch) {
                d.channels.remove(i);
            } else {
                d.channels.push(ch);
            }
        }),
        CalMsg::DraftOnChange(on) => ro::update_draft(|d| d.on_change = on),
        CalMsg::DraftOnRelease(on) => ro::update_draft(|d| d.on_release = on),
        CalMsg::DraftAdd => {
            let mut d = ro::draft();
            let needs_target = matches!(d.scope, al::Scope::Series | al::Scope::Kind | al::Scope::Country);
            let err = if needs_target && d.target.is_empty() {
                Some("先选目标")
            } else if d.offsets.is_empty() {
                Some("至少选一个提前量")
            } else if d.channels.is_empty() {
                Some("至少选一个渠道")
            } else {
                None
            };
            if let Some(e) = err {
                ro::set_alert_note(format!("✗ {e}"));
                return;
            }
            let snap = ro::snapshot();
            d.name = match d.scope {
                al::Scope::All => format!("全部 ≤ {}", ro::importance_label(d.max_importance)),
                al::Scope::Series => snap
                    .events
                    .iter()
                    .find(|e| e.series.as_deref() == Some(d.target.as_str()))
                    .and_then(|e| e.series_name.clone())
                    .unwrap_or_else(|| d.target.clone()),
                al::Scope::Kind => ro::kind_label(&d.target).to_string(),
                al::Scope::Country => ro::country_label(&d.target).to_string(),
                al::Scope::Event => d.target.clone(),
            };
            match al::write(|c| al::add_rule(c, &d).map(|_| ())) {
                Ok(()) => {
                    ro::set_alert_note(format!("✔ 已添加「{}」", d.name));
                    ro::reset_draft();
                }
                Err(e) => ro::set_alert_note(format!("✗ {e}")),
            }
        }
        CalMsg::AckMissed => {
            let _ = al::write(|c| al::ack_missed(c).map(|_| ()));
        }
        CalMsg::QuietFrom(t) => {
            let (_, b, p) = ro::quiet_edit(&al::snapshot().quiet);
            ro::set_quiet_edit((t, b, p));
        }
        CalMsg::QuietTo(t) => {
            let (a, _, p) = ro::quiet_edit(&al::snapshot().quiet);
            ro::set_quiet_edit((a, t, p));
        }
        CalMsg::QuietP0(on) => {
            let (a, b, _) = ro::quiet_edit(&al::snapshot().quiet);
            ro::set_quiet_edit((a, b, on));
        }
        CalMsg::QuietSave => {
            let (a, b, p) = ro::quiet_edit(&al::snapshot().quiet);
            let ok = |x: &str| chrono::NaiveTime::parse_from_str(x.trim(), "%H:%M").is_ok();
            if !ok(&a) || !ok(&b) {
                ro::set_alert_note("✗ 免打扰时间写成 HH:MM，如 23:00");
                return;
            }
            let q = al::Quiet { from: a.trim().to_string(), to: b.trim().to_string(), p0_exempt: p };
            match al::write(|c| al::set_quiet(c, &q)) {
                Ok(()) => {
                    ro::clear_quiet_edit();
                    ro::set_alert_note("✔ 免打扰已保存");
                }
                Err(e) => ro::set_alert_note(format!("✗ {e}")),
            }
        }
        CalMsg::TgToken(t) => ro::set_tg_token(t),
        CalMsg::TgChat(t) => ro::set_tg_chat(t),
        CalMsg::TgImp(i) => ro::set_tg_imp(i),
        CalMsg::TgSave | CalMsg::TgSaveProbe => {
            let probe = matches!(m, CalMsg::TgSaveProbe);
            let saved = al::telegram();
            let (token, chat, imp) = ro::tg_form(&saved);
            match al::save_telegram(Some(&token), &chat, imp) {
                Ok(note) => {
                    ro::clear_tg_form();
                    if probe {
                        match al::request_telegram("probe") {
                            Ok(n) => {
                                ro::set_tg_wait(n);
                                ro::set_alert_note(note);
                            }
                            Err(e) => ro::set_alert_note(format!("✗ 已保存，但发不出测试请求：{e}")),
                        }
                    } else {
                        ro::set_alert_note(note);
                    }
                }
                Err(e) => ro::set_alert_note(format!("✗ {e}")),
            }
        }
        CalMsg::TgDetect => {
            // 先把刚贴的令牌存上：读会话用的是已保存的令牌
            let saved = al::telegram();
            let (token, chat, imp) = ro::tg_form(&saved);
            if !token.trim().is_empty() {
                if let Err(e) = al::save_telegram(Some(&token), &chat, imp) {
                    ro::set_alert_note(format!("✗ {e}"));
                    return;
                }
                ro::set_tg_token(String::new());
            }
            if al::telegram().bot_token.trim().is_empty() {
                ro::set_alert_note("✗ 先填机器人令牌");
                return;
            }
            match al::request_telegram("detect") {
                Ok(n) => ro::set_tg_wait(n),
                Err(e) => ro::set_alert_note(format!("✗ {e}")),
            }
        }
        CalMsg::TgEnable(on) => {
            // 与出口总闸「Telegram 提醒」那一行同一个开关
            ro::set_alert_note(super::egress::action("telegram", if on { "start" } else { "stop" }));
        }
        CalMsg::TelegramTest => {
            let mut note = String::new();
            let r = al::write(|c| {
                note = match al::queue_test_message(c) {
                    Ok(m) => m,
                    Err(e) => format!("✗ {e}"),
                };
                Ok(())
            });
            ro::set_alert_note(match r {
                Ok(()) => note,
                Err(e) => format!("✗ {e}"),
            });
        }
        CalMsg::RulesTable(crate::ui::grid::GridMsg::Action(r, c)) => {
            if let Some(m) = super::calendar_view::rule_action(r, c) {
                handle(m);
            }
        }
        CalMsg::RulesTable(g) => crate::ui::grid::named_update("calendar.rules", g),
        CalMsg::NoticesTable(g) => crate::ui::grid::named_update("calendar.notices", g),
    }
}

/// 在「隐藏」列表里加 / 去一项。
fn toggle(v: &mut Vec<String>, x: String) {
    if let Some(i) = v.iter().position(|y| *y == x) {
        v.remove(i);
    } else {
        v.push(x);
    }
}
