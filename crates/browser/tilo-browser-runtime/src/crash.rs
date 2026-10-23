//! A panicking application costs its own window, not the page's desktop.
//!
//! The browser build aborts on a panic (`panic = "abort"` is the only
//! strategy wasm32 has on stable), so the module cannot continue and the page
//! must start over. The panic hook tells the page which window's application
//! was running ([`ui::widget::panic_scope`]); `web/app-crash.mjs` keeps that
//! window out of the next load, and [`quarantined`] shows its error in its
//! place while every other window runs.
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

use js_sys::{Function, Reflect};
use wasm_bindgen::{JsCast, prelude::*};

thread_local! {
    static QUARANTINE: RefCell<BTreeMap<u64, String>> = const { RefCell::new(BTreeMap::new()) };
}

/// Windows (tab ids, as decimal strings) whose application crashed an
/// earlier load, each with its panic message: `[{"tab", "message"}]`. They
/// show the message instead of running their application. Replaces the
/// previous set; call before mounting.
#[wasm_bindgen]
pub fn quarantine_windows(entries: &str) -> Result<(), JsValue> {
    #[derive(serde::Deserialize)]
    struct Entry {
        tab: String,
        message: String,
    }
    let entries: Vec<Entry> =
        serde_json::from_str(entries).map_err(|e| JsValue::from_str(&e.to_string()))?;
    QUARANTINE.with_borrow_mut(|quarantine| {
        quarantine.clear();
        quarantine.extend(
            entries
                .into_iter()
                .filter_map(|entry| Some((entry.tab.parse().ok()?, entry.message))),
        );
    });
    Ok(())
}

/// What `tab` shows in place of an application that crashed an earlier load:
/// what happened, and a Reopen button that runs `reopen` once the window is
/// out of quarantine.
pub(crate) fn quarantined(
    tab: u64,
    title: &str,
    mut reopen: impl FnMut() + 'static,
) -> Option<ui::widget::TileContent> {
    let message = QUARANTINE.with_borrow(|quarantine| quarantine.get(&tab).cloned())?;
    // The panic's own words are for whoever debugs it, not for the person
    // whose window it took: they read what happened and what to do.
    web_sys::console::warn_1(&format!("{title} was closed after it panicked: {message}").into());
    let notice = tilo_desktop_surface::crashed_app::CrashedApp::new(title, move || {
        forget(tab);
        reopen();
    });
    Some(ui::widget::TileContent::App(Box::new(notice)))
}

/// Take `tab` out of quarantine, here and in the page's record of crashed
/// windows, so the next build of the window runs its application again.
fn forget(tab: u64) {
    QUARANTINE.with_borrow_mut(|quarantine| quarantine.remove(&tab));
    if let Ok(listener) = Reflect::get(
        &js_sys::global(),
        &JsValue::from_str("__tiloForgetCrashedWindow"),
    ) && let Some(listener) = listener.dyn_ref::<Function>()
    {
        let _ = listener.call1(&JsValue::NULL, &JsValue::from_str(&tab.to_string()));
    }
}

static PANICKED: AtomicBool = AtomicBool::new(false);

/// Why a call fails once [`stopped`].
pub(crate) const STOPPED: &str = "the runtime stopped after an application panicked";

/// Whether a panic stopped the runtime: its state may still be borrowed by
/// the code that panicked, so nothing may enter it again.
pub(crate) fn stopped() -> bool {
    PANICKED.load(Ordering::Relaxed)
}

/// Tell the page about the first panic: the window whose application was
/// running (`null` outside any), and the message. Later panics follow from
/// the first (state it left borrowed) and would misname the window; false
/// for them.
pub(crate) fn report(panic: &std::panic::PanicHookInfo<'_>) -> bool {
    if PANICKED.swap(true, Ordering::Relaxed) {
        return false;
    }
    let tab = ui::widget::panic_scope::current();
    let message = panic.payload_as_str().unwrap_or("the application panicked");
    let report = serde_json::json!({
        "tab": tab.map(|tab| tab.to_string()),
        "message": message,
    });
    if let Ok(listener) = Reflect::get(&js_sys::global(), &JsValue::from_str("__tiloRuntimePanic"))
        && let Some(listener) = listener.dyn_ref::<Function>()
    {
        let _ = listener.call1(&JsValue::NULL, &JsValue::from_str(&report.to_string()));
    }
    true
}
