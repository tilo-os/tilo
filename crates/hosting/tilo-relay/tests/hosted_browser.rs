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
use std::time::{Duration, Instant};

use tilo_actions::remote::host_account::{NextStepKind, Reason, Refusal};
use tilo_actions::remote::{
    ActionHostError, Confirmation, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionFrame,
    RemoteActionMessage, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_relay::certify::ConfirmationAnswer;
use tilo_relay::hosted_browser::{
    BrowserEnvironments, BrowserMeter, HostedBrowsers, Material, PAGE_RESOURCE, Window,
    no_payment_method,
};
use tilo_relay::transport::REMOTE_ACTION_ENVELOPE_VERSION;
use tilo_relay::{AppServingPlane, Envelope, RelayClient};
use tilo_relay_client::Approver;
use tilo_user_identity::{
    DeviceCapabilities, HostWitness, RevocationList, prepare_stand_in_cert, sign_device_cert,
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
        workspace_id: u64,
        caller: &tilo_actions::remote::Caller,
        message: RemoteActionMessage,
    ) -> Option<tilo_actions::remote::Pending<RemoteActionMessage>> {
        // A page action that asks first is put to the person here, as the
        // relay does for its own agents.
        let RemoteActionMessage::Confirm {
            correlation,
            request,
        } = &message
        else {
            return None;
        };
        let correlation = *correlation;
        Some(
            self.0
                .confirm(workspace_id, caller, request)
                .map(move |result| RemoteActionMessage::ConfirmResponse {
                    correlation,
                    result: result.map(Box::new),
                }),
        )
    }

    fn route(
        &self,
        workspace_id: u64,
        sender: [u8; 32],
        message: &RemoteActionMessage,
    ) -> Option<tilo_relay::app_serving::Routing> {
        self.0.route(workspace_id, sender, message)
    }

    fn relayed(&self, workspace_id: u64, from: [u8; 32], message: &RemoteActionMessage) {
        self.0.relayed(workspace_id, from, message);
    }

    fn tracks(&self, workspace_id: u64) -> bool {
        self.0.tracks(workspace_id)
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

/// A page action with `confirmation` attached.
fn page_with(
    invocation: InvocationId,
    action: &str,
    confirmation: Option<Confirmation>,
) -> RemoteActionMessage {
    RemoteActionMessage::Invoke(InvokeRequest {
        on_behalf_of: None,
        invocation,
        target: RemoteActionTarget {
            provider: WINDOW.into(),
            address: ActionAddress::new(PAGE_RESOURCE, action),
        },
        argument: InvokeArgument::None,
        context: None,
        confirmation,
        revision: None,
        wait_ms: None,
        appearance: None,
    })
}

/// The question the agent puts to the relay about `invocation` of `action`.
fn confirm(correlation: u64, invocation: InvocationId, action: &str) -> RemoteActionMessage {
    let RemoteActionMessage::Invoke(mut request) = page_with(invocation, action, None) else {
        unreachable!("a page action is an invocation");
    };
    request.context = Some(tilo_actions::ActionContext {
        purpose: Some("Submit the application".into()),
        ..tilo_actions::ActionContext::default()
    });
    RemoteActionMessage::Confirm {
        correlation,
        request: Box::new(request),
    }
}

/// The invocation `envelope` answers.
fn receipt_of(envelope: Envelope) -> InvocationReceipt {
    let Envelope::RemoteAction { frame, .. } = envelope else {
        panic!("the caller is answered: {envelope:?}");
    };
    match RemoteActionFrame::decode(&frame).unwrap().message {
        RemoteActionMessage::Receipt(receipt) => receipt,
        other => panic!("an invocation is answered with a receipt: {other:?}"),
    }
}

/// The person's answer, as `envelope` carries it to them who asked.
fn confirmation_of(envelope: Envelope) -> Confirmation {
    let Envelope::RemoteAction { frame, .. } = envelope else {
        panic!("the caller is answered: {envelope:?}");
    };
    match RemoteActionFrame::decode(&frame).unwrap().message {
        RemoteActionMessage::ConfirmResponse {
            result: Ok(confirmed),
            ..
        } => confirmed.confirmation,
        other => panic!("the person allowed it: {other:?}"),
    }
}

/// The refusal the person's answer carries.
fn denied_of(envelope: Envelope) -> ActionHostError {
    let Envelope::RemoteAction { frame, .. } = envelope else {
        panic!("the caller is answered: {envelope:?}");
    };
    match RemoteActionFrame::decode(&frame).unwrap().message {
        RemoteActionMessage::ConfirmResponse {
            result: Err(error), ..
        } => error,
        other => panic!("the person declined: {other:?}"),
    }
}

/// The question the person's device sees waiting for them, once it is asked.
async fn asked(device: &mut Approver) -> tilo_relay::certify::PendingConfirmation {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let mut waiting = device.list_confirmations().await.unwrap();
        if let Some(question) = waiting.pop() {
            assert!(
                waiting.is_empty(),
                "one question per invocation: {waiting:?}"
            );
            return question;
        }
        assert!(Instant::now() < deadline, "the person was never asked");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
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

/// One page action a window was handed: who sent it, and whether it carried
/// the witnessed answer of the host that certified the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Handed {
    from: [u8; 32],
    witnessed: bool,
}

/// What a window does with a page action: run it, or — an action that asks
/// the person first — refuse it for want of their answer unless the window's
/// own host gave one for that invocation.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Asks {
    Never,
    WithoutThePersonsWord,
}

/// The device holding the workspace's Browser window: it answers every
/// invocation with a receipt and keeps who sent it.
async fn window_session(
    relay: &tilo_relay::Relay,
    user: &TestUser,
    material: &Material,
    asks: Asks,
) -> Arc<Mutex<Vec<Handed>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let device = DeviceIdentity::of(material);
    let client = subscribe(relay, user, &device).await;
    let heard = Arc::clone(&seen);
    let holder = material.device.device_public_key;
    let certifier = material
        .device
        .issuer
        .as_ref()
        .expect("the relay certified the environment's device")
        .device_public_key;
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
            // Only the host that certified this window, for this very
            // invocation and action, answers for the person.
            let witnessed = match &request.confirmation {
                Some(Confirmation::Witnessed(token)) => {
                    HostWitness::decode(token).is_some_and(|witness| {
                        witness.host_device_public_key == certifier
                            && witness.invocation == *request.invocation.0.as_bytes()
                            && witness.provider == request.target.provider
                            && witness.action == request.target.address.action
                            && witness.verify(tilo_clock::unix_now_ns()).is_ok()
                    })
                }
                _ => false,
            };
            heard.lock().unwrap().push(Handed {
                from: from_device_public_key,
                witnessed,
            });
            let status = if asks == Asks::WithoutThePersonsWord && !witnessed {
                ReceiptStatus::Rejected {
                    error: ActionHostError::ConfirmationRequired,
                }
            } else {
                ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput::default(),
                }
            };
            let answer = RemoteActionFrame::new(RemoteActionMessage::Receipt(InvocationReceipt {
                invocation: request.invocation,
                status,
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
    /// The person's own device, which answers what their agents ask them.
    laptop: TestDevice,
    relay: TestRelay,
    browsers: Arc<HostedBrowsers>,
    environments: Arc<InProcessEnvironments>,
    meter: Arc<Counting>,
}

async fn fixture(meter: Arc<Counting>) -> Fixture {
    let user = TestUser::fresh("Ada");
    let host = user.enrol("home", DeviceCapabilities::host());
    let laptop = user.enrol("laptop", DeviceCapabilities::primary());
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
        environments: relay.relay.environments(),
        runtime: tokio::runtime::Handle::current(),
        certification: relay.relay.confirmations(),
    });
    browsers
        .place(WS_NAME, WS, "record-1")
        .expect("the environment was placed");
    Fixture {
        user,
        home,
        agent,
        laptop,
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
    /// The person's approving device, connected to the relay: where a page
    /// action that asks first is put to them.
    async fn approving_device(&self) -> (iroh::endpoint::Endpoint, Approver) {
        let endpoint = client_endpoint().await;
        let approver = Approver::connect(
            &endpoint,
            self.relay.relay.node_addr(),
            self.laptop.cert.clone(),
            &self.laptop.sk,
            &RevocationList::new(),
        )
        .await
        .expect("the person's device asks the relay what waits for them");
        (endpoint, approver)
    }

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
    let seen = window_session(&fixture.relay.relay, &fixture.user, &material, Asks::Never).await;
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
        seen.lock()
            .unwrap()
            .iter()
            .map(|handed| handed.from)
            .collect::<Vec<_>>(),
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
    let seen = window_session(&fixture.relay.relay, &fixture.user, &material, Asks::Never).await;
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
    let seen = window_session(&fixture.relay.relay, &fixture.user, &material, Asks::Never).await;
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

/// An agent's page action that asks the person first: the window refuses it
/// without their answer, the relay puts the question to their approving
/// device for that one invocation, and only their answer runs it — counted
/// once, when it ran.
#[tokio::test]
async fn a_page_action_that_asks_first_waits_for_the_person_and_is_counted_once() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    fixture.list_environment().await;
    let material = fixture.environments.material();
    let handed = window_session(
        &fixture.relay.relay,
        &fixture.user,
        &material,
        Asks::WithoutThePersonsWord,
    )
    .await;
    let caller = subscribe(&fixture.relay.relay, &fixture.user, &fixture.agent).await;

    // Submitted without the person's answer: the window refuses it, nothing
    // ran, and nothing is counted.
    let id = InvocationId::new();
    caller
        .send(to_host(WS, fixture.home, frame(page(id, "submit-page"))))
        .unwrap();
    let refused = receipt_of(next(&caller, "caller").await);
    assert_eq!(
        refused.status,
        ReceiptStatus::Rejected {
            error: ActionHostError::ConfirmationRequired
        },
        "the agent is told the person's answer is needed"
    );
    assert_eq!(
        *fixture.meter.recorded.lock().unwrap(),
        0,
        "nothing ran to count"
    );

    // Exactly the same request again: the relay knows this one asks, so it is
    // refused here and the window is not troubled twice.
    caller
        .send(to_host(WS, fixture.home, frame(page(id, "submit-page"))))
        .unwrap();
    let again = receipt_of(next(&caller, "caller").await);
    assert_eq!(
        again.status,
        ReceiptStatus::Rejected {
            error: ActionHostError::ConfirmationRequired
        }
    );
    assert_eq!(handed.lock().unwrap().len(), 1, "the window saw it once");

    // The agent asks, and the person's own device answers.
    let (_endpoint, mut approver) = fixture.approving_device().await;
    caller
        .send(to_host(
            WS,
            fixture.home,
            frame(confirm(7, id, "submit-page")),
        ))
        .unwrap();
    let asked = asked(&mut approver).await;
    assert_eq!(asked.action_label, "Submit the application");
    assert_eq!(asked.resource_label, PAGE_RESOURCE);
    approver
        .answer(asked.id, ConfirmationAnswer::AllowOnce)
        .await
        .expect("the person answers");
    let Confirmation::Witnessed(token) = confirmation_of(next(&caller, "caller").await) else {
        panic!("their answer is a witnessed one");
    };

    // With their answer it reaches the window, which runs it as the agent's
    // without asking again, and it is counted once.
    caller
        .send(to_host(
            WS,
            fixture.home,
            frame(page_with(
                id,
                "submit-page",
                Some(Confirmation::Witnessed(token)),
            )),
        ))
        .unwrap();
    let ran = receipt_of(next(&caller, "caller").await);
    assert!(
        matches!(ran.status, ReceiptStatus::Completed { .. }),
        "{ran:?}"
    );
    assert_eq!(*fixture.meter.recorded.lock().unwrap(), 1, "counted once");
    let handed = handed.lock().unwrap();
    assert_eq!(handed.len(), 2, "the window saw it twice");
    assert_eq!(handed[0].from, fixture.agent.device_pubkey());
    assert!(!handed[0].witnessed, "the first had no answer to carry");
    assert!(
        handed[1].witnessed,
        "the second carried the person's answer"
    );
}

/// The person declining a page action refuses it: it never reaches the
/// window, and nothing is counted.
#[tokio::test]
async fn the_persons_denial_refuses_a_page_action_and_counts_nothing() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    fixture.list_environment().await;
    let material = fixture.environments.material();
    let handed = window_session(
        &fixture.relay.relay,
        &fixture.user,
        &material,
        Asks::WithoutThePersonsWord,
    )
    .await;
    let caller = subscribe(&fixture.relay.relay, &fixture.user, &fixture.agent).await;
    let (_endpoint, mut approver) = fixture.approving_device().await;

    let id = InvocationId::new();
    caller
        .send(to_host(WS, fixture.home, frame(page(id, "submit-page"))))
        .unwrap();
    let _ = receipt_of(next(&caller, "caller").await);

    caller
        .send(to_host(
            WS,
            fixture.home,
            frame(confirm(1, id, "submit-page")),
        ))
        .unwrap();
    let asked = asked(&mut approver).await;
    approver
        .answer(asked.id, ConfirmationAnswer::Deny)
        .await
        .expect("the person answers");
    assert_eq!(
        denied_of(next(&caller, "caller").await),
        ActionHostError::NotPermitted
    );
    assert_eq!(handed.lock().unwrap().len(), 1, "the window saw it once");
    assert_eq!(*fixture.meter.recorded.lock().unwrap(), 0, "nothing ran");
}

