//! The native desktop implementation of [`ProcessServices`].
//!
//! - clipboard: `tilo_clipboard::system()`, the connection the app's session serves it over;
//! - files: the chooser connection the launcher hands the app
//!   (`tilo_dialogs::inherited_shell_picker`), which asks the person in the
//!   session or the device daemon and answers exactly what they chose; with
//!   `TILO_SAVE_DIRECTORY` set, `tilo_dialogs::unattended_picker` answers a
//!   Save when nobody is there to choose. An app reaches no chooser of its own;
//! - opener: `system_opener()`, or `tilo_dialogs::inherited_shell_opener` in
//!   front of it when the launcher handed a connection;
//! - speech: the system speech stack via [`tts::default_speaker`];
//! - notifications: `tilo_notify::system(app_id)`, the connection the launcher opened to the
//!   session's notifier (a host running apps on a foreign desktop reads the portal instead, with
//!   the `host-portals` feature this SDK build leaves off);
//! - media sessions: MPRIS on the session bus (with the `mpris` feature);
//! - location: none here; the device answers it as its own action, and a host that can ask
//!   installs it with [`ProcessServices::with_location`];
//! - screen capture: the XDG `ScreenCast` portal on Linux (with the `portal`
//!   feature, which the SDK turns on only there).
//!
//! [`process_environment`] (`tilo_process::environment`) turns launcher
//! variables, XDG paths and accessibility flags into a
//! [`crate::process::ProcessEnvironment`].

#[cfg(not(target_os = "android"))]
use std::sync::Arc;

#[cfg(not(target_os = "android"))]
use crate::mpris::{MprisConfig, PlayerSnapshot};
#[cfg(not(target_os = "android"))]
use crate::process::{MediaSession, MediaSessionFactory, ProcessServices, SpeechFactory};

/// The native services for this process. `app_id` names the app to the
/// process's notification service; without one notifications are unavailable.
#[cfg(target_os = "android")]
pub use super::android::services;

/// The native services for this process: [`ProcessServices::system`]. `app_id`
/// names the app to the process's notification service; without one
/// notifications are unavailable.
#[cfg(not(target_os = "android"))]
pub fn services(app_id: Option<&str>) -> ProcessServices {
    ProcessServices::system(app_id)
}

/// `services` with the desktop backends that have no platform crate yet:
/// speech, media controls and screen capture. Android installs its own
/// speech in [`super::android::services`] and keeps null media/capture.
#[cfg(not(target_os = "android"))]
pub(crate) fn with_native_backends(services: ProcessServices) -> ProcessServices {
    // A session nobody sits at (a provider's guest) answers a Save itself; every
    // other choice there is refused, since only a person chooses a file.
    let unattended = std::env::var_os(tilo_dialogs::portal::SAVE_DIRECTORY_ENV)
        .filter(|directory| !directory.is_empty())
        .and_then(|directory| tilo_dialogs::unattended_picker(directory.into()));
    // What the session says nobody is there for — a Save into the unattended
    // directory — goes to the unattended picker. An app never draws a chooser
    // itself: without the connection its host hands it there is none.
    let elsewhere = unattended.unwrap_or_else(tilo_dialogs::unavailable_picker);
    let files = tilo_dialogs::inherited_shell_picker(elsewhere)
        .unwrap_or_else(tilo_dialogs::unavailable_picker);
    // A link with a scheme a Tilo app claims (`settings://updates`) opens in
    // that app through the shell, as its search opens it; the desktop's
    // opener has no handler registered for such schemes.
    let opener = tilo_dialogs::inherited_shell_opener(services.opener.clone())
        .unwrap_or_else(|| services.opener.clone());
    ProcessServices {
        files,
        opener,
        speech: Arc::new(SystemSpeech),
        media_session: Arc::new(super::mpris::DesktopMediaSessions),
        screen_capture: Arc::new(super::capture::DesktopScreenCapture),
        ..services
    }
}

/// Install [`services`] as the process default unless a process already
/// installed its own. Called by the native runner before it builds an app.
pub fn install_default_if_unset(app_id: Option<&str>) -> bool {
    crate::process::install_default_if_unset(|| services(app_id))
}

/// The process inputs a native launcher passes through the process environment.
pub use tilo_process::environment::process_environment;

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

    #[cfg(not(target_os = "android"))]
    #[test]
    fn native_services_resolve_one_shared_clipboard() {
        let a = services(None);
        let b = services(Some("so.tilo.example"));
        assert!(std::ptr::addr_eq(
            Arc::as_ptr(&a.clipboard),
            Arc::as_ptr(&b.clipboard)
        ));
        assert_eq!(a.clipboard.name(), tilo_clipboard::system().name());
    }
}
