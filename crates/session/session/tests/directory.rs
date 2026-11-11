//! "Join a workspace…" as a person on a computer set up for a community meets it: opening the sheet
//! reads that site's directory and lists the public workspaces it serves, who is in each one now
//! and how many belong. The workspaces this computer holds open instead of joining, and a site that
//! cannot be read says so without hiding them.
//! Joining one enters through the door its listing names, as the person at this computer and by
//! their own name, on the session this computer opens with the door's host: the workspace admitting
//! it is what puts the workspace on the desk, one it holds from then on, open rather than to join,
//! with everything the workspace held before this computer arrived. A door that refuses says why,
//! where the person pressed, and leaves the workspace one to try again.
//! A link or a code pasted into the sheet joins the same way: an invitation's text is redeemed
//! where it names, and a workspace's link joins the workspace on the site it points at.
//! A computer set up for a community (`community.toml`) is in it already: its session enters the
//! community's own door as it starts, through the same join, and asks once more when the machine's
//! network comes back while the join is still waiting. What the person decided — joined, or joined
//! and left — is theirs, and keeps every computer of theirs from joining it again on its own.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion"
)]

mod common;
#[path = "directory/community.rs"]
mod community;
#[path = "directory/sheet.rs"]
mod sheet;

use std::sync::{Arc, Mutex};
use std::time::Duration;
use tilo_session_client::{Cmd, Link, Outbox};

use automerge::ReadDoc as _;
use automerge::transaction::Transactable as _;
use collab_types::DocumentMutation;
use test_http::{Reply, Responder};
use tilo_desktop::desk_verbs::CHROME_RESOURCE;
use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_RECORD, SharedDesktop};
use tilo_desktop_model::{
    GroupId, Strip, StripId, Tab, TabId, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceSlot, shared_record,
};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_relay::workspaces::RemoveMemberRequest;
use tilo_relay_client::Host;
use tilo_relay_protocol::directory::{DirectoryEntry, DirectoryPage, Face, door_path};
use tilo_relay_protocol::workspaces::WorkspaceRole;
use tilo_session::{Options, Session};
use tilo_session_host::{MemoryPeerStore, PeerConfig};
use tilo_sheets::Seat as _;
use tilo_sheets::directory::{Listing, ListingAction, Status};

// A host admits its members' records through an Automerge worker: this binary is that worker, as it
// is the session's own.
tilo_automerge_isolate::test_worker!();

/// The app the lounge's workspace talks in, and the record its people wrote there before this
/// computer joined: the chat's own workspace record.
const CHAT: &str = "so.tilo.chat";
const CHAT_RECORD: &str = "";
/// How long the lounge's chat ran before this computer joined: long enough that the host asks the
/// page that wrote it for the record, as it does once a workspace's sealed history is a fair
/// length.
const HISTORY: i64 = 300;
/// Twelve hours, in nanoseconds: how long the certificate a session starts with has been out.
const TWELVE_HOURS_NS: u64 = 43_200_000_000_000;
/// A day and a half, in nanoseconds: how long ago the certificate a session starts with was issued.
const A_DAY_AND_A_HALF_NS: u64 = 129_600_000_000_000;

const COMMUNITY_WORKSPACE: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
const CLUB_WORKSPACE: &str = "1b7c4e08-3a5d-42f6-8c9e-2d6f0a1b3c5e";

/// A session on the headless backend, taking no part in the collaboration plane.
fn session() -> Session {
    crate::common::headless(Options {
        collab: false,
        ..Options::default()
    })
}

/// Point this computer at the community site `site` for the test's lifetime. Returns the
/// configuration's directory, which goes with it.
fn community_site(env: &mut tilo_test_env::ScopedEnv, site: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a directory for the configuration");
    let config = dir.path().join("community.toml");
    std::fs::write(&config, format!("site = \"{site}\"\n")).expect("the configuration is written");
    env.set("TILO_COMMUNITY_CONFIG", &config);
    dir
}

/// The workspaces this computer holds: the desk's id, the name shown and the workspace's id.
fn held(session: &Session) -> Vec<(String, String, String)> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .map(|held| {
                (
                    held.slot.0.to_string(),
                    held.name.clone(),
                    held.id().to_string(),
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
    wait_until_the_directory_answered(session);
}

/// Run the session until the sheet's directories answered the read it is on.
fn wait_until_the_directory_answered(session: &mut Session) {
    // The site's directories are answered by other runtimes, and a busy machine takes as long at
    // that as the suite allows, not a second count.
    let deadline = tilo_test_env::hang_deadline();
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

/// Read the site `site` in the open sheet, as the person typing its address into its search field
/// does, and wait until it answered.
fn look_up_site(session: &mut Session, site: &str) {
    let sheet = publisher(session, "look").expect("the directory sheet looks a site up");
    let arguments = serde_json::json!({ "text": site })
        .as_object()
        .expect("an object")
        .clone();
    session
        .invoke(&sheet, "look", &arguments)
        .expect("the session reads the site");
    session.step(Duration::from_millis(1)).unwrap();
    wait_until_the_directory_answered(session);
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

/// Write what the session shows where a person can look at it, when the run asked for shots:
/// `TILO_SURFACE_SHOTS` names where they go.
fn capture(session: &mut Session, name: &str) {
    let Some(directory) = std::env::var_os("TILO_SURFACE_SHOTS").map(std::path::PathBuf::from)
    else {
        return;
    };
    // Draw what the sheet holds now: a site's listings arrive after the sheet opens, and the frame
    // on screen may still be the one before them.
    for _ in 0..3 {
        session
            .step(std::time::Duration::from_millis(1))
            .expect("step");
    }
    let frame = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("a frame of the session");
    let pixels = image::RgbaImage::from_raw(frame.width, frame.height, frame.rgba)
        .expect("the frame's pixels fill it");
    let mut png = Vec::new();
    pixels
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("the shot");
    std::fs::create_dir_all(&directory).expect("the directory for shots");
    std::fs::write(directory.join(format!("{name}.png")), png).expect("the shot");
}

/// What the desk says to the person now, each notice's title and body, live or kept behind its
/// banner.
fn said(session: &Session) -> Vec<String> {
    let desk = session.notifications();
    let log = desk.log();
    log.active()
        .iter()
        .chain(log.history())
        .map(|notice| format!("{}: {}", notice.title, notice.body))
        .collect()
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
        workspace: COMMUNITY_WORKSPACE.to_owned(),
        name: "Tilers".to_owned(),
        description: "Anyone who lays tiles".to_owned(),
        default_role: WorkspaceRole::Editor,
        door: door_path(COMMUNITY_WORKSPACE),
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
        workspace: CLUB_WORKSPACE.to_owned(),
        name: "Book club".to_owned(),
        description: String::new(),
        default_role: WorkspaceRole::Viewer,
        door: door_path(CLUB_WORKSPACE),
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

/// A site that serves `entry.json` naming a directory, and the page the test puts in it.
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

/// A site that lists `entries` and answers nothing else: a door is entered on a session with its
/// host, never asked here.
fn joining_site(entries: Vec<DirectoryEntry>) -> Responder {
    let page = page_of(entries);
    Responder::handle(move |request| {
        let body = match (request.method.as_str(), request.target.as_str()) {
            ("GET", "/entry.json") => r#"{"directory":"directory"}"#.to_owned(),
            ("POST", "/directory") => page.clone(),
            _ => return Some(Reply::status(404)),
        };
        Some(
            Reply::status(200)
                .header("content-type", "application/json")
                .body(body),
        )
    })
}

/// This computer's person: a device of theirs, certified for their own root key, kept in an
/// identity root of the test's own. Returns the root's directory, and the key a door's invitation
/// for them is bound to.
fn this_computer(env: &mut tilo_test_env::ScopedEnv) -> (tempfile::TempDir, [u8; 32]) {
    let person = ed25519_dalek::SigningKey::from_bytes(&[0x21; 32]);
    let device = ed25519_dalek::SigningKey::from_bytes(&tilo_test_env::node_key());
    let root = a_device(env, &person, &device, "The laptop");
    (root, person.verifying_key().to_bytes())
}

/// A computer of `person` 's: the device `device` , certified for their root key and kept in an
/// identity root of the test's own, which is this computer's from here on. Returns the root's
/// directory.
fn a_device(
    env: &mut tilo_test_env::ScopedEnv,
    person: &ed25519_dalek::SigningKey,
    device: &ed25519_dalek::SigningKey,
    label: &str,
) -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("a directory for this computer's identity");
    let paths = tilo_user_identity::IdentityPaths::under(root.path());
    std::fs::create_dir_all(paths.device_privkey.parent().expect("the key's directory"))
        .expect("this computer's key directory");
    tilo_user_identity::persist::write_device_key(&paths.device_privkey, device)
        .expect("this computer's device key");
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: person.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: label.to_owned(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, person).expect("a certified device");
    tilo_user_identity::persist::write_device_cert(&paths.device_cert, &cert)
        .expect("this computer's certificate");
    env.set("TILO_IDENTITY_ROOT", root.path());
    // The name they go by, which the workspace learns of them: never their device's.
    ui::local_identity::install_source(Some(Arc::new(ui::local_identity::FixedIdentity(
        ui::local_identity::LocalIdentity {
            group_public_key: cert.user_public_key,
            display_name: "Ada".to_owned(),
            avatar_color: [40, 80, 120],
        },
    ))));
    root
}

/// The resource publishing `action` among everything the session publishes.
fn publisher(session: &Session, action: &str) -> Option<String> {
    fn find(resources: &[tilo_actions::ResourceSpec], action: &str) -> Option<String> {
        resources.iter().find_map(|resource| {
            if resource.actions.iter().any(|offered| offered.id == action) {
                Some(resource.id.as_str().to_owned())
            } else {
                find(&resource.children, action)
            }
        })
    }
    find(&session.resources(), action)
}

/// Press the sheet's button for joining the listing `key` .
fn ask_to_join(session: &mut Session, key: &str) {
    // The row is drawn from what the sites list, and other runtimes answer that: a busy machine
    // takes as long at it as the suite allows. Press a row that is there, so a press that cannot be
    // carried out says why instead of waiting out the suite's bound for a join never asked for.
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let standing = session
            .workspace_directory()
            .expect("the session hosts the workspace directory")
            .directory();
        if standing.listings.iter().any(|listing| listing.key == key) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the sheet lists the workspace to join: {standing:?}"
        );
        session.step(Duration::from_millis(10)).unwrap();
    }
    let sheet = publisher(session, "join").expect("the directory sheet publishes joining");
    assert_eq!(
        sheet, "workspace-directory.listing",
        "the row's button is the directory sheet's"
    );
    let arguments = serde_json::json!({ "key": key })
        .as_object()
        .expect("an object")
        .clone();
    session
        .invoke(&sheet, "join", &arguments)
        .expect("the session asks to join");
}

/// Run the session until `done` holds, or fail saying what never happened.
#[track_caller]
fn wait_until(session: &mut Session, what: &str, done: impl Fn(&Session) -> bool) {
    // What this waits for is other runtimes' work (the site's answer, the record arriving), so the
    // bound is the suite's, not a second count.
    common::until(session, what, |session| done(session));
}

/// Wait until the directory is done with the person: it closes once the join's enrollment settles,
/// which is the pass after the workspace is held here, so the person lands in the workspace they
/// joined a frame later.
fn wait_until_the_directory_is_done(session: &mut Session) {
    wait_until(
        session,
        "the directory is done, leaving them in the workspace they joined",
        |session| {
            !session
                .workspace_directory()
                .expect("the session hosts the workspace directory")
                .presentation()
                .is_open()
        },
    );
}

/// What the site lists: its own community workspace, a club, and the workspace this computer holds
/// already.
fn page_listing_the_workspace_on_this_desk(held_name: &str, held_room: &str) -> String {
    let mut entries = vec![community_entry(34, now_ms()), club_entry(now_ms())];
    entries.push(DirectoryEntry {
        workspace: held_room.to_owned(),
        name: held_name.to_owned(),
        description: "The one on this desk".to_owned(),
        default_role: WorkspaceRole::Commenter,
        door: door_path(held_room),
        here: Vec::new(),
        here_count: 0,
        members: 3,
        last_active_ms: Some(now_ms()),
        community: false,
    });
    page_of(entries)
}

/// What the community's own workspace says of itself on its row, and of the people in it now.
fn asserts_the_communitys_row(session: &Session) {
    let community = listing(session, "Tilers");
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
        community.activity, "2 here now",
        "how lively it is, on the row's own line"
    );
    assert_eq!(
        community.members.as_deref(),
        Some("34 members"),
        "and how many belong, in its own column"
    );
    assert_eq!(
        community.joins_as, "You join as an editor",
        "what joining gets the person"
    );
}

/// What a workspace that says nothing of itself says on its row.
fn asserts_the_clubs_row(session: &Session) {
    let club = listing(session, "Book club");
    assert_eq!(
        club.about, "127.0.0.1",
        "a workspace that says nothing of itself is named by its host"
    );
    assert_eq!(
        club.activity, "Active this week",
        "and says when it last had something new"
    );
    assert_eq!(
        club.members.as_deref(),
        Some("5 members"),
        "how many belong to it"
    );
    assert_eq!(
        club.joins_as, "You join as a viewer",
        "its own default role"
    );
    assert!(club.here.is_empty(), "nobody is in it now");
    assert_eq!(club.tag, None, "it is not the site's own workspace");
}

#[test]
fn opening_the_directory_lists_the_community_sites_public_workspaces() {
    let _settled = ui::motion::testing::settled();
    let (responder, page) = site();
    let mut env = tilo_test_env::ScopedEnv::new();
    let _config = community_site(&mut env, &responder.url());
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    let (_held_id, held_name, held_room) = held(&session)
        .into_iter()
        .next()
        .expect("the desk holds one workspace");
    *page.lock().expect("the page") =
        page_listing_the_workspace_on_this_desk(&held_name, &held_room);
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
                COMMUNITY_WORKSPACE.to_owned()
            ),
            (
                "Book club".to_owned(),
                ListingAction::Join,
                CLUB_WORKSPACE.to_owned()
            ),
        ],
        "the community's own first, then the liveliest, and none this computer holds"
    );
    asserts_the_communitys_row(&session);
    asserts_the_clubs_row(&session);
    capture(&mut session, "join-sheet");
}

