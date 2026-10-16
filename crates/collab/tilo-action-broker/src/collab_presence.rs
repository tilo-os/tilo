//! Semantic conversion at the app-service / peer-transport boundary.
//!
//! The peer wire owns no UI request enums. Only opaque key bytes and the
//! transport-neutral presence payload cross hosts; this module checks the
//! architecture-sized positions when translating to and from the local model.

use collab_types::{PeerPresence, PeerPresencePayload};
use ui::collaboration::PresencePayload;
use ui::presence::PresenceKey;

pub fn to_peer(key: &PresenceKey, payload: &PresencePayload) -> Option<PeerPresence> {
    let key = postcard::to_stdvec(key).ok()?;
    let payload = match payload {
        PresencePayload::Pointer { x, y } => PeerPresencePayload::Pointer { x: *x, y: *y },
        PresencePayload::Caret { pos } => PeerPresencePayload::Caret {
            pos: u64::try_from(*pos).ok()?,
        },
        PresencePayload::Selection { start, end } => PeerPresencePayload::Selection {
            start: u64::try_from(*start).ok()?,
            end: u64::try_from(*end).ok()?,
        },
        PresencePayload::Typing { start, end } => PeerPresencePayload::Typing {
            start: u64::try_from(*start).ok()?,
            end: u64::try_from(*end).ok()?,
        },
        PresencePayload::Arrived => PeerPresencePayload::Arrived,
        PresencePayload::Left => PeerPresencePayload::Left,
        PresencePayload::Following { target } => PeerPresencePayload::Following {
            target: target.clone(),
        },
        PresencePayload::Interaction(update) => {
            PeerPresencePayload::InteractionJson(serde_json::to_string(update).ok()?)
        }
        PresencePayload::Status { text, automatic } => PeerPresencePayload::Status {
            text: text.clone(),
            automatic: *automatic,
        },
    };
    Some(PeerPresence { key, payload })
}

pub fn from_peer(presence: PeerPresence) -> Option<(PresenceKey, PresencePayload)> {
    let key = postcard::from_bytes(&presence.key).ok()?;
    let payload = match presence.payload {
        PeerPresencePayload::Pointer { x, y } => PresencePayload::Pointer { x, y },
        PeerPresencePayload::Caret { pos } => PresencePayload::Caret {
            pos: usize::try_from(pos).ok()?,
        },
        PeerPresencePayload::Selection { start, end } => PresencePayload::Selection {
            start: usize::try_from(start).ok()?,
            end: usize::try_from(end).ok()?,
        },
        PeerPresencePayload::Typing { start, end } => PresencePayload::Typing {
            start: usize::try_from(start).ok()?,
            end: usize::try_from(end).ok()?,
        },
        PeerPresencePayload::Arrived => PresencePayload::Arrived,
        PeerPresencePayload::Left => PresencePayload::Left,
        PeerPresencePayload::Following { target } => PresencePayload::Following { target },
        PeerPresencePayload::InteractionJson(json) => {
            PresencePayload::Interaction(Box::new(serde_json::from_str(&json).ok()?))
        }
        PeerPresencePayload::Status { text, automatic } => {
            PresencePayload::Status { text, automatic }
        }
    };
    Some((key, payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_local_variants_survive_the_typed_peer_boundary() {
        let key = PresenceKey::surface_pointer("editor/document-7");
        let variants = vec![
            PresencePayload::Pointer { x: 1.25, y: 9.5 },
            PresencePayload::Caret { pos: 17 },
            PresencePayload::Selection { start: 4, end: 11 },
            PresencePayload::Typing { start: 6, end: 8 },
            PresencePayload::Arrived,
            PresencePayload::Left,
            PresencePayload::Following {
                target: Some("peer@7".into()),
            },
            PresencePayload::Following { target: None },
            PresencePayload::Interaction(Box::new(tilo_actions::SemanticInteraction {
                version: tilo_actions::INTERACTION_VERSION,
                invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(7)),
                sequence: 2,
                actor: tilo_actions::InteractionActor {
                    id: "quill".into(),
                    kind: tilo_actions::ActorKind::Agent,
                    for_person: None,
                },
                phase: tilo_actions::InteractionPhase::Committed,
                action: tilo_actions::InteractionAction {
                    id: "rename".into(),
                    label: "Rename".into(),
                    app: None,
                },
                resource: tilo_actions::ActionResource {
                    id: "file:draft".into(),
                    kind: "files.entry".into(),
                    label: "Draft.md".into(),
                },
                anchor: tilo_actions::InteractionAnchor::ListItems {
                    ids: vec!["file:draft".into()],
                },
                group: None,
            })),
            PresencePayload::Status {
                text: "Reviewing Q2 assumptions".into(),
                automatic: true,
            },
        ];
        for payload in variants {
            let wire = to_peer(&key, &payload).expect("encode typed presence");
            let bytes = wire.to_bytes().expect("canonical peer bytes");
            let decoded = PeerPresence::from_bytes(&bytes).expect("decode canonical peer bytes");
            assert_eq!(from_peer(decoded), Some((key.clone(), payload)));
        }
    }

    #[test]
    fn malformed_key_bytes_fail_closed() {
        let wire = PeerPresence {
            key: vec![0xff, 0xff],
            payload: PeerPresencePayload::Arrived,
        };
        assert!(from_peer(wire).is_none());
    }
}
