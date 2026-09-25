use super::*;
#[cfg(target_os = "linux")]
use crate::connection::model::Conflict;
use crate::connection::{
    endpoint,
    model::{EndpointIdentity, EntryState, RelativePath},
    store::{ConnectionPolicy, ConnectionStore, EndpointBinding, Lifecycle, StoreError},
};
use std::time::{Duration, Instant};

#[cfg(not(target_os = "linux"))]
#[test]
fn unsupported_host_refuses_before_creating_store_or_touching_selection() {
    let temp = tempfile::tempdir().unwrap();
    let store = temp.path().join("absent-store");
    assert!(matches!(
        ConnectedService::open_at(&store),
        Err(ConnectionError::UnsupportedHost)
    ));
    assert!(matches!(
        SelectedFolders::probe(&temp.path().join("source"), &temp.path().join("target")),
        Err(ConnectionError::UnsupportedHost)
    ));
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
}

fn fixture_store() -> (tempfile::TempDir, ConnectionStore) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().canonicalize().unwrap().join("store");
    let store = ConnectionStore::open_at(&path).unwrap();
    (temp, store)
}

fn seed(store: &mut ConnectionStore, number: u8, root: &std::path::Path) -> ConnectionId {
    let root = root.canonicalize().unwrap();
    let id = ConnectionId([number; 16]);
    store
        .create_connection(
            id,
            EndpointBinding {
                identity: EndpointIdentity {
                    volume: [1; 32],
                    root: [number; 32],
                },
                locator: endpoint::EndpointLocator::new(
                    &root.join(format!("unavailable-source-{number}")),
                )
                .unwrap(),
            },
            EndpointBinding {
                identity: EndpointIdentity {
                    volume: [2; 32],
                    root: [number; 32],
                },
                locator: endpoint::EndpointLocator::new(
                    &root.join(format!("unavailable-destination-{number}")),
                )
                .unwrap(),
            },
            ConnectionPolicy {
                deletion_retention_seconds: 2_592_000,
            },
        )
        .unwrap();
    id
}

#[test]
fn real_worker_pages_reopens_pauses_and_durably_disconnects() {
    let (temp, mut store) = fixture_store();
    let first = seed(&mut store, 1, temp.path());
    seed(&mut store, 2, temp.path());
    let service = spawn(store).unwrap();
    let page = service.list(None, 1).unwrap().wait().unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].id, first);
    let next = service.list(page.next, 1).unwrap().wait().unwrap();
    assert_eq!(next.entries[0].id, ConnectionId([2; 16]));
    assert!(next.next.is_none());
    let observer = service.reopen(first).unwrap().wait().unwrap();
    assert_eq!(
        observer.snapshot().phase,
        Phase::Pending(Pending::NotCompared)
    );
    service.pause(first).unwrap().wait().unwrap();
    assert_eq!(observer.snapshot().phase, Phase::Paused);
    service.disconnect(first).unwrap().wait().unwrap();
    assert_eq!(observer.snapshot().phase, Phase::Disconnected);
    assert_eq!(
        service.start(first).unwrap().wait().err(),
        Some(ConnectionError::Disconnected)
    );
    drop(service);
    let deadline = Instant::now() + Duration::from_secs(5);
    let path = temp.path().canonicalize().unwrap().join("store");
    let store = loop {
        match ConnectionStore::open_at(&path) {
            Ok(store) => break store,
            Err(StoreError::Busy) if Instant::now() < deadline => std::thread::yield_now(),
            other => panic!("store did not release: {:?}", other.err()),
        }
    };
    assert_eq!(
        store.load_connection(first).unwrap().lifecycle,
        Lifecycle::Disconnected
    );
}

#[test]
fn real_worker_rejects_oversized_pages_and_keeps_exclusive_store() {
    let (temp, store) = fixture_store();
    let service = spawn(store).unwrap();
    assert!(matches!(
        service.list(None, 65),
        Err(ConnectionError::LimitExceeded)
    ));
    assert!(matches!(
        ConnectionStore::open_at(&temp.path().canonicalize().unwrap().join("store")),
        Err(StoreError::Busy)
    ));
}

#[test]
fn full_command_queue_refuses_without_blocking() {
    let (sender, _receiver) = std::sync::mpsc::sync_channel(COMMAND_CAPACITY);
    let service = ConnectedService::from_sender(sender);
    for _ in 0..COMMAND_CAPACITY {
        let _ = service.list(None, 1).unwrap();
    }
    assert!(matches!(service.list(None, 1), Err(ConnectionError::Busy)));
}

