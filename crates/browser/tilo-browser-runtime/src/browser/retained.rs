//! What a mount's retained target holds: the frame presented last, which
//! each frame repaints only where it changed, and in debug builds the check
//! that such a repaint matches a full render.

#[cfg(debug_assertions)]
use wasm_bindgen::JsValue;

pub(super) struct Retained {
    pub(super) raster: ui::retained_raster::RetainedRaster,
    pub(super) frames: ui::retained_raster::RetainedFrames,
    /// Changed whenever the target's pixels may not be the last frame's.
    pub(super) generation: u64,
    /// Debug builds compare a frame repainted in part with a full render.
    #[cfg(debug_assertions)]
    check: Option<ui::retained_raster::RasterCheck>,
}

impl Retained {
    /// `on_cpu` rasterizes frames on the CPU and uploads the pixels: how a
    /// page draws until its renderer's pipelines are compiled.
    pub(super) fn new(on_cpu: bool) -> Self {
        Self {
            raster: ui::retained_raster::RetainedRaster::new(on_cpu),
            frames: ui::retained_raster::RetainedFrames::default(),
            generation: 0,
            #[cfg(debug_assertions)]
            check: None,
        }
    }

    /// Draw the frames after this one on the CPU or on the GPU: the target
    /// then holds nothing of the last frame, so the next frame repaints it
    /// whole.
    pub(super) fn switch_raster(&mut self, on_cpu: bool) {
        if self.raster.on_cpu() == on_cpu {
            return;
        }
        self.raster = ui::retained_raster::RetainedRaster::new(on_cpu);
        self.forget();
    }

    /// The target no longer shows the last presented frame: repaint all of
    /// the next one.
    pub(super) fn forget(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.frames.forget();
        self.raster.invalidate();
        #[cfg(debug_assertions)]
        {
            self.check = None;
        }
    }
}

/// Compare the last frame repainted in part, once read back, with a full
/// render of it, and start the check of this one if it was repainted in
/// part: a stale pixel is reported, never silently presented again.
#[cfg(debug_assertions)]
pub(super) fn check_retained(
    retained: &mut Retained,
    surface: &vello::util::RenderSurface<'static>,
    renderer: Option<&mut vello::Renderer>,
    handle: &ui::gpu::DeviceHandle,
    physical: &vello::Scene,
    key: ui::retained_raster::TargetKey,
    full_redraw: bool,
) {
    // The check renders a full frame on the GPU to compare with: a target
    // rasterized on the CPU, or one whose renderer has not compiled yet, has
    // nothing to compare with.
    if retained.raster.on_cpu() {
        retained.check = None;
        return;
    }
    let Some(renderer) = renderer else {
        retained.check = None;
        return;
    };
    if let Some(check) = &retained.check {
        let Some(differing) = check.differing() else {
            return;
        };
        if differing != 0 {
            web_sys::console::error_1(&JsValue::from_str(&format!(
                "retained frame differs from a full render in {differing} pixels"
            )));
        }
        retained.check = None;
    }
    if full_redraw {
        return;
    }
    let target = ui::retained_raster::RetainedTarget {
        texture: &surface.target_texture,
        view: &surface.target_view,
    };
    match ui::retained_raster::RasterCheck::start(
        (&handle.device, &handle.queue),
        renderer,
        target,
        physical,
        (key.width, key.height),
        key.base_color,
    ) {
        Ok(check) => retained.check = Some(check),
        Err(error) => web_sys::console::error_1(&JsValue::from_str(&error)),
    }
}
