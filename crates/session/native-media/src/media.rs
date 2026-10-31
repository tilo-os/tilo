//! What carries a call: the channel a call is in, and the session's own side
//! of it.
//!
//! A call is held by whoever can put this device into the channel: the
//! collaboration plane, where being in the channel is the presence everyone
//! in the room reads. A factory prepares one session per call and says who is
//! in the record's channels; the session says what the call's own state is,
//! which microphone it sends, and stops when it ends.

use std::sync::Arc;
use std::time::Instant;

use runtime_collab::native_media::{Input, Inventory, Issue, Roster, State};

/// Inert exact lookup input. A factory must independently consult its real
/// current owner; these strings are not media authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallTarget {
    /// The workspace (its room id) the app's call is in.
    pub workspace: String,
    /// The app the call belongs to.
    pub app: String,
    /// The app's record holding the voice channels.
    pub record: String,
    /// The voice channel this call joins.
    pub channel: String,
}

/// A call being prepared, or why it cannot be.
pub type Prepare = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Box<dyn MediaSession>, Issue>> + Send>,
>;
/// Who is in the call of each voice channel of a target's record.
pub type Rosters = std::pin::Pin<Box<dyn std::future::Future<Output = Vec<Roster>> + Send>>;

/// What puts this device into a channel, and who else is in the record's
/// channels.
pub trait MediaFactory: Send + Sync {
    /// Start this device's side of the call: joining the channel, and the
    /// state that follows from it.
    fn prepare(&self, target: CallTarget) -> Prepare;
    /// The calls of `target`'s record (its `channel` is unused) as the
    /// carrier last heard them, sorted by channel. A factory without a room
    /// knows none.
    fn rosters(&self, _target: CallTarget) -> Rosters {
        Box::pin(async { Vec::new() })
    }
    /// Woken whenever a room's call list may have changed, so a voice
    /// channel shows a join or leave at once. A factory without a room has
    /// none.
    fn rosters_changed(&self) -> Option<Arc<tokio::sync::Notify>> {
        None
    }
}

/// This device's side of one call: where it stands, and what it sends.
pub trait MediaSession: Send {
    /// Bounded polling only: no external I/O, device state wait, or worker
    /// join.
    fn tick(&mut self, now: Instant) -> Result<State, Issue>;
    /// The streams this device publishes for the call, never an inferred
    /// participant roster.
    fn inventory(&self) -> Inventory {
        Inventory::default()
    }
    /// The microphones this call could send, labelled as the person
    /// chooses between them. Empty when the carrier sends no microphone:
    /// a call with none leaves the person in it and muted.
    fn inputs(&self) -> Vec<Input> {
        Vec::new()
    }
    /// Send this device's microphone from `input`.
    ///
    /// The call service calls this only after the person asked to be heard
    /// and allowed the microphone through the session's normal prompt; a
    /// carrier must not open a source the person did not allow.
    fn start_capture(&mut self, _input: &str) -> Result<(), Issue> {
        Err(Issue::SourceLost)
    }
    /// Stop sending the microphone at once.
    fn stop_capture(&mut self) {}
    /// Leave the call: stop everything this session started.
    fn close(&mut self) {}
}
