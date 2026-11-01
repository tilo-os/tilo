//! The power menu as a person, a screen reader and an agent meet it: where
//! it hangs, what it offers, what a choice asks the shell for.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_sheets::Seat as _;
use tilo_sheets::power::{Offers, PowerApp, PowerEffect, PowerSeat};
use ui::a11y::Action as A11yAction;
use ui::framework::testing::HeadlessApp;
use ui::vello::kurbo::Rect;

const VIEWPORT: (u32, u32) = (1280, 800);

fn staged(scenario: &str) -> HeadlessApp<PowerApp> {
    HeadlessApp::<PowerApp>::capture_at(Some(scenario), VIEWPORT).unwrap()
}

fn rows(app: &mut HeadlessApp<PowerApp>) -> Vec<(String, Rect)> {
    let mut rows: Vec<(String, Rect)> = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| node.role() == ui::accesskit::Role::MenuItem)
        .filter_map(|(_, node)| {
            let bounds = node.bounds()?;
            Some((
                node.label()?.to_owned(),
                Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1),
            ))
        })
        .collect();
    rows.sort_by(|a, b| a.1.y0.total_cmp(&b.1.y0));
    rows
}

/// A menu the shell put up, lent the seat it reports to.
fn seated(offers: Offers) -> (HeadlessApp<PowerApp>, PowerSeat) {
    let seat = PowerSeat::default();
    seat.set_offers(offers);
    seat.presentation().open(false);
    let mut app = HeadlessApp::<PowerApp>::for_person_with_lent("person", seat.clone()).unwrap();
    app.harness().resize(VIEWPORT).unwrap();
    app.settle();
    (app, seat)
}

fn key(app: &mut HeadlessApp<PowerApp>, key: &str) {
    app.harness().key(key);
    app.settle();
}

#[test]
fn the_menu_hangs_under_the_bar_against_the_trailing_edge() {
    let mut app = staged("open");
    let rows = rows(&mut app);
    let names: Vec<&str> = rows.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Lock",
            "Log out",
            "Switch user",
            "Suspend",
            "Restart",
            "Shut down"
        ],
        "every action, in the order a person expects them"
    );
    for (name, rect) in &rows {
        assert!(rect.y0 >= 40.0, "{name} hangs below the bar: {rect:?}");
        assert!(
            rect.x1 <= f64::from(VIEWPORT.0) && rect.x0 > f64::from(VIEWPORT.0) / 2.0,
            "{name} stands at the trailing edge: {rect:?}"
        );
        assert!(
            rect.height() >= 24.0,
            "{name} is a pointer target: {rect:?}"
        );
    }
    for pair in rows.windows(2) {
        assert!(
            pair[0].1.y1 <= pair[1].1.y0,
            "rows do not overlap: {pair:?}"
        );
    }
}

#[test]
fn the_menu_paints_only_its_panel_over_the_desktop() {
    let mut app = staged("open");
    let pixels = app.harness().render_rgba_transparent();
    let (width, _) = VIEWPORT;
    let alpha = |x: u32, y: u32| pixels[usize::try_from((y * width + x) * 4 + 3).unwrap()];
    assert_eq!(
        alpha(20, 400),
        0,
        "the desktop under the sheet shows through"
    );
    assert_eq!(alpha(20, 20), 0);
    let lock = rows(&mut app)[0].1;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a pixel inside the viewport"
    )]
    let (x, y) = (lock.center().x as u32, lock.center().y as u32);
    assert!(alpha(x, y) > 0, "the panel itself is painted");
}

#[test]
fn a_session_that_does_not_own_the_machine_offers_a_shut_down_that_ends_it() {
    let mut app = staged("detached");
    let names: Vec<String> = rows(&mut app).into_iter().map(|(name, _)| name).collect();
    // Shut down stays: in a session started from another desktop it ends
    // that session, it does not power the machine off.
    assert_eq!(names, ["Lock", "Log out", "Shut down"]);
    let mut app = staged("no-switching");
    let names: Vec<String> = rows(&mut app).into_iter().map(|(name, _)| name).collect();
    assert!(!names.iter().any(|name| name == "Switch user"));
    assert!(names.iter().any(|name| name == "Shut down"));
}

