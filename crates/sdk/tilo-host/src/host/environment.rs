//! Host inputs — who the process is, where its files live, and how its user
//! asked to be served.
//!
//! Toolkit code takes these from [`environment`] (or from constructor
//! parameters such as `ui::record::RecordSet::open_in`) rather than
//! reading the process environment itself. Only the native runner derives
//! them from the process — environment variables, the settings file — in
//! `ui::native::services::process_environment`. Another host installs its
//! own with [`install_environment`].

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use unic_langid::LanguageIdentifier;

/// Inert, app-owned resume projection sent only to the local shell.
///
/// The bytes are deliberately opaque to the toolkit so each app can version
/// its CRDT-derived schema. The shell validates size and path redaction before
/// retaining them for same-identity continuation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContinuationMetadata {
    pub schema_version: u16,
    pub bytes: Vec<u8>,
}

/// Decode a hex resume payload and its schema version. Invalid input is
/// ignored so an app outside Tilo or an older sender opens normally.
#[cfg(feature = "native")]
pub(crate) fn decode_continuation(hex: &str, schema: &str) -> Option<ContinuationMetadata> {
    let bytes = hex::decode(hex).ok()?;
    let schema_version = schema.parse().ok()?;
    (!bytes.is_empty() && bytes.len() <= 32 * 1024).then_some(ContinuationMetadata {
        schema_version,
        bytes,
    })
}

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
    /// The launcher configured a collaboration service for this process, so
    /// its absence is a failure rather than a standalone launch.
    pub collab_service_configured: bool,
    /// The process runs as an admitted managed agent, which receives task work
    /// on its own broker queue instead of the room's task-run stream.
    pub managed_agent_context: bool,
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
    /// This process's own cache directory (`XDG_CACHE_HOME`). In a sandbox it
    /// is the app's private, persistent `.cache`, which no other app reads.
    pub cache_home: Option<PathBuf>,
    /// The session's read-only pipeline cache directory
    /// (`TILO_PIPELINE_SEED_DIR`), bound in by the launcher.
    pub pipeline_seed: Option<PathBuf>,
}

impl HostPaths {
    /// The per-user configuration directory: [`Self::config_home`], else
    /// `.config` under [`Self::home`].
    pub fn config_dir(&self) -> Option<PathBuf> {
        self.config_home
            .clone()
            .or_else(|| self.home.as_ref().map(|home| home.join(".config")))
    }

    /// This process's cache directory: [`Self::cache_home`], else `.cache`
    /// under [`Self::home`].
    pub fn cache_dir(&self) -> Option<PathBuf> {
        self.cache_home
            .clone()
            .or_else(|| self.home.as_ref().map(|home| home.join(".cache")))
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
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent opt-in switch a developer sets"
)]
pub struct RunnerDiagnostics {
    /// Debug overlays (only honoured in debug builds).
    pub debug_overlay: bool,
    /// Per-frame performance logging.
    pub perf_log: bool,
    /// Disable the scroll-reposition layout fast path.
    pub disable_scroll_fast_path: bool,
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
    /// Record content-free render stages under explicit presentation diagnostics.
    pub presentation_timing: bool,
    /// Open windows without taking focus from the active application.
    pub window_no_activate: bool,
    /// Restore the always-refresh hosted pump for paired host measurements.
    pub host_perf_baseline: bool,
    /// Print one payload-free timing line per hosted pump.
    pub hosted_pump_timing: bool,
}

/// Everything the host tells a `ui` process about itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HostEnvironment {
    pub identity: HostIdentity,
    pub paths: HostPaths,
    pub accessibility: AccessibilityInputs,
    /// The user's preferred locale, as a POSIX or BCP 47 tag.
    pub locale: Option<String>,
    /// The desktop session (`XDG_CURRENT_DESKTOP`, a colon-separated list),
    /// which decides who draws History and Search.
    pub desktop: Option<String>,
    pub diagnostics: RunnerDiagnostics,
    /// Directory `tilo-app dev` uses to hand the open document, route, and
    /// caret to the next process. Absent outside that loop.
    pub dev_session: Option<PathBuf>,
    /// Directory of live `ui/*.json` screens. Absent when the app uses the
    /// graph embedded in its binary.
    pub ui_graph_dir: Option<PathBuf>,
    /// Session `settings.json` the launcher named for this process
    /// (`TILO_SESSION_SETTINGS`). Present when this process paints the same
    /// document the shell chrome watches.
    pub session_settings: Option<PathBuf>,
}

