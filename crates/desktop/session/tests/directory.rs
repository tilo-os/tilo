//! "Join a workspace…" as a person meets it: opening the sheet reads the sites' directories and
//! lists the public workspaces they serve, who is in each one now and how many belong. The
//! workspaces this computer holds open instead of joining, and a site that cannot be read says so
//! without hiding them.
//! A workspace another person's host keeps is joined through the open entry its site publishes,
//! pressing Join on its listing or pasting a link to it, or with an invitation: its text, or the
//! link a site holds sealed for it, is pasted into the sheet and redeemed where it names.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion"
)]

#[path = "directory/sheet.rs"]
mod sheet;

use std::sync::{Arc, Mutex};
use std::time::Duration;
use tilo_session_client::{Cmd, Link, Outbox};

use automerge::ReadDoc as _;
use automerge::transaction::Transactable as _;
use collab_types::DocumentMutation;
use test_http::{Reply, Responder};
use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT, SharedDesktop};
use tilo_desktop_model::{
    GroupId, Strip, StripId, Tab, TabId, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceId, shared_resource,
};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server_client::ServerAddress;
use tilo_server_protocol::directory::{DirectoryEntry, DirectoryPage, Face, entry_path};
use tilo_server_protocol::workspaces::Role;
use tilo_session::{Options, Session};
use tilo_session_client::{MemoryPeerStore, PeerConfig};
use tilo_sheets::SheetState as _;
use tilo_workspace_service::workspaces::RemoveMemberRequest;

use crate::common::directory::{
    community_site, held, layout, open_directory, pointer_here, recorded_layout, tile_here,
};
use crate::common::held::{certified, this_computer_with_an_identity};
use crate::common::publisher;
use tilo_sheets::directory::{Listing, ListingAction, Status};

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

/// How long the test keeps stepping a removed computer's shell after the lounge writes past the
/// removal: an arrival the site fans out at once, so what has not arrived by then never would.
const AFTER_THE_REMOVAL: Duration = Duration::from_secs(10);

/// The screen the session draws its shell on.
const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;
const COMMUNITY_WORKSPACE: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
const CLUB_WORKSPACE: &str = "1b7c4e08-3a5d-42f6-8c9e-2d6f0a1b3c5e";

