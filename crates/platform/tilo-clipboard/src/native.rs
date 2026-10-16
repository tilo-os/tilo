//! The desktop clipboard on Linux, macOS and Windows, resolved per operation.
//!
//! Order: the app's Wayland connection once attached (Linux), else `arboard`
//! when this process is not a Wayland client (X11, `NSPasteboard`, the Windows
//! clipboard), else a process-local buffer that announces itself in the log.

use std::sync::{Mutex, OnceLock};

use crate::{ClipboardBackend, ClipboardTarget, InProcessClipboard, warn_fallback_once};

#[derive(Default)]
pub(crate) struct NativeClipboard {
    local: InProcessClipboard,
}

/// The app's Wayland connection, once [`attach_wayland`] ran.
#[cfg(target_os = "linux")]
static WAYLAND: OnceLock<crate::wayland::WaylandClipboard> = OnceLock::new();

#[cfg(target_os = "linux")]
pub(crate) fn attach_wayland(handle: &impl raw_window_handle::HasDisplayHandle) -> bool {
    use raw_window_handle::RawDisplayHandle;

    let Ok(display) = handle.display_handle() else {
        return false;
    };
    let RawDisplayHandle::Wayland(wayland) = display.as_raw() else {
        return false;
    };
    let mut attached = false;
    WAYLAND.get_or_init(|| {
        attached = true;
        // SAFETY: the pointer comes from the runner's active event loop, which
        // owns the `wl_display` for the whole run of the app; the backend is a
        // process global that is never used after the loop exits.
        unsafe { crate::wayland::WaylandClipboard::new(wayland.display.as_ptr()) }
    });
    attached
}

#[cfg(target_os = "linux")]
fn wayland() -> Option<&'static crate::wayland::WaylandClipboard> {
    WAYLAND.get()
}

#[cfg(not(target_os = "linux"))]
fn wayland() -> Option<&'static NoWayland> {
    None
}

/// Stand-in so the resolution order reads the same on every desktop.
#[cfg(not(target_os = "linux"))]
struct NoWayland;

#[cfg(not(target_os = "linux"))]
#[allow(clippy::unused_self, reason = "mirrors the Wayland backend's methods")]
impl NoWayland {
    fn set_text(&self, _target: ClipboardTarget, _text: &str) -> bool {
        false
    }
    fn text(&self, _target: ClipboardTarget) -> Option<String> {
        None
    }
    fn request_text(&self, _target: ClipboardTarget) -> std::sync::mpsc::Receiver<Option<String>> {
        let (reply, result) = std::sync::mpsc::channel();
        let _ = reply.send(None);
        result
    }
    fn set_secret_text(&self, _text: &str) -> bool {
        false
    }
    fn set_image(&self, _width: usize, _height: usize, _rgba: &[u8]) -> bool {
        false
    }
    fn image(&self) -> Option<crate::ClipboardImage> {
        None
    }
}

/// Whether `target` has no system storage here, and stays in the process.
/// Only Linux has a primary selection; elsewhere select/middle-click stays
/// local to the app instead of overwriting the user's Ctrl+C clipboard.
fn local_only(target: ClipboardTarget) -> bool {
    target == ClipboardTarget::Primary && !cfg!(target_os = "linux")
}

impl NativeClipboard {
    /// Write to a clipboard other processes can read, if there is one.
    fn set_shared(target: ClipboardTarget, text: &str) -> bool {
        if let Some(wayland) = wayland()
            && wayland.set_text(target, text)
        {
            return true;
        }
        if local_only(target) {
            return false;
        }
        let Some(cb) = arboard_clipboard() else {
            return false;
        };
        let Ok(mut handle) = cb.lock() else {
            return false;
        };
        set_arboard_text(&mut handle, target, text)
    }
}

