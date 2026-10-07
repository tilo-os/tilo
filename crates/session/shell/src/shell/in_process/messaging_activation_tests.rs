use super::*;
use std::sync::mpsc;

fn wake_channel() -> (crate::session_runtime::Wake, mpsc::Receiver<()>) {
    let (send, receive) = mpsc::channel();
    (
        Arc::new(move || {
            let _ = send.send(());
        }),
        receive,
    )
}
fn completed(wakes: &mpsc::Receiver<()>) {
    wakes
        .recv_timeout(ui::time::Duration::from_secs(5))
        .unwrap();
}

#[test]
fn owned_verification_runs_off_ui_and_requires_acceptance_before_ready() {
    let rig = Rig::new();
    rig.grant(AccessDecision::AllowForSession);
    let mut apps = rig.apps();
    let account = apps.prepare_signal_account(rig.startup()).unwrap();
    let mounted = mount_messages(&mut apps, AccountState::Unavailable);
    let owner_thread = std::thread::current().id();
    let (sender, peer) = UnixStream::pair().unwrap();
    let probes = Arc::new(AtomicUsize::new(0));
    let send_daemon = daemon(peer, probes.clone());
    let (receiver, peer) = UnixStream::pair().unwrap();
    let receive_daemon = daemon(peer, probes.clone());
    let owner = rig.owner.clone();
    let root = rig.root.path().join("worker-owned");
    let (wake, wakes) = wake_channel();
    let mut pending = account
        .activate_on_worker(wake, move |authority| {
            assert_ne!(std::thread::current().id(), owner_thread);
            assemble_fixture(&owner, &root, (sender, receiver), authority)
        })
        .unwrap();
    completed(&wakes);
    let selected = ServiceAccount::new("signal", "fixture-signal");
    assert_eq!(probes.load(Ordering::SeqCst), 2);
    assert_eq!(
        mounted.client.account_state(&selected),
        AccountState::Unavailable,
        "worker completion cannot publish into the native UI owner's account"
    );
    assert_eq!(pending.poll(), Some(Ok(())));
    assert_eq!(pending.poll(), None, "completion is consumed exactly once");
    drop(pending);
    assert_eq!(
        mounted.client.account_state(&selected),
        AccountState::Ready,
        "acceptance activates the same loan that was already mounted"
    );
    assert!(matches!(
        account.activate_on_worker(Arc::new(|| {}), |_| panic!("no second activation")),
        Err(SendError::AlreadyReserved)
    ));
    drop(mounted);
    drop(account);
    drop(apps);
    send_daemon.join().unwrap();
    receive_daemon.join().unwrap();
    assert!(rig.admission_live());
    drop(rig);
}

#[test]
fn cancelled_activation_stays_reserved_until_its_original_worker_finishes() {
    let rig = Rig::new();
    rig.grant(AccessDecision::AllowForSession);
    let mut apps = rig.apps();
    let account = apps.prepare_signal_account(rig.startup()).unwrap();
    let (entered, started) = mpsc::channel();
    let (release, proceed) = mpsc::channel();
    let (wake, wakes) = wake_channel();
    let pending = account
        .activate_on_worker(wake, move |_| {
            entered.send(()).unwrap();
            proceed
                .recv_timeout(ui::time::Duration::from_secs(5))
                .unwrap();
            Err(SendError::Unavailable)
        })
        .unwrap();
    completed(&started);
    drop(pending);
    assert!(matches!(
        account.activate_on_worker(Arc::new(|| {}), |_| panic!("still reserved")),
        Err(SendError::AlreadyReserved)
    ));
    assert_eq!(
        account
            .client()
            .account_state(&ServiceAccount::new("signal", "fixture-signal")),
        AccountState::Unavailable
    );
    release.send(()).unwrap();
    completed(&wakes);
    let (wake, wakes) = wake_channel();
    let mut retry = account
        .activate_on_worker(wake, |_| Err(SendError::Unavailable))
        .unwrap();
    completed(&wakes);
    assert_eq!(retry.poll(), Some(Err(SendError::Unavailable)));
    drop(retry);
    drop(account);
    drop(apps);
    assert!(rig.admission_live());
    drop(rig);
}

