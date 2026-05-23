//! Two-peer end-to-end test across a real Unix socket.
//!
//! Closes outstanding-work.md §3 #4: all prior collab tests went
//! through the in-process `MemoryRelay`, leaving the actual wire
//! protocol in `ui::collaboration` (4-byte LE length prefix
//! + JSON body over a `UnixStream`) unexercised.
//!
//! This test stands up a small test-only broker that:
//!   * binds a `UnixListener`
//!   * accepts exactly two `CollaborationManager` connections
//!   * reads framed `CollabRequest`s from each peer
//!   * translates them to the matching `CollabEvent` and writes the
//!     framed event to the *other* peer's stream
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

use ui::collaboration::{
    CollabEvent, CollabRequest, CollaborationManager, PresencePayload,
};
use ui::presence::PresenceKey;

/// Read one length-prefixed frame from `s`. Returns `None` on EOF /
/// broken pipe — the broker treats that as the peer closing.
fn read_frame(s: &mut UnixStream) -> Option<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    s.read_exact(&mut len_buf).ok()?;
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    s.read_exact(&mut buf).ok()?;
    Some(buf)
}

/// Write one length-prefixed frame to `s`. Errors are intentionally
/// swallowed — in a test, if the peer has gone away we just stop.
fn write_frame(s: &mut UnixStream, bytes: &[u8]) {
    let len = bytes.len() as u32;
    let _ = s.write_all(&len.to_le_bytes());
    let _ = s.write_all(bytes);
}

#[test]
fn real_socket_fans_out_incremental_and_presence() {
    // 1. Choose a per-pid temp socket path so parallel test runs
    //    (or an abandoned previous run) don't collide.
    let socket_path = std::env::temp_dir()
        .join(format!("quick-collab-{}.sock", std::process::id()));
    // Best-effort cleanup in case a prior run left a stale inode.
    let _ = std::fs::remove_file(&socket_path);

    let listener = UnixListener::bind(&socket_path)
        .expect("bind UnixListener for broker");

    // 2. Spawn the broker. It accepts 2 connections, spawns a reader
    //    per connection, and fans requests to the opposite peer.
    let (fwd_tx, fwd_rx) = mpsc::channel::<(usize, Vec<u8>)>();
    let broker_socket_path = socket_path.clone();
    let broker = thread::spawn(move || {
        // Accept exactly two peers.
        let (s0, _) = listener.accept().expect("accept peer 0");
        let (s1, _) = listener.accept().expect("accept peer 1");

        // Shared write-side access to both streams so each reader
        // thread can send to the *other* peer.
        let streams: Arc<Mutex<Vec<UnixStream>>> = Arc::new(Mutex::new(vec![
            s0.try_clone().expect("clone s0"),
            s1.try_clone().expect("clone s1"),
        ]));

        // Spawn one reader per peer. Each forwards raw frames tagged
        // with the source index over `fwd_tx`.
        let mut readers = Vec::new();
        for (idx, mut read_stream) in [s0, s1].into_iter().enumerate() {
            let tx = fwd_tx.clone();
            readers.push(thread::spawn(move || {
                while let Some(frame) = read_frame(&mut read_stream) {
                    if tx.send((idx, frame)).is_err() {
                        break;
                    }
                }
                // Drop tx when the reader exits; when both readers
                // exit and the main broker's own clone is dropped,
                // the recv loop below will return `Err(_)`.
            }));
        }
        // Drop the broker-owned clone so the recv loop can terminate
        // once both peers close.
        drop(fwd_tx);

        // Main forwarding loop.
        while let Ok((src_idx, frame)) = fwd_rx.recv() {
            let Ok(req) = serde_json::from_slice::<CollabRequest>(&frame)
            else {
                continue; // bad frame — skip
            };

            // Map request → event. `Hello` is swallowed (no fan-out).
            let event = match req {
                CollabRequest::Hello { .. } => continue,
                CollabRequest::SyncIncremental { data } => {
                    CollabEvent::CRDTIncremental { data }
                }
                CollabRequest::SyncCRDT { data } => {
                    CollabEvent::CRDTUpdate { data }
                }
                CollabRequest::Presence { key, payload } => {
                    CollabEvent::RemotePresenceUpdate {
                        user_id: format!("peer-{src_idx}"),
                        key,
                        payload,
                    }
                }
            };

            let bytes = match serde_json::to_vec(&event) {
                Ok(b) => b,
                Err(_) => continue,
            };

            // Forward to the OTHER peer.
            let dst_idx = 1 - src_idx;
            if let Ok(mut guard) = streams.lock() {
                if let Some(stream) = guard.get_mut(dst_idx) {
                    write_frame(stream, &bytes);
                }
            }
        }

        // Reader threads exit once their read_frame returns None
        // (i.e. the peer closed); join them for cleanliness.
        for r in readers {
            let _ = r.join();
        }
        // Help the test binary (and any concurrent runs) by
        // removing the socket file. Best effort.
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

    // Drain any Hello echoes. The broker swallows `Hello`, so this
    // should be empty — but poll anyway to match the intended
    // steady-state API pattern.
    let _ = peer_a.poll();
    let _ = peer_b.poll();

    // --- Assertion 1: CRDT incremental ------------------------------
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

    // --- Assertion 2: presence fanout -------------------------------
    peer_a.send(CollabRequest::Presence {
        key: PresenceKey::Workspace { id: "77".into() },
        payload: PresencePayload::Arrived,
    });
    thread::sleep(Duration::from_millis(100));

    let events_b = peer_b.poll();
    let presence_match = events_b.iter().any(|e| matches!(
        e,
        CollabEvent::RemotePresenceUpdate {
            user_id,
            key: PresenceKey::Workspace { id },
            payload: PresencePayload::Arrived,
        } if user_id == "peer-0" && id == "77"
    ));
    assert!(
        presence_match,
        "peer B should see RemotePresenceUpdate tagged peer-0 for Workspace 77, got events={events_b:?}"
    );

    // 4. Clean up: drop the managers so their UnixStreams close,
    //    which unblocks the broker's reader threads, which drops
    //    the last `fwd_tx` clones, which breaks the broker's recv
    //    loop. Then join the broker.
    drop(peer_a);
    drop(peer_b);

    broker
        .join()
        .expect("broker thread panicked");

    // Socket file is removed by the broker on exit; best-effort
    // again in case the broker never reached the cleanup line.
    let _ = std::fs::remove_file(&socket_path);
}
