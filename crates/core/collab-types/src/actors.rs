//! The actors a device writes as: one handle per author on a device, each with a part only that
//! device has, so a change names who wrote it and no two of a person's computers write as one
//! actor.

/// Separates the handle of a [`device_actor`] from its device part.
pub const DEVICE_SEPARATOR: char = '\u{1f}';

/// The actor `handle` writes a document as from the device whose writer is `device`: the handle,
/// which [`crate::document`]'s history reads back, and a part only that device has. Automerge numbers each actor's changes, so one person writing from two
/// devices under the bare handle would make two different changes with the same number, which no
/// replica can merge.
#[must_use]
pub fn device_actor(handle: &[u8], device: &automerge::ActorId) -> Vec<u8> {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(device.to_bytes());
    let mut actor = handle.to_vec();
    actor.push(DEVICE_SEPARATOR as u8);
    actor.extend(hex::encode(digest.get(..4).unwrap_or_default()).into_bytes());
    actor
}
