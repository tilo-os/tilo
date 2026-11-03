//! "Join a workspace…" as a person on a computer set up for a community
//! meets it: opening the sheet reads that site's directory and lists the
//! public workspaces it serves, who is in each one now and how many belong.
//! The workspaces this computer holds open instead of joining, and a site
//! that cannot be read says so without hiding them.
//!
//! Joining one asks the door its listing names, as the person at this
//! computer and by their own name, and the invitation the door answers with
//! is what puts the workspace on the desk: one it holds from then on, open
//! rather than to join, with everything the workspace held before this
//! computer arrived. A door that refuses says why, where the person
//! pressed, and leaves the workspace one to try again.
//!
//! A link or a code pasted into the sheet joins the same way: an
//! invitation's text is redeemed where it names, and a workspace's link
//! joins the workspace on the site it points at.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use automerge::ReadDoc as _;
use automerge::transaction::Transactable as _;
use collab_types::DocumentMutation;
use ed25519_dalek::Signer as _;
use sha2::Digest as _;
use test_http::{Reply, Responder};
use tilo_browser_peer::{Host, MemoryPeerStore, PeerConfig, PeerEvent, PeerHandle};
use tilo_compositor::OutputConfig;
use tilo_desktop::desk_verbs::CHROME_RESOURCE;
use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_RECORD, SharedDesktop};
use tilo_desktop_model::{
    GroupId, Strip, StripId, Tab, TabId, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceSlot, shared_record,
};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_relay::rooms::{PreAdmitRequest, RemoveMemberRequest};
use tilo_relay_protocol::directory::{DirectoryEntry, DirectoryPage, Face, door_path};
use tilo_relay_protocol::rooms::{PreAdmission, RoomRole};
use tilo_session::{Options, Session};
use tilo_sheets::Seat as _;
use tilo_sheets::directory::{Listing, ListingAction, Status};

// A host admits its members' records through an Automerge worker: this
// binary is that worker, as it is the session's own.
tilo_automerge_isolate::test_worker!();

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;

/// The app the lounge's workspace talks in, and the record its people wrote
/// there before this computer joined: the chat's own workspace record.
const CHAT: &str = "so.tilo.chat";
const CHAT_RECORD: &str = "";
/// How long the lounge's chat ran before this computer joined: long enough
/// that the host asks the page that wrote it for the record, as it does
/// once a workspace's sealed history is a fair length.
const HISTORY: i64 = 300;

const COMMUNITY_WORKSPACE: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
const CLUB_WORKSPACE: &str = "1b7c4e08-3a5d-42f6-8c9e-2d6f0a1b3c5e";

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

/// Point this computer at the community site `site` for the test's
/// lifetime. Returns the configuration's directory, which goes with it.
fn community_site(env: &mut tilo_test_env::ScopedEnv, site: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a directory for the configuration");
    let config = dir.path().join("community.toml");
    std::fs::write(&config, format!("site = \"{site}\"\n")).expect("the configuration is written");
    env.set("TILO_COMMUNITY_CONFIG", &config);
    dir
}

