//! Clipboard backends — how a `ui` process actually reaches the selection.
//!
//! [`crate::text_selection`] exposes the four verbs every widget uses
//! (`clipboard_copy`, `clipboard_paste`, and the primary-selection pair). This
//! module is what those verbs resolve to at runtime, and there are three
//! possibilities:
//!
//! 1. **A host-installed backend.** The compositor hosts `terminal`,
//!    `file-explorer`, the notes and editor apps and all of its own chrome
//!    *inside its own process*. Those widgets have no Wayland client
//!    connection to copy over — they are the server. The compositor installs a
//!    backend that reads and writes its selection mirror directly.
//! 2. **The Wayland selection**, via [`WaylandClipboard`]. This is the path
//!    every sandboxed `ui` app takes. It speaks `wl_data_device` and
//!    `zwp_primary_selection_device_v1` on the app's *own* connection, so the
//!    compositor sees a normal focused client asking for the clipboard.
//! 3. **An in-process buffer**, when neither of the above exists — a unit
//!    test, a headless scenario run, an X11 session with no clipboard owner.
//!
//! The third case is the one that caused the bug this module exists to fix. It
//! round-trips inside one process, so a test passes and a demo looks right
//! while cross-process copy is silently dead. It is still here, because tests
//! need it, but it now announces itself: the first time a copy or paste
//! resolves to the fallback the process logs a warning naming it. A clipboard
//! that does not work must never again be indistinguishable from one that
//! does.
//!
//! ## Why not `arboard` on Wayland
//!
//! `arboard`'s Wayland backend is `wl-clipboard-rs`, which requires
//! `zwlr_data_control_manager_v1` — the clipboard-*manager* protocol. qOS does
//! not offer that global to sandboxed apps, deliberately: it exposes every
//! selection on the seat to any client that binds it, with no focus check, and
//! the app fleet routinely copies TOTP codes and recovery phrases. (The
//! reasoning lives in `shell/src/compositor/selection.rs`.) `wl_data_device`
//! is the protocol an *application* is supposed to use, it is focus-gated, and
//! it is what [`WaylandClipboard`] speaks. `arboard` is kept only for X11,
//! where it is the right answer.

use std::sync::{Arc, OnceLock, RwLock};

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
    /// which is honest for backends that only carry text.
    fn set_image(&self, width: usize, height: usize, rgba: &[u8]) -> bool {
        let _ = (width, height, rgba);
        false
    }

    /// Short name for diagnostics, e.g. `"compositor-mirror"`.
    fn name(&self) -> &'static str;
}

/// The backend this process resolved to, if any.
///
/// `RwLock` rather than `OnceLock` because the resolution order is not known
/// at first use: a hosted widget may copy something before the compositor has
/// finished installing its backend, and an app's Wayland backend only exists
/// once winit has a display. Reads are the common path and are uncontended.
static BACKEND: OnceLock<RwLock<Option<Arc<dyn ClipboardBackend>>>> = OnceLock::new();

fn backend_slot() -> &'static RwLock<Option<Arc<dyn ClipboardBackend>>> {
    BACKEND.get_or_init(|| RwLock::new(None))
}

/// Install `backend` as this process's clipboard, replacing any previous one.
///
/// Called by the compositor for its in-process widgets, and by
/// [`crate::app`] for a windowed app once it has a Wayland display. An app
/// that wants a different clipboard entirely (a remote session, a test double)
/// can call this before its window comes up.
pub fn install_clipboard_backend(backend: Arc<dyn ClipboardBackend>) {
    let name = backend.name();
    if let Ok(mut slot) = backend_slot().write() {
        *slot = Some(backend);
        tracing::debug!(target: "ui::clipboard", backend = name, "clipboard backend installed");
    }
}

/// The installed backend, if one has been.
pub fn clipboard_backend() -> Option<Arc<dyn ClipboardBackend>> {
    backend_slot().read().ok()?.clone()
}

/// Emit the "this clipboard does not leave the process" warning exactly once.
///
/// Once per process, not once per call: a text editor pastes often, and a log
/// line per keystroke would bury the fact instead of reporting it.
pub fn warn_fallback_once(op: &str) {
    static WARNED: OnceLock<()> = OnceLock::new();
    WARNED.get_or_init(|| {
        tracing::warn!(
            target: "ui::clipboard",
            operation = op,
            "no system clipboard: falling back to an in-process buffer. Copy and paste \
             will work inside this process and silently do nothing between processes. \
             On Wayland this means the compositor advertises neither \
             wl_data_device_manager nor zwp_primary_selection_device_manager_v1, or the \
             window has no keyboard focus yet."
        );
    });
}

