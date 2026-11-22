//! A computer joins a real workspace service: the composition its deployment
//! runs, started from this tree, with nothing of its join path stood in for.
//!
//! The service is `tilo-server workspace --persistent` as the Lounge image starts
//! it: a governed workspace on a temporary state volume, a generated owner
//! root, the members' desktop shared, people taken by invitation. It names its
//! site in an `entry.json` written as the web entry image writes one — the home
//! workspace, its admission entry, and the site's directory — and every route
//! the session reads is the service's own: the entry, the directory's listing,
//! the entry's invitation and the host's answer to the enrollment.
//!
//! This binary is the join from the desktop session itself, which no page test
//! can reach: the session opens "Join a workspace", reads the site, joins
//! through its entry, and shows what the member laid out and where their
//! pointer is. A person nobody invited is refused in the entry's own words, and
//! the shell is unchanged. An app the person opens in the joined workspace is
//! launched there and shows its window.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tilo_session_client::{Cmd, Link, Outbox};

use collab_types::DocumentMutation;
use test_http::{Reply, Responder};
use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT, SharedDesktop};
use tilo_desktop_model::{
    GroupId, Strip, StripId, Tab, TabId, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceId,
};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server_client::ServerAddress;
use tilo_server_protocol::operator::{OperatorCommand, OperatorEvent};
use tilo_session::apps::Apps;
use tilo_session::{AppActions, Options, Session};
use tilo_session_client::{MemoryPeerStore, PeerConfig};
use tilo_sheets::directory::ListingAction;
use tilo_test_env::DiesWithTheTest as _;
use tilo_test_env::SpawnUnderEnvLock as _;
use tilo_user_identity::{DeviceCapabilities, DeviceCert};

use crate::common::directory::{
    community_site, held, layout, look_up_site, open_directory, pointer_here, recorded_layout,
    tile_here,
};
use crate::common::held::{certified, this_computer_with_an_identity};
use crate::common::publisher;

/// The origin the deployment's ingress publishes for the service: what its pages and its links
/// name. On this machine it is the address the service is reached at, because the service
/// publishes its live session at that origin's host and the port it bound, and the member's page
/// dials it there.
const ORIGIN: &str = "https://127.0.0.1";

/// A workspace service as its deployment runs it: `tilo-server workspace --persistent` with the
/// deployment's own routes beside it, spoken to over its stdin as the operator (whose input a
/// deployment leaves closed).
struct WorkspaceService {
    child: Child,
    stdin: Option<ChildStdin>,
    /// The plain listener the deployment's ingress forwards to.
    plain: String,
    /// The governed workspace's UUID.
    workspace: String,
    /// The owner root the service serves: its person.
    owner: ed25519_dalek::SigningKey,
    server_cert: DeviceCert,
    /// The service's iroh endpoint: how its pages reach its session.
    host: ServerAddress,
    /// The runtime the page and the service's routes run on: the host's own session and its HTTP
    /// clients belong to a runtime, and the test drives them from here.
    runtime: tokio::runtime::Runtime,
    _state: tempfile::TempDir,
}

