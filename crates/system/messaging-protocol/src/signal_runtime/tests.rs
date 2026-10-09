use super::*;
use crate::ProtocolReference;
use crate::routing::LinkState;
use crate::signal_session::JournalError;
use std::io::BufReader;

fn owner() -> Owner {
    Owner {
        principal: "11".repeat(32),
        device: "device-a".into(),
        service: Service::Signal,
        account: "tilo-account-a".into(),
        linking_generation: 1,
    }
}

fn route() -> Route {
    Route::new(
        Service::Signal,
        "tilo-account-a".into(),
        "+15555550102".into(),
    )
    .unwrap()
}

fn accounts() -> accounts::AccountStore {
    let mut accounts = accounts::AccountStore::new();
    accounts.upsert(accounts::Account::new("tilo-account-a", "signal"));
    accounts
}

fn binding() -> LiveBinding {
    LiveBinding {
        owner: owner(),
        state: LinkState::Linked,
    }
}

fn journal(root: &Path) -> EncryptedJournal {
    EncryptedJournal::open(root, &owner(), secret_vault::VaultKey::from_bytes([7; 32])).unwrap()
}

fn runtime(root: &Path, stream: UnixStream) -> SignalRuntime {
    SignalRuntime::from_journal(owner(), "+15555550101".into(), stream, journal(root)).unwrap()
}

fn event(account: &str) -> Vec<u8> {
    let value = serde_json::json!({"jsonrpc":"2.0","method":"receive","params":{
        "account":account,"envelope":{"sourceUuid":"01234567-89ab-cdef-0123-456789abcdef",
        "dataMessage":{"timestamp":42,"message":"received text","expiresInSeconds":0,
        "isExpirationUpdate":false,"viewOnce":false,"isEndSession":false,
        "isProfileKeyUpdate":false,"hasProfileKey":true}}}});
    let mut encoded = serde_json::to_vec(&value).unwrap();
    encoded.push(b'\n');
    encoded
}

