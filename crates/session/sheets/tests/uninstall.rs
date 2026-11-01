//! "Uninstall \<name\>?" as a person, a screen reader and an agent meet it:
//! what goes with the app, the folder each copy is in, the data switch, and
//! the answer going back to the shell.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::PathBuf;

use tilo_sheets::Seat as _;
use tilo_sheets::uninstall::{
    App, Going, REMOVE_DATA_SWITCH, Removal, UninstallApp, UninstallEffect, UninstallSeat,
};
use ui::a11y::Action as A11yAction;
use ui::framework::testing::HeadlessApp;

const VIEWPORT: (u32, u32) = (1280, 800);

fn staged(scenario: &str) -> HeadlessApp<UninstallApp> {
    let mut app = HeadlessApp::<UninstallApp>::capture_at(Some(scenario), VIEWPORT).unwrap();
    app.settle();
    app
}

/// A downloaded program with `copies`, the file that starts it first.
fn downloaded(copies: &[&str]) -> App {
    App {
        desktop_id: "tilo-downloaded-tilo-imager".to_owned(),
        name: "Tilo Imager".to_owned(),
        removal: Removal::Uninstall,
        going: Going::Files {
            copies: copies.iter().map(PathBuf::from).collect(),
            home: Some(PathBuf::from("/home/ute")),
        },
    }
}

/// An app installed on the computer, which saved data of its own.
fn installed(device_wide: bool) -> App {
    App {
        desktop_id: "transmission-gtk".to_owned(),
        name: "Transmission".to_owned(),
        removal: Removal::Uninstall,
        going: Going::Data { device_wide },
    }
}

/// The sheet as the shell lends it, with `app` asked about.
fn seated(app: App) -> (HeadlessApp<UninstallApp>, UninstallSeat) {
    let seat = UninstallSeat::default();
    seat.show(app);
    seat.presentation().open(true);
    let mut app =
        HeadlessApp::<UninstallApp>::for_person_with_lent("person", seat.clone()).unwrap();
    app.harness().resize(VIEWPORT).unwrap();
    app.settle();
    (app, seat)
}

fn says(app: &mut HeadlessApp<UninstallApp>, text: &str) -> bool {
    app.harness().a11y_nodes().iter().any(|(_, node)| {
        node.label().is_some_and(|label| label.contains(text))
            || node.description().is_some_and(|said| said.contains(text))
            || node.value().is_some_and(|value| value.contains(text))
    })
}

fn press(app: &mut HeadlessApp<UninstallApp>, id: &str) {
    app.harness()
        .perform_named(id, &A11yAction::Activate)
        .unwrap();
    app.settle();
}

#[test]
fn uninstalling_lists_every_copy_that_goes_to_the_trash() {
    let mut app = staged("copies");
    for text in [
        "Uninstall Tilo Imager?",
        "All 2 copies go to the Trash, where you can restore them.",
        "Applications",
        "Downloads",
        "Uninstall",
        "Cancel",
    ] {
        assert!(says(&mut app, text), "{text:?} is shown");
    }
    assert!(
        !says(&mut app, "Its file goes to the Trash"),
        "two copies are not one file"
    );
    let rows = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| {
            node.label()
                .is_some_and(|label| label.contains("tilo-imager.AppImage"))
        })
        .count();
    assert_eq!(rows, 2, "each copy is a row of its own");
}

#[test]
fn one_copy_reads_as_one_file_in_the_folder_it_is_in() {
    let mut app = staged("one-copy");
    assert!(says(
        &mut app,
        "Its file goes to the Trash, where you can restore it."
    ));
    assert!(!says(&mut app, "copies go to the Trash"));
    assert!(says(&mut app, "tilo-imager-linux"));
    assert!(says(&mut app, "Downloads"));
}

#[test]
fn a_copy_in_the_home_folder_says_so() {
    let (mut app, _seat) = seated(downloaded(&["/home/ute/tilo-imager.AppImage"]));
    assert!(says(&mut app, "Home"), "the home folder is named as such");
    assert!(!says(&mut app, "/home/ute"), "not by its path");
}

#[test]
fn an_installed_app_asks_whether_its_data_goes_too() {
    let (mut app, seat) = seated(installed(false));
    for text in [
        "Uninstall Transmission?",
        "Remove its data",
        "Your settings and saved files go to the Trash, where you can restore them.",
        "Uninstall",
        "Cancel",
    ] {
        assert!(says(&mut app, text), "{text:?} is shown");
    }
    assert!(
        seat.offers_data_switch(),
        "an installed app may have saved something"
    );
    press(&mut app, "uninstall.primary");
    assert_eq!(
        seat.take_asked(),
        [UninstallEffect::Confirm {
            remove_data: Some(true)
        }],
        "its data goes unless the person says otherwise"
    );
}

#[test]
fn keeping_the_data_is_the_persons_choice() {
    let (mut app, seat) = seated(installed(false));
    press(&mut app, REMOVE_DATA_SWITCH);
    press(&mut app, "uninstall.primary");
    assert_eq!(
        seat.take_asked(),
        [UninstallEffect::Confirm {
            remove_data: Some(false)
        }]
    );
}

#[test]
fn an_app_installed_for_everyone_says_so() {
    let mut app = staged("installed");
    assert!(says(&mut app, "Remove its data"));
    let (mut device_wide, _seat) = seated(installed(true));
    assert!(
        says(
            &mut device_wide,
            "It's removed for everyone on this computer."
        ),
        "the person is told the app is not only theirs"
    );
}

#[test]
fn a_part_of_the_system_only_leaves_the_persons_apps() {
    let mut app = staged("from-apps");
    for text in [
        "Remove Transmission from apps?",
        "It's part of the system, so it stays on this computer and no longer shows in your apps.",
        "Remove from apps",
        "Cancel",
    ] {
        assert!(says(&mut app, text), "{text:?} is shown");
    }
}

#[test]
fn cancel_and_closing_leave_the_app_where_it_is() {
    let (mut app, seat) = seated(downloaded(&["/home/ute/Downloads/tilo-imager"]));
    press(&mut app, "uninstall.secondary");
    assert_eq!(seat.take_asked(), [UninstallEffect::Cancel]);

    let (mut app, seat) = seated(downloaded(&["/home/ute/Downloads/tilo-imager"]));
    app.harness().key("Escape");
    app.settle();
    assert_eq!(seat.take_asked(), [UninstallEffect::Cancel]);
}

#[test]
fn enter_keeps_an_uninstall() {
    let (mut app, seat) = seated(downloaded(&["/home/ute/Downloads/tilo-imager"]));
    app.harness().key("Enter");
    app.settle();
    assert_eq!(
        seat.take_asked(),
        [UninstallEffect::Cancel],
        "the keyboard starts on the safe answer of a deletion"
    );
}

#[test]
fn a_part_of_the_system_is_removed_by_its_own_button() {
    let (mut app, seat) = seated(App {
        removal: Removal::FromApps,
        going: Going::Entry,
        ..installed(false)
    });
    press(&mut app, "uninstall.primary");
    assert_eq!(
        seat.take_asked(),
        [UninstallEffect::Confirm { remove_data: None }],
        "taking it off the apps loses nothing on the computer, so nothing is asked about data"
    );
}
