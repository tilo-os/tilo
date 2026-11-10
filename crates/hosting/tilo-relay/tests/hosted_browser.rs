//! A hosted workspace's Browser: opening it places an environment whose
//! session joins the workspace as a device and holds the window, and an
//! agent's page actions reach that window as the agent's — counted, and
//! refused with the host's typed refusal past the free allowance.
#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
#![allow(clippy::unwrap_used, reason = "in a test these are the assertions")]

use std::sync::{Arc, Mutex};

use tilo_actions::remote::host_account::{NextStepKind, Reason, Refusal};
use tilo_actions::remote::{
    ActionHostError, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionFrame,
    RemoteActionMessage, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_relay::hosted_browser::{
    BrowserEnvironments, BrowserMeter, HostedBrowsers, Material, PAGE_RESOURCE, Window,
    no_payment_method,
};
use tilo_relay::transport::REMOTE_ACTION_ENVELOPE_VERSION;
use tilo_relay::{AppServingPlane, Envelope, RelayClient};
use tilo_user_identity::{
    DeviceCapabilities, RevocationList, prepare_stand_in_cert, sign_device_cert,
};

use crate::common::{
    TestDevice, TestRelay, TestUser, admit_members, client_endpoint, spawn_relay_serving_apps,
};

const WS: u64 = 21;
const OTHER_WS: u64 = 22;
const WS_NAME: &str = "11111111-1111-4111-8111-111111111111";
const WINDOW: &str = "browser@42";

/// Places environments in-process: each "environment" is the device the
/// relay certified, which the test's client then joins the workspace as.
#[derive(Default)]
struct InProcessEnvironments {
    starts: Mutex<Vec<Material>>,
}

impl InProcessEnvironments {
    fn starts(&self) -> usize {
        self.starts.lock().unwrap().len()
    }

    fn material(&self) -> Material {
        self.starts.lock().unwrap()[0].clone()
    }
}

impl BrowserEnvironments for InProcessEnvironments {
    fn start(&self, material: &Material) -> Result<Window, String> {
        self.starts.lock().unwrap().push(material.clone());
        Ok(Window {
            device: material.device.device_public_key,
            provider: WINDOW.into(),
        })
    }

    fn stop(&self, _: &str) {}
}

/// Counts what reached it, and refuses what it is told to.
#[derive(Default)]
struct Counting {
    recorded: Mutex<usize>,
    refusal: Mutex<Option<Refusal>>,
}

impl BrowserMeter for Counting {
    fn admit(&self, _: &str) -> Result<(), Refusal> {
        let refusal = self.refusal.lock().unwrap().clone();
        match refusal {
            Some(refusal) => Err(refusal),
            None => Ok(()),
        }
    }

    fn record(&self, _: &str, _: &str) {
        let mut recorded = self.recorded.lock().unwrap();
        *recorded = recorded.saturating_add(1);
    }
}

/// The plane of a relay that routes its workspaces' Browser page actions to
/// the window holding them.
struct Routing(Arc<HostedBrowsers>);

impl AppServingPlane for Routing {
    fn serve(
        &self,
        _: u64,
        _: &tilo_actions::remote::Caller,
        _: RemoteActionMessage,
    ) -> Option<tilo_actions::remote::Pending<RemoteActionMessage>> {
        None
    }

    fn route(
        &self,
        workspace_id: u64,
        _: [u8; 32],
        message: &RemoteActionMessage,
    ) -> Option<tilo_relay::app_serving::Routing> {
        self.0.route(workspace_id, message)
    }
}

fn frame(message: RemoteActionMessage) -> Vec<u8> {
    RemoteActionFrame::new(message).encode().unwrap()
}

fn page(invocation: InvocationId, action: &str) -> RemoteActionMessage {
    RemoteActionMessage::Invoke(InvokeRequest {
        on_behalf_of: None,
        invocation,
        target: RemoteActionTarget {
            provider: WINDOW.into(),
            address: ActionAddress::new(PAGE_RESOURCE, action),
        },
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
    })
}

fn to_host(workspace_id: u64, host: [u8; 32], frame: Vec<u8>) -> Envelope {
    Envelope::RemoteAction {
        workspace_id,
        version: REMOTE_ACTION_ENVELOPE_VERSION,
        to_device_public_key: host,
        from_device_public_key: [0xEE; 32],
        from_user_public_key: [0xEE; 32],
        frame,
    }
}

async fn subscribe(relay: &tilo_relay::Relay, user: &TestUser, device: &TestDevice) -> RelayClient {
    subscribe_in(relay, user, device, WS).await
}

async fn subscribe_in(
    relay: &tilo_relay::Relay,
    user: &TestUser,
    device: &TestDevice,
    workspace_id: u64,
) -> RelayClient {
    let client = RelayClient::connect(
        client_endpoint().await,
        relay.node_addr(),
        user.user_pk,
        device.cert.clone(),
        device.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("connect");
    client.send(Envelope::Subscribe { workspace_id }).unwrap();
    let ready = tilo_test_env::within("ready timeout", client.recv()).await;
    assert!(matches!(ready, Some(Envelope::Ready { .. })), "{ready:?}");
    client
}

async fn next(client: &RelayClient, who: &str) -> Envelope {
    tokio::time::timeout(tilo_test_env::hang_bound(), client.recv())
        .await
        .unwrap_or_else(|_| panic!("{who} timed out"))
        .unwrap_or_else(|| panic!("{who} connection closed"))
}

/// The device holding the workspace's Browser window: it answers every
/// invocation with a receipt and keeps who sent it.
async fn window_session(
    relay: &tilo_relay::Relay,
    user: &TestUser,
    material: &Material,
) -> Arc<Mutex<Vec<[u8; 32]>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let device = DeviceIdentity::of(material);
    let client = subscribe(relay, user, &device).await;
    let heard = Arc::clone(&seen);
    let holder = material.device.device_public_key;
    let person = material.user_public_key;
    tokio::spawn(async move {
        while let Some(envelope) = client.recv().await {
            let Envelope::RemoteAction {
                workspace_id,
                from_device_public_key,
                frame,
                ..
            } = envelope
            else {
                continue;
            };
            let Ok(RemoteActionFrame { message, .. }) = RemoteActionFrame::decode(&frame) else {
                continue;
            };
            let RemoteActionMessage::Invoke(request) = message else {
                continue;
            };
            heard.lock().unwrap().push(from_device_public_key);
            let answer = RemoteActionFrame::new(RemoteActionMessage::Receipt(InvocationReceipt {
                invocation: request.invocation,
                status: ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput::default(),
                },
            }))
            .encode()
            .unwrap();
            let _ = client.send(Envelope::RemoteAction {
                workspace_id,
                version: REMOTE_ACTION_ENVELOPE_VERSION,
                to_device_public_key: from_device_public_key,
                from_device_public_key: holder,
                from_user_public_key: person,
                frame: answer,
            });
        }
    });
    seen
}

