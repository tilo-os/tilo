//! Connection-type awareness for prefetch: speculative downloads wait unless the
//! connection is stable and unmetered.

/// Whether speculative downloads wait: metered or unknown.
pub fn is_metered() -> bool {
    // `TILO_MUSIC_METERED=1` or `=0` forces the answer, for tests and headless boxes.
    if let Ok(v) = std::env::var("TILO_MUSIC_METERED") {
        return v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes");
    }
    tilo_net::status::current().metered.unwrap_or(true)
}

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
