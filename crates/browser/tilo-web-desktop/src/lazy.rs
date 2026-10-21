//! Framework code the first frame does not need, loaded the first time
//! something uses it: the search engine (tantivy and its codecs), and the
//! spelling dictionaries (the embedded English list and Hunspell), which
//! the first text field that checks spelling asks for, the hyphenation
//! patterns, which the first hyphenated paragraph asks for, the image
//! decoders, which the first web page image or decoded picture asks for, and
//! the model service's clients, which the first model request or provider
//! settings change asks for.
//!
//! Like an app's `definition()` (`crate::split`), each export below is the
//! only way to the code it hands over, so the splitter moves that code into
//! its own module. The crate that uses the code is built without it (its
//! deferring feature) and asks the loader registered here for it on first
//! use; the module's export then installs it, and every mount lays out and
//! draws again so that what the code changes shows.

use std::ffi::c_void;

use crate::split::Callback;

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

pub(crate) fn register() {
    tilo_search::defer_engine(fetch_search);
    spellcheck::defer_dictionaries(fetch_spelling);
    tilo_ui::defer_hyphenation(fetch_hyphenation);
    tilo_ui::image_decoders::defer_image_decoders(fetch_page_images);
    tilo_browser_runtime::defer_model_clients(fetch_model_clients);
}
