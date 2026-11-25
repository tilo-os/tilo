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
/// The file, in the profile the window opened, the connector writes the asks it answers to.
const ASKS: &str = "connector-asks";

type TubeApp = TestApp<TlApp>;

/// The one test a window starts this binary to run as its connector
/// (`window::app`'s `connector_command`): nothing else of the window's runs there. A run of the
/// whole binary, which starts no connector, has nothing to stand in for.
#[test]
fn the_connector_process() {
    if std::env::var_os(runtime_portal::origin_connector::ORIGIN_ENV).is_none() {
        return;
    }
    connector_process();
}

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
            let service: std::sync::Arc<dyn tilolang_youtube::Service> =
                std::sync::Arc::new(Asked {
                    service: scripted(),
                    log: connector_log(),
                });
            tilolang_youtube::connector::serve(&service, attachments);
        }
        other => panic!("the window started no connector this test stands in for: {other}"),
    }
}

/// The file the connector writes each ask it answers to, beside the profile the window opened:
/// the window's own process is this child's parent, so both name the same directory.
fn asked_log(profile: &std::path::Path) -> std::path::PathBuf {
    profile.join(ASKS)
}

/// [`asked_log`] as the connector's process names it, from the process that started it.
fn connector_log() -> std::path::PathBuf {
    #[cfg(unix)]
    let parent = std::os::unix::process::parent_id();
    #[cfg(not(unix))]
    let parent = 0;
    asked_log(&std::env::temp_dir().join(format!("tilo-tube-origins-{parent}")))
}

/// The asks the connector answered, oldest first: `search <query>`, `stream <key>`.
fn asks(profile: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(asked_log(profile)).map_or_else(
        |_| Vec::new(),
        |log| log.lines().map(str::to_owned).collect(),
    )
}

/// Whether the app's store under `profile` keeps `text` in the record of the window's resource
/// `resource` (`tl:<app>:<document>/<item>`): each resource of the documents the store keeps that
/// record in (its own, and those of its collections), read through the store's own reads as it
/// opens again from what the window's instance wrote. `false` until the store holds them whole.
fn record_keeps(profile: &std::path::Path, resource: &str, text: &str) -> bool {
    let Some((app, path)) = resource
        .strip_prefix("tl:")
        .and_then(|rest| rest.split_once(':'))
    else {
        return false;
    };
    let record = path.split('/').next().unwrap_or(path);
    let Some(store) = stores_under(profile)
        .into_iter()
        .find(|dir| dir.parent().is_some_and(|parent| parent.ends_with(app)))
    else {
        return false;
    };
    let Ok(store) = tilolang_tilo::resources::LocalPort::at(store) else {
        return false;
    };
    use tilolang_tilo::resources::Port as _;
    store
        .documents()
        .iter()
        .filter(|document| document.contains(record))
        .flat_map(|document| store.held(document).unwrap_or_default())
        .flat_map(|(kind, id)| {
            [
                collab_types::store::wire::Command::Get {
                    kind: kind.clone(),
                    id: id.clone(),
                },
                collab_types::store::wire::Command::Content { kind, id },
            ]
        })
        .filter_map(|read| store.command(read).ok())
        .any(|answer| format!("{answer:?}").contains(text))
}

/// Every store directory (`resources`) under `dir`.
fn stores_under(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for path in entries.flatten().map(|entry| entry.path()) {
        if !path.is_dir() {
            continue;
        }
        if path.file_name().is_some_and(|name| name == "resources") {
            found.push(path);
        } else {
            found.extend(stores_under(&path));
        }
    }
    found
}

/// The scripted service, with each ask it answers written down: what the window asked the
/// connector for, which the window's own record is not readable for.
struct Asked {
    service: tilolang_youtube::Scripted,
    log: std::path::PathBuf,
}

impl Asked {
    /// Note the ask `line`, for the test that started this connector.
    fn noted(&self, line: &str) {
        use std::io::Write as _;
        if let Ok(mut log) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
        {
            let _ = writeln!(log, "{line}");
        }
    }
}

impl tilolang_youtube::Service for Asked {
    fn search(
        &self,
        query: &str,
        filter: &tilolang_youtube::SearchFilter,
    ) -> Result<tilolang_youtube::SearchPage, tilolang_youtube::ServiceError> {
        self.noted(&format!("search {query}"));
        self.service.search(query, filter)
    }

    fn channel(
        &self,
        key: &str,
    ) -> Result<tilolang_youtube::ChannelPage, tilolang_youtube::ServiceError> {
        self.service.channel(key)
    }

