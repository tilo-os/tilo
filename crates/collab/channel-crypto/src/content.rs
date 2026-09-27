//! Content keys: the key material ops are encrypted under, separate from
//! MLS's own epoch secrets.
//!
//! ## Model
//!
//! Every MLS epoch `e` of a channel has one 256-bit *content key*, derived
//! by every member of that epoch from the MLS exporter:
//! `MLS-Exporter("tilo channel content key v2", channel_id, 32)`. Ops are
//! sealed under the key of the sender's current epoch with a `tilo-seal`
//! at-rest envelope ([`tilo_seal::Label::ChannelContent`]) whose context
//! binds the channel, the epoch and the artifact. Unlike MLS application
//! messages, a content-key ciphertext can be opened any number of times, by
//! any holder of the key, long after the epoch ended — that is what makes
//! history readable.
//!
//! A [`ContentKeyring`] holds every content key a member has: the keys of
//! epochs it lived through, plus the keys it received on joining. When a
//! member is added, the inviter hands the joiner a [`KeyHistory`] bundle
//! containing every key it holds, sealed under a transport key exported
//! from the joiner's first epoch (so only members of that epoch — which
//! includes the joiner and excludes anyone removed earlier — can open it).
//! The bundle grows with the number of membership epochs, never with the
//! amount of content: a channel with a million ops and twenty membership
//! changes hands over twenty keys.
//!
//! ## Security properties
//!
//! * **Joiners read everything.** A member added at epoch `e` can decrypt
//!   every content-key op of every earlier epoch, locally, without any
//!   other member online: the keys arrive once with the join.
//! * **Removal protects future content.** Removing a member is an MLS
//!   commit; the next epoch's exporter secret is unknown to the removed
//!   member, so it cannot derive that epoch's content key or any later one.
//!   It keeps the keys (and so the content) it legitimately held before.
//! * **Forward secrecy applies to membership removal, not to history.**
//!   Content keys are deliberately long-lived and stored in every member's
//!   sealed state. Compromise of a current member's state reveals the whole
//!   history, exactly as compromising its document replica would. MLS still
//!   provides forward secrecy and post-compromise security for its own
//!   handshake and epoch secrets.
//! * **No sender authentication inside the envelope.** Any holder of a
//!   content key can seal for that epoch. Authorship comes from the outer
//!   `SignedOp` signature and the broker's live role check; the broker also
//!   refuses new ops sealed under an epoch older than the channel's latest
//!   membership change, so a removed member cannot inject content under a
//!   key it still holds.
//! * **Lagging senders.** A member that has not yet processed a removal
//!   commit still seals under the previous epoch; until it catches up, that
//!   content is readable by the removed member. The broker's epoch check
//!   rejects such ops once it has recorded the removal.

use std::collections::BTreeMap;

use collab_types::{ArtifactId, ChannelId, ContentRetention, EncryptionMetadata};
use serde::{Deserialize, Serialize};
use tilo_seal::{Context, Label, SymmetricKey};
use zeroize::{Zeroize as _, Zeroizing};

use crate::{Error, Result};

/// MLS exporter label for per-epoch content keys.
pub(crate) const CONTENT_KEY_EXPORTER_LABEL: &str = "tilo channel content key v2";
/// MLS exporter label for the transport key that seals a joiner's history.
pub(crate) const HISTORY_TRANSPORT_EXPORTER_LABEL: &str = "tilo channel key history v2";
const KEY_HISTORY_VERSION: u8 = 1;
/// Upper bound on keys in one history bundle, far above any realistic
/// number of membership epochs, so a hostile bundle cannot exhaust memory.
pub const MAX_HISTORY_KEYS: usize = 1 << 20;

/// Every content key a member holds for one channel, by epoch.
#[derive(Clone, Default)]
pub struct ContentKeyring {
    keys: BTreeMap<u64, SymmetricKey>,
}

