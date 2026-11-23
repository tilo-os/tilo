#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    reason = "a failed unwrap or index in a test is its assertion, and one end-to-end flow reads as one test"
)]
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tilo_contact_directory::routing::Route;
use tilo_messaging::{Delivery, HistoryError, MessagingClient, SendError};
use tilo_messaging_protocol::{
    Service,
    journal::{EncryptedJournal, Owner},
    routing::{LinkState, LiveBinding},
    signal_broker::{Authorization, History, SignalAuthority, SignalBroker},
    signal_runtime::SignalRuntime,
};
use ui::prelude::Id;
const ACI: &str = "01234567-89ab-cdef-0123-456789abcdef";
const OWN_ACI: &str = "11111111-1111-4111-8111-111111111111";
const SCOPE: &str = "signal:11111111-1111-4111-8111-111111111111";
/// How long a fixture daemon waits for the broker's next line before failing.
///
/// The wait for the per-contact identity probe spans opening the broker and
/// confirming the contact, which on a saturated nextest shard has taken longer
/// than five seconds. The bound only has to stop a hung exchange, not measure
/// the broker.
const DAEMON_READ_DEADLINE: ui::time::Duration = ui::time::Duration::from_secs(30);
fn revision() -> String {
    format!("signal-identity:05{}", "42".repeat(32))
}
const PERSON: &str = "00000000-0000-4000-8000-000000000001";
#[derive(Clone)]
struct Authority {
    revoked: Arc<AtomicBool>,
}
fn route() -> Route {
    Route {
        person: Id::from_raw(PERSON),
        association: Id::from_raw("broker"),
        sender: Id::from_raw("fixture-signal"),
        service: "signal".into(),
        scope: SCOPE.into(),
        recipient: ACI.into(),
        revision: revision(),
    }
}
fn owner() -> Owner {
    Owner {
        actor: "11".repeat(32),
        device: "test-device".into(),
        service: Service::Signal,
        account: "fixture-signal".into(),
        linking_generation: 1,
    }
}
impl SignalAuthority for Authority {
    fn authorize(&self, requested: &Route) -> Result<Authorization, SendError> {
        if self.revoked.load(Ordering::SeqCst) || *requested != route() {
            return Err(SendError::Revoked);
        }
        let mut accounts = accounts::person::AccountList::default();
        accounts
            .keep(&accounts::Account::new("fixture-signal", "signal"))
            .map_err(|_| SendError::Revoked)?;
        let mut directory = tilo_contact_directory::ContactDirectory::default();
        directory
            .project_contact(Id::from_raw(PERSON), "Fixture person".into())
            .map_err(|_| SendError::Revoked)?;
        directory
            .confirm_account(
                Id::from_raw(PERSON),
                Id::from_raw("broker"),
                "signal".into(),
                SCOPE.into(),
                ACI.into(),
                revision(),
            )
            .map_err(|_| SendError::Revoked)?;
        Ok(Authorization::new(
            directory,
            accounts,
            LiveBinding {
                owner: owner(),
                state: LinkState::Linked,
            },
            route(),
        ))
    }
    fn incoming(&self, aci: &str) -> Result<Authorization, SendError> {
        if aci != ACI {
            return Err(SendError::Revoked);
        }
        self.authorize(&route())
    }
}
fn broker(root: &Path, stream: UnixStream, authority: Authority) -> SignalBroker {
    let journal = EncryptedJournal::open(
        &root.join("journal"),
        &owner(),
        secret_vault::VaultKey::from_bytes([7; 32]),
    )
    .expect("private journal");
    let runtime = SignalRuntime::from_journal(owner(), OWN_ACI.into(), stream, journal)
        .expect("native adapter");
    let history = History::open(
        &root.join("history"),
        &owner(),
        secret_vault::VaultKey::from_bytes([7; 32]),
    )
    .expect("encrypted history");
    SignalBroker::new(runtime, history, Arc::new(authority), OWN_ACI.into())
        .expect("registered service")
}
fn send() -> tilo_messaging::SendRequest {
    tilo_messaging::SendRequest {
        route: route(),
        operation: "fixture-operation".into(),
        text: zeroize::Zeroizing::new("private outgoing".into()),
    }
}
fn field(value: &serde_json::Value, path: &str) -> serde_json::Value {
    value
        .pointer(path)
        .expect("required provider field")
        .clone()
}
fn account_probe(reader: &mut BufReader<UnixStream>, peer: &mut UnixStream) {
    let mut probe = String::new();
    reader.read_line(&mut probe).expect("account probe");
    let probe: serde_json::Value = serde_json::from_str(&probe).expect("account request");
    assert_eq!(field(&probe, "/method"), "listAccounts");
    writeln!(peer,"{}",serde_json::json!({"jsonrpc":"2.0","id":field(&probe,"/id"),"result":[{"number":"+15555550101"}]})).expect("account evidence");
    let mut own_line = String::new();
    reader
        .read_line(&mut own_line)
        .expect("local own-identity probe");
    let own_probe: serde_json::Value =
        serde_json::from_str(&own_line).expect("local own-identity request");
    assert_eq!(field(&own_probe, "/method"), "listIdentities");
    assert_eq!(
        field(&own_probe, "/params/number"),
        field(&own_probe, "/params/account")
    );
    writeln!(peer, "{}", serde_json::json!({"jsonrpc":"2.0","id":field(&own_probe, "/id"),"result":serde_json::json!([{"uuid":OWN_ACI,"number":"+15555550101","trustLevel":"TRUSTED_VERIFIED","fingerprint":format!("05 {}", ["aa"; 32].join(" "))}])})).expect("local own-identity evidence");
}
fn subscribe(reader: &mut BufReader<UnixStream>, peer: &mut UnixStream) {
    let mut line = String::new();
    reader.read_line(&mut line).expect("subscription request");
    let request: serde_json::Value = serde_json::from_str(&line).expect("subscription JSON");
    assert_eq!(field(&request, "/method"), "subscribeReceive");
    assert_eq!(
        field(&request, "/params"),
        serde_json::json!({"account":"+15555550101"})
    );
    writeln!(
        peer,
        "{}",
        serde_json::json!({"jsonrpc":"2.0","id":field(&request, "/id"),"result":0})
    )
    .expect("subscription acknowledgment");
}
fn daemon(peer: UnixStream) -> std::thread::JoinHandle<UnixStream> {
    daemon_with_receipt(peer, None)
}
fn daemon_with_receipt(
    peer: UnixStream,
    receipt: Option<serde_json::Value>,
) -> std::thread::JoinHandle<UnixStream> {
    daemon_with_revocation(peer, receipt, None)
}
fn daemon_with_revocation(
    mut peer: UnixStream,
    receipt: Option<serde_json::Value>,
    revoked: Option<Arc<AtomicBool>>,
) -> std::thread::JoinHandle<UnixStream> {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(peer.try_clone().expect("reader"));
        account_probe(&mut reader, &mut peer);
        let mut probe = String::new();
        reader.read_line(&mut probe).expect("identity probe");
        let probe: serde_json::Value = serde_json::from_str(&probe).expect("identity request");
        assert_eq!(field(&probe, "/method"), "listIdentities");
        assert_eq!(field(&probe, "/params/number"), ACI);
        writeln!(peer,"{}",serde_json::json!({"jsonrpc":"2.0","id":field(&probe,"/id"),"result":[{"uuid":ACI,"fingerprint":format!("05{}"," 42".repeat(32)),"trustLevel":"TRUSTED_VERIFIED"}]})).expect("provider proof");
        let mut line = String::new();
        reader.read_line(&mut line).expect("actual request");
        let request: serde_json::Value = serde_json::from_str(&line).expect("protocol JSON");
        assert_eq!(field(&request, "/params/account"), "+15555550101");
        assert_eq!(
            field(&request, "/params/recipient"),
            serde_json::json!([ACI])
        );
        assert_eq!(field(&request, "/params/message"), "private outgoing");
        assert_eq!(field(&request, "/id"), "fixture-operation");
        let event = serde_json::json!({"jsonrpc":"2.0","method":"receive","params":{"account":"+15555550101","envelope":{"sourceUuid":ACI,"dataMessage":{"timestamp":42,"message":"private incoming"}}}});
        writeln!(peer, "{event}").expect("inbound");
        if let Some(receipt) = receipt {
            writeln!(peer, "{receipt}").expect("interleaved native recipient evidence");
        }
        if let Some(revoked) = revoked {
            revoked.store(true, Ordering::SeqCst);
        }
        writeln!(
            peer,
            "{}",
            serde_json::json!({"jsonrpc":"2.0","id":"fixture-operation","result":{"timestamp":43}})
        )
        .expect("server acknowledgment");
        peer
    })
}