#[test]
fn a_site_that_cannot_be_read_says_so_and_offers_nothing_to_join() {
    let _settled = ui::motion::testing::settled();
    // A site with nothing at its entry.
    let responder = Responder::handle(|_| Some(Reply::status(404)));
    let mut env = tilo_test_env::ScopedEnv::new();
    let _config = community_site(&mut env, &responder.url());
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    open_directory(&mut session);
    let sheet = session
        .workspace_directory()
        .expect("the session hosts the workspace directory");
    assert_eq!(
        sheet.directory().status,
        Status::Failed(tilo_sheets::word("directory-error-no-workspace")),
        "the sheet says why nothing was listed"
    );
    assert!(
        rows(&session).is_empty(),
        "and it offers nothing: the workspaces this computer holds are not ones to join"
    );
}

#[test]
fn a_computer_with_no_community_site_has_nothing_to_join() {
    let _settled = ui::motion::testing::settled();
    let mut env = tilo_test_env::ScopedEnv::new();
    let _config = community_site(&mut env, "");
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    open_directory(&mut session);
    let sheet = session
        .workspace_directory()
        .expect("the session hosts the workspace directory");
    assert_eq!(
        sheet.directory().status,
        Status::Listed,
        "a computer with no community site has nothing to read, not a failure"
    );
    assert!(
        rows(&session).is_empty(),
        "and nothing to join: it holds no one else's workspace"
    );
}

#[test]
fn joining_a_listed_workspace_enters_through_its_door_and_lands_in_it() {
    let _settled = ui::motion::testing::settled();
    let mut env = tilo_test_env::ScopedEnv::new();
    let (_identity, _person) = this_computer(&mut env);
    let responder = joining_site(vec![community_entry(34, now_ms()), club_entry(now_ms())]);
    let _config = community_site(&mut env, &responder.url());
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    open_directory(&mut session);
    assert_eq!(
        listing(&session, "Book club").action,
        ListingAction::Join,
        "the club is listed to join"
    );
    ask_to_join(&mut session, CLUB_WORKSPACE);
    wait_until(&mut session, "the club was joined", |session| {
        held(session)
            .iter()
            .any(|(_, _, workspace)| workspace == CLUB_WORKSPACE)
    });
    let joined = held(&session)
        .into_iter()
        .find(|(_, _, workspace)| workspace == CLUB_WORKSPACE)
        .expect("the workspace joined");
    assert_eq!(
        joined.1, "Book club",
        "the workspace goes by the name it was listed under"
    );
    assert_eq!(
        session.handle().read(|tree| tree.active.0.to_string()),
        joined.0,
        "and the person lands in it"
    );
    // The door is entered on the session this computer opens with its host, never asked over HTTP:
    // the site was asked for its entry and its directory, and nothing else.
    let asked: Vec<(String, String)> = responder
        .drain_requests()
        .into_iter()
        .map(|request| (request.method, request.target))
        .collect();
    assert!(
        asked
            .iter()
            .all(|(_, target)| target == "/entry.json" || target == "/directory"),
        "nothing but the site's entry and directory was asked over HTTP: {asked:?}"
    );
    wait_until_the_directory_is_done(&mut session);
    // Opening the directory again does not offer the club to join: this computer holds it now.
    open_directory(&mut session);
    assert!(
        !rows(&session).iter().any(|(name, ..)| name == "Book club"),
        "the club is no longer one to join"
    );
}

