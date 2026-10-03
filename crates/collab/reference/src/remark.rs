//! [`Remark`] — something said *about* a resource — and its passage into the
//! real chat document.
//!
//! A remark is not a second kind of message: it is encoded as the chat app's own
//! `SendMessage`, under chat's own `type_id`, so it lands in the same document,
//! channel and history as anything typed in the chat window. What it adds is two
//! fields beside the body — `reference` and `delivery`.
//!
//! Those fields ride a JSON payload that carries no `deny_unknown_fields`, so a
//! chat build that has not adopted them decodes the message and ignores them,
//! while a consumer that wants the address reads it straight out with
//! [`read_chat_action`] — machine-readable without chat having to change first.
//!
//! [`Remark::to_chat_action`] fails closed: chat's `SendMessage` inserts a row
//! into a channel and has no private delivery, and encoding a
//! [`Delivery::Private`] as one would put a message the author marked private in
//! front of the whole room.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use tilo_actions::EncodedAction;

use crate::delivery::{Delivery, Destination, Id, MentionRef, deliver};
use crate::direct::DirectRoom;
use crate::reference::ResourceRef;

/// The registry `type_id` chat registers its action enum under. Matching it is
/// what makes a remark an ordinary chat message rather than a parallel one.
pub const CHAT_ACTION_TYPE: &str = "chat.action";

/// The channel every room starts with — the room's conversation.
///
/// An agreement between a writer that is not chat and the document chat owns: an
/// app composing a remark cannot enumerate chat's channels, which live in chat's
/// document in another process, so without a name it could address nothing. It
/// is also the weakest link here — a room whose conversation was deleted has no
/// such channel, and a remark addressed to it lands where nobody is reading — and
/// the fix, resolving the conversation from the room, needs somewhere to ask.
pub const ROOM_CONVERSATION: &str = "c_general";

/// The payload key carrying the resource address; writer and reader must agree
/// on it, and a chat build adopts it when it grows a chip of its own.
const REFERENCE_KEY: &str = "reference";
/// The payload key carrying the routing decision.
const DELIVERY_KEY: &str = "delivery";

/// Something said about a resource.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remark {
    pub text: String,
    /// Not optional: a remark without a reference is just a message, and belongs
    /// in the chat window.
    pub reference: ResourceRef,
    pub mentions: Vec<MentionRef>,
    /// Where the author said it should go, as shown by the selector.
    pub destination: Destination,
}

impl Remark {
    pub fn new(text: impl Into<String>, reference: ResourceRef, destination: Destination) -> Self {
        Self {
            text: text.into(),
            reference,
            mentions: Vec::new(),
            destination,
        }
    }

    #[must_use]
    pub fn with_mentions(mut self, mentions: Vec<MentionRef>) -> Self {
        self.mentions = mentions;
        self
    }

    /// The destination the author chose, refined by who the body pings.
    pub fn delivery(&self) -> Delivery {
        deliver(&self.destination, &self.mentions)
    }

    /// Encode as the chat app's own `SendMessage` action.
    ///
    /// `id`, `author` and `ts` are minted by the caller at authoring time, as
    /// every Tilo action requires, so the change replays byte-identically on every
    /// peer. A delivery the action cannot express is [`Undeliverable`] rather
    /// than downgraded — see the module docs.
    ///
    /// # Errors
    ///
    /// Returns [`Undeliverable::PrivateNeedsDirectTransport`] if this
    /// remark's delivery resolves to [`Delivery::Private`] — chat's
    /// `SendMessage` action has no channel to encode that into; use
    /// [`to_direct_action`](Self::to_direct_action) instead.
    pub fn to_chat_action(
        &self,
        id: impl Into<Id>,
        author: impl Into<Id>,
        ts: i64,
    ) -> Result<EncodedAction, Undeliverable> {
        let delivery = self.delivery();
        let channel = match &delivery {
            Delivery::Channel { channel } | Delivery::DirectedInChannel { channel, .. } => {
                channel.clone()
            }
            Delivery::Private { to } => {
                return Err(Undeliverable::PrivateNeedsDirectTransport { to: to.clone() });
            }
        };
        Ok(self.encode_into(&channel, &delivery, id, author, ts))
    }

