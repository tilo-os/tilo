//! The operating system's clipboard through `arboard`: Wayland's data-control
//! protocol, X11, `NSPasteboard` and the Windows clipboard, with a buffer
//! private to the process when the seat offers none.
//!
//! `arboard` is the protocol made for a client without focus: on Wayland it
//! talks `zwlr_data_control_manager_v1` (falling back to X11/XWayland when the
//! compositor lacks it), which is what lets the launcher — no surface, no
//! keyboard focus — own the selection on the person's behalf.

use crate::{ClipboardBackend, ClipboardTarget, InProcessClipboard, warn_fallback_once};

#[derive(Default)]
pub(crate) struct NativeClipboard {
    local: std::sync::Arc<InProcessClipboard>,
}

/// Whether `target` has no system storage here, and stays in the process.
/// Only Linux has a primary selection; elsewhere select/middle-click stays
/// local to the app instead of overwriting the user's Ctrl+C clipboard.
fn local_only(target: ClipboardTarget) -> bool {
    target == ClipboardTarget::Primary && !cfg!(target_os = "linux")
}

impl NativeClipboard {
    fn set_shared(target: ClipboardTarget, text: &str) -> bool {
        if local_only(target) {
            return false;
        }
        system::set_text(target, text)
    }
}

impl ClipboardBackend for NativeClipboard {
    fn request_text(&self, target: ClipboardTarget) -> std::sync::mpsc::Receiver<Option<String>> {
        let (reply, result) = std::sync::mpsc::channel();
        let _ = reply.send(self.text(target));
        result
    }

    fn set_text(&self, target: ClipboardTarget, text: &str) -> bool {
        if Self::set_shared(target, text) {
            return true;
        }
        if local_only(target) {
            return self.local.set_text(target, text);
        }
        warn_fallback_once("copy");
        self.local.set_text(target, text)
    }

    fn set_shared_text(&self, target: ClipboardTarget, text: &str) -> bool {
        Self::set_shared(target, text)
    }

    fn set_secret_text(&self, text: &str) -> bool {
        system::set_secret_text(text)
    }

    fn set_rich_text(&self, html: &str, plain: &str) -> bool {
        if system::set_html(html, plain) {
            return true;
        }
        warn_fallback_once("copy");
        self.local.set_rich_text(html, plain)
    }

    fn text(&self, target: ClipboardTarget) -> Option<String> {
        if local_only(target) {
            return self.local.text(target);
        }
        if let SystemText::Read(text) = system::text(target) {
            return text;
        }
        warn_fallback_once("paste");
        self.local.text(target)
    }

    fn set_image(&self, width: usize, height: usize, rgba: &[u8]) -> bool {
        let well_formed = width > 0
            && height > 0
            && rgba.len() <= crate::MAX_CLIPBOARD_IMAGE_INPUT_BYTES
            && width
                .checked_mul(height)
                .and_then(|pixels| pixels.checked_mul(4))
                == Some(rgba.len());
        if !well_formed {
            return false;
        }
        system::set_image(width, height, rgba)
    }

    fn rich_text(&self) -> Option<crate::RichTextOffer> {
        match system::html() {
            SystemText::Read(html) => html
                .map(|html| crate::RichTextOffer {
                    html,
                    plain: match system::text(ClipboardTarget::Clipboard) {
                        SystemText::Read(plain) => plain,
                        SystemText::Unavailable => None,
                    },
                })
                .filter(crate::RichTextOffer::is_valid),
            SystemText::Unavailable => self.local.rich_text(),
        }
    }

    fn image(&self) -> Option<crate::ClipboardImage> {
        system::image()
    }

    fn name(&self) -> &'static str {
        "native"
    }
}

