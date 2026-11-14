//! "Join a workspace…" as a person, a screen reader and an agent meet it:
//! what it lists, how search narrows it, what Join, Enter and Escape ask.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::cell::RefCell;
use std::rc::Rc;

use tilo_sheets::Seat as _;
use tilo_sheets::directory::{
    self, Directory, DirectoryApp, DirectoryEffect, DirectorySeat, Listing, ListingAction, Status,
};
use ui::a11y::Action as A11yAction;
use ui::framework::testing::HeadlessApp;
use ui::headless::FrameHarness;

const VIEWPORT: (u32, u32) = (1280, 800);

fn staged(scenario: &str) -> HeadlessApp<DirectoryApp> {
    HeadlessApp::<DirectoryApp>::capture_at(Some(scenario), VIEWPORT).unwrap()
}

fn frame_says(frame: &FrameHarness, text: &str) -> bool {
    frame.a11y_nodes().iter().any(|(_, node)| {
        node.label().is_some_and(|label| label.contains(text))
            || node.description().is_some_and(|said| said.contains(text))
            || node.value().is_some_and(|value| value.contains(text))
    })
}

fn says(app: &mut HeadlessApp<DirectoryApp>, text: &str) -> bool {
    frame_says(app.harness(), text)
}

fn listing(key: &str, name: &str, action: ListingAction) -> Listing {
    Listing {
        key: key.to_owned(),
        name: name.to_owned(),
        about: format!("About {name}"),
        host: "tilo.sh".to_owned(),
        activity: "Active today".to_owned(),
        joins_as: "You join as an editor".to_owned(),
        action,
        ..Listing::default()
    }
}

fn directory() -> Directory {
    Directory {
        status: Status::Listed,
        listings: vec![
            listing("garden", "Gardeners", ListingAction::Join),
            listing("chess", "Chess club", ListingAction::Open),
        ],
        note: None,
    }
}

fn seated(directory: Directory) -> (HeadlessApp<DirectoryApp>, DirectorySeat) {
    let seat = DirectorySeat::default();
    seat.show(directory);
    seat.presentation().open(false);
    let mut app =
        HeadlessApp::<DirectoryApp>::for_person_with_lent("person", seat.clone()).unwrap();
    app.harness().resize(VIEWPORT).unwrap();
    app.settle();
    (app, seat)
}

/// Whether the sheet alone, built from `listings` and nothing else, says `text`.
fn sheet_says(listings: Vec<Listing>, text: &str) -> bool {
    let ask: directory::Ask = Rc::new(|_: DirectoryEffect| {});
    let query = ui::LocalState::new(String::new());
    let inputs = ui::headless::fixture_runner_inputs();
    let directory = Directory {
        status: Status::Listed,
        listings,
        note: None,
    };
    let mut frame = FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        directory::sheet(&directory, &query, &ask, None),
        VIEWPORT,
        inputs.theme,
        inputs.text_renderer,
        ui::collaboration::local_user_id(),
    );
    frame.relayout();
    frame_says(&frame, text)
}

#[test]
fn every_scenario_says_where_the_list_stands() {
    assert!(says(&mut staged("listed"), "Gardeners"));
    assert!(says(
        &mut staged("loading"),
        "Looking for public workspaces"
    ));
    assert!(says(&mut staged("failed"), "no site answered"));
    assert!(says(&mut staged("joining"), "Joining"));
}

#[test]
fn join_and_open_ask_the_shell_for_the_listing() {
    let (mut app, seat) = seated(directory());
    app.harness()
        .perform_named("directory-row-garden", &A11yAction::Activate)
        .unwrap();
    app.settle();
    app.harness()
        .perform_named("directory-row-chess", &A11yAction::Activate)
        .unwrap();
    app.settle();
    assert_eq!(
        seat.take_asked(),
        [
            DirectoryEffect::Join {
                key: "garden".to_owned()
            },
            DirectoryEffect::Open {
                key: "chess".to_owned()
            },
        ]
    );
}

#[test]
fn search_narrows_the_list_and_enter_looks_up_what_was_typed() {
    let (mut app, seat) = seated(directory());
    app.harness()
        .perform_named(
            "directory-search",
            &A11yAction::SetValue("chess".to_owned()),
        )
        .unwrap();
    app.settle();
    assert_eq!(seat.query(), "chess");
    assert!(says(&mut app, "Chess club"));
    assert!(!says(&mut app, "Gardeners"), "search narrows the list");
    app.harness().key("Enter");
    app.settle();
    assert_eq!(
        seat.take_asked(),
        [DirectoryEffect::Look {
            text: "chess".to_owned()
        }]
    );
}

