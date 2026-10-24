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
//! those saves through [`Saves`] ([`RecordEnv::saves`](super::RecordEnv::saves)),
//! and the same surface says them. It never shows a save pill, a "Saving…"
//! line or a "Saved" notice of its own.

use std::cell::RefCell;
use std::rc::Rc;

use ui_core::record::RecordSaveStatus;
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
    /// The view presents the save status on a surface of its own.
    shown_by_view: bool,
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

    /// The view presents the window's save status itself, on a
    /// [`SaveStatusSurface`] of its own it feeds from
    /// [`RecordEnv::save_status`](super::RecordEnv::save_status); the
    /// framework then says none. The view keeps the same rules: it says
    /// nothing for a save that lands.
    pub fn shown_by_view(&self) {
        self.0.borrow_mut().shown_by_view = true;
    }

    /// The state last reported, `None` before any: for automation and
    /// tests, never for a "Saving…" or "Saved" of the app's own.
    pub fn state(&self) -> Option<SaveState> {
        self.0.borrow().state.clone()
    }

    fn is_shown_by_view(&self) -> bool {
        self.0.borrow().shown_by_view
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
    retry_record: bool,
}

impl SaveIndicator {
    pub(crate) fn new(saves: Saves) -> Self {
        Self {
            surface: SaveStatusSurface::new(),
            record: None,
            saves,
            retry_record: false,
        }
    }

    /// The view presents the status itself ([`Saves::shown_by_view`]).
    fn inert(&self) -> bool {
        self.saves.is_shown_by_view()
    }

    /// The host reported the record's durability.
    pub(crate) fn record(&mut self, status: &RecordSaveStatus, now: Instant) {
        self.record = Some(SaveState::from(status));
        self.follow(now);
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
        if self.inert() {
            return;
        }
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
        if self.inert() {
            return;
        }
        if let Some(action) = self.surface.take_bar_action() {
            self.acted(action);
        }
        self.follow(now);
        self.surface.publish();
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
        self.retry_record = false;
        self.follow(now);
        if !self.inert() {
            self.surface.publish();
        }
    }

