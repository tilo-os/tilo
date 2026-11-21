//! The person's own workspace, and how to find it from their root alone.
//!
//! It is an ordinary workspace whose only member is the person, in which each
//! of their devices takes part. It holds what is the person's alone: their
//! servers (the `so.tilo.servers` document), and with them their devices,
//! the list of their workspaces and the settings that follow them. Its id
//! follows from their root, so every one of their devices finds the same one
//! without being told ([`own_workspace_id`]).

/// The id of the own workspace of the person whose root is `person`: the
/// same on each of their devices, so none has to be told it.
#[must_use]
pub fn own_workspace_id(person: &[u8; 32]) -> String {
    // The domain names the workspace's first use, the person's servers; the
    // ids of the workspaces already founded follow from it.
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
