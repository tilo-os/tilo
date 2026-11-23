#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "isolated D-Bus fixtures must construct successfully before production-path assertions, and one end-to-end journey reads as one test"
)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tilo_test_env::PrivateBus;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use super::*;
use crate::routing::LinkState;

const MODEM_PATH: &str = "/org/freedesktop/ModemManager1/Modem/7";
const SIM_PATH: &str = "/org/freedesktop/ModemManager1/SIM/7";
const MESSAGE_PATH: &str = "/org/freedesktop/ModemManager1/SMS/9";
const ICCID: &str = "8901000000000000001";

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Fault {
    #[default]
    None,
    SwapAfterCreate,
    WrongRecipient,
    MissingMember,
    AlreadySent,
    IncomingPdu,
    FailSend,
    DelaySend,
    DelayAfterCreate,
    SwapOnRead,
    ChangeOnRead,
    RevokeAfterCreate,
}
#[derive(Default)]
struct SimPermission(AtomicBool);
impl SimAuthority for SimPermission {
    fn check(&self) -> Result<(), Error> {
        if self.0.load(Ordering::Acquire) {
            Err(Error::WrongAccount)
        } else {
            Ok(())
        }
    }
}
#[derive(Default)]
struct Evidence {
    iccid: String,
    number: String,
    text: String,
    creates: usize,
    sends: usize,
    fault: Fault,
    incoming: bool,
    incoming_state: u32,
    incoming_pdu: u32,
    class: i32,
    data: Vec<u8>,
    timestamp: String,
    state_reads: usize,
    inbox_copies: usize,
    permission: Arc<SimPermission>,
    journal_at_revoke: Option<std::path::PathBuf>,
    revoked_bytes: Option<Vec<u8>>,
}
#[derive(Clone)]
struct Modem(OwnedObjectPath);
#[zbus::interface(name = "org.freedesktop.ModemManager1.Modem")]
impl Modem {
    #[zbus(property)]
    fn sim(&self) -> OwnedObjectPath {
        self.0.clone()
    }
}
#[derive(Clone)]
struct Sim(Arc<Mutex<Evidence>>);
#[zbus::interface(name = "org.freedesktop.ModemManager1.Sim")]
impl Sim {
    #[zbus(property)]
    async fn sim_identifier(&self) -> String {
        let (iccid, delay) = {
            let state = self.0.lock().unwrap();
            (
                state.iccid.clone(),
                matches!(state.fault, Fault::DelayAfterCreate | Fault::DelaySend)
                    && state.creates == 1,
            )
        };
        // Deliberately exceed the old fixture's 250ms wall budget before Send.
        // Response-timeout tests must observe Send before advancing their clock.
        if delay {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        iccid
    }
}
#[derive(Clone)]
struct Messaging(Arc<Mutex<Evidence>>);
#[zbus::interface(name = "org.freedesktop.ModemManager1.Modem.Messaging")]
impl Messaging {
    fn create(&self, mut properties: HashMap<String, OwnedValue>) -> OwnedObjectPath {
        assert_eq!(properties.len(), 3);
        assert!(bool::try_from(properties.remove("delivery-report-request").unwrap()).unwrap());
        let mut state = self.0.lock().unwrap();
        state.creates = state.creates.checked_add(1).unwrap();
        state.number = String::try_from(properties.remove("number").unwrap()).unwrap();
        state.text = String::try_from(properties.remove("text").unwrap()).unwrap();
        if state.fault == Fault::SwapAfterCreate {
            state.iccid = "8901000000000000002".into();
        }
        if state.fault == Fault::RevokeAfterCreate {
            state.revoked_bytes =
                Some(std::fs::read(state.journal_at_revoke.as_ref().unwrap()).unwrap());
            state.permission.0.store(true, Ordering::Release);
        }
        drop(state);
        MESSAGE_PATH.try_into().unwrap()
    }
    #[zbus(property)]
    fn messages(&self) -> Vec<OwnedObjectPath> {
        let state = self.0.lock().unwrap();
        if (!state.incoming && state.creates == 0) || state.fault == Fault::MissingMember {
            Vec::new()
        } else {
            vec![MESSAGE_PATH.try_into().unwrap(); state.inbox_copies.max(1)]
        }
    }
}
#[derive(Clone)]
struct Sms(Arc<Mutex<Evidence>>);
#[zbus::interface(name = "org.freedesktop.ModemManager1.Sms")]
impl Sms {
    async fn send(&self) -> zbus::fdo::Result<()> {
        let (fail, delay) = {
            let mut state = self.0.lock().unwrap();
            state.sends = state.sends.checked_add(1).unwrap();
            (
                state.fault == Fault::FailSend,
                state.fault == Fault::DelaySend,
            )
        };
        if delay {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        if fail {
            Err(zbus::fdo::Error::Failed("fixture failure".into()))
        } else {
            Ok(())
        }
    }
    #[zbus(property)]
    fn number(&self) -> String {
        let state = self.0.lock().unwrap();
        if state.fault == Fault::WrongRecipient {
            "+12025550199".into()
        } else {
            state.number.clone()
        }
    }
    #[zbus(property)]
    fn text(&self) -> String {
        let mut state = self.0.lock().unwrap();
        let text = state.text.clone();
        if state.fault == Fault::SwapOnRead {
            state.iccid = "8901000000000000002".into();
        }
        if state.fault == Fault::ChangeOnRead {
            state.text.push('!');
        }
        drop(state);
        text
    }
    #[zbus(property)]
    fn state(&self) -> u32 {
        let mut state = self.0.lock().unwrap();
        state.state_reads = state.state_reads.checked_add(1).unwrap();
        if state.incoming {
            return state.incoming_state;
        }
        if state.fault == Fault::AlreadySent {
            5
        } else {
            0
        }
    }
    #[zbus(property)]
    fn pdu_type(&self) -> u32 {
        let state = self.0.lock().unwrap();
        if state.incoming {
            return state.incoming_pdu;
        }
        if state.fault == Fault::IncomingPdu {
            1
        } else {
            2
        }
    }
    #[zbus(property)]
    fn class(&self) -> i32 {
        self.0.lock().unwrap().class
    }
    #[zbus(property)]
    fn data(&self) -> Vec<u8> {
        self.0.lock().unwrap().data.clone()
    }
    #[zbus(property)]
    fn timestamp(&self) -> String {
        self.0.lock().unwrap().timestamp.clone()
    }
    #[zbus(property)]
    fn delivery_report_request(&self) -> bool {
        self.0.lock().unwrap().creates > 0
    }
}

struct Fixture {
    bus: PrivateBus,
    server: zbus::Connection,
    client: zbus::Connection,
    state: Arc<Mutex<Evidence>>,
}
impl Fixture {
    async fn new() -> Self {
        let bus = PrivateBus::start();
        let state = Arc::new(Mutex::new(Evidence {
            iccid: ICCID.into(),
            ..Evidence::default()
        }));
        let server = zbus::connection::Builder::address(bus.address())
            .unwrap()
            .name("org.freedesktop.ModemManager1")
            .unwrap()
            .serve_at(MODEM_PATH, Modem(SIM_PATH.try_into().unwrap()))
            .unwrap()
            .serve_at(MODEM_PATH, Messaging(state.clone()))
            .unwrap()
            .serve_at(SIM_PATH, Sim(state.clone()))
            .unwrap()
            .serve_at(MESSAGE_PATH, Sms(state.clone()))
            .unwrap()
            .build()
            .await
            .unwrap();
        let client = zbus::connection::Builder::address(bus.address())
            .unwrap()
            .build()
            .await
            .unwrap();
        Self {
            bus,
            server,
            client,
            state,
        }
    }
    fn runtime(&self, root: &Path) -> SmsRuntime {
        let modem = SimBinding::new(
            self.server.unique_name().unwrap().to_string(),
            MODEM_PATH.into(),
            SIM_PATH.into(),
            ICCID.into(),
            7,
        )
        .unwrap();
        let journal =
            EncryptedJournal::open(root, &owner(), secret_vault::VaultKey::from_bytes([13; 32]))
                .unwrap();
        SmsRuntime::new(
            owner(),
            DbusTransport::new(
                self.client.clone(),
                modem,
                self.state.lock().unwrap().permission.clone(),
            )
            .unwrap(),
            journal,
        )
        .unwrap()
    }
}
struct DeadlineFixture {
    fixture: Fixture,
    io: Option<tokio::runtime::Runtime>,
}
impl DeadlineFixture {
    async fn new() -> Self {
        let io = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let fixture = io.spawn(Fixture::new()).await.unwrap();
        Self {
            fixture,
            io: Some(io),
        }
    }
}
impl Drop for DeadlineFixture {
    fn drop(&mut self) {
        if let Some(io) = self.io.take() {
            io.shutdown_background();
        }
    }
}

/// Budget for a send or read the fixture answers at once, when the test needs
/// it to reach the modem. A send's budget also covers the durable
/// reservation, whose fsync takes seconds on a loaded host, and a read's
/// several D-Bus round trips stall just as long there, so give the runtime's
/// maximum: only a hang uses it up.
const ANSWERED_BUDGET: Duration = Duration::from_secs(60);

fn owner() -> Owner {
    Owner {
        actor: "11".repeat(32),
        device: "fixture-device".into(),
        service: Service::Sms,
        account: "fixture-sim".into(),
        linking_generation: 1,
    }
}
fn binding() -> LiveBinding {
    LiveBinding {
        owner: owner(),
        state: LinkState::Linked,
    }
}
fn accounts() -> accounts::person::AccountList {
    let mut accounts = accounts::person::AccountList::default();
    accounts
        .keep(&accounts::Account::new("fixture-sim", "sms"))
        .unwrap();
    accounts
}
fn route() -> Route {
    Route::new(Service::Sms, "fixture-sim".into(), "+12025550123".into()).unwrap()
}
#[tokio::test]
async fn sms_send_journey_from_binding_through_refusal_and_uncertainty() {
    // The binding is bound to its exact owner, the current hardware and the
    // original permission; a resume that cannot prove all three stops here.
    let fixture = Fixture::new().await;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("sim.enc");
    let key = secret_vault::VaultKey::from_slice(&[0x44; 32]).unwrap();
    let original = SimBinding::new(
        fixture.server.unique_name().unwrap().to_string(),
        MODEM_PATH.into(),
        SIM_PATH.into(),
        ICCID.into(),
        7,
    )
    .unwrap();
    original.save(&path, &owner(), &key).unwrap();
    let ciphertext = std::fs::read(&path).unwrap();
    assert!(
        !ciphertext
            .windows(ICCID.len())
            .any(|bytes| bytes == ICCID.as_bytes())
    );
    let restored = SimBinding::load(&path, &owner(), &key, &fixture.client)
        .await
        .unwrap();
    let permission = fixture.state.lock().unwrap().permission.clone();
    DbusTransport::new(fixture.client.clone(), restored, permission.clone())
        .unwrap()
        .validate()
        .await
        .unwrap();
    let mut foreign = owner();
    foreign.account = "foreign-sim-account".into();
    assert!(matches!(
        SimBinding::load(&path, &foreign, &key, &fixture.client).await,
        Err(Error::WrongAccount)
    ));
    fixture.state.lock().unwrap().iccid = "8901000000000000002".into();
    let changed = SimBinding::load(&path, &owner(), &key, &fixture.client)
        .await
        .unwrap();
    assert_eq!(
        DbusTransport::new(fixture.client.clone(), changed, permission.clone())
            .unwrap()
            .validate()
            .await,
        Err(Error::WrongAccount)
    );
    permission.0.store(true, Ordering::Release);
    let restored = SimBinding::load(&path, &owner(), &key, &fixture.client)
        .await
        .unwrap();
    assert!(DbusTransport::new(fixture.client.clone(), restored, permission).is_err());
    assert_eq!(std::fs::read(path).unwrap(), ciphertext);
    assert_eq!(fixture.state.lock().unwrap().creates, 0);
    assert_eq!(fixture.state.lock().unwrap().sends, 0);

    // A real Send is submitted once, its reservation is durable and encrypted,
    // and neither a restart nor a repeat can submit it again.
    let fixture = Fixture::new().await;
    let root = tempfile::tempdir().unwrap();
    let mut runtime = fixture.runtime(root.path());
    let submitted = runtime
        .send(
            &accounts(),
            &binding(),
            &route(),
            "op",
            "private fixture text",
            ANSWERED_BUDGET,
        )
        .await
        .unwrap();
    assert_eq!(
        submitted,
        ProtocolReference::Sms {
            modem_object: MODEM_PATH.into(),
            message_object: MESSAGE_PATH.into(),
            daemon_epoch: 7
        }
    );
    assert_eq!(
        runtime.status("op"),
        Some(Status::Submitted(submitted.clone()))
    );
    drop(runtime);
    let bytes = std::fs::read(root.path().join(secret_vault::VAULT_FILE)).unwrap();
    assert!(
        !bytes
            .windows(20)
            .any(|part| part == b"private fixture text")
    );
    let mut runtime = fixture.runtime(root.path());
    assert_eq!(runtime.status("op"), Some(Status::Submitted(submitted)));
    assert!(
        runtime
            .send(
                &accounts(),
                &binding(),
                &route(),
                "op",
                "private fixture text",
                Duration::from_secs(2)
            )
            .await
            .is_err()
    );
    {
        let state = fixture.state.lock().unwrap();
        assert_eq!((state.creates, state.sends), (1, 1));
        assert_eq!(state.number, route().recipient());
    }

    // Revoked permission, a replaced SIM and a replaced daemon all refuse
    // before any hardware call or journal write.
    let fixture = Fixture::new().await;
    let root = tempfile::tempdir().unwrap();
    let mut runtime = fixture.runtime(root.path());
    let file = root.path().join(secret_vault::VAULT_FILE);
    let before = std::fs::read(&file).ok();
    fixture
        .state
        .lock()
        .unwrap()
        .permission
        .0
        .store(true, Ordering::Release);
    assert!(
        runtime
            .send(
                &accounts(),
                &binding(),
                &route(),
                "revoked",
                "private fixture text",
                Duration::from_secs(2)
            )
            .await
            .is_err()
    );
    {
        let state = fixture.state.lock().unwrap();
        assert_eq!((state.creates, state.sends), (0, 0));
    }
    assert_eq!(std::fs::read(file).ok(), before);
    assert_eq!(runtime.status("revoked"), None);
    let fixture = Fixture::new().await;
    let root = tempfile::tempdir().unwrap();
    let mut runtime = fixture.runtime(root.path());
    fixture.state.lock().unwrap().iccid = "8901000000000000002".into();
    assert!(
        runtime
            .send(
                &accounts(),
                &binding(),
                &route(),
                "op",
                "fixture text",
                Duration::from_secs(2)
            )
            .await
            .is_err()
    );
    assert_eq!(runtime.status("op"), None);
    fixture.state.lock().unwrap().iccid = ICCID.into();
    fixture
        .server
        .release_name("org.freedesktop.ModemManager1")
        .await
        .unwrap();
    assert!(
        runtime
            .send(
                &accounts(),
                &binding(),
                &route(),
                "op",
                "fixture text",
                Duration::from_secs(2)
            )
            .await
            .is_err()
    );
    assert_eq!(runtime.status("op"), None);
    {
        let state = fixture.state.lock().unwrap();
        assert_eq!((state.creates, state.sends), (0, 0));
    }

    // Revocation during Create reaches no Send and rewrites nothing durable.
    let fixture = Fixture::new().await;
    let root = tempfile::tempdir().unwrap();
    let mut runtime = fixture.runtime(root.path());
    let file = root.path().join(secret_vault::VAULT_FILE);
    {
        let mut state = fixture.state.lock().unwrap();
        state.fault = Fault::RevokeAfterCreate;
        state.journal_at_revoke = Some(file.clone());
    }
    assert!(matches!(
        runtime
            .send(
                &accounts(),
                &binding(),
                &route(),
                "interrupted",
                "private fixture text",
                ANSWERED_BUDGET
            )
            .await,
        Err(CheckedSendError::Transport(SendError::Uncertain))
    ));
    {
        let state = fixture.state.lock().unwrap();
        assert_eq!((state.creates, state.sends), (1, 0));
        assert_eq!(
            std::fs::read(file).unwrap(),
            *state.revoked_bytes.as_ref().unwrap()
        );
    }
    assert_eq!(runtime.status("interrupted"), Some(Status::Started));

    // A native object that does not answer with this exact message is
    // uncertain, never sent, and unresendable after a restart.
    for fault in [
        Fault::SwapAfterCreate,
        Fault::WrongRecipient,
        Fault::MissingMember,
        Fault::AlreadySent,
        Fault::IncomingPdu,
    ] {
        let fixture = Fixture::new().await;
        fixture.state.lock().unwrap().fault = fault;
        let root = tempfile::tempdir().unwrap();
        let mut runtime = fixture.runtime(root.path());
        assert_eq!(
            runtime
                .send(
                    &accounts(),
                    &binding(),
                    &route(),
                    "op",
                    "fixture text",
                    ANSWERED_BUDGET
                )
                .await,
            Err(CheckedSendError::Transport(SendError::Uncertain))
        );
        assert_eq!(runtime.status("op"), Some(Status::Uncertain));
        drop(runtime);
        let mut runtime = fixture.runtime(root.path());
        assert!(
            runtime
                .send(
                    &accounts(),
                    &binding(),
                    &route(),
                    "op",
                    "fixture text",
                    Duration::from_secs(2)
                )
                .await
                .is_err()
        );
        let state = fixture.state.lock().unwrap();
        assert_eq!((state.creates, state.sends), (1, 0));
    }

    // A native error and a response timeout are the same uncertainty.
    for delayed in [false, true] {
        let fixture_owner = DeadlineFixture::new().await;
        let fixture = &fixture_owner.fixture;
        fixture.state.lock().unwrap().fault = if delayed {
            Fault::DelaySend
        } else {
            Fault::FailSend
        };
        let root = tempfile::tempdir().unwrap();
        let mut runtime = fixture.runtime(root.path());
        // Freeze only fixture time while admitting real D-Bus I/O, then expire
        // the 250ms operation budget after the exact Send handler started.
        tokio::time::pause();
        let result = {
            let accounts = accounts();
            let binding = binding();
            let route = route();
            let mut send = std::pin::pin!(runtime.send(
                &accounts,
                &binding,
                &route,
                "op",
                "fixture text",
                Duration::from_millis(250)
            ));
            if delayed {
                assert!(
                    drive_sms_send(send.as_mut(), &fixture.state, true)
                        .await
                        .is_none()
                );
                tokio::time::advance(Duration::from_millis(251)).await;
            }
            drive_sms_send(send.as_mut(), &fixture.state, false)
                .await
                .unwrap()
        };
        tokio::time::resume();
        assert_eq!(
            result,
            Err(CheckedSendError::Transport(SendError::Uncertain))
        );
        assert_eq!(runtime.status("op"), Some(Status::Uncertain));
        assert!(
            runtime
                .send(
                    &accounts(),
                    &binding(),
                    &route(),
                    "next",
                    "fixture text",
                    Duration::from_secs(2)
                )
                .await
                .is_err()
        );
        let state = fixture.state.lock().unwrap();
        assert_eq!((state.creates, state.sends), (1, 1));
    }

    // A deadline that expires during Create stops before Send.
    let fixture_owner = DeadlineFixture::new().await;
    let fixture = &fixture_owner.fixture;
    fixture.state.lock().unwrap().fault = Fault::DelayAfterCreate;
    let root = tempfile::tempdir().unwrap();
    let mut runtime = fixture.runtime(root.path());
    tokio::time::pause();
    let result = {
        let accounts = accounts();
        let binding = binding();
        let route = route();
        let mut send = std::pin::pin!(runtime.send(
            &accounts,
            &binding,
            &route,
            "op",
            "fixture text",
            Duration::from_millis(250)
        ));
        let started = tilo_clock::now();
        std::future::poll_fn(|cx| {
            assert!(send.as_mut().poll(cx).is_pending());
            if fixture.state.lock().unwrap().creates == 1 {
                return std::task::Poll::Ready(());
            }
            assert!(
                started.elapsed() < tilo_test_env::hang_bound(),
                "private SMS Create was not observed"
            );
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        })
        .await;
        tokio::time::advance(Duration::from_millis(251)).await;
        drive_sms_send(send.as_mut(), &fixture.state, false)
            .await
            .unwrap()
    };
    tokio::time::resume();
    assert_eq!(
        result,
        Err(CheckedSendError::Transport(SendError::Uncertain))
    );
    assert_eq!(runtime.status("op"), Some(Status::Uncertain));
    let state = fixture.state.lock().unwrap();
    assert_eq!((state.creates, state.sends), (1, 0));
}

// Keep the paused runtime runnable while the external private bus admits I/O.
// Otherwise Tokio can auto-advance the operation timer before the server runs.
async fn drive_sms_send(
    mut send: std::pin::Pin<
        &mut impl std::future::Future<Output = Result<ProtocolReference, CheckedSendError>>,
    >,
    evidence: &Mutex<Evidence>,
    until_send: bool,
) -> Option<Result<ProtocolReference, CheckedSendError>> {
    let started = tilo_clock::now();
    std::future::poll_fn(|cx| {
        if let std::task::Poll::Ready(result) = send.as_mut().poll(cx) {
            return std::task::Poll::Ready(Some(result));
        }
        if until_send && evidence.lock().unwrap().sends == 1 {
            return std::task::Poll::Ready(None);
        }
        assert!(
            started.elapsed() < tilo_test_env::hang_bound(),
            "private SMS fixture did not reach its controlled RPC phase"
        );
        cx.waker().wake_by_ref();
        std::task::Poll::Pending
    })
    .await
}

fn receive_state(state: &mut Evidence) {
    state.incoming = true;
    state.incoming_state = 3;
    state.incoming_pdu = 1;
    state.number = route().recipient().into();
    state.text = "private incoming SMS".into();
    state.class = -1;
    state.timestamp = "2026-10-06T12:34:56+00:00".into();
}

#[tokio::test]
async fn received_sms_journey_is_exact_and_refuses_unverifiable_evidence() {
    let fixture = Fixture::new().await;
    receive_state(&mut fixture.state.lock().unwrap());
    let root = tempfile::tempdir().unwrap();
    let runtime = fixture.runtime(root.path());
    let incoming = runtime
        .read_received(
            &accounts(),
            &binding(),
            &route(),
            MESSAGE_PATH,
            ANSWERED_BUDGET,
        )
        .await
        .unwrap();
    assert_eq!(incoming.number, route().recipient());
    assert_eq!(&*incoming.text, "private incoming SMS");
    assert_eq!(incoming.timestamp_ms, 1_791_290_096_000);
    let again = runtime
        .read_received(
            &accounts(),
            &binding(),
            &route(),
            MESSAGE_PATH,
            ANSWERED_BUDGET,
        )
        .await
        .unwrap();
    assert_eq!(incoming.id, again.id);
    assert_eq!(runtime.status(&incoming.id), None);
    {
        let state = fixture.state.lock().unwrap();
        assert_eq!((state.creates, state.sends), (0, 0));
    }
    // Partial, non-text, binary, foreign, badly timed, SIM-swapped and
    // incomplete native objects are all refused.
    for case in 0..7 {
        {
            let mut state = fixture.state.lock().unwrap();
            receive_state(&mut state);
            state.data.clear();
            state.fault = Fault::None;
            state.iccid = ICCID.into();
            match case {
                0 => state.incoming_state = 2,
                1 => state.incoming_pdu = 3,
                2 => state.data.push(1),
                3 => state.number = "+12025550199".into(),
                4 => state.timestamp = "not a timestamp".into(),
                5 => state.iccid = "8901000000000000002".into(),
                _ => state.fault = Fault::SwapOnRead,
            }
        }
        assert!(
            runtime
                .read_received(
                    &accounts(),
                    &binding(),
                    &route(),
                    MESSAGE_PATH,
                    Duration::from_secs(2)
                )
                .await
                .is_err(),
            "case {case}"
        );
    }
    let state = fixture.state.lock().unwrap();
    assert_eq!((state.creates, state.sends), (0, 0));
}

#[cfg(feature = "signal-broker")]
struct ReceiveHold(std::sync::atomic::AtomicBool);
#[cfg(feature = "signal-broker")]
fn contact_route() -> tilo_contact_directory::routing::Route {
    tilo_contact_directory::routing::Route {
        person: ui::prelude::Id::from_raw("00000000-0000-4000-8000-000000000001"),
        association: ui::prelude::Id::from_raw("verified-sms-association"),
        sender: ui::prelude::Id::from_raw("fixture-sim"),
        service: "sms".into(),
        scope: "verified-provider".into(),
        recipient: route().recipient().into(),
        revision: "reviewed-identity".into(),
    }
}
#[cfg(feature = "signal-broker")]
impl crate::external_broker::ExternalAuthority for ReceiveHold {
    fn incoming(
        &self,
        recipient: &str,
    ) -> Result<crate::signal_broker::Authorization, tilo_messaging::SendError> {
        if recipient != contact_route().recipient {
            return Err(tilo_messaging::SendError::Revoked);
        }
        self.authorize(&contact_route())
    }
    fn authorize(
        &self,
        requested: &tilo_contact_directory::routing::Route,
    ) -> Result<crate::signal_broker::Authorization, tilo_messaging::SendError> {
        use tilo_messaging::SendError;
        let current = contact_route();
        if self.0.load(std::sync::atomic::Ordering::SeqCst) || *requested != current {
            return Err(SendError::Revoked);
        }
        let mut directory = tilo_contact_directory::ContactDirectory::default();
        directory
            .project_contact(current.person.clone(), "Fixture".into())
            .map_err(|_| SendError::Revoked)?;
        directory
            .confirm_account(
                current.person.clone(),
                current.association.clone(),
                current.service.clone(),
                current.scope.clone(),
                current.recipient.clone(),
                current.revision.clone(),
            )
            .map_err(|_| SendError::Revoked)?;
        Ok(crate::signal_broker::Authorization::new(
            directory,
            accounts(),
            binding(),
            current,
        ))
    }
}

#[cfg(feature = "signal-broker")]
fn inbox_broker(fixture: &Fixture, root: &Path) -> crate::external_broker::ExternalBroker {
    crate::external_broker::ExternalBroker::new(
        Box::new(fixture.runtime(&root.join("journal"))),
        crate::signal_broker::History::open(
            &root.join("history"),
            &owner(),
            secret_vault::VaultKey::from_bytes([19; 32]),
        )
        .unwrap(),
        "verified-provider".into(),
        Arc::new(ReceiveHold(std::sync::atomic::AtomicBool::new(false))),
    )
    .unwrap()
}
#[cfg(feature = "signal-broker")]
async fn added(fixture: &Fixture) {
    fixture
        .server
        .emit_signal(
            None::<&str>,
            MODEM_PATH,
            "org.freedesktop.ModemManager1.Modem.Messaging",
            "Added",
            &(OwnedObjectPath::try_from(MESSAGE_PATH).unwrap(), true),
        )
        .await
        .unwrap();
}
#[cfg(feature = "signal-broker")]
async fn state_changed(fixture: &Fixture) {
    let state = fixture.state.lock().unwrap().incoming_state;
    fixture
        .server
        .emit_signal(
            None::<&str>,
            MESSAGE_PATH,
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            &(
                "org.freedesktop.ModemManager1.Sms",
                HashMap::from([("State", zbus::zvariant::Value::from(state))]),
                Vec::<String>::new(),
            ),
        )
        .await
        .unwrap();
}
#[cfg(feature = "signal-broker")]
async fn wait_inbox(broker: &crate::external_broker::ExternalBroker) {
    let deadline = tokio::time::Instant::now()
        .checked_add(tilo_test_env::hang_bound())
        .expect("bounded fixture deadline");
    loop {
        let page = broker
            .client()
            .history()
            .read(contact_route(), None)
            .await
            .unwrap();
        if !page.messages.is_empty() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "real subscribed native inbox must arrive"
        );
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}
#[cfg(feature = "signal-broker")]
async fn wait_account(supervisor: &Arc<crate::supervisor::AccountSupervisor>, failed: bool) {
    use tilo_messaging::{AccountState, ServiceAccount};
    let account = ServiceAccount::new("sms", "fixture-sim");
    let deadline = tokio::time::Instant::now()
        .checked_add(tilo_test_env::hang_bound())
        .expect("bounded fixture deadline");
    while (supervisor.client().account_state(&account) == AccountState::Failed) != failed {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

#[cfg(feature = "signal-broker")]
#[tokio::test]
async fn subscribed_sms_journey_from_enumeration_to_revoked_reply() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    // Initial enumeration admits exactly the readable message, durably and
    // without touching the native object.
    let fixture = Fixture::new().await;
    receive_state(&mut fixture.state.lock().unwrap());
    let root = tempfile::tempdir().unwrap();
    let broker = inbox_broker(&fixture, root.path());
    let guard = broker.start_sms_receiver().await.unwrap();
    let page = broker
        .client()
        .history()
        .read(contact_route(), None)
        .await
        .unwrap();
    assert_eq!(page.messages.len(), 1);
    assert_eq!(
        page.messages.first().unwrap().text.as_str(),
        "private incoming SMS"
    );
    assert!(!guard.stopped());
    {
        let state = fixture.state.lock().unwrap();
        assert_eq!((state.creates, state.sends), (0, 0));
    }
    drop(guard);
    tokio::task::yield_now().await;
    drop(broker);
    let history = crate::signal_broker::History::open(
        &root.path().join("history"),
        &owner(),
        secret_vault::VaultKey::from_bytes([19; 32]),
    )
    .unwrap();
    assert_eq!(
        history.page(contact_route(), 128).unwrap().messages.len(),
        1
    );
    let bytes = std::fs::read(root.path().join("history/history.enc")).unwrap();
    assert!(
        !bytes
            .windows(b"private incoming SMS".len())
            .any(|part| part == b"private incoming SMS")
    );

    // A partial message persists nothing until its State completes; an unknown
    // recipient has no route until the object names the confirmed one.
    let fixture = Fixture::new().await;
    let root = tempfile::tempdir().unwrap();
    let broker = inbox_broker(&fixture, root.path());
    let guard = broker.start_sms_receiver().await.unwrap();
    {
        let mut state = fixture.state.lock().unwrap();
        receive_state(&mut state);
        state.incoming_state = 2;
        state.number = "+12025550999".into();
    }
    added(&fixture).await;
    let deadline = tokio::time::Instant::now()
        .checked_add(tilo_test_env::hang_bound())
        .expect("bounded fixture deadline");
    while fixture.state.lock().unwrap().state_reads == 0 {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert!(
        broker
            .client()
            .history()
            .read(contact_route(), None)
            .await
            .unwrap()
            .messages
            .is_empty()
    );
    {
        let mut state = fixture.state.lock().unwrap();
        state.incoming_state = 3;
        state.number = contact_route().recipient;
    }
    state_changed(&fixture).await;
    wait_inbox(&broker).await;
    added(&fixture).await;
    state_changed(&fixture).await;
    let page = broker
        .client()
        .history()
        .read(contact_route(), None)
        .await
        .unwrap();
    assert_eq!(page.messages.len(), 1);
    assert_eq!(page.messages.first().unwrap().delivery, None);
    assert!(!guard.stopped());
    {
        let state = fixture.state.lock().unwrap();
        assert_eq!((state.creates, state.sends), (0, 0));
    }

    // An oversized native inbox is refused rather than truncated.
    let fixture = Fixture::new().await;
    receive_state(&mut fixture.state.lock().unwrap());
    fixture.state.lock().unwrap().inbox_copies = 513;
    let root = tempfile::tempdir().unwrap();
    let broker = inbox_broker(&fixture, root.path());
    assert!(matches!(
        broker.start_sms_receiver().await,
        Err(tilo_messaging::SendError::Unavailable)
    ));
    assert!(
        broker
            .client()
            .history()
            .read(contact_route(), None)
            .await
            .unwrap()
            .messages
            .is_empty()
    );
    {
        let state = fixture.state.lock().unwrap();
        assert_eq!((state.creates, state.sends, state.state_reads), (0, 0, 0));
    }

    // A SIM change, a replaced daemon and a store that cannot persist each
    // fail the owned receiver instead of publishing a ready history.
    for failure in ["sim", "daemon", "storage"] {
        let fixture = Fixture::new().await;
        let root = tempfile::tempdir().unwrap();
        let broker = inbox_broker(&fixture, root.path());
        let supervisor = crate::supervisor::AccountSupervisor::new(&owner()).unwrap();
        supervisor.install_sms_receiving(&broker).await.unwrap();
        wait_account(&supervisor, false).await;
        match failure {
            "sim" => fixture.state.lock().unwrap().iccid = "8901000000000000002".into(),
            "daemon" => {
                fixture
                    .server
                    .release_name("org.freedesktop.ModemManager1")
                    .await
                    .unwrap();
                let replacement = zbus::connection::Builder::address(fixture.bus.address())
                    .unwrap()
                    .name("org.freedesktop.ModemManager1")
                    .unwrap()
                    .build()
                    .await
                    .unwrap();
                assert_ne!(replacement.unique_name(), fixture.server.unique_name());
            }
            _ => {
                let file = root.path().join("history/history.enc");
                std::fs::remove_file(&file).unwrap();
                std::fs::create_dir(&file).unwrap();
                receive_state(&mut fixture.state.lock().unwrap());
            }
        }
        added(&fixture).await;
        wait_account(&supervisor, true).await;
        assert!(!supervisor.client().supports(&contact_route()));
        assert!(
            supervisor
                .client()
                .history()
                .read(contact_route(), None)
                .await
                .is_err()
        );
        if failure == "sim" {
            // The native bus name stays with its host; only the owned receiver
            // stops.
            drop(supervisor);
            let bus = zbus::fdo::DBusProxy::new(&fixture.client).await.unwrap();
            assert_eq!(
                bus.get_name_owner("org.freedesktop.ModemManager1".try_into().unwrap())
                    .await
                    .unwrap(),
                *fixture.server.unique_name().unwrap()
            );
        }
        let state = fixture.state.lock().unwrap();
        assert_eq!((state.creates, state.sends), (0, 0));
    }

    // The broker's own admission is idempotent, refuses a conflicting native
    // object, wakes private readers once, and stops on revocation.
    let fixture = Fixture::new().await;
    receive_state(&mut fixture.state.lock().unwrap());
    let root = tempfile::tempdir().unwrap();
    let hold = Arc::new(ReceiveHold(std::sync::atomic::AtomicBool::new(false)));
    let key = || secret_vault::VaultKey::from_bytes([19; 32]);
    let history_root = root.path().join("history");
    let broker = crate::external_broker::ExternalBroker::new(
        Box::new(fixture.runtime(&root.path().join("journal"))),
        crate::signal_broker::History::open(&history_root, &owner(), key()).unwrap(),
        "verified-provider".into(),
        hold.clone(),
    )
    .unwrap();
    let client = broker.client();
    let wakes = Arc::new(AtomicUsize::new(0));
    let counted = wakes.clone();
    let observer = client
        .history()
        .observe(Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        }))
        .unwrap();
    assert!(
        broker
            .observe_sms_message(contact_route(), MESSAGE_PATH.into())
            .await
            .unwrap()
    );
    assert!(
        !broker
            .observe_sms_message(contact_route(), MESSAGE_PATH.into())
            .await
            .unwrap()
    );
    let page = client.history().read(contact_route(), None).await.unwrap();
    assert_eq!(page.messages.len(), 1);
    assert_eq!(&*page.messages[0].text, "private incoming SMS");
    assert_eq!(page.messages[0].delivery, None);
    assert_eq!(wakes.load(Ordering::SeqCst), 1);
    fixture.state.lock().unwrap().text = "conflicting native object".into();
    assert_eq!(
        broker
            .observe_sms_message(contact_route(), MESSAGE_PATH.into())
            .await,
        Err(tilo_messaging::SendError::InvalidRequest)
    );
    assert_eq!(wakes.load(Ordering::SeqCst), 1);

    // The person replies through the broker, the native object becomes the
    // outgoing SMS, and revocation stops both native submission and private
    // history writes.
    fixture.state.lock().unwrap().incoming = false;
    let reply = tilo_messaging::SendRequest {
        route: contact_route(),
        operation: "sms-native-reply".into(),
        text: zeroize::Zeroizing::new("private SMS reply".into()),
    };
    let receipt = client.send(reply).await.unwrap();
    assert_eq!(
        receipt.delivery,
        tilo_messaging::Delivery::Submitted,
        "a native Send reply is not evidence of carrier delivery"
    );
    let page = client.history().read(contact_route(), None).await.unwrap();
    assert!(
        page.messages
            .iter()
            .any(|message| message.text.as_str() == "private SMS reply"
                && message.delivery == Some(tilo_messaging::Delivery::Submitted))
    );
    {
        let state = fixture.state.lock().unwrap();
        assert_eq!((state.creates, state.sends), (1, 1));
        assert_eq!(state.number, contact_route().recipient);
        assert_eq!(state.text, "private SMS reply");
    }
    let private_files = |directory: &Path| {
        let mut files: Vec<_> = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                (path.clone(), std::fs::read(path).unwrap())
            })
            .collect();
        files.sort_by(|left, right| left.0.cmp(&right.0));
        files
    };
    let before = private_files(&history_root);
    hold.0.store(true, Ordering::SeqCst);
    let refused = tilo_messaging::SendRequest {
        route: contact_route(),
        operation: "sms-native-revoked-reply".into(),
        text: zeroize::Zeroizing::new("revoked SMS reply".into()),
    };
    assert!(client.send(refused).await.is_err());
    assert_eq!(
        broker
            .observe_sms_message(contact_route(), MESSAGE_PATH.into())
            .await,
        Err(tilo_messaging::SendError::Revoked)
    );
    assert_eq!(
        private_files(&history_root),
        before,
        "revocation must stop private history writes as well as native submission"
    );
    drop(client);
    drop(observer);
    drop(broker);
    let reopened = crate::signal_broker::History::open(&history_root, &owner(), key()).unwrap();
    assert_eq!(
        reopened.page(contact_route(), 128).unwrap().messages.len(),
        2
    );
    let bytes = std::fs::read(history_root.join("history.enc")).unwrap();
    assert!(
        !bytes
            .windows(20)
            .any(|part| part == b"private incoming SMS")
    );
}
