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

use crate::abi::Imp;
use crate::core::{Args, Core, HResult, ret_i32, trap};
use crate::engine::{HostFail, Wasm};
use crate::value::Value;

/// The media type a pasted image is asked for, in the order the device's clipboard is asked: the
/// types an image copied anywhere (a screenshot, a browser, a viewer) is offered as.
pub const IMAGE_TYPES: [&str; 3] = ["image/png", "image/jpeg", "image/webp"];

/// What the device's clipboard answers (`Runtime::set_clipboard`).
pub trait ClipboardHost {
    /// Offer `bytes` of `mime` (`text/plain` or one of [`IMAGE_TYPES`]) as what the person copied.
    /// False when the device did not take it: no selection was available, or it carries no image.
    fn copy(&self, mime: &str, bytes: &[u8]) -> bool;

    /// What the selection holds of the first of `types` it offers, as its media type and bytes.
    /// None when it holds none of them, or the read did not finish.
    fn paste(&self, types: &[&str]) -> Option<(String, Vec<u8>)>;
}

impl Core {
    /// `clipboard.copy`, `clipboard.copy_image`, `clipboard.paste` and `clipboard.paste_image`.
    pub(crate) fn clipboard_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        match imp {
            Imp::ClipboardCopyText => {
                let text = self.text(a.h(0)?)?;
                ret_i32(i32::from(
                    self.clipboard_copy("text/plain", text.as_bytes()),
                ))
            }
            Imp::ClipboardCopyImage => {
                let file = self.file_arg(a.h(0)?)?;
                if !IMAGE_TYPES.contains(&file.media.as_str()) {
                    return trap(format!(
                        "`{}` is {}, and the clipboard holds an image of {}",
                        file.name,
                        file.media,
                        IMAGE_TYPES.join(", ")
                    ));
                }
                let Some(bytes) = self
                    .blobs
                    .borrow()
                    .as_ref()
                    .and_then(|blobs| blobs.read(&file.hash))
                else {
                    return trap(format!(
                        "the content of `{}` is not on this device",
                        file.name
                    ));
                };
                ret_i32(i32::from(self.clipboard_copy(&file.media, &bytes)))
            }
            Imp::ClipboardPasteText => match self.clipboard_paste(&["text/plain"]) {
                Some((_, bytes)) => match String::from_utf8(bytes) {
                    Ok(text) => self.ret_h(Value::some(Value::text(text))),
                    Err(_) => trap("the clipboard holds text that is not UTF-8"),
                },
                None => self.ret_h(Value::none()),
            },
            Imp::ClipboardPasteImage => match self.clipboard_paste(&IMAGE_TYPES) {
                Some((mime, bytes)) => {
                    let blobs = self.blobs.borrow();
                    let Some(blobs) = blobs.as_ref() else {
                        return trap("this host keeps no files");
                    };
                    let name = format!("pasted.{}", extension_of(&mime));
                    let file = blobs.keep(&name, &mime, &bytes).map_err(HostFail::Trap)?;
                    self.ret_h(Value::some(Value::file(file)))
                }
                None => self.ret_h(Value::none()),
            },
            _ => trap("not a clipboard import"),
        }
    }

    /// Whether the device took `bytes` of `mime` as the person's copy: false on a host that
    /// serves no clipboard, as a device without one does.
    fn clipboard_copy(&self, mime: &str, bytes: &[u8]) -> bool {
        let clipboard = self.clipboard.borrow();
        clipboard
            .as_ref()
            .is_some_and(|host| host.copy(mime, bytes))
    }

    /// What the selection holds of the first of `types` it offers; none on a host that serves no
    /// clipboard.
    fn clipboard_paste(&self, types: &[&str]) -> Option<(String, Vec<u8>)> {
        let clipboard = self.clipboard.borrow();
        clipboard.as_ref().and_then(|host| host.paste(types))
    }
}

/// The file extension a pasted image of `mime` is kept under.
fn extension_of(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        _ => "png",
    }
}
