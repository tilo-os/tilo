#![allow(
    clippy::needless_pass_by_value,
    reason = "the stand-in BlueZ's methods take what their D-Bus signatures hand over"
)]

use std::sync::{Arc, Mutex};

use tilo_bluez::{AGENT_CAPABILITY, AGENT_PATH, DeviceNames};
use tilo_service::{Answer, Question, Questions};
use tokio::sync::mpsc;
use zbus::zvariant::{ObjectPath, OwnedObjectPath};
use zeroize::Zeroizing;

use super::*;

const DEVICE: &str = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF";
const OTHER_DEVICE: &str = "/org/bluez/hci0/dev_11_22_33_44_55_66";
const GUID: &str = "0123456789abcdef0123456789abcdef";

type Calls = Arc<Mutex<Vec<String>>>;

fn record(calls: &Calls, call: String) {
    calls.lock().expect("calls").push(call);
}

/// `BlueZ`'s agent manager, recording each call.
struct FakeAgentManager {
    calls: Calls,
}

#[zbus::interface(name = "org.bluez.AgentManager1")]
impl FakeAgentManager {
    fn register_agent(&self, agent: OwnedObjectPath, capability: String) {
        record(
            &self.calls,
            format!("register {} {capability}", agent.as_str()),
        );
    }

    fn request_default_agent(&self, agent: OwnedObjectPath) {
        record(&self.calls, format!("default {}", agent.as_str()));
    }

    fn unregister_agent(&self, agent: OwnedObjectPath) {
        record(&self.calls, format!("unregister {}", agent.as_str()));
    }
}

/// The device being paired.
struct FakeDevice {
    calls: Calls,
    paired: bool,
}

#[zbus::interface(name = "org.bluez.Device1")]
impl FakeDevice {
    fn cancel_pairing(&self) {
        record(&self.calls, "cancel-pairing".to_owned());
    }

    #[zbus(property)]
    fn paired(&self) -> bool {
        self.paired
    }
}

/// The agent asking in fresh questions, registered with a stand-in
/// `BlueZ` over a peer-to-peer link.
struct Setup {
    questions: Questions,
    changes: mpsc::UnboundedReceiver<()>,
    bluez: zbus::Connection,
    calls: Calls,
    service: PairingService,
}

async fn setup(timeout: Duration) -> Setup {
    let questions = Questions::new();
    let (notify, changes) = mpsc::unbounded_channel();
    questions.on_change(move || {
        let _ = notify.send(());
    });
    let calls = Calls::default();
    let (agent_socket, bluez_socket) = tokio::net::UnixStream::pair().expect("socket pair");
    let agent_side = zbus::connection::Builder::unix_stream(agent_socket)
        .server(GUID)
        .expect("guid")
        .p2p();
    let bluez_side = zbus::connection::Builder::unix_stream(bluez_socket)
        .p2p()
        .serve_at(
            "/org/bluez",
            FakeAgentManager {
                calls: Arc::clone(&calls),
            },
        )
        .expect("agent manager")
        .serve_at(
            DEVICE,
            FakeDevice {
                calls: Arc::clone(&calls),
                paired: false,
            },
        )
        .expect("device");
    let (agent_conn, bluez) =
        futures_util::try_join!(agent_side.build(), bluez_side.build()).expect("p2p pair");
    let names = DeviceNames::new();
    names.set(DEVICE, "Studio Headphones");
    let client = BluezClient::with_connection(agent_conn, names);
    let service = PairingService::start_with(client, questions.clone(), timeout)
        .await
        .expect("registered");
    Setup {
        questions,
        changes,
        bluez,
        calls,
        service,
    }
}

fn deadline() -> tokio::time::Instant {
    tokio::time::Instant::from_std(tilo_test_env::hang_deadline())
}

impl Setup {
    /// The question pending once `ready` holds for it.
    async fn question_where(&mut self, ready: impl Fn(&Question) -> bool) -> Question {
        let deadline = deadline();
        loop {
            if let Some(question) = self.questions.pending().into_iter().find(&ready) {
                return question;
            }
            tokio::time::timeout_at(deadline, self.changes.recv())
                .await
                .expect("a question in time")
                .expect("the questions stay alive");
        }
    }

    async fn question(&mut self) -> Question {
        self.question_where(|_| true).await
    }

