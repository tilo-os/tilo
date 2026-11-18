//! The Servers workspace of a person: the one workspace that holds what their
//! servers are, and how to find it from their root alone.
//!
//! It is an ordinary workspace whose only member is the person, in which each
//! of their devices takes part. Its id follows from their root, so every one
//! of them finds the same one without being told ([`admin_workspace_id`]).

/// The id of the admin workspace of the person whose root is `person`: the
/// same on each of their devices, so none has to be told it.
#[must_use]
pub fn admin_workspace_id(person: &[u8; 32]) -> String {
    let hash = blake3::Hasher::new()
        .update(b"tilo admin workspace v1\0")
        .update(person)
        .finalize();
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    uuid::Builder::from_random_bytes(bytes)
        .into_uuid()
        .to_string()
}
