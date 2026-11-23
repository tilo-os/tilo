//! A device's actions reach its host over the action service, and nowhere else: the host
//! answers them through the app action server it serves, as the caller its signature and
//! certificate name, refuses a device the workspace does not admit, and passes a request
//! addressed to another device on to that device alone.
#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
use std::sync::{Arc, Mutex};

use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationLog,
    InvocationReceipt, InvocationState, InvokeRequest, Pending, ReceiptStatus, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};
use tilo_peer_state::actions::{ActionService, NoActions, RemoteDevice};
use tilo_peer_state::direct::Replica;
use tilo_server::{ActionProviderServer, WorkspaceId};
use tilo_user_identity::{DeviceCapabilities, DeviceCert};

use crate::common::{TestDevice, TestUser, device_endpoint, spawn_server_serving_apps};

const WS: WorkspaceId = WorkspaceId::from_u128(11);

/// A device asking its host's actions at `addr` as itself: an endpoint keyed by its own
/// device key, and the action service that signs what it asks.
async fn asking(
    device: &TestDevice,
    addr: iroh_base::EndpointAddr,
    served_by: [u8; 32],
) -> (iroh::Endpoint, RemoteDevice, Arc<ActionService>) {
    let endpoint = device_endpoint(device).await;
    let actions = Arc::new(ActionService::new(device.sk.clone(), Arc::new(NoActions)));
    // A Tilo server's action service signs as its own workspace service, which
    // its endpoint is keyed apart from.
    actions.keyed_apart(served_by, *addr.id.as_bytes());
    let remote = actions.remote(&endpoint, &addr, &WS.to_string());
    (endpoint, remote, actions)
}

fn invoke(invocation: InvocationId) -> InvokeRequest {
    InvokeRequest {
        on_behalf_of: None,
        invocation,
        target: RemoteActionTarget {
            provider: "app".into(),
            address: ActionAddress::new("note:1", "archive"),
        },
        argument: tilo_actions::remote::InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one host's whole action service, its calls and their answers"
)]
async fn the_host_answers_its_devices_and_refuses_the_rest() {
    let user = TestUser::fresh("Alice");
    let host = user.enrol("home", DeviceCapabilities::member());
    let a = user.enrol("laptop", DeviceCapabilities::primary());
    let stranger = user.enrol("phone", DeviceCapabilities::member());
    let actions = Arc::new(RecordingHost::default());
    let server = Box::pin(spawn_server_serving_apps(
        &user,
        host,
        &[WS],
        &[&a.cert],
        Arc::new(ActionProviderServer::new(SharedHost(Arc::clone(&actions)))),
    ))
    .await;
    let service_device = server
        .server
        .workspaces()
        .expect("the server holds workspaces")
        .action_service()
        .device();
    let (a_endpoint, remote, _a_actions) =
        asking(&a, server.server.node_addr(), service_device).await;
    let caller = Caller::authenticated(user.user_pk, a.device_pubkey());

    // The host answers the catalogue and the invocation through the app action server it
    // serves, as the caller the request's own signature and certificate name.
    let page = tilo_test_env::within(
        "the host answers a catalogue query",
        ActionProvider::query(&remote, &caller, CatalogueQuery::default()),
    )
    .await
    .expect("the host answers");
    assert!(page.is_ok(), "the host answers the catalogue: {page:?}");
    let id = InvocationId::new();
    let receipt = tilo_test_env::within(
        "the host answers an invocation",
        ActionProvider::invoke(&remote, &caller, invoke(id)),
    )
    .await
    .expect("the host answers")
    .expect("the invocation runs");
    assert_eq!(receipt.invocation, id);
    assert!(matches!(receipt.status, ReceiptStatus::Completed { .. }));
    assert_eq!(
        *actions.callers.lock().unwrap(),
        [Caller::authenticated(user.user_pk, a.device_pubkey()).in_workspace(WS.to_string())],
        "the app action server sees the device that asked, nobody else"
    );

    // A device the workspace's roster does not name asks nothing, and runs nothing.
    let (stranger_endpoint, stranger_remote, _stranger_actions) =
        asking(&stranger, server.server.node_addr(), service_device).await;
    let nobody = Caller::authenticated(user.user_pk, stranger.device_pubkey());
    let refused = tilo_test_env::within(
        "the host refuses a device it does not admit",
        ActionProvider::query(&stranger_remote, &nobody, CatalogueQuery::default()),
    )
    .await
    .expect("the host answers")
    .expect_err("a device off the roster asks nothing");
    assert!(
        matches!(
            refused,
            ActionCallError::NotPermitted | ActionCallError::Refused { .. }
        ),
        "{refused:?}"
    );
    assert_eq!(
        actions.callers.lock().unwrap().len(),
        1,
        "a device off the roster never reaches the app action server"
    );

    drop((a_endpoint, stranger_endpoint));
    drop(server);
}