// -----------------------------------------------------------------------------
// The Wayland client backend
// -----------------------------------------------------------------------------

/// `wl_data_device` + `zwp_primary_selection_device_v1` on the app's own
/// Wayland connection.
///
/// Built on `smithay-clipboard`, which runs the selection protocol on a
/// dedicated event queue over the *existing* connection. Sharing the
/// connection is the point: the compositor gates `set_selection` on keyboard
/// focus, and only the connection that owns the focused surface has it. A
/// second, separate connection would be a different client, would never hold
/// focus, and every copy from it would be refused.
#[cfg(target_os = "linux")]
pub struct WaylandClipboard {
    /// `smithay_clipboard::Clipboard` owns an mpsc receiver, so it is `Send`
    /// but not `Sync`. The mutex is what makes the backend shareable; it is
    /// never contended in practice, because clipboard traffic is
    /// user-initiated and the protocol work happens on the clipboard's own
    /// thread rather than under this lock's critical path.
    inner: std::sync::Mutex<smithay_clipboard::Clipboard>,
}

#[cfg(target_os = "linux")]
impl WaylandClipboard {
    /// Attach to the `wl_display` behind `display_ptr`.
    ///
    /// # Safety
    ///
    /// `display_ptr` must be a live `wl_display *` that outlives the returned
    /// value. In practice it comes from winit's display handle and lives as
    /// long as the event loop, which outlives every widget that could copy.
    pub unsafe fn new(display_ptr: *mut std::ffi::c_void) -> Self {
        Self {
            inner: std::sync::Mutex::new(unsafe {
                smithay_clipboard::Clipboard::new(display_ptr)
            }),
        }
    }
}

#[cfg(target_os = "linux")]
impl ClipboardBackend for WaylandClipboard {
    fn set_text(&self, target: ClipboardTarget, text: &str) -> bool {
        let Ok(inner) = self.inner.lock() else {
            return false;
        };
        match target {
            ClipboardTarget::Clipboard => inner.store(text.to_string()),
            ClipboardTarget::Primary => inner.store_primary(text.to_string()),
        }
        // `store` is a queued request, not a round trip: it cannot report
        // whether the compositor accepted the selection. A failure surfaces on
        // the next read instead, which is the same contract every Wayland
        // clipboard has.
        true
    }

    fn text(&self, target: ClipboardTarget) -> Option<String> {
        let inner = self.inner.lock().ok()?;
        let read = match target {
            ClipboardTarget::Clipboard => inner.load(),
            ClipboardTarget::Primary => inner.load_primary(),
        };
        match read {
            Ok(text) => Some(text),
            // An empty selection and a failed transfer are both "nothing to
            // paste" from a widget's point of view; the distinction is only
            // useful in a log.
            Err(e) => {
                tracing::debug!(target: "ui::clipboard", error = %e, ?target, "clipboard read returned nothing");
                None
            }
        }
    }

    fn name(&self) -> &'static str {
        "wayland-data-device"
    }
}

/// Install [`WaylandClipboard`] for this process if it is running on Wayland.
///
/// Returns `true` when a Wayland backend was installed. A no-op when a backend
/// is already installed (the compositor's own, or one the app chose), when the
/// process is on X11, or when the display handle is not a Wayland one — in all
/// of those cases the caller's existing resolution is already correct.
#[cfg(target_os = "linux")]
pub fn install_wayland_backend_from_display(
    handle: &impl winit::raw_window_handle::HasDisplayHandle,
) -> bool {
    use winit::raw_window_handle::RawDisplayHandle;

    if clipboard_backend().is_some() {
        return false;
    }
    let Ok(display) = handle.display_handle() else {
        return false;
    };
    let RawDisplayHandle::Wayland(wayland) = display.as_raw() else {
        return false;
    };
    // SAFETY: the pointer comes from winit's active event loop, which owns the
    // `wl_display` for the whole run of the app; the backend is a process
    // global that is never used after the loop exits.
    let clipboard = unsafe { WaylandClipboard::new(wayland.display.as_ptr()) };
    install_clipboard_backend(Arc::new(clipboard));
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A backend that records what it was asked to do, so the resolution order
    /// in `text_selection` can be asserted against the real trait rather than
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
        assert_eq!(b.text(ClipboardTarget::Clipboard).as_deref(), Some("copied"));
        assert_eq!(b.text(ClipboardTarget::Primary).as_deref(), Some("selected"));
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
