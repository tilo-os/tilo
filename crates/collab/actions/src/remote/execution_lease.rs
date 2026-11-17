//! The lifetime of one admitted connection's executions.

/// Provider-owned execution lifetime. Clones share closure, never authority.
/// Equality and hashing use the immutable minted identity, not liveness.
#[derive(Clone, Debug)]
pub struct ExecutionLease {
    pub(super) id: uuid::Uuid,
    live: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl PartialEq for ExecutionLease {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for ExecutionLease {}
impl std::hash::Hash for ExecutionLease {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}
impl ExecutionLease {
    /// A new execution, live until [`Self::close`]: the provider that admitted a
    /// connection mints one for it.
    #[must_use]
    pub fn mint() -> Self {
        Self {
            id: uuid::Uuid::new_v4(),
            live: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }

    #[must_use]
    pub fn is_live(&self) -> bool {
        self.live.load(std::sync::atomic::Ordering::Acquire)
    }
    /// Retire observation of this execution. This does not cancel any app operation.
    pub fn close(&self) {
        self.live.store(false, std::sync::atomic::Ordering::Release);
    }
}
