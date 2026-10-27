//! The browser desktop bundle: `tilo-browser-runtime` plus every Tilo app
//! whose manifest declares the web platform, registered with
//! `ui::application::registry` before the page mounts anything. Every app
//! registers deferred and loads from its own module when first opened
//! (`split`), so the start module holds no app's code.
//!
//! The runtime's exports (`mountDesktop`, `connectPeer`, …) are this
//! module's exports, so the page host loads it exactly as it loads the
//! runtime alone.
#![cfg(target_arch = "wasm32")]

// Linked for its `#[wasm_bindgen]` exports, not named.
extern crate tilo_browser_runtime;

mod activity;
mod apps;
mod feedback;
mod lazy;
mod split;

#[wasm_bindgen::prelude::wasm_bindgen(start)]
fn start() {
    tilo_browser_runtime::start();
    activity::register();
    lazy::register();
    apps::register();
    // After the apps: the composer stays last in launcher order.
    feedback::register();
}
