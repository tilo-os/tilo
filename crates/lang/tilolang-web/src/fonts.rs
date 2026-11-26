//! The fonts a page's windows set their text in: the product catalog's startup files, which a page
//! fetches and hands over before it opens an app (a page has no fonts of its own to read), checked
//! against the digests this build pins.

use std::cell::RefCell;
use std::collections::BTreeMap;

use serde_json::json;
use wasm_bindgen::prelude::*;

thread_local! {
    static HANDED: RefCell<BTreeMap<String, (Vec<u8>, String)>> = const { RefCell::new(BTreeMap::new()) };
    static READY: RefCell<bool> = const { RefCell::new(false) };
}

/// The font files a page hands over before it opens an app: `[{file, bytes, sha256}]`, each by
/// its name in the product catalog, its size and its pinned SHA-256.
#[wasm_bindgen]
#[must_use]
pub fn startup_fonts() -> String {
    let Ok(manifest) = tilo_font_catalog::manifest() else {
        return "[]".to_owned();
    };
    let files: Vec<_> = manifest
        .font
        .iter()
        .filter(|asset| asset.scope == tilo_font_catalog::Scope::Product && asset.browser_startup)
        .map(|asset| json!({ "file": asset.file, "bytes": asset.bytes, "sha256": asset.sha256 }))
        .collect();
    json!(files).to_string()
}

/// Hand over the font file `file` of [`startup_fonts`]: its bytes, and the lowercase hex
/// SHA-256 the page computed of them (`crypto.subtle.digest`).
#[wasm_bindgen]
pub fn add_font(file: &str, bytes: Vec<u8>, sha256: &str) {
    HANDED.with(|handed| {
        handed
            .borrow_mut()
            .insert(file.to_owned(), (bytes, sha256.to_owned()));
    });
}

/// Set every window's text in the fonts handed over: `{ok}`, or `{ok: false, message}` naming
/// a file that is missing or not the pinned one.
#[wasm_bindgen]
#[must_use]
pub fn fonts_ready() -> String {
    let handed = HANDED.with(|handed| std::mem::take(&mut *handed.borrow_mut()));
    let catalog =
        tilo_ui_base::font_catalog::FontCatalog::product("", tilo_ui_base::i18n::I18nManager::current_locale());
    let mut handed = handed;
    match catalog.load_browser_startup_bytes(|file| handed.remove(file)) {
        Ok(system) => {
            drop(tilo_ui_base::text_render::TextRenderer::with_browser_font_system(
                system,
            ));
            READY.with(|ready| *ready.borrow_mut() = true);
            json!({ "ok": true }).to_string()
        }
        Err(error) => json!({ "ok": false, "message": error.to_string() }).to_string(),
    }
}

/// Whether a window can set text: in a page, once the fonts were handed over; natively, in the
/// fonts the machine's catalog holds.
pub(crate) fn ready() -> Result<(), String> {
    if cfg!(not(target_arch = "wasm32")) || READY.with(|ready| *ready.borrow()) {
        return Ok(());
    }
    Err(
        "no fonts yet: hand over each of startup_fonts() with add_font, then call fonts_ready"
            .to_owned(),
    )
}
