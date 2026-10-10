use super::*;
use accounts::{AccessDecision, SecretItem, SecretStore};
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    rc::Rc,
    sync::atomic::{AtomicUsize, Ordering},
};
use tilo_action_broker::collab_broker_capnp::tests::{
    CertifiedPersonFixture, certified_person_fixture,
};
use tilo_messaging::ServiceAccount;
use tilo_messaging_protocol::{
    Service, journal::EncryptedJournal, signal_broker::History, signal_runtime::SignalRuntime,
};
use ui::record::RecordId;

#[path = "messaging_activation_tests.rs"]
mod owned_activation;

struct Provider {
    reads: Arc<AtomicUsize>,
}
impl SignalAuthority for Provider {
    fn check_account_scope(&self, scope: &str) -> Result<(), SendError> {
        if scope == "signal:11111111-1111-4111-8111-111111111111" {
            self.check_account()
        } else {
            Err(SendError::Revoked)
        }
    }
    fn check_account(&self) -> Result<(), SendError> {
        // Deliberately injected synthetic account source; the native current
        // record producer is exercised separately by the mounted fixtures.
        Ok(())
    }
    fn authorize(&self, _: &Route) -> Result<Authorization, SendError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Err(SendError::Unavailable)
    }
    fn incoming(&self, _: &str) -> Result<Authorization, SendError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Err(SendError::Unavailable)
    }
}
impl tilo_messaging_protocol::external_broker::ExternalAuthority for Provider {
    fn authorize(&self, route: &Route) -> Result<Authorization, SendError> {
        SignalAuthority::authorize(self, route)
    }
    fn incoming(&self, recipient: &str) -> Result<Authorization, SendError> {
        SignalAuthority::incoming(self, recipient)
    }
}
struct Rig {
    admission: CertifiedPersonFixture,
    broker: Arc<Mutex<CredentialBroker>>,
    request: AccessRequest,
    owner: Owner,
    root: tempfile::TempDir,
}
impl Rig {
    fn new() -> Self {
        let admission = certified_person_fixture();
        let root = tempfile::tempdir().unwrap();
        let mut store = SecretStore::open_in(root.path());
        let mut credential = SecretItem::login(
            "Synthetic Signal account",
            "fixture-signal",
            "synthetic private credential",
        );
        credential.add_origin("https://signal.test").unwrap();
        store.put(&mut credential).unwrap();
        let request = AccessRequest {
            app_id: "so.tilo.messages".into(),
            item_id: credential.id,
            purpose: "Use the explicitly linked Signal account".into(),
            origin: Some("https://signal.test".into()),
        };
        let owner = Owner {
            principal: admission.context.principal().into(),
            device: "sdk-profile-device".into(),
            service: Service::Signal,
            account: "fixture-signal".into(),
            linking_generation: 1,
        };
        Self {
            admission,
            broker: Arc::new(Mutex::new(CredentialBroker::new(store))),
            request,
            owner,
            root,
        }
    }
    fn startup(&self) -> SignalAccountStartup {
        SignalAccountStartup {
            identity: NativeMessagingIdentity::new(
                self.admission.context.clone(),
                self.admission.host.clone(),
                self.owner.clone(),
            )
            .unwrap(),
            credential: NativeMessagingCredential::new(
                self.broker.clone(),
                self.request.clone(),
                self.owner.clone(),
            )
            .unwrap(),
            authority: Arc::new(Provider {
                reads: Arc::default(),
            }),
        }
    }
    fn grant(&self, decision: AccessDecision) {
        self.broker
            .lock()
            .unwrap()
            .request_scoped(&self.request, &credential_scope(&self.owner), |_, _, _| {
                decision
            })
            .unwrap();
    }
    fn grant_whatsapp(&self, decision: AccessDecision) {
        let mut owner = self.owner.clone();
        owner.service = Service::Whatsapp;
        self.broker
            .lock()
            .unwrap()
            .request_scoped(&self.request, &credential_scope(&owner), |_, _, _| decision)
            .unwrap();
    }
    fn whatsapp_startup(&self) -> whatsapp::WhatsappAccountStartup {
        let SignalAccountStartup {
            mut identity,
            mut credential,
            authority: _,
        } = self.startup();
        identity.owner.service = Service::Whatsapp;
        credential.owner.service = Service::Whatsapp;
        whatsapp::WhatsappAccountStartup {
            identity,
            credential,
            authority: Arc::new(Provider {
                reads: Arc::default(),
            }),
        }
    }
    fn apps(&self) -> super::super::InProcessApps {
        super::super::InProcessApps::new(
            Rc::new(durable_store::MemoryStore::new()),
            &self.owner.device,
            &self.owner.principal,
        )
    }
    fn admission_live(&self) -> bool {
        self.admission.context.revalidate().is_ok()
    }
}
struct MountedMessages {
    client: tilo_messaging::MessagingClient,
    harness: ui::headless::FrameHarness,
}
impl Drop for MountedMessages {
    fn drop(&mut self) {
        self.harness.close_components();
    }
}
fn mount_messages(apps: &mut super::super::InProcessApps, state: AccountState) -> MountedMessages {
    let mut service = None;
    let expected_loans =
        if apps.native_broker_socket.is_some() && apps.messaging_record_wake.is_some() {
            2
        } else {
            1
        };
    let component = apps
        .mount(
            messages::definition(),
            "6a3f0c2e-0f1b-4c5d-9e8f-123456789abc",
            41,
            RecordId::room_default(),
            |context| {
                let loans = context.take_lent();
                assert_eq!(
                    loans.len(),
                    expected_loans,
                    "the normal Messages mount lends each host capability once"
                );
                let mut client = None;
                for loan in loans {
                    match loan.downcast::<tilo_messaging::MessagingClient>() {
                        Ok(service) => {
                            assert!(client.replace(service).is_none());
                        }
                        Err(loan) => context
                            .lend(*loan.downcast::<tilo_messaging::EnrollmentClient>().unwrap()),
                    }
                }
                let client = client.unwrap();
                assert_eq!(
                    client.account_state(&ServiceAccount::new("signal", "fixture-signal")),
                    state
                );
                service = Some((*client).clone());
                context.lend(*client);
            },
        )
        .unwrap();
    let mut harness = ui::headless::FrameHarness::from_component(
        component,
        (900, 600),
        ui::application::ComponentRunnerInputs::from_native_process(),
    )
    .unwrap();
    harness.relayout();
    assert!(!harness.dump_json().contains("synthetic private credential"));
    MountedMessages {
        client: service.unwrap(),
        harness,
    }
}

