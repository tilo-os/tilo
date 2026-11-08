//! A computer joins a real workspace service: the composition its deployment
//! runs, started from this tree, with nothing of its join path stood in for.
//!
//! The service is `tilo-relay room --persistent` as the Lounge image starts
//! it: a governed workspace on a temporary state volume, a generated owner
//! root, the members' desktop shared, people taken by invitation. It names its
//! site in an `entry.json` written as the web entry image writes one — the home
//! workspace, its admission door, and the site's directory — and every route
//! the session reads is the service's own: the entry, the directory's listing,
//! the door's invitation and the host's answer to the enrollment.
//!
//! What a person's *page* does in the Lounge is the publish gate's
//! (`os/containers/tilo-lounge/integration/people.mjs`, two real browsers);
//! this binary is the same join from the desktop session itself, which no page
//! test can reach: the session opens "Join a workspace", reads the site, joins
//! through its door, and shows what the member laid out and where their
//! pointer is. A person nobody invited is refused in the door's own words, and
//! the desk is unchanged. An app the person opens in the joined workspace is
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

mod common;

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use collab_types::DocumentMutation;
use test_http::{Reply, Responder};
use tilo_browser_peer::{Host, MemoryPeerStore, PeerConfig, PeerHandle};
use tilo_compositor::OutputConfig;
use tilo_desktop::desk_verbs::CHROME_RESOURCE;
use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_RECORD, SharedDesktop};
use tilo_desktop_model::{
    GroupId, Strip, StripId, Tab, TabId, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceSlot,
};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_relay_protocol::operator::{OperatorCommand, OperatorEvent};
use tilo_session::mount::apps::Apps;
use tilo_session::{AppActions, Options, Session};
use tilo_sheets::directory::{ListingAction, Status};
use tilo_test_env::SpawnUnderEnvLock as _;
use tilo_user_identity::{DeviceCapabilities, DeviceCert};

tilo_automerge_isolate::test_worker!();

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;

/// The origin the deployment's ingress publishes for the room: what its pages and its links name.
/// On this machine it is the address the room is reached at, because the room publishes its live
/// plane at that origin's host and the port it bound, and the member's page dials it there.
const ORIGIN: &str = "https://127.0.0.1";

/// A room service as its deployment runs it: `tilo-relay room --persistent` with the deployment's
/// own routes beside it, spoken to over its stdin as the operator (whose input a deployment leaves
/// closed).
struct Room {
    child: Child,
    stdin: Option<ChildStdin>,
    /// The plain listener the deployment's ingress forwards to.
    plain: String,
    /// The governed workspace's UUID.
    workspace: String,
    /// The owner root the room serves: its person.
    owner: ed25519_dalek::SigningKey,
    host_cert: DeviceCert,
    /// The runtime the page and the room's routes run on: the host's own session and its HTTP
    /// clients belong to a runtime, and the test drives them from here.
    runtime: tokio::runtime::Runtime,
    _state: tempfile::TempDir,
}

