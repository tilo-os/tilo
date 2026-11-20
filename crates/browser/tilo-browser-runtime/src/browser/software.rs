//! A mount's frames drawn on the CPU and put on the page: what it does while
//! its GPU cannot draw a frame (`crate::gpu_recovery`).
//!
//! The frame is laid out and rasterized by the same retained raster every
//! other presenter draws with, so the pixels are the shell's own; only their way to
//! the screen differs. A canvas that had a `webgpu` context never gives a `2d`
//! one, so the pixels go on a canvas of their own laid over the mount's: the
//! mount's stays where winit put it, its box the window's size and every key,
//! pointer and IME event landing on it, and the overlay only shows.

use js_sys::Uint8ClampedArray;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement, HtmlElement, ImageData};

use super::Retained;

/// The overlay's place: the mount's canvas's box exactly, letting every
/// pointer through to the canvas beneath it.
const OVERLAY_STYLE: &str =
    "position:absolute;inset:0;width:100%;height:100%;pointer-events:none;display:block;";

/// One mount's frames drawn on the CPU, and where they go.
pub(super) struct Software {
    /// The canvas the pixels are put on.
    canvas: HtmlCanvasElement,
    context: CanvasRenderingContext2d,
    /// The mount's own canvas, which winit and the page's input read: covered
    /// while this draws, and shown again once a GPU draws there.
    covered: HtmlCanvasElement,
    /// The overlay's pixels, at the extent it was last given.
    pixels: Option<Pixels>,
    /// The last frames, repainted only where they changed.
    pub(super) retained: Retained,
}

struct Pixels {
    width: u32,
    height: u32,
    /// The frame as `ImageData` holds it: straight alpha, rows of `width * 4`.
    straight: Vec<u8>,
    image: ImageData,
    /// The bytes of `image`, written through for the frame's changed pixels.
    data: Uint8ClampedArray,
}

impl Software {
    /// Lay a canvas for CPU-drawn frames over `canvas`, the mount's own.
    pub(super) fn new(canvas: &HtmlCanvasElement) -> Result<Self, String> {
        let document = web_sys::window()
            .and_then(|window| window.document())
            .ok_or("the page has no document")?;
        let parent = canvas.parent_node().ok_or("the canvas is in no element")?;
        // The overlay is placed within the canvas's own box: an element that
        // is not positioned yet is where it lies, not where a page meant it.
        if let Some(parent) = parent.dyn_ref::<HtmlElement>()
            && let Ok(position) = parent.style().get_property_value("position")
            && (position.is_empty() || position == "static")
        {
            let _ = parent.style().set_property("position", "relative");
        }
        let overlay = document
            .create_element("canvas")
            .map_err(describe)?
            .dyn_into::<HtmlCanvasElement>()
            .map_err(describe)?;
        overlay
            .set_attribute("style", OVERLAY_STYLE)
            .map_err(describe)?;
        overlay
            .set_attribute("aria-hidden", "true")
            .map_err(describe)?;
        parent
            .insert_before(&overlay, canvas.next_sibling().as_ref())
            .map_err(describe)?;
        let context = overlay
            .get_context("2d")
            .map_err(describe)?
            .and_then(|context| context.dyn_into::<CanvasRenderingContext2d>().ok())
            .ok_or("the page gives the desktop no 2D canvas")?;
        // The device's last frame would stay under the overlay's until a GPU
        // presents again: hidden, not removed, so winit keeps its box and
        // every input still lands on it.
        if let Some(covered) = canvas.dyn_ref::<HtmlElement>() {
            let _ = covered.style().set_property("opacity", "0");
        }
        Ok(Self {
            canvas: overlay,
            context,
            covered: canvas.clone(),
            pixels: None,
            retained: Retained::new(true),
        })
    }

    /// Put on the page the frame `retained` just rasterized.
    pub(super) fn present(&mut self) -> Result<(), String> {
        let Some(frame) = self.retained.raster.cpu_frame() else {
            return Ok(());
        };
        let (width, height) = (frame.width, frame.height);
        if width == 0 || height == 0 || frame.dirty.is_empty() {
            return Ok(());
        }
        let stride = usize::try_from(width)
            .map_err(|_| "the frame is too wide")?
            .saturating_mul(4);
        let length = stride
            .checked_mul(usize::try_from(height).map_err(|_| "the frame is too tall")?)
            .ok_or("the frame is too large")?;
        if self
            .pixels
            .as_ref()
            .is_none_or(|pixels| (pixels.width, pixels.height) != (width, height))
        {
            if self.canvas.width() != width {
                self.canvas.set_width(width);
            }
            if self.canvas.height() != height {
                self.canvas.set_height(height);
            }
            let image = self
                .context
                .create_image_data_with_sw_and_sh(f64::from(width), f64::from(height))
                .map_err(describe)?;
            let data = js_sys::Reflect::get(&image, &JsValue::from_str("data"))
                .map_err(describe)?
                .dyn_into::<Uint8ClampedArray>()
                .map_err(describe)?;
            self.pixels = Some(Pixels {
                width,
                height,
                straight: vec![0; length],
                image,
                data,
            });
        }
        let pixels = self.pixels.as_mut().ok_or("the overlay holds no pixels")?;
        for region in frame.dirty {
            // Whole rows: the columns beside the region hold the frames
            // before this one, which are as current as it is.
            let from = usize::try_from(region.y)
                .unwrap_or(usize::MAX)
                .saturating_mul(stride);
            let rows = usize::try_from(region.height).unwrap_or(usize::MAX);
            let to = from.saturating_add(rows.saturating_mul(stride)).min(length);
            let (Some(source), Ok(start), Ok(end)) = (
                frame.pixels.get(from..to),
                u32::try_from(from),
                u32::try_from(to),
            ) else {
                continue;
            };
            let Some(straight) = pixels.straight.get_mut(from..to) else {
                continue;
            };
            straight.copy_from_slice(source);
            ui::retained_raster::straighten(straight);
            pixels.data.subarray(start, end).copy_from(straight);
            self.context
                .put_image_data_with_dirty_x_and_dirty_y_and_dirty_width_and_dirty_height(
                    &pixels.image,
                    0.0,
                    0.0,
                    f64::from(region.x),
                    f64::from(region.y),
                    f64::from(region.width),
                    f64::from(region.height),
                )
                .map_err(describe)?;
        }
        Ok(())
    }
}

impl Drop for Software {
    fn drop(&mut self) {
        self.canvas.remove();
        if let Some(covered) = self.covered.dyn_ref::<HtmlElement>() {
            let _ = covered.style().set_property("opacity", "1");
        }
    }
}

/// `error` as the page names it, for a log that reads.
fn describe(error: impl Into<JsValue>) -> String {
    let error = error.into();
    error
        .as_string()
        .or_else(|| {
            js_sys::Reflect::get(&error, &JsValue::from_str("message"))
                .ok()
                .and_then(|message| message.as_string())
        })
        .unwrap_or_else(|| format!("{error:?}"))
}