impl ContentKeyring {
    /// An empty keyring.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the key of `epoch`. An existing key for the epoch must match;
    /// a mismatch means two members disagree about the channel's history.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidState`] when a different key is already recorded.
    pub fn insert(&mut self, epoch: u64, key: SymmetricKey) -> Result<()> {
        match self.keys.get(&epoch) {
            Some(existing) if *existing != key => Err(Error::InvalidState(format!(
                "conflicting content key for epoch {epoch}"
            ))),
            Some(_) => Ok(()),
            None => {
                self.keys.insert(epoch, key);
                Ok(())
            }
        }
    }

    /// The key of `epoch`, if held.
    pub fn get(&self, epoch: u64) -> Option<&SymmetricKey> {
        self.keys.get(&epoch)
    }

    /// Number of epochs with a key.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Whether no key is held.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Oldest and newest epoch with a key.
    pub fn epoch_range(&self) -> Option<(u64, u64)> {
        Some((*self.keys.keys().next()?, *self.keys.keys().next_back()?))
    }

    /// Open a content-key payload for `artifact`. Needs only the keyring, so
    /// history pages fetched long after joining decrypt locally.
    ///
    /// # Errors
    ///
    /// [`Error::MissingContentKey`] when the epoch's key is not held,
    /// [`Error::ContentCrypto`] when the payload fails to authenticate, and
    /// [`Error::InvalidState`] for metadata of another scheme.
    pub fn open(
        &self,
        channel: ChannelId,
        artifact: ArtifactId,
        metadata: &EncryptionMetadata,
        payload: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>> {
        if metadata.retention().is_none() || metadata.channel != channel {
            return Err(Error::InvalidState(
                "payload is not content-key ciphertext for this channel".into(),
            ));
        }
        let key = self
            .get(metadata.epoch)
            .ok_or(Error::MissingContentKey(metadata.epoch))?;
        let record = content_record(channel, metadata.epoch, artifact);
        tilo_seal::envelope::open(key, Context::new(Label::ChannelContent, &record), payload)
            .map_err(Error::ContentCrypto)
    }

    /// Seal `plaintext` for `artifact` under the key of `epoch`.
    ///
    /// # Errors
    ///
    /// [`Error::MissingContentKey`] when the epoch's key is not held.
    pub fn seal(
        &self,
        channel: ChannelId,
        epoch: u64,
        artifact: ArtifactId,
        retention: ContentRetention,
        plaintext: &[u8],
    ) -> Result<(EncryptionMetadata, Vec<u8>)> {
        let key = self.get(epoch).ok_or(Error::MissingContentKey(epoch))?;
        let record = content_record(channel, epoch, artifact);
        let sealed =
            tilo_seal::envelope::seal(key, Context::new(Label::ChannelContent, &record), plaintext)
                .map_err(Error::ContentCrypto)?;
        Ok((
            EncryptionMetadata::content_key(epoch, channel, retention),
            sealed,
        ))
    }

    /// Merge every key of `other`, refusing conflicting keys.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidState`] on a conflict; the keyring is unchanged then.
    pub fn merge(&mut self, other: &Self) -> Result<()> {
        for (epoch, key) in &other.keys {
            if self.keys.get(epoch).is_some_and(|existing| existing != key) {
                return Err(Error::InvalidState(format!(
                    "conflicting content key for epoch {epoch}"
                )));
            }
        }
        for (epoch, key) in &other.keys {
            self.keys.entry(*epoch).or_insert_with(|| key.clone());
        }
        Ok(())
    }

    pub(crate) fn to_wire(&self) -> Vec<(u64, [u8; 32])> {
        self.keys
            .iter()
            .map(|(epoch, key)| (*epoch, *key.expose_secret()))
            .collect()
    }

    pub(crate) fn from_wire(mut entries: Vec<(u64, [u8; 32])>) -> Result<Self> {
        let mut keyring = Self::new();
        let result = entries
            .iter()
            .try_for_each(|(epoch, key)| keyring.insert(*epoch, SymmetricKey::from_bytes(*key)));
        for (_, key) in &mut entries {
            key.zeroize();
        }
        result.map(|()| keyring)
    }
}

impl std::fmt::Debug for ContentKeyring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContentKeyring")
            .field("epochs", &self.len())
            .field("range", &self.epoch_range())
            .finish()
    }
}

