//! A person goes through setup as setup's app draws it, in a real session: each step's title
//! shows, and each press lands on the control the person reads, by the words it shows them, a key
//! or the pointer.

#![cfg(target_os = "linux")]

use std::time::Duration;

use tilo_compositor::OutputConfig;
use tilo_onboarding_session::ceremony::Ceremony;
use tilo_onboarding_session::identity::MintedIdentity;
use tilo_session::{BeforeShell, Options, Session};
use ui::window_targets::TargetAction;

use crate::common::{KEY_ESCAPE, click, headless, key, type_text, until};

const INTRO: &str = "Set up your Tilo identity";
const EXISTING: &str = "Use your existing identity";
const PROTECT: &str = "Protect your Tilo identity";
const CREATE_NEW: &str = "Create a new identity";
const USE_EXISTING: &str = "Use my existing Tilo identity";
const UNDERSTOOD: &str =
    "I understand that losing this device without a backup means losing this identity.";
const CREATE: &str = "Create local identity";
const SKIP_UNDERSTOOD: &str = "I understand that without a recovery key or backup, losing this device means losing this identity.";
const CONTINUE_WITHOUT: &str = "Continue without backup";
const BACKUP_STEP: &str = "Step 3 of 4";
const WELCOME: &str = "Welcome to Tilo";
const FINISH: &str = "Continue to desktop";

/// Setup at its first page, whose identity is made in memory rather than on disk.
fn ceremony() -> (Ceremony, tilo_test_env::TestHome) {
    let home = tilo_test_env::TestHome::new().expect("a home");
    let record = home.config().join("tilo/identity/primary.json");
    let ceremony = Ceremony::with_minter(Box::new(move || {
        Ok(MintedIdentity {
            fingerprint: "ab".repeat(32),
            path: record.clone(),
        })
    }));
    (ceremony, home)
}

/// A session standing setup's `ceremony` in front of the shell.
fn session_with(ceremony: Ceremony) -> Session {
    let mut session = headless(Options {
        setup: Some(BeforeShell::Ceremony(Box::new(ceremony))),
        collab: false,
        ..Options::default()
    });
    session.step(Duration::from_millis(1)).expect("a frame");
    assert!(session.setup_up(), "setup stands in front of the shell");
    session
}

fn says(session: &Session, words: &str) -> bool {
    session
        .accessible_labels()
        .iter()
        .any(|label| label.contains(words))
}

fn offers(session: &Session, name: &str) -> bool {
    session
        .controls()
        .iter()
        .any(|control| control.name == name)
}

/// Wait until setup shows `title`.
#[track_caller]
fn shows(session: &mut Session, title: &str) {
    until(session, &format!("setup shows {title:?}"), |session| {
        says(session, title)
    });
}

/// Wait until setup no longer shows `title`.
#[track_caller]
fn leaves(session: &mut Session, title: &str) {
    until(session, &format!("setup leaves {title:?}"), |session| {
        !says(session, title)
    });
}

/// Run a few frames, for a press that should change nothing to have had its chance to.
fn settle(session: &mut Session) {
    for _ in 0..5 {
        session.step(Duration::from_millis(1)).expect("a frame");
    }
}

/// Press the control the person reads as `name`, once setup shows it.
#[track_caller]
fn press(session: &mut Session, name: &str) {
    until(session, &format!("setup offers {name:?}"), |session| {
        offers(session, name)
    });
    session
        .press_action(name, TargetAction::Activate)
        .unwrap_or_else(|why| panic!("pressing {name:?}: {why}"));
    session.step(Duration::from_millis(1)).expect("a frame");
}

/// Assert the control the person reads as `name` stands dimmed: a press of it is refused.
#[track_caller]
fn dimmed(session: &mut Session, name: &str) {
    until(session, &format!("setup offers {name:?}"), |session| {
        offers(session, name)
    });
    assert!(
        session.press_action(name, TargetAction::Activate).is_err(),
        "{name:?} cannot be pressed yet"
    );
    settle(session);
}

/// Where the control `name` stands on screen.
#[track_caller]
fn rect_of(session: &mut Session, name: &str) -> (f64, f64, f64, f64) {
    until(session, &format!("setup offers {name:?}"), |session| {
        offers(session, name)
    });
    session
        .controls()
        .into_iter()
        .find(|control| control.name == name)
        .map(|control| control.rect)
        .expect("the control just listed")
}

/// From the first page to the backup step: create an identity, having accepted that it is the
/// person's to protect.
fn to_backup(session: &mut Session) {
    shows(session, INTRO);
    press(session, CREATE_NEW);
    shows(session, PROTECT);
    press(session, UNDERSTOOD);
    press(session, CREATE);
    shows(session, BACKUP_STEP);
}

