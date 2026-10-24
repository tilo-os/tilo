//! A document's save status, said in the window's bar
//! ([`crate::window_toolbar::notify`]).
//!
//! A save that lands within [`SAVE_STALL`] says nothing, however long it
//! took: a document that autosaves needs no "Saving…" and no "Saved".
//! Unconfirmed after [`SAVE_STALL`], the bar's notice says "Couldn't save
//! yet. Retrying…" and the surface asks for one more submission
//! ([`SaveStatusSurface::take_retry`]); the save that then lands says
//! "Saved" for a notice's lifetime. After [`SAVE_GIVE_UP`] the notice says
//! "Not saved" with "Try again" beside it, and stays. If nobody sees the
//! window then (the document is out of sight), the surface asks the host to
//! raise a notification ([`SaveStatusSurface::take_notice`]), since the
//! person could lose work they cannot see.
//!
//! A save that failed with a reason is an error notice with the reason and
//! "Try again". It stays, through the retries it asks for, until a save
//! succeeds; that save says "Saved" once, however quickly it landed. The
//! state itself stays readable through [`SaveStatusSurface::state`] for an
//! app's accessibility status and automation. A framework window gets a
//! surface from its root, fed and routed with no app code (tilo-app's
//! `framework::Saves`).

use std::cell::RefCell;

use crate::time::{Duration, Instant};
use crate::widget::toast::{ToastKind, Transient};
use crate::window_toolbar::{ToolbarAction, ToolbarNotice};

/// What a document's save state is, as the save status surface says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveState {
    /// Nothing to report (memory only, or nothing changed yet).
    Idle,
    Saving,
    /// The last change is stored.
    Saved,
    /// The last save failed; the text says why.
    Failed(String),
}

impl From<&crate::record::RecordSaveStatus> for SaveState {
    fn from(status: &crate::record::RecordSaveStatus) -> Self {
        use crate::record::RecordSaveStatus;
        match status {
            RecordSaveStatus::Saved => Self::Saved,
            RecordSaveStatus::Saving => Self::Saving,
            RecordSaveStatus::Failed(reason) => Self::Failed(reason.clone()),
            RecordSaveStatus::MemoryOnly => Self::Idle,
        }
    }
}

/// What a person did about a save.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveAction {
    /// "Try again" on a save that failed or gave up waiting.
    Retry,
}

/// The bar action id of the save notice's "Try again".
pub const SAVE_RETRY_ACTION: &str = "save-status-retry";

/// How long a save may wait for the store to confirm before the surface
/// says so in plain words and asks the host to submit the changes again.
/// Until then it says nothing: a document whose every change is a CRDT
/// operation saves continuously, and a save that is merely slow is a
/// performance bug to fix where it happens, never news for the person.
pub const SAVE_STALL: Duration = Duration::from_secs(10);
/// How long a save may stay unconfirmed before the surface stops waiting
/// and asks the person to try again: the first submission's
/// [`SAVE_STALL`], then as long again for the retry it asked for, so the
/// retry gets the same chance before the person is asked.
pub const SAVE_GIVE_UP: Duration = Duration::from_secs(20);
/// How long a save that gave up may go unseen before the surface takes it
/// that nobody sees the document: a window in view draws in the frame its
/// wake asks for, far sooner.
const UNSEEN_GRACE: Duration = Duration::from_secs(1);

/// How far an unconfirmed save has gone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    /// Not long enough to mention.
    #[default]
    Quiet,
    /// Past [`SAVE_STALL`]: "Couldn't save yet. Retrying…".
    Retrying,
    /// Past [`SAVE_GIVE_UP`]: "Not saved" with "Try again".
    NotSaved,
}

/// What to tell a person who cannot see a document whose save gave up:
/// post it as a notification whose action is "Try again", and route that
/// action to [`SaveStatusSurface::retry_from_notice`]. See
/// [`SaveStatusSurface::take_notice`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveNotice {
    pub title: String,
    pub body: String,
    pub action: String,
}

/// A document's save status, in the window's bar (see the module).
#[derive(Debug, Default)]
pub struct SaveStatusSurface {
    state: Option<SaveState>,
    /// The reason of the failure a save has not yet ended: it stands
    /// through the retries until a save succeeds.
    failure: Option<String>,
    /// "Saved", or "Retrying…" while a save waits, for a notice's lifetime.
    passing: Transient<(String, ToastKind)>,
    /// When the current save began waiting.
    saving_since: Option<Instant>,
    stage: Stage,
    /// A submission the surface asked for, not yet taken by the host.
    retry: bool,
    /// When a save gave up without the window being seen since.
    unseen_since: Option<Instant>,
    /// Whether a notification about this save is up.
    notified: bool,
    /// A notification to take down, not yet taken by the host.
    withdraw: bool,
    /// What [`Self::publish`] put in the window's bar.
    published: RefCell<Option<ToolbarNotice>>,
}