/// The workspaces this computer holds: the desk's id, the name shown and the
/// workspace's id.
fn held(session: &Session) -> Vec<(String, String, String)> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .map(|held| {
                (
                    held.slot.0.to_string(),
                    held.name.clone(),
                    held.room().to_string(),
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
    // The site's directories are answered by other runtimes, and a busy
    // machine takes as long at that as the suite allows, not a second count.
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
        room: COMMUNITY_WORKSPACE.to_owned(),
        name: "Tilers".to_owned(),
        description: "Anyone who lays tiles".to_owned(),
        default_role: RoomRole::Editor,
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
        room: CLUB_WORKSPACE.to_owned(),
        name: "Book club".to_owned(),
        description: String::new(),
        default_role: RoomRole::Viewer,
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

/// A site that lists `entries` and answers the door of the workspace named
/// `workspace` with `door`, a function of the join request it was asked.
fn joining_site(
    workspace: &str,
    entries: Vec<DirectoryEntry>,
    door: impl Fn(&test_http::Request) -> Reply + Send + Sync + 'static,
) -> Responder {
    let page = page_of(entries);
    let door_path = format!("/directory/{workspace}/join");
    Responder::handle(move |request| {
        let body = match (request.method.as_str(), request.target.as_str()) {
            ("GET", "/entry.json") => {
                return Some(
                    Reply::status(200)
                        .header("content-type", "application/json")
                        .body(r#"{"directory":"directory"}"#),
                );
            }
            ("POST", "/directory") => page.clone(),
            ("POST", target) if target == door_path => return Some(door(request)),
            _ => return Some(Reply::status(404)),
        };
        Some(
            Reply::status(200)
                .header("content-type", "application/json")
                .body(body),
        )
    })
}

/// This computer's person: a device of theirs, certified for their own root
/// key, kept in an identity root of the test's own. Returns the root's
/// directory, and the key a door's invitation for them is bound to.
fn this_computer(env: &mut tilo_test_env::ScopedEnv) -> (tempfile::TempDir, [u8; 32]) {
    let root = tempfile::tempdir().expect("a directory for this computer's identity");
    let paths = tilo_user_identity::IdentityPaths::under(root.path());
    std::fs::create_dir_all(paths.device_privkey.parent().expect("the key's directory"))
        .expect("this computer's key directory");
    let person = ed25519_dalek::SigningKey::from_bytes(&[0x21; 32]);
    let device = ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]);
    tilo_user_identity::persist::write_device_key(&paths.device_privkey, &device)
        .expect("this computer's device key");
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: person.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "The laptop".to_owned(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut cert, &person).expect("a certified device");
    tilo_user_identity::persist::write_device_cert(&paths.device_cert, &cert)
        .expect("this computer's certificate");
    env.set("TILO_IDENTITY_ROOT", root.path());
    // The name they go by, which the workspace learns of them: never their
    // device's.
    ui::local_identity::install_source(Some(Arc::new(ui::local_identity::FixedIdentity(
        ui::local_identity::LocalIdentity {
            group_public_key: cert.user_public_key,
            display_name: "Ada".to_owned(),
            avatar_color: [40, 80, 120],
        },
    ))));
    (root, cert.user_public_key)
}

/// A door's answer letting `person` into `workspace`: an invitation bound to
/// them, signed by a device of the workspace's host.
fn invitation_for(workspace: &str, person: [u8; 32]) -> String {
    let host = ed25519_dalek::SigningKey::from_bytes(&[0x31; 32]);
    let host_device = ed25519_dalek::SigningKey::from_bytes(&[0x32; 32]);
    let now = tilo_clock::unix_now_ns();
    let mut issuer = tilo_user_identity::DeviceCert {
        user_public_key: host.verifying_key().to_bytes(),
        device_public_key: host_device.verifying_key().to_bytes(),
        device_label: "The book club's host".to_owned(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: now,
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut issuer, &host).expect("a certified host");
    let grant = tilo_workspace_join::InviteGrant {
        workspace_id: tilo_workspace_join::room_workspace_id(workspace).expect("a workspace's id"),
        room: workspace.to_owned(),
        issuer,
        role: tilo_workspace_join::MemberRole::Viewer,
        nonce: [0x33; 16],
        recipient_user_public_key: Some(person),
        invitee_name: None,
        expires_at_ns: now.saturating_add(3_600_000_000_000),
        transport: Vec::new(),
    };
    let signed =
        tilo_workspace_join::issue_invitation(grant, &host_device).expect("a signed invitation");
    serde_json::json!({
        "invitation": tilo_workspace_join::encode_invitation_text(&signed),
        "admissionToken": "one-time-token",
    })
    .to_string()
}

/// The resource publishing `action` among everything the session
/// publishes.
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

/// Press the sheet's button for joining the listing `key`.
fn ask_to_join(session: &mut Session, key: &str) {
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
    // What this waits for is other runtimes' work (the site's answer, the
    // record arriving), so the bound is the suite's, not a second count.
    let deadline = tilo_test_env::hang_deadline();
    loop {
        session.step(Duration::from_millis(10)).unwrap();
        if done(session) {
            return;
        }
        assert!(std::time::Instant::now() < deadline, "{what}");
    }
}

/// What the site lists: its own community workspace, a club, and the
/// workspace this computer holds already.
fn page_listing_the_workspace_on_this_desk(held_name: &str, held_room: &str) -> String {
    let mut entries = vec![community_entry(34, now_ms()), club_entry(now_ms())];
    entries.push(DirectoryEntry {
        room: held_room.to_owned(),
        name: held_name.to_owned(),
        description: "The one on this desk".to_owned(),
        default_role: RoomRole::Commenter,
        door: door_path(held_room),
        here: Vec::new(),
        here_count: 0,
        members: 3,
        last_active_ms: Some(now_ms()),
        community: false,
    });
    page_of(entries)
}

/// What the community's own workspace says of itself on its row, and of
/// the people in it now.
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
    let (held_id, held_name, held_room) = held(&session)
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
            (held_name, ListingAction::Open, held_id),
            (
                "Book club".to_owned(),
                ListingAction::Join,
                CLUB_WORKSPACE.to_owned()
            ),
        ],
        "the community's own first, then the liveliest, and the held workspace opens rather than joins"
    );
    asserts_the_communitys_row(&session);
    asserts_the_clubs_row(&session);
}

