//! `OutputId` allocation + DRM-output bookkeeping.
//!
//! `OutputId(u32)` must be minted from connector handles deterministically,
//! so re-enumerating after a VT switch keeps the same ids. Pure data — this
//! never touches DRM; callers feed it `(card_dev_t, connector_handle)` pairs.

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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(device: u64, connector: u32) -> DrmOutputKey {
        DrmOutputKey { device, connector }
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
}
