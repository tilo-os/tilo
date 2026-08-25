//! [`Remark`] — something said *about* a resource — and its passage into the
//! real chat document.
//!
//! ## One messaging system
//!
//! A remark is not a second kind of message. It is encoded as the chat app's
//! own `SendMessage` action, under chat's own `type_id`, so it lands in the
//! same document, the same channel and the same history as anything typed in
//! the chat window. What a remark adds is two fields beside the body:
//! `reference` (the address of the thing being talked about) and `delivery`
//! (where it goes).
//!
//! Those fields ride the action payload, which is JSON
//! ([`EncodedAction::encode`](qos_actions::EncodedAction::encode) serialises
//! with `serde_json`) and carries no `deny_unknown_fields`. So a chat build
//! that has not yet adopted them decodes the message and ignores them, while a
//! consumer that wants the address — an agent, a search index, a chip that
//! wants to be followed — reads it straight out with [`read_chat_action`]. The
//! reference is machine-readable from the first day, without the chat app
//! having to change to make it so.
//!
//! ## Fail closed
//!
//! [`Remark::to_chat_action`] will not encode a delivery the chat action cannot
//! faithfully represent. Chat's `SendMessage` inserts a row into a channel; it
//! has no private delivery. Encoding a [`Delivery::Private`] as a channel row
//! would put a message the author marked private in front of the whole room —
//! a disclosure, produced by the code that was supposed to route it. So that
//! case is [`Undeliverable`] and nothing is sent. See
//! [`Undeliverable::PrivateNeedsDirectTransport`].

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use qos_actions::EncodedAction;

use crate::delivery::{Delivery, Destination, Id, MentionRef, deliver};
use crate::direct::DirectRoom;
use crate::reference::ResourceRef;

/// The registry `type_id` chat registers its action enum under. Matching it is
/// what makes a remark an ordinary chat message rather than a parallel one.
pub const CHAT_ACTION_TYPE: &str = "chat.action";

/// The channel every room starts with — the room's conversation.
///
/// Named here for the same reason [`CHAT_ACTION_TYPE`] is: it is an agreement
/// between a writer that is not chat and the document chat owns. An app
/// composing a remark has no way to enumerate chat's channels — they live in
/// chat's document, in another process — so without a name it could address
/// nothing at all, and the "room" line in every destination selector would be
/// an option that silently went nowhere.
///
/// This is the id the chat app seeds into a fresh room's document for its
/// starter `general` channel, and it is the weakest link in this path: a room
/// whose conversation has been renamed still has this channel, but a room where
/// it has been deleted does not, and a remark addressed to it lands in a
/// channel nobody is reading. Resolving the room's conversation from the room
/// rather than by convention is the fix, and it needs somewhere for an app to
/// ask — which is not this crate.
pub const ROOM_CONVERSATION: &str = "c_general";

/// The payload key carrying the resource address. Named here because both the
/// writer and the reader must agree on it, and because it is the field a chat
/// build adopts when it grows a reference chip of its own.
const REFERENCE_KEY: &str = "reference";
/// The payload key carrying the routing decision.
const DELIVERY_KEY: &str = "delivery";

/// Something said about a resource: the body, the address of what it is about,
/// who is pinged, and where it goes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remark {
    pub text: String,
    /// What this remark is about. Not optional — a remark without a reference
    /// is just a message, and belongs in the chat window.
    pub reference: ResourceRef,
    pub mentions: Vec<MentionRef>,
    /// Where the author said it should go, as shown by the selector.
    pub destination: Destination,
}

impl Remark {
    pub fn new(
        text: impl Into<String>,
        reference: ResourceRef,
        destination: Destination,
    ) -> Self {
        Self {
            text: text.into(),
            reference,
            mentions: Vec::new(),
            destination,
        }
    }

    pub fn with_mentions(mut self, mentions: Vec<MentionRef>) -> Self {
        self.mentions = mentions;
        self
    }

    /// How this remark is actually delivered — the destination the author chose,
    /// refined by who the body pings.
    pub fn delivery(&self) -> Delivery {
        deliver(&self.destination, &self.mentions)
    }

    /// Encode as the chat app's own `SendMessage` action.
    ///
    /// `id`, `author` and `ts` are minted by the caller at authoring time, as
    /// every qOS action requires, so the change replays byte-identically on
    /// every peer.
    ///
    /// Returns [`Undeliverable`] rather than downgrading a delivery the action
    /// cannot express — see the module docs.
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