#[test]
fn a_site_that_cannot_be_read_says_so_and_still_lists_what_is_held() {
    let _settled = ui::motion::testing::settled();
    // A site with nothing at its entry.
    let responder = Responder::handle(|_| Some(Reply::status(404)));
    let mut env = tilo_test_env::ScopedEnv::new();
    let _config = community_site(&mut env, &responder.url());
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
    let mut env = tilo_test_env::ScopedEnv::new();
    let _config = community_site(&mut env, "");
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

#[test]
fn joining_a_listed_workspace_asks_its_door_and_lands_in_it() {
    let _settled = ui::motion::testing::settled();
    let mut env = tilo_test_env::ScopedEnv::new();
    let (_identity, person) = this_computer(&mut env);
    let responder = joining_site(
        CLUB_WORKSPACE,
        vec![community_entry(34, now_ms()), club_entry(now_ms())],
        move |_| {
            Reply::status(200)
                .header("content-type", "application/json")
                .body(invitation_for(CLUB_WORKSPACE, person))
        },
    );
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
    let asked = responder
        .drain_requests()
        .into_iter()
        .find(|request| request.method == "POST" && request.target.ends_with("/join"))
        .expect("the sheet asked the club's door");
    assert_eq!(
        asked.target,
        format!("/directory/{CLUB_WORKSPACE}/join"),
        "at the door the listing named"
    );
    let body: serde_json::Value =
        serde_json::from_slice(&asked.body).expect("the join request a door reads");
    assert_eq!(
        body,
        serde_json::json!({
            "userPublicKey": hex::encode(person),
            "displayName": "Ada",
        }),
        "as this person, by the name they go by rather than their device's"
    );
    assert!(
        !session
            .workspace_directory()
            .expect("the session hosts the workspace directory")
            .presentation()
            .is_open(),
        "and the sheet is done, leaving them in the workspace they joined"
    );
    // Opening the directory again shows the club as one this computer holds.
    open_directory(&mut session);
    assert_eq!(
        listing(&session, "Book club").action,
        ListingAction::Open,
        "the club is no longer one to join: it is one to open"
    );
}

#[test]
fn a_door_that_refuses_says_why_and_offers_to_try_again() {
    let _settled = ui::motion::testing::settled();
    let mut env = tilo_test_env::ScopedEnv::new();
    let (_identity, _person) = this_computer(&mut env);
    let responder = joining_site(CLUB_WORKSPACE, vec![club_entry(now_ms())], |_| {
        Reply::status(403)
            .header("content-type", "application/json")
            .body(r#"{"error":"this workspace is for members only"}"#)
    });
    let _config = community_site(&mut env, &responder.url());
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    open_directory(&mut session);
    ask_to_join(&mut session, CLUB_WORKSPACE);
    wait_until(&mut session, "the door answered", |session| {
        listing(session, "Book club").action == ListingAction::TryAgain
    });
    let sheet = session
        .workspace_directory()
        .expect("the session hosts the workspace directory");
    let shown = sheet.directory();
    assert_eq!(
        shown.note.as_deref(),
        Some(
            "Couldn't join Book club: the workspace did not let you in: this workspace is for members only"
        ),
        "the sheet says why it is not joined"
    );
    let club = listing(&session, "Book club");
    assert_eq!(
        club.activity,
        "Didn't join: the workspace did not let you in: this workspace is for members only",
        "and the row says so where the person pressed"
    );
    assert!(
        sheet.presentation().is_open(),
        "the sheet stays up to pick another workspace"
    );
    assert!(
        !held(&session)
            .iter()
            .any(|(_, _, workspace)| workspace == CLUB_WORKSPACE),
        "and no workspace was added"
    );
}

/// A session that takes part in the collaboration plane, keeping the workspaces
/// it holds beside `state`.
fn collaborative_session(state: &std::path::Path) -> Session {
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: true,
            state: Some(state.to_path_buf()),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session")
}

/// `key`'s certificate as a device of the person whose root key is `root`.
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
    };
    tilo_user_identity::sign_device_cert(&mut cert, root).expect("a certified device");
    cert
}

/// The token a page spends at the lounge's door, for the one invitation it
/// redeems.
const TOKEN: &[u8] = b"the lounge's one-time token";

/// A grant admitting `person` to `workspace` as an editor, signed by the lounge's
/// owner `root` as its door signs one.
fn grant_for(owner_key: &ed25519_dalek::SigningKey, workspace: &str, label: &str) -> PreAdmission {
    let mut grant = PreAdmission {
        room: workspace.to_owned(),
        role: RoomRole::Editor,
        token_sha256: sha2::Sha256::digest(TOKEN).into(),
        label: label.to_owned(),
        expires_at_ns: tilo_clock::unix_now_ns().saturating_add(600_000_000_000),
        owner_user_public_key: owner_key.verifying_key().to_bytes(),
        signature: [0; 64],
    };
    grant.signature = owner_key
        .sign(&grant.digest().expect("a grant to sign"))
        .to_bytes();
    grant
}

/// This computer's person: an identity of its own in `home`'s
/// configuration, and a device certified for it under a key root of the
/// test's own. Returns the root's directory, and the person's key.
fn this_computer_with_an_identity(
    home: &mut tilo_test_env::TestHome,
) -> (tempfile::TempDir, [u8; 32]) {
    let root = tempfile::tempdir().expect("a directory for this computer's identity");
    let identity_dir = home.config().join("tilo").join("identity");
    tilo_identity_host::provisioning::ensure_collaboration_identity(
        &identity_dir,
        &tilo_user_identity::IdentityPaths::under(root.path()),
        tilo_identity_host::provisioning::IdentityStorage::Files,
    )
    .expect("this computer's identity");
    home.env().set("TILO_IDENTITY_ROOT", root.path());
    let person = runtime_identity::list_identities(&identity_dir)
        .expect("this computer's identities")
        .into_iter()
        .next()
        .expect("an identity")
        .public_key;
    // The name they go by, which the site's door learns of them: never
    // their device's.
    ui::local_identity::install_source(Some(Arc::new(ui::local_identity::FixedIdentity(
        ui::local_identity::LocalIdentity {
            group_public_key: person,
            display_name: "Ada".to_owned(),
            avatar_color: [40, 80, 120],
        },
    ))));
    (root, person)
}