    /// Encode a **private** remark as an ordinary message in the two-member room
    /// the pair share — the direct transport
    /// [`Undeliverable::PrivateNeedsDirectTransport`] asks for.
    ///
    /// This answers that refusal rather than weakening it: what differs is *which
    /// document the row lands in*, `room`'s conversation being a channel only its
    /// two members are in. The caller must route the action to `room` —
    /// [`DirectRoom::room_id`] is the cluster it belongs to, and the composing
    /// room's own connection would post it in the wrong room.
    ///
    /// Fails closed on a delivery that is not private (the caller holds two
    /// answers about where this goes), on a room that is not `(author, to)`'s,
    /// and on an author who is not a member of the room they write into.
    ///
    /// # Errors
    ///
    /// Returns [`Undeliverable::NotPrivate`] if this remark's delivery
    /// isn't [`Delivery::Private`], or [`Undeliverable::NotThisPairsRoom`]
    /// if `room` isn't the `(author, to)` pair's shared room.
    pub fn to_direct_action(
        &self,
        room: &DirectRoom,
        id: impl Into<Id>,
        author: impl Into<Id>,
        ts: i64,
    ) -> Result<EncodedAction, Undeliverable> {
        let delivery = self.delivery();
        let Delivery::Private { to } = &delivery else {
            return Err(Undeliverable::NotPrivate { delivery });
        };
        let author = author.into();
        // The recipient must be the author's counterpart, not merely a member.
        // Checking both ends separately admits `to == author`, which writes a
        // remark addressed to nobody but its author into a room the other member
        // reads — a disclosure, not a misfile.
        if room.other_than(&author) != Some(to.as_str()) {
            return Err(Undeliverable::NotThisPairsRoom {
                to: to.clone(),
                room: room.room_id(),
            });
        }
        Ok(self.encode_into(ROOM_CONVERSATION, &delivery, id, author, ts))
    }

    /// Shared by both encoders, so a remark is the same message however it was
    /// routed and there is one place the payload can drift from chat's struct.
    #[allow(
        clippy::expect_used,
        reason = "ResourceRef/Delivery/MentionRef/the payload map are all plain derive(Serialize) \
                  data with no floats or non-string keys, so encoding cannot actually fail here"
    )]
    fn encode_into(
        &self,
        channel: &str,
        delivery: &Delivery,
        id: impl Into<Id>,
        author: impl Into<Id>,
        ts: i64,
    ) -> EncodedAction {
        // Every field chat's `SendMessage` declares, including the empty ones, so
        // the payload decodes without relying on any `#[serde(default)]`.
        let mut fields = Map::new();
        fields.insert("channel".into(), json!(channel));
        fields.insert("id".into(), json!(id.into()));
        fields.insert("author".into(), json!(author.into()));
        fields.insert("ts".into(), json!(ts));
        fields.insert("text".into(), json!(self.text));
        fields.insert("parent".into(), Value::Null);
        fields.insert("attachment".into(), Value::Null);
        // The two fields that make it a remark.
        fields.insert(
            REFERENCE_KEY.into(),
            serde_json::to_value(&self.reference).expect("ResourceRef is serializable"),
        );
        fields.insert(
            DELIVERY_KEY.into(),
            serde_json::to_value(delivery).expect("Delivery is serializable"),
        );
        if !self.mentions.is_empty() {
            fields.insert(
                "mentions".into(),
                serde_json::to_value(&self.mentions).expect("MentionRef is serializable"),
            );
        }

        // Externally tagged, matching serde's default for chat's action enum.
        let payload = json!({ "SendMessage": Value::Object(fields) });
        EncodedAction {
            type_id: CHAT_ACTION_TYPE.to_string(),
            payload: serde_json::to_vec(&payload).expect("payload is serializable"),
        }
    }
}

