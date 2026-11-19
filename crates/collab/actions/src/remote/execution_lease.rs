//! The lifetime of one admitted connection's executions.

/// Orders mints and closes of every execution in this process: what tells a connection that
/// replaced an ended one from one that ran beside it.
static ORDER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn next() -> u64 {
    ORDER.fetch_add(1, std::sync::atomic::Ordering::AcqRel)
}

/// Provider-owned execution lifetime. Clones share closure, never authority.
/// Equality and hashing use the immutable minted identity, not liveness.
#[derive(Clone, Debug)]
pub struct ExecutionLease {
    pub(super) id: uuid::Uuid,
    live: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// When it was minted, in [`ORDER`].
    minted: u64,
    /// When it was closed, in [`ORDER`]; zero while it is live.
    closed: std::sync::Arc<std::sync::atomic::AtomicU64>,
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
            minted: next(),
            closed: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    #[must_use]
    pub fn is_live(&self) -> bool {
        self.live.load(std::sync::atomic::Ordering::Acquire)
    }
    /// Retire observation of this execution. This does not cancel any app operation.
    pub fn close(&self) {
        let _ = self.closed.compare_exchange(
            0,
            next(),
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        );
        self.live.store(false, std::sync::atomic::Ordering::Release);
    }

    /// Whether this execution was minted after `ended` closed: the connection that replaced it,
    /// never one that ran beside it.
    #[must_use]
    pub fn replaced(&self, ended: &Self) -> bool {
        let closed = ended.closed.load(std::sync::atomic::Ordering::Acquire);
        closed != 0 && self.is_live() && self.minted > closed
    }
}
