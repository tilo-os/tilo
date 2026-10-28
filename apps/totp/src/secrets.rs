//! The secret half of each account: its base32 seed, kept in the app's
//! secret store on this device as the account's item secret
//! (`secrets().item("seed", id)`).
//!
//! The document names accounts and never holds a seed, so every device keeps
//! its own copy of the seeds it was given. A seed that cannot be read or
//! written is kept for this session and reported; the user retries once the
//! store answers again, never a loop.

use std::collections::{HashMap, HashSet};

use ui::secrets::SecretStore;

use crate::model::AccountId;

/// The purpose every account's seed is stored under.
const SEED: &str = "seed";

/// A change the store has not taken yet.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Unsaved {
    Put(String),
    Forget,
}

/// The seeds of the accounts a view shows, read from and written to one
/// store.
pub(crate) struct Seeds {
    store: SecretStore,
    /// Whether this view may change the store. A past version shows the
    /// seeds it finds and never forgets or writes one.
    writable: bool,
    known: HashMap<AccountId, String>,
    /// Accounts whose seed was looked up, present or not.
    probed: HashSet<AccountId>,
    unsaved: HashMap<AccountId, Unsaved>,
    /// A lookup failed: the store is locked or gone.
    unreadable: bool,
}

impl Seeds {
    pub(crate) fn new(store: SecretStore, writable: bool) -> Self {
        Self {
            store,
            writable,
            known: HashMap::new(),
            probed: HashSet::new(),
            unsaved: HashMap::new(),
            unreadable: false,
        }
    }

    /// Follow the accounts the document holds now: look up the seed of each
    /// new one, and forget the seed of an account that left the document,
    /// as deleting it there asks.
    pub(crate) fn follow(&mut self, accounts: &[AccountId]) {
        let present: HashSet<AccountId> = accounts.iter().copied().collect();
        let gone: Vec<AccountId> = self
            .probed
            .iter()
            .filter(|id| !present.contains(id))
            .copied()
            .collect();
        for id in gone {
            self.probed.remove(&id);
            self.known.remove(&id);
            if self.writable {
                self.write(id, Unsaved::Forget);
            }
        }
        for &id in accounts {
            if !self.probed.contains(&id) {
                self.look_up(id);
            }
        }
    }

    fn look_up(&mut self, id: AccountId) {
        match self.store.item(SEED, &id.to_string()).get_blocking() {
            Ok(found) => {
                self.probed.insert(id);
                if let Some(seed) =
                    found.and_then(|seed| String::from_utf8(seed.expose().to_vec()).ok())
                    && !self.unsaved.contains_key(&id)
                {
                    self.known.insert(id, seed);
                }
            }
            Err(error) => {
                tracing::warn!(%error, "authenticator: an account's seed could not be read");
                self.unreadable = true;
            }
        }
    }

    /// This device's seed for `id`, if it has one.
    pub(crate) fn get(&self, id: AccountId) -> Option<&str> {
        self.known.get(&id).map(String::as_str)
    }

    /// Keep `seed` as the seed of `id` on this device. `false` when the store
    /// did not take it: the seed still works for this session.
    pub(crate) fn put(&mut self, id: AccountId, seed: &str) -> bool {
        self.known.insert(id, seed.to_owned());
        self.probed.insert(id);
        self.write(id, Unsaved::Put(seed.to_owned()))
    }

    fn write(&mut self, id: AccountId, change: Unsaved) -> bool {
        let item = self.store.item(SEED, &id.to_string());
        let written = match &change {
            Unsaved::Put(seed) => item.put_blocking(seed.as_bytes()),
            Unsaved::Forget => item.delete_blocking(),
        };
        match written {
            Ok(()) => {
                self.unsaved.remove(&id);
                true
            }
            Err(error) => {
                tracing::warn!(%error, "authenticator: an account's seed could not be saved");
                self.unsaved.insert(id, change);
                false
            }
        }
    }

    /// Whether a seed is unsaved or the store could not be read.
    pub(crate) fn pending(&self) -> bool {
        self.unreadable || !self.unsaved.is_empty()
    }

    /// Whether the store could not be read.
    pub(crate) fn unreadable(&self) -> bool {
        self.unreadable
    }

    /// Try the store again: write what it did not take, and look up the
    /// seeds of `accounts` it could not read. `true` when nothing is left.
    pub(crate) fn retry(&mut self, accounts: &[AccountId]) -> bool {
        let unsaved: Vec<(AccountId, Unsaved)> = self.unsaved.drain().collect();
        for (id, change) in unsaved {
            self.write(id, change);
        }
        if self.unreadable {
            self.unreadable = false;
            for &id in accounts {
                if !self.probed.contains(&id) {
                    self.look_up(id);
                }
            }
        }
        !self.pending()
    }
}

