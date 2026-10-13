//! The browser desktop bundle: `tilo-browser-runtime` plus every Tilo app
//! whose manifest declares the web platform, registered with
//! `ui::application::registry` before the page mounts anything. Most apps
//! register deferred and load from their own module when first opened
//! (`split`).
//!
//! The runtime's exports (`mountDesktop`, `connectPeer`, …) are this
//! module's exports, so the page host loads it exactly as it loads the
//! runtime alone.
#![cfg(target_arch = "wasm32")]

// Linked for its `#[wasm_bindgen]` exports, not named.
extern crate tilo_browser_runtime;

mod apps;
mod split;

#[wasm_bindgen::prelude::wasm_bindgen(start)]
fn start() {
    apps::register();
}
