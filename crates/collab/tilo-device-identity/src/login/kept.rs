//! A login as this computer keeps it: [`Place`], [`load`] and [`store`].

use anyhow::{Context as _, Result, anyhow, bail};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tilo_user_identity::DeviceCert;

use super::ServerAddress;
use crate::IdentityPaths;
use crate::persist::{load_device_key, read_device_cert, write_device_cert};

/// What a login this computer cannot read says.
pub const LOGIN_DAMAGED: &str = "This computer's Tilo login is damaged: log in again with a ticket \
     `tilo device add` prints on one of your devices.";

/// What a login says whose certificate is for a device key this computer no longer has.
const LOGIN_STALE: &str = "This computer's Tilo login is for a device key it no longer has: \
     log in again with a ticket `tilo device add` prints on one of your devices.";

/// The file in [`Place::dir`] a login is kept in.
const LOGIN_FILE: &str = "login.json";

/// Where this account keeps its login and what the login keeps on this computer (its folders,
/// its background process's state): `$XDG_STATE_HOME/tilo/device`. The device key and its certificates are
/// this computer's, kept where its session keeps them.
#[derive(Clone, Debug)]
pub struct Place {
    dir: PathBuf,
    keys: IdentityPaths,
}

impl Place {
    /// This account's place: its state directory's, with `keys`, the device key and
    /// certificates its session keeps
    /// (`tilo_identity::provisioning::collaboration_identity_paths`).
    ///
    /// # Errors
    /// This account has no state directory.
    pub fn here(keys: IdentityPaths) -> Result<Self> {
        Ok(Self {
            dir: tilo_paths::state_dir()
                .context("this account has no state directory (no HOME)")?
                .join("tilo/device"),
            keys,
        })
    }

    /// A login kept in `dir`, with the device key and certificates `keys` names.
    #[must_use]
    pub fn at(dir: PathBuf, keys: IdentityPaths) -> Self {
        Self { dir, keys }
    }

    /// The directory the login is kept in.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// This computer's device key and certificates.
    #[must_use]
    pub fn keys(&self) -> &IdentityPaths {
        &self.keys
    }

    /// Whether this computer logged in, without reading the device key: a process that reads it
    /// makes itself undumpable.
    #[must_use]
    pub fn logged_in(&self) -> bool {
        self.dir.join(LOGIN_FILE).exists()
    }
}

/// A login: where this computer goes, and as which device of the person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub site: String,
    pub host_origin: String,
    pub workspace: String,
    pub workspace_name: Option<String>,
    pub agent: String,
    /// The certificate the person's root signed for this computer's device key.
    pub cert: DeviceCert,
    /// The host's own certificate, as the login was answered it.
    pub server_cert: DeviceCert,
    /// Where the server is dialed over iroh, as its signed invitation named it: a server reached
    /// that way only (behind a NAT, through a packet relay) has no web origin to ask.
    pub server_address: Option<ServerAddress>,
}

impl Device {
    /// How the person's devices name this computer.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.cert.device_label
    }

    /// The person's user public key, hex.
    #[must_use]
    pub fn user(&self) -> String {
        hex::encode(self.cert.user_public_key)
    }
}

/// A login as [`LOGIN_FILE`] keeps it: the certificate is kept with the device key, and found by
/// the person it names.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Kept {
    site: String,
    host_origin: String,
    workspace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workspace_name: Option<String>,
    user: String,
    agent: String,
    /// The host's certificate, its canonical bytes in hex.
    server_cert: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    server_address: Option<ServerAddress>,
}

/// Keep `device` in `place`: its certificate beside this computer's device key, which it must
/// certify, and the rest in [`LOGIN_FILE`].
///
/// # Errors
/// The certificate is for another key than this computer's, or the files cannot be written.
pub fn store(place: &Place, device: &Device) -> Result<()> {
    let key = load_device_key(&place.keys).context("reading this computer's device key")?;
    if device.cert.device_public_key != key.verifying_key().to_bytes() {
        bail!("the login's certificate is for another device key than this computer's");
    }
    write_device_cert(
        &place
            .keys
            .device_cert_for_user(&device.cert.user_public_key),
        &device.cert,
    )?;
    let kept = Kept {
        site: device.site.clone(),
        host_origin: device.host_origin.clone(),
        workspace: device.workspace.clone(),
        workspace_name: device.workspace_name.clone(),
        user: device.user(),
        agent: device.agent.clone(),
        server_cert: hex::encode(tilo_user_identity::canonical::canonical_bytes(
            &device.server_cert,
        )?),
        server_address: device.server_address.clone(),
    };
    tilo_files::native::atomic::write_private(
        &place.dir.join(LOGIN_FILE),
        &serde_json::to_vec_pretty(&kept)?,
    )?;
    Ok(())
}

