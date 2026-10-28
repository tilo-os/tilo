//! The page's CPU raster: the desktop drawn where the browser offers no
//! WebGPU.
//!
//! [`ui::scene_parts::partition`] splits the frame the WebGPU renderer would
//! consume into layers, and [`ui::cpu_layers::CpuLayers`] replays each of
//! them through `tilo-cpu-raster` into a target of its own, repainting only
//! where that layer's own content changed. The page presents them through
//! the presenter `web/cpu-present.mjs` makes for the mount's canvas: one
//! WebGL2 texture per layer drawn where it goes, so a frame that only panned
//! the strip uploads nothing and moves the textures it has. A canvas without
//! WebGL2 (or `?cpuPresent=2d`) has no layers: the frame is drawn into one
//! canvas and written with `putImageData`.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "pixel counts of a canvas bounded by the viewport: a row offset is width * 4, and every multiply is checked by the scan that follows it"
)]

use js_sys::{Function, Reflect, Uint32Array};
use wasm_bindgen::{JsCast, JsValue};

use ui::cpu_canvas::CpuCanvas;
use ui::cpu_layers::CpuLayers;
use ui::retained_raster::FrameContent;

/// What one frame drawn as layers cost, and whether it moved any.
pub(super) struct Layered {
    /// The pixels rasterized, over every layer.
    pub(super) rasterized: u64,
    /// The frame moved layers without drawing a pixel.
    pub(super) moved: bool,
    /// What each layer did, for the frame metrics (`M` it moved, `D` it
    /// repainted its damage, `W` it was drawn whole, `C` it moved and
    /// changed, `X` it moved somewhere its pixels did not reach).
    pub(super) kinds: String,
}

