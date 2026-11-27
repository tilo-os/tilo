//! A person changes their servers' entries from any of their devices: naming
//! a server and retiring it are actions on their Servers workspace, which
//! their devices hold like any other workspace of theirs.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the devices and routers a test starts stay alive until each test ends"
)]

use std::time::Duration;

use serde_json::Value;
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, RemoteActionMessage, RemoteActionTarget,
};
use tilo_runtime::device::keep::servers::applied;
use tilo_server_protocol::WorkSetting;
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::servers::person_workspace_id;
use tilo_user_identity::DeviceCapabilities;
use tilo_workspace_service::workspaces::Workspaces;
use tilo_workspace_service::workspaces::servers;

use crate::keep::common::{
    Hosted, Linked, TestDevice, TestUser, join_person_workspace, serve_in, serve_on,
};

/// Ask the device `linked` reaches to do `action` in `workspace` with
/// `argument`, and the device's answer.
async fn try_ask(
    linked: &Linked,
    workspace: &str,
    action: &str,
    argument: Value,
) -> Result<RemoteActionMessage, String> {
    let message = RemoteActionMessage::Invoke(InvokeRequest {
        asks: false,
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
    tilo_test_env::within(
        "the serving device answers the action",
        linked.actions.ask_message(message),
    )
    .await
    .ok_or_else(|| "the device that answers the action was dropped".to_owned())?
    .map_err(|error| error.to_string())
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

/// A person with one server and a laptop, the server holding their Servers
/// workspace and the laptop handed it, both applying what it says.
struct Fixture {
    /// The person: their root signs a revocation of one of their devices, and their devices
    /// hold their workspaces' keys.
    person: TestUser,
    nas: TestDevice,
    laptop: TestDevice,
    /// Another of the person's devices, which asks the host its operations.
    phone: TestDevice,
    desktop: Box<Hosted>,
    admin: String,
    server: Box<Hosted>,
    state: tempfile::TempDir,
    _policy: tempfile::TempDir,
    _following: Vec<applied::Following>,
}

impl Fixture {
    /// The phone's link to the person's host in the Servers workspace it holds.
    async fn linked(&self) -> Linked {
        let workspace = WorkspaceId::parse(&self.admin).unwrap();
        Linked::open_hosted(&self.desktop, &self.phone, workspace)
            .await
            .expect("the workspace admits the person's device")
    }

    fn workspaces(&self) -> &Workspaces {
        self.desktop.host.workspaces()
    }
}

fn person_with_a_server() -> std::pin::Pin<Box<dyn std::future::Future<Output = Fixture> + Send>> {
    Box::pin(async move {
        let alice = TestUser::fresh("Alice");
        let nas = alice.enrol("nas", DeviceCapabilities::member());
        let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
        let phone = alice.enrol("phone", DeviceCapabilities::member());
        let server = Box::new(serve_on(&alice, &nas, WorkSetting::own_person()).await);
        let desktop = Box::new(serve_on(&alice, &laptop, WorkSetting::own_person()).await);
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
        let admin = person_workspace_id(&alice.user_pk);
        tilo_workspace_service::workspaces::person_workspace::found(desktop.host.workspaces())
            .await
            .unwrap();
        desktop
            .host
            .workspaces()
            .enter(&admin, (&phone.cert, &phone.sk))
            .await
            .expect("the workspace takes the person's other device in");
        join_person_workspace(&desktop, &nas, &server).await;
        following.push(applied::follow(
            desktop.host.workspaces(),
            applied::Requests::at(policy.path().join("laptop.request")),
            None,
            state.path().join("laptop-state"),
        ));
        Fixture {
            admin,
            nas,
            laptop,
            phone,
            desktop,
            server,
            person: alice,
            state,
            _policy: policy,
            _following: following,
        }
    })
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
    let linked = fixture.linked().await;
    let nas = fixture.nas.device_pubkey();
    let ask = |action: &'static str, argument: Value| {
        let (linked, admin) = (&linked, &fixture.admin);
        async move {
            try_ask(linked, admin, action, argument)
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
        serde_json::json!({"server": server, "revocation": revocation(&fixture.person.user_sk, nas)}),
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
}

/// A server the person retires while it is away, so the retire is not
/// answered: their device retires its entry itself and refuses its key. Once
/// the server is back it finds out by itself, does what an answered retire
/// does, and is not started again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_server_retired_while_away_retires_itself_once_back() {
    let Fixture {
        person,
        nas,
        laptop: _laptop,
        phone: _phone,
        desktop,
        admin: _admin,
        server,
        state,
        _policy: policy,
        _following: mut following,
    } = person_with_a_server().await;
    following.remove(0).abort();
    let kept = (*server).stop_keeping().await;
    // What the person's device does when the server does not answer in time.
    let id = servers::device_id(&nas.device_pubkey());
    servers::apply(
        desktop.host.workspaces(),
        servers::Servers::ASK_RETIREMENT
            .with((id.clone(), revocation(&person.user_sk, nas.device_pubkey()))),
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

    let back = serve_in(&person, &nas, WorkSetting::own_person(), kept).await;
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
        person: user,
        nas,
        laptop,
        desktop,
        admin,
        server,
        state,
        _following: following,
        ..
    } = person_with_a_server().await;
    let (kept, admin, device) = (server.host.workspaces(), &admin, laptop.device_pubkey());
    // When the server last failed to reach the laptop, once later than `after`: each failed dial
    // marks a later time, and none is under way while the laptop stands unreachable.
    let failed = |after| {
        tilo_test_env::within("the server to fail to reach the laptop again", async move {
            loop {
                let report = kept.copy_report(admin).await.unwrap();
                match report.unreachable.get(&device) {
                    Some(&since) if since > after => return since,
                    _ => tokio::time::sleep(Duration::from_millis(20)).await,
                }
            }
        })
    };
    desktop.stop().await;
    // Unreachable: the server finds the laptop gone.
    failed(0).await;
    // Reachable and refusing: the laptop holds none of the person's workspaces. Once it serves
    // again, the server stands unreachable between two of its dials, and the next of them to
    // fail started after the laptop served.
    let _again = serve_on(&user, &laptop, WorkSetting::own_person()).await;
    failed(failed(0).await).await;
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

/// What `hosted`'s copy of `workspace` holds of the changes `writer` signed: each change's hash
/// by the number its writer gave it. Every copy of the workspace must agree on it.
fn signed_by(
    hosted: &Hosted,
    workspace: WorkspaceId,
    writer: &automerge::ActorId,
) -> std::collections::BTreeMap<u64, automerge::ChangeHash> {
    let copy = hosted.host.workspaces().copy_now(workspace).unwrap();
    let mut numbered = std::collections::BTreeMap::new();
    for bytes in copy.peer.workspace_changes_since(&[]) {
        let change = automerge::Change::try_from(bytes.as_slice()).unwrap();
        if change.actor_id() == writer {
            let earlier = numbered.insert(change.seq(), change.hash());
            assert!(
                earlier.is_none_or(|earlier| earlier == change.hash()),
                "the copy holds two changes under one number of one writer"
            );
        }
    }
    numbered
}

/// Wait until `hosted`'s copy of `workspace` holds `device`'s own row.
async fn lists(hosted: &Hosted, workspace: WorkspaceId, device: &TestDevice, what: &str) {
    tilo_test_env::within(what, async {
        while !hosted
            .host
            .workspaces()
            .copy_now(workspace)
            .is_some_and(|copy| copy.peer.governance().lists_device(&device.cert))
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
}

/// A server a person adds signs itself into their workspace twice: its session signs its row
/// on the governance the person's device answered it, and the hand-over, after the server
/// restarted, has it sign its row on the copy it was handed, which lacks the first. The person
/// then hands it the workspace again. The server signs one change under each number all the
/// same, every copy agrees on them, and a write on either device reaches the other after.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_server_signed_in_twice_and_handed_its_workspace_twice_keeps_one_change_per_number() {
    use tilo_peer_state::join_log::GovernanceMessages as _;

    let alice = TestUser::fresh("Alice");
    let nas = alice.enrol("nas", DeviceCapabilities::member());
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("phone", DeviceCapabilities::member());
    let tablet = alice.enrol("tablet", DeviceCapabilities::member());
    let server = Box::new(serve_on(&alice, &nas, WorkSetting::own_person()).await);
    let desktop = Box::new(serve_on(&alice, &laptop, WorkSetting::own_person()).await);
    let admin = person_workspace_id(&alice.user_pk);
    let workspace = WorkspaceId::parse(&admin).unwrap();
    tilo_workspace_service::workspaces::person_workspace::found(desktop.host.workspaces())
        .await
        .unwrap();

    // The server's session signs its row on what the laptop answered it, and keeps it in the
    // server's store as it sends it, before the laptop hands it the workspace: the copy the
    // hand-over brings does not hold that row.
    let answered = desktop
        .host
        .workspaces()
        .copy_now(workspace)
        .unwrap()
        .peer
        .governance_messages()
        .unwrap();
    let session = tilo_peer_state::own_admission::sign_further_device_row(
        &admin,
        (&answered, &[]),
        &nas.cert,
        &nas.sk,
        tilo_clock::wall(),
    )
    .await
    .unwrap();
    server
        .host
        .workspaces()
        .keep_own_admission(workspace, &nas.device_pubkey(), &session)
        .unwrap();
    let first = automerge::Change::try_from(session[0].as_slice()).unwrap();
    let writer = first.actor_id().clone();

    // The server restarts before the hand-over reaches it.
    let kept = (*server).stop_keeping().await;
    let server = Box::new(serve_in(&alice, &nas, WorkSetting::own_person(), kept).await);
    join_person_workspace(&desktop, &nas, &server).await;
    lists(
        &server,
        workspace,
        &nas,
        "the server's copy to hold its own row",
    )
    .await;
    lists(
        &desktop,
        workspace,
        &nas,
        "the laptop to hold the server's row",
    )
    .await;
    let numbered = signed_by(&server, workspace, &writer);
    assert_eq!(
        numbered.get(&first.seq()),
        Some(&first.hash()),
        "the hand-over signed another change under the number the session's row took"
    );

    // Handed over again, the server takes nothing in a second time and signs nothing again.
    desktop
        .host
        .workspaces()
        .hand_over_named(&admin, nas.device_pubkey(), server.host.node_addr(), &admin)
        .await
        .unwrap();
    assert_eq!(signed_by(&server, workspace, &writer), numbered);

    // Writes go both ways after: the laptop signs a device in, and the server another.
    desktop
        .host
        .workspaces()
        .enter(&admin, (&phone.cert, &phone.sk))
        .await
        .unwrap();
    lists(
        &server,
        workspace,
        &phone,
        "the server to take in the laptop's write",
    )
    .await;
    server
        .host
        .workspaces()
        .enter(&admin, (&tablet.cert, &tablet.sk))
        .await
        .unwrap();
    lists(
        &desktop,
        workspace,
        &tablet,
        "the laptop to take in the server's write",
    )
    .await;
    assert_eq!(
        signed_by(&server, workspace, &writer),
        signed_by(&desktop, workspace, &writer)
    );
}

/// The other way round: the hand-over has the server sign its row on the copy it was handed,
/// the server restarts, and its session then signs itself in on what the person's device had
/// answered it before the hand-over, which lacks that row. What the session signs builds on the
/// row the server's store keeps: no second change under its number.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_server_restarted_after_the_hand_over_signs_its_session_in_after_its_row() {
    use tilo_peer_state::join_log::GovernanceMessages as _;

    let alice = TestUser::fresh("Alice");
    let nas = alice.enrol("nas", DeviceCapabilities::member());
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let server = Box::new(serve_on(&alice, &nas, WorkSetting::own_person()).await);
    let desktop = Box::new(serve_on(&alice, &laptop, WorkSetting::own_person()).await);
    let admin = person_workspace_id(&alice.user_pk);
    let workspace = WorkspaceId::parse(&admin).unwrap();
    tilo_workspace_service::workspaces::person_workspace::found(desktop.host.workspaces())
        .await
        .unwrap();
    let answered = desktop
        .host
        .workspaces()
        .copy_now(workspace)
        .unwrap()
        .peer
        .governance_messages()
        .unwrap();

    join_person_workspace(&desktop, &nas, &server).await;
    lists(
        &server,
        workspace,
        &nas,
        "the server's copy to hold its own row",
    )
    .await;
    let kept = (*server).stop_keeping().await;
    let server = Box::new(serve_in(&alice, &nas, WorkSetting::own_person(), kept).await);
    let own = server
        .host
        .workspaces()
        .own_admissions(workspace, &nas.device_pubkey());
    assert!(
        !own.is_empty(),
        "the server's store keeps the row it signed"
    );
    let writer = automerge::Change::try_from(own.last().unwrap().as_slice())
        .unwrap()
        .actor_id()
        .clone();
    let numbered = signed_by(&server, workspace, &writer);

    let session = tilo_peer_state::own_admission::sign_further_device_row(
        &admin,
        (&answered, &own),
        &nas.cert,
        &nas.sk,
        tilo_clock::wall(),
    )
    .await
    .unwrap();
    for bytes in &session {
        let change = automerge::Change::try_from(bytes.as_slice()).unwrap();
        if change.actor_id() == &writer {
            assert_eq!(
                numbered.get(&change.seq()),
                Some(&change.hash()),
                "the session signed another change under a number the server's row took"
            );
        }
    }
}