impl Drop for WorkspaceService {
    fn drop(&mut self) {
        if let Some(mut stdin) = self.stdin.take() {
            let _ = stdin.write_all(OperatorCommand::Shutdown {}.to_line().as_bytes());
            let _ = stdin.flush();
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl WorkspaceService {
    /// The service as eu1 runs it: the Lounge image's own entrypoint, given the deployment's
    /// environment. The entrypoint maps it to the host's flags and execs `tilo-server` , so the test
    /// starts exactly what the image starts and cannot drift from it.
    fn start() -> Self {
        Self::with_admission("invite")
    }

    /// The service as tip's own Lounge runs it: the same entrypoint and the same environment, with
    /// `admission` open or by invitation. The Lounge's entry is open — its directory lists it and
    /// anyone who asks is admitted as its default role, an editor — so a person joins it with no
    /// invitation of their own.
    fn open() -> Self {
        Self::with_admission("open")
    }

    fn with_admission(admission: &str) -> Self {
        let state = tempfile::tempdir().expect("a state volume");
        let owner = ed25519_dalek::SigningKey::from_bytes(&random_key());
        let root_file = state.path().join("root");
        std::fs::write(&root_file, hex::encode(owner.to_bytes())).expect("the owner root");
        let entrypoint = entrypoint();
        let mut command = Command::new("sh");
        command
            .arg(&entrypoint)
            // `exec tilo-server` finds the binary the run built beside the test's own programs.
            .env("PATH", server_on_path())
            // eu1's environment, as apps/tilo-eu1/deployment.yaml sets it.
            .env("TILO_WORKSPACES_PERSISTENT", "lounge")
            .env("TILO_WORKSPACES_NAME", "Lounge")
            .env("TILO_WORKSPACES_ADMISSION", admission)
            // The origin the ingress publishes. The listeners below stand where it forwards to,
            // each on a port the kernel picks: the plain one's is announced once the service has bound
            // it, and the live session's is the service's own origin host and that port.
            .env("TILO_WORKSPACES_SERVER_ORIGIN", ORIGIN)
            .env("TILO_WORKSPACES_PUBLIC_API", ORIGIN)
            .env("TILO_WORKSPACES_WEB_ENTRY", "https://tilo.sh/")
            .env(
                "TILO_WORKSPACES_SITE_ORIGINS",
                "https://tilo.sh,https://tilo.so",
            )
            .env("TILO_WORKSPACES_ROOT_FILE", &root_file)
            .env("TILO_WORKSPACES_FEEDBACK_CHANNEL", "feedback")
            // The listeners, on loopback, each on a port the kernel picks as it binds: two workspaces on
            // one machine take the ports they are given, and the service announces the ones it bound.
            .env(
                "TILO_WORKSPACES_STATE",
                state.path().join("tilo-workspaces"),
            )
            .env("TILO_WORKSPACES_WEB", "127.0.0.1:0")
            .env("TILO_WORKSPACES_SERVER_PLAIN", "127.0.0.1:0")
            .env("TILO_WORKSPACES_LIVE", "127.0.0.1:0")
            .env("TILO_WORKSPACES_HTTP", "127.0.0.1:0")
            // The service's state volume, where the host keeps its secrets.
            .env(
                "TILO_SECRETS_DIR",
                state.path().join("tilo-workspaces/secrets"),
            )
            .env("TILO_SERVER_LOG", "warn")
            .env(
                "TILO_SOURCE_REVISION",
                "0123456789abcdef0123456789abcdef01234567",
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            // The room service a test hosts ends with the test, however the test ends.
            .dies_with_the_test();
        // eu1 keeps a wait list at its entry. The list is what an entry taking people by invitation
        // offers those it turns away: the image refuses to start an open entry with one.
        if admission == "invite" {
            command.env("TILO_WORKSPACES_WAIT_LIST_MAX", "16");
        }
        let mut child = command
            .spawn_locked()
            .expect("the workspace service starts");
        let stdin = Some(child.stdin.take().expect("the service's operator input"));
        let stdout = child.stdout.take().expect("the service's operator output");
        let (sender, events) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(event) = OperatorEvent::from_line(&line) {
                    let _ = sender.send(event);
                }
            }
        });
        let started = loop {
            let event = events
                .recv_timeout(Duration::from_secs(120))
                .expect("the workspace service says it started");
            if matches!(event, OperatorEvent::Started { .. }) {
                break event;
            }
        };
        let OperatorEvent::Started {
            workspace: Some(workspace),
            plain_origin: Some(plain),
            host_info: Some(info),
            ..
        } = started
        else {
            panic!(
                "a persistent service announces its workspace, its plain listener and who its host is: {started:?}"
            );
        };
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a runtime for the service's page");
        Self {
            child,
            stdin,
            plain,
            workspace,
            owner,
            server_cert: info.server_cert,
            host: ServerAddress::at(
                info.address
                    .expect("the service publishes its iroh endpoint")
                    .endpoint_addr,
            ),
            runtime,
            _state: state,
        }
    }

    /// The service's person: the root it serves, and its own device.
    fn owner_member(&self) -> Member {
        let key = ed25519_dalek::SigningKey::from_bytes(&[0x2f; 32]);
        let cert = certified(
            &self.owner,
            &key,
            "The Lounge's own",
            DeviceCapabilities::member(),
        );
        Member { key, cert }
    }
}

/// The Lounge image's own entrypoint in this checkout: found from the crate's own sources, which
/// the run built from, then from the test binary's place. A build whose `target/` sits outside the
/// checkout (a builder's container) has the sources only where the crate was compiled.
fn entrypoint() -> std::path::PathBuf {
    let built = std::env::current_exe().expect("the test binary's own path");
    let sources = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    sources
        .ancestors()
        .chain(built.ancestors())
        .map(|dir| dir.join("os/containers/tilo-lounge/entrypoint.sh"))
        .find(|entrypoint| entrypoint.is_file())
        .unwrap_or_else(|| {
            panic!(
                "no Lounge image entrypoint above {} or {}",
                sources.display(),
                built.display()
            )
        })
}

/// A PATH holding the server the run built, so the image's `exec tilo-server` runs it: the run's own
/// programs beside the test binary, then this machine's.
fn server_on_path() -> std::ffi::OsString {
    let server = tilo_test_env::sibling_binary("tilo-server", "tilo-server");
    let programs = server
        .parent()
        .expect("the server's directory")
        .to_path_buf();
    let mut paths = vec![programs];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::join_paths(paths).expect("a PATH")
}

/// A person in the service: their root, a device of theirs and its certificate.
struct Member {
    key: ed25519_dalek::SigningKey,
    cert: DeviceCert,
}

fn random_key() -> [u8; 32] {
    let mut key = [0; 32];
    getrandom::fill(&mut key).expect("a key");
    key
}

/// A page of `member` 's device on the service's iroh endpoint, in the workspace it serves: the same
/// session a member's browser runs.
fn page(service: &WorkspaceService, member: &Member) -> Link {
    let _entered = service.runtime.enter();
    let page = tilo_session_client::start_session(
        PeerConfig {
            host: service.host.clone(),
            cert: member.cert.clone(),
            server_cert: service.server_cert.clone(),
            revocations: tilo_user_identity::RevocationList::new(),
            entering: None,
        },
        Arc::new(member.key.clone()) as Arc<dyn tilo_server_client::DeviceSigner>,
        Arc::new(MemoryPeerStore::default()),
    );
    page.join_placed(&service.workspace, "Lounge")
        .expect("the member takes part in the service");
    page
}

/// The route of the service's shared desktop, as every member's page writes it.
fn desktop_route(service: &WorkspaceService) -> DocumentRoute {
    DocumentRoute {
        app_id: DESKTOP_APP.to_owned(),
        cluster_id: service.workspace.clone(),
        document: DESKTOP_DOCUMENT.to_owned(),
    }
}

/// Strips laid out by hand: each one's id, its name, and its windows' ids and titles.
type LaidOut<'a> = [(u64, &'a str, &'a [(u64, &'a str)]); 2];

