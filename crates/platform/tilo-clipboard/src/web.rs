//! The page's clipboard: the trusted clipboard event being dispatched, or a
//! synchronous copy inside the user's click. Browsers allow nothing else, so
//! outside those a read answers `None` and a write `false`.

use wasm_bindgen::prelude::*;

use crate::events::{self, CopyReport};
use crate::{ClipboardBackend, ClipboardTarget};

#[wasm_bindgen(module = "/web/clipboard.mjs")]
extern "C" {
    #[wasm_bindgen(js_name = writeClipboardText)]
    fn write_clipboard_text(text: &str, report: &js_sys::Function) -> bool;
}

pub(crate) struct BrowserClipboard;

impl ClipboardBackend for BrowserClipboard {
    fn name(&self) -> &'static str {
        "browser-clipboard-event"
    }

    fn set_text(&self, target: ClipboardTarget, text: &str) -> bool {
        if target != ClipboardTarget::Clipboard {
            return false;
        }
        if let Some(written) = events::write(text) {
            return written;
        }
        let Some(report) = events::input_report() else {
            return false;
        };
        // The copy command runs its event synchronously, so the closure is
        // called, if at all, before this returns.
        let forward =
            Closure::<dyn Fn(bool, Option<String>)>::new(move |ok, error: Option<String>| {
                report(if ok {
                    CopyReport::Copied
                } else {
                    CopyReport::Failed(error.unwrap_or_default())
                });
            });
        write_clipboard_text(text, forward.as_ref().unchecked_ref())
    }

    fn text(&self, target: ClipboardTarget) -> Option<String> {
        if target != ClipboardTarget::Clipboard {
            return None;
        }
        events::read()
    }
}
