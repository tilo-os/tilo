//! Which devices hosting legacy windows are reachable right now.
//!
//! A legacy window runs only on the device that started it; elsewhere its
//! tab says where it runs and, while that device cannot be reached, that it
//! is offline. The layout around it is shared state and changes either way;
//! only using the window needs its host. The network tells this registry
//! which devices are live; the tile reads it when its card is built, and the
//! frame rebuilds the cards when the set changes.

use std::collections::BTreeSet;
use std::sync::RwLock;

static LIVE: RwLock<(BTreeSet<String>, u64)> = RwLock::new((BTreeSet::new(), 0));

/// Record the devices (lowercase hex device keys) that are live now.
/// Returns whether that changed anything.
pub fn set_live(devices: BTreeSet<String>) -> bool {
    let Ok(mut live) = LIVE.write() else {
        return false;
    };
    if live.0 == devices {
        return false;
    }
    live.0 = devices;
    live.1 = live.1.wrapping_add(1);
    true
}

/// Whether the device `host` (hex device key) is live now.
pub fn is_live(host: &str) -> bool {
    LIVE.read()
        .is_ok_and(|live| live.0.contains(&host.to_ascii_lowercase()))
}

/// Moves whenever the set of live devices changes.
pub fn revision() -> u64 {
    LIVE.read().map_or(0, |live| live.1)
}

/// What a hosted window's card says about where it runs: the host's name
/// (a short key when it has none), and whether it is reachable.
pub fn running_on(host: &str, host_label: &str, live: bool) -> String {
    let device = if host_label.trim().is_empty() {
        host.chars().take(8).collect::<String>()
    } else {
        host_label.to_string()
    };
    crate::shell::i18n::t_args(
        if live {
            "hosted-window-running-on"
        } else {
            "hosted-window-running-on-offline"
        },
        &[("device", device.into())],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hosted_window_says_when_its_host_is_offline() {
        assert_eq!(
            running_on("ab12cd34ef", "Laptop", true),
            "Running on Laptop"
        );
        assert_eq!(
            running_on("ab12cd34ef", "Laptop", false),
            "Running on Laptop — offline"
        );
        assert_eq!(
            running_on("ab12cd34ef", " ", false),
            "Running on ab12cd34 — offline"
        );
    }
}
