//! The owner seam shared by both hosts' browser-facing services.
//!
//! A [`HostOwner`] is the one serialized authority of a workspace host (the
//! device host's committed owner publication, or the Cloud Store). Services such
//! as media and live never hold the owner itself: they borrow one [`OwnerTurn`]
//! under the owner's current authority boundary, read what they need, and return
//! a result that cannot borrow the turn. No asynchronous work belongs in a turn.
use crate::{
    SessionOwner,
    committed_authority::{CommittedAuthority, Current, Observation},
    room_media_policy::RoomMediaPolicy,
};
use tilo_room::StatePeer;

/// Outcome of asking an owner for a current turn.
pub enum Observed<T> {
    /// The action ran against current, committed owner state.
    Current(T),
    /// Actual temporary owner contention, never unknown or corrupt state.
    Busy,
    /// The owner is retired, recovering or failed; no authority is current.
    Denied,
}

/// Read-only view of an owner's current committed state.
pub trait OwnerTurn {
    /// The committed workspace peer.
    fn peer(&self) -> &StatePeer;
    /// Media call policy derived from the committed room.
    fn media_policy(&self) -> &RoomMediaPolicy;
    /// Governed workspace id.
    fn workspace(&self) -> u64;
    /// Owner continuity: changes whenever durable owner state is replaced, so a
    /// grant admitted under one continuity is not honoured under another.
    fn continuity(&self) -> u64;
    /// The owner's trusted clock, sampled when this turn began.
    fn now_ns(&self) -> u64;
    /// Resample the same trusted clock while this turn is still held, or
    /// `None` once the owner's clock is no longer this turn's.
    fn fresh_now_ns(&self) -> Option<u64>;
}

/// One workspace host's serialized authority.
pub trait HostOwner: Send + Sync + 'static {
    /// The borrowed view handed to one action.
    type Turn<'a>: OwnerTurn
    where
        Self: 'a;

    /// Invoke `action` once while holding the owner's current authority.
    fn with_current<R>(&self, action: impl FnOnce(&Self::Turn<'_>) -> R) -> Observed<R>;

    /// The owner's trusted clock without taking a turn, or `None` once the
    /// owner is retired.
    fn now_ns(&self) -> Option<u64>;
}

#[cfg(test)]
thread_local! {
    pub(crate) static CURRENT_TURNS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The device host's owner, observed through its committed publication.
pub struct NativeOwner {
    pub(crate) committed: CommittedAuthority,
    workspace: u64,
}

/// One turn over the device host's committed publication.
pub struct NativeTurn<'a> {
    pub(crate) current: Current<'a>,
    workspace: u64,
    now_ns: u64,
}

impl NativeOwner {
    pub(crate) fn new(owner: &SessionOwner, committed: CommittedAuthority) -> Self {
        Self {
            committed,
            workspace: owner.workspace_id(),
        }
    }
}

impl HostOwner for NativeOwner {
    type Turn<'a> = NativeTurn<'a>;

    #[track_caller]
    fn with_current<R>(&self, action: impl FnOnce(&Self::Turn<'_>) -> R) -> Observed<R> {
        match self.committed.observe() {
            Observation::Current(current) => {
                #[cfg(test)]
                CURRENT_TURNS.with(|count| count.set(count.get() + 1));
                Observed::Current(action(&NativeTurn {
                    current,
                    workspace: self.workspace,
                    now_ns: tilo_delivery_protocol::clock::now_ns(),
                }))
            }
            Observation::Updating => Observed::Busy,
            Observation::Denied => Observed::Denied,
        }
    }

    fn now_ns(&self) -> Option<u64> {
        Some(tilo_delivery_protocol::clock::now_ns())
    }
}

impl OwnerTurn for NativeTurn<'_> {
    fn peer(&self) -> &StatePeer {
        self.current.peer()
    }
    fn media_policy(&self) -> &RoomMediaPolicy {
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
    fn fresh_now_ns(&self) -> Option<u64> {
        Some(tilo_delivery_protocol::clock::now_ns())
    }
}

impl<'a> NativeTurn<'a> {
    /// A turn over an already held committed read on the owner loop.
    pub(crate) fn held(current: Current<'a>, workspace: u64) -> Self {
        Self {
            current,
            workspace,
            now_ns: tilo_delivery_protocol::clock::now_ns(),
        }
    }
}
