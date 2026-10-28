//! The native desktop implementation of [`HostServices`].
//!
//! - clipboard: `tilo_clipboard::system()`;
//! - files and opener: `tilo_dialogs::system_picker()` and
//!   `system_opener()` (the XDG portal on Linux, `NSOpenPanel`/`NSWorkspace`
//!   on macOS, the Common Item Dialogs on Windows), or with
//!   `TILO_SAVE_DIRECTORY` set, `tilo_dialogs::unattended_picker` over them;
//!   handed the session shell's chooser connection,
//!   `tilo_dialogs::inherited_shell_picker` in front of either and
//!   `tilo_dialogs::inherited_shell_opener` in front of the opener;
//! - speech: the system speech stack via [`tts::default_speaker`];
//! - notifications: `tilo_notify::system(app_id)`;
//! - media sessions: MPRIS on the session bus (with the `mpris` feature);
//! - location: `tilo_location::system()`;
//! - screen capture: the XDG `ScreenCast` portal on Linux (with the `portal`
//!   feature, which the SDK turns on only there).
//!
//! [`process_environment`] (`tilo_process::environment`) turns launcher
//! variables, XDG paths and accessibility flags into a
//! [`crate::host::HostEnvironment`].

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
    // A link with a scheme a Tilo app claims (`settings://updates`) opens in
    // that app through the shell, as its search opens it; the desktop's
    // opener has no handler registered for such schemes.
    let opener = tilo_dialogs::inherited_shell_opener(services.opener.clone())
        .unwrap_or_else(|| services.opener.clone());
    HostServices {
        files,
        opener,
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
pub use tilo_process::environment::process_environment;

/// Attach the native clipboard to the runner's Wayland display, so copies go
/// through the connection that owns the focused surface — the compositor only
/// accepts `set_selection` from that one.
///
/// The runner keeps the returned guard until close. A no-op when the host
/// installed its own clipboard, already attached, or is not on Wayland.
pub fn attach_display(
    handle: winit::event_loop::OwnedDisplayHandle,
) -> Option<tilo_clipboard::ClipboardAttachment> {
    if !std::ptr::addr_eq(
        Arc::as_ptr(&tilo_clipboard::system()),
        Arc::as_ptr(&crate::host::current().clipboard),
    ) {
        return None;
    }
    tilo_clipboard::attach_wayland_display(handle)
}

#[cfg(not(target_os = "android"))]
use self::desktop_media::SystemSpeech;

/// The desktop's speech and media sessions, which Android does not have: one
/// platform boundary for both, so a reader sees a single "not here" rather than
/// three.
#[cfg(not(target_os = "android"))]
mod desktop_media {
    use super::{MediaSession, MediaSessionFactory, MprisConfig, PlayerSnapshot, SpeechFactory};

    pub(super) struct SystemSpeech;

    impl SpeechFactory for SystemSpeech {
        fn make(&self) -> Box<dyn tts::Speaker> {
            tts::default_speaker()
        }
    }

    impl MediaSessionFactory for super::super::mpris::DesktopMediaSessions {
        fn start(&self, config: MprisConfig, initial: PlayerSnapshot) -> Option<MediaSession> {
            super::super::mpris::start_desktop_session(config, initial)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
