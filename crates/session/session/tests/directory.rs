//! "Join a workspace…" as a person on a computer set up for a community
//! meets it: opening the sheet reads that site's directory and lists the
//! public workspaces it serves, who is in each one now and how many belong.
//! The workspaces this computer holds open instead of joining, and a site
//! that cannot be read says so without hiding them.
//!
//! Joining one asks the door its listing names, as the person at this
//! computer and by their own name, and the invitation the door answers with
//! is what puts the workspace on the desk: one it holds from then on, open
//! rather than to join. A door that refuses says why, where the person
//! pressed, and leaves the workspace one to try again.

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

/// Point this computer at the community site `site` for the test's
/// lifetime. Returns the configuration's directory, which goes with it.
fn community_site(env: &mut tilo_test_env::ScopedEnv, site: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a directory for the configuration");
    let config = dir.path().join("community.toml");
    std::fs::write(&config, format!("site = \"{site}\"\n")).expect("the configuration is written");
    env.set("TILO_COMMUNITY_CONFIG", &config);
    dir
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

/// A site that lists `entries` and answers the door of the workspace in
/// `room` with `door`, a function of the join request it was asked.
fn joining_site(
    room: &str,
    entries: Vec<DirectoryEntry>,
    door: impl Fn(&test_http::Request) -> Reply + Send + Sync + 'static,
) -> Responder {
    let page = page_of(entries);
    let door_path = format!("/directory/{room}/join");
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
    // The name they go by, which the room learns of them: never their
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

/// A door's answer letting `person` into `room`: an invitation bound to
/// them, signed by a device of the workspace's host.
fn invitation_for(room: &str, person: [u8; 32]) -> String {
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
        workspace_id: tilo_workspace_join::room_workspace_id(room).expect("a room's id"),
        room: room.to_owned(),
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
    let deadline = std::time::Instant::now()
        .checked_add(Duration::from_secs(20))
        .expect("a deadline");
    loop {
        session.step(Duration::from_millis(10)).unwrap();
        if done(session) {
            return;
        }
        assert!(std::time::Instant::now() < deadline, "{what}");
    }
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
        CLUB_ROOM,
        vec![community_entry(34, now_ms()), club_entry(now_ms())],
        move |_| {
            Reply::status(200)
                .header("content-type", "application/json")
                .body(invitation_for(CLUB_ROOM, person))
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
    ask_to_join(&mut session, CLUB_ROOM);
    wait_until(&mut session, "the club was joined", |session| {
        held(session).iter().any(|(_, _, room)| room == CLUB_ROOM)
    });
    let joined = held(&session)
        .into_iter()
        .find(|(_, _, room)| room == CLUB_ROOM)
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
        format!("/directory/{CLUB_ROOM}/join"),
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
    let responder = joining_site(CLUB_ROOM, vec![club_entry(now_ms())], |_| {
        Reply::status(403)
            .header("content-type", "application/json")
            .body(r#"{"error":"this workspace is for members only"}"#)
    });
    let _config = community_site(&mut env, &responder.url());
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    open_directory(&mut session);
    ask_to_join(&mut session, CLUB_ROOM);
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
        !held(&session).iter().any(|(_, _, room)| room == CLUB_ROOM),
        "and no workspace was added"
    );
}
