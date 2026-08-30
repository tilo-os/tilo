//! Connection-type awareness for prefetch decisions.
//!
//! Prefetching lyrics / the next queued song / album metadata is only polite on
//! a **stable, non-metered** connection (home Wi-Fi / ethernet) — never on a
//! metered cellular link where it would burn the user's data. This module
//! answers "is the connection metered?" from `NetworkManager` when it's available
//! (the qOS default), with an env override for tests + headless boxes and a
//! sensible default (assume unmetered / stable) when NM can't be reached.

use std::sync::OnceLock;

/// Whether the active connection is metered (cellular / capped). `false` (i.e.
/// "stable, prefetch is fine") is the default when the status is unknown.
pub fn is_metered() -> bool {
    // Deterministic override first: tests + operators set `QOS_MUSIC_METERED=1`
    // (metered) or `=0` (stable) to force the decision.
    if let Ok(v) = std::env::var("QOS_MUSIC_METERED") {
        return v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes");
    }
    // Otherwise consult NetworkManager once, cached for the process lifetime.
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| nm_metered().unwrap_or(false))
}

/// Whether prefetch should run — the inverse of [`is_metered`], named for the
/// call site.
pub fn connection_is_stable() -> bool {
    !is_metered()
}

/// Best-effort `NetworkManager` metered probe via `nmcli`. Returns `None` when
/// `nmcli` is missing / errors (so the caller falls back to the default).
///
/// NM's metered enum: `yes`/`yes (guessed)` ⇒ metered; `no`/`no (guessed)` ⇒
/// not metered; `unknown` ⇒ we can't tell.
fn nm_metered() -> Option<bool> {
    let out = std::process::Command::new("nmcli")
        .args(["-t", "-f", "GENERAL.METERED", "device", "show"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut saw_known = false;
    for line in text.lines() {
        let val = line
            .split_once(':')
            .map_or("", |(_, v)| v)
            .trim()
            .to_ascii_lowercase();
        if val.starts_with("yes") {
            return Some(true);
        }
        if val.starts_with("no") {
            saw_known = true;
        }
    }
    // Only assert "not metered" if at least one device reported a known state.
    saw_known.then_some(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_override_forces_metered_state() {
        // SAFETY: single-threaded test mutating a process env var it owns.
        unsafe { std::env::set_var("QOS_MUSIC_METERED", "1") };
        assert!(is_metered());
        assert!(!connection_is_stable());
        unsafe { std::env::set_var("QOS_MUSIC_METERED", "0") };
        assert!(!is_metered());
        assert!(connection_is_stable());
        unsafe { std::env::remove_var("QOS_MUSIC_METERED") };
    }
}
