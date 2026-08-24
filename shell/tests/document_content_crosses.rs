//! A picture dropped into a shared document has to reach the machines that
//! have to draw it.
//!
//! # The defect this pins
//!
//! Bytes deliberately do not live in the document — `qdoc::blobstore` says why,
//! and `qdoc/tests/blob_reference.rs` pins it. So the document carries a
//! reference and the content has to travel some other way, and the way is the
//! collaboration socket: an app's blob store is inside its sandbox (the
//! launcher gives every app a private `HOME` and `XDG_CACHE_HOME`,
//! deliberately), so there is no directory an app and its shell can both read
//! and there must not be one.
//!
//! That door existed on the legacy JSON dialect and not on the Cap'n Proto one
//! — and Cap'n Proto is the only one a real session uses
//! (`compositor/native.rs`: "in a real session nothing dials it — apps take the
//! capnp descriptor"). An app's offer of content was answered with a
//! `tracing::warn!` and dropped, in a process that installs no tracing
//! subscriber. The result on a live run: the shell on the machine the image was
//! added on logged that it did not hold the content its own app had just
//! stored, asked the room for it, and gave up after 38 s. Nobody in the room
//! had ever been given the bytes, so every screen showed a grey box.
//!
//! # What is real here and what is modelled
//!
//! The broker, the Cap'n Proto socket, the SDK client an app uses, and the
//! documents are the production ones. What stands in is the *room*: a sink
//! behind the same `PeerPublishSink` the shell's libp2p network implements, so
//! "the room holds what was offered and can answer for it" is exercised
//! without standing gossipsub up. The machine-to-machine hop
//! below that has its own coverage in `shell/src/network/blob_relay.rs`, and
//! was never the broken half — the logs show it asking, correctly, for content
//! nobody on this side had been given.

use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use qdoc::Document;
use qdoc::blobstore::{self, BlobState, BlobStore, MemoryBlobStore};

use shell::shell::collab_broker::spawn_with_publisher;
use shell::shell::collab_broker_capnp::dial_capnp_for_test;
use shell::shell::collab_outbound::{PeerPublishSink, PeerPublisher};

/// The room, as far as this test needs one.
///
/// Implements the same [`PeerPublishSink`] the shell's libp2p network does, so
/// the broker cannot tell it apart from the real thing. It keeps what it is
/// offered and can answer for it, which is the property the transfer depends
/// on: a sink that took content and then denied holding it would let this test
/// prove a delivery no real machine could make.
///
/// Everything below this trait — chunking, retries, gossip — is real code with
/// its own coverage (`shell/src/network/blob_relay.rs`) and was never the
/// broken half.
#[derive(Default)]
struct Room {
    blobs: Mutex<std::collections::HashMap<String, Vec<u8>>>,
    asked: Mutex<Vec<String>>,
}

impl PeerPublishSink for Room {
    fn app_crdt_incremental(&self, _: String, _: String, _: String, _: Vec<u8>) {}
    fn app_crdt_signed(&self, _: String, _: String, _: String, _: collab_types::SignedOp) {}
    fn app_crdt_snapshot(&self, _: String, _: String, _: String, _: Vec<u8>) {}
    fn app_presence(&self, _: String, _: String, _: Vec<u8>) {}
    fn scrollback_request(&self, _: collab_types::ArtifactId, _: u64, _: u16) {}

    fn offer_blob(&self, _cluster_id: &str, hash: &str, bytes: &[u8]) -> bool {
        // The key is the content's identity. Taking bytes that hash to
        // something else would let one machine poison every replica that later
        // reads that hash — the same rule every store in this system applies.
        if !blobstore::verify(hash, bytes) {
            return false;
        }
        self.blobs
            .lock()
            .unwrap()
            .insert(hash.to_string(), bytes.to_vec());
        true
    }

    fn blob(&self, hash: &str) -> Option<Vec<u8>> {
        self.blobs.lock().unwrap().get(hash).cloned()
    }

    fn want_blob(&self, _cluster_id: &str, hash: &str) {
        self.asked.lock().unwrap().push(hash.to_string());
    }
}

/// The app's side of the same door: `qdoc`'s two hooks, wired onto the two
/// channel verbs.
///
/// This is the shape `ui::collaboration::ShellBlobTransport` has in a real app
/// — storing content offers it, a failed read asks for it — reproduced here
/// because `ui`'s is private to `ui`. What it proves is that no call site has
/// to know content transfer exists: the document code below never mentions it.
struct AppDoor {
    cmds: Mutex<mpsc::Sender<Cmd>>,
}

impl blobstore::BlobTransport for AppDoor {
    fn offer(&self, hash: &str, bytes: &[u8]) {
        let _ = self.cmds.lock().unwrap().send(Cmd::Offer {
            hash: hash.to_string(),
            bytes: bytes.to_vec(),
        });
    }
    fn want(&self, hash: &str) {
        let _ = self.cmds.lock().unwrap().send(Cmd::Want {
            hash: hash.to_string(),
        });
    }
}

