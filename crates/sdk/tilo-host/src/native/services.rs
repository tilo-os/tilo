//! The native desktop implementation of [`HostServices`].
//!
//! - clipboard: `tilo_clipboard::system()`;
//! - files and opener: `tilo_dialogs::system_picker()` and
//!   `system_opener()` (the XDG portal on Linux, `NSOpenPanel`/`NSWorkspace`
//!   on macOS, the Common Item Dialogs on Windows), or with
//!   `TILO_SAVE_DIRECTORY` set, `tilo_dialogs::unattended_picker` over them;
//!   handed the session shell's chooser connection (a session with no
//!   portal), `tilo_dialogs::inherited_shell_picker` in front of either;
//! - speech: the system speech stack via [`tts::default_speaker`];
//! - notifications: `tilo_notify::system(app_id)`;
//! - media sessions: MPRIS on the session bus (with the `mpris` feature);
//! - location: `tilo_location::system()`;
//! - screen capture: the XDG `ScreenCast` portal on Linux (with the `portal`
//!   feature, which the SDK turns on only there).
//!
//! It is also the one place the toolkit reads the process environment:
//! [`process_environment`] turns launcher variables, XDG paths and
//! accessibility flags into a [`crate::host::HostEnvironment`].

use std::sync::Arc;

#[cfg(not(target_os = "android"))]
use crate::host::{HostServices, MediaSession, MediaSessionFactory, SpeechFactory};
#[cfg(not(target_os = "android"))]
use crate::mpris::{MprisConfig, PlayerSnapshot};

/// The native services for this process. `app_id` names the app to the
/// host's notification service; without one notifications are unavailable.
#[cfg(target_os = "android")]
pub use super::android::services;

/// The native services for this process: [`HostServices::system`]. `app_id`
/// names the app to the host's notification service; without one
/// notifications are unavailable.
#[cfg(not(target_os = "android"))]
pub fn services(app_id: Option<&str>) -> HostServices {
    HostServices::system(app_id)
}

/// `services` with the desktop backends that have no platform crate yet:
/// speech, media controls and screen capture. Android installs its own
/// speech in [`super::android::services`] and keeps null media/capture.
#[cfg(not(target_os = "android"))]
pub(crate) fn with_native_backends(services: HostServices) -> HostServices {
    // A session nobody sits at (a provider's guest) answers choosers itself.
    let unattended = std::env::var_os(tilo_dialogs::portal::SAVE_DIRECTORY_ENV)
        .filter(|directory| !directory.is_empty())
        .and_then(|directory| {
            tilo_dialogs::unattended_picker(directory.into(), services.files.clone())
        });
    // A session without a desktop portal draws choosers in its shell, and
    // hands the unattended picker the ones nobody is there to answer.
    let files = tilo_dialogs::inherited_shell_picker(unattended.clone())
        .or(unattended)
        .unwrap_or_else(|| services.files.clone());
    HostServices {
        files,
        speech: Arc::new(SystemSpeech),
        media_session: Arc::new(super::mpris::DesktopMediaSessions),
        screen_capture: Arc::new(super::capture::DesktopScreenCapture),
        ..services
    }
}

/// Install [`services`] as the process default unless a host already
/// installed its own. Called by the native runner before it builds an app.
pub fn install_default_if_unset(app_id: Option<&str>) -> bool {
    crate::host::install_default_if_unset(|| services(app_id))
}

