use channel_crypto::ChannelContentKeys;
use collab_types::{ArtifactId, ChannelId, ContentRetention, EncryptionMetadata};

/// What picture `sequence` of the stream `parts` name under `domain` is bound
/// to: opened as another stream's or another place in it, it does not
/// authenticate.
pub(crate) fn artifact(domain: &[u8], parts: &[&[u8]], kind: u8, sequence: u64) -> ArtifactId {
    let mut hash = blake3::Hasher::new();
    hash.update(domain);
    for part in parts {
        hash.update(&u64::try_from(part.len()).unwrap_or(u64::MAX).to_le_bytes());
        hash.update(part);
    }
    collab_types::live_artifact_id(hash.finalize().as_bytes(), kind, sequence)
}

/// `plaintext` sealed as `artifact`: the epoch of the key, little-endian,
/// then the sealed bytes.
pub(crate) fn seal(
    keys: &ChannelContentKeys,
    artifact: ArtifactId,
    plaintext: &[u8],
) -> Option<Vec<u8>> {
    let (metadata, sealed) = keys
        .seal_content(artifact, ContentRetention::Delta, plaintext)
        .ok()?;
    let mut payload = Vec::with_capacity(sealed.len().saturating_add(8));
    payload.extend_from_slice(&metadata.epoch.to_le_bytes());
    payload.extend_from_slice(&sealed);
    Some(payload)
}

/// Open a payload [`seal`] made, or `None` when it does not authenticate.
pub(crate) fn open(
    keys: &ChannelContentKeys,
    channel: ChannelId,
    artifact: ArtifactId,
    payload: &[u8],
) -> Option<Vec<u8>> {
    let (epoch, sealed) = payload.split_first_chunk::<8>()?;
    let metadata = EncryptionMetadata::content_key(
        u64::from_le_bytes(*epoch),
        channel,
        ContentRetention::Delta,
    );
    keys.open_content(artifact, &metadata, sealed)
        .ok()
        .map(|plaintext| plaintext.to_vec())
}
