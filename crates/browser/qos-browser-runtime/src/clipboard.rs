//! Clipboard data exists only for the current trusted DOM event.
use js_sys::{Function, Reflect};
use std::{cell::RefCell, sync::Arc};
use ui::clipboard::{ClipboardBackend, ClipboardTarget};
use wasm_bindgen::{JsCast, prelude::*};
struct Transfer {
    value: JsValue,
    read: bool,
    copied: bool,
}
thread_local! {static TRANSFER:RefCell<Option<Transfer>>=const{RefCell::new(None)};}
struct BrowserClipboard;
impl ClipboardBackend for BrowserClipboard {
    fn name(&self) -> &'static str {
        "browser-clipboard-event"
    }
    fn set_text(&self, target: ClipboardTarget, text: &str) -> bool {
        if target != ClipboardTarget::Clipboard {
            return false;
        }
        TRANSFER.with_borrow_mut(|slot| {
            let Some(t) = slot.as_mut() else {
                return false;
            };
            if t.copied {
                return true;
            }
            t.copied = Reflect::get(&t.value, &"setData".into())
                .ok()
                .and_then(|f| f.dyn_into::<Function>().ok())
                .is_some_and(|f| {
                    f.call2(&t.value, &"text/plain".into(), &text.into())
                        .is_ok()
                });
            t.copied
        })
    }
    fn text(&self, target: ClipboardTarget) -> Option<String> {
        if target != ClipboardTarget::Clipboard {
            return None;
        }
        TRANSFER.with_borrow_mut(|slot| {
            let t = slot.as_mut()?;
            if t.read {
                return None;
            }
            t.read = true;
            Reflect::get(&t.value, &"getData".into())
                .ok()?
                .dyn_into::<Function>()
                .ok()?
                .call1(&t.value, &"text/plain".into())
                .ok()?
                .as_string()
                .filter(|s| !s.is_empty())
        })
    }
}
pub fn install() {
    ui::clipboard::install_clipboard_backend(Arc::new(BrowserClipboard));
}
pub fn copied() -> bool {
    TRANSFER.with_borrow(|t| t.as_ref().is_some_and(|t| t.copied))
}
pub fn during<R>(transfer: JsValue, run: impl FnOnce() -> R) -> R {
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            TRANSFER.with_borrow_mut(|t| *t = None);
        }
    }
    TRANSFER.with_borrow_mut(|t| {
        *t = Some(Transfer {
            value: transfer,
            read: false,
            copied: false,
        })
    });
    let _clear = Clear;
    run()
}