#[test]
fn initial_preview_pages_are_bounded_and_preserve_native_names() {
    use crate::connection::{
        model::{Encoding, Endpoint, NamePolicy, Observation, ObservedTree},
        transaction::initial_preview_for_test,
    };
    let (temp, mut store) = fixture_store();
    let id = seed(&mut store, 3, temp.path());
    let record = store.load_connection(id).unwrap();
    let entries = (0..130)
        .map(|number| {
            (
                RelativePath::unix(format!("item-{number:03}").as_bytes()).unwrap(),
                EntryState::File {
                    sha256: [7; 32],
                    bytes: 3,
                    executable: false,
                },
            )
        })
        .collect::<Vec<_>>();
    let tree = |identity, entries| {
        ObservedTree::new(
            Endpoint {
                identity,
                encoding: Encoding::Unix,
                names: NamePolicy::Exact,
            },
            entries,
            Observation::Complete,
        )
        .unwrap()
    };
    let preview = initial_preview_for_test(
        &store,
        id,
        tree(record.source.identity, entries),
        tree(record.destination.identity, Vec::new()),
    )
    .unwrap();
    let page = preview.worker_page(0).unwrap();
    assert_eq!(page.entries.len(), 64);
    assert_eq!(
        page.entries[0].path,
        RelativePath::unix(b"item-000").unwrap()
    );
    assert_eq!(page.next_offset, Some(64));
    let page = preview.worker_page(128).unwrap();
    assert_eq!(page.entries.len(), 2);
    assert!(page.next_offset.is_none());
    assert!(matches!(
        preview.worker_page(131),
        Err(StoreError::LimitExceeded)
    ));
    assert_eq!(store.load_connection(id).unwrap().baseline_revision, None);
}

#[test]
fn stale_review_identifier_is_rejected_by_dispatch_without_writing() {
    let (temp, mut store) = fixture_store();
    let id = seed(&mut store, 1, temp.path());
    let service = spawn(store).unwrap();
    let observer = service.reopen(id).unwrap().wait().unwrap();
    let revision = observer.snapshot().connection.revision;
    let bogus = ReviewId {
        generation: 1,
        lifetime: Arc::new(()),
    };
    assert_eq!(
        service.accept_initial(bogus).unwrap().wait(),
        Err(ConnectionError::StaleReview)
    );
    assert_eq!(observer.snapshot().connection.revision, revision);
}