#[test]
fn a_screen_reader_meets_a_named_modal_dialog_of_described_rows() {
    let mut app = staged("open");
    let nodes = app.harness().a11y_nodes();
    let dialog = nodes
        .iter()
        .map(|(_, node)| node)
        .find(|node| node.role() == ui::accesskit::Role::Dialog)
        .expect("the menu is a dialog");
    assert_eq!(dialog.label(), Some("Power"));
    assert!(dialog.is_modal());
    assert!(
        dialog
            .description()
            .is_some_and(|text| text.contains("switch user"))
    );
    let items: Vec<_> = nodes
        .iter()
        .map(|(_, node)| node)
        .filter(|node| node.role() == ui::accesskit::Role::MenuItem)
        .collect();
    assert_eq!(items.len(), 6);
    let logout = items
        .iter()
        .find(|node| node.label() == Some("Log out"))
        .unwrap();
    assert_eq!(logout.description(), Some("Open apps close"));
}

#[test]
fn every_action_is_published_and_those_not_offered_say_why() {
    let (mut app, _seat) = seated(Offers {
        host_power: false,
        ..Offers::default()
    });
    let surface = app.agent_surface().to_string();
    for id in [
        "lock",
        "log-out",
        "switch-user",
        "suspend",
        "restart",
        "shut-down",
    ] {
        assert!(surface.contains(id), "{id} is published: {surface}");
    }
    assert!(
        app.effect(&PowerEffect::ShutDown).is_err(),
        "a session that does not own the machine does not shut it down"
    );
}

#[test]
fn a_press_asks_the_shell_and_closes_the_menu() {
    let (mut app, seat) = seated(Offers::default());
    app.harness()
        .perform_named("power_shut_down", &A11yAction::Activate)
        .unwrap();
    app.settle();
    assert_eq!(seat.take_asked(), [PowerEffect::ShutDown]);
    assert!(!seat.presentation().is_open(), "the menu took itself down");
}

#[test]
fn an_agent_invoking_an_action_reaches_the_same_door() {
    let (mut app, seat) = seated(Offers::default());
    app.effect(&PowerEffect::Lock).unwrap();
    app.settle();
    assert_eq!(seat.take_asked(), [PowerEffect::Lock]);
    // Restart takes everything down: whoever acts for the person is asked.
    let (mut app, seat) = seated(Offers::default());
    assert!(app.effect(&PowerEffect::Restart).is_err() || seat.take_asked().is_empty());
    app.effect_confirmed(&PowerEffect::Restart).unwrap();
    app.settle();
    assert_eq!(seat.take_asked(), [PowerEffect::Restart]);
}

#[test]
fn the_keyboard_reaches_every_row_and_escape_leaves() {
    let (mut app, seat) = seated(Offers::default());
    seat.presentation().open(true);
    app.settle();
    key(&mut app, "ArrowDown");
    key(&mut app, "Enter");
    assert_eq!(
        seat.take_asked(),
        [PowerEffect::LogOut],
        "from the first row, one step down is Log out"
    );

    seat.presentation().open(true);
    app.settle();
    key(&mut app, "Escape");
    assert!(seat.take_asked().is_empty());
    assert!(!seat.presentation().is_open(), "Escape takes the menu down");
}

#[test]
fn the_shell_taking_the_sheet_down_closes_the_menu() {
    let (mut app, seat) = seated(Offers::default());
    assert!(!rows(&mut app).is_empty());
    assert!(!seat.rows().is_empty(), "the seat knows where the rows are");
    seat.presentation().close();
    app.settle();
    assert!(rows(&mut app).is_empty());
    assert!(seat.rows().is_empty());
}

#[test]
fn every_scenario_renders() {
    for scenario in ["open", "keyboard", "detached", "no-switching"] {
        let mut app = staged(scenario);
        assert!(!rows(&mut app).is_empty(), "{scenario}");
    }
}
