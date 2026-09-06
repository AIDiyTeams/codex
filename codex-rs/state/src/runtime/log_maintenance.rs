//! Periodic maintenance for the dedicated Codex log database.
//!
//! The retention policy intentionally lives outside `logs.rs` so an upstream
//! Codex upgrade has one small, conflict-resistant integration point. Operators
//! can override the default with `CODEX_LOG_RETENTION_DAYS` without rebuilding.

use super::StateRuntime;
use std::sync::Arc;
use std::sync::Weak;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::time::MissedTickBehavior;
use tracing::info;
use tracing::warn;

pub(crate) const DEFAULT_RETENTION_DAYS: i64 = 2;
const MIN_RETENTION_DAYS: i64 = 1;
const MAX_RETENTION_DAYS: i64 = 30;
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Resolve the configured retention window, falling back to two days for an
/// absent or invalid value. Keeping this parser pure makes policy changes easy
/// to test without mutating process-global environment state.
pub(crate) fn parse_retention_days(value: Option<&str>) -> i64 {
    value
        .and_then(|raw| raw.trim().parse::<i64>().ok())
        .filter(|days| (MIN_RETENTION_DAYS..=MAX_RETENTION_DAYS).contains(days))
        .unwrap_or(DEFAULT_RETENTION_DAYS)
}

pub(crate) fn configured_retention_days() -> i64 {
    parse_retention_days(std::env::var("CODEX_LOG_RETENTION_DAYS").ok().as_deref())
}

/// Start one weakly-held maintenance loop for a StateRuntime.
///
/// The task does not keep the runtime alive: dropping the last runtime causes
/// the weak upgrade to fail on the next tick. `close()` also flips `stop` so a
/// deliberately closed runtime does not run another maintenance pass.
pub(crate) fn spawn(runtime: &Arc<StateRuntime>, stop: Arc<AtomicBool>) {
    let weak_runtime: Weak<StateRuntime> = Arc::downgrade(runtime);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(MAINTENANCE_INTERVAL);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        // `interval` ticks immediately; consume that tick so startup
        // maintenance remains the only immediate pass.
        interval.tick().await;
        loop {
            interval.tick().await;
            if stop.load(Ordering::Acquire) {
                return;
            }
            let Some(runtime) = weak_runtime.upgrade() else {
                return;
            };
            let retention_days = configured_retention_days();
            match runtime.run_logs_maintenance(retention_days).await {
                Ok(deleted_rows) => info!(
                    retention_days,
                    deleted_rows, "periodic Codex log maintenance completed"
                ),
                Err(error) => warn!(%error, "periodic Codex log maintenance failed"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::DEFAULT_RETENTION_DAYS;
    use super::parse_retention_days;

    #[test]
    fn invalid_values_fall_back_to_two_days() {
        assert_eq!(parse_retention_days(None), DEFAULT_RETENTION_DAYS);
        assert_eq!(parse_retention_days(Some("")), DEFAULT_RETENTION_DAYS);
        assert_eq!(parse_retention_days(Some("0")), DEFAULT_RETENTION_DAYS);
        assert_eq!(parse_retention_days(Some("31")), DEFAULT_RETENTION_DAYS);
        assert_eq!(
            parse_retention_days(Some("not-a-number")),
            DEFAULT_RETENTION_DAYS
        );
    }

    #[test]
    fn valid_values_are_bounded_and_trimmed() {
        assert_eq!(parse_retention_days(Some(" 1 ")), 1);
        assert_eq!(parse_retention_days(Some("2")), 2);
        assert_eq!(parse_retention_days(Some("30")), 30);
    }
}