#[cfg(target_os = "linux")]
struct NativeFixture {
    _temp: tempfile::TempDir,
    source: std::path::PathBuf,
    destination: std::path::PathBuf,
    store: std::path::PathBuf,
}
#[cfg(target_os = "linux")]
impl NativeFixture {
    fn new() -> Self {
        let root = std::env::var_os("QOS_ENDPOINT_FIXTURE").expect("native worker tests require QOS_ENDPOINT_FIXTURE: an authorized mounted private fixture root");
        let helper = std::env::var_os("QOS_LEASE_HELPER")
            .expect("native worker tests require QOS_LEASE_HELPER: built early-host example");
        assert!(
            std::path::Path::new(&helper).is_file(),
            "native lease helper must exist"
        );
        let temp = tempfile::tempdir_in(root).unwrap();
        let base = temp.path().canonicalize().unwrap();
        let source = base.join("source");
        let destination = base.join("destination");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&destination).unwrap();
        Self {
            _temp: temp,
            source,
            destination,
            store: base.join("store"),
        }
    }
    fn service(&self) -> ConnectedService {
        ConnectedService::open_at(&self.store).unwrap()
    }
    fn create(&self, service: &ConnectedService) -> ConnectionInfo {
        service
            .create(SelectedFolders::probe(&self.source, &self.destination).unwrap())
            .unwrap()
            .wait()
            .unwrap()
    }
}
fn terminal(observer: &Observer) -> Snapshot {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let snapshot = observer.snapshot();
        if !matches!(snapshot.phase, Phase::Scanning | Phase::Syncing) {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "native worker did not settle: {snapshot:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[cfg(target_os = "linux")]
fn accept(service: &ConnectedService, observer: &Observer) {
    let Phase::AwaitingReview { review, .. } = terminal(observer).phase else {
        panic!(
            "expected real initial comparison: {:?}",
            observer.snapshot()
        );
    };
    service.accept_initial(review).unwrap().wait().unwrap();
}
#[cfg(target_os = "linux")]
fn reopen_service(fixture: &NativeFixture) -> ConnectedService {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match ConnectedService::open_at(&fixture.store) {
            Ok(service) => return service,
            Err(ConnectionError::Busy) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(error) => panic!("service failed to reopen: {error:?}"),
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn native_facade_propagates_both_ways_preserves_conflict_and_reopens() {
    let fixture = NativeFixture::new();
    std::fs::write(fixture.source.join("document"), b"original").unwrap();
    let service = fixture.service();
    assert!(matches!(
        ConnectedService::open_at(&fixture.store),
        Err(ConnectionError::Busy)
    ));
    let info = fixture.create(&service);
    assert_eq!(info.deletion_retention_seconds, 2_592_000);
    let observer = service.start(info.id).unwrap().wait().unwrap();
    accept(&service, &observer);
    let first_observer = observer.clone();
    assert_native_up_to_date(&observer);
    assert_eq!(
        std::fs::read(fixture.destination.join("document")).unwrap(),
        b"original"
    );
    std::fs::write(fixture.destination.join("document"), b"destination edit").unwrap();
    let observer = service.start(info.id).unwrap().wait().unwrap();
    assert_native_up_to_date(&observer);
    assert_eq!(
        std::fs::read(fixture.source.join("document")).unwrap(),
        b"destination edit"
    );
    std::fs::write(fixture.source.join("document"), b"source edit").unwrap();
    let observer = service.start(info.id).unwrap().wait().unwrap();
    assert_native_up_to_date(&observer);
    assert_eq!(
        std::fs::read(fixture.destination.join("document")).unwrap(),
        b"source edit"
    );
    std::fs::write(fixture.source.join("document"), b"left diverged").unwrap();
    std::fs::write(fixture.destination.join("document"), b"right diverged").unwrap();
    let observer = service.start(info.id).unwrap().wait().unwrap();
    let snapshot = terminal(&observer);
    assert_eq!(snapshot.phase, Phase::Conflict);
    assert_eq!(first_observer.snapshot().phase, Phase::Conflict);
    assert_eq!(
        snapshot.latest_conflict.unwrap().kind,
        Conflict::DivergentEdits
    );
    assert_eq!(
        std::fs::read(fixture.source.join("document")).unwrap(),
        b"left diverged"
    );
    assert_eq!(
        std::fs::read(fixture.destination.join("document")).unwrap(),
        b"right diverged"
    );
    let conflicts = service.conflicts(info.id, None).unwrap().wait().unwrap();
    assert_eq!(conflicts.entries.len(), 1);
    assert!(conflicts.entries[0].preserved_versions >= 2);
    drop(observer);
    drop(service);
    let service = reopen_service(&fixture);
    assert_eq!(
        service.list(None, 64).unwrap().wait().unwrap().entries[0].id,
        info.id
    );
    assert_eq!(
        service
            .conflicts(info.id, None)
            .unwrap()
            .wait()
            .unwrap()
            .entries
            .len(),
        1
    );
    service.disconnect(info.id).unwrap().wait().unwrap();
    drop(service);
    let service = reopen_service(&fixture);
    assert_eq!(
        service
            .reopen(info.id)
            .unwrap()
            .wait()
            .unwrap()
            .snapshot()
            .phase,
        Phase::Disconnected
    );
    assert_eq!(
        std::fs::read(fixture.source.join("document")).unwrap(),
        b"left diverged"
    );
    assert_eq!(
        std::fs::read(fixture.destination.join("document")).unwrap(),
        b"right diverged"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn native_facade_rejects_stale_preview_including_empty_comparison() {
    let fixture = NativeFixture::new();
    let service = fixture.service();
    let info = fixture.create(&service);
    let observer = service.start(info.id).unwrap().wait().unwrap();
    let Phase::AwaitingReview {
        review: empty_review,
        entries: 0,
    } = terminal(&observer).phase
    else {
        panic!("empty folders still require explicit review");
    };
    assert!(!observer.snapshot().connection.has_baseline);
    std::fs::write(fixture.source.join("document"), b"preview").unwrap();
    assert_eq!(
        service.accept_initial(empty_review).unwrap().wait(),
        Err(ConnectionError::StaleReview)
    );
    let observer = service.start(info.id).unwrap().wait().unwrap();
    let Phase::AwaitingReview { review, entries } = terminal(&observer).phase else {
        panic!("missing preview");
    };
    assert_eq!(entries, 1);
    assert_eq!(
        service
            .preview(review.clone(), 0)
            .unwrap()
            .wait()
            .unwrap()
            .entries
            .len(),
        1
    );
    std::fs::write(fixture.source.join("document"), b"changed after preview").unwrap();
    assert_eq!(
        service.accept_initial(review.clone()).unwrap().wait(),
        Err(ConnectionError::StaleReview)
    );
    assert_eq!(
        service.accept_initial(review).unwrap().wait(),
        Err(ConnectionError::StaleReview)
    );
    assert!(!fixture.destination.join("document").exists());
    assert!(!observer.snapshot().connection.has_baseline);
}

#[cfg(target_os = "linux")]
#[test]
fn native_facade_dropped_observer_pause_and_offline_do_not_lose_bytes() {
    let fixture = NativeFixture::new();
    std::fs::write(fixture.source.join("document"), b"must survive").unwrap();
    let service = fixture.service();
    let info = fixture.create(&service);
    let observer = service.start(info.id).unwrap().wait().unwrap();
    accept(&service, &observer);
    drop(observer);
    // Reopen is Busy during native work; dropping the last observer cannot
    // interrupt it or turn progress into an unbounded queued event stream.
    let deadline = Instant::now() + Duration::from_secs(90);
    let observer = loop {
        match service.reopen(info.id).unwrap().wait() {
            Ok(observer) => break observer,
            Err(ConnectionError::Busy) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10))
            }
            other => panic!("reopen failed: {:?}", other.err()),
        }
    };
    assert_native_up_to_date(&observer);
    service.pause(info.id).unwrap().wait().unwrap();
    assert_eq!(observer.snapshot().phase, Phase::Paused);
    assert_eq!(
        std::fs::read(fixture.destination.join("document")).unwrap(),
        b"must survive"
    );
    std::fs::rename(
        &fixture.source,
        fixture.source.with_file_name("source-offline"),
    )
    .unwrap();
    let observer = service.start(info.id).unwrap().wait().unwrap();
    assert_eq!(
        terminal(&observer).phase,
        Phase::Unavailable {
            side: Side::Source,
            error: ConnectionError::Unavailable
        }
    );
    assert_eq!(
        std::fs::read(fixture.destination.join("document")).unwrap(),
        b"must survive"
    );
    service.disconnect(info.id).unwrap().wait().unwrap();
    assert_eq!(
        std::fs::read(fixture.destination.join("document")).unwrap(),
        b"must survive"
    );
}

#[test]
fn pending_intent_reopens_as_recovery_and_disconnect_never_reactivates_it() {
    use crate::connection::{model::Decision, store::OperationIntent};
    let (temp, mut store) = fixture_store();
    let id = seed(&mut store, 1, temp.path());
    let record = store.load_connection(id).unwrap();
    store
        .begin_intent(OperationIntent {
            connection: id,
            expected_revision: record.revision,
            expected_baseline: None,
            path: RelativePath::unix(b"pending").unwrap(),
            source_before: Some(EntryState::File {
                sha256: [1; 32],
                bytes: 5,
                executable: false,
            }),
            destination_before: None,
            decision: Decision::Propagate { from: Side::Source },
        })
        .unwrap();
    let service = spawn(store).unwrap();
    let observer = service.reopen(id).unwrap().wait().unwrap();
    assert_eq!(
        observer.snapshot().phase,
        Phase::RecoveryRequired(ConnectionError::RecoveryRequired)
    );
    service.pause(id).unwrap().wait().unwrap();
    assert_eq!(
        observer.snapshot().phase,
        Phase::RecoveryRequired(ConnectionError::RecoveryRequired)
    );
    assert_eq!(
        service.disconnect(id).unwrap().wait(),
        Err(ConnectionError::RecoveryRequired)
    );
    assert_eq!(
        observer.snapshot().connection.lifecycle,
        ConnectionLifecycle::Disconnecting
    );
    assert_eq!(
        observer.snapshot().phase,
        Phase::RecoveryRequired(ConnectionError::RecoveryRequired)
    );
    assert_eq!(
        service.start(id).unwrap().wait().err(),
        Some(ConnectionError::Disconnected)
    );
}

#[cfg(target_os = "linux")]
#[test]
fn native_incomplete_observation_never_becomes_deletion_or_up_to_date() {
    let fixture = NativeFixture::new();
    std::fs::write(fixture.destination.join("keep"), b"keep local").unwrap();
    let service = fixture.service();
    let info = fixture.create(&service);
    std::os::unix::fs::symlink(
        "unavailable-target",
        fixture.source.join("unsupported-link"),
    )
    .unwrap();
    let observer = service.start(info.id).unwrap().wait().unwrap();
    assert_eq!(
        terminal(&observer).phase,
        Phase::Pending(Pending::IncompleteObservation)
    );
    assert_eq!(
        std::fs::read(fixture.destination.join("keep")).unwrap(),
        b"keep local"
    );
    assert!(!observer.snapshot().connection.has_baseline);
}

#[cfg(target_os = "linux")]
struct StepHold {
    id: ConnectionId,
    entered: SyncSender<()>,
    release: Receiver<()>,
}
#[cfg(target_os = "linux")]
static STEP_HOLD: Mutex<Option<StepHold>> = Mutex::new(None);

#[cfg(target_os = "linux")]
pub(super) fn before_native_step(id: ConnectionId) {
    let hold = {
        let mut slot = STEP_HOLD.lock().unwrap();
        if slot.as_ref().is_some_and(|hold| hold.id == id) {
            slot.take()
        } else {
            None
        }
    };
    if let Some(hold) = hold {
        hold.entered.send(()).unwrap();
        hold.release
            .recv_timeout(Duration::from_secs(30))
            .expect("test must release begun worker step");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn native_pause_and_disconnect_wait_for_begun_step_after_ui_handles_drop() {
    let fixture = NativeFixture::new();
    std::fs::write(fixture.source.join("document"), b"baseline").unwrap();
    let service = fixture.service();
    let info = fixture.create(&service);
    let observer = service.start(info.id).unwrap().wait().unwrap();
    accept(&service, &observer);
    assert_native_up_to_date(&observer);
    std::fs::write(fixture.source.join("document"), b"begun edit must finish").unwrap();
    let (entered, ready) = mpsc::sync_channel(1);
    let (release, held) = mpsc::sync_channel(1);
    *STEP_HOLD.lock().unwrap() = Some(StepHold {
        id: info.id,
        entered,
        release: held,
    });
    let observer = service.start(info.id).unwrap().wait().unwrap();
    ready
        .recv_timeout(Duration::from_secs(30))
        .expect("worker must enter native dispatch");
    let pause = service.pause(info.id).unwrap();
    let disconnect = service.disconnect(info.id).unwrap();
    assert!(pause.try_take().unwrap().is_none());
    assert!(disconnect.try_take().unwrap().is_none());
    drop(observer);
    drop(service);
    release.send(()).unwrap();
    pause.wait().unwrap();
    assert_eq!(
        disconnect.wait().unwrap().lifecycle,
        ConnectionLifecycle::Disconnected
    );
    let service = reopen_service(&fixture);
    assert_eq!(
        service
            .reopen(info.id)
            .unwrap()
            .wait()
            .unwrap()
            .snapshot()
            .phase,
        Phase::Disconnected
    );
    assert_eq!(
        std::fs::read(fixture.source.join("document")).unwrap(),
        b"begun edit must finish"
    );
    assert_eq!(
        std::fs::read(fixture.destination.join("document")).unwrap(),
        b"begun edit must finish"
    );
}

#[test]
fn same_connection_restart_keeps_existing_observers_current() {
    let (temp, mut store) = fixture_store();
    let id = seed(&mut store, 1, temp.path());
    let service = spawn(store).unwrap();
    let first = service.reopen(id).unwrap().wait().unwrap();
    let restarted = service.start(id).unwrap().wait().unwrap();
    assert!(matches!(
        restarted.snapshot().phase,
        Phase::Unavailable { .. }
    ));
    assert_eq!(first.snapshot().phase, restarted.snapshot().phase);
    service.pause(id).unwrap().wait().unwrap();
    assert_eq!(first.snapshot().phase, Phase::Paused);
    service.disconnect(id).unwrap().wait().unwrap();
    assert_eq!(first.snapshot().phase, Phase::Disconnected);
}

#[test]
fn switching_connections_detaches_old_observers_before_discarding_state() {
    let (temp, mut store) = fixture_store();
    let first_id = seed(&mut store, 1, temp.path());
    let second_id = seed(&mut store, 2, temp.path());
    let service = spawn(store).unwrap();
    let first = service.reopen(first_id).unwrap().wait().unwrap();
    let second = service.reopen(second_id).unwrap().wait().unwrap();
    assert_eq!(first.snapshot().phase, Phase::Detached);
    service.disconnect(first_id).unwrap().wait().unwrap();
    assert_eq!(first.snapshot().phase, Phase::Detached);
    assert_eq!(second.snapshot().connection.id, second_id);
    assert_eq!(
        second.snapshot().phase,
        Phase::Pending(Pending::NotCompared)
    );
}

#[test]
fn unavailable_start_keeps_pending_recovery_and_endpoint_cause() {
    use crate::connected::EndpointIssue;
    use crate::connection::{model::Decision, store::OperationIntent};
    let (temp, mut store) = fixture_store();
    let id = seed(&mut store, 1, temp.path());
    let record = store.load_connection(id).unwrap();
    store
        .begin_intent(OperationIntent {
            connection: id,
            expected_revision: record.revision,
            expected_baseline: None,
            path: RelativePath::unix(b"pending").unwrap(),
            source_before: Some(EntryState::File {
                sha256: [1; 32],
                bytes: 5,
                executable: false,
            }),
            destination_before: None,
            decision: Decision::Propagate { from: Side::Source },
        })
        .unwrap();
    let service = spawn(store).unwrap();
    let observer = service.start(id).unwrap().wait().unwrap();
    let snapshot = observer.snapshot();
    assert_eq!(
        snapshot.phase,
        Phase::RecoveryRequired(ConnectionError::RecoveryRequired)
    );
    assert!(snapshot.pending_operation);
    assert_eq!(
        snapshot.endpoint_issue,
        Some(EndpointIssue {
            side: Side::Source,
            error: ConnectionError::Unavailable
        })
    );
    let reopened = service.reopen(id).unwrap().wait().unwrap();
    assert_eq!(reopened.snapshot(), snapshot);
}

struct CommandPressure {
    id: ConnectionId,
    sender: SyncSender<Command>,
    observed_step: SyncSender<usize>,
    handled: usize,
}
static COMMAND_PRESSURE: Mutex<Option<CommandPressure>> = Mutex::new(None);

pub(super) fn after_dispatch_command(id: Option<ConnectionId>) {
    let mut slot = COMMAND_PRESSURE.lock().unwrap();
    if let Some(pressure) = slot.as_mut().filter(|pressure| Some(pressure.id) == id) {
        pressure.handled += 1;
        if pressure.handled < 128 {
            let (reply, _discarded) = mpsc::sync_channel(1);
            pressure
                .sender
                .try_send(Command::List(None, 1, reply))
                .unwrap();
        }
    }
}

pub(super) fn before_dispatch_step(id: ConnectionId) {
    let pressure = {
        let mut slot = COMMAND_PRESSURE.lock().unwrap();
        if slot.as_ref().is_some_and(|pressure| pressure.id == id) {
            slot.take()
        } else {
            None
        }
    };
    if let Some(pressure) = pressure {
        pressure.observed_step.send(pressure.handled).unwrap();
    }
}

#[test]
fn queued_read_commands_cannot_starve_dispatch() {
    let (temp, mut store) = fixture_store();
    let id = seed(&mut store, 90, temp.path());
    let mut worker = Worker {
        store,
        active: None,
        lifetime: Arc::new(()),
        generation: 0,
    };
    worker.open_connection(id, false).unwrap();
    // The subject is the actual dispatcher, independent of host support. This
    // fixture supplies no native session: dispatch must refuse, never execute
    // a synthetic operation or claim success.
    worker.active.as_mut().unwrap().running = true;
    let (sender, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
    let (observed_step, step) = mpsc::sync_channel(1);
    *COMMAND_PRESSURE.lock().unwrap() = Some(CommandPressure {
        id,
        sender: sender.clone(),
        observed_step,
        handled: 0,
    });
    let service = ConnectedService::from_sender(sender);
    let _request = service.list(None, 1).unwrap();
    let handles = service.command_lifetime();
    let thread = std::thread::spawn(move || worker.run(&receiver, &handles));
    let handled = step
        .recv_timeout(Duration::from_secs(30))
        .expect("production dispatcher must reach its step");
    drop(service);
    thread.join().unwrap();
    assert!(
        handled <= 16,
        "native dispatch starved behind {handled} read commands"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn native_pre_step_refusal_keeps_pending_recovery_and_endpoint_cause() {
    use crate::connection::{model::Decision, store::OperationIntent};
    let fixture = NativeFixture::new();
    let store = ConnectionStore::open_at(&fixture.store).unwrap();
    let mut worker = Worker {
        store,
        active: None,
        lifetime: Arc::new(()),
        generation: 0,
    };
    let selection = SelectedFolders::probe(&fixture.source, &fixture.destination).unwrap();
    let info = worker.create(selection).unwrap();
    let record = worker.store.load_connection(info.id).unwrap();
    worker
        .store
        .begin_intent(OperationIntent {
            connection: info.id,
            expected_revision: record.revision,
            expected_baseline: None,
            path: RelativePath::unix(b"pending").unwrap(),
            source_before: Some(EntryState::File {
                sha256: [1; 32],
                bytes: 5,
                executable: false,
            }),
            destination_before: None,
            decision: Decision::Propagate { from: Side::Source },
        })
        .unwrap();
    let before = worker.store.executor_position(info.id).unwrap();
    let observer = worker.open_connection(info.id, true).unwrap();
    assert_eq!(observer.snapshot().phase, Phase::Scanning);
    std::fs::rename(
        &fixture.source,
        fixture.source.with_file_name("source-offline"),
    )
    .unwrap();
    // Real pinned endpoints now refuse on the production pre-step check. The
    // durable fixture is discovery only and never reaches native admission.
    worker.step();
    let snapshot = observer.snapshot();
    assert_eq!(
        snapshot.phase,
        Phase::RecoveryRequired(ConnectionError::RecoveryRequired),
        "{snapshot:?}"
    );
    assert!(snapshot.pending_operation, "{snapshot:?}");
    assert_eq!(
        snapshot.endpoint_issue,
        Some(EndpointIssue {
            side: Side::Source,
            error: ConnectionError::Unavailable,
        }),
        "{snapshot:?}"
    );
    assert_eq!(worker.store.executor_position(info.id).unwrap(), before);
    assert_eq!(
        worker
            .store
            .load_connection(info.id)
            .unwrap()
            .baseline_revision,
        None
    );
    assert_eq!(
        worker.open_connection(info.id, false).unwrap().snapshot(),
        snapshot
    );
}

// A single bounded diagnostic slot is sufficient for the serial native matrix;
// overlapping tests can replace it, but cannot affect worker behavior. Read-only
// metadata is never fed back into native admission or continuation.
static NATIVE_POSITION: Mutex<Option<(ConnectionId, String)>> = Mutex::new(None);

pub(super) fn record_native_position(store: &ConnectionStore, id: ConnectionId) {
    let diagnostic = match store.executor_position(id) {
        Ok((pending, latest)) => {
            let stage = |operation: Option<super::super::store::OperationId>| {
                operation.map(|operation| {
                    store
                        .executor_receipt(operation)
                        .map(|receipt| (receipt.id.serial, receipt.revision, receipt.stage))
                })
            };
            format!("pending={:?}, latest={:?}", stage(pending), stage(latest))
        }
        Err(error) => format!("position unavailable: {error:?}"),
    };
    eprintln!("native worker durable position {id:?}: {diagnostic}");
    *NATIVE_POSITION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((id, diagnostic));
}

fn assert_native_up_to_date(observer: &Observer) {
    let snapshot = terminal(observer);
    let diagnostic = {
        let slot = NATIVE_POSITION
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        slot.as_ref()
            .filter(|(id, _)| *id == snapshot.connection.id)
            .map_or_else(
                || "no matching retained diagnostic".to_owned(),
                |(_, value)| value.clone(),
            )
    };
    assert_eq!(
        snapshot.phase,
        Phase::UpToDate,
        "native worker snapshot: {snapshot:?}; durable position: {diagnostic}"
    );
}

#[test]
fn last_command_owner_drop_detaches_surviving_observer() {
    let (temp, mut store) = fixture_store();
    let id = seed(&mut store, 4, temp.path());
    let service = spawn(store).unwrap();
    let observer = service.reopen(id).unwrap().wait().unwrap();
    drop(service);
    let deadline = Instant::now() + Duration::from_secs(30);
    while observer.snapshot().phase != Phase::Detached {
        assert!(
            Instant::now() < deadline,
            "publisher did not detach: {:?}",
            observer.snapshot()
        );
        std::thread::yield_now();
    }
}

#[test]
fn native_diagnostic_assertion_failure_is_isolated_and_poison_is_tolerated() {
    let (temp, mut store) = fixture_store();
    let id = seed(&mut store, 91, temp.path());
    let mut worker = Worker {
        store,
        active: None,
        lifetime: Arc::new(()),
        generation: 0,
    };
    // Exercise the real diagnostic assertion against a real pending snapshot.
    // No endpoint or native execution is needed to prove failure isolation.
    let observer = worker.open_connection(id, false).unwrap();
    record_native_position(&worker.store, id);
    let failed_assertion = std::panic::catch_unwind(|| assert_native_up_to_date(&observer));
    assert!(
        failed_assertion.is_err(),
        "pending must still fail UpToDate"
    );
    assert!(
        !NATIVE_POSITION.is_poisoned(),
        "diagnostic assertion poisoned the shared slot"
    );
    record_native_position(&worker.store, id);

    // An unrelated diagnostic panic must not stop later writes or replace the
    // original snapshot assertion with an incidental poisoned-lock panic.
    let poison = std::panic::catch_unwind(|| {
        let _slot = NATIVE_POSITION.lock().unwrap();
        panic!("intentional diagnostic poison");
    });
    assert!(poison.is_err());
    record_native_position(&worker.store, id);
    let failed_assertion = std::panic::catch_unwind(|| assert_native_up_to_date(&observer))
        .expect_err("pending must still fail UpToDate after diagnostic poison");
    NATIVE_POSITION.clear_poison();
    let message = failed_assertion.downcast_ref::<String>().unwrap();
    assert!(message.contains("native worker snapshot:"), "{message}");
}

#[cfg(target_os = "linux")]
#[test]
fn native_empty_baseline_review_restart_and_later_real_operations() {
    let fixture = NativeFixture::new();
    let service = fixture.service();
    let info = fixture.create(&service);
    let observer = service.start(info.id).unwrap().wait().unwrap();
    let Phase::AwaitingReview { review, entries: 0 } = terminal(&observer).phase else {
        panic!(
            "empty comparison requires review: {:?}",
            observer.snapshot()
        );
    };
    assert!(!observer.snapshot().connection.has_baseline);
    assert!(
        service
            .preview(review.clone(), 0)
            .unwrap()
            .wait()
            .unwrap()
            .entries
            .is_empty()
    );
    service.accept_initial(review).unwrap().wait().unwrap();
    assert_eq!(terminal(&observer).phase, Phase::UpToDate);
    assert!(observer.snapshot().connection.has_baseline);
    drop(observer);
    drop(service);
    let service = reopen_service(&fixture);
    let observer = service.start(info.id).unwrap().wait().unwrap();
    assert_eq!(terminal(&observer).phase, Phase::UpToDate);
    for (source, destination, name, text) in [
        (
            &fixture.source,
            &fixture.destination,
            "left-created",
            b"left".as_slice(),
        ),
        (
            &fixture.destination,
            &fixture.source,
            "right-created",
            b"right".as_slice(),
        ),
        (
            &fixture.destination,
            &fixture.source,
            "left-created",
            b"edited from right".as_slice(),
        ),
    ] {
        std::fs::write(source.join(name), text).unwrap();
        let observer = service.start(info.id).unwrap().wait().unwrap();
        assert_eq!(terminal(&observer).phase, Phase::UpToDate);
        assert_eq!(std::fs::read(destination.join(name)).unwrap(), text);
        assert!(observer.snapshot().verified_steps > 0);
    }
    drop(observer);
    drop(service);
    let deadline = Instant::now() + Duration::from_secs(30);
    let store = loop {
        match ConnectionStore::open_at(&fixture.store) {
            Ok(store) => break store,
            Err(StoreError::Busy) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(error) => panic!("reopen: {error:?}"),
        }
    };
    let (pending, latest) = store.executor_position(info.id).unwrap();
    assert!(pending.is_none());
    assert!(
        latest.is_some(),
        "later edits must produce genuine transaction receipts"
    );
    assert_eq!(
        store.executor_receipt(latest.unwrap()).unwrap().stage,
        super::super::store::ReceiptStage::BaselineCommitted
    );
}

#[path = "recovery_tests.rs"]
mod recovery_tests;