use tilo_actions::remote::InvocationReceipt;

/// The test device a material names: what the session in the environment
/// joins as.
struct DeviceIdentity;

impl DeviceIdentity {
    fn of(material: &Material) -> TestDevice {
        TestDevice {
            sk: material.key().unwrap(),
            cert: material.device.clone(),
        }
    }
}

struct Fixture {
    user: TestUser,
    home: [u8; 32],
    agent: TestDevice,
    relay: TestRelay,
    browsers: Arc<HostedBrowsers>,
    environments: Arc<InProcessEnvironments>,
    meter: Arc<Counting>,
}

async fn fixture(meter: Arc<Counting>) -> Fixture {
    let user = TestUser::fresh("Ada");
    let host = user.enrol("home", DeviceCapabilities::host());
    let agent = user.enrol("agent", DeviceCapabilities::member());
    let home = host.device_pubkey();
    let host_key = host.sk.clone();
    let host_cert = {
        let now = tilo_clock::unix_now_ns();
        let mut stand_in = prepare_stand_in_cert(
            user.user_pk,
            host_key.verifying_key().to_bytes(),
            "Ada's host",
            now,
            now.saturating_add(24 * 60 * 60 * 1_000_000_000),
        );
        sign_device_cert(&mut stand_in, &user.user_sk).unwrap();
        stand_in
    };
    let environments = Arc::new(InProcessEnvironments::default());
    let browsers = Arc::new(HostedBrowsers::new(
        Box::new(EnvironmentsHandle(Arc::clone(&environments))),
        Box::new(SharedMeter(Arc::clone(&meter))),
        "acct_ada".into(),
    ));
    let relay = Box::pin(spawn_relay_serving_apps(
        &user,
        host,
        &[WS, OTHER_WS],
        &[agent.device_pubkey()],
        Arc::new(Routing(Arc::clone(&browsers))),
    ))
    .await;
    browsers.serve_from(tilo_relay::hosted_browser::Authority {
        user_public_key: user.user_pk,
        cert: host_cert,
        key: host_key,
        relay: relay.relay.node_addr(),
        workspaces: None,
        runtime: tokio::runtime::Handle::current(),
    });
    browsers
        .place(WS_NAME, WS, "record-1")
        .expect("the environment was placed");
    Fixture {
        user,
        home,
        agent,
        relay,
        browsers,
        environments,
        meter,
    }
}

