use crate::widget::toast::{Status, Toast};
use rustc_hash::FxHashMap;
use std::time::{Duration, Instant};

/// Base dedupe window for duplicate error toasts.
///
/// When the same error keeps repeating, this window is increased with
/// exponential backoff to reduce long-outage noise.
const ERROR_DEDUPE_BASE_WINDOW_SECS: u64 = 30;

/// Maximum dedupe window for duplicate error toasts.
const ERROR_DEDUPE_MAX_WINDOW_SECS: u64 = 300;

/// Time window to suppress duplicate non-error toasts.
const DEFAULT_DEDUPE_WINDOW_SECS: u64 = 10;

/// How long we keep dedupe entries before dropping them.
const DEDUPE_CACHE_TTL_SECS: u64 = 300;

/// Stores active toasts and hides repeated ones for a short time.
#[derive(Debug, Default)]
pub struct Notifications {
    /// Toasts currently shown in the UI.
    toasts: Vec<Toast>,
    /// 通知中心（docs/35 §6.1）：本次运行里弹过的每一条（时刻, 提示），新的在后，最多 200 条。
    /// 同时最多显示 3 条，挤掉的、自动消失的都还能在这里查到。
    history: std::collections::VecDeque<(String, Toast)>,
    /// Per-toast state used for dedupe windows and retry aggregation.
    dedupe_cache: FxHashMap<Toast, DedupeState>,
}

/// Internal dedupe state per unique toast payload.
#[derive(Debug, Clone, Copy)]
struct DedupeState {
    /// Last time this toast (or its aggregated variant) was shown.
    last_emitted_at: Instant,
    /// Last time this toast key was seen (shown or deduped).
    last_seen_at: Instant,
    /// Number of duplicate events hidden since last emission.
    suppressed_since_emit: u32,
    /// Backoff level for repeated error notifications.
    error_backoff_level: u8,
}

impl Notifications {
    /// Creates an empty notifications store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a toast unless the same one was shown very recently.
    pub fn push(&mut self, toast: Toast) {
        let now = Instant::now();
        self.prune_cache(now);

        let key = toast.clone();

        if let Some(state) = self.dedupe_cache.get_mut(&key) {
            state.last_seen_at = now;
            let window = dedupe_window_for(&key, state.error_backoff_level);

            if now.duration_since(state.last_emitted_at) < window {
                state.suppressed_since_emit = state.suppressed_since_emit.saturating_add(1);
                return;
            }

            let error_toast_retried =
                matches!(key.status(), Status::Danger) && state.suppressed_since_emit > 0;

            let toast_to_show = if error_toast_retried {
                aggregate_toast(&key, state.suppressed_since_emit)
            } else {
                toast
            };

            if error_toast_retried {
                state.error_backoff_level = state.error_backoff_level.saturating_add(1).min(5);
            } else {
                state.error_backoff_level = 0;
            }

            state.last_emitted_at = now;
            state.suppressed_since_emit = 0;

            self.show(toast_to_show);
            return;
        }

        self.dedupe_cache.insert(
            key,
            DedupeState {
                last_emitted_at: now,
                last_seen_at: now,
                suppressed_since_emit: 0,
                error_backoff_level: 0,
            },
        );
        self.show(toast);
    }

    /// 显示一条：记进通知中心；同时最多 3 条，超了挤掉最早的（历史里还在）。
    fn show(&mut self, toast: Toast) {
        self.history.push_back((chrono::Local::now().format("%H:%M:%S").to_string(), toast.clone()));
        while self.history.len() > 200 {
            self.history.pop_front();
        }
        self.toasts.push(toast);
        while self.toasts.len() > 3 {
            self.toasts.remove(0);
        }
    }

    /// 通知中心的历史（新的在后）。
    pub fn history(&self) -> impl Iterator<Item = &(String, Toast)> {
        self.history.iter()
    }

    /// Removes a toast by index if the index exists.
    pub fn remove(&mut self, index: usize) {
        if index < self.toasts.len() {
            self.toasts.remove(index);
        }
    }

    /// Returns all toasts to render.
    pub fn toasts(&self) -> &[Toast] {
        &self.toasts
    }

    /// Drops old dedupe entries so the cache stays small.
    fn prune_cache(&mut self, now: Instant) {
        self.dedupe_cache.retain(|_, state| {
            now.duration_since(state.last_seen_at).as_secs() <= DEDUPE_CACHE_TTL_SECS
        });
    }
}

fn dedupe_window_for(toast: &Toast, error_backoff_level: u8) -> Duration {
    match toast.status() {
        Status::Danger => {
            let factor = 1_u64 << error_backoff_level.min(5);
            let secs = ERROR_DEDUPE_BASE_WINDOW_SECS
                .saturating_mul(factor)
                .min(ERROR_DEDUPE_MAX_WINDOW_SECS);
            Duration::from_secs(secs)
        }
        _ => Duration::from_secs(DEFAULT_DEDUPE_WINDOW_SECS),
    }
}

fn aggregate_toast(base: &Toast, suppressed_count: u32) -> Toast {
    let retries = if suppressed_count == 1 {
        "1 retry".to_string()
    } else {
        format!("{suppressed_count} retries")
    };

    Toast::custom(
        base.title().to_string(),
        format!("{} (still failing, {retries})", base.body()),
        base.status(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 最多显示三条_历史都留着() {
        let mut n = Notifications::new();
        for i in 0..5 {
            n.push(Toast::info(format!("第 {i} 条")));
        }
        assert_eq!(n.toasts().len(), 3, "同时最多 3 条");
        assert_eq!(n.toasts()[0].body(), "第 2 条", "挤掉的是最早的");
        assert_eq!(n.history().count(), 5, "挤掉的在通知中心里还查得到");
    }
}
