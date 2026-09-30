//! Connection-type awareness for prefetch decisions.
//!
//! Prefetching lyrics / the next queued song / album metadata is only polite on
//! a **stable, non-metered** connection (home Wi-Fi / ethernet) — never on a
//! metered cellular link where it would burn the user's data. This module
//! answers "is the connection metered?" from [`tilo_net::status`] (`NetworkManager`
//! on Linux, the Network Information API in a page), with an env override for
//! tests + headless boxes and a sensible default (assume unmetered / stable)
//! when the platform cannot tell.

use std::sync::OnceLock;

/// Whether the active connection is metered (cellular / capped). `false` (i.e.
/// "stable, prefetch is fine") is the default when the status is unknown.
pub fn is_metered() -> bool {
    // Consult the platform once, cached for the process lifetime.
    static CACHED: OnceLock<bool> = OnceLock::new();
    // Deterministic override first: tests + operators set `TILO_MUSIC_METERED=1`
    // (metered) or `=0` (stable) to force the decision.
    if let Ok(v) = std::env::var("TILO_MUSIC_METERED") {
        return v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes");
    }
    *CACHED.get_or_init(|| tilo_net::status::current().metered.unwrap_or(false))
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
