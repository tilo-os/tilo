//! What the host knows about its network connection.

/// Whether the active connection is metered (cellular or capped), as
/// `NetworkManager` reports it. `None` when that is unknown: `NetworkManager` is
/// missing or fails, no device reports a known state, or the platform has no
/// such probe.
///
/// This runs `nmcli` with a short deadline; call it off the UI thread or
/// cache the answer.
#[must_use]
pub fn connection_metered() -> Option<bool> {
    #[cfg(target_os = "linux")]
    {
        // NM's metered enum: `yes`/`yes (guessed)` ⇒ metered; `no`/`no (guessed)`
        // ⇒ not metered; `unknown` ⇒ we can't tell.
        let out = bounded_command::run(
            "nmcli",
            &["-t", "-f", "GENERAL.METERED", "device", "show"],
            None,
            256 * 1024,
            std::time::Duration::from_secs(3),
        )
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
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}
