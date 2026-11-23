//! A fresh page joining a real Lounge, as the deployment rehearsal: the
//! image's own entrypoint with eu1's environment, `tilo-server workspace
//! --persistent` on a copy of the deployment's state, watched by pages that
//! were never here before.
//!
//! This is the deployment's own composition, not a fixture: the host is
//! `os/containers/tilo-lounge/entrypoint.sh` given eu1's `TILO_WORKSPACES_*`
//! (`apps/tilo-eu1/deployment.yaml`), the state volume is the operator's
//! copy of production, and the pages go through the host's real entry
//! (the session the entry serves) with invitations the
//! host itself mints. What it asks about are the records people use — Chat's
//! and the shared desktop's — and what it checks is what a person sees: the
//! records' keys arrive, Chat's history opens, Chat's composer is there, the
//! layout shows the members' windows, and a second page sees the first page's
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
//! `TILO_LOUNGE_REFOUND` is passed to the host as `TILO_WORKSPACES_REFOUND`, the
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
use tilo_session_client::{Cmd, Event, Events, Link, Outbox};

use std::time::Duration;

use collab_types::presence::Presence;
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server_client::ServerAddress;
use tilo_server_client::invite::guest_certificate;
use tilo_server_protocol::operator::{OperatorCommand, OperatorEvent};
use tilo_server_protocol::workspaces::{Role, WorkspacesReply, WorkspacesRequest};
use tilo_test_env::SpawnUnderEnvLock as _;
use tilo_user_identity::{DeviceCapabilities, DeviceCert};

/// The shared desktop every member sees: the record the deployment refounds.
const DESKTOP_APP: &str = "so.tilo.browser-desktop";
const DESKTOP_DOCUMENT: &str = "desktop";
/// Chat's app.
const CHAT_APP: &str = "so.tilo.chat";
/// The type ids of Chat's text channel and message, as its items record them.
const CHAT_TEXT_CHANNEL: &str = "q7llygsa";
const CHAT_MESSAGE: &str = "hk2t6bvk";

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

/// A PATH holding the server this run built, so the image's `exec tilo-server`
/// runs it.
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