/// A request addressed to another device the host holds a link to is passed on to that
/// device alone, and runs there as the device that signed it.
#[tokio::test]
async fn a_request_addressed_to_another_device_is_passed_on() {
    let user = TestUser::fresh("Alice");
    let host = user.enrol("home", DeviceCapabilities::member());
    let a = user.enrol("laptop", DeviceCapabilities::primary());
    let page = user.enrol("browser", DeviceCapabilities::member());
    // What the page holds of the workspace, made before its host is handed to the server.
    let page_workspace = Replica::new(workspace_of(
        WS,
        &host,
        &[a.cert.clone(), page.cert.clone()],
    ));
    let server = Box::pin(spawn_server_serving_apps(
        &user,
        host,
        &[WS],
        &[&a.cert, &page.cert],
        Arc::new(ActionProviderServer::new(SharedHost(Arc::default()))),
    ))
    .await;
    // The page holds a link to the host and answers its own actions on it.
    let held = Arc::new(RecordingHost::default());
    let page_actions = ActionService::new(page.sk.clone(), Arc::new(SharedHost(Arc::clone(&held))));
    page_actions.hold(page_workspace);
    let (host_end, page_end) =
        tilo_peer_state::actions::testing::duplex(Some(page.device_pubkey()), None);
    page_actions.serve_link(page_end);
    server
        .server
        .workspaces()
        .expect("the server holds workspaces")
        .action_service()
        .reach_device_at(page.device_pubkey(), host_end);

    // A asks its host for the page's actions: the host passes the request on to the page, which
    // answers it as A.
    let service_device = server
        .server
        .workspaces()
        .expect("the server holds workspaces")
        .action_service()
        .device();
    let (a_endpoint, _remote, a_actions) =
        asking(&a, server.server.node_addr(), service_device).await;
    let caller = Caller::authenticated(user.user_pk, a.device_pubkey());
    let through = a_actions.through(
        &a_endpoint,
        &server.server.node_addr(),
        &WS.to_string(),
        page.device_pubkey(),
    );
    let id = InvocationId::new();
    let receipt = tilo_test_env::within(
        "the host passes the request on",
        ActionProvider::invoke(&through, &caller, invoke(id)),
    )
    .await
    .expect("the page answers")
    .expect("the page runs the invocation");
    assert_eq!(receipt.invocation, id);
    assert!(matches!(receipt.status, ReceiptStatus::Completed { .. }));
    assert_eq!(
        *held.callers.lock().unwrap(),
        [Caller::authenticated(user.user_pk, a.device_pubkey()).in_workspace(WS.to_string())],
        "the page runs the request as the device that signed it"
    );

    drop(a_endpoint);
    drop(server);
}

/// The workspace `id`, made by `host` with `roster` in it: what a page holds of the
/// workspaces it takes part in, without a store.
fn workspace_of(
    id: WorkspaceId,
    host: &TestDevice,
    roster: &[DeviceCert],
) -> tilo_peer_state::StatePeer {
    let now = tilo_clock::wall();
    let mut roster = roster.to_vec();
    // The device that founded the workspace holds it among its roster.
    if !roster
        .iter()
        .any(|cert| cert.device_public_key == host.cert.device_public_key)
    {
        roster.push(host.cert.clone());
    }
    let mut peer = tilo_peer_state::StatePeer::new(
        &id.to_string(),
        host.cert.clone(),
        roster.clone(),
        tilo_user_identity::RevocationList::new(),
        now,
    )
    .expect("the workspace opens");
    peer.select_exact_admissions()
        .expect("its admissions stand");
    peer.install_seed(
        tilo_peer_state::SeedState {
            workspace_changes: tilo_peer_state::seed_workspace(
                &id.to_string(),
                &host.cert,
                &roster,
            )
            .expect("the workspace seeds"),
            documents: Vec::new(),
        },
        &host.sk,
        now,
    )
    .expect("the seed lands");
    peer
}

/// Records which callers reached it; runs every admitted invocation to
/// completion immediately.
#[derive(Default)]
struct RecordingHost {
    callers: Mutex<Vec<Caller>>,
    log: Mutex<InvocationLog<()>>,
}

/// Lets the test keep a handle on the host the server owns.
#[derive(Clone)]
struct SharedHost(Arc<RecordingHost>);

impl ActionProvider for SharedHost {
    fn query(
        &self,
        _caller: &Caller,
        _query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Ok(CataloguePage::default()))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        self.0.callers.lock().unwrap().push(caller.clone());
        let admitted = self
            .0
            .log
            .lock()
            .unwrap()
            .admit(caller, &request, || Ok(()))
            .map(|_| ());
        Pending::ready(admitted.map(|()| InvocationReceipt {
            invocation: request.invocation,
            status: ReceiptStatus::Completed {
                output: tilo_actions::ActionOutput::default(),
            },
        }))
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.0.log.lock().unwrap().get(caller, id).map(|()| {
            InvocationState::new(
                *id,
                InvocationUpdate::Succeeded {
                    invocation: *id,
                    output: tilo_actions::ActionOutput::default(),
                },
            )
        })
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), ActionCallError>> {
        tilo_actions::remote::Pending::ready(Err(ActionCallError::NotCancellable {
            reason: "finished".into(),
        }))
    }
}