enum SystemText {
    Unavailable,
    /// The clipboard was read: its text, or `None` when it holds none. A
    /// Linux build without the `os` feature has no such clipboard to read.
    #[cfg_attr(
        all(target_os = "linux", not(feature = "os")),
        allow(
            dead_code,
            reason = "a Linux build without os never reads a system clipboard"
        )
    )]
    Read(Option<String>),
}

/// The system clipboard through `arboard`: Wayland data-control (and X11 as
/// its fallback) with the `os` feature, `NSPasteboard`, the Windows clipboard.
#[cfg(any(not(target_os = "linux"), feature = "os"))]
mod system {
    use std::sync::{Mutex, OnceLock};

    use crate::ClipboardTarget;

    pub(super) fn set_text(target: ClipboardTarget, text: &str) -> bool {
        let Some(cb) = arboard_clipboard() else {
            return false;
        };
        let Ok(mut handle) = cb.lock() else {
            return false;
        };
        set_arboard_text(&mut handle, target, text)
    }

    pub(super) fn set_secret_text(text: &str) -> bool {
        let Some(cb) = arboard_clipboard() else {
            return false;
        };
        let Ok(mut handle) = cb.lock() else {
            return false;
        };
        set_arboard_secret_text(&mut handle, text)
    }

    pub(super) fn text(target: ClipboardTarget) -> super::SystemText {
        let Some(mut handle) = arboard_clipboard().and_then(|cb| cb.lock().ok()) else {
            return super::SystemText::Unavailable;
        };
        super::SystemText::Read(arboard_text(&mut handle, target))
    }

    /// Formatted text: `html` with `plain` as its fallback.
    pub(super) fn set_html(html: &str, plain: &str) -> bool {
        let Some(mut handle) = arboard_clipboard().and_then(|cb| cb.lock().ok()) else {
            return false;
        };
        handle.set_html(html, Some(plain)).is_ok()
    }

    pub(super) fn html() -> super::SystemText {
        let Some(mut handle) = arboard_clipboard().and_then(|cb| cb.lock().ok()) else {
            return super::SystemText::Unavailable;
        };
        super::SystemText::Read(
            handle
                .get()
                .html()
                .ok()
                .filter(|html| html.len() <= crate::MAX_CLIPBOARD_HTML_BYTES),
        )
    }

