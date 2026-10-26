//! Framework code the first frame does not need, loaded the first time
//! something uses it: the search engine (tantivy and its codecs), the
//! spelling dictionaries (the embedded English list and Hunspell), which
//! the page fetches and builds in its idle time once its first frame is up, the hyphenation
//! patterns, which the first hyphenated paragraph asks for, the image
//! decoders, which the first web page image or decoded picture asks for,
//! the model service's clients, which the first model request or provider
//! settings change asks for, and the pinned people overlay, which pinning
//! who is here asks for.
//!
//! Like an app's `definition()` (`crate::split`), each export below is the
//! only way to the code it hands over, so the splitter moves that code into
//! its own module. The crate that uses the code is built without it (its
//! deferring feature) and asks the loader registered here for it on first
//! use; the module's export then installs it, and every mount lays out and
//! draws again so that what the code changes shows.

use std::ffi::c_void;

use wasm_bindgen::prelude::{Closure, JsValue, wasm_bindgen};

use crate::split::Callback;

#[wasm_bindgen(inline_js = r#"
export function afterIdle(delay, work) {
    const idle = globalThis.requestIdleCallback
        ?? (run => setTimeout(() => run({ timeRemaining: () => 4 }), 0));
    setTimeout(() => idle(deadline => work(performance.now() + Math.max(1, deadline.timeRemaining())),
        { timeout: 1000 }), delay);
}
export function now() { return performance.now(); }
"#)]
extern "C" {
    /// Run `work` in the page's first idle period `delay` ms from now (or
    /// within a second more, if the page is never idle), with the time it
    /// should hand the page back by.
    #[wasm_bindgen(js_name = afterIdle)]
    fn after_idle(delay: u32, work: &JsValue);
    #[wasm_bindgen(js_name = now)]
    fn now() -> f64;
}

#[link(wasm_import_module = "./__wasm_split.js")]
#[allow(
    improper_ctypes,
    reason = "the function pointer only passes between Rust"
)]
unsafe extern "C" {
    #[link_name = "__wasm_split_load_search_8c1f5e0d2b7a4936_engine"]
    fn load_search(callback: Callback, data: *const c_void);
    #[link_name = "__wasm_split_00___search___00_import_8c1f5e0d2b7a4936_engine"]
    fn search_engine() -> tilo_search::MakeEngine;
    #[link_name = "__wasm_split_load_spelling_3e9a71c4d05b28f6_dictionaries"]
    fn load_spelling(callback: Callback, data: *const c_void);
    #[link_name = "__wasm_split_00___spelling___00_import_3e9a71c4d05b28f6_dictionaries"]
    fn spelling_dictionaries() -> spellcheck::MakeDictionaries;
    #[link_name = "__wasm_split_load_hyphenation_b7d24f9a610ce583_patterns"]
    fn load_hyphenation(callback: Callback, data: *const c_void);
    #[link_name = "__wasm_split_00___hyphenation___00_import_b7d24f9a610ce583_patterns"]
    fn hyphenation_patterns() -> tilo_ui::MakeHyphenation;
    #[link_name = "__wasm_split_load_page_images_90894fa21934d3a4_decoder"]
    fn load_page_images(callback: Callback, data: *const c_void);
    #[link_name = "__wasm_split_00___page_images___00_import_90894fa21934d3a4_decoder"]
    fn image_decoders() -> tilo_ui::image_decoders::MakeImageDecoders;
    #[link_name = "__wasm_split_load_model_clients_5d2c8e71a94f03b6_clients"]
    fn load_model_clients(callback: Callback, data: *const c_void);
    #[link_name = "__wasm_split_00___model_clients___00_import_5d2c8e71a94f03b6_clients"]
    fn model_clients() -> tilo_browser_runtime::MakeModelClients;
    #[link_name = "__wasm_split_load_people_overlay_c0c7a1e1c6874f57_roster"]
    fn load_people_overlay(callback: Callback, data: *const c_void);
    #[link_name = "__wasm_split_00___people_overlay___00_import_c0c7a1e1c6874f57_roster"]
    fn people_overlay() -> tilo_browser_runtime::MakePinnedRoster;
}

#[unsafe(export_name = "__wasm_split_00___search___00_export_8c1f5e0d2b7a4936_engine")]
#[allow(
    improper_ctypes_definitions,
    reason = "the function pointer only passes between Rust"
)]
extern "C" fn export_search_engine() -> tilo_search::MakeEngine {
    tilo_search::tantivy_engine
}

#[unsafe(export_name = "__wasm_split_00___spelling___00_export_3e9a71c4d05b28f6_dictionaries")]
#[allow(
    improper_ctypes_definitions,
    reason = "the function pointer only passes between Rust"
)]
extern "C" fn export_spelling_dictionaries() -> spellcheck::MakeDictionaries {
    spellcheck::builtin_dictionaries
}

#[unsafe(export_name = "__wasm_split_00___hyphenation___00_export_b7d24f9a610ce583_patterns")]
#[allow(
    improper_ctypes_definitions,
    reason = "the function pointer only passes between Rust"
)]
extern "C" fn export_hyphenation_patterns() -> tilo_ui::MakeHyphenation {
    tilo_ui::builtin_hyphenation
}

#[unsafe(export_name = "__wasm_split_00___page_images___00_export_90894fa21934d3a4_decoder")]
#[allow(
    improper_ctypes_definitions,
    reason = "the function pointer only passes between Rust"
)]
extern "C" fn export_image_decoders() -> tilo_ui::image_decoders::MakeImageDecoders {
    tilo_ui::image_decoders::builtin_image_decoders
}

