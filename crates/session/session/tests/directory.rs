//! "Join a workspace…" as a person on a computer set up for a community
//! meets it: opening the sheet reads that site's directory and lists the
//! public workspaces it serves, who is in each one now and how many belong.
//! The workspaces this computer holds open instead of joining, and a site
//! that cannot be read says so without hiding them.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use test_http::{Reply, Responder};
use tilo_compositor::OutputConfig;
use tilo_desktop::desktop_actions::CHROME_RESOURCE;
use tilo_relay_protocol::directory::{DirectoryEntry, DirectoryPage, Face, door_path};
use tilo_relay_protocol::rooms::RoomRole;
use tilo_session::{Options, Session};
use tilo_sheets::Seat as _;
use tilo_sheets::directory::{Listing, ListingAction, Status};

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;

const COMMUNITY_ROOM: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
const CLUB_ROOM: &str = "1b7c4e08-3a5d-42f6-8c9e-2d6f0a1b3c5e";

/// A session on the headless backend, taking no part in the collaboration
/// plane.
fn session() -> Session {
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session")
}

/// Set this computer's community site to `site` for the test's lifetime.
/// Returns the file and the scope; the file goes when the scope does.
fn community_site(site: &str) -> (tempfile::TempDir, tilo_test_env::ScopedEnv) {
    let dir = tempfile::tempdir().expect("a directory for the configuration");
    let config = dir.path().join("community.toml");
    std::fs::write(&config, format!("site = \"{site}\"\n")).expect("the configuration is written");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_COMMUNITY_CONFIG", &config);
    (dir, env)
}

/// The workspaces this computer holds: id, name, room.
fn held(session: &Session) -> Vec<(String, String, String)> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .map(|workspace| {
                (
                    workspace.id.0.to_string(),
                    workspace.name.clone(),
                    workspace.room().to_string(),
                )
            })
            .collect()
    })
}

/// Open "Join a workspace…" and wait until the directories answered.
fn open_directory(session: &mut Session) {
    session
        .invoke(CHROME_RESOURCE, "join-workspace", &serde_json::Map::new())
        .expect("the session opens the directory");
    session.step(Duration::from_millis(1)).unwrap();
    let deadline = std::time::Instant::now()
        .checked_add(Duration::from_secs(20))
        .expect("a deadline");
    loop {
        let standing = session
            .workspace_directory()
            .expect("the session hosts the workspace directory")
            .directory()
            .status;
        if standing != Status::Loading {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the directory answered"
        );
        session.step(Duration::from_millis(10)).unwrap();
    }
}

/// The rows the sheet lists: name, action, key.
fn rows(session: &Session) -> Vec<(String, ListingAction, String)> {
    session
        .workspace_directory()
        .expect("the session hosts the workspace directory")
        .directory()
        .listings
        .iter()
        .map(|listing| (listing.name.clone(), listing.action, listing.key.clone()))
        .collect()
}

fn listing(session: &Session, name: &str) -> Listing {
    session
        .workspace_directory()
        .expect("the session hosts the workspace directory")
        .directory()
        .listings
        .into_iter()
        .find(|listing| listing.name == name)
        .unwrap_or_else(|| panic!("the sheet lists {name}"))
}

fn page_of(entries: Vec<DirectoryEntry>) -> String {
    serde_json::to_string(&DirectoryPage {
        workspaces: entries,
    })
    .expect("a directory page")
}

fn face(name: &str, key: &str) -> Face {
    Face {
        user_public_key: key.to_owned(),
        name: name.to_owned(),
    }
}

fn community_entry(members: u32, now_ms: u64) -> DirectoryEntry {
    DirectoryEntry {
        room: COMMUNITY_ROOM.to_owned(),
        name: "Tilers".to_owned(),
        description: "Anyone who lays tiles".to_owned(),
        default_role: RoomRole::Editor,
        door: door_path(COMMUNITY_ROOM),
        here: vec![face("Ada", "a1b2"), face("Bo", "c3d4")],
        here_count: 2,
        members,
        last_active_ms: Some(now_ms),
        community: true,
    }
}

fn club_entry(now_ms: u64) -> DirectoryEntry {
    /// Three days ago: this week, not today.
    const THREE_DAYS_MS: u64 = 259_200_000;
    let week = THREE_DAYS_MS;
    DirectoryEntry {
        room: CLUB_ROOM.to_owned(),
        name: "Book club".to_owned(),
        description: String::new(),
        default_role: RoomRole::Viewer,
        door: door_path(CLUB_ROOM),
        here: Vec::new(),
        here_count: 0,
        members: 5,
        last_active_ms: Some(now_ms.saturating_sub(week)),
        community: false,
    }
}

fn now_ms() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("this computer's clock is past 1970")
            .as_millis(),
    )
    .expect("a time in milliseconds")
}

