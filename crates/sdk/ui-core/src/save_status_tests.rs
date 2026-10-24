use super::*;
use crate::window_toolbar::{ToolbarEvent, deliver, notice, notify};

const DOC: Option<&str> = Some("Launch checklist");

fn init() {
    crate::i18n::I18nManager::init("en-US");
    notify(None);
}

/// What the window's bar says, and the button beside it.
fn bar() -> Option<(String, ToastKind, Option<String>)> {
    notice().map(|notice| {
        (
            notice.text,
            notice.kind,
            notice.action.map(|action| action.name),
        )
    })
}

/// "Try again" pressed in the window's bar.
fn press_try_again() {
    deliver(ToolbarEvent::Action {
        id: SAVE_RETRY_ACTION.to_owned(),
    });
}

/// A save that lands before it stalls says nothing, however slow it was;
/// one that stalls says "Retrying…" and asks for one more submission, and
/// its landing says "Saved" for a notice's lifetime.
#[test]
fn autosave_is_silent_until_a_save_stalls() {
    init();
    let t0 = Instant::now();
    let mut surface = SaveStatusSurface::new();
    surface.set(SaveState::Saving, t0);
    surface.advance(t0 + SAVE_STALL - Duration::from_millis(1));
    surface.publish();
    assert_eq!(bar(), None, "nothing before the stall");
    surface.set(SaveState::Saved, t0 + Duration::from_secs(9));
    surface.publish();
    assert_eq!(bar(), None, "no Saved for a save nobody waited on");
    assert!(!surface.take_retry());

    let t1 = t0 + Duration::from_secs(30);
    surface.set(SaveState::Saving, t1);
    assert_eq!(surface.next_wake_deadline(), Some(t1 + SAVE_STALL));
    surface.advance(t1 + SAVE_STALL);
    surface.publish();
    assert_eq!(
        bar(),
        Some((
            "Couldn't save yet. Retrying…".to_owned(),
            ToastKind::Warning,
            None
        ))
    );
    assert!(surface.take_retry(), "one more submission");
    assert!(!surface.take_retry(), "once");
    let landed = t1 + SAVE_STALL + Duration::from_secs(1);
    surface.set(SaveState::Saved, landed);
    surface.publish();
    assert_eq!(bar(), Some(("Saved".to_owned(), ToastKind::Success, None)));
    let gone = surface
        .next_wake_deadline()
        .expect("Saved leaves on its own");
    surface.advance(gone + Duration::from_millis(1));
    surface.publish();
    assert_eq!(bar(), None);
    assert_eq!(
        surface.next_wake_deadline(),
        None,
        "nothing left to wake for"
    );
}

/// A failed save is an error in the window's bar with "Try again" beside
/// it. It stays through the retry it asks for, and a save that succeeds
/// takes it down and says "Saved".
#[test]
fn a_failed_save_stays_in_the_bar_with_try_again_until_a_save_succeeds() {
    init();
    let t0 = Instant::now();
    let mut surface = SaveStatusSurface::new();
    surface.set(SaveState::Failed("Disk full".into()), t0);
    surface.publish();
    let held = Some((
        "Save failed: Disk full".to_owned(),
        ToastKind::Error,
        Some("Try again".to_owned()),
    ));
    assert_eq!(bar(), held);
    assert_eq!(
        surface.next_wake_deadline(),
        None,
        "a failure has no lifetime"
    );
    let much_later = t0 + Duration::from_secs(3600);
    surface.advance(much_later);
    surface.publish();
    assert_eq!(bar(), held, "it waits for the person");

    press_try_again();
    assert!(
        crate::window_toolbar::take_events().is_empty(),
        "the press is the save's, not among the app's slot events"
    );
    assert_eq!(surface.take_bar_action(), Some(SaveAction::Retry));
    assert_eq!(surface.take_bar_action(), None, "once");
    surface.set(SaveState::Saving, much_later);
    surface.advance(much_later + SAVE_GIVE_UP);
    surface.publish();
    assert_eq!(bar(), held, "the failure stands while the retry runs");

    surface.set(SaveState::Saved, much_later + SAVE_GIVE_UP);
    surface.publish();
    assert_eq!(
        bar(),
        Some(("Saved".to_owned(), ToastKind::Success, None)),
        "the save that ends the failure says so once"
    );
    assert_eq!(surface.failure(), None);
}

