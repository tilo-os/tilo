//! The Super+P display sheet as a person, a screen reader and an agent meet
//! it: the layouts the displays allow, Keep and Revert while a change
//! waits, the other devices' screens, the wireless receivers, and every
//! choice going to the shell.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_sheets::displays::{
    Band, Discovery, DisplaysApp, DisplaysEffect, DisplaysSeat, Layout, LayoutRow, Link, Receiver,
    Screen, Sheet,
};
use ui::framework::testing::HeadlessApp;

const VIEWPORT: (u32, u32) = (1280, 800);

fn staged(scenario: &str) -> HeadlessApp<DisplaysApp> {
    HeadlessApp::<DisplaysApp>::capture_at(Some(scenario), VIEWPORT).unwrap()
}

fn seated(sheet: Sheet) -> (HeadlessApp<DisplaysApp>, DisplaysSeat) {
    use tilo_sheets::Seat as _;
    let seat = DisplaysSeat::default();
    seat.presentation().open(true);
    seat.show(sheet);
    let mut app = HeadlessApp::<DisplaysApp>::for_person_with_lent("person", seat.clone()).unwrap();
    app.harness().resize(VIEWPORT).unwrap();
    app.settle();
    (app, seat)
}

fn key(app: &mut HeadlessApp<DisplaysApp>, spec: &str) {
    app.harness().key(spec);
    app.settle();
}

fn super_key(app: &mut HeadlessApp<DisplaysApp>, spec: &str, shift: bool) {
    let mut held = vec!["Super".to_owned()];
    if shift {
        held.push("Shift".to_owned());
    }
    app.harness().set_modifiers(&held);
    app.harness().key(spec);
    app.harness().set_modifiers(&[]);
    app.settle();
}

/// A laptop with a monitor, extended.
fn two_displays() -> Sheet {
    Sheet {
        layouts: Layout::ALL
            .into_iter()
            .map(|layout| LayoutRow {
                layout,
                enabled: true,
                current: layout == Layout::Extend,
                stages: true,
            })
            .collect(),
        ..Sheet::default()
    }
}

fn screen(device: &str, output: &str, revision: u64, enabled: bool) -> Screen {
    Screen {
        device: device.to_owned(),
        output: output.to_owned(),
        revision,
        name: output.to_owned(),
        number: 2,
        device_label: device.to_owned(),
        online: true,
        active: true,
        enabled,
    }
}

/// The name of the row the highlight is on.
fn highlighted(app: &mut HeadlessApp<DisplaysApp>) -> Option<String> {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| {
            node.role() == ui::accesskit::Role::ListItem && node.is_selected() == Some(true)
        })
        .and_then(|(_, node)| node.label().map(str::to_owned))
}

#[test]
fn every_scenario_renders_a_modal_display_dialog() {
    use ui::framework::ResourceApp as _;
    for scenario in DisplaysApp::scenarios() {
        let mut app = staged(scenario);
        let nodes = app.harness().a11y_nodes();
        let dialog = nodes
            .iter()
            .map(|(_, node)| node)
            .find(|node| node.role() == ui::accesskit::Role::Dialog)
            .unwrap_or_else(|| panic!("{scenario}: the sheet is a dialog"));
        assert_eq!(dialog.label(), Some("Display"), "{scenario}");
        assert!(dialog.is_modal(), "{scenario}");
        assert!(
            dialog
                .description()
                .is_some_and(|text| text.contains("Super+P")),
            "{scenario}: the keys are said"
        );
        let pixels = app.harness().render_rgba_transparent();
        assert!(
            pixels.chunks(4).any(|pixel| pixel[3] > 0),
            "{scenario} draws"
        );
    }
}

#[test]
fn super_p_cycles_through_every_layout_the_displays_allow() {
    let (mut app, seat) = seated(two_displays());
    assert_eq!(highlighted(&mut app).as_deref(), Some("Extend"));
    let mut cycle = Vec::new();
    for _ in 0..4 {
        super_key(&mut app, "p", false);
        cycle.push(highlighted(&mut app).unwrap());
    }
    assert_eq!(
        cycle,
        ["Mirror", "Only this screen", "Only external", "Extend"]
    );
    assert!(
        seat.take_asked().is_empty(),
        "moving the highlight chooses nothing"
    );
}

