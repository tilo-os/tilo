//! Two-`App` end-to-end test proving that cross-process presence is
//! actually wired: peer A's pointer broadcast over a real `UnixStream`
//! lands in peer B's `PresenceCtx::remote_cursors` after a single
//! `pump_collab_presence()` tick.
//!
//! This is the deliverable for the "wire cross-process presence into
//! the UI toolkit" task: the existing `real_socket_two_peer.rs` proves
//! the wire-level fanout works, but stops at `CollabEvent::poll()`
//! returning the right event. This test goes one step further and
//! drives the App-side pump, which is what real `cargo run -p
//! terminal` instances would do every frame.
//!
//! Mirrors the broker setup from `real_socket_two_peer.rs` so we
//! don't reintroduce a bespoke broker per test.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use ui::app::App;
use ui::collaboration::{CollabEvent, CollabRequest, PresencePayload};
use ui::presence::PresenceKey;
use ui::state::State;
use ui::widget::Container;

fn read_frame(s: &mut UnixStream) -> Option<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    s.read_exact(&mut len_buf).ok()?;
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    s.read_exact(&mut buf).ok()?;
    Some(buf)
}

fn write_frame(s: &mut UnixStream, bytes: &[u8]) {
    let len = bytes.len() as u32;
    let _ = s.write_all(&len.to_le_bytes());
    let _ = s.write_all(bytes);
}

#[test]
fn cross_process_pump_lands_remote_cursor_on_peer_b() {
    // 1. Spin up a per-pid socket so parallel test runs don't collide.
    let socket_path = std::env::temp_dir().join(format!(
        "quick-collab-pump-{}.sock",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&socket_path);
    let listener =
        UnixListener::bind(&socket_path).expect("bind UnixListener for broker");

    // 2. Broker — accept two peers, fan Presence requests from one to
    // the other tagged with `peer-0` / `peer-1`.
    let (fwd_tx, fwd_rx) = mpsc::channel::<(usize, Vec<u8>)>();
    let broker_socket_path = socket_path.clone();
    let broker = thread::spawn(move || {
        let (s0, _) = listener.accept().expect("accept peer 0");
        let (s1, _) = listener.accept().expect("accept peer 1");
        let streams: Arc<Mutex<Vec<UnixStream>>> = Arc::new(Mutex::new(vec![
            s0.try_clone().expect("clone s0"),
            s1.try_clone().expect("clone s1"),
        ]));
        let mut readers = Vec::new();
        for (idx, mut read_stream) in [s0, s1].into_iter().enumerate() {
            let tx = fwd_tx.clone();
            readers.push(thread::spawn(move || {
                while let Some(frame) = read_frame(&mut read_stream) {
                    if tx.send((idx, frame)).is_err() {
                        break;
                    }
                }
            }));
        }
        drop(fwd_tx);

        while let Ok((src_idx, frame)) = fwd_rx.recv() {
            let Ok(req) = serde_json::from_slice::<CollabRequest>(&frame) else {
                continue;
            };
            let event = match req {
                CollabRequest::Hello { .. } => continue,
                CollabRequest::SyncIncremental { data } => {
                    CollabEvent::CRDTIncremental { data }
                }
                CollabRequest::SyncCRDT { data } => CollabEvent::CRDTUpdate { data },
                CollabRequest::Presence { key, payload } => {
                    CollabEvent::RemotePresenceUpdate {
                        user_id: format!("peer-{src_idx}"),
                        key,
                        payload,
                    }
                }
            };
            let Ok(bytes) = serde_json::to_vec(&event) else {
                continue;
            };
            let dst_idx = 1 - src_idx;
            if let Ok(mut guard) = streams.lock() {
                if let Some(stream) = guard.get_mut(dst_idx) {
                    write_frame(stream, &bytes);
                }
            }
        }
        for r in readers {
            let _ = r.join();
        }
        let _ = std::fs::remove_file(&broker_socket_path);
    });

    // 3. Point CollaborationManager at the broker. SAFETY: per-call
    // serialisation on the test thread — broker thread doesn't touch
    // env.
    unsafe {
        std::env::set_var("QUICK_COLLAB_SOCK", &socket_path);
    }

    // Build two Apps with trivial content. Both connect via
    // CollaborationManager::new() in App::new() → the broker's accept
    // calls return in order.
    let mut app_a = App::new(State::new(), Box::new(Container::new()));
    let mut app_b = App::new(State::new(), Box::new(Container::new()));

    // 4. App A broadcasts a pointer presence under a workspace key.
    // This is what the App::run loop does on CursorMoved (subject to
    // the 33 ms throttle, which we bypass here by calling the public
    // `broadcast_presence` directly).
    app_a.broadcast_presence(
        PresenceKey::Workspace { id: "home".into() },
        PresencePayload::Pointer { x: 320.0, y: 240.0 },
    );

    // Give the broker a tick to forward.
    thread::sleep(Duration::from_millis(100));

    // 5. App B pumps. The broker tagged the event as peer-0 (App A
    // was the first connection), so App B's PresenceCtx should now
    // hold a remote cursor for peer-0 at (320, 240).
    let any = app_b.pump_collab_presence();
    assert!(any, "App B should have processed at least one event");

    let presence = app_b.presence_ctx();
    let alice = presence
        .remote_cursors
        .iter()
        .find(|c| c.user_id == "peer-0")
        .expect("peer-0's pointer broadcast should populate App B's remote_cursors");
    assert_eq!(
        alice.position,
        [320.0, 240.0],
        "remote cursor must record the broadcast coordinates verbatim",
    );

    // 6. The pointers map should ALSO have the entry — widgets that
    // already read from `pointers[key]` (e.g. the shell's overlay
    // host) keep working.
    let key = PresenceKey::Workspace { id: "home".into() };
    let pointer = presence
        .pointer_for(&key, &ui::permissions::UserId("peer-0".into()))
        .expect("pointers[workspace home] should also be populated");
    assert_eq!(pointer, (320.0, 240.0));

    // 7. Now App A "leaves" — App B should purge them everywhere.
    app_a.broadcast_presence(
        PresenceKey::Workspace { id: "home".into() },
        PresencePayload::Left,
    );
    thread::sleep(Duration::from_millis(100));
    app_b.pump_collab_presence();
    let presence = app_b.presence_ctx();
    assert!(
        presence
            .remote_cursors
            .iter()
            .all(|c| c.user_id != "peer-0"),
        "App A's Left payload should drop their overlay cursor",
    );

    // 8. Clean teardown.
    drop(app_a);
    drop(app_b);
    broker.join().expect("broker thread panicked");
    let _ = std::fs::remove_file(&socket_path);
}