/// A chat record holding `line`: what one of the lounge's people wrote.
fn chat_with(line: &str) -> automerge::AutoCommit {
    use automerge::ROOT;
    let mut document = automerge::AutoCommit::new();
    document.put(ROOT, "text", line).expect("a chat line");
    document.commit();
    document
}

/// A site's workspace as a site runs it: a host on this machine governs the
/// workspace, a page of the site's person recorded there before anyone joined
/// this computer, and the site's entry and door stand in front of it.
///
/// Dropping it takes the page down before the runtime it runs on, and shuts
/// the host down.
struct Lounge {
    page: Option<PeerHandle>,
    relay: Option<tilo_relay::daemon::Relay>,
    _door: Responder,
    runtime: tokio::runtime::Runtime,
    rooms: tilo_relay::rooms::Rooms,
    host_cert: tilo_user_identity::DeviceCert,
    /// The certificate of the site's page, the device that recorded in the
    /// workspace before anyone joined.
    page_device: tilo_user_identity::DeviceCert,
    /// The page's own copy of the workspace's shared desktop: what it laid
    /// out, and the base of every later change it writes.
    page_desktop: std::cell::RefCell<automerge::AutoCommit>,
    person: [u8; 32],
    /// The site's workspace, by id.
    workspace: String,
    /// The site's entry, where its links and its directory hang.
    entry: String,
    /// The site's own state, which goes with it.
    _state: tempfile::TempDir,
}

impl Drop for Lounge {
    fn drop(&mut self) {
        drop(self.page.take());
        if let Some(relay) = self.relay.take() {
            self.runtime.block_on(relay.shutdown());
        }
    }
}

