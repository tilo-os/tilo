//! The native desktop implementation of [`HostServices`].
//!
//! - clipboard: the app's own Wayland connection once the runner has a
//!   display, `arboard` on X11 and other desktops, and an in-process buffer
//!   when neither exists;
//! - files: the XDG Desktop Portal on Unix and the Common Item Dialogs on
//!   Windows (with the `portal` feature; see [`crate::portal`]);
//! - opener: the desktop's registered URI handler through `open`;
//! - speech: the system speech stack via [`tts::default_speaker`];
//! - notifications: Windows toasts (with the `notifications` feature);
//! - media sessions: MPRIS on the session bus (with the `mpris` feature).
//!
//! It is also the one place the toolkit reads the process environment:
//! [`process_environment`] turns launcher variables, XDG paths and
//! accessibility flags into a [`crate::host::HostEnvironment`].

use std::sync::{Arc, Mutex, OnceLock};

use crate::clipboard::{ClipboardBackend, ClipboardTarget, InProcessClipboard, warn_fallback_once};
use crate::host::{
    HostServices, MediaSession, MediaSessionFactory, Notifier, SpeechFactory, UriOpener,
};
#[cfg(all(windows, feature = "notifications"))]
use crate::host::{Notification, NotifyError};
use crate::mpris::{MprisConfig, PlayerSnapshot};

/// The native services for this process. `app_id` names the app to the
/// host's notification service; without one notifications are unavailable.
pub fn services(app_id: Option<&str>) -> HostServices {
    HostServices {
        clipboard: native_clipboard(),
        files: Arc::new(super::portal::DesktopFilePicker),
        opener: Arc::new(DesktopOpener),
        speech: Arc::new(SystemSpeech),
        notifications: notifier(app_id),
        media_session: Arc::new(super::mpris::DesktopMediaSessions),
    }
}

/// Install [`services`] as the process default unless a host already
/// installed its own. Called by the native runner before it builds an app.
pub fn install_default_if_unset(app_id: Option<&str>) -> bool {
    crate::host::install_default_if_unset(|| services(app_id))
}

