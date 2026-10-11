//! The session's agent door.
//!
//! `$XDG_RUNTIME_DIR/tilo-session/agent.sock` (0600, in the session's 0700
//! directory) is where a process of this login — an AI agent running in a
//! terminal — reaches this desktop's actions as an *agent*, never as the
//! person. Every production compositor binds it once first login is
//! complete; `tilo-runtime-mcp --connect` inside the login relays to it
//! (`tilo_runtime::session_door`). Framing is newline-delimited MCP JSON-RPC.
//!
//! Three things stand between a connection and the desktop.
//!
//! **The peer** (`SO_PEERCRED`): this session's own account, in its own user
//! and mount namespaces. A sandboxed app is refused — it acts through its
//! own collaboration connection, with its own identity and grants. An
//! unconfined app this session launched, and everything started inside it
//! (a terminal and the agent in it), is exactly who this door is for, so it
//! is admitted to ask — and the app it descends from is named in the prompt.
//! Unlike the admin control socket, the door carries none of the person's
//! authority, so it does not need to refuse them.
//!
//! **The person** (`shell::agent_admission`): nothing the client sends past
//! its `initialize` is read until the person, in trusted chrome, admits it as
//! an agent member of the workspace in front of them.
//!
//! **The broker**: an admitted connection is served the runtime's MCP
//! projection as `Caller::local_agent(agent).in_room(room)` in a fresh
//! execution, so discovery and invocation pass the same gate as every other
//! caller: attributed to `agent:<id>`, limited to what the workspace grants
//! that agent, and revoked live with its membership. There are no pixel
//! tools.

use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use tilo_runtime::session_door::Consent;

use crate::shell::agent_admission::{ClientFacts, DoorRequest};

/// Names a development compositor's door when it is not the real seat's.
/// Honoured only in builds with developer overrides.
pub const AGENT_DOOR_ENV: &str = "TILO_AGENT_DOOR";

/// Connections served at once; more are refused.
const MAX_CONNECTIONS: usize = 16;

/// Where this compositor's door goes, or `None` when it serves none: the
/// real seat's at the session's well-known path; a development compositor
/// (nested, headless) only where its developer named one, since its
/// runtime directory is usually the developer's real session's.
pub(crate) fn door_path(
    real_seat: bool,
    dev_overrides: bool,
    named: Option<std::ffi::OsString>,
    runtime_dir: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    if dev_overrides && let Some(named) = named {
        return Some(PathBuf::from(named)).filter(|path| path.is_absolute());
    }
    if !real_seat {
        return None;
    }
    tilo_runtime::session_door::agent_socket_path(runtime_dir)
}

/// Bind the door at `path`. A path another live session answers on is left
/// alone; a stale socket is replaced.
///
/// # Errors
/// Names why the door is not served.
pub(crate) fn bind(path: &Path) -> Result<UnixListener, String> {
    if path.exists() && UnixStream::connect(path).is_ok() {
        return Err(format!("{} already has a live listener", path.display()));
    }
    let listener = crate::shell::test_control_socket::bind_private(path)
        .map_err(|error| format!("bind failed: {error}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("bind failed: {error}"))?;
    Ok(listener)
}

/// What the kernel says about the process on the other end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Peer {
    pub uid: libc::uid_t,
    /// Whether it shares this session's user and mount namespaces.
    pub shares_namespaces: bool,
}

/// Why a connection was refused before anyone was asked.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PeerRefusal {
    /// No peer credentials: nothing to decide on.
    Unidentified,
    OtherAccount,
    Sandboxed,
    /// The session is not ready to serve agents (its broker is not up).
    Unavailable,
    /// Too many connections at once.
    Busy,
}

impl PeerRefusal {
    pub(crate) fn message(&self) -> &'static str {
        match self {
            Self::Unidentified => "the agent door could not identify the connecting process",
            Self::OtherAccount => {
                "the agent door serves only processes of the account whose session it is"
            }
            Self::Sandboxed => {
                "a sandboxed process cannot use the agent door; apps act through their own \
                 collaboration connection"
            }
            Self::Unavailable => "this session's actions are not available yet; try again shortly",
            Self::Busy => "the agent door is serving as many connections as it takes",
        }
    }
}

