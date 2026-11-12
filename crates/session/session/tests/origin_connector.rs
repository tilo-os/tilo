//! A tilolang app whose types hold an outside service's resources keeps them in step with that
//! service in a real session (`docs/architecture/apps.md` §1, §8).
//! The app is the `YouTube` client (`apps/tl/tube`), its package a fixture installed as an app
//! whose signed manifest declares `origins = ["youtube"]` . The session's own launch path starts
//! the app's connector as a sandboxed process — here the launcher is a stand-in that runs a child
//! of this test binary as that process — and the child runs the platform's
//! `tilolang_youtube::connector::serve` loop over a scripted service: the same connector the system
//! ships, answering a script instead of `YouTube` .
//! What this proves is the whole path and nothing test-only about it: a search action an instance
//! runs queues an outside effect, the session hands the effect to the connector over the socket it
//! started, and the answer lands in the client's record — its videos and the search's settled state
//! — which the session publishes and its door reads.
//! Before this work the session started no connector at all: the effect stayed in the instance's
//! own process, and nothing outside ever asked `YouTube` .

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session outlives every call"
)]

mod common;

use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use serde_json::json;
use tilo_service::Questions;
use tilo_session::{ActionPlane, AppActions, Launches, Options, Session};

use common::{PERSON, private_home};

/// The `YouTube` client, as a package names it.
const TUBE: &str = "so.tilo.lang.tube";
/// The channel the scripted `YouTube` service answers with.
const NAME: &str = "Low Level Learning";
/// The video the scripted service answers with.
const VIDEO: &str = "I rewrote my window compositor in Rust";
/// Set only in the child this test starts: the connector process.
const CHILD_ENV: &str = "TILO_YOUTUBE_CONNECTOR_TEST_CHILD";

/// The child side, run in a copy of this test binary: it is the app's connector process. Without
/// [`CHILD_ENV`] it is not the child and has nothing to do.
#[test]
fn youtube_connector_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    // The launcher's release: the descriptor the connector's door came in on. The stand-in
    // launcher is the launcher's sandbox here: it hands the door straight into this process,
    // where a real launch would hand it in as the `tilolang-origin` service.
    if runtime_portal::registration_gate::wait_on_stdin().is_err() {
        return;
    }
    let Some(door) = runtime_portal::registration_gate::take_window_door() else {
        return;
    };
    let attachments = std::iter::from_fn(move || {
        match runtime_portal::origin_connector::receive_attachment(&door) {
            Ok(Some(connection)) => Some(std::os::unix::net::UnixStream::from(connection)),
            Ok(None) => None,
            Err(error) => {
                eprintln!("the connector's door failed: {error}");
                None
            }
        }
    });
    // The production connector loop, over the scripted service rather than YouTube.
    tilolang_youtube::connector::serve(&tilolang_youtube::scripted(scripted()), attachments);
}

/// The scripted `YouTube` service: one search's answer.
fn scripted() -> tilolang_youtube::Scripted {
    tilolang_youtube::Scripted {
        searches: [(
            "compositor".to_owned(),
            tilolang_youtube::SearchPage {
                query: "compositor".to_owned(),
                channels: vec![tilolang_youtube::ChannelItem {
                    key: "UC-lowlevel".to_owned(),
                    name: NAME.to_owned(),
                    ..tilolang_youtube::ChannelItem::default()
                }],
                playlists: Vec::new(),
                videos: vec![tilolang_youtube::VideoItem {
                    key: "v-compositor".to_owned(),
                    title: VIDEO.to_owned(),
                    channel_key: "UC-lowlevel".to_owned(),
                    channel_name: NAME.to_owned(),
                    published: "2026-11-01T12:00:00Z".to_owned(),
                    duration: 900,
                    views: 12_000,
                    ..tilolang_youtube::VideoItem::default()
                }],
            },
        )]
        .into_iter()
        .collect(),
        ..tilolang_youtube::Scripted::default()
    }
}

