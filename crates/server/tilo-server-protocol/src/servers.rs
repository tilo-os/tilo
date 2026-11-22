//! The person's workspace, and how to find it from their root alone.
//!
//! It is an ordinary workspace whose only member is the person, and each of
//! their devices takes part in it. It holds what is the person's alone: their
//! servers (the `so.tilo.servers` resource), and with them their devices, the
//! list of their workspaces and the settings that follow them. Its id follows
//! from their root, so every one of their devices finds the same one without
//! being told ([`person_workspace_id`]).

/// The id of the workspace of the person whose root is `person`: the same on
/// each of their devices, so none has to be told it.
#[must_use]
pub fn person_workspace_id(person: &[u8; 32]) -> String {
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

/// The device a device of the person asks to take it into the person's
/// workspace: one that holds the workspace, as the device that approved the
/// asking one knew it. The person's first device has none, and founds the
/// workspace itself.
///
/// The asking device calls the `person.take-in` action of the device named
/// here with its own endpoint, holds the workspace from then on, and lets the
/// introducer go once it does.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Introducer {
    /// The certificate of the device that holds the workspace, which the
    /// person's root signed.
    pub device: tilo_user_identity::DeviceCert,
    /// Where that device is dialled.
    pub endpoint: iroh_base::EndpointAddr,
}
