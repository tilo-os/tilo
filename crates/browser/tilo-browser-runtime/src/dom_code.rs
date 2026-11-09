//! The code that shows a mount as HTML, for a page that asks for it
//! (`mountDesktop(el, {dom: true})`) instead of the WebGPU canvas: the
//! toolkit's widget keys (`ui::scene_parts::Keyer`), the scene-to-DOM
//! encoder (`tilo_cpu_raster::dom`) and what hands each frame to the page.
//!
//! Most pages never ask for it, so it is reached only through [`DomCode`],
//! declared with `ui::lazy_code!`: the browser desktop keeps it out of its
//! start module (`deferred-dom-presenter`) and fetches it the first time a
//! page asks; a build without that feature links it. The mount's own path
//! (`browser/dom.rs`) only dispatches to the frame drawing it hands over.

use js_sys::Function;
use wasm_bindgen::JsValue;

/// One frame for the page's presenter.
pub(crate) struct Frame<'a> {
    pub present: &'a Function,
    pub id: u64,
    pub scene: &'a vello::Scene,
    pub background: ui::peniko::Color,
    pub viewport: (u32, u32),
    pub layout_ms: f64,
    /// The theme, when the page does not hold its variables yet.
    pub theme: Option<&'a ui::Theme>,
}

/// Turns frames into the page's DOM operations, keeping fonts and glyph
/// outlines it has seen, and the scroll regions of the last frame.
pub(crate) trait DomPresent {
    /// Hand `frame` to the page.
    fn present(&mut self, frame: Frame<'_>);
    /// Scroll the region keyed `key` of the last frame to `offset` CSS
    /// pixels in `root`'s tree; whether it is still there.
    fn scrolled(&self, root: &mut dyn ui::widget::Widget, key: u64, offset: f64) -> bool;
}

#[derive(Default)]
struct Presenter {
    encoder: tilo_cpu_raster::dom::DomEncoder,
    scrolls: Vec<ui::scene_parts::DrawnScroll>,
}

impl DomPresent for Presenter {
    fn present(&mut self, frame: Frame<'_>) {
        let started = web_time::Instant::now();
        ui::scene_parts::retain_shown(frame.scene);
        self.scrolls = ui::scene_parts::drawn_scrolls();
        let encoded = self.encoder.encode(frame.scene.encoding());
        let theme = frame.theme.map(|theme| {
            crate::page_theme::css_variables(theme)
                .into_iter()
                .map(|(name, value)| (name.to_owned(), serde_json::Value::from(value)))
                .collect::<serde_json::Map<_, _>>()
        });
        let background = frame.background.to_rgba8();
        let meta = serde_json::json!({
            "id": frame.id,
            "background": format!(
                "rgba({},{},{},{})",
                background.r,
                background.g,
                background.b,
                f32::from(background.a) / 255.0
            ),
            "viewport": [frame.viewport.0, frame.viewport.1],
            "layoutMs": frame.layout_ms,
            "encodeMs": started.elapsed().as_secs_f64() * 1000.0,
            "texts": encoded.texts,
            "draws": encoded.draws,
            "bytes": encoded.ops.len(),
            "scrolls": scrolls_json(&self.scrolls),
            "theme": theme,
        });
        let args = js_sys::Array::new();
        args.push(&JsValue::from_str(&meta.to_string()));
        args.push(&JsValue::from_str(&encoded.ops));
        args.push(&new_fonts(&encoded.fonts));
        if let Err(error) = frame.present.apply(&JsValue::NULL, &args) {
            web_sys::console::error_1(&error);
        }
    }

    fn scrolled(&self, root: &mut dyn ui::widget::Widget, key: u64, offset: f64) -> bool {
        self.scrolls
            .iter()
            .find(|scroll| scroll.key == key)
            .is_some_and(|scroll| {
                let offset = offset / scroll.scale.max(f64::MIN_POSITIVE);
                ui::scene_parts::scroll_widget(root, scroll, offset)
            })
    }
}

/// The fonts a frame names for the first time, as the page registers them:
/// `[family, bytes, index, ascent, descent]` each.
fn new_fonts(fonts: &[tilo_cpu_raster::dom::NewFont]) -> js_sys::Array {
    let list = js_sys::Array::new();
    for font in fonts {
        let entry = js_sys::Array::new();
        entry.push(&JsValue::from_str(&font.family));
        entry.push(&js_sys::Uint8Array::from(font.bytes.data()).into());
        entry.push(&JsValue::from(font.index));
        entry.push(&JsValue::from(font.ascent));
        entry.push(&JsValue::from(font.descent));
        list.push(&entry);
    }
    list
}

/// The scroll regions a frame drew, as the page reads them: in CSS pixels.
fn scrolls_json(scrolls: &[ui::scene_parts::DrawnScroll]) -> Vec<serde_json::Value> {
    scrolls
        .iter()
        .map(|scroll| {
            serde_json::json!({
                "key": format!("{:016x}", scroll.key),
                "offset": scroll.offset * scroll.scale,
                "content": scroll.content * scroll.scale,
                "viewport": scroll.viewport * scroll.scale,
                "rect": [scroll.rect.x0, scroll.rect.y0, scroll.rect.width(), scroll.rect.height()],
            })
        })
        .collect()
}

/// A new presenter for a mount.
pub(crate) fn presenter() -> Box<dyn DomPresent> {
    Box::new(Presenter::default())
}

/// The HTML presentation's code: what the module a page fetches hands over.
/// Drawing a frame reaches everything else: the widget keys, the encoder
/// and the presenter.
#[derive(Clone, Copy)]
pub struct DomCode {
    pub(crate) draw: fn(&mut crate::browser::Mount) -> Result<(), String>,
}

/// The HTML presentation's code this build links.
#[must_use]
pub fn builtin_dom() -> DomCode {
    DomCode {
        draw: crate::browser::draw_dom_frame,
    }
}

ui::lazy_code! {
    /// The HTML presentation's code.
    pub static DOM_CODE: DomCode = builtin_dom, feature = "deferred-dom-presenter";
}