    /// Once no question is pending.
    async fn none_pending(&mut self) {
        let deadline = deadline();
        while !self.questions.pending().is_empty() {
            tokio::time::timeout_at(deadline, self.changes.recv())
                .await
                .expect("withdrawn in time")
                .expect("the questions stay alive");
        }
    }

    async fn agent(&self) -> zbus::Proxy<'static> {
        zbus::Proxy::new(
            &self.bluez,
            "org.tilo.Agent",
            AGENT_PATH,
            "org.bluez.Agent1",
        )
        .await
        .expect("agent proxy")
    }

    /// Call `method` on the agent as `BlueZ` does, without waiting for it.
    async fn call<A, R>(
        &self,
        method: &'static str,
        args: A,
    ) -> tokio::task::JoinHandle<zbus::Result<R>>
    where
        A: serde::Serialize + zbus::zvariant::DynamicType + Send + Sync + 'static,
        R: for<'d> zbus::zvariant::DynamicDeserialize<'d> + Send + 'static,
    {
        let agent = self.agent().await;
        tokio::spawn(async move { agent.call(method, &args).await })
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("calls").clone()
    }

    async fn until_called(&self, call: &str) {
        let deadline = deadline();
        while !self.calls().iter().any(|made| made == call) {
            assert!(tokio::time::Instant::now() < deadline, "{call} in time");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

fn device(path: &str) -> ObjectPath<'static> {
    ObjectPath::try_from(path.to_owned()).expect("device path")
}

fn details(question: &Question) -> PairingDetails {
    serde_json::from_value(question.details.clone()).expect("pairing details")
}

fn agree(typed: Option<&str>) -> Answer {
    Answer {
        choice: Some(0),
        values: typed
            .map(|typed| Zeroizing::new(typed.to_owned()))
            .into_iter()
            .collect(),
        ..Answer::default()
    }
}

fn rejected(result: &zbus::Result<impl std::fmt::Debug>) -> bool {
    matches!(result, Err(zbus::Error::MethodError(name, _, _)) if name.as_str() == "org.bluez.Error.Rejected")
}

#[tokio::test]
async fn a_passkey_to_compare_is_a_question_and_confirming_pairs() {
    let mut setup = setup(Duration::from_secs(30)).await;
    assert_eq!(
        setup.calls(),
        [
            format!("register {AGENT_PATH} {AGENT_CAPABILITY}"),
            format!("default {AGENT_PATH}"),
        ],
        "registered as the default agent"
    );
    let call = setup
        .call::<_, ()>("RequestConfirmation", (device(DEVICE), 123_456_u32))
        .await;
    let question = setup.question().await;
    assert_eq!(question.kind, KIND);
    assert_eq!(question.label, "Studio Headphones");
    assert!(
        question.handle.starts_with("bluetooth:"),
        "{}",
        question.handle
    );
    assert_eq!(
        details(&question),
        PairingDetails {
            device: "Studio Headphones".into(),
            device_path: DEVICE.into(),
            method: Method::ConfirmPasskey,
            code: Some("123456".into()),
            entered: None,
            service: None,
            service_key: None,
        }
    );
    assert!(setup.questions.answer(&question.handle, agree(None)));
    call.await.expect("call task").expect("paired");
    assert!(setup.questions.pending().is_empty());

    let calls = Arc::clone(&setup.calls);
    setup.service.shutdown().await;
    assert!(
        calls
            .lock()
            .expect("calls")
            .contains(&format!("unregister {AGENT_PATH}")),
        "unregistered on shutdown"
    );
}

#[tokio::test]
async fn declining_refuses_the_pairing() {
    let mut setup = setup(Duration::from_secs(30)).await;
    let call = setup
        .call::<_, ()>("RequestAuthorization", (device(DEVICE),))
        .await;
    let question = setup.question().await;
    assert_eq!(details(&question).method, Method::ConfirmPairing);
    assert!(setup.questions.decline(&question.handle));
    let result = call.await.expect("call task");
    assert!(rejected(&result), "{result:?}");
}

#[tokio::test]
async fn typed_passkeys_and_pins_reach_bluez_as_typed() {
    let mut setup = setup(Duration::from_secs(30)).await;
    let call = setup
        .call::<_, u32>("RequestPasskey", (device(DEVICE),))
        .await;
    let question = setup.question().await;
    assert_eq!(details(&question).method, Method::EnterPasskey);
    assert!(
        setup
            .questions
            .answer(&question.handle, agree(Some("654321")))
    );
    assert_eq!(call.await.expect("call task").expect("passkey"), 654_321);

    let call = setup
        .call::<_, String>("RequestPinCode", (device(DEVICE),))
        .await;
    let question = setup.question().await;
    assert_eq!(details(&question).method, Method::EnterPin);
    assert!(
        setup
            .questions
            .answer(&question.handle, agree(Some(" 0000 ")))
    );
    assert_eq!(call.await.expect("call task").expect("PIN"), "0000");

    let call = setup
        .call::<_, u32>("RequestPasskey", (device(DEVICE),))
        .await;
    let question = setup.question().await;
    assert!(
        setup
            .questions
            .answer(&question.handle, agree(Some("1000000")))
    );
    let result = call.await.expect("call task");
    assert!(
        rejected(&result),
        "a passkey past six digits refuses: {result:?}"
    );
}

#[tokio::test]
async fn a_second_question_declines_the_first() {
    let mut setup = setup(Duration::from_secs(30)).await;
    let first = setup
        .call::<_, ()>("RequestConfirmation", (device(DEVICE), 1_u32))
        .await;
    let _ = setup.question().await;
    let second = setup
        .call::<_, ()>("RequestAuthorization", (device(OTHER_DEVICE),))
        .await;
    let result = first.await.expect("call task");
    assert!(rejected(&result), "{result:?}");
    let question = setup
        .question_where(|question| details(question).device_path == OTHER_DEVICE)
        .await;
    assert_eq!(setup.questions.pending().len(), 1, "never two at once");
    assert_eq!(question.label, "11:22:33:44:55:66", "named by its address");
    assert!(setup.questions.answer(&question.handle, agree(None)));
    second.await.expect("call task").expect("accepted");
}

#[tokio::test]
async fn a_code_on_show_follows_the_device_and_declining_it_stops_the_pairing() {
    let mut setup = setup(Duration::from_secs(30)).await;
    setup
        .call::<_, ()>("DisplayPasskey", (device(DEVICE), 42_u32, 0_u16))
        .await
        .await
        .expect("call task")
        .expect("shown");
    let first = setup.question().await;
    assert_eq!(details(&first).method, Method::DisplayPasskey);
    assert_eq!(details(&first).code.as_deref(), Some("000042"));
    setup
        .call::<_, ()>("DisplayPasskey", (device(DEVICE), 42_u32, 3_u16))
        .await
        .await
        .expect("call task")
        .expect("shown");
    let updated = setup
        .question_where(|question| details(question).entered == Some(3))
        .await;
    assert_eq!(
        updated.handle, first.handle,
        "the same question, shown anew"
    );
    assert_eq!(setup.questions.pending().len(), 1);
    assert!(setup.questions.decline(&updated.handle));
    setup.until_called("cancel-pairing").await;
}

#[tokio::test]
async fn a_code_on_show_goes_once_the_device_is_paired() {
    let mut setup = setup(Duration::from_secs(30)).await;
    setup
        .call::<_, ()>("DisplayPinCode", (device(DEVICE), "0000".to_owned()))
        .await
        .await
        .expect("call task")
        .expect("shown");
    let question = setup.question().await;
    assert_eq!(details(&question).method, Method::DisplayPin);
    assert_eq!(details(&question).code.as_deref(), Some("0000"));
    let device = setup
        .bluez
        .object_server()
        .interface::<_, FakeDevice>(DEVICE)
        .await
        .expect("device");
    device.get_mut().await.paired = true;
    // The agent follows the device once it has asked; tell it until it
    // has heard.
    let deadline = deadline();
    while !setup.questions.pending().is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "taken down in time");
        device
            .get()
            .await
            .paired_changed(device.signal_emitter())
            .await
            .expect("signal");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!setup.calls().iter().any(|call| call == "cancel-pairing"));
}

