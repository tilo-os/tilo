//! Which device keeps an app's long-lived connection ([`Turn::Connection`]): two devices of one
//! workspace, each running the trigger runner that takes connection turns, with an app host that
//! records the process it starts and stops.
#![allow(
    clippy::large_futures,
    clippy::significant_drop_tightening,
    reason = "a fixture failing is its test failing, and each test keeps both devices to the end"
)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ed25519_dalek::SigningKey;
use tilo_actions::ActionOutput;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, InvocationReceipt, InvokeRequest, Pending,
    ReceiptStatus,
};
use tilo_automation::{AutomationInvocationId, InvocationReport};
use tilo_user_identity::{DeviceCapabilities, DeviceCert};

use super::super::Workspaces;
use super::super::fixtures::{TestHost, device_cert};
use super::tests::{member_service, shared_with};
use crate::apps::collab_app_broker::AppBrokerState;
use crate::apps::services::ConnectionActions;
use crate::apps::triggers::{Firing, Runner, StartRun};
use crate::work::{
    ConnectionCall, HostConnections, Ineligible, NotTaken, Order, Turn, connection_call,
    connection_holder, takes_turn,
};
use crate::workspaces::providers::Reach;

const BRIDGE: &str = "so.tilo.bridge";
const NEEDS: [&str; 2] = ["network.http", "secrets"];

/// The order both devices keep here: no device waits for another, so each look decides.
const AT_ONCE: Order = Order {
    server_grace: Duration::ZERO,
    other_grace: Duration::ZERO,
};

/// What a device has for the bridge, and the bridge processes it runs.
#[derive(Default)]
struct Device {
    has: Mutex<Has>,
    running: Mutex<BTreeSet<(String, String)>>,
    starts: Mutex<u32>,
}

#[derive(Clone, Copy, Default)]
struct Has {
    installed: bool,
    capabilities: bool,
    power: bool,
}

impl Has {
    const ALL: Self = Self {
        installed: true,
        capabilities: true,
        power: true,
    };
}

impl Device {
    fn with(has: Has) -> Arc<Self> {
        let device = Arc::new(Self::default());
        {
            let mut state = locked(&device.has);
            *state = has;
        }
        device
    }

    fn runs_bridge(&self) -> bool {
        !locked(&self.running).is_empty()
    }
}

/// What the mutex `held` keeps, for a fixture whose state no thread panicked on.
fn locked<T>(held: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    let Ok(kept) = held.lock() else {
        unreachable!("a fixture's state is not poisoned")
    };
    kept
}

impl HostConnections for Device {
    fn connections(&self) -> Vec<(String, Vec<String>)> {
        if locked(&self.has).installed {
            vec![(BRIDGE.to_owned(), NEEDS.map(str::to_owned).to_vec())]
        } else {
            Vec::new()
        }
    }
    fn installed(&self, app: &str) -> bool {
        app == BRIDGE && locked(&self.has).installed
    }
    fn gives(&self, _: &str, _: &str, capability: &str) -> bool {
        NEEDS.contains(&capability) && locked(&self.has).capabilities
    }
    fn on_power(&self) -> bool {
        locked(&self.has).power
    }
    fn start(&self, workspace: &str, app: &str) -> Result<(), String> {
        let mut starts = locked(&self.starts);
        *starts = starts.saturating_add(1);
        drop(starts);
        locked(&self.running).insert((workspace.to_owned(), app.to_owned()));
        Ok(())
    }
    fn runs(&self, workspace: &str, app: &str) -> bool {
        locked(&self.running).contains(&(workspace.to_owned(), app.to_owned()))
    }
    fn stop(&self, workspace: &str, app: &str) {
        locked(&self.running).remove(&(workspace.to_owned(), app.to_owned()));
    }
}

/// No shortcut fires in these tests.
struct NoRuns;

impl StartRun for NoRuns {
    fn start(&self, _: &Firing, _: AutomationInvocationId) -> Result<u64, ActionCallError> {
        Err(ActionCallError::Unavailable {
            reason: "no shortcut runs here".into(),
        })
    }
    fn ended(&self, _: &Firing, _: AutomationInvocationId) -> Option<InvocationReport> {
        None
    }
}

fn runner(workspaces: &Workspaces) -> Runner {
    Runner::new(workspaces.clone(), Arc::new(NoRuns), AT_ONCE)
}

/// The root of the person both devices are of.
fn persons_root() -> SigningKey {
    SigningKey::from_bytes(&[1; 32])
}

/// A desktop with a workspace and a laptop in it, each running the bridge host `hosts` names.
async fn two_devices(
    states: (&std::path::Path, &std::path::Path),
    seed: u8,
    hosts: (&Arc<Device>, &Arc<Device>),
) -> (TestHost, TestHost, String) {
    let laptop = device_cert(&persons_root(), "laptop", DeviceCapabilities::member());
    let (desktop, laptop, created, invitation) =
        shared_with(states.0, "Bridged", seed, laptop).await;
    let laptop = member_service(states.1, laptop).await;
    if laptop.workspaces().join_member(&invitation).await.is_err() {
        unreachable!("the laptop joins the workspace it was invited to");
    }
    desktop
        .workspaces()
        .run_connections(Arc::clone(hosts.0) as Arc<dyn HostConnections>);
    laptop
        .workspaces()
        .run_connections(Arc::clone(hosts.1) as Arc<dyn HostConnections>);
    (desktop, laptop, created.workspace)
}

