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

fn state_path(
    state_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    let root = state_home
        .map(PathBuf::from)
        .or_else(|| home.map(|home| PathBuf::from(home).join(".local/state")))?;
    Some(root.join("tilo").join("vpn-local-profiles.json"))
}

fn path() -> Option<PathBuf> {
    state_path(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"))
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
    atomic_file::write_private(&path, &bytes).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_path_prefers_xdg_state_home() {
        let path = state_path(Some("/state".into()), Some("/home/user".into())).unwrap();
        assert_eq!(path, PathBuf::from("/state/tilo/vpn-local-profiles.json"));
    }

    #[test]
    fn state_path_uses_freedesktop_home_fallback() {
        let path = state_path(None, Some("/home/user".into())).unwrap();
        assert_eq!(
            path,
            PathBuf::from("/home/user/.local/state/tilo/vpn-local-profiles.json")
        );
    }
}
