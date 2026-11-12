//! What an application endpoint holds for one channel: content keys only.
//!
//! A device keeps its MLS member state (signer, HPKE and ratchet secrets)
//! in its trusted session service. An application process that edits the
//! channel's document gets a [`ChannelContentKeys`]: the current epoch and
//! every content key the device holds. That is enough to seal new content
//! and open history, and not enough to create commits, admit or remove
//! anyone, or answer a Welcome. When the device's group moves to a new
//! epoch the service hands the app a fresh value.
//!
//! The same keys derive the per-stream keys a call's media frames are
//! sealed with ([`ChannelContentKeys::media_key`]), so a call needs no key
//! exchange of its own and no key server.

use collab_types::{ArtifactId, ChannelId, ContentRetention, EncryptionMetadata, EncryptionScheme};
use serde::{Deserialize, Serialize};
use tilo_seal::SymmetricKey;
use zeroize::{Zeroize as _, Zeroizing};

use crate::content::ContentKeyring;
use crate::{Error, Result};

const WIRE_VERSION: u8 = 1;

/// Length of the tag naming one media stream: its lane and its source.
pub const MEDIA_STREAM_TAG_LEN: usize = 2;

/// Domain of the per-stream media keys, so a key one of them derives is
/// never one derived for another purpose.
const MEDIA_KEY_DOMAIN: &[u8] = b"tilo-call-media-v1";

/// Domain of the secret record content keys derive from.
const RECORD_CONTENT_DOMAIN: &[u8] = b"tilo-record-content-v1";

/// Content keys of one channel, as handed to an application endpoint.
#[derive(Clone)]
pub struct ChannelContentKeys {
    channel: ChannelId,
    epoch: u64,
    keyring: ContentKeyring,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    version: u8,
    channel: ChannelId,
    epoch: u64,
    keys: Vec<(u64, [u8; 32])>,
}

impl ChannelContentKeys {
    pub(crate) fn new(channel: ChannelId, epoch: u64, keyring: ContentKeyring) -> Self {
        Self {
            channel,
            epoch,
            keyring,
        }
    }

    /// These keys for `unit`, a storage unit of the scope they came from:
    /// the seal context names the unit while the keys stay the scope's, so
    /// content is bound to its own record and artifact and a membership
    /// change costs the scope one commit, not the unit one.
    #[must_use]
    pub fn for_unit(&self, unit: ChannelId) -> Self {
        Self {
            channel: unit,
            epoch: self.epoch,
            keyring: self.keyring.clone(),
        }
    }

    /// Keep the keys `earlier`, an earlier group of the same channel, left
    /// behind: content it sealed opens still, under whichever key its epoch
    /// names, while the current epoch's key keeps sealing. A unit that moved
    /// from its own group to the key scope that carries it reads its whole
    /// history from one keyring.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidState`] when the merged keyring would be full.
    pub fn with_earlier(mut self, earlier: &Self) -> Result<Self> {
        if earlier.channel == self.channel {
            self.keyring.merge(&earlier.keyring)?;
        }
        Ok(self)
    }

