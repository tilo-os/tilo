//! Per-output GPU targets.
//!
//! [`VelloDrmBridge::paint_output_until`] renders the scene through an [`OutputView`] into a
//! retained surface the size of that output, then runs the direct path's final pass (copy, PQ
//! encode or HDR composite) into the output's scanout target. Each output keeps its own pixels
//! and the revision they hold, so retained damage stays sparse per output. The surface swaps
//! into the bridge's target for the render: the vello renderer, its pipelines, client frames
//! and recovery budget, and the device are shared by every output.

use std::sync::Arc;
use std::time::Instant;

use vello::kurbo::{Affine, Rect};
use vello::{FineDamageRect, Scene, wgpu};

use super::{BridgeError, GpuFrameStats, RetainedGpuTarget, SceneDamage, VelloDrmBridge};
use crate::shell::hdr_surface::HdrLayer;
use crate::shell::render::OutputId;

/// Which part of the scene one output shows, and at what size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OutputView {
    pub id: OutputId,
    /// Scene → output pixels: `translate(-position)`. Folding the output transform in as
    /// well is step 2 of TIL-148.
    pub transform: Affine,
    /// The output's extent in pixels; its targets must have exactly this size.
    pub size: (u32, u32),
}

impl OutputView {
    /// The view of an output whose top-left corner sits at `position` in the scene.
    pub fn at(id: OutputId, position: (i32, i32), size: (u32, u32)) -> Self {
        Self {
            id,
            transform: Affine::translate((-f64::from(position.0), -f64::from(position.1))),
            size,
        }
    }

    /// The whole-pixel offset `transform` subtracts, when it is a pure translation by whole
    /// pixels: then scene damage maps onto output damage exactly.
    fn whole_pixel_offset(&self) -> Option<(i64, i64)> {
        let [xx, yx, xy, yy, tx, ty] = self.transform.as_coeffs();
        let linear_identity = xx.to_bits() == 1.0_f64.to_bits()
            && yy.to_bits() == 1.0_f64.to_bits()
            && yx == 0.0
            && xy == 0.0;
        let whole = |value: f64| {
            value.is_finite()
                && value.trunc().to_bits() == value.to_bits()
                && value.abs() <= f64::from(i32::MAX)
        };
        if !linear_identity || !whole(tx) || !whole(ty) {
            return None;
        }
        #[allow(
            clippy::cast_possible_truncation,
            reason = "both offsets are whole numbers within i32 range, checked above"
        )]
        Some(((-tx) as i64, (-ty) as i64))
    }

    /// Scene damage seen by this output, clipped to it. `None` when the view cannot map it
    /// exactly; the output then renders in full.
    fn damage(&self, regions: &[FineDamageRect]) -> Option<Vec<FineDamageRect>> {
        let (origin_x, origin_y) = self.whole_pixel_offset()?;
        let (width, height) = (i64::from(self.size.0), i64::from(self.size.1));
        let mut mapped = Vec::with_capacity(regions.len());
        for rect in regions {
            let x0 = i64::from(rect.x).saturating_sub(origin_x).max(0);
            let y0 = i64::from(rect.y).saturating_sub(origin_y).max(0);
            let x1 = i64::from(rect.x)
                .saturating_add(i64::from(rect.width))
                .saturating_sub(origin_x)
                .min(width);
            let y1 = i64::from(rect.y)
                .saturating_add(i64::from(rect.height))
                .saturating_sub(origin_y)
                .min(height);
            if x1 <= x0 || y1 <= y0 {
                continue;
            }
            mapped.push(FineDamageRect {
                x: u32::try_from(x0).ok()?,
                y: u32::try_from(y0).ok()?,
                width: u32::try_from(x1.saturating_sub(x0)).ok()?,
                height: u32::try_from(y1.saturating_sub(y0)).ok()?,
            });
        }
        Some(mapped)
    }

    fn covers_whole_scene(&self, scene_size: (u32, u32)) -> bool {
        self.transform == Affine::IDENTITY && self.size == scene_size
    }
}

/// How the final pass encodes an output: where SDR white sits for a PQ target, and the HDR
/// layers it composites, in scene coordinates.
#[derive(Clone, Copy)]
pub struct OutputSignal<'a> {
    pub sdr_white_nits: f32,
    pub hdr_layers: &'a [HdrLayer],
}