#[cfg(target_os = "linux")]
fn set_arboard_text(handle: &mut arboard::Clipboard, target: ClipboardTarget, text: &str) -> bool {
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
fn set_arboard_text(handle: &mut arboard::Clipboard, _target: ClipboardTarget, text: &str) -> bool {
    handle.set_text(text.to_string()).is_ok()
}

/// A copy clipboard managers are asked to leave out of their histories:
/// `x-kde-passwordManagerHint` on X11, the `nspasteboard.org` marker on
/// macOS, and Windows' history and cloud-sync exclusion formats.
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

impl ClipboardBackend for NativeClipboard {
    fn request_text(&self, target: ClipboardTarget) -> std::sync::mpsc::Receiver<Option<String>> {
        if let Some(wayland) = wayland() {
            return wayland.request_text(target);
        }
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
        // An attached Wayland connection is authoritative: its refusal must
        // not route a secret through another clipboard.
        if let Some(wayland) = wayland() {
            return wayland.set_text(target, text);
        }
        Self::set_shared(target, text)
    }

    fn set_secret_text(&self, text: &str) -> bool {
        // Like `set_shared_text`: an attached Wayland connection is
        // authoritative, and nothing falls back to the in-process buffer.
        if let Some(wayland) = wayland() {
            return wayland.set_secret_text(text);
        }
        let Some(cb) = arboard_clipboard() else {
            return false;
        };
        let Ok(mut handle) = cb.lock() else {
            return false;
        };
        set_arboard_secret_text(&mut handle, text)
    }

    fn text(&self, target: ClipboardTarget) -> Option<String> {
        if let Some(wayland) = wayland() {
            return wayland.text(target);
        }
        if local_only(target) {
            return self.local.text(target);
        }
        if let Some(cb) = arboard_clipboard()
            && let Ok(mut handle) = cb.lock()
        {
            return arboard_text(&mut handle, target);
        }
        warn_fallback_once("paste");
        self.local.text(target)
    }

    fn set_image(&self, width: usize, height: usize, rgba: &[u8]) -> bool {
        if let Some(wayland) = wayland() {
            return wayland.set_image(width, height, rgba);
        }
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

    fn rich_text(&self) -> Option<crate::RichTextOffer> {
        #[cfg(target_os = "linux")]
        if let Some(wayland) = WAYLAND.get() {
            return wayland.rich_text();
        }
        None
    }

    fn image(&self) -> Option<crate::ClipboardImage> {
        if let Some(wayland) = wayland() {
            return wayland.image();
        }
        arboard_image()
    }

    fn name(&self) -> &'static str {
        "native"
    }
}

/// The X11 clipboard's image, as a PNG.
#[cfg(target_os = "linux")]
fn arboard_image() -> Option<crate::ClipboardImage> {
    let image = arboard_clipboard()?.lock().ok()?.get_image().ok()?;
    let bytes = crate::wayland::encode_png(image.width, image.height, &image.bytes)?;
    Some(crate::ClipboardImage {
        mime: "image/png",
        bytes,
    })
}

/// Reading an image needs an encoder this target does not carry.
#[cfg(not(target_os = "linux"))]
fn arboard_image() -> Option<crate::ClipboardImage> {
    None
}

/// Lazy global `arboard` handle, used **only off Wayland**.
///
/// On Wayland `arboard` talks `zwlr_data_control_manager_v1`, which Tilo does
/// not offer to sandboxed apps; an app there goes through the Wayland backend
/// and `wl_data_device` instead. Construction is skipped rather than attempted
/// on Wayland because arboard's probe there is slow, and a handle that fails
/// on every call is worse than no handle.
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
    cfg!(target_os = "linux") && std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Off Linux the primary selection is the app's own: a drag-select must
    /// not replace what the user explicitly copied.
    #[test]
    fn a_primary_selection_never_replaces_an_explicit_copy() {
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

    /// Copy → paste must round-trip within the process even with no system
    /// clipboard (the headless test env), and equally when a real clipboard
    /// is present.
    #[test]
    fn native_clipboard_round_trips_with_or_without_a_system_clipboard() {
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