fn native_clipboard() -> Arc<dyn ClipboardBackend> {
    static CLIPBOARD: OnceLock<Arc<NativeClipboard>> = OnceLock::new();
    CLIPBOARD
        .get_or_init(|| Arc::new(NativeClipboard::default()))
        .clone()
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
    let continuity_resume = text("TILO_CONTINUITY_RESUME_HEX").and_then(|hex| {
        crate::collaboration::decode_continuation(&hex, &text("TILO_CONTINUITY_RESUME_SCHEMA")?)
    });
    crate::host::HostEnvironment {
        identity: crate::host::HostIdentity {
            package_id: text("TILO_PACKAGE_ID"),
            development_app_id: text("QUICK_APP_ID"),
            app_name: text("TILO_APP_NAME"),
            workspace_id: text("TILO_WORKSPACE_ID"),
            cluster_id: text("QUICK_CLUSTER_ID"),
            user_id: text("TILO_USER_ID"),
            presence_handle: text("TILO_PRESENCE_HANDLE"),
            account_name: text("USER"),
            continuity_resume,
            continuity_instance: text("TILO_CONTINUITY_INSTANCE"),
        },
        paths: crate::host::HostPaths {
            user_home: path("TILO_USER_HOME"),
            home: path("HOME"),
            config_home: path("XDG_CONFIG_HOME"),
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
        locale: ["LC_ALL", "LC_MESSAGES", "LANGUAGE", "LANG"]
            .into_iter()
            .filter_map(|name| std::env::var(name).ok())
            .find(|value| crate::i18n::is_usable_locale(value)),
        diagnostics: crate::host::RunnerDiagnostics {
            debug_overlay: std::env::var_os("QUICK_UI_DEBUG").is_some(),
            perf_log: set("QUICK_UI_PERF"),
            disable_scroll_fast_path: set("QUICK_UI_NO_SCROLL_OPT"),
            disable_split_cache: set("QUICK_UI_NO_SPLIT_CACHE"),
            gpu_timestamps: std::env::var("VELLO_GPU_TIMESTAMPS").is_ok_and(|v| v == "1"),
            gpu_dispatch_timestamps: std::env::var("VELLO_GPU_DISPATCH_TIMESTAMPS")
                .is_ok_and(|v| v == "1"),
            profile_hud: set("TILO_PROFILE"),
            profile_out: path("TILO_PROFILE_OUT"),
            presentation_profile: crate::render_thread::timing::gate(
                true,
                std::env::var("QOS_PROFILE_PRESENTATION").ok().as_deref(),
                std::env::var_os("QOS_NATIVE_SCENARIO").as_deref(),
            ),
            window_no_activate: std::env::var("QOS_PROFILE_WINDOW_NO_ACTIVATE").as_deref()
                == Ok("1"),
        },
    }
}

/// Attach the native clipboard to the runner's Wayland display, so copies go
/// through the connection that owns the focused surface — the compositor only
/// accepts `set_selection` from that one.
///
/// Returns `true` when it attached. A no-op when the current host's clipboard
/// is not the native one (a host or test installed its own), when already
/// attached, or when the display is not Wayland.
#[cfg(target_os = "linux")]
pub fn attach_display(handle: &impl winit::raw_window_handle::HasDisplayHandle) -> bool {
    use winit::raw_window_handle::RawDisplayHandle;

    let native = native_clipboard();
    if !std::ptr::addr_eq(
        Arc::as_ptr(&native),
        Arc::as_ptr(&crate::host::current().clipboard),
    ) {
        return false;
    }
    let Ok(display) = handle.display_handle() else {
        return false;
    };
    let RawDisplayHandle::Wayland(wayland) = display.as_raw() else {
        return false;
    };
    let mut attached = false;
    WAYLAND.get_or_init(|| {
        attached = true;
        // SAFETY: the pointer comes from winit's active event loop, which owns
        // the `wl_display` for the whole run of the app; the backend is a
        // process global that is never used after the loop exits.
        unsafe { crate::clipboard::WaylandClipboard::new(wayland.display.as_ptr()) }
    });
    attached
}

/// The native clipboard's Wayland connection, once [`attach_display`] ran.
#[cfg(target_os = "linux")]
static WAYLAND: OnceLock<crate::clipboard::WaylandClipboard> = OnceLock::new();

/// The desktop clipboard, resolved per operation.
///
/// Order: the app's Wayland connection once attached, else `arboard` when
/// this process is not a Wayland client, else a process-local buffer that
/// announces itself in the log.
#[derive(Default)]
struct NativeClipboard {
    local: InProcessClipboard,
}

impl NativeClipboard {
    #[cfg(target_os = "linux")]
    fn wayland() -> Option<&'static crate::clipboard::WaylandClipboard> {
        WAYLAND.get()
    }

    #[cfg(not(target_os = "linux"))]
    fn wayland() -> Option<&'static NoWayland> {
        None
    }

    /// Write to a clipboard other processes can read, if there is one.
    fn set_shared(&self, target: ClipboardTarget, text: &str) -> bool {
        if let Some(wayland) = Self::wayland()
            && wayland.set_text(target, text)
        {
            return true;
        }
        #[cfg(windows)]
        if target == ClipboardTarget::Primary {
            return false;
        }
        let Some(cb) = arboard_clipboard() else {
            return false;
        };
        let Ok(mut handle) = cb.lock() else {
            return false;
        };
        match target {
            ClipboardTarget::Clipboard => handle.set_text(text.to_string()).is_ok(),
            #[cfg(target_os = "linux")]
            ClipboardTarget::Primary => {
                use arboard::{LinuxClipboardKind, SetExtLinux};
                handle
                    .set()
                    .clipboard(LinuxClipboardKind::Primary)
                    .text(text.to_string())
                    .is_ok()
            }
            // No primary selection outside Linux; the regular clipboard is
            // the closest equivalent.
            #[cfg(not(target_os = "linux"))]
            ClipboardTarget::Primary => handle.set_text(text.to_string()).is_ok(),
        }
    }
}