fn assemble_fixture(
    owner: &Owner,
    root: &std::path::Path,
    streams: (UnixStream, UnixStream),
    authority: Arc<dyn SignalAuthority>,
) -> Result<(SignalBroker, SignalReceiver), SendError> {
    let key = || secret_vault::VaultKey::from_bytes([7; 32]);
    let journal = EncryptedJournal::open(&root.join("journal"), owner, key())
        .map_err(|_| SendError::Unavailable)?;
    let runtime = SignalRuntime::from_journal(
        owner.clone(),
        "11111111-1111-4111-8111-111111111111".into(),
        streams.0,
        journal,
    )
    .map_err(|_| SendError::Unavailable)?;
    let history = History::open(&root.join("history"), owner, key())?;
    let broker = SignalBroker::new(
        runtime,
        history,
        authority,
        "11111111-1111-4111-8111-111111111111".into(),
    )?;
    let receiver = SignalReceiver::new(
        owner.clone(),
        "11111111-1111-4111-8111-111111111111".into(),
        streams.1,
    )
    .map_err(|_| SendError::Unavailable)?;
    Ok((broker, receiver))
}
fn daemon(mut peer: UnixStream, probes: Arc<AtomicUsize>) -> std::thread::JoinHandle<()> {
    peer.set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(peer.try_clone().unwrap());
        let mut line = String::new();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
        let probe: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(probe["method"], "listAccounts");
        probes.fetch_add(1, Ordering::SeqCst);
        writeln!(peer, "{}", serde_json::json!({"jsonrpc":"2.0", "id":probe["id"], "result":[{ "number":"+15555550101"}]})).unwrap();
        let mut own_line = String::new();
        reader.read_line(&mut own_line).unwrap();
        let own_probe: serde_json::Value = serde_json::from_str(&own_line).unwrap();
        assert_eq!(own_probe["method"], "listIdentities");
        assert_eq!(
            own_probe["params"]["number"],
            own_probe["params"]["account"]
        );
        writeln!(peer, "{}", serde_json::json!({"jsonrpc":"2.0","id":own_probe["id"],"result":serde_json::json!([{"uuid":"11111111-1111-4111-8111-111111111111","number":"+15555550101","trustLevel":"TRUSTED_VERIFIED","fingerprint":format!("05 {}", vec!["aa"; 32].join(" "))}])})).unwrap();
        line.clear();
        if reader.read_line(&mut line).unwrap() != 0 {
            let subscription: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(subscription["method"], "subscribeReceive");
            assert_eq!(
                subscription["params"],
                serde_json::json!({"account":"+15555550101"})
            );
            writeln!(
                peer,
                "{}",
                serde_json::json!({"jsonrpc":"2.0","id":subscription["id"],"result":0})
            )
            .unwrap();
            line.clear();
            assert_eq!(
                reader.read_line(&mut line).unwrap(),
                0,
                "no additional provider operations"
            );
        }
    })
}

