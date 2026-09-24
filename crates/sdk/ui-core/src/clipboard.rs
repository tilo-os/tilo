//! Clipboard backends — how a `ui` process accesses the selection.
//!
//! [`crate::text_selection`]'s verbs resolve to the clipboard of the current
//! [`crate::host::HostServices`]: a host-installed one (the compositor hosts
//! several apps in-process and mirrors its own selection for them), the
//! native desktop clipboard (see `ui::native::services`), or an
//! [`InProcessClipboard`] when the host offers none.
//!
//! The native clipboard uses [`WaylandClipboard`] for every sandboxed app,
//! over its own `wl_data_device`/`zwp_primary_selection_device_v1`
//! connection, and falls back to an in-process buffer when no system
//! clipboard exists. That fallback cannot copy across processes, so its first
//! copy or paste logs a warning.
//!
//! `arboard`'s Wayland path needs `zwlr_data_control_manager_v1`, a
//! clipboard-*manager* protocol Tilo deliberately withholds from sandboxed
//! apps: it exposes every selection on the seat with no focus check, and the
//! fleet copies TOTP codes and recovery phrases (see
//! `crates/session/shell/src/compositor/selection.rs`). [`WaylandClipboard`]'s
//! `wl_data_device` is focus-gated instead; `arboard` is kept only for X11.

use std::sync::{Arc, Mutex, OnceLock};

pub const MAX_CLIPBOARD_IMAGE_BYTES: usize = 8 * 1024 * 1024;

pub const MAX_CLIPBOARD_IMAGE_INPUT_BYTES: usize = 64 * 1024 * 1024;

/// Which selection a clipboard operation refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardTarget {
    /// The Ctrl+C / Ctrl+V clipboard.
    Clipboard,
    /// The select-then-middle-click-paste primary selection. Distinct storage
    /// from [`ClipboardTarget::Clipboard`] — a terminal writes it on every
    /// drag-select without disturbing what the user last copied.
    Primary,
}

/// A source of clipboard content for this process.
///
/// Implementors must be cheap to call and must never block on the compositor's
/// event loop from a widget's event handler — the host backend reads a mirror
/// in memory, and [`WaylandClipboard`] does its protocol work on its own
/// thread.
pub trait ClipboardBackend: Send + Sync {
    /// Take ownership of `target` and offer `text` on it. Returns `false` if
    /// the backend could not.
    fn set_text(&self, target: ClipboardTarget, text: &str) -> bool;

    /// Read `target` as UTF-8. `None` when it is empty, unavailable, or holds
    /// something that is not text.
    fn text(&self, target: ClipboardTarget) -> Option<String>;

    /// Offer an RGBA8 image on the clipboard. Optional: the default refuses,
    /// which is appropriate for backends that only carry text.
    fn set_image(&self, width: usize, height: usize, rgba: &[u8]) -> bool {
        let _ = (width, height, rgba);
        false
    }

    /// Offer `text` only where other processes can read it. Returns `false`
    /// rather than keeping it in a buffer private to this process, so a
    /// secret is never reported as copied when it was not. The default is
    /// [`set_text`](Self::set_text), right for a backend that only reaches a
    /// shared clipboard.
    fn set_shared_text(&self, target: ClipboardTarget, text: &str) -> bool {
        self.set_text(target, text)
    }

    /// Short name for diagnostics, e.g. `"compositor-mirror"`.
    fn name(&self) -> &'static str;
}

/// A clipboard private to this process, for hosts that offer no shared one.
///
/// Keeps copy → paste round-tripping inside the process so editors stay usable
/// and the caret/clipboard core stays testable headlessly. It refuses
/// [`ClipboardBackend::set_shared_text`] and images.
#[derive(Debug, Default)]
pub struct InProcessClipboard {
    clipboard: Mutex<Option<String>>,
    primary: Mutex<Option<String>>,
}

impl InProcessClipboard {
    fn slot(&self, target: ClipboardTarget) -> &Mutex<Option<String>> {
        match target {
            ClipboardTarget::Clipboard => &self.clipboard,
            ClipboardTarget::Primary => &self.primary,
        }
    }
}

