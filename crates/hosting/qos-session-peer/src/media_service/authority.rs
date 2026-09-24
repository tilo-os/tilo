//! Trusted owner adapters. A turn is borrowed under the real owner's current
//! authority boundary; none of these interfaces establishes installation.
use super::*;
use crate::committed_authority::Current;
pub use crate::room_media_policy::RoomMediaPolicy;

pub enum MediaObservation<T> {
    Current(T),
    /// Actual temporary owner-lock contention, never unknown/corrupt state.
    Busy,
    Denied,
}

pub trait MediaAuthoritySource: Send + Sync + 'static {
    type Turn<'a>: MediaAuthorityTurn
    where
        Self: 'a;

    /// Invoke once while retaining the actual committed owner guard. The
    /// result cannot borrow that guard; no asynchronous work belongs here.
    fn with_current<R>(&self, action: impl FnOnce(&Self::Turn<'_>) -> R) -> MediaObservation<R>;
}

pub trait MediaAuthorityTurn {
    fn peer(&self) -> &StatePeer;
    fn policy(&self) -> &RoomMediaPolicy;
    fn workspace(&self) -> u64;
    fn continuity(&self) -> u64;
    fn now_ns(&self) -> u64;
}

/// Only the common, authenticated media control constructs these inputs.
pub struct MediaSigningInput([u8; 32]);
impl MediaSigningInput {
    pub fn digest(&self) -> &[u8; 32] {
        &self.0
    }
    pub(super) fn new(digest: [u8; 32]) -> Self {
        Self(digest)
    }
}

pub struct NativeAuthoritySource {
    pub(super) committed: CommittedAuthority,
    workspace: u64,
}
pub struct NativeMediaTurn<'a> {
    pub(super) current: Current<'a>,
    workspace: u64,
    now_ns: u64,
}
impl NativeAuthoritySource {
    pub(super) fn new(owner: &SessionOwner, committed: CommittedAuthority) -> Self {
        Self {
            committed,
            workspace: owner.workspace_id(),
        }
    }
}
impl MediaAuthoritySource for NativeAuthoritySource {
    type Turn<'a> = NativeMediaTurn<'a>;

    #[track_caller]
    fn with_current<R>(&self, action: impl FnOnce(&Self::Turn<'_>) -> R) -> MediaObservation<R> {
        match self.committed.observe() {
            Observation::Current(current) => {
                #[cfg(test)]
                record_current_turn();
                MediaObservation::Current(action(&NativeMediaTurn {
                    current,
                    workspace: self.workspace,
                    now_ns: tilo_relay::clock::now_ns(),
                }))
            }
            Observation::Updating => MediaObservation::Busy,
            Observation::Denied => MediaObservation::Denied,
        }
    }
}
impl MediaAuthorityTurn for NativeMediaTurn<'_> {
    fn peer(&self) -> &StatePeer {
        self.current.peer()
    }
    fn policy(&self) -> &RoomMediaPolicy {
        self.current.media_policy()
    }
    fn workspace(&self) -> u64 {
        self.workspace
    }
    fn continuity(&self) -> u64 {
        self.current.continuity()
    }
    fn now_ns(&self) -> u64 {
        self.now_ns
    }
}
