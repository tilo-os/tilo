//! What one route's endpoint can seal and open.
//!
//! Every collaboration route of a certified app is an encrypted channel.
//! The endpoint asks the workspace identity service for the route's content
//! keys when it joins and keeps asking until they arrive; until then the
//! route is *waiting*: nothing the app writes leaves the endpoint in the
//! clear, and ciphertext that arrives is held rather than shown as garbage
//! or dropped. When the keys arrive the held edits are sealed and sent and
//! the held ciphertext is opened, in order.
//!
//! Once a route holds keys it asks again only when told its channel changed
//! (`channelKeysChanged`), when ciphertext arrives that its keys can't open,
//! or when the broker refuses an edit sealed under them; see
//! `broker::KeySchedule`.
//!
//! Only an endpoint with no identity service at all (an explicit test
//! harness) stays plaintext, and only until a group is installed on it.

use std::collections::VecDeque;

use collab_types::inbound::{InboundHold, OpKey};
use collab_types::{ArtifactId, ContentRetention, EncryptionMetadata, SignedOp};
use runtime_collab::OpAdmission;
use zeroize::Zeroizing;

use super::ChannelAccessState;

/// Most edits held while waiting for access, and most bytes.
const MAX_HELD_OUTBOUND: usize = 4096;
const MAX_HELD_OUTBOUND_BYTES: usize = 64 * 1024 * 1024;
/// Most ops held while waiting for keys or a verdict; past it the oldest is
/// owed by history.
const MAX_HELD_INBOUND: usize = 4096;

pub(super) enum Keys {
    /// No identity service: plaintext.
    Plain,
    /// Channel keys requested and not held.
    Pending(ChannelAccessState),
    /// A group installed explicitly on this endpoint.
    Group(Box<channel_crypto::ChannelGroup>),
    /// Content keys from the identity service, at a service generation.
    Content {
        keys: channel_crypto::ChannelContentKeys,
        generation: u64,
    },
}

/// Outcome of sealing one frame.
pub(super) enum Sealed {
    /// Send this payload with this metadata.
    Ready(Vec<u8>, Option<EncryptionMetadata>),
    /// Hold the frame until keys arrive.
    Hold,
    /// The frame cannot be sent.
    Refused(String),
}

/// Why an inbound envelope did not open.
#[derive(Debug)]
pub(super) enum Unopened {
    /// No key for it yet: hold and retry when keys change.
    Later,
    /// It never will.
    Never(Never),
}

/// Why an inbound envelope never opens on this route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Never {
    /// Sealed for another channel than this route's.
    WrongChannel,
    /// Its ciphertext is malformed, or the route's keys reject it.
    Undecryptable,
    /// Cleartext the broker says must be sealed: a broker that fanned out
    /// what its own gate refuses.
    Downgrade,
    /// Sealed, on a route that is cleartext.
    SealedOnPlain,
}

impl std::fmt::Display for Never {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::WrongChannel => "sealed for another channel",
            Self::Undecryptable => "ciphertext the route's keys reject",
            Self::Downgrade => "cleartext the broker says must be sealed",
            Self::SealedOnPlain => "sealed on a cleartext route",
        })
    }
}

/// A frame waiting for keys, with the claims its op will carry.
pub(super) type HeldFrame = (Vec<u8>, Vec<collab_types::ChangeClaim>);

/// An inbound op as the broker delivered it, with its verdict.
#[derive(Debug, Clone)]
pub(super) struct Arrived {
    pub(super) envelope: SignedOp,
    pub(super) from_user_id: String,
    pub(super) admission: OpAdmission,
}

/// A route's keys plus what waits on them.
pub(super) struct RouteKeys {
    pub(super) keys: Keys,
    held_outbound: VecDeque<HeldFrame>,
    held_outbound_bytes: usize,
    held_inbound: InboundHold<Arrived>,
    /// The state and key generation held ops were last decided under.
    decided_under: Option<(ChannelAccessState, u64)>,
    reported: Option<ChannelAccessState>,
    /// The generation of the keys that last failed to open held ciphertext
    /// and asked for newer ones: asked once per generation.
    asked_for_held: Option<u64>,
    /// The channel service answered the last question about these keys, so
    /// it tells the route when that answer may change. False until the
    /// first answer, and after a question the identity service refused or
    /// could not take.
    answered: bool,
}

