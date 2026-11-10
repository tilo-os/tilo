//! The session's records' content: what an app kept outlives the session that kept it, and a device
//! that joins the workspace later reads it from the device that kept it.
//! The two planes here have no transport between them, so the test carries what one plane says to
//! the other as the workspace's hosts carry it: a want where the workspace's members hear it, and
//! an answer to the one device that asked, with the answering device as the host stamps it.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the planes \
              outlive every call made into them"
)]

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iroh::EndpointId;
use neighbour_wire::NeighbourMsg;
use tilo_action_broker::record_content::{ContentSources, ContentState};
use tilo_broker_core::route::CrdtRoute;
use tilo_content::hash_bytes;
use tilo_desktop_model::WorkspaceId as ModelWorkspaceId;
use tilo_network_actor::{RecordContentDelivery, WorkspaceId, workspace_topic_id};

use super::Plane;
use crate::platform::plane::Cmd;

/// The app whose record references the content.
const APP: &str = "so.tilo.notes";
/// The workspace the record is kept in.
const WORKSPACE: &str = "0f2b7c1e-9a4d-4c8e-9d3a-1b5e6f7a8c90";
/// How long the two planes have to hand content over.
const DEADLINE: Duration = Duration::from_secs(10);
/// One turn of the loop that carries what the planes say.
const PASS: Duration = Duration::from_millis(2);

fn route() -> CrdtRoute {
    CrdtRoute::app(APP, WORKSPACE)
}

fn topic() -> WorkspaceId {
    let workspace = ModelWorkspaceId::parse(WORKSPACE).expect("a workspace id");
    workspace_topic_id(workspace)
}

/// One of the workspace's devices: its plane, its address on the network, and what it said, waiting
/// to be carried.
struct Device {
    plane: Arc<Plane>,
    address: EndpointId,
    said: tokio::sync::mpsc::UnboundedReceiver<Cmd>,
}

impl Device {
    /// The plane of a session whose state is `state` , reached on the network as device `seed` .
    fn open(state: Option<&Path>, seed: u8) -> Self {
        let address = iroh::SecretKey::from_bytes(&[seed; 32]).public();
        let plane = Plane::open(state);
        let (said, spoken) = tokio::sync::mpsc::unbounded_channel();
        plane.bind(said, address);
        Self {
            plane,
            address,
            said: spoken,
        }
    }

    /// This device hears that `other` could hand the workspace's content over.
    fn sees(&self, other: &Self) {
        let peer = neighbour_wire::peer_handle(other.address.as_bytes());
        self.plane.sources_changed(
            topic(),
            ContentSources {
                peers: [peer].into_iter().collect(),
                hosts: std::collections::BTreeSet::new(),
            },
        );
    }

    /// Hand what this device said to `other` , as the workspace's hosts do: a message on the
    /// workspace's topic reaches the member, and one for the device that asked reaches it alone,
    /// stamped with the device that sent it.
    fn carry(&mut self, other: &Self) {
        while let Ok(outgoing) = self.said.try_recv() {
            match outgoing {
                Cmd::Publish { ws, msg } => {
                    assert_eq!(ws, topic(), "content on its own workspace's topic");
                    other.plane.heard(ws, WORKSPACE, self.address, &msg);
                }
                Cmd::Content(RecordContentDelivery { ws, target, msg }) => {
                    assert_eq!(ws, topic(), "content on its own workspace's topic");
                    assert!(
                        matches!(
                            msg,
                            NeighbourMsg::RecordContentPiece { .. }
                                | NeighbourMsg::RecordContentLacks { .. }
                        ),
                        "only an answer travels to the device that asked: {msg:?}"
                    );
                    assert_eq!(
                        target,
                        Some(other.address),
                        "the answer names the device that asked"
                    );
                    other.plane.heard(ws, WORKSPACE, self.address, &msg);
                }
                other => panic!("a record's content says nothing else: {other:?}"),
            }
        }
    }
}

/// Run the planes' passes until `done` holds, or the deadline passes: whether `done` held.
fn until(mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + DEADLINE;
    loop {
        if done() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(PASS);
    }
}

/// The bytes as an app stores them: a picture in a note.
fn picture() -> Vec<u8> {
    (0..300_000_u32)
        .map(|index| u8::try_from(index % 251).expect("a byte"))
        .collect()
}

#[test]
fn content_an_app_kept_is_read_back_after_the_session_restarted() {
    let state = tempfile::tempdir().expect("a session's state");
    let bytes = picture();
    let hash = hash_bytes(&bytes);
    {
        let plane = Plane::open(Some(state.path()));
        plane
            .route()
            .offer(&route(), &hash, &bytes)
            .expect("the app's content is kept");
    }
    // The session restarted: nothing of the plane that kept it is left, and the app opens its
    // record again and asks for the content it references.
    let plane = Plane::open(Some(state.path()));
    let content = plane.route();
    let mut news = content.subscribe(&route());
    content.want(&route(), &hash);
    let arrived = news
        .try_recv()
        .expect("this device hands over what it kept at once");
    assert_eq!(arrived.state, ContentState::Resolved);
    assert_eq!(arrived.hash, hash);
    assert_eq!(arrived.bytes, bytes, "the content the record references");
}

