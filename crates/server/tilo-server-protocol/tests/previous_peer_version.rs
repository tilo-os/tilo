//! The previous peer version's main flows against this build.
//!
//! A person's servers update one at a time, so for a while some are on this
//! build and some the one before. Two devices work together when their peer
//! versions are at most one apart (`neighbour_wire::works_with_peer_version`),
//! and that holds only if this build reads and writes the frames of the main
//! flows exactly as the version it lets in did: a server holding a workspace
//! (the handshake, the hosted set it announces, a subscription and its
//! answer), sync, a governance change committed to a server, and the identity
//! announcement a workspace's devices start with. The bytes the oldest
//! version this build works with wrote for each are frozen in
//! `previous-peer-frames.txt` beside `neighbour-wire`; this test decodes each
//! with today's types and writes it back, and fails on any byte that differs.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

use std::fmt::Write as _;

use collab_types::WorkspaceId;
use neighbour_wire::{NeighbourMsg, OLDEST_PEER_PROTOCOL_VERSION, PEER_PROTOCOL_VERSION};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tilo_server_protocol::auth::{Hello, HelloAck, decode_handshake};
use tilo_server_protocol::transport::{ALPN, DISCOVERY_ALPN, Envelope, SIBLING_ALPN};

const FROZEN: &str = include_str!("../../../collab/neighbour-wire/previous-peer-frames.txt");

/// The frozen frames, by flow, and the version that wrote them.
fn frozen() -> (u32, Vec<(&'static str, &'static str)>) {
    let mut version = None;
    let mut frames = Vec::new();
    for line in FROZEN
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        let (key, value) = line.split_once(' ').expect("each line is `name value`");
        if key == "version" {
            version = Some(value.parse().unwrap());
        } else {
            frames.push((key, value));
        }
    }
    (version.expect("the file names its version"), frames)
}

fn cert() -> tilo_user_identity::DeviceCert {
    tilo_user_identity::DeviceCert {
        user_public_key: [1; 32],
        device_public_key: [2; 32],
        device_label: "Attic".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: 10,
        expires_at_ns: None,
        signature: [3; 64],
        issuer: None,
    }
}

const WORKSPACE: WorkspaceId = WorkspaceId::from_u128(0x5eed);

fn encode<T: Serialize>(value: &T) -> Vec<u8> {
    postcard::to_stdvec(value).unwrap()
}

/// Each flow's frame as this build writes it, and how this build reads one
/// back and writes it again.
#[expect(
    clippy::type_complexity,
    clippy::too_many_lines,
    reason = "a table of one frame per flow"
)]
fn flows() -> Vec<(&'static str, Vec<u8>, fn(&[u8]) -> Vec<u8>)> {
    fn again<T: Serialize + DeserializeOwned>(bytes: &[u8]) -> Vec<u8> {
        encode(&postcard::from_bytes::<T>(bytes).unwrap())
    }
    fn handshake<T: Serialize + DeserializeOwned>(bytes: &[u8]) -> Vec<u8> {
        encode(&decode_handshake::<T>(bytes).unwrap())
    }
    let mut hello = Hello::new(cert(), [4; 32]);
    hello.protocol = OLDEST_PEER_PROTOCOL_VERSION;
    let ack = HelloAck {
        protocol: OLDEST_PEER_PROTOCOL_VERSION,
        cert: cert(),
        server_nonce: [5; 32],
        sig_over_client_nonce: [6; 64],
    };
    vec![
        // A server holding a workspace: the handshake, the workspaces each
        // server announces, a device's subscription and the server's answer.
        ("hello", encode(&hello), handshake::<Hello>),
        ("hello-ack", encode(&ack), handshake::<HelloAck>),
        (
            "peer-server-hello",
            encode(&Envelope::PeerServerHello {
                hosted_workspaces: vec![WORKSPACE],
            }),
            again::<Envelope>,
        ),
        (
            "subscribe",
            encode(&Envelope::Subscribe {
                workspace_id: WORKSPACE,
            }),
            again::<Envelope>,
        ),
        (
            "ready",
            encode(&Envelope::Ready {
                workspace_id: WORKSPACE,
                pending_deltas: 2,
                documents_when_opened: false,
            }),
            again::<Envelope>,
        ),
        // Sync: a delta fanned out, and anti-entropy's ask and answer.
        (
            "delta",
            encode(&Envelope::Delta {
                workspace_id: WORKSPACE,
                author_device_public_key: [2; 32],
                authored_at_ns: 7,
                delta: vec![0x85, 0x6f, 0x4a, 0x83],
                signature: [8; 64],
            }),
            again::<Envelope>,
        ),
        (
            "sync-request",
            encode(&Envelope::SyncRequest {
                workspace_id: WORKSPACE,
                have_heads: vec![[9; 32]],
            }),
            again::<Envelope>,
        ),
        (
            "sync-response",
            encode(&Envelope::SyncResponse {
                workspace_id: WORKSPACE,
                delta_since: vec![0x85, 0x6f],
            }),
            again::<Envelope>,
        ),
        // A governance change (a lease claim is one too: both are changes to
        // the workspace's document) committed to its server, and the answer.
        (
            "commit",
            encode(&Envelope::Commit {
                workspace_id: WORKSPACE,
                op: [10; 16],
                author_device_public_key: [2; 32],
                authored_at_ns: 11,
                delta: vec![0x85, 0x6f, 0x4a, 0x83],
            }),
            again::<Envelope>,
        ),
        (
            "committed",
            encode(&Envelope::Committed {
                workspace_id: WORKSPACE,
                op: [10; 16],
                seq: 12,
            }),
            again::<Envelope>,
        ),
        // What a workspace's devices start with on a direct link.
        (
            "identity-hello",
            encode(&NeighbourMsg::IdentityHello {
                protocol: OLDEST_PEER_PROTOCOL_VERSION,
                cert: Box::new(cert()),
                revocations: Vec::new(),
            }),
            again::<NeighbourMsg>,
        ),
        (
            "sync-resp",
            encode(&NeighbourMsg::SyncResp {
                changes: vec![vec![0x85, 0x6f]],
                workspace: WORKSPACE.to_string(),
            }),
            again::<NeighbourMsg>,
        ),
    ]
}