/// Stand-in so the non-Linux resolution order reads like the Linux one.
#[cfg(not(target_os = "linux"))]
struct NoWayland;

#[cfg(not(target_os = "linux"))]
impl NoWayland {
    fn set_text(&self, _target: ClipboardTarget, _text: &str) -> bool {
        false
    }
    fn text(&self, _target: ClipboardTarget) -> Option<String> {
        None
    }
    fn set_image(&self, _width: usize, _height: usize, _rgba: &[u8]) -> bool {
        false
    }
}

impl ClipboardBackend for NativeClipboard {
    fn set_text(&self, target: ClipboardTarget, text: &str) -> bool {
        if self.set_shared(target, text) {
            return true;
        }
        // Windows has no primary-selection protocol. Keep select/middle-click
        // local to the app instead of overwriting the user's Ctrl+C clipboard.
        #[cfg(windows)]
        if target == ClipboardTarget::Primary {
            return self.local.set_text(target, text);
        }
        warn_fallback_once("copy");
        self.local.set_text(target, text)
    }

    fn set_shared_text(&self, target: ClipboardTarget, text: &str) -> bool {
        // An attached Wayland connection is authoritative: its refusal must
        // not route a secret through another clipboard.
        if let Some(wayland) = Self::wayland() {
            return wayland.set_text(target, text);
        }
        self.set_shared(target, text)
    }

    fn text(&self, target: ClipboardTarget) -> Option<String> {
        if let Some(wayland) = Self::wayland() {
            return wayland.text(target);
        }
        #[cfg(windows)]
        if target == ClipboardTarget::Primary {
            return self.local.text(target);
        }
        if let Some(cb) = arboard_clipboard()
            && let Ok(mut handle) = cb.lock()
        {
            return match target {
                ClipboardTarget::Clipboard => handle.get_text().ok(),
                #[cfg(target_os = "linux")]
                ClipboardTarget::Primary => {
                    use arboard::{GetExtLinux, LinuxClipboardKind};
                    handle
                        .get()
                        .clipboard(LinuxClipboardKind::Primary)
                        .text()
                        .ok()
                }
                #[cfg(not(target_os = "linux"))]
                ClipboardTarget::Primary => handle.get_text().ok(),
            };
        }
        warn_fallback_once("paste");
        self.local.text(target)
    }

    fn set_image(&self, width: usize, height: usize, rgba: &[u8]) -> bool {
        if let Some(wayland) = Self::wayland() {
            return wayland.set_image(width, height, rgba);
        }
        let Some(cb) = arboard_clipboard() else {
            return false;
        };
        let Ok(mut handle) = cb.lock() else {
            return false;
        };
        let img = arboard::ImageData {
            width,
            height,
            bytes: std::borrow::Cow::Borrowed(rgba),
        };
        handle.set_image(img).is_ok()
    }

    fn name(&self) -> &'static str {
        "native"
    }
}

/// Lazy global `arboard` handle, used **only on X11** and other non-Wayland
/// platforms.
///
/// On Wayland `arboard` talks `zwlr_data_control_manager_v1`, which Tilo does
/// not offer to sandboxed apps (see `crates/session/shell/src/compositor/selection.rs`); a
/// `ui` app there goes through [`crate::clipboard::WaylandClipboard`] and
/// `wl_data_device` instead. Construction is skipped rather than attempted
/// on Wayland because arboard's probe there is slow, and a handle that
/// fails on every call is worse than no handle.
fn arboard_clipboard() -> Option<&'static Mutex<arboard::Clipboard>> {
    static CB: OnceLock<Option<Mutex<arboard::Clipboard>>> = OnceLock::new();
    CB.get_or_init(|| {
        if running_on_wayland() {
            return None;
        }
        arboard::Clipboard::new().ok().map(Mutex::new)
    })
    .as_ref()
}

