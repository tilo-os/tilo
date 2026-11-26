//! The clipboard: what the person copied, and what pastes into the app now.
//!
//! Copying reaches the device (the person's clipboard, outside Tilo) and pasting reads what only
//! the device knows, so both are effects an action declares with `uses clipboard`: an invocation
//! that copies is recorded as reaching outside, and one that pastes records what it read, so a
//! replay pastes what the person pasted and not whatever is on the clipboard then. Neither is
//! read from a view: a view is drawn again from what it is shown.
//!
//! The device's answer is a host's ([`ClipboardHost`], [`crate::Runtime::set_clipboard`]). Until a
//! host is set nothing copies and nothing pastes, and the code says so rather than pretending the
//! clipboard was empty.

/// The media type a pasted image is asked for, in the order the device's clipboard is asked: the
/// types an image copied anywhere (a screenshot, a browser, a viewer) is offered as.
pub const IMAGE_TYPES: [&str; 3] = ["image/png", "image/jpeg", "image/webp"];

/// How long a secret copy (`clipboard.copy_secret`) stays on the clipboard: long enough to paste
/// it where it goes, short enough that it is not still there for whatever is pasted next.
pub const SECRET_CLEARS_AFTER: std::time::Duration = std::time::Duration::from_secs(45);

/// What the device's clipboard answers (`Runtime::set_clipboard`).
pub trait ClipboardHost {
    /// Offer `bytes` of `mime` (`text/plain` or one of [`IMAGE_TYPES`]) as what the person copied.
    /// False when the device did not take it: no selection was available, or it carries no image.
    fn copy(&self, mime: &str, bytes: &[u8]) -> bool;

    /// Offer `text` as a secret copy (a password, a token, a key): histories and device sync are
    /// told not to keep it, and it leaves the clipboard after `clear_after` unless something else
    /// was copied meanwhile. False when the device did not take it; a host that cannot keep a
    /// secret's copy apart refuses it rather than copy it as any text.
    fn copy_secret(&self, text: &str, clear_after: std::time::Duration) -> bool {
        let _ = (text, clear_after);
        false
    }

    /// What the selection holds of the first of `types` it offers, as its media type and bytes.
    /// None when it holds none of them, or the read did not finish.
    fn paste(&self, types: &[&str]) -> Option<(String, Vec<u8>)>;
}