/// A save status gone with its window leaves the window's bar with it.
impl Drop for SaveStatusSurface {
    fn drop(&mut self) {
        if let Some(notice) = self.published.get_mut().take() {
            crate::window_toolbar::withdraw(&notice);
        }
    }
}

impl SaveStatusSurface {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Report the save state. A change says it; the same state again does
    /// nothing, so calling this every frame is fine. Returns whether what
    /// the surface says changed.
    pub fn set(&mut self, state: SaveState, now: Instant) -> bool {
        if self.state.as_ref() == Some(&state) {
            return false;
        }
        let before = self.notice();
        let noticed = self.stage > Stage::Quiet;
        self.saving_since = (state == SaveState::Saving).then_some(now);
        self.stage = Stage::Quiet;
        self.unseen_since = None;
        self.withdraw |= self.notified;
        self.notified = false;
        let recovered = self.failure.is_some() && state == SaveState::Saved;
        match &state {
            SaveState::Failed(reason) => self.failure = Some(reason.clone()),
            // A retry passes through the saving state before it lands.
            SaveState::Saving => {}
            SaveState::Saved | SaveState::Idle => self.failure = None,
        }
        self.state = Some(state);
        // Only a save the person saw waiting, or the one that ends a
        // failure, is worth confirming.
        if self.state == Some(SaveState::Saved) && (noticed || recovered) {
            self.passing
                .show((crate::i18n!("save-status-saved"), ToastKind::Success));
        } else {
            self.passing.dismiss();
        }
        before != self.notice()
    }

    /// The save state last reported, for an app's accessibility status and
    /// automation.
    #[must_use]
    pub fn state(&self) -> Option<&SaveState> {
        self.state.as_ref()
    }

    /// Move an unconfirmed save along at `now`: "Retrying…" (asking for a
    /// submission) past [`SAVE_STALL`], "Not saved" past [`SAVE_GIVE_UP`],
    /// and let a passing notice go once its time is up. Returns whether
    /// what the surface says changed.
    pub fn advance(&mut self, now: Instant) -> bool {
        let before = self.notice();
        self.passing
            .keep_while(self.stage == Stage::Retrying && self.waiting());
        self.passing.expire();
        if self.waiting() {
            let since = *self.saving_since.get_or_insert(now);
            let waited = now.saturating_duration_since(since);
            let stage = if waited >= SAVE_GIVE_UP {
                Stage::NotSaved
            } else if waited >= SAVE_STALL {
                Stage::Retrying
            } else {
                Stage::Quiet
            };
            if stage > self.stage {
                if self.stage < Stage::Retrying {
                    self.retry = true;
                }
                if stage == Stage::NotSaved {
                    // Seen only once the window is.
                    self.unseen_since = Some(now);
                    self.passing.dismiss();
                } else {
                    self.passing
                        .show_progress((crate::i18n!("save-status-stalled"), ToastKind::Warning));
                }
                self.stage = stage;
            }
        }
        before != self.notice()
    }

    /// A save is in flight and no failure stands over it.
    fn waiting(&self) -> bool {
        self.state == Some(SaveState::Saving) && self.failure.is_none()
    }

    /// The window was seen (it drew a frame): a save that gave up is said
    /// in place, and a notification about it would only repeat it.
    pub fn seen(&mut self) {
        if self.unseen_since.take().is_some() {
            self.withdraw |= self.notified;
            self.notified = false;
        }
    }

    /// Whether the surface asked for the record's changes to be submitted
    /// again (a save that stalled), once. A host routes it like
    /// [`SaveAction::Retry`].
    pub fn take_retry(&mut self) -> bool {
        std::mem::take(&mut self.retry)
    }

    /// "Try again" pressed beside the save's notice in the window's bar,
    /// once: the host submits the changes again. A save that gave up
    /// waits afresh.
    pub fn take_bar_action(&mut self) -> Option<SaveAction> {
        if !crate::window_toolbar::take_notice_press(SAVE_RETRY_ACTION) {
            return None;
        }
        if self.gave_up() {
            self.wait_again();
        } else if self.failure.is_none() {
            return None;
        }
        Some(SaveAction::Retry)
    }

