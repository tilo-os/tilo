//! "Join a workspace…" as a person, a screen reader and an agent meet it:
//! what it lists, how search narrows it, what Join, Enter and Escape ask.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_sheets::Seat as _;
use tilo_sheets::directory::{
    Directory, DirectoryApp, DirectoryEffect, DirectorySeat, Listing, ListingAction, Status,
};
use ui::a11y::Action as A11yAction;
use ui::framework::testing::HeadlessApp;

const VIEWPORT: (u32, u32) = (1280, 800);

fn staged(scenario: &str) -> HeadlessApp<DirectoryApp> {
    HeadlessApp::<DirectoryApp>::capture_at(Some(scenario), VIEWPORT).unwrap()
}

fn says(app: &mut HeadlessApp<DirectoryApp>, text: &str) -> bool {
    app.harness().a11y_nodes().iter().any(|(_, node)| {
        node.label().is_some_and(|label| label.contains(text))
            || node.description().is_some_and(|said| said.contains(text))
            || node.value().is_some_and(|value| value.contains(text))
    })
}

fn listing(key: &str, name: &str, action: ListingAction) -> Listing {
    Listing {
        key: key.to_owned(),
        name: name.to_owned(),
        about: format!("About {name}"),
        host: "tilo.coffee".to_owned(),
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
    assert!(!seat.presentation().is_drawn());
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
    let joins: Vec<&(String, bool)> = buttons
        .iter()
        .filter(|(name, _)| ["Join", "Joining…", "Open"].contains(&name.as_str()))
        .collect();
    assert!(!joins.is_empty(), "{buttons:?}");
    assert!(joins.iter().all(|(_, disabled)| *disabled), "{buttons:?}");
}