    pub fn channel_id(&self) -> ChannelId {
        self.channel
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn holds_epoch(&self, epoch: u64) -> bool {
        self.keyring.get(epoch).is_some()
    }

    /// Seal an op payload for `artifact` under the current epoch's key.
    ///
    /// # Errors
    ///
    /// [`Error::MissingContentKey`] when the current epoch's key is absent,
    /// or a sealing error.
    pub fn seal_content(
        &self,
        artifact: ArtifactId,
        retention: ContentRetention,
        plaintext: &[u8],
    ) -> Result<(EncryptionMetadata, Vec<u8>)> {
        self.keyring
            .seal(self.channel, self.epoch, artifact, retention, plaintext)
    }

    /// Open a content-key payload of any epoch whose key is held.
    ///
    /// # Errors
    ///
    /// [`Error::MissingContentKey`] for an epoch not held,
    /// [`Error::ContentCrypto`] for a payload that fails to authenticate, and
    /// [`Error::InvalidState`] for another channel or a single-use MLS
    /// application message, which an endpoint without MLS state cannot open.
    pub fn open_content(
        &self,
        artifact: ArtifactId,
        metadata: &EncryptionMetadata,
        payload: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>> {
        if metadata.channel != self.channel {
            return Err(Error::InvalidState("payload names another channel".into()));
        }
        if metadata.scheme == EncryptionScheme::MlsApplication {
            return Err(Error::InvalidState(
                "MLS application messages open only at the device's MLS member".into(),
            ));
        }
        self.keyring.open(self.channel, artifact, metadata, payload)
    }

    /// Open a new write's payload: only one sealed under the channel's
    /// current epoch. A device removed from the channel still holds older
    /// epochs' keys, so anything else is refused, never opened.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidState`] for another channel or epoch, and as
    /// [`Self::open_content`].
    pub fn open_current_content(
        &self,
        artifact: ArtifactId,
        metadata: &EncryptionMetadata,
        payload: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>> {
        if metadata.epoch != self.epoch {
            return Err(Error::InvalidState(
                "payload is not sealed under the channel's current epoch".into(),
            ));
        }
        self.open_content(artifact, metadata, payload)
    }

    /// The secret the content a record references is sealed under now: a
    /// BLAKE3 keyed hash, under the current epoch's content key, of
    /// `"tilo-record-content-v1"`. The channel is left out, so every record
    /// one key scope carries shares it, and the same bytes stored for two
    /// of them are one item (`tilo_content::ContentStore::put_reader`).
    ///
    /// Holders of the epoch's key derive it, and nobody else: a device
    /// removed before the epoch derives nothing.
    ///
    /// # Errors
    ///
    /// [`Error::MissingContentKey`] when this endpoint holds no key for the
    /// current epoch.
    pub fn record_content_secret(&self) -> Result<SymmetricKey> {
        let content = self
            .keyring
            .get(self.epoch)
            .ok_or(Error::MissingContentKey(self.epoch))?;
        let mut hash = blake3::Hasher::new_keyed(content.expose_secret());
        hash.update(RECORD_CONTENT_DOMAIN);
        Ok(SymmetricKey::from_bytes(*hash.finalize().as_bytes()))
    }

    /// The key that seals one device's frames of one media stream under
    /// `epoch`: a BLAKE3 keyed hash, under that epoch's content key, of
    /// `"tilo-call-media-v1" ‖ channel ‖ epoch ‖ sender ‖ stream`.
    ///
    /// `sender` is the sending device's public key and `stream` the two
    /// bytes naming the lane and, within it, the source (a call lane's own
    /// vocabulary, so nothing but bytes of it lives here). Every holder of
    /// the epoch's content key derives the same value, which is why a member
    /// that joined later opens frames a lagging sender still seals under an
    /// earlier epoch, exactly as it reads that epoch's content. The sender,
    /// the stream and the channel are hashed in, so a frame cannot be
    /// replayed as another device's, on another stream or in another
    /// channel's call. A device removed before `epoch` holds no key for it
    /// and derives nothing.
    ///
    /// # Errors
    ///
    /// [`Error::MissingContentKey`] when this endpoint holds no content key
    /// for `epoch`.
    pub fn media_key(
        &self,
        epoch: u64,
        sender: [u8; 32],
        stream: [u8; MEDIA_STREAM_TAG_LEN],
    ) -> Result<SymmetricKey> {
        let content = self
            .keyring
            .get(epoch)
            .ok_or(Error::MissingContentKey(epoch))?;
        let mut hash = blake3::Hasher::new_keyed(content.expose_secret());
        hash.update(MEDIA_KEY_DOMAIN);
        hash.update(self.channel.0.as_bytes());
        hash.update(&epoch.to_le_bytes());
        hash.update(&sender);
        hash.update(&stream);
        Ok(SymmetricKey::from_bytes(*hash.finalize().as_bytes()))
    }

    /// Encode for the session service's local socket to the app.
    ///
    /// # Errors
    ///
    /// [`Error::Serialization`] if encoding fails.
    pub fn to_bytes(&self) -> Result<Zeroizing<Vec<u8>>> {
        let mut wire = Wire {
            version: WIRE_VERSION,
            channel: self.channel,
            epoch: self.epoch,
            keys: self.keyring.to_wire(),
        };
        let encoded = postcard::to_allocvec(&wire)
            .map(Zeroizing::new)
            .map_err(|error| Error::Serialization(error.to_string()));
        for (_, key) in &mut wire.keys {
            key.zeroize();
        }
        encoded
    }

    /// Decode what [`Self::to_bytes`] produced, refusing trailing bytes and
    /// a current epoch whose key is missing.
    ///
    /// # Errors
    ///
    /// [`Error::Serialization`] or [`Error::InvalidState`] for malformed or
    /// inconsistent bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let (mut wire, rest) = postcard::take_from_bytes::<Wire>(bytes)
            .map_err(|error| Error::Serialization(error.to_string()))?;
        let keys = std::mem::take(&mut wire.keys);
        if !rest.is_empty() || wire.version != WIRE_VERSION {
            let mut keys = keys;
            for (_, key) in &mut keys {
                key.zeroize();
            }
            return Err(Error::Serialization(
                "malformed channel content keys".into(),
            ));
        }
        if keys.len() > crate::content::MAX_HISTORY_KEYS {
            return Err(Error::InvalidState("too many channel content keys".into()));
        }
        let keyring = ContentKeyring::from_wire(keys)?;
        if keyring.get(wire.epoch).is_none() {
            return Err(Error::MissingContentKey(wire.epoch));
        }
        Ok(Self {
            channel: wire.channel,
            epoch: wire.epoch,
            keyring,
        })
    }
}

impl std::fmt::Debug for ChannelContentKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelContentKeys")
            .field("channel", &self.channel)
            .field("epoch", &self.epoch)
            .field("keyring", &self.keyring)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChannelGroup, MemberKey};

