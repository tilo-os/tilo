//! A QR code drawn as SVG for the page: the pairing sheet shows a link as a
//! code a phone's camera reads. The encoding and the markup are the
//! toolkit's (`ui::widget::qr`), so a page shows the code the native shell
//! paints.

use wasm_bindgen::prelude::*;

/// `text` as SVG markup: one path of dark modules inside the quiet zone.
///
/// # Errors
///
/// The text does not fit a code.
#[wasm_bindgen(js_name = qrSvg)]
pub fn qr_svg(text: &str) -> Result<String, JsValue> {
    ui::widget::qr::svg(text).map_err(|error| js_sys::Error::new(&error).into())
}
