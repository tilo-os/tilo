//! Host inputs: who the process is, where its files live, and how its user asked to
//! be served.
//!
//! Toolkit and collab-client code take these from [`environment`] rather than reading
//! the process environment. Only the native fallback derives them from the process
//! ([`process_environment`], feature `native`); another host installs its own with
//! [`install_environment`].

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use unic_langid::LanguageIdentifier;

#[cfg(feature = "native")]
mod native;
#[cfg(feature = "native")]
pub use native::process_environment;

/// Inert, app-owned resume projection sent only to the local shell: the bytes are
/// opaque to the toolkit, so each app can version its own schema, and the shell
/// validates size and path redaction before keeping them.
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

/// Who this process is and which workspace it serves; every field is optional, and
/// each reader applies its own fallback.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostIdentity {
    pub app_id: Option<String>,
    pub development_app_id: Option<String>,
    pub app_name: Option<String>,
    pub workspace_id: Option<String>,
    pub user_id: Option<String>,
    pub account_name: Option<String>,
    pub continuity_resume: Option<ContinuationMetadata>,
    pub continuity_instance: Option<String>,
    /// The launcher configured a collaboration service for this process, so
    /// its absence is a failure rather than a standalone launch.
    pub collab_service_configured: bool,
    /// The process runs as an admitted managed agent, which receives task work
    /// on its own broker queue instead of the room's task-run stream.
    pub managed_agent_context: bool,
}

/// The MPRIS bus name of the media player a launched package publishes:
/// `org.mpris.MediaPlayer2.<package id>`.
#[must_use]
pub fn media_player_bus_name(app_id: &str) -> String {
    format!("org.mpris.MediaPlayer2.{app_id}")
}

impl HostIdentity {
    /// The MPRIS bus name this process's media player goes by: its app's
    /// ([`media_player_bus_name`]), else `fallback_app_id`'s, so an app run
    /// outside a launcher still names its player.
    #[must_use]
    pub fn media_player_bus_name(&self, fallback_app_id: &str) -> String {
        media_player_bus_name(self.app_id.as_deref().unwrap_or(fallback_app_id))
    }
}

/// Env var naming the font index directory, for a process the launcher puts
/// in a sandbox ([`HostPaths::font_index`]).
pub const FONT_INDEX_ENV: &str = "TILO_FONT_INDEX";

/// `TILO_REQUIRE_FONTS=1` promises every process that sets text the product's
/// font catalog, as `TILO_REQUIRE_GPU=1` promises a GPU: a test run sets it.
pub const FONTS_REQUIRED_ENV: &str = "TILO_REQUIRE_FONTS";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostPaths {
    pub user_home: Option<PathBuf>,
    pub home: Option<PathBuf>,
    pub config_home: Option<PathBuf>,
    pub cache_home: Option<PathBuf>,
    /// The session's read-only pipeline cache directory (`TILO_PIPELINE_SEED_DIR`), bound
    /// in by the launcher.
    pub pipeline_seed: Option<PathBuf>,
    /// The directory this process reads and records its font index in
    /// (`TILO_FONT_INDEX`), when the host names one; else it is
    /// `tilo/font-index` under [`Self::cache_dir`].
    pub font_index: Option<PathBuf>,
    /// The font catalog to read instead of the compiled-in one (`TILO_FONT_CATALOG`); a
    /// build bakes in the pinned catalog's path, which a binary built elsewhere does not
    /// have.
    pub font_catalog: Option<PathBuf>,
    pub font_artifact_cache: Option<PathBuf>,
    /// Explicit development-host record for public identity presentation only.
    /// It is not signing authority or permission to resolve a secret provider.
    pub public_identity_record: Option<PathBuf>,
}

impl HostPaths {
    pub fn config_dir(&self) -> Option<PathBuf> {
        self.config_home
            .clone()
            .or_else(|| self.home.as_ref().map(|home| home.join(".config")))
    }