/// From the first page to the desktop: create an identity, accept that it is the person's to
/// protect, skip the backup once its consequence is acknowledged, and enter the desktop.
#[test]
fn a_person_creates_an_identity_and_reaches_the_desktop_by_what_setup_shows() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (ceremony, _home) = ceremony();
    let mut session = session_with(ceremony);
    to_backup(&mut session);
    press(&mut session, SKIP_UNDERSTOOD);
    press(&mut session, CONTINUE_WITHOUT);
    shows(&mut session, WELCOME);
    press(&mut session, FINISH);
    until(&mut session, "setup gives the desktop back", |session| {
        !session.setup_up()
    });
}

/// Creating waits for the acknowledgement, and a click on the acknowledgement's words, not only
/// on its box, is what ticks it.
#[test]
fn creating_waits_for_the_acknowledgement_its_words_tick() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (ceremony, _home) = ceremony();
    let mut session = session_with(ceremony);
    shows(&mut session, INTRO);
    press(&mut session, CREATE_NEW);
    shows(&mut session, PROTECT);

    dimmed(&mut session, CREATE);
    assert!(
        says(&session, PROTECT),
        "nothing is created before the person acknowledged it"
    );

    // The pointer lands on the sentence, well past the box at its leading edge.
    let (x, y, width, height) = rect_of(&mut session, UNDERSTOOD);
    click(&mut session, (x + width * 0.75, y + height / 2.0));
    settle(&mut session);
    press(&mut session, CREATE);
    shows(&mut session, BACKUP_STEP);
}

/// Continuing without a backup waits for its acknowledgement, then moves on.
#[test]
fn continuing_without_a_backup_waits_for_its_acknowledgement() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (ceremony, _home) = ceremony();
    let mut session = session_with(ceremony);
    to_backup(&mut session);

    dimmed(&mut session, CONTINUE_WITHOUT);
    assert!(
        says(&session, BACKUP_STEP) && !says(&session, WELCOME),
        "the backup step holds until its consequence is acknowledged"
    );

    press(&mut session, SKIP_UNDERSTOOD);
    press(&mut session, CONTINUE_WITHOUT);
    shows(&mut session, WELCOME);
}

/// Escape goes back where the step can still go back (the ways to bring an existing identity, and
/// the page saying nobody can recover it), and does nothing where there is no way back.
#[test]
fn escape_goes_back_only_where_setup_can_go_back() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (ceremony, _home) = ceremony();
    let mut session = session_with(ceremony);
    shows(&mut session, INTRO);

    key(&mut session, KEY_ESCAPE);
    settle(&mut session);
    assert!(
        offers(&session, CREATE_NEW),
        "the first page has nothing to go back to"
    );

    press(&mut session, USE_EXISTING);
    shows(&mut session, EXISTING);
    for (way, title) in [
        (
            "Use a code from another device",
            "Add this computer with a code",
        ),
        (
            "Enter your recovery key",
            "Type or paste the 24 words in order.",
        ),
        (
            "Restore from a backup",
            "Choose the encrypted backup you saved from Tilo",
        ),
    ] {
        press(&mut session, way);
        shows(&mut session, title);
        key(&mut session, KEY_ESCAPE);
        shows(&mut session, EXISTING);
        leaves(&mut session, title);
    }
    key(&mut session, KEY_ESCAPE);
    shows(&mut session, INTRO);
    leaves(&mut session, EXISTING);

    press(&mut session, CREATE_NEW);
    shows(&mut session, PROTECT);
    key(&mut session, KEY_ESCAPE);
    shows(&mut session, INTRO);
    leaves(&mut session, PROTECT);

    to_backup(&mut session);
    key(&mut session, KEY_ESCAPE);
    settle(&mut session);
    assert!(
        offers(&session, CONTINUE_WITHOUT),
        "the identity exists: Escape does not take the backup step away"
    );
}

/// Every page of the tour offers Skip tour, and Escape on any of them skips it and gives the
/// desktop back.
#[test]
fn escape_skips_the_tour() {
    let _settled = tilo_ui_base::motion::testing::settled();
    for pages_on in 0..=3 {
        let mut session = session_with(Ceremony::new_tour());
        shows(&mut session, "Everything is close at hand");
        for _ in 0..pages_on {
            assert!(
                offers(&session, "Skip tour"),
                "every tour page offers Skip tour"
            );
            press(&mut session, "Continue");
        }
        assert!(
            offers(&session, "Skip tour"),
            "every tour page offers Skip tour"
        );
        key(&mut session, KEY_ESCAPE);
        until(
            &mut session,
            &format!("the tour is skipped from page {}", pages_on + 1),
            |session| !session.setup_up(),
        );
    }
}

/// Whether a field of setup's holds `text`, as a screen reader reads its value.
fn field_holds(session: &Session, text: &str) -> bool {
    session.a11y_windows().iter().any(|(_, _, tree)| {
        tree.nodes
            .iter()
            .any(|(_, node)| node.value().is_some_and(|value| value == text))
    })
}

