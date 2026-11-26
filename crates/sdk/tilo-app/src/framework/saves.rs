//! A window's save status, presented by the framework (HIG §7, "Status has
//! two homes").
//!
//! Saving is the system's job, so a framework app says nothing about it.
//! The root of every framework window holds one [`SaveStatusSurface`] and
//! feeds it the durability the host reports for the window's record; no
//! app wires it. A save that lands, however slowly, says nothing. Only a
//! save still unconfirmed after [`SAVE_STALL`](ui_core::SAVE_STALL)
//! ("Couldn't save yet. Retrying…", then "Not saved" with "Try again") or
//! one that failed (an error with "Try again", held until a save succeeds)
//! is said, in the window's bar, and the save that ends either says
//! "Saved" once.
//!
//! An app that also persists outside its record (a file it writes) reports
//! those saves through [`Saves`] ([`ResourceEnv::saves`](super::ResourceEnv::saves)),
//! and the same surface says them. It never shows a save pill, a "Saving…"
//! line or a "Saved" notice of its own.

use std::cell::RefCell;
use std::rc::Rc;

use ui_core::resource::ResourceSaveStatus;
use ui_core::time::Instant;
use ui_core::{SaveAction, SaveState, SaveStatusSurface};

/// Saves an app makes outside its record, for the window's save status.
///
/// The framework already presents the record's own durability; this is for
/// what it cannot see, such as a file the app writes. Report each save's
/// state as it changes ([`Self::saving`], [`Self::saved`], [`Self::failed`]);
/// the window says nothing unless the save stalls or fails, and then offers
/// "Try again", which the app reads with [`Self::take_retry`] and answers by
/// saving again. Clones share one report.
#[derive(Clone, Default)]
pub struct Saves(Rc<RefCell<Reported>>);

#[derive(Default)]
struct Reported {
    /// The last state reported; `None` before any.
    state: Option<SaveState>,
    /// The person asked to try the app's failed save again.
    retry: bool,
}

impl Saves {
    /// A save started and is not stored yet.
    pub fn saving(&self) {
        self.report(SaveState::Saving);
    }

    /// The save landed.
    pub fn saved(&self) {
        self.report(SaveState::Saved);
    }

    /// The save failed; `reason` says why, in the person's words.
    pub fn failed(&self, reason: impl Into<String>) {
        self.report(SaveState::Failed(reason.into()));
    }

    /// Report a save's state.
    pub fn report(&self, state: SaveState) {
        self.0.borrow_mut().state = Some(state);
    }

    /// Whether the person asked to try the app's stalled or failed save
    /// again, once. The app saves again and reports it.
    pub fn take_retry(&self) -> bool {
        std::mem::take(&mut self.0.borrow_mut().retry)
    }

    /// The state last reported, `None` before any: for automation and
    /// tests, never for a "Saving…" or "Saved" of the app's own.
    pub fn state(&self) -> Option<SaveState> {
        self.0.borrow().state.clone()
    }

    fn ask_retry(&self) {
        self.0.borrow_mut().retry = true;
    }
}

/// How bad a save state is, to show the worse of two.
fn severity(state: &SaveState) -> u8 {
    match state {
        SaveState::Idle => 0,
        SaveState::Saved => 1,
        SaveState::Saving => 2,
        SaveState::Failed(_) => 3,
    }
}

/// Whether a save in `state` is one "Try again" would submit again.
fn unsettled(state: &SaveState) -> bool {
    matches!(state, SaveState::Saving | SaveState::Failed(_))
}

/// The save status a framework root says in the window's bar: the
/// record's durability and the app's own [`Saves`], on one
/// [`SaveStatusSurface`].
pub(crate) struct SaveIndicator {
    surface: SaveStatusSurface,
    /// What the host last reported of the record; `None` before it did
    /// (a past version never reports).
    record: Option<SaveState>,
    saves: Saves,
    /// The record's save is to be submitted again, not yet taken by the
    /// host.
    retry_resource: bool,
    /// The notification raised for a save that gave up out of sight.
    notices: super::save_notice::SaveNotices,
    /// The app's name, which that notification says the changes are in.
    app: &'static str,
}

impl SaveIndicator {
    pub(crate) fn new(saves: Saves, app: &'static str) -> Self {
        Self {
            surface: SaveStatusSurface::new(),
            record: None,
            saves,
            retry_resource: false,
            notices: super::save_notice::SaveNotices::new(),
            app,
        }
    }