/// Write the launcher the session starts the apps' connectors through: the stand-in runs this test
/// binary as the connector process, on the gate the launcher is handed. The apps are the ones
/// installed in `home` 's own store ([`common::install_apps_in`]), the `YouTube` client's signed
/// package among them; its manifest declares the `youtube` origin.
fn connector_launcher(home: &tilo_test_env::TestHome) -> tilo_test_env::StandIn {
    // The launcher the session starts connectors through: it runs this test binary as the connector
    // process, keeping the gate it was handed as stdin.
    tilo_test_env::stand_in(
        home.home().join("tilo-launcher"),
        &format!(
            "export {CHILD_ENV}=1\n\
             exec '{exe}' --exact youtube_connector_child --nocapture\n",
            exe = std::env::current_exe()
                .expect("this test's program")
                .display(),
        ),
    )
    .unwrap()
}

/// A headless session with the shell as the whole screen and the seeded apps published (the
/// `YouTube` client among them), its apps started through `home` 's launcher. The apps are the ones
/// installed in `home` 's own app store, as a device's are: the client's package is a signed
/// `.tapp` whose manifest declares the `youtube` origin, so the session plans its connector the way
/// it plans any installed app's.
fn session(
    home: &tilo_test_env::TestHome,
    root: &Path,
    launcher: &tilo_test_env::StandIn,
) -> Session {
    let apps = Rc::new(tilo_session::mount::apps::Apps::with_launcher(
        tilo_app_launch::SessionEnvironment::from_process(),
        None,
        Some(launcher.path().to_owned()),
    ));
    let store = home.data().join("tilo-apps");
    let seeded = common::seeded_apps_in(&store);
    crate::common::headless(Options {
        person: PERSON.to_owned(),
        shell: true,
        collab: false,
        services: vec![Box::new(seeded)],
        questions: Some(Questions::new()),
        action_plane: ActionPlane::At(root.join("plane.sock")),
        agent_door: Some(root.join("agent.sock")),
        test_control: Some(root.join("test-control.sock")),
        apps: Some(Rc::clone(&apps) as Rc<dyn AppActions>),
        launch: Some(apps),
        launches: Arc::new(Launches::default()),
        ..Options::default()
    })
}

#[test]
fn an_outside_effect_reaches_the_connector_and_the_answer_lands_in_the_record() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    // The apps a development session runs, installed in this test's own store: the YouTube client's
    // signed package is one of them, with the manifest that declares its origin.
    let store = home.data().join("tilo-apps");
    common::install_apps_in(&store, &home.state().join("tilolang"));
    home.env().set("TILO_APPS_DATA_DIR", &store);
    let launcher = connector_launcher(&home);
    let root = private_home();
    let mut session = session(&home, root.path(), &launcher);

    // The client's root is published by the seeded apps, and the video the scripted service answers
    // with is not in it: only the scenario's own are.
    common::until(&mut session, "the client's root", |session| {
        session
            .resources()
            .iter()
            .any(|resource| resource.kind == format!("tl.{TUBE}.YouTube"))
    });
    assert!(
        !record_says(&session, VIDEO),
        "the answer's video is not in the record before the search"
    );

    // The person's search, through the session's door as any action: the outside effect it queues
    // goes to the connector the session started for the app's origin.
    let root = session
        .resources()
        .into_iter()
        .find(|resource| resource.kind == format!("tl.{TUBE}.YouTube"))
        .expect("the client's root");
    // The client's search: the one action of its root that asks a query.
    let search = root
        .actions
        .iter()
        .find(|action| action.params().iter().any(|param| param.name == "query"))
        .map_or_else(
            || {
                panic!(
                    "the client offers its search: {:?}",
                    root.actions
                        .iter()
                        .map(|action| (&action.id, &action.label))
                        .collect::<Vec<_>>()
                )
            },
            |action| action.id.clone(),
        );
    session
        .invoke(
            root.id.as_str(),
            &search,
            &serde_json::Map::from_iter([("query".to_owned(), json!("compositor"))]),
        )
        .expect("the search action runs");

    // The answer lands in the client's record: the video the scripted service found and the channel
    // it names, each a resource of the client's the door reads, and the search settled
    // (`search_state` `done` , so the client's view stops saying it searches).
    common::until(&mut session, "the answer in the record", |session| {
        record_says(session, VIDEO) && record_says(session, NAME)
    });
    let videos = client_items(&session, "Video");
    assert!(
        videos.iter().any(|(_, title)| title == VIDEO),
        "the video YouTube answered with is a resource of the client's: {videos:?}"
    );
    let channels = client_items(&session, "Channel");
    assert!(
        channels.iter().any(|(_, name)| name == NAME),
        "the channel YouTube answered with is a resource of the client's: {channels:?}"
    );
}

