//! Connection-type awareness for prefetch decisions.
//!
//! Prefetching lyrics / the next queued song / album metadata is only polite on
//! a **stable, non-metered** connection (home Wi-Fi / ethernet) — never on a
//! metered cellular link where it would burn the user's data. This module
//! reads the latest [`tilo_net::status`] snapshot (the session portal on Linux,
//! the Network Information API in a page), with an env override for tests and
//! headless boxes. Unknown metering postpones speculative prefetch; explicit
//! playback remains available.

/// Whether speculative downloads should be withheld: metered or unknown.
/// Read the latest snapshot so moving onto a tether updates the decision.
pub fn is_metered() -> bool {
    // Deterministic override first: tests + operators set `TILO_MUSIC_METERED=1`
    // (metered) or `=0` (stable) to force the decision.
    if let Ok(v) = std::env::var("TILO_MUSIC_METERED") {
        return v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes");
    }
    tilo_net::status::current().metered.unwrap_or(true)
}

/// Whether prefetch should run — the inverse of [`is_metered`], named for the
/// call site.
pub fn connection_is_stable() -> bool {
    !is_metered()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_status_withholds_speculative_prefetch() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.remove("TILO_MUSIC_METERED");
        env.set(
            "DBUS_SESSION_BUS_ADDRESS",
            "unix:path=/nonexistent/tilo-network-test.sock",
        );
        assert_eq!(tilo_net::status::current().metered, None);
        assert!(is_metered());
        assert!(!connection_is_stable());
    }

    #[test]
    fn env_override_forces_metered_state() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_MUSIC_METERED", "1");
        assert!(is_metered());
        assert!(!connection_is_stable());
        env.set("TILO_MUSIC_METERED", "0");
        assert!(!is_metered());
        assert!(connection_is_stable());
    }
}