/// The desktop the member laid out: two strips, three windows, all theirs.
fn the_members_desktop(member: &Member) -> SharedDesktop {
    let person = hex::encode(member.cert.user_public_key);
    let strips: LaidOut = [
        (0x41, "Talk", &[(0x42, "Chat"), (0x43, "Notes")]),
        (0x44, "Work", &[(0x45, "Sheets")]),
    ];
    let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Lounge");
    let mut authors = std::collections::BTreeMap::new();
    for (strip_id, name, windows) in strips {
        let mut strip = Strip::new(StripId::from(strip_id), name);
        authors.insert(strip_id, person.clone());
        for (tab, title) in windows {
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
            authors.insert(*tab, person.clone());
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
        authors,
        opened_by: std::collections::BTreeMap::new(),
    }
}

/// A desktop session of this computer, taking part in the peer network, keeping the
/// workspaces it holds beside `state` .
fn collaborative_session(state: &std::path::Path) -> Session {
    crate::common::headless(Options {
        collab: true,
        state: Some(state.to_path_buf()),
        ..Options::default()
    })
}

/// The site's `entry.json` , as the web entry image writes it from its `TILO_WEB_HOME_*` : the home
/// workspace, and the site's directory of public ones.
/// The deployment sets no home entry: the Lounge is joined through the entry its listing names, below
/// the directory's address (`directory/<workspace>/`).
fn entry_json(service: &WorkspaceService) -> String {
    serde_json::json!({
        "home": {
            "name": "Lounge",
            "host": { "origin": service.plain },
            "app": "so.tilo.chat",
        },
        "directory": format!("{}/directory", service.plain),
    })
    .to_string()
}

/// The site the person types into the sheet: the web entry's pages, whose `entry.json` names the
/// workspace service's own directory and entry.
fn site(service: &WorkspaceService) -> Responder {
    let entry = entry_json(service);
    Responder::handle(move |request: &test_http::Request| {
        Some(match (request.method.as_str(), request.target.as_str()) {
            ("GET", "/entry.json") => Reply::status(200)
                .header("content-type", "application/json")
                .body(entry.clone()),
            _ => Reply::status(404),
        })
    })
}

/// The row the sheet lists the workspace `key` under.
fn listing_row(session: &Session, key: &str) -> tilo_sheets::directory::Listing {
    session
        .workspace_directory()
        .expect("the session hosts the workspace directory")
        .directory()
        .listings
        .into_iter()
        .find(|listing| listing.key == key)
        .unwrap_or_else(|| {
            panic!(
                "the sheet lists {key}: {:?}",
                session
                    .workspace_directory()
                    .map(tilo_sheets::directory::DirectoryState::directory)
            )
        })
}

/// Press the sheet's button for joining the listing `key` .
fn ask_to_join(session: &mut Session, key: &str) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let sheet = session
            .workspace_directory()
            .expect("the session hosts the workspace directory")
            .directory();
        if sheet.listings.iter().any(|listing| listing.key == key) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the sheet lists the workspace to join: {sheet:?}"
        );
        session.step(Duration::from_millis(10)).unwrap();
    }
    let sheet = publisher(session, "join").expect("the directory sheet publishes joining");
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
    crate::common::until(session, what, |session| done(session));
}