    #[test]
    fn endpoint_keys_round_trip_and_open_what_the_group_sealed() {
        let channel = ChannelId::new();
        let group = ChannelGroup::create(channel, MemberKey::generate("owner").unwrap()).unwrap();
        let artifact = ArtifactId::new();
        let (metadata, sealed) = group
            .seal_content(artifact, ContentRetention::Delta, b"edit")
            .unwrap();
        let keys = group.content_keys().unwrap();
        let decoded = ChannelContentKeys::from_bytes(&keys.to_bytes().unwrap()).unwrap();
        assert_eq!(decoded.epoch(), group.epoch());
        assert_eq!(
            decoded
                .open_content(artifact, &metadata, &sealed)
                .unwrap()
                .as_slice(),
            b"edit"
        );
        let (metadata, sealed) = decoded
            .seal_content(artifact, ContentRetention::Checkpoint, b"state")
            .unwrap();
        let mut group = group;
        assert_eq!(
            group
                .open_content(artifact, &metadata, &sealed)
                .unwrap()
                .as_slice(),
            b"state"
        );
        let mut bytes = keys.to_bytes().unwrap().to_vec();
        bytes.push(0);
        assert!(ChannelContentKeys::from_bytes(&bytes).is_err());
        assert!(!format!("{decoded:?}").contains("key:"));
    }

