//! The agent integration harness is compiled with `shell` as a normal dependency.
//! These checks keep its fixture certificate authority attached in that exact graph.

use ui::collaboration::CollabRequest;

use crate::typed_peer;

const APP: &str = "dev.tilo.word-processor";

fn hello(handle: &str) -> CollabRequest {
    CollabRequest::Hello {
        app_id: APP.into(),
        cluster_id: typed_peer::room(),
        record: None,
        record_kind: None,
        handle: Some(handle.into()),
    }
}

#[test]
fn harness_admits_an_explicitly_enrolled_certified_device() {
    let dir = tempfile::tempdir().expect("temporary harness directory");
    let socket = dir.path().join("enrolled.sock");
    let _broker = typed_peer::spawn_with_agent_actions(&socket, &[]);
    let mut peer = typed_peer::TypedPeer::connect(&socket);

    peer.announce(&socket, &hello("quinten"))
        .expect("the fixture authority admits its explicitly enrolled device");
}

#[test]
fn harness_rejects_a_certified_but_unlisted_device() {
    let dir = tempfile::tempdir().expect("temporary harness directory");
    let socket = dir.path().join("unlisted.sock");
    let _broker = typed_peer::spawn_with_agent_actions(&socket, &[]);
    let mut peer = typed_peer::TypedPeer::connect(&socket);

    let error = peer
        .announce(&socket, &hello("unlisted-device"))
        .expect_err("a root-signed certificate is insufficient without fixture enrollment");
    let detail = format!("{error:#}");
    assert!(
        detail.contains("not enrolled"),
        "the rejection must name enrollment authority, not relax or lose it: {detail}"
    );
}