#[test]
fn verified_result_refuses_revoked_original_grant_before_publication() {
    let rig = Rig::new();
    rig.grant(AccessDecision::AllowForSession);
    let mut apps = rig.apps();
    let account = apps.prepare_signal_account(rig.startup()).unwrap();
    let (sender, peer) = UnixStream::pair().unwrap();
    let probes = Arc::new(AtomicUsize::new(0));
    let send_daemon = daemon(peer, probes.clone());
    let (receiver, peer) = UnixStream::pair().unwrap();
    let receive_daemon = daemon(peer, probes.clone());
    let owner = rig.owner.clone();
    let root = rig.root.path().join("revoked-owned");
    let (wake, wakes) = wake_channel();
    let mut pending = account
        .activate_on_worker(wake, move |authority| {
            assemble_fixture(&owner, &root, (sender, receiver), authority)
        })
        .unwrap();
    completed(&wakes);
    assert_eq!(probes.load(Ordering::SeqCst), 2);
    rig.broker
        .lock()
        .unwrap()
        .revoke_app("so.tilo.messages")
        .unwrap();
    assert_eq!(pending.poll(), Some(Err(SendError::Revoked)));
    assert_eq!(
        account
            .client()
            .account_state(&ServiceAccount::new("signal", "fixture-signal")),
        AccountState::Locked
    );
    drop(pending);
    drop(account);
    drop(apps);
    send_daemon.join().unwrap();
    receive_daemon.join().unwrap();
    assert!(rig.admission_live());
    drop(rig);
}

#[test]
fn isolated_verification_failure_releases_its_reservation_without_replay() {
    let rig = Rig::new();
    rig.grant(AccessDecision::AllowForSession);
    let mut apps = rig.apps();
    let account = apps.prepare_signal_account(rig.startup()).unwrap();
    let (wake, wakes) = wake_channel();
    let mut pending = account
        .activate_on_worker(wake, |_| panic!("synthetic verification failure"))
        .unwrap();
    completed(&wakes);
    assert_eq!(pending.poll(), Some(Err(SendError::Unavailable)));
    assert_eq!(pending.poll(), None);
    assert_eq!(
        account
            .client()
            .account_state(&ServiceAccount::new("signal", "fixture-signal")),
        AccountState::Failed
    );
    drop(pending);
    let (wake, wakes) = wake_channel();
    let mut retry = account
        .activate_on_worker(wake, |_| Err(SendError::Unavailable))
        .unwrap();
    completed(&wakes);
    assert_eq!(retry.poll(), Some(Err(SendError::Unavailable)));
    drop(retry);
    drop(account);
    drop(apps);
    assert!(rig.admission_live());
    drop(rig);
}

#[test]
fn cancellation_interrupts_only_owned_provider_streams_and_releases_the_worker() {
    let rig = Rig::new();
    rig.grant(AccessDecision::AllowForSession);
    let mut apps = rig.apps();
    let account = apps.prepare_signal_account(rig.startup()).unwrap();
    let (sender, peer) = UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(ui::time::Duration::from_secs(5)))
        .unwrap();
    let mut observed = BufReader::new(peer);
    let (receiver, untouched_receiver_peer) = UnixStream::pair().unwrap();
    let interrupts = [sender.try_clone().unwrap(), receiver.try_clone().unwrap()];
    let (mut other, mut other_peer) = UnixStream::pair().unwrap();
    other_peer
        .set_read_timeout(Some(ui::time::Duration::from_secs(1)))
        .unwrap();
    let owner = rig.owner.clone();
    let root = rig.root.path().join("interrupted-owned");
    let (wake, wakes) = wake_channel();
    let pending = account
        .activate_with_interrupts(wake, interrupts, move |authority| {
            assemble_fixture(&owner, &root, (sender, receiver), authority)
        })
        .unwrap();
    let mut line = String::new();
    assert_ne!(observed.read_line(&mut line).unwrap(), 0);
    let probe: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(probe["method"], "listAccounts");
    // Withhold the provider reply: actual verification is blocked on this
    // owned stream, and cancellation must end it before its 10s RPC deadline.
    drop(pending);
    completed(&wakes);
    line.clear();
    assert_eq!(observed.read_line(&mut line).unwrap(), 0);
    other.write_all(b"live").unwrap();
    let mut alive = [0; 4];
    other_peer.read_exact(&mut alive).unwrap();
    assert_eq!(&alive, b"live", "another stream owner was not interrupted");
    let (wake, wakes) = wake_channel();
    let mut retry = account
        .activate_on_worker(wake, |_| Err(SendError::Unavailable))
        .unwrap();
    completed(&wakes);
    assert_eq!(retry.poll(), Some(Err(SendError::Unavailable)));
    drop(retry);
    drop(untouched_receiver_peer);
    drop(observed);
    drop(other);
    drop(other_peer);
    drop(account);
    drop(apps);
    assert!(rig.admission_live());
    drop(rig);
}

