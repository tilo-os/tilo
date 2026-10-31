//! Who sits at this device and which identity each room presents, kept
//! current by workers: the session's loop never waits on the disk to learn
//! either.
//!
//! The session reads each once at startup, then a worker reads it again
//! whenever a directory it lives in changes ([`tilo_files::native::Watched`]):
//!
//! - the person's public identity and photo ([`ui::local_identity`]), which
//!   the workspace sheet's people rows and the apps naming an author ask
//!   for; and
//! - the identity registry
//!   ([`tilo_identity_host::registry::IdentityRegistryStore::load_reconciled_prompt`]),
//!   which every publish and frame asks which identity a room presents. A
//!   reconcile lists and parses every identity record, so it runs on the
//!   worker and the loop is handed the registry it produced.

use std::path::PathBuf;
use std::sync::Arc;

use tilo_files::native::Watched;
use ui::local_identity::{LocalIdentity, LocalIdentitySource, RuntimeIdentity};

/// Who sits at this device, as the loop asks for it.
#[derive(Clone)]
struct Present {
    /// Their public identity.
    identity: Option<LocalIdentity>,
    /// The public avatar bound to their root key, as [`RuntimeIdentity`]
    /// reads it: the file it lives in is read with the identity, on the
    /// worker, and an avatar a frame paints only clones pixels.
    photo: Option<([u8; 32], runtime_identity::ProfilePhoto)>,
}

/// [`Present`], kept current by a worker.
struct WatchedLocalIdentity(Watched<Present>);

impl LocalIdentitySource for WatchedLocalIdentity {
    fn load(&self) -> Option<LocalIdentity> {
        self.0.get().identity.clone()
    }

    fn public_photo(&self) -> Option<([u8; 32], runtime_identity::ProfilePhoto)> {
        self.0.get().photo.clone()
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

/// Read who sits here at startup, and keep it current from then on. A value
/// that cannot be watched is read where it is asked for, as before.
///
/// Called before the session builds what reads identity, so nothing is
/// served the directory's state from before the workers' first read: the
/// first read is this one, on this thread, with the watches already armed.
pub fn install() {
    match tilo_identity_host::registry::IdentityRegistryStore::from_env() {
        Ok(store) => {
            if let Err(error) = store.load_reconciled_prompt() {
                tracing::warn!(%error, "cannot read the identity registry at startup");
            }
        }
        Err(error) => tracing::warn!(%error, "the identity registry is not at hand"),
    }
    match Watched::start("local-identity", identity_directories(), || Present {
        identity: RuntimeIdentity.load(),
        photo: RuntimeIdentity.public_photo(),
    }) {
        Ok(watched) => {
            ui::local_identity::install_source(Some(Arc::new(WatchedLocalIdentity(watched))));
        }
        Err(error) => tracing::warn!(%error, "cannot watch the person's identity"),
    }
}