#[unsafe(export_name = "__wasm_split_00___model_clients___00_export_5d2c8e71a94f03b6_clients")]
#[allow(
    improper_ctypes_definitions,
    reason = "the function pointer only passes between Rust"
)]
extern "C" fn export_model_clients() -> tilo_browser_runtime::MakeModelClients {
    tilo_browser_runtime::builtin_model_clients
}

#[unsafe(export_name = "__wasm_split_00___people_overlay___00_export_c0c7a1e1c6874f57_roster")]
#[allow(
    improper_ctypes_definitions,
    reason = "the function pointer only passes between Rust"
)]
extern "C" fn export_people_overlay() -> tilo_browser_runtime::MakePinnedRoster {
    tilo_browser_runtime::builtin_pinned_roster
}

/// Fetch the search engine's module, then install the engine.
fn fetch_search() {
    wasm_bindgen_futures::spawn_local(async {
        if crate::split::load(load_search).await {
            // SAFETY: the module that defines `search_engine` is loaded.
            tilo_search::install_engine(unsafe { search_engine() });
            tilo_browser_runtime::refresh_mounts();
        } else {
            tilo_search::engine_load_failed();
        }
    });
}

/// Fetch the spelling dictionaries' module, then install them.
fn fetch_spelling() {
    wasm_bindgen_futures::spawn_local(async {
        if crate::split::load(load_spelling).await {
            // SAFETY: the module that defines `spelling_dictionaries` is
            // loaded.
            spellcheck::install_dictionaries(unsafe { spelling_dictionaries() });
            tilo_browser_runtime::refresh_mounts();
        } else {
            spellcheck::dictionaries_load_failed();
        }
    });
}

/// Fetch the hyphenation patterns' module, then install them.
fn fetch_hyphenation() {
    wasm_bindgen_futures::spawn_local(async {
        if crate::split::load(load_hyphenation).await {
            // SAFETY: the module that defines `hyphenation_patterns` is
            // loaded.
            tilo_ui::install_hyphenation(unsafe { hyphenation_patterns() });
            tilo_browser_runtime::refresh_mounts();
        } else {
            tilo_ui::hyphenation_load_failed();
        }
    });
}

/// Fetch the image decoders' module, then install them.
fn fetch_page_images() {
    wasm_bindgen_futures::spawn_local(async {
        if crate::split::load(load_page_images).await {
            // SAFETY: the module that defines `image_decoders` is loaded.
            tilo_ui::image_decoders::install_image_decoders(unsafe { image_decoders() });
            tilo_browser_runtime::refresh_mounts();
        } else {
            tilo_ui::image_decoders::image_decoders_load_failed();
        }
    });
}

/// Fetch the model service's clients' module, then install them.
fn fetch_model_clients() {
    wasm_bindgen_futures::spawn_local(async {
        if crate::split::load(load_model_clients).await {
            // SAFETY: the module that defines `model_clients` is loaded.
            tilo_browser_runtime::install_model_clients(unsafe { model_clients() });
        } else {
            tilo_browser_runtime::model_clients_load_failed();
        }
    });
}

/// Fetch the pinned people overlay's module, then show it.
fn fetch_people_overlay() {
    wasm_bindgen_futures::spawn_local(async {
        if crate::split::load(load_people_overlay).await {
            // SAFETY: the module that defines `people_overlay` is loaded.
            tilo_browser_runtime::install_people_overlay(unsafe { people_overlay() });
            tilo_browser_runtime::refresh_mounts();
        } else {
            tilo_browser_runtime::people_overlay_load_failed();
        }
    });
}

/// How often the page looks again whether its first frame is up, or the
/// dictionaries' code is in.
const SPELLING_POLL_MS: u32 = 250;

/// How long after the first frame the page begins on the spelling
/// dictionaries: the app in front, if its code is still coming, gets the
/// link first.
const SPELLING_AFTER_FIRST_FRAME_MS: u32 = 1_000;

/// Whether the first frame is up and its grace period has been granted.
static SPELLING_RELEASED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Once the page has drawn its first frame, fetch the spelling dictionaries'
/// code and build the English dictionary in the page's idle time, a slice per
/// idle period, so that neither the first frame nor any keystroke waits for
/// either. A field typed in before then flags nothing yet, and checks again
/// once the dictionary is built.
fn prepare_spelling_when_idle(delay: u32) {
    let work = Closure::once_into_js(move |deadline: f64| {
        use std::sync::atomic::Ordering;
        if !SPELLING_RELEASED.load(Ordering::Relaxed) {
            if tilo_browser_runtime::first_frame_presented() {
                SPELLING_RELEASED.store(true, Ordering::Relaxed);
                prepare_spelling_when_idle(SPELLING_AFTER_FIRST_FRAME_MS);
            } else {
                prepare_spelling_when_idle(SPELLING_POLL_MS);
            }
            return;
        }
        match spellcheck::prepare_dictionaries(&mut || now() < deadline) {
            Some(true) => tilo_browser_runtime::refresh_mounts(),
            Some(false) => prepare_spelling_when_idle(0),
            // The code is still on its way.
            None => prepare_spelling_when_idle(SPELLING_POLL_MS),
        }
    });
    after_idle(delay, &work);
}

pub(crate) fn register() {
    tilo_search::defer_engine(fetch_search);
    spellcheck::defer_dictionaries(fetch_spelling);
    spellcheck::build_dictionaries_in_steps();
    prepare_spelling_when_idle(SPELLING_POLL_MS);
    tilo_ui::defer_hyphenation(fetch_hyphenation);
    tilo_ui::image_decoders::defer_image_decoders(fetch_page_images);
    tilo_browser_runtime::defer_model_clients(fetch_model_clients);
    tilo_browser_runtime::defer_people_overlay(fetch_people_overlay);
}