#[test]
fn a_door_that_refuses_says_why_where_the_person_now_is() {
    let _settled = ui::motion::testing::settled();
    tilo_secrets::keep_in_memory_for_process();
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    let (_root, person) = this_computer_with_an_identity(&mut home);
    // The site lists the lounge, but its host never made it public: its door lets nobody in.
    let lounge = Lounge::start_with(person, "Coffee's on", LoungeDoor::Private);
    let _config = community_site(home.env(), "");
    let state = tempfile::tempdir().expect("a directory for this computer's desk");
    let mut session = collaborative_session(state.path());
    session.step(Duration::from_millis(1)).unwrap();
    open_directory(&mut session);
    look_up_site(&mut session, &lounge.entry);
    ask_to_join(&mut session, &lounge.workspace);
    // The sheet leaves as soon as the join starts; the door's refusal, on this computer's session
    // with its host, reaches the person as a banner where they are now.
    wait_until(&mut session, "the door answered", |session| {
        listing(session, "Lounge").action == ListingAction::TryAgain
    });
    let sheet = session
        .workspace_directory()
        .expect("the session hosts the workspace directory");
    let shown = sheet.directory();
    assert_eq!(
        shown.note.as_deref(),
        Some("Couldn't join Lounge: that workspace is not public"),
        "the sheet's note says why it is not joined, in the door's own words"
    );
    let row = listing(&session, "Lounge");
    assert_eq!(
        row.activity, "Didn't join: that workspace is not public",
        "and the row says so where the person pressed"
    );
    assert!(
        !sheet.presentation().is_drawn(),
        "the sheet left once the join started, leaving no sheet behind"
    );
    wait_until(&mut session, "the refusal is said as a banner", |session| {
        said(session)
            .iter()
            .any(|notice| notice.contains("Couldn't join Lounge: that workspace is not public"))
    });
    assert!(
        !held(&session)
            .iter()
            .any(|(_, _, workspace)| workspace == &lounge.workspace),
        "and no workspace was added"
    );
}

#[test]
fn a_members_only_lounge_says_ask_for_an_invite_and_lands_nowhere() {
    let _settled = ui::motion::testing::settled();
    tilo_secrets::keep_in_memory_for_process();
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    let (_root, person) = this_computer_with_an_identity(&mut home);
    let lounge = Lounge::start_with(person, "Coffee's on", LoungeDoor::InvitationOnly);
    let _config = community_site(home.env(), "");
    let state = tempfile::tempdir().expect("a directory for this computer's desk");
    let mut session = collaborative_session(state.path());
    session.step(Duration::from_millis(1)).unwrap();
    open_directory(&mut session);
    look_up_site(&mut session, &lounge.entry);
    let desk_before: Vec<(String, String, String)> = held(&session);
    let in_front_before = session
        .handle()
        .read(|tree| tree.active_workspace().map(|workspace| workspace.slot));
    ask_to_join(&mut session, &lounge.workspace);
    wait_until(&mut session, "the door answered", |session| {
        listing(session, "Lounge").action == ListingAction::TryAgain
    });
    // Choosing Join closed the sheet when the join started: the refusal reaches the person as a
    // banner, in the door's own words.
    wait_until(&mut session, "the refusal is said as a banner", |session| {
        said(session)
            .iter()
            .any(|notice| notice.contains("joined by invitation"))
    });
    let shown = said(&session);
    assert!(
        shown
            .iter()
            .any(|notice| notice.contains("Lounge") && notice.contains("joined by invitation")),
        "the host's own refusal, in its own words: {shown:?}"
    );
    capture(&mut session, "join-refused");
    // The person stays where they were: no empty workspace was ever shown to arrive in, and none
    // was taken away again.
    assert_eq!(
        session
            .handle()
            .read(|tree| tree.active_workspace().map(|workspace| workspace.slot)),
        in_front_before,
        "the person is where they were: the workspace in front did not move"
    );
    assert_eq!(
        held(&session),
        desk_before,
        "and the desk holds exactly what it held before the join"
    );
}

#[test]
fn a_join_with_an_expired_certificate_renews_it_before_presenting_it() {
    let _settled = ui::motion::testing::settled();
    tilo_secrets::keep_in_memory_for_process();
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    // This computer's certificate lasted a day, and it ran out twelve hours ago: custody starts
    // holding one the host would refuse for having run out.
    let (person_hex, expired_paths) = expired_throwaway_identity(&mut home);
    let person = hex::decode(&person_hex)
        .ok()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        .expect("the person's root key");
    let expired = tilo_user_identity::persist::read_device_cert(&expired_paths.device_cert)
        .expect("the expired certificate");
    assert!(
        expired
            .expires_at_ns
            .is_some_and(|expires| expires <= tilo_clock::unix_now_ns()),
        "this computer starts with an expired certificate"
    );
    let lounge = Lounge::start(person, "Coffee's on");
    // The lounge is not this computer's community: the person types its address into the sheet, as
    // anyone joining a site they were given does.
    let _config = community_site(home.env(), "");
    let state = tempfile::tempdir().expect("a directory for this computer's desk");
    // The renewal is this session's own: it resolves the identity from the environment the test
    // set, so a certificate issued to the device that joins is renewed, never a sibling test's.
    let mut session = collaborative_session(state.path());
    session.step(Duration::from_millis(1)).unwrap();
    open_directory(&mut session);
    look_up_site(&mut session, &lounge.entry);
    ask_to_join(&mut session, &lounge.workspace);
    wait_until_the_lounge_is_here(&mut session, &lounge);
    // What the join presented was the renewal, not the certificate that had run out: custody holds
    // a certificate issued since, and the workspace holds this device under it.
    let presented = tilo_user_identity::persist::read_device_cert(&expired_paths.device_cert)
        .expect("the presented certificate");
    assert!(
        presented.issued_at_ns > expired.issued_at_ns,
        "the certificate the join presented was issued after the expired one"
    );
    assert!(
        presented
            .expires_at_ns
            .is_some_and(|expires| expires > tilo_clock::unix_now_ns()),
        "and it has not run out"
    );
    // The workspace is held before it is in front: the person lands in it a frame later, so wait
    // for the landing rather than sampling the frame.
    wait_until(
        &mut session,
        "the person lands in the lounge: the renewal was admitted",
        |session| {
            session.handle().read(|tree| {
                tree.active_workspace()
                    .is_some_and(|workspace| workspace.id().to_string() == lounge.workspace)
            })
        },
    );
}

/// A throwaway identity in `home` , with its certificate replaced by one issued a day and a half
/// ago — expired, as a session that slept past it left it. Returns the person's root key in hex and
/// the paths its keys and certificate live under, which live as long as `home` does.
fn expired_throwaway_identity(
    home: &mut tilo_test_env::TestHome,
) -> (String, tilo_user_identity::IdentityPaths) {
    use tilo_identity_host::provisioning::IdentityStorage;
    let system = home.root().join("system");
    let paths = tilo_user_identity::IdentityPaths::under(&system);
    let identity_dir = home.config().join("tilo").join("identity");
    tilo_identity_host::provisioning::ensure_throwaway_test_identity(
        &identity_dir,
        &paths,
        IdentityStorage::Files,
    )
    .expect("this computer's throwaway identity");
    // The throwaway certificate lasts a day; issue it a day and a half ago,
    // so it has run out.
    let identity = runtime_identity::load_from_path(identity_dir.join("primary.json"))
        .expect("this computer's identity");
    let held = tilo_user_identity::persist::read_device_cert(&paths.device_cert)
        .expect("the throwaway certificate");
    let mut aged = tilo_user_identity::DeviceCert {
        user_public_key: held.user_public_key,
        device_public_key: held.device_public_key,
        device_label: held.device_label.clone(),
        capabilities: held.capabilities,
        issued_at_ns: tilo_clock::unix_now_ns().saturating_sub(A_DAY_AND_A_HALF_NS),
        expires_at_ns: Some(tilo_clock::unix_now_ns().saturating_sub(TWELVE_HOURS_NS)),
        signature: [0; 64],
        issuer: None,
    };
    let digest = tilo_user_identity::device_cert_signing_digest(&aged).expect("a digest to sign");
    aged.signature = identity
        .sign(&digest)
        .expect("the aged certificate is signed");
    tilo_user_identity::verify_device_cert(&aged).expect("the aged certificate verifies");
    tilo_user_identity::persist::write_device_cert(&paths.device_cert, &aged)
        .expect("the aged certificate");
    tilo_user_identity::persist::write_device_cert(
        &paths.device_cert_for_user(&identity.group_public_key()),
        &aged,
    )
    .expect("the aged certificate where the identity keeps it");
    home.env().set("TILO_IDENTITY_ROOT", &system);
    ui::local_identity::install_source(Some(Arc::new(ui::local_identity::FixedIdentity(
        ui::local_identity::LocalIdentity {
            group_public_key: aged.user_public_key,
            display_name: "Ada".to_owned(),
            avatar_color: [40, 80, 120],
        },
    ))));
    (hex::encode(aged.user_public_key), paths)
}

/// A session that takes part in the collaboration plane, keeping the workspaces it holds beside
/// `state` .
fn collaborative_session(state: &std::path::Path) -> Session {
    crate::common::headless(Options {
        collab: true,
        state: Some(state.to_path_buf()),
        ..Options::default()
    })
}

/// `key` 's certificate as a device of the person whose root key is `root` .
fn certified(
    root: &ed25519_dalek::SigningKey,
    key: &ed25519_dalek::SigningKey,
    label: &str,
    capabilities: tilo_user_identity::DeviceCapabilities,
) -> tilo_user_identity::DeviceCert {
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.to_owned(),
        capabilities,
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, root).expect("a certified device");
    cert
}