#[tokio::test]
async fn bluez_cancelling_withdraws_the_question() {
    let mut setup = setup(Duration::from_secs(30)).await;
    let call = setup
        .call::<_, ()>("RequestConfirmation", (device(DEVICE), 1_u32))
        .await;
    let _ = setup.question().await;
    setup
        .call::<_, ()>("Cancel", ())
        .await
        .await
        .expect("call task")
        .expect("cancelled");
    setup.none_pending().await;
    let result = call.await.expect("call task");
    assert!(rejected(&result), "{result:?}");
}

#[tokio::test]
async fn an_unanswered_question_expires_without_consenting() {
    let mut setup = setup(Duration::from_millis(100)).await;
    let call = setup
        .call::<_, ()>("RequestConfirmation", (device(DEVICE), 1_u32))
        .await;
    let _ = setup.question().await;
    let result = call.await.expect("call task");
    assert!(
        matches!(&result, Err(zbus::Error::MethodError(name, _, _)) if name.as_str() == "org.bluez.Error.Canceled"),
        "nobody answered: {result:?}"
    );
    setup.none_pending().await;
}

fn typed(prompt: PairingPrompt, choice: Option<usize>, entry: Option<&str>) -> PairingReply {
    let answer = Answer {
        choice,
        values: entry
            .map(|entry| Zeroizing::new(entry.to_owned()))
            .into_iter()
            .collect(),
        ..Answer::default()
    };
    reply(&prompt, Some(&answer))
}

