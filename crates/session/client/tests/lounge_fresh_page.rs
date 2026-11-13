//! A fresh page joining a real Lounge, as the deployment rehearsal: the
//! image's own entrypoint with eu1's environment, `tilo-relay room
//! --persistent` on a copy of the deployment's state, watched by pages that
//! were never here before.
//!
//! This is the deployment's own composition, not a fixture: the host is
//! `os/containers/tilo-lounge/entrypoint.sh` given eu1's `TILO_ROOMS_*`
//! (`apps/tilo-eu1/deployment.yaml`), the state volume is the operator's
//! copy of production, and the pages go through the host's real door
//! (`/host`, and the session the door serves) with invitations the
//! host itself mints. What it asks about are the records people use — Chat's
//! and the shared desktop's — and what it checks is what a person sees: the
//! records' keys arrive, Chat's history opens, Chat's composer is there, the
//! desk shows the members' windows, and a second page sees the first page's
//! pointer.
//!
//! It runs only when the operator names the state volume, so an ordinary
//! test run skips it:
//!
//! ```text
//! TILO_LOUNGE_STATE=/path/to/copy TILO_LOUNGE_REFOUND=so.tilo.browser-desktop:desktop \
//!   cargo test -p tilo-session-client -p workspace-unify --test lounge_fresh_page -- --nocapture
//! ```
//!
//! `TILO_LOUNGE_REFOUND` is passed to the host as `TILO_ROOMS_REFOUND`, the
//! way the deployment will set it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "a rehearsal fixture: a failed step is the answer, and it says which check failed"
)]

use std::io::{BufRead, BufReader, Write as _};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use tilo_session_client::{Cmd, Event, Link, Outbox};

use std::time::Duration;
use tilo_relay_client::https::HttpFuture;

use collab_types::presence::Presence;
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_relay_client::Host;
use tilo_relay_client::invite::guest_certificate;
use tilo_relay_protocol::operator::{OperatorCommand, OperatorEvent};
use tilo_relay_protocol::workspaces::{Role, WorkspacesReply, WorkspacesRequest};
use tilo_test_env::SpawnUnderEnvLock as _;
use tilo_user_identity::{DeviceCapabilities, DeviceCert};

/// The shared desktop every member sees: the record the deployment refounds.
const DESKTOP_APP: &str = "so.tilo.browser-desktop";
const DESKTOP_RECORD: &str = "desktop";
/// Chat's app.
const CHAT_APP: &str = "so.tilo.chat";

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

/// A PATH holding the relay this run built, so the image's `exec tilo-relay`
/// runs it.
fn relay_on_path() -> std::ffi::OsString {
    let relay = tilo_test_env::sibling_binary("tilo-relay", "tilo-relay");
    let programs = relay.parent().expect("the relay's directory").to_path_buf();
    let mut paths = vec![programs];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::join_paths(paths).expect("a PATH")
}

/// The Lounge, as eu1 runs it.
struct Lounge {
    child: Child,
    stdin: Option<ChildStdin>,
    /// The workspace's UUID.
    workspace: String,
    host_cert: DeviceCert,
    /// Where the host's iroh endpoint answers: what a page's session dials,
    /// which is what `/host` publishes.
    address: tilo_relay_protocol::web::HostAddress,
    /// The snapshot's owner root: the workspace's owner, from the volume.
    owner_root: SigningKey,
    _state: tempfile::TempDir,
}