#[test]
fn native_startup_ungranted_and_consumed_once_are_locked_without_rpc() {
    for decision in [None, Some(AccessDecision::AllowOnce)] {
        let rig = Rig::new();
        if let Some(decision) = decision {
            rig.grant(decision);
        }
        let mut apps = rig.apps();
        let supervisor = apps
            .start_signal_account(rig.startup(), |_| {
                panic!("ungranted startup must never assemble or contact a daemon")
            })
            .unwrap();
        assert_eq!(
            supervisor
                .client()
                .account_state(&ServiceAccount::new("signal", "fixture-signal")),
            AccountState::Locked
        );
        drop(supervisor);
        mount_messages(&mut apps, AccountState::Locked);
        assert!(
            rig.broker
                .lock()
                .unwrap()
                .request_existing(&rig.request)
                .is_err(),
            "startup never creates a grant"
        );
        assert!(rig.admission_live());
        drop(rig);
    }
}

#[test]
fn native_startup_existing_grant_owns_real_streams_mounts_and_rechecks_revocation() {
    let rig = Rig::new();
    rig.grant(AccessDecision::AllowForSession);
    let mut apps = rig.apps();
    let root = tempfile::tempdir().unwrap();
    let (sender, peer) = UnixStream::pair().unwrap();
    let probes = Arc::new(AtomicUsize::new(0));
    let send_daemon = daemon(peer, probes.clone());
    let (receiver, peer) = UnixStream::pair().unwrap();
    let receive_daemon = daemon(peer, probes.clone());
    let owner = rig.owner.clone();
    let supervisor = apps
        .start_signal_account(rig.startup(), |authority| {
            assemble_fixture(&owner, root.path(), (sender, receiver), authority)
        })
        .unwrap();
    let account = ServiceAccount::new("signal", "fixture-signal");
    assert_eq!(
        probes.load(Ordering::SeqCst),
        2,
        "the actual send and independent receive handles verify their own account"
    );
    assert_eq!(
        supervisor.client().account_state(&account),
        AccountState::Ready
    );
    mount_messages(&mut apps, AccountState::Ready);
    rig.broker
        .lock()
        .unwrap()
        .revoke_app("so.tilo.messages")
        .unwrap();
    assert_eq!(
        supervisor.client().account_state(&account),
        AccountState::Locked,
        "current credential revocation is visible without an attempted send"
    );
    assert_eq!(
        probes.load(Ordering::SeqCst),
        2,
        "revocation creates no extra RPC or retry"
    );
    assert!(
        rig.admission_live(),
        "credential revocation did not retire the independently admitted person"
    );
    drop(rig);
    drop(apps);
    drop(supervisor);
    send_daemon.join().unwrap();
    receive_daemon.join().unwrap();
}