    /// Encode a **private** remark as an ordinary message in the two-member
    /// room the pair share — the direct transport
    /// [`Undeliverable::PrivateNeedsDirectTransport`] says this needs.
    ///
    /// This does not weaken that refusal; it supplies what the refusal was
    /// asking for. [`Self::to_chat_action`] still refuses every private
    /// delivery, because in the room it is called from — the room the remark
    /// was composed in — a channel row genuinely is readable by everybody. The
    /// difference here is *which document the row lands in*. `room` is a room
    /// with two members ([`DirectRoom`]), so its conversation is a channel only
    /// those two are in, and a row there discloses the message to exactly the
    /// person it was addressed to.
    ///
    /// The caller is responsible for putting the returned action on the route
    /// that reaches `room` — `DirectRoom::room_id()` is the cluster it belongs
    /// to. Handing it to the composing room's own connection would post it in
    /// the wrong room, which is the failure this whole path exists to prevent,
    /// so it is worth saying plainly: the room id is not decoration.
    ///
    /// Fails closed, three ways:
    ///
    /// * a delivery that is not private has a channel of its own and does not
    ///   need a room — being handed one means the caller has two ideas about
    ///   where this is going, and encoding either of them would make the
    ///   destination selector a liar;
    /// * a room that is not the room for `(author, to)` — the one case that
    ///   would deliver a private message to somebody it was not addressed to;
    /// * an author who is not a member of the room they are writing into.
    pub fn to_direct_action(
        &self,
        room: &DirectRoom,
        id: impl Into<Id>,
        author: impl Into<Id>,
        ts: i64,
    ) -> Result<EncodedAction, Undeliverable> {
        let delivery = self.delivery();
        let Delivery::Private { to } = &delivery else {
            return Err(Undeliverable::NotPrivate {
                delivery: delivery.clone(),
            });
        };
        let author = author.into();
        // Both ends checked, not just the recipient. An author outside the room
        // is writing into a conversation they are not part of, and the room
        // would carry a message from somebody neither member can account for.
        if !room.contains(to) || !room.contains(&author) {
            return Err(Undeliverable::NotThisPairsRoom {
                to: to.clone(),
                room: room.room_id(),
            });
        }
        Ok(self.encode_into(ROOM_CONVERSATION, &delivery, id, author, ts))
    }

