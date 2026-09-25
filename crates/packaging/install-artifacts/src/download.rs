use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
pub(crate) static NEXT_OPERATION: AtomicU64 = AtomicU64::new(1);

pub(crate) fn next_operation(counter: &AtomicU64) -> Result<u64, String> {
    counter
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            if n == 0 { None } else { n.checked_add(1) }
        })
        .map_err(|_| "Operation identifier exhausted".into())
}

/// Read-only cooperative stop view for a scoped consumer. The transfer latch
/// cannot be reset or retained beyond the borrowed consumption scope.
pub(crate) struct Cancellation<'a> {
    _cancelled: &'a AtomicBool,
}
impl<'a> Cancellation<'a> {
    pub(crate) fn new(cancelled: &'a AtomicBool) -> Self {
        Self {
            _cancelled: cancelled,
        }
    }
    #[cfg(test)]
    pub(crate) fn is_cancelled(&self) -> bool {
        self._cancelled.load(Ordering::Acquire)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub(crate) operation: u64,
    pub(crate) selection: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    SavingPreference,
    Authenticating,
    Acquiring,
    Preparing,
    Downloading,
    Ready,
    Consuming,
    Consumed,
    Paused,
    Cancelled,
    Failed,
    Released,
    Discarded,
}
#[derive(Clone, Debug)]
pub struct Status {
    pub token: Token,
    pub phase: Phase,
    /// Last positively acknowledged prefix per role (not bytes merely received).
    pub committed: [u64; 2],
    pub progress: Option<Progress>,
    pub finals: [bool; 2],
    /// Includes unknown residue; false is not a claim that no other cache exists.
    pub residue: bool,
    pub uncertain: bool,
    pub error: Option<String>,
    pub failure_kind: Option<crate::ErrorKind>,
    pub preference_commit: Option<PreferenceCommit>,
}
impl Status {
    pub(crate) fn new(token: Token) -> Self {
        Self {
            token,
            phase: Phase::Authenticating,
            committed: [0; 2],
            progress: None,
            finals: [false; 2],
            residue: false,
            uncertain: false,
            error: None,
            failure_kind: None,
            preference_commit: None,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferPhase {
    Prefix,
    Requesting,
    Receiving,
    Retrying,
    Resetting,
    Verifying,
    Complete,
    Failed,
    Cancelled,
}
/// Progress is advisory. `verified` stays zero until the complete authenticated
/// digest matches; prefix hashes only validate checkpoint consistency. A later
/// coordinator must match the operation ID and successful result before readiness.
#[derive(Clone, Copy, Debug)]
pub struct Progress {
    pub operation: u64,
    pub component: usize,
    pub phase: TransferPhase,
    pub received: u64,
    pub verified: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Availability {
    Available,
    UnsupportedHost,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreferencePublication {
    NotPublished,
    Published,
    PublishedDurabilityUnknown,
    PublicationUnknown,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreferenceNamespace {
    Unchanged,
    Durable,
    CreatedDurabilityUnknown,
    NamespaceUnknown,
    Unproven,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreferenceDurability {
    Durable,
    NamespaceUnproven,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreferenceCommit {
    pub durability: PreferenceDurability,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreferenceFailure {
    pub kind: crate::ErrorKind,
    pub message: String,
    pub publication: PreferencePublication,
    pub namespace: PreferenceNamespace,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PreferenceState {
    Loading,
    Loaded(Option<Vec<u8>>),
    Failed(PreferenceFailure),
}

impl Default for PreferenceState {
    fn default() -> Self {
        Self::Loading
    }
}