/// The host inputs a native launcher passes through the process environment.
pub fn process_environment() -> crate::host::HostEnvironment {
    /// Empty is unset — an explicit `""` is the shape an unset forwarded
    /// shell variable produces, and taking it literally would put every such
    /// app in the same (empty-id) room.
    fn text(name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|value| !value.is_empty())
    }
    fn path(name: &str) -> Option<std::path::PathBuf> {
        std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(std::path::PathBuf::from)
    }
    /// Non-empty, and not `"0"`/`"false"` (case-insensitive).
    fn flag(names: &[&str]) -> bool {
        names.iter().any(|name| {
            std::env::var(name).is_ok_and(|value| {
                let value = value.trim();
                !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
            })
        })
    }
    /// A finite, positive factor; a bad value is ignored rather than
    /// crashing the app at startup.
    fn factor(names: &[&str]) -> Option<f32> {
        names.iter().find_map(|name| {
            std::env::var(name)
                .ok()
                .and_then(|value| value.trim().parse::<f32>().ok())
                .filter(|factor| factor.is_finite() && *factor > 0.0)
        })
    }
    let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    let ui_set = |name: &str| crate::env::var_os(name).is_some_and(|value| !value.is_empty());
    let continuity_resume = text("TILO_CONTINUITY_RESUME_HEX").and_then(|hex| {
        crate::host::environment::decode_continuation(&hex, &text("TILO_CONTINUITY_RESUME_SCHEMA")?)
    });
    crate::host::HostEnvironment {
        identity: crate::host::HostIdentity {
            package_id: text("TILO_PACKAGE_ID"),
            development_app_id: text("QUICK_APP_ID"),
            app_name: text("TILO_APP_NAME"),
            workspace_id: text("TILO_WORKSPACE_ID"),
            user_id: text("TILO_USER_ID"),
            presence_handle: text("TILO_PRESENCE_HANDLE"),
            account_name: text("USER"),
            continuity_resume,
            continuity_instance: text("TILO_CONTINUITY_INSTANCE"),
            collab_service_configured: std::env::var_os("TILO_SVC_COLLAB_FD").is_some(),
            managed_agent_context: std::env::var_os("TILO_MANAGED_AGENT_CONTEXT").is_some(),
        },
        paths: crate::host::HostPaths {
            user_home: path("TILO_USER_HOME"),
            home: path("HOME"),
            config_home: path("XDG_CONFIG_HOME"),
            cache_home: path("XDG_CACHE_HOME"),
            pipeline_seed: path("TILO_PIPELINE_SEED_DIR"),
        },
        accessibility: crate::host::AccessibilityInputs {
            screen_reader: flag(&["TILO_A11Y_SCREEN_READER", "TILO_SCREEN_READER"]),
            high_contrast: flag(&["TILO_A11Y_HIGH_CONTRAST", "TILO_HIGH_CONTRAST"]),
            reduce_motion: flag(&["TILO_A11Y_REDUCE_MOTION", "TILO_REDUCE_MOTION"]),
            text_scale: factor(&["TILO_A11Y_TEXT_SCALE", "TILO_TEXT_SCALE"]),
            // The shell mirrors the user's UI-scale preference into spawned
            // sandboxes so an app comes up already scaled.
            ui_scale: factor(&["TILO_UI_SCALE", "TILO_A11Y_UI_SCALE"]),
        },
        locale: crate::host::environment::locale_from_vars(|name| std::env::var(name).ok()),
        desktop: text("XDG_CURRENT_DESKTOP"),
        diagnostics: crate::host::RunnerDiagnostics {
            debug_overlay: crate::env::var_os("TILO_UI_DEBUG").is_some(),
            perf_log: ui_set("TILO_UI_PERF"),
            disable_scroll_fast_path: ui_set("TILO_UI_NO_SCROLL_OPT"),
            gpu_timestamps: std::env::var("VELLO_GPU_TIMESTAMPS").is_ok_and(|v| v == "1"),
            gpu_dispatch_timestamps: std::env::var("VELLO_GPU_DISPATCH_TIMESTAMPS")
                .is_ok_and(|v| v == "1"),
            profile_hud: set("TILO_PROFILE"),
            profile_out: path("TILO_PROFILE_OUT"),
            presentation_profile: presentation_profile_requested(
                std::env::var("TILO_PROFILE_PRESENTATION").ok().as_deref(),
                std::env::var_os("TILO_NATIVE_SCENARIO").as_deref(),
            ),
            presentation_timing: presentation_timing_requested(
                std::env::var("TILO_PROFILE_PRESENTATION").ok().as_deref(),
            ),
            window_no_activate: std::env::var("TILO_PROFILE_WINDOW_NO_ACTIVATE").as_deref()
                == Ok("1"),
            host_perf_baseline: ["TILO_HOST_PERF_BASELINE", "TILO_HOST_PERF2_BASELINE"]
                .iter()
                .any(|name| std::env::var_os(name).is_some_and(|v| v == "1")),
            hosted_pump_timing: std::env::var_os("TILO_HOSTED_PUMP_TIMING")
                .is_some_and(|v| v == "1"),
        },
        dev_session: path("TILO_DEV_SESSION"),
        ui_graph_dir: path("TILO_UI_GRAPH_DIR"),
        // `settings_store::SESSION_SETTINGS_ENV`. Empty is unset.
        session_settings: path("TILO_SESSION_SETTINGS"),
    }
}