struct MissingAccount;
#[test]
fn native_host_refuses_a_full_start_queue_before_registering_another_account() {
    let rigs: Vec<_> = (0..5)
        .map(|index| {
            let mut rig = Rig::new();
            rig.owner.account = format!("queued-synthetic-{index}");
            rig.grant(AccessDecision::AllowForSession);
            rig
        })
        .collect();
    let mut apps = rigs[0].apps();
    apps.bind_messaging_record_wake(Arc::new(|| {}));
    let (entered, started) = mpsc::channel();
    let mut releases = Vec::new();
    let mut accounts = Vec::new();
    for rig in &rigs[..4] {
        let (release, proceed) = mpsc::channel();
        releases.push(release);
        let entered = entered.clone();
        accounts.push(
            apps.start_signal_account_on_worker(rig.startup(), move |_| {
                entered.send(()).unwrap();
                proceed
                    .recv_timeout(ui::time::Duration::from_secs(5))
                    .unwrap();
                Err(SendError::Unavailable)
            })
            .unwrap(),
        );
    }
    for _ in 0..4 {
        completed(&started);
    }
    assert!(
        apps.start_signal_account_on_worker(rigs[4].startup(), |_| panic!(
            "full queue must not assemble"
        ))
        .is_err()
    );
    assert_eq!(
        apps.messaging_services.len(),
        4,
        "refusal must not leave an orphan supervisor"
    );
    for release in releases {
        release.send(()).unwrap();
    }
    let deadline = ui::time::Instant::now()
        .checked_add(ui::time::Duration::from_secs(5))
        .unwrap();
    loop {
        apps.pump_messaging_records();
        if accounts.iter().all(|account| {
            account.client().account_state(&ServiceAccount::new(
                "signal",
                &account.gate.startup.identity.owner.account,
            )) == AccountState::Failed
        }) {
            break;
        }
        assert!(
            ui::time::Instant::now() < deadline,
            "the original native loop must collect every completed start"
        );
        ui::background::Background::current().park(
            ui::time::Instant::now()
                .checked_add(ui::time::Duration::from_millis(1))
                .unwrap(),
        );
    }
    assert!(apps.messaging_starts.as_ref().unwrap().available());
    drop(accounts);
    drop(apps);
    assert!(rigs.iter().all(Rig::admission_live));
    drop(rigs);
}

impl SignalAuthority for MissingAccount {
    fn authorize(&self, _: &Route) -> Result<Authorization, SendError> {
        Err(SendError::Unavailable)
    }
    fn incoming(&self, _: &str) -> Result<Authorization, SendError> {
        Err(SendError::Unavailable)
    }
}
#[test]
fn missing_current_account_producer_cannot_schedule_provider_verification() {
    let rig = Rig::new();
    rig.grant(AccessDecision::AllowForSession);
    let mut apps = rig.apps();
    let account = {
        let SignalAccountStartup {
            identity,
            credential,
            ..
        } = rig.startup();
        apps.prepare_signal_account(SignalAccountStartup {
            identity,
            credential,
            authority: Arc::new(MissingAccount),
        })
        .unwrap()
    };
    assert!(matches!(
        account.activate_on_worker(Arc::new(|| {}), |_| panic!("no producer, no provider I/O")),
        Err(SendError::Unavailable)
    ));
    assert_eq!(
        account
            .client()
            .account_state(&ServiceAccount::new("signal", "fixture-signal")),
        AccountState::Unavailable
    );
    drop(account);
    drop(apps);
    assert!(rig.admission_live());
    drop(rig);
}
