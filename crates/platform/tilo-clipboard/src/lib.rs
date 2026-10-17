#![cfg_attr(
    test,
    allow(
        clippy::disallowed_methods,
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
#[cfg(target_os = "linux")]
mod lifecycle;
#[cfg(target_os = "linux")]
pub use lifecycle::ClipboardAttachment;

/// Runner-owned native clipboard attachment (Wayland only).
#[cfg(not(target_os = "linux"))]
pub struct ClipboardAttachment {
    _private: (),
}
pub mod conformance;
pub mod events;

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
mod native;
#[cfg(target_os = "linux")]
mod wayland;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod web;

pub use caps::{Availability, Feature, Unsupported, availability, available};

/// The Wayland backend, for the compositor's clipboard tests. Apps use
/// [`system`].
#[cfg(target_os = "linux")]
#[doc(hidden)]
pub use wayland::WaylandClipboard;

/// Largest HTML payload accepted from a clipboard offer.
pub const MAX_CLIPBOARD_HTML_BYTES: usize = 1024 * 1024;

/// Largest plain-text fallback captured with a rich clipboard offer.
pub const MAX_CLIPBOARD_PLAIN_FALLBACK_BYTES: usize = 1024 * 1024;

/// HTML and its optional plain fallback captured from the same clipboard offer.
/// Adapters must bound payloads before copying them into this value. Decoders
/// may use this fallback after rejecting HTML, without rereading the clipboard.
#[derive(Clone, PartialEq, Eq)]
pub struct RichTextOffer {
    pub html: String,
    pub plain: Option<String>,
}

impl RichTextOffer {
    /// Whether this offer fits the shared transport limits and contains HTML.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        !self.html.is_empty()
            && self.html.len() <= MAX_CLIPBOARD_HTML_BYTES
            && self
                .plain
                .as_ref()
                .is_none_or(|plain| plain.len() <= MAX_CLIPBOARD_PLAIN_FALLBACK_BYTES)
    }
}

impl std::fmt::Debug for RichTextOffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RichTextOffer")
            .field("html_bytes", &self.html.len())
            .field("plain_bytes", &self.plain.as_ref().map(String::len))
            .finish()
    }
}

/// Marks a Tilo copy as a secret: the compositor keeps it out of clipboard
/// history and never syncs it to the person's other devices. Its bytes are
/// [`SECRET_MARKER`].
pub const SECRET_MIME: &str = "application/x-tilo-secret";

/// The password-manager convention other Linux clipboard managers honour
/// (KDE Klipper, `cliphist`, `CopyQ`): an offer carrying it is kept out of
/// their histories. Its bytes are [`SECRET_MARKER`].
pub const PASSWORD_MANAGER_HINT_MIME: &str = "x-kde-passwordManagerHint";

/// What a secret copy answers for [`SECRET_MIME`] and
/// [`PASSWORD_MANAGER_HINT_MIME`].
pub const SECRET_MARKER: &[u8] = b"secret";

/// Largest PNG a clipboard image may encode to.
pub const MAX_CLIPBOARD_IMAGE_BYTES: usize = 8 * 1024 * 1024;

/// Largest RGBA buffer accepted for a clipboard image.
pub const MAX_CLIPBOARD_IMAGE_INPUT_BYTES: usize = 64 * 1024 * 1024;

/// Largest encoded image read from another app's clipboard. A larger one is
/// not read at all.
pub const MAX_PASTED_IMAGE_BYTES: usize = 32 * 1024 * 1024;

/// Encoded image types read from the clipboard, most preferred first.
pub const PASTED_IMAGE_TYPES: [&str; 3] = ["image/png", "image/jpeg", "image/webp"];

/// An encoded image on the clipboard: what a screenshot tool or an image
/// viewer copied.
#[derive(Clone, PartialEq, Eq)]
pub struct ClipboardImage {
    /// One of [`PASTED_IMAGE_TYPES`].
    pub mime: &'static str,
    /// The image file's bytes, as the copying app offered them.
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for ClipboardImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipboardImage")
            .field("mime", &self.mime)
            .field("len", &self.bytes.len())
            .finish()
    }
}