/// Wait until the shell draws the member's pointer at `(x, y)` over the tile of `tab` , pointing
/// again as it waits.
/// Their pointer is a live frame the host fans out and never keeps: a host whose queue for this
/// device is full drops it, so the test says where they are more than once rather than sampling a
/// frame that may have been shed.
#[track_caller]
fn wait_until_their_pointer(
    session: &mut Session,
    member_page: &Link,
    service: &WorkspaceService,
    tab: u64,
    at: (f32, f32),
    what: &str,
) {
    let workspace_id =
        tilo_server_protocol::WorkspaceId::parse(&service.workspace).expect("a workspace id");
    let deadline = Instant::now()
        .checked_add(tilo_test_env::hang_bound())
        .expect("the wait's bound fits in an Instant");
    loop {
        member_page
            .send(Cmd::Presence(
                workspace_id,
                vec![collab_types::presence::Presence::pointing(
                    &service.workspace,
                    tab,
                    at.0,
                    at.1,
                )],
            ))
            .expect("the member says where they are");
        if crate::common::steps_within(session, Duration::from_millis(200), |session| {
            pointer_here(session, tab, at).then_some(())
        })
        .is_some()
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "gave up waiting for {what}; the pointers drawn: {:?}",
            session.drawn_peer_pointers()
        );
    }
}

