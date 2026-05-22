//! `OutputId` allocation + DRM-output bookkeeping.
//!
//! The compositor `Backend` trait uses `OutputId(u32)` to identify a
//! display. The DRM backend has to mint these from connector handles
//! deterministically (so that re-enumerating after a VT switch keeps
//! the same output IDs) and clean them up when connectors disappear.
//!
//! This module holds the pure-data registry. It does not touch DRM;
//! callers feed it `(card_dev_t, connector_handle)` pairs and it
//! hands out stable IDs.

use std::collections::BTreeMap;

use crate::compositor::backend::{Output, OutputId};

use super::devices::ModeInfo;

/// Stable identity of a DRM output: which `cardN` device and which
/// connector handle on that device.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct DrmOutputKey {
    pub device: u64,
    pub connector: u32,
}

/// Plain-data snapshot of one connector after smithay/drm enumeration.
/// Lives here (not in `runtime`) so unit tests can construct fixtures
/// without a real DRM device. The shape mirrors what
/// `super::runtime::enumerate_connectors` produces; that function is
/// the only place that builds a `ConnectorInfo` from a live device.
///
/// `modes` is the connector's mode list (any order); the registry's
/// `populate` walks it via [`super::devices::preferred_mode`] to pick
/// one. `device_id` is `dev_t` of the `cardN` node; `connector_id` is
/// the DRM connector handle's raw u32.
#[derive(Clone, Debug)]
pub struct ConnectorInfo {
    /// `dev_t` of the `cardN` node, so two connectors on the same GPU
    /// stay grouped and two GPUs don't collide on connector ids.
    pub device_id: u64,
    /// DRM connector handle (raw u32). Stable across VT switches as
    /// long as the underlying connector slot stays the same.
    pub connector_id: u32,
    /// True when the connector reports DRM `State::Connected` — only
    /// these become public outputs. Disconnected connectors stay in
    /// the registry plumbing for future hot-plug events but don't
    /// appear in the `outputs()` list.
    pub connected: bool,
    /// All modes the connector advertises. The registry calls
    /// `preferred_mode` to pick one; empty lists are tolerated (the
    /// connector is skipped).
    pub modes: Vec<ModeInfo>,
}

/// Allocates `OutputId`s for `(device, connector)` pairs and remembers
/// previously-issued IDs across re-enumerations.
#[derive(Debug, Default)]
pub struct OutputRegistry {
    map: BTreeMap<DrmOutputKey, OutputId>,
    next_id: u32,
}

impl OutputRegistry {
    pub fn new() -> Self {
        Self {
            map: BTreeMap::new(),
            next_id: 0,
        }
    }

    /// Get (or mint) the `OutputId` for the given DRM output.
    pub fn intern(&mut self, key: DrmOutputKey) -> OutputId {
        if let Some(id) = self.map.get(&key) {
            return *id;
        }
        let id = OutputId(self.next_id);
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("OutputId counter exhausted (more than 4 billion outputs)");
        self.map.insert(key, id);
        id
    }

    /// Number of outputs registered so far. Used by tests + status logs.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Iterate over all `(key, id)` pairs. Used by the runtime to walk
    /// the registry after enumeration.
    pub fn iter(&self) -> impl Iterator<Item = (&DrmOutputKey, &OutputId)> {
        self.map.iter()
    }