/// The most preferred image type among `offered`, if any.
pub fn preferred_image_type<'a>(
    offered: impl IntoIterator<Item = &'a str>,
) -> Option<&'static str> {
    let offered: Vec<&str> = offered.into_iter().collect();
    PASTED_IMAGE_TYPES
        .into_iter()
        .find(|mime| offered.contains(mime))
}

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

    /// Request text for an explicit paste. A native transfer may finish after
    /// this call returns; the backend must wake its owner when it answers.
    /// No answer may substitute text from an older selection or focus owner.
    fn request_text(&self, target: ClipboardTarget) -> std::sync::mpsc::Receiver<Option<String>> {
        let (reply, result) = std::sync::mpsc::channel();
        let _ = reply.send(self.text(target));
        result
    }

    /// Read HTML and its plain fallback from one Ctrl+V clipboard offer.
    /// Optional: unsupported backends return `None` without reading plain text.
    /// A successful read consumes event-scoped offers; use the captured plain
    /// fallback if decoding refuses the HTML. Never reread to recover a stale
    /// selection, a permission refusal, or an active composition.
    fn rich_text(&self) -> Option<RichTextOffer> {
        None
    }

    /// Offer an RGBA8 image on the clipboard. Optional: the default refuses,
    /// which is appropriate for backends that only carry text.
    fn set_image(&self, width: usize, height: usize, rgba: &[u8]) -> bool {
        let _ = (width, height, rgba);
        false
    }

    /// The image on the Ctrl+V clipboard, when it holds one: a screenshot,
    /// an image copied in a viewer or a browser. Optional: the default reads
    /// none, which is right for backends that only carry text.
    fn image(&self) -> Option<ClipboardImage> {
        None
    }

    /// Offer `text` only where other processes can read it. Returns `false`
    /// rather than keeping it in a buffer private to this process, so a
    /// secret is never reported as copied when it was not. The default is
    /// [`set_text`](Self::set_text), right for a backend that only reaches a
    /// shared clipboard.
    fn set_shared_text(&self, target: ClipboardTarget, text: &str) -> bool {
        self.set_text(target, text)
    }

    /// Offer `text` on the Ctrl+V clipboard marked as a secret: a password,
    /// a one-time code, an enrolment token. Other apps can still paste it as
    /// plain text, but clipboard managers and Tilo's own history and device
    /// sync are told not to keep it ([`SECRET_MIME`],
    /// [`PASSWORD_MANAGER_HINT_MIME`]).
    ///
    /// Returns `false` rather than keeping the secret in a buffer private to
    /// this process, so it is never reported as copied when it was not. The
    /// default refuses: a backend must opt in to carrying secrets.
    fn set_secret_text(&self, text: &str) -> bool {
        let _ = text;
        false
    }

    /// Start a Ctrl+C copy without waiting for it, and answer its outcome
    /// through the returned [`ClipboardCopy`]. A copy through an app's own
    /// Wayland connection waits on compositor roundtrips; this lets the UI
    /// thread go on while it does, and wakes the app when it settles.
    ///
    /// The default copies now through the methods above, so its outcome is
    /// settled before this returns.
    fn copy(&self, content: CopyContent<'_>) -> ClipboardCopy {
        copy_now(self, content)
    }

    /// Short name for diagnostics, e.g. `"compositor-mirror"`.
    fn name(&self) -> &'static str;
}

/// [`ClipboardBackend::copy`] through the blocking methods.
fn copy_now<B: ClipboardBackend + ?Sized>(backend: &B, content: CopyContent<'_>) -> ClipboardCopy {
    ClipboardCopy::settled(match content {
        CopyContent::Text(text) => backend.set_text(ClipboardTarget::Clipboard, text),
        CopyContent::SharedText(text) => backend.set_shared_text(ClipboardTarget::Clipboard, text),
        CopyContent::SecretText(text) => backend.set_secret_text(text),
        CopyContent::Image {
            width,
            height,
            rgba,
        } => backend.set_image(width, height, rgba),
    })
}

/// What a [`ClipboardBackend::copy`] puts on the Ctrl+C clipboard; each
/// matches the blocking method of the same meaning.
#[derive(Clone, Copy)]
pub enum CopyContent<'a> {
    /// [`ClipboardBackend::set_text`]: may land in an in-process buffer.
    Text(&'a str),
    /// [`ClipboardBackend::set_shared_text`]: only where other processes
    /// can read it.
    SharedText(&'a str),
    /// [`ClipboardBackend::set_secret_text`].
    SecretText(&'a str),
    /// [`ClipboardBackend::set_image`]: RGBA8, `width * height * 4` bytes.
    Image {
        width: usize,
        height: usize,
        rgba: &'a [u8],
    },
}

impl std::fmt::Debug for CopyContent<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the bytes: a secret must not reach a log.
        match self {
            Self::Text(text) => f.debug_tuple("Text").field(&text.len()).finish(),
            Self::SharedText(text) => f.debug_tuple("SharedText").field(&text.len()).finish(),
            Self::SecretText(_) => f.write_str("SecretText"),
            Self::Image { width, height, .. } => f
                .debug_struct("Image")
                .field("width", width)
                .field("height", height)
                .finish(),
        }
    }
}

/// The outcome of a [`ClipboardBackend::copy`]: copied, failed, or not
/// known yet. It settles exactly once, and settling wakes the app that
/// started the copy, so the app reads it on its next pass through its event
/// loop.
#[derive(Clone)]
#[must_use = "a copy can fail after it returns; read its outcome"]
pub struct ClipboardCopy {
    state: std::sync::Arc<CopyState>,
}

struct CopyState {
    progress: Mutex<CopyProgress>,
    settled: std::sync::Condvar,
    wake: tilo_rt::wake::WakeHandle,
}

#[derive(Default)]
struct CopyProgress {
    outcome: Option<bool>,
    /// What to run with the outcome once it settles.
    then: Vec<Box<dyn FnOnce(bool) + Send>>,
}