/// The client's items of the kind `kind` (`Video`, `Channel` ): each its published id and the text
/// of each of its fields that is a text, read through the session's door.
fn client_items(session: &Session, kind: &str) -> Vec<(String, String)> {
    let Some(root) = session
        .resources()
        .into_iter()
        .find(|resource| resource.kind == format!("tl.{TUBE}.YouTube"))
    else {
        return Vec::new();
    };
    let Some(reading) = session.read(root.id.as_str()) else {
        return Vec::new();
    };
    let wanted = format!("#{kind}");
    let mut items = Vec::new();
    for list in reading
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(_, value)| value["list"].as_array())
    {
        for item in list {
            let (Some(id), Some(ty)) = (item["ref"].as_str(), item["ty"].as_str()) else {
                continue;
            };
            if !ty.ends_with(&wanted) {
                continue;
            }
            if let Some(fields) = session.read(id) {
                for text in fields
                    .as_object()
                    .into_iter()
                    .flatten()
                    .filter_map(|(_, value)| value.as_str())
                {
                    items.push((id.to_owned(), text.to_owned()));
                }
            }
        }
    }
    items
}

/// "youtube videos of <creator>" typed on the canvas places that creator's channel, live: the
/// channel is not in the client's records when the person types, and the client's own search
/// action asks its origin (here a **scripted test origin**, not YouTube), whose answer becomes the
/// channel's record and then the place. Nothing here is special-cased for YouTube: the query names
/// the client by its app and the channel by its type, and the app's own search action answers it
/// (search.md §4, §7; shell.md §8 "Finding what to place").
#[test]
fn a_live_canvas_query_places_a_channel_the_client_never_held() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let store = home.data().join("tilo-apps");
    common::install_apps_in(&store, &home.state().join("tilolang"));
    home.env().set("TILO_APPS_DATA_DIR", &store);
    let launcher = connector_launcher(&home);
    let root = private_home();
    let mut session = session(&home, root.path(), &launcher);

    // The channel the scripted test origin answers for "compositor" is not held before the query:
    // only the client's own scenario channels are.
    common::until(&mut session, "the client's root", |session| {
        session
            .resources()
            .iter()
            .any(|resource| resource.kind == format!("tl.{TUBE}.YouTube"))
    });
    assert!(
        !record_says(&session, NAME),
        "the channel the scripted test origin answers with is not in the record before the query"
    );

    // The person types `youtube videos of <creator>` on the canvas, as the canvas entry's own
    // `place-found` action receives it.
    session
        .invoke(
            "shell:arrangement",
            "place-found",
            serde_json::json!({"text": "youtube videos of compositor", "x": 80, "y": 60})
                .as_object()
                .expect("arguments"),
        )
        .expect("the arranging action runs");

    // The client's own search action asked its origin, and the answer became the channel's
    // record: the channel the scripted test origin named, held now by the client.
    common::until(
        &mut session,
        "the answered channel in the record",
        |session| record_says(session, NAME),
    );
    // And that channel's page is the place, not the client's feed.
    common::until(&mut session, "the answered channel placed", |session| {
        placed(session)
            .iter()
            .any(|(kind, label)| kind == &format!("tl.{TUBE}.Channel") && label == NAME)
    });
    let held = placed(&session);
    assert!(
        held.iter()
            .any(|(kind, label)| kind == &format!("tl.{TUBE}.Channel") && label == NAME),
        "the query places the answered channel's page: {held:?}"
    );
    assert!(
        !held
            .iter()
            .any(|(kind, _)| kind == &format!("tl.{TUBE}.YouTube")),
        "the client's front page is not the place: {held:?}"
    );
}

/// What stands on the canvas, by the kind and label of the resource each place shows.
fn placed(session: &Session) -> Vec<(String, String)> {
    common::placed_resources(session)
}

/// Whether any of the client's records holds `text` in a text field, as the session's door reads
/// them.
fn record_says(session: &Session, text: &str) -> bool {
    ["Video", "Channel"].into_iter().any(|kind| {
        client_items(session, kind)
            .iter()
            .any(|(_, held)| held == text)
    })
}
