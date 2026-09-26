//! Test fixtures shared by the unit tests.

use std::time::SystemTime;

use collab_types::{
    ArtifactId, AttestationId, DocumentMutation, Ed25519PublicKey, OpClass, SignedOp,
};

use crate::{PresenceChange, PresenceUpdate, Route};

pub fn route(app: &str) -> Route {
    Route::new(app, "room-1", "")
}

fn op(payload: Vec<u8>, seq: u64) -> SignedOp {
    SignedOp::new_unsigned(
        payload,
        ArtifactId(uuid::Uuid::from_u128(7)),
        Ed25519PublicKey::from_bytes([3; 32]),
        AttestationId(uuid::Uuid::from_u128(9)),
        OpClass::Splice,
        SystemTime::UNIX_EPOCH,
        seq,
    )
}

pub fn incremental(seq: u64) -> SignedOp {
    op(
        DocumentMutation::Incremental(vec![1, 2, u8::try_from(seq % 256).unwrap_or(0)])
            .encode()
            .unwrap(),
        seq,
    )
}

pub fn snapshot(seq: u64) -> SignedOp {
    op(
        DocumentMutation::Snapshot(b"automerge snapshot".to_vec())
            .encode()
            .unwrap(),
        seq,
    )
}

/// A snapshot large enough to travel as `segments` signed segments.
pub fn snapshot_segments(segments: usize) -> Vec<SignedOp> {
    let len = collab_types::document_transfer::DOCUMENT_SEGMENT_BYTES
        .saturating_mul(segments.saturating_sub(1))
        .saturating_add(17);
    DocumentMutation::Snapshot(vec![5; len])
        .encode_frames()
        .unwrap()
        .into_iter()
        .zip(1_u64..)
        .map(|(payload, seq)| op(payload, seq))
        .collect()
}

pub fn presence(participant: &str, change: PresenceChange) -> PresenceUpdate {
    PresenceUpdate {
        participant: participant.into(),
        room: "room-1".into(),
        scope: vec![1, 0, 3],
        change,
    }
}

/// One invocation's first and failed lifecycle updates.
pub fn interaction() -> (
    tilo_actions::SemanticInteraction,
    tilo_actions::SemanticInteraction,
) {
    let started = tilo_actions::SemanticInteraction {
        version: tilo_actions::INTERACTION_VERSION,
        invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(42)),
        sequence: 1,
        actor: tilo_actions::InteractionActor {
            id: "alice".into(),
            kind: tilo_actions::InteractionActorKind::Person,
        },
        phase: tilo_actions::InteractionPhase::Applying,
        action: tilo_actions::InteractionAction {
            id: "send".into(),
            label: "Send".into(),
            app: None,
        },
        resource: tilo_actions::ActionResource {
            id: "mail.draft:1".into(),
            kind: "mail.draft".into(),
            label: "Draft".into(),
            sensitivity: tilo_actions::Sensitivity::Public,
        },
        anchor: tilo_actions::InteractionAnchor::Resource,
        group: None,
    };
    let mut failed = started.clone();
    failed.sequence = 2;
    failed.phase = tilo_actions::InteractionPhase::Failed {
        code: None,
        message: "Couldn't send".into(),
    };
    (started, failed)
}

/// An Inspect answer with one personal text field.
pub fn inspect_reply() -> tilo_reference::inspect::InspectReply {
    tilo_reference::inspect::InspectReply {
        target: Some("t:to".into()),
        nodes: vec![tilo_reference::inspect::InspectedNode {
            id: "t:to".into(),
            identity: tilo_reference::inspect::Identity::Durable,
            role: "text_input".into(),
            label: Some("To".into()),
            value: Some("Ada Lovelace".into()),
            sensitivity: tilo_actions::Sensitivity::Personal,
            content: tilo_reference::inspect::Content::Value,
            bounds: None,
            visible: None,
            pixels: tilo_reference::inspect::Pixels::Described,
            state: tilo_reference::inspect::NodeState {
                focused: true,
                disabled: false,
            },
            children: Vec::new(),
            reference: None,
        }],
        ..tilo_reference::inspect::InspectReply::empty()
    }
}