/// The id of the window on the strip in front: whose tile the shell shows, so a pointer over it is
/// drawn.
fn window_in_front(session: &Session) -> Option<u64> {
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

/// The member's pointer is drawn on the shell where they put it, and follows when they move it.
#[track_caller]
fn the_pointer_follows(session: &mut Session, member_page: &Link, service: &WorkspaceService) {
    // Their pointer, over a window they work in: the shell draws it there,
    // without the roster being opened.
    let shown =
        window_in_front(session).expect("the workspace in front shows a strip with a window");
    wait_until_their_pointer(
        session,
        member_page,
        service,
        shown,
        (12.0, 34.0),
        "the member's pointer on the window in front",
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
    wait_until_their_pointer(
        session,
        member_page,
        service,
        shown,
        (40.0, 55.0),
        "the member's pointer follows where they moved it",
    );
}

/// The invitation the service issues for `person` , as its own API does: a page's `invite` over the
/// host's session, made by a member of the workspace.
fn invite(service: &WorkspaceService, member_page: &Link, person: [u8; 32]) -> String {
    let reply = member_page
        .ask_workspaces(
            tilo_server_protocol::workspaces::WorkspacesRequest::Invite {
                workspace: service.workspace.clone(),
                request_id: random_key(),
                role: tilo_server_protocol::workspaces::Role::Editor,
                expires_at_ns: tilo_clock::unix_now_ns().saturating_add(3_600_000_000_000),
                inviter_name: Some("The Lounge".to_owned()),
                wait_list_entry: None,
                // The invitation names the person it is for: a workspace taking people by
                // invitation answers its entry with it when they ask.
                recipient: Some(person),
                administrator: false,
            },
        )
        .expect("the member's page asks the host to invite");
    let reply = reply
        .recv_timeout(Duration::from_secs(60))
        .expect("the host answers the invite")
        .expect("the host takes the invitation");
    let tilo_server_protocol::workspaces::WorkspacesReply::Invitation {
        signed_invitation, ..
    } = reply
    else {
        panic!("the service issued no invitation: {reply:?}");
    };
    tilo_workspace_join::encode_invitation_text(&signed_invitation)
}

/// A computer joins a real workspace service through its own entry and shows the members' desktop, with
/// nothing of the join path stood in for.
/// The site's `entry.json` is written as the web entry image writes one; the directory listing, the
/// entry's invitation and the host's answer to the enrollment are the service's. The member laid the
/// workspace out through the same shared-desktop write a page makes, and the joining session shows
/// what they laid out and draws their pointer where they put it.
#[test]
fn a_computer_joins_the_lounge_through_its_workspace_services_own_entry() {
    let _settled = ui::motion::testing::settled();
    tilo_secrets::keep_in_memory_for_process();
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    let (_root, person) = this_computer_with_an_identity(&mut home);
    let service = WorkspaceService::start();
    let member = service.owner_member();

    // The member lays the workspace out through the same path a page uses: one write of the shared
    // desktop record, kept by the host before the test goes on.
    let member_page = page(&service, &member);
    let desktop = the_members_desktop(&member);
    {
        let mut doc = automerge::AutoCommit::new();
        tilo_desktop_model::shared_resource::write(&mut doc, &desktop)
            .expect("the member lays the service out");
        doc.commit_with(
            automerge::transaction::CommitOptions::default().with_message(
                tilo_desktop_model::shared_resource::change_message(Some(&hex::encode(
                    member.cert.user_public_key,
                ))),
            ),
        );
        member_page
            .write(
                desktop_route(&service),
                &DocumentMutation::Snapshot(doc.save()),
            )
            .expect("the member publishes the service's desktop");
    }
    member_page
        .barrier()
        .expect("the service confirms the desktop")
        .recv_timeout(Duration::from_secs(60))
        .expect("the service answers the barrier")
        .expect("the service keeps the desktop");

    // The site the person types: the web entry's `entry.json` naming the workspace service's own
    // directory and entry.
    let site = site(&service);
    let site_url = format!("{}/", site.url());
    let _config = community_site(home.env(), "");

    // The person opens "Join a workspace" before anyone invited them: the
    // Lounge is listed, and pressing Join is refused in the entry's own words,
    // leaving the shell as it was.
    let state = tempfile::tempdir().expect("a directory for this computer's shell");
    let mut session = collaborative_session(state.path());
    session.step(Duration::from_millis(1)).unwrap();
    open_directory(&mut session);
    look_up_site(&mut session, &site_url);
    assert_eq!(
        listing_row(&session, &service.workspace).action,
        ListingAction::Join,
        "the site lists the Lounge, and this computer does not hold it yet"
    );
    ask_to_join(&mut session, &service.workspace);
    let refused = crate::common::wait_maybe_past(
        &mut session,
        tilo_session::sync::JOIN_PATIENCE,
        |session| {
            (listing_row(session, &service.workspace).action == ListingAction::TryAgain)
                .then_some(())
        },
    );
    assert!(
        refused.is_some(),
        "the entry refused the uninvited person: {:?}",
        session
            .workspace_directory()
            .map(tilo_sheets::directory::DirectoryState::directory)
    );
    wait_until(
        &mut session,
        "the refusal is said where the person is",
        |session| {
            // A refusal is a banner that stays until the person acts on it, and is kept behind it
            // in the centre once it left: either is where the person reads it.
            let shell = session.notifications();
            let log = shell.log();
            log.active().iter().chain(log.history()).any(|notice| {
                notice.body.contains("Lounge") && notice.body.contains("joined by invitation")
            })
        },
    );
    assert!(
        !held(&session)
            .iter()
            .any(|(_, _, workspace)| workspace == &service.workspace),
        "and the shell is unchanged: no empty workspace to arrive in"
    );

    // The members invite them through the service's own API: a page's invite over the host's session,
    // naming the person the invitation is for.
    let invitation = invite(&service, &member_page, person);
    assert!(!invitation.is_empty());

    // They open "Join a workspace" again, as the person the banner told to ask for an invitation
    // does, and press Join: the entry hands them their invitation, the host admits their device, and
    // the workspace is held and shown here.
    open_directory(&mut session);
    look_up_site(&mut session, &site_url);
    ask_to_join(&mut session, &service.workspace);
    wait_until(&mut session, "the Lounge is held here", |session| {
        held(session)
            .iter()
            .any(|(_, _, workspace)| workspace == &service.workspace)
    });
    let shown = crate::common::wait_maybe_past(
        &mut session,
        tilo_session::sync::JOIN_PATIENCE,
        |session| (layout(session, &service.workspace) == recorded_layout(&desktop)).then_some(()),
    );
    assert!(
        shown.is_some(),
        "the shell shows what the member laid out: {:?}, the service holds {:?}; refusals: {:?}",
        layout(&session, &service.workspace),
        recorded_layout(&desktop),
        session.shell_refusals()
    );

    // The member's pointer is drawn on their window and follows a move.
    the_pointer_follows(&mut session, &member_page, &service);
}

/// An installed app in this computer's own store: a package whose entrypoint is the launcher's to
/// start, as `tilo-install` writes one.
fn installed(home: &mut tilo_test_env::TestHome, app: &str, name: &str) {
    let store = home.data().join("tilo-apps");
    let payload = store.join("payload").join(app);
    std::fs::create_dir_all(payload.join("bin")).expect("the app's payload");
    let manifest = payload.join("manifest.toml");
    std::fs::write(
        &manifest,
        format!(
            "[package]\n\
             id = \"{app}\"\n\
             version = \"1.0.0\"\n\
             kind = \"app\"\n\
             name = \"{name}\"\n\
             \n\
             [publisher]\n\
             identity = \"ed25519:{key}\"\n\
             signature = \"\"\n\
             \n\
             [sandbox]\n\
             namespaces = [\"user\"]\n\
             seccomp = \"tilo-default\"\n\
             landlock = \"strict\"\n\
             \n\
             [sandbox.net]\n\
             mode = \"off\"\n\
             \n\
             [entrypoint]\n\
             bin = \"bin/app\"\n\
             type = \"gui\"\n\
             \n\
             [providers]\n\
             schemes = [\"probe\"]\n",
            key = "33".repeat(32),
        ),
    )
    .expect("the app's manifest");
    std::fs::write(
        store.join("registry.toml"),
        format!(
            "[[entry]]\nid = \"{app}\"\nversion = \"1.0.0\"\n\
             payload_root = \"{payload}\"\nmanifest_path = \"{manifest}\"\n",
            payload = payload.display(),
            manifest = manifest.display(),
        ),
    )
    .expect("the store's registry");
    home.env().set("TILO_APPS_DATA_DIR", &store);
}

/// What the launcher the test starts was handed: its arguments, and the display it maps the app's
/// window on.
const LAUNCH_WINDOWS: &str = "TILO_LOUNGE_TEST_WINDOWS";

/// The child side, run in a copy of this test binary in the launcher's place: it records what it
/// was handed, maps one window of the app named there, as a package's window does, and stays up
/// until its session ends. Without [`LAUNCH_WINDOWS`] it is not the launcher and has nothing to do.
#[test]
fn the_launchers_child() {
    let Some(records) = std::env::var_os(LAUNCH_WINDOWS) else {
        return;
    };
    let arguments = std::env::var("TILO_LOUNGE_TEST_ARGS").unwrap_or_default();
    std::fs::write(&records, &arguments).expect("the launcher records its launch");
    let app = arguments
        .split_whitespace()
        .skip_while(|argument| *argument != "--app-id")
        .nth(1)
        .unwrap_or("so.tilo.probe")
        .to_owned();
    // The display the session handed the launch: `WAYLAND_DISPLAY` inside the `XDG_RUNTIME_DIR` the
    // session runs in, as an app's own window finds it.
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").expect("the session's runtime directory");
    let display = std::env::var_os("WAYLAND_DISPLAY").expect("the display the session handed");
    let socket = std::path::Path::new(&runtime).join(display);
    let title = app.clone();
    let _window = crate::common::window::Client::on(&socket, &app, &title);
}

/// The launcher a session starts packages through: this test binary, in the launcher's place, which
/// records what it was handed and maps the app's window on the display it was given.
fn the_launchers_place(
    home: &tilo_test_env::TestHome,
) -> (tilo_test_env::StandIn, std::path::PathBuf) {
    let records = home.home().join("launches");
    let launcher = tilo_test_env::stand_in(
        home.home().join("tilo-launcher"),
        &format!(
            "export {LAUNCH_WINDOWS}='{records}'\n\
             export TILO_LOUNGE_TEST_ARGS=\"$*\"\n\
             exec '{exe}' --exact lounge_e2e::the_launchers_child --nocapture --test-threads=1\n",
            records = records.display(),
            exe = std::env::current_exe()
                .expect("this test's program")
                .display(),
        ),
    )
    .expect("a launcher");
    (launcher, records)
}

/// A session that opens apps through `launcher`, in the workspaces it holds,
/// taking part in the peer network as [`collaborative_session`] does.
fn session_opening(
    state: &std::path::Path,
    home: &tilo_test_env::TestHome,
    launcher: &tilo_test_env::StandIn,
) -> (Session, Rc<Apps>) {
    let mut environment = tilo_app_launch::SessionEnvironment::from_process();
    environment.wayland_socket = Some(home.runtime().join("wayland-7"));
    let apps = Rc::new(Apps::with_launcher(
        environment,
        None,
        Some(launcher.path().to_owned()),
    ));
    let session = crate::common::headless(Options {
        collab: true,
        state: Some(state.to_path_buf()),
        apps: Some(Rc::clone(&apps) as Rc<dyn AppActions>),
        launch: Some(Rc::clone(&apps)),
        ..Options::default()
    });
    (session, apps)
}

/// An app opened in a workspace joined from a real workspace service shows its window there.
/// The service's entry admits this person by itself — the Lounge's open entry, a person joining it with
/// no invitation of their own — so the join is the one the live Lounge makes (`member=false`). The
/// app is opened the way the launchpad and the applications list open one: through the session's
/// own launch path, into the workspace in front.
#[test]
fn an_app_opened_in_a_joined_lounge_shows_its_window() {
    let _settled = ui::motion::testing::settled();
    tilo_secrets::keep_in_memory_for_process();
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    let _person = this_computer_with_an_identity(&mut home);
    let app = "so.tilo.probe";
    installed(&mut home, app, "Probe");
    let (launcher, records) = the_launchers_place(&home);

    let service = WorkspaceService::open();
    let site = site(&service);
    let site_url = format!("{}/", site.url());
    let _config = community_site(home.env(), "");
    let state = tempfile::tempdir().expect("a directory for this computer's shell");
    let (mut session, apps) = session_opening(state.path(), &home, &launcher);
    session.step(Duration::from_millis(1)).unwrap();

    // The person joins the Lounge through its open entry: no invitation, and the entry admits them as
    // an editor of a workspace that is not theirs.
    open_directory(&mut session);
    look_up_site(&mut session, &site_url);
    ask_to_join(&mut session, &service.workspace);
    wait_until(&mut session, "the Lounge is held here", |session| {
        held(session)
            .iter()
            .any(|(_, _, workspace)| workspace == &service.workspace)
    });
    wait_until(
        &mut session,
        "the Lounge is the workspace in front",
        |session| {
            session.handle().read(|tree| {
                tree.active_workspace()
                    .is_some_and(|workspace| workspace.id().to_string() == service.workspace)
            })
        },
    );

    // The person opens Probe there, as the applications list opens one.
    apps.open(&tilo_app_launch::Launch::Package {
        app: app.to_owned(),
        open: None,
    })
    .expect("Probe opens in the Lounge");
    let started =
        crate::common::wait_maybe_past(&mut session, tilo_session::sync::JOIN_PATIENCE, |_| {
            records.exists().then_some(())
        });
    assert!(
        started.is_some(),
        "the launcher the session starts packages through was asked to start {app}"
    );
    let launch = std::fs::read_to_string(&records).unwrap_or_default();
    assert!(
        launch.contains(&format!("--app-id {app}")),
        "the launch names the app the person opened: {launch}"
    );
    assert!(
        launch.contains(&format!("--workspace-id {}", service.workspace)),
        "the app is started in the workspace the person opened it in: {launch}"
    );
    // The app's window is a window of the Lounge: its ticket is the one the shell makes for the
    // workspace the person opened it in, not for their own.
    wait_until(
        &mut session,
        "the app shows its window in the Lounge",
        |session| {
            layout(session, &service.workspace)
                .iter()
                .any(|(_, windows)| windows.iter().any(|title| title == app || title == "Probe"))
        },
    );
}
