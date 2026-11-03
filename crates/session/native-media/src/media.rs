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

use runtime_collab::native_media::{Input, Inventory, Issue, Picture, Roster, Shown, State};

/// How long apart the session hands a shared screen its next picture: the
/// call encodes at most this many a second.
pub const SCREEN_EVERY: std::time::Duration = std::time::Duration::from_millis(1000 / 15);

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
    /// Play nothing of the call here while `deafened`, and play it again
    /// after: what the person hears, which the service's own mute and the
    /// microphone's stop do not reach. The frames the call receives while
    /// deafened are dropped, not held.
    fn set_deafened(&mut self, _deafened: bool) {}
    /// Leave the call: stop everything this session started.
    fn close(&mut self) {}
    /// Show these people's pictures here, each at the layer named: the call
    /// sends exactly these people's pictures, at exactly these layers, and
    /// nothing else. A person, source or layer left out is not encoded.
    ///
    /// Called with the app's whole list whenever what it shows changes.
    fn show(&mut self, _pictures: Vec<Shown>) {}
    /// Turn this person's camera on in the call, sending it to everyone who
    /// shows it, or off. Turned on, the carrier asks through the session's
    /// normal prompt for the camera first, and opens nothing the person did
    /// not allow.
    ///
    /// # Errors
    /// No camera, or the person did not allow it.
    fn set_camera(&mut self, _on: bool) -> Result<(), Issue> {
        Err(Issue::CameraLost)
    }
    /// Share this person's screen in the call, or stop. Started, the carrier
    /// asks through the session's normal prompt for the screen first.
    ///
    /// # Errors
    /// No screen to share, or the person did not allow it.
    fn set_screen(&mut self, _on: bool) -> Result<(), Issue> {
        Err(Issue::ScreenLost)
    }
    /// The pictures that arrived after `after`, oldest first, together with
    /// the cursor the one after them starts at: an app draws them in that
    /// order. The cursor of the first collection is the call's, so a client
    /// that reconnects starts where the pictures do.
    fn pictures(&mut self, after: u64) -> (Vec<Picture>, u64) {
        (Vec::new(), after)
    }
    /// Woken whenever a picture arrives, so the app collects it instead of
    /// asking again and again.
    fn pictures_changed(&self) -> Option<Arc<tokio::sync::Notify>> {
        None
    }
}

/// The desktop a call shares: the session hands it the frame its render pass
/// just drew, and the call encodes what it took.
///
/// Nothing of a locked screen is handed on: the share withholds its last
/// frame, and a locked desktop publishes nothing new until it is unlocked.
#[derive(Debug, Default)]
pub struct ScreenShare {
    state: std::sync::Mutex<Share>,
}

#[derive(Debug, Default)]
struct Share {
    /// The person shares this desktop in a call.
    sharing: bool,
    /// The screen is locked: its last frame is withheld, and the call keeps
    /// sending nothing until the desktop is unlocked.
    withheld: bool,
    /// The newest frame, and when the render pass drew it.
    frame: Option<(Instant, u32, u32, Vec<u8>)>,
}

impl ScreenShare {
    /// A desktop no call shares yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Share this desktop from now, or stop sharing it: stopping forgets the
    /// frame, so nothing of it stays behind to be sent.
    pub fn set_sharing(&self, sharing: bool) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.sharing = sharing;
        if sharing {
            state.withheld = false;
        } else {
            state.withheld = false;
            state.frame = None;
        }
    }

    /// The person shares this desktop.
    #[must_use]
    pub fn sharing(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sharing
    }

    /// The frame the render pass drew must be handed on: the share is on, and
    /// the one it holds is a picture interval old.
    #[must_use]
    pub fn wants_frame(&self, now: Instant) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.sharing
            && state
                .frame
                .as_ref()
                .is_none_or(|(drawn, ..)| now.saturating_duration_since(*drawn) >= SCREEN_EVERY)
    }

    /// When the next frame of a shared screen is due; `None` while nothing is
    /// shared, or a locked desktop withholds its picture.
    #[must_use]
    pub fn next_frame(&self, now: Instant) -> Option<Instant> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (state.sharing && !state.withheld).then(|| {
            state.frame.as_ref().map_or(now, |(drawn, ..)| {
                drawn.checked_add(SCREEN_EVERY).unwrap_or(now)
            })
        })
    }

    /// Hand on the desktop as the render pass just drew it.
    pub fn publish_rgba(&self, width: u32, height: u32, rgba: Vec<u8>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.sharing {
            return;
        }
        state.withheld = false;
        state.frame = Some((Instant::now(), width, height, rgba));
    }

    /// Forget the frame: a locked screen is never shared, not even the one
    /// drawn before the lock.
    pub fn withhold(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.withheld = true;
        state.frame = None;
    }

    /// The screen is locked, so the share holds nothing to send, not even
    /// what it drew before the lock.
    #[must_use]
    pub fn withheld(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .withheld
    }

    /// The newest frame, if one was drawn since the last was taken.
    #[must_use]
    pub fn take(&self) -> Option<(u32, u32, Vec<u8>)> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .frame
            .take()
            .map(|(_, width, height, rgba)| (width, height, rgba))
    }
}