/// An answer names one invocation of one host: another invocation's answer,
/// or another host's, runs nothing and is counted not at all.
#[tokio::test]
async fn a_page_action_answer_names_one_invocation_of_one_host() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    fixture.list_environment().await;
    let material = fixture.environments.material();
    let handed = window_session(
        &fixture.relay.relay,
        &fixture.user,
        &material,
        Asks::WithoutThePersonsWord,
    )
    .await;
    let caller = subscribe(&fixture.relay.relay, &fixture.user, &fixture.agent).await;
    let (_endpoint, mut approver) = fixture.approving_device().await;

    let id = InvocationId::new();
    caller
        .send(to_host(WS, fixture.home, frame(page(id, "submit-page"))))
        .unwrap();
    let _ = receipt_of(next(&caller, "caller").await);
    caller
        .send(to_host(
            WS,
            fixture.home,
            frame(confirm(2, id, "submit-page")),
        ))
        .unwrap();
    let asked = asked(&mut approver).await;
    approver
        .answer(asked.id, ConfirmationAnswer::AllowOnce)
        .await
        .expect("the person answers");
    let Confirmation::Witnessed(token) = confirmation_of(next(&caller, "caller").await) else {
        panic!("a witnessed answer");
    };

    // The same answer for another invocation is no answer.
    let other = InvocationId::new();
    caller
        .send(to_host(
            WS,
            fixture.home,
            frame(page_with(
                other,
                "submit-page",
                Some(Confirmation::Witnessed(token.clone())),
            )),
        ))
        .unwrap();
    let refused = receipt_of(next(&caller, "caller").await);
    assert_eq!(
        refused.status,
        ReceiptStatus::Rejected {
            error: ActionHostError::NotPermitted
        }
    );

    // So is another host's word, even for this very invocation.
    let elsewhere = ed25519_dalek::SigningKey::from_bytes(&[0x11; 32]);
    let mut witness = HostWitness::decode(&token).expect("the answer names its witness");
    witness.host_device_public_key = elsewhere.verifying_key().to_bytes();
    witness.sign(&elsewhere).expect("the other host signs it");
    caller
        .send(to_host(
            WS,
            fixture.home,
            frame(page_with(
                id,
                "submit-page",
                Some(Confirmation::Witnessed(witness.encode())),
            )),
        ))
        .unwrap();
    let refused = receipt_of(next(&caller, "caller").await);
    assert_eq!(
        refused.status,
        ReceiptStatus::Rejected {
            error: ActionHostError::NotPermitted
        }
    );

    assert_eq!(
        handed.lock().unwrap().len(),
        1,
        "neither reached the window"
    );
    assert_eq!(*fixture.meter.recorded.lock().unwrap(), 0, "nothing ran");
}

