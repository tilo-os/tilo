use super::super::{BrokerChannels, CapnpBrokerState, FanoutState, PeerPublisher};
use crate::services::{HostStepOutcome, HostStepRequest, HostStepServices, LiveRead, LiveView};
use crate::test_support::{
    Grants as _, KindSet, Subject, TreePath, Workspace, WorkspaceId, WorkspaceTree,
    WorkspaceTreeHandle,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::{
    ActionHostError, Caller, InvokeArgument, InvokeRequest, Pending, RemoteActionTarget,
};

const PROVIDER: &str = "test.live";
const APP: &str = "so.tilo.activity";
const RESOURCE: &str = "counter";

/// A count the test moves, served live.
struct Counter {
    value: Arc<Mutex<u64>>,
    changes: tokio::sync::watch::Sender<u64>,
    reads: Arc<AtomicUsize>,
}

impl Counter {
    fn new() -> Self {
        Self {
            value: Arc::new(Mutex::new(0)),
            changes: tokio::sync::watch::channel(0).0,
            reads: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Set the count, and say the result may have changed.
    fn set(&self, value: u64) {
        *self.value.lock().unwrap() = value;
        self.changed();
    }

    fn changed(&self) {
        self.changes
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }
}

impl HostStepServices for Counter {
    fn provider(&self) -> &'static str {
        PROVIDER
    }

    fn app_id(&self) -> &'static str {
        APP
    }

    fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
        vec![tilo_actions::ActionDescriptor {
            semantic_identity: None,
            address: tilo_actions::ActionAddress::new(RESOURCE, "read"),
            resource_kind: RESOURCE.into(),
            resource_label: "Counter".into(),
            action_label: "Read".into(),
            description: None,
            locus: tilo_actions::ExecutionLocus::default(),
            kind: tilo_actions::OperationKind::View,
            contract: tilo_actions::ActionContract::live_view(
                serde_json::json!({"type": "object"}),
            ),
            input: tilo_actions::InputSpec::None,
            params: Vec::new(),
            view: None,
        }]
    }

    fn start(&self, _: &HostStepRequest<'_>) -> Result<Pending<HostStepOutcome>, String> {
        Err("a live view is followed, not started".into())
    }

    fn watch(&self, _: &HostStepRequest<'_>) -> Result<LiveView, String> {
        let value = Arc::clone(&self.value);
        let reads = Arc::clone(&self.reads);
        Ok(LiveView {
            read: Box::new(move || {
                reads.fetch_add(1, Ordering::SeqCst);
                Pending::ready(
                    Ok(serde_json::json!({ "count": *value.lock().unwrap() })) as LiveRead
                )
            }),
            changes: self.changes.subscribe(),
        })
    }
}

fn broker(
    counter: Arc<Counter>,
    grants: impl FnOnce(&mut Workspace),
) -> (Arc<CapnpBrokerState>, WorkspaceTreeHandle, String, String) {
    let owner = hex::encode([41; 32]);
    let mut workspace = Workspace::new(WorkspaceId(9), "Live");
    workspace.set_owner(owner.clone());
    grants(&mut workspace);
    let room = workspace.room().to_string();
    let mut tree = WorkspaceTree::default();
    tree.workspaces.push(workspace);
    let tree = tree.shared();
    let (state, _bridge_rx) = CapnpBrokerState::new_configured(
        &Arc::new(FanoutState::new()),
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::disabled(),
        Some(tree.authority()),
        None,
        None,
        crate::services::HostServices {
            host_steps: vec![counter],
            ..Default::default()
        },
    );
    (state, tree, owner, room)
}

fn follow() -> InvokeRequest {
    InvokeRequest {
        on_behalf_of: None,
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: PROVIDER.into(),
            address: tilo_actions::ActionAddress::new(RESOURCE, "read"),
        },
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
    }
}

fn invoked(
    state: &Arc<CapnpBrokerState>,
    caller: &Caller,
    request: &InvokeRequest,
) -> Result<tilo_actions::remote::InvocationReceipt, ActionHostError> {
    state
        .action_invoke(caller, None, None, request)
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("admission answers at once")
}