/// A restart of the same private state: the account probe on a fresh stream,
/// then the broker that serves it.
fn restarted(root: &Path, authority: Authority) -> (SignalBroker, UnixStream) {
    let (stream, mut peer) = UnixStream::pair().expect("restart stream");
    let setup = std::thread::spawn(move || {
        let mut reader = BufReader::new(peer.try_clone().expect("restart reader"));
        account_probe(&mut reader, &mut peer);
        peer
    });
    let service = broker(root, stream, authority);
    let peer = setup.join().expect("restart account probe");
    peer.set_nonblocking(true)
        .expect("restart replay observation");
    (service, peer)
}

async fn wait_for_page(client: &MessagingClient, wanted: usize) -> Vec<tilo_messaging::Message> {
    let deadline = std::time::Instant::now()
        .checked_add(tilo_test_env::hang_bound())
        .expect("bounded wait");
    loop {
        let page = client
            .history()
            .read(route(), None)
            .await
            .expect("private history");
        if page.messages.len() >= wanted || std::time::Instant::now() > deadline {
            return page.messages;
        }
        std::thread::park_timeout(ui::time::Duration::from_millis(1));
    }
}

/// Revocation after the native write keeps the reservation uncertain, never
/// replays the write, and keeps that state across a restart; a completed send
/// is durable and equally unresendable.
#[tokio::test]
async fn signal_send_never_replays_after_revocation_duplication_or_restart() {
    let root = tempfile::tempdir().expect("private history");
    let revoked = Arc::new(AtomicBool::new(false));
    let (stream, peer) = UnixStream::pair().expect("actual protocol streams");
    peer.set_read_timeout(Some(DAEMON_READ_DEADLINE))
        .expect("bounded daemon fixture");
    let writer = daemon_with_revocation(peer, None, Some(revoked.clone()));
    let service = broker(
        root.path(),
        stream,
        Authority {
            revoked: revoked.clone(),
        },
    );
    let client = service.client();
    assert!(matches!(
        client.send(send()).await,
        Err(SendError::Uncertain)
    ));
    assert!(!client.supports(&route()));
    let mut peer = writer.join().expect("acknowledged real stream");
    peer.set_nonblocking(true).expect("no replay observation");
    revoked.store(false, Ordering::SeqCst);
    let history = client
        .history()
        .read(route(), None)
        .await
        .expect("restored access");
    assert_eq!(history.messages.len(), 1);
    assert_eq!(
        history.messages.first().expect("reserved send").delivery,
        Some(Delivery::Uncertain)
    );
    assert!(matches!(
        client.send(send()).await,
        Err(SendError::AlreadyReserved)
    ));
    assert_eq!(
        peer.read(&mut [0; 1])
            .expect_err("no duplicate native write")
            .kind(),
        std::io::ErrorKind::WouldBlock
    );
    drop(client);
    drop(service);
    let (service, mut peer) = restarted(root.path(), Authority { revoked });
    let client = service.client();
    let history = client
        .history()
        .read(route(), None)
        .await
        .expect("durable uncertain state");
    assert_eq!(
        history.messages.first().expect("reserved send").delivery,
        Some(Delivery::Uncertain)
    );
    assert!(matches!(
        client.send(send()).await,
        Err(SendError::AlreadyReserved)
    ));
    assert_eq!(
        peer.read(&mut [0; 1])
            .expect_err("restart cannot replay")
            .kind(),
        std::io::ErrorKind::WouldBlock
    );

    // A completed native submission keeps both sides of the conversation,
    // stays private on disk and refuses to be submitted again: before or
    // after a restart, and after its authority is revoked.
    let root = tempfile::tempdir().expect("isolated state");
    let authority = Authority {
        revoked: Arc::new(AtomicBool::new(false)),
    };
    let (stream, peer) = UnixStream::pair().expect("daemon fixture");
    peer.set_read_timeout(Some(DAEMON_READ_DEADLINE))
        .expect("deadline");
    let writer = daemon(peer);
    let service = broker(root.path(), stream, authority.clone());
    let client = service.client();
    let receipt = client.send(send()).await.expect("confirmed actual send");
    assert_eq!(receipt.delivery, Delivery::ServerAcknowledged);
    let page = client.history().read(route(), None).await.expect("history");
    assert!(
        page.messages
            .iter()
            .any(|m| m.text.as_str() == "private outgoing"
                && m.delivery == Some(Delivery::ServerAcknowledged)),
        "a native acknowledgment is not a delivery"
    );
    assert!(
        page.messages
            .iter()
            .any(|m| m.text.as_str() == "private incoming")
    );
    assert!(matches!(
        client.send(send()).await,
        Err(SendError::AlreadyReserved)
    ));
    let mut peer = writer.join().expect("daemon");
    peer.set_nonblocking(true).expect("observation");
    assert_eq!(
        peer.read(&mut [0; 1])
            .expect_err("no duplicate write")
            .kind(),
        std::io::ErrorKind::WouldBlock
    );
    authority.revoked.store(true, Ordering::SeqCst);
    assert!(matches!(
        client.history().read(route(), None).await,
        Err(HistoryError::Revoked)
    ));
    drop(client);
    drop(service);
    authority.revoked.store(false, Ordering::SeqCst);
    let (service, mut peer) = restarted(root.path(), authority);
    let client = service.client();
    let page = client
        .history()
        .read(route(), None)
        .await
        .expect("durable history");
    assert!(
        page.messages
            .iter()
            .any(|m| m.text.as_str() == "private incoming")
    );
    assert!(
        page.messages
            .iter()
            .any(|m| m.text.as_str() == "private outgoing")
    );
    assert!(client.send(send()).await.is_err());
    assert_eq!(
        peer.read(&mut [0; 1])
            .expect_err("restart cannot replay")
            .kind(),
        std::io::ErrorKind::WouldBlock
    );
    let ciphertext = std::fs::read(root.path().join("history/history.enc")).expect("ciphertext");
    assert!(
        !ciphertext
            .windows(b"private incoming".len())
            .any(|w| w == b"private incoming")
    );
}

