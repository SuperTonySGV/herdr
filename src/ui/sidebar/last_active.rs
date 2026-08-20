//! Relative "time since the agent last did anything" shown by the sidebar's
//! `last_active` token.
//!
//! The number answers "how long since I touched this session", the colour
//! answers "is this session's prompt cache still warm". Both read from the same
//! elapsed duration: an agent's prompt cache is refreshed by every request it
//! makes, so the time since its last state change is also the time since its
//! last request, to within a turn.

use std::time::Duration;

use crate::config::LastActiveConfig;

const MINUTE: u64 = 60;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;

/// How warm the pane's prompt cache is, by elapsed time against the configured
/// thresholds. Purely advisory: the cache TTL is the API's to decide, and a
/// working agent is refreshing it continuously whatever the clock says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CacheWarmth {
    /// The agent is working, so it is keeping its own cache warm.
    Live,
    Warm,
    Expiring,
    Cold,
}

/// Renders elapsed time the way the mobile app does: `now`, `27m`, `17h`, `3d`.
pub(crate) fn label(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    match secs {
        s if s < MINUTE => "now".to_string(),
        s if s < HOUR => format!("{}m", s / MINUTE),
        s if s < DAY => format!("{}h", s / HOUR),
        s => format!("{}d", s / DAY),
    }
}

pub(crate) fn warmth(elapsed: Duration, working: bool, config: &LastActiveConfig) -> CacheWarmth {
    if working {
        return CacheWarmth::Live;
    }
    let secs = elapsed.as_secs();
    if secs >= config.cold_after_seconds {
        CacheWarmth::Cold
    } else if secs >= config.warn_after_seconds {
        CacheWarmth::Expiring
    } else {
        CacheWarmth::Warm
    }
}

/// How long until this entry's rendering would change — whichever comes first
/// of the next label boundary and the next colour threshold.
///
/// The panel repaints on this deadline rather than on a fixed interval, so
/// `59m` flips to `1h` when it actually should, and an idle Herdr stays idle.
pub(crate) fn next_change(elapsed: Duration, working: bool, config: &LastActiveConfig) -> Duration {
    let secs = elapsed.as_secs();
    let step = match secs {
        s if s < MINUTE => MINUTE - s,
        s if s < HOUR => MINUTE - (s % MINUTE),
        s if s < DAY => HOUR - (s % HOUR),
        s => DAY - (s % DAY),
    };
    // A working agent renders as `now` regardless of elapsed time, so only the
    // thresholds it will cross once it stops matter — and those are re-armed
    // when it stops. Until then the label is static.
    let thresholds = if working {
        Vec::new()
    } else {
        [config.warn_after_seconds, config.cold_after_seconds]
            .into_iter()
            .filter(|threshold| *threshold > secs)
            .map(|threshold| threshold - secs)
            .collect::<Vec<_>>()
    };
    let step = thresholds.into_iter().fold(step, u64::min);
    Duration::from_secs(step.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> LastActiveConfig {
        LastActiveConfig::default()
    }

    #[test]
    fn labels_match_the_mobile_app_granularity() {
        assert_eq!(label(Duration::from_secs(0)), "now");
        assert_eq!(label(Duration::from_secs(59)), "now");
        assert_eq!(label(Duration::from_secs(60)), "1m");
        assert_eq!(label(Duration::from_secs(27 * 60 + 30)), "27m");
        assert_eq!(label(Duration::from_secs(59 * 60 + 59)), "59m");
        assert_eq!(label(Duration::from_secs(HOUR)), "1h");
        assert_eq!(label(Duration::from_secs(17 * HOUR)), "17h");
        assert_eq!(label(Duration::from_secs(DAY)), "1d");
        assert_eq!(label(Duration::from_secs(3 * DAY + 5 * HOUR)), "3d");
    }

    #[test]
    fn a_working_agent_is_live_however_long_it_has_been_working() {
        // The case that makes "time since last state change" the wrong reading
        // on its own: two hours of work is the warmest cache on the machine.
        assert_eq!(
            warmth(Duration::from_secs(2 * HOUR), true, &config()),
            CacheWarmth::Live
        );
    }

    #[test]
    fn idle_warmth_follows_the_configured_thresholds() {
        let config = config();
        assert_eq!(
            warmth(Duration::from_secs(0), false, &config),
            CacheWarmth::Warm
        );
        assert_eq!(
            warmth(
                Duration::from_secs(config.warn_after_seconds - 1),
                false,
                &config
            ),
            CacheWarmth::Warm
        );
        assert_eq!(
            warmth(
                Duration::from_secs(config.warn_after_seconds),
                false,
                &config
            ),
            CacheWarmth::Expiring
        );
        assert_eq!(
            warmth(
                Duration::from_secs(config.cold_after_seconds - 1),
                false,
                &config
            ),
            CacheWarmth::Expiring
        );
        assert_eq!(
            warmth(
                Duration::from_secs(config.cold_after_seconds),
                false,
                &config
            ),
            CacheWarmth::Cold
        );
    }

    #[test]
    fn next_change_lands_on_the_label_boundary() {
        let config = config();
        // "now" holds until the first minute.
        assert_eq!(
            next_change(Duration::from_secs(0), false, &config),
            Duration::from_secs(60)
        );
        // Mid-minute: only the remainder is left.
        assert_eq!(
            next_change(Duration::from_secs(90), false, &config),
            Duration::from_secs(30)
        );
        // The flip this feature exists for: 59m -> 1h, to the second.
        assert_eq!(
            next_change(Duration::from_secs(59 * 60 + 59), false, &config),
            Duration::from_secs(1)
        );
        // Past an hour the label only moves hourly.
        assert_eq!(
            next_change(Duration::from_secs(HOUR + 10), false, &config),
            Duration::from_secs(HOUR - 10)
        );
    }

    #[test]
    fn next_change_also_wakes_for_a_colour_threshold() {
        // Deliberately off a minute boundary, so the threshold and the label
        // disagree about when the next repaint is due.
        let config = LastActiveConfig {
            warn_after_seconds: 44 * 60 + 40,
            cold_after_seconds: 3600,
        };
        // 44m10s: the next minute boundary is 50s out, amber only 30s.
        assert_eq!(
            next_change(Duration::from_secs(44 * 60 + 10), false, &config),
            Duration::from_secs(30)
        );
        // 44m50s: amber is already behind us, so the label boundary wins again.
        assert_eq!(
            next_change(Duration::from_secs(44 * 60 + 50), false, &config),
            Duration::from_secs(10)
        );
    }

    #[test]
    fn a_working_entry_only_waits_for_the_label() {
        let config = config();
        // No threshold wake-ups while working: the label reads `now` either way.
        assert_eq!(
            next_change(Duration::from_secs(44 * 60 + 10), true, &config),
            Duration::from_secs(50)
        );
    }

    #[test]
    fn next_change_is_never_zero() {
        let config = config();
        assert!(next_change(Duration::from_secs(HOUR), false, &config) >= Duration::from_secs(1));
        assert!(next_change(Duration::from_secs(0), true, &config) >= Duration::from_secs(1));
    }
}
