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
        PresencePayload::Availability { state, call } => PeerPresencePayload::Availability {
            state: state.id().to_owned(),
            call: call.clone(),
        },
    };
    Some(PeerPresence { key, payload })
}

/// The presence a peer sent, as this desktop shows it. `None` for what it
/// does not show (talking beside a pointer is the desktop's, never an
/// app's) and for a frame that does not decode, which is logged with why.
pub fn from_peer(presence: PeerPresence) -> Option<(PresenceKey, PresencePayload)> {
    decode_peer(presence).unwrap_or_else(|reason| {
        tracing::warn!(%reason, "peer presence frame refused");
        None
    })
}

/// [`from_peer`], telling a frame shown to nobody (`Ok(None)`) from one
/// that does not decode.
///
/// # Errors
/// Why the frame does not decode.
pub fn decode_peer(
    presence: PeerPresence,
) -> Result<Option<(PresenceKey, PresencePayload)>, String> {
    let index = |name: &str, value: u64| {
        usize::try_from(value).map_err(|_| format!("{name} {value} is out of range"))
    };
    let key = postcard::from_bytes(&presence.key)
        .map_err(|error| format!("presence key does not decode: {error}"))?;
    let payload = match presence.payload {
        PeerPresencePayload::Pointer { x, y } => PresencePayload::Pointer { x, y },
        PeerPresencePayload::Caret { pos } => PresencePayload::Caret {
            pos: index("caret", pos)?,
        },
        PeerPresencePayload::Selection { start, end } => PresencePayload::Selection {
            start: index("selection start", start)?,
            end: index("selection end", end)?,
        },
        PeerPresencePayload::Typing { start, end } => PresencePayload::Typing {
            start: index("typing start", start)?,
            end: index("typing end", end)?,
        },
        PeerPresencePayload::Arrived => PresencePayload::Arrived,
        PeerPresencePayload::Left => PresencePayload::Left,
        PeerPresencePayload::Following { target } => PresencePayload::Following { target },
        PeerPresencePayload::InteractionJson(json) => PresencePayload::Interaction(Box::new(
            serde_json::from_str(&json)
                .map_err(|error| format!("interaction does not decode: {error}"))?,
        )),
        PeerPresencePayload::Status { text, automatic } => {
            PresencePayload::Status { text, automatic }
        }
        PeerPresencePayload::Availability { state, call } => PresencePayload::Availability {
            state: ui::Availability::from_id(&state)
                .ok_or_else(|| format!("availability {state:?} is unknown"))?,
            call,
        },
        // Talking, greeting and waves beside a pointer are the desktop's to
        // show, never an app's.
        PeerPresencePayload::Chat { .. }
        | PeerPresencePayload::Reaction { .. }
        | PeerPresencePayload::Wave { .. } => return Ok(None),
    };
    Ok(Some((key, payload)))
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
            PresencePayload::Availability {
                state: ui::Availability::Focusing,
                call: Some("voice".into()),
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
        let refused = decode_peer(wire.clone()).unwrap_err();
        assert!(
            refused.contains("presence key does not decode"),
            "{refused}"
        );
        assert!(from_peer(wire).is_none());
    }
}
