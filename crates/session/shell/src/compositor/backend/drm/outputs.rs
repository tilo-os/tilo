//! `OutputId` allocation + DRM-output bookkeeping.
//!
//! `OutputId(u32)` must be minted from connector handles deterministically,
//! so re-enumerating after a VT switch keeps the same ids. Pure data — this
//! never touches DRM; callers feed it `(card_dev_t, connector_handle)` pairs.

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

/// Plain-data snapshot of one connector after enumeration. Lives here, not
/// in `runtime`, so unit tests can build fixtures without a real DRM device;
/// `super::runtime::enumerate_connectors` is the only place that builds one
/// from a live device.
#[derive(Clone, Debug)]
pub struct ConnectorInfo {
    /// `dev_t` of the `cardN` node, so two connectors on the same GPU
    /// stay grouped and two GPUs don't collide on connector ids.
    pub device_id: u64,
    /// DRM connector handle (raw u32). Stable across VT switches as
    /// long as the underlying connector slot stays the same.
    pub connector_id: u32,
    /// DRM `State::Connected`; only these become public outputs.
    /// Disconnected connectors stay in the registry for hot-plug but don't
    /// appear in the `outputs()` list.
    pub connected: bool,
    /// Every mode the connector advertises; the registry picks via
    /// `preferred_mode`, and an empty list skips the connector.
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
        // Saturate rather than panic: a `u32` counter needs more than 4
        // billion distinct connectors to ever trip this, far beyond
        // anything reachable via hotplug in one compositor process's
        // lifetime. Saturating means the last-ever id would be reused
        // instead of taking the session down.
        self.next_id = self.next_id.saturating_add(1);
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

    /// Emit one [`Output`] per connected connector advertising at least one
    /// mode, interning each so its `OutputId` is stable across
    /// re-enumeration. A connected connector with no modes is skipped and
    /// warned rather than crashing the backend. Result order follows
    /// `connectors`, so it is deterministic for tests.
    pub fn populate(&mut self, connectors: &[ConnectorInfo]) -> Vec<(OutputId, Output)> {
        use super::devices::preferred_mode;
        let mut out = Vec::with_capacity(connectors.len());
        // Lay connectors out left-to-right: each output's left edge is the
        // sum of prior connected outputs' widths (one output ⇒ origin).
        let mut next_x: i32 = 0;
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
                position: (next_x, 0),
                size: (u32::from(mode.width), u32::from(mode.height)),
                // HiDPI policy lives at the compositor layer, not the
                // backend, so scale defaults to 1.0 here.
                scale: 1.0,
                transform: settings_store::hardware::OutputTransform::Normal,
                color_profile: None,
                refresh_mhz: None,
                logical_position: (next_x, 0),
                identity: crate::compositor::backend::OutputIdentity::default(),
            };
            next_x = next_x.saturating_add(i32::from(mode.width));
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
    fn output_registry_interns_stable_distinct_ids() {
        // Phase `new_registry_is_empty`.
        {
            let r = OutputRegistry::new();
            assert!(r.is_empty());
            assert_eq!(r.len(), 0);
        }
        // Phase `intern_mints_ids_starting_at_zero`.
        {
            let mut r = OutputRegistry::new();
            assert_eq!(r.intern(key(1, 10)), OutputId(0));
            assert_eq!(r.intern(key(1, 11)), OutputId(1));
            assert_eq!(r.intern(key(2, 10)), OutputId(2));
        }
        // Phase `intern_is_stable_across_calls`.
        {
            let mut r = OutputRegistry::new();
            let first = r.intern(key(1, 42));
            let second = r.intern(key(1, 42));
            assert_eq!(first, second);
            // No new id minted.
            assert_eq!(r.len(), 1);
        }
        // Phase `intern_distinct_devices_get_distinct_ids`.
        {
            let mut r = OutputRegistry::new();
            let a = r.intern(key(1, 10));
            let b = r.intern(key(2, 10));
            assert_ne!(a, b);
        }
        // Phase `iter_yields_all_entries`.
        {
            let mut r = OutputRegistry::new();
            r.intern(key(1, 10));
            r.intern(key(1, 11));
            let count = r.iter().count();
            assert_eq!(count, 2);
        }
    }

    #[test]
    fn registry_populates_only_connected_connectors_with_modes() {
        // Phase `registry_populates_from_connector_list`.
        {
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
        // Phase `populate_skips_disconnected_connectors`.
        {
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
        // Phase `populate_skips_connected_connector_with_no_modes`.
        {
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
        // Phase `populate_returns_stable_ids_across_calls`.
        {
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
            assert_eq!(
                first[0].0, second[0].0,
                "OutputId must be stable across re-enumeration"
            );
            assert_eq!(r.len(), 1, "no extra entries on re-enumeration");
        }
    }
}