/// The invocation's state once `until` holds of it, waiting on the
/// broker's own change signal.
fn observed(
    state: &Arc<CapnpBrokerState>,
    caller: &Caller,
    request: &InvokeRequest,
    until: impl Fn(&InvocationUpdate) -> bool,
) -> InvocationUpdate {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let mut changes = state.invocation_changes();
    loop {
        if let Some(observed) = state.action_observe(caller, &request.invocation)
            && until(&observed.update)
        {
            return observed.update;
        }
        runtime
            .block_on(async {
                tokio::time::timeout(tilo_test_env::hang_bound(), changes.changed()).await
            })
            .expect("the view moves")
            .unwrap();
    }
}

fn count(update: &InvocationUpdate) -> Option<u64> {
    match update {
        InvocationUpdate::Item { value, .. } => value["count"].as_u64(),
        _ => None,
    }
}

#[test]
fn a_live_view_sends_each_new_result_until_cancelled() {
    let counter = Arc::new(Counter::new());
    let (state, _tree, owner, room) = broker(Arc::clone(&counter), |_| {});
    let person = Caller::local_person(owner).in_room(room);
    let request = follow();
    invoked(&state, &person, &request).expect("the person follows the view");
    let first = observed(&state, &person, &request, |update| count(update) == Some(0));
    assert!(matches!(first, InvocationUpdate::Item { sequence: 1, .. }));

    counter.set(3);
    let moved = observed(&state, &person, &request, |update| count(update) == Some(3));
    assert!(matches!(moved, InvocationUpdate::Item { sequence: 2, .. }));

    // A change that changed nothing is read, and sends nothing new.
    let reads = counter.reads.load(Ordering::SeqCst);
    counter.changed();
    counter.set(4);
    let moved = observed(&state, &person, &request, |update| count(update) == Some(4));
    assert!(matches!(moved, InvocationUpdate::Item { sequence: 3, .. }));
    assert!(counter.reads.load(Ordering::SeqCst) > reads);

    crate::collab_broker_capnp::answered(state.action_cancel(&person, &request.invocation))
        .expect("the follower cancels it");
    observed(&state, &person, &request, |update| {
        matches!(update, InvocationUpdate::Cancelled { .. })
    });
    let reads = counter.reads.load(Ordering::SeqCst);
    counter.set(5);
    assert_eq!(
        counter.reads.load(Ordering::SeqCst),
        reads,
        "a cancelled view is not read again"
    );
}

#[test]
fn a_caller_without_read_permission_is_refused_and_a_withdrawn_grant_ends_the_view() {
    let counter = Arc::new(Counter::new());
    let (state, tree, _owner, room) = broker(Arc::clone(&counter), |workspace| {
        workspace.admit_agent("scribe", KindSet::READ);
    });

    let stranger = Caller::local_agent("intruder").in_room(room.clone());
    assert!(
        invoked(&state, &stranger, &follow()).is_err(),
        "an agent the room never admitted reads nothing"
    );
    assert_eq!(counter.reads.load(Ordering::SeqCst), 0, "nothing was read");

    let agent = Caller::local_agent("scribe").in_room(room.clone());
    let request = follow();
    invoked(&state, &agent, &request).expect("the admitted agent follows the view");
    observed(&state, &agent, &request, |update| count(update) == Some(0));

    tree.write()
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.room().to_string() == room)
        .unwrap()
        .deny(
            Subject::agent("scribe"),
            &TreePath::root().app(APP),
            KindSet::READ,
        );
    let reads = counter.reads.load(Ordering::SeqCst);
    counter.set(7);
    let ended = observed(&state, &agent, &request, InvocationUpdate::is_terminal);
    match ended {
        InvocationUpdate::Failed { error, .. } => assert_eq!(error.code, "not_permitted"),
        other => panic!("the view must end refused, not {other:?}"),
    }
    assert_eq!(
        counter.reads.load(Ordering::SeqCst),
        reads,
        "the withdrawn grant is decided before the read"
    );
}