/// The environments a test started, behind the trait the relay holds.
struct EnvironmentsHandle(Arc<InProcessEnvironments>);

impl BrowserEnvironments for EnvironmentsHandle {
    fn start(&self, material: &Material) -> Result<Window, String> {
        self.0.start(material)
    }

    fn stop(&self, workspace: &str) {
        self.0.stop(workspace);
    }
}

/// The meter a test counts with, behind the trait the relay holds.
struct SharedMeter(Arc<Counting>);

impl BrowserMeter for SharedMeter {
    fn admit(&self, account: &str) -> Result<(), Refusal> {
        self.0.admit(account)
    }

    fn record(&self, workspace: &str, account: &str) {
        self.0.record(workspace, account);
    }
}

impl Fixture {
    /// The workspace lists the window's device, as the relay lists it on a
    /// host that governs the workspace.
    async fn list_environment(&self) {
        let device = self.browsers.window(WS).unwrap().device;
        admit_members(
            &self.relay.relay,
            WS,
            &[self.agent.device_pubkey(), device],
            &self.relay.state_root,
        )
        .await;
    }
}

#[tokio::test]
async fn opening_browser_places_an_environment_whose_session_holds_the_window() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let window = fixture.browsers.window(WS).unwrap();
    assert_eq!(window.provider, WINDOW);
    assert_eq!(fixture.environments.starts(), 1, "one environment opened");
    let material = fixture.environments.material();
    assert_eq!(material.record, "record-1");
    assert_eq!(material.workspace, WS_NAME);
    assert_eq!(material.workspace_id, WS);
    assert_eq!(
        material.device.capabilities,
        DeviceCapabilities::member(),
        "the device is an ordinary member device of the workspace's person"
    );
    assert_eq!(
        material.device.user_public_key, fixture.user.user_pk,
        "of the person the workspace belongs to"
    );
    assert!(
        material.device.issuer.is_some(),
        "certified by the host's stand-in, not by the root"
    );
    assert_eq!(
        material.device.expires_at_ns.unwrap() - material.device.issued_at_ns,
        u64::try_from(tilo_relay::hosted_browser::DEVICE_LIFETIME.as_nanos()).unwrap(),
        "it lasts as long as the environment may run"
    );
    assert_eq!(
        material.device.device_public_key, window.device,
        "the window is the device the relay certified"
    );
    // A second open attaches to the window already up.
    let again = fixture.browsers.place(WS_NAME, WS, "record-1").unwrap();
    assert_eq!(again, window);
    assert_eq!(fixture.environments.starts(), 1, "no second environment");
}

