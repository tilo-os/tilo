//! An app's writes are attributed to the agent its running invocation's
//! custody names, and to nothing an app could claim.
use super::invocation_journal::InvocationOwner;
use super::*;
use crate::services::{FileWriteAttribution, FileWriteAttributions};
use crate::test_support::{Workspace, WorkspaceId, WorkspaceTree};

/// The filesystem's side, recording what it was handed.
#[derive(Default)]
struct Recorder(Mutex<Vec<FileWriteAttribution>>);

impl FileWriteAttributions for Recorder {
    fn attribute(&self, attribution: FileWriteAttribution) -> Result<(), String> {
        self.0.lock().unwrap().push(attribution);
        Ok(())
    }
}

/// A live invocation of connection `peer` for `actor`, as the broker
/// admitted it: retained `owner`, and the invoker it stamped (`invoker`).
fn pending(
    peer: u64,
    cluster_id: &str,
    owner: InvocationOwner,
    actor: &str,
    invoker: Option<tilo_actions::ActionInvoker>,
) -> (tilo_actions::InvocationId, PendingInvocation) {
    let (tx, _rx) = std::sync::mpsc::channel();
    let invocation = tilo_actions::InvocationId::new();
    let mut context = tilo_actions::ActionContext::for_resource("files", "files", "Files");
    context.invoker = invoker;
    (
        invocation,
        PendingInvocation {
            caller: None,
            activity: None,
            owner,
            network_evidence: None,
            original: None,
            policy_time: None,
            journal_key: None,
            peer_id: peer,
            tx,
            update: tilo_actions::InvocationUpdate::Accepted { invocation },
            prepared_effect: Arc::default(),
            last_sequence: 0,
            actor: actor.into(),
            cancellable: false,
            retryable_on_disconnect: false,
            execution: tilo_actions::ExecutionMode::Immediate,
            result: tilo_actions::ResultSpec::None,
            operation_kind: tilo_actions::OperationKind::Effect,
            cluster_id: cluster_id.into(),
            context,
            last_activity: std::time::Instant::now(),
            cancellation_requested_at: None,
            cancellation_fence: Arc::new(AtomicBool::new(false)),
        },
    )
}