/// Decide one connection from its peer's facts. An app this session
/// launched is *not* refused: the door carries no authority of the person's,
/// only what the person grants the agent they admit.
pub(crate) fn peer_decision(peer: &Peer, own_uid: libc::uid_t) -> Result<(), PeerRefusal> {
    if peer.uid != own_uid {
        return Err(PeerRefusal::OtherAccount);
    }
    if !peer.shares_namespaces {
        return Err(PeerRefusal::Sandboxed);
    }
    Ok(())
}

/// Authorise an accepted connection from the kernel's account of its peer,
/// and gather what the person is shown about it.
pub(crate) fn authorize(stream: &UnixStream) -> Result<ClientFacts, PeerRefusal> {
    let credentials =
        super::native::wayland_peer_credentials(stream).ok_or(PeerRefusal::Unidentified)?;
    let pid = u32::try_from(credentials.pid)
        .ok()
        .filter(|pid| *pid != 0)
        .ok_or(PeerRefusal::Unidentified)?;
    let peer = Peer {
        uid: credentials.uid,
        shares_namespaces: super::native::peer_shares_host_security_namespaces(pid),
    };
    // SAFETY: `geteuid` has no failure mode.
    peer_decision(&peer, unsafe { libc::geteuid() })?;
    Ok(ClientFacts {
        client: String::new(),
        version: None,
        pid,
        exe: std::fs::read_link(format!("/proc/{pid}/exe")).ok(),
        host_app: crate::shell::launcher::verified_package_for_descendant(pid),
    })
}

/// Tell a refused client why, as a JSON-RPC error, then let it go.
pub(crate) fn refuse(mut stream: UnixStream, refusal: &PeerRefusal) {
    use std::io::Write;
    let reply =
        tilo_runtime::session_door::error_reply(&serde_json::Value::Null, refusal.message());
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
    let _ = stream.write_all(format!("{reply}\n").as_bytes());
}