/// This computer's person: an identity of its own in `home` 's configuration, and a device
/// certified for it under a key root, kept in `home` too so it lives as long as the test. Returns
/// the root to hold, and the person's key.
fn this_computer_with_an_identity(
    home: &mut tilo_test_env::TestHome,
) -> (std::path::PathBuf, [u8; 32]) {
    // The keys live under the test's own home, not a second temporary
    // directory: the session's renewal resolves them from the environment,
    // and nothing may delete them while it runs.
    let root = home.root().join("system");
    let identity_dir = home.config().join("tilo").join("identity");
    tilo_identity_host::provisioning::ensure_collaboration_identity(
        &identity_dir,
        &tilo_user_identity::IdentityPaths::under(&root),
        tilo_identity_host::provisioning::IdentityStorage::Files,
    )
    .expect("this computer's identity");
    home.env().set("TILO_IDENTITY_ROOT", &root);
    let person = runtime_identity::list_identities(&identity_dir)
        .expect("this computer's identities")
        .into_iter()
        .next()
        .expect("an identity")
        .public_key;
    // The name they go by, which the site's door learns of them: never their device's.
    ui::local_identity::install_source(Some(Arc::new(ui::local_identity::FixedIdentity(
        ui::local_identity::LocalIdentity {
            group_public_key: person,
            display_name: "Ada".to_owned(),
            avatar_color: [40, 80, 120],
        },
    ))));
    (root, person)
}

/// A chat record holding `line` : what one of the lounge's people wrote.
fn chat_with(line: &str) -> automerge::AutoCommit {
    use automerge::ROOT;
    let mut document = automerge::AutoCommit::new();
    document.put(ROOT, "text", line).expect("a chat line");
    document.commit();
    document
}

/// A site's workspace as a site runs it: a host on this machine governs the workspace, a page of
/// the site's person recorded there before anyone joined this computer, and the site's entry and
/// door stand in front of it.
/// Dropping it takes the page down before the runtime it runs on, and shuts the host down.
struct Lounge {
    page: Option<Link>,
    relay: Option<tilo_relay::daemon::Relay>,
    _door: Responder,
    /// The host's web listener, which serves the door's host (`/host`) a device asks before its
    /// session enters through the door.
    _web: tilo_relay::web::WebHandle,
    runtime: tokio::runtime::Runtime,
    workspaces: tilo_relay::workspaces::Workspaces,
    host_cert: tilo_user_identity::DeviceCert,
    /// The certificate of the site's page, the device that recorded in the workspace before anyone
    /// joined.
    page_device: tilo_user_identity::DeviceCert,
    /// The page's own copy of the workspace's shared desktop: what it laid out, and the base of
    /// every later change it writes.
    page_desktop: std::cell::RefCell<automerge::AutoCommit>,
    person: [u8; 32],
    /// The site's workspace, by id.
    workspace: String,
    /// The site's entry, where its links and its directory hang.
    entry: String,
    /// The site's own state, which goes with it.
    _state: tempfile::TempDir,
}

/// A runtime, or a handle onto one: both run a future to completion.
trait RunOn {
    fn run_on<F: std::future::Future>(&self, work: F) -> F::Output;
}

impl RunOn for tokio::runtime::Runtime {
    fn run_on<F: std::future::Future>(&self, work: F) -> F::Output {
        self.block_on(work)
    }
}

impl RunOn for tokio::runtime::Handle {
    fn run_on<F: std::future::Future>(&self, work: F) -> F::Output {
        self.block_on(work)
    }
}

/// Run `work` on `runtime` , giving up after the suite's hang bound.
/// The lounge's host, its pages and its governance live on runtimes of their own, so what a test
/// waits for there is not a step of the session the suite's waits drive: a loaded machine takes a
/// while over a relay's start, a page's write and a governance change, and a wait with no bound at
/// all reports nothing when one of them never comes back.
#[track_caller]
fn block_on_within<T>(
    runtime: &impl RunOn,
    what: &str,
    work: impl std::future::Future<Output = T>,
) -> T {
    let bound = tilo_test_env::hang_bound();
    // The timer is made inside the runtime: a timeout built outside it has no reactor to be woken
    // by.
    match runtime.run_on(async move { tokio::time::timeout(bound, work).await }) {
        Ok(value) => value,
        Err(elapsed) => panic!(
            "gave up waiting for {what}: {elapsed} (set TILO_TEST_HANG_BOUND_SECS to wait longer)"
        ),
    }
}

impl Drop for Lounge {
    fn drop(&mut self) {
        drop(self.page.take());
        let Some(relay) = self.relay.take() else {
            return;
        };
        if std::thread::panicking() {
            // A test that already failed leaves with its report: the relay's own shutdown is not
            // waited for on the way out of a panic, where a second panic would take the report
            // away.
            return;
        }
        block_on_within(
            &self.runtime,
            "the lounge's relay to shut down",
            relay.shutdown(),
        );
    }
}

/// Who the lounge's door lets in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LoungeDoor {
    /// Anyone who asks, as an editor: the workspace is public.
    Public,
    /// Only people invited: the workspace is listed, and its door takes people by invitation.
    InvitationOnly,
    /// Nobody: the site lists the workspace, but its host never made it public.
    Private,
}

impl Lounge {
    /// The lounge's workspace, whose chat already holds `written` , and whose door lets anyone in.
    fn start(person: [u8; 32], written: &str) -> Self {
        Self::start_with(person, written, LoungeDoor::Public)
    }

    /// [`Self::start`], with a door that lets in whom `door` says.
    fn start_with(person: [u8; 32], written: &str, door: LoungeDoor) -> Self {
        // The lounge's host and page run in this test's process: what they did with a join is only
        // said in its log.
        common::people::install_log();
        let state = tempfile::tempdir().expect("a directory for the lounge's host");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a runtime for the lounge's host");
        let host = LoungeHost::start(&runtime, state.path());
        let page = host.page(&runtime, written);
        let (site, web, entry) = host.door(&runtime, state.path(), door);
        Self {
            page: Some(page),
            relay: Some(host.relay),
            _door: site,
            _web: web,
            runtime,
            workspaces: host.workspaces,
            host_cert: host.host_cert,
            page_device: host.page_device,
            page_desktop: std::cell::RefCell::new(automerge::AutoCommit::new()),
            person,
            workspace: host.workspace,
            entry,
            _state: state,
        }
    }
}

/// A key the lounge keeps to itself: drawn for each lounge rather than a constant, because the
/// suites on a machine share one network, and two lounges answering to one node id answer each
/// other's joins as a host that does not govern the room the join names.
fn lounge_key() -> [u8; 32] {
    let mut key = [0; 32];
    getrandom::fill(&mut key).expect("a key for the lounge");
    key
}

/// The host behind the lounge's site: a relay of this machine, certified under a key root of the
/// test's own, which governs the lounge's workspace and keeps what its page writes sealed.
struct LoungeHost {
    relay: tilo_relay::daemon::Relay,
    workspaces: tilo_relay::workspaces::Workspaces,
    host_cert: tilo_user_identity::DeviceCert,
    /// The key and certificate of the site's page, the device that recorded in the workspace before
    /// anyone joined.
    page_key: ed25519_dalek::SigningKey,
    page_device: tilo_user_identity::DeviceCert,
    workspace: String,
}

impl LoungeHost {
    /// A host governing a workspace of its own, with the site's page entered in it.
    fn start(runtime: &tokio::runtime::Runtime, state: &std::path::Path) -> Self {
        let owner_key = ed25519_dalek::SigningKey::from_bytes(&lounge_key());
        let host_key = ed25519_dalek::SigningKey::from_bytes(&lounge_key());
        let host_cert = certified(
            &owner_key,
            &host_key,
            "The lounge's host",
            tilo_user_identity::cloud_enrollment::host_capabilities(),
        );
        let page_key = ed25519_dalek::SigningKey::from_bytes(&lounge_key());
        let page_device = certified(
            &owner_key,
            &page_key,
            "The lounge's page",
            tilo_user_identity::DeviceCapabilities::member(),
        );
        let (relay, workspaces, workspace) =
            block_on_within(runtime, "the lounge's host to start", async {
                let relay = tilo_relay::daemon::Relay::start(
                    tilo_relay::RelayConfig {
                        state_root: state.join("host"),
                        ..tilo_relay::RelayConfig::default()
                    },
                    tilo_relay::daemon::RelayIdentity {
                        user_public_key: owner_key.verifying_key().to_bytes(),
                        device_signing_key: host_key.clone(),
                        device_cert: host_cert.clone(),
                        revocations: tilo_user_identity::RevocationList::new(),
                    },
                )
                .await
                .expect("the lounge's host");
                let workspaces = relay
                    .workspaces()
                    .expect("the host governs workspaces")
                    .clone();
                let workspace = workspaces
                    .create("Lounge")
                    .await
                    .expect("the lounge's workspace")
                    .workspace;
                // The site's page is in its workspace.
                workspaces
                    .enter(&workspace, &page_device)
                    .await
                    .expect("the site's page is in its workspace");
                (relay, workspaces, workspace)
            });
        Self {
            relay,
            workspaces,
            host_cert,
            page_key,
            page_device,
            workspace,
        }
    }

