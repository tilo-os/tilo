//! The `YouTube` client as a tilolang app (apps/tl/tube): a canvas query "youtube videos of
//! <creator>" places the Channel, and its page draws.
//!
//! The channel is the client's, published under `tl.so.tilo.tube.Channel`; the typed
//! words "youtube" (the app's name) and "videos" (a type of the app) need not match its text,
//! so the creator's name is all that must, and the channel leads by its title with the type
//! bonus (search.md §4). Its default view is its page, which reads the client's videos and
//! draws them beside the channel's name.
//!
//! The app is seeded from its scenario, the way a development session seeds it: the fixture is
//! labelled, and nothing here is invented as if `YouTube` had answered it.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session outlives every call"
)]

use tilo_session::Session;

use crate::common::{Api, CANVAS, on_screen, placed_resources, private_home, until};

/// The `YouTube` client, as a package names it.
const TUBE: &str = "so.tilo.tube";
/// The channel the client's scenario seeds, by the name its page shows.
const CREATOR: &str = "MKBHD";
/// A video the channel published, by the title its page lists.
const VIDEO: &str = "The phone that finally lasts a week";

/// The seeded apps with the `YouTube` client among them: notes, tasks, chat, mail and the client,
/// each filled by its first scenario, on the canvas.
fn seeded_session(root: &std::path::Path) -> Session {
    crate::common::seeded_session(root)
}

/// Every resource the session publishes, the ones a record holds included: what the session's
/// `find` searches.
fn published(session: &Session) -> Vec<(String, String)> {
    fn walk(resources: &[tilo_actions::ResourceSpec], out: &mut Vec<(String, String)>) {
        for resource in resources {
            out.push((resource.kind.clone(), resource.label.clone()));
            walk(&resource.children, out);
        }
    }
    let mut out = Vec::new();
    walk(&session.resources(), &mut out);
    out
}

/// "youtube videos of <creator>" places the client's Channel, and its page draws: the channel's
/// name, and the videos it published under it.
#[test]
fn youtube_videos_of_a_creator_places_the_channel_and_its_page_draws() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());

    // The client publishes the channel its scenario seeded, as the session's own resources.
    let resources = published(&session);
    assert!(
        resources
            .iter()
            .any(|(kind, label)| kind == &format!("tl.{TUBE}.Channel") && label == CREATOR),
        "the client publishes its seeded channel: {:?}",
        resources
            .iter()
            .filter(|(kind, _)| kind.starts_with(&format!("tl.{TUBE}.")))
            .collect::<Vec<_>>()
    );
    let mut repl = Api::open(&mut session, root.path());
    let (answer, failed) = repl.run(
        &mut session,
        &format!(
            "session.place_found(text: \"youtube videos of {CREATOR}\", view: .none, x: .some(80), y: .some(60))"
        ),
    );
    assert!(!failed, "the program ran: {answer}");

    until(&mut session, "the channel placed", |session| {
        !placed_resources(session).is_empty()
    });
    let placed = placed_resources(&session);
    assert_eq!(
        placed,
        vec![(format!("tl.{TUBE}.Channel"), CREATOR.to_owned())],
        "the query places the client's channel, and only it: {placed:?}"
    );

    // Its page draws: the channel's name, and the video it published, as its own package draws it.
    until(&mut session, "the channel's page", |session| {
        session.shell_says(CREATOR) && session.shell_says(VIDEO)
    });
    assert_eq!(
        session.shell_apps(),
        vec![TUBE.to_owned()],
        "the client's package is the one the place loads"
    );
    assert!(
        on_screen(&session, CANVAS).is_some(),
        "the canvas keeps its entry beside the place: {:?}",
        session.accessible_labels()
    );
}

/// The root's default view is the subscriptions feed: the videos of the channels the person
/// follows, drawn as cards, with the subscriptions themselves in the sidebar.
#[test]
fn youtube_places_the_client_and_its_feed_draws() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let mut repl = Api::open(&mut session, root.path());

    let (answer, failed) = repl.run(
        &mut session,
        "session.place_found(text: \"youtube\", view: .none, x: .some(80), y: .some(60))",
    );
    assert!(!failed, "the program ran: {answer}");
    until(&mut session, "the client placed", |session| {
        !placed_resources(session).is_empty()
    });
    let placed = placed_resources(&session);
    assert_eq!(
        placed,
        vec![(format!("tl.{TUBE}.YouTube"), "YouTube".to_owned())],
        "\"youtube\" places the client's root, which its app draws as the feed: {placed:?}"
    );
    // The feed draws the videos of the channels the person follows, and the places beside them.
    until(&mut session, "the subscriptions feed", |session| {
        session.shell_says(VIDEO) && session.shell_says("Subscriptions")
    });
    assert!(
        session.shell_apps().contains(&TUBE.to_owned()),
        "the client's package loads for the place"
    );
}