impl RouteKeys {
    /// Keys of a route whose endpoint has an identity service to ask.
    pub(super) fn requested() -> Self {
        Self::with(Keys::Pending(ChannelAccessState::WaitingForAccess))
    }

    /// Keys of a harness route with no identity service.
    pub(super) fn plain() -> Self {
        Self::with(Keys::Plain)
    }

    fn with(keys: Keys) -> Self {
        Self {
            keys,
            held_outbound: VecDeque::new(),
            held_outbound_bytes: 0,
            held_inbound: InboundHold::new(MAX_HELD_INBOUND),
            decided_under: None,
            reported: None,
            asked_for_held: None,
            answered: false,
        }
    }

    /// Whether this endpoint asks the identity service for keys.
    pub(super) fn asks_identity_service(&self) -> bool {
        matches!(self.keys, Keys::Pending(_) | Keys::Content { .. })
    }

    /// Whether this route asks the identity service and holds no keys yet:
    /// it waits for access, was removed, or the service is unavailable.
    pub(super) fn awaiting_keys(&self) -> bool {
        matches!(self.keys, Keys::Pending(_))
    }

    /// Whether the endpoint asks about this route on its timer while the
    /// broker pushes channel changes: it holds no keys, and either nothing
    /// will tell it when that changes (no answer from the channel service
    /// yet) or it was removed, whose rejoin each question asks for again.
    pub(super) fn asks_on_timer(&self) -> bool {
        match self.keys {
            Keys::Pending(ChannelAccessState::Removed) => true,
            Keys::Pending(_) => !self.answered,
            _ => false,
        }
    }

    /// Record whether the channel service answered the last question.
    pub(super) fn set_answered(&mut self, answered: bool) {
        self.answered = answered;
    }

    /// Whether held ciphertext should make the endpoint ask for newer keys
    /// now: the route holds keys, they failed to open what arrived (sealed
    /// under an epoch they lack), and this generation has not asked yet.
    pub(super) fn wants_keys_for_held(&mut self) -> bool {
        let Keys::Content { generation, .. } = self.keys else {
            return false;
        };
        if self.held_inbound.is_empty() || self.asked_for_held == Some(generation) {
            return false;
        }
        self.asked_for_held = Some(generation);
        true
    }

    /// The service generation of the keys held, 0 when none.
    pub(super) fn generation(&self) -> u64 {
        match &self.keys {
            Keys::Content { generation, .. } => *generation,
            _ => 0,
        }
    }

    pub(super) fn state(&self) -> ChannelAccessState {
        match &self.keys {
            Keys::Plain => ChannelAccessState::Unencrypted,
            Keys::Pending(state) => *state,
            Keys::Group(_) | Keys::Content { .. } => ChannelAccessState::Encrypted,
        }
    }

    /// The state, once per change, for the app.
    pub(super) fn take_state_change(&mut self) -> Option<ChannelAccessState> {
        let state = self.state();
        (self.reported != Some(state)).then(|| {
            self.reported = Some(state);
            state
        })
    }

    /// Install an explicitly provisioned group.
    pub(super) fn install_group(&mut self, group: channel_crypto::ChannelGroup) {
        self.keys = Keys::Group(Box::new(group));
    }

    /// Apply what the identity service answered.
    pub(super) fn update(
        &mut self,
        access: runtime_identity::wire::ChannelAccess,
        generation: u64,
        encoded: Option<&[u8]>,
    ) {
        use runtime_identity::wire::ChannelAccess as Wire;
        match access {
            Wire::Ready => {
                if let Some(encoded) = encoded {
                    match channel_crypto::ChannelContentKeys::from_bytes(encoded) {
                        Ok(keys) => self.keys = Keys::Content { keys, generation },
                        Err(error) => {
                            tracing::warn!(%error, "collab: identity service sent malformed channel keys");
                        }
                    }
                }
            }
            Wire::Waiting => self.keys = Keys::Pending(ChannelAccessState::WaitingForAccess),
            Wire::Removed => self.keys = Keys::Pending(ChannelAccessState::Removed),
            Wire::Unavailable => self.keys = Keys::Pending(ChannelAccessState::Unavailable),
            Wire::NoSecretStore => self.keys = Keys::Pending(ChannelAccessState::NoSecretStore),
        }
    }

