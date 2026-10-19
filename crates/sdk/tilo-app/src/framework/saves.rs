//! A window's save status, presented by the framework (HIG §7, "Status has
//! two homes").
//!
//! Saving is the system's job, so a framework app says nothing about it.
//! The root of every framework window holds one [`SaveStatusSurface`] over
//! its view and feeds it the durability the host reports for the window's
//! record; no app wires it. A save that lands, however slowly, shows nothing.
//! Only a save still unconfirmed after [`SAVE_STALL`](ui_core::SAVE_STALL)
//! ("Couldn't save yet. Retrying…", then "Not saved" with "Try again") or one
//! that failed (an error card with "Try again") appears, and the save that
//! ends either says "Saved" once.
//!
//! An app that also persists outside its record (a file it writes) reports
//! those saves through [`Saves`] ([`RecordEnv::saves`](super::RecordEnv::saves)),
//! and the same surface presents them. It never paints a save pill, a
//! "Saving…" line or a "Saved" toast of its own.

use std::cell::RefCell;
use std::rc::Rc;

use ui_core::Theme;
use ui_core::a11y::AccessNode;
use ui_core::record::RecordSaveStatus;
use ui_core::text_render::TextRenderer;
use ui_core::time::Instant;
use ui_core::vello::Scene;
use ui_core::vello::kurbo::{Affine, Point, Rect};
use ui_core::{SaveAction, SaveState, SaveStatusSurface};

/// The action target prefix of the surface's buttons for assistive
/// technology: `tilo.save-status.retry` and `tilo.save-status.dismiss`.
const TARGET: &str = "tilo.save-status";

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
    /// [`SaveStatusSurface`] it places (in its header row, beside the
    /// document's title) and feeds from
    /// [`RecordEnv::save_status`](super::RecordEnv::save_status); the
    /// framework then paints none over it. The view keeps the same rules:
    /// it says nothing for a save that lands.
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

/// The save status a framework root presents over its view: the record's
/// durability and the app's own [`Saves`], on one [`SaveStatusSurface`].
pub(crate) struct SaveIndicator {
    surface: SaveStatusSurface,
    /// What the host last reported of the record; `None` before it did
    /// (a past version never reports).
    record: Option<SaveState>,
    saves: Saves,
    /// The record's save is to be submitted again, not yet taken by the
    /// host.
    retry_record: bool,
    /// Where the surface last painted.
    drawn: Option<Rect>,
    /// What the surface shows changed since the last frame.
    dirty: bool,
    /// Where the pointer last was.
    pointer: Point,
}

impl SaveIndicator {
    pub(crate) fn new(saves: Saves) -> Self {
        Self {
            surface: SaveStatusSurface::new(),
            record: None,
            saves,
            retry_record: false,
            drawn: None,
            dirty: false,
            pointer: Point::ZERO,
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
            self.dirty |= self.surface.set(state, now);
        }
        self.dirty |= self.surface.advance(now);
        if self.surface.take_retry() {
            self.ask_retry();
        }
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
        self.drawn = None;
        self.dirty = true;
        self.follow(now);
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
        self.dirty = true;
    }

    /// Whether the record's save is to be submitted again, once.
    pub(crate) fn take_record_retry(&mut self) -> bool {
        std::mem::take(&mut self.retry_record)
    }