static INSTALLED: RwLock<Option<Arc<HostEnvironment>>> = RwLock::new(None);

/// A POSIX locale value (`de_DE.UTF-8`, or a `LANGUAGE` list such as
/// `de:en_US`, whose first entry wins) as a language identifier. `C`, `POSIX`,
/// empty and malformed values name none.
pub fn parse_posix_locale(locale: &str) -> Option<LanguageIdentifier> {
    let first = locale.split(':').next().unwrap_or(locale).trim();
    if first.is_empty() || is_c_locale(first) {
        return None;
    }
    let core = first.split(['.', '@']).next().unwrap_or(first);
    core.replace('_', "-").parse().ok()
}

/// Whether a POSIX locale value names the `C`/`POSIX` locale (`C`,
/// `C.UTF-8`, `POSIX`), which means untranslated source copy.
fn is_c_locale(value: &str) -> bool {
    let first = value.split(':').next().unwrap_or(value).trim();
    let core = first.split(['.', '@']).next().unwrap_or(first);
    core.eq_ignore_ascii_case("C") || core.eq_ignore_ascii_case("POSIX")
}

/// The user's message locale from the variables `var` reads: `LC_ALL`,
/// `LC_MESSAGES`, `LANGUAGE`, then `LANG`. The first variable with a usable
/// value decides: an explicit `C`/`POSIX` there means source copy (`None`),
/// not "ignore this and consult the next variable". Unset, empty and
/// malformed values are skipped.
#[cfg(any(feature = "native", test))]
pub(crate) fn locale_from_vars(var: impl Fn(&str) -> Option<String>) -> Option<String> {
    for name in ["LC_ALL", "LC_MESSAGES", "LANGUAGE", "LANG"] {
        let Some(value) = var(name) else {
            continue;
        };
        if value.trim().is_empty() {
            continue;
        }
        if is_c_locale(&value) {
            return None;
        }
        if parse_posix_locale(&value).is_some() {
            return Some(value);
        }
    }
    None
}

/// Install `environment` for this process, replacing any earlier one.
pub fn install_environment(environment: HostEnvironment) {
    if let Ok(mut slot) = INSTALLED.write() {
        *slot = Some(Arc::new(environment));
    }
}

/// The host inputs for this process: the installed ones, else — with the
/// `native` feature — what the native runner reads from the process, else
/// none.
///
/// The native fallback is read afresh on each call, because the process
/// environment is the routing truth for a launched app and tests change it
/// between cases.
pub fn environment() -> Arc<HostEnvironment> {
    if let Some(installed) = INSTALLED.read().ok().and_then(|slot| slot.clone()) {
        return installed;
    }
    #[cfg(feature = "native")]
    return Arc::new(crate::native::services::process_environment());
    #[cfg(not(feature = "native"))]
    Arc::new(HostEnvironment::default())
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

    #[test]
    fn cache_dir_falls_back_to_home() {
        let paths = HostPaths {
            home: Some("/home/ada".into()),
            ..HostPaths::default()
        };
        assert_eq!(paths.cache_dir(), Some(PathBuf::from("/home/ada/.cache")));
        let paths = HostPaths {
            home: Some("/home/ada".into()),
            cache_home: Some("/run/tilo/app/.cache".into()),
            ..HostPaths::default()
        };
        assert_eq!(
            paths.cache_dir(),
            Some(PathBuf::from("/run/tilo/app/.cache"))
        );
        assert_eq!(HostPaths::default().cache_dir(), None);
    }

    #[test]
    fn an_explicit_c_locale_ends_the_environment_search() {
        let env = |vars: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                vars.iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_owned())
            }
        };
        let found = |vars| {
            locale_from_vars(env(vars))
                .and_then(|value| parse_posix_locale(&value))
                .map(|l| l.to_string())
        };
        assert_eq!(
            found(&[("LC_ALL", "C.UTF-8"), ("LANG", "de_DE.UTF-8")]),
            None
        );
        assert_eq!(
            found(&[("LC_ALL", ""), ("LANG", "de_DE.UTF-8")]).as_deref(),
            Some("de-DE")
        );
        assert_eq!(
            found(&[("LC_MESSAGES", "not a locale!"), ("LANG", "fr_FR")]).as_deref(),
            Some("fr-FR"),
            "a malformed value is skipped"
        );
        assert_eq!(found(&[]), None);
    }
}