/// A backup's path typed by hand is setup's to restore from, as one picked in the picker is: the
/// restore waits for it, the field keeps what the person typed, and a file that is not there is
/// named as the reason it could not be connected.
#[test]
fn a_backup_path_typed_by_hand_is_restored_from() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (ceremony, home) = ceremony();
    let mut session = session_with(ceremony);
    press(&mut session, USE_EXISTING);
    press(&mut session, "Restore from a backup");
    shows(
        &mut session,
        "Choose the encrypted backup you saved from Tilo",
    );
    dimmed(&mut session, "Restore identity");

    // The path field stands just before Browse on its row.
    let (x, y, _, h) = rect_of(&mut session, "Browse");
    click(&mut session, (x - 24.0, y + h / 2.0));
    let typed = home.config().join("no-such-backup.tilo-identity");
    let typed = typed.to_str().expect("a test path is UTF-8");
    type_text(&mut session, typed);
    until(&mut session, "the field holds the typed path", |session| {
        field_holds(session, typed)
    });

    press(&mut session, "Restore identity");
    shows(&mut session, "That identity could not be connected");
    assert!(
        field_holds(&session, typed),
        "what the person typed stays in the field after setup answered it"
    );
}

/// A code that cannot be redeemed here leads the nearby page with why, and Join stays offered.
#[test]
fn the_nearby_page_says_why_a_join_did_not_start() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (ceremony, _home) = ceremony();
    let mut session = session_with(ceremony);
    press(&mut session, USE_EXISTING);
    press(&mut session, "Use a code from another device");
    shows(&mut session, "Add this computer with a code");
    press(&mut session, "Join");
    // This setup holds no device identity to join with: the banner names that cause.
    shows(&mut session, "Adding a computer is unavailable here");
    assert!(offers(&session, "Join"), "Join is offered again");
    assert!(!offers(&session, "Cancel"), "no join runs to cancel");
}

/// Large text on a small display.
fn large_text() -> ui::application::ComponentRunnerInputs {
    let mut inputs = ui::headless::fixture_runner_inputs();
    inputs.theme = inputs.theme.with_text_scale(1.5);
    inputs.text_renderer.set_text_scale(1.5);
    inputs
}

/// Assert the control `name` stands whole on an 800 by 600 display.
#[track_caller]
fn on_small_display(session: &mut Session, name: &str) {
    let (x, y, width, height) = rect_of(session, name);
    assert!(
        x >= 0.0 && y >= 0.0 && x + width <= 800.0 && y + height <= 600.0 && width > 0.0,
        "{name:?} stands whole on the display: {:?}",
        (x, y, width, height)
    );
}

/// On a small display with large text, every step's way on stays on screen, the existing
/// identity's ways in and out included: the page scrolls, its actions do not.
#[test]
fn every_way_on_stays_on_a_small_display_with_large_text() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let small = [OutputConfig::new("HEADLESS-1", 800, 600)];
    let (ceremony, _home) = ceremony();
    let mut session = Session::headless(
        Options {
            presentation: large_text,
            setup: Some(BeforeShell::Ceremony(Box::new(ceremony))),
            collab: false,
            ..Options::default()
        },
        &small,
    )
    .expect("session");
    session.step(Duration::from_millis(1)).expect("a frame");
    shows(&mut session, INTRO);
    on_small_display(&mut session, USE_EXISTING);
    press(&mut session, USE_EXISTING);
    shows(&mut session, EXISTING);
    for (way, on) in [
        ("Use a code from another device", "Join"),
        ("Enter your recovery key", "Restore identity"),
        ("Restore from a backup", "Restore identity"),
    ] {
        on_small_display(&mut session, way);
        press(&mut session, way);
        on_small_display(&mut session, on);
        on_small_display(&mut session, "Back");
        press(&mut session, "Back");
    }
    on_small_display(&mut session, "Back");
    press(&mut session, "Back");
    on_small_display(&mut session, CREATE_NEW);
    press(&mut session, CREATE_NEW);
    on_small_display(&mut session, UNDERSTOOD);
    on_small_display(&mut session, CREATE);
    press(&mut session, UNDERSTOOD);
    press(&mut session, CREATE);
    on_small_display(&mut session, SKIP_UNDERSTOOD);
    on_small_display(&mut session, CONTINUE_WITHOUT);
    press(&mut session, SKIP_UNDERSTOOD);
    press(&mut session, CONTINUE_WITHOUT);
    on_small_display(&mut session, FINISH);
    drop(session);

    let mut tour = Session::headless(
        Options {
            presentation: large_text,
            setup: Some(BeforeShell::Ceremony(Box::new(Ceremony::new_tour()))),
            collab: false,
            ..Options::default()
        },
        &small,
    )
    .expect("session");
    tour.step(Duration::from_millis(1)).expect("a frame");
    for _ in 0..3 {
        on_small_display(&mut tour, "Skip tour");
        press(&mut tour, "Continue");
    }
    on_small_display(&mut tour, "Skip tour");
    on_small_display(&mut tour, "Enter Tilo");
}