    pub(super) fn set_image(width: usize, height: usize, rgba: &[u8]) -> bool {
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

    #[cfg(target_os = "linux")]
    fn set_arboard_text(
        handle: &mut arboard::Clipboard,
        target: ClipboardTarget,
        text: &str,
    ) -> bool {
        use arboard::{LinuxClipboardKind, SetExtLinux};
        match target {
            ClipboardTarget::Clipboard => handle.set_text(text.to_string()).is_ok(),
            ClipboardTarget::Primary => handle
                .set()
                .clipboard(LinuxClipboardKind::Primary)
                .text(text.to_string())
                .is_ok(),
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn set_arboard_text(
        handle: &mut arboard::Clipboard,
        _target: ClipboardTarget,
        text: &str,
    ) -> bool {
        handle.set_text(text.to_string()).is_ok()
    }

    /// A copy clipboard managers are asked to leave out of their histories:
    /// `x-kde-passwordManagerHint` (data-control offers it too), the
    /// `nspasteboard.org` marker on macOS, and Windows' history and cloud-sync
    /// exclusion formats.
    #[cfg(target_os = "linux")]
    fn set_arboard_secret_text(handle: &mut arboard::Clipboard, text: &str) -> bool {
        use arboard::SetExtLinux;
        handle
            .set()
            .exclude_from_history()
            .text(text.to_string())
            .is_ok()
    }

    #[cfg(target_os = "macos")]
    fn set_arboard_secret_text(handle: &mut arboard::Clipboard, text: &str) -> bool {
        use arboard::SetExtApple;
        handle
            .set()
            .exclude_from_history()
            .text(text.to_string())
            .is_ok()
    }

    #[cfg(windows)]
    fn set_arboard_secret_text(handle: &mut arboard::Clipboard, text: &str) -> bool {
        use arboard::SetExtWindows;
        handle
            .set()
            .exclude_from_history()
            .exclude_from_cloud()
            .text(text.to_string())
            .is_ok()
    }

    #[cfg(target_os = "linux")]
    fn arboard_text(handle: &mut arboard::Clipboard, target: ClipboardTarget) -> Option<String> {
        use arboard::{GetExtLinux, LinuxClipboardKind};
        match target {
            ClipboardTarget::Clipboard => handle.get_text().ok(),
            ClipboardTarget::Primary => handle
                .get()
                .clipboard(LinuxClipboardKind::Primary)
                .text()
                .ok(),
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn arboard_text(handle: &mut arboard::Clipboard, _target: ClipboardTarget) -> Option<String> {
        handle.get_text().ok()
    }

    pub(super) fn image() -> Option<crate::ClipboardImage> {
        let image = arboard_clipboard()?.lock().ok()?.get_image().ok()?;
        let bytes = crate::images::encode_png(image.width, image.height, &image.bytes)?;
        Some(crate::ClipboardImage {
            mime: "image/png",
            bytes,
        })
    }

    /// The process's one `arboard` handle: Wayland's data-control protocol on
    /// a Wayland session, X11/XWayland where the compositor lacks it, and the
    /// platform clipboard elsewhere.
    fn arboard_clipboard() -> Option<&'static Mutex<arboard::Clipboard>> {
        static CB: OnceLock<Option<Mutex<arboard::Clipboard>>> = OnceLock::new();
        CB.get_or_init(|| arboard::Clipboard::new().ok().map(Mutex::new))
            .as_ref()
    }
}

/// A Linux build without `os` is a Wayland client only, as an app in Tilo's
/// sandbox is: it copies and pastes through the service, never through a
/// library in its own process. The launcher turns `os` on to serve that
/// service from the person's own desktop.
#[cfg(all(target_os = "linux", not(feature = "os")))]
mod system {
    use crate::ClipboardTarget;

    pub(super) fn set_text(_target: ClipboardTarget, _text: &str) -> bool {
        false
    }

    pub(super) fn set_secret_text(_text: &str) -> bool {
        false
    }

    pub(super) fn set_html(_html: &str, _plain: &str) -> bool {
        false
    }

    pub(super) fn html() -> super::SystemText {
        super::SystemText::Unavailable
    }

    pub(super) fn text(_target: ClipboardTarget) -> super::SystemText {
        super::SystemText::Unavailable
    }

    pub(super) fn set_image(_width: usize, _height: usize, _rgba: &[u8]) -> bool {
        false
    }

    pub(super) fn image() -> Option<crate::ClipboardImage> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn a_native_copy_is_pasted_back_and_a_selection_does_not_replace_it;

        phase a_primary_selection_never_replaces_an_explicit_copy;
        {
            let _system = crate::tests::SYSTEM.lock();
            let clipboard = NativeClipboard::default();
            clipboard.set_text(ClipboardTarget::Clipboard, "explicit copy");
            clipboard.set_text(ClipboardTarget::Primary, "selected text");
            assert_eq!(
                clipboard.text(ClipboardTarget::Primary).as_deref(),
                Some("selected text")
            );
            if !cfg!(target_os = "linux") {
                assert_eq!(
                    clipboard.text(ClipboardTarget::Clipboard).as_deref(),
                    Some("explicit copy")
                );
            }
        }

        phase native_clipboard_round_trips_with_or_without_a_system_clipboard;
        {
            let _system = crate::tests::SYSTEM.lock();
            let clipboard = NativeClipboard::default();
            let payload = "https://example.com/feed.xml";
            clipboard.set_text(ClipboardTarget::Clipboard, payload);
            assert_eq!(
                clipboard.text(ClipboardTarget::Clipboard).as_deref(),
                Some(payload)
            );
        }
    }
}