    /// The site's page, in the workspace, having recorded `written` and then a
    /// fair history after it. The host keeps what its members write sealed,
    /// so it asks the page that wrote for the record it holds and folds that
    /// in: the page answers, as a page does, with the record.
    fn page(&self, runtime: &tokio::runtime::Runtime, written: &str) -> Link {
        let route = DocumentRoute {
            app_id: CHAT.to_owned(),
            cluster_id: self.workspace.clone(),
            record: CHAT_RECORD.to_owned(),
        };
        let page = {
            let _entered = runtime.enter();
            let page = tilo_session_host::start_session(
                PeerConfig {
                    host: Host::at(self.relay.node_addr()),
                    cert: self.page_device.clone(),
                    host_cert: self.host_cert.clone(),
                    revocations: tilo_user_identity::RevocationList::new(),
                    entering: None,
                },
                Arc::new(self.page_key.clone()) as Arc<dyn tilo_relay_client::DeviceSigner>,
                Arc::new(MemoryPeerStore::default()),
            );
            page.join_placed(&self.workspace, "Lounge")
                .expect("the site's page joins the lounge's workspace");
            page
        };
        let mut chat = chat_with(written);
        page.write(route.clone(), &DocumentMutation::Snapshot(chat.save()))
            .expect("the site's page records in the lounge's chat");
        for line in 0..HISTORY {
            let before = chat.get_heads();
            chat.put(automerge::ROOT, format!("line{line}"), line)
                .expect("a chat line");
            chat.commit();
            page.write(
                route.clone(),
                &DocumentMutation::Incremental(chat.save_after(&before)),
            )
            .expect("the site's page goes on talking");
        }
        // The line is the lounge's before anyone joins: the test proves a join brings it, not that
        // a write lands. The page's writes reach the host through other runtimes' work, so the wait
        // is the suite's, not a second count. The page's session hands the host the record it
        // holds when the host asks for it, from its own replica.
        tilo_test_env::wait_until("the page's line to reach the lounge's host", || {
            let held = block_on_within(
                runtime,
                "the lounge's records",
                self.workspaces.record_routes(&self.workspace),
            )
            .expect("the lounge's records");
            held.iter()
                .any(|(app, record)| app == CHAT && record == CHAT_RECORD)
        });
        page
    }

    /// The site as the world meets it: its entry and the directory of the workspaces it serves,
    /// whose door is the host's own, served by its web listener (kept under `state` ), which a
    /// device enters through on its session with the host. The door lets in whom `door` says.
    /// Returns the site, the host's web listener, and the site's entry.
    fn door(
        &self,
        runtime: &tokio::runtime::Runtime,
        state: &std::path::Path,
        door: LoungeDoor,
    ) -> (Responder, tilo_relay::web::WebHandle, String) {
        let web = block_on_within(runtime, "the lounge's host to serve its web", async {
            if door != LoungeDoor::Private {
                self.workspaces
                    .publish(
                        &self.workspace,
                        &self.host_cert,
                        tilo_relay_protocol::directory::Listing {
                            description: "Where the site's people talk".to_owned(),
                            default_role: WorkspaceRole::Editor,
                        },
                    )
                    .await
                    .expect("the lounge is public");
            }
            if door == LoungeDoor::InvitationOnly {
                self.workspaces
                    .list_invitation_only(&self.workspace, &self.host_cert, true)
                    .await
                    .expect("the lounge takes people by invitation");
            }
            self.relay
                .serve_web(tilo_relay::web::WebConfig {
                    listen: "127.0.0.1:0".parse().expect("an address"),
                    tls: tilo_relay::web::TlsSource::Development {
                        hosts: vec!["localhost".into(), "127.0.0.1".into()],
                        store: Some(state.join("web-dev-identity")),
                    },
                    allowed_origins: Vec::new(),
                    plain_listen: Some("127.0.0.1:0".parse().expect("an address")),
                    public_origin: None,
                    fetch_proxy: None,
                    door: None,
                    link: None,
                })
                .await
                .expect("the lounge's host serves its web")
        });
        let host_door = format!(
            "{}{}/{}",
            web.plain_origin
                .as_deref()
                .expect("the host's loopback listener"),
            tilo_relay_protocol::web::API_PREFIX,
            door_path(&self.workspace)
        );
        let origin = Arc::new(Mutex::new(String::new()));
        let served_origin = Arc::clone(&origin);
        let page_of_the_site = page_of(vec![DirectoryEntry {
            workspace: self.workspace.clone(),
            name: "Lounge".to_owned(),
            description: "Where the site's people talk".to_owned(),
            default_role: WorkspaceRole::Editor,
            door: host_door.clone(),
            here: Vec::new(),
            here_count: 0,
            members: 2,
            last_active_ms: Some(now_ms()),
            community: true,
        }]);
        let site = Responder::handle(move |request| {
            let json = |body: String| {
                Some(
                    Reply::status(200)
                        .header("content-type", "application/json")
                        .body(body),
                )
            };
            match (request.method.as_str(), request.target.as_str()) {
                ("GET", "/entry.json") => json(
                    serde_json::json!({
                        "directory": "directory",
                        "home": {
                            "name": "Lounge",
                            "host": {
                                "origin": served_origin.lock().expect("the site's origin").clone()
                            },
                            "admission": host_door,
                        },
                    })
                    .to_string(),
                ),
                ("POST", "/directory") => json(page_of_the_site.clone()),
                _ => Some(Reply::status(404)),
            }
        });
        *origin.lock().expect("the site's origin") = format!("{}/", site.url());
        let entry = format!("{}/", site.url());
        (site, web, entry)
    }
}

