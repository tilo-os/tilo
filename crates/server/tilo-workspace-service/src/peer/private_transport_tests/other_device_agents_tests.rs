//! An agent of the person's that comes from another of their devices acts on this computer only
//! while the person lets such agents in here, which the workspace's grants keep for this device:
//! it is the same grant the workspace's governance records, and the same change Settings makes.

use super::*;
use tilo_action_policy::{Answer, Change, Grantee, MemberAccess, WorkspacePolicy};
use tilo_actions::remote::{
    ActionCallError, Caller, InvocationReceipt, InvokeArgument, InvokeRequest, Pending,
    ReceiptStatus, RemoteActionFrame, RemoteActionMessage, RemoteActionTarget,
};
use tilo_server_protocol::invocation::sign_invocation_now;
use tilo_server_protocol::refusal::Refusal;
use tilo_user_identity::{AgentScope, DeviceCapabilities};

/// The name the agent's connection says it runs it under.
const AGENT: &str = "Claude Code";

/// What the agent's calls reach on this computer's app action server: the caller and whether
/// the person's asks for its steps are answered here.
#[derive(Default)]
struct Recording(std::sync::Mutex<Vec<Caller>>);

impl crate::app_serving::AppActionServer for Recording {
    fn serve(
        &self,
        _workspace_id: WorkspaceId,
        caller: &Caller,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        self.0.lock().unwrap().push(caller.clone());
        match message {
            RemoteActionMessage::Invoke(request) => Some(Pending::ready(
                RemoteActionMessage::Receipt(InvocationReceipt {
                    invocation: request.invocation,
                    status: ReceiptStatus::Completed {
                        output: tilo_actions::ActionOutput::default(),
                    },
                }),
            )),
            other => tilo_actions::remote::refuse(
                &other,
                ActionCallError::Refused {
                    reason: "recorded".into(),
                },
            )
            .map(Pending::ready),
        }
    }
}

fn request() -> InvokeRequest {
    InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: "so.tilo.notes".into(),
            address: tilo_actions::ActionAddress::new("note:1", "rename"),
        },
        argument: InvokeArgument::Text("Plan".into()),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

fn frame(message: RemoteActionMessage) -> Vec<u8> {
    RemoteActionFrame::new(message).encode().unwrap()
}

