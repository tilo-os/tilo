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
    /// Debug builds: what the frame the check started on was repainted
    /// with, and what the renderer had lost or grown at the time.
    #[cfg(debug_assertions)]
    check_note: Option<String>,
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
            #[cfg(debug_assertions)]
            check_note: None,
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
            self.check_note = None;
        }
    }
}

/// How the frame the target now holds was drawn: what the check reports in
/// its place when the frame it compares differs from a full render.
#[cfg(debug_assertions)]
pub(super) struct Drawn {
    /// The renderer's scratch now.
    pub(super) scratch: ui::render_bump_recovery::Scratch,
    /// The frame's repaint, and the state the renderer drew it in.
    pub(super) note: String,
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
    drawn: Drawn,
) {
    // The check renders a full frame on the GPU to compare with: a target
    // rasterized on the CPU, or one whose renderer has not compiled yet, has
    // nothing to compare with.
    if retained.raster.on_cpu() {
        retained.check = None;
        retained.check_note = None;
        return;
    }
    let Some(renderer) = renderer else {
        retained.check = None;
        retained.check_note = None;
        return;
    };
    if let Some(check) = &retained.check {
        let Some(readback) = check.differences(drawn.scratch) else {
            return;
        };
        match readback {
            ui::retained_raster::Readback::Differing(differences) if differences.count != 0 => {
                let where_ = differences
                    .bounds
                    .map_or_else(String::new, |(x0, y0, x1, y1)| {
                        format!(", at {x0},{y0} to {x1},{y1}")
                    });
                let runs = differences
                    .rows
                    .iter()
                    .map(|(y, runs)| {
                        let runs: Vec<String> = runs
                            .iter()
                            .map(|(from, to)| format!("{from}..{to}"))
                            .collect();
                        format!("{y}: {}", runs.join(","))
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                web_sys::console::error_1(&JsValue::from_str(&format!(
                    "retained frame differs from a full render in {} pixels{where_}, {} of them inside the bounds, {} at the base colour in the target and {} in the full render, after {}: at {:?} it shows the repainted frame's colour, then the full render's; rows {runs}",
                    differences.count,
                    differences.inside,
                    differences.kept_base,
                    differences.render_base,
                    retained
                        .check_note
                        .as_deref()
                        .unwrap_or("a frame the check did not start on"),
                    differences.first,
                )));
            }
            // A check that could not read the frames back proves nothing:
            // it says so, rather than that the frame is stale.
            ui::retained_raster::Readback::Unreadable(why) => {
                web_sys::console::warn_1(&JsValue::from_str(&format!(
                    "the retained check proves nothing for {}: {why}",
                    retained.check_note.as_deref().unwrap_or("an earlier frame")
                )));
            }
            ui::retained_raster::Readback::Differing(_) => {}
        }
        retained.check = None;
        retained.check_note = None;
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
        drawn.scratch,
    ) {
        Ok(check) => {
            retained.check = Some(check);
            retained.check_note = Some(drawn.note);
        }
        Err(error) => web_sys::console::error_1(&JsValue::from_str(&error)),
    }
}