impl Lounge {
    /// The lounge's workspace, whose chat already holds `written`.
    fn start(person: [u8; 32], written: &str) -> Self {
        let state = tempfile::tempdir().expect("a directory for the lounge's host");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a runtime for the lounge's host");
        let host = LoungeHost::start(&runtime, state.path());
        let page = host.page(&runtime, written);
        let (door, entry) = host.door(&runtime, person);
        Self {
            page: Some(page),
            relay: Some(host.relay),
            _door: door,
            runtime,
            rooms: host.rooms,
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

/// The host behind the lounge's site: a relay of this machine, certified
/// under a key root of the test's own, which governs the lounge's workspace and
/// keeps what its page writes sealed.
struct LoungeHost {
    relay: tilo_relay::daemon::Relay,
    rooms: tilo_relay::rooms::Rooms,
    /// The key the host's certificates are signed under.
    owner_key: ed25519_dalek::SigningKey,
    host_cert: tilo_user_identity::DeviceCert,
    /// The key and certificate of the site's page, the device that recorded
    /// in the workspace before anyone joined.
    page_key: ed25519_dalek::SigningKey,
    page_device: tilo_user_identity::DeviceCert,
    workspace: String,
}

impl LoungeHost {
    /// A host governing a workspace of its own, with the site's page entered in
    /// it.
    fn start(runtime: &tokio::runtime::Runtime, state: &std::path::Path) -> Self {
        let owner_key = ed25519_dalek::SigningKey::from_bytes(&[0x41; 32]);
        let host_key = ed25519_dalek::SigningKey::from_bytes(&[0x42; 32]);
        let host_cert = certified(
            &owner_key,
            &host_key,
            "The lounge's host",
            tilo_user_identity::cloud_enrollment::host_capabilities(),
        );
        let page_key = ed25519_dalek::SigningKey::from_bytes(&[0x43; 32]);
        let page_device = certified(
            &owner_key,
            &page_key,
            "The lounge's page",
            tilo_user_identity::DeviceCapabilities::member(),
        );
        let (relay, workspaces, workspace) = runtime.block_on(async {
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
            let workspaces = relay.rooms().expect("the host governs workspaces").clone();
            let workspace = workspaces
                .create("Lounge")
                .await
                .expect("the lounge's workspace")
                .room;
            // The site's page is in its workspace.
            workspaces
                .enter(&workspace, &page_device)
                .await
                .expect("the site's page is in its workspace");
            (relay, workspaces, workspace)
        });
        Self {
            relay,
            rooms: workspaces,
            owner_key,
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
    fn page(&self, runtime: &tokio::runtime::Runtime, written: &str) -> PeerHandle {
        let route = DocumentRoute {
            app_id: CHAT.to_owned(),
            cluster_id: self.workspace.clone(),
            record: CHAT_RECORD.to_owned(),
        };
        let page = {
            let _entered = runtime.enter();
            let page = PeerHandle::start(
                PeerConfig {
                    host: Host::Iroh {
                        addr: self.relay.node_addr(),
                        relay_url: None,
                    },
                    cert: self.page_device.clone(),
                    host_cert: self.host_cert.clone(),
                    revocations: tilo_user_identity::RevocationList::new(),
                    watching: false,
                    background: false,
                },
                Arc::new(self.page_key.clone()) as Arc<dyn tilo_browser_peer::DeviceSigner>,
                Arc::new(MemoryPeerStore::default()),
            );
            page.join_placed(&self.workspace, "Lounge")
                .expect("the site's page joins the lounge's workspace");
            page
        };
        let mut chat = chat_with(written);
        page.publish(route.clone(), &DocumentMutation::Snapshot(chat.save()))
            .expect("the site's page records in the lounge's chat");
        for line in 0..HISTORY {
            let before = chat.get_heads();
            chat.put(automerge::ROOT, format!("line{line}"), line)
                .expect("a chat line");
            chat.commit();
            page.publish(
                route.clone(),
                &DocumentMutation::Incremental(chat.save_after(&before)),
            )
            .expect("the site's page goes on talking");
        }
        // The line is the lounge's before anyone joins: the test proves a
        // join brings it, not that a write lands.
        // The page's writes reach the host through other runtimes' work, so the
        // bound is the suite's, not a second count.
        let deadline = tilo_test_env::hang_deadline();
        loop {
            for event in page.drain() {
                if let PeerEvent::CompactionRequested {
                    route: asked,
                    request,
                } = event
                {
                    assert_eq!(asked, route, "the record the page wrote");
                    page.compact(request, chat.save())
                        .expect("the page hands its record over");
                }
            }
            let held = runtime
                .block_on(self.rooms.record_routes(&self.workspace))
                .expect("the lounge's records");
            if held
                .iter()
                .any(|(app, record)| app == CHAT && record == CHAT_RECORD)
            {
                return page;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the page's line reaches the lounge's host: {held:?} {:?}",
                page.status(),
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The site as the world meets it: its entry, the directory of the
    /// workspaces it serves, and the door it admits `person` at, where a page
    /// spends the one-time token and is given an invitation only they can
    /// redeem. Returns the door and the site's entry.
    fn door(&self, runtime: &tokio::runtime::Runtime, person: [u8; 32]) -> (Responder, String) {
        let origin = Arc::new(Mutex::new(String::new()));
        let served_origin = Arc::clone(&origin);
        let page_of_the_site = page_of(vec![DirectoryEntry {
            room: self.workspace.clone(),
            name: "Lounge".to_owned(),
            description: "Where the site's people talk".to_owned(),
            default_role: RoomRole::Editor,
            door: door_path(&self.workspace),
            here: Vec::new(),
            here_count: 0,
            members: 2,
            last_active_ms: Some(now_ms()),
            community: true,
        }]);
        let behind = format!("/directory/{}/join", self.workspace);
        let workspace = self.workspace.clone();
        let answer = runtime.handle().clone();
        let admittances = Arc::new(AtomicU32::new(1));
        let next = Arc::clone(&admittances);
        let workspaces = self.rooms.clone();
        let host_cert = self.host_cert.clone();
        let owner_key = self.owner_key.clone();
        let door = Responder::handle(move |request| {
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
                            "admission": "door/",
                        },
                    })
                    .to_string(),
                ),
                ("POST", "/directory") => json(page_of_the_site.clone()),
                ("POST", asked) if asked == behind || asked == "/door/join" => {
                    // The door admits this person: an invitation only they
                    // can redeem, and the one-time token a page spends.
                    let mut request_id = [0u8; 32];
                    request_id[..4]
                        .copy_from_slice(&next.fetch_add(1, Ordering::Relaxed).to_le_bytes());
                    let issued = answer
                        .block_on(workspaces.pre_admit(
                            &host_cert,
                            PreAdmitRequest {
                                request_id,
                                grant: grant_for(&owner_key, &workspace, "Ada"),
                                recipient_user_public_key: Some(person),
                                inviter_name: Some("The lounge".to_owned()),
                            },
                        ))
                        .expect("an invitation for this person");
                    json(
                        serde_json::json!({
                            "invitation": tilo_workspace_join::encode_invitation_text(
                                &issued.signed_invitation
                            ),
                            "admissionToken": String::from_utf8_lossy(TOKEN),
                        })
                        .to_string(),
                    )
                }
                _ => Some(Reply::status(404)),
            }
        });
        *origin.lock().expect("the site's origin") = format!("{}/", door.url());
        let entry = format!("{}/", door.url());
        (door, entry)
    }
}

/// The strips and windows a desk shows in `workspace`, in order: each strip
/// by its name and the titles of the windows in it.
fn desk_layout(session: &Session, workspace: &str) -> Vec<(String, Vec<String>)> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .find(|held| held.room().to_string() == workspace)
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

/// The strips and windows a shared desktop holds, as [`desk_layout`] gives a
/// desk's: what every device showing the workspace should see.
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

/// A workspace's shared desktop, as the people in it left it: `strips` names
/// each strip with the windows opened in it, all of them `person`'s.
fn shared_desktop(person: &str, strips: &[(u64, &str, &[(u64, &str)])]) -> SharedDesktop {
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
    }
}

impl Lounge {
    /// The site's page lays its workspace out as `desktop` says: the strips
    /// and windows its people see there, written into the workspace's shared
    /// desktop record as every member's page writes it.
    fn page_writes_desktop(&self, desktop: &SharedDesktop) {
        let page = self.page.as_ref().expect("the site's page runs");
        let person = hex::encode(self.page_device.user_public_key);
        let mut doc = self.page_desktop.borrow_mut();
        let before = doc.get_heads();
        // The record's first publication in a session is a snapshot: the
        // route it is kept on is founded by it, which is what a host replays
        // to the devices that join later.
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
        page.publish(
            DocumentRoute {
                app_id: DESKTOP_APP.to_owned(),
                cluster_id: self.workspace.clone(),
                record: DESKTOP_RECORD.to_owned(),
            },
            &mutation,
        )
        .expect("the site's page lays out the workspace's desktop");
    }

    /// Wait until the lounge's host holds `desktop` as its shared desktop:
    /// the page's write reached it, and every device that joins now sees it.
    fn wait_for_desktop(&self, desktop: &SharedDesktop) {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let held = self
                .runtime
                .block_on(
                    self.rooms
                        .record_document(&self.workspace, DESKTOP_APP, DESKTOP_RECORD),
                )
                .expect("the lounge's desktop record");
            let recorded = held
                .and_then(|bytes| automerge::AutoCommit::load(&bytes).ok())
                .and_then(|doc| shared_record::read(&doc, None));
            if recorded.as_ref().map(recorded_layout) == Some(recorded_layout(desktop)) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the page's desktop reaches the lounge's host: {:?}",
                recorded.as_ref().map(recorded_layout)
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// A link to the lounge's workspace carrying an invitation for this
    /// person, as whoever shares the workspace copies it.
    fn invitation_link(&self) -> String {
        let issued = self
            .runtime
            .block_on(self.rooms.issue(
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
            ))
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
            .find(|held| held.room().to_string() == workspace)
            .and_then(|held| held.owner().cloned())
    })
}

#[test]
fn a_person_who_joins_a_listed_workspace_arrives_in_it_with_its_people() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    let (_identity, person) = this_computer_with_an_identity(&mut home);
    let lounge = Lounge::start(person, "Coffee's on");
    let _config = community_site(home.env(), &lounge.entry);
    let state = tempfile::tempdir().expect("a directory for this computer's desk");
    let mut session = collaborative_session(state.path());
    session.step(Duration::from_millis(1)).unwrap();
    open_directory(&mut session);
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
    let joined = held(&session)
        .into_iter()
        .find(|(_, _, workspace)| workspace == &lounge.workspace)
        .expect("the workspace is on the desk");
    assert_eq!(joined.1, "Lounge", "under the name the site listed it by");
    // Whose the workspace is stays its host's governance's to say: joining it
    // never makes it this person's.
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
    assert!(
        !session
            .workspace_directory()
            .expect("the session hosts the workspace directory")
            .presentation()
            .is_open(),
        "and the sheet is done, leaving them in the workspace they joined"
    );
}

#[test]
fn a_pasted_invitation_link_joins_its_workspace_and_its_people() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    let (_identity, person) = this_computer_with_an_identity(&mut home);
    let lounge = Lounge::start(person, "Pulled up a chair");
    let _config = community_site(home.env(), &lounge.entry);
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
    let joined = held(&session)
        .into_iter()
        .find(|(_, _, workspace)| workspace == &lounge.workspace)
        .expect("the workspace is on the desk");
    assert_eq!(
        joined.1, "Lounge",
        "under the name the site's entry gives it"
    );
    assert!(
        !session
            .workspace_directory()
            .expect("the session hosts the workspace directory")
            .presentation()
            .is_open(),
        "and the sheet is done, leaving them in the workspace they joined"
    );
}

