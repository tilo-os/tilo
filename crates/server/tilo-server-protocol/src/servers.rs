//! The person's workspace, and how to find it from their root alone.
//!
//! It is an ordinary workspace whose only member is the person, and each of
//! their devices takes part in it. It holds what is the person's alone: their
//! servers (the `so.tilo.servers` resource), and with them their devices, the
//! list of their workspaces and the settings that follow them. Its id follows
//! from their root, so every one of their devices finds the same one without
//! being told ([`person_workspace_id`]).

/// One of the person's servers, as the document of their workspace names
/// it: what their other devices list, and where they reach it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PersonServer {
    /// Its device key, lowercase hex.
    pub device: String,
    /// The name people see for it.
    pub label: String,
    /// Where its service is dialled, as it last advertised it; `None` until it
    /// did.
    pub endpoint: Option<iroh_base::EndpointAddr>,
}

/// The disk recovery key of one of the person's machines, as the vault of their workspace keeps
/// it: what opens the machine's disk with nothing else. The machine keeps no copy.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RecoveryKey {
    /// The machine's device key, lowercase hex.
    pub machine: String,
    /// The name the machine was certified under.
    pub label: String,
    /// The recovery key.
    pub key: String,
}

impl std::fmt::Debug for RecoveryKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecoveryKey")
            .field("machine", &self.machine)
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

/// The id of the workspace of the person whose root is `person`, which the workspace policy
/// derives so that every process that decides a record's changes finds the workspace holding it.
pub use tilo_action_policy::roles::person_workspace_id;

/// The device of the person that approved another, as an enrollment's answer
/// names it: its certificate and where it is dialled. The person's first
/// device has none.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Introducer {
    /// The certificate of the device that holds the workspace, which the
    /// person's root signed.
    pub device: tilo_user_identity::DeviceCert,
    /// Where that device is dialled.
    pub endpoint: iroh_base::EndpointAddr,
}

/// Where a device keeps the [`Introducer`] it was admitted with, below its
/// identity root, until it holds the person's workspace.
#[must_use]
pub fn introducer_path(identity_root: &std::path::Path) -> std::path::PathBuf {
    identity_root.join("peers").join("introducer.json")
}

/// Keep `introducer` below `identity_root`, or let the one kept there go
/// (`None`), once this device holds the person's workspace or never needs one.
///
/// # Errors
/// The file could not be replaced or removed.
pub fn keep_introducer(
    identity_root: &std::path::Path,
    introducer: Option<&Introducer>,
) -> std::io::Result<()> {
    let path = introducer_path(identity_root);
    let Some(introducer) = introducer else {
        return match std::fs::remove_file(&path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        };
    };
    let encoded = serde_json::to_vec_pretty(introducer)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    // A certificate and an address are public; the device's service reads
    // the file whatever account wrote it.
    tilo_files::native::atomic::write_creating_parent_with_mode(&path, &encoded, 0o644)
}

impl Introducer {
    /// The introducer as `person`'s device approving another hands it: its
    /// own certificate, and the address its service answers on, which
    /// follows from its key.
    #[must_use]
    pub fn of_device(
        device: tilo_user_identity::DeviceCert,
        device_secret: &ed25519_dalek::SigningKey,
    ) -> Self {
        let service =
            iroh_base::SecretKey::from_bytes(&crate::server_node_secret(&device_secret.to_bytes()));
        Self {
            device,
            endpoint: iroh_base::EndpointAddr::new(service.public()),
        }
    }

    /// Whether this names a device of `person`'s whose certificate verifies.
    ///
    /// # Errors
    /// It names another person's device, or its certificate does not verify.
    pub fn checked(&self, person: &[u8; 32]) -> Result<(), String> {
        if self.device.user_public_key != *person {
            return Err("the introducer is another person's device".into());
        }
        tilo_user_identity::verify_device_cert(&self.device)
            .map_err(|error| format!("the introducer's certificate does not verify: {error}"))
    }
}