#[test]
fn escape_clears_the_search_first_then_closes() {
    let (mut app, seat) = seated(directory());
    app.harness()
        .perform_named("directory-search", &A11yAction::SetValue("x".to_owned()))
        .unwrap();
    app.settle();
    app.harness().key("Escape");
    app.settle();
    assert_eq!(seat.query(), "");
    assert!(seat.take_asked().is_empty());
    app.harness().key("Escape");
    app.settle();
    assert_eq!(seat.take_asked(), [DirectoryEffect::Done]);
    assert_eq!(
        seat.presentation().phase(),
        tilo_sheets::Phase::Leaving,
        "the card leaves the screen; the shell hears it is gone once it has"
    );
}

#[test]
fn an_agent_joins_through_the_same_door() {
    let (mut app, seat) = seated(directory());
    app.effect(&DirectoryEffect::Join {
        key: "garden".to_owned(),
    })
    .unwrap();
    app.settle();
    assert_eq!(
        seat.take_asked(),
        [DirectoryEffect::Join {
            key: "garden".to_owned()
        }]
    );
}

#[test]
fn a_join_running_holds_the_other_buttons() {
    let mut app = staged("joining");
    let buttons: Vec<(String, bool)> = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| node.role() == ui::accesskit::Role::Button)
        .map(|(_, node)| {
            (
                node.label().unwrap_or_default().to_owned(),
                node.is_disabled(),
            )
        })
        .collect();
    // A row's button is announced with the workspace it acts on ("Joining
    // Tilo Coffee"), so the rows are found by those names; the way in by a
    // link or code is no row of the list and stays live.
    let rows = ["Joining Tilo Coffee", "Join Gardeners", "Open Chess club"];
    let held: Vec<&(String, bool)> = buttons
        .iter()
        .filter(|(name, _)| rows.contains(&name.as_str()))
        .collect();
    assert_eq!(
        held.iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        rows,
        "{buttons:?}"
    );
    assert!(held.iter().all(|(_, disabled)| *disabled), "{buttons:?}");
}

#[test]
fn a_listing_that_draws_no_faces_says_how_many_are_here_in_words_only() {
    // A host that keeps no faces (a listing stored before faces existed) still
    // says how many are here. The row says that in its activity line; a count
    // of who is past the faces would sit there alone, past nothing.
    let mut quiet = listing("lounge", "Lounge", ListingAction::Join);
    quiet.here_count = 3;
    quiet.activity = "3 here now".to_owned();
    assert!(sheet_says(vec![quiet.clone()], "3 here now"));
    assert!(!sheet_says(vec![quiet], "+3"), "no count with no faces");
}

#[test]
fn faces_carry_the_count_of_those_past_them() {
    let mut busy = listing("busy", "Busy", ListingAction::Join);
    busy.here = (0..4)
        .map(|at| (format!("Face{at}"), format!("id{at}")))
        .collect();
    busy.here_count = 6;
    assert!(sheet_says(vec![busy.clone()], "Face0"));
    assert!(sheet_says(vec![busy], "+2"));
}

#[test]
fn the_sheet_alone_lists_a_fixture_and_asks_through_the_door_it_was_given() {
    // One surface, two hosts: the session's window and a web page both build
    // this sheet from the directory they keep, the search they keep, and their
    // own way to ask. Mounted with nothing but those three, it lists what the
    // directory holds and asks through the door it was handed.
    let asked: Rc<RefCell<Vec<DirectoryEffect>>> = Rc::default();
    let ask: directory::Ask = {
        let asked = Rc::clone(&asked);
        Rc::new(move |effect| asked.borrow_mut().push(effect))
    };
    let query = ui::LocalState::new(String::new());
    let inputs = ui::headless::fixture_runner_inputs();
    let mut frame = FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        directory::sheet(&directory(), &query, &ask, None),
        VIEWPORT,
        inputs.theme,
        inputs.text_renderer,
        ui::collaboration::local_user_id(),
    );
    frame.relayout();
    assert!(
        frame_says(&frame, "Gardeners"),
        "the fixture's first listing"
    );
    assert!(
        frame_says(&frame, "Chess club"),
        "the fixture's second listing"
    );
    frame
        .perform_named(
            "directory-search",
            &A11yAction::SetValue("garden".to_owned()),
        )
        .unwrap();
    frame.relayout();
    // The search is the host's: it keeps it, and builds the sheet again when
    // it moves (the page's directory compares its revision as the window's
    // switch does).
    assert_eq!(query.with(String::clone), "garden");
    frame
        .perform_named("directory-row-garden", &A11yAction::Activate)
        .unwrap();
    frame.relayout();
    assert_eq!(
        asked.borrow().as_slice(),
        [DirectoryEffect::Join {
            key: "garden".to_owned()
        },]
    );
}