    /// A scope's keys carry many units: each is named for its own channel, so a
    /// payload stays bound to the record it was written for, and a unit's
    /// earlier group's keys keep opening what they sealed.
    #[test]
    fn a_scopes_keys_serve_each_unit_and_keep_its_earlier_group_readable() {
        let scope = ChannelId::new();
        let group = ChannelGroup::create(scope, MemberKey::generate("owner").unwrap()).unwrap();
        let scope_keys = group.content_keys().unwrap();
        let (unit, other) = (ChannelId::new(), ChannelId::new());
        let unit_keys = scope_keys.for_unit(unit);
        assert_eq!(unit_keys.channel_id(), unit);
        assert_eq!(unit_keys.epoch(), scope_keys.epoch());

        // Sealed for the unit, opened by another endpoint of the scope once it
        // is handed the unit's keys.
        let artifact = ArtifactId::new();
        let (metadata, sealed) = unit_keys
            .seal_content(artifact, ContentRetention::Delta, b"minutes")
            .unwrap();
        assert_eq!(metadata.channel, unit);
        let elsewhere =
            ChannelContentKeys::from_bytes(&scope_keys.for_unit(unit).to_bytes().unwrap()).unwrap();
        assert_eq!(
            elsewhere
                .open_content(artifact, &metadata, &sealed)
                .unwrap()
                .as_slice(),
            b"minutes"
        );
        assert!(
            scope_keys
                .for_unit(other)
                .open_content(artifact, &metadata, &sealed)
                .is_err(),
            "another unit's keys refuse the payload"
        );

        // The unit's own group sealed before key scopes: its keys open what it
        // sealed, merged behind the scope's.
        let earlier_group =
            ChannelGroup::create(unit, MemberKey::generate("owner").unwrap()).unwrap();
        let earlier = earlier_group.content_keys().unwrap();
        let (old_metadata, old_sealed) = earlier
            .seal_content(artifact, ContentRetention::Delta, b"before scopes")
            .unwrap();
        let merged = unit_keys.with_earlier(&earlier).unwrap();
        assert_eq!(merged.epoch(), scope_keys.epoch());
        assert_eq!(
            merged
                .open_content(artifact, &old_metadata, &old_sealed)
                .unwrap()
                .as_slice(),
            b"before scopes"
        );
        assert_eq!(
            merged
                .open_content(artifact, &metadata, &sealed)
                .unwrap()
                .as_slice(),
            b"minutes"
        );
        // Keys of another channel are not merged: they would open nothing here.
        assert!(matches!(
            scope_keys.with_earlier(&earlier.for_unit(other)),
            Ok(keys) if keys.channel_id() == scope
        ));
    }

    #[test]
    fn record_content_secret_is_the_scopes_for_every_unit_and_moves_with_the_epoch() {
        let channel = ChannelId::new();
        let mut group =
            ChannelGroup::create(channel, MemberKey::generate("owner").unwrap()).unwrap();
        let scope = group.content_keys().unwrap();
        let secret = scope.record_content_secret().unwrap();
        assert_eq!(
            secret,
            scope
                .for_unit(ChannelId::new())
                .record_content_secret()
                .unwrap(),
            "every record the scope carries shares it"
        );
        assert_ne!(
            secret,
            ChannelGroup::create(ChannelId::new(), MemberKey::generate("other").unwrap())
                .unwrap()
                .content_keys()
                .unwrap()
                .record_content_secret()
                .unwrap(),
            "another scope has its own"
        );
        let joiner = MemberKey::generate("joiner").unwrap();
        group
            .add_member(joiner.fresh_key_package().unwrap())
            .unwrap();
        assert_ne!(
            secret,
            group
                .content_keys()
                .unwrap()
                .record_content_secret()
                .unwrap(),
            "a new epoch has a new one"
        );
    }

    #[test]
    fn media_keys_follow_the_epoch_sender_and_stream_and_need_the_epoch_key() {
        let channel = ChannelId::new();
        let group = ChannelGroup::create(channel, MemberKey::generate("owner").unwrap()).unwrap();
        let owner = group.content_keys().unwrap();
        // Another endpoint of the same channel and epoch, keys and all.
        let elsewhere = ChannelContentKeys::from_bytes(&owner.to_bytes().unwrap()).unwrap();
        let epoch = group.epoch();
        let key = owner.media_key(epoch, [1; 32], [0, 0]).unwrap();
        assert_eq!(key, elsewhere.media_key(epoch, [1; 32], [0, 0]).unwrap());
        assert_ne!(key, owner.media_key(epoch, [2; 32], [0, 0]).unwrap());
        assert_ne!(key, owner.media_key(epoch, [1; 32], [0, 1]).unwrap());
        assert_ne!(key, owner.media_key(epoch, [1; 32], [1, 0]).unwrap());
        assert!(matches!(
            owner.media_key(epoch + 1, [1; 32], [0, 0]),
            Err(Error::MissingContentKey(missing)) if missing == epoch + 1
        ));
    }
}
