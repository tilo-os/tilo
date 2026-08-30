//! N-peer end-to-end test across a real Unix socket, exercising the
//! production broker (`broker::serve`, the same code path the binary runs).
//!
//! Generalizes `real_socket_two_peer.rs` to three peers, asserting the two
//! properties that matter for N > 2:
//!
//!   1. **Fan-out excludes the sender.** A `SyncIncremental` from peer A
//!      reaches B and C exactly once each, and never echoes back to A.
//!   2. **Presence is attributed per peer.** A presence from A and B
//!      arrives at C tagged with the id each announced — so the two are
//!      distinct — and A never receives its own presence back.
//!
//! Everything is one `#[test]` so the process-global `QUICK_COLLAB_SOCK`
//! and `QUICK_APP_ID` env vars aren't raced by parallel tests in this binary.

use std::thread;
use std::time::Duration;

use ui::collaboration::{CollabEvent, CollabRequest, CollaborationManager, PresencePayload};
use ui::presence::PresenceKey;

#[test]
#[serial_test::serial(quick_collab_sock)]
fn n_peer_fanout_and_per_peer_attribution() {
    let socket_path =
        std::env::temp_dir().join(format!("quick-collab-n-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket_path);

    let listener =
        std::os::unix::net::UnixListener::bind(&socket_path).expect("bind broker listener");

    // Run the production broker on a side thread; it serves until the
    // process exits (which kills the thread).
    thread::spawn(move || ui::collaboration::broker::serve(listener));

    // SAFETY: only this thread mutates env, and it does so between
    // manager constructions — this is the single test in this binary, so
    // nothing else in the process reads the environment concurrently.
    //
    // `QUICK_APP_ID` must be set per peer: an app with none falls back to
    // a single process-wide identity (see `auto_hello_env`), so three
    // peers need three separate launches with three separate ids.
    unsafe {
        std::env::set_var("QUICK_COLLAB_SOCK", &socket_path);
    }

    let announce = |id: &str| {
        // SAFETY: as above.
        unsafe { std::env::set_var("QUICK_APP_ID", id) };
        CollaborationManager::new()
    };
    let mut a = announce("peer-a");
    let mut b = announce("peer-b");
    let mut c = announce("peer-c");

    // Give the broker time to register all three Hello handshakes.
    thread::sleep(Duration::from_millis(150));
    let _ = a.poll();
    let _ = b.poll();
    let _ = c.poll();

    // --- 1. Fan-out excludes the sender -----------------------------
    a.send(CollabRequest::SyncIncremental {
        data: b"X".to_vec(),
    });
    thread::sleep(Duration::from_millis(150));

    let count_incremental = |evs: &[CollabEvent]| {
        evs.iter()
            .filter(|e| matches!(e, CollabEvent::CRDTIncremental { data } if data == b"X"))
            .count()
    };
    let b_evs = b.poll();
    let c_evs = c.poll();
    let a_evs = a.poll();
    assert_eq!(
        count_incremental(&b_evs),
        1,
        "B should get A's incremental once: {b_evs:?}"
    );
    assert_eq!(
        count_incremental(&c_evs),
        1,
        "C should get A's incremental once: {c_evs:?}"
    );
    assert_eq!(
        count_incremental(&a_evs),
        0,
        "A must not receive its own incremental: {a_evs:?}"
    );

    // --- 2. Per-peer presence attribution ---------------------------
    // A announces on Workspace "1"; B announces on Workspace "2".
    a.send(CollabRequest::Presence {
        key: PresenceKey::Workspace { id: "1".into() },
        payload: PresencePayload::Pointer { x: 1.0, y: 1.0 },
    });
    b.send(CollabRequest::Presence {
        key: PresenceKey::Workspace { id: "2".into() },
        payload: PresencePayload::Pointer { x: 2.0, y: 2.0 },
    });
    thread::sleep(Duration::from_millis(150));

    let c_evs = c.poll();
    // Extract the attributed user_id for each source by its distinct key.
    let uid_for = |evs: &[CollabEvent], ws: &str| -> Option<String> {
        evs.iter().find_map(|e| match e {
            CollabEvent::RemotePresenceUpdate {
                user_id,
                key: PresenceKey::Workspace { id },
                ..
            } if id == ws => Some(user_id.clone()),
            _ => None,
        })
    };
    let uid_a = uid_for(&c_evs, "1").expect("C sees A's Workspace-1 presence");
    let uid_b = uid_for(&c_evs, "2").expect("C sees B's Workspace-2 presence");
    // Asserts the exact announced ids, not just `uid_a != uid_b` — that
    // weaker check would still pass if the broker swapped them.
    assert_eq!(uid_a, "peer-a", "A's presence must be attributed to A");
    assert_eq!(uid_b, "peer-b", "B's presence must be attributed to B");
    assert_ne!(uid_a, uid_b, "A and B must be attributed distinct user ids");

    // A must NOT receive its own Workspace-1 presence, but SHOULD see
    // B's Workspace-2 presence.
    let a_evs = a.poll();
    assert!(
        uid_for(&a_evs, "1").is_none(),
        "A must not receive its own presence back: {a_evs:?}"
    );
    assert!(
        uid_for(&a_evs, "2").is_some(),
        "A should receive B's presence: {a_evs:?}"
    );

    drop(a);
    drop(b);
    drop(c);
    let _ = std::fs::remove_file(&socket_path);
}