/// A session on the headless backend, taking no part in the peer network.
fn session() -> Session {
    crate::common::headless(Options {
        collab: false,
        ..Options::default()
    })
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

/// The join sheet as the person sees it, kept as `name.png` where the run asked for shots: a site's
/// listings arrive after the sheet opens, and the frame on screen may still be the one before them.
fn capture(session: &mut Session, name: &str) {
    for _ in 0..3 {
        session
            .step(std::time::Duration::from_millis(1))
            .expect("step");
    }
    crate::common::shot(session, name);
}

/// What the shell says to the person now, each notice's title and body, live or kept behind its
/// banner.
fn said(session: &Session) -> Vec<String> {
    let session = session.notifications();
    let log = session.log();
    log.active()
        .iter()
        .chain(log.history())
        .map(|notice| format!("{}: {}", notice.title, notice.body))
        .collect()
}

fn page_of(entries: Vec<DirectoryEntry>) -> String {
    serde_json::to_string(&DirectoryPage {
        host: None,
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
        default_role: Role::Editor,
        entry: entry_path(COMMUNITY_WORKSPACE),
        here: vec![face("Ada", "a1b2"), face("Bo", "c3d4")],
        here_count: 2,
        members,
        last_active_ms: Some(now_ms),
        community: true,
        invitation_only: false,
        wait_list: None,
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
        default_role: Role::Viewer,
        entry: entry_path(CLUB_WORKSPACE),
        here: Vec::new(),
        here_count: 0,
        members: 5,
        last_active_ms: Some(now_ms.saturating_sub(week)),
        community: false,
        invitation_only: false,
        wait_list: None,
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

/// A site that lists `entries` and publishes the host their entries are at, and answers nothing
/// else: an entry is entered at that host, never asked here.
fn joining_site(entries: Vec<DirectoryEntry>) -> Responder {
    let host = ed25519_dalek::SigningKey::from_bytes(&[0x51; 32]);
    let page = serde_json::to_string(&DirectoryPage {
        host: Some(tilo_server_protocol::web::PageHost {
            server_cert: certified(
                &host,
                &host,
                "The site",
                tilo_user_identity::DeviceCapabilities::member(),
            ),
            address: None,
            origin: None,
        }),
        workspaces: entries,
    })
    .expect("a directory page");
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
/// identity root of the test's own. Returns the root's directory, and the key an entry's invitation
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
    let paths = tilo_device_identity::IdentityPaths::under(root.path());
    std::fs::create_dir_all(paths.device_privkey.parent().expect("the key's directory"))
        .expect("this computer's key directory");
    tilo_device_identity::persist::write_device_key(&paths.device_privkey, device)
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
    tilo_device_identity::persist::write_device_cert(&paths.device_cert, &cert)
        .expect("this computer's certificate");
    env.set("TILO_IDENTITY_ROOT", root.path());
    // The name they go by, which the workspace learns of them: never their device's.
    tilo_ui_base::local_identity::install_source(Some(Arc::new(
        tilo_ui_base::local_identity::FixedIdentity(tilo_ui_base::local_identity::LocalIdentity {
            group_public_key: cert.user_public_key,
            display_name: "Ada".to_owned(),
            avatar_color: [40, 80, 120],
            device_public_key: None,
        }),
    )));
    root
}

/// Press the sheet's button for joining the listing `key` .
fn ask_to_join(session: &mut Session, key: &str) {
    // The row is drawn from what the sites list, and other runtimes answer that: a busy machine
    // takes as long at it as the suite allows. Press a row that is there, so a press that cannot be
    // carried out says why instead of waiting out the suite's bound for a join never asked for.
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let state = session
            .workspace_directory()
            .expect("the session hosts the workspace directory")
            .directory();
        if state.listings.iter().any(|listing| listing.key == key) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the sheet lists the workspace to join: {state:?}"
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
    crate::common::until(session, what, |session| done(session));
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
fn page_listing_the_workspace_on_this_shell(held_name: &str, held_workspace: &str) -> String {
    let mut entries = vec![community_entry(34, now_ms()), club_entry(now_ms())];
    entries.push(DirectoryEntry {
        workspace: held_workspace.to_owned(),
        name: held_name.to_owned(),
        description: "The one on this session".to_owned(),
        default_role: Role::Commenter,
        entry: entry_path(held_workspace),
        here: Vec::new(),
        here_count: 0,
        members: 3,
        last_active_ms: Some(now_ms()),
        community: false,
        invitation_only: false,
        wait_list: None,
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
    let _settled = tilo_ui_base::motion::testing::settled();
    let (responder, page) = site();
    let mut env = tilo_test_env::ScopedEnv::new();
    let _config = community_site(&mut env, &responder.url());
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    let (_held_id, held_name, held_workspace) = held(&session)
        .into_iter()
        .next()
        .expect("the session holds one workspace");
    *page.lock().expect("the page") =
        page_listing_the_workspace_on_this_shell(&held_name, &held_workspace);
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
    let _settled = tilo_ui_base::motion::testing::settled();
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
    let _settled = tilo_ui_base::motion::testing::settled();
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
fn a_join_with_an_expired_certificate_renews_it_before_presenting_it() {
    let _settled = tilo_ui_base::motion::testing::settled();
    tilo_secrets::keep_in_memory_for_process();
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    // This computer's certificate lasted a day, and it ran out twelve hours ago: ownership starts
    // holding one the host would refuse for having run out.
    let (person_hex, expired_paths) = expired_throwaway_identity(&mut home);
    let person = hex::decode(&person_hex)
        .ok()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        .expect("the person's root key");
    let expired = tilo_device_identity::persist::read_device_cert(&expired_paths.device_cert)
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
    let state = tempfile::tempdir().expect("a directory for this computer's session");
    // The renewal is this session's own: it resolves the identity from the environment the test
    // set, so a certificate issued to the device that joins is renewed, never a sibling test's.
    let mut session = collaborative_session(state.path());
    session.step(Duration::from_millis(1)).unwrap();
    open_directory(&mut session);
    paste_link(&mut session, &lounge.invitation_link());
    wait_until_the_lounge_is_here(&mut session, &lounge);
    // What the join presented was the renewal, not the certificate that had run out: ownership holds
    // a certificate issued since, and the workspace holds this device under it.
    let presented = tilo_device_identity::persist::read_device_cert(&expired_paths.device_cert)
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
) -> (String, tilo_device_identity::IdentityPaths) {
    use tilo_identity::provisioning::IdentityStorage;
    let system = home.root().join("system");
    let paths = tilo_device_identity::IdentityPaths::under(&system);
    // The account's own store below the identity directory, where every reader looks; its root is
    // sealed with the device key the environment's identity root holds.
    let identity_dir =
        tilo_identity::store::account_identity_dir(&home.config().join("tilo").join("identity"));
    home.env().set("TILO_IDENTITY_ROOT", &system);
    // This process stands in for the machine's daemon: it takes the device key first, so the
    // identity is sealed with it, and lets go of it once the identity is made, for the daemon
    // the test starts here takes it itself.
    crate::common::take_device_key(&tilo_identity::provisioning::collaboration_identity_paths());
    tilo_identity::provisioning::ensure_throwaway_test_identity(
        &identity_dir,
        &paths,
        IdentityStorage::Files,
    )
    .expect("this computer's throwaway identity");
    // The throwaway certificate lasts a day; issue it a day and a half ago,
    // so it has run out.
    let identity = tilo_identity::store::load_from_path(identity_dir.join("primary.json"))
        .expect("this computer's identity");
    let held = tilo_device_identity::persist::read_device_cert(&paths.device_cert)
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
    tilo_device_identity::persist::write_device_cert(&paths.device_cert, &aged)
        .expect("the aged certificate");
    tilo_device_identity::persist::write_device_cert(
        &paths.device_cert_for_user(&identity.group_public_key()),
        &aged,
    )
    .expect("the aged certificate where the identity keeps it");
    tilo_ui_base::local_identity::install_source(Some(Arc::new(
        tilo_ui_base::local_identity::FixedIdentity(tilo_ui_base::local_identity::LocalIdentity {
            group_public_key: aged.user_public_key,
            display_name: "Ada".to_owned(),
            avatar_color: [40, 80, 120],
            device_public_key: None,
        }),
    )));
    crate::common::let_go_of_device_keys();
    (hex::encode(aged.user_public_key), paths)
}

/// A session that takes part in the peer network, keeping the workspaces it holds beside
/// `state` .
fn collaborative_session(state: &std::path::Path) -> crate::common::held::Hosted {
    // This machine's loopback relay, as the lounge's host is: nothing here reaches the
    // person's relays out on the internet.
    let mut env = tilo_test_env::ScopedEnv::new();
    let (name, relay) = test_server::loopback_relay();
    env.set(name, relay);
    crate::common::held::hosted_in_env(
        env,
        Options {
            collab: true,
            state: Some(state.to_path_buf()),
            ..Options::default()
        },
        &[tilo_compositor::OutputConfig::new(
            "HEADLESS-1",
            WIDTH,
            HEIGHT,
        )],
    )
}

/// A chat record holding `line` : what one of the lounge's people wrote.
fn chat_with(line: &str) -> automerge::AutoCommit {
    use automerge::ROOT;
    let mut document = automerge::AutoCommit::new();
    document.put(ROOT, "text", line).expect("a chat line");
    document.commit();
    document
}

/// A site's workspace as a site runs it: a host on this machine serves the workspace, a page of
/// the site's person recorded there before anyone joined this computer, and the site's entry in
/// front of it.
/// Dropping it takes the page down before the runtime it runs on, and shuts the host down.
struct Lounge {
    page: Option<Link>,
    server: Option<tilo_runtime::device::keep::server::Server>,
    _entry: Responder,
    /// The host's web listener, which serves the entry's host (`/host`) a device asks before its
    /// session enters through the entry.
    _web: tilo_runtime::device::keep::web::WebHandle,
    runtime: tokio::runtime::Runtime,
    workspaces: tilo_workspace_service::workspaces::Workspaces,
    server_cert: tilo_user_identity::DeviceCert,
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
/// while over a server's start, a page's write and a governance change, and a wait with no bound at
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
        let Some(server) = self.server.take() else {
            return;
        };
        if std::thread::panicking() {
            // A test that already failed leaves with its report: the server's own shutdown is not
            // waited for on the way out of a panic, where a second panic would take the report
            // away.
            return;
        }
        block_on_within(
            &self.runtime,
            "the lounge's relay to shut down",
            server.shutdown(),
        );
    }
}

impl Lounge {
    /// The lounge's workspace, whose chat already holds `written`: its people invite those they
    /// want in, and this computer joins by an invitation one of them made.
    fn start(person: [u8; 32], written: &str) -> Self {
        // The lounge's host and page run in this test's process: what they did with a join is only
        // said in its log.
        crate::common::people::install_log();
        let state = tempfile::tempdir().expect("a directory for the lounge's host");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a runtime for the lounge's host");
        let host = LoungeHost::start(&runtime, state.path());
        let page = host.page(&runtime, written);
        let (site, web, entry) = host.serve_site(&runtime, state.path());
        Self {
            page: Some(page),
            server: Some(host.server),
            _entry: site,
            _web: web,
            runtime,
            workspaces: host.workspaces,
            server_cert: host.server_cert,
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
/// other's joins as a host that does not serve the room the join names.
fn lounge_key() -> [u8; 32] {
    let mut key = [0; 32];
    getrandom::fill(&mut key).expect("a key for the lounge");
    key
}

/// The host behind the lounge's site: a server of this machine, certified under a key root of the
/// test's own, which hosts the lounge's workspace and keeps what its page writes sealed.
struct LoungeHost {
    server: tilo_runtime::device::keep::server::Server,
    workspaces: tilo_workspace_service::workspaces::Workspaces,
    server_cert: tilo_user_identity::DeviceCert,
    /// The key and certificate of the site's page, the device that recorded in the workspace before
    /// anyone joined.
    page_key: ed25519_dalek::SigningKey,
    page_device: tilo_user_identity::DeviceCert,
    workspace: String,
}

impl LoungeHost {
    /// A host serving a workspace of its own, with the site's page entered in it.
    fn start(runtime: &tokio::runtime::Runtime, state: &std::path::Path) -> Self {
        let owner_key = ed25519_dalek::SigningKey::from_bytes(&lounge_key());
        let host_key = ed25519_dalek::SigningKey::from_bytes(&lounge_key());
        let server_cert = certified(
            &owner_key,
            &host_key,
            "The lounge's host",
            tilo_user_identity::DeviceCapabilities::member(),
        );
        let page_key = ed25519_dalek::SigningKey::from_bytes(&lounge_key());
        let page_device = certified(
            &owner_key,
            &page_key,
            "The lounge's page",
            tilo_user_identity::DeviceCapabilities::member(),
        );
        let (server, workspaces, workspace) =
            block_on_within(runtime, "the lounge's host to start", async {
                let server = tilo_runtime::device::keep::server::Server::start_with_relay_mode(
                    tilo_server_protocol::config::ServerConfig::loopback(state.join("host")),
                    tilo_runtime::device::keep::server::ServerIdentity {
                        user_public_key: owner_key.verifying_key().to_bytes(),
                        device_signing_key: host_key.clone(),
                        device_cert: server_cert.clone(),
                        revocations: tilo_user_identity::RevocationList::new(),
                    },
                    test_server::relay_mode(),
                )
                .await
                .expect("the lounge's host");
                let workspaces = server
                    .workspaces()
                    .expect("the host serves workspaces")
                    .clone();
                let workspace = workspaces
                    .create("Lounge")
                    .await
                    .expect("the lounge's workspace")
                    .workspace;
                // The site's page is in its workspace.
                workspaces
                    .enter(&workspace, (&page_device, &page_key))
                    .await
                    .expect("the site's page is in its workspace");
                (server, workspaces, workspace)
            });
        Self {
            server,
            workspaces,
            server_cert,
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
            document: CHAT_RECORD.to_owned(),
        };
        let page = {
            let _entered = runtime.enter();
            let page = tilo_session_client::start_session(
                PeerConfig {
                    host: ServerAddress::at(self.server.node_addr()),
                    cert: self.page_device.clone(),
                    server_cert: self.server_cert.clone(),
                    revocations: tilo_user_identity::RevocationList::new(),
                    entering: None,
                },
                Arc::new(self.page_key.clone()) as Arc<dyn tilo_server_client::DeviceSigner>,
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
                self.workspaces.document_routes(&self.workspace),
            )
            .expect("the lounge's records");
            held.iter()
                .any(|(app, record)| app == CHAT && record == CHAT_RECORD)
        });
        page
    }

    /// The site as the world meets it: its entry and the directory of the workspaces it serves,
    /// served by its web listener (kept under `state` ). Returns the site, the host's web
    /// listener, and the site's entry.
    fn serve_site(
        &self,
        runtime: &tokio::runtime::Runtime,
        state: &std::path::Path,
    ) -> (
        Responder,
        tilo_runtime::device::keep::web::WebHandle,
        String,
    ) {
        let web = block_on_within(runtime, "the lounge's host to serve its web", async {
            self.workspaces
                .publish(
                    &self.workspace,
                    &self.server_cert,
                    tilo_server_protocol::directory::Listing {
                        description: "Where the site's people talk".to_owned(),
                        default_role: Role::Editor,
                    },
                )
                .await
                .expect("the lounge is public");
            self.server
                .serve_web(tilo_runtime::device::keep::web::WebConfig {
                    listen: "127.0.0.1:0".parse().expect("an address"),
                    tls: tilo_runtime::device::keep::web::TlsSource::Development {
                        hosts: vec!["localhost".into(), "127.0.0.1".into()],
                        store: Some(state.join("web-dev-identity")),
                    },
                    allowed_origins: Vec::new(),
                    plain_listen: Some("127.0.0.1:0".parse().expect("an address")),
                    public_origin: None,
                    fetch_proxy: None,
                    site: None,
                })
                .await
                .expect("the lounge's host serves its web")
        });
        let host_entry = format!(
            "{}{}/{}",
            web.plain_origin
                .as_deref()
                .expect("the host's loopback listener"),
            tilo_server_protocol::web::API_PREFIX,
            entry_path(&self.workspace)
        );
        let origin = Arc::new(Mutex::new(String::new()));
        let served_origin = Arc::clone(&origin);
        let page_of_the_site = page_of(vec![DirectoryEntry {
            workspace: self.workspace.clone(),
            name: "Lounge".to_owned(),
            description: "Where the site's people talk".to_owned(),
            default_role: Role::Editor,
            entry: host_entry.clone(),
            here: Vec::new(),
            here_count: 0,
            members: 2,
            last_active_ms: Some(now_ms()),
            community: true,
            invitation_only: false,
            wait_list: None,
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
                            "admission": host_entry,
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

/// A strip the site holds as a test writes it: its id, its name, and the windows in it, each
/// window's id and title.
type HeldStrip<'a> = (u64, &'a str, &'a [(u64, &'a str)]);

/// A workspace's shared desktop, as the people in it left it: `strips` names each strip with the
/// windows opened in it, all of them `person` 's.
fn shared_layout(person: &str, strips: &[HeldStrip<'_>]) -> SharedDesktop {
    let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Lounge");
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
        tree: tilo_desktop_model::Layout {
            active: workspace.id(),
            workspaces: vec![workspace],
            ..tilo_desktop_model::Layout::default()
        },
        records: Vec::new(),
        items: Vec::new(),
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
            shared_resource::write(&mut doc, desktop).expect("the page lays its workspace out"),
            "the page's desktop is news to the record"
        );
        doc.commit_with(
            automerge::transaction::CommitOptions::default()
                .with_message(shared_resource::change_message(&person)),
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
                document: DESKTOP_DOCUMENT.to_owned(),
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
                    .resource_document(&self.workspace, DESKTOP_APP, DESKTOP_DOCUMENT),
            )
            .expect("the lounge's desktop record");
            let recorded = held
                .and_then(|bytes| automerge::AutoCommit::load(&bytes).ok())
                .and_then(|doc| shared_resource::read(&doc, None));
            recorded.as_ref().map(recorded_layout) == Some(recorded_layout(desktop))
        });
    }

    /// The invitation for this person as an invitation's text: the code a host hands a session to
    /// join with, not the link that carries it.
    fn invitation_text(&self) -> String {
        let issued = block_on_within(
            &self.runtime,
            "the lounge to issue this person an invitation",
            self.workspaces.issue(
                &self.workspace,
                &self.server_cert,
                tilo_workspace_join::invitations::IssueInvitationRequest {
                    request_id: [0x61; 32],
                    role: tilo_workspace_join::Role::Editor,
                    recipient_user_public_key: Some(self.person),
                    invitee_name: None,
                    expires_at_ns: tilo_clock::unix_now_ns().saturating_add(3_600_000_000_000),
                    inviter_name: Some("The lounge".to_owned()),
                },
            ),
        )
        .expect("an invitation to the lounge");
        tilo_workspace_join::encode_invitation_text(&issued.signed_invitation)
    }

    /// A link to the lounge's workspace carrying an invitation for this person, as whoever shares
    /// the workspace copies it.
    fn invitation_link(&self) -> String {
        format!(
            "{}#/w/{}?invite={}",
            self.entry,
            self.workspace,
            self.invitation_text()
        )
    }
}

#[test]
fn a_pasted_invitation_link_joins_its_workspace_and_its_people() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    let (_root, person) = this_computer_with_an_identity(&mut home);
    let lounge = Lounge::start(person, "Pulled up a chair");
    // The lounge is not this computer's community: the person types its address into the sheet, as
    // anyone joining a site they were given does.
    let _config = community_site(home.env(), "");
    let state = tempfile::tempdir().expect("a directory for this computer's session");
    let mut session = collaborative_session(state.path());
    session.step(Duration::from_millis(1)).unwrap();
    open_directory(&mut session);
    paste_link(&mut session, &lounge.invitation_link());
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
                        .any(|member| member.id.person_id() == Some(hex::encode(person).as_str()))
                })
        },
    );
    let people = session
        .people(&lounge.workspace)
        .expect("the linked workspace's people are known here");
    let member = people
        .iter()
        .find(|member| member.id.person_id() == Some(hex::encode(person).as_str()))
        .expect("the linked workspace is shared with this person");
    assert_eq!(
        member.role,
        Some(tilo_workspace_join::Role::Editor),
        "in the part the invitation gave them"
    );
    assert!(!member.removed, "and the workspace has not removed them");
    // The slot is made once the host admits the device, and the person lands in it: wait for the
    // shell to show it, as the person would see it.
    wait_until(
        &mut session,
        "the linked workspace is on the session",
        |session| {
            held(session)
                .iter()
                .any(|(_, _, workspace)| workspace == &lounge.workspace)
        },
    );
    let joined = held(&session)
        .into_iter()
        .find(|(_, _, workspace)| workspace == &lounge.workspace)
        .expect("the workspace is on the session");
    assert_eq!(
        joined.1, "Lounge",
        "under the name the site's entry gives it"
    );
    wait_until_the_directory_is_done(&mut session);
}

/// A session a host started with an invitation joins its workspace with nobody at the sheet, and
/// the notice that says so names the workspace by its own name: the one its governance carries,
/// which the top bar shows, never a stand-in for a name the join has not learned.
#[test]
fn a_session_started_with_an_invitation_names_the_workspace_it_joins_in_its_notice() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    let (_root, person) = this_computer_with_an_identity(&mut home);
    let lounge = Lounge::start(person, "Pulled up a chair");
    let _config = community_site(home.env(), "");
    // The invitation the host hands the session, as a provider's guest gets it: the workspace's
    // own name is in no invitation, only the workspace its governance names.
    let invitation = home.root().join("invitation.txt");
    std::fs::write(&invitation, lounge.invitation_text()).expect("the invitation is written");
    home.env()
        .set("TILO_SHELL_JOIN_INVITATION_FILE", &invitation);
    let state = tempfile::tempdir().expect("a directory for this computer's session");
    let mut session = collaborative_session(state.path());
    wait_until(
        &mut session,
        "the invited workspace is on the session",
        |session| {
            held(session)
                .iter()
                .any(|(_, _, workspace)| workspace == &lounge.workspace)
        },
    );
    let joined = held(&session)
        .into_iter()
        .find(|(_, _, workspace)| workspace == &lounge.workspace)
        .expect("the workspace is on the session");
    assert_eq!(
        joined.1, "Lounge",
        "the entry goes by the name the workspace's governance gives it"
    );
    wait_until(&mut session, "the join is said", |session| {
        said(session)
            .iter()
            .any(|notice| notice.starts_with("You joined "))
    });
    let notices = said(&session);
    assert!(
        notices
            .iter()
            .any(|notice| notice.starts_with("You joined Lounge:")),
        "the notice names the workspace it joined, as its people read it: {notices:?}"
    );
}

/// The lounge's workspace as its people left it: two strips, and three windows written by another
/// member. What a computer that joins sees.
fn the_lounges_desktop(person: &str) -> SharedDesktop {
    shared_layout(
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
    let _settled = tilo_ui_base::motion::testing::settled();
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
    let state = tempfile::tempdir().expect("a directory for this computer's session");
    let mut session = collaborative_session(state.path());
    session.step(Duration::from_millis(1)).unwrap();
    join_the_lounge(&mut session, &lounge);
    // The window another member works in: the work strip's only window.
    let sheets = layout_tab(&laid_out, "Sheets");
    assert!(sheets != 0, "the site opened a window in the work strip");
    // The workspace in front is the lounge, and it shows exactly what the site holds: the same
    // strips, with the same windows in them.
    wait_until_shell(
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
        layout(&session, &lounge.workspace),
        recorded_layout(&laid_out),
        "the same strips, with the same windows in them"
    );

    // Another member opens a window beside one of theirs, and moves one of theirs: the shell here
    // follows without anyone touching it.
    let reopened = another_member_edits(laid_out, &member);
    lounge.page_writes_desktop(&reopened);
    lounge.wait_for_desktop(&reopened);
    wait_until_shell(
        &mut session,
        &lounge.workspace,
        &recorded_layout(&reopened),
        "the window another member opened and the one they moved arrive here",
    );

    the_pointer_follows(&mut session, &lounge);

    // The sheets window is the one the site opened first: it moved with the rest, and is not this
    // computer's to see twice.
    assert!(
        layout(&session, &lounge.workspace)
            .iter()
            .flat_map(|(_, windows)| windows.clone())
            .any(|title| title == "Sheets"),
        "the window the site opened is still there: {:?}",
        layout(&session, &lounge.workspace)
    );
}

/// [`wait_until`], for a shell that must come to show what the site holds: says what the shell shows
/// instead when it never does.
#[track_caller]
fn wait_until_shell(
    session: &mut Session,
    workspace: &str,
    want: &[(String, Vec<String>)],
    what: &str,
) {
    let shown = crate::common::wait_maybe(session, |session| {
        (layout(session, workspace) == want).then_some(())
    });
    assert!(
        shown.is_some(),
        "{what}: the session shows {:?}, the site holds {want:?}; the shell's refusals: {:?}",
        layout(session, workspace),
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
        let strip = tree.active_strip_for(workspace)?;
        workspace
            .strips
            .iter()
            .find(|held| held.id == strip)?
            .window_groups
            .first()
            .map(|group| group.active_tab)
    })
}

/// The pointer of a person working in another member's window: the shell draws it there, wherever
/// the person here is looking, without the roster being opened, and follows where they move it.
fn the_pointer_follows(session: &mut Session, lounge: &Lounge) {
    // Their pointer, over a window they work in: the shell draws it there,
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
fn layout_tab(desktop: &SharedDesktop, title: &str) -> u64 {
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

/// Join the lounge's workspace with the invitation it sends, and wait until its content and its
/// people reached this computer.
fn join_the_lounge(session: &mut Session, lounge: &Lounge) {
    open_directory(session);
    paste_link(session, &lounge.invitation_link());
    wait_until_the_lounge_is_here(session, lounge);
}

/// Paste `link` into the sheet's link-or-code field.
fn paste_link(session: &mut Session, link: &str) {
    let sheet =
        publisher(session, "link-or-code").expect("the directory sheet takes a link or a code");
    let arguments = serde_json::json!({ "text": link })
        .as_object()
        .expect("an object")
        .clone();
    session
        .invoke(&sheet, "link-or-code", &arguments)
        .expect("the person pastes the link");
}

/// The lounge's other member says they are here, as every device does when it joins: their presence
/// in the workspace, under their own device.
fn other_member_arrives(lounge: &Lounge) {
    let page = lounge.page.as_ref().expect("the site's page runs");
    page.send(Cmd::Presence(
        tilo_server_protocol::WorkspaceId::parse(&lounge.workspace).expect("a workspace id"),
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
        tilo_server_protocol::WorkspaceId::parse(&lounge.workspace).expect("a workspace id"),
        vec![collab_types::presence::Presence::pointing(
            &lounge.workspace,
            tab,
            x,
            y,
        )],
    ))
    .expect("the other member's pointer moves");
}

/// How long the shell is stepped between one pointing and the next.
const POINT_AGAIN: Duration = Duration::from_millis(200);

/// Wait until the shell draws the other member's pointer at `(x, y)` over the tile of `tab` ,
/// pointing again as it waits.
/// Their pointer is a live frame the host fans out and never keeps: a host whose queue for this
/// device is full drops it (that is what ephemeral means), and a person's pointer keeps moving, so
/// the test says where they are more than once rather than sampling a frame that may have been
/// shed. What the wait is for is unchanged — the shell drawing their pointer where they put it — and
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
        let drawn = crate::common::steps_within(session, POINT_AGAIN, |session| {
            pointer_here(session, tab, at).then_some(())
        });
        if drawn.is_some() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "gave up waiting for {what} after {:?}; it looks hung (set TILO_TEST_HANG_BOUND_SECS to wait longer).\n\
             the shell's refusals: {:?}\nthe session says: {:?}\nthe pointers drawn: {:?}\n\
             the other member's channels and app_wires: {}",
            tilo_test_env::hang_bound(),
            session.shell_refusals(),
            session.accessible_labels(),
            session.drawn_peer_pointers(),
            lounge
                .page
                .as_ref()
                .map(|page| page.status().key_state)
                .unwrap_or_default()
        );
    }
}

/// Wait until the lounge's workspace and its people reached this computer,
/// saying which of the two is still missing when they do not.
fn wait_until_the_lounge_is_here(session: &mut Session, lounge: &Lounge) {
    // The join is the site's entry, the workspace's keys and its roster arriving from other
    // runtimes, and the session gives a host its own patience to answer them: the wait spans that
    // patience and the suite's hang bound after it, so a host answering slowly is waited for rather
    // than called hung, and the session's note on a join that did not happen is what the test
    // reports when the wait runs out.
    let here =
        crate::common::wait_maybe_past(session, tilo_session::sync::JOIN_PATIENCE, |session| {
            let held = session.held_workspaces();
            let people = session.people(&lounge.workspace);
            (held.iter().any(|workspace| workspace == &lounge.workspace)
                && people.as_ref().is_ok_and(|people| {
                    people.iter().any(|member| {
                        member.id.person_id() == Some(hex::encode(lounge.person).as_str())
                    })
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
            .map(tilo_sheets::directory::DirectoryState::directory)
    );
}

/// Wait until the workspace's roster, as this computer reads it, admits `devices` devices: the
/// roster is the workspace's governance arriving here, which is not ordered against its records —
/// a line of the chat reading is no sign that the devices the workspace admits have all reached
/// this computer.
fn wait_until_the_roster_holds(
    session: &mut Session,
    workspace: &str,
    devices: usize,
    what: &str,
) -> Vec<String> {
    let mut roster = Vec::new();
    crate::common::until(session, what, |session| {
        roster = session
            .roster_devices(workspace)
            .expect("the workspace's roster is known here")
            .iter()
            .map(publisher_key)
            .collect();
        roster.len() == devices
    });
    roster
}

/// The lounge's page writes after the person is removed: the workspace's content from then on is not
/// this computer's, so the line never reads here. The removal reached this computer the way it
/// reaches every device — the roster read here is the workspace's governance, and holds no device of
/// the person's any more — so what the window rules out is the record arriving, not the removal
/// still being on its way.
fn the_lounge_writes_past_the_removal(session: &mut Session, lounge: &Lounge) {
    let page = lounge.page.as_ref().expect("the site's page runs");
    let route = DocumentRoute {
        app_id: CHAT.to_owned(),
        cluster_id: lounge.workspace.clone(),
        document: CHAT_RECORD.to_owned(),
    };
    let mut after = automerge::AutoCommit::load(
        &block_on_within(
            &lounge.runtime,
            "the lounge's chat",
            lounge
                .workspaces
                .resource_document(&lounge.workspace, CHAT, CHAT_RECORD),
        )
        .expect("the lounge's chat")
        .expect("the lounge holds its chat"),
    )
    .expect("the chat's document");
    let before = after.get_heads();
    after
        .put(automerge::ROOT, "text", "The shop's shut")
        .expect("a chat line after the removal");
    after.commit();
    page.write(
        route,
        &DocumentMutation::Incremental(after.save_after(&before)),
    )
    .expect("the site's page goes on talking without this computer");
    // The record no longer opening here is the removal itself; only another line is the write.
    let arrived = crate::common::steps_within(session, AFTER_THE_REMOVAL, |session| {
        chat_here(session, &lounge.workspace)
            .is_some_and(|line| line != "Pulled up a chair")
            .then_some(())
    });
    assert!(
        arrived.is_none(),
        "what the lounge wrote after the removal reads here: {:?}",
        chat_here(session, &lounge.workspace)
    );
}

/// The line the lounge's chat holds here, as the person reads it: `None` until this computer may
/// open the record's sealed history.
fn chat_here(session: &Session, workspace: &str) -> Option<String> {
    // The workspace may hold nothing yet, and until this computer is admitted to the record's
    // channel its writes are not this computer's to open.
    let Ok(Some(record)) = session.read_resource(workspace, CHAT, CHAT_RECORD) else {
        return None;
    };
    let chat = automerge::AutoCommit::load(&record).ok()?;
    let (text, _) = chat.get(automerge::ROOT, "text").ok()??;
    text.into_string().ok()
}

/// The publisher the lounge's host is known by.
fn host_publisher(lounge: &Lounge) -> String {
    publisher_of(&lounge.server_cert)
}

/// The publisher the lounge's page is known by.
fn page_publisher(lounge: &Lounge) -> String {
    publisher_of(&lounge.page_device)
}

/// The publisher a device's session is known by.
fn publisher_of(cert: &tilo_user_identity::DeviceCert) -> String {
    publisher_key(&cert.device_public_key)
}

/// The publisher a device key is known by.
fn publisher_key(device: &[u8; 32]) -> String {
    iroh::EndpointId::from_bytes(device)
        .expect("a device key is a publisher")
        .to_string()
}

#[test]
fn a_workspace_joined_brings_the_chat_written_before_it() {
    let _settled = tilo_ui_base::motion::testing::settled();
    // This computer keeps its own channel state in the platform secret store,
    // which the test keeps in memory.
    tilo_secrets::keep_in_memory_for_process();
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    let (_root, person) = this_computer_with_an_identity(&mut home);
    let lounge = Lounge::start(person, "Coffee's on");
    // The lounge is not this computer's community: the person types its address into the sheet, as
    // anyone joining a site they were given does.
    let _config = community_site(home.env(), "");
    let state = tempfile::tempdir().expect("a directory for this computer's session");
    let mut session = collaborative_session(state.path());
    session.step(Duration::from_millis(1)).unwrap();
    join_the_lounge(&mut session, &lounge);
    // The record is sealed in the workspace's channel, and this computer reads it once the channel
    // service's ask for its keys is answered: the wait spans the service's own longest wait between
    // asks, so an ask a busy machine answered late is waited for rather than called a record that
    // never came.
    let read = crate::common::wait_maybe_past(
        &mut session,
        tilo_workspace_keys::asking_again_wait(),
        |session| {
            (chat_here(session, &lounge.workspace).as_deref() == Some("Coffee's on")).then_some(())
        },
    );
    assert!(
        read.is_some(),
        "the lounge's chat, written before this computer joined, reads here: it reads {:?}",
        chat_here(&session, &lounge.workspace)
    );
    let chat = chat_here(&session, &lounge.workspace).expect("the lounge's chat opened here");
    assert_eq!(
        chat, "Coffee's on",
        "the line the lounge wrote before this computer joined reads as it was written"
    );
    // The record the line came in is the workspace's, sealed as its host keeps it: what the joined
    // device read it through is the channel service, not a plain copy of the workspace's document.
    let record = session
        .read_resource(&lounge.workspace, CHAT, CHAT_RECORD)
        .expect("the chat is this computer's to read")
        .expect("the chat is here");
    assert!(
        automerge::AutoCommit::load(&record).is_ok(),
        "and it is the chat app's own document"
    );
    // The workspace's window streams carry between its members' devices: the page that recorded
    // there before this computer arrived, and this computer itself. Its host serves it and is no
    // member's device. The roster is the workspace's governance, which reaches this computer on
    // its own, so it is waited for rather than read the moment the chat is here.
    let roster = wait_until_the_roster_holds(
        &mut session,
        &lounge.workspace,
        2,
        "the workspace's devices are on its roster",
    );
    assert!(
        roster.contains(&page_publisher(&lounge)) && !roster.contains(&host_publisher(&lounge)),
        "the lounge's page is among them, and its host is not: {roster:?}"
    );
}

#[test]
fn a_member_the_lounge_removes_leaves_its_roster_and_reads_nothing_more() {
    let _settled = tilo_ui_base::motion::testing::settled();
    tilo_secrets::keep_in_memory_for_process();
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    let (_root, person) = this_computer_with_an_identity(&mut home);
    let lounge = Lounge::start(person, "Pulled up a chair");
    // The lounge is not this computer's community: the person types its address into the sheet, as
    // anyone joining a site they were given does.
    let _config = community_site(home.env(), "");
    let state = tempfile::tempdir().expect("a directory for this computer's session");
    let mut session = collaborative_session(state.path());
    session.step(Duration::from_millis(1)).unwrap();
    join_the_lounge(&mut session, &lounge);
    // As above: the chat is sealed in the workspace's channel, and its keys arrive when the channel
    // service's next ask is answered.
    let read = crate::common::wait_maybe_past(
        &mut session,
        tilo_workspace_keys::asking_again_wait(),
        |session| {
            chat_here(session, &lounge.workspace)
                .is_some()
                .then_some(())
        },
    );
    assert!(
        read.is_some(),
        "the lounge's chat reads here while this computer is a member"
    );
    let lounge_devices = [host_publisher(&lounge), page_publisher(&lounge)];
    let roster = wait_until_the_roster_holds(
        &mut session,
        &lounge.workspace,
        2,
        "the lounge's page and this computer are on the workspace's roster",
    );
    assert!(
        roster.iter().any(|device| !lounge_devices.contains(device)),
        "this computer is on the roster beside the lounge's page: {roster:?}"
    );

    // The lounge's owner removes this person, and every device of theirs.
    block_on_within(
        &lounge.runtime,
        "the lounge to remove this person",
        lounge.workspaces.remove_member(
            &lounge.workspace,
            &lounge.server_cert,
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
                .workspace_members
                .get(&lounge.workspace)
                .is_some_and(|members| !members.contains(&removed))
        },
    );
    the_lounge_writes_past_the_removal(&mut session, &lounge);
}
