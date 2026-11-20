//! The code that shows a mount as HTML, for a page that asks for it
//! (`mountDesktop(el, {dom: true})`) instead of the WebGPU canvas: each
//! frame's composition ([`ui::composition`]) goes to the page, which renders
//! it as elements (`web/view-present.mjs`), and the pictures of the widgets
//! that are their paint are rasterized here when the page asks for them.
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
    /// The frame's composition, as JSON: in full, or since the one the page shows
    /// ([`ui::composition::Composition::since`]).
    pub composition: String,
    pub background: ui::peniko::Color,
    pub layout_ms: f64,
    /// The time the composition above took to build and serialize, after the
    /// frame laid out.
    pub encode_ms: f64,
    /// The theme, when the page does not hold its variables yet.
    pub theme: Option<&'a ui::Theme>,
}

/// Hands frames to the page. A frame that composes what the last did never reaches it
/// ([`ui::headless::FrameHarness::compose_frame`]). Returns whether the page took it: a page
/// that did not shows none of it, and is handed the next one in full.
pub(crate) trait DomPresent {
    fn present(&mut self, frame: Frame<'_>) -> bool;
}

struct Presenter;

impl DomPresent for Presenter {
    fn present(&mut self, frame: Frame<'_>) -> bool {
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
            "layoutMs": frame.layout_ms,
            "encodeMs": frame.encode_ms,
            "bytes": frame.composition.len(),
            "theme": theme,
        });
        let args = js_sys::Array::new();
        args.push(&JsValue::from_str(&meta.to_string()));
        args.push(&JsValue::from_str(&frame.composition));
        if let Err(error) = frame.present.apply(&JsValue::NULL, &args) {
            web_sys::console::error_1(&error);
            return false;
        }
        true
    }
}

/// A new presenter for a mount.
pub(crate) fn presenter() -> Box<dyn DomPresent> {
    Box::new(Presenter)
}

/// The picture of a widget that is its paint: `scene`, rasterized into a
/// `width` x `height` image of straight RGBA, as a page's image data is.
fn picture(scene: &vello::Scene, width: u32, height: u32) -> Option<Vec<u8>> {
    let stride = (width as usize).checked_mul(4)?;
    let mut rgba = vec![0_u8; stride.checked_mul(height as usize)?];
    let mut target = tilo_cpu_raster::Target::new(&mut rgba, width, height, stride).ok()?;
    tilo_cpu_raster::CpuRasterizer::new(tilo_cpu_raster::Options::default()).render(
        scene.encoding(),
        &mut target,
        None,
        tilo_cpu_raster::color::AlphaColor::TRANSPARENT,
    );
    ui::retained_raster::straighten(&mut rgba);
    Some(rgba)
}

/// The pictures of the widgets at `paths` of `m`, at `scale` device pixels
/// per logical pixel: for each, `[width, height, pixels]`, or null when the
/// page's composition has no widget at the path or it paints nothing.
fn paint(m: &mut crate::browser::Mount, paths: &[String], scale: f64) -> JsValue {
    let out = js_sys::Array::new();
    for painted in crate::browser::paint_composed(m, paths, scale) {
        out.push(&painted.map_or(JsValue::NULL, |(scene, rect)| shown(&scene, rect, scale)));
    }
    out.into()
}

/// `scene`, painted over `rect`, as the page's `[width, height, pixels]` at
/// `scale`, or null when it paints nothing.
fn shown(scene: &vello::Scene, rect: [f64; 4], scale: f64) -> JsValue {
    if scene.encoding().is_empty() {
        return JsValue::NULL;
    }
    let size = |side: f64| (side * scale).ceil().clamp(1.0, 4096.0);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 1..=4096 just above"
    )]
    let (width, height) = (size(rect[2]) as u32, size(rect[3]) as u32);
    let Some(rgba) = picture(scene, width, height) else {
        return JsValue::NULL;
    };
    let out = js_sys::Array::new();
    out.push(&JsValue::from(width));
    out.push(&JsValue::from(height));
    out.push(&js_sys::Uint8ClampedArray::from(rgba.as_slice()).into());
    out.into()
}

/// The HTML presentation's code: what the module a page fetches hands over.
/// Drawing a frame reaches everything else: the composition, the presenter
/// and the pictures' rasterizer.
#[derive(Clone, Copy)]
pub struct DomCode {
    pub(crate) draw: fn(&mut crate::browser::Mount) -> Result<(), String>,
    pub(crate) paint: fn(&mut crate::browser::Mount, &[String], f64) -> JsValue,
}

/// The HTML presentation's code this build links.
#[must_use]
pub fn builtin_dom() -> DomCode {
    // The toolkit's own code that only a page shows (a control's face) comes
    // with the presenter, the only code that composes.
    ui::composition::HTML_CODE.install(ui::composition::builtin_html());
    DomCode {
        draw: crate::browser::draw_dom_frame,
        paint,
    }
}

ui::lazy_code! {
    /// The HTML presentation's code.
    pub static DOM_CODE: DomCode = builtin_dom, feature = "deferred-dom-presenter";
}
