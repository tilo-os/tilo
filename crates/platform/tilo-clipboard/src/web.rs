//! The page's clipboard: the trusted clipboard event being dispatched, or a
//! synchronous copy inside the user's click, or a copy the click claims for
//! text that arrives after it ([`ClipboardBackend::reserve_text`]). Browsers
//! allow nothing else, so outside those a read answers `None` and a write
//! `false`.

use std::cell::RefCell;
use std::collections::HashMap;

use wasm_bindgen::prelude::*;

use crate::events::{self, CopyReport};
use crate::{ClipboardBackend, ClipboardCopy, ClipboardTarget, ReservedCopy};

#[wasm_bindgen(module = "/web/clipboard.mjs")]
extern "C" {
    #[wasm_bindgen(js_name = writeClipboardText)]
    fn write_clipboard_text(text: &str, report: &js_sys::Function) -> bool;
    #[wasm_bindgen(js_name = writeClipboardRich)]
    fn write_clipboard_rich(html: &str, plain: &str, report: &js_sys::Function) -> bool;
    #[wasm_bindgen(js_name = reserveClipboardText)]
    fn reserve_clipboard_text(report: &JsValue) -> JsValue;
}

thread_local! {
    /// The `fill` of each copy claimed and not filled yet, by reservation:
    /// a page's functions stay on its thread, the reservation may move.
    static RESERVED: RefCell<(u64, HashMap<u64, js_sys::Function>)> =
        RefCell::new((0, HashMap::new()));
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

    fn reserve_text(&self) -> Option<ReservedCopy> {
        let report = events::input_report()?;
        let (copy, settle) = ClipboardCopy::pending();
        let settled = Closure::once_into_js(move |ok: bool, error: Option<String>| {
            // A claim given up is no failure to tell the person about.
            match (ok, error) {
                (true, _) => report(CopyReport::Copied),
                (false, Some(error)) => report(CopyReport::Failed(error)),
                (false, None) => {}
            }
            settle.settle(ok);
        });
        let fill: js_sys::Function = reserve_clipboard_text(&settled).dyn_into().ok()?;
        let id = RESERVED.with_borrow_mut(|(next, reserved)| {
            *next = next.wrapping_add(1);
            reserved.insert(*next, fill);
            *next
        });
        Some(ReservedCopy::new(move |text| {
            if let Some(fill) = RESERVED.with_borrow_mut(|(_, reserved)| reserved.remove(&id)) {
                let text = text.map_or(JsValue::NULL, JsValue::from);
                let _ = fill.call1(&JsValue::NULL, &text);
            }
            copy
        }))
    }

    fn set_rich_text(&self, html: &str, plain: &str) -> bool {
        if let Some(written) = events::write_rich(html, plain) {
            return written;
        }
        let Some(report) = events::input_report() else {
            return false;
        };
        let forward =
            Closure::<dyn Fn(bool, Option<String>)>::new(move |ok, error: Option<String>| {
                report(if ok {
                    CopyReport::Copied
                } else {
                    CopyReport::Failed(error.unwrap_or_default())
                });
            });
        write_clipboard_rich(html, plain, forward.as_ref().unchecked_ref())
    }

    /// A page can only write `text/plain`: the browser offers no way to
    /// mark a copy as a secret, so it goes out unmarked.
    fn set_secret_text(&self, text: &str) -> bool {
        self.set_text(ClipboardTarget::Clipboard, text)
    }

    fn rich_text(&self) -> Option<crate::RichTextOffer> {
        events::read_rich()
    }

    fn text(&self, target: ClipboardTarget) -> Option<String> {
        if target != ClipboardTarget::Clipboard {
            return None;
        }
        events::read()
    }

    fn image(&self) -> Option<crate::ClipboardImage> {
        events::read_image()
    }
}