#[test]
fn native_startup_wrong_profile_and_retired_admission_refuse_before_assembly() {
    let rig = Rig::new();
    rig.grant(AccessDecision::AllowForSession);
    let mut apps = super::super::InProcessApps::new(
        Rc::new(durable_store::MemoryStore::new()),
        "other-sdk-device",
        &rig.owner.principal,
    );
    assert!(
        apps.start_signal_account(rig.startup(), |_| panic!(
            "wrong profile must not contact a daemon"
        ))
        .is_err()
    );
    let result = {
        let startup = rig.startup();
        rig.admission.caller.retire_execution();
        rig.apps().start_signal_account(startup, |_| {
            panic!("retired admission must not contact a daemon")
        })
    };
    assert!(result.is_err());
    drop(result);
    assert!(!rig.admission_live());
    drop(rig);
}

#[test]
fn native_startup_refuses_another_cryptographic_device_and_cross_account_grant() {
    let rig = Rig::new();
    let mut selected = rig.admission.host.clone();
    let certificate = selected.device_cert.clone();
    let root = ed25519_dalek::SigningKey::from_bytes(&[144; 32]);
    selected.device_cert.user_public_key = root.verifying_key().to_bytes();
    tilo_user_identity::sign_device_cert(&mut selected.device_cert, &root).unwrap();
    let workspace = tilo_relay_protocol::room_workspace_id(
        &collab_types::person_room(&certificate.user_public_key).to_string(),
    )
    .unwrap();
    selected
        .workspace_device_certs
        .insert(workspace, certificate);
    assert!(
        NativeMessagingIdentity::new(rig.admission.context.clone(), selected, rig.owner.clone())
            .is_ok(),
        "native custody selection follows the admitted person's room rather than the host's unrelated default identity"
    );
    let mut host = rig.admission.host.clone();
    host.device_signing_key = ed25519_dalek::SigningKey::from_bytes(&[143; 32]);
    assert!(
        NativeMessagingIdentity::new(rig.admission.context.clone(), host, rig.owner.clone())
            .is_err()
    );
    let mut restricted = rig.admission.host.clone();
    restricted.device_cert.capabilities.foreign = true;
    tilo_user_identity::sign_device_cert(
        &mut restricted.device_cert,
        &ed25519_dalek::SigningKey::from_bytes(&[141; 32]),
    )
    .unwrap();
    assert!(
        NativeMessagingIdentity::new(rig.admission.context.clone(), restricted, rig.owner.clone())
            .is_err(),
        "current signed foreign-device restrictions cannot disappear behind an older personal admission"
    );
    let result = {
        let mut startup = Some(rig.startup());
        startup.as_mut().unwrap().credential.owner.account = "other-signal-account".into();
        let result = rig
            .apps()
            .start_signal_account(startup.take().unwrap(), |_| {
                panic!("another account's grant must not assemble this account")
            });
        drop(startup);
        result
    };
    assert!(result.is_err());
    drop(result);
    let mut request = rig.request.clone();
    request.app_id = "so.tilo.other-app".into();
    assert!(
        NativeMessagingCredential::new(rig.broker.clone(), request, rig.owner.clone()).is_err()
    );
    assert!(rig.admission_live());
    drop(rig);
}

#[test]
fn native_startup_account_verification_failure_is_visible_and_never_ready() {
    let rig = Rig::new();
    rig.grant(AccessDecision::AllowForSession);
    let mut apps = rig.apps();
    let supervisor = apps
        .start_signal_account(rig.startup(), |_| Err(SendError::Unavailable))
        .unwrap();
    assert_eq!(
        supervisor
            .client()
            .account_state(&ServiceAccount::new("signal", "fixture-signal")),
        AccountState::Failed
    );
    mount_messages(&mut apps, AccountState::Failed);
    rig.admission.current.store(false, Ordering::SeqCst);
    assert_eq!(
        supervisor
            .client()
            .account_state(&ServiceAccount::new("signal", "fixture-signal")),
        AccountState::Locked
    );
    drop(supervisor);
    assert!(!rig.admission_live());
    drop(rig);
}