/// What a content ciphertext is bound to.
fn content_record(channel: ChannelId, epoch: u64, artifact: ArtifactId) -> [u8; 40] {
    let mut record = [0_u8; 40];
    let (channel_part, rest) = record.split_at_mut(16);
    channel_part.copy_from_slice(channel.0.as_bytes());
    let (epoch_part, artifact_part) = rest.split_at_mut(8);
    epoch_part.copy_from_slice(&epoch.to_le_bytes());
    artifact_part.copy_from_slice(artifact.0.as_bytes());
    record
}

pub(crate) fn history_record(channel: ChannelId, epoch: u64) -> [u8; 24] {
    let mut record = [0_u8; 24];
    let (channel_part, epoch_part) = record.split_at_mut(16);
    channel_part.copy_from_slice(channel.0.as_bytes());
    epoch_part.copy_from_slice(&epoch.to_le_bytes());
    record
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyHistoryPlaintext {
    version: u8,
    channel: ChannelId,
    keys: Vec<(u64, [u8; 32])>,
}

/// A joiner's content-key history, sealed for the epoch it joins at.
///
/// Wire form: postcard of `{ epoch, sealed }` where `sealed` is a
/// `tilo-seal` envelope ([`Label::ChannelKeyHistory`], context =
/// channel id ‖ epoch) under the MLS-exported transport key of `epoch`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyHistory {
    /// The epoch whose exporter seals the bundle: the joiner's first epoch.
    pub epoch: u64,
    sealed: Vec<u8>,
}

impl KeyHistory {
    pub(crate) fn seal(
        channel: ChannelId,
        epoch: u64,
        transport: &SymmetricKey,
        keyring: &ContentKeyring,
    ) -> Result<Self> {
        let mut plaintext = KeyHistoryPlaintext {
            version: KEY_HISTORY_VERSION,
            channel,
            keys: keyring.to_wire(),
        };
        let encoded = postcard::to_allocvec(&plaintext)
            .map(Zeroizing::new)
            .map_err(|error| Error::Serialization(error.to_string()));
        for (_, key) in &mut plaintext.keys {
            key.zeroize();
        }
        let record = history_record(channel, epoch);
        let sealed = tilo_seal::envelope::seal(
            transport,
            Context::new(Label::ChannelKeyHistory, &record),
            &encoded?,
        )
        .map_err(Error::ContentCrypto)?;
        Ok(Self { epoch, sealed })
    }

    pub(crate) fn open(
        &self,
        channel: ChannelId,
        transport: &SymmetricKey,
    ) -> Result<ContentKeyring> {
        let record = history_record(channel, self.epoch);
        let plaintext = tilo_seal::envelope::open(
            transport,
            Context::new(Label::ChannelKeyHistory, &record),
            &self.sealed,
        )
        .map_err(Error::ContentCrypto)?;
        let decoded: KeyHistoryPlaintext = postcard::from_bytes(&plaintext)
            .map_err(|error| Error::Serialization(error.to_string()))?;
        if decoded.version != KEY_HISTORY_VERSION {
            return Err(Error::InvalidState(
                "unsupported key history version".into(),
            ));
        }
        if decoded.channel != channel {
            return Err(Error::InvalidState(
                "key history names another channel".into(),
            ));
        }
        if decoded.keys.len() > MAX_HISTORY_KEYS {
            return Err(Error::InvalidState("key history is too large".into()));
        }
        ContentKeyring::from_wire(decoded.keys)
    }