/// The strips and windows a desk shows in `workspace` , in order: each strip by its name and the
/// titles of the windows in it.
fn desk_layout(session: &Session, workspace: &str) -> Vec<(String, Vec<String>)> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .find(|held| held.id().to_string() == workspace)
            .map(|held| {
                held.strips
                    .iter()
                    .map(|strip| {
                        (
                            strip.name.clone(),
                            strip
                                .window_groups
                                .iter()
                                .flat_map(|group| group.tabs.iter().map(|tab| tab.title.clone()))
                                .collect(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// The strips and windows a shared desktop holds, as [`desk_layout`] gives a desk's: what every
/// device showing the workspace should see.
fn recorded_layout(desktop: &SharedDesktop) -> Vec<(String, Vec<String>)> {
    let mut layout: Vec<(String, Vec<String>)> = desktop
        .tree
        .workspaces
        .first()
        .map(|workspace| {
            workspace
                .strips
                .iter()
                .map(|strip| {
                    (
                        strip.name.clone(),
                        strip
                            .window_groups
                            .iter()
                            .flat_map(|group| group.tabs.iter().map(|tab| tab.title.clone()))
                            .collect(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    layout.retain(|(_, windows): &(String, Vec<String>)| !windows.is_empty());
    layout
}

/// A strip the site holds as a test writes it: its id, its name, and the windows in it, each
/// window's id and title.
type HeldStrip<'a> = (u64, &'a str, &'a [(u64, &'a str)]);

/// A workspace's shared desktop, as the people in it left it: `strips` names each strip with the
/// windows opened in it, all of them `person` 's.
fn shared_desktop(person: &str, strips: &[HeldStrip<'_>]) -> SharedDesktop {
    let mut workspace = Workspace::new(WorkspaceSlot(1), "Lounge");
    let mut authors = std::collections::BTreeMap::new();
    for (strip_id, name, windows) in strips {
        let mut strip = Strip::new(StripId::from(*strip_id), *name);
        authors.insert(*strip_id, person.to_owned());
        for (tab, title) in *windows {
            strip.window_groups.push(WindowGroup {
                id: GroupId::from(tab.saturating_add(1_000)),
                tabs: vec![Tab {
                    id: TabId::from(*tab),
                    title: (*title).to_owned(),
                    tile: TileContent::LocalApp {
                        app_id: "so.tilo.notes".to_owned(),
                        label: (*title).to_owned(),
                        note: String::new(),
                    },
                    app_title: None,
                    view: String::new(),
                    pinned: false,
                }],
                active_tab: TabId::from(*tab),
                width: WindowWidth::Normal,
                maximized: false,
                focus_history: Vec::new(),
            });
            authors.insert(*tab, person.to_owned());
        }
        workspace.strips.push(strip);
    }
    SharedDesktop {
        tree: tilo_desktop_model::DesktopTree {
            active: workspace.slot,
            workspaces: vec![workspace],
            ..tilo_desktop_model::DesktopTree::default()
        },
        records: Vec::new(),
        authors,
        opened_by: std::collections::BTreeMap::new(),
    }
}

impl Lounge {
    /// The site's page lays its workspace out as `desktop` says: the strips and windows its people
    /// see there, written into the workspace's shared desktop record as every member's page writes
    /// it.
    fn page_writes_desktop(&self, desktop: &SharedDesktop) {
        let page = self.page.as_ref().expect("the site's page runs");
        let person = hex::encode(self.page_device.user_public_key);
        let mut doc = self.page_desktop.borrow_mut();
        let before = doc.get_heads();
        // The record's first publication in a session is a snapshot: the route it is kept on is
        // founded by it, which is what a host replays to the devices that join later.
        let first = before.is_empty();
        assert!(
            shared_record::write(&mut doc, desktop).expect("the page lays its workspace out"),
            "the page's desktop is news to the record"
        );
        doc.commit_with(
            automerge::transaction::CommitOptions::default()
                .with_message(shared_record::change_message(Some(&person))),
        );
        let mutation = if first {
            DocumentMutation::Snapshot(doc.save())
        } else {
            DocumentMutation::Incremental(doc.save_after(&before))
        };
        page.write(
            DocumentRoute {
                app_id: DESKTOP_APP.to_owned(),
                cluster_id: self.workspace.clone(),
                record: DESKTOP_RECORD.to_owned(),
            },
            &mutation,
        )
        .expect("the site's page lays out the workspace's desktop");
    }

    /// Wait until the lounge's host holds `desktop` as its shared desktop: the page's write reached
    /// it, and every device that joins now sees it.
    fn wait_for_desktop(&self, desktop: &SharedDesktop) {
        tilo_test_env::wait_until("the page's desktop to reach the lounge's host", || {
            let held = block_on_within(
                &self.runtime,
                "the lounge's desktop record",
                self.workspaces
                    .record_document(&self.workspace, DESKTOP_APP, DESKTOP_RECORD),
            )
            .expect("the lounge's desktop record");
            let recorded = held
                .and_then(|bytes| automerge::AutoCommit::load(&bytes).ok())
                .and_then(|doc| shared_record::read(&doc, None));
            recorded.as_ref().map(recorded_layout) == Some(recorded_layout(desktop))
        });
    }

    /// A link to the lounge's workspace carrying an invitation for this person, as whoever shares
    /// the workspace copies it.
    fn invitation_link(&self) -> String {
        let issued = block_on_within(
            &self.runtime,
            "the lounge to issue this person an invitation",
            self.workspaces.issue(
                &self.workspace,
                &self.host_cert,
                tilo_workspace_join::invitations::IssueInvitationRequest {
                    request_id: [0x61; 32],
                    role: tilo_workspace_join::MemberRole::Editor,
                    recipient_user_public_key: Some(self.person),
                    invitee_name: None,
                    expires_at_ns: tilo_clock::unix_now_ns().saturating_add(3_600_000_000_000),
                    inviter_name: Some("The lounge".to_owned()),
                },
            ),
        )
        .expect("an invitation to the lounge");
        format!(
            "{}#/w/{}?invite={}",
            self.entry,
            self.workspace,
            tilo_workspace_join::encode_invitation_text(&issued.signed_invitation)
        )
    }
}

/// Whose the workspace named `workspace` is, as the desk names them.
fn owner_on_desk(session: &Session, workspace: &str) -> Option<String> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .find(|held| held.id().to_string() == workspace)
            .and_then(|held| held.owner().cloned())
    })
}

#[test]
fn a_person_who_joins_a_listed_workspace_arrives_in_it_with_its_people() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    let (_root, person) = this_computer_with_an_identity(&mut home);
    let lounge = Lounge::start(person, "Coffee's on");
    // The lounge is not this computer's community: the person types its address into the sheet, as
    // anyone joining a site they were given does.
    let _config = community_site(home.env(), "");
    let state = tempfile::tempdir().expect("a directory for this computer's desk");
    let mut session = collaborative_session(state.path());
    session.step(Duration::from_millis(1)).unwrap();
    open_directory(&mut session);
    look_up_site(&mut session, &lounge.entry);
    assert_eq!(
        listing(&session, "Lounge").action,
        ListingAction::Join,
        "the lounge's workspace is listed to join"
    );
    ask_to_join(&mut session, &lounge.workspace);
    wait_until_the_lounge_is_here(&mut session, &lounge);
    let people = session
        .people(&lounge.workspace)
        .expect("the lounge's people are known here");
    let member = people
        .iter()
        .find(|member| member.person == hex::encode(person))
        .expect("the lounge is shared with this person");
    assert_eq!(
        member.role,
        tilo_workspace_join::MemberRole::Editor,
        "in the part the invitation gave them"
    );
    assert!(!member.removed, "and the lounge has not removed them");
    // The slot is made once the host admits the device, and the person lands in it: wait for the
    // desk to show it, as the person would see it.
    wait_until(&mut session, "the lounge is on the desk", |session| {
        held(session)
            .iter()
            .any(|(_, _, workspace)| workspace == &lounge.workspace)
    });
    let joined = held(&session)
        .into_iter()
        .find(|(_, _, workspace)| workspace == &lounge.workspace)
        .expect("the workspace is on the desk");
    assert_eq!(joined.1, "Lounge", "under the name the site listed it by");
    // Whose the workspace is stays its host's governance's to say: joining it never makes it this
    // person's.
    wait_until(
        &mut session,
        "the lounge's owner is named here",
        |session| owner_on_desk(session, &lounge.workspace).is_some(),
    );
    let host = hex::encode(lounge.host_cert.user_public_key);
    assert_eq!(
        owner_on_desk(&session, &lounge.workspace).as_deref(),
        Some(host.as_str()),
        "the workspace joined keeps the owner its host's governance names"
    );
    assert_ne!(
        owner_on_desk(&session, &lounge.workspace).as_deref(),
        Some(hex::encode(person).as_str()),
        "and never the person who only joined it"
    );
    wait_until_the_directory_is_done(&mut session);
}

#[test]
fn a_pasted_invitation_link_joins_its_workspace_and_its_people() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    let (_root, person) = this_computer_with_an_identity(&mut home);
    let lounge = Lounge::start(person, "Pulled up a chair");
    // The lounge is not this computer's community: the person types its address into the sheet, as
    // anyone joining a site they were given does.
    let _config = community_site(home.env(), "");
    let state = tempfile::tempdir().expect("a directory for this computer's desk");
    let mut session = collaborative_session(state.path());
    session.step(Duration::from_millis(1)).unwrap();
    open_directory(&mut session);
    let link = lounge.invitation_link();
    let sheet =
        publisher(&session, "link-or-code").expect("the directory sheet takes a link or a code");
    let arguments = serde_json::json!({ "text": link })
        .as_object()
        .expect("an object")
        .clone();
    session
        .invoke(&sheet, "link-or-code", &arguments)
        .expect("the person pastes the link");
    wait_until(
        &mut session,
        "the linked workspace is held here",
        |session| {
            session
                .held_workspaces()
                .iter()
                .any(|held| held == &lounge.workspace)
                && session.people(&lounge.workspace).is_ok_and(|people| {
                    people
                        .iter()
                        .any(|member| member.person == hex::encode(person))
                })
        },
    );
    let people = session
        .people(&lounge.workspace)
        .expect("the linked workspace's people are known here");
    let member = people
        .iter()
        .find(|member| member.person == hex::encode(person))
        .expect("the linked workspace is shared with this person");
    assert_eq!(
        member.role,
        tilo_workspace_join::MemberRole::Editor,
        "in the part the invitation gave them"
    );
    assert!(!member.removed, "and the workspace has not removed them");
    // The slot is made once the host admits the device, and the person lands in it: wait for the
    // desk to show it, as the person would see it.
    wait_until(
        &mut session,
        "the linked workspace is on the desk",
        |session| {
            held(session)
                .iter()
                .any(|(_, _, workspace)| workspace == &lounge.workspace)
        },
    );
    let joined = held(&session)
        .into_iter()
        .find(|(_, _, workspace)| workspace == &lounge.workspace)
        .expect("the workspace is on the desk");
    assert_eq!(
        joined.1, "Lounge",
        "under the name the site's entry gives it"
    );
    wait_until_the_directory_is_done(&mut session);
}

/// The lounge's workspace as its people left it: two strips, and three windows written by another
/// member. What a computer that joins sees.
fn the_lounges_desktop(person: &str) -> SharedDesktop {
    shared_desktop(
        person,
        &[
            (0x41, "Talk", &[(0x42, "Chat"), (0x43, "Notes")]),
            (0x44, "Work", &[(0x45, "Sheets")]),
        ],
    )
}

/// A computer joins the lounge and shows its workspace as the site holds it: the strips and windows
/// another member laid out, a window they open or move afterwards, and their pointer drawn in the
/// workspace where they are.
#[test]
fn a_computer_that_joins_the_lounge_shows_its_desktop_as_the_site_holds_it() {
    let _settled = ui::motion::testing::settled();
    tilo_secrets::keep_in_memory_for_process();
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    let (_root, person) = this_computer_with_an_identity(&mut home);
    let lounge = Lounge::start(person, "Coffee's on");
    // The site's people left the workspace's desktop as two strips of windows; another member wrote
    // them before this computer joined.
    let member = hex::encode(lounge.page_device.user_public_key);
    let laid_out = the_lounges_desktop(&member);
    lounge.page_writes_desktop(&laid_out);
    lounge.wait_for_desktop(&laid_out);
    // The lounge is not this computer's community: the person types its address into the sheet, as
    // anyone joining a site they were given does.
    let _config = community_site(home.env(), "");
    let state = tempfile::tempdir().expect("a directory for this computer's desk");
    let mut session = collaborative_session(state.path());
    session.step(Duration::from_millis(1)).unwrap();
    join_the_lounge(&mut session, &lounge);
    // The window another member works in: the work strip's only window.
    let sheets = shared_desktop_tab(&laid_out, "Sheets");
    assert!(sheets != 0, "the site opened a window in the work strip");
    // The workspace in front is the lounge, and it shows exactly what the site holds: the same
    // strips, with the same windows in them.
    wait_until_desk(
        &mut session,
        &lounge.workspace,
        &recorded_layout(&laid_out),
        "the lounge's desktop arrives as the site holds it",
    );
    let active = session.handle().read(|tree| {
        tree.active_workspace()
            .map(|workspace| workspace.id().to_string())
    });
    assert_eq!(
        active.as_deref(),
        Some(lounge.workspace.as_str()),
        "the workspace in front is the one joined"
    );
    assert_eq!(
        desk_layout(&session, &lounge.workspace),
        recorded_layout(&laid_out),
        "the same strips, with the same windows in them"
    );

    // Another member opens a window beside one of theirs, and moves one of theirs: the desk here
    // follows without anyone touching it.
    let reopened = another_member_edits(laid_out, &member);
    lounge.page_writes_desktop(&reopened);
    lounge.wait_for_desktop(&reopened);
    wait_until_desk(
        &mut session,
        &lounge.workspace,
        &recorded_layout(&reopened),
        "the window another member opened and the one they moved arrive here",
    );

    the_pointer_follows(&mut session, &lounge);

    // The sheets window is the one the site opened first: it moved with the rest, and is not this
    // computer's to see twice.
    assert!(
        desk_layout(&session, &lounge.workspace)
            .iter()
            .flat_map(|(_, windows)| windows.clone())
            .any(|title| title == "Sheets"),
        "the window the site opened is still there: {:?}",
        desk_layout(&session, &lounge.workspace)
    );
}

/// Where the desk draws the tile of window `tab` , in the surface's coordinates, as of the last
/// layout: `None` while it is not shown.
fn tile_here(session: &Session, tab: u64) -> Option<ui::kurbo::Rect> {
    session
        .handle()
        .shown_tiles()
        .into_iter()
        .find(|(shown, _)| *shown == tab)
        .map(|(_, rect)| rect)
}

/// [`wait_until`], for a desk that must come to show what the site holds: says what the desk shows
/// instead when it never does.
#[track_caller]
fn wait_until_desk(
    session: &mut Session,
    workspace: &str,
    want: &[(String, Vec<String>)],
    what: &str,
) {
    let shown = common::wait_maybe(session, |session| {
        (desk_layout(session, workspace) == want).then_some(())
    });
    assert!(
        shown.is_some(),
        "{what}: the desk shows {:?}, the site holds {want:?}; the shell's refusals: {:?}",
        desk_layout(session, workspace),
        session.shell_refusals()
    );
}

/// The desktop as another member leaves it: a window they open beside one of theirs in the work
/// strip, and the chat window they move there from the talk strip.
fn another_member_edits(mut reopened: SharedDesktop, member: &str) -> SharedDesktop {
    let work = reopened
        .tree
        .workspaces
        .first_mut()
        .and_then(|workspace| workspace.strips.get_mut(1))
        .expect("the work strip");
    work.window_groups.insert(
        0,
        WindowGroup {
            id: GroupId::from(0x47_u64),
            tabs: vec![Tab {
                id: TabId::from(0x46_u64),
                title: "Files".to_owned(),
                tile: TileContent::LocalApp {
                    app_id: "so.tilo.files".to_owned(),
                    label: "Files".to_owned(),
                    note: String::new(),
                },
                app_title: None,
                view: String::new(),
                pinned: false,
            }],
            active_tab: TabId::from(0x46_u64),
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        },
    );
    reopened.authors.insert(0x46, member.to_owned());
    reopened.authors.insert(0x47, member.to_owned());
    // Chat moves into the work strip, after the files.
    let talk = reopened
        .tree
        .workspaces
        .first_mut()
        .and_then(|workspace| workspace.strips.get_mut(0))
        .expect("the talk strip");
    let chat = talk.window_groups.remove(0);
    reopened
        .tree
        .workspaces
        .first_mut()
        .and_then(|workspace| workspace.strips.get_mut(1))
        .expect("the work strip")
        .window_groups
        .push(chat);
    reopened
}

/// The window the workspace in front shows: the active strip's first.
fn window_in_front(session: &Session) -> Option<TabId> {
    session.handle().read(|tree| {
        let workspace = tree.active_workspace()?;
        let strip = tilo_desktop_surface::navigation::active_strip(tree, workspace)?;
        workspace
            .strips
            .iter()
            .find(|held| held.id == strip)?
            .window_groups
            .first()
            .map(|group| group.active_tab)
    })
}

/// The pointer of a person working in another member's window: the desk draws it there, wherever
/// the person here is looking, without the roster being opened, and follows where they move it.
fn the_pointer_follows(session: &mut Session, lounge: &Lounge) {
    // Their pointer, over a window they work in: the desk draws it there,
    // wherever the person here is looking, without the roster being opened.
    // The window is on the strip in front, so its tile is where the pointer
    // lands.
    let front = window_in_front(session);
    let shown = front.expect("the workspace in front shows a strip with a window");
    other_member_arrives(lounge);
    wait_until_their_pointer(
        session,
        lounge,
        shown,
        (12.0, 34.0),
        "the other member's pointer is drawn here",
    );
    let drawn = session.drawn_peer_pointers();
    let tile = tile_here(session, shown).expect("the window in front draws its tile");
    assert_eq!(
        drawn.len(),
        1,
        "one other person's pointer is drawn: {drawn:?}"
    );
    let (x, y) = drawn[0].1;
    assert!(
        (x - (tile.x0 + 12.0)).abs() < 0.5 && (y - (tile.y0 + 34.0)).abs() < 0.5,
        "drawn over the window's tile where they put it: {drawn:?}, tile {tile:?}"
    );
    // And it follows them.
    wait_until_their_pointer(
        session,
        lounge,
        shown,
        (40.0, 55.0),
        "the other member's pointer follows where they moved it",
    );
}

/// The id of the window titled `title` on `desktop` .
fn shared_desktop_tab(desktop: &SharedDesktop, title: &str) -> u64 {
    desktop
        .tree
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .find(|tab| tab.title == title)
        .map_or_else(
            || panic!("the desktop holds a window titled {title}"),
            |tab| tab.id,
        )
}

/// Join the lounge's listed workspace and wait until its content and its people reached this
/// computer.
fn join_the_lounge(session: &mut Session, lounge: &Lounge) {
    open_directory(session);
    look_up_site(session, &lounge.entry);
    ask_to_join(session, &lounge.workspace);
    wait_until_the_lounge_is_here(session, lounge);
}

/// The lounge's other member says they are here, as every device does when it joins: their presence
/// in the workspace, under their own device.
fn other_member_arrives(lounge: &Lounge) {
    let page = lounge.page.as_ref().expect("the site's page runs");
    page.send(Cmd::Presence(
        tilo_relay_protocol::room_workspace_id(&lounge.workspace).expect("a workspace id"),
        vec![collab_types::presence::Presence::desktop(
            &lounge.workspace,
            collab_types::PeerPresencePayload::Arrived,
        )],
    ))
    .expect("the other member says they are here");
}

/// The lounge's other member moves their pointer to `(x, y)` of the tile of `tab` in the workspace.
fn other_member_points_at(lounge: &Lounge, tab: u64, x: f32, y: f32) {
    let page = lounge.page.as_ref().expect("the site's page runs");
    page.send(Cmd::Presence(
        tilo_relay_protocol::room_workspace_id(&lounge.workspace).expect("a workspace id"),
        vec![collab_types::presence::Presence::pointing(
            &lounge.workspace,
            tab,
            x,
            y,
        )],
    ))
    .expect("the other member's pointer moves");
}

/// How long the desk is stepped between one pointing and the next.
const POINT_AGAIN: Duration = Duration::from_millis(200);

/// Wait until the desk draws the other member's pointer at `(x, y)` over the tile of `tab` ,
/// pointing again as it waits.
/// Their pointer is a live frame the host fans out and never keeps: a host whose queue for this
/// device is full drops it (that is what ephemeral means), and a person's pointer keeps moving, so
/// the test says where they are more than once rather than sampling a frame that may have been
/// shed. What the wait is for is unchanged — the desk drawing their pointer where they put it — and
/// its only bound is the suite's hang bound.
#[track_caller]
fn wait_until_their_pointer(
    session: &mut Session,
    lounge: &Lounge,
    tab: u64,
    at: (f32, f32),
    what: &str,
) {
    let deadline = std::time::Instant::now()
        .checked_add(tilo_test_env::hang_bound())
        .expect("the wait's bound fits in an Instant");
    loop {
        other_member_points_at(lounge, tab, at.0, at.1);
        let drawn = common::steps_within(session, POINT_AGAIN, |session| {
            pointer_here(session, tab, at).then_some(())
        });
        if drawn.is_some() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "gave up waiting for {what} after {:?}; it looks hung (set TILO_TEST_HANG_BOUND_SECS to wait longer).\n\
             the shell's refusals: {:?}\nthe session says: {:?}\nthe pointers drawn: {:?}\n\
             the other member's channels and planes: {}",
            tilo_test_env::hang_bound(),
            session.shell_refusals(),
            session.accessible_labels(),
            session.drawn_peer_pointers(),
            lounge
                .page
                .as_ref()
                .map(|page| page.status().channels)
                .unwrap_or_default()
        );
    }
}

/// Whether the desk drew someone's pointer at `(x, y)` of the tile of `tab` .
fn pointer_here(session: &Session, tab: u64, (x, y): (f32, f32)) -> bool {
    session
        .drawn_peer_pointers()
        .iter()
        .any(|(_, (at_x, at_y))| {
            tile_here(session, tab).is_some_and(|tile| {
                (at_x - (tile.x0 + f64::from(x))).abs() < 0.5
                    && (at_y - (tile.y0 + f64::from(y))).abs() < 0.5
            })
        })
}

/// Wait until the lounge's workspace and its people reached this computer,
/// saying which of the two is still missing when they do not.
fn wait_until_the_lounge_is_here(session: &mut Session, lounge: &Lounge) {
    // The join is the site's door, the workspace's keys and its roster arriving from other
    // runtimes, and the session gives a host its own patience to answer them: the wait spans that
    // patience and the suite's hang bound after it, so a host answering slowly is waited for rather
    // than called hung, and the session's note on a join that did not happen is what the test
    // reports when the wait runs out.
    let here = common::wait_maybe_past(session, tilo_session::sync::JOIN_PATIENCE, |session| {
        let held = session.held_workspaces();
        let people = session.people(&lounge.workspace);
        (held.iter().any(|workspace| workspace == &lounge.workspace)
            && people.as_ref().is_ok_and(|people| {
                people
                    .iter()
                    .any(|member| member.person == hex::encode(lounge.person))
            }))
        .then_some(())
    });
    assert!(
        here.is_some(),
        "the lounge's workspace is held here: held {:?}, people {:?}, directory {:?}",
        session.held_workspaces(),
        session.people(&lounge.workspace),
        session
            .workspace_directory()
            .map(tilo_sheets::directory::DirectorySeat::directory)
    );
}

/// The line the lounge's chat holds here, as the person reads it: `None` until this computer may
/// open the record's sealed history.
fn chat_here(session: &Session, workspace: &str) -> Option<String> {
    // The workspace may hold nothing yet, and until this computer is admitted to the record's
    // channel its writes are not this computer's to open.
    let Ok(Some(record)) = session.read_record(workspace, CHAT, CHAT_RECORD) else {
        return None;
    };
    let chat = automerge::AutoCommit::load(&record).ok()?;
    let (text, _) = chat.get(automerge::ROOT, "text").ok()??;
    text.into_string().ok()
}

/// The publisher the lounge's host is known by.
fn host_publisher(lounge: &Lounge) -> String {
    publisher_of(&lounge.host_cert)
}

/// The publisher the lounge's page is known by.
fn page_publisher(lounge: &Lounge) -> String {
    publisher_of(&lounge.page_device)
}

/// The publisher a device's session is known by.
fn publisher_of(cert: &tilo_user_identity::DeviceCert) -> String {
    iroh::EndpointId::from_bytes(&cert.device_public_key)
        .expect("a device key is a publisher")
        .to_string()
}

#[test]
fn a_workspace_joined_brings_the_chat_written_before_it() {
    let _settled = ui::motion::testing::settled();
    // This computer keeps its own channel state in the platform secret store,
    // which the test keeps in memory.
    tilo_secrets::keep_in_memory_for_process();
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    let (_root, person) = this_computer_with_an_identity(&mut home);
    let lounge = Lounge::start(person, "Coffee's on");
    // The lounge is not this computer's community: the person types its address into the sheet, as
    // anyone joining a site they were given does.
    let _config = community_site(home.env(), "");
    let state = tempfile::tempdir().expect("a directory for this computer's desk");
    let mut session = collaborative_session(state.path());
    session.step(Duration::from_millis(1)).unwrap();
    join_the_lounge(&mut session, &lounge);
    wait_until(
        &mut session,
        "the lounge's chat, written before this computer joined, reads here",
        |session| chat_here(session, &lounge.workspace).as_deref() == Some("Coffee's on"),
    );
    let chat = chat_here(&session, &lounge.workspace).expect("the lounge's chat opened here");
    assert_eq!(
        chat, "Coffee's on",
        "the line the lounge wrote before this computer joined reads as it was written"
    );
    // The record the line came in is the workspace's, sealed as its host keeps it: what the joined
    // device read it through is the channel service, not a plain copy of the workspace's document.
    let record = session
        .read_record(&lounge.workspace, CHAT, CHAT_RECORD)
        .expect("the chat is this computer's to read")
        .expect("the chat is here");
    assert!(
        automerge::AutoCommit::load(&record).is_ok(),
        "and it is the chat app's own document"
    );
    // The workspace's window streams carry between the devices the workspace admits:
    // its host, the page that recorded there before this computer arrived,
    // and this computer itself.
    let roster = session
        .roster(&lounge.workspace)
        .expect("the workspace's roster is known here");
    assert_eq!(roster.len(), 3, "the workspace's devices: {roster:?}");
    for device in [host_publisher(&lounge), page_publisher(&lounge)] {
        assert!(
            roster.contains(&device),
            "the lounge's own devices are among them: {roster:?}"
        );
    }
}

#[test]
fn a_member_the_lounge_removes_leaves_its_roster_and_reads_nothing_more() {
    let _settled = ui::motion::testing::settled();
    tilo_secrets::keep_in_memory_for_process();
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    let (_root, person) = this_computer_with_an_identity(&mut home);
    let lounge = Lounge::start(person, "Pulled up a chair");
    // The lounge is not this computer's community: the person types its address into the sheet, as
    // anyone joining a site they were given does.
    let _config = community_site(home.env(), "");
    let state = tempfile::tempdir().expect("a directory for this computer's desk");
    let mut session = collaborative_session(state.path());
    session.step(Duration::from_millis(1)).unwrap();
    join_the_lounge(&mut session, &lounge);
    wait_until(
        &mut session,
        "the lounge's chat reads here while this computer is a member",
        |session| chat_here(session, &lounge.workspace).is_some(),
    );
    let lounge_devices = [host_publisher(&lounge), page_publisher(&lounge)];
    let roster = session
        .roster(&lounge.workspace)
        .expect("the workspace's roster is known here");
    assert!(
        roster.iter().any(|device| !lounge_devices.contains(device)),
        "this computer is on the roster beside the lounge's own devices: {roster:?}"
    );
    assert_eq!(
        roster.len(),
        3,
        "the lounge's devices and this computer: {roster:?}"
    );

    // The lounge's owner removes this person, and every device of theirs.
    block_on_within(
        &lounge.runtime,
        "the lounge to remove this person",
        lounge.workspaces.remove_member(
            &lounge.workspace,
            &lounge.host_cert,
            RemoveMemberRequest {
                request_id: [0x62; 32],
                user_public_key: person,
            },
        ),
    )
    .expect("the lounge removes this person");
    // The workspace's governance is what every device's roster is read from, and it lists this
    // person no longer: the devices of theirs on the workspace's roster are gone.
    let (_generation, members) = block_on_within(
        &lounge.runtime,
        "the lounge's members",
        lounge.workspaces.membership(&lounge.workspace),
    )
    .expect("the lounge's members");
    let mark = members
        .get(&hex::encode(person))
        .expect("the workspace remembers the person it removed");
    assert!(mark.removed, "the workspace removed this person: {mark:?}");
    // The page, still a member, hears the workspace's governance and lists the person no more, as
    // every device's roster follows it.
    let page = lounge.page.as_ref().expect("the site's page runs");
    let removed = hex::encode(person);
    tilo_test_env::wait_until(
        "the lounge's remaining devices to hear its governance",
        || {
            page.status()
                .room_members
                .get(&lounge.workspace)
                .is_some_and(|members| !members.contains(&removed))
        },
    );

    // The lounge's page writes after the removal: the workspace's content from now on is not this
    // computer's, so the line never reads here.
    let route = DocumentRoute {
        app_id: CHAT.to_owned(),
        cluster_id: lounge.workspace.clone(),
        record: CHAT_RECORD.to_owned(),
    };
    let mut after = automerge::AutoCommit::load(
        &block_on_within(
            &lounge.runtime,
            "the lounge's chat",
            lounge
                .workspaces
                .record_document(&lounge.workspace, CHAT, CHAT_RECORD),
        )
        .expect("the lounge's chat")
        .expect("the lounge holds its chat"),
    )
    .expect("the chat's document");
    let before = after.get_heads();
    after
        .put(automerge::ROOT, "text", "The door's shut")
        .expect("a chat line after the removal");
    after.commit();
    page.write(
        route,
        &DocumentMutation::Incremental(after.save_after(&before)),
    )
    .expect("the site's page goes on talking without this computer");
    let held = std::time::Instant::now()
        .checked_add(Duration::from_secs(5))
        .expect("a deadline");
    while std::time::Instant::now() < held {
        session.step(Duration::from_millis(1)).unwrap();
        assert_eq!(
            chat_here(&session, &lounge.workspace).as_deref(),
            Some("Pulled up a chair"),
            "what the lounge wrote after the removal never reads here"
        );
    }
}