/// The Lounge, as eu1 runs it.
struct Lounge {
    child: Child,
    stdin: Option<ChildStdin>,
    /// The workspace's UUID.
    workspace: String,
    server_cert: DeviceCert,
    /// Where the host's iroh endpoint answers: what a page's session dials,
    /// which is what `/host` publishes.
    address: tilo_server_protocol::web::ServerAddress,
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
            state.path().join("lounge/workspace-id").is_file(),
            "the volume holds the workspace's id: {}",
            state.path().join("lounge").display()
        );
        let mounted = [
            from.join("secrets/workspace-root-lounge"),
            from.join("secrets/workspace-root"),
            snapshot.join("secrets/workspace-root-lounge"),
            snapshot.join("secrets/workspace-root"),
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
        let root_file = secrets.join("workspace-root-lounge");
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
        let root_file = secrets.join("workspace-root-lounge");
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
            .env("TILO_WORKSPACES_LINK", "127.0.0.1:0")
            .env("TILO_WORKSPACES_LINK_ORIGIN", "https://127.0.0.1:0")
            .env("PATH", server_on_path())
            // eu1's environment, as apps/tilo-eu1/deployment.yaml sets it,
            // without the Discord, mail and model secrets: this run starts no
            // bridge that needs them.
            .env("TILO_WORKSPACES_PERSISTENT", "lounge")
            .env("TILO_WORKSPACES_NAME", "Lounge")
            .env(
                "TILO_WORKSPACES_DESCRIPTION",
                "The Tilo community: chat, notes and a shared desktop",
            )
            .env("TILO_WORKSPACES_DEFAULT_ROLE", "editor")
            .env("TILO_WORKSPACES_LAYOUT", "shared")
            .env("TILO_WORKSPACES_ADMISSION", "invite")
            .env("TILO_WORKSPACES_SERVER_ORIGIN", "https://127.0.0.1")
            .env("TILO_WORKSPACES_WEB_ENTRY", "https://tilo.sh/")
            .env(
                "TILO_WORKSPACES_SITE_ORIGINS",
                "https://tilo.sh,https://tilo.so",
            )
            .env("TILO_WORKSPACES_ROOT_FILE", root_file)
            // The deployment starts without seeding Chat: the volume's record
            // cannot be served to new members until it is founded again.
            .env("TILO_WORKSPACES_SEED_APPS", "")
            .env("TILO_WORKSPACES_STATE", state.path())
            .env("TILO_WORKSPACES_WEB", "127.0.0.1:0")
            .env("TILO_WORKSPACES_SERVER_PLAIN", "127.0.0.1:0")
            .env("TILO_WORKSPACES_LIVE", "127.0.0.1:0")
            .env("TILO_WORKSPACES_HTTP", "127.0.0.1:0")
            .env("TILO_SECRETS_DIR", state.path().join("secrets"))
            .env("TILO_SERVER_LOG", "info")
            .env(
                "TILO_SOURCE_REVISION",
                "0123456789abcdef0123456789abcdef01234567",
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if let Some(refound) = refound {
            command.env("TILO_WORKSPACES_REFOUND", refound);
        }
        // The image lays every version of each built-in app's `surface.toml`
        // at this one path and the host publishes them into its content store
        // at start (`tools/ci/publish/lounge-context.sh`). The image has them;
        // a rehearsal can name the same set built from its checkout.
        if let Some(surfaces) = std::env::var_os("TILO_LOUNGE_SURFACES") {
            command.env("TILO_SURFACES", surfaces);
        }
        let mut child = command.spawn_locked().expect("the Lounge starts");
        let stdin = Some(child.stdin.take().expect("the workspace's operator input"));
        let stdout = child
            .stdout
            .take()
            .expect("the workspace's operator output");
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
            host_info: Some(info),
            ..
        } = started
        else {
            panic!(
                "a persistent workspace announces its workspace and who its host is: {started:?}"
            );
        };
        let server_cert = info.server_cert;
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
            server_cert,
            address,
            owner_root,
            _state: state,
        }
    }

    /// The workspace's owner as a device of theirs: what the entry's
    /// administration asks as.
    fn owner_device(&self) -> Person {
        let key = SigningKey::from_bytes(&[0x5a; 32]);
        let mut cert = DeviceCert {
            user_public_key: self.owner_root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "the Lounge's operator".into(),
            capabilities: DeviceCapabilities::member(),
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
                administrator: false,
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

    /// A page of `who`'s device on this host's entry.
    fn page(&self, who: &Person) -> Link {
        self.page_entering(who, None)
    }

    /// [`Self::page`], its session's first message carrying `entering`.
    fn page_entering(
        &self,
        who: &Person,
        entering: Option<tilo_server_protocol::entering::Entering>,
    ) -> Link {
        // A page reaches its host over WebTransport, at the listener `/host`
        // names: exactly what a browser in the Lounge has.
        let host =
            ServerAddress::published(&self.address).over(tilo_server_client::Carrier::WebTransport);
        tilo_session_client::start_session(
            tilo_session_client::PeerConfig {
                host,
                cert: who.cert.clone(),
                server_cert: self.server_cert.clone(),
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
        server_cert: Some(lounge.server_cert.clone()),
    };
    let (_, entering) = tilo_session_client::device::invitation_entry(
        &lounge.server_cert,
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

/// The messages of `channel` in the page's Chat: the items of Chat's record placed in that
/// channel whose type is Chat's message, as the window lists them.
fn chat_messages(page: &Link, workspace: &str, channel: &str) -> usize {
    use automerge::ReadDoc as _;
    let record = DocumentRoute {
        app_id: CHAT_APP.to_owned(),
        cluster_id: workspace.to_owned(),
        document: String::new(),
    };
    let Some(chat_doc) = crate::support::resource_of(page, &record) else {
        println!("the page holds none of Chat's record");
        return 0;
    };
    let map = |parent: &automerge::ObjId, key: &str| match chat_doc.get(parent, key) {
        Ok(Some((automerge::Value::Object(automerge::ObjType::Map), object))) => Some(object),
        _ => None,
    };
    let text = |parent: &automerge::ObjId, key: &str| match chat_doc.get(parent, key) {
        Ok(Some((automerge::Value::Scalar(value), _))) => value.as_str().map(str::to_owned),
        _ => None,
    };
    let Some(app) = map(&automerge::ROOT, "tl").and_then(|records| map(&records, CHAT_APP)) else {
        println!("the page's Chat record holds no conversations");
        return 0;
    };
    let mut messages = 0usize;
    let mut channels = Vec::new();
    for resource in chat_doc.keys(&app).collect::<Vec<_>>() {
        let Some(items) = map(&app, &resource).and_then(|held| map(&held, "$items")) else {
            continue;
        };
        for id in chat_doc.keys(&items).collect::<Vec<_>>() {
            let Some(item) = map(&items, &id) else {
                continue;
            };
            let placed_in = text(&item, "$at")
                .and_then(|at| serde_json::from_str::<serde_json::Value>(&at).ok())
                .and_then(|at| {
                    at.get("in")
                        .and_then(|parent| parent.as_str().map(str::to_owned))
                });
            match text(&item, "$type").as_deref() {
                Some(CHAT_TEXT_CHANNEL) => channels.push(id),
                Some(CHAT_MESSAGE) if placed_in.as_deref() == Some(channel) => messages += 1,
                _ => {}
            }
        }
    }
    println!(
        "the page's Chat record: {} channel(s) {channels:?}",
        channels.len()
    );
    messages
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
    let workspace_id = tilo_server_protocol::WorkspaceId::parse(&lounge.workspace).unwrap();
    println!("the fresh page joined the Lounge");

    let desktop = DocumentRoute {
        app_id: DESKTOP_APP.to_owned(),
        cluster_id: lounge.workspace.clone(),
        document: DESKTOP_DOCUMENT.to_owned(),
    };
    let chat = DocumentRoute {
        app_id: CHAT_APP.to_owned(),
        cluster_id: lounge.workspace.clone(),
        document: String::new(),
    };
    page.send(Cmd::OpenRoute(chat.clone())).unwrap();
    page.send(Cmd::OpenRoute(desktop.clone())).unwrap();
    // 1. The records' keys: each record's channel is the page's, ready.
    // Their admission is the workspace's members' (the host's own device
    // among them), not a note's, so this waits on the two routes themselves.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    let channels = loop {
        let channels = page.status().key_state;
        let ready = [(CHAT_APP, ""), (DESKTOP_APP, DESKTOP_DOCUMENT)]
            .into_iter()
            .all(|(app, record)| {
                channels.lines().any(|line| {
                    line.starts_with(&format!("route {app}/{record}:")) && line.contains(": ready;")
                })
            });
        if ready || tokio::time::Instant::now() >= deadline {
            break channels;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    println!("--- the page's channels:\n{channels}");
    for (app, record) in [(CHAT_APP, ""), (DESKTOP_APP, DESKTOP_DOCUMENT)] {
        let prefix = format!("route {app}/{record}:");
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

    // 2. Chat's history: its record opens, and its messages read as the window lists them, once
    // the record is in the page's replica.
    let started = std::time::Instant::now();
    let wait = tokio::time::Instant::now() + Duration::from_secs(300);
    while crate::support::resource_of(&page, &chat).is_none() && tokio::time::Instant::now() < wait
    {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    println!("the page read Chat's record after {:?}", started.elapsed());
    let history = chat_messages(&page, &lounge.workspace, "c_general");
    println!("CHECK chat history: {history} message(s) in #general");

    // 3. The layout: the members' windows, as the shared desktop record holds
    // them, read through the desktop model the window draws.
    let layout = crate::support::resource_of(&page, &desktop)
        .and_then(|doc| tilo_desktop_model::shared_resource::read(&doc, None));
    let windows: Vec<String> = layout
        .as_ref()
        .map(|layout| {
            layout
                .tree
                .workspaces
                .iter()
                .flat_map(|workspace| workspace.strips.iter())
                .flat_map(|strip| strip.window_groups.iter())
                .flat_map(|group| group.tabs.iter().map(|tab| tab.title.clone()))
                .collect()
        })
        .unwrap_or_default();
    println!("CHECK layout: {} window(s) {windows:?}", windows.len());

    // 4. A second page, another person the workspace never saw, sees the
    // first page's pointer on the layout.
    let other = person("Second visitor").await;
    let second_invitation = lounge.invite(&other);
    let second = fresh_page(&lounge, &other, &second_invitation).await;
    let second_heard = second.events();
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
            workspace_id,
            vec![Presence::pointing(&lounge.workspace, tab, at.0, at.1)],
        ))
        .unwrap();
        page.send(Cmd::Presence(
            workspace_id,
            vec![Presence::desktop(
                &lounge.workspace,
                collab_types::presence::Payload::Status {
                    text: "here".into(),
                    automatic: false,
                },
            )],
        ))
        .unwrap();
        let (frames, people) = presence_frames(&second_heard, 1).await;
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
            .workspace_members
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
        second.status().key_state
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

/// Two pages join a persistent Lounge host through its invite entry on a fresh
/// state volume, and the second sees where the first put its pointer.
///
/// This is the deployment's own composition (`os/containers/tilo-lounge/
/// entrypoint.sh` starting `tilo-server workspace --persistent`), with nothing of
/// production: the host makes its workspace and its owner root as it starts,
/// and both people are guests the host invites through the entry it serves. It
/// is the fast check of the one thing a member's page needs to see another
/// member move: both pages join the workspace's live session over the host's
/// packet relay, the first says where its pointer is on that live session's presence
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
    let workspace_id = workspace_id(&lounge);
    // Both pages are in the workspace's live session before anything is said:
    // a page says presence only on a live session it joined, and a live session joined
    // after the word would not draw it.
    wait_for_live_session(&page).await;
    wait_for_live_session(&other).await;

    let (tab, at) = (0x41, (12.0f32, 34.0f32));
    let bound = Duration::from_secs(10);
    let mut heard = Vec::new();
    let mut live_peers = std::collections::BTreeSet::new();
    let mut seen = None;
    // The first page says where its pointer is, once, and the second page's
    // board draws it: what a person's page does when they move the pointer,
    // measured from the word leaving the first page to the second drawing it.
    let other_heard = other.events();
    let started = std::time::Instant::now();
    page.send(Cmd::Presence(
        workspace_id,
        vec![Presence::pointing(&lounge.workspace, tab, at.0, at.1)],
    ))
    .unwrap();
    loop {
        gather_presence(&other_heard, &mut heard, &mut live_peers);
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
        other.status().key_state
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

/// The presence frames the person's page says while the bridge is in the workspace.
const BRIDGE_FRAMES: usize = 200;

/// An agent's bridge (`tilo mcp`) is a device of the person in their workspace, asking the host's
/// actions through its session, while the person's page in the same workspace says where its
/// pointer is many times a second and another member's page draws it. Nothing on the bridge shows
/// people, so its live session subscribes to none of their presence: not one frame reaches it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agents_bridge_keeps_nothing_of_a_busy_workspaces_presence() {
    let lounge = Lounge::fresh();
    let person_tab = person("The person").await;
    let member = person("Another member").await;
    let agent = person("The agent's computer").await;
    let tab_invitation = lounge.invite(&person_tab);
    let member_invitation = lounge.invite(&member);
    let agent_invitation = lounge.invite(&agent);
    let tab = fresh_page(&lounge, &person_tab, &tab_invitation).await;
    let other = fresh_page(&lounge, &member, &member_invitation).await;
    let bridge = fresh_page(&lounge, &agent, &agent_invitation).await;
    let workspace_id = workspace_id(&lounge);
    wait_for_live_session(&tab).await;
    wait_for_live_session(&other).await;
    wait_for_live_session(&bridge).await;
    // What `tilo mcp` holds once it entered: the device's session and the host's actions asked
    // through it, and nothing that reads the workspace's live frames.
    let actions = tilo_session_client::device::Device::of(
        bridge.clone(),
        agent.cert.clone(),
        lounge.server_cert.clone(),
        "Claude Code",
    )
    .actions(workspace_id);
    let drawn = other.events_where(|event| {
        matches!(
            event,
            Event::LiveSession {
                track: tilo_live::live::Track::Presence,
                ..
            }
        )
    });
    // Each frame is said once the other member's page drew the one before it.
    let deadline = tilo_test_env::hang_deadline();
    let mut heard = 0_usize;
    for n in 0..BRIDGE_FRAMES {
        let at = f32::from(u16::try_from(n).unwrap_or(0));
        tab.send(Cmd::Presence(
            workspace_id,
            vec![Presence::pointing(&lounge.workspace, 0x41, at, at / 2.0)],
        ))
        .unwrap();
        while heard <= n {
            heard = heard.saturating_add(drawn.drain().len());
            assert!(
                std::time::Instant::now() < deadline,
                "the other member's page drew {heard} of the person's presence frames and no more"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
    let channels = bridge.status().key_state;
    println!("the other member drew {heard} presence frame(s); the bridge's channels:\n{channels}");
    assert!(
        channels
            .lines()
            .any(|line| line.starts_with("live session ") && line.ends_with(", heard 0 object(s)")),
        "the bridge heard nothing on its workspace's live_session while the person said {heard} presence \
         frames: {channels}"
    );
    drop(actions);
    bridge.close();
    other.close();
    tab.close();
}

/// Wait until `page` is in the workspace's live session: the live session reports the
/// devices it reads and the objects it heard, which it lists only once joined.
async fn wait_for_live_session(page: &Link) {
    let started = tokio::time::Instant::now();
    loop {
        let channels = page.status().key_state;
        if channels
            .lines()
            .any(|line| line.starts_with("live session ") && line.contains(" epoch "))
        {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the page never joined the workspace's live session; its channels:\n{channels}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The workspace's id as a page numbers it.
fn workspace_id(lounge: &Lounge) -> tilo_server_protocol::WorkspaceId {
    tilo_server_protocol::WorkspaceId::parse(&lounge.workspace).unwrap()
}

/// Every presence frame `page` heard over `secs`, and the devices that said
/// anything: presence is a live frame the host fans out and never keeps, so
/// the frames are gathered over a while and replayed into a board. A page
/// takes another device's presence off its live session's presence track
/// ([`Event::LiveSession`] with `Track::Presence`): the live session opened the
/// scope's seal, and the body is the [`Presence`] frame the device said.
async fn presence_frames(
    page: &Events,
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
    page: &Events,
    frames: &mut Vec<([u8; 32], Vec<u8>)>,
    people: &mut std::collections::BTreeSet<[u8; 32]>,
) -> bool {
    let mut heard = false;
    for event in page.drain() {
        if let Event::LiveSession {
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
/// the page keeps: the frames it heard, merged, placed over the tile the layout
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
