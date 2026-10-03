//! `~/.local/share/ush/` — where the runtime keeps what is not the record.
//!
//! ```text
//! environments/<record>/home/   an environment's home
//! environments/<record>/var/    its /var/lib and /var/cache
//! sessions/<id>/                live session scratch: generated /etc entries,
//!                               the broker socket, an ephemeral home
//! audit/                        the broker's append-only log
//! ```
//!
//! The environment records themselves are Terminal's (see [`crate::records`]);
//! [`Store::records_root`] says where they are.
//!
//! **The store is never bound into a guest.** A guest sees its own home and
//! `/var` state at their guest paths, never the store around them, so it
//! cannot reach another environment's home or edit the record of what it
//! asked for.

use std::path::{Path, PathBuf};

/// Environment override for the store root; set by tests or to run two
/// independent runtimes on one machine. With it set, the environment
/// records live under the store too.
pub const STORE_ENV: &str = "TILO_USH_HOME";

/// Where the environment records are, when not where [`Store::discover`]
/// would look. The supervisor hands both locations to the sessions it
/// starts, so a session always uses its supervisor's.
pub const RECORDS_ENV: &str = "TILO_USH_RECORDS";

/// Where granted directories' contents are, when not on this machine at the
/// paths the grants name: a grant of `/home/you/src` binds
/// `<grant root>/home/you/src` at `/home/you/src`. A relay sets it to the
/// member's own file area; unset, grants bind the machine's own paths.
pub const GRANT_ROOT_ENV: &str = "TILO_USH_GRANT_ROOT";

/// The runtime's store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    root: PathBuf,
    records: PathBuf,
    grant_root: Option<PathBuf>,
}

impl Store {
    /// The store at its default location, or wherever [`STORE_ENV`] and
    /// [`RECORDS_ENV`] point.
    pub fn discover() -> Self {
        let mut store = if let Some(raw) = std::env::var_os(STORE_ENV) {
            Self::at(PathBuf::from(raw))
        } else {
            let data = xdg_paths::data_dir().unwrap_or_else(|| {
                // No `$HOME` and no `$XDG_DATA_HOME` is a broken session, but it is
                // not a reason to write into the current directory.
                PathBuf::from(format!("/run/user/{}", unsafe { libc::geteuid() }))
            });
            Self {
                root: data.join("ush"),
                records: crate::records::terminal_data_home(&data),
                grant_root: None,
            }
        };
        if let Some(raw) = std::env::var_os(RECORDS_ENV) {
            store.records = PathBuf::from(raw);
        }
        store.grant_root = std::env::var_os(GRANT_ROOT_ENV)
            .map(PathBuf::from)
            .filter(|root| root.is_absolute());
        store
    }

    /// The store rooted at an explicit path, with the records inside it.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            records: root.join("records"),
            root,
            grant_root: None,
        }
    }

    /// This store with granted directories found below `root` (see
    /// [`GRANT_ROOT_ENV`]).
    #[must_use]
    pub fn with_grant_root(mut self, root: PathBuf) -> Self {
        self.grant_root = Some(root);
        self
    }

    /// Where granted directories' contents are, when not at their own paths.
    pub fn grant_root(&self) -> Option<&Path> {
        self.grant_root.as_deref()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The data home the environment records are kept under.
    pub fn records_root(&self) -> &Path {
        &self.records
    }

    pub fn environments_dir(&self) -> PathBuf {
        self.root.join("environments")
    }

    /// Everything one environment owns outside its record.
    pub fn environment_dir(&self, record: &str) -> PathBuf {
        self.environments_dir().join(record)
    }

    /// An environment's persistent home.
    pub fn environment_home(&self, record: &str) -> PathBuf {
        self.environment_dir(record).join("home")
    }

    /// An environment's `/var` state.
    pub fn environment_var(&self, record: &str) -> PathBuf {
        self.environment_dir(record).join("var")
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }

    pub fn audit_dir(&self) -> PathBuf {
        self.root.join("audit")
    }

    /// Per-session scratch: the generated `/etc` entries, the broker socket.
    pub fn session_dir(&self, session_id: &str) -> PathBuf {
        self.sessions_dir().join(session_id)
    }

    /// Create the directory skeleton. Idempotent.
    ///
    /// # Errors
    ///
    /// Returns an error if any of the store's subdirectories can't be
    /// created, or the store root's permissions can't be restricted to
    /// owner-only.
    pub fn ensure(&self) -> std::io::Result<()> {
        for dir in [
            self.root.clone(),
            self.environments_dir(),
            self.sessions_dir(),
            self.audit_dir(),
        ] {
            std::fs::create_dir_all(&dir)?;
        }
        // The store carries every environment's home and the record of what
        // asked for what; other users on the machine have no business reading
        // either.
        restrict_to_owner(&self.root)?;
        Ok(())
    }
}

fn restrict_to_owner(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o700);
    std::fs::set_permissions(path, perms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_lays_out_a_private_store_idempotently() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().join("ush"));
        store.ensure().unwrap();

        assert!(
            store
                .environment_home("default")
                .ends_with("environments/default/home")
        );
        assert!(
            store
                .environment_var("default")
                .ends_with("environments/default/var")
        );
        assert!(store.records_root().starts_with(store.root()));
        assert!(store.sessions_dir().is_dir());
        assert!(store.audit_dir().is_dir());

        let mode = std::fs::metadata(store.root())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o077,
            0,
            "store root is group/world accessible: {mode:o}"
        );

        store.ensure().unwrap();
    }
}
