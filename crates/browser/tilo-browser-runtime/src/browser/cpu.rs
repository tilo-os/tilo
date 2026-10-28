//! The page's CPU raster: the desktop drawn where the browser offers no
//! WebGPU.
//!
//! [`ui::cpu_canvas::CpuCanvas`] replays the same `vello::Scene` the WebGPU
//! renderer consumes through `tilo-cpu-raster` into a retained canvas of
//! premultiplied RGBA8, and repaints only what the frame changed, so a
//! steady-state frame costs its damage and not the window. The page presents
//! it through the presenter `web/cpu-present.mjs` makes for the mount's
//! canvas: a WebGL2 texture where the browser has one, else `putImageData`.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "pixel counts of a canvas bounded by the viewport: a row offset is width * 4, and every multiply is checked by the scan that follows it"
)]

use js_sys::{Function, Reflect};
use wasm_bindgen::{JsCast, JsValue};

use ui::cpu_canvas::CpuCanvas;
use ui::retained_raster::FrameContent;

/// One mount's canvas, drawn on the CPU.
pub(super) struct Cpu {
    canvas: CpuCanvas,
    /// `{resize(width, height), present(x, y, width, height, pixels)}` over
    /// the mount's canvas element, made by `web/cpu-present.mjs`.
    presenter: JsValue,
    /// The canvas extent the page's presenter was told.
    size: (u32, u32),
    /// One region's rows, kept from frame to frame: the page is handed a
    /// contiguous copy of each region, not the retained canvas' rows.
    region: Vec<u8>,
}

impl Cpu {
    /// The canvas this mount draws on, or `None` when the code it draws with
    /// is not in yet (`load_cpu_raster`).
    pub(super) fn new(presenter: JsValue) -> Option<Self> {
        Some(Self {
            canvas: CpuCanvas::new()?,
            presenter,
            size: (0, 0),
            region: Vec::new(),
        })
    }

    /// The canvas no longer holds the last presented frame: repaint all of
    /// the next one, and set its extent again.
    pub(super) fn invalidate(&mut self) {
        self.canvas.invalidate();
        self.size = (0, 0);
    }

    /// Rasterize `frame` into the retained canvas, repainting only its
    /// damage (or all of it), and put what it drew on the page's canvas.
    /// Returns the pixels rasterized, as the WebGPU path's repaint does.
    ///
    /// # Errors
    ///
    /// When the scene cannot be rasterized at this extent, or the page
    /// cannot present it.
    pub(super) fn present(&mut self, frame: FrameContent<'_>) -> Result<u64, String> {
        let size = (frame.width, frame.height);
        let rasterized = self.canvas.rasterize(&frame)?;
        let presenter = self.presenter.clone();
        if self.size != size {
            // A canvas the browser resized holds nothing; the regions below
            // cover it whole, the canvas having been invalidated with the
            // size change.
            method(&presenter, "resize")?
                .call2(&presenter, &JsValue::from(size.0), &JsValue::from(size.1))
                .map_err(|error| format!("the page could not resize its CPU canvas: {error:?}"))?;
            self.size = size;
        }
        let present = method(&presenter, "present")?;
        let stride = size.0 as usize * 4;
        let pixels = self.canvas.pixels();
        let mut drawn = false;
        for region in self.canvas.drawn() {
            let Some(rows) = region_rows(
                &mut self.region,
                pixels,
                (region.x, region.y),
                (region.width, region.height),
                stride,
            ) else {
                continue;
            };
            present
                .call5(
                    &presenter,
                    &JsValue::from(region.x),
                    &JsValue::from(region.y),
                    &JsValue::from(region.width),
                    &JsValue::from(region.height),
                    // The canvas takes premultiplied RGBA8, which is what the
                    // CPU rasterizer writes.
                    &js_sys::Uint8ClampedArray::from(rows),
                )
                .map_err(|error| format!("the page could not present a CPU frame: {error:?}"))?;
            drawn = true;
        }
        if drawn {
            // The frame's regions are all in: the page draws them (one
            // textured quad, or nothing for `putImageData`).
            method(&presenter, "flush")?
                .call0(&presenter)
                .map_err(|error| format!("the page could not flush a CPU frame: {error:?}"))?;
        }
        Ok(rasterized)
    }
}

/// What one method of the page's presenter is.
fn method(presenter: &JsValue, name: &str) -> Result<Function, String> {
    Reflect::get(presenter, &JsValue::from_str(name))
        .ok()
        .and_then(|method| method.dyn_into::<Function>().ok())
        .ok_or_else(|| format!("the page's CPU presenter has no {name}"))
}

/// `width` x `height` pixels at `(x, y)` of `pixels`, whose rows are
/// `stride` bytes, as contiguous rows in `scratch`.
fn region_rows<'a>(
    scratch: &'a mut Vec<u8>,
    pixels: &[u8],
    (x, y): (u32, u32),
    (width, height): (u32, u32),
    stride: usize,
) -> Option<&'a [u8]> {
    let row = width as usize * 4;
    scratch.clear();
    scratch
        .try_reserve(row.checked_mul(height as usize)?)
        .ok()?;
    for line in y..y.checked_add(height)? {
        let start = (line as usize)
            .checked_mul(stride)?
            .checked_add(x as usize * 4)?;
        scratch.extend_from_slice(pixels.get(start..start.checked_add(row)?)?);
    }
    Some(scratch)
}