impl ClipboardBackend for InProcessClipboard {
    fn set_text(&self, target: ClipboardTarget, text: &str) -> bool {
        let Ok(mut slot) = self.slot(target).lock() else {
            return false;
        };
        *slot = Some(text.to_string());
        true
    }

    fn text(&self, target: ClipboardTarget) -> Option<String> {
        self.slot(target).lock().ok()?.clone()
    }

    fn set_shared_text(&self, _target: ClipboardTarget, _text: &str) -> bool {
        false
    }

    fn name(&self) -> &'static str {
        "in-process"
    }
}

/// Install `backend` as this process's clipboard, replacing any previous one.
///
/// Shorthand for replacing the clipboard of the default
/// [`crate::host::HostServices`]; see [`crate::host::install_default`].
pub fn install_clipboard_backend(backend: Arc<dyn ClipboardBackend>) {
    tracing::debug!(target: "tilo_ui::clipboard", backend = backend.name(), "clipboard backend installed");
    crate::host::replace_default_clipboard(backend);
}

/// The clipboard effects on this thread currently reach.
///
/// Always `Some`; the `Option` is kept for existing callers.
pub fn clipboard_backend() -> Option<Arc<dyn ClipboardBackend>> {
    Some(crate::host::current().clipboard)
}

/// Emit the "this clipboard does not leave the process" warning exactly once.
///
/// Once per process, not once per call: a text editor pastes often, and a log
/// line per keystroke would bury the fact instead of reporting it.
pub fn warn_fallback_once(op: &str) {
    static WARNED: OnceLock<()> = OnceLock::new();
    WARNED.get_or_init(|| {
        tracing::warn!(
            target: "tilo_ui::clipboard",
            operation = op,
            "no system clipboard: falling back to an in-process buffer. Copy and paste \
             will work inside this process and silently do nothing between processes. \
             On Wayland this means the compositor advertises neither \
             wl_data_device_manager nor zwp_primary_selection_device_manager_v1, or the \
             window has no keyboard focus yet."
        );
    });
}

/// The native Wayland clipboard; see [`crate::native::wayland_clipboard`].
#[cfg(all(feature = "native", target_os = "linux"))]
pub use crate::native::wayland_clipboard::WaylandClipboard;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A backend that records what it was asked to do, so the resolution order
    /// in `text_selection` can be asserted against the trait rather than
    /// against a reimplementation of it.
    #[derive(Default)]
    struct RecordingBackend {
        clipboard: Mutex<Option<String>>,
        primary: Mutex<Option<String>>,
    }

    impl ClipboardBackend for RecordingBackend {
        fn set_text(&self, target: ClipboardTarget, text: &str) -> bool {
            let slot = match target {
                ClipboardTarget::Clipboard => &self.clipboard,
                ClipboardTarget::Primary => &self.primary,
            };
            *slot.lock().unwrap() = Some(text.to_string());
            true
        }

        fn text(&self, target: ClipboardTarget) -> Option<String> {
            match target {
                ClipboardTarget::Clipboard => self.clipboard.lock().unwrap().clone(),
                ClipboardTarget::Primary => self.primary.lock().unwrap().clone(),
            }
        }

        fn name(&self) -> &'static str {
            "recording"
        }
    }

    /// The two targets must not alias. This is the property that made the
    /// terminal's middle-click paste return the Ctrl+C text instead of the
    /// drag-selected one.
    #[test]
    fn backend_targets_are_independent() {
        let b = RecordingBackend::default();
        b.set_text(ClipboardTarget::Clipboard, "copied");
        b.set_text(ClipboardTarget::Primary, "selected");
        assert_eq!(
            b.text(ClipboardTarget::Clipboard).as_deref(),
            Some("copied")
        );
        assert_eq!(
            b.text(ClipboardTarget::Primary).as_deref(),
            Some("selected")
        );
    }

    /// A backend that carries only text must say so rather than pretending an
    /// image was copied — the PDF reader checks the return value to decide
    /// whether to tell the user the copy worked.
    #[test]
    fn text_only_backend_refuses_images() {
        let b = RecordingBackend::default();
        assert!(!b.set_image(2, 2, &[0u8; 16]));
    }
}
