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
//! Only an endpoint with no identity service at all (an explicit test
//! harness) stays plaintext, and only until a group is installed on it.

use std::collections::VecDeque;

use collab_types::{ArtifactId, ContentRetention, EncryptionMetadata, SignedOp};
use zeroize::Zeroizing;

use super::ChannelAccessState;

/// Most edits held while waiting for access, and most bytes.
const MAX_HELD_OUTBOUND: usize = 4096;
const MAX_HELD_OUTBOUND_BYTES: usize = 64 * 1024 * 1024;
/// Most ciphertext ops held while waiting for keys.
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
    /// It never will (wrong channel, tampered, downgrade).
    Never,
}

/// A route's keys plus what waits on them.
pub(super) struct RouteKeys {
    pub(super) keys: Keys,
    held_outbound: VecDeque<Vec<u8>>,
    held_outbound_bytes: usize,
    held_inbound: VecDeque<(SignedOp, String)>,
    reported: Option<ChannelAccessState>,
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
            held_inbound: VecDeque::new(),
            reported: None,
        }
    }

    /// Whether this endpoint asks the identity service for keys.
    pub(super) fn asks_identity_service(&self) -> bool {
        matches!(self.keys, Keys::Pending(_) | Keys::Content { .. })
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
            Err(error) => Sealed::Refused(format!("content encryption failed: {error}")),
        }
    }

    /// Open an encrypted envelope's payload. `live` refuses plaintext on an
    /// encrypted route (a downgrade); history pages from the local broker
    /// may still carry plaintext from before the channel was encrypted,
    /// which the broker admitted when plaintext was allowed.
    pub(super) fn open(
        &mut self,
        envelope: &SignedOp,
        live: bool,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, Unopened> {
        let Some(metadata) = envelope.encryption else {
            return match &self.keys {
                Keys::Plain => Ok(None),
                _ if live => Err(Unopened::Never),
                _ => Ok(None),
            };
        };
        let ciphertext = collab_types::sealed_ciphertext(envelope).map_err(|_| Unopened::Never)?;
        let opened = match &mut self.keys {
            Keys::Plain => return Err(Unopened::Never),
            Keys::Pending(_) => return Err(Unopened::Later),
            Keys::Group(group) => {
                if group.channel_id() != metadata.channel {
                    return Err(Unopened::Never);
                }
                group.open_content(envelope.artifact, &metadata, &ciphertext)
            }
            Keys::Content { keys, .. } => {
                if keys.channel_id() != metadata.channel {
                    return Err(Unopened::Never);
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
            Err(_) => Err(Unopened::Never),
        }
    }

    /// Keep a frame until keys arrive. `false` when the hold is full.
    pub(super) fn hold_outbound(&mut self, frame: Vec<u8>) -> bool {
        let bytes = self.held_outbound_bytes.saturating_add(frame.len());
        if self.held_outbound.len() >= MAX_HELD_OUTBOUND || bytes > MAX_HELD_OUTBOUND_BYTES {
            return false;
        }
        self.held_outbound_bytes = bytes;
        self.held_outbound.push_back(frame);
        true
    }

    /// Take every held frame, oldest first, once keys are held.
    pub(super) fn take_held_outbound(&mut self) -> Vec<Vec<u8>> {
        if !self.state().is_open() {
            return Vec::new();
        }
        self.held_outbound_bytes = 0;
        self.held_outbound.drain(..).collect()
    }

    /// Keep an envelope that could not be opened yet. The oldest is dropped
    /// when the hold is full; history paging recovers it once keys arrive.
    pub(super) fn hold_inbound(&mut self, envelope: SignedOp, from_user_id: String) {
        if self.held_inbound.len() >= MAX_HELD_INBOUND {
            self.held_inbound.pop_front();
        }
        self.held_inbound.push_back((envelope, from_user_id));
    }

    /// Take every held envelope, oldest first, once keys are held.
    pub(super) fn take_held_inbound(&mut self) -> Vec<(SignedOp, String)> {
        if !self.state().is_open() {
            return Vec::new();
        }
        self.held_inbound.drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::{AttestationId, ChannelId, Ed25519PublicKey, OpClass};
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

    fn envelope(payload: Vec<u8>, metadata: Option<EncryptionMetadata>) -> SignedOp {
        let artifact = ArtifactId(uuid::Uuid::from_bytes([1; 16]));
        match metadata {
            Some(metadata) => SignedOp::new_unsigned_with_encryption(
                payload,
                artifact,
                Ed25519PublicKey::from_bytes([2; 32]),
                AttestationId::default(),
                OpClass::Splice,
                std::time::SystemTime::UNIX_EPOCH,
                1,
                metadata,
            ),
            None => SignedOp::new_unsigned(
                payload,
                artifact,
                Ed25519PublicKey::from_bytes([2; 32]),
                AttestationId::default(),
                OpClass::Splice,
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
        assert!(route.hold_outbound(b"first".to_vec()));
        assert!(route.hold_outbound(b"second".to_vec()));
        let arrived = envelope(sealed, Some(metadata));
        assert!(matches!(route.open(&arrived, true), Err(Unopened::Later)));
        route.hold_inbound(arrived, "peer".into());
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
            vec![b"first".to_vec(), b"second".to_vec()]
        );
        let held = route.take_held_inbound();
        assert_eq!(held.len(), 1);
        let (arrived, _) = &held[0];
        assert_eq!(
            route.open(arrived, true).unwrap().unwrap().as_slice(),
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
        compaction.op_class = OpClass::Compaction;
        let mut route = RouteKeys::requested();
        route.update(Wire::Ready, 1, Some(&encoded));
        assert_eq!(
            route.open(&compaction, true).unwrap().unwrap().as_slice(),
            b"merged record"
        );
        let mut grouped = RouteKeys::plain();
        grouped.install_group(group);
        assert_eq!(
            grouped.open(&compaction, true).unwrap().unwrap().as_slice(),
            b"merged record"
        );
        compaction.payload.push(0);
        assert!(matches!(
            route.open(&compaction, true),
            Err(Unopened::Never)
        ));
    }

    #[test]
    fn plaintext_is_a_downgrade_live_but_readable_from_history() {
        let channel = ChannelId::new();
        let (_, encoded) = keys(channel);
        let mut route = RouteKeys::requested();
        route.update(Wire::Ready, 1, Some(&encoded));
        let old = envelope(b"from before encryption".to_vec(), None);
        assert!(matches!(route.open(&old, true), Err(Unopened::Never)));
        assert!(matches!(route.open(&old, false), Ok(None)));
        let other = keys(ChannelId::new()).0;
        let (metadata, sealed) = other
            .seal_content(old.artifact, ContentRetention::Delta, b"x")
            .unwrap();
        assert!(matches!(
            route.open(&envelope(sealed, Some(metadata)), true),
            Err(Unopened::Never)
        ));
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
            plain.open(&envelope(vec![1], None), true),
            Ok(None)
        ));
    }
}