impl Drop for Lounge {
    fn drop(&mut self) {
        if let Some(mut stdin) = self.stdin.take() {
            let _ = stdin.write_all(OperatorCommand::Shutdown {}.to_line().as_bytes());
            let _ = stdin.flush();
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Lounge {
    /// The state volume: the operator's copy, with the deployment's secret in
    /// it where the entrypoint reads it. The operator's copy is a directory
    /// holding what the pod mounts: `state/` (the volume) and `secrets/` (the
    /// mounted secret files). A plain copy of the volume alone is taken as it
    /// is.
    fn volume(from: &std::path::Path) -> (tempfile::TempDir, std::path::PathBuf) {
        let snapshot = if from.join("state").is_dir() {
            from.join("state")
        } else {
            from.to_path_buf()
        };
        let state = tempfile::tempdir().expect("a state volume for the run");
        let copied = Command::new("cp")
            .arg("-a")
            .arg(format!("{}/.", snapshot.display()))
            .arg(state.path())
            .status_locked()
            .expect("cp runs");
        assert!(copied.success(), "the snapshot copies into the volume");
        assert!(
            state.path().join("lounge/room-id").is_file(),
            "the volume holds the workspace's id: {}",
            state.path().join("lounge").display()
        );
        let mounted = [
            from.join("secrets/room-root-lounge"),
            from.join("secrets/room-root"),
            snapshot.join("secrets/room-root-lounge"),
            snapshot.join("secrets/room-root"),
        ]
        .into_iter()
        .find(|path| path.is_file())
        .unwrap_or_else(|| {
            panic!(
                "the snapshot carries the owner root under {}/secrets",
                from.display()
            )
        });
        let secrets = state.path().join("secrets");
        std::fs::create_dir_all(&secrets).expect("the secrets directory");
        let root_file = secrets.join("room-root-lounge");
        std::fs::copy(&mounted, &root_file).expect("the owner root");
        (state, root_file)
    }

    /// The Lounge on a fresh state volume: nothing of the deployment's state,
    /// a generated owner root, and a workspace the host makes as it starts.
    /// This is the shape a first deployment starts in, and it needs no copy of
    /// production to prove what two pages do there.
    fn fresh() -> Self {
        let state = tempfile::tempdir().expect("a state volume for the run");
        let secrets = state.path().join("secrets");
        std::fs::create_dir_all(&secrets).expect("the secrets directory");
        let root_file = secrets.join("room-root-lounge");
        let owner_root = SigningKey::generate(&mut UnwrapErr(SysRng));
        std::fs::write(&root_file, hex::encode(owner_root.to_bytes())).expect("the owner root");
        Self::start_at(state, &root_file, Some(owner_root), None)
    }

    fn start(from: &std::path::Path, refound: Option<&str>) -> Self {
        let (state, root_file) = Self::volume(from);
        Self::start_at(state, &root_file, None, refound)
    }

    fn start_at(
        state: tempfile::TempDir,
        root_file: &std::path::Path,
        owner_root: Option<SigningKey>,
        refound: Option<&str>,
    ) -> Self {
        let owner_root = owner_root.unwrap_or_else(|| read_root(root_file));
        let entrypoint = entrypoint();
        let mut command = Command::new("sh");
        command
            .arg(&entrypoint)
            // Pages reach the host over WebTransport at an origin naming its
            // UDP port, as the deployment's own name does. The host takes the
            // port the kernel picks and publishes it at `/host`, so nothing
            // here holds a port no one is bound to.
            .env("TILO_ROOMS_LINK", "127.0.0.1:0")
            .env("TILO_ROOMS_LINK_ORIGIN", "https://127.0.0.1:0")
            .env("PATH", relay_on_path())
            // eu1's environment, as apps/tilo-eu1/deployment.yaml sets it,
            // without the Discord, mail and model secrets: this run starts no
            // bridge that needs them.
            .env("TILO_ROOMS_PERSISTENT", "lounge")
            .env("TILO_ROOMS_NAME", "Lounge")
            .env(
                "TILO_ROOMS_DESCRIPTION",
                "The Tilo community: chat, notes and a shared desktop",
            )
            .env("TILO_ROOMS_DEFAULT_ROLE", "editor")
            .env("TILO_ROOMS_LAYOUT", "shared")
            .env("TILO_ROOMS_ADMISSION", "invite")
            .env("TILO_ROOMS_RELAY_ORIGIN", "https://127.0.0.1")
            .env("TILO_ROOMS_WEB_ENTRY", "https://tilo.sh/")
            .env("TILO_ROOMS_SITE_ORIGINS", "https://tilo.sh,https://tilo.so")
            .env("TILO_ROOMS_ROOT_FILE", root_file)
            .env("TILO_ROOMS_FEEDBACK_CHANNEL", "feedback")
            // The deployment starts without seeding Chat: the volume's record
            // cannot be served to new members until it is founded again.
            .env("TILO_ROOMS_SEED_APPS", "")
            .env("TILO_ROOMS_STATE", state.path())
            .env("TILO_ROOMS_WEB", "127.0.0.1:0")
            .env("TILO_ROOMS_RELAY_PLAIN", "127.0.0.1:0")
            .env("TILO_ROOMS_LIVE", "127.0.0.1:0")
            .env("TILO_ROOMS_HTTP", "127.0.0.1:0")
            .env("TILO_SECRETS_DIR", state.path().join("secrets"))
            .env("TILO_RELAY_LOG", "info")
            .env(
                "TILO_SOURCE_REVISION",
                "0123456789abcdef0123456789abcdef01234567",
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if let Some(refound) = refound {
            command.env("TILO_ROOMS_REFOUND", refound);
        }
        // The image lays every version of each built-in app's `surface.toml`
        // at this one path and the host publishes them into its content plane
        // at start (`tools/ci/publish/lounge-context.sh`). The image has them;
        // a rehearsal can name the same set built from its checkout.
        if let Some(surfaces) = std::env::var_os("TILO_LOUNGE_SURFACES") {
            command.env("TILO_SURFACES", surfaces);
        }
        let mut child = command.spawn_locked().expect("the Lounge starts");
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
                .recv_timeout(Duration::from_secs(600))
                .expect("the Lounge says it started");
            if matches!(event, OperatorEvent::Started { .. }) {
                break event;
            }
        };
        let OperatorEvent::Started {
            workspace: Some(workspace),
            plain_origin: Some(plain),
            certificate_sha256: Some(sha256),
            ..
        } = started
        else {
            panic!(
                "a persistent room announces its workspace, its plain listener and its certificate: {started:?}"
            );
        };
        let _ = sha256;
        // The door answers on loopback as `localhost:PORT`; a page's session
        // no longer rides it, but `/host` does.
        let door = Arc::new(Plain(plain.trim_end_matches('/').to_owned()));
        let info = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(tilo_relay_client::https::host_info(door.as_ref()))
        })
        .expect("the host's certificate");
        let host_cert = info.host_cert;
        let address = info.address.expect("the host publishes its iroh endpoint");
        let link = address
            .web
            .as_ref()
            .expect("the host publishes its WebTransport listener");
        assert!(
            !link.origin.ends_with(":0"),
            "the host names the UDP port it bound: {}",
            link.origin
        );
        Self {
            child,
            stdin,
            workspace,
            host_cert,
            address,
            owner_root,
            _state: state,
        }
    }

    /// The workspace's owner as a device of theirs: what the door's
    /// administration asks as.
    fn owner_device(&self) -> Person {
        let key = SigningKey::from_bytes(&[0x5a; 32]);
        let mut cert = DeviceCert {
            user_public_key: self.owner_root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "the Lounge's operator".into(),
            capabilities: DeviceCapabilities::host(),
            issued_at_ns: tilo_clock::unix_now_ns(),
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &self.owner_root).unwrap();
        Person {
            root: self.owner_root.clone(),
            key,
            cert,
        }
    }

    /// Mint an invitation for `to` through the host's own mechanism: the
    /// owner's device asks its session's workspace administration, as a
    /// member does from a page.
    fn invite(&self, to: &Person) -> Vec<u8> {
        let owner = self.owner_device();
        let page = self.page(&owner);
        page.join_placed(&self.workspace, "Lounge").unwrap();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(crate::support::ready(&[&page]));
        });
        let reply = page
            .ask_workspaces(WorkspacesRequest::Invite {
                workspace: self.workspace.clone(),
                // The host takes one request per id, and a second invitation is
                // a request of its own: the id names this ask, not a fixed
                // number two asks would share.
                request_id: *blake3::hash(&to.root.verifying_key().to_bytes()).as_bytes(),
                role: Role::Editor,
                expires_at_ns: tilo_clock::unix_now_ns().saturating_add(600_000_000_000),
                inviter_name: Some("The Lounge".into()),
                wait_list_entry: None,
                recipient: Some(to.root.verifying_key().to_bytes()),
            })
            .unwrap();
        let reply = reply
            .recv_timeout(Duration::from_secs(180))
            .expect("the host answers the invite")
            .expect("the host takes the invitation");
        page.close();
        match reply {
            WorkspacesReply::Invitation {
                signed_invitation, ..
            } => signed_invitation,
            other => panic!("the Lounge issued no invitation: {other:?}"),
        }
    }

    /// A page of `who`'s device on this host's door.
    fn page(&self, who: &Person) -> Link {
        self.page_entering(who, None)
    }

    /// [`Self::page`], its session's first message carrying `entering`.
    fn page_entering(
        &self,
        who: &Person,
        entering: Option<tilo_relay_protocol::entering::Entering>,
    ) -> Link {
        // A page reaches its host over WebTransport, at the listener `/host`
        // names: exactly what a browser in the Lounge has.
        let host = Host::published(&self.address).over(tilo_relay_client::Carrier::WebTransport);
        tilo_session_client::start_session(
            tilo_session_client::PeerConfig {
                host,
                cert: who.cert.clone(),
                host_cert: self.host_cert.clone(),
                revocations: tilo_user_identity::RevocationList::new(),
                entering,
            },
            Arc::new(who.key.clone()),
            Arc::new(tilo_session_client::MemoryPeerStore::default()),
        )
    }
}