#[test]
fn enter_chooses_the_highlighted_layout_and_the_keyboard_goes_to_keep() {
    let (mut app, seat) = seated(two_displays());
    key(&mut app, "ArrowDown");
    assert_eq!(highlighted(&mut app).as_deref(), Some("Mirror"));
    key(&mut app, "Enter");
    assert_eq!(seat.take_asked(), [DisplaysEffect::Mirror]);
    // The change is staged: Keep and Revert stand under the layouts.
    seat.show(Sheet {
        band: Band::Confirm {
            status: "Reverting in 12 s".to_owned(),
            answering: false,
        },
        ..two_displays()
    });
    app.settle();
    key(&mut app, "Enter");
    assert_eq!(seat.take_asked(), [DisplaysEffect::Keep]);
}

#[test]
fn number_keys_choose_a_layout_at_once() {
    let (mut app, seat) = seated(two_displays());
    key(&mut app, "3");
    assert_eq!(seat.take_asked(), [DisplaysEffect::BuiltinOnly]);
}

#[test]
fn keep_and_revert_wait_for_the_answer_in_flight() {
    let (mut app, seat) = seated(Sheet {
        band: Band::Confirm {
            status: "Saving the display answer…".to_owned(),
            answering: true,
        },
        ..two_displays()
    });
    let answers: Vec<bool> = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| {
            node.role() == ui::accesskit::Role::Button
                && matches!(node.label(), Some("Keep" | "Revert"))
        })
        .map(|(_, node)| node.is_disabled())
        .collect();
    assert_eq!(answers, [true, true]);
    key(&mut app, "ArrowDown");
    key(&mut app, "ArrowDown");
    key(&mut app, "ArrowDown");
    key(&mut app, "ArrowDown");
    key(&mut app, "Enter");
    assert!(seat.take_asked().is_empty(), "no second answer is sent");
}

#[test]
fn a_single_display_offers_no_layout_and_says_what_is_reachable() {
    let mut app = staged("one-display");
    let nodes = app.harness().a11y_nodes();
    let said: Vec<String> = nodes
        .iter()
        .filter_map(|(_, node)| node.label().map(str::to_owned))
        .collect();
    assert!(
        said.iter().any(|text| text == "No other display connected"),
        "{said:?}"
    );
    assert!(
        said.iter().any(|text| text == "Looking for displays…"),
        "{said:?}"
    );
    assert!(!said.iter().any(|text| text == "Mirror"), "{said:?}");
}

#[test]
fn a_refusal_is_said() {
    let mut app = staged("refused");
    let dialog = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| node.role() == ui::accesskit::Role::Dialog)
        .unwrap()
        .1;
    assert!(
        dialog
            .description()
            .is_some_and(|text| text.contains("no longer permits"))
    );
}

#[test]
fn a_screen_is_included_against_the_revision_shown_and_followed_when_rows_move() {
    let sheet = Sheet {
        screens: vec![
            screen("desk", "DP-1", 4, true),
            screen("tablet", "DSI-1", 9, false),
        ],
        screens_editable: true,
        ..Sheet::default()
    };
    let (mut app, seat) = seated(sheet.clone());
    key(&mut app, "ArrowDown");
    key(&mut app, "Space");
    assert_eq!(
        seat.take_asked(),
        [DisplaysEffect::Include {
            device: "tablet".to_owned(),
            output: "DSI-1".to_owned(),
            revision: 9,
            include: true,
        }]
    );
    // A collaborator reorders the screens: the highlight stays on the
    // tablet's.
    let mut moved = sheet;
    moved.screens.reverse();
    seat.show(moved);
    app.settle();
    key(&mut app, "Enter");
    assert!(matches!(
        seat.take_asked().as_slice(),
        [DisplaysEffect::Include { device, .. }] if device == "tablet"
    ));
}

