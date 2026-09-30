//! Owner-local association of a shared profile id with a `NetworkManager` UUID.
//!
//! This file deliberately contains neither raw configuration nor credentials.
//! It is an atomic, owner-only cache of the `NetworkManager` UUID *and local
//! display facts* (endpoint, address, DNS, routes and public key) needed to
//! reconnect and render a locally provisioned profile after restart. It is
//! never copied into Automerge, action envelopes, resource search or logs. A
//! missing/corrupt cache is recoverable: the UI truthfully asks for exact
//! profile provisioning again.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use crate::model::{ConnId, LocalProfileDetails};

/// `$XDG_STATE_HOME/tilo/vpn-local-profiles.json`, else under
/// `$HOME/.local/state`; an empty or relative `XDG_STATE_HOME` is ignored.
fn path() -> Option<PathBuf> {
    Some(
        tilo_paths::xdg::state_home()?
            .join("tilo")
            .join("vpn-local-profiles.json"),
    )
}

pub fn load() -> HashMap<ConnId, LocalProfileDetails> {
    let Some(path) = path() else {
        return HashMap::new();
    };
    let Ok(bytes) = fs::read(path) else {
        return HashMap::new();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

/// Atomically replace the owner-local cache. The caller keeps the existing
/// in-memory map when this fails and surfaces an honest recoverable error.
///
/// # Errors
///
/// Returns an error when no state directory is available or an owner-only
/// directory, temporary file, atomic rename, or sync operation fails.
pub fn save<S: std::hash::BuildHasher>(
    entries: &HashMap<ConnId, LocalProfileDetails, S>,
) -> Result<(), String> {
    let Some(path) = path() else {
        return Err(
            "no local state directory is available; local VPN association cannot be saved".into(),
        );
    };
    let bytes = serde_json::to_vec(entries).map_err(|error| error.to_string())?;
    tilo_files::native::atomic::write_private(&path, &bytes).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_env<T>(state_home: Option<&str>, home: &str, check: impl FnOnce() -> T) -> T {
        let mut env = tilo_test_env::ScopedEnv::new();
        match state_home {
            Some(value) => env.set("XDG_STATE_HOME", value),
            None => env.remove("XDG_STATE_HOME"),
        };
        env.set("HOME", home);
        check()
    }

    #[test]
    #[serial_test::serial(vpn_env)]
    fn state_path_prefers_xdg_state_home_then_home_fallback() {
        // Case `state_path_prefers_xdg_state_home`.
        {
            let path = with_env(Some("/state"), "/home/quinten", path).unwrap();
            assert_eq!(path, PathBuf::from("/state/tilo/vpn-local-profiles.json"));
        }
        // Case `state_path_uses_freedesktop_home_fallback`.
        {
            let expected = PathBuf::from("/home/quinten/.local/state/tilo/vpn-local-profiles.json");
            for state_home in [None, Some(""), Some("relative/state")] {
                let path = with_env(state_home, "/home/quinten", path).unwrap();
                assert_eq!(path, expected, "XDG_STATE_HOME={state_home:?}");
            }
        }
    }
}