/// The agent's key and its certificate: its person's root certified it for that one agent in
/// the workspace, as a device of theirs gives its agent a key.
fn agent_key(
    root: &ed25519_dalek::SigningKey,
    user: [u8; 32],
    workspace: WorkspaceId,
    agent: &str,
) -> (ed25519_dalek::SigningKey, DeviceCert) {
    let key = ed25519_dalek::SigningKey::from_bytes(&[0x77; 32]);
    // An agent's grant is bounded: it ends within the lifetime a grant may have.
    let now = unix_now_ns();
    let mut cert = DeviceCert {
        user_public_key: user,
        device_public_key: key.verifying_key().to_bytes(),
        device_label: AGENT.into(),
        capabilities: DeviceCapabilities::agent_grant(AgentScope::agent_in(
            *workspace.as_uuid().as_bytes(),
            agent,
        )),
        issued_at_ns: now.saturating_sub(60_000_000_000),
        expires_at_ns: Some(now.saturating_add(3_600_000_000_000)),
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    (key, cert)
}

fn signed_by(
    workspace: WorkspaceId,
    request: &InvokeRequest,
    cert: &DeviceCert,
    key: &ed25519_dalek::SigningKey,
) -> InvokeRequest {
    let mut request = request.clone();
    request.signed = Some(Box::new(
        sign_invocation_now(&workspace.to_string(), &request, cert, key, unix_now_ns()).unwrap(),
    ));
    request
}

fn person(person: &str) -> Grantee {
    Grantee::person(person.to_owned())
}

/// The person changes the computer's answer for agents from their other devices, through the
/// change the workspace's governance action carries ([`Change::Grant`], [`Change::Revoke`]).
fn keep(
    policy: &mut WorkspacePolicy,
    device: &[u8; 32],
    answer: Option<Answer>,
) -> Result<(), tilo_action_policy::Refusal> {
    let owner = policy.owner.clone().unwrap_or_default();
    let change = match answer {
        Some(answer) => Change::Grant(tilo_action_policy::device_agents::grant(
            &policy.workspace,
            device,
            answer,
            person(&owner),
        )),
        None => Change::Revoke(tilo_action_policy::device_agents::grant_id(device)),
    };
    // The authority is decided on the policy as it stands, which the change then moves on.
    let before = policy.clone();
    let authority = tilo_action_policy::Authority::person(before.party(&owner));
    policy.apply(
        &owner,
        &authority,
        change,
        &tilo_action_policy::Context::at_time(tilo_clock::wall()),
        true,
    )
}

/// An agent of the person's from another of their devices is refused here while the person has
/// not let such agents in on this computer, saying which setting would; let in, it acts as the
/// agent, and its steps ask of the person here or go unasked as the grant's answer says; and the
/// person taking the answer back refuses the next call.
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one session's whole setup, the four steps and the answers between them"
)]
async fn an_agent_from_another_of_the_persons_devices_acts_here_only_while_let_in() {
    let (root, user) = UserIdentity::generate("alice", 1);
    let (host, _) = certified_device(&root, user.public_key, "host");
    let (laptop, _) = certified_device(&root, user.public_key, "laptop");
    let id = WorkspaceId::from_u128(31);
    let agent_id = tilo_actions::remote::device_agent_id(&user.public_key, AGENT);
    let (agent_signing_key, agent_cert) = agent_key(&root, user.public_key, id, &agent_id);
    let owner = hex::encode(user.public_key);
    let tmp = tempfile::tempdir().unwrap();
    let mut wr = crate::workspace::WorkspaceRuntime::open(
        tmp.path(),
        id,
        crate::workspace::Membership::default(),
    )
    .unwrap();
    wr.admit_device(laptop.device_public_key).unwrap();
    let (atx, _arx) = mpsc::channel(8);
    let (disconnect, _disconnect_rx) = mpsc::channel(1);
    wr.add_subscriber(Subscriber {
        device_public_key: laptop.device_public_key,
        tx: atx.clone(),
        disconnect: disconnect.clone(),
        content_waiting: Arc::default(),
        peer: laptop.clone(),
    });
    let registry = Arc::new(AsyncMutex::new(HashMap::from([(
        id,
        Arc::new(AsyncMutex::new(wr)),
    )])));
    let holds = Holds::open();
    // The policy every member's access in the workspace carries, as the host projects it.
    let policy = WorkspacePolicy::new(id.to_string(), &owner);
    project(&registry, id, &policy, host.device_public_key).await;
    let recording = Arc::new(Recording::default());
    let server: Arc<dyn crate::app_serving::AppActionServer> = recording.clone();
    let mut session = holds.ctx(&registry, &atx, &disconnect, (&user.public_key, &host.cert));
    session.app_serving = Some(&server);
    // The connection said it is the run of the agent ([`Envelope::AgentSession`]).
    session.agent.set(AGENT.into()).unwrap();
    let route = || RemoteActionRoute {
        workspace_id: id,
        version: crate::transport::REMOTE_ACTION_ENVELOPE_VERSION,
        to: host.device_public_key,
    };
    let refused = |answer: Option<Envelope>| match answer {
        Some(Envelope::RemoteActionUndeliverable { refusal, .. }) => refusal,
        other => panic!("expected a refusal, got {other:?}"),
    };
    let call = || {
        frame(RemoteActionMessage::Invoke(signed_by(
            id,
            &request(),
            &agent_cert,
            &agent_signing_key,
        )))
    };
    // What this computer keeps for its own agents from the person's other devices: the policy
    // every member's access in the workspace carries.
    let answer = std::sync::Mutex::new(policy);

    // 1 and 2: the agent from the laptop connects and calls, and nobody let such agents in here.
    let refusal = refused(handle_remote_action(&laptop, &session, &[id], route(), call()).await);
    assert!(
        matches!(&refusal, Refusal::NotAllowed(reason)
            if reason.contains("Accept agents from my other devices")),
        "{refusal:?}"
    );
    assert!(recording.0.lock().unwrap().is_empty(), "nothing ran");

    // The person turns it on, through the workspace's governance as Settings does: its steps are
    // put to them here.
    let projected = {
        let mut kept = answer.lock().unwrap();
        keep(&mut kept, &host.device_public_key, Some(Answer::Ask)).unwrap();
        kept.clone()
    };
    project(&registry, id, &projected, host.device_public_key).await;
    assert_eq!(
        handle_remote_action(&laptop, &session, &[id], route(), call()).await,
        None
    );
    {
        let seen = recording.0.lock().unwrap();
        let [caller] = seen.as_slice() else {
            panic!("one call reached the app action server: {seen:?}");
        };
        assert!(
            caller.acting().is_agent(),
            "it acts as the agent: {caller:?}"
        );
        assert!(
            !caller.lets_steps_go_unasked(),
            "a step its grants ask first is put to the person here"
        );
    }

    // Asking goes to the person here; letting them act without asking answers it on the device.
    let projected = {
        let mut kept = answer.lock().unwrap();
        keep(&mut kept, &host.device_public_key, None).unwrap();
        keep(&mut kept, &host.device_public_key, Some(Answer::Allow)).unwrap();
        kept.clone()
    };
    project(&registry, id, &projected, host.device_public_key).await;
    assert_eq!(
        handle_remote_action(&laptop, &session, &[id], route(), call()).await,
        None
    );
    assert!(
        recording
            .0
            .lock()
            .unwrap()
            .last()
            .is_some_and(Caller::lets_steps_go_unasked),
        "its steps go unasked where the person let them"
    );

    // 3 and 4: the person turns it off again through the same change, and the next call is
    // refused with the reason that names the setting.
    let projected = {
        let mut kept = answer.lock().unwrap();
        keep(&mut kept, &host.device_public_key, None).unwrap();
        kept.clone()
    };
    project(&registry, id, &projected, host.device_public_key).await;
    let before = recording.0.lock().unwrap().len();
    let refusal = refused(handle_remote_action(&laptop, &session, &[id], route(), call()).await);
    assert!(
        matches!(&refusal, Refusal::NotAllowed(reason)
            if reason.contains("Accept agents from my other devices")),
        "{refusal:?}"
    );
    assert_eq!(
        recording.0.lock().unwrap().len(),
        before,
        "nothing it asked since reached the app action server"
    );
}

/// Give the workspace's runtime `policy` from now on, as the server's access projection does:
/// `device` is a member whose access carries it.
async fn project(
    registry: &WorkspaceRegistry,
    id: WorkspaceId,
    policy: &WorkspacePolicy,
    device: [u8; 32],
) {
    let shared = registry.lock().await.get(&id).cloned().unwrap();
    let person = tilo_action_policy::Acting::person(policy.owner.clone().unwrap_or_default());
    let mut access = crate::workspace::DeviceAccess::default();
    access.insert(device, MemberAccess::new(policy.clone(), person));
    shared.lock().await.project_access(access);
}
