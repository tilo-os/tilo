//! Keyboard transfers and clicked Copy share the browser system clipboard.
//! The clipboard itself is `tilo_clipboard`'s web backend; this binds the
//! page's events to it.
use js_sys::{Function, Reflect};
use std::rc::Rc;
use ui::clipboard::events::{self, CopyReport, EventTransfer};
use ui::clipboard::{MAX_CLIPBOARD_HTML_BYTES, MAX_CLIPBOARD_PLAIN_FALLBACK_BYTES, RichTextOffer};
use wasm_bindgen::{JsCast, prelude::*};

/// Bind a write to the originating mount for this input only. No clipboard
/// payload survives the event, and late completion only reports to that mount.
pub fn during_input<R>(status: &Function, run: impl FnOnce() -> R) -> R {
    let status = status.clone();
    let report = move |report: CopyReport| {
        let json = match report {
            CopyReport::Copied => r#"{"clipboard":"copied"}"#.to_owned(),
            CopyReport::Failed(error) => serde_json::json!({
                "clipboard": "failed",
                "clipboardError": error,
            })
            .to_string(),
        };
        let _ = status.call1(&JsValue::NULL, &json.into());
    };
    events::during_input(Rc::new(report), run)
}

// getData returns a browser-owned string. Check its UTF-8 size in JavaScript
// before wasm-bindgen copies it into WASM memory; JS length counts UTF-16 units.
// The loop is bounded by maxBytes and handles lone surrogates like TextEncoder.
#[wasm_bindgen(inline_js = r#"
export function boundedClipboardText(transfer, mime, maxBytes) {
    let text;
    try { text = transfer.getData(mime); } catch { return null; }
    if (typeof text !== 'string' || text.length > maxBytes) return null;
    let bytes = 0;
    for (let i = 0; i < text.length; i++) {
        const unit = text.charCodeAt(i);
        if (unit < 0x80) bytes += 1;
        else if (unit < 0x800) bytes += 2;
        else if (unit >= 0xd800 && unit <= 0xdbff && i + 1 < text.length
            && text.charCodeAt(i + 1) >= 0xdc00 && text.charCodeAt(i + 1) <= 0xdfff) {
            bytes += 4;
            i++;
        } else bytes += 3;
        if (bytes > maxBytes) return null;
    }
    return text;
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = boundedClipboardText)]
    fn bounded_clipboard_text(transfer: &JsValue, mime: &str, max_bytes: usize) -> JsValue;
}

/// A trusted event's `DataTransfer`.
struct DataTransfer(JsValue);

impl DataTransfer {
    fn method(&self, name: &str) -> Option<Function> {
        Reflect::get(&self.0, &name.into()).ok()?.dyn_into().ok()
    }
}

impl EventTransfer for DataTransfer {
    fn set_text(&self, text: &str) -> bool {
        self.method("setData")
            .is_some_and(|f| f.call2(&self.0, &"text/plain".into(), &text.into()).is_ok())
    }

    fn rich_text(&self) -> Option<RichTextOffer> {
        let html = bounded_clipboard_text(&self.0, "text/html", MAX_CLIPBOARD_HTML_BYTES)
            .as_string()
            .filter(|html| !html.is_empty())?;
        let plain =
            bounded_clipboard_text(&self.0, "text/plain", MAX_CLIPBOARD_PLAIN_FALLBACK_BYTES)
                .as_string()?;
        Some(RichTextOffer {
            html,
            plain: Some(plain),
        })
    }

    fn text(&self) -> Option<String> {
        self.method("getData")?
            .call1(&self.0, &"text/plain".into())
            .ok()?
            .as_string()
    }
}

pub fn copied() -> bool {
    events::copied()
}

pub fn during<R>(transfer: JsValue, run: impl FnOnce() -> R) -> R {
    events::during(Rc::new(DataTransfer(transfer)), run)
}