#[test]
fn answers_are_exact_and_refuse_rather_than_guess() {
    let passkey = || PairingPrompt::ConfirmPasskey { passkey: 424_242 };
    let long_pin = "9".repeat(MAX_PIN_CODE_BYTES + 1);
    let cases = [
        (passkey(), Some(0), None, PairingReply::Accept),
        (passkey(), Some(1), None, PairingReply::Reject),
        (passkey(), None, None, PairingReply::Reject),
        (
            PairingPrompt::RequestPasskey,
            Some(0),
            Some("654321"),
            PairingReply::Passkey(654_321),
        ),
        (
            PairingPrompt::RequestPasskey,
            Some(0),
            Some(""),
            PairingReply::Reject,
        ),
        (
            PairingPrompt::RequestPasskey,
            Some(0),
            Some("   "),
            PairingReply::Reject,
        ),
        (
            PairingPrompt::RequestPasskey,
            Some(0),
            None,
            PairingReply::Reject,
        ),
        (
            PairingPrompt::RequestPasskey,
            Some(0),
            Some("abcdef"),
            PairingReply::Reject,
        ),
        (
            PairingPrompt::RequestPasskey,
            Some(0),
            Some("1000000"),
            PairingReply::Reject,
        ),
        (
            PairingPrompt::RequestPinCode,
            Some(0),
            Some("0000"),
            PairingReply::PinCode("0000".into()),
        ),
        (
            PairingPrompt::RequestPinCode,
            Some(0),
            Some(long_pin.as_str()),
            PairingReply::Reject,
        ),
        (
            PairingPrompt::DisplayPasskey {
                passkey: 1,
                entered: 0,
            },
            Some(0),
            None,
            PairingReply::Reject,
        ),
        (
            PairingPrompt::AuthorizeService {
                uuid: "0000110b-0000-1000-8000-00805f9b34fb".into(),
            },
            Some(0),
            None,
            PairingReply::Accept,
        ),
    ];
    for (prompt, choice, entry, expected) in cases {
        let case = format!("{prompt:?} choice {choice:?} entry {entry:?}");
        assert_eq!(typed(prompt, choice, entry), expected, "{case}");
    }
    assert_eq!(reply(&passkey(), None), PairingReply::Reject, "declined");
}

#[test]
fn bluetooth_services_are_named_or_left_out_never_shown_as_uuids() {
    assert_eq!(
        service_key("0000110B-0000-1000-8000-00805F9B34FB"),
        Some("bluetooth-service-audio")
    );
    assert_eq!(
        service_key("00001124-0000-1000-8000-00805f9b34fb"),
        Some("bluetooth-service-input")
    );
    assert_eq!(service_key("0000fe2c-0000-1000-8000-00805f9b34fb"), None);
    assert_eq!(service_key("6e400001-b5a3-f393-e0a9-e50e24dcca9e"), None);
}

#[test]
fn a_service_request_names_the_service_by_what_it_does() {
    let request = PairingRequest {
        device_path: DEVICE.into(),
        device_name: "Studio Headphones".into(),
        prompt: PairingPrompt::AuthorizeService {
            uuid: "0000110b-0000-1000-8000-00805f9b34fb".into(),
        },
    };
    let shown = PairingDetails::from(&request);
    assert_eq!(shown.method, Method::AuthorizeService);
    assert_eq!(shown.code, None);
    assert_eq!(
        shown.service_key.as_deref(),
        Some("bluetooth-service-audio")
    );
    assert_eq!(
        serde_json::to_value(&shown).expect("json")["method"],
        "authorize_service"
    );
}