    /// The host reported the record's durability.
    /// Said in the window's bar at once: a save lands or fails outside any
    /// sync of the window, and the window shows it without one.
    /// Returns whether the window shows anything new of it: its bar's notice or the unsaved
    /// mark changed. A save that lands within the stall window shows nothing.
    pub(crate) fn record(&mut self, status: &ResourceSaveStatus, now: Instant) -> bool {
        let before = (self.surface.notice(), self.surface.shows_unsaved());
        self.record = Some(match status {
            // What it means for the person, not the storage error behind it
            // (a path, an errno), which goes to the log.
            ResourceSaveStatus::Failed(reason) => {
                tracing::warn!(%reason, "the window's record failed to save");
                SaveState::Failed(ui_core::i18n::widget_text("save-status-failed-detail"))
            }
            status => SaveState::from(status),
        });
        self.follow(now);
        self.present();
        before != (self.surface.notice(), self.surface.shows_unsaved())
    }

    /// Keep the window's bar saying the save: its notice, and the mark on a
    /// document whose work is not yet saved.
    fn present(&self) {
        self.surface.publish();
        ui_core::window_toolbar::show_edited(self.surface.shows_unsaved());
    }

    /// The worse of the record's state and the app's own.
    fn merged(&self) -> Option<SaveState> {
        match (self.record.clone(), self.saves.state()) {
            (Some(record), Some(app)) if severity(&app) > severity(&record) => Some(app),
            (Some(record), _) => Some(record),
            (None, app) => app,
        }
    }

    /// Bring the surface up to date at `now`: the merged state, the stage
    /// of a save that waits, and a submission the surface asked for.
    pub(crate) fn follow(&mut self, now: Instant) {
        if let Some(state) = self.merged() {
            self.surface.set(state, now);
        }
        self.surface.advance(now);
        if self.surface.take_retry() {
            self.ask_retry();
        }
    }

    /// On every sync of the window: run "Try again" pressed in the window's
    /// bar, bring the surface up to `now`, and keep the bar saying it.
    pub(crate) fn sync(&mut self, now: Instant) {
        if let Some(action) = self.surface.take_bar_action() {
            self.acted(action);
        }
        self.follow(now);
        // Out of sight, a save that gave up is said by a notification,
        // named by the document the window's bar names.
        let title = ui_core::window_toolbar::declared()
            .and_then(|bar| bar.location)
            .map(|location| location.label);
        if self
            .notices
            .deliver(&mut self.surface, title.as_deref(), self.app, now)
        {
            self.ask_retry();
        }
        self.present();
    }

    /// The window drew a frame: the person can see what its bar says.
    pub(crate) fn seen(&mut self) {
        self.surface.seen();
    }

    /// Present the save state as it stands at `now`, forgetting how long it
    /// took to get there. Staging a capture spends wall time no person
    /// waited through (decodes, page renders, a loaded machine), and a save
    /// that waited past [`SAVE_STALL`](ui_core::SAVE_STALL) in it would put
    /// "Retrying…", or the "Saved" that confirms a noticed wait, into the
    /// frame — or not, depending on how fast the machine was. A save that
    /// landed says nothing, as one that landed at once; a failure still
    /// shows; a save still in flight starts its wait now.
    pub(crate) fn settle_for_capture(&mut self, now: Instant) {
        self.surface = SaveStatusSurface::new();
        self.retry_resource = false;
        self.follow(now);
        self.present();
    }

    /// Submit again whichever save is unsettled: the record's, the app's,
    /// or both.
    fn ask_retry(&mut self) {
        if self.record.as_ref().is_some_and(unsettled) {
            self.retry_resource = true;
        }
        if self.saves.state().as_ref().is_some_and(unsettled) {
            self.saves.ask_retry();
        }
    }

    fn acted(&mut self, action: SaveAction) {
        if action == SaveAction::Retry {
            self.ask_retry();
        }
    }

    /// Whether the record's save is to be submitted again, once.
    pub(crate) fn take_resource_retry(&mut self) -> bool {
        std::mem::take(&mut self.retry_resource)
    }

    /// When the surface next moves along by itself.
    pub(crate) fn next_wake_deadline(&self) -> Option<Instant> {
        self.surface.next_wake_deadline()
    }

    /// Whether the window needs a sync for the bar to say what changed.
    pub(crate) fn needs_redraw(&self, now: Instant) -> bool {
        self.merged()
            .is_some_and(|state| self.surface.state() != Some(&state))
            || self.surface.needs_publish()
            || self.next_wake_deadline().is_some_and(|due| due <= now)
    }
}