/// The texture the final pass writes: an imported scanout buffer, or any render target of
/// the bridge's device in the same formats.
#[derive(Clone, Copy)]
pub struct OutputTarget<'a> {
    pub view: &'a wgpu::TextureView,
    /// `Bgra8Unorm` receives a copy; the HDR scanout formats receive the PQ encode.
    pub format: wgpu::TextureFormat,
    pub size: (u32, u32),
}

impl<'a> OutputTarget<'a> {
    #[cfg(target_os = "linux")]
    pub fn imported(target: &'a ui::dmabuf_import::ImportedRenderTarget) -> Self {
        Self {
            view: &target.view,
            format: target.format,
            size: (target.width, target.height),
        }
    }
}

/// One output's retained pixels and the revision they hold: the parts of
/// [`RetainedGpuTarget`] that differ between outputs.
pub(super) struct OutputSurface {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    band: Option<(wgpu::Texture, wgpu::TextureView, u32)>,
    size: (u32, u32),
    retained_target_valid: bool,
    last_successful_revision: Option<u64>,
}

impl OutputSurface {
    fn new(device: &wgpu::Device, size: (u32, u32)) -> Result<Self, BridgeError> {
        RetainedGpuTarget::validate_size(device, size.0, size.1)?;
        let (texture, view) = RetainedGpuTarget::allocate_texture(device, size.0, size.1);
        Ok(Self {
            texture,
            view,
            band: None,
            size,
            retained_target_valid: false,
            last_successful_revision: None,
        })
    }

    /// Exchange these pixels with the ones `target` renders into. Calling it twice
    /// restores both.
    fn swap(&mut self, target: &mut RetainedGpuTarget) {
        std::mem::swap(&mut self.texture, &mut target.texture);
        std::mem::swap(&mut self.view, &mut target.view);
        std::mem::swap(&mut self.band, &mut target.band);
        std::mem::swap(&mut self.size, &mut target.size);
        std::mem::swap(
            &mut self.retained_target_valid,
            &mut target.retained_target_valid,
        );
        std::mem::swap(
            &mut self.last_successful_revision,
            &mut target.last_successful_revision,
        );
    }

    fn invalidate(&mut self) {
        self.retained_target_valid = false;
        self.last_successful_revision = None;
    }
}

/// `frame`'s backdrops as `transform` sees them.
fn backdrop_through(
    frame: &ui::backdrop::BackdropFrame,
    transform: Affine,
) -> ui::backdrop::BackdropFrame {
    ui::backdrop::BackdropFrame {
        regions: frame
            .regions
            .iter()
            .map(|region| {
                let mut below = Scene::new();
                below.append(&region.below, Some(transform));
                ui::backdrop::BackdropRegion {
                    image: region.image.clone(),
                    rect: transform.transform_rect_bbox(region.rect),
                    blur_px: region.blur_px,
                    below,
                    ground: region.ground,
                }
            })
            .collect(),
    }
}

impl VelloDrmBridge {
    /// Render `scene` through `view` into one output's imported direct-scanout target, in one
    /// submission, and wait for its completion marker. The final pass is the one
    /// [`Self::paint_imported_until`] runs; that function is the case where the view is the
    /// whole scene.
    ///
    /// # Errors
    ///
    /// Fails when the target is not the view's size, the output's surface cannot be
    /// allocated, rendering or submission fails, or the marker misses `deadline`.
    #[cfg(target_os = "linux")]
    pub fn paint_output_until(
        &mut self,
        scene: &Scene,
        view: &OutputView,
        damage: SceneDamage<'_>,
        target: &ui::dmabuf_import::ImportedRenderTarget,
        deadline: Instant,
        signal: OutputSignal<'_>,
    ) -> Result<GpuFrameStats, BridgeError> {
        self.paint_output_into(
            scene,
            view,
            damage,
            OutputTarget::imported(target),
            deadline,
            signal,
        )
    }