/// The record a customer wrote before Browser opened: another app's.
const NOTES: &str = "so.tilo.notes";
const NOTES_RECORD: &str = "notes-1";

/// The channel the window's own record lives on, and the one a customer's
/// document lives on: what every plane of the workspace tells a record by.
fn window_channel() -> [u8; 32] {
    tilo_relay::environment::record_channel(tilo_relay::hosted_browser::APP, "record-1")
}

fn notes_channel() -> [u8; 32] {
    tilo_relay::environment::record_channel(NOTES, NOTES_RECORD)
}

/// One change of `channel`'s record, as a device writes it.
fn change(channel: [u8; 32], marker: &[u8]) -> Vec<u8> {
    tilo_relay::coedit::CoeditPayload::RetainedDelta {
        channel,
        body: marker.to_vec(),
    }
    .encode()
    .unwrap()
}

/// Every envelope a session hears in `within`, the ones it already holds first.
async fn heard(client: &RelayClient, within: std::time::Duration) -> Vec<Envelope> {
    let mut out = Vec::new();
    while let Ok(Some(envelope)) = tokio::time::timeout(within, client.recv()).await {
        out.push(envelope);
    }
    out
}

/// The record channel each change a session heard belongs to.
fn changed(envelopes: &[Envelope]) -> Vec<[u8; 32]> {
    envelopes
        .iter()
        .filter_map(|envelope| {
            let Envelope::Delta { delta, .. } = envelope else {
                return None;
            };
            match tilo_relay::coedit::CoeditPayload::decode(delta)? {
                tilo_relay::coedit::CoeditPayload::Checkpoint { channel, .. }
                | tilo_relay::coedit::CoeditPayload::RetainedDelta { channel, .. } => Some(channel),
            }
        })
        .collect()
}