#[test]
fn native_startup_authority_rechecks_grant_before_every_provider_read() {
    let rig = Rig::new();
    rig.grant(AccessDecision::AllowForSession);
    let reads = Arc::new(AtomicUsize::new(0));
    let mut authority = None;
    {
        let startup = SignalAccountStartup {
            authority: Arc::new(Provider {
                reads: reads.clone(),
            }),
            ..rig.startup()
        };
        rig.apps()
            .start_signal_account(startup, |checked| {
                authority = Some(checked);
                Err(SendError::Unavailable)
            })
            .unwrap();
    }
    let authority = authority.unwrap();
    assert!(matches!(
        authority.incoming("synthetic-aci"),
        Err(SendError::Unavailable)
    ));
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    rig.broker
        .lock()
        .unwrap()
        .revoke_app("so.tilo.messages")
        .unwrap();
    assert!(matches!(
        authority.incoming("synthetic-aci"),
        Err(SendError::Revoked)
    ));
    assert_eq!(
        reads.load(Ordering::SeqCst),
        1,
        "revoked credentials never reach provider metadata, private history or native I/O"
    );
    assert!(rig.admission_live());
    drop(rig);
}

#[test]
fn native_startup_production_without_grant_touches_neither_daemon_nor_custody() {
    let rig = Rig::new();
    let root = tempfile::tempdir().unwrap();
    let private_root = root.path().join("unopened-account");
    let (sender, mut send_peer) = UnixStream::pair().unwrap();
    let (receiver, mut receive_peer) = UnixStream::pair().unwrap();
    let mut apps = rig.apps();
    apps.bind_messaging_record_wake(Arc::new(|| {}));
    let supervisor = apps
        .start_native_signal_account(
            rig.startup(),
            NativeSignalHandles {
                private_root: private_root.clone(),
                signal_account: "+15555550101".into(),
                sender,
                receiver,
            },
        )
        .unwrap();
    assert_eq!(
        supervisor
            .client()
            .account_state(&ServiceAccount::new("signal", "fixture-signal")),
        AccountState::Locked
    );
    drop(supervisor);
    assert!(
        !private_root.exists(),
        "ungranted production startup never opens or creates private account storage"
    );
    assert_eq!(
        send_peer.read(&mut [0; 1]).unwrap(),
        0,
        "no daemon RPC before a grant"
    );
    assert_eq!(
        receive_peer.read(&mut [0; 1]).unwrap(),
        0,
        "no inbox RPC before a grant"
    );
    mount_messages(&mut apps, AccountState::Locked);
    assert!(rig.admission_live());
    drop(rig);
}

#[test]
fn native_startup_existing_grant_does_not_cover_other_item_or_origin() {
    for other_item in [false, true] {
        let rig = Rig::new();
        rig.grant(AccessDecision::AllowForSession);
        let supervisor = {
            let mut startup = Some(rig.startup());
            if other_item {
                startup.as_mut().unwrap().credential.request.item_id = accounts::SecretId::new();
            } else {
                startup.as_mut().unwrap().credential.request.origin =
                    Some("https://lookalike-signal.test".into());
            }
            let account = rig
                .apps()
                .start_signal_account(startup.take().unwrap(), |_| {
                    panic!("another item or origin must not open the native account")
                })
                .unwrap();
            drop(startup);
            account
        };
        assert_eq!(
            supervisor
                .client()
                .account_state(&ServiceAccount::new("signal", "fixture-signal")),
            AccountState::Locked
        );
        drop(supervisor);
        assert!(rig.admission_live());
        drop(rig);
    }
}

