//! The places of this login that first use reads and writes: the home a
//! prepared setup, a migration or a picker starts from, and the system
//! identity root that minting, restoring and pairing provision into.
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use tilo_user_identity::IdentityPaths;

/// Taken from the process environment once, where the shell builds the
/// view, so nothing behind the view reads the environment again.
#[derive(Clone, Debug, Default)]
pub(crate) struct SessionPaths {
    pub(crate) home: Option<PathBuf>,
    /// The account whose removable media a picker offers.
    pub(crate) user: Option<OsString>,
    pub(crate) identity: Option<SystemIdentity>,
    /// What this device calls itself to a device it pairs with.
    pub(crate) device_label: String,
}

#[derive(Clone, Debug)]
pub(crate) struct SystemIdentity {
    pub(crate) root: PathBuf,
    pub(crate) paths: IdentityPaths,
}

impl SessionPaths {
    pub(crate) fn from_process() -> Self {
        let root = std::env::var_os("TILO_IDENTITY_ROOT")
            .filter(|value| !value.is_empty())
            .map_or_else(|| PathBuf::from("/var/lib/tilo"), PathBuf::from);
        Self {
            home: crate::onboarding_prepared::session_home(),
            user: std::env::var_os("USER").filter(|value| !value.is_empty()),
            identity: Some(SystemIdentity {
                root,
                paths: crate::firstboot::collaboration_identity_paths(),
            }),
            device_label: std::env::var("HOSTNAME").unwrap_or_else(|_| "Tilo device".into()),
        }
    }

    /// OOBE is trusted compositor chrome, so its chooser follows the user rather
    /// than an application's capability scope. Keep roots narrow: the home tree
    /// plus conventional removable-media mount roots, never the whole filesystem.
    pub(crate) fn picker_scope(&self) -> (PathBuf, Vec<PathBuf>) {
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

    pub(crate) fn identity_paths(&self) -> Option<IdentityPaths> {
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

pub(crate) fn identity_root_unavailable() -> String {
    "this login has no system identity root".to_string()
}

#[cfg(test)]
impl super::OnboardingView {
    /// The production view of a login with no home and no system identity
    /// root: nothing outside `identity_dir` reaches it.
    pub(crate) fn isolated(identity_dir: PathBuf, recovery_only: bool) -> Self {
        Self::new_in_session(
            identity_dir,
            SessionPaths::default(),
            super::OnboardingContext::FullSystem,
            recovery_only,
        )
    }

    /// [`Self::isolated`] for a Tilo session on an existing Linux host.
    pub(crate) fn isolated_host_desktop(identity_dir: PathBuf) -> Self {
        Self::new_in_session(
            identity_dir,
            SessionPaths::default(),
            super::OnboardingContext::HostDesktop,
            false,
        )
    }
}