impl Fixture {
    /// The environment's own session: the device the relay certified, joining
    /// the one workspace it was placed in.
    async fn environment_session(&self) -> RelayClient {
        let material = self.environments.material();
        let environment = DeviceIdentity::of(&material);
        subscribe(&self.relay.relay, &self.user, &environment).await
    }
}

#[tokio::test]
async fn the_relay_confines_an_environments_device_to_the_record_it_placed() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    let device = fixture.environments.material().device.device_public_key;
    assert_eq!(
        tilo_relay::environment::scope_of(&fixture.relay.relay.environments(), &device),
        Some(tilo_relay::environment::EnvironmentScope::for_record(
            WS,
            tilo_relay::hosted_browser::APP,
            "record-1"
        )),
        "the placement names the one record its device reaches"
    );
}

#[tokio::test]
async fn an_environment_hears_its_own_record_and_no_other() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    fixture.list_environment().await;
    let environment = fixture.environment_session().await;
    let writer = subscribe(&fixture.relay.relay, &fixture.user, &fixture.agent).await;
    for (channel, marker) in [
        (notes_channel(), b"a customer's document".as_slice()),
        (window_channel(), b"the window's own state".as_slice()),
    ] {
        writer
            .send(Envelope::Delta {
                workspace_id: WS,
                author_device_public_key: fixture.agent.device_pubkey(),
                authored_at_ns: tilo_clock::unix_now_ns(),
                delta: change(channel, marker),
                signature: [0; 64],
            })
            .unwrap();
    }
    let heard = heard(&environment, std::time::Duration::from_millis(500)).await;
    assert_eq!(
        changed(&heard),
        [window_channel()],
        "a page that escapes Chromium reads no other record of the workspace: {heard:?}"
    );
}