    /// [`Self::paint_output_until`] for any target of this bridge's device.
    ///
    /// # Errors
    ///
    /// As [`Self::paint_output_until`].
    pub fn paint_output_into(
        &mut self,
        scene: &Scene,
        view: &OutputView,
        damage: SceneDamage<'_>,
        target: OutputTarget<'_>,
        deadline: Instant,
        signal: OutputSignal<'_>,
    ) -> Result<GpuFrameStats, BridgeError> {
        if target.size != view.size {
            return Err(BridgeError::InvalidSize(format!(
                "output {} target is {}x{}, the output is {}x{}",
                view.id.0, target.size.0, target.size.1, view.size.0, view.size.1
            )));
        }
        if view.covers_whole_scene(self.target.size) {
            return self.paint_final_pass(scene, damage, target, deadline, signal);
        }
        // Rebuild a failed renderer at the scene's size before an output's surface stands
        // in for the scene target.
        if self.target.poisoned {
            let (width, height) = self.target.size;
            self.reconstruct_target(width, height)?;
        }
        let mut surface = self.take_output_surface(view)?;
        let mut view_scene = Scene::new();
        view_scene.append(scene, Some(view.transform));
        let regions = damage.regions.and_then(|regions| view.damage(regions));
        let damage = SceneDamage {
            revision: damage.revision,
            previous_revision: damage.previous_revision,
            regions: regions.as_deref(),
        };
        let clip = Rect::new(0.0, 0.0, f64::from(view.size.0), f64::from(view.size.1));
        let layers: Vec<HdrLayer> = signal
            .hdr_layers
            .iter()
            .filter_map(|layer| layer.transformed(view.transform, clip))
            .collect();
        let backdrop = self.target.backdrop.clone();
        self.target.backdrop = backdrop
            .as_deref()
            .map(|frame| Arc::new(backdrop_through(frame, view.transform)));
        surface.swap(&mut self.target);
        let painted = self.paint_final_pass(
            &view_scene,
            damage,
            target,
            deadline,
            OutputSignal {
                sdr_white_nits: signal.sdr_white_nits,
                hdr_layers: &layers,
            },
        );
        surface.swap(&mut self.target);
        self.target.backdrop = backdrop;
        self.output_surfaces.push((view.id, surface));
        painted
    }

    /// The output's surface, removed from the list for the render; a new one when the
    /// output has none or changed size.
    fn take_output_surface(&mut self, view: &OutputView) -> Result<OutputSurface, BridgeError> {
        if let Some(index) = self
            .output_surfaces
            .iter()
            .position(|(id, _)| *id == view.id)
        {
            let (_, surface) = self.output_surfaces.swap_remove(index);
            if surface.size == view.size {
                return Ok(surface);
            }
        }
        OutputSurface::new(&self.target.device, view.size)
    }

    /// Release the surfaces of outputs that are gone.
    pub fn retain_output_views(&mut self, live: &[OutputId]) {
        self.output_surfaces.retain(|(id, _)| live.contains(id));
    }

    /// Every output renders its next frame in full.
    pub(super) fn invalidate_output_surfaces(&mut self) {
        for (_, surface) in &mut self.output_surfaces {
            surface.invalidate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view_at(position: (i32, i32), size: (u32, u32)) -> OutputView {
        OutputView::at(OutputId(1), position, size)
    }

    #[test]
    fn output_view_maps_scene_damage_onto_its_own_pixels() {
        let view = view_at((100, 0), (100, 100));
        let damage = view
            .damage(&[
                FineDamageRect {
                    x: 90,
                    y: 10,
                    width: 20,
                    height: 5,
                },
                FineDamageRect {
                    x: 0,
                    y: 0,
                    width: 50,
                    height: 50,
                },
            ])
            .expect("a whole-pixel translation maps damage exactly");
        assert_eq!(
            damage,
            vec![FineDamageRect {
                x: 0,
                y: 10,
                width: 10,
                height: 5,
            }],
            "damage is clipped to the output and the other output's damage drops"
        );
        let fractional = OutputView {
            transform: Affine::translate((-100.5, 0.0)),
            ..view
        };
        assert_eq!(
            fractional.damage(&[]),
            None,
            "an inexact view renders in full"
        );
        assert!(view_at((0, 0), (200, 100)).covers_whole_scene((200, 100)));
        assert!(!view.covers_whole_scene((200, 100)));
    }
}