    /// Submit again whichever save is unsettled: the record's, the app's,
    /// or both.
    fn ask_retry(&mut self) {
        if self.record.as_ref().is_some_and(unsettled) {
            self.retry_record = true;
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
    pub(crate) fn take_record_retry(&mut self) -> bool {
        std::mem::take(&mut self.retry_record)
    }

    /// When the surface next moves along by itself.
    pub(crate) fn next_wake_deadline(&self) -> Option<Instant> {
        if self.inert() {
            return None;
        }
        self.surface.next_wake_deadline()
    }

    /// Whether the window needs a sync for the bar to say what changed.
    pub(crate) fn needs_redraw(&self, now: Instant) -> bool {
        if self.inert() {
            return false;
        }
        self.merged()
            .is_some_and(|state| self.surface.state() != Some(&state))
            || self.next_wake_deadline().is_some_and(|due| due <= now)
    }

    /// What the window's bar says of the save, for a test.
    #[cfg(test)]
    pub(crate) fn said(&self) -> Option<String> {
        self.surface.notice().map(|notice| notice.text)
    }

    /// The failure that stands, for a test.
    #[cfg(test)]
    pub(crate) fn failure(&self) -> Option<&str> {
        self.surface.failure()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui_core::SAVE_STALL;
    use ui_core::time::Duration;

    fn at(t0: Instant, secs: u64) -> Instant {
        t0.checked_add(Duration::from_secs(secs)).unwrap()
    }

    fn init() {
        ui_core::i18n::I18nManager::init("en-US");
        ui_core::window_toolbar::notify(None);
    }

    #[test]
    fn a_record_save_that_lands_says_nothing_however_slow() {
        init();
        let t0 = ui_core::time::now();
        let mut indicator = SaveIndicator::new(Saves::default());
        indicator.record(&RecordSaveStatus::Saving, t0);
        indicator.sync(at(t0, 9));
        assert_eq!(indicator.said(), None, "nothing before the stall");
        assert_eq!(ui_core::window_toolbar::notice(), None, "the bar is quiet");
        indicator.record(&RecordSaveStatus::Saved, at(t0, 9));
        assert_eq!(
            indicator.said(),
            None,
            "no Saved for a save nobody waited on"
        );
        assert!(!indicator.take_record_retry());
    }

    #[test]
    fn a_stalled_record_save_speaks_at_the_stall_and_retries_once() {
        init();
        let t0 = ui_core::time::now();
        let mut indicator = SaveIndicator::new(Saves::default());
        indicator.record(&RecordSaveStatus::Saving, t0);
        assert_eq!(
            indicator.next_wake_deadline(),
            Some(at(t0, SAVE_STALL.as_secs())),
            "the window wakes at the stall, not before"
        );
        let stalled = at(t0, SAVE_STALL.as_secs());
        assert!(indicator.needs_redraw(stalled), "the due stall is a sync");
        indicator.sync(stalled);
        assert_eq!(
            ui_core::window_toolbar::notice()
                .map(|notice| notice.text)
                .as_deref(),
            Some("Couldn't save yet. Retrying…"),
            "the window's bar says it"
        );
        assert!(indicator.take_record_retry(), "the stall submits once more");
        assert!(!indicator.take_record_retry());
        indicator.record(&RecordSaveStatus::Saved, at(t0, 12));
        assert_eq!(
            indicator.said().as_deref(),
            Some("Saved"),
            "the waited-on save ends with Saved"
        );
    }

    /// A capture's staging can outlast the stall on a slow machine; the
    /// frame it takes shows the record's state, never a stall or the
    /// "Saved" that confirms one, so two captures of one scenario agree.
    #[test]
    fn a_capture_shows_the_save_state_not_how_long_staging_took() {
        init();
        let t0 = ui_core::time::now();
        let mut indicator = SaveIndicator::new(Saves::default());
        indicator.record(&RecordSaveStatus::Saving, t0);
        indicator.follow(at(t0, SAVE_STALL.as_secs()));
        indicator.record(&RecordSaveStatus::Saved, at(t0, 12));
        assert_eq!(indicator.said().as_deref(), Some("Saved"));
        indicator.settle_for_capture(at(t0, 12));
        assert_eq!(indicator.said(), None, "a landed save says nothing");
        assert!(!indicator.take_record_retry(), "the stall's retry is gone");

        let mut failed = SaveIndicator::new(Saves::default());
        failed.record(&RecordSaveStatus::Failed("The disk is full".into()), t0);
        failed.settle_for_capture(at(t0, 30));
        assert_eq!(
            failed.failure(),
            Some("The disk is full"),
            "a failure still shows"
        );
    }

    /// "Try again" pressed in the window's bar.
    fn press_try_again() {
        ui_core::window_toolbar::deliver(ui_core::window_toolbar::ToolbarEvent::Action {
            id: ui_core::SAVE_RETRY_ACTION.to_owned(),
        });
    }

    #[test]
    fn a_failed_save_holds_try_again_in_the_bar_which_reaches_the_record() {
        init();
        let t0 = ui_core::time::now();
        let mut indicator = SaveIndicator::new(Saves::default());
        indicator.record(&RecordSaveStatus::Failed("The disk is full".into()), t0);
        indicator.sync(t0);
        assert_eq!(indicator.failure(), Some("The disk is full"));
        let held = ui_core::window_toolbar::notice().expect("the bar says the failure");
        assert_eq!(held.text, "Save failed: The disk is full");
        assert_eq!(
            held.action.as_ref().map(|action| action.name.as_str()),
            Some("Try again")
        );
        indicator.sync(at(t0, 600));
        assert_eq!(
            ui_core::window_toolbar::notice().as_ref(),
            Some(&held),
            "held for as long as the failure stands"
        );
        press_try_again();
        indicator.sync(at(t0, 601));
        assert!(
            indicator.take_record_retry(),
            "Try again submits the record again"
        );
        indicator.record(&RecordSaveStatus::Saving, at(t0, 602));
        indicator.sync(at(t0, 602));
        assert_eq!(
            ui_core::window_toolbar::notice().as_ref(),
            Some(&held),
            "it stands through the retry"
        );
        indicator.record(&RecordSaveStatus::Saved, at(t0, 603));
        indicator.sync(at(t0, 603));
        assert_eq!(indicator.failure(), None);
        assert_eq!(
            ui_core::window_toolbar::notice()
                .map(|notice| notice.text)
                .as_deref(),
            Some("Saved"),
            "the save that ends a failure says so once"
        );
    }

    #[test]
    fn an_apps_own_save_shows_through_the_same_surface() {
        init();
        let t0 = ui_core::time::now();
        let saves = Saves::default();
        let mut indicator = SaveIndicator::new(saves.clone());
        indicator.record(&RecordSaveStatus::Saved, t0);
        saves.saving();
        indicator.sync(at(t0, 3));
        assert_eq!(indicator.said(), None);
        saves.failed("The folder is read-only");
        assert!(
            indicator.needs_redraw(at(t0, 4)),
            "a reported failure is a sync"
        );
        indicator.sync(at(t0, 4));
        assert_eq!(indicator.failure(), Some("The folder is read-only"));
        press_try_again();
        indicator.sync(at(t0, 4));
        assert!(saves.take_retry(), "the app is asked to save again");
        assert!(!saves.take_retry());
        assert!(
            !indicator.take_record_retry(),
            "the record was stored; only the app's save retries"
        );
        saves.saved();
        indicator.sync(at(t0, 5));
        assert_eq!(indicator.failure(), None);
    }

    #[test]
    fn a_view_that_shows_the_status_itself_gets_nothing_said_for_it() {
        init();
        let t0 = ui_core::time::now();
        let saves = Saves::default();
        saves.shown_by_view();
        let mut indicator = SaveIndicator::new(saves);
        indicator.record(&RecordSaveStatus::Failed("The disk is full".into()), t0);
        indicator.sync(t0);
        assert_eq!(indicator.said(), None);
        assert_eq!(ui_core::window_toolbar::notice(), None);
        assert_eq!(indicator.next_wake_deadline(), None);
    }
}

/// The save status as a framework window presents it, with no wiring in
/// the app: its view only reports a file save through [`Saves`].
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod window_tests {
    use super::Saves;
    use crate::prelude::*;
    use std::cell::RefCell;
    use ui_core::time::{Duration, Instant};

    thread_local! {
        static SAVES: RefCell<Option<Saves>> = const { RefCell::new(None) };
    }

    #[derive(Model, Default)]
    pub struct Draft {
        pub text: String,
    }

    #[resource(kind = "test.save-status-draft", label = "Draft")]
    impl Draft {
        /// Set the text.
        #[action]
        pub fn write(&mut self, text: String) {
            self.text = text;
        }
    }

    struct DraftApp;

    impl ResourceApp for DraftApp {
        type Resource = Draft;
        const ID: &'static str = "so.tilo.test-save-status";
        const TITLE: &'static str = "Draft";

        fn view(cx: &mut ViewCx<Draft>) -> Element {
            SAVES.with_borrow_mut(|saves| *saves = Some(cx.saves()));
            Box::new(Column::new().push(Box::new(Text::new("A draft"))))
        }
    }

    fn saves() -> Saves {
        SAVES.with_borrow(|saves| saves.clone().unwrap())
    }

    /// What the window's bar says, and the button beside it.
    fn bar() -> Option<(String, Option<String>)> {
        ui_core::window_toolbar::notice()
            .map(|notice| (notice.text, notice.action.map(|action| action.name)))
    }

    /// Bring the window's save status to `at`, as its wake there would,
    /// then run a frame.
    fn frame_at(app: &mut TestApp<DraftApp>, at: Instant) {
        app.harness()
            .with_widget_mut(|root: &mut crate::framework::root::ResourceRoot| {
                crate::framework::root::RootModel::save_indicator_mut(root).follow(at);
            })
            .unwrap();
        app.settle();
    }

    #[test]
    fn a_window_is_silent_until_a_save_stalls_then_offers_try_again_when_it_fails() {
        ui_core::i18n::I18nManager::init("en-US");
        let mut app = TestApp::<DraftApp>::new().unwrap();
        app.settle();
        assert_eq!(bar(), None, "a stored record says nothing");

        saves().saving();
        let t0 = ui_core::time::now();
        let after = |secs| t0.checked_add(Duration::from_secs(secs)).unwrap();
        frame_at(&mut app, t0);
        frame_at(&mut app, after(9));
        assert_eq!(bar(), None, "nothing while a save may still land");

        frame_at(&mut app, after(10));
        assert_eq!(
            bar(),
            Some(("Couldn't save yet. Retrying…".to_owned(), None)),
            "the stall speaks at ten seconds"
        );
        assert!(saves().take_retry(), "and asks the app to save again");

        saves().failed("The disk is full");
        frame_at(&mut app, after(11));
        let held = Some((
            "Save failed: The disk is full".to_owned(),
            Some("Try again".to_owned()),
        ));
        assert_eq!(bar(), held, "a failure is said with Try again");
        frame_at(&mut app, after(600));
        assert_eq!(bar(), held, "and held while it stands");
        app.harness()
            .press_window_slot(ui_core::window_toolbar::ToolbarEvent::Action {
                id: ui_core::SAVE_RETRY_ACTION.to_owned(),
            });
        app.settle();
        assert!(saves().take_retry(), "Try again reaches the app");

        saves().saved();
        frame_at(&mut app, after(601));
        assert_eq!(
            bar(),
            Some(("Saved".to_owned(), None)),
            "the save that ends a failure says so once; the failure is gone"
        );
    }
}
