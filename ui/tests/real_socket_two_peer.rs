//! Two-peer end-to-end test across a real Unix socket.
//!
//! All prior collab tests went through the in-process `MemoryRelay`,
//! leaving the actual wire protocol in `ui::collaboration` (4-byte LE
//! length prefix + JSON body over a `UnixStream`) unexercised.
//!
//! Stands up a small test-only broker that binds a `UnixListener`, accepts
//! exactly two `CollaborationManager` connections, reads framed
//! `CollabRequest`s from each peer, and translates + writes each as the
//! matching `CollabEvent` to the *other* peer's stream.
//!
//! Two assertions:
//!   1. `SyncIncremental { data: b"hello" }` from peer A arrives at
//!      peer B as `CRDTIncremental { data: b"hello" }`.
//!   2. `Presence { Workspace { "77" }, Arrived }` from peer A
//!      arrives at peer B as `RemotePresenceUpdate` tagged
//!      `user_id = "peer-0"`.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use ui::collaboration::{CollabEvent, CollabRequest, CollaborationManager, PresencePayload};
use ui::presence::PresenceKey;

/// Returns `None` on EOF / broken pipe — the broker treats that as the peer closing.
fn read_frame(s: &mut UnixStream) -> Option<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    s.read_exact(&mut len_buf).ok()?;
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    s.read_exact(&mut buf).ok()?;
    Some(buf)
}

/// Errors are intentionally swallowed — if the peer has gone away we just stop.
fn write_frame(s: &mut UnixStream, bytes: &[u8]) {
    let len = bytes.len() as u32;
    let _ = s.write_all(&len.to_le_bytes());
    let _ = s.write_all(bytes);
}

#[test]
#[serial_test::serial(quick_collab_sock)]
fn real_socket_fans_out_incremental_and_presence() {
    // Per-pid temp socket path so parallel test runs don't collide.
    let socket_path =
        std::env::temp_dir().join(format!("quick-collab-{}.sock", std::process::id()));
    // Best-effort cleanup in case a prior run left a stale inode.
    let _ = std::fs::remove_file(&socket_path);

    let listener = UnixListener::bind(&socket_path).expect("bind UnixListener for broker");

    // The broker accepts 2 connections, spawns a reader per connection, and
    // fans requests to the opposite peer.
    let (fwd_tx, fwd_rx) = mpsc::channel::<(usize, Vec<u8>)>();
    let broker_socket_path = socket_path.clone();
    let broker = thread::spawn(move || {
        let (s0, _) = listener.accept().expect("accept peer 0");
        let (s1, _) = listener.accept().expect("accept peer 1");

        // Shared write-side access to both streams so each reader
        // thread can send to the *other* peer.
        let streams: Arc<Mutex<Vec<UnixStream>>> = Arc::new(Mutex::new(vec![
            s0.try_clone().expect("clone s0"),
            s1.try_clone().expect("clone s1"),
        ]));

        // One reader per peer, forwarding raw frames tagged with the source index.
        let mut readers = Vec::new();
        for (idx, mut read_stream) in [s0, s1].into_iter().enumerate() {
            let tx = fwd_tx.clone();
            readers.push(thread::spawn(move || {
                while let Some(frame) = read_frame(&mut read_stream) {
                    if tx.send((idx, frame)).is_err() {
                        break;
                    }
                }
                // tx drops when the reader exits; once both readers exit and
                // the broker's own clone (below) is dropped, recv returns Err.
            }));
        }
        drop(fwd_tx);

        while let Ok((src_idx, frame)) = fwd_rx.recv() {
            let Ok(req) = serde_json::from_slice::<CollabRequest>(&frame) else {
                continue; // bad frame — skip
            };

            // Map request → event. `Hello` is swallowed (no fan-out).
            let event = match req {
                CollabRequest::Hello { .. } => continue,
                CollabRequest::SyncIncremental { data } => CollabEvent::CRDTIncremental { data },
                CollabRequest::SyncCRDT { data } => CollabEvent::CRDTUpdate { data },
                CollabRequest::Presence { key, payload } => CollabEvent::RemotePresenceUpdate {
                    user_id: format!("peer-{src_idx}"),
                    key,
                    payload,
                },
                // Not part of this two-peer plaintext scenario — swallow like `Hello`.
                CollabRequest::SignedSyncIncremental { .. }
                | CollabRequest::FetchScrollback { .. } => continue,
            };

            let bytes = match serde_json::to_vec(&event) {
                Ok(b) => b,
                Err(_) => continue,
            };

            let dst_idx = 1 - src_idx;
            if let Ok(mut guard) = streams.lock()
                && let Some(stream) = guard.get_mut(dst_idx)
            {
                write_frame(stream, &bytes);
            }
        }

        // Readers exit once read_frame returns None (peer closed).
        for r in readers {
            let _ = r.join();
        }
        // Remove the socket file, best effort.
        let _ = std::fs::remove_file(&broker_socket_path);
    });

    // 3. Point both CollaborationManagers at the broker.
    //    SAFETY: set_var on std::env is unsafe on edition 2024 when
    //    other threads may be reading env concurrently. The broker
    //    thread does not touch env; the two `new()` calls happen
    //    serially on this thread.
    unsafe {
        std::env::set_var("QUICK_COLLAB_SOCK", &socket_path);
    }

    let mut peer_a = CollaborationManager::new();
    let mut peer_b = CollaborationManager::new();

    // The broker swallows `Hello`, so this should be empty — polled anyway
    // to match the intended steady-state API pattern.
    let _ = peer_a.poll();
    let _ = peer_b.poll();

    // Assertion 1: CRDT incremental.
    peer_a.send(CollabRequest::SyncIncremental {
        data: b"hello".to_vec(),
    });
    thread::sleep(Duration::from_millis(100));

    let events_b = peer_b.poll();
    let crdt_hits: Vec<&Vec<u8>> = events_b
        .iter()
        .filter_map(|e| match e {
            CollabEvent::CRDTIncremental { data } => Some(data),
            _ => None,
        })
        .collect();
    assert_eq!(
        crdt_hits.len(),
        1,
        "peer B should see exactly one CRDTIncremental, got events={events_b:?}"
    );
    assert_eq!(
        crdt_hits[0].as_slice(),
        b"hello",
        "CRDT payload should round-trip verbatim"
    );

    // Assertion 2: presence fanout.
    peer_a.send(CollabRequest::Presence {
        key: PresenceKey::Workspace { id: "77".into() },
        payload: PresencePayload::Arrived,
    });
    thread::sleep(Duration::from_millis(100));

    let events_b = peer_b.poll();
    let presence_match = events_b.iter().any(|e| {
        matches!(
            e,
            CollabEvent::RemotePresenceUpdate {
                user_id,
                key: PresenceKey::Workspace { id },
                payload: PresencePayload::Arrived,
            } if user_id == "peer-0" && id == "77"
        )
    });
    assert!(
        presence_match,
        "peer B should see RemotePresenceUpdate tagged peer-0 for Workspace 77, got events={events_b:?}"
    );

    // Dropping the managers closes their UnixStreams, unblocking the
    // broker's readers, which drops the last `fwd_tx` clones and breaks its recv loop.
    drop(peer_a);
    drop(peer_b);

    broker.join().expect("broker thread panicked");

    // Best-effort again in case the broker never reached its own cleanup.
    let _ = std::fs::remove_file(&socket_path);
}