/// A secret store that refuses everything while it is down, for tests of
/// what the app does then.
#[cfg(test)]
pub(crate) mod flaky {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use ui::secrets::{Protection, Secret, SecretBackend, SecretError};

    /// A store that refuses everything while `down` is set.
    pub(crate) struct Flaky {
        pub(crate) down: AtomicBool,
        inner: Arc<dyn SecretBackend>,
    }

    impl Flaky {
        pub(crate) fn new() -> Arc<Self> {
            Arc::new(Self {
                down: AtomicBool::new(false),
                inner: ui::secrets::memory_backend(),
            })
        }

        fn check(&self) -> Result<(), SecretError> {
            if self.down.load(Ordering::SeqCst) {
                Err(SecretError::Denied)
            } else {
                Ok(())
            }
        }
    }

    impl SecretBackend for Flaky {
        fn protection(&self) -> Protection {
            Protection::Memory
        }

        fn get(&self, app: &str, key: &str) -> Result<Option<Secret>, SecretError> {
            self.check()?;
            self.inner.get(app, key)
        }

        fn put(&self, app: &str, key: &str, value: &[u8], label: &str) -> Result<(), SecretError> {
            self.check()?;
            self.inner.put(app, key, value, label)
        }

        fn delete(&self, app: &str, key: &str) -> Result<(), SecretError> {
            self.check()?;
            self.inner.delete(app, key)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::flaky::Flaky;
    use super::*;
    use std::sync::atomic::Ordering;

    fn id(n: u128) -> AccountId {
        AccountId::from_u128(n)
    }

    #[test]
    fn seeds_live_under_the_account_id_and_leave_with_the_account() {
        let store = SecretStore::memory("so.tilo.totp");
        let mut seeds = Seeds::new(store.clone(), true);
        assert!(seeds.put(id(1), "JBSWY3DPEHPK3PXP"));
        assert_eq!(
            store
                .item(SEED, &id(1).to_string())
                .get_blocking()
                .unwrap()
                .map(|seed| seed.expose().to_vec()),
            Some(b"JBSWY3DPEHPK3PXP".to_vec())
        );

        let mut other = Seeds::new(store.clone(), true);
        other.follow(&[id(1), id(2)]);
        assert_eq!(other.get(id(1)), Some("JBSWY3DPEHPK3PXP"));
        assert_eq!(other.get(id(2)), None, "no seed was given for this one");

        // The account leaves the document: its seed leaves the device.
        other.follow(&[id(2)]);
        assert_eq!(other.get(id(1)), None);
        assert!(!store.item(SEED, &id(1).to_string()).present_blocking());
    }

    #[test]
    fn a_past_version_never_forgets_a_seed() {
        let store = SecretStore::memory("so.tilo.totp");
        Seeds::new(store.clone(), true).put(id(1), "JBSWY3DPEHPK3PXP");
        let mut past = Seeds::new(store.clone(), false);
        past.follow(&[id(1)]);
        assert_eq!(past.get(id(1)), Some("JBSWY3DPEHPK3PXP"));
        past.follow(&[]);
        assert!(store.item(SEED, &id(1).to_string()).present_blocking());
    }

    #[test]
    fn a_refused_write_is_kept_for_the_session_until_a_retry_lands() {
        let backend = Flaky::new();
        let store = SecretStore::with_backend("so.tilo.totp", backend.clone());
        let mut seeds = Seeds::new(store.clone(), true);
        backend.down.store(true, Ordering::SeqCst);
        assert!(!seeds.put(id(1), "JBSWY3DPEHPK3PXP"));
        assert!(seeds.pending());
        assert_eq!(seeds.get(id(1)), Some("JBSWY3DPEHPK3PXP"), "usable now");
        assert!(!seeds.retry(&[id(1)]), "still refused");

        backend.down.store(false, Ordering::SeqCst);
        assert!(seeds.retry(&[id(1)]));
        assert!(!seeds.pending());
        assert!(store.item(SEED, &id(1).to_string()).present_blocking());
    }

    #[test]
    fn an_unreadable_store_is_reported_and_read_again_on_retry() {
        let backend = Flaky::new();
        let store = SecretStore::with_backend("so.tilo.totp", backend.clone());
        Seeds::new(store.clone(), true).put(id(1), "JBSWY3DPEHPK3PXP");

        backend.down.store(true, Ordering::SeqCst);
        let mut seeds = Seeds::new(store, true);
        seeds.follow(&[id(1)]);
        assert!(seeds.unreadable());
        assert_eq!(seeds.get(id(1)), None);

        backend.down.store(false, Ordering::SeqCst);
        assert!(seeds.retry(&[id(1)]));
        assert_eq!(seeds.get(id(1)), Some("JBSWY3DPEHPK3PXP"));
    }
}