#[test]
fn a_device_that_joins_later_reads_content_another_device_kept() {
    let kept = tempfile::tempdir().expect("the keeping device's state");
    let joined = tempfile::tempdir().expect("the joining device's state");
    let bytes = picture();
    let hash = hash_bytes(&bytes);
    // The first device's app stored the content, and that session ended since.
    {
        let plane = Plane::open(Some(kept.path()));
        plane
            .route()
            .offer(&route(), &hash, &bytes)
            .expect("the app's content is kept");
    }
    let mut keeping = Device::open(Some(kept.path()), 0xA1);
    // The second device joins the workspace, and both hear the other there.
    let mut joining = Device::open(Some(joined.path()), 0xB2);
    keeping.sees(&joining);
    joining.sees(&keeping);

    let content = joining.plane.route();
    let mut news = content.subscribe(&route());
    content.want(&route(), &hash);
    let mut heard = Vec::new();
    let handed_over = until(|| {
        keeping.carry(&joining);
        joining.carry(&keeping);
        heard.extend(std::iter::from_fn(|| news.try_recv().ok()));
        heard
            .iter()
            .any(|news| news.state == ContentState::Resolved)
    });
    assert!(
        handed_over,
        "the device that kept the content hands it over: {:?}",
        heard.iter().map(|news| news.state).collect::<Vec<_>>()
    );
    let arrived = heard
        .into_iter()
        .find(|news| news.state == ContentState::Resolved)
        .expect("the content reaches the device that asked");
    assert_eq!(arrived.hash, hash);
    assert_eq!(arrived.bytes, bytes);

    // And the device that asked now keeps it: its next session reads it without anybody else on the
    // workspace.
    drop(joining);
    let plane = Plane::open(Some(joined.path()));
    let content = plane.route();
    let mut news = content.subscribe(&route());
    content.want(&route(), &hash);
    let arrived = news.try_recv().expect("the content this device kept");
    assert_eq!(arrived.state, ContentState::Resolved);
    assert_eq!(arrived.bytes, bytes);
}

#[test]
fn content_a_device_cannot_serve_is_reported_as_such_not_waited_on() {
    let kept = tempfile::tempdir().expect("the keeping device's state");
    let joined = tempfile::tempdir().expect("the joining device's state");
    let mut keeping = Device::open(Some(kept.path()), 0xA1);
    let mut joining = Device::open(Some(joined.path()), 0xB2);
    keeping.sees(&joining);
    joining.sees(&keeping);

    let missing = hash_bytes(b"content nobody on the workspace keeps");
    let content = joining.plane.route();
    let mut news = content.subscribe(&route());
    content.want(&route(), &missing);
    let mut heard = Vec::new();
    let answered = until(|| {
        keeping.carry(&joining);
        joining.carry(&keeping);
        heard.extend(std::iter::from_fn(|| news.try_recv().ok()));
        heard
            .iter()
            .any(|news| news.state == ContentState::Unavailable)
    });
    assert!(
        answered,
        "a want nobody can answer ends: {:?}",
        heard.iter().map(|news| news.state).collect::<Vec<_>>()
    );
}

#[test]
fn a_workspace_or_app_name_that_leaves_the_store_keeps_no_content() {
    let state = tempfile::tempdir().expect("a session's state");
    let plane = Plane::open(Some(state.path()));
    let bytes = picture();
    let hash = hash_bytes(&bytes);
    for escaping in ["../outside", "a/b", "", ".."] {
        let escaped = CrdtRoute::app(APP, escaping);
        assert!(
            plane.route().offer(&escaped, &hash, &bytes).is_err(),
            "{escaping:?} names no store of this device's"
        );
    }
    // And the put door refuses the same names rather than opening an upload it could never finish.
    assert!(plane.puts().begin("../outside").is_err());
    assert!(!Path::new(&state.path().join("tilo").join("record-content")).exists());
}

#[test]
fn what_an_app_uploaded_through_the_put_door_outlives_the_session() {
    let state = tempfile::tempdir().expect("a session's state");
    let first = b"the first half".as_slice();
    let rest = b" of it".as_slice();
    let bytes = [first, rest].concat();
    let hash = {
        let plane = Plane::open(Some(state.path()));
        let mut upload = plane.puts().begin(WORKSPACE).expect("an upload");
        upload.append(first).expect("the first half");
        upload.append(rest).expect("the rest");
        let (hash, len) = upload.finish().expect("the upload is kept");
        assert_eq!(hash, hash_bytes(&bytes));
        assert_eq!(len, bytes.len() as u64, "the length the app was told");
        assert!(plane.puts().holds(WORKSPACE, &hash));
        hash
    };
    let plane = Plane::open(Some(state.path()));
    assert!(
        plane.puts().holds(WORKSPACE, &hash),
        "an upload outlives the session that made it"
    );
}