    fn playlist(
        &self,
        key: &str,
    ) -> Result<tilolang_youtube::PlaylistPage, tilolang_youtube::ServiceError> {
        self.service.playlist(key)
    }

    fn trending(&self) -> Result<tilolang_youtube::TrendingPage, tilolang_youtube::ServiceError> {
        self.service.trending()
    }

    fn stream(&self, key: &str, quality: &str) -> Result<String, tilolang_youtube::ServiceError> {
        let answered = self.service.stream(key, quality);
        match &answered {
            Ok(address) => self.noted(&format!("stream {key} {address}")),
            Err(_) => self.noted(&format!("stream {key} failed")),
        }
        answered
    }

    fn comments(
        &self,
        key: &str,
    ) -> Result<tilolang_youtube::CommentsPage, tilolang_youtube::ServiceError> {
        self.service.comments(key)
    }

    fn segments(
        &self,
        key: &str,
    ) -> Result<tilolang_youtube::SegmentPage, tilolang_youtube::ServiceError> {
        self.service.segments(key)
    }

    fn branding(
        &self,
        key: &str,
    ) -> Result<tilolang_youtube::Branding, tilolang_youtube::ServiceError> {
        self.service.branding(key)
    }

    fn votes(&self, key: &str) -> Result<tilolang_youtube::Votes, tilolang_youtube::ServiceError> {
        self.service.votes(key)
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

/// `count` frames of the window, each settling what the last brought in: the settles a window
/// goes through while it waits on a connector's answer.
fn frames(app: &mut TubeApp, count: usize) {
    for _ in 0..count {
        app.settle();
        app.frame();
        app.harness()
            .wait_until(ui::time::Duration::from_millis(10), |_| false);
    }
}

/// The resource `app` publishes `action` on, once it does: the one the window names with `marker`
/// when a marker is named (the video the search found, by its title), else any.
fn published(app: &mut TubeApp, action: &str, marker: Option<&str>) -> String {
    let began = std::time::Instant::now();
    while began.elapsed() < std::time::Duration::from_secs(60) {
        app.settle();
        // The resources the window publishes, each under the name the app calls it by: a video's
        // is its title.
        let named: Vec<(String, String)> = app
            .harness()
            .resources()
            .iter()
            .map(|resource| (resource.id.as_str().to_owned(), resource.label.clone()))
            .collect();
        let surface = app.agent_surface();
        for row in surface.as_array().into_iter().flatten() {
            if row["action"] != action {
                continue;
            }
            let Some(resource) = row["resource"].as_str() else {
                continue;
            };
            let holds = marker.is_none_or(|marker| {
                named
                    .iter()
                    .any(|(id, label)| id == resource && label.contains(marker))
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
    app.call_on_finished(&root, "search", json!({ "query": "compositor" }))
        .expect("the client searches");
    // The answer reached the record through the connector, and the window shows it.
    until(&mut app, "shows the video the search found", |app| {
        app.texts().iter().any(|text| text.contains(TITLE))
    });
    // The connector answered `Pending`: the window waits on it, and many settles while and
    // after the answer landed do not ask it again.
    frames(&mut app, 20);
    let searched = "search compositor";
    assert_eq!(
        asks(&profile).iter().filter(|line| *line == searched).count(),
        1,
        "the connector is asked a search once: {:?}",
        asks(&profile)
    );
    // Playing it asks the connector for the address its bytes play from: the address only the
    // scripted service names, answered for the video the window plays, and the record keeps what
    // the connector answered.
    let video = published(&mut app, "progress", Some(TITLE));
    app.call_on_finished(&video, "progress", json!({ "at": 0.0 }))
        .expect("the client plays the video");
    let streamed = format!("stream {VIDEO} {STREAM}");
    until(
        &mut app,
        "asks the connector where the video plays from",
        |_| asks(&profile).contains(&streamed),
    );
    until(&mut app, "keeps the video's stream in its record", |_| {
        record_keeps(&profile, &video, STREAM)
    });
    frames(&mut app, 20);
    assert_eq!(
        asks(&profile)
            .iter()
            .filter(|line| **line == streamed)
            .count(),
        1,
        "the connector is asked the stream once: {:?}",
        asks(&profile)
    );
    // A read the app refuses comes back refused, not as an empty success.
    let refused = app
        .call_on_finished(&video, "read", json!({ "version": "an-older-one" }))
        .expect_err("a past version is read from the app's versions");
    assert!(
        refused.to_string().contains("versions"),
        "the caller hears the app's own refusal: {refused}"
    );
    drop(app);
    let _ = std::fs::remove_dir_all(&profile);
}
