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

/// Where a device keeps the [`Introducer`] it was admitted with, below its
/// identity root, until it holds the person's workspace.
#[must_use]
pub fn introducer_path(identity_root: &std::path::Path) -> std::path::PathBuf {
    identity_root.join("peers").join("introducer.json")
}

/// Largest introducer file read.
const INTRODUCER_FILE_LIMIT: usize = 64 * 1024;

/// The introducer this device keeps below `identity_root`, if it keeps one: a
/// device of `person` whose certificate verifies.
///
/// # Errors
/// The file cannot be read, is larger than 64 KiB, or names no device of
/// `person`'s.
pub fn read_introducer(
    identity_root: &std::path::Path,
    person: &[u8; 32],
) -> std::io::Result<Option<Introducer>> {
    let invalid = |message: String| std::io::Error::new(std::io::ErrorKind::InvalidData, message);
    let bytes = match std::fs::read(introducer_path(identity_root)) {
        Ok(bytes) if bytes.len() <= INTRODUCER_FILE_LIMIT => bytes,
        Ok(_) => return Err(invalid("the introducer exceeds 64 KiB".into())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let introducer: Introducer = serde_json::from_slice(&bytes)
        .map_err(|error| invalid(format!("the introducer is not one: {error}")))?;
    introducer.checked(person).map_err(invalid)?;
    Ok(Some(introducer))
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