/// The window is exactly one version wide, and the frozen frames are the
/// oldest version's this build lets in.
#[test]
fn the_window_is_one_version_and_its_frames_are_frozen() {
    let (version, _) = frozen();
    const {
        assert!(
            OLDEST_PEER_PROTOCOL_VERSION + 1 >= PEER_PROTOCOL_VERSION,
            "this build works with at most the one version before it"
        );
    }
    assert_eq!(
        version, OLDEST_PEER_PROTOCOL_VERSION,
        "previous-peer-frames.txt holds the frames of the oldest version this build works with"
    );
    assert!(neighbour_wire::works_with_peer_version(
        PEER_PROTOCOL_VERSION + 1
    ));
    assert!(!neighbour_wire::works_with_peer_version(
        PEER_PROTOCOL_VERSION + 2
    ));
    assert!(!neighbour_wire::works_with_peer_version(
        OLDEST_PEER_PROTOCOL_VERSION - 1
    ));
}

/// Every frozen frame reads with today's types and is written back byte for
/// byte, and today's build writes each flow's frame as the frozen one: each
/// side reads what the other sends.
#[test]
fn the_previous_versions_main_flows_read_and_write_alike() {
    let (_, frames) = frozen();
    let flows = flows();
    let mut written = String::new();
    for (name, today, _) in &flows {
        writeln!(written, "{name} {}", hex::encode(today)).unwrap();
    }
    for (name, today, read_again) in flows {
        let Some((_, kept)) = frames.iter().find(|(frozen, _)| *frozen == name) else {
            panic!(
                "previous-peer-frames.txt keeps no `{name}` frame; this build writes:\n{written}"
            );
        };
        let kept = hex::decode(kept).unwrap();
        assert_eq!(
            read_again(&kept),
            kept,
            "the previous version's `{name}` frame reads and writes back differently"
        );
        assert_eq!(
            today, kept,
            "this build writes the `{name}` frame differently from the previous version:\n{written}"
        );
    }
}

/// The connection protocols a server answers on keep the names the previous
/// version dials.
#[test]
fn a_server_answers_on_the_protocols_the_previous_version_dials() {
    let (_, frames) = frozen();
    let answered =
        [ALPN, SIBLING_ALPN, DISCOVERY_ALPN].map(|alpn| String::from_utf8(alpn.to_vec()).unwrap());
    let dialled = frames
        .iter()
        .filter(|(name, _)| *name == "alpn")
        .map(|(_, alpn)| *alpn)
        .collect::<Vec<_>>();
    assert!(
        !dialled.is_empty(),
        "previous-peer-frames.txt names the protocols it dials"
    );
    for alpn in dialled {
        assert!(
            answered.iter().any(|answered| answered == alpn),
            "this build no longer answers on {alpn}"
        );
    }
}
