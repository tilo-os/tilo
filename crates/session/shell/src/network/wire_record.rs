//! Room-scoped wire names for records whose local id embeds a path.
//!
//! A toolkit record id such as `file:/home/alice/contracts/draft.odt` (or a
//! record derived from one) is local identity: it must not leave the device.
//! Before such an id is placed in a [`super::NeighbourMsg`], it is replaced
//! by a sealed name: a keyed BLAKE3 hash of the app id and local record id,
//! with the key derived from the room id. The name is stable, so two
//! devices that hold the same record in the same room still meet on one
//! route, and it is not reversible to the path. It is also distinct per room
//! and per app, so the same file opened in two rooms is not linkable across
//! them.
//!
//! A receiver maps a sealed name back to its own local record id through
//! the records its apps have joined or published ([`WireRecordIds`]). A
//! sealed name with no local counterpart names a record nobody on this
//! device has open; it is dropped, because its envelope cannot be bound to
//! a route here.
//!
//! What this does not hide: a room member (or host) who can guess the exact
//! path can confirm the guess, exactly as it already can against the signed
//! envelope's deterministic artifact id. Record ids without a path (`uuid:`,
//! `tab:`, derived from those) travel unchanged, so peers that predate
//! sealing keep exchanging them.

use std::collections::{HashMap, VecDeque};

const SEALED_PREFIX: &str = "sealed:";
const KEY_CONTEXT: &str = "tilo shell wire record id v1";
/// Local routes remembered for resolving inbound sealed names. Joins are
/// per open document, so this comfortably covers a session; the oldest
/// entries are forgotten first.
const MAX_REMEMBERED_RECORDS: usize = 4096;

/// Whether `record_id` carries a local filesystem path anywhere in it.
pub fn embeds_local_path(record_id: &str) -> bool {
    record_id.starts_with("file:") || record_id.contains("@file:")
}

/// Whether `record_id` is a sealed wire name.
pub fn is_sealed(record_id: &str) -> bool {
    record_id.starts_with(SEALED_PREFIX)
}