fn agent(actor: &str) -> Option<tilo_actions::ActionInvoker> {
    Some(tilo_actions::ActionInvoker::actor(actor).with_kind(tilo_actions::ActorKind::Agent))
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one fixture keeps every custody case beside the one it differs from"
)]
fn writes_are_attributed_only_to_an_agent_its_invocations_custody_names() {
    let owner_key = ed25519_dalek::SigningKey::from_bytes(&[81; 32]);
    let mut workspace = Workspace::new(WorkspaceId(81), "Files");
    let room = workspace.room.to_string();
    workspace.set_owner(principal_id_for_root(owner_key.verifying_key().to_bytes()));
    let mut tree = WorkspaceTree::default();
    tree.workspaces.push(workspace);
    let route = CrdtRoute::record("so.tilo.file-explorer", &room, "main");
    let admitted = admit_verified_join(
        &tree,
        &route,
        VerifiedJoinIdentity {
            user_public_key: owner_key.verifying_key().to_bytes(),
            device_public_key: [82; 32],
            capped: false,
        },
    )
    .expect("the owner's room admits its person");
    let recorder = Arc::new(Recorder::default());
    let (state, bridge) = CapnpBrokerState::new_configured(
        &Arc::new(FanoutState::new()),
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::disabled(),
        Some(tree.shared().authority()),
        None,
        None,
        HostServices {
            file_write_attributions: Some(recorder.clone()),
            ..HostServices::default()
        },
    );
    let files = CapnpChannelHandle {
        committed_read: Rc::new(std::cell::RefCell::new(committed_document::Slot::default())),
        state: state.clone(),
        directories: Rc::new(LocalActionDirectoryRegistry::new(bridge)),
        channel_id: route.channel_key(),
        peer_id: 7,
        verified_identity: admitted,
        record_kind: None,
        lease: None,
    };
    let mut job = 0;
    let mut admit = |(invocation, pending): (tilo_actions::InvocationId, PendingInvocation)| {
        job += 1;
        state
            .pending_invocations
            .lock()
            .unwrap()
            .insert(job, pending);
        invocation
    };
    let local = || InvocationOwner::Local {
        actor: "agent:scribe".into(),
    };
    let paths = || vec!["/var/workspace/Reports".to_string()];

    // A broker-leased agent run on this desktop: the invoker the broker
    // stamped names the agent the invocation was admitted for.
    let scribe = admit(pending(
        7,
        &room,
        local(),
        "agent:scribe",
        agent("agent:scribe"),
    ));
    assert_eq!(
        files.attribute_writes_for_invocation(paths(), Some(scribe)),
        Ok(true)
    );
    let (red, green, blue) = ui::presence::user_color("agent:scribe");
    assert_eq!(
        *recorder.0.lock().unwrap(),
        [FileWriteAttribution {
            paths: paths(),
            agent_id: "scribe".into(),
            label: "scribe".into(),
            color: [red, green, blue],
        }]
    );

    // An agent on a connection the broker admitted itself, by the caller it
    // retained.
    let (invocation, mut retained) = pending(
        7,
        &room,
        InvocationOwner::AuthenticatedLocal {
            root: owner_key.verifying_key().to_bytes(),
            device: [82; 32],
            agent: Some("courier".into()),
        },
        "agent:courier",
        None,
    );
    retained.caller = Some(ContinuingCaller::Caller(
        tilo_actions::remote::Caller::local_agent("courier"),
    ));
    let courier = admit((invocation, retained));
    assert_eq!(
        files.attribute_writes_for_invocation(paths(), Some(courier)),
        Ok(true)
    );
    assert_eq!(
        recorder.0.lock().unwrap().last().unwrap().agent_id,
        "courier"
    );

    // Nothing else is attributed, whatever the context claims.
    let before = recorder.0.lock().unwrap().len();
    let person = admit(pending(
        7,
        &room,
        InvocationOwner::Local {
            actor: "ada".into(),
        },
        "ada",
        Some(tilo_actions::ActionInvoker::actor("ada")),
    ));
    assert_eq!(
        files.attribute_writes_for_invocation(paths(), Some(person)),
        Ok(false)
    );
    let claimed = admit(pending(7, &room, local(), "ada", agent("agent:scribe")));
    assert_eq!(
        files.attribute_writes_for_invocation(paths(), Some(claimed)),
        Ok(false),
        "an invoker that is not the admitted actor names nobody"
    );
    let peer_device = admit(pending(
        7,
        &room,
        InvocationOwner::Certified {
            root: [9; 32],
            device: [10; 32],
        },
        "agent:scribe",
        agent("agent:scribe"),
    ));
    assert_eq!(
        files.attribute_writes_for_invocation(paths(), Some(peer_device)),
        Ok(false),
        "a peer's device does not write through this person's session"
    );
    assert_eq!(recorder.0.lock().unwrap().len(), before);

    // Only this connection's own live invocation, named exactly.
    let theirs = admit(pending(
        8,
        &room,
        local(),
        "agent:scribe",
        agent("agent:scribe"),
    ));
    assert!(
        files
            .attribute_writes_for_invocation(paths(), Some(theirs))
            .is_err()
    );
    assert!(
        files
            .attribute_writes_for_invocation(paths(), Some(tilo_actions::InvocationId::new()))
            .is_err()
    );
    assert!(
        files
            .attribute_writes_for_invocation(paths(), None)
            .is_err()
    );
    assert!(
        files
            .attribute_writes_for_invocation(Vec::new(), Some(scribe))
            .is_err()
    );
    assert_eq!(recorder.0.lock().unwrap().len(), before);
}