/// Why a remark could not be encoded as a chat message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Undeliverable {
    /// The remark is private, and chat's `SendMessage` only inserts rows into
    /// channels. Refused rather than posted: a routing layer that "helpfully"
    /// widens a private message is a disclosure bug.
    PrivateNeedsDirectTransport { to: Id },
    /// A direct room was supplied for a delivery that is not private. A channel
    /// delivery names its own channel, so the caller holds two answers about
    /// where this is going; encoding either would contradict the selector.
    NotPrivate { delivery: Delivery },
    /// The remark is private to somebody the supplied room is not shared with,
    /// or is authored by somebody who is not in it — a room whose members are
    /// not `(author, to)` has at least one reader who was never addressed.
    NotThisPairsRoom { to: Id, room: String },
}

impl std::fmt::Display for Undeliverable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Undeliverable::PrivateNeedsDirectTransport { to } => write!(
                f,
                "remark is private to `{to}` and would have to be posted in a channel to send"
            ),
            Undeliverable::NotPrivate { delivery } => write!(
                f,
                "remark is not private ({delivery:?}) — it names a channel, so it needs no direct room"
            ),
            Undeliverable::NotThisPairsRoom { to, room } => write!(
                f,
                "remark is private to `{to}`, which is not who room `{room}` is shared with"
            ),
        }
    }
}

impl std::error::Error for Undeliverable {}

/// What a consumer recovers from a sent message: what was said, what it was said
/// about, and where it went — each a typed field, none recovered from prose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceivedRemark {
    pub text: String,
    pub reference: ResourceRef,
    pub delivery: Delivery,
    pub mentions: Vec<MentionRef>,
    pub author: Id,
    pub ts: i64,
}