#[test]
fn screens_the_person_may_not_arrange_are_shown_but_not_changed() {
    let (mut app, seat) = seated(Sheet {
        screens: vec![screen("desk", "DP-1", 4, true)],
        screens_editable: false,
        ..Sheet::default()
    });
    key(&mut app, "Enter");
    assert!(seat.take_asked().is_empty());
}

fn receivers() -> Sheet {
    Sheet {
        receivers: vec![
            Receiver {
                id: "airplay:tv".to_owned(),
                name: "TV".to_owned(),
                protocol: "AirPlay".to_owned(),
                asks_pin: true,
                ..Receiver::default()
            },
            Receiver {
                id: "cast:hub".to_owned(),
                name: "Hub".to_owned(),
                protocol: "Google Cast".to_owned(),
                ..Receiver::default()
            },
        ],
        ..Sheet::default()
    }
}

#[test]
fn a_receiver_that_asks_for_a_pin_is_chosen_first_and_the_pin_never_enters_an_action() {
    let (mut app, seat) = seated(receivers());
    key(&mut app, "Enter");
    assert_eq!(
        seat.take_asked(),
        [DisplaysEffect::Choose {
            receiver: "airplay:tv".to_owned()
        }]
    );
    let mut chosen = receivers();
    chosen.receivers[0].chosen = true;
    chosen.asks_pin = true;
    seat.show(chosen);
    app.settle();
    app.harness().type_text("4321");
    app.settle();
    assert_eq!(seat.pin(), "4321");
    key(&mut app, "Enter");
    let asked = seat.take_asked();
    assert_eq!(
        asked,
        [DisplaysEffect::Connect {
            receiver: "airplay:tv".to_owned()
        }]
    );
    assert!(!format!("{asked:?}").contains("4321"));
    assert_eq!(seat.pin(), "4321", "the shell connects with the PIN typed");
}

#[test]
fn a_connected_receiver_stops_and_other_receivers_connect() {
    let mut sheet = receivers();
    sheet.receivers[0].link = Link::Connected;
    let (mut app, seat) = seated(sheet);
    key(&mut app, "Enter");
    assert_eq!(seat.take_asked(), [DisplaysEffect::Stop]);
    key(&mut app, "ArrowDown");
    key(&mut app, "Enter");
    assert_eq!(
        seat.take_asked(),
        [DisplaysEffect::Connect {
            receiver: "cast:hub".to_owned()
        }]
    );
    key(&mut app, "r");
    assert_eq!(seat.take_asked(), [DisplaysEffect::Search]);
}

#[test]
fn a_failed_search_says_what_to_do_and_offers_to_try_again() {
    let (mut app, _seat) = seated(Sheet {
        discovery: Discovery::Failed {
            title: "Couldn't look for wireless displays".to_owned(),
            next_step: "Check that Wi-Fi is on, then try again.".to_owned(),
        },
        ..Sheet::default()
    });
    let said: Vec<String> = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .filter_map(|(_, node)| node.label().map(str::to_owned))
        .collect();
    assert!(said.iter().any(|text| text == "Try again"), "{said:?}");
    assert!(
        !said.iter().any(|text| text.contains("os error")),
        "{said:?}"
    );
}

#[test]
fn escape_and_super_shift_p_close() {
    let (mut app, seat) = seated(two_displays());
    key(&mut app, "Escape");
    assert_eq!(seat.take_asked(), [DisplaysEffect::Close]);
    let (mut app, seat) = seated(two_displays());
    super_key(&mut app, "p", true);
    assert_eq!(seat.take_asked(), [DisplaysEffect::Close]);
}

#[test]
fn keeping_a_change_is_the_persons_answer() {
    use ui::tilo_resource::Effect as _;
    for declaration in DisplaysEffect::declarations() {
        assert_eq!(
            declaration.attended,
            declaration.id == "keep",
            "only Keep needs the person: {}",
            declaration.id
        );
    }
}