impl Drop for Room {
    fn drop(&mut self) {
        if let Some(mut stdin) = self.stdin.take() {
            let _ = stdin.write_all(OperatorCommand::Shutdown {}.to_line().as_bytes());
            let _ = stdin.flush();
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Room {
    /// The room as eu1 runs it: the Lounge image's own entrypoint, given the deployment's
    /// environment. The entrypoint maps it to the host's flags and execs `tilo-relay` , so the test
    /// starts exactly what the image starts and cannot drift from it.
    fn start() -> Self {
        Self::with_admission("invite")
    }

    /// The room as tip's own Lounge runs it: the same entrypoint and the same environment, with
    /// `admission` open or by invitation. The Lounge's door is open — its directory lists it and
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
            // `exec tilo-relay` finds the binary the run built beside the test's own programs.
            .env("PATH", relay_on_path())
            // eu1's environment, as apps/tilo-eu1/deployment.yaml sets it.
            .env("TILO_ROOMS_PERSISTENT", "lounge")
            .env("TILO_ROOMS_NAME", "Lounge")
            .env("TILO_ROOMS_ADMISSION", admission)
            // The origin the ingress publishes. The listeners below stand where it forwards to,
            // each on a port the kernel picks: the plain one's is announced once the room has bound
            // it, and the live plane's is the room's own origin host and that port.
            .env("TILO_ROOMS_RELAY_ORIGIN", ORIGIN)
            .env("TILO_ROOMS_PUBLIC_API", ORIGIN)
            .env("TILO_ROOMS_WEB_ENTRY", "https://tilo.coffee/")
            .env(
                "TILO_ROOMS_SITE_ORIGINS",
                "https://tilo.coffee,https://tilo.so",
            )
            .env("TILO_ROOMS_ROOT_FILE", &root_file)
            .env("TILO_ROOMS_FEEDBACK_CHANNEL", "feedback")
            .env("TILO_ROOMS_WAIT_LIST_MAX", "16")
            // The listeners, on loopback, each on a port the kernel picks as it binds: two rooms on
            // one machine take the ports they are given, and the room announces the ones it bound.
            .env("TILO_ROOMS_STATE", state.path().join("tilo-rooms"))
            .env("TILO_ROOMS_WEB", "127.0.0.1:0")
            .env("TILO_ROOMS_RELAY_PLAIN", "127.0.0.1:0")
            .env("TILO_ROOMS_LIVE", "127.0.0.1:0")
            .env("TILO_ROOMS_HTTP", "127.0.0.1:0")
            // The room's state volume, where the host keeps its secrets.
            .env("TILO_SECRETS_DIR", state.path().join("tilo-rooms/secrets"))
            .env("TILO_RELAY_LOG", "warn")
            .env(
                "TILO_SOURCE_REVISION",
                "0123456789abcdef0123456789abcdef01234567",
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = command.spawn_locked().expect("the room service starts");
        let stdin = Some(child.stdin.take().expect("the room's operator input"));
        let stdout = child.stdout.take().expect("the room's operator output");
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
                .expect("the room service says it started");
            if matches!(event, OperatorEvent::Started { .. }) {
                break event;
            }
        };
        let OperatorEvent::Started {
            workspace: Some(workspace),
            plain_origin: Some(plain),
            ..
        } = started
        else {
            panic!("a persistent room announces its workspace and its plain listener: {started:?}");
        };
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a runtime for the room's page");
        let host_cert = host_info(&runtime, &plain).expect("the host's certificate");
        Self {
            child,
            stdin,
            plain,
            workspace,
            owner,
            host_cert,
            runtime,
            _state: state,
        }
    }

    /// The room's person: the root it serves, and its own device.
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

/// The Lounge image's own entrypoint in this checkout, found from the test binary's place: the
/// source tree is the one the run built from, wherever that build ran.
fn entrypoint() -> std::path::PathBuf {
    let built = std::env::current_exe().expect("the test binary's own path");
    // The checkout is the nearest ancestor of the binary holding the image's entrypoint: the binary
    // sits under `target/` , however deep the profile and the builder's artifact directory put it.
    built
        .ancestors()
        .map(|dir| dir.join("os/containers/tilo-lounge/entrypoint.sh"))
        .find(|entrypoint| entrypoint.is_file())
        .unwrap_or_else(|| {
            panic!(
                "no Lounge image entrypoint in any directory above {}",
                built.display()
            )
        })
}

/// A PATH holding the relay the run built, so the image's `exec tilo-relay` runs it: the run's own
/// programs beside the test binary, then this machine's.
fn relay_on_path() -> std::ffi::OsString {
    let relay = tilo_test_env::sibling_binary("tilo-relay", "tilo-relay");
    let programs = relay.parent().expect("the relay's directory").to_path_buf();
    let mut paths = vec![programs];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::join_paths(paths).expect("a PATH")
}

/// A person in the room: their root, a device of theirs and its certificate.
struct Member {
    key: ed25519_dalek::SigningKey,
    cert: DeviceCert,
}

/// `key` 's certificate as a device of the person whose root key is `root` .
fn certified(
    root: &ed25519_dalek::SigningKey,
    key: &ed25519_dalek::SigningKey,
    label: &str,
    capabilities: DeviceCapabilities,
) -> DeviceCert {
    let mut cert = DeviceCert {
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

fn random_key() -> [u8; 32] {
    let mut key = [0; 32];
    getrandom::fill(&mut key).expect("a key");
    key
}

/// Plain HTTP/1.1 to the room's plain listener: a page's `fetch` .
struct Plain(String);

impl tilo_browser_peer::HttpClient for Plain {
    fn post(
        &self,
        path: &str,
        session: Option<&str>,
        body: Vec<u8>,
    ) -> tilo_browser_peer::HttpFuture<'_> {
        let url = format!("{}{path}", self.0);
        let session = session.map(str::to_owned);
        Box::pin(async move {
            let client = client()?;
            let mut request = client
                .post(&url)
                .header("content-type", "application/octet-stream")
                .body(body);
            if let Some(session) = session {
                request = request.header(tilo_relay_protocol::web::SESSION_HEADER, session);
            }
            let response = request.send().await.map_err(|error| error.to_string())?;
            let status = response.status().as_u16();
            let body = response
                .bytes()
                .await
                .map_err(|error| error.to_string())?
                .to_vec();
            Ok((status, body))
        })
    }
}

/// The client the site and the room's own routes are read over: the desktop's connection pool.
fn client() -> Result<tilo_net::Client, String> {
    tilo_net::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| error.to_string())
}

/// The host's certificate, as a page certified by it pinned.
fn host_info(runtime: &tokio::runtime::Runtime, plain: &str) -> Result<DeviceCert, String> {
    runtime
        .block_on(tilo_browser_peer::host_info(&Plain(plain.to_owned())))
        .map(|info| info.host_cert)
        .map_err(|error| error.to_string())
}

/// A page of `member` 's device on the room's plain listener, in the room's workspace: the same
/// session a member's browser runs.
fn page(room: &Room, member: &Member) -> PeerHandle {
    let _entered = room.runtime.enter();
    let page = PeerHandle::start(
        PeerConfig {
            host: Host::Https(Arc::new(Plain(room.plain.clone()))),
            cert: member.cert.clone(),
            host_cert: room.host_cert.clone(),
            revocations: tilo_user_identity::RevocationList::new(),
            background: false,
        },
        Arc::new(member.key.clone()) as Arc<dyn tilo_browser_peer::DeviceSigner>,
        Arc::new(MemoryPeerStore::default()),
    );
    page.join_placed(&room.workspace, "Lounge")
        .expect("the member takes part in the room");
    page
}

/// The route of the room's shared desktop, as every member's page writes it.
fn desktop_route(room: &Room) -> DocumentRoute {
    DocumentRoute {
        app_id: DESKTOP_APP.to_owned(),
        cluster_id: room.workspace.clone(),
        record: DESKTOP_RECORD.to_owned(),
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
    let mut workspace = Workspace::new(WorkspaceSlot(1), "Lounge");
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
        tree: tilo_desktop_model::DesktopTree {
            active: workspace.slot,
            workspaces: vec![workspace],
            ..tilo_desktop_model::DesktopTree::default()
        },
        records: Vec::new(),
        authors,
    }
}

/// The strips and windows a shared desktop holds, as a desk shows them, in order: each strip by its
/// name and the titles of the windows in it.
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

/// The strips and windows a desk shows in `workspace` , as [`recorded_layout`] gives a shared
/// desktop's: what every device showing it should see.
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

/// A desktop session of this computer, taking part in the collaboration plane, keeping the
/// workspaces it holds beside `state` .
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

/// This computer's person: an identity of its own in `home` 's configuration and a device certified
/// for it, kept in `home` too so it lives as long as the test.
fn this_computer_with_an_identity(
    home: &mut tilo_test_env::TestHome,
) -> (std::path::PathBuf, [u8; 32]) {
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
    ui::local_identity::install_source(Some(Arc::new(ui::local_identity::FixedIdentity(
        ui::local_identity::LocalIdentity {
            group_public_key: person,
            display_name: "Ada".to_owned(),
            avatar_color: [40, 80, 120],
        },
    ))));
    (root, person)
}

/// The site's `entry.json` , as the web entry image writes it from its `TILO_WEB_HOME_*` : the home
/// workspace, and the site's directory of public ones.
/// The deployment sets no home door: the Lounge is joined through the door its listing names, below
/// the directory's address (`directory/<workspace>/`).
fn entry_json(room: &Room) -> String {
    serde_json::json!({
        "home": {
            "name": "Lounge",
            "host": { "origin": room.plain },
            "app": "so.tilo.chat",
        },
        "directory": format!("{}/directory", room.plain),
    })
    .to_string()
}

/// The site the person types into the sheet: the web entry's pages, whose `entry.json` names the
/// room service's own directory and door.
fn site(room: &Room) -> Responder {
    let entry = entry_json(room);
    Responder::handle(move |request: &test_http::Request| {
        Some(match (request.method.as_str(), request.target.as_str()) {
            ("GET", "/entry.json") => Reply::status(200)
                .header("content-type", "application/json")
                .body(entry.clone()),
            _ => Reply::status(404),
        })
    })
}

/// Point this computer at the community site `site` for the test's lifetime,
/// as a site named in `community.toml` is. Returns the configuration's
/// directory, which goes with it.
fn community_site(env: &mut tilo_test_env::ScopedEnv, site: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a directory for the configuration");
    let config = dir.path().join("community.toml");
    std::fs::write(&config, format!("site = \"{site}\"\n")).expect("the configuration is written");
    env.set("TILO_COMMUNITY_CONFIG", &config);
    dir
}

/// The key the session's directory sheet publishes `action` under.
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
                    .map(tilo_sheets::directory::DirectorySeat::directory)
            )
        })
}

