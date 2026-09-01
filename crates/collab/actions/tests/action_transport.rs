//! An action crossing a real boundary.
//!
//! [`ActionTransport`] had exactly one implementation, and it pushed envelopes
//! into a `Vec` that nothing ever drained — so "replicate" and "submit to a
//! host" were shapes in the type system with no bytes behind them. These tests
//! run the same [`dispatch`] over a genuine `AF_UNIX` socket pair, with a
//! second thread standing in for the peer or host at the far end, and check
//! that what comes out the other side reconstructs the *identical* document.
//!
//! A socket pair rather than an in-memory `Vec<u8>` on purpose: the interesting
//! failures here — a half-closed stream, a reply that never comes, a decision
//! for the wrong envelope — do not exist in a buffer.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration-test setup and assertions"
)]

use std::io::{BufReader, Write};
use std::os::unix::net::UnixStream;
use std::thread;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, ScalarValue};
use serde::{Deserialize, Serialize};

use qos_actions::{
    Action, ActionError, ActionRegistry, Dispatched, Doc, EncodedAction, ExecutionLocus,
    IoTransport, WireMessage, dispatch, read_message, replay,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
enum Queue {
    Push { value: String },
    Set { key: String, value: i64 },
}

const QUEUE_TYPE: &str = "test.queue";

impl Action for Queue {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            Queue::Push { value } => {
                let list = match doc.get(&ROOT, "queue")? {
                    Some((automerge::Value::Object(ObjType::List), id)) => id,
                    _ => doc.put_object(&ROOT, "queue", ObjType::List)?,
                };
                let len = doc.length(&list);
                doc.insert(&list, len, ScalarValue::Str(value.as_str().into()))?;
            }
            Queue::Set { key, value } => {
                doc.put(&ROOT, key, ScalarValue::Int(*value))?;
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        match self {
            // A shared queue wants a host to serialize it.
            Queue::Push { .. } => ExecutionLocus::ServerPreferred,
            Queue::Set { .. } => ExecutionLocus::ClientLocal,
        }
    }
}

fn registry() -> ActionRegistry {
    let mut r = ActionRegistry::new();
    r.register::<Queue>(QUEUE_TYPE);
    r
}

/// Two replicas sharing history, so a replayed change has identical deps.
fn two_replicas() -> (AutoCommit, AutoCommit) {
    let mut a = AutoCommit::new();
    a.put(&ROOT, "seed", ScalarValue::Str("s".into())).unwrap();
    a.commit();
    let b = AutoCommit::load(&a.save()).unwrap();
    (a, b)
}

/// Build a transport over one end of a socket pair, returning the far end for
/// whoever is playing peer/host.
fn wired() -> (IoTransport<UnixStream, BufReader<UnixStream>>, UnixStream) {
    let (near, far) = UnixStream::pair().expect("socket pair");
    let reader = BufReader::new(near.try_clone().expect("clone for reading"));
    (IoTransport::new(near, reader), far)
}

/// A `ClientLocal` action is applied here and its envelope goes down the wire;
/// the far end replays it and lands on the byte-identical document — same head
/// hashes, not merely the same values.
#[test]
fn a_replicated_envelope_crosses_the_socket_and_replays_identically() {
    let (mut origin, mut peer) = two_replicas();
    assert_eq!(origin.get_heads(), peer.get_heads(), "same base state");

    let (mut transport, far) = wired();
    let action = Queue::Set {
        key: "score".into(),
        value: 42,
    };

    let d = dispatch(
        &mut origin,
        &action,
        EncodedAction::encode(QUEUE_TYPE, &action).unwrap(),
        &mut transport,
    )
    .unwrap();
    assert!(matches!(d, Dispatched::AppliedLocal(_)));
    drop(transport); // close the write half so the reader sees EOF

    // The far end — a different reader, holding only bytes.
    let mut reader = BufReader::new(far);
    let msg = read_message(&mut reader)
        .expect("read")
        .expect("one message arrived");
    let WireMessage::Replicate(env) = msg else {
        panic!("expected a replicate, got {msg:?}");
    };

    replay(&mut peer, &env, &registry()).unwrap();
    assert_eq!(
        origin.get_heads(),
        peer.get_heads(),
        "the far end reconstructed the identical change from the wire alone"
    );
    assert_eq!(
        peer.get(&ROOT, "score").unwrap().unwrap().0.to_i64(),
        Some(42)
    );
}

/// A `ServerPreferred` action is offered to a host over the socket. The host
/// accepts, so the client does **not** apply it; the host executes it and its
/// document carries the change.
#[test]
fn a_host_across_the_socket_accepts_and_executes_authoritatively() {
    let (mut client_doc, mut host_doc) = two_replicas();
    let before = client_doc.get_heads();

    let (mut transport, far) = wired();

    // The host lives at the far end: read the submission, apply it, answer.
    let host = thread::spawn(move || {
        let mut reader = BufReader::new(far.try_clone().unwrap());
        let mut writer = far;
        let msg = read_message(&mut reader).unwrap().unwrap();
        let WireMessage::Submit(env) = msg else {
            panic!("expected a submit, got {msg:?}");
        };
        // Authoritative execution is the ordinary replay path — a host is not a
        // second implementation of apply.
        replay(&mut host_doc, &env, &registry()).unwrap();
        writer
            .write_all(
                WireMessage::Decision {
                    id: env.id,
                    accepted: true,
                }
                .to_line()
                .as_bytes(),
            )
            .unwrap();
        writer.flush().unwrap();
        host_doc
    });

    let action = Queue::Push {
        value: "clip-1".into(),
    };
    let d = dispatch(
        &mut client_doc,
        &action,
        EncodedAction::encode(QUEUE_TYPE, &action).unwrap(),
        &mut transport,
    )
    .unwrap();

    assert!(
        matches!(d, Dispatched::SubmittedToHost(_)),
        "the host's acceptance came back across the socket"
    );
    assert!(!d.mutated_local());
    assert_eq!(
        client_doc.get_heads(),
        before,
        "a host-accepted action is not also applied locally"
    );

    let host_doc = host.join().unwrap();
    let (_, list) = host_doc.get(&ROOT, "queue").unwrap().unwrap();
    assert_eq!(host_doc.length(&list), 1, "the host executed it");
}

/// When the host declines, the client applies locally instead — the user is
/// never blocked by a host that will not take the action.
#[test]
fn a_declining_host_falls_back_to_a_local_apply() {
    let (mut client_doc, _) = two_replicas();
    let (mut transport, far) = wired();

    let host = thread::spawn(move || {
        let mut reader = BufReader::new(far.try_clone().unwrap());
        let mut writer = far;
        let msg = read_message(&mut reader).unwrap().unwrap();
        let WireMessage::Submit(env) = msg else {
            panic!("expected a submit");
        };
        writer
            .write_all(
                WireMessage::Decision {
                    id: env.id,
                    accepted: false,
                }
                .to_line()
                .as_bytes(),
            )
            .unwrap();
        writer.flush().unwrap();
    });

    let action = Queue::Push {
        value: "clip-2".into(),
    };
    let d = dispatch(
        &mut client_doc,
        &action,
        EncodedAction::encode(QUEUE_TYPE, &action).unwrap(),
        &mut transport,
    )
    .unwrap();
    host.join().unwrap();

    assert!(matches!(d, Dispatched::AppliedLocalFallback(_)));
    let (_, list) = client_doc.get(&ROOT, "queue").unwrap().unwrap();
    assert_eq!(client_doc.length(&list), 1, "the action still happened");
}

/// A decision that names a *different* envelope is not an acceptance. Without
/// the id check, a stale or misrouted reply would silently convince the client
/// that a host had taken an action nobody executed — the action would vanish.
#[test]
fn a_decision_for_another_envelope_is_not_an_acceptance() {
    let (mut client_doc, _) = two_replicas();
    let (mut transport, far) = wired();

    let host = thread::spawn(move || {
        let mut reader = BufReader::new(far.try_clone().unwrap());
        let mut writer = far;
        let _ = read_message(&mut reader).unwrap().unwrap();
        writer
            .write_all(
                WireMessage::Decision {
                    // Not the id that was submitted.
                    id: qos_actions::ActionId::new(),
                    accepted: true,
                }
                .to_line()
                .as_bytes(),
            )
            .unwrap();
        writer.flush().unwrap();
    });

    let action = Queue::Push {
        value: "clip-3".into(),
    };
    let d = dispatch(
        &mut client_doc,
        &action,
        EncodedAction::encode(QUEUE_TYPE, &action).unwrap(),
        &mut transport,
    )
    .unwrap();
    host.join().unwrap();

    assert!(
        matches!(d, Dispatched::AppliedLocalFallback(_)),
        "a mismatched decision must not be read as acceptance"
    );
    let (_, list) = client_doc.get(&ROOT, "queue").unwrap().unwrap();
    assert_eq!(client_doc.length(&list), 1, "the action was not lost");
}

/// A dead wire degrades to a local apply rather than dropping the action, and
/// says why. Losing a user's edit because a socket closed is the one outcome
/// that must not be possible.
#[test]
fn a_broken_wire_falls_back_locally_and_records_the_error() {
    let (mut client_doc, _) = two_replicas();
    let (mut transport, far) = wired();
    drop(far); // the far end is gone before anything is sent

    let action = Queue::Push {
        value: "clip-4".into(),
    };
    let d = dispatch(
        &mut client_doc,
        &action,
        EncodedAction::encode(QUEUE_TYPE, &action).unwrap(),
        &mut transport,
    )
    .unwrap();

    assert!(matches!(d, Dispatched::AppliedLocalFallback(_)));
    let (_, list) = client_doc.get(&ROOT, "queue").unwrap().unwrap();
    assert_eq!(client_doc.length(&list), 1, "the edit survived");
    assert!(
        transport.last_error.is_some(),
        "a failed write is reported, not silently successful"
    );
}

/// The framing is newline-delimited JSON, one message per line — the same
/// framing the compositor's control socket speaks, so a host that multiplexes
/// both needs no second parser.
#[test]
fn messages_are_one_json_object_per_line() {
    let (mut doc, _) = two_replicas();
    let (mut transport, far) = wired();

    for i in 0..3 {
        let action = Queue::Set {
            key: format!("k{i}"),
            value: i,
        };
        dispatch(
            &mut doc,
            &action,
            EncodedAction::encode(QUEUE_TYPE, &action).unwrap(),
            &mut transport,
        )
        .unwrap();
    }
    assert_eq!(transport.sent.len(), 3, "each dispatch wrote once");
    drop(transport);

    let mut reader = BufReader::new(far);
    let mut seen = 0;
    while let Some(msg) = read_message(&mut reader).unwrap() {
        assert!(matches!(msg, WireMessage::Replicate(_)));
        seen += 1;
    }
    assert_eq!(seen, 3, "three lines, three messages, clean EOF");
}