    /// Paint what the surface shows over `area`, the view's rect.
    pub(crate) fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        now: Instant,
    ) {
        if self.inert() {
            return;
        }
        self.follow(now);
        self.drawn = self
            .surface
            .paint(scene, transform, tr, theme, area, now)
            .map(|rect| ui_core::widget::floating_surface::painted_bounds(rect, theme));
    }

    /// The pointer moved to `p`; whether the hovered button changed.
    pub(crate) fn pointer_moved(&mut self, p: Point) -> bool {
        self.pointer = p;
        if self.inert() {
            return false;
        }
        let changed = self.surface.pointer_moved(p);
        self.dirty |= changed;
        changed
    }

    /// A press where the pointer is: whether it was the surface's.
    pub(crate) fn press(&mut self) -> bool {
        let p = self.pointer;
        if self.inert() || !self.surface.contains(p) {
            return false;
        }
        if let Some(action) = self.surface.click(p) {
            self.acted(action);
        }
        true
    }

    /// The surface for assistive technology, when it says anything.
    pub(crate) fn accessibility(&self) -> Option<AccessNode> {
        if self.inert() {
            return None;
        }
        self.surface.accessibility(TARGET)
    }

    /// Assistive technology activated `target`; whether it was the
    /// surface's.
    pub(crate) fn perform(&mut self, target: &str) -> bool {
        if self.inert() || !target.starts_with(TARGET) {
            return false;
        }
        let Some(action) = self.surface.perform(TARGET, target) else {
            return false;
        };
        self.acted(action);
        true
    }

    /// When the surface next moves along by itself.
    pub(crate) fn next_wake_deadline(&self) -> Option<Instant> {
        if self.inert() {
            return None;
        }
        self.surface.next_wake_deadline()
    }

    /// Whether the next frame shows something different.
    pub(crate) fn needs_redraw(&self, now: Instant) -> bool {
        if self.inert() {
            return false;
        }
        self.dirty
            || self.surface.needs_frame(now)
            || self
                .merged()
                .is_some_and(|state| self.surface.state() != Some(&state))
            || self.next_wake_deadline().is_some_and(|due| due <= now)
    }

    /// Whether something is showing at `now`.
    pub(crate) fn is_visible(&self, now: Instant) -> bool {
        !self.inert() && self.surface.is_visible(now)
    }

    /// Where the next frame differs: where the surface was while it only
    /// fades, and all of `area` when what it shows changed (its new place is
    /// known only once it paints).
    pub(crate) fn collect_damage(&self, area: Rect, now: Instant, regions: &mut Vec<Rect>) {
        if !self.needs_redraw(now) {
            return;
        }
        let fading = !self.dirty
            && self.surface.needs_frame(now)
            && self
                .merged()
                .is_none_or(|state| self.surface.state() == Some(&state));
        match self.drawn {
            Some(drawn) if fading => regions.push(drawn),
            drawn => {
                regions.extend(drawn);
                regions.push(area);
            }
        }
    }

    pub(crate) fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    /// What the bubble says, for a test.
    #[cfg(test)]
    pub(crate) fn bubble_text(&self) -> &str {
        self.surface.bubble_text()
    }

    /// The failure showing, for a test.
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
    }

    // USER REQUIREMENT (ledger, 2026-10-18): "Autosave is silent; show something only when a save stalls or fails"
    #[test]
    fn a_record_save_that_lands_says_nothing_however_slow() {
        init();
        let t0 = ui_core::time::now();
        let mut indicator = SaveIndicator::new(Saves::default());
        indicator.record(&RecordSaveStatus::Saving, t0);
        indicator.follow(at(t0, 9));
        assert_eq!(indicator.bubble_text(), "", "nothing before the stall");
        assert!(!indicator.is_visible(at(t0, 9)));
        assert!(
            indicator.accessibility().is_none(),
            "silent for assistive technology too"
        );
        indicator.record(&RecordSaveStatus::Saved, at(t0, 9));
        assert_eq!(
            indicator.bubble_text(),
            "",
            "no Saved for a save nobody waited on"
        );
        assert!(!indicator.is_visible(at(t0, 9)));
        assert!(!indicator.take_record_retry());
    }

    // USER REQUIREMENT (ledger, 2026-10-18): "Autosave is silent; show something only when a save stalls or fails"
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
        assert!(indicator.needs_redraw(stalled), "the due stall is a frame");
        indicator.follow(stalled);
        assert_eq!(indicator.bubble_text(), "Couldn't save yet. Retrying…");
        assert!(indicator.take_record_retry(), "the stall submits once more");
        assert!(!indicator.take_record_retry());
        indicator.record(&RecordSaveStatus::Saved, at(t0, 12));
        assert_eq!(
            indicator.bubble_text(),
            "Saved",
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
        assert_eq!(indicator.bubble_text(), "Saved");
        indicator.settle_for_capture(at(t0, 12));
        assert_eq!(indicator.bubble_text(), "", "a landed save says nothing");
        assert!(!indicator.is_visible(at(t0, 12)));
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

    // USER REQUIREMENT (ledger, 2026-10-18): "Autosave is silent; show something only when a save stalls or fails"
    #[test]
    fn a_failed_save_offers_try_again_which_reaches_the_record() {
        init();
        let t0 = ui_core::time::now();
        let mut indicator = SaveIndicator::new(Saves::default());
        indicator.record(&RecordSaveStatus::Failed("The disk is full".into()), t0);
        assert_eq!(indicator.failure(), Some("The disk is full"));
        let node = indicator.accessibility().expect("a failure is announced");
        let retry = node
            .children
            .iter()
            .find_map(|child| {
                child
                    .action_target
                    .clone()
                    .filter(|t| t == "tilo.save-status.retry")
            })
            .expect("a Try again button");
        assert!(indicator.perform(&retry));
        assert!(
            indicator.take_record_retry(),
            "Try again submits the record again"
        );
        indicator.record(&RecordSaveStatus::Saving, at(t0, 1));
        indicator.record(&RecordSaveStatus::Saved, at(t0, 2));
        assert_eq!(indicator.failure(), None);
        assert_eq!(
            indicator.bubble_text(),
            "Saved",
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
        indicator.follow(at(t0, 3));
        assert_eq!(indicator.bubble_text(), "");
        saves.failed("The folder is read-only");
        assert!(
            indicator.needs_redraw(at(t0, 4)),
            "a reported failure is a frame"
        );
        indicator.follow(at(t0, 4));
        assert_eq!(indicator.failure(), Some("The folder is read-only"));
        let node = indicator.accessibility().expect("announced");
        let retry = node
            .children
            .iter()
            .find_map(|child| {
                child
                    .action_target
                    .clone()
                    .filter(|t| t == "tilo.save-status.retry")
            })
            .expect("a Try again button");
        assert!(indicator.perform(&retry));
        assert!(saves.take_retry(), "the app is asked to save again");
        assert!(!saves.take_retry());
        assert!(
            !indicator.take_record_retry(),
            "the record was stored; only the app's save retries"
        );
        saves.saved();
        indicator.follow(at(t0, 5));
        assert_eq!(indicator.failure(), None);
    }

    #[test]
    fn a_view_that_shows_the_status_itself_gets_nothing_painted_over_it() {
        init();
        let t0 = ui_core::time::now();
        let saves = Saves::default();
        saves.shown_by_view();
        let mut indicator = SaveIndicator::new(saves);
        indicator.record(&RecordSaveStatus::Failed("The disk is full".into()), t0);
        assert!(!indicator.is_visible(t0));
        assert!(indicator.accessibility().is_none());
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

    /// The labels of every node about saving the window exports.
    fn save_labels(app: &mut TestApp<DraftApp>) -> Vec<String> {
        app.harness()
            .a11y_nodes()
            .into_iter()
            .filter_map(|(_, node)| node.label().map(str::to_owned))
            .filter(|label| {
                ["Saved", "Saving…", "Save failed", "Not saved", "Try again"]
                    .contains(&label.as_str())
                    || label.contains("save")
            })
            .collect()
    }

    /// What the window's save bubble says.
    fn bubble(app: &mut TestApp<DraftApp>) -> String {
        app.harness()
            .with_widget_mut(|root: &mut crate::framework::root::ResourceRoot| {
                crate::framework::root::RootModel::save_indicator(root)
                    .bubble_text()
                    .to_owned()
            })
            .unwrap()
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

    // USER REQUIREMENT (ledger, 2026-10-18): "Autosave is silent; show something only when a save stalls or fails"
    #[test]
    fn a_window_is_silent_until_a_save_stalls_then_offers_try_again_when_it_fails() {
        ui_core::i18n::I18nManager::init("en-US");
        let mut app = TestApp::<DraftApp>::new().unwrap();
        app.settle();
        assert!(
            save_labels(&mut app).is_empty(),
            "a stored record says nothing"
        );

        saves().saving();
        let t0 = ui_core::time::now();
        let after = |secs| t0.checked_add(Duration::from_secs(secs)).unwrap();
        frame_at(&mut app, t0);
        frame_at(&mut app, after(9));
        assert_eq!(bubble(&mut app), "", "nothing while a save may still land");
        assert!(save_labels(&mut app).is_empty(), "no Saving… for anyone");

        frame_at(&mut app, after(10));
        assert_eq!(
            bubble(&mut app),
            "Couldn't save yet. Retrying…",
            "the stall speaks at ten seconds"
        );
        assert!(saves().take_retry(), "and asks the app to save again");

        saves().failed("The disk is full");
        frame_at(&mut app, after(11));
        let labels = save_labels(&mut app);
        assert!(
            labels.contains(&"Save failed".to_owned()) && labels.contains(&"Try again".to_owned()),
            "a failure is announced with Try again: {labels:?}"
        );
        app.harness()
            .perform_semantic_named(
                "tilo.save-status.retry",
                None,
                &ui_core::a11y::Action::Activate,
            )
            .unwrap();
        assert!(saves().take_retry(), "Try again reaches the app");

        saves().saved();
        frame_at(&mut app, after(13));
        assert_eq!(
            bubble(&mut app),
            "Saved",
            "the save that ends a failure says so once"
        );
        assert!(
            !save_labels(&mut app).contains(&"Save failed".to_owned()),
            "the failure is gone"
        );
    }
}
