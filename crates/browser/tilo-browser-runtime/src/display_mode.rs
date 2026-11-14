//! Whether the page runs in a browser tab or installed as an app window
//! (`display-mode: standalone`), as the page reports it. A tab's browser keeps
//! Ctrl+W, Ctrl+T, Ctrl+N and Ctrl+Tab for its own tabs, so the desk answers
//! them only in an app window, and the shortcut guide offers the keys that
//! reach the page where it runs.

use std::cell::Cell;

use wasm_bindgen::prelude::*;

thread_local! {
    static APP_WINDOW: Cell<bool> = const { Cell::new(false) };
}

/// Whether the page is installed as an app window.
pub(crate) fn app_window() -> bool {
    APP_WINDOW.with(Cell::get)
}

/// The page is shown as an app window (`true`) or in a browser tab: told at
/// start and whenever the browser moves it between the two.
#[wasm_bindgen]
pub fn set_app_window(app_window: bool) {
    if APP_WINDOW.with(|cell| cell.replace(app_window)) != app_window {
        crate::browser::wake_desktops();
    }
}
