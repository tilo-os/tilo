//! The content keys the 0.1 releases kept, read so what they sealed stays
//! readable.
//!
//! Those releases gave every channel of a workspace a group of its own and
//! kept one sealed peer state per channel. Each op sealed then names its
//! channel and an epoch of that channel's group
//! ([`collab_types::EncryptionMetadata::released_channel`]), and opens only
//! under the content key that group derived for the epoch, bound to the
//! channel ([`crate::ContentKeyring::open`]). This build keys one group by
//! its workspace, so those groups are never joined or followed again: their
//! keys are read once from the state a release left, handed to the
//! workspace's group as history ([`crate::GroupPeer::adopt`]) and never
//! seal anything. The state itself is left where the release kept it.

use collab_types::{MlsAccessRequestV1, MlsGovernanceFrame};
use serde::Deserialize;
use tilo_seal::{Context, Label};
use zeroize::Zeroize as _;

use crate::content::RELEASED_EPOCHS;
use crate::persistence::released_member_keys;
use crate::{ContentKeyring, Error, Result, VaultKey};

/// Record the 0.1 releases bound a channel's sealed peer state to.
const PEER_RECORD: &[u8] = b"channel-crypto/channel-peer";
/// The peer state format the 0.1 releases wrote last; earlier ones were
/// brought to it by those releases as they read them.
const PEER_VERSION: u8 = 3;
/// Sealed peer state larger than this is refused.
const MAX_PEER_STATE: usize = 320 * 1024 * 1024;

/// A channel's peer state as the 0.1 releases wrote it, field for field.
/// Its frames decode as this build's: a channel id and a workspace id are
/// both one UUID, and nothing else in them moved.
#[derive(Deserialize)]
struct PeerFile {
    _version: u8,
    channel: uuid::Uuid,
    record: Vec<u8>,
    parent: Option<Vec<u8>>,
    _head: Option<MlsGovernanceFrame>,
    _branch: Vec<MlsGovernanceFrame>,
    _lost: Vec<([u8; 32], u64)>,
    _genesis: Option<[u8; 32]>,
    _seen_epoch: Option<u64>,
    _requests: Vec<MlsAccessRequestV1>,
    _outbox: Vec<MlsGovernanceFrame>,
    _issued: u64,
    orphaned: Vec<(u64, [u8; 32])>,
}

/// The channel a 0.1 release kept `sealed` for, and every content key that
/// state held: its group's, those of the group state before its last
/// commit, and those of branches it left behind. Each is kept at its
/// epoch with the top bit set, where no epoch of a workspace's group
/// reaches: it opens what that release sealed and never seals.
///
/// # Errors
///
/// [`Error::StateCrypto`] for state `vault` did not seal as a release's
/// peer, [`Error::OtherFormat`] for a format no 0.1 release wrote last, and
/// what decoding it returns.
pub fn released_channel_keys(
    sealed: &[u8],
    vault: &VaultKey,
) -> Result<(uuid::Uuid, ContentKeyring)> {
    if sealed.len() > MAX_PEER_STATE {
        return Err(Error::InvalidState("sealed peer state is too large".into()));
    }
    let plaintext = tilo_seal::envelope::open(
        &vault.0,
        Context::new(Label::GroupMlsState, PEER_RECORD),
        sealed,
    )
    .map_err(|_| Error::StateCrypto("state authentication failed"))?;
    if plaintext.first() != Some(&PEER_VERSION) {
        return Err(Error::OtherFormat);
    }
    let mut file: PeerFile = postcard::from_bytes(&plaintext)
        .map_err(|error| Error::Serialization(error.to_string()))?;
    let keys = (|| {
        let mut keys = ContentKeyring::from_wire(std::mem::take(&mut file.orphaned))?;
        keys.merge(&released_member_keys(&file.record, vault)?)?;
        if let Some(parent) = &file.parent {
            keys.merge(&released_member_keys(parent, vault)?)?;
        }
        // Kept where no epoch of the workspace's group reaches.
        let mut wire = keys.to_wire();
        for (epoch, _) in &mut wire {
            *epoch |= RELEASED_EPOCHS;
        }
        Ok((file.channel, ContentKeyring::from_wire(wire)?))
    })();
    file.record.zeroize();
    if let Some(parent) = &mut file.parent {
        parent.zeroize();
    }
    keys
}

#[cfg(test)]
mod tests;