/// The lounge's workspace as its people left it: two strips, and three
/// windows written by another member. What a computer that joins sees.
fn the_lounges_desktop(person: &str) -> SharedDesktop {
    shared_desktop(
        person,
        &[
            (0x41, "Talk", &[(0x42, "Chat"), (0x43, "Notes")]),
            (0x44, "Work", &[(0x45, "Sheets")]),
        ],
    )
}

/// A computer joins the lounge and shows its workspace as the site holds it:
/// the strips and windows another member laid out, a window they open or move
/// afterwards, and their pointer drawn in the workspace where they are.
#[test]
fn a_computer_that_joins_the_lounge_shows_its_desktop_as_the_site_holds_it() {
    let _settled = ui::motion::testing::settled();
    tilo_secrets::keep_in_memory_for_process();
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    let (_identity, person) = this_computer_with_an_identity(&mut home);
    let lounge = Lounge::start(person, "Coffee's on");
    // The site's people left the workspace's desktop as two strips of
    // windows; another member wrote them before this computer joined.
    let member = hex::encode(lounge.page_device.user_public_key);
    let laid_out = the_lounges_desktop(&member);
    lounge.page_writes_desktop(&laid_out);
    lounge.wait_for_desktop(&laid_out);
    let _config = community_site(home.env(), &lounge.entry);
    let state = tempfile::tempdir().expect("a directory for this computer's desk");
    let mut session = collaborative_session(state.path());
    session.step(Duration::from_millis(1)).unwrap();
    join_the_lounge(&mut session, &lounge);
    // The window another member works in: the work strip's only window.
    let sheets = shared_desktop_tab(&laid_out, "Sheets");
    assert!(sheets != 0, "the site opened a window in the work strip");
    // The workspace in front is the lounge, and it shows exactly what the
    // site holds: the same strips, with the same windows in them.
    wait_until_desk(
        &mut session,
        &lounge.workspace,
        &recorded_layout(&laid_out),
        "the lounge's desktop arrives as the site holds it",
    );
    let active = session.handle().read(|tree| {
        tree.active_workspace()
            .map(|workspace| workspace.room().to_string())
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

    // Another member opens a window beside one of theirs, and moves one of
    // theirs: the desk here follows without anyone touching it.
    let mut reopened = laid_out.clone();
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
            }],
            active_tab: TabId::from(0x46_u64),
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        },
    );
    reopened.authors.insert(0x46, member.clone());
    reopened.authors.insert(0x47, member.clone());
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
    lounge.page_writes_desktop(&reopened);
    lounge.wait_for_desktop(&reopened);
    wait_until_desk(
        &mut session,
        &lounge.workspace,
        &recorded_layout(&reopened),
        "the window another member opened and the one they moved arrive here",
    );

    // Their pointer, over a window they work in: the desk draws it there,
    // wherever the person here is looking, without the roster being opened.
    // The window is on the strip in front, so its tile is where the pointer
    // lands.
    let front = session.handle().read(|tree| {
        let workspace = tree.active_workspace()?;
        let strip = tilo_desktop_surface::navigation::active_strip(tree, workspace)?;
        workspace
            .strips
            .iter()
            .find(|held| held.id == strip)?
            .window_groups
            .first()
            .map(|group| group.active_tab)
    });
    let shown = front.expect("the workspace in front shows a strip with a window");
    other_member_arrives(&lounge);
    other_member_points_at(&lounge, shown, 12.0, 34.0);
    wait_until(
        &mut session,
        "the other member's pointer is drawn here",
        |session| {
            session.drawn_peer_pointers().iter().any(|(_, (x, y))| {
                tile_here(session, shown).is_some_and(|tile| {
                    (x - (tile.x0 + 12.0)).abs() < 0.5 && (y - (tile.y0 + 34.0)).abs() < 0.5
                })
            })
        },
    );
    let drawn = session.drawn_peer_pointers();
    let tile = tile_here(&session, shown).expect("the window in front draws its tile");
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
    other_member_points_at(&lounge, shown, 40.0, 55.0);
    wait_until(
        &mut session,
        "the other member's pointer follows where they moved it",
        |session| {
            session
                .drawn_peer_pointers()
                .first()
                .is_some_and(|(_, (x, y))| {
                    tile_here(session, shown).is_some_and(|tile| {
                        (x - (tile.x0 + 40.0)).abs() < 0.5 && (y - (tile.y0 + 55.0)).abs() < 0.5
                    })
                })
        },
    );
    // The sheets window is the one the site opened first: it moved with the
    // rest, and is not this computer's to see twice.
    assert!(
        desk_layout(&session, &lounge.workspace)
            .iter()
            .flat_map(|(_, windows)| windows.clone())
            .any(|title| title == "Sheets"),
        "the window the site opened is still there: {:?}",
        desk_layout(&session, &lounge.workspace)
    );
}