#[test]
fn native_startup_later_explicit_grant_activates_the_original_open_messages_loan_once() {
    let rig = Rig::new();
    let mut apps = rig.apps();
    let account = apps
        .start_signal_account(rig.startup(), |_| panic!("no grant, no assembly"))
        .unwrap();
    let mut mounted = mount_messages(&mut apps, AccountState::Locked);
    let identity = ServiceAccount::new("signal", "fixture-signal");
    rig.grant(AccessDecision::AllowForSession);
    let root = tempfile::tempdir().unwrap();
    let probes = Arc::new(AtomicUsize::new(0));
    let (sender, peer) = UnixStream::pair().unwrap();
    let send_daemon = daemon(peer, probes.clone());
    let (receiver, peer) = UnixStream::pair().unwrap();
    let receive_daemon = daemon(peer, probes.clone());
    account
        .activate(|authority| {
            assemble_fixture(&rig.owner, root.path(), (sender, receiver), authority)
        })
        .unwrap();
    assert_eq!(
        mounted.client.account_state(&identity),
        AccountState::Ready,
        "the original service loan in the still-open Messages component sees activation without a remount"
    );
    mounted.harness.relayout();
    assert!(
        !mounted
            .harness
            .dump_json()
            .contains("synthetic private credential")
    );
    assert_eq!(probes.load(Ordering::SeqCst), 2);
    assert!(matches!(
        account.activate(|_| panic!(
            "duplicate activation must never assemble a second daemon or receiver"
        )),
        Err(SendError::AlreadyReserved)
    ));
    assert_eq!(probes.load(Ordering::SeqCst), 2);
    rig.broker
        .lock()
        .unwrap()
        .revoke_app("so.tilo.messages")
        .unwrap();
    assert_eq!(
        mounted.client.account_state(&identity),
        AccountState::Locked
    );
    drop(account);
    assert!(rig.admission_live());
    drop(rig);
    drop(mounted);
    drop(apps);
    send_daemon.join().unwrap();
    receive_daemon.join().unwrap();
}

#[test]
fn native_startup_recovery_rechecks_permission_and_can_retry_only_failed_initial_assembly() {
    let rig = Rig::new();
    rig.grant(AccessDecision::AllowForSession);
    let account = rig
        .apps()
        .start_signal_account(rig.startup(), |_| Err(SendError::Unavailable))
        .unwrap();
    let attempts = AtomicUsize::new(0);
    assert!(matches!(
        account.activate(|_| {
            attempts.fetch_add(1, Ordering::SeqCst);
            Err(SendError::Unavailable)
        }),
        Err(SendError::Unavailable)
    ));
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1,
        "failed pre-installation setup has an explicit recovery door"
    );
    rig.broker
        .lock()
        .unwrap()
        .revoke_app("so.tilo.messages")
        .unwrap();
    assert!(matches!(
        account.activate(|_| panic!("revoked recovery must not touch an adapter")),
        Err(SendError::Revoked)
    ));
    assert_eq!(
        account
            .client()
            .account_state(&ServiceAccount::new("signal", "fixture-signal")),
        AccountState::Locked
    );
    drop(account);
    assert!(rig.admission_live());
    drop(rig);
}

#[tokio::test]
async fn native_whatsapp_without_existing_grant_never_constructs_sdk() {
    let rig = Rig::new();
    let account = rig
        .apps()
        .prepare_whatsapp_account(rig.whatsapp_startup())
        .unwrap();
    let service = ServiceAccount::new("whatsapp", &rig.owner.account);
    assert_eq!(
        account.client().account_state(&service),
        AccountState::Locked
    );
    assert_eq!(
        account
            .activate(|_| async { panic!("no grant must mean no SDK factory") })
            .await,
        Err(SendError::Revoked)
    );
    assert!(rig.admission_live());
    drop(account);
    drop(rig);
}

