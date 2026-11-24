//! The session's records' content, as it reaches it: the device's daemon keeps each scope's
//! bytes on one shelf, and serves them on its document-media socket. What an app kept outlives the
//! session that kept it, a record the session keeps itself keeps its parts sealed there, and what
//! this device lacks is asked for and read back once the daemon has it.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tilo_fs_client::blobs::BlobClient;
use tilo_runtime::profile::MediaHost;

use super::{RecordContent, open};

/// The app whose record references the content.
const APP: &str = "so.tilo.notes";
/// The workspace the record is kept in.
const WORKSPACE: &str = "0f2b7c1e-9a4d-4c8e-9d3a-1b5e6f7a8c90";
/// How long content asked for has to arrive.
const DEADLINE: Duration = Duration::from_secs(10);

/// One device: its content store, served on a socket as its daemon serves it, and a session of
/// it, reading and keeping through that socket as a session does.
struct Device {
    socket: PathBuf,
    media: MediaHost,
    _state: tempfile::TempDir,
    content: Arc<RecordContent>,
}

impl Device {
    fn open() -> Self {
        let state = tempfile::tempdir().expect("a device's state");
        let socket = state.path().join("blobs.sock");
        let media = MediaHost::start(&state.path().join("store"), &socket)
            .expect("the device serves its content");
        let content = open(Some(socket.clone()));
        Self {
            socket,
            media,
            _state: state,
            content,
        }
    }

    /// This session's content, as a session that restarted sees it.
    fn restarted(&self) -> Arc<RecordContent> {
        open(Some(self.socket.clone()))
    }
}

/// The bytes as an app stores them: a picture in a note.
fn picture() -> Vec<u8> {
    (0..300_000_u32)
        .map(|index| u8::try_from(index % 251).expect("a byte"))
        .collect()
}

/// Run `done` until it holds, or the deadline passes: whether it held.
fn until(mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + DEADLINE;
    loop {
        if done() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn content_an_app_kept_is_read_back_after_the_session_restarted() {
    let device = Device::open();
    let bytes = picture();
    let hash = tilo_content::hash_bytes(&bytes);
    device
        .content
        .keep_in(WORKSPACE, APP, &bytes)
        .expect("the app's content is kept");

    // The session restarted: nothing of the one that kept it is left, and the next reads the
    // content its record references from the shelf the daemon kept it on.
    let restarted = device.restarted();
    assert_eq!(
        restarted.read(WORKSPACE, APP, &hash),
        Some(bytes),
        "the content the record references"
    );
}

#[test]
fn what_an_app_kept_through_its_workspace_outlives_the_session() {
    let device = Device::open();
    let bytes = picture();
    let hash = device
        .content
        .keep(WORKSPACE, &bytes)
        .expect("the workspace's content is kept");
    assert_eq!(hash, tilo_content::hash_bytes(&bytes));

    let restarted = device.restarted();
    assert!(
        restarted.holds(WORKSPACE, &hash),
        "content the session kept for its workspace is still here"
    );
    assert_eq!(
        restarted.read(WORKSPACE, super::PUT_SCOPE, &hash),
        Some(bytes)
    );
}

/// The store a session lends the records it keeps itself: what an action too large to record whole
/// keeps is sealed and kept on the one shelf its workspace's devices fetch it from, and reads back
/// through the same store.
#[test]
fn a_record_the_session_keeps_itself_keeps_its_parts_sealed_on_the_shelf() {
    let device = Device::open();
    let bytes = picture();
    device.content.seal_under(
        WORKSPACE,
        APP,
        Some(tilo_seal::SymmetricKey::generate().expect("a content secret")),
    );
    let mine = device.content.store(WORKSPACE, APP);
    let reference = mine.put(&bytes).expect("the record's store keeps the part");
    assert!(
        tilo_content::is_sealed(&reference),
        "a record's parts are sealed: {reference}"
    );
    assert_eq!(
        mine.read(&reference).as_deref(),
        Some(bytes.as_slice()),
        "the device that kept the part reads it back"
    );

    // The same bytes kept again are the same reference, and one item: the members' secret, not the
    // bytes alone, is what derives each keep's key.
    let again = device.content.store(WORKSPACE, APP).put(&bytes);
    assert_eq!(again.expect("kept again").as_str(), reference.as_str());
}

/// What this device does not keep is asked for by the daemon, which asks the workspace's other
/// devices and keeps what one answers: the session's next read finds it there.
#[test]
fn content_this_device_lacks_is_asked_for_and_read_once_it_is_here() {
    let device = Device::open();
    let bytes = picture();
    let hash = tilo_content::hash_bytes(&bytes);
    assert!(
        device.content.read(WORKSPACE, APP, &hash).is_none(),
        "the device keeps none of it yet"
    );

    // The daemon's record content, as its scope's ask reaches it: here it answers with the bytes,
    // which the store keeps for the scope.
    let socket = device.socket.clone();
    let answering = bytes.clone();
    let asked_for = hash.clone();
    device.media.fetch_content_through(Arc::new(
        move |scope: &tilo_fs_client::blobs::BlobScope, asked: &str| {
            if asked != asked_for {
                return;
            }
            let session = BlobClient::connect_session(&socket).expect("the content store");
            let scoped = session
                .scope(scope.workspace(), scope.app())
                .expect("a scope of the store");
            let _ = scoped
                .put(&answering, "image/png")
                .expect("the bytes are kept");
        },
    ));

    let arrived = until(|| device.content.read(WORKSPACE, APP, &hash).is_some());
    assert!(arrived, "the content the daemon fetched reads back");
    assert_eq!(device.content.read(WORKSPACE, APP, &hash), Some(bytes));
}

#[test]
fn a_workspace_or_app_name_that_leaves_the_store_keeps_no_content() {
    let device = Device::open();
    let bytes = picture();
    for escaping in ["../outside", "a/b", "", ".."] {
        assert!(
            device.content.keep_in(escaping, APP, &bytes).is_err(),
            "{escaping:?} names no store of this device's"
        );
        assert!(
            device.content.keep_in(WORKSPACE, escaping, &bytes).is_err(),
            "{escaping:?} names no app of this device's"
        );
    }
}