#[tokio::test]
async fn a_page_action_reaches_the_window_as_the_caller() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    fixture.list_environment().await;
    let material = fixture.environments.material();
    let seen = window_session(&fixture.relay.relay, &fixture.user, &material).await;
    let caller = subscribe(&fixture.relay.relay, &fixture.user, &fixture.agent).await;

    let id = InvocationId::new();
    caller
        .send(to_host(WS, fixture.home, frame(page(id, "read-page"))))
        .unwrap();
    let answer = next(&caller, "caller").await;
    let Envelope::RemoteAction {
        to_device_public_key,
        from_device_public_key,
        frame: answer,
        ..
    } = answer
    else {
        panic!("the window answered: {answer:?}");
    };
    assert_eq!(to_device_public_key, fixture.agent.device_pubkey());
    assert_eq!(
        from_device_public_key, material.device.device_public_key,
        "the answer comes from the window's device"
    );
    let RemoteActionMessage::Receipt(receipt) = RemoteActionFrame::decode(&answer).unwrap().message
    else {
        panic!("an invocation is answered with a receipt");
    };
    assert!(matches!(receipt.status, ReceiptStatus::Completed { .. }));
    assert_eq!(
        *seen.lock().unwrap(),
        [fixture.agent.device_pubkey()],
        "the window ran it as the caller, not as the relay: History records the agent's run"
    );
    assert_eq!(*fixture.meter.recorded.lock().unwrap(), 1, "counted");
}

#[tokio::test]
async fn a_page_action_past_the_allowance_is_refused_and_says_how_to_continue() {
    let meter = Arc::new(Counting::default());
    *meter.refusal.lock().unwrap() = Some(no_payment_method(3));
    let fixture = fixture(Arc::clone(&meter)).await;
    fixture.list_environment().await;
    let material = fixture.environments.material();
    let seen = window_session(&fixture.relay.relay, &fixture.user, &material).await;
    let caller = subscribe(&fixture.relay.relay, &fixture.user, &fixture.agent).await;

    let id = InvocationId::new();
    caller
        .send(to_host(WS, fixture.home, frame(page(id, "fill-page"))))
        .unwrap();
    let Envelope::RemoteAction { frame: answer, .. } = next(&caller, "caller").await else {
        panic!("the caller is answered");
    };
    let RemoteActionMessage::Receipt(receipt) = RemoteActionFrame::decode(&answer).unwrap().message
    else {
        panic!("a refusal is a receipt");
    };
    assert_eq!(receipt.invocation, id);
    let ReceiptStatus::Rejected {
        error: ActionHostError::Refused { reason },
    } = receipt.status
    else {
        panic!("refused: {receipt:?}");
    };
    let refusal = Refusal::from_reason(&reason).expect("the refusal is the host's typed one");
    assert_eq!(refusal.reason, Reason::NoPaymentMethod);
    assert!(
        refusal.step(NextStepKind::AddPaymentMethod).is_some(),
        "one next step: add a payment method"
    );
    assert!(
        seen.lock().unwrap().is_empty(),
        "a refused action never reaches the window"
    );
    assert_eq!(*meter.recorded.lock().unwrap(), 0, "nothing ran to count");
}

#[tokio::test]
async fn an_environment_serves_the_workspace_it_was_placed_for_and_no_other() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    fixture.list_environment().await;
    let material = fixture.environments.material();
    let seen = window_session(&fixture.relay.relay, &fixture.user, &material).await;
    let environment = DeviceIdentity::of(&material);
    let client = RelayClient::connect(
        client_endpoint().await,
        fixture.relay.relay.node_addr(),
        fixture.user.user_pk,
        environment.cert.clone(),
        environment.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("the environment's device dials the host");
    client
        .send(Envelope::Subscribe {
            workspace_id: OTHER_WS,
        })
        .unwrap();
    let answer = next(&client, "environment").await;
    assert!(
        !matches!(answer, Envelope::Ready { .. }),
        "a device of one workspace joins no other: {answer:?}"
    );
    assert!(seen.lock().unwrap().is_empty());
}
