//! The shell's own resources, kept as the person's: where they live on this device, and how they
//! reach the person's other devices.
//!
//! The shell keeps the person's shell choices in their own workspace (`resources: .own`): the
//! style, the bar and the dock, the strips' and the frames' settings, and the person's layout.
//! They are the person's as an app's own resources are, so they are kept beside those: a store of
//! the shell's own, in the directory the launcher binds into every app as `TILO_PERSON_DIR`,
//! which writes each change as the shell's parts make it.
//!
//! Between the person's devices they travel in their own workspace
//! ([`crate::PersonResources`]), which the device's daemon keeps: what it holds of the shell's
//! documents is merged here (`Shell::sync_resource`), and the session never writes it. Each
//! document is named there as the store names it here, under the shell's app: the name the
//! device's dispatch writes when it runs the shell's actions (an agent's `shell#show-style`, a
//! press on a desktop page), so the person's shell is one set of documents, whoever changes it.
//!
//! A store this build cannot read is never written over: the session keeps the shell's resources
//! in memory for that session alone and says so (`ShellRecord::take_refusal`), so a person is
//! never shown a shell whose choices were quietly reset.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use tilolang_tilo::resources::{LocalPort, ResourceStore};

/// The app whose resources these are: the shell package the session runs, which declares
/// `resources: .own` (`apps/tl/shell/app.tl`).
pub(crate) const APP: &str = "so.tilo.shell";

/// Where the shell's resources live in this session, and what became of the ones it kept.
pub(crate) struct ShellRecord {
    port: Rc<LocalPort>,
    /// The store the shell's parts draw from and write into, shared by every part.
    store: ResourceStore,
    /// Where the store is kept on this device, while it is.
    path: Option<PathBuf>,
    /// Why the store on disk was not opened, once: the person is told, and is not told again.
    refused: RefCell<Option<String>>,
}

impl ShellRecord {
    /// The shell's resources when this device keeps state at `state` (`None` on a session that
    /// keeps nothing — a test's, a screenshot's — which draws the defaults and writes nothing),
    /// read back from what an earlier session kept on this device; none on a device that never
    /// showed the shell, whose defaults this session's first part seeds by the package's
    /// migrations.
    pub(crate) fn open(state: Option<&Path>) -> Self {
        let Some(dir) = state.and_then(|_| person_dir()) else {
            return Self::keeping(LocalPort::in_memory(), None, None);
        };
        let dir = dir.join(APP).join("resources");
        match LocalPort::at(dir.clone()) {
            Ok(port) => Self::keeping(port, Some(dir), None),
            Err(error) => {
                tracing::error!(
                    %error,
                    "the shell's resources are not opened; the shell starts from the defaults \
                     this session"
                );
                Self::keeping(LocalPort::in_memory(), None, Some(error))
            }
        }
    }

    fn keeping(port: LocalPort, path: Option<PathBuf>, refused: Option<String>) -> Self {
        let port = Rc::new(port);
        let store = ResourceStore::new(Rc::clone(&port) as Rc<dyn tilolang_tilo::resources::Port>);
        Self {
            port,
            store,
            path,
            refused: RefCell::new(refused),
        }
    }

    /// The store the shell's parts draw from and write into.
    #[must_use]
    pub(crate) fn store(&self) -> ResourceStore {
        self.store.clone()
    }

    /// Where the store is kept on this device, while it is.
    #[must_use]
    pub(crate) fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Why the store on disk was not opened, once: the person is told, and is not told again.
    pub(crate) fn take_refusal(&self) -> Option<String> {
        self.refused.borrow_mut().take()
    }

    /// Take in `theirs`, what another of the person's devices holds of the document `doc`, so
    /// this session's shell draws it. Whether it changed.
    pub(crate) fn take_in(&self, doc: &str, theirs: &[u8]) -> bool {
        match self.port.merge(doc, theirs) {
            Ok(changed) => {
                if changed {
                    // What was read of it before is not what it holds now.
                    self.store.moved();
                }
                changed
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    "the shell's resources did not take in what another device holds"
                );
                false
            }
        }
    }
}

/// Where the person's own records are kept on this device: the directory the launcher binds into
/// an app as `TILO_PERSON_DIR`, else `$XDG_DATA_HOME/tilo-person`, where the launcher keeps it
/// (`launcher::appdir`). What every app's own record of the person's is kept in, so the shell's
/// are there with them.
fn person_dir() -> Option<PathBuf> {
    std::env::var_os("TILO_PERSON_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            launcher::appdir::host_data_home().map(|home| home.join(launcher::appdir::PERSON_DIR))
        })
}
