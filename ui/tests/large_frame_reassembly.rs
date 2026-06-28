//! A large `SyncCRDT` payload (a full-document snapshot — the late-join
//! reply) does not fit in one socket read, so it arrives split across
//! reads. `CollaborationManager::poll` must reassemble it from its
//! persistent buffer instead of dropping the frame and desyncing the
//! stream. This is the bug that made two live notes instances fail to
//! converge while small presence frames went through fine — the test
//! relay only ever sent tiny payloads, so it was never exercised.

use std::os::unix::net::UnixListener;
use std::thread;
use std::time::Duration;

use ui::collaboration::{CollabEvent, CollabRequest, CollaborationManager};

#[test]
fn large_frame_survives_split_reads() {
    let socket_path = std::env::temp_dir()
        .join(format!("quick-collab-big-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path).expect("bind broker");
    thread::spawn(move || ui::collaboration::broker::serve(listener));

    // SAFETY: single-threaded env mutation before any manager connects.
    unsafe {
        std::env::set_var("QUICK_COLLAB_SOCK", &socket_path);
    }
    let mut a = CollaborationManager::new();
    let mut b = CollaborationManager::new();
    thread::sleep(Duration::from_millis(150));
    let _ = a.poll();
    let _ = b.poll();

    // 256 KB — far larger than a single socket read, so it MUST span
    // multiple reads on B's side.
    let big = vec![0xABu8; 256 * 1024];
    a.send(CollabRequest::SyncCRDT { data: big.clone() });

    // Poll B for up to ~2s, accumulating the frame across reads.
    let mut got: Option<Vec<u8>> = None;
    for _ in 0..40 {
        thread::sleep(Duration::from_millis(50));
        for e in b.poll() {
            if let CollabEvent::CRDTUpdate { data } = e {
                got = Some(data);
            }
        }
        if got.is_some() {
            break;
        }
    }

    let data = got.expect("B must receive the large CRDTUpdate frame");
    assert_eq!(data.len(), big.len(), "payload length must match");
    assert!(data == big, "payload must round-trip byte-for-byte");

    drop(a);
    drop(b);
    let _ = std::fs::remove_file(&socket_path);
}
