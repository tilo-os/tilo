//! `~/.local/share/ush/` — where the runtime keeps everything.
//!
//! ```text
//! layers/persistent/       the layer a normal session writes to
//! layers/ephemeral/<id>/   one per `--ephemeral` session, destroyed at exit
//! sessions/<id>/           live session metadata: pid, profile, one-time list
//! audit/                   the broker's append-only log
//! policy.json              saved permission decisions
//! ```
//!
//! **The store is never bound into a guest.** A guest can write its own layer,
//! because the layer is the overlay upper it is standing on — but `policy.json`
//! and `audit/` are not reachable from inside at any path, so a guest cannot
//! grant itself a directory or edit the record of what it asked for.

use std::path::{Path, PathBuf};

use crate::layer::{Layer, LayerKind};

/// Environment override for the store root; set by tests or to run two
/// independent runtimes on one machine.
pub const STORE_ENV: &str = "TILO_USH_HOME";

/// The layer store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// The store at its default location, or wherever [`STORE_ENV`] points.
    pub fn discover() -> Self {
        if let Some(raw) = std::env::var_os(STORE_ENV) {
            return Self::at(PathBuf::from(raw));
        }
        let data = xdg_paths::data_dir().unwrap_or_else(|| {
            // No `$HOME` and no `$XDG_DATA_HOME` is a broken session, but it is
            // not a reason to write into the current directory.
            PathBuf::from(format!("/run/user/{}", unsafe { libc::geteuid() }))
        });
        Self::at(data.join("ush"))
    }

    /// The store rooted at an explicit path.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn layers_dir(&self) -> PathBuf {
        self.root.join("layers")
    }

    pub fn persistent_dir(&self) -> PathBuf {
        self.layers_dir().join("persistent")
    }

    pub fn ephemeral_dir(&self) -> PathBuf {
        self.layers_dir().join("ephemeral")
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }

    pub fn audit_dir(&self) -> PathBuf {
        self.root.join("audit")
    }

    pub fn policy_path(&self) -> PathBuf {
        self.root.join("policy.json")
    }

    /// Per-session scratch: the overlay `workdir`s, the broker socket, the
    /// rollback record for one-time installs.
    pub fn session_dir(&self, session_id: &str) -> PathBuf {
        self.sessions_dir().join(session_id)
    }

    /// The guest's home directory — one per store, shared by every session.
    /// This is what makes "write-isolated by default" concrete: the guest's
    /// `$HOME` is bound over the user's home, so a shell sees its own dotfiles
    /// and none of the user's files; `perm trust-dir` is the only way a user
    /// directory appears.
    pub fn guest_home(&self) -> PathBuf {
        self.root.join("home")
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
            self.layers_dir(),
            self.ephemeral_dir(),
            self.sessions_dir(),
            self.audit_dir(),
            self.guest_home(),
        ] {
            std::fs::create_dir_all(&dir)?;
        }
        // The store carries permission decisions and the record of what asked for
        // them; other users on the machine have no business reading either.
        restrict_to_owner(&self.root)?;
        Ok(())
    }

    /// The persistent layer, created if it does not exist yet.
    pub fn persistent_layer(&self) -> Layer {
        Layer::new(self.persistent_dir(), LayerKind::Persistent)
    }

    /// An ephemeral layer for one session.
    pub fn ephemeral_layer(&self, session_id: &str) -> Layer {
        Layer::new(self.ephemeral_dir().join(session_id), LayerKind::Ephemeral)
    }

    /// Every ephemeral layer currently on disk, newest name last.
    pub fn ephemeral_layers(&self) -> Vec<Layer> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(self.ephemeral_dir()) else {
            return out;
        };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        paths.sort();
        for p in paths {
            out.push(Layer::new(p, LayerKind::Ephemeral));
        }
        out
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

        // The documented paths.
        assert!(store.persistent_dir().ends_with("layers/persistent"));
        assert!(store.ephemeral_dir().ends_with("layers/ephemeral"));
        assert!(store.sessions_dir().ends_with("sessions"));
        assert!(store.audit_dir().ends_with("audit"));
        assert!(store.policy_path().ends_with("policy.json"));

        assert!(store.ephemeral_dir().is_dir());
        assert!(store.sessions_dir().is_dir());
        assert!(store.audit_dir().is_dir());

        // The store holds the user's permission decisions and the audit log. A
        // mode that let another account read them would make both worthless.
        let mode = std::fs::metadata(store.root())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o077,
            0,
            "store root is group/world accessible: {mode:o}"
        );

        // Ephemeral layers are listed from disk.
        assert!(store.ephemeral_layers().is_empty());
        std::fs::create_dir_all(store.ephemeral_dir().join("aaa")).unwrap();
        std::fs::create_dir_all(store.ephemeral_dir().join("bbb")).unwrap();
        let found: Vec<String> = store
            .ephemeral_layers()
            .iter()
            .map(|l| l.path().file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(found, vec!["aaa".to_string(), "bbb".to_string()]);

        // Ensure is idempotent (on a fresh store and on this populated one).
        let fresh = Store::at(dir.path().join("ush-fresh"));
        fresh.ensure().unwrap();
        fresh.ensure().unwrap();
        store.ensure().unwrap();
    }
}
