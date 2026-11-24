//! The actors a device writes as: one handle per author on a device, each with a part only that
//! device has, so a change names who wrote it and no two of a person's computers write as one
//! actor.

/// Separates the handle of a [`device_actor`] from its device part.
pub const DEVICE_SEPARATOR: char = '\u{1f}';

/// How many bytes of a [`copy_writer`] name the device it was made on.
const DEVICE_PART: usize = 4;

/// The actor `handle` writes a document as from the copy of it whose writer is `device`: the
/// handle, which [`crate::document`]'s history reads back, then the start of that writer, which a
/// [`copy_writer`] makes the device's, and a part only that copy has. Automerge numbers each
/// actor's changes, so one person writing from two devices (or two copies on one) under the bare
/// handle would make two different changes with the same number, which no replica can merge.
#[must_use]
pub fn device_actor(handle: &[u8], device: &automerge::ActorId) -> Vec<u8> {
    use sha2::Digest as _;
    let writer = device.to_bytes();
    let digest = sha2::Sha256::digest(writer);
    let mut actor = handle.to_vec();
    actor.push(DEVICE_SEPARATOR as u8);
    actor.extend(hex::encode(writer.get(..DEVICE_PART).unwrap_or(writer)).into_bytes());
    actor.extend(hex::encode(digest.get(..4).unwrap_or_default()).into_bytes());
    actor
}

/// The writer of one copy of a record opened on the device `device` names: its start is the
/// device's, which every copy opened there shares, so what the person did in an earlier copy (before
/// a restart) is still told apart as done here; the rest is the copy's own, so no two copies
/// (two processes, or one opened after a crash before the last one's changes were kept) number
/// changes as one writer.
#[must_use]
pub fn copy_writer(device: &[u8]) -> automerge::ActorId {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(device);
    let mut writer = digest.get(..DEVICE_PART).unwrap_or_default().to_vec();
    let mut own = [0_u8; 12];
    // A copy without its own part would number changes as another copy does.
    #[allow(
        clippy::expect_used,
        reason = "the platform's randomness never fails here"
    )]
    getrandom::fill(&mut own).expect("randomness for a writer");
    writer.extend_from_slice(&own);
    automerge::ActorId::from(writer)
}

/// The part of a writer `device` that names the device it was made on, as [`device_actor`]
/// writes it after the handle.
#[must_use]
pub fn device_part(device: &automerge::ActorId) -> String {
    let writer = device.to_bytes();
    hex::encode(writer.get(..DEVICE_PART).unwrap_or(writer))
}