/// The door's listener with what serving a connection needs.
pub(crate) struct Door {
    pub listener: UnixListener,
    requests: std::sync::mpsc::Sender<DoorRequest>,
    wake: crate::session_runtime::Wake,
    active: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl Door {
    pub(crate) fn new(
        listener: UnixListener,
        requests: std::sync::mpsc::Sender<DoorRequest>,
        wake: crate::session_runtime::Wake,
    ) -> Self {
        Self {
            listener,
            requests,
            wake,
            active: std::sync::Arc::default(),
        }
    }

    /// Accept every waiting connection. Each authorised one is served on
    /// its own thread over `broker`, the session's action broker.
    pub(crate) fn accept(&self, broker: Option<&crate::shell::collab_broker::AppActionBroker>) {
        loop {
            let stream = match self.listener.accept() {
                Ok((stream, _)) => stream,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
                Err(error) => {
                    tracing::warn!(%error, "agent door: accept failed");
                    return;
                }
            };
            let admitted = authorize(&stream).and_then(|facts| {
                let broker = broker.cloned().ok_or(PeerRefusal::Unavailable)?;
                if self.active.load(std::sync::atomic::Ordering::Relaxed) >= MAX_CONNECTIONS {
                    return Err(PeerRefusal::Busy);
                }
                Ok((facts, broker))
            });
            match admitted {
                Ok((facts, broker)) => self.serve(stream, facts, broker),
                Err(refusal) => {
                    tracing::info!(?refusal, "agent door: connection refused");
                    refuse(stream, &refusal);
                }
            }
        }
    }

    fn serve(
        &self,
        stream: UnixStream,
        facts: ClientFacts,
        broker: crate::shell::collab_broker::AppActionBroker,
    ) {
        let requests = self.requests.clone();
        let wake = std::sync::Arc::clone(&self.wake);
        let active = std::sync::Arc::clone(&self.active);
        active.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let spawned = std::thread::Builder::new()
            .name("tilo-agent-door".into())
            .spawn(move || {
                let ended = tilo_runtime::session_door::serve(
                    stream,
                    crate::shell::remote_actions::BrokerActionHost::new(broker),
                    |hello, connected| ask_person(&requests, &wake, facts, hello, connected),
                );
                if let Err(error) = ended {
                    tracing::debug!(%error, "agent door: client disconnected");
                }
                active.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                // A prompt for a client that is gone is withdrawn next turn.
                wake();
            });
        if let Err(error) = spawned {
            self.active
                .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            tracing::warn!(%error, "agent door: could not serve a connection");
        }
    }
}

/// Hand one client to the compositor thread's consent and wait for the
/// person's answer, giving up when the client leaves.
fn ask_person(
    requests: &std::sync::mpsc::Sender<DoorRequest>,
    wake: &crate::session_runtime::Wake,
    mut facts: ClientFacts,
    hello: &tilo_runtime::session_door::Hello,
    connected: &dyn Fn() -> bool,
) -> Consent {
    facts.client.clone_from(&hello.client_name);
    facts.version.clone_from(&hello.client_version);
    tracing::info!(
        client = %facts.client,
        pid = facts.pid,
        exe = ?facts.exe,
        host_app = ?facts.host_app,
        "agent door: asking the person"
    );
    let (decide, mut decided) = tokio::sync::oneshot::channel();
    if requests.send(DoorRequest { facts, decide }).is_err() {
        return Consent::Refuse("this session is ending".into());
    }
    wake();
    loop {
        match decided.try_recv() {
            Ok(consent) => return consent,
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                return Consent::Refuse("this session is ending".into());
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
        }
        if !connected() {
            // Dropping `decided` withdraws the prompt.
            return Consent::Refuse("the client left".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWN: libc::uid_t = 1000;

    fn peer(uid: libc::uid_t, shares_namespaces: bool) -> Peer {
        Peer {
            uid,
            shares_namespaces,
        }
    }

    #[test]
    fn only_this_accounts_unconfined_processes_may_ask() {
        assert_eq!(peer_decision(&peer(OWN, true), OWN), Ok(()));
        assert_eq!(
            peer_decision(&peer(1001, true), OWN),
            Err(PeerRefusal::OtherAccount),
            "another user"
        );
        assert_eq!(
            peer_decision(&peer(0, true), OWN),
            Err(PeerRefusal::OtherAccount),
            "root acts as itself, not as this session"
        );
        assert_eq!(
            peer_decision(&peer(OWN, false), OWN),
            Err(PeerRefusal::Sandboxed),
            "a sandboxed app runs in its own namespaces"
        );
        assert_eq!(
            peer_decision(&peer(1001, false), OWN),
            Err(PeerRefusal::OtherAccount)
        );
    }

    /// The live check over a real socket: this test process is its own
    /// account, in its own namespaces. The app it would descend from, had
    /// this session launched it, is recorded rather than refused.
    #[test]
    fn a_connection_from_this_account_is_authorised_with_its_kernel_facts() {
        let (client, server) = UnixStream::pair().expect("socket pair");
        let facts = authorize(&server).expect("authorised");
        assert_eq!(facts.pid, std::process::id());
        assert_eq!(
            facts.exe.as_deref(),
            std::env::current_exe().ok().as_deref(),
            "the executable the kernel names"
        );
        assert_eq!(facts.host_app, None, "no session launched this test");
        drop(client);
    }

    #[test]
    fn a_refused_client_reads_why_as_a_json_rpc_error() {
        let (mut client, server) = UnixStream::pair().expect("socket pair");
        refuse(server, &PeerRefusal::Sandboxed);
        let mut line = String::new();
        std::io::Read::read_to_string(&mut client, &mut line).expect("read refusal");
        let reply: serde_json::Value = serde_json::from_str(line.trim()).expect("one JSON line");
        assert_eq!(reply["jsonrpc"], "2.0");
        assert!(
            reply["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("sandboxed"))
        );
    }

    #[test]
    fn the_real_seat_serves_the_session_path_and_a_development_one_only_where_named() {
        let runtime = || Some(std::ffi::OsString::from("/run/user/1000"));
        assert_eq!(
            door_path(true, false, None, runtime()),
            Some(PathBuf::from("/run/user/1000/tilo-session/agent.sock"))
        );
        assert_eq!(door_path(false, true, None, runtime()), None);
        assert_eq!(
            door_path(false, true, Some("/tmp/x/agent.sock".into()), runtime()),
            Some(PathBuf::from("/tmp/x/agent.sock"))
        );
        assert_eq!(
            door_path(false, false, Some("/tmp/x/agent.sock".into()), runtime()),
            None,
            "a release build ignores the override"
        );
        assert_eq!(
            door_path(true, true, Some("relative.sock".into()), runtime()),
            None
        );
        assert_eq!(door_path(true, false, None, None), None);
    }

    /// The whole door in one process, over the production broker gate: a
    /// client (what `tilo-runtime-mcp --connect` relays from an agent in a
    /// terminal) connects, is served nothing until the person admits it
    /// through the `permission-prompts` decision, then lists the live apps
    /// its grants reach as its own agent, and loses them with its membership.
    #[test]
    #[allow(
        clippy::too_many_lines,
        clippy::significant_drop_tightening,
        reason = "one scenario read top to bottom; the broker handle lives for all of it"
    )]
    fn an_admitted_agent_lists_the_live_apps_through_the_door_and_loses_them_on_removal() {
        use crate::shell::chrome_state::SearchScope;
        use crate::shell::chrome_state::{ChromeState, PromptRequest};
        use crate::shell::search::SearchRegistry;
        use crate::shell::search::action_api::{ActionCatalog, AllVisible, ArgValue, ResourceRef};
        use crate::shell::search::providers::permission_prompts::{
            PROVIDER_ID, PermissionPromptsProvider,
        };
        use crate::shell::state::{FreshId as _, WorkspaceAuthority as _};
        use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree, WorkspaceTreeHandle};
        use collab_types::caps::{CapOverride, Scope, Subject, Verb};
        use std::io::{BufRead, Write};
        use std::sync::Arc;

        let mut tree = WorkspaceTree::default();
        let id = WorkspaceId::new();
        let mut workspace = Workspace::new(id, "Demo");
        workspace.set_owner("shell-fixture");
        let room = workspace.room;
        tree.desktop.workspaces.push(workspace);
        tree.desktop.active = id;
        let handle = WorkspaceTreeHandle::new(tree);
        let chrome = ChromeState::new_with_fixture_handle(
            handle.clone(),
            crate::shell::hosted_inputs::HostedTileInputs::default(),
        );
        chrome.borrow_mut().test_only_owner_override = Some(true);
        let broker =
            crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_authority(
                Arc::new(crate::shell::broker_services::NativeWorkspaceAuthority(
                    handle.clone(),
                )),
                tilo_action_broker::services::HostServices::default(),
            );
        broker.replace_published_surfaces_for_test(vec![
            crate::shell::collab_broker_capnp::PublishedSurface {
                peer_id: 7,
                app_id: "so.tilo.settings".into(),
                route: crate::shell::collab_broker::CrdtRoute::app(
                    "so.tilo.settings",
                    room.to_string(),
                ),
                actions: vec![crate::shell::collab_broker_capnp::PublishedAction {
                    resource: "settings.display".into(),
                    semantic_identity: None,
                    action: "inspect".into(),
                    resource_kind: "settings".into(),
                    resource_label: "Displays".into(),
                    action_label: "Inspect".into(),
                    description: None,
                    operation_kind: tilo_actions::OperationKind::View,
                    params: vec![],
                    contract: tilo_actions::ActionContract {
                        permission: tilo_actions::PermissionRequirement::Read,
                        ..tilo_actions::ActionContract::view()
                    },
                    input: crate::shell::collab_broker_capnp::PublishedInput::None,
                    view: None,
                }],
                live: true,
            },
        ]);

        broker.replace_action_directory_roots_for_test(vec![
            crate::shell::collab_broker_capnp::ActionDirectoryRoot {
                peer_id: 7,
                app_id: "so.tilo.settings".into(),
                cluster_id: room.to_string(),
                record_id: String::new(),
            },
        ]);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.sock");
        let (requests, mut consent) = crate::shell::agent_admission::channel(None, None);
        let door = Door::new(bind(&path).unwrap(), requests, Arc::new(|| {}));
        let mut client = UnixStream::connect(&path).unwrap();
        let (replies_tx, replies) = std::sync::mpsc::channel::<serde_json::Value>();
        {
            let reader = client.try_clone().unwrap();
            std::thread::spawn(move || {
                for line in std::io::BufReader::new(reader).lines() {
                    let Ok(line) = line else { return };
                    if replies_tx
                        .send(serde_json::from_str(&line).unwrap())
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }
        let mut send = |value: serde_json::Value| {
            client.write_all(format!("{value}\n").as_bytes()).unwrap();
        };
        // One turn of the compositor loop: accept, then hand on answers.
        let turn = |consent: &mut crate::shell::agent_admission::AgentDoorConsent| {
            door.accept(Some(&broker));
            consent.pump(&chrome);
        };
        let reply = |consent: &mut crate::shell::agent_admission::AgentDoorConsent| {
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            loop {
                turn(consent);
                if let Ok(reply) = replies.recv_timeout(Duration::from_millis(20)) {
                    return reply;
                }
                assert!(std::time::Instant::now() < deadline, "no reply");
            }
        };
        let list_apps = |id: u64| {
            serde_json::json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
                "params":{"name":"tilo_list_apps","arguments":{}}})
        };

        send(
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-06-18","capabilities":{},
            "clientInfo":{"name":"Claude Code","version":"2"}}}),
        );
        // Queued behind the hello: it must not run before the person answers.
        send(list_apps(2));
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let prompt = loop {
            turn(&mut consent);
            let front = chrome.borrow().pending_grants.front().map(|pending| {
                assert!(matches!(pending.request, PromptRequest::AgentAdmission(_)));
                pending.id
            });
            if let Some(front) = front {
                break front;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the person was never asked"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        for _ in 0..5 {
            turn(&mut consent);
        }
        assert!(
            replies.recv_timeout(Duration::from_millis(200)).is_err(),
            "the door answered before the person did"
        );

        // The person admits it, through the same decision a click makes.
        let decision = format!("allow-once@{prompt}");
        let mut registry = SearchRegistry::new();
        registry.register(std::rc::Rc::new(PermissionPromptsProvider::default()));
        let mut doc = automerge::AutoCommit::new();
        let actions = ActionCatalog::build_exact(
            &registry,
            &ResourceRef::new(PROVIDER_ID, &decision),
            SearchScope::All,
            None,
            &chrome.borrow(),
            &doc,
            &automerge::ROOT,
            &AllVisible,
        );
        actions
            .select(&ResourceRef::new(PROVIDER_ID, &decision))
            .expect("the waiting decision")
            .invoke(
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &chrome,
                [("confirm".into(), ArgValue::Boolean(true))],
            )
            .expect("the owner admits it");

        let initialized = reply(&mut consent);
        assert_eq!(initialized["id"], 1, "{initialized}");
        assert!(
            initialized["result"]["serverInfo"].is_object(),
            "{initialized}"
        );
        let members: Vec<_> = handle.read().desktop.workspaces[0]
            .agent_members()
            .into_iter()
            .map(|(agent, _)| agent)
            .collect();
        assert_eq!(
            members,
            ["claude-code"],
            "admitted as a visible, revocable member"
        );

        // What the person reviews in Settings → Agents: the Settings app.
        handle.write().desktop.workspaces[0].grant_capability(
            CapOverride::grant(
                Subject::agent("claude-code"),
                Scope::App("so.tilo.settings".into()),
                Verb::Read,
            )
            .with_id("agent-door-test-settings"),
        );
        let listed = loop {
            let listed = reply(&mut consent);
            if listed["id"] == 3 {
                break listed;
            }
            // The call queued before admission answers too, as the agent.
            assert_eq!(listed["id"], 2, "{listed}");
            send(list_apps(3));
        };
        assert_eq!(listed["result"]["isError"], false, "{listed}");
        let apps = listed["result"]["structuredContent"]["apps"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            apps.iter().any(|app| app["app_id"] == "so.tilo.settings"),
            "the live app is listed: {listed}"
        );

        // Removing the agent revokes its catalogue at once.
        handle.write().desktop.workspaces[0]
            .capabilities
            .retain(|cap| {
                !matches!(
                    &cap.subject,
                    Subject::Principal(collab_types::caps::Principal::Agent(_))
                )
            });
        send(list_apps(4));
        let revoked = reply(&mut consent);
        assert_eq!(revoked["id"], 4);
        let still = revoked["result"]["structuredContent"]["apps"]
            .as_array()
            .is_some_and(|apps| apps.iter().any(|app| app["app_id"] == "so.tilo.settings"));
        assert!(!still, "a removed agent still lists the app: {revoked}");
    }
}