/// This computer's device key and the login kept in `place`; `None` when this computer did not
/// log in.
///
/// # Errors
/// The login is damaged, or its certificate is for a device key this computer no longer has.
pub fn load(place: &Place) -> Result<Option<(SigningKey, Device)>> {
    let damaged = |what: &str, why: &dyn std::fmt::Display| {
        tracing::debug!(%why, "reading this computer's Tilo {what}");
        anyhow!(LOGIN_DAMAGED)
    };
    let bytes = match std::fs::read(place.dir.join(LOGIN_FILE)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(damaged("login", &error)),
    };
    let kept: Kept = serde_json::from_slice(&bytes).map_err(|error| damaged("login", &error))?;
    let user: [u8; 32] = hex::decode(&kept.user)
        .ok()
        .and_then(|user| user.try_into().ok())
        .ok_or_else(|| anyhow!(LOGIN_DAMAGED))?;
    let key = load_device_key(&place.keys).map_err(|error| damaged("device key", &error))?;
    let cert = read_device_cert(&place.keys.device_cert_for_user(&user))
        .map_err(|error| damaged("device certificate", &error))?;
    if cert.device_public_key != key.verifying_key().to_bytes() || cert.user_public_key != user {
        bail!(LOGIN_STALE);
    }
    let server_cert = hex::decode(&kept.server_cert)
        .ok()
        .and_then(|bytes| tilo_user_identity::canonical::from_canonical_bytes(&bytes).ok())
        .ok_or_else(|| anyhow!(LOGIN_DAMAGED))?;
    Ok(Some((
        key,
        Device {
            site: kept.site,
            host_origin: kept.host_origin,
            workspace: kept.workspace,
            workspace_name: kept.workspace_name,
            agent: kept.agent,
            cert,
            server_cert,
            server_address: kept.server_address,
        },
    )))
}

/// The workspace this computer's login entered: its id, and the name the
/// device keeps for it, which is how a command that acts in it says which
/// workspace it used, with this computer's device key and certificates at `keys`. `None` when
/// this computer is not logged in.
///
/// # Errors
/// The login is damaged.
pub fn entered_workspace(keys: IdentityPaths) -> Result<Option<(String, String)>> {
    let Some((_, device)) = load(&Place::here(keys)?)? else {
        return Ok(None);
    };
    let name = device
        .workspace_name
        .clone()
        .unwrap_or_else(|| device.workspace.clone());
    Ok(Some((device.workspace, name)))
}

/// Where this account's device holders listen: the session's runtime
/// directory when it has one, else one of this account's in the temporary
/// directory, made so that only this account enters it. One someone else
/// made, or others may enter, is refused: whoever reaches a holder acts as
/// the person. Short, since a socket's path is (about a hundred bytes), and a
/// home's state directory need not be.
///
/// # Errors
/// `login_dir` is not readable, or the directory is not this account's own.
#[cfg(unix)]
pub fn holder_dir(login_dir: &Path) -> Result<PathBuf, String> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
    let owner = std::fs::metadata(login_dir)
        .map_err(|error| format!("{} is not readable: {error}", login_dir.display()))?
        .uid();
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|runtime| runtime.is_absolute() && runtime.is_dir())
        .map_or_else(
            || std::env::temp_dir().join(format!("tilo-{owner}")),
            |runtime| runtime.join("tilo"),
        );
    if let Err(error) = std::fs::DirBuilder::new().mode(0o700).create(&dir)
        && error.kind() != std::io::ErrorKind::AlreadyExists
    {
        return Err(format!("{} could not be made: {error}", dir.display()));
    }
    let made = std::fs::symlink_metadata(&dir)
        .map_err(|error| format!("{} is not readable: {error}", dir.display()))?;
    if !made.is_dir() || made.uid() != owner || made.mode() & 0o077 != 0 {
        return Err(format!(
            "{} is not this account's own private directory",
            dir.display()
        ));
    }
    Ok(dir)
}

/// What the holder of the device whose public key is `key` names its files
/// by.
#[must_use]
pub fn holder_name(key: &[u8; 32]) -> String {
    format!("session-{}", hex::encode(&key[..8]))
}

/// The socket the holder named `name` serves Tilo's API on, in `dir`
/// ([`holder_dir`]): as this computer's device, over its one session.
#[must_use]
pub fn holder_api_socket(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.api.sock"))
}

/// Where this computer's device holder serves Tilo's API, when this computer
/// logged in: every program of the login acts as the device there, over the
/// device's one session, and never opens a second one under the same key,
/// which would cut the first off at the server. `keys` are this computer's device key and
/// certificates.
///
/// # Errors
/// The login is damaged, or the holders' directory is not this account's.
#[cfg(unix)]
pub fn held_api(keys: IdentityPaths) -> Result<Option<PathBuf>> {
    let place = Place::here(keys)?;
    let Some((key, _)) = load(&place)? else {
        return Ok(None);
    };
    let holders = holder_dir(place.dir()).map_err(|why| anyhow!(why))?;
    Ok(Some(holder_api_socket(
        &holders,
        &holder_name(&key.verifying_key().to_bytes()),
    )))
}