#[tokio::test]
async fn native_whatsapp_failure_is_not_ready_and_recovery_requires_current_grant() {
    let rig = Rig::new();
    let account = rig
        .apps()
        .prepare_whatsapp_account(rig.whatsapp_startup())
        .unwrap();
    let service = ServiceAccount::new("whatsapp", &rig.owner.account);
    let original = account.client();
    rig.grant_whatsapp(AccessDecision::AllowForSession);
    assert_eq!(
        account
            .activate(|_| async { Err(SendError::Unavailable) })
            .await,
        Err(SendError::Unavailable)
    );
    assert_eq!(original.account_state(&service), AccountState::Failed);
    let calls = AtomicUsize::new(0);
    assert_eq!(
        account
            .activate(|_| async {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(SendError::Unavailable)
            })
            .await,
        Err(SendError::Unavailable)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    rig.broker
        .lock()
        .unwrap()
        .revoke_app("so.tilo.messages")
        .unwrap();
    assert_eq!(original.account_state(&service), AccountState::Locked);
    assert_eq!(
        account
            .activate(|_| async { panic!("revoked recovery must not open native custody") })
            .await,
        Err(SendError::Revoked)
    );
    assert!(rig.admission_live());
    drop(account);
    drop(rig);
}

#[tokio::test]
async fn native_whatsapp_cancelled_setup_drops_reservation_and_never_looks_ready() {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};
    let rig = Rig::new();
    rig.grant_whatsapp(AccessDecision::AllowForSession);
    let account = rig
        .apps()
        .prepare_whatsapp_account(rig.whatsapp_startup())
        .unwrap();
    let mut first = Box::pin(account.activate(|_| std::future::pending()));
    assert!(matches!(
        first.as_mut().poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    assert_eq!(
        account
            .activate(|_| async { panic!("another activation must perform no I/O") })
            .await,
        Err(SendError::AlreadyReserved)
    );
    drop(first);
    assert_eq!(
        account
            .client()
            .account_state(&ServiceAccount::new("whatsapp", &rig.owner.account)),
        AccountState::Failed
    );
    assert_eq!(
        account
            .activate(|_| async { Err(SendError::Unavailable) })
            .await,
        Err(SendError::Unavailable)
    );
    assert!(rig.admission_live());
    drop(account);
    drop(rig);
}

#[tokio::test]
async fn native_whatsapp_current_grant_guards_retained_provider_authority() {
    let rig = Rig::new();
    rig.grant_whatsapp(AccessDecision::AllowForSession);
    let reads = Arc::new(AtomicUsize::new(0));
    let mut startup = Some(rig.whatsapp_startup());
    startup.as_mut().unwrap().authority = Arc::new(Provider {
        reads: reads.clone(),
    });
    let account = rig
        .apps()
        .prepare_whatsapp_account(startup.take().unwrap())
        .unwrap();
    drop(startup);
    let mut authority = None;
    assert_eq!(
        account
            .activate(|checked| {
                authority = Some(checked);
                async { Err(SendError::Unavailable) }
            })
            .await,
        Err(SendError::Unavailable)
    );
    let authority = authority.unwrap();
    assert!(matches!(
        authority.incoming("synthetic-recipient@lid"),
        Err(SendError::Unavailable)
    ));
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    rig.broker
        .lock()
        .unwrap()
        .revoke_app("so.tilo.messages")
        .unwrap();
    assert!(matches!(
        authority.incoming("synthetic-recipient@lid"),
        Err(SendError::Revoked)
    ));
    assert_eq!(
        reads.load(Ordering::SeqCst),
        1,
        "revoked SDK events never read private provider metadata"
    );
    assert!(rig.admission_live());
    drop(account);
    drop(rig);
}

#[test]
fn native_whatsapp_registration_refuses_signal_owner_and_other_profile() {
    let rig = Rig::new();
    let SignalAccountStartup {
        identity,
        credential,
        authority: _,
    } = rig.startup();
    let startup = whatsapp::WhatsappAccountStartup {
        identity,
        credential,
        authority: Arc::new(Provider {
            reads: Arc::default(),
        }),
    };
    assert!(rig.apps().prepare_whatsapp_account(startup).is_err());
    let mut apps = super::super::InProcessApps::new(
        Rc::new(durable_store::MemoryStore::new()),
        "other-sdk-profile",
        &rig.owner.principal,
    );
    assert!(
        apps.prepare_whatsapp_account(rig.whatsapp_startup())
            .is_err()
    );
    assert!(rig.admission_live());
    drop(rig);
}

#[path = "messaging_native_source_tests.rs"]
mod native_source;
