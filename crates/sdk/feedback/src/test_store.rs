//! Inject a writer's initial refusal without replacing the production outbox.

use durable_store::{
    Durability, DurableStore, Lease, MemoryStore, ReadyWake, Revision, SettleWatch, SharedLease,
    StorageKey, StoreError, Stored, WriteOp,
};
use std::cell::Cell;

pub(crate) struct RejectOnceStore {
    inner: MemoryStore,
    reject: Cell<bool>,
}

impl RejectOnceStore {
    pub(crate) fn new() -> Self {
        Self {
            inner: MemoryStore::new(),
            reject: Cell::new(true),
        }
    }
}

impl DurableStore for RejectOnceStore {
    fn namespace(&self) -> &str {
        self.inner.namespace()
    }
    fn read(&self, key: &StorageKey) -> Result<Option<Stored>, StoreError> {
        self.inner.read(key)
    }
    fn write(
        &self,
        key: &StorageKey,
        op: WriteOp,
        writer: Option<&[u8]>,
    ) -> Result<Revision, StoreError> {
        if self.reject.replace(false) {
            return Err(StoreError::Failed {
                context: "test writer".into(),
                message: "initial write refused".into(),
            });
        }
        self.inner.write(key, op, writer)
    }
    fn durability(&self, key: &StorageKey, revision: Revision) -> Durability {
        self.inner.durability(key, revision)
    }
    fn flush(&self, key: Option<&StorageKey>) -> Result<(), StoreError> {
        self.inner.flush(key)
    }
    fn wake_when_settled(&self, key: &StorageKey, wake: ReadyWake) -> SettleWatch {
        self.inner.wake_when_settled(key, wake)
    }
    fn lease(&self, key: &StorageKey) -> Result<Lease, StoreError> {
        self.inner.lease(key)
    }
    fn lease_shared(&self, key: &StorageKey) -> Result<SharedLease, StoreError> {
        self.inner.lease_shared(key)
    }
    fn claim(&self, key: &StorageKey, claimant: &str) -> Result<bool, StoreError> {
        self.inner.claim(key, claimant)
    }
}