#[test]
fn socket_send_maps_registry_account_to_signal_and_restart_refuses_duplicates() {
    let root = tempfile::tempdir().unwrap();
    let (client, mut peer) = UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let peer_reader = peer.try_clone().unwrap();
    let daemon = std::thread::spawn(move || {
        let mut request = String::new();
        BufReader::new(peer_reader).read_line(&mut request).unwrap();
        let value: serde_json::Value = serde_json::from_str(&request).unwrap();
        assert_eq!(value["params"]["account"], "+15555550101");
        assert_eq!(
            value["params"]["recipient"],
            serde_json::json!(["+15555550102"])
        );
        assert_eq!(value["params"]["message"], "private text");
        assert_eq!(value["id"], "op");
        peer.write_all(&event("+15555550101")).unwrap();
        peer.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":\"op\",\"result\":{\"timestamp\":43}}\n")
            .unwrap();
    });
    let mut runtime = runtime(root.path(), client);
    assert_eq!(
        runtime
            .send(
                &accounts(),
                &binding(),
                &route(),
                "op",
                "private text",
                Duration::from_secs(2)
            )
            .unwrap()
            .timestamp,
        43
    );
    assert_eq!(runtime.take_events().len(), 1);
    assert_eq!(
        runtime.status("op"),
        Some(Status::Acknowledged(ProtocolReference::Signal {
            timestamp: 43
        }))
    );
    daemon.join().unwrap();
    drop(runtime);
    let (client, mut peer) = UnixStream::pair().unwrap();
    peer.set_nonblocking(true).unwrap();
    let mut restarted = self::runtime(root.path(), client);
    assert_eq!(
        restarted.send(
            &accounts(),
            &binding(),
            &route(),
            "op",
            "private text",
            Duration::from_secs(1)
        ),
        Err(CheckedSendError::Transport(SendError::Journal(
            JournalError::AlreadyReserved
        )))
    );
    assert_eq!(
        peer.read(&mut [0; 1]).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn silent_socket_expires_and_preserves_uncertainty_without_retry() {
    let root = tempfile::tempdir().unwrap();
    let (client, mut peer) = UnixStream::pair().unwrap();
    let mut runtime = runtime(root.path(), client);
    let start = tilo_clock::now();
    assert_eq!(
        runtime.send(
            &accounts(),
            &binding(),
            &route(),
            "op",
            "private text",
            Duration::from_millis(40)
        ),
        Err(CheckedSendError::Transport(SendError::Uncertain))
    );
    assert!(start.elapsed() < Duration::from_secs(2));
    assert_eq!(runtime.status("op"), Some(Status::Uncertain));
    let mut request = String::new();
    BufReader::new(peer.try_clone().unwrap())
        .read_line(&mut request)
        .unwrap();
    assert!(request.ends_with('\n'));
    assert_eq!(
        runtime.send(
            &accounts(),
            &binding(),
            &route(),
            "new-op",
            "private text",
            Duration::from_secs(1)
        ),
        Err(CheckedSendError::Transport(SendError::BeforeSubmission(
            Error::ConnectionUnavailable
        )))
    );
    peer.set_nonblocking(true).unwrap();
    assert_eq!(
        peer.read(&mut [0; 1]).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn receive_uses_signal_account_and_returns_registry_scoped_identity() {
    let root = tempfile::tempdir().unwrap();
    let (client, mut peer) = UnixStream::pair().unwrap();
    let mut runtime = runtime(root.path(), client);
    peer.write_all(&event("+15555550101")).unwrap();
    let received = runtime.receive(Duration::from_secs(1)).unwrap();
    assert_eq!(received.id.account, "tilo-account-a");
    assert_eq!(received.text.as_str(), "received text");
    peer.write_all(&event("+15555550999")).unwrap();
    assert_eq!(
        runtime.receive(Duration::from_secs(1)).unwrap_err(),
        Error::WrongAccount
    );
    assert_eq!(
        runtime.receive(Duration::from_secs(1)).unwrap_err(),
        Error::ConnectionUnavailable
    );
}

#[test]
fn invalid_budgets_and_revoked_routes_write_no_bytes() {
    let root = tempfile::tempdir().unwrap();
    let (client, mut peer) = UnixStream::pair().unwrap();
    peer.set_nonblocking(true).unwrap();
    let mut runtime = runtime(root.path(), client);
    for timeout in [
        Duration::ZERO,
        MAX_OPERATION_TIMEOUT + Duration::from_secs(1),
    ] {
        assert_eq!(
            runtime.send(
                &accounts(),
                &binding(),
                &route(),
                "op",
                "private text",
                timeout
            ),
            Err(CheckedSendError::Transport(SendError::BeforeSubmission(
                Error::InvalidRequest
            )))
        );
    }
    let mut revoked = binding();
    revoked.state = LinkState::Revoked;
    assert_eq!(
        runtime.send(
            &accounts(),
            &revoked,
            &route(),
            "op",
            "private text",
            Duration::from_secs(1)
        ),
        Err(CheckedSendError::Routing(crate::routing::Refusal::Revoked))
    );
    assert!(runtime.status("op").is_none());
    assert_eq!(
        peer.read(&mut [0; 1]).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn frozen_journal_owner_and_signal_identity_are_required() {
    for (account, device) in [("display-name", "device-a"), ("+15555550101", "device-b")] {
        let root = tempfile::tempdir().unwrap();
        let (client, _) = UnixStream::pair().unwrap();
        let supplied = Owner {
            device: device.into(),
            ..owner()
        };
        assert!(matches!(
            SignalRuntime::from_journal(supplied, account.into(), client, journal(root.path())),
            Err(OpenError::WrongOwner)
        ));
    }
    assert!(valid_signal_account("01234567-89ab-cdef-0123-456789abcdef"));
}

#[test]
fn deadline_is_total_and_buffered_bytes_cannot_extend_it() {
    let (client, mut peer) = UnixStream::pair().unwrap();
    let deadline = Deadline::default();
    deadline.arm(Duration::from_secs(1)).unwrap();
    let mut reader = DeadlineReader {
        stream: DeadlineStream {
            stream: client,
            deadline: deadline.clone(),
        },
        buffer: Zeroizing::new(vec![0; 8192]),
        start: 0,
        end: 0,
    };
    peer.write_all(b"one\ntwo\n").unwrap();
    let mut first = String::new();
    reader.read_line(&mut first).unwrap();
    assert_eq!(first, "one\n");
    // Expire the actual deadline used by the production reader, while bytes
    // remain buffered. A per-read timeout alone would incorrectly admit them.
    *deadline.0.lock().unwrap() = tilo_clock::now().checked_sub(Duration::from_secs(1));
    assert_eq!(
        reader.read_line(&mut String::new()).unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
}

#[test]
fn manual_receiver_resolves_aci_to_number_and_requires_its_subscription() {
    let aci = "11111111-1111-4111-8111-111111111111";
    for subscription in [Some(7), Some(8), None] {
        let (stream, mut peer) = UnixStream::pair().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let provider = std::thread::spawn(move || {
            let mut reader = BufReader::new(peer.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], "listAccounts");
            writeln!(peer, "{}", serde_json::json!({"jsonrpc":"2.0","id":request["id"],"result":[{"number":"+15555550101"}]})).unwrap();
            let mut own_line = String::new();
            reader.read_line(&mut own_line).unwrap();
            let own_probe: serde_json::Value = serde_json::from_str(&own_line).unwrap();
            assert_eq!(own_probe["method"], "listIdentities");
            assert_eq!(
                own_probe["params"]["number"],
                own_probe["params"]["account"]
            );
            writeln!(peer, "{}", serde_json::json!({"jsonrpc":"2.0","id":own_probe["id"],"result":serde_json::json!([{"uuid":aci,"number":"+15555550101","trustLevel":"TRUSTED_VERIFIED","fingerprint":format!("05 {}", vec!["aa"; 32].join(" "))}])})).unwrap();
            line.clear();
            reader.read_line(&mut line).unwrap();
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], "subscribeReceive");
            assert_eq!(
                request["params"],
                serde_json::json!({"account":"+15555550101"})
            );
            writeln!(
                peer,
                "{}",
                serde_json::json!({"jsonrpc":"2.0","id":request["id"],"result":7})
            )
            .unwrap();
            let incoming: serde_json::Value =
                serde_json::from_slice(&event("+15555550101")).unwrap();
            let mut params = serde_json::json!({"result":incoming["params"]});
            if let Some(subscription) = subscription {
                params["subscription"] = serde_json::json!(subscription);
            }
            writeln!(
                peer,
                "{}",
                serde_json::json!({"jsonrpc":"2.0","method":"receive","params":params})
            )
            .unwrap();
        });
        let mut receiver = SignalReceiver::new(owner(), aci.into(), stream).unwrap();
        let outcome = receiver.receive();
        if subscription == Some(7) {
            assert_eq!(outcome.unwrap().text.as_str(), "received text");
        } else {
            assert!(matches!(outcome, Err(Error::WrongRequest)));
            assert!(matches!(
                receiver.receive(),
                Err(Error::ConnectionUnavailable)
            ));
            assert!(receiver.take_events().is_empty());
        }
        provider.join().unwrap();
    }
}
