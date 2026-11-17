//! This computer's own login, as `tilo login` keeps it: the device key the
//! person's root certified for this computer, and what it reaches. Every
//! program of the login that acts as the person reads it here: `tilo`, and
//! `tilolang repl`.

use anyhow::{Context as _, Result, anyhow};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// What a login this computer cannot read says.
pub const LOGIN_DAMAGED: &str = "This computer's Tilo login is damaged: ask the person for a new code from their Tilo tab \
     (Set up with your agent).";

/// Where this account keeps the device: `$XDG_STATE_HOME/tilo/device`.
///
/// # Errors
/// This account has no state directory.
pub fn device_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("TILO_DEVICE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    Ok(tilo_paths::state_dir()
        .context("this account has no state directory (no HOME)")?
        .join("tilo/device"))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Device {
    pub site: String,
    pub host_origin: String,
    pub workspace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_name: Option<String>,
    pub user: String,
    pub agent: String,
    pub label: String,
    pub cert: String,
    pub host_cert: String,
}

impl Device {
    /// The device's certificate and the host's.
    ///
    /// # Errors
    /// The login is damaged.
    pub fn certs(
        &self,
    ) -> Result<(
        tilo_user_identity::DeviceCert,
        tilo_user_identity::DeviceCert,
    )> {
        let read = |hexed: &str| -> Result<tilo_user_identity::DeviceCert> {
            hex::decode(hexed)
                .ok()
                .and_then(|bytes| postcard::from_bytes(&bytes).ok())
                .ok_or_else(|| anyhow!(LOGIN_DAMAGED))
        };
        Ok((read(&self.cert)?, read(&self.host_cert)?))
    }
}

/// Keep `key` and `device` in `dir`: the key readable by this account only,
/// both replaced in one rename each.
///
/// # Errors
/// The files cannot be written.
pub fn store(dir: &Path, key: &SigningKey, device: &Device) -> Result<()> {
    let write = |name: &str, bytes: &[u8]| -> Result<()> {
        tilo_files::native::atomic::write_private(&dir.join(name), bytes)?;
        Ok(())
    };
    write("key", &key.to_bytes())?;
    write("device.json", &serde_json::to_vec_pretty(device)?)?;
    Ok(())
}

/// The key and the device kept in `dir`; `None` when this computer did not
/// log in.
///
/// # Errors
/// The login is damaged.
pub fn load(dir: &Path) -> Result<Option<(SigningKey, Device)>> {
    use std::os::unix::fs::PermissionsExt as _;
    let key_path = dir.join("key");
    if !key_path.exists() {
        return Ok(None);
    }
    // The key is this account's alone: one others can read is closed to
    // them here, rather than asking the person to log in over a mode bit.
    if std::fs::metadata(&key_path)?.permissions().mode() & 0o077 != 0
        && std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).is_err()
    {
        return Err(anyhow!(LOGIN_DAMAGED));
    }
    let damaged = |what: &str, why: &dyn std::fmt::Display| {
        tracing::debug!(%why, "reading this computer's Tilo {what}");
        anyhow!(LOGIN_DAMAGED)
    };
    let bytes: [u8; 32] = std::fs::read(&key_path)
        .map_err(|error| damaged("key", &error))?
        .try_into()
        .map_err(|_| anyhow!(LOGIN_DAMAGED))?;
    let device: Device = serde_json::from_slice(
        &std::fs::read(dir.join("device.json")).map_err(|error| damaged("login", &error))?,
    )
    .map_err(|error| damaged("login", &error))?;
    Ok(Some((SigningKey::from_bytes(&bytes), device)))
}

/// The workspace this computer's login entered: its id, and the name the
/// device keeps for it, which is how a command that acts in it says which
/// workspace it used. `None` when this computer is not logged in.
///
/// # Errors
/// The login is damaged.
pub fn entered_workspace() -> Result<Option<(String, String)>> {
    let Some((_, device)) = load(&device_dir()?)? else {
        return Ok(None);
    };
    let name = device
        .workspace_name
        .clone()
        .unwrap_or_else(|| device.workspace.clone());
    Ok(Some((device.workspace, name)))
}