/// A structurally valid PNG with a body big enough that finding it by a window
/// scan is trivial — so an assertion about "the bytes arrived" cannot be
/// satisfied by anything smaller than the bytes.
fn png() -> Vec<u8> {
    let mut v = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    v.extend_from_slice(&13u32.to_be_bytes());
    v.extend_from_slice(b"IHDR");
    v.extend_from_slice(&640u32.to_be_bytes());
    v.extend_from_slice(&360u32.to_be_bytes());
    v.extend_from_slice(&[8, 6, 0, 0, 0]);
    v.extend_from_slice(&[0u8; 4]);
    // Grain rather than a flat run: a photograph's size is the thing that broke
    // the old path, and a body that compresses to nothing would not be one.
    let mut x: u32 = 0x9E37_79B9;
    for _ in 0..(150 * 1024) {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        v.push((x & 0xFF) as u8);
    }
    v
}

fn unique_paths(label: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir();
    let n = std::process::id() as u64 * 1_000_003
        + std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos() as u64;
    (
        base.join(format!("qos-{label}-{n}.json.sock")),
        base.join(format!("qos-{label}-{n}.capnp.sock")),
    )
}

/// What the capnp worker thread is told to do, and what it reports back.
enum Cmd {
    /// The app that has the picture hands it to its shell.
    Offer { hash: String, bytes: Vec<u8> },
    /// A second app asks its shell for content it does not have.
    Want { hash: String },
    Stop,
}