#[tokio::test]
async fn an_environment_writes_its_record_and_no_other() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    fixture.list_environment().await;
    let environment = fixture.environment_session().await;
    let device = fixture.environments.material().device.device_public_key;
    environment
        .send(Envelope::Delta {
            workspace_id: WS,
            author_device_public_key: device,
            authored_at_ns: tilo_clock::unix_now_ns(),
            delta: change(notes_channel(), b"overwritten by a page"),
            signature: [0; 64],
        })
        .unwrap();
    let refusal = next(&environment, "environment").await;
    let Envelope::NotAuthorised {
        workspace_id,
        reason,
    } = refusal
    else {
        panic!("a write to another record is refused: {refusal:?}");
    };
    assert_eq!(workspace_id, WS);
    assert!(
        reason.contains("environment"),
        "the refusal names the rule: {reason}"
    );

    environment
        .send(Envelope::Delta {
            workspace_id: WS,
            author_device_public_key: device,
            authored_at_ns: tilo_clock::unix_now_ns(),
            delta: change(window_channel(), b"the page clicked"),
            signature: [0; 64],
        })
        .unwrap();
    let tolerance = heard(&environment, std::time::Duration::from_millis(300)).await;
    assert!(
        !tolerance
            .iter()
            .any(|envelope| matches!(envelope, Envelope::NotAuthorised { .. })),
        "its own record is its to write: {tolerance:?}"
    );
    let retained: Vec<Envelope> =
        tilo_relay::store::WorkspaceStore::open(&fixture.relay.state_root, WS, true)
            .unwrap()
            .load_retained()
            .unwrap()
            .into_iter()
            .filter_map(|frame| {
                tilo_relay::frame::decode(frame.get(4..)?)
                    .ok()
                    .filter(|envelope| matches!(envelope, Envelope::Delta { .. }))
            })
            .collect();
    assert_eq!(
        changed(&retained),
        [window_channel()],
        "the window's own write is the only record this workspace keeps from it"
    );
}