/// Recover `(text, resource address, delivery)` from a sent chat action.
///
/// The test of the whole idea: if this ever has to look at
/// [`ReceivedRemark::text`] to find out what "this" meant, the reference has
/// degraded back into prose. `None` when the action is not a chat `SendMessage`,
/// or carries no reference.
pub fn read_chat_action(encoded: &EncodedAction) -> Option<ReceivedRemark> {
    if encoded.type_id != CHAT_ACTION_TYPE {
        return None;
    }
    let value: Value = serde_json::from_slice(&encoded.payload).ok()?;
    let fields = value.get("SendMessage")?.as_object()?;

    // A message with no reference is an ordinary chat message, not a remark.
    let reference: ResourceRef = serde_json::from_value(fields.get(REFERENCE_KEY)?.clone()).ok()?;
    let delivery: Delivery = serde_json::from_value(fields.get(DELIVERY_KEY)?.clone()).ok()?;
    let mentions: Vec<MentionRef> = fields
        .get("mentions")
        .and_then(|m| serde_json::from_value(m.clone()).ok())
        .unwrap_or_default();

    Some(ReceivedRemark {
        text: fields.get("text")?.as_str()?.to_string(),
        reference,
        delivery,
        mentions,
        author: fields.get("author")?.as_str()?.to_string(),
        ts: fields.get("ts")?.as_i64()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reference::Span;

    fn run_sheet() -> ResourceRef {
        ResourceRef::ranged(
            "word_processor.block",
            "word-processor.block:blk_7",
            "¶3 of Run sheet",
            Span::chars(12, 41),
        )
    }

    fn general() -> Destination {
        Destination::channel("c_general", "general")
    }

    /// A channel remark is written as chat's own `SendMessage` and read back
    /// structurally; a private one refuses to become a channel row; nothing
    /// that is not a remark is read as one.
    #[test]
    fn a_channel_remark_round_trips_through_chats_send_message() {
        // Phase: a consumer recovers text, address and recipient without
        // parsing prose. The label is deliberately misleading: a consumer that
        // reads it instead of the address gets the wrong answer.
        let mut reference = run_sheet();
        reference.label = "something else entirely".into();
        let remark = Remark::new("the times contradict the prose", reference, general());

        let encoded = remark.to_chat_action("m_1", "u_me", 1_700_000_000).unwrap();
        let got = read_chat_action(&encoded).expect("a remark is recoverable");

        assert_eq!(
            got.reference.resource_id().as_str(),
            "word-processor.block:blk_7"
        );
        assert_eq!(got.reference.kind, "word_processor.block");
        assert_eq!(got.reference.span, Some(Span::chars(12, 41)));
        assert_eq!(got.text, "the times contradict the prose");
        assert!(
            !got.text.contains("blk_7"),
            "the address must not be smuggled through the body"
        );
        assert_eq!(
            got.delivery,
            Delivery::Channel {
                channel: "c_general".into()
            }
        );
        assert_eq!(got.author, "u_me");

        // Phase: a remark is encoded as chat's own send message.
        let encoded = Remark::new("look at this", run_sheet(), general())
            .to_chat_action("m_1", "u_me", 42)
            .unwrap();
        assert_eq!(encoded.type_id, "chat.action");

        let v: Value = serde_json::from_slice(&encoded.payload).unwrap();
        let f = v
            .get("SendMessage")
            .expect("externally tagged variant")
            .as_object()
            .unwrap();
        for key in [
            "channel",
            "id",
            "author",
            "ts",
            "text",
            "parent",
            "attachment",
        ] {
            assert!(
                f.contains_key(key),
                "chat's `{key}` field must be written explicitly"
            );
        }
        assert_eq!(f["channel"], json!("c_general"));
        assert_eq!(f["parent"], Value::Null);

        // Phase: a mention rides along and the channel still receives it.
        let remark = Remark::new("@Ada thoughts?", run_sheet(), general())
            .with_mentions(vec![MentionRef::new("u_ada", 0, 4, false)]);
        let encoded = remark.to_chat_action("m_1", "u_me", 42).unwrap();
        let got = read_chat_action(&encoded).unwrap();

        assert_eq!(
            got.delivery,
            Delivery::DirectedInChannel {
                channel: "c_general".into(),
                to: vec!["u_ada".into()]
            }
        );
        assert_eq!(got.mentions, vec![MentionRef::new("u_ada", 0, 4, false)]);
        let v: Value = serde_json::from_slice(&encoded.payload).unwrap();
        assert_eq!(v["SendMessage"]["channel"], json!("c_general"));

        // Phase: a private remark refuses to become a channel row.
        let remark = Remark::new(
            "just for you",
            run_sheet(),
            Destination::participant("u_ada", "Ada", false),
        );
        assert_eq!(
            remark.to_chat_action("m_1", "u_me", 42),
            Err(Undeliverable::PrivateNeedsDirectTransport { to: "u_ada".into() }),
            "a private delivery must fail closed, never widen to the channel"
        );

        // Phase: a plain chat message is not read as a remark.
        let payload = json!({"SendMessage": {
            "channel": "c_general", "id": "m_2", "author": "u_me",
            "ts": 42, "text": "morning", "parent": null, "attachment": null
        }});
        let encoded = EncodedAction {
            type_id: CHAT_ACTION_TYPE.into(),
            payload: serde_json::to_vec(&payload).unwrap(),
        };
        assert_eq!(read_chat_action(&encoded), None);

        // Phase: another app's action is not read as a remark.
        let encoded = EncodedAction {
            type_id: "word-processor.action".into(),
            payload: b"{}".to_vec(),
        };
        assert_eq!(read_chat_action(&encoded), None);
    }

    const ADA: &str = "u_1f4c3a9b2d7e5061";
    const BOB: &str = "u_9a0b1c2d3e4f5061";
    const CAROL: &str = "u_cccccccccccccccc";

    fn pair() -> DirectRoom {
        DirectRoom::between(ADA, BOB).expect("two people name a room")
    }

    /// A private remark becomes an ordinary chat message in the pair's own
    /// room, written by either member, and stays private even with mentions.
    #[test]
    fn a_private_remark_travels_in_the_pairs_direct_room() {
        // Phase: a private remark becomes a message in the two-member room.
        let remark = Remark::new(
            "just for you",
            run_sheet(),
            Destination::participant(BOB, "Bob", false),
        );
        // The channel encoder is unchanged and still refusing, asserted beside the
        // direct path so an edit that loosened it fails here too.
        assert_eq!(
            remark.to_chat_action("m_1", ADA, 42),
            Err(Undeliverable::PrivateNeedsDirectTransport { to: BOB.into() }),
        );

        let encoded = remark
            .to_direct_action(&pair(), "m_1", ADA, 42)
            .expect("the pair's own room can carry it");
        let got = read_chat_action(&encoded).expect("it is an ordinary chat message");

        assert_eq!(encoded.type_id, CHAT_ACTION_TYPE);
        let v: Value = serde_json::from_slice(&encoded.payload).unwrap();
        assert_eq!(v["SendMessage"]["channel"], json!(ROOM_CONVERSATION));
        assert_eq!(got.author, ADA);
        assert_eq!(got.text, "just for you");
        // A row that forgot it was private would be indistinguishable from a
        // channel post to anything deciding where it may be shown.
        assert_eq!(got.delivery, Delivery::Private { to: BOB.into() });
        assert_eq!(got.delivery.addressed(), [BOB.to_string()]);

        // Phase: either member writes into the same room.
        let to_bob = Remark::new(
            "a",
            run_sheet(),
            Destination::participant(BOB, "Bob", false),
        )
        .to_direct_action(&pair(), "m_1", ADA, 1)
        .expect("Ada writes to Bob");
        let to_ada = Remark::new(
            "b",
            run_sheet(),
            Destination::participant(ADA, "Ada", false),
        )
        .to_direct_action(&pair(), "m_2", BOB, 2)
        .expect("Bob writes to Ada");
        for encoded in [&to_bob, &to_ada] {
            let v: Value = serde_json::from_slice(&encoded.payload).unwrap();
            assert_eq!(v["SendMessage"]["channel"], json!(ROOM_CONVERSATION));
        }

        // Phase: a mention in a private remark does not widen it — the
        // invariant `deliver` upholds, which this path must not undo.
        let remark = Remark::new(
            "@Ada thoughts?",
            run_sheet(),
            Destination::participant(BOB, "Bob", false),
        )
        .with_mentions(vec![MentionRef::new(ADA, 0, 4, false)]);
        let encoded = remark.to_direct_action(&pair(), "m_1", ADA, 42).unwrap();
        let got = read_chat_action(&encoded).unwrap();
        assert_eq!(got.delivery, Delivery::Private { to: BOB.into() });
        assert!(
            got.delivery.is_private(),
            "a mention must not widen a private remark"
        );
    }

    /// Direct delivery refuses every room/author/remark combination that would
    /// put a message where its recipient is not, or redirect a channel remark.
    #[test]
    fn direct_delivery_refuses_what_it_cannot_keep_between_the_pair() {
        // Case: a room that is not the pair's refuses to carry their message.
        let wrong = DirectRoom::between(ADA, CAROL).unwrap();
        let remark = Remark::new(
            "just for you",
            run_sheet(),
            Destination::participant(BOB, "Bob", false),
        );
        assert_eq!(
            remark.to_direct_action(&wrong, "m_1", ADA, 42),
            Err(Undeliverable::NotThisPairsRoom {
                to: BOB.into(),
                room: wrong.room_id(),
            }),
            "a private message must never be written into a room its recipient is not in"
        );

        // Case: an author outside the room cannot write into it.
        let remark = Remark::new(
            "just for you",
            run_sheet(),
            Destination::participant(BOB, "Bob", false),
        );
        assert!(matches!(
            remark.to_direct_action(&pair(), "m_1", CAROL, 42),
            Err(Undeliverable::NotThisPairsRoom { .. })
        ));

        // Case: a remark addressed to its own author is refused.
        // Both ends are in the room, so a membership check passes each of them
        // separately. Writing it would put a remark addressed to nobody but Ada
        // into the room Bob reads.
        let remark = Remark::new(
            "a note to myself",
            run_sheet(),
            Destination::participant(ADA, "Ada", false),
        );
        assert!(matches!(
            remark.to_direct_action(&pair(), "m_1", ADA, 42),
            Err(Undeliverable::NotThisPairsRoom { .. })
        ));

        // Case: a channel remark handed a room is refused rather than redirected.
        let remark = Remark::new("look at this", run_sheet(), general());
        assert_eq!(
            remark.to_direct_action(&pair(), "m_1", ADA, 42),
            Err(Undeliverable::NotPrivate {
                delivery: Delivery::Channel {
                    channel: "c_general".into()
                }
            }),
        );
    }
}
