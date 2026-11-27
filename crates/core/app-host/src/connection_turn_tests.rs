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
use tilo_action_policy::{Answer, Grant, Grantee, KindSet, TreePath};
use tilo_actions::ActionOutput;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, InvocationReceipt, InvokeRequest, Pending,
    ReceiptStatus,
};
use tilo_automation::{AutomationInvocationId, InvocationReport};
use tilo_user_identity::{DeviceCapabilities, DeviceCert};

use crate::collab_app_broker::AppBrokerState;
use crate::services::ConnectionActions;
use crate::triggers::{Firing, Runner, StartRun};
use tilo_workspace_service::work::{
    ConnectionCall, HostConnections, Ineligible, NotTaken, Order, StartRefusal, Turn,
    connection_call, connection_holder, takes_turn,
};
use tilo_workspace_service::workspaces::Workspaces;
use tilo_workspace_service::workspaces::fixtures::{TestHost, device_cert};
use tilo_workspace_service::workspaces::fixtures::{
    admit_scribe, member_service, persons_root, reach_device as reach, shared_with,
};
use tilo_workspace_service::workspaces::providers::Reach;

const BRIDGE: &str = "so.tilo.bridge";
/// The kind of the bridge's channel record, as its process publishes it.
const CHANNEL_KIND: &str = "bridge.channel";
const NEEDS: [&str; 2] = ["network.http", "secrets"];

/// The order both devices keep here: no device waits for another, so each look decides.
const AT_ONCE: Order = Order {
    server_grace: Duration::ZERO,
    other_grace: Duration::ZERO,
};

/// The person's own second device, as tests that need no second person join it.
fn same_person_laptop() -> (SigningKey, DeviceCert) {
    device_cert(&persons_root(), "laptop", DeviceCapabilities::member())
}

