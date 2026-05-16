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

use crate::compositor::backend::OutputId;

/// Stable identity of a DRM output: which `cardN` device and which
/// connector handle on that device.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct DrmOutputKey {
    pub device: u64,
    pub connector: u32,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(device: u64, connector: u32) -> DrmOutputKey {
        DrmOutputKey { device, connector }
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
}