/// A site that serves `entry.json` naming a directory, and the page the
/// test puts in it.
fn site() -> (Responder, Arc<Mutex<String>>) {
    let page = Arc::new(Mutex::new(String::new()));
    let served = Arc::clone(&page);
    let responder = Responder::handle(move |request| {
        let body = match (request.method.as_str(), request.target.as_str()) {
            ("GET", "/entry.json") => r#"{"directory":"directory"}"#.to_owned(),
            ("POST", "/directory") => served.lock().expect("the page").clone(),
            _ => return Some(Reply::status(404)),
        };
        Some(
            Reply::status(200)
                .header("content-type", "application/json")
                .body(body),
        )
    });
    (responder, page)
}

#[test]
fn opening_the_directory_lists_the_community_sites_public_workspaces() {
    let _settled = ui::motion::testing::settled();
    let (responder, page) = site();
    let (_config, _community) = community_site(&responder.url());
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    let (held_id, held_name, held_room) = held(&session)
        .into_iter()
        .next()
        .expect("the desk holds one workspace");
    // What the site lists: its own community workspace, a club, and the
    // workspace this computer holds already.
    let mut entries = vec![community_entry(34, now_ms()), club_entry(now_ms())];
    entries.push(DirectoryEntry {
        room: held_room.clone(),
        name: held_name.clone(),
        description: "The one on this desk".to_owned(),
        default_role: RoomRole::Commenter,
        door: door_path(&held_room),
        here: Vec::new(),
        here_count: 0,
        members: 3,
        last_active_ms: Some(now_ms()),
        community: false,
    });
    *page.lock().expect("the page") = page_of(entries);
    open_directory(&mut session);
    let sheet = session
        .workspace_directory()
        .expect("the session hosts the workspace directory");
    assert_eq!(
        sheet.directory().status,
        Status::Listed,
        "the site's directory was read: {:?}",
        sheet.directory().note
    );
    assert!(
        sheet.presentation().is_open(),
        "and the sheet is up to show it"
    );
    assert_eq!(
        rows(&session),
        [
            (
                "Tilers".to_owned(),
                ListingAction::Join,
                COMMUNITY_ROOM.to_owned()
            ),
            (held_name, ListingAction::Open, held_id),
            (
                "Book club".to_owned(),
                ListingAction::Join,
                CLUB_ROOM.to_owned()
            ),
        ],
        "the community's own first, then the liveliest, and the held workspace opens rather than joins"
    );
    let community = listing(&session, "Tilers");
    assert_eq!(
        community.about, "Anyone who lays tiles",
        "what it is, in its own words"
    );
    assert_eq!(
        community.tag.as_deref(),
        Some("127.0.0.1's own"),
        "and whose own workspace it is"
    );
    assert_eq!(
        community.here,
        [
            ("Ada".to_owned(), "a1b2".to_owned()),
            ("Bo".to_owned(), "c3d4".to_owned())
        ],
        "who is in it now"
    );
    assert_eq!(community.here_count, 2, "and how many in all");
    assert_eq!(
        community.activity, "2 here now · 34 members",
        "how lively it is and how many belong"
    );
    assert_eq!(
        community.joins_as, "You join as an editor",
        "what joining gets the person"
    );
    let club = listing(&session, "Book club");
    assert_eq!(
        club.about, "127.0.0.1",
        "a workspace that says nothing of itself is named by its host"
    );
    assert_eq!(
        club.activity, "Active this week · 5 members",
        "and says when it last had something new"
    );
    assert_eq!(
        club.joins_as, "You join as a viewer",
        "its own default role"
    );
    assert!(club.here.is_empty(), "nobody is in it now");
    assert_eq!(club.tag, None, "it is not the site's own workspace");
}

#[test]
fn a_site_that_cannot_be_read_says_so_and_still_lists_what_is_held() {
    let _settled = ui::motion::testing::settled();
    // A site with nothing at its entry.
    let responder = Responder::handle(|_| Some(Reply::status(404)));
    let (_config, _community) = community_site(&responder.url());
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    let kept = held(&session);
    open_directory(&mut session);
    let sheet = session
        .workspace_directory()
        .expect("the session hosts the workspace directory");
    assert_eq!(
        sheet.directory().status,
        Status::Failed("that site has no workspace".to_owned()),
        "the sheet says why nothing was listed"
    );
    assert_eq!(
        rows(&session),
        kept.iter()
            .map(|(id, name, _)| (name.clone(), ListingAction::Open, id.clone()))
            .collect::<Vec<_>>(),
        "and the workspaces this computer holds are still there to open"
    );
}

#[test]
fn no_community_site_lists_the_workspaces_this_computer_holds() {
    let _settled = ui::motion::testing::settled();
    let (_config, _community) = community_site("");
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    let kept = held(&session);
    open_directory(&mut session);
    let sheet = session
        .workspace_directory()
        .expect("the session hosts the workspace directory");
    assert_eq!(
        sheet.directory().status,
        Status::Listed,
        "a computer with no community site has nothing to read, not a failure"
    );
    assert_eq!(
        rows(&session),
        kept.iter()
            .map(|(id, name, _)| (name.clone(), ListingAction::Open, id.clone()))
            .collect::<Vec<_>>(),
        "it lists what it holds"
    );
}
