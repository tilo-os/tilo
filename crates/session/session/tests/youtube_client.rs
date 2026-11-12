//! The `YouTube` client as a tilolang app (apps/tl/tube): a canvas query "youtube videos of
//! <creator>" places the Channel, and its page draws.
//!
//! The channel is the client's, published under `tl.so.tilo.lang.tube.Channel`; the typed
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

use crate::common::{CANVAS_ENTRY, Repl, on_screen, placed_resources, private_home, until};

/// The `YouTube` client, as a package names it.
const TUBE: &str = "so.tilo.lang.tube";
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

    let mut repl = Repl::connect(&root.path().join("agent.sock"));
    repl.opened(&mut session);
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
        on_screen(&session, CANVAS_ENTRY).is_some(),
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
    let mut repl = Repl::connect(&root.path().join("agent.sock"));
    repl.opened(&mut session);

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
    let mut repl = Repl::connect(&root.path().join("agent.sock"));
    repl.opened(&mut session);
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