/// Where the desk draws the tile of window `tab`, in the surface's
/// coordinates, as of the last layout: `None` while it is not shown.
fn tile_here(session: &Session, tab: u64) -> Option<ui::kurbo::Rect> {
    session
        .handle()
        .shown_tiles()
        .into_iter()
        .find(|(shown, _)| *shown == tab)
        .map(|(_, rect)| rect)
}

/// [`wait_until`], for a desk that must come to show what the site holds:
/// says what the desk shows instead when it never does.
#[track_caller]
fn wait_until_desk(
    session: &mut Session,
    workspace: &str,
    want: &[(String, Vec<String>)],
    what: &str,
) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        session.step(Duration::from_millis(10)).unwrap();
        let shown = desk_layout(session, workspace);
        if shown == want {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{what}: the desk shows {shown:?}, the site holds {want:?}"
        );
    }
}

/// The id of the window titled `title` on `desktop`.
fn shared_desktop_tab(desktop: &SharedDesktop, title: &str) -> u64 {
    desktop
        .tree
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .find(|tab| tab.title == title)
        .map(|tab| tab.id)
        .unwrap_or_else(|| panic!("the desktop holds a window titled {title}"))
}

/// Join the lounge's listed workspace and wait until its content and its
/// people reached this computer.
fn join_the_lounge(session: &mut Session, lounge: &Lounge) {
    open_directory(session);
    ask_to_join(session, &lounge.workspace);
    wait_until_the_lounge_is_here(session, lounge);
}

