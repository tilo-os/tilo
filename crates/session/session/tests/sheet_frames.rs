//! A hosted sheet is what the session shows and what its control socket names: the frame a snapshot
//! writes has the sheet's own pixels in it, and `list_targets` lists the controls the sheet is
//! pressed by — the same way an app's window is listed and pressed.
//! A sheet is drawn by a hosted surface of the session's own, not by a client's window, so both
//! halves are the session's to answer: what it composited, and what it lends a tool that presses by
//! name.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              outlives every call made into it"
)]

use std::path::Path;

use serde_json::{Value, json};
use tilo_desktop::desk_verbs::CHROME_RESOURCE;
use tilo_session::{Options, Session};
use tilo_sheets::Seat as _;
use tilo_sheets::workspace_settings::Tab;

/// A headless session on one display, its control socket at `socket` .
fn session(socket: &Path) -> Session {
    crate::common::headless(Options {
        collab: false,
        test_control: Some(socket.to_path_buf()),
        ..Options::default()
    })
}

/// Invoke `action` of `resource` , as the person's own choice.
fn invoke(session: &mut Session, resource: &str, action: &str) {
    session
        .invoke(resource, action, &serde_json::Map::new())
        .unwrap_or_else(|error| panic!("{resource} {action}: {error:?}"));
    session
        .step(std::time::Duration::from_millis(1))
        .expect("a frame");
}

/// The desk as `snapshot` writes it, the request `tilo-control snapshot` makes.
fn snapshot(session: &mut Session, socket: &Path, path: &Path) -> image::RgbaImage {
    let answer = crate::common::control(
        session,
        socket,
        &json!({"op": "snapshot", "path": path.to_str().unwrap()}),
    );
    assert_eq!(
        answer,
        json!({"ok": {"frame": 0, "locked": false}}),
        "{answer}"
    );
    image::open(path).expect("a PNG").into_rgba8()
}

/// The controls the socket lists, by name.
fn targets(session: &mut Session, socket: &Path) -> Vec<String> {
    let answer = crate::common::control(session, socket, &json!({"op": "list_targets"}));
    answer["targets"]
        .as_array()
        .unwrap_or_else(|| panic!("list_targets answers its targets: {answer}"))
        .iter()
        .filter_map(|target: &Value| target["name"].as_str().map(str::to_owned))
        .collect()
}

/// Where `after` differs from `before` : what the sheet changed, and how much of the display that
/// is.
fn differed(before: &image::RgbaImage, after: &image::RgbaImage) -> (u32, u32, u32, u32, u32) {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    let mut changed: u32 = 0;
    for y in 0..after.height() {
        for x in 0..after.width() {
            if before.get_pixel(x, y) != after.get_pixel(x, y) {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
                changed = changed.saturating_add(1);
            }
        }
    }
    (x0, y0, x1, y1, changed)
}

#[test]
fn a_sheet_the_person_opened_is_in_the_snapshot_and_its_controls_are_listed() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let socket = root.path().join("test-control.sock");
    let mut session = session(&socket);

    let desk = snapshot(&mut session, &socket, &root.path().join("desk.png"));
    invoke(&mut session, CHROME_RESOURCE, "workspace-settings");
    let seat = session
        .workspace_settings()
        .expect("the session hosts the workspace settings sheet")
        .clone();
    assert!(
        seat.presentation().is_open(),
        "the sheet the person opened is up"
    );

    // The next frame is the sheet's: the display changed where the sheet stands, over a large part
    // of it.
    let sheet = snapshot(&mut session, &socket, &root.path().join("sheet.png"));
    let (x0, y0, x1, y1, changed) = differed(&desk, &sheet);
    assert!(
        x1 > x0 && y1 > y0,
        "the snapshot shows the sheet: nothing changed in it"
    );
    assert!(
        changed > 40_000,
        "the sheet's own panel is drawn, not a sliver of it: {changed} pixels changed, \
         between ({x0}, {y0}) and ({x1}, {y1})"
    );

    // And the desk names the controls the sheet is pressed by, as it names an app's window's: the
    // tabs it switches with, by what they say.
    let people = tilo_desktop_surface::i18n::t("workspace-settings-tab-people");
    let agents = tilo_desktop_surface::i18n::t("workspace-settings-tab-agents");
    let listed = targets(&mut session, &socket);
    for name in [&people, &agents] {
        assert!(
            listed.iter().any(|target| target == name),
            "the sheet's {name} tab is a control a tool presses: {listed:?}"
        );
    }

    // A press on one of them through the socket reaches the sheet: its tabs switch as they do under
    // a person's press.
    let pressed = crate::common::control(
        &mut session,
        &socket,
        &json!({"op": "click_named", "name": agents, "verb": "activate"}),
    );
    assert_eq!(pressed, json!({"ok": {}}), "{pressed}");
    crate::common::until(
        &mut session,
        "the Agents tab to be the one shown",
        |session| {
            session
                .workspace_settings()
                .is_some_and(|seat| seat.settings().tab == Tab::Agents)
        },
    );
}

#[test]
fn the_invite_sheet_the_launchpad_opens_is_drawn_and_listed() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let socket = root.path().join("test-control.sock");
    let mut session = session(&socket);

    let desk = snapshot(&mut session, &socket, &root.path().join("desk.png"));
    // The empty desk's launchpad is what the person presses to invite someone: the sheet it opens
    // is the session's own, drawn over the desk.
    let invite = tilo_desktop_surface::i18n::t("invite-title");
    let pressed = crate::common::control(
        &mut session,
        &socket,
        &json!({"op": "click_named", "name": invite, "verb": "activate"}),
    );
    assert_eq!(pressed, json!({"ok": {}}), "{pressed}");
    session
        .step(std::time::Duration::from_millis(1))
        .expect("a frame");
    let seat = session
        .invite_sheet()
        .expect("the session hosts the invite sheet")
        .clone();
    assert!(
        seat.presentation().is_open(),
        "the launchpad's invite card puts the sheet up"
    );

    let sheet = snapshot(&mut session, &socket, &root.path().join("invite.png"));
    let (x0, y0, x1, y1, changed) = differed(&desk, &sheet);
    assert!(
        x1 > x0 && y1 > y0,
        "the snapshot shows the invite sheet: nothing changed in it"
    );
    assert!(
        changed > 40_000,
        "the invite sheet's own card is drawn, not a sliver of it: {changed} pixels changed, \
         between ({x0}, {y0}) and ({x1}, {y1})"
    );

    let close = tilo_desktop_surface::i18n::t("invite-close");
    let listed = targets(&mut session, &socket);
    assert!(
        listed.iter().any(|target| target == &close),
        "the invite sheet's {close} control is one a tool presses: {listed:?}"
    );
}