    /// Walk a freshly-enumerated connector list and emit one [`Output`]
    /// per **connected** connector that advertises at least one mode.
    /// Disconnected connectors are skipped silently; connected
    /// connectors with no modes are also skipped (a misbehaving driver
    /// shouldn't crash the backend) but logged via `tracing::warn!`.
    ///
    /// Side-effect: interns each emitted output into the registry so
    /// the returned `OutputId`s are stable across re-enumeration.
    ///
    /// Returns `(OutputId, Output)` pairs in the order their connectors
    /// appeared in `connectors` — deterministic for tests.
    pub fn populate(&mut self, connectors: &[ConnectorInfo]) -> Vec<(OutputId, Output)> {
        use super::devices::preferred_mode;
        let mut out = Vec::with_capacity(connectors.len());
        for ci in connectors {
            if !ci.connected {
                continue;
            }
            let Some(mode) = preferred_mode(&ci.modes) else {
                tracing::warn!(
                    device_id = ci.device_id,
                    connector_id = ci.connector_id,
                    "DRM output skipped: connected connector advertises no modes"
                );
                continue;
            };
            let id = self.intern(DrmOutputKey {
                device: ci.device_id,
                connector: ci.connector_id,
            });
            let output = Output {
                id,
                size: (mode.width as u32, mode.height as u32),
                // Scale defaults to 1.0; per-output scale is a Phase 6+
                // follow-up (HiDPI policy lives at the compositor layer,
                // not the backend).
                scale: 1.0,
            };
            out.push((id, output));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(device: u64, connector: u32) -> DrmOutputKey {
        DrmOutputKey { device, connector }
    }

    fn mode_1080p() -> ModeInfo {
        ModeInfo {
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
            preferred: true,
        }
    }

    fn mode_4k() -> ModeInfo {
        ModeInfo {
            width: 3840,
            height: 2160,
            refresh_mhz: 60_000,
            preferred: false,
        }
    }

    #[test]
    fn new_registry_is_empty() {
        let r = OutputRegistry::new();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
    }

    #[test]
    fn intern_mints_ids_starting_at_zero() {
        let mut r = OutputRegistry::new();
        assert_eq!(r.intern(key(1, 10)), OutputId(0));
        assert_eq!(r.intern(key(1, 11)), OutputId(1));
        assert_eq!(r.intern(key(2, 10)), OutputId(2));
    }

    #[test]
    fn intern_is_stable_across_calls() {
        let mut r = OutputRegistry::new();
        let first = r.intern(key(1, 42));
        let second = r.intern(key(1, 42));
        assert_eq!(first, second);
        // No new id minted.
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn intern_distinct_devices_get_distinct_ids() {
        let mut r = OutputRegistry::new();
        let a = r.intern(key(1, 10));
        let b = r.intern(key(2, 10));
        assert_ne!(a, b);
    }

    #[test]
    fn iter_yields_all_entries() {
        let mut r = OutputRegistry::new();
        r.intern(key(1, 10));
        r.intern(key(1, 11));
        let collected: Vec<_> = r.iter().collect();
        assert_eq!(collected.len(), 2);
    }

    #[test]
    fn registry_populates_from_connector_list() {
        let mut r = OutputRegistry::new();
        let connectors = vec![
            ConnectorInfo {
                device_id: 1,
                connector_id: 10,
                connected: true,
                modes: vec![mode_1080p()],
            },
            ConnectorInfo {
                device_id: 1,
                connector_id: 11,
                connected: true,
                modes: vec![mode_4k()],
            },
        ];
        let outputs = r.populate(&connectors);
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0].1.size, (1920, 1080));
        assert_eq!(outputs[1].1.size, (3840, 2160));
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn populate_skips_disconnected_connectors() {
        let mut r = OutputRegistry::new();
        let connectors = vec![
            ConnectorInfo {
                device_id: 1,
                connector_id: 10,
                connected: false,
                modes: vec![mode_1080p()],
            },
            ConnectorInfo {
                device_id: 1,
                connector_id: 11,
                connected: true,
                modes: vec![mode_4k()],
            },
        ];
        let outputs = r.populate(&connectors);
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].1.size, (3840, 2160));
        // Disconnected connector should not be interned either.
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn populate_skips_connected_connector_with_no_modes() {
        let mut r = OutputRegistry::new();
        let connectors = vec![ConnectorInfo {
            device_id: 1,
            connector_id: 10,
            connected: true,
            modes: vec![],
        }];
        let outputs = r.populate(&connectors);
        assert!(outputs.is_empty());
        assert!(r.is_empty());
    }

    #[test]
    fn populate_returns_stable_ids_across_calls() {
        // Same connector list twice → same output ids → same Output
        // entries. Verifies the `intern` integration.
        let mut r = OutputRegistry::new();
        let connectors = vec![ConnectorInfo {
            device_id: 7,
            connector_id: 99,
            connected: true,
            modes: vec![mode_1080p()],
        }];
        let first = r.populate(&connectors);
        let second = r.populate(&connectors);
        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert_eq!(first[0].0, second[0].0, "OutputId must be stable across re-enumeration");
        assert_eq!(r.len(), 1, "no extra entries on re-enumeration");
    }
}