/// The lounge's other member says they are here, as every device does when it
/// joins: their presence in the workspace, under their own device.
fn other_member_arrives(lounge: &Lounge) {
    let page = lounge.page.as_ref().expect("the site's page runs");
    page.publish_presence(
        tilo_relay_protocol::room_workspace_id(&lounge.workspace).expect("a workspace id"),
        vec![
            postcard::to_stdvec(&neighbour_wire::desktop_presence(
                neighbour_wire::peer_handle(&lounge.page_device.device_public_key),
                &lounge.workspace,
                neighbour_wire::workspace_key(&lounge.workspace),
                collab_types::PeerPresencePayload::Arrived,
            ))
            .expect("a presence body"),
        ],
    )
    .expect("the other member says they are here");
}

/// The lounge's other member moves their pointer to `(x, y)` of the tile of
/// `tab` in the workspace.
fn other_member_points_at(lounge: &Lounge, tab: u64, x: f32, y: f32) {
    let page = lounge.page.as_ref().expect("the site's page runs");
    let key = postcard::to_stdvec(&collab_types::presence::PresenceKey::Pointer {
        space: collab_types::presence::PointerSpace::Tile {
            room: lounge.workspace.clone(),
            tab,
        },
    })
    .expect("a pointer key");
    let body = postcard::to_stdvec(&neighbour_wire::desktop_presence(
        neighbour_wire::peer_handle(&lounge.page_device.device_public_key),
        &lounge.workspace,
        key,
        collab_types::PeerPresencePayload::Pointer { x, y },
    ))
    .expect("a pointer body");
    page.publish_presence(
        tilo_relay_protocol::room_workspace_id(&lounge.workspace).expect("a workspace id"),
        vec![body],
    )
    .expect("the other member's pointer moves");
}

/// Wait until the lounge's workspace and its people reached this computer,
/// saying which of the two is still missing when they do not.
fn wait_until_the_lounge_is_here(session: &mut Session, lounge: &Lounge) {
    // The join is the site's door, the workspace's keys and its roster arriving
    // from other runtimes, so the bound is the suite's, not a second count.
    let deadline = tilo_test_env::hang_deadline();
    loop {
        session.step(Duration::from_millis(10)).unwrap();
        let held = session.held_workspaces();
        let people = session.people(&lounge.workspace);
        if held.iter().any(|workspace| workspace == &lounge.workspace)
            && people.as_ref().is_ok_and(|people| {
                people
                    .iter()
                    .any(|member| member.person == hex::encode(lounge.person))
            })
        {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the lounge's workspace is held here: held {held:?}, people {people:?}"
        );
    }
}

/// The line the lounge's chat holds here, as the person reads it: `None`
/// until this computer may open the record's sealed history.
fn chat_here(session: &Session, workspace: &str) -> Option<String> {
    // The workspace may hold nothing yet, and until this computer is admitted to
    // the record's channel its writes are not this computer's to open.
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
    let (_identity, person) = this_computer_with_an_identity(&mut home);
    let lounge = Lounge::start(person, "Coffee's on");
    let _config = community_site(home.env(), &lounge.entry);
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
    // The record the line came in is the workspace's, sealed as its host keeps it:
    // what the joined device read it through is the channel service, not a
    // plain copy of the workspace's document.
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
    let (_identity, person) = this_computer_with_an_identity(&mut home);
    let lounge = Lounge::start(person, "Pulled up a chair");
    let _config = community_site(home.env(), &lounge.entry);
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
    lounge
        .runtime
        .block_on(lounge.rooms.remove_member(
            &lounge.workspace,
            &lounge.host_cert,
            RemoveMemberRequest {
                request_id: [0x62; 32],
                user_public_key: person,
            },
        ))
        .expect("the lounge removes this person");
    // The workspace's governance is what every device's roster is read from, and
    // it lists this person no longer: the devices of theirs on the workspace's
    // roster are gone.
    let (_generation, members) = lounge
        .runtime
        .block_on(lounge.rooms.membership(&lounge.workspace))
        .expect("the lounge's members");
    let mark = members
        .get(&hex::encode(person))
        .expect("the workspace remembers the person it removed");
    assert!(mark.removed, "the workspace removed this person: {mark:?}");
    // The page, still a member, hears the workspace's governance and lists the
    // person no more, as every device's roster follows it.
    let page = lounge.page.as_ref().expect("the site's page runs");
    let deadline = tilo_test_env::hang_deadline();
    while page
        .status()
        .room_members
        .get(&lounge.workspace)
        .is_none_or(|members| members.contains(&hex::encode(person)))
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the lounge's remaining devices hear its governance: {:?}",
            page.status().room_members,
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // The lounge's page writes after the removal: the workspace's content from
    // now on is not this computer's, so the line never reads here.
    let route = DocumentRoute {
        app_id: CHAT.to_owned(),
        cluster_id: lounge.workspace.clone(),
        record: CHAT_RECORD.to_owned(),
    };
    let mut after = automerge::AutoCommit::load(
        &lounge
            .runtime
            .block_on(
                lounge
                    .rooms
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
    page.publish(
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