/// The device the workspace, as `workspaces` reads it, says holds the bridge's lease.
async fn holder(workspaces: &Workspaces, workspace: &str) -> Option<[u8; 32]> {
    let Ok(held) = connection_holder(workspaces, workspace, BRIDGE).await else {
        unreachable!("the bridge's lease can be read")
    };
    held
}

/// Wait until `workspaces` reads `device` holding the bridge's lease.
async fn reads_holder(workspaces: &Workspaces, workspace: &str, device: [u8; 32]) {
    let (workspaces, workspace) = (workspaces.clone(), workspace.to_owned());
    tilo_test_env::within("the claim reaches the other device", async move {
        while holder(&workspaces, &workspace).await != Some(device) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
}

/// The bridge runs on the device that has it installed, gives it what it declares it needs and
/// is on power; a device that lacks any of them says why, and never claims or starts it.
#[tokio::test]
async fn a_connection_runs_only_on_a_device_that_has_what_the_app_needs() {
    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (desktop_has, laptop_has) = (Device::with(Has::default()), Device::with(Has::ALL));
    let (desktop, laptop, workspace) =
        two_devices((one.path(), two.path()), 51, (&desktop_has, &laptop_has)).await;
    let laptop_device = laptop.workspaces().lease_device(&workspace).await;
    let turn = Turn::connection(BRIDGE, NEEDS.map(str::to_owned).to_vec());

    let mut on_desktop = runner(desktop.workspaces());
    on_desktop.pass(Vec::new()).await;
    assert!(
        on_desktop.connections_held().is_empty(),
        "nothing is installed there"
    );
    assert_eq!(
        takes_turn(desktop.workspaces(), &workspace, &turn)
            .await
            .err(),
        Some(NotTaken::Ineligible(Ineligible::NotInstalled))
    );
    desktop_has.has.lock().unwrap().installed = true;
    assert_eq!(
        takes_turn(desktop.workspaces(), &workspace, &turn)
            .await
            .err(),
        Some(NotTaken::Ineligible(Ineligible::LacksCapability))
    );
    desktop_has.has.lock().unwrap().capabilities = true;
    assert_eq!(
        takes_turn(desktop.workspaces(), &workspace, &turn)
            .await
            .err(),
        Some(NotTaken::Ineligible(Ineligible::OnBattery))
    );
    on_desktop.pass(Vec::new()).await;
    assert!(
        on_desktop.connections_held().is_empty() && !desktop_has.runs_bridge(),
        "a device on its battery leaves the connection to another"
    );
    assert_eq!(*desktop_has.starts.lock().unwrap(), 0);
    assert_eq!(
        holder(desktop.workspaces(), &workspace).await,
        None,
        "no claim was written"
    );

    let mut on_laptop = runner(laptop.workspaces());
    on_laptop.pass(Vec::new()).await;
    assert_eq!(
        on_laptop.connections_held(),
        [(workspace.clone(), BRIDGE.to_owned())]
    );
    assert!(
        laptop_has.runs_bridge(),
        "the laptop runs the bridge's process"
    );
    reads_holder(desktop.workspaces(), &workspace, laptop_device).await;

    // Plugged in, the desktop could run it too, but the laptop holds it.
    desktop_has.has.lock().unwrap().power = true;
    on_desktop.pass(Vec::new()).await;
    assert!(on_desktop.connections_held().is_empty() && !desktop_has.runs_bridge());

    // Once the laptop runs on its battery it lets the turn go at its next renewal, and the
    // desktop takes it.
    laptop_has.has.lock().unwrap().power = false;
    let wall = tilo_clock::testing::SharedWall::default();
    wall.follow();
    wall.pin(tilo_clock::wall() + Duration::from_secs(16));
    on_laptop.pass(Vec::new()).await;
    assert!(on_laptop.connections_held().is_empty() && !laptop_has.runs_bridge());
    let desktop_device = desktop.workspaces().lease_device(&workspace).await;
    tilo_test_env::within(
        "the desktop takes the connection the laptop let go",
        async {
            while !desktop_has.runs_bridge() {
                on_desktop.pass(Vec::new()).await;
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        },
    )
    .await;
    wall.release();
    assert_eq!(
        holder(desktop.workspaces(), &workspace).await,
        Some(desktop_device)
    );
    assert!(!laptop_has.runs_bridge(), "never on both");
}

/// Both devices can run the bridge; exactly one holds it. When that device goes silent (asleep,
/// offline), the other takes the connection over once its claim is over, and the first stops its
/// process the moment it looks again, before it could run beside the new holder.
#[tokio::test]
async fn a_silent_holder_is_taken_over_and_stops_its_connection_when_it_resumes() {
    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (desktop_has, laptop_has) = (Device::with(Has::ALL), Device::with(Has::ALL));
    let (desktop, laptop, workspace) =
        two_devices((one.path(), two.path()), 52, (&desktop_has, &laptop_has)).await;
    let desktop_device = desktop.workspaces().lease_device(&workspace).await;
    let laptop_device = laptop.workspaces().lease_device(&workspace).await;
    let (mut on_desktop, mut on_laptop) =
        (runner(desktop.workspaces()), runner(laptop.workspaces()));

    on_desktop.pass(Vec::new()).await;
    assert!(
        desktop_has.runs_bridge(),
        "the first device to look takes it"
    );
    reads_holder(laptop.workspaces(), &workspace, desktop_device).await;
    for _ in 0..3 {
        on_laptop.pass(Vec::new()).await;
        on_desktop.pass(Vec::new()).await;
        assert!(on_laptop.connections_held().is_empty() && !laptop_has.runs_bridge());
        assert!(desktop_has.runs_bridge(), "the holder keeps it");
    }

    // The desktop goes silent: it looks no more, and its process is left as it was. A minute
    // after its last renewal, its claim is over.
    let wall = tilo_clock::testing::SharedWall::default();
    wall.follow();
    let silent = tilo_clock::wall()
        + Duration::from_nanos(tilo_peer_state::CLOCK_SKEW_NS)
        + Duration::from_secs(1);
    wall.pin(silent);
    on_laptop.pass(Vec::new()).await;
    assert_eq!(
        on_laptop.connections_held(),
        [(workspace.clone(), BRIDGE.to_owned())]
    );
    assert!(
        laptop_has.runs_bridge(),
        "the laptop took the connection over"
    );
    reads_holder(desktop.workspaces(), &workspace, laptop_device).await;

    // The desktop wakes: before anything else, it stops the connection it no longer holds, and
    // looking again it finds the laptop holding it.
    on_desktop.pass(Vec::new()).await;
    assert!(
        !desktop_has.runs_bridge(),
        "a device that resumes stops the connection"
    );
    for _ in 0..3 {
        on_desktop.pass(Vec::new()).await;
        on_laptop.pass(Vec::new()).await;
        assert!(on_desktop.connections_held().is_empty() && !desktop_has.runs_bridge());
        assert!(laptop_has.runs_bridge());
    }
    wall.release();
    assert_eq!(
        holder(desktop.workspaces(), &workspace).await,
        Some(laptop_device)
    );
    assert_eq!(
        *desktop_has.starts.lock().unwrap(),
        1,
        "the desktop started it once"
    );
}

/// A call to the bridge's actions runs where its connection runs: on the device that has the
/// lease, which the other device sends it to. Before any device has it, the call is told why, and
/// no device starts the bridge for it.
#[tokio::test]
async fn a_call_to_a_connection_goes_to_the_device_that_has_its_lease() {
    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let desktop_has = Device::with(Has {
        power: false,
        ..Has::ALL
    });
    let laptop_has = Device::with(Has::ALL);
    let (desktop, laptop, workspace) =
        two_devices((one.path(), two.path()), 53, (&desktop_has, &laptop_has)).await;
    let laptop_device = laptop.workspaces().lease_device(&workspace).await;

    let unplaced = |call: ConnectionCall| match call {
        ConnectionCall::Unplaced(why) => why,
        other => panic!("no device has the lease yet, got {other:?}"),
    };
    let why = unplaced(connection_call(desktop.workspaces(), &workspace, BRIDGE).await);
    assert!(why.contains(&Ineligible::OnBattery.to_string()), "{why}");
    let why = unplaced(connection_call(laptop.workspaces(), &workspace, BRIDGE).await);
    assert!(why.contains("takes it shortly"), "{why}");
    assert_eq!(
        (
            *desktop_has.starts.lock().unwrap(),
            *laptop_has.starts.lock().unwrap()
        ),
        (0, 0),
        "a call starts no connection"
    );
    assert_eq!(
        connection_call(desktop.workspaces(), &workspace, "so.tilo.notes").await,
        ConnectionCall::Here,
        "an app that keeps no connection is called as any app"
    );

    runner(laptop.workspaces()).pass(Vec::new()).await;
    reads_holder(desktop.workspaces(), &workspace, laptop_device).await;
    assert_eq!(
        connection_call(desktop.workspaces(), &workspace, BRIDGE).await,
        ConnectionCall::On(laptop_device)
    );
    assert_eq!(
        connection_call(laptop.workspaces(), &workspace, BRIDGE).await,
        ConnectionCall::Here
    );
    assert!(!desktop_has.runs_bridge(), "never on both");
}

/// Where this device's app calls send a call to an app that keeps a connection: the device that
/// has the lease, reached over the link it has to it.
struct Leased(Workspaces);

/// The actions of `device`, as `workspaces`' device reaches it: over the link `device` keeps open
/// to it, or dialled by its key.
fn reach(
    workspaces: &Workspaces,
    device: [u8; 32],
    workspace: &str,
) -> tilo_peer_state::actions::RemoteDevice {
    let Ok(endpoint) = iroh_base::EndpointId::from_bytes(&device) else {
        unreachable!("a device's key is an endpoint id")
    };
    let service = workspaces.action_service();
    service.remote_held(device, workspace).unwrap_or_else(|| {
        service.remote(
            &workspaces.inner.host.endpoint,
            &iroh_base::EndpointAddr::new(endpoint),
            workspace,
        )
    })
}

impl crate::apps::services::HostedWorkspaces for Leased {
    fn host_actions(&self, _: &str) -> Option<Arc<dyn ActionProvider + Send + Sync>> {
        None
    }

    fn connection_actions<'a>(
        &'a self,
        workspace: &'a str,
        app: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ConnectionActions> + Send + 'a>> {
        Box::pin(async move {
            match connection_call(&self.0, workspace, app).await {
                ConnectionCall::Here => ConnectionActions::Here,
                ConnectionCall::Unplaced(why) => ConnectionActions::Unplaced(why),
                ConnectionCall::On(device) => {
                    ConnectionActions::On(Arc::new(reach(&self.0, device, workspace)))
                }
            }
        })
    }

    fn reach(
        &self,
        workspace: &str,
        device: [u8; 32],
    ) -> Option<tilo_peer_state::actions::RemoteDevice> {
        Some(reach(&self.0, device, workspace))
    }
}

/// The bridge's process on a device, serving its channel's actions: it records who each call
/// ran as.
#[derive(Default)]
struct BridgeProcess {
    ran: Mutex<Vec<(Caller, InvokeRequest)>>,
    /// Whether a call it takes runs on until the test ends it, and the calls it keeps running.
    keep_open: std::sync::atomic::AtomicBool,
    running: Mutex<Vec<(tilo_actions::InvocationId, Completer)>>,
}

type Completer = tilo_actions::remote::Completer<Result<InvocationReceipt, ActionCallError>>;

impl BridgeProcess {
    /// End every call it keeps running.
    fn end_running(&self) {
        for (invocation, done) in locked(&self.running).drain(..) {
            done.complete(Ok(InvocationReceipt {
                invocation,
                status: ReceiptStatus::Completed {
                    output: ActionOutput::default(),
                },
            }));
        }
    }
}

/// The bridge's channel, as its process publishes it.
const CHANNEL: &str = "channel";

impl ActionProvider for BridgeProcess {
    fn published(&self) -> Vec<tilo_actions::ResourceSpec> {
        let Ok(send) = tilo_actions::ActionSpec::operation(
            "send",
            "Send",
            tilo_actions::OperationKind::Effect,
            "test.bridge-send",
            &(),
        ) else {
            unreachable!("the bridge's send declares its operation")
        };
        vec![
            tilo_actions::ResourceSpec::new(CHANNEL, "bridge.channel", "Channel").with_action(send),
        ]
    }

    fn query(
        &self,
        _: &Caller,
        _: tilo_actions::remote::CatalogueQuery,
    ) -> Pending<Result<tilo_actions::remote::CataloguePage, ActionCallError>> {
        Pending::ready(Ok(tilo_actions::remote::CataloguePage::default()))
    }

    fn invocation(
        &self,
        address: &tilo_actions::ActionAddress,
        _: Option<&str>,
    ) -> Option<tilo_actions::ActionDescriptor> {
        (address.resource == CHANNEL && address.action == "send").then(|| {
            tilo_actions::ActionDescriptor {
                did: None,
                state: tilo_actions::ActionState::default(),
                semantic_identity: None,
                address: address.clone(),
                resource_kind: "bridge.channel".into(),
                resource_label: "Channel".into(),
                action_label: "Send".into(),
                keywords: Vec::new(),
                icon: None,
                description: None,
                locus: tilo_actions::ExecutionLocus::default(),
                kind: tilo_actions::OperationKind::Effect,
                contract: tilo_actions::ActionContract::for_operation(
                    tilo_actions::OperationKind::Effect,
                ),
                input: tilo_actions::InputSpec::None,
                params: Vec::new(),
                view: None,
            }
        })
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let invocation = request.invocation;
        locked(&self.ran).push((caller.clone(), request));
        if self.keep_open.load(std::sync::atomic::Ordering::SeqCst) {
            let (done, answer) = tilo_actions::remote::pending();
            locked(&self.running).push((invocation, done));
            return answer;
        }
        Pending::ready(Ok(InvocationReceipt {
            invocation,
            status: ReceiptStatus::Completed {
                output: ActionOutput::default(),
            },
        }))
    }

    fn observe(
        &self,
        _: &Caller,
        _: &tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::InvocationState> {
        None
    }

    fn cancel(
        &self,
        _: &Caller,
        _: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownInvocation))
    }
}

/// `workspaces`' device's app calls, admitting by a workspace that lets the agent `scribe` act,
/// answering the bridge's calls from every device of the workspace and sending one to the
/// device that has the bridge's lease.
fn app_calls(
    workspaces: &Workspaces,
    workspace: &str,
    journal: &std::path::Path,
) -> Arc<AppBrokerState> {
    use crate::apps::test_support::Grants as _;
    let Some(workspace_id) = crate::apps::test_support::WorkspaceId::parse(workspace) else {
        unreachable!("the workspace the devices share parses as an id")
    };
    let mut tree_workspace = crate::apps::test_support::Workspace::new(workspace_id, "Bridged");
    tree_workspace
        .policy
        .set_owner(hex::encode(persons_root().verifying_key().to_bytes()));
    tree_workspace.allow_agent("scribe", crate::apps::test_support::KindSet::ALL);
    // Sending on the bridge reaches outside the workspace: the agent does it unasked only where
    // its person allowed that one action, as "Always allow this" does.
    tree_workspace
        .policy
        .grant(tilo_action_policy::agent_answers::grant(
            workspace,
            (
                "scribe",
                &hex::encode(persons_root().verifying_key().to_bytes()),
            ),
            (
                &tilo_action_policy::agent_answers::AnswerScope::Action {
                    app: BRIDGE.into(),
                    kind: "bridge.channel".into(),
                    action: "send".into(),
                },
                tilo_action_policy::ActionKind::Outside,
            ),
            tilo_action_policy::Answer::Allow,
            tilo_action_policy::Grantee::person(hex::encode(
                persons_root().verifying_key().to_bytes(),
            )),
        ));
    let mut tree = crate::apps::test_support::WorkspaceTree::default();
    tree.workspaces.push(tree_workspace);
    let membership = tree.shared();
    let state = AppBrokerState::test_state_only(
        Some(membership.authority()),
        crate::apps::services::ProcessServices {
            providers: Some(workspaces.providers()),
            hosted_workspaces: Some(Arc::new(Leased(workspaces.clone()))),
            // A call is admitted with its evidence, as on a device.
            journal_root: Some(journal.to_path_buf()),
            ..membership.services()
        },
    );
    state.serve_invocations(Arc::default());
    state.answer_app_calls(workspace, BRIDGE);
    state
}

/// The agent `scribe` of the person both devices are of, admitted to `workspace` with its own
/// key and let run the bridge's actions there.
async fn admit_scribe(workspaces: &Workspaces, workspace: &str) -> (SigningKey, DeviceCert) {
    let root = persons_root();
    let (key, mut cert) = device_cert(&root, "Agent: Scribe", DeviceCapabilities::nothing());
    if tilo_user_identity::sign_device_cert(&mut cert, &root).is_err() {
        unreachable!("a fixture's agent certificate signs");
    }
    let now = tilo_clock::unix_now_ns();
    let Some(workspace_id) = tilo_server_protocol::WorkspaceId::parse(workspace) else {
        unreachable!("the workspace the devices share parses as an id")
    };
    let Ok(delegation) = tilo_server_protocol::delegation::Delegation::sign(
        &root,
        cert.device_public_key,
        tilo_server_protocol::delegation::Delegate::Agent {
            agent_id: "scribe".into(),
        },
        "Scribe",
        [10, 20, 30],
        tilo_server_protocol::delegation::DelegateScope {
            workspaces: vec![workspace_id],
            apps: Vec::new(),
        },
        now.saturating_sub(1_000_000_000),
        now.saturating_add(3_600_000_000_000),
    ) else {
        unreachable!("a fixture's agent delegation signs")
    };
    {
        let Ok(state) = workspaces.workspace(workspace).await else {
            unreachable!("the workspace the devices share is held")
        };
        let hosted = state.lock().await;
        if hosted
            .peer()
            .governance_mut()
            .admit_agent(cert.clone(), delegation, &key, tilo_clock::wall())
            .is_err()
        {
            unreachable!("the agent is admitted to the workspace");
        }
    }
    let person = hex::encode(root.verifying_key().to_bytes());
    super::super::fixtures::record_grants(
        workspaces,
        workspace,
        &[tilo_action_policy::Grant::new(
            "scribe-bridges",
            tilo_action_policy::Grantee::agent("scribe", person.as_str()),
            tilo_action_policy::TreePath::workspace(workspace).rest(),
            tilo_action_policy::KindSet::ALL,
            tilo_action_policy::Answer::Allow,
            tilo_action_policy::Grantee::person(person),
        )],
    )
    .await;
    (key, cert)
}

/// A call of the bridge's channel's `send` in `workspace`, signed by `signer` when given.
fn send(workspace: &str, signer: Option<(&DeviceCert, &SigningKey)>) -> InvokeRequest {
    let mut request = InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: tilo_actions::remote::RemoteActionTarget {
            provider: tilo_actions::remote::application_provider_token(workspace, BRIDGE),
            address: tilo_actions::ActionAddress::new(CHANNEL, "send"),
        },
        argument: tilo_actions::remote::InvokeArgument::default(),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    if let Some((cert, key)) = signer {
        let Ok(attached) = tilo_server_protocol::invocation::sign_invocation_now(
            workspace,
            &request,
            cert,
            key,
            tilo_clock::unix_now_ns(),
        ) else {
            unreachable!("a fixture's invocation signs")
        };
        request.signed = Some(Box::new(attached));
    }
    request
}

/// How `request` ends when `provider` is asked it as `caller`: its receipt once settled.
async fn ends(
    provider: &(impl ActionProvider + Sync + ?Sized),
    caller: &Caller,
    request: InvokeRequest,
) -> Result<ReceiptStatus, ActionCallError> {
    let invocation = request.invocation;
    let receipt = provider
        .invoke(caller, request)
        .await
        .unwrap_or(Err(ActionCallError::Unanswered))?;
    let mut status = receipt.status;
    tilo_test_env::within("the call settles", async {
        while matches!(status, ReceiptStatus::Accepted) {
            tokio::time::sleep(Duration::from_millis(20)).await;
            if let Some(state) = provider.observe(caller, &invocation) {
                status = state.receipt().status;
            }
        }
    })
    .await;
    Ok(status)
}

/// An agent's call to the bridge's channel, made on the device without the bridge's lease, runs
/// on the device with it, as the agent, with the device that brought it recorded: both devices'
/// devices decide it, and only the one with the lease runs the bridge. A call naming the agent
/// without its signature never leaves the first device, nor runs when sent on anyway; once the
/// lease moves on, the device that lost it refuses a call brought to it.
#[tokio::test]
async fn an_agents_call_runs_where_the_connection_is_as_the_agent() {
    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    // The host identity a device signs the evidence of each call it admits with.
    let mut identity = tilo_test_env::ScopedEnv::new();
    identity
        .set("XDG_CONFIG_HOME", one.path().join("config"))
        .set("TILO_IDENTITY_ROOT", one.path().join("identity"))
        .set(tilo_identity::provisioning::FILE_SECRETS_ENV, "1");
    let _taken_key = tilo_identity::provisioning::provisioned_as_its_daemon(
        &tilo_identity::store::account_identity_dir(&one.path().join("config/tilo/identity")),
        &tilo_device_identity::IdentityPaths::under(one.path().join("identity")),
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    // The person's devices, as the device that keeps their record answers: this one is listed.
    tilo_identity::sources::test_trust::install();
    let (desktop_has, laptop_has) = (Device::with(Has::ALL), Device::with(Has::ALL));
    let (desktop, laptop, workspace) =
        two_devices((one.path(), two.path()), 54, (&desktop_has, &laptop_has)).await;
    let desktop_device = desktop.workspaces().lease_device(&workspace).await;
    let laptop_device = laptop.workspaces().lease_device(&workspace).await;
    let (mut on_desktop, mut on_laptop) =
        (runner(desktop.workspaces()), runner(laptop.workspaces()));
    let (scribe_key, scribe) = admit_scribe(desktop.workspaces(), &workspace).await;
    // The laptop's app calls answer the calls brought to it while the test runs.
    let (on_desktop_calls, _on_laptop_calls) = (
        app_calls(
            desktop.workspaces(),
            &workspace,
            &one.path().join("invocations"),
        ),
        app_calls(
            laptop.workspaces(),
            &workspace,
            &two.path().join("invocations"),
        ),
    );
    let (desktop_bridge, laptop_bridge) = (
        Arc::new(BridgeProcess::default()),
        Arc::new(BridgeProcess::default()),
    );
    serve_bridges((&desktop, &laptop), (&desktop_bridge, &laptop_bridge));

    // The desktop has the lease first, then lets it go to the laptop.
    on_desktop.pass(Vec::new()).await;
    reads_holder(laptop.workspaces(), &workspace, desktop_device).await;
    desktop_has.has.lock().unwrap().power = false;
    let wall = tilo_clock::testing::SharedWall::default();
    wall.follow();
    wall.pin(tilo_clock::wall() + Duration::from_secs(16));
    on_desktop.pass(Vec::new()).await;
    tilo_test_env::within("the laptop takes the connection", async {
        while !laptop_has.runs_bridge() {
            on_laptop.pass(Vec::new()).await;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    wall.release();
    reads_holder(desktop.workspaces(), &workspace, laptop_device).await;

    // The agent asks the desktop; its call runs on the laptop.
    let agent = Caller::delegated_agent(
        "scribe",
        persons_root().verifying_key().to_bytes(),
        scribe.device_public_key,
    )
    .in_workspace(workspace.clone());
    let pair = Pair {
        desktop: &desktop,
        laptop: &laptop,
        workspace: &workspace,
        desktop_device,
        laptop_device,
    };
    the_agents_call_runs_on_the_laptops_bridge(
        &pair,
        &on_desktop_calls,
        (&desktop_bridge, &laptop_bridge),
        (&agent, (&scribe, &scribe_key)),
    )
    .await;
    a_call_without_its_signature_runs_nowhere(&pair, &on_desktop_calls, &laptop_bridge, &agent)
        .await;
    a_call_to_the_device_that_lost_the_lease_is_refused(
        &pair,
        (&scribe, &scribe_key),
        (&desktop_bridge, &desktop_has),
    )
    .await;
    the_bridges_own_call_runs_for_the_agent_and_ends(
        &pair,
        &on_desktop_calls,
        &laptop_bridge,
        (&agent, (&scribe, &scribe_key)),
    )
    .await;
}

/// Serve the bridge's process on each device, answering the calls its channel's `send` brings.
fn serve_bridges(
    (desktop, laptop): (&TestHost, &TestHost),
    (desktop_bridge, laptop_bridge): (&Arc<BridgeProcess>, &Arc<BridgeProcess>),
) {
    for (workspaces, bridge) in [
        (desktop.workspaces(), desktop_bridge),
        (laptop.workspaces(), laptop_bridge),
    ] {
        if workspaces
            .providers()
            .serve(
                "test.bridge-process",
                BRIDGE,
                Reach::Devices,
                Arc::clone(bridge) as Arc<dyn ActionProvider + Send + Sync>,
            )
            .is_err()
        {
            unreachable!("both devices serve the bridge's process");
        }
    }
}

/// The two devices an agent's call crosses, as the phases of its test keep them.
struct Pair<'a> {
    desktop: &'a TestHost,
    laptop: &'a TestHost,
    workspace: &'a str,
    desktop_device: [u8; 32],
    laptop_device: [u8; 32],
}

/// The agent's call, asked of the desktop, runs on the laptop that holds the connection: as the
/// agent, brought by the desktop, its signature with it.
async fn the_agents_call_runs_on_the_laptops_bridge(
    pair: &Pair<'_>,
    on_desktop_calls: &Arc<AppBrokerState>,
    (desktop_bridge, laptop_bridge): (&Arc<BridgeProcess>, &Arc<BridgeProcess>),
    (agent, (scribe, scribe_key)): (&Caller, (&DeviceCert, &SigningKey)),
) {
    let ran = tilo_test_env::within(
        "the agent's call runs once its grant reached the laptop",
        async {
            loop {
                let call = send(pair.workspace, Some((scribe, scribe_key)));
                match ends(&AppCallsOf(Arc::clone(on_desktop_calls)), agent, call).await {
                    Ok(ReceiptStatus::Completed { .. }) => break,
                    Err(ActionCallError::NotPermitted) => {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    other => unreachable!("the agent's call runs on the laptop: {other:?}"),
                }
            }
            let Some(ran) = locked(&laptop_bridge.ran).pop() else {
                unreachable!("the laptop's bridge ran the agent's call")
            };
            ran
        },
    )
    .await;
    assert!(
        locked(&desktop_bridge.ran).is_empty(),
        "never where the lease is not"
    );
    let (ran_as, request) = ran;
    assert_eq!(
        ran_as.acting().member_id(),
        Some(&tilo_action_policy::MemberId::agent(
            "scribe",
            hex::encode(persons_root().verifying_key().to_bytes())
        )),
        "the call runs as the agent"
    );
    assert_eq!(ran_as.device_public_key(), Some(&scribe.device_public_key));
    assert_eq!(ran_as.forwarder(), Some(&pair.desktop_device));
    assert!(
        request.signed.is_some(),
        "the agent's signature travelled with it"
    );
}

/// Its call without its signature never leaves the desktop, nor runs on the laptop when the
/// desktop sends it on anyway.
async fn a_call_without_its_signature_runs_nowhere(
    pair: &Pair<'_>,
    on_desktop_calls: &Arc<AppBrokerState>,
    laptop_bridge: &Arc<BridgeProcess>,
    agent: &Caller,
) {
    let ran_before = locked(&laptop_bridge.ran).len();
    let unsigned = ends(
        &AppCallsOf(Arc::clone(on_desktop_calls)),
        agent,
        send(pair.workspace, None),
    )
    .await;
    assert!(
        matches!(&unsigned, Err(ActionCallError::Refused { reason }) if reason.contains("no signature")),
        "{unsigned:?}"
    );
    let to_laptop = reach(
        pair.desktop.workspaces(),
        pair.laptop_device,
        pair.workspace,
    );
    let mut naming_the_agent = send(pair.workspace, None);
    naming_the_agent.context = Some(tilo_actions::ActionContext {
        invoker: Some(tilo_actions::ActionInvoker::admitted(agent.acting())),
        ..tilo_actions::ActionContext::default()
    });
    let sent_on = ends(&to_laptop, agent, naming_the_agent).await;
    assert!(
        matches!(&sent_on, Err(ActionCallError::Refused { reason }) if reason.contains("no signature")),
        "{sent_on:?}"
    );
    assert_eq!(locked(&laptop_bridge.ran).len(), ran_before, "neither ran");
}

/// The desktop no longer has the lease: a call the laptop brings it is refused there.
async fn a_call_to_the_device_that_lost_the_lease_is_refused(
    pair: &Pair<'_>,
    (scribe, scribe_key): (&DeviceCert, &SigningKey),
    (desktop_bridge, desktop_has): (&Arc<BridgeProcess>, &Arc<Device>),
) {
    let to_desktop = reach(
        pair.laptop.workspaces(),
        pair.desktop_device,
        pair.workspace,
    );
    let laptop_caller = Caller::authenticated(
        persons_root().verifying_key().to_bytes(),
        pair.laptop_device,
    )
    .in_workspace(pair.workspace.to_owned());
    let lost = ends(
        &to_desktop,
        &laptop_caller,
        send(pair.workspace, Some((scribe, scribe_key))),
    )
    .await;
    assert!(
        matches!(&lost, Err(ActionCallError::Refused { reason }) if reason.contains("no longer runs")),
        "{lost:?}"
    );
    assert!(locked(&desktop_bridge.ran).is_empty());
    assert!(!desktop_has.runs_bridge(), "never on both");
}

/// The bridge, running the agent's call, makes a call of its own for it to what the desktop keeps
/// of the bridge: it runs there as the agent while the agent's call runs, and proves nothing once
/// that call ended.
async fn the_bridges_own_call_runs_for_the_agent_and_ends(
    pair: &Pair<'_>,
    on_desktop_calls: &Arc<AppBrokerState>,
    laptop_bridge: &Arc<BridgeProcess>,
    (agent, (scribe, scribe_key)): (&Caller, (&DeviceCert, &SigningKey)),
) {
    let records = Arc::new(BridgeProcess::default());
    if pair
        .desktop
        .workspaces()
        .providers()
        .serve(
            "test.bridge-records",
            BRIDGE,
            Reach::Devices,
            Arc::clone(&records) as Arc<dyn ActionProvider + Send + Sync>,
        )
        .is_err()
    {
        unreachable!("the desktop serves what the bridge keeps");
    }
    laptop_bridge
        .keep_open
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let original = send(pair.workspace, Some((scribe, scribe_key)));
    let running = {
        let (to, agent, original) = (
            AppCallsOf(Arc::clone(on_desktop_calls)),
            agent.clone(),
            original.clone(),
        );
        tokio::spawn(async move { ends(&to, &agent, original).await })
    };
    tilo_test_env::within("the bridge runs the agent's call", async {
        while locked(&laptop_bridge.running).is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let to_desktop = reach(
        pair.laptop.workspaces(),
        pair.desktop_device,
        pair.workspace,
    );
    let laptop_caller = Caller::authenticated(
        persons_root().verifying_key().to_bytes(),
        pair.laptop_device,
    )
    .in_workspace(pair.workspace.to_owned());
    let for_it = || InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: tilo_actions::remote::RemoteActionTarget {
            provider: "test.bridge-records".into(),
            address: tilo_actions::ActionAddress::new(CHANNEL, "send"),
        },
        on_behalf_of: Some(original.invocation),
        signed: original.signed.clone(),
        ..send(pair.workspace, None)
    };
    let served = ends(&to_desktop, &laptop_caller, for_it()).await;
    assert!(
        matches!(served, Ok(ReceiptStatus::Completed { .. })),
        "the bridge's call for the agent runs: {served:?}"
    );
    let Some((ran_as, _)) = locked(&records.ran).pop() else {
        unreachable!("the desktop's records ran the bridge's call")
    };
    assert_eq!(
        ran_as.acting().member_id(),
        Some(&tilo_action_policy::MemberId::agent(
            "scribe",
            hex::encode(persons_root().verifying_key().to_bytes())
        )),
        "the bridge's call runs as the agent it serves, never as the laptop"
    );
    assert_eq!(ran_as.forwarder(), Some(&pair.laptop_device));

    // The bridge finishes the agent's call: the desktop hands the agent what became of it, and
    // a call carrying it is refused from then on.
    laptop_bridge.end_running();
    let Ok(ended) = running.await else {
        unreachable!("the agent's call runs on a task that was not cancelled")
    };
    assert!(
        matches!(ended, Ok(ReceiptStatus::Completed { .. })),
        "the agent's call settles where it was asked: {ended:?}"
    );
    let after = ends(&to_desktop, &laptop_caller, for_it()).await;
    assert!(
        matches!(&after, Err(ActionCallError::Refused { reason }) if reason.contains("has ended")),
        "an ended invocation proves nothing: {after:?}"
    );
    assert!(locked(&records.ran).is_empty());
}

/// A device's app calls as the device's dispatch hands them to it.
struct AppCallsOf(Arc<AppBrokerState>);

impl ActionProvider for AppCallsOf {
    fn query(
        &self,
        caller: &Caller,
        query: tilo_actions::remote::CatalogueQuery,
    ) -> Pending<Result<tilo_actions::remote::CataloguePage, ActionCallError>> {
        self.0.action_query(caller, &query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        self.0.action_invoke(caller, None, None, &request)
    }

    fn observe(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::InvocationState> {
        self.0.action_observe(caller, id)
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionCallError>> {
        self.0.action_cancel(caller, id)
    }
}