fn read_root(path: &std::path::Path) -> SigningKey {
    let hex = std::fs::read_to_string(path).expect("the owner root reads");
    let bytes: [u8; 32] = hex::decode(hex.trim())
        .expect("the root is hex")
        .try_into()
        .expect("the root is 32 bytes");
    SigningKey::from_bytes(&bytes)
}

/// A person the workspace has never seen: a root of their own and a device
/// they certified with it.
struct Person {
    root: SigningKey,
    key: SigningKey,
    cert: DeviceCert,
}

async fn person(name: &str) -> Person {
    let root = SigningKey::generate(&mut UnwrapErr(SysRng));
    let key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let cert = guest_certificate(
        &root,
        key.verifying_key().to_bytes(),
        name,
        tilo_clock::unix_now_ns(),
    )
    .await
    .unwrap();
    Person { root, key, cert }
}

/// A page that joins and takes part: `who`'s device, admitted by `invitation`, which its
/// session's first message carries.
async fn fresh_page(lounge: &Lounge, who: &Person, invitation: &[u8]) -> Link {
    let held = tilo_session_client::device::Held {
        cert: who.cert.clone(),
        host_cert: Some(lounge.host_cert.clone()),
    };
    let (_, entering) = tilo_session_client::device::invitation_entry(
        &lounge.host_cert,
        invitation,
        &who.key,
        tilo_session_client::device::Entering {
            held: Some(&held),
            root: None,
            label: "Fresh visitor",
            name: "Fresh visitor",
            token: None,
        },
    )
    .await
    .unwrap();
    let page = lounge.page_entering(who, Some(entering));
    let admitted = tilo_session_client::device::entered(&page).await;
    assert!(
        admitted.is_ok(),
        "the invitation admits the page: {admitted:?}"
    );
    page.join_placed(&lounge.workspace, "Lounge").unwrap();
    crate::support::ready(&[&page]).await;
    page
}

