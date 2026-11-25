//! A window with no session browsing a video site: the window runs its instance here, so it has
//! no network of its own, and reaches `YouTube` through the connector process this launch starts
//! for the app's `youtube` origin (`TILO_TILOLANG_CONNECTOR`), as a session's host reaches the
//! connector its daemon runs. The child this test starts is the connector: the platform's own
//! connector loop over a scripted service, on the socket the window handed it, so what reaches
//! the record is what the connector answered.
//!
//! The service's client lives in the connector's engine library in a real runtime bundle; the
//! child here answers from a script, as the session's connector test does, and everything else
//! is the production path: the window's socket pair, the connector's role, and the origin that
//! writes the answers into the app's record.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion"
)]

use serde_json::json;
use tilo_app_test_host::TestApp;
use tilolang_process::window::app::TlApp;

/// The app's directory, which `support::installed` reads the package from.
const TUBE_DIR: &str = "tube";
/// The video the scripted service answers a search with.
const VIDEO: &str = "v-compositor";
/// Its title, as the window names it.
const TITLE: &str = "I rewrote my window compositor in Rust";
/// The address the scripted service says its bytes play from.
const STREAM: &str = "https://media.example/v-compositor.mp4";
/// The channel it belongs to.
const CHANNEL: &str = "Low Level Learning";

type TubeApp = TestApp<TlApp>;

/// The connector process this window starts: this test binary, run with the origin the window
/// names and the socket it handed over.
fn connector_process() {
    use runtime_portal::origin_connector;
    let origin =
        std::env::var(origin_connector::ORIGIN_ENV).expect("the window names the origin it starts");
    let socket = origin_connector::take_socket().expect("the window handed the connector a socket");
    let attachments = origin_connector::attachments(socket, |failed| {
        if let Some(error) = failed {
            eprintln!("the window's socket failed: {error}");
        }
    });
    match origin.as_str() {
        "youtube" => {
            tilolang_youtube::connector::serve(
                &tilolang_youtube::scripted(scripted()),
                attachments,
            );
        }
        other => panic!("the window started no connector this test stands in for: {other}"),
    }
}

/// What the scripted `YouTube` answers: one search's page, and the video's stream.
fn scripted() -> tilolang_youtube::Scripted {
    tilolang_youtube::Scripted {
        searches: [(
            "compositor".to_owned(),
            tilolang_youtube::SearchPage {
                query: "compositor".to_owned(),
                channels: Vec::new(),
                playlists: Vec::new(),
                videos: vec![tilolang_youtube::VideoItem {
                    key: VIDEO.to_owned(),
                    title: TITLE.to_owned(),
                    channel_key: "UC-lowlevel".to_owned(),
                    channel_name: CHANNEL.to_owned(),
                    published: "2026-11-01T12:00:00Z".to_owned(),
                    duration: 900,
                    views: 12_000,
                    ..tilolang_youtube::VideoItem::default()
                }],
            },
        )]
        .into_iter()
        .collect(),
        streams: [(VIDEO.to_owned(), STREAM.to_owned())]
            .into_iter()
            .collect(),
        ..tilolang_youtube::Scripted::default()
    }
}

/// The frames `app` runs until it does `done`, and what it showed when it never did.
fn until(app: &mut TubeApp, what: &str, mut done: impl FnMut(&mut TubeApp) -> bool) {
    let began = std::time::Instant::now();
    while began.elapsed() < std::time::Duration::from_secs(60) {
        app.settle();
        if done(app) {
            return;
        }
        app.frame();
        app.harness()
            .wait_until(ui::time::Duration::from_millis(10), |_| false);
    }
    panic!("the window never {what}; it shows {:?}", app.texts());
}

/// The resource `app` publishes `action` on, once it does: the one whose record holds `marker`
/// when a marker is named (the video the search found), else any.
fn published(app: &mut TubeApp, action: &str, marker: Option<&str>) -> String {
    let began = std::time::Instant::now();
    while began.elapsed() < std::time::Duration::from_secs(60) {
        app.settle();
        let surface = app.agent_surface();
        for row in surface.as_array().into_iter().flatten() {
            if row["action"] != action {
                continue;
            }
            let Some(resource) = row["resource"].as_str() else {
                continue;
            };
            let holds = marker.is_none_or(|marker| {
                ui_core::resource::DocumentId::parse(resource)
                    .and_then(|id| app.resource_document(&id))
                    .is_some_and(|bytes| String::from_utf8_lossy(&bytes).contains(marker))
            });
            if holds {
                return resource.to_owned();
            }
        }
        app.frame();
        app.harness()
            .wait_until(ui::time::Duration::from_millis(10), |_| false);
    }
    panic!(
        "the window never offers `{action}`; it shows {:?}",
        app.texts()
    );
}

#[test]
fn a_window_without_a_session_browses_and_plays_through_the_connector() {
    // The child the window starts as its connector.
    if std::env::var_os(runtime_portal::origin_connector::ORIGIN_ENV).is_some() {
        connector_process();
        return;
    }
    let profile = std::env::temp_dir().join(format!("tilo-tube-origins-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&profile);
    crate::support::installed(TUBE_DIR);
    let mut app = TubeApp::open_data_dir(&profile).expect("the window opens");
    let root = published(&mut app, "search", None);
    app.call_on(&root, "search", json!({ "query": "compositor" }))
        .expect("the client searches");
    // The answer reached the record through the connector, and the window shows it.
    until(&mut app, "shows the video the search found", |app| {
        app.texts().iter().any(|text| text.contains(TITLE))
    });
    // Playing it asks the connector for the address its bytes play from, and the record keeps
    // what the connector answered.
    let video = published(&mut app, "progress", Some(TITLE));
    app.call_on(&video, "progress", json!({ "at": 0.0 }))
        .expect("the client plays the video");
    until(&mut app, "plays the video's stream", |app| {
        ui_core::resource::DocumentId::parse(&video)
            .and_then(|id| app.resource_document(&id))
            .is_some_and(|bytes| String::from_utf8_lossy(&bytes).contains(STREAM))
    });
    drop(app);
    let _ = std::fs::remove_dir_all(&profile);
}