    /// Seal one cleartext document frame, or say to hold it.
    pub(super) fn seal(
        &self,
        artifact: ArtifactId,
        retention: ContentRetention,
        cleartext: Vec<u8>,
    ) -> Sealed {
        let sealed = match &self.keys {
            Keys::Plain => return Sealed::Ready(cleartext, None),
            Keys::Pending(ChannelAccessState::Removed) => {
                return Sealed::Refused("this device was removed from the channel".into());
            }
            Keys::Pending(_) => return Sealed::Hold,
            Keys::Group(group) => group.seal_content(artifact, retention, &cleartext),
            Keys::Content { keys, .. } => keys.seal_content(artifact, retention, &cleartext),
        };
        match sealed {
            Ok((metadata, ciphertext)) => Sealed::Ready(ciphertext, Some(metadata)),
            // The epoch's key has not reached this device (yet), or a commit
            // is being processed: waiting fixes it, so the edit is held and
            // sent once keys arrive rather than failing the whole channel.
            Err(channel_crypto::Error::MissingContentKey(_)) => Sealed::Hold,
            Err(error) => Sealed::Refused(format!("content encryption failed: {error}")),
        }
    }

    /// Open an envelope's payload under the broker's `admission` for it.
    /// Whether cleartext belongs on the channel is the broker's call alone:
    /// cleartext it admitted applies whatever this route's keys, cleartext
    /// it says must be sealed is refused, and cleartext with no verdict
    /// waits for one.
    pub(super) fn open(
        &mut self,
        envelope: &SignedOp,
        admission: OpAdmission,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, Unopened> {
        let Some(metadata) = envelope.encryption else {
            return match admission.admits_cleartext() {
                Some(true) => Ok(None),
                Some(false) => Err(Unopened::Never(Never::Downgrade)),
                None => Err(Unopened::Later),
            };
        };
        let ciphertext = collab_types::sealed_ciphertext(envelope)
            .map_err(|_| Unopened::Never(Never::Undecryptable))?;
        let opened = match &mut self.keys {
            Keys::Plain => return Err(Unopened::Never(Never::SealedOnPlain)),
            Keys::Pending(_) => return Err(Unopened::Later),
            Keys::Group(group) => {
                if group.channel_id() != metadata.channel {
                    return Err(Unopened::Never(Never::WrongChannel));
                }
                group.open_content(envelope.artifact, &metadata, &ciphertext)
            }
            Keys::Content { keys, .. } => {
                if keys.channel_id() != metadata.channel {
                    return Err(Unopened::Never(Never::WrongChannel));
                }
                if !keys.holds_epoch(metadata.epoch) {
                    return Err(Unopened::Later);
                }
                keys.open_content(envelope.artifact, &metadata, &ciphertext)
            }
        };
        match opened {
            Ok(plaintext) => Ok(Some(plaintext)),
            Err(channel_crypto::Error::MissingContentKey(_)) => Err(Unopened::Later),
            Err(_) => Err(Unopened::Never(Never::Undecryptable)),
        }
    }

    /// Keep a frame until keys arrive. `false` when the hold is full.
    pub(super) fn hold_outbound(
        &mut self,
        frame: Vec<u8>,
        claims: Vec<collab_types::ChangeClaim>,
    ) -> bool {
        let bytes = self.held_outbound_bytes.saturating_add(frame.len());
        if self.held_outbound.len() >= MAX_HELD_OUTBOUND || bytes > MAX_HELD_OUTBOUND_BYTES {
            return false;
        }
        self.held_outbound_bytes = bytes;
        self.held_outbound.push_back((frame, claims));
        true
    }

    /// Take every held frame, oldest first, once keys are held.
    pub(super) fn take_held_outbound(&mut self) -> Vec<HeldFrame> {
        if !self.state().is_open() {
            return Vec::new();
        }
        self.held_outbound_bytes = 0;
        self.held_outbound.drain(..).collect()
    }

    /// Keep an envelope that could not be opened yet, until the route's
    /// state changes or history supplies it. When the hold is full its
    /// oldest is owed by history instead ([`Self::owed_pages`]).
    pub(super) fn hold_inbound(&mut self, arrived: Arrived) {
        if let Some(owed) = self
            .held_inbound
            .hold(OpKey::of(&arrived.envelope), arrived)
        {
            tracing::info!(op = %owed, "collab: inbound hold full; history owes its oldest op");
        }
    }

    /// Every held envelope, oldest first, once per change of the route's
    /// state or keys, to decide again; what still cannot open is held again.
    pub(super) fn take_held_inbound(&mut self) -> Vec<Arrived> {
        let now = (self.state(), self.generation());
        if self.decided_under == Some(now) {
            return Vec::new();
        }
        self.decided_under = Some(now);
        self.held_inbound
            .take()
            .into_iter()
            .map(|(_, arrived)| arrived)
            .collect()
    }

    /// History supplied this op: it no longer waits here.
    pub(super) fn settle(&mut self, envelope: &SignedOp) {
        self.held_inbound.settle(&OpKey::of(envelope));
    }

    /// Whether history owes this route the op.
    pub(super) fn owes(&self, envelope: &SignedOp) -> bool {
        self.held_inbound.is_owed(&OpKey::of(envelope))
    }

    /// Where to page history for what the full hold let go of, once the
    /// route can open it.
    pub(super) fn owed_pages(&self) -> Vec<(ArtifactId, u64)> {
        if self.state().is_open() {
            self.held_inbound.owed_pages()
        } else {
            Vec::new()
        }
    }

    /// History has nothing older for `artifact`: the ops it never supplied.
    pub(super) fn give_up_owed(&mut self, artifact: ArtifactId) -> Vec<OpKey> {
        self.held_inbound.give_up_owed(artifact)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::{AttestationId, ChannelId, Ed25519PublicKey, OpKind};
    use runtime_identity::wire::ChannelAccess as Wire;

    fn keys(channel: ChannelId) -> (channel_crypto::ChannelGroup, Vec<u8>) {
        let group = channel_crypto::ChannelGroup::create(
            channel,
            channel_crypto::MemberKey::generate("device").unwrap(),
        )
        .unwrap();
        let bytes = group.content_keys().unwrap().to_bytes().unwrap().to_vec();
        (group, bytes)
    }

    #[test]
    fn an_op_history_supplied_while_held_is_not_released_again() {
        let channel = ChannelId::new();
        let (group, encoded) = keys(channel);
        let (metadata, sealed) = group
            .seal_content(
                ArtifactId(uuid::Uuid::from_bytes([1; 16])),
                ContentRetention::Delta,
                b"from a peer",
            )
            .unwrap();
        let arrived = envelope(sealed, Some(metadata));
        let mut route = RouteKeys::requested();
        route.hold_inbound(held(arrived.clone()));
        route.settle(&arrived);
        route.update(Wire::Ready, 1, Some(&encoded));
        assert!(route.take_held_inbound().is_empty());
    }

    #[test]
    fn held_ops_are_decided_again_only_when_the_route_changes() {
        let channel = ChannelId::new();
        let (_, encoded) = keys(channel);
        let mut route = RouteKeys::requested();
        route.hold_inbound(held(envelope(vec![1], None)));
        let again = route.take_held_inbound();
        assert_eq!(again.len(), 1);
        route.hold_inbound(again.into_iter().next().unwrap());
        assert!(route.take_held_inbound().is_empty());
        route.update(Wire::Ready, 1, Some(&encoded));
        assert_eq!(route.take_held_inbound().len(), 1);
    }

    fn held(envelope: SignedOp) -> Arrived {
        Arrived {
            envelope,
            from_user_id: "peer".into(),
            admission: OpAdmission::EncryptionRequired,
        }
    }

    fn envelope(payload: Vec<u8>, metadata: Option<EncryptionMetadata>) -> SignedOp {
        let artifact = ArtifactId(uuid::Uuid::from_bytes([1; 16]));
        match metadata {
            Some(metadata) => SignedOp::new_unsigned_with_encryption(
                payload,
                artifact,
                Ed25519PublicKey::from_bytes([2; 32]),
                AttestationId::default(),
                OpKind::Change,
                std::time::SystemTime::UNIX_EPOCH,
                1,
                metadata,
            ),
            None => SignedOp::new_unsigned(
                payload,
                artifact,
                Ed25519PublicKey::from_bytes([2; 32]),
                AttestationId::default(),
                OpKind::Change,
                std::time::SystemTime::UNIX_EPOCH,
                1,
            ),
        }
    }

    #[test]
    fn a_waiting_route_holds_both_directions_and_releases_them_in_order() {
        let channel = ChannelId::new();
        let (group, encoded) = keys(channel);
        let artifact = ArtifactId(uuid::Uuid::from_bytes([1; 16]));
        let (metadata, sealed) = group
            .seal_content(artifact, ContentRetention::Delta, b"from a peer")
            .unwrap();
        let mut route = RouteKeys::requested();
        assert_eq!(
            route.take_state_change(),
            Some(ChannelAccessState::WaitingForAccess)
        );
        assert_eq!(route.take_state_change(), None);
        assert!(matches!(
            route.seal(artifact, ContentRetention::Delta, b"mine".to_vec()),
            Sealed::Hold
        ));
        assert!(route.hold_outbound(b"first".to_vec(), Vec::new()));
        assert!(route.hold_outbound(b"second".to_vec(), Vec::new()));
        let arrived = envelope(sealed, Some(metadata));
        assert!(matches!(
            route.open(&arrived, OpAdmission::EncryptionRequired),
            Err(Unopened::Later)
        ));
        route.hold_inbound(held(arrived));
        assert!(
            route.take_held_outbound().is_empty(),
            "nothing leaves while waiting"
        );

        route.update(Wire::Ready, 7, Some(&encoded));
        assert_eq!(
            route.take_state_change(),
            Some(ChannelAccessState::Encrypted)
        );
        assert_eq!(route.generation(), 7);
        assert_eq!(
            route.take_held_outbound(),
            vec![
                (b"first".to_vec(), Vec::new()),
                (b"second".to_vec(), Vec::new())
            ]
        );
        let held = route.take_held_inbound();
        assert_eq!(held.len(), 1);
        assert_eq!(
            route
                .open(&held[0].envelope, held[0].admission)
                .unwrap()
                .unwrap()
                .as_slice(),
            b"from a peer"
        );
        let Sealed::Ready(ciphertext, Some(metadata)) =
            route.seal(artifact, ContentRetention::Delta, b"mine".to_vec())
        else {
            panic!("an admitted route seals");
        };
        assert_ne!(ciphertext, b"mine");
        let mut group = group;
        assert_eq!(
            group
                .open_content(artifact, &metadata, &ciphertext)
                .unwrap()
                .as_slice(),
            b"mine"
        );
    }

    #[test]
    fn a_sealed_compaction_opens_like_a_sealed_snapshot() {
        let channel = ChannelId::new();
        let (group, encoded) = keys(channel);
        let artifact = ArtifactId(uuid::Uuid::from_bytes([1; 16]));
        let (write_metadata, write) = group
            .seal_content(artifact, ContentRetention::Delta, b"an edit")
            .unwrap();
        let superseded = envelope(write, Some(write_metadata));
        let (metadata, sealed) = group
            .seal_content(artifact, ContentRetention::Checkpoint, b"merged record")
            .unwrap();
        let payload = collab_types::SealedCompaction::new(&[superseded], sealed)
            .unwrap()
            .encode()
            .unwrap();
        let mut compaction = envelope(payload, Some(metadata));
        compaction.kind = OpKind::Compaction;
        let mut route = RouteKeys::requested();
        route.update(Wire::Ready, 1, Some(&encoded));
        assert_eq!(
            route
                .open(&compaction, OpAdmission::EncryptionRequired)
                .unwrap()
                .unwrap()
                .as_slice(),
            b"merged record"
        );
        let mut grouped = RouteKeys::plain();
        grouped.install_group(group);
        assert_eq!(
            grouped
                .open(&compaction, OpAdmission::EncryptionRequired)
                .unwrap()
                .unwrap()
                .as_slice(),
            b"merged record"
        );
        compaction.payload.push(0);
        assert!(matches!(
            route.open(&compaction, OpAdmission::EncryptionRequired),
            Err(Unopened::Never(_))
        ));
    }

    #[test]
    fn cleartext_opens_exactly_when_the_broker_admits_it() {
        let channel = ChannelId::new();
        let (_, encoded) = keys(channel);
        let mut route = RouteKeys::requested();
        let old = envelope(b"from before encryption".to_vec(), None);
        assert!(matches!(
            route.open(&old, OpAdmission::CleartextAdmitted),
            Ok(None)
        ));
        route.update(Wire::Ready, 1, Some(&encoded));
        assert!(matches!(
            route.open(&old, OpAdmission::CleartextAdmitted),
            Ok(None)
        ));
        assert!(matches!(
            route.open(&old, OpAdmission::EncryptionRequired),
            Err(Unopened::Never(Never::Downgrade))
        ));
        assert!(matches!(
            route.open(&old, OpAdmission::Unstated),
            Err(Unopened::Later)
        ));
        let other = keys(ChannelId::new()).0;
        let (metadata, sealed) = other
            .seal_content(old.artifact, ContentRetention::Delta, b"x")
            .unwrap();
        assert!(matches!(
            route.open(
                &envelope(sealed, Some(metadata)),
                OpAdmission::EncryptionRequired
            ),
            Err(Unopened::Never(Never::WrongChannel))
        ));
    }

    /// Ciphertext the held keys can't open (sealed under a newer epoch) asks
    /// for newer keys once per generation, and never while keys are awaited
    /// (those routes are asked anyway).
    #[test]
    fn unopenable_ciphertext_asks_for_newer_keys_once_per_generation() {
        let channel = ChannelId::new();
        let (_, encoded) = keys(channel);
        let artifact = ArtifactId(uuid::Uuid::from_bytes([1; 16]));
        let mut route = RouteKeys::requested();
        assert!(route.awaiting_keys());
        route.hold_inbound(held(envelope(vec![1], None)));
        assert!(
            !route.wants_keys_for_held(),
            "a waiting route is asked anyway"
        );

        route.update(Wire::Ready, 1, Some(&encoded));
        assert!(!route.awaiting_keys());
        assert_eq!(route.take_held_inbound().len(), 1, "released with the keys");
        assert!(
            !route.wants_keys_for_held(),
            "nothing held asks for nothing"
        );
        let newer = keys(channel).0;
        let (metadata, sealed) = newer
            .seal_content(artifact, ContentRetention::Delta, b"from a newer epoch")
            .unwrap();
        route.hold_inbound(held(envelope(sealed, Some(metadata))));
        assert!(route.wants_keys_for_held());
        assert!(
            !route.wants_keys_for_held(),
            "asked once for this generation"
        );

        route.update(Wire::Ready, 2, Some(&encoded));
        assert!(route.wants_keys_for_held(), "newer keys may still lack it");
        assert!(!route.wants_keys_for_held());
    }

    #[test]
    fn removal_refuses_new_edits_and_other_states_hold_them() {
        let mut route = RouteKeys::requested();
        route.update(Wire::Removed, 3, None);
        assert_eq!(route.state(), ChannelAccessState::Removed);
        assert!(matches!(
            route.seal(ArtifactId::new(), ContentRetention::Delta, vec![1]),
            Sealed::Refused(_)
        ));
        route.update(Wire::Unavailable, 4, None);
        assert!(matches!(
            route.seal(ArtifactId::new(), ContentRetention::Delta, vec![1]),
            Sealed::Hold
        ));
        let mut plain = RouteKeys::plain();
        assert!(!plain.asks_identity_service());
        assert!(matches!(
            plain.seal(ArtifactId::new(), ContentRetention::Delta, vec![1]),
            Sealed::Ready(_, None)
        ));
        assert!(matches!(
            plain.open(&envelope(vec![1], None), OpAdmission::CleartextAdmitted),
            Ok(None)
        ));
    }
}
