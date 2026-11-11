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
        let drawn_fields = ui::scene_parts::drawn_fields();
        let fields = self.fields_json(&drawn_fields);
        // A widget that scrolls natively or holds a field the page shows
        // stays elements; pictures and widgets of many draws become images.
        let keep: std::collections::HashSet<u64> = self
            .scrolls
            .iter()
            .map(|scroll| scroll.key)
            .chain(drawn_fields.iter().map(|field| field.key))
            .collect();
        let pictures = ui::scene_parts::drawn_pictures();
        let encoded = self.encoder.encode_with(
            frame.scene.encoding(),
            &tilo_cpu_raster::dom::Islands {
                pictures: Some(&pictures),
                keep: Some(&keep),
                viewport: ui::kurbo::Rect::new(
                    0.0,
                    0.0,
                    f64::from(frame.viewport.0),
                    f64::from(frame.viewport.1),
                ),
            },
        );
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
            "fields": fields,
            "theme": theme,
        });
        let args = js_sys::Array::new();
        args.push(&JsValue::from_str(&meta.to_string()));
        args.push(&JsValue::from_str(&encoded.ops));
        args.push(&new_fonts(&encoded.fonts));
        args.push(&new_images(&encoded.images));
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

/// The images a frame names for the first time, as the page keeps them:
/// `[id, width, height, straight RGBA bytes, the id it replaces or null]`
/// each.
fn new_images(images: &[tilo_cpu_raster::dom::NewImage]) -> js_sys::Array {
    let list = js_sys::Array::new();
    for image in images {
        let entry = js_sys::Array::new();
        entry.push(&JsValue::from(image.id));
        entry.push(&JsValue::from(image.width));
        entry.push(&JsValue::from(image.height));
        entry.push(&js_sys::Uint8ClampedArray::from(image.rgba.as_slice()).into());
        entry.push(&image.replaces.map_or(JsValue::NULL, JsValue::from));
        list.push(&entry);
    }
    list
}

impl Presenter {
    /// The text fields the frame left to the page, as it shows them: each
    /// widget's key, where its text shows and where its line starts on its
    /// baseline, and the face, size and colour it is set in. A face the
    /// page was not given yet goes with this frame's fonts.
    fn fields_json(&mut self, fields: &[ui::scene_parts::DrawnField]) -> Vec<serde_json::Value> {
        fields
            .iter()
            .map(|field| {
                let (family, variations) = self.encoder.text_face(&field.font, &field.coords);
                let color = field.color.to_rgba8();
                serde_json::json!({
                    "key": format!("{:016x}", field.key),
                    "area": [field.area.x0, field.area.y0, field.area.width(), field.area.height()],
                    "baseline": [field.baseline.x, field.baseline.y],
                    "size": field.size,
                    "family": family,
                    "variations": variations,
                    "color": format!(
                        "rgba({},{},{},{})",
                        color.r,
                        color.g,
                        color.b,
                        f32::from(color.a) / 255.0
                    ),
                })
            })
            .collect()
    }
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