/// Press the sheet's button for joining the listing `key` .
fn ask_to_join(session: &mut Session, key: &str) {
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
    common::until(session, what, |session| done(session));
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

/// Where the desk draws the tile of window `tab` : `None` while it is not shown.
fn tile_here(session: &Session, tab: u64) -> Option<ui::kurbo::Rect> {
    session
        .handle()
        .shown_tiles()
        .into_iter()
        .find(|(shown, _)| *shown == tab)
        .map(|(_, rect)| rect)
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

/// Wait until the desk draws the member's pointer at `(x, y)` over the tile of `tab` , pointing
/// again as it waits.
/// Their pointer is a live frame the host fans out and never keeps: a host whose queue for this
/// device is full drops it, so the test says where they are more than once rather than sampling a
/// frame that may have been shed.
#[track_caller]
fn wait_until_their_pointer(
    session: &mut Session,
    member_page: &PeerHandle,
    room: &Room,
    tab: u64,
    at: (f32, f32),
    what: &str,
) {
    let workspace_id =
        tilo_relay_protocol::room_workspace_id(&room.workspace).expect("a workspace id");
    let deadline = Instant::now()
        .checked_add(tilo_test_env::hang_bound())
        .expect("the wait's bound fits in an Instant");
    loop {
        member_page
            .publish_presence(
                workspace_id,
                vec![collab_types::presence::Presence::pointing(
                    &room.workspace,
                    tab,
                    at.0,
                    at.1,
                )],
            )
            .expect("the member says where they are");
        if common::steps_within(session, Duration::from_millis(200), |session| {
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

/// The id of the window on the strip in front: whose tile the desk shows, so a pointer over it is
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

/// The member's pointer is drawn on the desk where they put it, and follows when they move it.
#[track_caller]
fn the_pointer_follows(session: &mut Session, member_page: &PeerHandle, room: &Room) {
    // Their pointer, over a window they work in: the desk draws it there,
    // without the roster being opened.
    let shown =
        window_in_front(session).expect("the workspace in front shows a strip with a window");
    wait_until_their_pointer(
        session,
        member_page,
        room,
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
        room,
        shown,
        (40.0, 55.0),
        "the member's pointer follows where they moved it",
    );
}

/// The invitation the room issues for `person` , as its own API does: a page's `invite` over the
/// host's session, made by a member of the workspace.
fn invite(room: &Room, member_page: &PeerHandle, person: [u8; 32]) -> String {
    let (answer, reply) = std::sync::mpsc::sync_channel(1);
    member_page
        .rooms_then(
            tilo_relay_protocol::workspaces::WorkspacesRequest::Invite {
                workspace: room.workspace.clone(),
                request_id: random_key(),
                role: tilo_relay_protocol::workspaces::WorkspaceRole::Editor,
                expires_at_ns: tilo_clock::unix_now_ns().saturating_add(3_600_000_000_000),
                inviter_name: Some("The Lounge".to_owned()),
                wait_list_entry: None,
                // The invitation names the person it is for: a workspace taking people by
                // invitation answers its door with it when they ask.
                recipient: Some(person),
            },
            Box::new(move |reply| {
                let _ = answer.send(reply);
            }),
        )
        .expect("the member's page asks the host to invite");
    let reply = reply
        .recv_timeout(Duration::from_secs(60))
        .expect("the host answers the invite")
        .expect("the host takes the invitation");
    let tilo_relay_protocol::workspaces::WorkspacesReply::Invitation {
        signed_invitation, ..
    } = reply
    else {
        panic!("the room issued no invitation: {reply:?}");
    };
    tilo_workspace_join::encode_invitation_text(&signed_invitation)
}

/// A computer joins a real room service through its own door and shows the members' desktop, with
/// nothing of the join path stood in for.
/// The site's `entry.json` is written as the web entry image writes one; the directory listing, the
/// door's invitation and the host's answer to the enrollment are the service's. The member laid the
/// workspace out through the same shared-desktop write a page makes, and the joining session shows
/// what they laid out and draws their pointer where they put it.
#[test]
fn a_computer_joins_the_lounge_through_its_room_services_own_door() {
    let _settled = ui::motion::testing::settled();
    tilo_secrets::keep_in_memory_for_process();
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    let (_root, person) = this_computer_with_an_identity(&mut home);
    let room = Room::start();
    let member = room.owner_member();

    // The member lays the workspace out through the same path a page uses: one write of the shared
    // desktop record, kept by the host before the test goes on.
    let member_page = page(&room, &member);
    let desktop = the_members_desktop(&member);
    {
        let mut doc = automerge::AutoCommit::new();
        tilo_desktop_model::shared_record::write(&mut doc, &desktop)
            .expect("the member lays the room out");
        doc.commit_with(
            automerge::transaction::CommitOptions::default().with_message(
                tilo_desktop_model::shared_record::change_message(Some(&hex::encode(
                    member.cert.user_public_key,
                ))),
            ),
        );
        member_page
            .publish(
                desktop_route(&room),
                &DocumentMutation::Snapshot(doc.save()),
            )
            .expect("the member publishes the room's desktop");
    }
    member_page
        .barrier()
        .expect("the room confirms the desktop")
        .recv_timeout(Duration::from_secs(60))
        .expect("the room answers the barrier")
        .expect("the room keeps the desktop");

    // The site the person types: the web entry's `entry.json` naming the room service's own
    // directory and door.
    let site = site(&room);
    let site_url = format!("{}/", site.url());
    let _config = community_site(home.env(), "");

    // The person opens "Join a workspace" before anyone invited them: the
    // Lounge is listed, and pressing Join is refused in the door's own words,
    // leaving the desk as it was.
    let state = tempfile::tempdir().expect("a directory for this computer's desk");
    let mut session = collaborative_session(state.path());
    session.step(Duration::from_millis(1)).unwrap();
    open_directory(&mut session);
    look_up_site(&mut session, &site_url);
    assert_eq!(
        listing_row(&session, &room.workspace).action,
        ListingAction::Join,
        "the site lists the Lounge, and this computer does not hold it yet"
    );
    ask_to_join(&mut session, &room.workspace);
    let refused =
        common::wait_maybe_past(&mut session, tilo_session::sync::JOIN_PATIENCE, |session| {
            (listing_row(session, &room.workspace).action == ListingAction::TryAgain).then_some(())
        });
    assert!(
        refused.is_some(),
        "the door refused the uninvited person: {:?}",
        session
            .workspace_directory()
            .map(tilo_sheets::directory::DirectorySeat::directory)
    );
    wait_until(
        &mut session,
        "the refusal is said where the person is",
        |session| {
            session
                .notifications()
                .log()
                .history()
                .iter()
                .any(|notice| {
                    notice.body.contains("Lounge") && notice.body.contains("joined by invitation")
                })
        },
    );
    assert!(
        !held(&session)
            .iter()
            .any(|(_, _, workspace)| workspace == &room.workspace),
        "and the desk is unchanged: no empty workspace to arrive in"
    );

    // The members invite them through the room's own API: a page's invite over the host's session,
    // naming the person the invitation is for.
    let invitation = invite(&room, &member_page, person);
    assert!(!invitation.is_empty());

    // They open "Join a workspace" again, as the person the banner told to ask for an invitation
    // does, and press Join: the door hands them their invitation, the host admits their device, and
    // the workspace is held and shown here.
    open_directory(&mut session);
    look_up_site(&mut session, &site_url);
    ask_to_join(&mut session, &room.workspace);
    wait_until(&mut session, "the Lounge is held here", |session| {
        held(session)
            .iter()
            .any(|(_, _, workspace)| workspace == &room.workspace)
    });
    let shown =
        common::wait_maybe_past(&mut session, tilo_session::sync::JOIN_PATIENCE, |session| {
            (desk_layout(session, &room.workspace) == recorded_layout(&desktop)).then_some(())
        });
    assert!(
        shown.is_some(),
        "the desk shows what the member laid out: {:?}, the room holds {:?}; refusals: {:?}",
        desk_layout(&session, &room.workspace),
        recorded_layout(&desktop),
        session.shell_refusals()
    );

    // The member's pointer is drawn on their window and follows a move.
    the_pointer_follows(&mut session, &member_page, &room);
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
    let _window = common::window::Client::on(&socket, &app, &title);
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
             exec '{exe}' --exact the_launchers_child --nocapture --test-threads=1\n",
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
/// taking part in the collaboration plane as [`collaborative_session`] does.
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
    let session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: true,
            state: Some(state.to_path_buf()),
            apps: Some(Rc::clone(&apps) as Rc<dyn AppActions>),
            launch: Some(Rc::clone(&apps)),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    (session, apps)
}

/// An app opened in a workspace joined from a real room service shows its window there.
/// The room's door admits this person by itself — the Lounge's open door, a person joining it with
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

    let room = Room::open();
    let site = site(&room);
    let site_url = format!("{}/", site.url());
    let _config = community_site(home.env(), "");
    let state = tempfile::tempdir().expect("a directory for this computer's desk");
    let (mut session, apps) = session_opening(state.path(), &home, &launcher);
    session.step(Duration::from_millis(1)).unwrap();

    // The person joins the Lounge through its open door: no invitation, and the door admits them as
    // an editor of a workspace that is not theirs.
    open_directory(&mut session);
    look_up_site(&mut session, &site_url);
    ask_to_join(&mut session, &room.workspace);
    wait_until(&mut session, "the Lounge is held here", |session| {
        held(session)
            .iter()
            .any(|(_, _, workspace)| workspace == &room.workspace)
    });
    wait_until(
        &mut session,
        "the Lounge is the workspace in front",
        |session| {
            session.handle().read(|tree| {
                tree.active_workspace()
                    .is_some_and(|workspace| workspace.id().to_string() == room.workspace)
            })
        },
    );

    // The person opens Probe there, as the applications list opens one.
    apps.open(&tilo_app_launch::Launch::Package {
        app: app.to_owned(),
        open: None,
    })
    .expect("Probe opens in the Lounge");
    let started = common::wait_maybe_past(&mut session, tilo_session::sync::JOIN_PATIENCE, |_| {
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
        launch.contains(&format!("--workspace-id {}", room.workspace)),
        "the app is started in the workspace the person opened it in: {launch}"
    );
    // The app's window is a window of the Lounge: its ticket is the one the desk makes for the
    // workspace the person opened it in, not for their own.
    wait_until(
        &mut session,
        "the app shows its window in the Lounge",
        |session| {
            desk_layout(session, &room.workspace)
                .iter()
                .any(|(_, windows)| windows.iter().any(|title| title == app || title == "Probe"))
        },
    );
}
