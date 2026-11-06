//! A tilolang app whose types hold an outside service's resources keeps them in step with that
//! service in a real session (`docs/architecture/apps.md` §1, §8).
//!
//! The app is the YouTube client (`apps/tl/tube`), its package a fixture installed as an app
//! whose signed manifest declares `origins = ["youtube"]`. The session's own launch path starts
//! the app's connector as a sandboxed process — here the launcher is a stand-in that runs a
//! child of this test binary as that process — and the child runs the platform's
//! `tilolang_youtube::connector::serve` loop over a scripted service: the same connector the
//! system ships, answering a script instead of YouTube.
//!
//! What this proves is the whole path and nothing test-only about it: a search action an
//! instance runs queues an outside effect, the session hands the effect to the connector over
//! the socket it started, and the answer lands in the client's record — its videos and the
//! search's settled state — which the session publishes and its door reads.
//!
//! Before this work the session started no connector at all: the effect stayed in the
//! instance's own process, and nothing outside ever asked YouTube.

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
use std::time::Duration;

use serde_json::json;
use tilo_compositor::OutputConfig;
use tilo_service::Questions;
use tilo_session::{ActionPlane, AppActions, Launches, Options, Session};

use common::{HEIGHT, PERSON, WIDTH, private_home};

/// The YouTube client, as a package names it.
const TUBE: &str = "so.tilo.lang.tube";
/// The channel the scripted YouTube service answers with.
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
    // The launcher's release: the descriptor the connector's door came in on.
    if runtime_portal::registration_gate::wait_on_stdin().is_err() {
        return;
    }
    let door = runtime_portal::origin_connector::take_door().expect("the connector's door");
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
    tilolang_youtube::connector::serve(tilolang_youtube::scripted(scripted()), attachments);
}

/// The scripted YouTube service: one search's answer.
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
                    ..tilolang_youtube::VideoItem::default()
                }],
            },
        )]
        .into_iter()
        .collect(),
        ..tilolang_youtube::Scripted::default()
    }
}

/// Install the YouTube client's package in `home`'s app store, its manifest declaring the
/// `youtube` origin, and write the launcher the session starts it through: the stand-in runs
/// this test binary as the connector process, on the gate the launcher is handed.
fn installed_tube(home: &tilo_test_env::TestHome) -> tilo_test_env::StandIn {
    let store = home.data().join("tilo-apps");
    let payload = store.join("payload").join(TUBE);
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    std::fs::write(payload.join("app.wasm"), b"").unwrap();
    std::fs::write(payload.join("app.ids"), b"").unwrap();
    let manifest = payload.join("manifest.toml");
    std::fs::write(
        &manifest,
        format!(
            "[package]\n\
             id = \"{TUBE}\"\n\
             version = \"1.0.0\"\n\
             kind = \"app\"\n\
             name = \"YouTube\"\n\
             \n\
             [publisher]\n\
             identity = \"ed25519:{key}\"\n\
             signature = \"\"\n\
             \n\
             [sandbox]\n\
             namespaces = [\"user\", \"net\"]\n\
             seccomp = \"tilo-default\"\n\
             landlock = \"strict\"\n\
             \n\
             [sandbox.net]\n\
             mode = \"host\"\n\
             \n\
             [entrypoint]\n\
             runtime = \"tilolang/0.1.0+tilolang\"\n\
             bin = \"bin/tilolang-app\"\n\
             type = \"gui\"\n\
             origins = [\"youtube\"]\n\
             \n\
             [providers]\n",
            key = "22".repeat(32),
        ),
    )
    .unwrap();
    std::fs::write(
        store.join("registry.toml"),
        format!(
            "[[entry]]\nid = \"{TUBE}\"\nversion = \"1.0.0\"\n\
             payload_root = \"{payload}\"\nmanifest_path = \"{manifest}\"\n",
            payload = payload.display(),
            manifest = manifest.display(),
        ),
    )
    .unwrap();
    // The launcher the session starts connectors through: it runs this test binary as the
    // connector process, keeping the gate it was handed as stdin.
    tilo_test_env::stand_in(
        home.home().join("tilo-launcher"),
        &format!(
            "export {CHILD_ENV}=1\n\
             exec '{exe}' --exact origin_connector::youtube_connector_child --nocapture\n",
            exe = std::env::current_exe()
                .expect("this test's program")
                .display(),
        ),
    )
    .unwrap()
}

/// A headless session with the shell as the whole screen and the seeded apps published (the
/// YouTube client among them), its apps started through `home`'s launcher.
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
    let seeded = tilo_session::seeded_apps::seeded(PERSON).expect("the seeded apps");
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
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
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session")
}

/// The published reading of the client's root, as the session's door reads it.
fn youtube_root(session: &Session) -> Option<serde_json::Value> {
    session
        .resources()
        .into_iter()
        .find(|resource| resource.kind == format!("tl.{TUBE}.YouTube"))
        .and_then(|resource| session.read(resource.id.as_str()))
}

#[test]
fn an_outside_effect_reaches_the_connector_and_the_answer_lands_in_the_record() {
    let _settled = ui::motion::testing::settled();
    let home = tilo_test_env::TestHome::new().expect("a home");
    let launcher = installed_tube(&home);
    let root = private_home();
    let mut session = session(&home, root.path(), &launcher);

    // The client's root is published by the seeded apps, with its search settled as it stood.
    let before = common::wait_for(&mut session, "the client's root", |session| {
        youtube_root(session)
    });
    assert_eq!(
        before["search_state"], "",
        "nothing was searched yet: {before}"
    );

    // The person's search, through the session's door as any action: the outside effect it
    // queues goes to the connector the session started for the app's origin.
    let created = session
        .invoke(
            youtube_root_id(&session).as_str(),
            "search",
            &serde_json::Map::from_iter([("query".to_owned(), json!("compositor"))]),
        )
        .expect("the search action runs");

    // The answer lands in the client's record: the video the scripted service found, and the
    // search settled (so its view never reads "searching" for ever).
    let after = common::wait_for(&mut session, "the search answered", |session| {
        youtube_root(session).filter(|root| root["search_state"] == "done")
    });
    assert_eq!(after["search_state"], "done", "{after}");
    assert!(
        after["videos"].as_array().is_some_and(|videos| {
            videos
                .iter()
                .any(|video| video["value"]["title"] == json!(VIDEO))
        }),
        "the video YouTube answered with is in the record: {after}"
    );
    // The videos were found for the query, so a search's view lists them.
    assert!(
        after["videos"]
            .as_array()
            .expect("videos")
            .iter()
            .any(|video| video["value"]["found_for"] == json!("compositor")),
        "{after}"
    );
    let _ = created;
}

/// The published id of the client's root.
fn youtube_root_id(session: &Session) -> String {
    session
        .resources()
        .into_iter()
        .find(|resource| resource.kind == format!("tl.{TUBE}.YouTube"))
        .map(|resource| resource.id.as_str().to_owned())
        .expect("the client's root")
}
