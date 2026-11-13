// An idle collaboration connection costs no wake. Nothing here sends,
// nothing arrives, and no timer asks whether something did: the shovel
// thread sleeps until the broker, the app or the identity service hands it
// something. A thread that wakes with nothing to do spends a still
// desktop's battery, so each case holds a connection with nothing to do and
// counts the wakeups of the thread that serves it.

use tilo_test_env::idle::{asleep_runs, blocked};

/// How long a connection is held with nothing to do. The shortest poll this
/// code has ever used asks ten times a second, which shows as twenty
/// wakeups in here.
const IDLE_HELD: Duration = Duration::from_secs(2);

/// The shovel thread serving `manager`, once it runs.
#[cfg(unix)]
fn shovel_thread(manager: &CollaborationManager) -> String {
    let mut tid = None;
    eventually("the shovel runs", || {
        tid = manager.shovel_thread();
        tid.is_some()
    });
    tid.expect("the shovel runs").to_string()
}

/// How many times thread `tid` woke over [`IDLE_HELD`] with nothing to do.
///
/// Counted as the wakeups of a thread asleep at both ends: a thread blocked
/// on its source is scheduled in only when that source has something for it.
#[cfg(unix)]
fn wakes_while_held(tid: &str) -> u64 {
    let settled = asleep_runs(tid);
    thread::sleep(IDLE_HELD);
    asleep_runs(tid).saturating_sub(settled)
}

/// A connection that joined its route with nothing to send or receive holds
/// still: the shovel wakes for nothing at all.
#[test]
#[cfg(unix)]
fn an_idle_connection_wakes_nothing() {
    let manager = connected_manager();
    let tid = shovel_thread(&manager);
    // The join's own traffic (the route, its streams) settles first.
    eventually("the shovel sleeps", || blocked(&tid));
    assert_eq!(
        wakes_while_held(&tid),
        0,
        "the shovel woke with nothing to send or receive"
    );
    // The connection is still the one held: the count is of a live thread.
    assert!(manager.is_connected(), "the connection ended meanwhile");
}

/// A committed read the app has not merged yet is something to wait for, not
/// something to ask about: the shovel waits on the app's own answer, and
/// wakes for nothing while an app takes its time over a capture. Asking here
/// is the shovel polling for an answer only the app has.
#[test]
#[cfg(unix)]
fn a_read_the_app_takes_its_time_over_wakes_nothing() {
    let workspace = Uuid::new_v4();
    let route = RouteSpec::record(
        "so.tilo.notes",
        workspace,
        "derived:notes@derived:notebook@workspace",
    );
    let broker = FakeBroker {
        captures: Arc::new(std::sync::Mutex::new(Captures {
            committed_history_fixture: Some(automerge::AutoCommit::new().save()),
            committed_awaits: Some(Arc::new(tokio::sync::Notify::new())),
            ..Captures::default()
        })),
        ..FakeBroker::default()
    };
    let (fd, _captures, _thread) = stand_up_fake_broker(broker);
    let mut manager = CollaborationManager::from_sdk_fd_for_test(fd, "so.tilo.notes", workspace);
    manager.join_route(route.clone(), crate::record::RecordKind::new(""));
    let tid = shovel_thread(&manager);
    // The route's first read arrives as an event for the app, which takes
    // its time: this test never merges it.
    let events = poll_until(&mut manager, |events| {
        events.iter().any(|event| {
            matches!(event, CollabEvent::CommittedDocument { route: read, .. } if *read == route)
        })
    });
    assert!(
        events
            .iter()
            .any(|event| matches!(event, CollabEvent::CommittedDocument { .. })),
        "the route's first committed read arrived: {events:?}"
    );
    eventually("the shovel sleeps", || blocked(&tid));
    assert_eq!(
        wakes_while_held(&tid),
        0,
        "the shovel woke while it waited for the app to take the read"
    );
}