/// Ordinary timing diagnostics require explicit opt-in without a synthetic scenario.
fn presentation_timing_requested(profile: Option<&str>) -> bool {
    profile == Some("1")
}

/// Presentation profiling needs an explicit `profile == "1"` and a native
/// scenario to profile. Whether this platform can profile presentation at all
/// is the renderer's decision.
fn presentation_profile_requested(
    profile: Option<&str>,
    scenario: Option<&std::ffi::OsStr>,
) -> bool {
    profile == Some("1") && scenario.is_some_and(|path| !path.is_empty())
}

/// Attach the native clipboard to the runner's Wayland display, so copies go
/// through the connection that owns the focused surface — the compositor only
/// accepts `set_selection` from that one.
///
/// Returns `true` when it attached. A no-op when the current host's clipboard
/// is not the system one (a host or test installed its own), when already
/// attached, or when the display is not Wayland.
pub fn attach_display(handle: &impl winit::raw_window_handle::HasDisplayHandle) -> bool {
    std::ptr::addr_eq(
        Arc::as_ptr(&tilo_clipboard::system()),
        Arc::as_ptr(&crate::host::current().clipboard),
    ) && tilo_clipboard::attach_wayland_display(handle)
}

#[cfg(not(target_os = "android"))]
struct SystemSpeech;

#[cfg(not(target_os = "android"))]
impl SpeechFactory for SystemSpeech {
    fn make(&self) -> Box<dyn tts::Speaker> {
        tts::default_speaker()
    }
}

#[cfg(not(target_os = "android"))]
impl MediaSessionFactory for super::mpris::DesktopMediaSessions {
    fn start(&self, config: MprisConfig, initial: PlayerSnapshot) -> Option<MediaSession> {
        super::mpris::start_desktop_session(config, initial)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_presentation_timing_requires_explicit_host_diagnostics() {
        assert!(presentation_timing_requested(Some("1")));
        for value in [None, Some(""), Some("0"), Some("true")] {
            assert!(!presentation_timing_requested(value));
        }
    }

    #[test]
    fn presentation_profile_requires_a_native_scenario_and_explicit_feedback() {
        use std::ffi::OsStr;
        assert!(presentation_profile_requested(
            Some("1"),
            Some(OsStr::new("scenario.json"))
        ));
        for (profile, scenario) in [
            (None, Some(OsStr::new("scenario.json"))),
            (Some("0"), Some(OsStr::new("scenario.json"))),
            (Some("1"), None),
            (Some("1"), Some(OsStr::new(""))),
        ] {
            assert!(!presentation_profile_requested(profile, scenario));
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_primary_selection_does_not_replace_explicit_copy() {
        let clipboard = tilo_clipboard::system();
        clipboard.set_text(
            tilo_clipboard::ClipboardTarget::Clipboard,
            "explicit Windows copy",
        );
        clipboard.set_text(
            tilo_clipboard::ClipboardTarget::Primary,
            "selected terminal text",
        );
        assert_eq!(
            clipboard
                .text(tilo_clipboard::ClipboardTarget::Primary)
                .as_deref(),
            Some("selected terminal text")
        );
        assert_eq!(
            clipboard
                .text(tilo_clipboard::ClipboardTarget::Clipboard)
                .as_deref(),
            Some("explicit Windows copy")
        );
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn native_services_share_one_round_tripping_clipboard() {
        // phase: native_clipboard_round_trips_with_or_without_a_system_clipboard
        // Copy → paste must round-trip within the process even with no system
        // clipboard (the headless test env), and equally when a real clipboard
        // is present.
        {
            let clipboard = tilo_clipboard::system();
            let payload = "https://example.com/feed.xml";
            clipboard.set_text(tilo_clipboard::ClipboardTarget::Clipboard, payload);
            assert_eq!(
                clipboard
                    .text(tilo_clipboard::ClipboardTarget::Clipboard)
                    .as_deref(),
                Some(payload)
            );
        }
        // phase: native_services_resolve_one_shared_clipboard
        {
            let a = services(None);
            let b = services(Some("so.tilo.example"));
            assert!(std::ptr::addr_eq(
                Arc::as_ptr(&a.clipboard),
                Arc::as_ptr(&b.clipboard)
            ));
            assert_eq!(a.clipboard.name(), "native");
        }
    }
}
