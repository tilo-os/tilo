#![cfg_attr(
    test,
    allow(
        clippy::disallowed_methods,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

//! The clipboard: one API on Linux, macOS and the web.
//!
//! [`ClipboardBackend`] is the seam a host fills; [`system`] is this target's
//! backend:
//!
//! | Target | Backend |
//! |---|---|
//! | Linux | the app's own Wayland connection once the runner attaches it ([`attach_wayland_display`]), `arboard` on X11 |
//! | macOS | `NSPasteboard` through `arboard` |
//! | Web | the trusted clipboard event being dispatched ([`events`]), else a synchronous copy inside the user's click |
//!
//! Each falls back to an [`InProcessClipboard`] when there is no system
//! clipboard, and logs that once. [`availability`] says what the target can
//! do; a backend may do less at run time, never more.
//!
//! `arboard`'s Wayland path needs `zwlr_data_control_manager_v1`, a
//! clipboard-*manager* protocol Tilo deliberately withholds from sandboxed
//! apps: it exposes every selection on the seat with no focus check, and the
//! fleet copies TOTP codes and recovery phrases (see
//! `crates/session/shell/src/compositor/selection.rs`). The Wayland backend's
//! `wl_data_device` is focus-gated instead; `arboard` is kept only for X11.

use std::sync::{Mutex, OnceLock};

mod caps;
pub mod conformance;
pub mod events;

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
mod native;
#[cfg(target_os = "linux")]
mod wayland;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod web;

pub use caps::{Availability, CAPABILITIES, Feature, Unsupported, availability};

/// The Wayland backend, for the compositor's clipboard tests. Apps use
/// [`system`].
#[cfg(target_os = "linux")]
#[doc(hidden)]
pub use wayland::WaylandClipboard;

/// Largest PNG a clipboard image may encode to.
pub const MAX_CLIPBOARD_IMAGE_BYTES: usize = 8 * 1024 * 1024;

/// Largest RGBA buffer accepted for a clipboard image.
pub const MAX_CLIPBOARD_IMAGE_INPUT_BYTES: usize = 64 * 1024 * 1024;

/// Which selection a clipboard operation refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardTarget {
    /// The Ctrl+C / Ctrl+V clipboard.
    Clipboard,
    /// The select-then-middle-click-paste primary selection. Distinct storage
    /// from [`ClipboardTarget::Clipboard`] — a terminal writes it on every
    /// drag-select without disturbing what the user last copied. Only Linux
    /// has one ([`Feature::PrimarySelection`]).
    Primary,
}

/// A source of clipboard content for this process.
///
/// Implementors must be cheap to call and must never block on the compositor's
/// event loop from a widget's event handler — a host backend reads a mirror
/// in memory, and the Wayland backend does its protocol work on its own
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

/// This target's clipboard, shared by every caller in the process.
pub fn system() -> std::sync::Arc<dyn ClipboardBackend> {
    static SYSTEM: OnceLock<std::sync::Arc<dyn ClipboardBackend>> = OnceLock::new();
    SYSTEM.get_or_init(backend).clone()
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn backend() -> std::sync::Arc<dyn ClipboardBackend> {
    std::sync::Arc::new(native::NativeClipboard::default())
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
fn backend() -> std::sync::Arc<dyn ClipboardBackend> {
    std::sync::Arc::new(web::BrowserClipboard)
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    windows,
    all(target_arch = "wasm32", target_os = "unknown")
)))]
fn backend() -> std::sync::Arc<dyn ClipboardBackend> {
    std::sync::Arc::new(InProcessClipboard::default())
}

/// Route [`system`] through the app's own Wayland connection, so copies go
/// through the connection that owns the focused surface — the compositor only
/// accepts `set_selection` from that one.
///
/// The runner calls this once it has a display. Returns `true` when it
/// attached: never on a display that is not Wayland, nor a second time.
pub fn attach_wayland_display(handle: &impl raw_window_handle::HasDisplayHandle) -> bool {
    attach(handle)
}

#[cfg(target_os = "linux")]
fn attach(handle: &impl raw_window_handle::HasDisplayHandle) -> bool {
    native::attach_wayland(handle)
}

#[cfg(not(target_os = "linux"))]
fn attach(_handle: &impl raw_window_handle::HasDisplayHandle) -> bool {
    false
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Tests that write the real system clipboard take turns.
    pub(crate) static SYSTEM: Mutex<()> = Mutex::new(());

    #[test]
    fn the_in_process_clipboard_conforms() {
        conformance::check_local(&InProcessClipboard::default());
    }

    #[test]
    fn the_system_clipboard_conforms_with_or_without_a_system_clipboard() {
        let _system = SYSTEM.lock();
        let system = system();
        conformance::check_system(&*system);
        assert!(std::ptr::addr_eq(
            std::sync::Arc::as_ptr(&system),
            std::sync::Arc::as_ptr(&self::system())
        ));
    }
}
