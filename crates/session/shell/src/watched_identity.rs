//! The identity state the compositor's loop asks for, kept current by
//! workers: the loop never waits on the disk to learn who sits at this
//! device.
//!
//! The session started here reads each value once at startup, then a worker
//! reads it again whenever the directory it lives in changes
//! ([`tilo_files::native::Watched`]):
//!
//! - who the person is ([`session_user`]), which the loop asks on every
//!   frame;
//! - the person's public identity ([`ui::local_identity`]), which the top
//!   bar and in-process apps ask for on every update; and
//! - the identity registry ([`crate::identity_registry`]), which the loop asks
//!   which identity each room presents
//!   ([`tilo_identity_host::registry::IdentityRegistryStore::load_reconciled_prompt`]);
//!   and
//! - whether this session's person administers the device ([`device_admin`]),
//!   which the session actions ask whenever the palette or an agent lists
//!   them.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use tilo_files::native::Watched;
use ui::local_identity::{LocalIdentity, LocalIdentitySource, RuntimeIdentity};

/// The person's public identity, as [`RuntimeIdentity`] reads it, kept
/// current by a worker.
struct WatchedLocalIdentity(Watched<Option<LocalIdentity>>);

impl LocalIdentitySource for WatchedLocalIdentity {
    fn load(&self) -> Option<LocalIdentity> {
        (*self.0.get()).clone()
    }

    fn public_photo(&self) -> Option<([u8; 32], runtime_identity::ProfilePhoto)> {
        RuntimeIdentity.public_photo()
    }
}

/// The directories the person's identity record can live in: the identity
/// directory, and the directory of the record the launcher bound.
fn identity_directories() -> Vec<PathBuf> {
    runtime_identity::identity_dir_from_env()
        .ok()
        .into_iter()
        .chain(
            std::env::var_os(runtime_identity::IDENTITY_ENV_VAR)
                .map(PathBuf::from)
                .and_then(|record| record.parent().map(std::path::Path::to_path_buf)),
        )
        .collect()
}

/// Who the person here is ([`crate::shell::state::local_user_id`]).
static SESSION_USER: OnceLock<Watched<String>> = OnceLock::new();

/// Who the person here is, as a worker last read it; `None` in a process
/// that did not [`install`] it (a test), whose caller reads it itself.
#[must_use]
pub fn session_user() -> Option<String> {
    SESSION_USER.get().map(|user| (*user.get()).clone())
}

/// Whether the person here administers this device: certified as its owner,
/// and in its administrators' group.
static DEVICE_ADMIN: OnceLock<Watched<bool>> = OnceLock::new();

/// Whether the person here administers this device, as a worker last read
/// it; `None` in a process that did not [`install`] it (a test), whose
/// caller reads it itself.
#[must_use]
pub fn device_admin() -> Option<bool> {
    DEVICE_ADMIN.get().map(|admin| *admin.get())
}

/// [`device_admin`] read from the disk: the owner claim, this device's
/// certificate and key, the active identity record and the group database.
#[cfg(target_os = "linux")]
pub(crate) fn read_device_admin() -> bool {
    runtime_identity::identity_dir_from_env().is_ok_and(|identity| {
        crate::compositor::admin_control::authentication_session_decision(&identity).is_ok()
    })
}

/// Where [`read_device_admin`] reads.
#[cfg(target_os = "linux")]
fn device_admin_directories() -> Vec<PathBuf> {
    let paths = tilo_identity_host::provisioning::collaboration_identity_paths();
    runtime_identity::identity_dir_from_env()
        .ok()
        .into_iter()
        .chain(
            [paths.user_pubkey, paths.device_cert, paths.device_privkey]
                .iter()
                .filter_map(|path| path.parent().map(std::path::Path::to_path_buf)),
        )
        // The administrators' group lives in the group database.
        .chain(std::iter::once(PathBuf::from("/etc")))
        .collect()
}

/// Read the identity state once, here at startup, and keep it current from
/// then on. A value that cannot be watched is read where it is asked for,
/// as before.
pub fn install() {
    #[cfg(target_os = "linux")]
    match Watched::start(
        "device-admin",
        device_admin_directories(),
        read_device_admin,
    ) {
        Ok(watched) => {
            let _ = DEVICE_ADMIN.set(watched);
        }
        Err(error) => tracing::warn!(%error, "cannot watch who administers this device"),
    }
    let user_key = crate::firstboot::collaboration_identity_paths().user_pubkey;
    match Watched::start(
        "session-user",
        user_key
            .parent()
            .map(std::path::Path::to_path_buf)
            .into_iter()
            .collect(),
        crate::shell::state::read_local_user_id,
    ) {
        Ok(watched) => {
            let _ = SESSION_USER.set(watched);
        }
        Err(error) => tracing::warn!(%error, "cannot watch who the person here is"),
    }
    match Watched::start("local-identity", identity_directories(), || {
        RuntimeIdentity.load()
    }) {
        Ok(watched) => {
            ui::local_identity::install_source(Some(Arc::new(WatchedLocalIdentity(watched))));
        }
        Err(error) => tracing::warn!(%error, "cannot watch the person's identity"),
    }
    if let Ok(store) = crate::identity_registry::IdentityRegistryStore::from_env()
        && let Err(error) = store.load_reconciled_prompt()
    {
        tracing::warn!(%error, "cannot read the identity registry at startup");
    }
}