    /// A notification to raise about a save that gave up while nobody saw
    /// the window, once, at `now`. It names the `document` (its title, if
    /// it has one) and says the changes are still in `app`, since whoever
    /// reads it is somewhere else. The host posts it and routes its action
    /// to [`Self::retry_from_notice`]; [`Self::take_withdrawal`] says when
    /// to take it down.
    pub fn take_notice(
        &mut self,
        now: Instant,
        document: Option<&str>,
        app: &str,
    ) -> Option<SaveNotice> {
        self.advance(now);
        let unseen = self.unseen_since?;
        if self.notified || now.saturating_duration_since(unseen) < UNSEEN_GRACE {
            return None;
        }
        self.notified = true;
        let title = match document.map(str::trim).filter(|title| !title.is_empty()) {
            Some(document) => crate::i18n!("save-status-not-saved-named", "document" => document),
            None => crate::i18n!("save-status-not-saved"),
        };
        Some(SaveNotice {
            title,
            body: crate::i18n!("save-status-not-saved-notice", "app" => app),
            action: crate::i18n!("save-status-retry"),
        })
    }

    /// Whether a notification [`Self::take_notice`] raised should come
    /// down (the save landed or failed differently, the person tried again,
    /// or the window is in view again and says it in place), once.
    pub fn take_withdrawal(&mut self) -> bool {
        std::mem::take(&mut self.withdraw)
    }

    /// Start an unconfirmed save's wait over: the person (or the
    /// notification's action) asked to try again.
    fn wait_again(&mut self) {
        // The next [`Self::advance`] starts the wait at its `now`.
        self.saving_since = None;
        self.stage = Stage::Quiet;
        self.unseen_since = None;
        self.withdraw |= self.notified;
        self.notified = false;
    }

    /// "Try again" from the notification [`Self::take_notice`] raised: the
    /// host submits the changes again and the wait starts over.
    pub fn retry_from_notice(&mut self) {
        if self.gave_up() {
            self.wait_again();
        }
    }

    /// Whether the current save gave up waiting and asks the person.
    #[must_use]
    pub fn gave_up(&self) -> bool {
        self.waiting() && self.stage == Stage::NotSaved
    }

    /// The reason of the failure that stands, if any.
    #[must_use]
    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    /// What the window's bar says of the save, if anything: a failure or a
    /// save that gave up, held with "Try again", or a passing "Saved" or
    /// "Retrying…".
    #[must_use]
    pub fn notice(&self) -> Option<ToolbarNotice> {
        let retry = || {
            ToolbarAction::new(
                SAVE_RETRY_ACTION,
                "rotate-cw",
                crate::i18n!("save-status-retry"),
            )
        };
        if let Some(reason) = &self.failure {
            let text = format!("{}: {reason}", crate::i18n!("save-status-failed"));
            return Some(ToolbarNotice::new(text, ToastKind::Error).action(retry()));
        }
        if self.gave_up() {
            return Some(
                ToolbarNotice::new(crate::i18n!("save-status-not-saved"), ToastKind::Error)
                    .action(retry()),
            );
        }
        self.passing
            .get()
            .map(|(text, kind)| ToolbarNotice::new(text.clone(), *kind))
    }

    /// Keep the window's bar saying the save's notice while there is one,
    /// and take it down once there is not; call on every sync. A notice
    /// shown since from elsewhere in the window stays.
    pub fn publish(&self) {
        let want = self.notice();
        let mut published = self.published.borrow_mut();
        if *published == want {
            return;
        }
        if let Some(gone) = published.take() {
            crate::window_toolbar::withdraw(&gone);
        }
        if let Some(notice) = want {
            crate::window_toolbar::notify(Some(notice.clone()));
            *published = Some(notice);
        }
    }

    /// When the surface next moves along by itself: a passing notice's
    /// end, or an unconfirmed save's next stage. A held notice waits for
    /// the person.
    #[must_use]
    pub fn next_wake_deadline(&self) -> Option<Instant> {
        let since = self.saving_since;
        let stage = match self.stage {
            _ if !self.waiting() => None,
            Stage::Quiet => since.and_then(|t| t.checked_add(SAVE_STALL)),
            Stage::Retrying => since.and_then(|t| t.checked_add(SAVE_GIVE_UP)),
            Stage::NotSaved => self
                .unseen_since
                .filter(|_| !self.notified)
                .and_then(|t| t.checked_add(UNSEEN_GRACE)),
        };
        match (self.passing.deadline(), stage) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

#[cfg(test)]
#[path = "save_status_tests.rs"]
mod tests;