/// The client publishes the watch history its scenario seeded, and a video it names opens in its
/// own page: the video's title, and the way to play it, which records the watch.
#[test]
fn the_watch_history_is_published_and_a_video_it_names_opens() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());
    let entries = session
        .resources()
        .into_iter()
        .flat_map(|resource| {
            let mut out = vec![resource.clone()];
            out.extend(resource.children.iter().cloned());
            out
        })
        .filter(|resource| resource.kind == format!("tl.{TUBE}.WatchEntry"))
        .count();
    assert!(
        entries > 0,
        "the client publishes the watch entries its scenario seeded"
    );
    let mut repl = Api::open(&mut session, root.path());
    // The video the person watched part way, by the title its history row shows.
    let (answer, failed) = repl.run(
        &mut session,
        "session.place_found(text: \"The watch that needs no winding\", view: .none, x: .some(80), y: .some(60))",
    );
    assert!(!failed, "the program ran: {answer}");
    until(&mut session, "a video placed", |session| {
        !placed_resources(session).is_empty()
    });
    // Its page draws its title and offers to play it, which records the watch in the history the
    // client's own place lists.
    until(&mut session, "the video's page", |session| {
        session.shell_says("The watch that needs no winding")
    });
}

/// Every resource the client publishes, its actions' address among them: each kind, label and
/// published id, the root's items under it.
fn client_resources(session: &Session) -> Vec<(String, String, String)> {
    fn walk(resources: &[tilo_actions::ResourceSpec], out: &mut Vec<(String, String, String)>) {
        for resource in resources {
            out.push((
                resource.kind.clone(),
                resource.label.clone(),
                resource.id.as_str().to_owned(),
            ));
            walk(&resource.children, out);
        }
    }
    let mut out = Vec::new();
    walk(&session.resources(), &mut out);
    out
}

/// The published id of the client's resource of `kind` labelled `label`.
fn named(session: &Session, kind: &str, label: &str) -> String {
    let wanted = format!("tl.{TUBE}.{kind}");
    client_resources(session)
        .into_iter()
        .find(|(found, name, _)| *found == wanted && name == label)
        .map_or_else(|| panic!("no {kind} called {label}"), |(_, _, id)| id)
}

/// How many resources of `kind` the client publishes.
fn count_of(session: &Session, kind: &str) -> usize {
    let wanted = format!("tl.{TUBE}.{kind}");
    client_resources(session)
        .into_iter()
        .filter(|(found, _, _)| *found == wanted)
        .count()
}

/// The arguments of a JSON object, as an invocation takes them.
fn args(given: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    given.as_object().cloned().unwrap_or_default()
}

/// The client's own actions, by the addresses the session publishes them under: watching a
/// video puts it in the watch history, forgetting it takes it out, deleting the history empties
/// it, and a playlist opens its own page.
#[test]
fn the_clients_history_and_playlist_actions_run_by_their_published_addresses() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = seeded_session(root.path());

    // A video the scenario left unwatched: playing it part way records it in the history.
    let contest = named(&session, "Video", "Twenty minutes before the contest");
    assert_eq!(
        count_of(&session, "WatchEntry"),
        3,
        "the scenario's three watched videos"
    );
    session
        .invoke(
            &contest,
            "progress",
            &args(serde_json::json!({ "at": 0.4 })),
        )
        .expect("the video's progress runs");
    until(&mut session, "the video in the history", |session| {
        client_resources(session).iter().any(|(kind, label, _)| {
            *kind == format!("tl.{TUBE}.WatchEntry") && label == "Twenty minutes before the contest"
        })
    });
    assert_eq!(count_of(&session, "WatchEntry"), 4);

    // Forgetting it takes it out again, the other three staying.
    session
        .invoke(&contest, "forget", &args(serde_json::json!({})))
        .expect("forget runs");
    until(&mut session, "the video out of the history", |session| {
        count_of(session, "WatchEntry") == 3
    });

    // Deleting the history empties it.
    let client = named(&session, "YouTube", "YouTube");
    session
        .invoke(&client, "delete_history", &args(serde_json::json!({})))
        .expect("deleting the history runs");
    until(&mut session, "the history emptied", |session| {
        count_of(session, "WatchEntry") == 0
    });

    // A playlist opens its own page, naming itself.
    let playlist = named(&session, "Playlist", "Science, explained");
    session
        .invoke(&playlist, "open", &args(serde_json::json!({})))
        .expect("the playlist opens");
    until(&mut session, "the playlist's page", |session| {
        placed_resources(session).iter().any(|(kind, label)| {
            *kind == format!("tl.{TUBE}.Playlist") && label == "Science, explained"
        })
    });
}