/// One mount's canvas, drawn on the CPU.
pub(super) struct Cpu {
    canvas: CpuCanvas,
    /// The frame's layers, each with a target of its own, where the page's
    /// presenter composites them.
    layers: Option<CpuLayers>,
    /// Which of the page's texture slots each layer was given, by layer key,
    /// and how many the page has been told about.
    slots: Vec<(u64, u32)>,
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
        // `?cpuLayers=1` draws the frame as layers, each with a target of its
        // own, so a frame that only pans draws nothing; `?cpuLayers=0` (the
        // default for now) draws it into one target. The two can be measured
        // against each other, and the layers are not the default until every
        // layer a frame has can be proven bounded (see
        // `crates/sdk/ui-core/src/cpu_layers.rs`).
        let search = web_sys::window()
            .and_then(|window| window.location().search().ok())
            .unwrap_or_default();
        let layered = !search.contains("cpuLayers=0")
            && search.contains("cpuLayers=1")
            && Reflect::get(&presenter, &JsValue::from_str("layered"))
                .ok()
                .and_then(|layered| layered.as_bool())
                .unwrap_or(false);
        Some(Self {
            canvas: CpuCanvas::new()?,
            layers: layered.then(CpuLayers::new).flatten(),
            slots: Vec::new(),
            presenter,
            size: (0, 0),
            region: Vec::new(),
        })
    }

    /// Whether this mount draws the frame as layers, each with a target of
    /// its own (`?cpuLayers=1`).
    pub(super) fn layered(&self) -> bool {
        self.layers.is_some()
    }

    /// The canvas no longer holds the last presented frame: repaint all of
    /// the next one, and set its extent again.
    pub(super) fn invalidate(&mut self) {
        self.canvas.invalidate();
        if let Some(layers) = self.layers.as_mut() {
            layers.invalidate();
        }
        self.size = (0, 0);
    }

    /// Draw `scene` as the layers it is made of and put them on the page's
    /// canvas, or `None` when this mount draws one target instead (a canvas
    /// without WebGL2, or a scene whose parts do not prove it drawn as
    /// layers).
    ///
    /// `parts` are the frame's damage parts and `to_device` maps the scene's
    /// own coordinates onto the target's pixels.
    ///
    /// # Errors
    ///
    /// When a layer cannot be rasterized, or the page cannot present it.
    pub(super) fn present_layers(
        &mut self,
        scene: &vello::Scene,
        parts: Option<&ui::scene_parts::SectionParts>,
        to_device: vello::kurbo::Affine,
        size: (u32, u32),
        background: vello::peniko::Color,
    ) -> Option<Result<Layered, String>> {
        let parts = parts?;
        let mut bounds = ui::scene_parts::DrawBounds::default();
        let framed = ui::scene_parts::partition(scene, parts, to_device, &mut bounds)?;
        let Some(layers) = self.layers.as_mut() else {
            return None;
        };
        // The one target this mount also keeps is not what the page shows:
        // a frame that falls back to it is drawn whole.
        self.canvas.invalidate();
        Some((|| {
            let rasterized = layers.update(&framed, size, to_device)?;
            if rasterized == 0 && !layers.moved() {
                // The canvas holds this frame already: nothing drew and
                // nothing moved, so the page is not touched at all.
                return Ok(Layered {
                    rasterized,
                    moved: false,
                    kinds: kinds_of(layers),
                });
            }
            let presenter = self.presenter.clone();
            if self.size != size {
                method(&presenter, "resize")?
                    .call2(&presenter, &JsValue::from(size.0), &JsValue::from(size.1))
                    .map_err(|error| {
                        format!("the page could not resize its CPU canvas: {error:?}")
                    })?;
                self.size = size;
            }
            let [red, green, blue, alpha] = background.components;
            method(&presenter, "beginFrame")?
                .call4(
                    &presenter,
                    &JsValue::from(f64::from(red * alpha)),
                    &JsValue::from(f64::from(green * alpha)),
                    &JsValue::from(f64::from(blue * alpha)),
                    &JsValue::from(f64::from(alpha)),
                )
                .map_err(|error| format!("the page could not start a CPU frame: {error:?}"))?;
            let draws = layers.draws().to_vec();
            let slots = slots_for(&mut self.slots, &draws);
            let mut placed: Vec<f32> = Vec::with_capacity(draws.len() * 8);
            let mut drawn = Vec::with_capacity(draws.len());
            let mut alphas = Vec::with_capacity(draws.len());
            for (draw, slot) in draws.iter().zip(&slots) {
                let Some((pixels, extent, regions)) = layers.layer(draw.key) else {
                    continue;
                };
                method(&presenter, "layer")?
                    .call3(
                        &presenter,
                        &JsValue::from(*slot),
                        &JsValue::from(extent.0),
                        &JsValue::from(extent.1),
                    )
                    .map_err(|error| format!("the page could not make a layer: {error:?}"))?;
                let upload = method(&presenter, "upload")?;
                let stride = extent.0 as usize * 4;
                for region in regions {
                    let Some(rows) = region_rows(
                        &mut self.region,
                        pixels,
                        (region.x, region.y),
                        (region.width, region.height),
                        stride,
                    ) else {
                        continue;
                    };
                    // Premultiplied RGBA8, as the CPU rasterizer writes it
                    // and as the canvas composites it.
                    upload
                        .call6(
                            &presenter,
                            &JsValue::from(*slot),
                            &JsValue::from(region.x),
                            &JsValue::from(region.y),
                            &JsValue::from(region.width),
                            &JsValue::from(region.height),
                            &js_sys::Uint8ClampedArray::from(rows),
                        )
                        .map_err(|error| format!("the page could not upload a layer: {error:?}"))?;
                }
                // Where the layer goes, and which part of its target that
                // shows: a layer moved past the edge keeps the pixels beyond
                // it and shows the rest.
                let (width, height) = (draw.source.width, draw.source.height);
                let scale = |pixels: u32, of: u32| f64::from(pixels) / f64::from(of.max(1));
                let (u0, v0) = (
                    scale(draw.source.x, extent.0),
                    scale(draw.source.y, extent.1),
                );
                placed.extend_from_slice(&[
                    draw.at.0 as f32,
                    draw.at.1 as f32,
                    width as f32,
                    height as f32,
                    u0 as f32,
                    v0 as f32,
                    (u0 + scale(width, extent.0)) as f32,
                    (v0 + scale(height, extent.1)) as f32,
                ]);
                drawn.push(*slot);
                alphas.push(draw.alpha);
            }
            method(&presenter, "composite")?
                .call3(
                    &presenter,
                    &Uint32Array::from(&drawn[..]),
                    &js_sys::Float32Array::from(&placed[..]),
                    &js_sys::Float32Array::from(&alphas[..]),
                )
                .map_err(|error| format!("the page could not composite a CPU frame: {error:?}"))?;
            Ok(Layered {
                rasterized,
                moved: layers.moved(),
                kinds: kinds_of(layers),
            })
        })())
    }

    /// Rasterize `frame` into the retained canvas, repainting only its
    /// damage (or all of it), and put what it drew on the page's canvas.
    /// Returns the pixels rasterized, as the WebGPU path's repaint does.
    /// This is the frame of a page whose canvas has no WebGL2, drawn into
    /// one target rather than as layers.
    ///
    /// # Errors
    ///
    /// When the scene cannot be rasterized at this extent, or the page
    /// cannot present it.
    pub(super) fn present(&mut self, frame: FrameContent<'_>) -> Result<u64, String> {
        if let Some(layers) = self.layers.as_mut() {
            // The layers are not what the page shows now: their next frame
            // draws whole.
            layers.invalidate();
        }
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

/// What each of a frame's layers did, in draw order (`M`, `D`, `W`, ...),
/// and how big its target is.
fn kinds_of(layers: &ui::cpu_layers::CpuLayers) -> String {
    format!(
        "{}:{:?}",
        layers.report(),
        ui::retained_present::last_pan_frame()
    )
}

/// The page's texture slot for each of `draws`, keeping the slots of the
/// layers drawn again and giving the rest one.
fn slots_for(slots: &mut Vec<(u64, u32)>, draws: &[ui::cpu_layers::LayerDraw]) -> Vec<u32> {
    slots.retain(|(key, _)| draws.iter().any(|draw| draw.key == *key));
    let mut assigned = Vec::with_capacity(draws.len());
    for draw in draws {
        let slot = match slots.iter().find(|(key, _)| *key == draw.key) {
            Some((_, slot)) => *slot,
            None => {
                let slot = (0u32..)
                    .find(|candidate| !slots.iter().any(|(_, used)| used == candidate))
                    .unwrap_or(0);
                slots.push((draw.key, slot));
                slot
            }
        };
        assigned.push(slot);
    }
    assigned
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
