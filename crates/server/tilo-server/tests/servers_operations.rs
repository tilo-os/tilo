//! A person reaches their servers from any of their devices: each operation is
//! an action on their Servers workspace, which their devices hold like any
//! other workspace of theirs. The server the entry names carries the request
//! out as its own and keeps what it found or did, and the action answers
//! with that. An agent of the person asks the same actions; another person is
//! refused.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the devices and routers a test starts stay alive until each test ends"
)]

use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, RemoteActionFrame, RemoteActionMessage, RemoteActionTarget,
};
use tilo_server::WorkspaceId;
use tilo_server::servers::{self, applied, person_workspace_id};
use tilo_server::transport::{Envelope, REMOTE_ACTION_ENVELOPE_VERSION, ServerAnswer};
use tilo_server::workspaces::Workspaces;
use tilo_server_protocol::WorkSetting;
use tilo_server_protocol::transport::ServerOperationResult;
use tilo_user_identity::DeviceCapabilities;

use crate::common::{
    Hosted, TestDevice, TestUser, device_endpoint, join_person_workspace, serve_in, serve_on,
};

/// Stand in for the root helper that carries out what changes the
/// machine: answer every request written under `state_root` at the same files
/// the real helper uses, until the guard is dropped.
fn operations_helper(state_root: &Path) -> Guard {
    let request = state_root.join(tilo_server::operations::CONTROL_REQUEST);
    let result = state_root.join(tilo_server::operations::CONTROL_RESULT);
    Guard(tokio::spawn(async move {
        let mut seen = String::new();
        loop {
            if let Ok(text) = std::fs::read_to_string(&request)
                && text != seen
            {
                seen.clone_from(&text);
                let id = text
                    .lines()
                    .find_map(|line| line.strip_prefix("id="))
                    .unwrap_or_default();
                std::fs::write(&result, format!("{id} 1\nNov 18 tilo-server: serving\n")).unwrap();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }))
}

struct Guard(tokio::task::JoinHandle<()>);

impl Drop for Guard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Ask the device `at` serves to do `action` in `workspace` with
/// `argument`, as `client`'s session.
async fn try_ask(
    client: &tilo_server::ServerClient,
    at: &TestDevice,
    workspace: &str,
    action: &str,
    argument: Value,
) -> Result<RemoteActionMessage, String> {
    let workspace_id = WorkspaceId::parse(workspace).ok_or("not a workspace id")?;
    let message = RemoteActionMessage::Invoke(InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget::workspace_governance(workspace, action),
        argument: InvokeArgument::Json(match argument {
            Value::Object(object) => object,
            _ => return Err("arguments are an object".into()),
        }),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    });
    client
        .send(Envelope::RemoteAction {
            workspace_id,
            version: REMOTE_ACTION_ENVELOPE_VERSION,
            to_device_public_key: at.device_pubkey(),
            from_device_public_key: [0; 32],
            from_user_public_key: [0; 32],
            frame: RemoteActionFrame::new(message)
                .encode()
                .map_err(|error| error.to_string())?,
        })
        .map_err(|_| "the session closed".to_owned())?;
    tilo_test_env::within("the serving device answers the action", async {
        loop {
            match client.recv().await {
                Some(Envelope::RemoteAction { frame, .. }) => {
                    break RemoteActionFrame::decode(&frame)
                        .map(|frame| frame.message)
                        .map_err(|error| error.to_string());
                }
                Some(Envelope::RemoteActionUndeliverable { refusal, .. }) => {
                    break Err(refusal.to_string());
                }
                None => break Err("the session closed".to_owned()),
                Some(_) => {}
            }
        }
    })
    .await
}

/// What an invocation came to: what the action answered, or why it was
/// refused.
fn completed(answer: RemoteActionMessage) -> Result<Value, String> {
    use tilo_actions::remote::ReceiptStatus;
    let RemoteActionMessage::Receipt(receipt) = answer else {
        return Err("an invocation is answered with a receipt".into());
    };
    match receipt.status {
        ReceiptStatus::Completed { output } => output
            .value
            .ok_or_else(|| "the action answered nothing".to_owned()),
        ReceiptStatus::Rejected { error } => Err(error.to_string()),
        other => Err(format!("the action answered {other:?}")),
    }
}

/// What an operation's invocation came to: the operation's result, or why it
/// was refused.
fn outcome(answer: RemoteActionMessage) -> Result<ServerOperationResult, String> {
    completed(answer)
        .and_then(|value| serde_json::from_value(value).map_err(|error| error.to_string()))
}

/// A person with one server and a laptop, the server holding their Servers
/// workspace and the laptop handed it, both applying what it says.
struct Fixture {
    /// The person's root, which signs a revocation of one of their devices.
    root: ed25519_dalek::SigningKey,
    nas: TestDevice,
    laptop: TestDevice,
    desktop: Hosted,
    admin: String,
    server: Hosted,
    state: tempfile::TempDir,
    _policy: tempfile::TempDir,
    _helper: Guard,
    _following: Vec<tokio::task::JoinHandle<()>>,
}

impl Fixture {
    /// Ask the server `action` with `argument` on `client`, the laptop's
    /// session with the laptop itself: the person's own device asking its
    /// Servers workspace, as it asks anything of it, on the one session it
    /// holds.
    async fn ask(
        &self,
        client: &tilo_server::ServerClient,
        action: &str,
        mut argument: Value,
    ) -> Result<ServerOperationResult, String> {
        argument["server"] = Value::String(hex::encode(self.nas.device_pubkey()));
        let started = std::time::Instant::now();
        // A refusal names the operation and how long its answer took: every
        // operation here answers in a few hundred milliseconds (measured, alone
        // and on a loaded builder), so one that waited seconds longer is read
        // where it happened rather than as the whole test's wall clock.
        try_ask(client, &self.laptop, &self.admin, action, argument)
            .await
            .and_then(outcome)
            .map_err(|error| format!("{action} took {:?}: {error}", started.elapsed()))
    }

    /// The laptop's session with its own workspace service, subscribed to the
    /// Servers workspace it holds.
    async fn session(&self) -> tilo_server::ServerClient {
        let client = self
            .desktop
            .host
            .connect_local(
                self.laptop.cert.clone(),
                self.laptop.sk.clone(),
                tilo_user_identity::RevocationList::new(),
            )
            .await
            .unwrap();
        let workspace_id = WorkspaceId::parse(&self.admin).unwrap();
        client.send(Envelope::Subscribe { workspace_id }).unwrap();
        tilo_test_env::within("the workspace is served here", async {
            while !matches!(
                client.recv().await.expect("the session stays open"),
                Envelope::Ready { .. }
            ) {}
        })
        .await;
        client
    }

    fn workspaces(&self) -> &Workspaces {
        self.desktop.host.workspaces()
    }
}

async fn person_with_a_server() -> Fixture {
    let alice = TestUser::fresh("Alice");
    let nas = alice.enrol("nas", DeviceCapabilities::member());
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let server = serve_on(&alice, &nas, WorkSetting::own_person()).await;
    let desktop = serve_on(&alice, &laptop, WorkSetting::own_person()).await;
    let state = tempfile::tempdir().unwrap();
    let policy = tempfile::tempdir().unwrap();
    let mut following = vec![applied::follow(
        server.host.workspaces(),
        applied::Requests::at(policy.path().join("nas.request")),
        None,
        state.path().to_path_buf(),
    )];
    // The person's own device makes the Servers workspace when it is first
    // needed, enters the server among their servers there, and hands it the
    // workspace as it hands a server any workspace its entry names.
    servers::found(desktop.host.workspaces()).await.unwrap();
    join_person_workspace(&desktop, &nas, &server).await;
    following.push(applied::follow(
        desktop.host.workspaces(),
        applied::Requests::at(policy.path().join("laptop.request")),
        None,
        state.path().join("laptop-state"),
    ));
    let helper = operations_helper(state.path());
    Fixture {
        admin: person_workspace_id(&alice.user_pk),
        root: alice.user_sk.clone(),
        nas,
        laptop,
        desktop,
        server,
        state,
        _policy: policy,
        _helper: helper,
        _following: following,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_persons_device_does_every_operation_on_its_server_as_an_action() {
    let fixture = person_with_a_server().await;
    let client = fixture.session().await;
    let status = fixture
        .ask(&client, "server.status", serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(status.operation, "status");
    assert!(status.done);
    let ServerAnswer::Status(read) = &status.answer else {
        panic!("status answers with the server: {status:?}");
    };
    assert!(
        read.holding
            .iter()
            .any(|held| held.workspace == fixture.admin),
        "it holds the Servers workspace: {read:?}"
    );

    let logs = fixture
        .ask(
            &client,
            "server.logs",
            serde_json::json!({"service": "tilo-server.service", "lines": 10}),
        )
        .await
        .unwrap();
    assert_eq!(
        logs.answer,
        ServerAnswer::Text("Nov 18 tilo-server: serving\n".into())
    );

    for (action, expected, done) in [
        ("server.logs", "logs", true),
        ("server.restart", "restart", true),
        ("server.update", "update", true),
        ("server.pause-updates", "pause-updates", true),
        ("server.resume-updates", "resume-updates", true),
        ("server.reboot", "reboot", false),
        ("server.rollback", "rollback", false),
        ("server.drain", "drain", true),
        ("server.set-default-start", "set-default-start", true),
    ] {
        let argument = if action == "server.restart" || action == "server.logs" {
            serde_json::json!({"service": "tilo-server.service", "lines": 5})
        } else {
            serde_json::json!({})
        };
        let result = fixture.ask(&client, action, argument).await.unwrap();
        assert_eq!(result.operation, expected, "{action}");
        assert_eq!(result.done, done, "{action}: {result:?}");
        assert!(!result.summary.is_empty(), "{action}");
    }

    // What was asked is in the entry the person reads, and what a later
    // status says of it.
    let servers = servers::read(fixture.workspaces()).await.unwrap();
    let asked: Vec<String> = servers
        .servers
        .iter()
        .flat_map(|(_, server)| {
            server
                .asked
                .iter()
                .map(|(_, asked)| asked.operation.clone())
        })
        .collect();
    for operation in [
        "status",
        "logs",
        "restart",
        "update",
        "pause-updates",
        "resume-updates",
        "reboot",
        "rollback",
        "drain",
        "set-default-start",
    ] {
        assert!(asked.iter().any(|asked| asked == operation), "{asked:?}");
    }
    let status = fixture
        .ask(&client, "server.status", serde_json::json!({}))
        .await
        .unwrap();
    let ServerAnswer::Status(read) = status.answer else {
        panic!("status answers with the server");
    };
    assert!(
        read.history.iter().any(|entry| entry.operation == "drain"),
        "the history is the entry's: {:?}",
        read.history
    );
    client.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_of_the_person_does_the_same_operations() {
    let fixture = person_with_a_server().await;
    let client = fixture.session().await;
    // The session says it is an agent's, as `tilo mcp` says: everything it
    // asks from then on is that agent's, for the person.
    client
        .send(Envelope::AgentSession {
            request_id: 7,
            label: "Claude Code".into(),
        })
        .unwrap();
    let refusal = tilo_test_env::within(
        "the serving device takes the session for the agent",
        async {
            loop {
                match client.recv().await {
                    Some(Envelope::AgentSessionAnswer { refusal, .. }) => break refusal,
                    Some(_) => {}
                    None => break Some("the session closed".to_owned()),
                }
            }
        },
    )
    .await;
    assert_eq!(
        refusal, None,
        "an agent of the person's is taken: {refusal:?}"
    );
    let answer = try_ask(
        &client,
        &fixture.laptop,
        &fixture.admin,
        "server.status",
        serde_json::json!({"server": hex::encode(fixture.nas.device_pubkey())}),
    )
    .await
    .unwrap();
    let status = outcome(answer).expect("the agent sees the server's status");
    assert_eq!(status.operation, "status");
    assert!(
        matches!(status.answer, ServerAnswer::Status(_)),
        "{status:?}"
    );
    client.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn another_person_reaches_no_server_of_this_persons() {
    let fixture = person_with_a_server().await;
    let mallory = TestUser::fresh("Mallory");
    let theirs = mallory.enrol("their laptop", DeviceCapabilities::primary());
    // The serving device admits its own person's devices only, so another
    // person's session never opens; one opened elsewhere would still be
    // refused the action.
    let refused = match tilo_server::ServerClient::connect_to_pinned_host(
        device_endpoint(&theirs).await,
        fixture.desktop.host.node_addr(),
        theirs.cert.clone(),
        theirs.sk.clone(),
        tilo_user_identity::RevocationList::new(),
    )
    .await
    {
        Err(_) => true,
        Ok(client) => {
            let asked = try_ask(
                &client,
                &fixture.laptop,
                &fixture.admin,
                "server.status",
                serde_json::json!({"server": hex::encode(fixture.nas.device_pubkey())}),
            )
            .await;
            let refused = match asked {
                Ok(answer) => outcome(answer).is_err(),
                Err(_) => true,
            };
            client.close().await;
            refused
        }
    };
    assert!(
        refused,
        "another person's device reaches no server of Alice's"
    );
}

/// The revocation of `device`'s key `root` signs, as the identity step answers
/// it.
fn revocation(root: &ed25519_dalek::SigningKey, device: [u8; 32]) -> String {
    let mut entry = tilo_user_identity::RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: device,
        revoked_at_ns: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            .try_into()
            .unwrap(),
        reason: tilo_user_identity::RevocationReason::Retired,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, root).unwrap();
    serde_json::to_string(&entry).unwrap()
}

/// A person names a server and takes it out of service from another of their
/// devices, as actions on its entry. Retiring it carries the revocation of its
/// key their root signed: the entry is retired, the server lets go of
/// everything it holds, and the server and their other device both refuse its
/// key.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_persons_device_renames_and_retires_a_server() {
    let fixture = person_with_a_server().await;
    let client = fixture.session().await;
    let nas = fixture.nas.device_pubkey();
    let ask = |action: &'static str, argument: Value| {
        let (client, laptop, admin) = (&client, &fixture.laptop, &fixture.admin);
        async move {
            try_ask(client, laptop, admin, action, argument)
                .await
                .and_then(completed)
        }
    };
    let server = hex::encode(nas);

    ask(
        "server.rename",
        serde_json::json!({"server": server, "name": "Kitchen NAS"}),
    )
    .await
    .unwrap();
    assert!(
        ask(
            "server.rename",
            serde_json::json!({"server": server, "name": "NAS\u{202e}hidden"}),
        )
        .await
        .is_err(),
        "a name that reads in another direction is refused"
    );
    let servers = servers::read(fixture.workspaces()).await.unwrap();
    assert_eq!(servers.active(&nas).unwrap().label, "Kitchen NAS");

    let mallory = TestUser::fresh("Mallory");
    for wrong in [revocation(&mallory.user_sk, nas), "{}".to_owned()] {
        assert!(
            ask(
                "server.retire",
                serde_json::json!({"server": server, "revocation": wrong}),
            )
            .await
            .is_err(),
            "a server is retired only with its own person's revocation of it"
        );
    }
    assert!(
        !fixture
            .workspaces()
            .revocations()
            .refuses(&fixture.nas.cert)
    );
    let retired = ask(
        "server.retire",
        serde_json::json!({"server": server, "revocation": revocation(&fixture.root, nas)}),
    )
    .await
    .unwrap();
    assert_eq!(retired["retired"], true);
    assert_eq!(
        retired["answered"], true,
        "the server took its retirement itself"
    );
    let servers = servers::read(fixture.workspaces()).await.unwrap();
    assert!(servers.active(&nas).is_none(), "its entry is retired");
    assert!(
        fixture
            .workspaces()
            .revocations()
            .refuses(&fixture.nas.cert),
        "the person's device refuses the server's key"
    );
    let held = fixture.server.host.workspaces();
    // It notes that it is out of service where its machine reads it, so its
    // daemon does not start again.
    let noted = fixture.state.path().join("decommissioned");
    tilo_test_env::within(
        "the server lets go of everything and refuses its own key",
        async {
            while !(held.list().await.is_empty()
                && held.revocations().refuses(&fixture.nas.cert)
                && noted.exists())
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        },
    )
    .await;
    client.close().await;
}

/// A server the person retires while it is away, so the retire is not
/// answered: their device retires its entry itself and refuses its key. Once
/// the server is back it finds out by itself, does what an answered retire
/// does, and is not started again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_server_retired_while_away_retires_itself_once_back() {
    let Fixture {
        root,
        nas,
        laptop: _laptop,
        desktop,
        admin: _admin,
        server,
        state,
        _policy: policy,
        _helper,
        _following: mut following,
    } = person_with_a_server().await;
    following.remove(0).abort();
    let kept = server.stop_keeping().await;
    // What the person's device does when the server does not answer in time.
    let id = servers::device_id(&nas.device_pubkey());
    servers::apply(
        desktop.host.workspaces(),
        servers::Servers::ASK_RETIREMENT.with((id.clone(), revocation(&root, nas.device_pubkey()))),
    )
    .await
    .unwrap();
    servers::apply(
        desktop.host.workspaces(),
        servers::Servers::RETIRE_SERVER.with(id),
    )
    .await
    .unwrap();
    tilo_test_env::within("the person's device refuses the server's key", async {
        while !desktop.host.workspaces().revocations().refuses(&nas.cert) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;

    let user = TestUser {
        user_pk: root.verifying_key().to_bytes(),
        user_sk: root,
    };
    let back = serve_in(&user, &nas, WorkSetting::own_person(), kept).await;
    let _follow = applied::follow(
        back.host.workspaces(),
        applied::Requests::at(policy.path().join("nas.request")),
        None,
        state.path().to_path_buf(),
    );
    let back = back.host.workspaces();
    let noted = state.path().join("decommissioned");
    tilo_test_env::within(
        "the server back finds out it was retired, lets go and notes it",
        async {
            while !(back.list().await.is_empty()
                && back.revocations().refuses(&nas.cert)
                && noted.exists())
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        },
    )
    .await;
}

/// A server its person's device cannot be reached from, then refuses for a
/// reason other than a revocation of its key (the device was set up again and
/// holds none of the person's workspaces), stays in service: only its root's
/// signed revocation retires it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_server_refused_without_a_revocation_stays_in_service() {
    let Fixture {
        root,
        nas,
        laptop,
        desktop,
        server,
        state,
        _following: following,
        ..
    } = person_with_a_server().await;
    let user = TestUser {
        user_pk: root.verifying_key().to_bytes(),
        user_sk: root,
    };
    desktop.stop().await;
    // Unreachable, then reachable and refusing: several of the server's
    // dials over each.
    tokio::time::sleep(Duration::from_secs(6)).await;
    let _again = serve_on(&user, &laptop, WorkSetting::own_person()).await;
    tokio::time::sleep(Duration::from_secs(6)).await;
    let kept = server.host.workspaces();
    assert!(
        !kept.revocations().refuses(&nas.cert),
        "the server still takes its own key"
    );
    assert!(!kept.list().await.is_empty(), "it keeps its workspaces");
    assert!(
        !state.path().join("decommissioned").exists(),
        "its machine starts it again"
    );
    drop(following);
}