/// What a device has for the app, and the app's processes it runs.
#[derive(Default)]
struct Device {
    app: String,
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
        Self::for_app(BRIDGE, has)
    }

    /// A device that runs `app`'s connection when `has` says what it is able of.
    fn for_app(app: &str, has: Has) -> Arc<Self> {
        let device = Arc::new(Self {
            app: app.to_owned(),
            ..Self::default()
        });
        {
            let mut state = locked(&device.has);
            *state = has;
        }
        device
    }

    fn runs_app(&self) -> bool {
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
            vec![(self.app.clone(), NEEDS.map(str::to_owned).to_vec())]
        } else {
            Vec::new()
        }
    }
    fn installed(&self, app: &str) -> bool {
        app == self.app && locked(&self.has).installed
    }
    fn gives(&self, _: &str, _: &str, capability: &str) -> bool {
        NEEDS.contains(&capability) && locked(&self.has).capabilities
    }
    fn on_power(&self) -> bool {
        locked(&self.has).power
    }
    fn start<'a>(
        &'a self,
        workspace: &'a str,
        app: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), StartRefusal>> + Send + 'a>>
    {
        Box::pin(async move {
            let mut starts = locked(&self.starts);
            *starts = starts.saturating_add(1);
            drop(starts);
            locked(&self.running).insert((workspace.to_owned(), app.to_owned()));
            Ok(())
        })
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

/// A desktop with a workspace and the `laptop` device in it, each running the app host `hosts`
/// names.
async fn two_devices(
    states: (&std::path::Path, &std::path::Path),
    laptop: (SigningKey, DeviceCert),
    seed: u8,
    hosts: (&Arc<Device>, &Arc<Device>),
) -> (TestHost, TestHost, String) {
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

/// The device the workspace, as `workspaces` reads it, says holds `app`'s lease.
async fn holder(workspaces: &Workspaces, workspace: &str, app: &str) -> Option<[u8; 32]> {
    let Ok(held) = connection_holder(workspaces, workspace, app).await else {
        unreachable!("the app's lease can be read")
    };
    held
}

/// Wait until `workspaces` reads `device` holding `app`'s lease.
async fn reads_holder(workspaces: &Workspaces, workspace: &str, device: [u8; 32], app: &str) {
    let (workspaces, workspace, app) = (workspaces.clone(), workspace.to_owned(), app.to_owned());
    tilo_test_env::within("the claim reaches the other device", async move {
        while holder(&workspaces, &workspace, &app).await != Some(device) {
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
    let (desktop, laptop, workspace) = two_devices(
        (one.path(), two.path()),
        same_person_laptop(),
        51,
        (&desktop_has, &laptop_has),
    )
    .await;
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
        on_desktop.connections_held().is_empty() && !desktop_has.runs_app(),
        "a device on its battery leaves the connection to another"
    );
    assert_eq!(*desktop_has.starts.lock().unwrap(), 0);
    assert_eq!(
        holder(desktop.workspaces(), &workspace, BRIDGE).await,
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
        laptop_has.runs_app(),
        "the laptop runs the bridge's process"
    );
    reads_holder(desktop.workspaces(), &workspace, laptop_device, BRIDGE).await;

    // Plugged in, the desktop could run it too, but the laptop holds it.
    desktop_has.has.lock().unwrap().power = true;
    on_desktop.pass(Vec::new()).await;
    assert!(on_desktop.connections_held().is_empty() && !desktop_has.runs_app());

    // Once the laptop runs on its battery it lets the turn go at its next renewal, and the
    // desktop takes it.
    laptop_has.has.lock().unwrap().power = false;
    let wall = tilo_clock::testing::SharedWall::default();
    wall.follow();
    wall.pin(tilo_clock::wall() + Duration::from_secs(16));
    on_laptop.pass(Vec::new()).await;
    assert!(on_laptop.connections_held().is_empty() && !laptop_has.runs_app());
    let desktop_device = desktop.workspaces().lease_device(&workspace).await;
    tilo_test_env::within(
        "the desktop takes the connection the laptop let go",
        async {
            while !desktop_has.runs_app() {
                on_desktop.pass(Vec::new()).await;
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        },
    )
    .await;
    wall.release();
    assert_eq!(
        holder(desktop.workspaces(), &workspace, BRIDGE).await,
        Some(desktop_device)
    );
    assert!(!laptop_has.runs_app(), "never on both");
}

/// Mail keeps its records sealed: the workspace decides which device may read them.
const MAIL: &str = "so.tilo.mail";

/// A device the workspace does not let read the sealed records an app keeps never claims its
/// connection, and starts nothing for it. The device whose person reads them takes the turn.
#[tokio::test]
async fn a_connection_runs_only_on_a_device_that_may_read_the_apps_sealed_records() {
    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    // The laptop belongs to another person, whose role the workspace's grants narrow.
    let laptop = device_cert(
        &SigningKey::from_bytes(&[42; 32]),
        "laptop",
        DeviceCapabilities::member(),
    );
    let (desktop_has, laptop_has) = (
        Device::for_app(MAIL, Has::ALL),
        Device::for_app(MAIL, Has::ALL),
    );
    let (desktop, laptop, workspace) = two_devices(
        (one.path(), two.path()),
        laptop,
        55,
        (&desktop_has, &laptop_has),
    )
    .await;
    let desktop_cert = desktop.workspaces().device_cert().clone();
    let laptop_person = hex::encode(laptop.workspaces().device_cert().user_public_key);
    desktop
        .workspaces()
        .put_workspace_grants(
            &workspace,
            &desktop_cert,
            [55; 32],
            vec![Grant::new(
                "no-mail",
                Grantee::person(laptop_person),
                TreePath::workspace(&workspace).app(MAIL),
                KindSet::READ,
                Answer::Deny,
                Grantee::person(hex::encode(desktop_cert.user_public_key)),
            )],
        )
        .await
        .unwrap();
    // Wait until the laptop reads that its person may not read Mail's records, asking as the
    // device it is, without claiming anything.
    let laptop_device = laptop.workspaces().lease_device(&workspace).await;
    {
        let (workspaces, workspace) = (laptop.workspaces().clone(), workspace.clone());
        tilo_test_env::within(
            "the laptop reads that its person may not read Mail",
            async move {
                loop {
                    let Some(peer) = workspaces.state(&workspace).await else {
                        unreachable!("the laptop holds the workspace it joined")
                    };
                    let now = tilo_clock::wall();
                    let reads = peer.permissions(laptop_device, now).is_ok_and(|access| {
                        collab_types::validator::reads_protected_document(
                            &workspace,
                            MAIL,
                            "",
                            Some(&access),
                            now,
                        )
                    });
                    if !reads {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            },
        )
        .await;
    }

    let turn = Turn::connection(MAIL, NEEDS.map(str::to_owned).to_vec());
    assert_eq!(
        takes_turn(laptop.workspaces(), &workspace, &turn)
            .await
            .err(),
        Some(NotTaken::Ineligible(Ineligible::CannotReadSealed))
    );
    assert_eq!(
        holder(laptop.workspaces(), &workspace, MAIL).await,
        None,
        "a device that may not read the app's records writes no claim"
    );
    let mut on_laptop = runner(laptop.workspaces());
    on_laptop.pass(Vec::new()).await;
    assert!(
        on_laptop.connections_held().is_empty() && !laptop_has.runs_app(),
        "and starts nothing for it"
    );
    assert_eq!(*laptop_has.starts.lock().unwrap(), 0);
    assert_eq!(holder(laptop.workspaces(), &workspace, MAIL).await, None);

    let desktop_device = desktop.workspaces().lease_device(&workspace).await;
    let mut on_desktop = runner(desktop.workspaces());
    on_desktop.pass(Vec::new()).await;
    assert_eq!(
        on_desktop.connections_held(),
        [(workspace.clone(), MAIL.to_owned())]
    );
    assert!(desktop_has.runs_app(), "the desktop runs Mail's connection");
    reads_holder(desktop.workspaces(), &workspace, desktop_device, MAIL).await;
}

/// Both devices can run the bridge; exactly one holds it. When that device goes silent (asleep,
/// offline), the other takes the connection over once its claim is over, and the first stops its
/// process the moment it looks again, before it could run beside the new holder.
#[tokio::test]
async fn a_silent_holder_is_taken_over_and_stops_its_connection_when_it_resumes() {
    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (desktop_has, laptop_has) = (Device::with(Has::ALL), Device::with(Has::ALL));
    let (desktop, laptop, workspace) = two_devices(
        (one.path(), two.path()),
        same_person_laptop(),
        52,
        (&desktop_has, &laptop_has),
    )
    .await;
    let desktop_device = desktop.workspaces().lease_device(&workspace).await;
    let laptop_device = laptop.workspaces().lease_device(&workspace).await;
    let (mut on_desktop, mut on_laptop) =
        (runner(desktop.workspaces()), runner(laptop.workspaces()));

    on_desktop.pass(Vec::new()).await;
    assert!(desktop_has.runs_app(), "the first device to look takes it");
    reads_holder(laptop.workspaces(), &workspace, desktop_device, BRIDGE).await;
    for _ in 0..3 {
        on_laptop.pass(Vec::new()).await;
        on_desktop.pass(Vec::new()).await;
        assert!(on_laptop.connections_held().is_empty() && !laptop_has.runs_app());
        assert!(desktop_has.runs_app(), "the holder keeps it");
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
    assert!(laptop_has.runs_app(), "the laptop took the connection over");
    reads_holder(desktop.workspaces(), &workspace, laptop_device, BRIDGE).await;

    // The desktop wakes: before anything else, it stops the connection it no longer holds, and
    // looking again it finds the laptop holding it.
    on_desktop.pass(Vec::new()).await;
    assert!(
        !desktop_has.runs_app(),
        "a device that resumes stops the connection"
    );
    for _ in 0..3 {
        on_desktop.pass(Vec::new()).await;
        on_laptop.pass(Vec::new()).await;
        assert!(on_desktop.connections_held().is_empty() && !desktop_has.runs_app());
        assert!(laptop_has.runs_app());
    }
    wall.release();
    assert_eq!(
        holder(desktop.workspaces(), &workspace, BRIDGE).await,
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
    let (desktop, laptop, workspace) = two_devices(
        (one.path(), two.path()),
        same_person_laptop(),
        53,
        (&desktop_has, &laptop_has),
    )
    .await;
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
    reads_holder(desktop.workspaces(), &workspace, laptop_device, BRIDGE).await;
    assert_eq!(
        connection_call(desktop.workspaces(), &workspace, BRIDGE).await,
        ConnectionCall::On(laptop_device)
    );
    assert_eq!(
        connection_call(laptop.workspaces(), &workspace, BRIDGE).await,
        ConnectionCall::Here
    );
    assert!(!desktop_has.runs_app(), "never on both");
}

/// How a device in these tests reaches the others, as the runtime's hosts do: where this
/// device's app calls send a call to an app that keeps a connection (the device that has the
/// lease, reached over the link it has to it), and where its answer to an ask another device
/// keeps goes. A device it was told the address of stands in for the network's discovery.
#[derive(Clone)]
struct Leased {
    workspaces: Workspaces,
    discovered: Vec<([u8; 32], iroh_base::EndpointAddr)>,
}

impl Leased {
    fn of(workspaces: &Workspaces) -> Self {
        Self {
            workspaces: workspaces.clone(),
            discovered: Vec::new(),
        }
    }

    /// The actions of `device`: over its link, else dialled where it was discovered, else by
    /// its key.
    fn device(&self, workspace: &str, device: [u8; 32]) -> tilo_peer_state::actions::RemoteDevice {
        let service = self.workspaces.action_service();
        if let Some(linked) = service.remote_held(device, workspace) {
            return linked;
        }
        match self.discovered.iter().find(|(found, _)| *found == device) {
            Some((_, address)) => tilo_workspace_service::workspaces::fixtures::reach_device_at(
                &self.workspaces,
                address,
                workspace,
            ),
            None => reach(&self.workspaces, device, workspace),
        }
    }

    /// Reach the other devices of `self`'s workspaces this way from now on.
    fn reach_devices(self) {
        let providers = self.workspaces.providers();
        providers.reach_devices_through(Arc::new(move |workspace: &str, device: [u8; 32]| {
            Some(self.device(workspace, device))
        }));
    }
}

impl crate::services::HostedWorkspaces for Leased {
    fn host_actions(&self, _: &str) -> Option<Arc<dyn ActionProvider + Send + Sync>> {
        None
    }

    fn connection_actions<'a>(
        &'a self,
        workspace: &'a str,
        app: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ConnectionActions> + Send + 'a>> {
        Box::pin(async move {
            match connection_call(&self.workspaces, workspace, app).await {
                ConnectionCall::Here => ConnectionActions::Here,
                ConnectionCall::Unplaced(why) => ConnectionActions::Unplaced(why),
                ConnectionCall::On(device) => {
                    ConnectionActions::On(Arc::new(self.device(workspace, device)))
                }
            }
        })
    }

    fn reach(
        &self,
        workspace: &str,
        device: [u8; 32],
    ) -> Option<tilo_peer_state::actions::RemoteDevice> {
        Some(self.device(workspace, device))
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
/// The channel's other action reaching outside, which its person never allowed the agent.
const POST: &str = "post";

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
        let Ok(post) = tilo_actions::ActionSpec::operation(
            POST,
            "Post",
            tilo_actions::OperationKind::Effect,
            "test.bridge-post",
            &(),
        ) else {
            unreachable!("the bridge's post declares its operation")
        };
        vec![
            tilo_actions::ResourceSpec::new(CHANNEL, CHANNEL_KIND, "Channel")
                .with_action(send)
                .with_action(post),
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
        (address.resource == CHANNEL && ["send", POST].contains(&address.action.as_str())).then(
            || {
                tilo_actions::ActionDescriptor {
                    did: None,
                    state: tilo_actions::ActionState::default(),
                    semantic_identity: None,
                    address: address.clone(),
                    resource_kind: CHANNEL_KIND.into(),
                    resource_label: "Channel".into(),
                    action_label: address.action.clone(),
                    keywords: Vec::new(),
                    icon: None,
                    description: None,
                    locus: tilo_actions::ExecutionLocus::default(),
                    kind: tilo_actions::OperationKind::Effect,
                    // Both reach outside the workspace, as a bridge's sends do.
                    contract: tilo_actions::ActionContract {
                        kind: tilo_action_policy::ActionKind::Outside,
                        ..tilo_actions::ActionContract::for_operation(
                            tilo_actions::OperationKind::Effect,
                        )
                    },
                    input: tilo_actions::InputSpec::None,
                    params: Vec::new(),
                    view: None,
                }
            },
        )
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
    use crate::test_support::Grants as _;
    let Some(workspace_id) = crate::test_support::WorkspaceId::parse(workspace) else {
        unreachable!("the workspace the devices share parses as an id")
    };
    let person = hex::encode(persons_root().verifying_key().to_bytes());
    let mut tree_workspace = crate::test_support::Workspace::new(workspace_id, "Bridged");
    tree_workspace.policy.set_owner(person.clone());
    // The agent of the person the workspace is theirs, as the settings grant it: not one of the
    // fixture's own default person.
    tree_workspace.allow(
        tilo_action_policy::Grantee::agent("scribe", person.as_str()),
        &crate::test_support::TreePath::root(),
        crate::test_support::KindSet::ALL,
    );
    // Sending on the bridge reaches outside the workspace: the agent does it unasked only where
    // its person allowed that one action, as "Always allow this" does.
    tree_workspace
        .policy
        .grant(tilo_action_policy::agent_answers::grant(
            workspace,
            ("scribe", person.as_str()),
            (
                &tilo_action_policy::agent_answers::AnswerScope::Action {
                    app: BRIDGE.into(),
                    kind: CHANNEL_KIND.into(),
                    action: "send".into(),
                },
                tilo_action_policy::ActionKind::Outside,
            ),
            tilo_action_policy::Answer::Allow,
            tilo_action_policy::Grantee::person(person.clone()),
        ));
    let mut tree = crate::test_support::WorkspaceTree::default();
    tree.workspaces.push(tree_workspace);
    let membership = tree.shared();
    let state = AppBrokerState::test_state_only(
        Some(membership.authority()),
        crate::services::ProcessServices {
            providers: Some(workspaces.providers()),
            hosted_workspaces: Some(Arc::new(Leased::of(workspaces))),
            // A call is admitted with its evidence, as on a device.
            journal_root: Some(journal.to_path_buf()),
            ..membership.services()
        },
    );
    state.serve_invocations(Arc::default());
    state.answer_app_calls(workspace, BRIDGE);
    state
}

/// A call of the bridge's channel's `send` in `workspace`, signed by `signer` when given.
fn send(workspace: &str, signer: Option<(&DeviceCert, &SigningKey)>) -> InvokeRequest {
    let request = InvokeRequest {
        asks: false,
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
    match signer {
        Some(signer) => signed(workspace, request, signer),
        None => request,
    }
}

/// `request` in `workspace`, signed by `signer`.
fn signed(
    workspace: &str,
    mut request: InvokeRequest,
    (cert, key): (&DeviceCert, &SigningKey),
) -> InvokeRequest {
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
    request
}

/// The person's answer for the bridge's channel `send` in `workspace`, which reaches outside the
/// workspace: an agent does such an action unasked only where its person allowed that one, as
/// "Always allow this" does. The answer is the workspace's own, as the settings write it, so
/// every device of the workspace decides the agent's call alike. Its `post` they want to be asked
/// about each time.
async fn allow_the_bridge_send(workspaces: &Workspaces, workspace: &str) {
    tilo_workspace_service::workspaces::fixtures::record_grants(
        workspaces,
        workspace,
        &[
            the_bridge_send_allowed(workspace),
            bridge_answer(workspace, POST, tilo_action_policy::Answer::Ask),
        ],
    )
    .await;
}

/// The person's answer for the bridge's send [`allow_the_bridge_send`] records.
fn the_bridge_send_allowed(workspace: &str) -> Grant {
    bridge_answer(workspace, "send", tilo_action_policy::Answer::Allow)
}

/// The person's `answer` for the agent's `action` on the bridge's channel in `workspace`.
fn bridge_answer(workspace: &str, action: &str, answer: tilo_action_policy::Answer) -> Grant {
    let person = hex::encode(persons_root().verifying_key().to_bytes());
    tilo_action_policy::agent_answers::grant(
        workspace,
        ("scribe", person.as_str()),
        (
            &tilo_action_policy::agent_answers::AnswerScope::Action {
                app: BRIDGE.into(),
                kind: CHANNEL_KIND.into(),
                action: action.into(),
            },
            tilo_action_policy::ActionKind::Outside,
        ),
        answer,
        tilo_action_policy::Grantee::person(person.as_str()),
    )
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
    let (desktop, laptop, workspace) = two_devices(
        (one.path(), two.path()),
        same_person_laptop(),
        54,
        (&desktop_has, &laptop_has),
    )
    .await;
    let desktop_device = desktop.workspaces().lease_device(&workspace).await;
    let laptop_device = laptop.workspaces().lease_device(&workspace).await;
    let three = tempfile::tempdir().unwrap();
    let phone = phone_joins(three.path(), (&desktop, &laptop), &workspace).await;
    let (mut on_desktop, mut on_laptop) =
        (runner(desktop.workspaces()), runner(laptop.workspaces()));
    let (scribe_key, scribe) = admit_scribe(desktop.workspaces(), &workspace).await;
    allow_the_bridge_send(desktop.workspaces(), &workspace).await;
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
    reads_holder(laptop.workspaces(), &workspace, desktop_device, BRIDGE).await;
    desktop_has.has.lock().unwrap().power = false;
    let wall = tilo_clock::testing::SharedWall::default();
    wall.follow();
    wall.pin(tilo_clock::wall() + Duration::from_secs(16));
    on_desktop.pass(Vec::new()).await;
    tilo_test_env::within("the laptop takes the connection", async {
        while !laptop_has.runs_app() {
            on_laptop.pass(Vec::new()).await;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    wall.release();
    reads_holder(desktop.workspaces(), &workspace, laptop_device, BRIDGE).await;

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
        phone: &phone,
        workspace: &workspace,
        desktop_device,
        laptop_device,
        phone_device: phone.workspaces().device_cert().device_public_key,
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
    /// The person's phone, which joined the workspace too and runs no connection.
    phone: &'a TestHost,
    workspace: &'a str,
    desktop_device: [u8; 32],
    laptop_device: [u8; 32],
    phone_device: [u8; 32],
}

/// The person's phone, joined to `workspace` on `state` by the invitation `desktop` issues it,
/// once it and `laptop` know each other's certificates.
async fn phone_joins(
    state: &std::path::Path,
    (desktop, laptop): (&TestHost, &TestHost),
    workspace: &str,
) -> TestHost {
    let phone = device_cert(&persons_root(), "phone", DeviceCapabilities::member());
    let invitation = tilo_workspace_service::workspaces::fixtures::invitation(
        desktop,
        &tilo_workspace_service::workspaces::fixtures::hosting_device().1,
        workspace,
        55,
        &phone.1,
    )
    .await;
    let phone = member_service(state, phone).await;
    if phone.workspaces().join_member(&invitation).await.is_err() {
        unreachable!("the phone joins the workspace it was invited to");
    }
    let laptop_device = laptop.workspaces().device_cert().device_public_key;
    knows(phone.workspaces(), workspace, laptop_device).await;
    let phone_device = phone.workspaces().device_cert().device_public_key;
    knows(laptop.workspaces(), workspace, phone_device).await;
    // The phone finds the laptop as the network's discovery would let it.
    Leased {
        discovered: vec![(laptop_device, laptop.node_addr())],
        ..Leased::of(phone.workspaces())
    }
    .reach_devices();
    phone
}

/// Wait until `workspaces` holds the certificate of `device` in `workspace`.
async fn knows(workspaces: &Workspaces, workspace: &str, device: [u8; 32]) {
    tilo_test_env::within("the device's certificate reaches the other device", async {
        loop {
            if let Ok(state) = workspaces.workspace(workspace).await
                && state
                    .lock()
                    .await
                    .peer()
                    .certificates()
                    .iter()
                    .any(|certificate| certificate.device_public_key == device)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
}

/// The agent's call, asked of the desktop, runs on the laptop that holds the connection: as the
/// agent, brought by the desktop, its signature with it.
async fn the_agents_call_runs_on_the_laptops_bridge(
    pair: &Pair<'_>,
    on_desktop_calls: &Arc<AppBrokerState>,
    (desktop_bridge, laptop_bridge): (&Arc<BridgeProcess>, &Arc<BridgeProcess>),
    (agent, (scribe, scribe_key)): (&Caller, (&DeviceCert, &SigningKey)),
) {
    // Its person allowed the agent the bridge's send, and asks to be asked for its post.
    a_call_its_person_must_allow_waits_for_their_answer(
        pair,
        on_desktop_calls,
        laptop_bridge,
        agent,
        (scribe, scribe_key),
    )
    .await;
    let call = send(pair.workspace, Some((scribe, scribe_key)));
    let ended = ends(&AppCallsOf(Arc::clone(on_desktop_calls)), agent, call).await;
    assert!(
        matches!(ended, Ok(ReceiptStatus::Completed { .. })),
        "the agent's call runs on the laptop: {ended:?}"
    );
    let Some(ran) = locked(&laptop_bridge.ran).pop() else {
        unreachable!("the laptop's bridge ran the agent's call")
    };
    assert!(
        locked(&desktop_bridge.ran).is_empty(),
        "never where the lease is not"
    );
    let (ran_as, request) = ran;
    assert_eq!(
        ran_as.acting().member_id(),
        Some(&tilo_member_types::MemberId::agent(
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

/// The agent's call the workspace leaves to its person, asked of the desktop and sent on to the
/// laptop that holds the connection, waits there in its asked record: the agent follows it from
/// the desktop and reads where it waits, nothing runs until the person answers, nobody else's
/// answer counts and nobody else follows it. Their approve on the laptop runs it once, and their
/// decline of another from their phone, which keeps no record of it, runs nothing; the agent
/// reads each outcome on the desktop.
async fn a_call_its_person_must_allow_waits_for_their_answer(
    pair: &Pair<'_>,
    on_desktop_calls: &Arc<AppBrokerState>,
    laptop_bridge: &Arc<BridgeProcess>,
    agent: &Caller,
    (scribe, scribe_key): (&DeviceCert, &SigningKey),
) {
    let laptop = pair.laptop.workspaces();
    let on_desktop: Arc<dyn ActionProvider + Send + Sync> =
        Arc::new(AppCallsOf(Arc::clone(on_desktop_calls)));
    let person = |device: [u8; 32]| {
        Caller::authenticated(persons_root().verifying_key().to_bytes(), device)
            .in_workspace(pair.workspace.to_owned())
    };
    let post = || {
        let mut request = send(pair.workspace, None);
        request.target.address = tilo_actions::ActionAddress::new(CHANNEL, POST);
        signed(pair.workspace, request, (scribe, scribe_key))
    };
    let approved = post();
    let record = waits_in_its_record(&on_desktop, agent, approved.clone()).await;
    assert_eq!(record.workspace, pair.workspace, "{record:?}");
    assert!(locked(&laptop_bridge.ran).is_empty(), "nothing ran unasked");

    nobody_else_answers(pair, (agent, &person(pair.laptop_device)), &record).await;
    nobody_else_follows(pair, &on_desktop, approved.invocation).await;
    let still = observed(&on_desktop, agent, approved.invocation).await;
    assert!(
        still.as_ref().is_some_and(|state| {
            tilo_actions::remote::asked::RecordRef::waited_in(&state.update).as_ref()
                == Some(&record)
        }),
        "it still waits for the person: {still:?}"
    );
    assert!(
        locked(&laptop_bridge.ran).is_empty(),
        "nothing ran on their word"
    );

    // The person's approve on the laptop runs it, once, and the agent reads that it ran.
    let answered = answer(
        laptop,
        &person(pair.laptop_device),
        &record,
        tilo_actions::remote::asked::Offer::Approve,
    )
    .await;
    assert!(answered.is_ok(), "the person approves it: {answered:?}");
    let ended = followed(&on_desktop, agent, approved.invocation).await;
    assert!(
        matches!(ended, ReceiptStatus::Completed { .. }),
        "the approved call ran: {ended:?}"
    );
    let ran = std::mem::take(&mut *locked(&laptop_bridge.ran));
    assert_eq!(
        ran.iter()
            .map(|(_, request)| request.invocation)
            .collect::<Vec<_>>(),
        vec![approved.invocation],
        "it ran once"
    );
    let again = answer(
        laptop,
        &person(pair.laptop_device),
        &record,
        tilo_actions::remote::asked::Offer::Approve,
    )
    .await;
    assert!(
        again.is_err(),
        "an answered question admits nothing again: {again:?}"
    );
    assert!(locked(&laptop_bridge.ran).is_empty(), "still once");

    // Their decline of another, on their phone, reaches the laptop that keeps it: it runs
    // nothing, and the agent reads on the desktop that they declined.
    let declined = post();
    let record = waits_in_its_record(&on_desktop, agent, declined.clone()).await;
    let answered = answer(
        pair.phone.workspaces(),
        &person(pair.phone_device),
        &record,
        tilo_actions::remote::asked::Offer::Decline,
    )
    .await;
    assert!(
        answered.is_ok(),
        "the person declines it on their phone: {answered:?}"
    );
    let ended = followed(&on_desktop, agent, declined.invocation).await;
    assert!(
        matches!(&ended, ReceiptStatus::Cancelled { message: Some(message) }
            if message == tilo_actions::remote::asked::DECLINED),
        "the agent reads that its person declined: {ended:?}"
    );
    assert!(locked(&laptop_bridge.ran).is_empty(), "nothing ran on a no");
}

/// Another member asking the desktop about the agent's call `invocation`, and asking the laptop
/// it waits on, reads it as an id nothing admitted, and cannot stop it.
async fn nobody_else_follows(
    pair: &Pair<'_>,
    on_desktop: &Arc<dyn ActionProvider + Send + Sync>,
    invocation: tilo_actions::InvocationId,
) {
    let other = Caller::authenticated(
        SigningKey::from_bytes(&[93; 32]).verifying_key().to_bytes(),
        SigningKey::from_bytes(&[94; 32]).verifying_key().to_bytes(),
    )
    .in_workspace(pair.workspace.to_owned());
    let laptop: Arc<dyn ActionProvider + Send + Sync> = pair.laptop.workspaces().device_actions();
    for (on, there) in [("the desktop", on_desktop), ("the laptop", &laptop)] {
        let read = observed(there, &other, invocation).await;
        assert!(
            read.is_none(),
            "another member reads nothing on {on}: {read:?}"
        );
        let stopped = there
            .cancel(&other, &invocation)
            .await
            .unwrap_or(Err(ActionCallError::Unanswered));
        assert!(
            matches!(stopped, Err(ActionCallError::UnknownInvocation)),
            "another member stops nothing on {on}: {stopped:?}"
        );
    }
}

/// Neither another person, nor the agent `agent` itself, nor a Shortcut's unattended run acting
/// as `person` (nobody is at a screen to answer) approves `record`, which asks `person`: not on
/// the laptop that keeps it, and not from the person's desktop or phone, which would bring it
/// there.
async fn nobody_else_answers(
    pair: &Pair<'_>,
    (agent, person): (&Caller, &Caller),
    record: &tilo_actions::remote::asked::RecordRef,
) {
    let (other_root, other_device) = (
        SigningKey::from_bytes(&[91; 32]),
        SigningKey::from_bytes(&[92; 32]),
    );
    let other_person = Caller::authenticated(
        other_root.verifying_key().to_bytes(),
        other_device.verifying_key().to_bytes(),
    )
    .in_workspace(pair.workspace.to_owned());
    let on = |device: [u8; 32]| {
        Caller::authenticated(persons_root().verifying_key().to_bytes(), device)
            .in_workspace(pair.workspace.to_owned())
            .running_shortcut("nightly")
    };
    let unattended = person.clone().running_shortcut("nightly");
    for (who, stranger, workspaces) in [
        ("another person", &other_person, pair.laptop.workspaces()),
        ("the agent", agent, pair.laptop.workspaces()),
        (
            "the person's unattended Shortcut",
            &unattended,
            pair.laptop.workspaces(),
        ),
        (
            "the person's unattended Shortcut on their desktop",
            &on(pair.desktop_device),
            pair.desktop.workspaces(),
        ),
        (
            "the person's unattended Shortcut on their phone",
            &on(pair.phone_device),
            pair.phone.workspaces(),
        ),
    ] {
        let answer = answer(
            workspaces,
            stranger,
            record,
            tilo_actions::remote::asked::Offer::Approve,
        )
        .await;
        assert!(answer.is_err(), "{who} approves nothing: {answer:?}");
    }
}

/// `request`, asked of `calls` as `caller`, waiting for its person's yes: the record it waits in,
/// as the caller reads it.
async fn waits_in_its_record(
    calls: &Arc<dyn ActionProvider + Send + Sync>,
    caller: &Caller,
    request: InvokeRequest,
) -> tilo_actions::remote::asked::RecordRef {
    let invocation = request.invocation;
    let receipt = calls
        .invoke(caller, request)
        .await
        .unwrap_or(Err(ActionCallError::Unanswered));
    assert!(
        matches!(&receipt, Ok(receipt) if receipt.status == ReceiptStatus::Accepted),
        "the call waits, never refused: {receipt:?}"
    );
    tilo_test_env::within("the caller reads where its call waits", async {
        loop {
            if let Some(record) = observed(calls, caller, invocation)
                .await
                .and_then(|state| tilo_actions::remote::asked::RecordRef::waited_in(&state.update))
            {
                return record;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
}

/// How `invocation`, which `caller` asked of `calls`, ends, as they follow it.
async fn followed(
    calls: &Arc<dyn ActionProvider + Send + Sync>,
    caller: &Caller,
    invocation: tilo_actions::InvocationId,
) -> ReceiptStatus {
    tilo_test_env::within("the call ends", async {
        loop {
            if let Some(state) = observed(calls, caller, invocation).await
                && state.update.is_terminal()
            {
                return state.receipt().status;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
}

/// `invocation` as `caller` reads it of `calls`, asked off this runtime's thread: reading a call
/// another device runs waits for that device's answer, which this runtime carries.
async fn observed(
    calls: &Arc<dyn ActionProvider + Send + Sync>,
    caller: &Caller,
    invocation: tilo_actions::InvocationId,
) -> Option<tilo_actions::remote::InvocationState> {
    let (calls, caller) = (Arc::clone(calls), caller.clone());
    let Ok(read) = tokio::task::spawn_blocking(move || calls.observe(&caller, &invocation)).await
    else {
        unreachable!("a read of a call does not panic")
    };
    read
}

/// `answering`'s `resolution` of the asked `record`, as `workspaces` takes it.
async fn answer(
    workspaces: &Workspaces,
    answering: &Caller,
    record: &tilo_actions::remote::asked::RecordRef,
    resolution: tilo_actions::remote::asked::Offer,
) -> Result<InvocationReceipt, ActionCallError> {
    let Ok(serde_json::Value::Object(arguments)) =
        serde_json::to_value(tilo_actions::remote::asked::Resolve {
            resolution,
            ..tilo_actions::remote::asked::Resolve::default()
        })
    else {
        unreachable!("an answer is an object")
    };
    let request = InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: tilo_actions::remote::RemoteActionTarget {
            provider: tilo_actions::remote::WORKSPACE_INVOCATIONS_PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(
                &record.record,
                tilo_actions::remote::asked::RESOLVE,
            ),
        },
        argument: tilo_actions::remote::InvokeArgument::Json(arguments),
        ..send(&record.workspace, None)
    };
    workspaces
        .resolve_asked(answering, &request)
        .await
        .unwrap_or(Err(ActionCallError::Unanswered))
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
    assert!(!desktop_has.runs_app(), "never on both");
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
        Some(&tilo_member_types::MemberId::agent(
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
