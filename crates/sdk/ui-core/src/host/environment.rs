//! Host inputs — who the process is, where its files live, and how its user
//! asked to be served.
//!
//! Toolkit code takes these from [`environment`] (or from constructor
//! parameters such as [`crate::record::RecordSet::open_in`]) rather than
//! reading the process environment itself. Only the native runner derives
//! them from the process — environment variables, the settings file — in
//! [`crate::host::native::process_environment`]. Another host installs its
//! own with [`install_environment`].

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use crate::collaboration::ContinuationMetadata;

/// Who this process is and which workspace it serves.
///
/// Every field is optional: a process started outside a launcher has none,
/// and each reader applies its own documented fallback.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostIdentity {
    /// The installed package this process runs as.
    pub package_id: Option<String>,
    /// A development-time application id, used when no package id is set.
    pub development_app_id: Option<String>,
    /// The human-readable application name.
    pub app_name: Option<String>,
    /// The workspace this process collaborates in.
    pub workspace_id: Option<String>,
    /// The collaboration cluster, used when no workspace id is set.
    pub cluster_id: Option<String>,
    /// The signed-in person's id, when the launcher bound one.
    pub user_id: Option<String>,
    /// The presence handle to announce, overriding the user id.
    pub presence_handle: Option<String>,
    /// The operating-system account name.
    pub account_name: Option<String>,
    /// The resume projection this window was relaunched with.
    pub continuity_resume: Option<ContinuationMetadata>,
    /// The launcher-issued instance identity of this window.
    pub continuity_instance: Option<String>,
}

/// Where this process's user files live.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostPaths {
    /// The user's home as bound into the sandbox by the launcher. Presence
    /// means "you can read this", not merely "this is where it would be".
    pub user_home: Option<PathBuf>,
    /// The account's home directory.
    pub home: Option<PathBuf>,
    /// The per-user configuration directory.
    pub config_home: Option<PathBuf>,
}

impl HostPaths {
    /// The per-user configuration directory: [`Self::config_home`], else
    /// `.config` under [`Self::home`].
    pub fn config_dir(&self) -> Option<PathBuf> {
        self.config_home
            .clone()
            .or_else(|| self.home.as_ref().map(|home| home.join(".config")))
    }
}

/// Accessibility requests the host passes to the app at startup.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AccessibilityInputs {
    pub screen_reader: bool,
    pub high_contrast: bool,
    pub reduce_motion: bool,
    /// Text scale factor; `None` keeps the default.
    pub text_scale: Option<f32>,
    /// Interface scale factor; `None` keeps the default.
    pub ui_scale: Option<f32>,
}

/// Opt-in developer diagnostics for the runner and the toolkit.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunnerDiagnostics {
    /// Debug overlays (only honoured in debug builds).
    pub debug_overlay: bool,
    /// Per-frame performance logging.
    pub perf_log: bool,
    /// Disable the scroll-reposition layout fast path.
    pub disable_scroll_fast_path: bool,
    /// Disable the navigation split's cached content layout.
    pub disable_split_cache: bool,
    /// Request GPU timestamp queries.
    pub gpu_timestamps: bool,
    /// Request timestamp queries inside compute passes.
    pub gpu_dispatch_timestamps: bool,
    /// Show the in-app frame profiler HUD.
    pub profile_hud: bool,
    /// Append per-frame profiles to this file.
    pub profile_out: Option<PathBuf>,
    /// Record presentation feedback for a native performance scenario.
    pub presentation_profile: bool,
    /// Open windows without taking focus from the active application.
    pub window_no_activate: bool,
}

/// Everything the host tells a `ui` process about itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HostEnvironment {
    pub identity: HostIdentity,
    pub paths: HostPaths,
    pub accessibility: AccessibilityInputs,
    /// The user's preferred locale, as a POSIX or BCP 47 tag.
    pub locale: Option<String>,
    pub diagnostics: RunnerDiagnostics,
}

static INSTALLED: RwLock<Option<Arc<HostEnvironment>>> = RwLock::new(None);

/// Install `environment` for this process, replacing any earlier one.
pub fn install_environment(environment: HostEnvironment) {
    if let Ok(mut slot) = INSTALLED.write() {
        *slot = Some(Arc::new(environment));
    }
}

/// The host inputs for this process: the installed ones, else — in a native
/// build — what the native runner reads from the process, else none.
///
/// The native fallback is read afresh on each call, because the process
/// environment is the routing truth for a launched app and tests change it
/// between cases.
pub fn environment() -> Arc<HostEnvironment> {
    if let Some(installed) = INSTALLED.read().ok().and_then(|slot| slot.clone()) {
        return installed;
    }
    Arc::new(crate::host::native::process_environment())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_dir_falls_back_to_home() {
        let paths = HostPaths {
            home: Some("/home/ada".into()),
            ..HostPaths::default()
        };
        assert_eq!(paths.config_dir(), Some(PathBuf::from("/home/ada/.config")));
        let paths = HostPaths {
            home: Some("/home/ada".into()),
            config_home: Some("/cfg".into()),
            ..HostPaths::default()
        };
        assert_eq!(paths.config_dir(), Some(PathBuf::from("/cfg")));
    }
}