#[test]
fn idle_inbound_arrival_persists_and_wakes_the_selected_conversation() {
    let root = tempfile::tempdir().expect("private history");
    let authority = Authority {
        revoked: Arc::new(AtomicBool::new(false)),
    };
    let (sender, mut sender_peer) = UnixStream::pair().expect("sender");
    let setup = std::thread::spawn(move || {
        let mut reader = BufReader::new(sender_peer.try_clone().expect("reader"));
        account_probe(&mut reader, &mut sender_peer);
        sender_peer
    });
    let service = broker(root.path(), sender, authority);
    let _sender_peer = setup.join().expect("sender setup");
    let client = service.client();
    let woke = Arc::new(AtomicBool::new(false));
    let flagged = woke.clone();
    let _observer = client
        .history()
        .observe(Arc::new(move || {
            flagged.store(true, Ordering::SeqCst);
        }))
        .expect("private history observer");
    let (stream, mut peer) = UnixStream::pair().expect("independent inbound");
    let (go, when) = std::sync::mpsc::channel();
    let daemon = std::thread::spawn(move || {
        let mut reader = BufReader::new(peer.try_clone().expect("reader"));
        account_probe(&mut reader, &mut peer);
        subscribe(&mut reader, &mut peer);
        when.recv().expect("arrive after mount");
        let event = serde_json::json!({"jsonrpc":"2.0","method":"receive","params":{"subscription":0,"result":{"account":"+15555550101","envelope":{"sourceUuid":ACI,"dataMessage":{"timestamp":44,"message":"idle private arrival"}}}}});
        writeln!(peer, "{event}").expect("incoming provider frame");
        let mut byte = [0];
        assert_eq!(peer.read(&mut byte).expect("receiver shutdown"), 0);
    });
    let receiver = tilo_messaging_protocol::signal_runtime::SignalReceiver::new(
        owner(),
        OWN_ACI.into(),
        stream,
    )
    .expect("verified inbox account");
    let guard = service
        .start_receiver(receiver)
        .expect("independent worker");
    go.send(()).expect("fixture arrival");
    let deadline = std::time::Instant::now()
        .checked_add(tilo_test_env::hang_bound())
        .expect("bounded wait");
    while !woke.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
        std::thread::park_timeout(ui::time::Duration::from_millis(1));
    }
    assert!(
        woke.load(Ordering::SeqCst),
        "actual arrival wakes the private history reader"
    );
    let messages = block_on(client.history().read(route(), None)).expect("history");
    assert!(
        messages
            .messages
            .iter()
            .any(|m| m.text.as_str() == "idle private arrival" && m.delivery.is_none()),
        "an idle arrival persists as an incoming message with no delivery claim"
    );
    assert!(!guard.stopped());
    drop(guard);
    daemon.join().expect("only owned receive connection stops");
}

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("blocking reader")
        .block_on(future)
}