#[tokio::test]
async fn an_environment_opens_its_own_record_and_no_other() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    fixture.list_environment().await;
    let environment = fixture.environment_session().await;
    environment
        .send(Envelope::OpenRecord {
            workspace_id: WS,
            channel: notes_channel(),
        })
        .unwrap();
    let refusal = next(&environment, "environment").await;
    assert!(
        matches!(&refusal, Envelope::NotAuthorised { reason, .. } if reason.contains("environment")),
        "another record does not open for it: {refusal:?}"
    );
    environment
        .send(Envelope::OpenRecord {
            workspace_id: WS,
            channel: window_channel(),
        })
        .unwrap();
    let answer = next(&environment, "environment").await;
    assert!(
        matches!(answer, Envelope::RecordOpened { channel, .. } if channel == window_channel()),
        "its own record opens: {answer:?}"
    );
}

#[tokio::test]
async fn an_environments_replay_is_its_own_record_and_no_other() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    fixture.list_environment().await;
    let writer = subscribe(&fixture.relay.relay, &fixture.user, &fixture.agent).await;
    for (channel, marker) in [
        (notes_channel(), b"a customer's document".as_slice()),
        (window_channel(), b"the window's own state".as_slice()),
    ] {
        writer
            .send(Envelope::Delta {
                workspace_id: WS,
                author_device_public_key: fixture.agent.device_pubkey(),
                authored_at_ns: tilo_clock::unix_now_ns(),
                delta: change(channel, marker),
                signature: [0; 64],
            })
            .unwrap();
    }
    // It joins after both records were written: what reaches it — queued
    // before its subscription, then replayed behind it — is its own record.
    let material = fixture.environments.material();
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
    .expect("connect");
    client
        .send(Envelope::Subscribe { workspace_id: WS })
        .unwrap();
    let mut heard = Vec::new();
    let mut ready = false;
    while let Ok(Some(envelope)) =
        tokio::time::timeout(std::time::Duration::from_millis(700), client.recv()).await
    {
        ready |= matches!(envelope, Envelope::Ready { .. });
        heard.push(envelope);
    }
    assert!(ready, "the environment's subscription is answered");
    assert_eq!(
        changed(&heard),
        [window_channel()],
        "the replay of another record stays out: {heard:?}"
    );
}

#[tokio::test]
async fn an_environment_answers_a_page_action_and_never_calls_one() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    fixture.list_environment().await;
    let material = fixture.environments.material();
    let seen = window_session(&fixture.relay.relay, &fixture.user, &material, Asks::Never).await;
    let environment = fixture.environment_session().await;
    // The window's device calls the workspace's door itself: not its to do.
    environment
        .send(Envelope::RemoteAction {
            workspace_id: WS,
            version: REMOTE_ACTION_ENVELOPE_VERSION,
            to_device_public_key: fixture.home,
            from_device_public_key: material.device.device_public_key,
            from_user_public_key: material.user_public_key,
            frame: frame(RemoteActionMessage::Invoke(InvokeRequest {
                on_behalf_of: None,
                invocation: InvocationId::new(),
                target: RemoteActionTarget {
                    provider: "so.tilo.notes".into(),
                    address: ActionAddress::new("notes", "delete"),
                },
                argument: InvokeArgument::None,
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: None,
                appearance: None,
            })),
        })
        .unwrap();
    let answer = next(&environment, "environment").await;
    assert!(
        matches!(answer, Envelope::RemoteActionUndeliverable { .. }),
        "a window's device calls no door of the workspace: {answer:?}"
    );
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_stopped_environment_reaches_nothing() {
    let fixture = fixture(Arc::new(Counting::default())).await;
    fixture.list_environment().await;
    let material = fixture.environments.material();
    let device = material.device.device_public_key;
    fixture.browsers.stopped(WS);
    assert_eq!(
        tilo_relay::environment::scope_of(&fixture.relay.relay.environments(), &device),
        None,
        "a stopped environment's device is forgotten"
    );
    // The relay forgets its device from the workspace as it stops it (`Workspaces`'
    // listing, which this fixture's host has none of).
    admit_members(
        &fixture.relay.relay,
        WS,
        &[fixture.agent.device_pubkey()],
        &fixture.relay.state_root,
    )
    .await;
    let environment = DeviceIdentity::of(&material);
    let refused = RelayClient::connect(
        client_endpoint().await,
        fixture.relay.relay.node_addr(),
        fixture.user.user_pk,
        environment.cert.clone(),
        environment.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("the environment's device dials the host");
    refused
        .send(Envelope::Subscribe { workspace_id: WS })
        .unwrap();
    let answer = next(&refused, "environment").await;
    assert!(
        !matches!(answer, Envelope::Ready { .. }),
        "a stopped environment's device is no device of the workspace: {answer:?}"
    );
}