    /// Encode for the wire.
    ///
    /// # Errors
    ///
    /// [`Error::Serialization`] if encoding fails.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        postcard::to_allocvec(self).map_err(|error| Error::Serialization(error.to_string()))
    }

    /// Decode from the wire, refusing trailing bytes.
    ///
    /// # Errors
    ///
    /// [`Error::Serialization`] for malformed bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let (history, rest) = postcard::take_from_bytes::<Self>(bytes)
            .map_err(|error| Error::Serialization(error.to_string()))?;
        if !rest.is_empty() {
            return Err(Error::Serialization("trailing key history bytes".into()));
        }
        Ok(history)
    }

    /// Size of the sealed bundle, for accounting what a join transfers.
    pub fn sealed_len(&self) -> usize {
        self.sealed.len()
    }
}

impl std::fmt::Debug for KeyHistory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyHistory")
            .field("epoch", &self.epoch)
            .field("sealed_len", &self.sealed.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Content is bound to channel, epoch and artifact; conflicting keys are refused; a history
    /// bundle round-trips only for its channel and epoch.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn sealed_content_opens_only_for_its_channel_epoch_and_artifact() {
        content_is_bound_to_channel_epoch_and_artifact();
        conflicting_keys_are_refused();
        history_bundle_round_trips_only_for_its_channel_and_epoch();
    }

    fn content_is_bound_to_channel_epoch_and_artifact() {
        let mut ring = ContentKeyring::new();
        ring.insert(1, SymmetricKey::generate().unwrap()).unwrap();
        ring.insert(2, SymmetricKey::generate().unwrap()).unwrap();
        let channel = ChannelId::new();
        let artifact = ArtifactId::new();
        let (metadata, sealed) = ring
            .seal(channel, 1, artifact, ContentRetention::Delta, b"op")
            .unwrap();
        assert_eq!(
            ring.open(channel, artifact, &metadata, &sealed)
                .unwrap()
                .as_slice(),
            b"op"
        );
        assert!(
            ring.open(channel, ArtifactId::new(), &metadata, &sealed)
                .is_err()
        );
        assert!(
            ring.open(ChannelId::new(), artifact, &metadata, &sealed)
                .is_err()
        );
        let mut wrong_epoch = metadata;
        wrong_epoch.epoch = 2;
        assert!(ring.open(channel, artifact, &wrong_epoch, &sealed).is_err());
        let v1 = EncryptionMetadata::mls_application(1, channel);
        assert!(ring.open(channel, artifact, &v1, &sealed).is_err());
    }

    fn conflicting_keys_are_refused() {
        let mut ring = ContentKeyring::new();
        let key = SymmetricKey::generate().unwrap();
        ring.insert(1, key.clone()).unwrap();
        ring.insert(1, key).unwrap();
        assert!(ring.insert(1, SymmetricKey::generate().unwrap()).is_err());
        let mut other = ContentKeyring::new();
        other.insert(1, SymmetricKey::generate().unwrap()).unwrap();
        other.insert(9, SymmetricKey::generate().unwrap()).unwrap();
        assert!(ring.merge(&other).is_err());
        assert!(ring.get(9).is_none(), "a refused merge changes nothing");
    }

    fn history_bundle_round_trips_only_for_its_channel_and_epoch() {
        let mut ring = ContentKeyring::new();
        for epoch in 0..20 {
            ring.insert(epoch, SymmetricKey::generate().unwrap())
                .unwrap();
        }
        let channel = ChannelId::new();
        let transport = SymmetricKey::generate().unwrap();
        let history = KeyHistory::seal(channel, 20, &transport, &ring).unwrap();
        let decoded = KeyHistory::from_bytes(&history.to_bytes().unwrap()).unwrap();
        let opened = decoded.open(channel, &transport).unwrap();
        assert_eq!(opened.len(), 20);
        assert_eq!(opened.get(7), ring.get(7));
        assert!(decoded.open(ChannelId::new(), &transport).is_err());
        assert!(
            decoded
                .open(channel, &SymmetricKey::generate().unwrap())
                .is_err()
        );
        let mut moved = decoded;
        moved.epoch = 21;
        assert!(moved.open(channel, &transport).is_err());
        assert!(
            !format!("{ring:?}").contains(&format!("{:?}", ring.get(7).unwrap().expose_secret()))
        );
    }
}