    pub fn cache_dir(&self) -> Option<PathBuf> {
        self.cache_home
            .clone()
            .or_else(|| self.home.as_ref().map(|home| home.join(".cache")))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AccessibilityInputs {
    pub screen_reader: bool,
    pub high_contrast: bool,
    pub reduce_motion: bool,
    pub text_scale: Option<f32>,
    pub ui_scale: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent opt-in switch a developer sets"
)]
pub struct RunnerDiagnostics {
    /// Debug overlays (only honoured in debug builds).
    pub debug_overlay: bool,
    pub perf_log: bool,
    pub disable_scroll_fast_path: bool,
    pub gpu_timestamps: bool,
    pub gpu_dispatch_timestamps: bool,
    pub profile_hud: bool,
    pub profile_out: Option<PathBuf>,
    pub presentation_profile: bool,
    pub presentation_timing: bool,
    /// Open windows without taking focus from the active application.
    pub window_no_activate: bool,
    /// Restore the always-refresh hosted pump for paired host measurements.
    pub host_perf_baseline: bool,
    pub hosted_pump_timing: bool,
    /// The run was promised the product's font catalog (`TILO_REQUIRE_FONTS=1`), so a
    /// process that finds none stops rather than setting text in the host's faces.
    pub fonts_required: bool,
}

pub const REVEAL_ENV: &str = "TILO_WINDOW_REVEAL";

/// The variable a launch sets (`1`) for a process the session starts with no
/// window: the app takes its actions and publishes what it offers, and makes
/// a window only once the shell asks it for one.
pub const WINDOWLESS_ENV: &str = "TILO_WINDOWLESS";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HostEnvironment {
    pub identity: HostIdentity,
    pub paths: HostPaths,
    pub accessibility: AccessibilityInputs,
    /// The user's preferred locale, as a POSIX or BCP 47 tag.
    pub locale: Option<String>,
    pub desktop: Option<String>,
    pub diagnostics: RunnerDiagnostics,
    pub dev_session: Option<PathBuf>,
    pub ui_graph_dir: Option<PathBuf>,
    pub reveal: Option<String>,
    pub windowless: bool,
}

static INSTALLED: RwLock<Option<Arc<HostEnvironment>>> = RwLock::new(None);

/// A POSIX locale value (`de_DE.UTF-8`, or a `LANGUAGE` list such as
/// `de:en_US`, whose first entry wins) as a language identifier. `C`, `POSIX`,
/// empty and malformed values name none.
pub fn parse_posix_locale(locale: &str) -> Option<LanguageIdentifier> {
    let first = locale.split(':').next().unwrap_or(locale);
    if !tilo_paths::names_language(first) {
        return None;
    }
    tilo_paths::locale_tag(first).parse().ok()
}

/// The user's message locale from the variables `var` reads: `LC_ALL`, `LC_MESSAGES`,
/// `LANGUAGE`, then `LANG`. The first variable with a usable value decides, and an
/// explicit `C`/`POSIX` there means source copy.
#[cfg(any(feature = "native", test))]
pub(crate) fn locale_from_vars(var: impl Fn(&str) -> Option<String>) -> Option<String> {
    for name in ["LC_ALL", "LC_MESSAGES", "LANGUAGE", "LANG"] {
        let Some(value) = var(name) else {
            continue;
        };
        if value.trim().is_empty() {
            continue;
        }
        if !tilo_paths::names_language(&value) {
            return None;
        }
        if parse_posix_locale(&value).is_some() {
            return Some(value);
        }
    }
    None
}

pub fn install_environment(environment: HostEnvironment) {
    if let Ok(mut slot) = INSTALLED.write() {
        *slot = Some(Arc::new(environment));
    }
}

/// The host inputs for this process: the installed ones, else (with `native`) what the
/// runner reads from the process environment afresh on each call, else none.
pub fn environment() -> Arc<HostEnvironment> {
    if let Some(installed) = INSTALLED.read().ok().and_then(|slot| slot.clone()) {
        return installed;
    }
    #[cfg(feature = "native")]
    return Arc::new(process_environment());
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
