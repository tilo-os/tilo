//! The places of this login that first use reads and writes: the home a
//! picker starts from, and the system identity that minting, restoring and
//! adding this computer with a code provision into.
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use tilo_device_identity::IdentityPaths;

/// Taken from the process environment once, where the shell builds the
/// view, so nothing behind the view reads the environment again.
#[derive(Clone, Debug, Default)]
pub struct SessionPaths {
    pub home: Option<PathBuf>,
    /// The account whose removable media a picker offers.
    pub user: Option<OsString>,
    pub identity: Option<SystemIdentity>,
    /// What this device calls itself to a device it pairs with.
    pub device_label: String,
}

#[derive(Clone, Debug)]
pub struct SystemIdentity {
    pub paths: IdentityPaths,
}

impl SessionPaths {
    pub fn from_process() -> Self {
        Self {
            home: crate::identity::session_home(),
            user: std::env::var_os("USER").filter(|value| !value.is_empty()),
            identity: Some(SystemIdentity {
                paths: crate::firstboot::collaboration_identity_paths(),
            }),
            device_label: std::env::var("HOSTNAME").unwrap_or_else(|_| "Tilo device".into()),
        }
    }

    /// OOBE is trusted compositor chrome, so the file it asks for is looked for
    /// where the user's files are rather than in an application's capability
    /// scope. Keep roots narrow: the home tree plus conventional removable-media
    /// mount roots, never the whole filesystem.
    pub fn picker_scope(&self) -> (PathBuf, Vec<PathBuf>) {
        let home = self
            .home
            .clone()
            .filter(|path| path.is_absolute() && path.is_dir());
        let mut roots = Vec::new();
        if let Some(home) = home.as_ref() {
            roots.push(home.clone());
        }
        let mut media_roots = Vec::new();
        if let Some(user) = &self.user {
            for base in [PathBuf::from("/run/media"), PathBuf::from("/media")] {
                let candidate = base.join(user);
                if candidate.is_dir() {
                    media_roots.push(candidate);
                }
            }
        }
        if media_roots.is_empty() && PathBuf::from("/media").is_dir() {
            media_roots.push(PathBuf::from("/media"));
        }
        let mounted = media_roots
            .iter()
            .find(|root| directory_has_subdirectory(root))
            .cloned();
        roots.extend(media_roots);
        let initial = mounted
            .or_else(|| home.clone())
            .unwrap_or_else(std::env::temp_dir);
        if roots.is_empty() {
            roots.push(initial.clone());
        }
        (initial, roots)
    }

    pub fn identity_paths(&self) -> Option<IdentityPaths> {
        self.identity
            .as_ref()
            .map(|identity| identity.paths.clone())
    }
}

fn directory_has_subdirectory(path: &Path) -> bool {
    std::fs::read_dir(path).is_ok_and(|entries| {
        entries.filter_map(Result::ok).any(|entry| {
            entry
                .file_type()
                .is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
        })
    })
}

#[cfg(any(test, feature = "test-support"))]
impl super::Ceremony {
    /// The production view of a login with no home and no system identity
    /// root: nothing outside `identity_dir` reaches it.
    pub fn isolated(identity_dir: PathBuf, recovery_only: bool) -> Self {
        Self::new_in_session(
            identity_dir,
            SessionPaths::default(),
            super::OnboardingContext::FullSystem,
            recovery_only,
        )
    }
}