    /// Write the action. Shared by both encoders so a remark is the same
    /// message however it was routed — one payload shape, one place it can
    /// drift from chat's struct.
    fn encode_into(
        &self,
        channel: &str,
        delivery: &Delivery,
        id: impl Into<Id>,
        author: impl Into<Id>,
        ts: i64,
    ) -> EncodedAction {
        // Every field chat's `SendMessage` declares is written explicitly,
        // including the ones we have nothing to say about, so the payload
        // decodes against chat's struct without relying on `#[serde(default)]`
        // being present on any of them.
        let mut fields = Map::new();
        fields.insert("channel".into(), json!(channel));
        fields.insert("id".into(), json!(id.into()));
        fields.insert("author".into(), json!(author.into()));
        fields.insert("ts".into(), json!(ts));
        fields.insert("text".into(), json!(self.text));
        fields.insert("parent".into(), Value::Null);
        fields.insert("attachment".into(), Value::Null);
        // The two fields that make it a remark. A chat build that does not know
        // them ignores them; a consumer that wants the address reads them.
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

        // Externally tagged, matching serde's default representation of chat's
        // action enum.
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
    /// channels. Refused rather than posted to the channel: the author marked
    /// it private, and a routing layer that "helpfully" widens that is a
    /// disclosure bug. A direct transport is what this needs.
    PrivateNeedsDirectTransport { to: Id },
    /// A direct room was supplied for a delivery that is not private.
    ///
    /// Refused rather than resolved either way round. A channel delivery names
    /// its own channel, so the room is a second answer to a question that
    /// already had one, and a caller holding two answers has a bug the
    /// destination selector cannot show. Encoding the channel would ignore the
    /// room; encoding the room would move a message the selector said was going
    /// to a channel.
    NotPrivate { delivery: Delivery },
    /// The remark is private to somebody the supplied room is not shared with,
    /// or is authored by somebody who is not in it.
    ///
    /// The one refusal that stops a private message reaching the wrong person:
    /// a room whose two members are not `(author, to)` is a room where at least
    /// one reader was never addressed.
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

/// What a consumer recovers from a sent message.
///
/// The three things an instruction has to carry: what was said, what it was
/// said about, and where it went. Every one of them is a typed field — none is
/// recovered by reading prose.
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
/// **This is the function the whole idea rests on.** An agent handed one of
/// these knows exactly which block or which file is being talked about, because
/// it reads an address out of a field — not a description out of a sentence. If
/// this ever needs to look at [`ReceivedRemark::text`] to find out what "this"
/// meant, the reference has degraded back into a string and the guessing is
/// back.
///
/// `None` when the action is not a chat `SendMessage`, or is an ordinary
/// message carrying no reference.
pub fn read_chat_action(encoded: &EncodedAction) -> Option<ReceivedRemark> {
    if encoded.type_id != CHAT_ACTION_TYPE {
        return None;
    }
    let value: Value = serde_json::from_slice(&encoded.payload).ok()?;
    let fields = value.get("SendMessage")?.as_object()?;

    // A message with no reference is an ordinary chat message, not a remark.
    let reference: ResourceRef =
        serde_json::from_value(fields.get(REFERENCE_KEY)?.clone()).ok()?;
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

    /// The test that protects the idea: a consumer recovers the address, the
    /// body and the routing as typed values, without parsing prose. The label
    /// is deliberately misleading here — a consumer that reads it instead of
    /// the address gets the wrong answer, so this fails if the reference ever
    /// degrades into text.
    #[test]
    fn a_consumer_recovers_text_address_and_recipient_without_parsing_prose() {
        let mut reference = run_sheet();
        reference.label = "something else entirely".into();
        let remark = Remark::new("the times contradict the prose", reference, general());

        let encoded = remark.to_chat_action("m_1", "u_me", 1_700_000_000).unwrap();
        let got = read_chat_action(&encoded).expect("a remark is recoverable");

        // The address — as an id, not a rendering.
        assert_eq!(got.reference.resource_id().as_str(), "word-processor.block:blk_7");
        assert_eq!(got.reference.kind, "word_processor.block");
        assert_eq!(got.reference.span, Some(Span::chars(12, 41)));
        // The body, verbatim and free of any encoded address.
        assert_eq!(got.text, "the times contradict the prose");
        assert!(
            !got.text.contains("blk_7"),
            "the address must not be smuggled through the body"
        );
        // The recipient.
        assert_eq!(got.delivery, Delivery::Channel { channel: "c_general".into() });
        assert_eq!(got.author, "u_me");
    }

    /// It is an ordinary chat message: chat's own action type, chat's own
    /// externally-tagged `SendMessage`, every field chat declares present.
    #[test]
    fn a_remark_is_encoded_as_chats_own_send_message() {
        let encoded = Remark::new("look at this", run_sheet(), general())
            .to_chat_action("m_1", "u_me", 42)
            .unwrap();
        assert_eq!(encoded.type_id, "chat.action");

        let v: Value = serde_json::from_slice(&encoded.payload).unwrap();
        let f = v.get("SendMessage").expect("externally tagged variant").as_object().unwrap();
        for key in ["channel", "id", "author", "ts", "text", "parent", "attachment"] {
            assert!(f.contains_key(key), "chat's `{key}` field must be written explicitly");
        }
        assert_eq!(f["channel"], json!("c_general"));
        assert_eq!(f["parent"], Value::Null);
    }

    /// A private remark must not be quietly posted to the room.
    #[test]
    fn a_private_remark_refuses_to_become_a_channel_row() {
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
    }

    /// A mention travels as a ping and the message stays in the channel — the
    /// selector said `general`, and `general` is where it goes.
    #[test]
    fn a_mention_rides_along_and_the_channel_still_receives_it() {
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
    }

    /// An ordinary chat message is not a remark, and must not be mistaken for
    /// one carrying an empty reference.
    #[test]
    fn a_plain_chat_message_is_not_read_as_a_remark() {
        let payload = json!({"SendMessage": {
            "channel": "c_general", "id": "m_2", "author": "u_me",
            "ts": 42, "text": "morning", "parent": null, "attachment": null
        }});
        let encoded = EncodedAction {
            type_id: CHAT_ACTION_TYPE.into(),
            payload: serde_json::to_vec(&payload).unwrap(),
        };
        assert_eq!(read_chat_action(&encoded), None);
    }

    const ADA: &str = "u_1f4c3a9b2d7e5061";
    const BOB: &str = "u_9a0b1c2d3e4f5061";
    const CAROL: &str = "u_cccccccccccccccc";

    fn pair() -> DirectRoom {
        DirectRoom::between(ADA, BOB).expect("two people name a room")
    }

    /// The refusal is answered, not weakened: the same private remark that
    /// `to_chat_action` will not encode goes through once there is a room only
    /// the two of them are in — and it goes into that room's conversation,
    /// which is the whole difference.
    #[test]
    fn a_private_remark_becomes_a_message_in_the_two_member_room() {
        let remark = Remark::new(
            "just for you",
            run_sheet(),
            Destination::participant(BOB, "Bob", false),
        );
        // Unchanged, and still refusing. Asserted here rather than only in the
        // test above so the two paths sit side by side: if an edit ever makes
        // the channel encoder start accepting private deliveries, this fails
        // alongside that one.
        assert_eq!(
            remark.to_chat_action("m_1", ADA, 42),
            Err(Undeliverable::PrivateNeedsDirectTransport { to: BOB.into() }),
        );

        let encoded = remark
            .to_direct_action(&pair(), "m_1", ADA, 42)
            .expect("the pair's own room can carry it");
        let got = read_chat_action(&encoded).expect("it is an ordinary chat message");

        // An ordinary chat message, under chat's own type, in the room's
        // conversation — not a second kind of message with a second reader.
        assert_eq!(encoded.type_id, CHAT_ACTION_TYPE);
        let v: Value = serde_json::from_slice(&encoded.payload).unwrap();
        assert_eq!(v["SendMessage"]["channel"], json!(ROOM_CONVERSATION));
        assert_eq!(got.author, ADA);
        assert_eq!(got.text, "just for you");
        // It still says it is private, and still says who to. A row that
        // forgot would be indistinguishable from a channel post the next time
        // anything had to decide where it may be shown.
        assert_eq!(got.delivery, Delivery::Private { to: BOB.into() });
        assert_eq!(got.delivery.addressed(), [BOB.to_string()]);
    }

    /// Addressed by participant id, and the room is the pair's — so whichever
    /// of them writes, the message lands in the same conversation.
    #[test]
    fn either_member_writes_into_the_same_room() {
        let to_bob = Remark::new("a", run_sheet(), Destination::participant(BOB, "Bob", false))
            .to_direct_action(&pair(), "m_1", ADA, 1)
            .expect("Ada writes to Bob");
        let to_ada = Remark::new("b", run_sheet(), Destination::participant(ADA, "Ada", false))
            .to_direct_action(&pair(), "m_2", BOB, 2)
            .expect("Bob writes to Ada");
        for encoded in [&to_bob, &to_ada] {
            let v: Value = serde_json::from_slice(&encoded.payload).unwrap();
            assert_eq!(v["SendMessage"]["channel"], json!(ROOM_CONVERSATION));
        }
    }

    /// The refusal that stops a private message reaching somebody it was not
    /// addressed to. A room shared with Carol cannot carry a remark private to
    /// Bob, however the caller came to be holding both.
    #[test]
    fn a_room_that_is_not_the_pairs_refuses_to_carry_their_message() {
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
    }

    /// An author outside the room is writing into a conversation they are not
    /// part of; both ends are checked, not only the recipient.
    #[test]
    fn an_author_outside_the_room_cannot_write_into_it() {
        let remark = Remark::new(
            "just for you",
            run_sheet(),
            Destination::participant(BOB, "Bob", false),
        );
        assert!(matches!(
            remark.to_direct_action(&pair(), "m_1", CAROL, 42),
            Err(Undeliverable::NotThisPairsRoom { .. })
        ));
    }

    /// A channel delivery names its own channel. Handed a room as well, the
    /// caller holds two answers, and silently picking one would move a message
    /// the selector said was going somewhere else.
    #[test]
    fn a_channel_remark_handed_a_room_is_refused_rather_than_redirected() {
        let remark = Remark::new("look at this", run_sheet(), general());
        assert_eq!(
            remark.to_direct_action(&pair(), "m_1", ADA, 42),
            Err(Undeliverable::NotPrivate {
                delivery: Delivery::Channel { channel: "c_general".into() }
            }),
        );
    }

    /// A mention inside a private remark still only pings — it must not widen
    /// the message back out to a channel. That is the invariant `deliver`
    /// upholds, and this path must not undo it.
    #[test]
    fn a_mention_in_a_private_remark_does_not_widen_it() {
        let remark = Remark::new(
            "@Ada thoughts?",
            run_sheet(),
            Destination::participant(BOB, "Bob", false),
        )
        .with_mentions(vec![MentionRef::new(ADA, 0, 4, false)]);
        let encoded = remark.to_direct_action(&pair(), "m_1", ADA, 42).unwrap();
        let got = read_chat_action(&encoded).unwrap();
        assert_eq!(got.delivery, Delivery::Private { to: BOB.into() });
        assert!(got.delivery.is_private(), "a mention must not widen a private remark");
    }

    #[test]
    fn another_apps_action_is_not_read_as_a_remark() {
        let encoded = EncodedAction {
            type_id: "word-processor.action".into(),
            payload: b"{}".to_vec(),
        };
        assert_eq!(read_chat_action(&encoded), None);
    }
}