enum Report {
    /// Both apps have joined and opened their content subscriptions.
    Ready,
    /// Content reached the second app's blob stream.
    Arrived { who: &'static str, hash: String, bytes: Vec<u8> },
}

#[test]
fn a_photograph_reaches_the_document_on_the_second_machine() {
    let (json_path, capnp_path) = unique_paths("content-crosses");

    // The room: holds what is offered to it and can answer for it afterwards,
    // which is exactly the contract `ShellNetwork` implements for real.
    let sink = Arc::new(Room::default());
    let publisher = PeerPublisher::new(Arc::clone(&sink) as Arc<dyn PeerPublishSink>);
    let handle = spawn_with_publisher(json_path.clone(), Some(capnp_path.clone()), publisher)
        .expect("broker spawns");

    let channel_id = uuid::Uuid::new_v4().to_string();
    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
    let (rep_tx, rep_rx) = mpsc::channel::<Report>();

    // Two apps in one room, each with its own connection to the broker. They
    // share a thread only because capnp-rpc futures are `!Send`; on the broker
    // they are two independent peers with two independent subscriptions, which
    // is what matters.
    let capnp_for_thread = capnp_path.clone();
    let channel_for_thread = channel_id.clone();
    let worker = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, async move {
            let (client_a, _drv_a) = dial_capnp_for_test(capnp_for_thread.clone())
                .await
                .expect("app A dials the broker");
            let (client_b, _drv_b) = dial_capnp_for_test(capnp_for_thread.clone())
                .await
                .expect("app B dials the broker");
            let sdk_a = runtime_collab::CollabClient::from_raw(client_a);
            let sdk_b = runtime_collab::CollabClient::from_raw(client_b);
            let chan_a = sdk_a
                .join_channel(&channel_for_thread, b"identity-A")
                .await
                .expect("app A joins");
            let chan_b = sdk_b
                .join_channel(&channel_for_thread, b"identity-B")
                .await
                .expect("app B joins");

            // Both apps subscribe: an app that opened a document with a
            // picture already in it is waiting for content before it has any
            // reason to ask for it, and that is the case that matters most.
            let blobs_a = chan_a.subscribe_blobs().await.expect("A subscribes");
            let blobs_b = chan_b.subscribe_blobs().await.expect("B subscribes");

            for (who, stream) in [("A", blobs_a), ("B", blobs_b)] {
                let tx = rep_tx.clone();
                tokio::task::spawn_local(async move {
                    while let Ok(evt) = stream.read().await {
                        if tx
                            .send(Report::Arrived {
                                who,
                                hash: evt.hash,
                                bytes: evt.bytes,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                });
            }
            let _ = rep_tx.send(Report::Ready);

            loop {
                // The command channel is synchronous and this task owns the
                // whole LocalSet, so yield between checks rather than blocking
                // the executor the RPC futures run on.
                match cmd_rx.try_recv() {
                    Ok(Cmd::Offer { hash, bytes }) => {
                        chan_a.offer_blob(&hash, &bytes).await.expect("A offers");
                    }
                    Ok(Cmd::Want { hash }) => {
                        chan_b.want_blob(&hash).await.expect("B asks");
                    }
                    Ok(Cmd::Stop) => break,
                    Err(mpsc::TryRecvError::Empty) => {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                    Err(mpsc::TryRecvError::Disconnected) => break,
                }
            }
        });
    });

    let ready = wait_for(&rep_rx, Duration::from_secs(10), |r| {
        matches!(r, Report::Ready)
    });
    assert!(ready.is_some(), "both apps must join before content can move");

    // --- the two documents ------------------------------------------------
    //
    // The blob store is a property of the machine, not of a document, so the
    // two machines are two stores and the test hands whichever is current to
    // `qdoc`. Each side only ever reads its own.
    let store_a: Arc<dyn BlobStore> = Arc::new(MemoryBlobStore::default());
    let store_b: Arc<dyn BlobStore> = Arc::new(MemoryBlobStore::default());

    // And the door out of the app process, which is what turns a `put_blob`
    // into an offer and a failed read into a request. In production this is
    // `ui`'s `ShellBlobTransport`; here it is the same two hooks onto the same
    // two channel verbs, so no call site below mentions content transfer —
    // exactly as no call site in any app does.
    blobstore::set_transport(Arc::new(AppDoor {
        cmds: Mutex::new(cmd_tx.clone()),
    }));

    blobstore::set_store(Arc::clone(&store_a));
    let bytes = png();
    let mut doc_a = Document::new();
    let p = doc_a.insert_block(None, "paragraph");
    doc_a.splice_text(&p, 0, 0, "Doors at seven, room seats 240.");
    let hash = doc_a.put_blob(&bytes);
    let img = doc_a.insert_block(Some(&p), "image");
    doc_a.as_mut().set_attr(&img, "blob", hash.as_str());
    let saved = doc_a.save();

    // The reference crosses in the document, the way it does in production.
    blobstore::set_store(Arc::clone(&store_b));
    let doc_b = Document::load(&saved).expect("the document reaches the second machine");
    assert!(
        !contains(&saved, &bytes[..256]),
        "the document carried the photograph's bytes; the reference is supposed to be \
         the only thing in it",
    );

    // Before the transfer: the second machine has the reference and not the
    // content, and says which of the two "no picture yet" cases it is in.
    assert_eq!(
        doc_b.blob(&hash),
        None,
        "the second machine cannot already hold content nobody has sent it",
    );
    assert_eq!(
        doc_b.blob_state(&hash),
        BlobState::Pending,
        "a reference whose content is still coming must not read as a failure",
    );

    // --- the transfer -----------------------------------------------------
    //
    // Nothing is asked for here. `put_blob` above already offered the content
    // through the door, and the failed read just above already asked for it:
    // app A hands its shell the bytes, the shell offers them to the room and
    // fans them to the other apps in it. That is the hop that did not exist.
    let got = wait_for(&rep_rx, Duration::from_secs(10), |r| {
        matches!(r, Report::Arrived { who: "B", .. })
    })
    .expect("content an app offered never reached the other app in the room");
    let Report::Arrived {
        hash: got_hash,
        bytes: got_bytes,
        ..
    } = got
    else {
        unreachable!()
    };
    assert_eq!(got_hash, hash, "content arrived under the wrong name");
    assert_eq!(
        got_bytes, bytes,
        "the bytes that arrived are not the photograph that was sent",
    );

    // The room was handed it too, so a machine that joins later can be
    // answered. Without this the transfer only ever works for whoever happened
    // to be listening at the moment the image was added.
    assert_eq!(
        sink.blob(&hash).as_deref(),
        Some(bytes.as_slice()),
        "the room was never given the content, so nobody else can obtain it",
    );

    // The receiving app stores what it verified, which is what `poll` does.
    assert!(
        store_b.put(&got_hash, &got_bytes),
        "arrived content did not verify against the name it arrived under",
    );

    // --- the payoff -------------------------------------------------------
    assert_eq!(
        doc_b.blob(&hash).as_deref(),
        Some(bytes.as_slice()),
        "the second machine's document still cannot resolve its own image",
    );
    assert_eq!(
        doc_b.blob_state(&hash),
        BlobState::Resolved,
        "content that has arrived must stop reporting itself as arriving",
    );

    // --- and for an app that was not listening when it was offered --------
    //
    // Drop what B holds and have it ask. The broker answers from the room
    // rather than making the whole room re-send, which is the path a late
    // joiner and a reopened document both take.
    cmd_tx
        .send(Cmd::Want { hash: hash.clone() })
        .expect("worker alive");
    let answered = wait_for(&rep_rx, Duration::from_secs(10), |r| {
        matches!(r, Report::Arrived { who: "B", .. })
    })
    .expect("an app asking for content it lacks was never answered");
    let Report::Arrived { bytes: again, .. } = answered else {
        unreachable!()
    };
    assert_eq!(
        again, bytes,
        "the answer to a content request was not the content",
    );

    let _ = cmd_tx.send(Cmd::Stop);
    let _ = worker.join();
    drop(handle);
    let _ = std::fs::remove_file(&json_path);
    let _ = std::fs::remove_file(&capnp_path);
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Take reports until one matches, discarding the rest. Returns `None` on
/// timeout so the caller can say what was being waited for.
fn wait_for(
    rx: &mpsc::Receiver<Report>,
    deadline: Duration,
    mut matches: impl FnMut(&Report) -> bool,
) -> Option<Report> {
    let end = Instant::now() + deadline;
    while Instant::now() < end {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(r) if matches(&r) => return Some(r),
            Ok(_) => continue,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => return None,
        }
    }
    None
}

/// Silences an unused-import warning when the file is read by a human wondering
/// where the shared mutex went: there is none, because this binary holds one
/// test and the process-wide store therefore has one owner.
const _: Option<Mutex<()>> = None;