impl CopyState {
    fn progress(&self) -> std::sync::MutexGuard<'_, CopyProgress> {
        self.progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl ClipboardCopy {
    /// A copy whose outcome is already known.
    pub fn settled(copied: bool) -> Self {
        let (copy, settle) = Self::pending();
        settle.settle(copied);
        copy
    }

    /// A copy whose outcome `CopySettler` reports later. Call on the thread
    /// whose event loop should wake when it does.
    pub fn pending() -> (Self, CopySettler) {
        let state = std::sync::Arc::new(CopyState {
            progress: Mutex::default(),
            settled: std::sync::Condvar::new(),
            wake: tilo_rt::wake::WakeHandle::new(),
        });
        (
            Self {
                state: std::sync::Arc::clone(&state),
            },
            CopySettler { state: Some(state) },
        )
    }

    /// `Some(true)` once copied, `Some(false)` once failed, `None` while the
    /// clipboard has not answered.
    #[must_use]
    pub fn outcome(&self) -> Option<bool> {
        self.state.progress().outcome
    }

    /// Run `then` with the outcome: at once when it is settled, else on the
    /// thread that settles it. A backend builds on another's copy with it,
    /// as the native clipboard falls back to its in-process buffer.
    pub fn on_settled(&self, then: impl FnOnce(bool) + Send + 'static) {
        let mut progress = self.state.progress();
        match progress.outcome {
            Some(copied) => {
                drop(progress);
                then(copied);
            }
            None => progress.then.push(Box::new(then)),
        }
    }

    /// Wait for the outcome. Only off the UI thread, which must never wait
    /// on the compositor: a worker finishing an effect, say. Every copy
    /// settles, if only as failed when whatever answers it goes away.
    #[must_use]
    pub fn wait(&self) -> bool {
        let mut progress = self.state.progress();
        loop {
            if let Some(copied) = progress.outcome {
                return copied;
            }
            progress = self
                .state
                .settled
                .wait(progress)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

impl std::fmt::Debug for ClipboardCopy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ClipboardCopy")
            .field(&self.outcome())
            .finish()
    }
}

/// Reports a pending [`ClipboardCopy`]'s outcome. Dropping it unsettled
/// reports a failure, so a copy whose worker went away never stays pending.
pub struct CopySettler {
    state: Option<std::sync::Arc<CopyState>>,
}

impl CopySettler {
    /// Settle the copy and wake the app that started it.
    pub fn settle(mut self, copied: bool) {
        self.finish(copied);
    }

    fn finish(&mut self, copied: bool) {
        if let Some(state) = self.state.take() {
            let then = {
                let mut progress = state.progress();
                progress.outcome = Some(copied);
                std::mem::take(&mut progress.then)
            };
            state.settled.notify_all();
            for then in then {
                then(copied);
            }
            state.wake.wake();
        }
    }
}

impl Drop for CopySettler {
    fn drop(&mut self) {
        self.finish(false);
    }
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
/// Pass the runner's owned display handle, not a borrow of the event loop.
/// Keep the returned guard until runner close. Dropping it refuses new calls
/// and schedules teardown without waiting on protocol workers.
pub fn attach_wayland_display(
    handle: impl raw_window_handle::HasDisplayHandle + Send + Sync + 'static,
) -> Option<ClipboardAttachment> {
    attach(handle)
}

#[cfg(target_os = "linux")]
fn attach(
    handle: impl raw_window_handle::HasDisplayHandle + Send + Sync + 'static,
) -> Option<ClipboardAttachment> {
    native::attach_wayland(handle)
}

#[cfg(not(target_os = "linux"))]
fn attach(
    _handle: impl raw_window_handle::HasDisplayHandle + Send + Sync + 'static,
) -> Option<ClipboardAttachment> {
    None
}

/// A clipboard private to this process, for hosts that offer no shared one.
///
/// Keeps copy → paste round-tripping inside the process so editors stay usable
/// and the caret/clipboard core stays testable headlessly. It refuses
/// [`ClipboardBackend::set_shared_text`], secrets and images.
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
    fn a_copy_settles_once_and_runs_what_waits_on_it() {
        let (copy, settler) = ClipboardCopy::pending();
        let (ran, outcomes) = std::sync::mpsc::channel();
        let early = ran.clone();
        copy.on_settled(move |copied| early.send(("before", copied)).unwrap());
        assert_eq!(copy.outcome(), None);
        assert!(
            outcomes.try_recv().is_err(),
            "nothing runs before it settles"
        );
        settler.settle(true);
        assert_eq!(copy.outcome(), Some(true));
        assert!(copy.wait());
        copy.on_settled(move |copied| ran.send(("after", copied)).unwrap());
        assert_eq!(
            outcomes.try_iter().collect::<Vec<_>>(),
            [("before", true), ("after", true)]
        );

        let (copy, settler) = ClipboardCopy::pending();
        drop(settler);
        assert_eq!(copy.outcome(), Some(false), "an abandoned copy failed");
    }

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
