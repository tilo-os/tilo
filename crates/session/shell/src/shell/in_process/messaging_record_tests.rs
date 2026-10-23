use super::*;
use std::sync::mpsc;

struct Fixture {
    rig: Rig,
    apps: super::super::super::super::InProcessApps,
    lifecycle: LocalSessionAuthority,
    source: Arc<NativePersonalSource>,
    authority: Arc<super::super::super::records::CurrentAuthority>,
    wakes: mpsc::Receiver<()>,
}
impl Fixture {
    fn new() -> Self {
        let mut rig = Rig::new();
        let lifecycle = LocalSessionAuthority::new();
        lifecycle.lifecycle().publish_native_state(false, false);
        let socket = rig.root.path().join("current-records.sock");
        let source = source(&rig, &lifecycle, &socket);
        let mut apps = super::super::super::super::InProcessApps::native_for_profile(
            &rig.root.path().join("apps"),
            &rig.root.path().join("apps").join("person"),
            &rig.owner.principal,
            socket,
        )
        .unwrap();
        rig.owner.device = apps.device_id.clone();
        let startup = native_startup(&rig, &mut apps);
        let (send, wakes) = mpsc::channel();
        apps.bind_messaging_record_wake(Arc::new(move || {
            let _ = send.send(());
        }));
        let authority = apps
            .native_messaging_authority(&startup.identity, &startup.credential, &route().scope)
            .unwrap();
        Self {
            rig,
            apps,
            lifecycle,
            source,
            authority,
            wakes,
        }
    }
    fn metadata(&self, enabled: bool, generation: u64) {
        let mut metadata = accounts::person::PersonAccounts::default();
        metadata.save_account(
            "fixture-signal".into(), "signal".into(), "Synthetic Signal".into(),
            serde_json::json!({"messaging_scope":route().scope,"linking_generation":generation}),
            enabled, 0,
        ).unwrap();
        seed_person(&self.apps, &metadata);
    }
}

#[test]
fn native_current_records_refuse_changed_account_generation_and_contacts_without_file_reopen() {
    let fixture = Fixture::new();
    assert_eq!(
        SignalAuthority::authorize(fixture.authority.as_ref(), &route())
            .unwrap()
            .route,
        route()
    );
    assert_eq!(
        SignalAuthority::incoming(fixture.authority.as_ref(), ACI)
            .unwrap()
            .route,
        route()
    );
    assert!(SignalAuthority::incoming(fixture.authority.as_ref(), OWN_ACI).is_err());
    assert!(SignalAuthority::check_account(fixture.authority.as_ref()).is_ok());
    assert!(
        SignalAuthority::check_account_scope(fixture.authority.as_ref(), &route().scope).is_ok()
    );
    assert!(matches!(
        SignalAuthority::check_account_scope(fixture.authority.as_ref(), "signal:other-primary"),
        Err(SendError::Revoked)
    ));
    let mut wrong = route();
    wrong.sender = tilo_resource::Id::from_raw("other-account");
    assert!(matches!(
        SignalAuthority::authorize(fixture.authority.as_ref(), &wrong),
        Err(SendError::Revoked)
    ));
    fixture.metadata(false, 1);
    assert!(matches!(
        SignalAuthority::check_account(fixture.authority.as_ref()),
        Err(SendError::Revoked)
    ));
    assert!(matches!(
        SignalAuthority::authorize(fixture.authority.as_ref(), &route()),
        Err(SendError::Revoked)
    ));
    fixture.metadata(true, 2);
    assert!(matches!(
        SignalAuthority::check_account(fixture.authority.as_ref()),
        Err(SendError::Revoked)
    ));
    assert!(matches!(
        SignalAuthority::authorize(fixture.authority.as_ref(), &route()),
        Err(SendError::Revoked)
    ));
    fixture.metadata(true, 1);
    let mut contacts = directory();
    contacts
        .remove_contact(tilo_resource::Id::from_raw(PERSON))
        .unwrap();
    seed_person(&fixture.apps, &contacts);
    assert!(
        SignalAuthority::check_account(fixture.authority.as_ref()).is_ok(),
        "transport readiness does not invent a recipient association"
    );
    assert!(matches!(
        SignalAuthority::authorize(fixture.authority.as_ref(), &route()),
        Err(SendError::Revoked)
    ));
    assert!(matches!(
        SignalAuthority::incoming(fixture.authority.as_ref(), ACI),
        Err(SendError::Revoked)
    ));
    assert!(fixture.rig.admission_live());
    drop(fixture);
}

#[test]
fn native_worker_current_record_reads_wake_and_observe_original_owner_edits_before_reply() {
    let mut fixture = Fixture::new();
    let authority = fixture.authority.clone();
    let worker =
        std::thread::spawn(move || SignalAuthority::authorize(authority.as_ref(), &route()));
    fixture
        .wakes
        .recv_timeout(tilo_test_env::hang_bound())
        .unwrap();
    fixture.metadata(false, 1);
    fixture.apps.pump_messaging_records();
    assert!(
        matches!(worker.join().unwrap(), Err(SendError::Revoked)),
        "queued reads inspect the current original document when serviced"
    );
    fixture.metadata(true, 1);
    let authority = fixture.authority.clone();
    let worker = std::thread::spawn(move || SignalAuthority::incoming(authority.as_ref(), ACI));
    fixture
        .wakes
        .recv_timeout(tilo_test_env::hang_bound())
        .unwrap();
    fixture.apps.pump_messaging_records();
    assert_eq!(worker.join().unwrap().unwrap().route, route());
    assert!(fixture.rig.admission_live());
    drop(fixture);
}

