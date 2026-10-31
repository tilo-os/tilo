//! "Add \<name\> to apps?" as a person, a screen reader and an agent meet it:
//! the program shown as it will be listed, the move offered only for an
//! `AppImage`, and the answer going back to the shell.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::PathBuf;

use tilo_sheets::Seat as _;
use tilo_sheets::add_app::{AddApp, AddAppEffect, AddSeat, MOVE, MOVE_SWITCH, Offer};
use ui::a11y::Action as A11yAction;
use ui::framework::testing::HeadlessApp;

const VIEWPORT: (u32, u32) = (1280, 800);

fn staged(scenario: &str) -> HeadlessApp<AddApp> {
    let mut app = HeadlessApp::<AddApp>::capture_at(Some(scenario), VIEWPORT).unwrap();
    app.settle();
    app
}

fn appimage() -> Offer {
    Offer {
        file: PathBuf::from("/home/ute/Downloads/krita-5.2.AppImage"),
        name: "Krita".to_owned(),
        appimage: true,
        artwork: None,
    }
}

/// The sheet as the shell lends it, with `offer` asked about.
fn seated(offer: Offer) -> (HeadlessApp<AddApp>, AddSeat) {
    let seat = AddSeat::default();
    seat.show(offer);
    seat.presentation().open(true);
    let mut app = HeadlessApp::<AddApp>::for_person_with_lent("person", seat.clone()).unwrap();
    app.harness().resize(VIEWPORT).unwrap();
    app.settle();
    (app, seat)
}

fn says(app: &mut HeadlessApp<AddApp>, text: &str) -> bool {
    app.harness().a11y_nodes().iter().any(|(_, node)| {
        node.label().is_some_and(|label| label.contains(text))
            || node.description().is_some_and(|said| said.contains(text))
            || node.value().is_some_and(|value| value.contains(text))
    })
}

fn press(app: &mut HeadlessApp<AddApp>, id: &str) {
    app.harness()
        .perform_named(id, &A11yAction::Activate)
        .unwrap();
    app.settle();
}

#[test]
fn an_appimage_is_shown_as_it_will_be_listed_with_the_move_offered() {
    let mut app = staged("appimage");
    for text in [
        "Add Krita to apps?",
        "Krita",
        "krita-5.2.AppImage",
        "Move to Applications",
        "Keeps it in the Applications folder in your files",
        "Add to apps",
        "Don't add",
    ] {
        assert!(says(&mut app, text), "{text:?} is shown");
    }
    assert!(
        says(&mut app, "krita-5.2.AppImage"),
        "the file it runs from is named"
    );
}

#[test]
fn a_program_from_a_tarball_is_offered_no_move() {
    let mut app = staged("program");
    assert!(says(&mut app, "Add Krita to apps?"));
    assert!(
        !says(&mut app, "Move to Applications"),
        "a program may need the files beside it, so it stays where it is"
    );
    assert_eq!(
        app.harness()
            .a11y_nodes()
            .into_iter()
            .filter(|(_, node)| node.label() == Some(MOVE))
            .count(),
        0
    );
}

#[test]
fn adding_asks_the_shell_for_the_program_and_no_move_unless_switched_on() {
    let (mut app, seat) = seated(appimage());
    press(&mut app, "add-app.primary");
    assert_eq!(
        seat.take_asked(),
        [AddAppEffect::Add {
            move_to_applications: None
        }],
        "the switch starts off, and adding where the file is is the default"
    );

    let (mut app, seat) = seated(appimage());
    press(&mut app, MOVE_SWITCH);
    press(&mut app, "add-app.primary");
    assert_eq!(
        seat.take_asked(),
        [AddAppEffect::Add {
            move_to_applications: Some(true)
        }]
    );
}

#[test]
fn not_adding_and_closing_both_decline_the_file() {
    let (mut app, seat) = seated(appimage());
    press(&mut app, "add-app.secondary");
    assert_eq!(seat.take_asked(), [AddAppEffect::Decline]);

    let (mut app, seat) = seated(appimage());
    app.harness().key("Escape");
    app.settle();
    assert_eq!(
        seat.take_asked(),
        [AddAppEffect::Decline],
        "closing it is an answer too: that file is not asked about again"
    );
}

#[test]
fn another_program_is_asked_about_with_the_switch_off_again() {
    let (mut app, seat) = seated(appimage());
    press(&mut app, MOVE_SWITCH);
    seat.show(Offer {
        file: PathBuf::from("/home/ute/Downloads/inkscape.AppImage"),
        name: "Inkscape".to_owned(),
        ..appimage()
    });
    app.settle();
    assert!(says(&mut app, "Add Inkscape to apps?"));
    press(&mut app, "add-app.primary");
    assert_eq!(
        seat.take_asked(),
        [AddAppEffect::Add {
            move_to_applications: None
        }],
        "each question starts where the person's answer to the last one ended"
    );
}
