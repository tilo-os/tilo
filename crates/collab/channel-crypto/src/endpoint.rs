//! What an application endpoint holds for one channel: content keys only.
//!
//! A device keeps its MLS member state (signer, HPKE and ratchet secrets)
//! in its trusted session service. An application process that edits the
//! channel's document gets a [`ChannelContentKeys`]: the current epoch and
//! every content key the device holds. That is enough to seal new content
//! and open history, and not enough to create commits, admit or remove
//! anyone, or answer a Welcome. When the device's group moves to a new
//! epoch the service hands the app a fresh value.

use collab_types::{ArtifactId, ChannelId, ContentRetention, EncryptionMetadata, EncryptionScheme};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize as _, Zeroizing};

use crate::content::ContentKeyring;
use crate::{Error, Result};

const WIRE_VERSION: u8 = 1;

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

    /// The channel these keys belong to.
    pub fn channel_id(&self) -> ChannelId {
        self.channel
    }

    /// The epoch new content is sealed under.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Whether the key of `epoch` is held.
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
}