#[test]
fn queued_original_record_reads_refuse_revoked_session_and_credential_before_reply() {
    let mut fixture = Fixture::new();
    let authority = fixture.authority.clone();
    let worker =
        std::thread::spawn(move || SignalAuthority::authorize(authority.as_ref(), &route()));
    fixture
        .wakes
        .recv_timeout(tilo_test_env::hang_bound())
        .unwrap();
    fixture
        .rig
        .broker
        .lock()
        .unwrap()
        .revoke_app("so.tilo.messages")
        .unwrap();
    fixture.apps.pump_messaging_records();
    assert!(matches!(worker.join().unwrap(), Err(SendError::Revoked)));
    assert!(matches!(
        SignalAuthority::incoming(fixture.authority.as_ref(), ACI),
        Err(SendError::Revoked)
    ));
    fixture.rig.grant(AccessDecision::AllowForSession);
    let authority = fixture.authority.clone();
    let worker =
        std::thread::spawn(move || SignalAuthority::authorize(authority.as_ref(), &route()));
    fixture
        .wakes
        .recv_timeout(tilo_test_env::hang_bound())
        .unwrap();
    fixture.lifecycle.lifecycle().set_locked(true);
    fixture.apps.pump_messaging_records();
    assert!(matches!(worker.join().unwrap(), Err(SendError::Revoked)));
    fixture.lifecycle.lifecycle().set_locked(false);
    assert!(matches!(
        SignalAuthority::authorize(fixture.authority.as_ref(), &route()),
        Err(SendError::Revoked)
    ));
    assert!(fixture.rig.admission_live());
    drop(fixture);
}

#[test]
fn unobserved_current_record_request_times_out_and_closed_original_owner_never_answers() {
    let Fixture {
        rig,
        mut apps,
        lifecycle,
        source,
        authority,
        wakes,
    } = Fixture::new();
    let worker_authority = authority.clone();
    let worker =
        std::thread::spawn(move || SignalAuthority::authorize(worker_authority.as_ref(), &route()));
    tilo_test_env::recv("the worker's request to wake the owner", &wakes);
    assert!(
        matches!(worker.join().unwrap(), Err(SendError::Unavailable)),
        "a check that cannot observe its subject must refuse"
    );
    apps.pump_messaging_records();
    drop(apps);
    assert!(matches!(
        SignalAuthority::check_account(authority.as_ref()),
        Err(SendError::Unavailable)
    ));
    assert!(matches!(
        SignalAuthority::authorize(authority.as_ref(), &route()),
        Err(SendError::Unavailable)
    ));
    assert!(rig.admission_live());
    drop(authority);
    drop(wakes);
    drop(source);
    drop(lifecycle);
    drop(rig);
}

#[test]
fn buffered_verified_activation_refuses_a_closed_original_native_owner() {
    let Fixture {
        rig,
        mut apps,
        lifecycle,
        source,
        authority,
        wakes,
    } = Fixture::new();
    let account = {
        let SignalAccountStartup {
            identity,
            credential,
            ..
        } = native_startup(&rig, &mut apps);
        apps.prepare_signal_account(SignalAccountStartup {
            identity,
            credential,
            authority: authority.clone(),
        })
        .unwrap()
    };
    let mut mounted = mount_messages(&mut apps, AccountState::Unavailable);
    let probes = Arc::new(AtomicUsize::new(0));
    let (sender, peer) = UnixStream::pair().unwrap();
    let send_daemon = daemon(peer, probes.clone());
    let (receiver, peer) = UnixStream::pair().unwrap();
    let receive_daemon = daemon(peer, probes.clone());
    let owner = rig.owner.clone();
    let root = rig.root.path().join("closed-native-owned");
    let (done, completed) = mpsc::channel();
    let mut pending = account
        .activate_on_worker(
            Arc::new(move || {
                let _ = done.send(());
            }),
            move |current| assemble_fixture(&owner, &root, (sender, receiver), current),
        )
        .unwrap();
    assert!(
        mounted
            .harness
            .wait_until(tilo_test_env::hang_bound(), |_| {
                apps.pump_messaging_records();
                completed.try_recv().is_ok()
            }),
        "actual worker verification must finish through the original current-record queue"
    );
    assert_eq!(probes.load(Ordering::SeqCst), 2);
    assert_eq!(
        mounted
            .client
            .account_state(&ServiceAccount::new("signal", "fixture-signal")),
        AccountState::Unavailable
    );
    drop(apps);
    assert_eq!(pending.poll(), Some(Err(SendError::Unavailable)));
    assert_eq!(
        mounted
            .client
            .account_state(&ServiceAccount::new("signal", "fixture-signal")),
        AccountState::Unavailable
    );
    assert!(matches!(
        account.activate_on_worker(Arc::new(|| {}), |_| panic!(
            "closed original host must not verify another provider"
        )),
        Err(SendError::Unavailable)
    ));
    drop(pending);
    drop(mounted);
    drop(account);
    drop(authority);
    drop(wakes);
    drop(source);
    drop(lifecycle);
    send_daemon.join().unwrap();
    receive_daemon.join().unwrap();
    assert!(rig.admission_live());
    drop(rig);
}