#[test]
fn signal_ready_publication_already_owns_receiver_and_observes_its_failure() {
    use tilo_messaging::{AccountState, ServiceAccount};
    use tilo_messaging_protocol::supervisor::AccountSupervisor;
    let root = tempfile::tempdir().expect("private state");
    let (sender, mut peer) = UnixStream::pair().expect("owned sender");
    let setup = std::thread::spawn(move || {
        let mut reader = BufReader::new(peer.try_clone().expect("reader"));
        account_probe(&mut reader, &mut peer);
        peer
    });
    let service = broker(
        root.path(),
        sender,
        Authority {
            revoked: Arc::new(AtomicBool::new(false)),
        },
    );
    let _sender_peer = setup.join().expect("owned account setup");
    let supervisor = AccountSupervisor::new(&owner()).expect("owned supervisor");
    let (stream, mut peer) = UnixStream::pair().expect("independent receive stream");
    let (close, native) = std::sync::mpsc::channel();
    let setup = std::thread::spawn(move || {
        let mut reader = BufReader::new(peer.try_clone().expect("reader"));
        account_probe(&mut reader, &mut peer);
        subscribe(&mut reader, &mut peer);
        drop(reader);
        close.send(peer).expect("native shutdown handle");
    });
    let native = std::sync::Mutex::new(native);
    let announced = AtomicBool::new(false);
    let current = Arc::downgrade(&supervisor);
    let (result, observed) = std::sync::mpsc::channel();
    let _observer = supervisor.client().history().observe(Arc::new(move || {
        if announced.swap(true, Ordering::SeqCst) {
            return;
        }
        let supervisor = current.upgrade().expect("live installation");
        let account = ServiceAccount::new("signal", "fixture-signal");
        assert_eq!(
            supervisor.client().account_state(&account),
            AccountState::Ready
        );
        drop(
            native
                .lock()
                .expect("shutdown lock")
                .recv_timeout(tilo_test_env::hang_bound())
                .expect("native peer"),
        );
        let deadline = std::time::Instant::now()
            .checked_add(tilo_test_env::hang_bound())
            .expect("bounded wait");
        while supervisor.client().account_state(&account) != AccountState::Failed
            && std::time::Instant::now() < deadline
        {
            std::thread::park_timeout(ui::time::Duration::from_millis(1));
        }
        result
            .send(supervisor.client().account_state(&account))
            .expect("publication observation");
    }));
    let receiver = tilo_messaging_protocol::signal_runtime::SignalReceiver::new(
        owner(),
        OWN_ACI.into(),
        stream,
    )
    .expect("owned receiver");
    supervisor
        .install_signal_receiving(&service, receiver)
        .expect("atomic native publication");
    setup.join().expect("bounded native fixture");
    assert_eq!(
        observed
            .recv_timeout(tilo_test_env::hang_bound())
            .expect("actual ready observer"),
        AccountState::Failed
    );
    assert!(!supervisor.client().supports(&route()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changed_provider_key_refuses_before_reservation_or_send() {
    let root = tempfile::tempdir().expect("private state");
    let authority = Authority {
        revoked: Arc::new(AtomicBool::new(false)),
    };
    let (sender, mut peer) = UnixStream::pair().expect("daemon");
    let daemon = std::thread::spawn(move || {
        let mut reader = BufReader::new(peer.try_clone().expect("reader"));
        account_probe(&mut reader, &mut peer);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read-only key probe");
        let request: serde_json::Value = serde_json::from_str(&line).expect("probe");
        assert_eq!(field(&request, "/method"), "listIdentities");
        writeln!(peer,"{}",serde_json::json!({"jsonrpc":"2.0","id":field(&request,"/id"),"result":[{"uuid":ACI,"fingerprint":format!("05{}"," 43".repeat(32)),"trustLevel":"TRUSTED_VERIFIED"}]})).expect("changed current key");
        peer
    });
    let service = broker(root.path(), sender, authority);
    let client = service.client();
    let changed = tilo_messaging::SendRequest {
        route: route(),
        operation: "changed-key-operation".into(),
        text: zeroize::Zeroizing::new("must not send".into()),
    };
    assert!(client.send(changed).await.is_err());
    let page = client.history().read(route(), None).await.expect("history");
    assert!(page.messages.is_empty(), "no reservation or history row");
    let mut peer = daemon.join().expect("probe daemon");
    peer.set_nonblocking(true).expect("wire observation");
    assert_eq!(
        peer.read(&mut [0; 1])
            .expect_err("no send follows changed key")
            .kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_signal_recipient_receipt_correlates_native_operation_and_updates_private_history() {
    let root = tempfile::tempdir().expect("private state");
    let authority = Authority {
        revoked: Arc::new(AtomicBool::new(false)),
    };
    let (stream, peer) = UnixStream::pair().expect("owned daemon fixture");
    peer.set_read_timeout(Some(DAEMON_READ_DEADLINE))
        .expect("deadline");
    let receipt = serde_json::json!({"jsonrpc":"2.0","method":"receive","params":{"account":"+15555550101",
        "envelope":{"sourceUuid":ACI,"receiptMessage":{"when":44,"isDelivery":false,"isRead":true,"isViewed":false,"timestamps":[43]}}}});
    let daemon = daemon_with_receipt(peer, Some(receipt));
    let service = broker(root.path(), stream, authority);
    let client = service.client();
    let receipt = client
        .send(send())
        .await
        .expect("confirmed native submission");
    assert_eq!(receipt.delivery, Delivery::ServerAcknowledged);
    let page = wait_for_page(&client, 2).await;
    assert_eq!(
        page.iter()
            .find(|m| m.text.as_str() == "private outgoing")
            .expect("outgoing row")
            .delivery,
        Some(Delivery::RecipientRead),
        "the interleaved native receipt promotes the exact operation"
    );
    assert!(client.send(send()).await.is_err());
    let mut peer = daemon.join().expect("daemon");
    peer.set_nonblocking(true).expect("observe native writes");
    assert_eq!(
        peer.read(&mut [0; 1])
            .expect_err("no replay after receipt")
            .kind(),
        std::io::ErrorKind::WouldBlock
    );
    drop(client);
    drop(service);
    let history = History::open(
        &root.path().join("history"),
        &owner(),
        secret_vault::VaultKey::from_bytes([7; 32]),
    )
    .expect("durable private history");
    drop(history);
    let journal = EncryptedJournal::open(
        &root.path().join("journal"),
        &owner(),
        secret_vault::VaultKey::from_bytes([7; 32]),
    )
    .expect("native journal survives restart");
    assert_eq!(
        journal.recipient_evidence("fixture-operation"),
        Some(tilo_messaging_protocol::journal::RecipientEvidence::Read)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_signal_timestamp_or_foreign_sender_never_promotes_recipient_delivery() {
    for (sender, timestamps) in [
        (ACI, vec![999]),
        ("22222222-2222-4222-8222-222222222222", vec![43]),
    ] {
        let root = tempfile::tempdir().expect("private state");
        let authority = Authority {
            revoked: Arc::new(AtomicBool::new(false)),
        };
        let (stream, peer) = UnixStream::pair().expect("owned daemon fixture");
        peer.set_read_timeout(Some(DAEMON_READ_DEADLINE))
            .expect("deadline");
        let receipt = serde_json::json!({"jsonrpc":"2.0","method":"receive","params":{"account":"+15555550101",
            "envelope":{"sourceUuid":sender,"receiptMessage":{"when":44,"isDelivery":false,"isRead":true,"isViewed":false,"timestamps":timestamps}}}});
        let daemon = daemon_with_receipt(peer, Some(receipt));
        let service = broker(root.path(), stream, authority);
        let client = service.client();
        client
            .send(send())
            .await
            .expect("server accepts the exact submission");
        let deadline = std::time::Instant::now()
            .checked_add(tilo_test_env::hang_bound())
            .expect("bounded wait");
        let mut delivery = None;
        while delivery.is_none() && std::time::Instant::now() < deadline {
            let page = client.history().read(route(), None).await.expect("history");
            delivery = page
                .messages
                .iter()
                .find(|m| m.text.as_str() == "private outgoing")
                .and_then(|m| m.delivery);
            std::thread::park_timeout(ui::time::Duration::from_millis(1));
        }
        assert_eq!(
            delivery,
            Some(Delivery::ServerAcknowledged),
            "unrelated receipts never promote recipient delivery"
        );
        daemon.join().expect("owned daemon");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_signal_receipt_wakes_the_selected_private_conversation() {
    let root = tempfile::tempdir().expect("private state");
    let authority = Authority {
        revoked: Arc::new(AtomicBool::new(false)),
    };
    let (sender, peer) = UnixStream::pair().expect("sender fixture");
    peer.set_read_timeout(Some(DAEMON_READ_DEADLINE))
        .expect("deadline");
    let sent = daemon(peer);
    let service = broker(root.path(), sender, authority);
    let client = service.client();
    client
        .send(send())
        .await
        .expect("confirmed native submission");
    let _sender_peer = sent.join().expect("sender fixture remains owned");
    let woke = Arc::new(AtomicBool::new(false));
    let flagged = woke.clone();
    let _observer = client
        .history()
        .observe(Arc::new(move || {
            flagged.store(true, Ordering::SeqCst);
        }))
        .expect("private history observer");
    let (stream, mut peer) = UnixStream::pair().expect("independent inbox");
    let (go, when) = std::sync::mpsc::channel();
    let inbox = std::thread::spawn(move || {
        let mut reader = BufReader::new(peer.try_clone().expect("reader"));
        account_probe(&mut reader, &mut peer);
        subscribe(&mut reader, &mut peer);
        when.recv().expect("arrive after subscription");
        let receipt = serde_json::json!({"jsonrpc":"2.0","method":"receive","params":{"subscription":0,"result":{"account":"+15555550101",
            "envelope":{"sourceUuid":ACI,"receiptMessage":{"when":44,"isDelivery":false,"isRead":true,"isViewed":false,"timestamps":[43]}}}}});
        writeln!(peer, "{receipt}").expect("idle native receipt");
        assert_eq!(peer.read(&mut [0; 1]).expect("owned shutdown"), 0);
    });
    let receiver = tilo_messaging_protocol::signal_runtime::SignalReceiver::new(
        owner(),
        OWN_ACI.into(),
        stream,
    )
    .expect("account-bound receiver");
    let guard = service
        .start_receiver(receiver)
        .expect("independent receive worker");
    go.send(()).expect("receipt arrival");
    let deadline = std::time::Instant::now()
        .checked_add(tilo_test_env::hang_bound())
        .expect("bounded wait");
    while !woke.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
        std::thread::park_timeout(ui::time::Duration::from_millis(1));
    }
    assert!(
        woke.load(Ordering::SeqCst),
        "the actual idle receipt persists and wakes private history"
    );
    let deadline = std::time::Instant::now()
        .checked_add(tilo_test_env::hang_bound())
        .expect("bounded wait");
    let mut delivery = None;
    while delivery != Some(Delivery::RecipientRead) && std::time::Instant::now() < deadline {
        let page = wait_for_page(&client, 2).await;
        delivery = page
            .iter()
            .find(|m| m.text.as_str() == "private outgoing")
            .and_then(|m| m.delivery);
        std::thread::park_timeout(ui::time::Duration::from_millis(1));
    }
    assert_eq!(delivery, Some(Delivery::RecipientRead));
    assert!(!guard.stopped());
    drop(guard);
    inbox.join().expect("only the owned receiver closes");
}