/// The id to put on the wire for `record_id` in `cluster_id`/`app_id`.
pub fn wire_record_id(cluster_id: &str, app_id: &str, record_id: &str) -> String {
    if !embeds_local_path(record_id) {
        return record_id.to_owned();
    }
    let key = blake3::derive_key(KEY_CONTEXT, cluster_id.as_bytes());
    let mut hasher = blake3::Hasher::new_keyed(&key);
    hasher.update(
        &u64::try_from(app_id.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    hasher.update(app_id.as_bytes());
    hasher.update(record_id.as_bytes());
    format!("{SEALED_PREFIX}{}", hasher.finalize().to_hex())
}

type RouteKey = (String, String, String);

/// Bounded map from sealed wire names back to the local record ids this
/// device's apps use for them.
#[derive(Default)]
pub struct WireRecordIds {
    by_wire: HashMap<RouteKey, String>,
    order: VecDeque<RouteKey>,
}

impl WireRecordIds {
    /// Remember a local route and return its wire id.
    pub fn register(&mut self, cluster_id: &str, app_id: &str, record_id: &str) -> String {
        let wire = wire_record_id(cluster_id, app_id, record_id);
        self.remember(cluster_id, app_id, &wire, record_id);
        wire
    }

    fn remember(&mut self, cluster_id: &str, app_id: &str, wire: &str, record_id: &str) {
        let key = (cluster_id.to_owned(), app_id.to_owned(), wire.to_owned());
        if self
            .by_wire
            .insert(key.clone(), record_id.to_owned())
            .is_none()
        {
            self.order.push_back(key);
            while self.order.len() > MAX_REMEMBERED_RECORDS {
                if let Some(oldest) = self.order.pop_front() {
                    self.by_wire.remove(&oldest);
                }
            }
        }
    }

    /// Resolve only a route a local app explicitly joined or published.
    /// Public ids are not implicitly observed merely because they can be
    /// copied from an inbound frame.
    pub fn resolve_registered(&self, cluster_id: &str, app_id: &str, wire: &str) -> Option<String> {
        self.by_wire
            .get(&(cluster_id.to_owned(), app_id.to_owned(), wire.to_owned()))
            .cloned()
    }

    /// The single explicitly registered route whose digest and canonical
    /// channel match a sealed live header. This lookup is read-only: an
    /// untrusted frame cannot register a route or request channel access.
    pub fn resolve_live_header(
        &self,
        cluster_id: &str,
        sealed: &tilo_relay_protocol::live::SealedLiveFrame,
    ) -> Option<(tilo_relay_protocol::live::LiveRecord, String)> {
        let mut matches = self
            .by_wire
            .iter()
            .filter_map(|((room, app, wire), local)| {
                if room != cluster_id {
                    return None;
                }
                let record = tilo_relay_protocol::live::LiveRecord {
                    app: app.clone(),
                    record: wire.clone(),
                };
                (record.digest() == sealed.record
                    && collab_types::document_channel_id(app, cluster_id, wire)
                        == sealed.channel_id())
                .then(|| (record, local.clone()))
            });
        let only = matches.next()?;
        matches.next().is_none().then_some(only)
    }

    /// The local record id for an inbound wire id. Unsealed ids are local
    /// ids already; a sealed id resolves only to a record this device has
    /// joined or published.
    pub fn resolve(&self, cluster_id: &str, app_id: &str, wire: &str) -> Option<String> {
        if !is_sealed(wire) {
            return Some(wire.to_owned());
        }
        self.by_wire
            .get(&(cluster_id.to_owned(), app_id.to_owned(), wire.to_owned()))
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOM: &str = "3f2a0f5e-7c1b-4d4e-9a55-0d6f1c2b3a4e";

    #[test]
    fn wire_ids_are_sealed_per_room_and_routes_are_bounded() {
        // Phase `path_bearing_ids_are_sealed_stably_and_per_room`.
        {
            let local = "file:/home/alice/Secret Plans/merger.odt";
            let wire = wire_record_id(ROOM, "so.tilo.writer", local);
            assert!(is_sealed(&wire));
            assert!(!wire.contains("alice") && !wire.contains('/'));
            assert_eq!(wire, wire_record_id(ROOM, "so.tilo.writer", local));
            assert_ne!(
                wire,
                wire_record_id("another-room", "so.tilo.writer", local),
                "the same file is not linkable across rooms"
            );
            assert_ne!(wire, wire_record_id(ROOM, "so.tilo.notes", local));
            let derived = "derived:annotations@file:/home/alice/book.pdf";
            assert!(is_sealed(&wire_record_id(ROOM, "so.tilo.pdf", derived)));
        }
        // Phase `ids_without_paths_travel_unchanged`.
        for id in [
            "",
            "uuid:1c2f",
            "tab:000000000000002a",
            "derived:notes@uuid:9",
        ] {
            assert_eq!(wire_record_id(ROOM, "app", id), id);
        }
        // Phase `a_registered_route_resolves_and_an_unknown_sealed_id_does_not`.
        {
            let mut ids = WireRecordIds::default();
            let local = "file:/work/report.odt";
            let wire = ids.register(ROOM, "app", local);
            assert_eq!(ids.resolve(ROOM, "app", &wire).as_deref(), Some(local));
            assert_eq!(ids.resolve("other-room", "app", &wire), None);
            assert_eq!(ids.resolve(ROOM, "other-app", &wire), None);
            assert_eq!(
                ids.resolve(ROOM, "app", "uuid:7").as_deref(),
                Some("uuid:7")
            );
            assert_eq!(ids.resolve_registered(ROOM, "app", "uuid:7"), None);
            ids.register(ROOM, "app", "uuid:7");
            assert_eq!(
                ids.resolve_registered(ROOM, "app", "uuid:7").as_deref(),
                Some("uuid:7")
            );
            ids.register(ROOM, "app", "");
            assert_eq!(ids.resolve_registered(ROOM, "app", "").as_deref(), Some(""));
            let unknown = wire_record_id(ROOM, "app", "file:/elsewhere");
            assert_eq!(ids.resolve(ROOM, "app", &unknown), None);
        }
        // Phase `remembered_routes_are_bounded`.
        {
            let mut ids = WireRecordIds::default();
            let first = ids.register(ROOM, "app", "file:/0");
            for index in 1..=MAX_REMEMBERED_RECORDS {
                ids.register(ROOM, "app", &format!("file:/{index}"));
            }
            assert_eq!(ids.by_wire.len(), MAX_REMEMBERED_RECORDS);
            assert_eq!(ids.resolve(ROOM, "app", &first), None, "oldest forgotten");
        }
    }

    #[test]
    fn live_header_resolves_only_one_explicit_exact_record() {
        let mut ids = WireRecordIds::default();
        ids.register(ROOM, "so.tilo.chat", "");
        let interaction = tilo_actions::SemanticInteraction {
            version: tilo_actions::INTERACTION_VERSION,
            invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(7)),
            sequence: 1,
            actor: tilo_actions::InteractionActor {
                id: "agent:scribe".into(),
                kind: tilo_actions::InteractionActorKind::Agent,
            },
            phase: tilo_actions::InteractionPhase::Applying,
            action: tilo_actions::InteractionAction {
                id: "message".into(),
                label: "Message".into(),
                app: Some(tilo_actions::ActionApp {
                    id: "so.tilo.chat".into(),
                    label: "Chat".into(),
                }),
            },
            resource: tilo_actions::ActionResource {
                id: "chat.person:scribe".into(),
                kind: "chat.person".into(),
                label: "Scribe".into(),
            },
            anchor: tilo_actions::InteractionAnchor::Resource,
            group: None,
        };
        let frame = tilo_relay_protocol::live::LiveFrame::Ops(
            tilo_relay_protocol::live::OpsFrame::Semantic {
                record: tilo_relay_protocol::live::LiveRecord {
                    app: "so.tilo.chat".into(),
                    record: String::new(),
                },
                event: tilo_relay_protocol::live::SemanticFrame {
                    version: 1,
                    seq: 1,
                    sent_at_ms: 1,
                    execution: interaction.invocation.0,
                    peer: 9,
                    interaction_json: serde_json::to_string(&interaction).unwrap(),
                },
            },
        );
        let channel = collab_types::document_channel_id("so.tilo.chat", ROOM, "");
        let sealed =
            tilo_relay_protocol::live::SealedLiveFrame::seal(&frame, |_, retention, bytes| {
                Ok::<_, String>((
                    collab_types::EncryptionMetadata::content_key(1, channel, retention),
                    bytes.to_vec(),
                ))
            })
            .unwrap();
        let sealed = tilo_relay_protocol::live::SealedLiveFrame::decode(&sealed).unwrap();
        assert_eq!(
            ids.resolve_live_header(ROOM, &sealed),
            Some((
                tilo_relay_protocol::live::LiveRecord {
                    app: "so.tilo.chat".into(),
                    record: String::new(),
                },
                String::new(),
            ))
        );
        assert_eq!(ids.resolve_live_header("other-room", &sealed), None);
        let mut wrong_channel = sealed.clone();
        wrong_channel.channel = uuid::Uuid::new_v4().into_bytes();
        assert_eq!(ids.resolve_live_header(ROOM, &wrong_channel), None);

        ids.register(ROOM, "so.tilo.chat", "duplicate-local-alias");
        assert_eq!(
            ids.resolve_live_header(ROOM, &sealed).unwrap().1,
            "",
            "a different record cannot match the exact empty-root digest and channel"
        );
    }
}