/// The messages of `channel` in the page's Chat, read through Chat's own
/// model over the record and message parts the page's replica holds.
fn chat_messages(page: &Link, workspace: &str, channel: &str) -> Vec<chat::model::Message> {
    let record = DocumentRoute {
        app_id: CHAT_APP.to_owned(),
        cluster_id: workspace.to_owned(),
        record: String::new(),
    };
    let Some(chat_doc) = crate::support::record_of(page, &record) else {
        println!("the page holds none of Chat's record");
        return Vec::new();
    };
    let state = chat::projection::read(&chat_doc);
    println!(
        "the page's Chat record: {} channel(s) {:?}",
        state.channels.len(),
        state
            .channels
            .ids()
            .map(|id| id.as_str().to_owned())
            .collect::<Vec<_>>()
    );
    let mut parts = Vec::new();
    for index in 0..8u64 {
        let part = DocumentRoute {
            app_id: CHAT_APP.to_owned(),
            cluster_id: workspace.to_owned(),
            record: chat::parts::part(index).route().to_owned(),
        };
        let Some(document) = crate::support::record_of(page, &part) else {
            continue;
        };
        if let Ok(held) = tilo_app::tilo_resource::Reader::<chat::parts::ChatPart>::read(&document)
        {
            parts.push((index, std::rc::Rc::clone(held.state())));
        }
    }
    println!("the page holds {} of Chat's message part(s)", parts.len());
    let messages = chat::messages::Messages::new(parts);
    state.message_rows(&messages, channel)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fresh_page_joins_the_lounge_and_reads_chat() {
    let Ok(snapshot) = std::env::var("TILO_LOUNGE_STATE") else {
        eprintln!("skipped: TILO_LOUNGE_STATE names a copy of the deployment's state");
        return;
    };
    let refound = std::env::var("TILO_LOUNGE_REFOUND").ok();
    let lounge = Lounge::start(std::path::Path::new(&snapshot), refound.as_deref());
    println!("the Lounge started; its workspace is {}", lounge.workspace);

    let visitor = person("Fresh visitor").await;
    let invitation = lounge.invite(&visitor);
    println!("the host minted an invitation for the visitor");

    let page = fresh_page(&lounge, &visitor, &invitation).await;
    let room_id = tilo_relay_protocol::WorkspaceId::parse(&lounge.workspace).unwrap();
    println!("the fresh page joined the Lounge");

    let desktop = DocumentRoute {
        app_id: DESKTOP_APP.to_owned(),
        cluster_id: lounge.workspace.clone(),
        record: DESKTOP_RECORD.to_owned(),
    };
    let chat = DocumentRoute {
        app_id: CHAT_APP.to_owned(),
        cluster_id: lounge.workspace.clone(),
        record: String::new(),
    };
    page.send(Cmd::OpenRoute(chat.clone())).unwrap();
    page.send(Cmd::OpenRoute(desktop.clone())).unwrap();
    // 1. The records' keys: each record's channel is the page's, ready.
    // Their admission is the workspace's participants' (the host's own device
    // among them), not a note's, so this waits on the two routes themselves.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    let channels = loop {
        let channels = page.status().channels;
        let ready = [(CHAT_APP, ""), (DESKTOP_APP, DESKTOP_RECORD)]
            .into_iter()
            .all(|(app, record)| {
                channels.lines().any(|line| {
                    line.starts_with(&format!("route {app}/{record} ")) && line.contains(": ready;")
                })
            });
        if ready || tokio::time::Instant::now() >= deadline {
            break channels;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    println!("--- the page's channels:\n{channels}");
    for (app, record) in [(CHAT_APP, ""), (DESKTOP_APP, DESKTOP_RECORD)] {
        let prefix = format!("route {app}/{record} ");
        let line = channels
            .lines()
            .find(|line| line.starts_with(&prefix))
            .unwrap_or_else(|| panic!("the page holds no channel for {app}/{record}:\n{channels}"));
        assert!(
            line.contains(": ready;"),
            "{app}/{record}'s keys arrive: {line}"
        );
    }
    println!("CHECK record keys: ready for Chat and the shared desktop");

    // 2. Chat's history: its record and message parts open, and the messages
    // read through Chat's own model, as the window shows them.
    // Chat's record and its message part, read once both are in the page's
    // replica.
    let part = DocumentRoute {
        app_id: CHAT_APP.to_owned(),
        cluster_id: lounge.workspace.clone(),
        record: chat::parts::part(0).route().to_owned(),
    };
    let started = std::time::Instant::now();
    let wait = tokio::time::Instant::now() + Duration::from_secs(300);
    while !(crate::support::record_of(&page, &chat).is_some()
        && crate::support::record_of(&page, &part).is_some())
        && tokio::time::Instant::now() < wait
    {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    println!("the page read Chat's record after {:?}", started.elapsed());
    let history = chat_messages(&page, &lounge.workspace, "c_general");
    println!(
        "CHECK chat history: {} message(s) in #general",
        history.len()
    );

    // 3. The desk: the members' windows, as the shared desktop record holds
    // them, read through the desktop model the window draws.
    let desk = crate::support::record_of(&page, &desktop)
        .and_then(|doc| tilo_desktop_model::shared_record::read(&doc, None));
    let windows: Vec<String> = desk
        .as_ref()
        .map(|desk| {
            desk.tree
                .workspaces
                .iter()
                .flat_map(|workspace| workspace.strips.iter())
                .flat_map(|strip| strip.window_groups.iter())
                .flat_map(|group| group.tabs.iter().map(|tab| tab.title.clone()))
                .collect()
        })
        .unwrap_or_default();
    println!("CHECK desk: {} window(s) {windows:?}", windows.len());

    // 4. A second page, another person the workspace never saw, sees the
    // first page's pointer on the desk.
    let other = person("Second visitor").await;
    let second_invitation = lounge.invite(&other);
    let second = fresh_page(&lounge, &other, &second_invitation).await;
    second.send(Cmd::OpenRoute(desktop.clone())).unwrap();
    let first_device = visitor.cert.device_public_key;
    let second_device = other.cert.device_public_key;
    let (tab, at) = (0x41, (12.0f32, 34.0f32));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    let mut heard = Vec::new();
    let mut live_seen = 0usize;
    let mut live_peers = std::collections::BTreeSet::new();
    let mut seen = None;
    loop {
        // The first page says where its pointer is; presence is a live frame
        // the host fans out and never keeps, so it says it again while the
        // second page listens.
        page.send(Cmd::Presence(
            room_id,
            vec![Presence::pointing(&lounge.workspace, tab, at.0, at.1)],
        ))
        .unwrap();
        page.send(Cmd::Presence(
            room_id,
            vec![Presence::desktop(
                &lounge.workspace,
                collab_types::presence::Payload::Status {
                    text: "here".into(),
                    automatic: false,
                },
            )],
        ))
        .unwrap();
        let (frames, people) = presence_frames(&second, 1).await;
        live_seen = live_seen.saturating_add(frames.len());
        live_peers.extend(people);
        heard.extend(frames);
        if let Some(found) = pointer_on(&heard, second_device, &lounge.workspace, tab) {
            seen = Some(found);
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
    }
    println!(
        "the first page's live note: {:?}; the second's: {:?}",
        page.live_note(),
        second.live_note()
    );
    let roster = |page: &Link| {
        page.status()
            .room_members
            .get(&lounge.workspace)
            .map_or(0, std::collections::BTreeSet::len)
    };
    println!(
        "the workspace's roster: {} on the first page, {} on the second",
        roster(&page),
        roster(&second)
    );
    println!(
        "the second page heard {live_seen} live frame(s) from {} device(s); its channels:\n{}",
        live_peers.len(),
        second.status().channels
    );
    println!(
        "CHECK second page: the first page's ({:02x?}) pointer over tab {tab:#x} at {seen:?}",
        &first_device[..4]
    );
    assert!(
        seen.is_some_and(
            |(x, y)| (x - f64::from(at.0)).abs() < 0.5 && (y - f64::from(at.1)).abs() < 0.5
        ),
        "the second page sees the first page's pointer where it put it: {seen:?}"
    );

    second.close();
    page.close();
}

/// Two pages join a persistent Lounge host through its invite door on a fresh
/// state volume, and the second sees where the first put its pointer.
///
/// This is the deployment's own composition (`os/containers/tilo-lounge/
/// entrypoint.sh` starting `tilo-relay room --persistent`), with nothing of
/// production: the host makes its workspace and its owner root as it starts,
/// and both people are guests the host invites through the door it serves. It
/// is the fast check of the one thing a member's page needs to see another
/// member move: both pages join the workspace's live plane over the host's
/// packet relay, the first says where its pointer is on that plane's presence
/// track, and the second's board places it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_pages_join_a_fresh_lounge_and_see_each_other_point() {
    let lounge = Lounge::fresh();
    println!("the Lounge started; its workspace is {}", lounge.workspace);

    let first = person("First visitor").await;
    let second = person("Second visitor").await;
    let first_invitation = lounge.invite(&first);
    let second_invitation = lounge.invite(&second);
    let page = fresh_page(&lounge, &first, &first_invitation).await;
    let other = fresh_page(&lounge, &second, &second_invitation).await;
    println!("both pages joined the Lounge");
    let workspace_id = room_id(&lounge);
    // Both pages are in the workspace's live plane before anything is said:
    // a page says presence only on a plane it joined, and a plane joined
    // after the word would not draw it.
    wait_for_plane(&page).await;
    wait_for_plane(&other).await;

    let (tab, at) = (0x41, (12.0f32, 34.0f32));
    let bound = Duration::from_secs(10);
    let mut heard = Vec::new();
    let mut live_peers = std::collections::BTreeSet::new();
    let mut seen = None;
    // The first page says where its pointer is, once, and the second page's
    // board draws it: what a person's page does when they move the pointer,
    // measured from the word leaving the first page to the second drawing it.
    let started = std::time::Instant::now();
    page.send(Cmd::Presence(
        workspace_id,
        vec![Presence::pointing(&lounge.workspace, tab, at.0, at.1)],
    ))
    .unwrap();
    loop {
        gather_presence(&other, &mut heard, &mut live_peers);
        if let Some(found) = pointer_on(
            &heard,
            second.cert.device_public_key,
            &lounge.workspace,
            tab,
        ) {
            seen = Some(found);
            break;
        }
        if started.elapsed() >= bound {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let elapsed = started.elapsed();
    let live_seen = heard.len();
    println!(
        "the first page's live note: {:?}; the second's: {:?}",
        page.live_note(),
        other.live_note()
    );
    println!(
        "the second page heard {live_seen} live frame(s) from {} device(s), the pointer after {elapsed:?}; its channels:\n{}",
        live_peers.len(),
        other.status().channels
    );
    assert!(
        seen.is_some_and(
            |(x, y)| (x - f64::from(at.0)).abs() < 0.5 && (y - f64::from(at.1)).abs() < 0.5
        ),
        "the second page sees the first page's pointer where it put it within {bound:?}: {seen:?}"
    );

    other.close();
    page.close();
}

/// Wait until `page` is in the workspace's live plane: the plane reports the
/// devices it reads and the objects it heard, which it lists only once joined.
async fn wait_for_plane(page: &Link) {
    let started = tokio::time::Instant::now();
    loop {
        let channels = page.status().channels;
        if channels
            .lines()
            .any(|line| line.starts_with("plane ") && line.contains(" epoch "))
        {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the page never joined the workspace's live plane; its channels:\n{channels}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The workspace's id as a page numbers it.
fn room_id(lounge: &Lounge) -> tilo_relay_protocol::WorkspaceId {
    tilo_relay_protocol::WorkspaceId::parse(&lounge.workspace).unwrap()
}

/// Every presence frame `page` heard over `secs`, and the devices that said
/// anything: presence is a live frame the host fans out and never keeps, so
/// the frames are gathered over a while and replayed into a board. A page
/// takes another device's presence off its plane's presence track
/// ([`Event::LivePlane`] with `Track::Presence`): the plane opened the
/// scope's seal, and the body is the [`Presence`] frame the device said.
async fn presence_frames(
    page: &Link,
    secs: u64,
) -> (
    Vec<([u8; 32], Vec<u8>)>,
    std::collections::BTreeSet<[u8; 32]>,
) {
    let started = tokio::time::Instant::now();
    let mut frames = Vec::new();
    let mut people = std::collections::BTreeSet::new();
    loop {
        gather_presence(page, &mut frames, &mut people);
        if started.elapsed() >= Duration::from_secs(secs) {
            return (frames, people);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Take every presence frame `page` holds now into `frames` (with its sender)
/// and `people`, and report whether it held any.
fn gather_presence(
    page: &Link,
    frames: &mut Vec<([u8; 32], Vec<u8>)>,
    people: &mut std::collections::BTreeSet<[u8; 32]>,
) -> bool {
    let mut heard = false;
    for event in page.drain() {
        if let Event::LivePlane {
            from,
            track: tilo_live::live::Track::Presence,
            body,
            ..
        } = event
        {
            people.insert(from);
            frames.push((from, body));
            heard = true;
        }
    }
    heard
}

/// Where `page` holds another member's pointer over `tab`, read off the board
/// the page keeps: the frames it heard, merged, placed over the tile the desk
/// shows `tab` in.
fn pointer_on(
    heard: &[([u8; 32], Vec<u8>)],
    own: [u8; 32],
    workspace: &str,
    tab: u64,
) -> Option<(f64, f64)> {
    let mut board = tilo_live::presence::Board::new(workspace, own);
    // The clock presence is heard and drawn on, as the session's board keeps it.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock reads past the epoch")
        .as_secs_f64()
        * 1000.0;
    for (from, body) in heard {
        if let Some(frame) = tilo_live::presence::decode(body) {
            board.hear(*from, &frame, now);
        }
    }
    let tiles = [tilo_live::presence::Tile {
        tab,
        x: 0.0,
        y: 0.0,
        width: 1000.0,
        height: 1000.0,
    }];
    let over = tiles.iter().find(|tile| tile.tab == tab)?;
    board
        .pointers(tab, now)
        .first()
        .map(|(_, x, y)| over.point((*x, *y)))
}

/// The host's plain listener, as a page's `fetch`: HTTP/1.1 to it, no TLS
/// (the deployment's ingress terminates TLS in front of it).
struct Plain(String);

impl tilo_relay_client::https::HttpClient for Plain {
    fn post(&self, path: &str, body: Vec<u8>) -> HttpFuture<'_> {
        let url = format!("{}{path}", self.0);
        Box::pin(async move {
            let client = tilo_net::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .build()
                .map_err(|error| error.to_string())?;
            let request = client
                .post(&url)
                .header("content-type", "application/octet-stream")
                .body(body);
            let response = request.send().await.map_err(|error| error.to_string())?;
            let status = response.status().as_u16();
            let bytes = response
                .bytes()
                .await
                .map_err(|error| error.to_string())?
                .to_vec();
            Ok((status, bytes))
        })
    }
}