/// An unconfirmed save that gave up asks the person in place: "Not saved"
/// with "Try again", held until it is pressed, and pressing it waits
/// afresh, quietly at first.
#[test]
fn a_save_that_gives_up_asks_in_the_bar_and_try_again_waits_afresh() {
    init();
    let t0 = Instant::now();
    let mut surface = SaveStatusSurface::new();
    surface.set(SaveState::Saving, t0);
    surface.advance(t0 + SAVE_GIVE_UP);
    surface.seen();
    surface.publish();
    assert!(surface.gave_up());
    assert_eq!(
        bar(),
        Some((
            "Not saved".to_owned(),
            ToastKind::Error,
            Some("Try again".to_owned())
        ))
    );
    press_try_again();
    assert_eq!(surface.take_bar_action(), Some(SaveAction::Retry));
    assert!(!surface.gave_up(), "the wait starts over");
    surface.advance(t0 + SAVE_GIVE_UP + Duration::from_secs(1));
    surface.publish();
    assert_eq!(bar(), None, "quietly at first");
}

/// A press on a notice's button that is not the save's leaves the save
/// alone.
#[test]
fn only_the_saves_own_button_retries() {
    init();
    let mut surface = SaveStatusSurface::new();
    surface.set(SaveState::Saved, Instant::now());
    surface.publish();
    press_try_again();
    assert_eq!(
        surface.take_bar_action(),
        None,
        "nothing to try again: no notice offered the button"
    );
}

#[test]
fn a_save_that_gives_up_out_of_sight_raises_a_notification() {
    init();
    let t0 = Instant::now();
    let mut surface = SaveStatusSurface::new();
    surface.set(SaveState::Saving, t0);
    // Out of sight: only the host's tick runs, the window never draws.
    assert_eq!(surface.take_notice(t0 + SAVE_STALL, DOC, "Notes"), None);
    assert!(
        surface.take_retry(),
        "the retry is asked for out of sight too"
    );
    let given_up = t0 + SAVE_GIVE_UP;
    assert_eq!(
        surface.take_notice(given_up, DOC, "Notes"),
        None,
        "a frame may yet come"
    );
    assert_eq!(surface.next_wake_deadline(), Some(given_up + UNSEEN_GRACE));
    let raised = surface
        .take_notice(given_up + UNSEEN_GRACE, DOC, "Notes")
        .expect("nobody saw it: notify");
    assert_eq!(raised.title, "Launch checklist wasn't saved");
    assert_eq!(raised.body, "Your changes are still in Notes.");
    assert_eq!(raised.action, "Try again");
    assert_eq!(
        surface.take_notice(given_up + UNSEEN_GRACE * 3, DOC, "Notes"),
        None,
        "once"
    );
    assert_eq!(surface.next_wake_deadline(), None);

    surface.retry_from_notice();
    assert!(surface.take_withdrawal(), "trying again takes it down");
    assert!(!surface.gave_up(), "and waits afresh");

    let t1 = given_up + Duration::from_secs(5);
    surface.advance(t1);
    let given_up = t1 + SAVE_GIVE_UP;
    surface.take_notice(given_up, DOC, "Notes");
    surface.seen();
    assert_eq!(
        surface.take_notice(given_up + UNSEEN_GRACE, None, "Notes"),
        None,
        "the window was seen: the bar says it in place"
    );
    surface.set(SaveState::Saved, given_up + UNSEEN_GRACE * 2);
    assert!(!surface.take_withdrawal(), "nothing was raised");
}

/// The save's notice leaves the bar with the window's surface.
#[test]
fn the_notice_leaves_with_its_surface() {
    init();
    let mut surface = SaveStatusSurface::new();
    surface.set(SaveState::Failed("Disk full".into()), Instant::now());
    surface.publish();
    assert!(notice().is_some());
    drop(surface);
    assert_eq!(notice(), None);
}