/// Whether this process is a Wayland client.
fn running_on_wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
}

/// Hands URIs to the desktop's registered handler. On Windows this uses
/// `ShellExecute` rather than interpreting the URI as a command line.
struct DesktopOpener;

impl UriOpener for DesktopOpener {
    fn open(&self, uri: &str) -> std::io::Result<()> {
        open::that_detached(uri)
    }
}

struct SystemSpeech;

impl SpeechFactory for SystemSpeech {
    fn make(&self) -> Box<dyn tts::Speaker> {
        tts::default_speaker()
    }
}

fn notifier(app_id: Option<&str>) -> Arc<dyn Notifier> {
    #[cfg(all(windows, feature = "notifications"))]
    if let Some(app_id) = app_id {
        return Arc::new(ToastNotifier {
            app_id: app_id.to_string(),
        });
    }
    let _ = app_id;
    Arc::new(crate::host::UnavailableNotifier)
}

/// Windows toast notifications under the app's registered id. Each call
/// opens its own notifier: the COM apartment it needs is bound to the calling
/// thread.
#[cfg(all(windows, feature = "notifications"))]
struct ToastNotifier {
    app_id: String,
}

#[cfg(all(windows, feature = "notifications"))]
impl Notifier for ToastNotifier {
    fn show(&self, notification: &Notification) -> Result<(), NotifyError> {
        super::host_notifications::HostNotifications::new(&self.app_id)
            .and_then(|host| host.show(&notification.tag, &notification.title, &notification.body))
            .map_err(|error| NotifyError::Failed(error.to_string()))
    }

    fn withdraw(&self, tag: &str) -> Result<(), NotifyError> {
        super::host_notifications::HostNotifications::new(&self.app_id)
            .and_then(|host| host.remove(tag))
            .map_err(|error| NotifyError::Failed(error.to_string()))
    }
}

impl MediaSessionFactory for super::mpris::DesktopMediaSessions {
    fn start(&self, config: MprisConfig, initial: PlayerSnapshot) -> Option<MediaSession> {
        super::mpris::start_desktop_session(config, initial)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_primary_selection_does_not_replace_explicit_copy() {
        let clipboard = NativeClipboard::default();
        clipboard.set_text(ClipboardTarget::Clipboard, "explicit Windows copy");
        clipboard.set_text(ClipboardTarget::Primary, "selected terminal text");
        assert_eq!(
            clipboard.text(ClipboardTarget::Primary).as_deref(),
            Some("selected terminal text")
        );
        assert_eq!(
            clipboard.text(ClipboardTarget::Clipboard).as_deref(),
            Some("explicit Windows copy")
        );
    }

    /// Copy → paste must round-trip within the process even with no system
    /// clipboard (the headless test env), and equally when a real clipboard
    /// is present.
    #[test]
    fn native_clipboard_round_trips_with_or_without_a_system_clipboard() {
        let clipboard = NativeClipboard::default();
        let payload = "https://example.com/feed.xml";
        clipboard.set_text(ClipboardTarget::Clipboard, payload);
        assert_eq!(
            clipboard.text(ClipboardTarget::Clipboard).as_deref(),
            Some(payload)
        );
    }

    #[test]
    fn native_services_resolve_one_shared_clipboard() {
        let a = services(None);
        let b = services(Some("so.tilo.example"));
        assert!(std::ptr::addr_eq(
            Arc::as_ptr(&a.clipboard),
            Arc::as_ptr(&b.clipboard)
        ));
        assert_eq!(a.clipboard.name(), "native");
    }
}
