//! The DRM bridge's own lifecycle: building it on a render context, painting scenes and
//! outputs, the HDR and calibrated final passes, recovery back-off, and its telemetry.

use std::{
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

use pollster::FutureExt;
use settings_store::hardware::OutputTransform;
use ui::gpu::RenderContext;
use vello::peniko::Color;
use vello::{Scene, wgpu};
use wgpu::util::TextureBlitter;

use super::subrect::tight_clip_rows;
use super::{
    BridgeAdapterInfo, BridgeError, BridgePhaseTiming, CompletionPhase, CompletionPolicy,
    DeviceErrorLatch, DispatchStageObserver, FrameOutput, GpuFrameStats, LINUX_FENCE_WAIT,
    MappedXrgbSource, NEXT_XRGB_HISTORY_GENERATION, OutputSignal, OutputTarget,
    RECOVERY_BACKOFF_INITIAL, RECOVERY_BACKOFF_MAX, RetainedGpuTarget, SceneDamage, VelloDrmBridge,
    WARNED_CPU_FALLBACK, XrgbHistoryIdentity,
};
use crate::shell::hdr_surface::HdrLayer;
use crate::shell::render::hdr_composite::HdrCompositor;
use crate::shell::render::hdr_encode::HdrEncodePipeline;
use crate::shell::render::icc_lut::IccLut;
use crate::shell::render::orientation::OrientedCopy;

/// The presenting renderer's wgpu context and the index of its device.
///
/// Every backend but OpenGL first: a hardware adapter there needs no OpenGL, whose EGL
/// driver otherwise stays loaded and initialised for the whole session — on a machine
/// without a GPU, Mesa's llvmpipe beside lavapipe with a second rasteriser thread pool
/// (`ui::gpu`'s own choice keeps OpenGL for a process that sees a render node and has no
/// pinned Vulkan driver, which the compositor is). Only when those backends offer no
/// hardware adapter and OpenGL does is the context's own instance used, for a GPU only
/// OpenGL drives, exactly as before. `WGPU_BACKEND` still decides alone.
fn open_render_context() -> Option<(RenderContext, usize)> {
    let mut context = RenderContext::new();
    if wgpu::Backends::from_env().is_some() {
        let id = context.device(None).block_on()?;
        return Some((context, id));
    }
    let hardware =
        |adapter: &wgpu::Adapter| adapter.get_info().device_type != wgpu::DeviceType::Cpu;
    let own = std::mem::replace(
        &mut context.instance,
        wgpu::Instance::new(wgpu::InstanceDescriptor {
            display: None,
            backends: wgpu::Backends::all().difference(wgpu::Backends::GL),
            flags: wgpu::InstanceFlags::from_build_config().with_env(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            backend_options: wgpu::BackendOptions::from_env_or_default(),
        }),
    );
    let found = context.device(None).block_on();
    if ui::gpu::software_fallback().is_some() {
        // The GPU was given up: the software adapter found is the one wanted.
        return found.map(|id| (context, id));
    }
    let found_hardware = found
        .and_then(|id| context.devices.get(id))
        .is_some_and(|handle| hardware(&handle.adapter));
    if let Some(id) = found
        && (found_hardware
            || !own
                .enumerate_adapters(wgpu::Backends::all())
                .block_on()
                .iter()
                .any(hardware))
    {
        return Some((context, id));
    }
    // No adapter without OpenGL, or a GPU only the context's own backends drive: open it
    // there instead, as before.
    context.devices.clear();
    context.instance = own;
    let id = context.device(None).block_on()?;
    Some((context, id))
}

impl VelloDrmBridge {
    /// Build a bridge sized at `width × height`.
    ///
    /// # Errors
    /// Returns [`BridgeError::InvalidSize`] if `width` or `height` is `0` — DRM connector
    /// enumeration is expected to guarantee non-zero modes, but a driver could still hand back a
    /// degenerate one, and a panic here would take down the whole compositor session (`shell`
    /// runs with `panic = "abort"`). Returns [`BridgeError::NoAdapter`] if no wgpu adapter is
    /// available, [`BridgeError::InvalidDevice`] if the freshly acquired device id is out of
    /// range (defensive; cannot happen in practice), or [`BridgeError::Renderer`] if the vello
    /// renderer fails to build.
    pub fn new(width: u32, height: u32) -> Result<Self, BridgeError> {
        if width == 0 || height == 0 {
            return Err(BridgeError::InvalidSize(format!(
                "VelloDrmBridge::new: width/height must be > 0, got {width}x{height}"
            )));
        }

        let xrgb_history_generation = Self::next_xrgb_history_generation();
        let (context, dev_id) = open_render_context().ok_or_else(|| {
            BridgeError::NoAdapter(
                "no wgpu adapter (install vulkan/llvmpipe drivers in the runtime)".to_owned(),
            )
        })?;
        let dev_handle = context.devices.get(dev_id).ok_or_else(|| {
            BridgeError::InvalidDevice(format!("wgpu device id {dev_id} out of range"))
        })?;
        let device = &dev_handle.device;
        let adapter_info = dev_handle.adapter.get_info();
        tracing::info!(
            adapter = %adapter_info.name,
            backend = ?adapter_info.backend,
            device_type = ?adapter_info.device_type,
            driver = %adapter_info.driver,
            vendor = format_args!("{:#06x}", adapter_info.vendor),
            device = format_args!("{:#06x}", adapter_info.device),
            pci_bus_id = %adapter_info.device_pci_bus_id,
            "DRM CPU-fallback wgpu adapter selected"
        );

        let device_errors = DeviceErrorLatch::install(device);
        let mut loss_watch = ui::gpu::LossWatch::new(dev_handle.loss_monitor());
        // A GPU that cannot be brought back loses every device opened on it, before its first
        // frame.
        #[cfg(target_os = "linux")]
        if super::dead_gpu_injected(ui::gpu::software_fallback().is_some()) {
            apply_injected_fault(device, super::InjectedGpuFault::Dead);
        }
        let mut target = match RetainedGpuTarget::new(dev_handle, width, height) {
            Ok(target) => target,
            Err(error) => {
                // A device that is lost, or whose GPU stopped, before it finished one frame.
                let reason = match &error {
                    BridgeError::DeviceLost(reason) => Some(reason.clone()),
                    // A GPU's warm-up frames are bounded by `NOT_PROGRESSING_AFTER`.
                    BridgeError::DeadlineExceeded | BridgeError::CompletionTimeout { .. }
                        if adapter_info.device_type != wgpu::DeviceType::Cpu =>
                    {
                        loss_watch
                            .stopped(dev_handle.queue.clone(), ui::gpu::NOT_PROGRESSING_AFTER);
                        dev_handle.loss_monitor().reason()
                    }
                    _ => None,
                }
                .or_else(|| dev_handle.loss_monitor().reason());
                if let Some(reason) = reason {
                    super::gpu_lost(&adapter_info, false, &reason);
                }
                return Err(error);
            }
        };
        // Client buffers are imported where the scene is drawn, so their
        // frames reach the panel without crossing the CPU.
        target.present_generation = Some(crate::shell::gpu_frames::publish_present_device(
            device,
            &dev_handle.queue,
        ));
        let bgra_blitter = TextureBlitter::new(device, wgpu::TextureFormat::Bgra8Unorm);
        let (readback_buf, padded_bytes_per_row) = Self::allocate_staging(device, width, height);
        let device_memory = crate::shell::heap_trim::register_device(&adapter_info, device);

        Ok(Self {
            context,
            target,
            device_errors,
            loss_watch,
            adapter: adapter_info,
            bgra_blitter,
            dispatch_stage_observer: std::env::var_os("TILO_PROFILE_GPU_STAGES")
                .is_some_and(|value| value == "1")
                .then(DispatchStageObserver::default),
            hdr_encoders: Vec::new(),
            oriented_copy: None,
            hdr_compositor: None,
            dev_id,

            readback_buf,
            padded_bytes_per_row,
            readback: Vec::new(),

            phase_timing_enabled: false,
            last_phase_timing: None,

            retained_staging_valid: false,
            staging_content: None,
            first_frame_pending: true,

            xrgb_history_generation,
            xrgb_history_epoch: Some(1),
            recovery_not_before: None,
            recovery_backoff: RECOVERY_BACKOFF_INITIAL,
            output_surfaces: Vec::new(),
            _device_memory: device_memory,
            logged_memory: None,
            windows_closed_seen: crate::shell::heap_trim::windows_closed(),
            scratch_shrinks_seen: 0,
            #[cfg(test)]
            fail_next_tracked_swizzle: false,
        })
    }

    /// Draw the floating surfaces of the frames rendered from now on over
    /// `backdrop` (see [`crate::compositor::frame::FramePaint::backdrop`]).
    /// `None` draws them opaque.
    pub fn set_backdrop(&mut self, backdrop: Option<Arc<ui::backdrop::BackdropFrame>>) {
        self.target.backdrop = backdrop;
    }

    /// [`RetainedGpuTarget::stage_scroll_copies`] on this bridge's target.
    pub fn stage_scroll_copies(
        &mut self,
        revision: Option<u64>,
        copies: Option<&crate::shell::frame::ScrollCopies>,
    ) {
        self.target.stage_scroll_copies(revision, copies);
    }

    /// [`RetainedGpuTarget::last_scroll_copy_work`] of this bridge's target.
    pub fn last_scroll_copy_work(&self) -> Option<(u64, u64)> {
        self.target.last_scroll_copy_work()
    }

    /// `true` exactly once: on the first call after this bridge was built, which its owner
    /// makes once the bridge's first frame was painted. The pipelines' compile scratch is
    /// free by then.
    pub fn take_first_frame(&mut self) -> bool {
        std::mem::take(&mut self.first_frame_pending)
    }

    /// Whether [`Self::paint`] has stored a complete tight RGBA frame.
    pub fn has_complete_readback(&self) -> bool {
        let (width, height) = self.target.size;
        let needed = usize::try_from(width)
            .unwrap_or(0)
            .saturating_mul(usize::try_from(height).unwrap_or(0))
            .saturating_mul(4);
        needed > 0 && self.readback.len() == needed
    }

    /// The tight RGBA of the last complete frame, when one is encoded.
    pub(crate) fn encoded_rgba(&self) -> Option<&[u8]> {
        self.has_complete_readback()
            .then_some(self.readback.as_slice())
    }

    /// Bring the encoded desktop up to `scene` inside `regions`: one render
    /// of the scene repairing only those regions (sparse when the target
    /// retains the previous one), then a readback of each region alone into
    /// the tight image. The combined framebuffer is not mapped; pixels
    /// outside `regions` stay as the previous frame encoded them.
    ///
    /// # Errors
    /// The desktop image is missing, `regions` is empty or one leaves it, or a
    /// render or region copy fails. The tight image is then partly stale:
    /// the caller must present a full frame instead.
    pub(crate) fn refresh_encoded_regions(
        &mut self,
        scene: &Scene,
        regions: &[vello::FineDamageRect],
    ) -> Result<(), BridgeError> {
        // A stale full-frame profile must not be reported for these regions.
        self.last_phase_timing = None;
        let started = Instant::now();
        self.render_encoded_regions(scene, regions)?;
        let rendered = Instant::now();
        let mut bytes = 0_u64;
        let mut patch_us = 0_u128;
        for &clip in regions {
            let tight = self.read_encoded_clip(clip)?;
            let patch_started = Instant::now();
            self.patch_encoded_clip(clip, &tight)?;
            patch_us = patch_us.saturating_add(patch_started.elapsed().as_micros());
            bytes = bytes.saturating_add(u64::try_from(tight.len()).unwrap_or(u64::MAX));
        }
        if self.phase_timing_enabled {
            self.last_phase_timing = Some(BridgePhaseTiming {
                submit_us: rendered.saturating_duration_since(started).as_micros(),
                completed_readback_map_us: rendered.elapsed().as_micros().saturating_sub(patch_us),
                cpu_copy_swizzle_us: patch_us,
                readback_bytes: bytes,
                cpu_output_bytes: bytes,
                ..BridgePhaseTiming::default()
            });
        }
        Ok(())
    }

    fn render_encoded_regions(
        &mut self,
        scene: &Scene,
        regions: &[vello::FineDamageRect],
    ) -> Result<(), BridgeError> {
        if !self.has_complete_readback() {
            return Err(BridgeError::InvalidSize(
                "no encoded desktop for the regions".to_owned(),
            ));
        }
        let (width, height) = self.target.size;
        let inside = |clip: &vello::FineDamageRect| {
            clip.width > 0
                && clip.height > 0
                && clip.x.checked_add(clip.width).is_some_and(|x1| x1 <= width)
                && clip
                    .y
                    .checked_add(clip.height)
                    .is_some_and(|y1| y1 <= height)
        };
        if regions.is_empty() || !regions.iter().all(inside) {
            return Err(BridgeError::InvalidSize(
                "a region is outside the encoded desktop".to_owned(),
            ));
        }
        let revision = self
            .target
            .last_successful_revision
            .map_or(1, |revision| revision.saturating_add(1));
        let previous = self.target.last_successful_revision;
        let damage = SceneDamage {
            revision,
            previous_revision: previous,
            regions: Some(regions),
        };
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(1))
            .unwrap_or_else(Instant::now);
        self.target.render_until(scene, damage, deadline)?;
        Ok(())
    }

    fn read_encoded_clip(&self, clip: vello::FineDamageRect) -> Result<Vec<u8>, BridgeError> {
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let tight_row = clip.width.saturating_mul(4);
        let padded = tight_row.div_ceil(align).saturating_mul(align);
        let size = u64::from(padded).saturating_mul(u64::from(clip.height));
        if size == 0 {
            return Err(BridgeError::InvalidSize("empty clip readback".to_owned()));
        }
        let buffer = self.target.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tilo-desktop-drm-clip-readback"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder =
            self.target
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("tilo-desktop-drm-clip-copy"),
                });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.target.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: clip.x,
                    y: clip.y,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(clip.height),
                },
            },
            wgpu::Extent3d {
                width: clip.width,
                height: clip.height,
                depth_or_array_layers: 1,
            },
        );
        let submission = self.target.queue.submit(Some(encoder.finish()));
        self.target
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(Duration::from_secs(1)),
            })
            .map_err(|error| BridgeError::Map(format!("clip copy: {error:?}")))?;
        let tight = {
            let mapped = Self::map_device_readback(&self.target.device, &buffer)?;
            tight_clip_rows(&mapped, padded, clip)
        };
        buffer.unmap();
        tight
    }

    fn patch_encoded_clip(
        &mut self,
        clip: vello::FineDamageRect,
        tight: &[u8],
    ) -> Result<(), BridgeError> {
        let scene_w = usize::try_from(self.target.size.0).unwrap_or(0);
        let row = usize::try_from(clip.width).unwrap_or(0).saturating_mul(4);
        let origin_x = usize::try_from(clip.x).unwrap_or(0);
        let origin_y = usize::try_from(clip.y).unwrap_or(0);
        let rows = usize::try_from(clip.height).unwrap_or(0);
        if scene_w == 0 || row == 0 || rows == 0 {
            return Err(BridgeError::InvalidSize(
                "encoded clip does not fit the desktop".to_owned(),
            ));
        }
        for row_index in 0..rows {
            let src_off = row_index.saturating_mul(row);
            let Some(src) = tight.get(src_off..src_off.saturating_add(row)) else {
                break;
            };
            let dest_off = origin_y
                .saturating_add(row_index)
                .saturating_mul(scene_w)
                .saturating_add(origin_x)
                .saturating_mul(4);
            let Some(dest) = self
                .readback
                .get_mut(dest_off..dest_off.saturating_add(row))
            else {
                break;
            };
            dest.copy_from_slice(src);
        }
        Ok(())
    }

    /// Override the per-frame clear color; matches the winit backend API so callers can swap themes uniformly.
    pub fn set_base_color(&mut self, color: Color) {
        if self.target.base_color == color {
            return;
        }
        self.target.base_color = color;
        // Sparse rendering never clears pixels outside its rectangles. A new clear color is a
        // full-frame change even if the scene recording itself is unchanged.
        self.invalidate_retained();
        self.invalidate_output_surfaces();
        self.bump_xrgb_history_epoch();
    }

    /// The device and queue this bridge renders with, for targets it renders
    /// into that were not imported from a dma-buf.
    pub fn gpu(&self) -> (&wgpu::Device, &wgpu::Queue) {
        (&self.target.device, &self.target.queue)
    }

    #[cfg(test)]
    pub(crate) fn present_generation_for_test(&self) -> Option<u64> {
        self.target.present_generation
    }

    /// Import a compositor-owned linear scanout dma-buf (`XRGB8888`, or `XBGR2101010` /
    /// `ABGR16161616F` for HDR) on this bridge's exact wgpu device.
    ///
    /// Keeping import here prevents a target created by another device from reaching the render
    /// callback. Retain the returned alias while the scanout allocation remains alive.
    ///
    /// # Errors
    ///
    /// Fails when the device cannot import the dma-buf as a render target.
    #[cfg(target_os = "linux")]
    pub fn import_scanout_target(
        &self,
        image: &ui::dmabuf_import::DmabufRef<'_>,
    ) -> Result<ui::dmabuf_import::ImportedRenderTarget, BridgeError> {
        ui::dmabuf_import::import_render_target(&self.target.device, image)
            .map_err(|error| BridgeError::Import(error.to_string()))
    }

    /// Render into an imported direct-scanout target in one submission, then wait for the
    /// existing completion marker before returning. An SDR (`Bgra8Unorm`) target receives a
    /// plain copy; an HDR target receives the PQ BT.2020 encode with SDR white at
    /// `sdr_white_nits`, compositing `hdr_layers` at full precision in linear light.
    ///
    /// The one-output case of [`Self::paint_output_until`]: the scene is the output, so the
    /// bridge's own target is the output's retained surface.
    #[cfg(target_os = "linux")]
    pub fn paint_imported_until(
        &mut self,
        scene: &Scene,
        damage: SceneDamage<'_>,
        target: &ui::dmabuf_import::ImportedRenderTarget,
        deadline: Instant,
        sdr_white_nits: f32,
        hdr_layers: &[HdrLayer],
    ) -> Result<GpuFrameStats, BridgeError> {
        self.paint_final_pass(
            scene,
            damage,
            OutputTarget::imported(target),
            deadline,
            OutputSignal {
                sdr_white_nits,
                hdr_layers,
                color_lut: None,
                dim: None,
                layers: None,
            },
        )
    }

    /// The PQ encode pipeline for `format` exists and encodes SDR white at `sdr_white_nits`
    /// into a target turned by `orientation`.
    fn prepare_hdr_encoder(
        &mut self,
        format: wgpu::TextureFormat,
        sdr_white_nits: f32,
        orientation: OutputTransform,
    ) {
        if !self
            .hdr_encoders
            .iter()
            .any(|pipeline| pipeline.format() == format)
        {
            self.hdr_encoders
                .push(HdrEncodePipeline::new(&self.target.device, format));
        }
        if let Some(pipeline) = self
            .hdr_encoders
            .iter()
            .find(|pipeline| pipeline.format() == format)
        {
            pipeline.set_params(&self.target.queue, sdr_white_nits, orientation);
        }
    }

    /// The final pass `output` needs exists and is set up for the next submission: the PQ
    /// encode for an HDR target, the oriented copy for an SDR one that is rotated,
    /// calibrated (`color_lut`) or dimmed in software (`dim`). Returns whether the oriented
    /// copy runs; an unrotated, uncalibrated, undimmed SDR target keeps the plain blit. An
    /// HDR target is dimmed by placing SDR white lower.
    fn prepare_final_pass(
        &mut self,
        output: OutputTarget<'_>,
        hdr: bool,
        sdr_white_nits: f32,
        (color_lut, dim): (Option<&IccLut>, Option<f32>),
    ) -> bool {
        if hdr {
            let sdr_white_nits = sdr_white_nits * dim.unwrap_or(1.0);
            self.prepare_hdr_encoder(output.format, sdr_white_nits, output.orientation);
            return false;
        }
        if output.orientation == OutputTransform::Normal && color_lut.is_none() && dim.is_none() {
            return false;
        }
        let (device, queue) = (&self.target.device, &self.target.queue);
        self.oriented_copy
            .get_or_insert_with(|| OrientedCopy::new(device))
            .prepare(device, queue, output.orientation, color_lut, dim);
        true
    }

    /// Render `scene` into the target currently swapped into `self.target`, then run the
    /// final pass into `output`: a copy for `Bgra8Unorm`, the PQ encode (and the HDR
    /// composite when there are layers) otherwise, each turned by `output.orientation`.
    /// Waits for the completion marker.
    pub(super) fn paint_final_pass(
        &mut self,
        scene: &Scene,
        damage: SceneDamage<'_>,
        output: OutputTarget<'_>,
        deadline: Instant,
        signal: OutputSignal<'_>,
    ) -> Result<GpuFrameStats, BridgeError> {
        // A strip pan is composited from the retained layers, which hold the
        // frame's SDR pixels upright, as a render of its scene would: the
        // same final pass then turns, calibrates or PQ-encodes them. HDR
        // surfaces need the scene's full-precision composite.
        if let Some(cache) = signal
            .layers
            .filter(|cache| signal.hdr_layers.is_empty() && self.target.layers_eligible(cache))
        {
            return self.paint_layers_final_pass(cache, damage.revision, output, deadline, signal);
        }
        self.paint_scene_final_pass(scene, damage, output, deadline, signal)
    }

    /// [`Self::paint_final_pass`] for a frame that came as layers: the
    /// target composites it ([`RetainedGpuTarget::render_layers_until`]),
    /// then the output's final pass (a copy, turned and calibrated when the
    /// output is, or the PQ encode of an HDR output) writes `output`.
    fn paint_layers_final_pass(
        &mut self,
        cache: &crate::shell::frame::FrameCache,
        revision: u64,
        output: OutputTarget<'_>,
        deadline: Instant,
        signal: OutputSignal<'_>,
    ) -> Result<GpuFrameStats, BridgeError> {
        self.last_phase_timing = None;
        if let Some(error) = self.take_device_error() {
            return Err(error);
        }
        if self.target.poisoned {
            let (width, height) = self.target.size;
            self.reconstruct_target(width, height)?;
        }
        output.check_shows(self.target.size)?;
        let hdr = output.format != wgpu::TextureFormat::Bgra8Unorm;
        // The profile describes the panel's SDR response; the PQ encode replaces it.
        let color_lut = signal.color_lut.filter(|_| !hdr);
        let copies =
            self.prepare_final_pass(output, hdr, signal.sdr_white_nits, (color_lut, signal.dim));
        let encode = self
            .hdr_encoders
            .iter()
            .find(|pipeline| hdr && pipeline.format() == output.format);
        let oriented = self.oriented_copy.as_ref().filter(|_| copies);
        let blitter = &self.bgra_blitter;
        let view = output.view;
        let rendered = self.target.render_layers_until(
            cache,
            revision,
            deadline,
            |device, encoder, source| match (encode, oriented) {
                (Some(pipeline), _) => pipeline.encode(device, encoder, source, view),
                (None, Some(copy)) => copy.copy(device, encoder, source, view, color_lut),
                (None, None) => blitter.copy(device, encoder, source, view),
            },
        );
        rendered.and_then(|stats| self.take_device_error().map_or(Ok(stats), Err))
    }

    /// [`Self::paint_final_pass`] for a frame rendered from its scene.
    fn paint_scene_final_pass(
        &mut self,
        scene: &Scene,
        damage: SceneDamage<'_>,
        output: OutputTarget<'_>,
        deadline: Instant,
        signal: OutputSignal<'_>,
    ) -> Result<GpuFrameStats, BridgeError> {
        let OutputSignal {
            sdr_white_nits,
            hdr_layers,
            ..
        } = signal;
        self.last_phase_timing = None;
        if let Some(error) = self.take_device_error() {
            return Err(error);
        }
        if self.target.poisoned {
            // A failed direct frame must not leave the next attempt with a
            // target that refuses to render.
            let (width, height) = self.target.size;
            self.reconstruct_target(width, height)?;
        }
        output.check_shows(self.target.size)?;
        // Frames this output skipped while its flip was pending left their damage owed: the
        // retained pixels already claim those revisions, so this render repairs it too.
        let repaired = self.target.take_owed_damage(damage.regions);
        if repaired.is_some() {
            // The skipped frames' pixels are owed, not in the target: a copy
            // would move what they replaced.
            self.target.stage_scroll_copies(None, None);
        }
        let damage = damage.with_regions(repaired.as_deref());
        let hdr = output.format != wgpu::TextureFormat::Bgra8Unorm;
        let composite = hdr && !hdr_layers.is_empty();
        if composite {
            let (device, queue) = (&self.target.device, &self.target.queue);
            let compositor = self
                .hdr_compositor
                .get_or_insert_with(|| HdrCompositor::new(device, queue));
            self.target.hdr_pass = Some(compositor.prepare(
                self.target.size,
                hdr_layers,
                // Dimmed in the final pass: SDR white sits lower.
                Some((output.format, sdr_white_nits * signal.dim.unwrap_or(1.0))),
            ));
            compositor.set_orientation(output.orientation);
        }
        // The substituted layer images are not what the retained target holds
        // from an earlier frame, so a composited frame renders densely.
        let damage = if composite {
            SceneDamage {
                revision: damage.revision,
                previous_revision: None,
                regions: None,
            }
        } else {
            damage
        };
        // The profile describes the panel's SDR response; the PQ encode replaces it.
        let color_lut = signal.color_lut.filter(|_| !hdr);
        let copies = self.prepare_final_pass(output, hdr, sdr_white_nits, (color_lut, signal.dim));
        let encode = self
            .hdr_encoders
            .iter()
            .find(|pipeline| hdr && pipeline.format() == output.format);
        let oriented = self.oriented_copy.as_ref().filter(|_| copies);
        let blitter = &self.bgra_blitter;
        let view = output.view;
        let format = output.format;
        let compositor = self.hdr_compositor.as_ref().filter(|_| composite);
        let previous_dispatch_submission = self.dispatch_stage_observer.as_ref().and_then(|_| {
            self.target
                .renderer
                .gpu_dispatch_timestamp_latest_submission_id()
        });
        let mut timing = self.phase_timing_enabled.then(BridgePhaseTiming::default);
        let rendered = self.target.render(
            scene,
            Some(damage),
            FrameOutput::GpuOnly,
            CompletionPolicy::Absolute(deadline),
            timing.as_mut(),
            |device, encoder, source| match (compositor, encode, oriented) {
                (Some(compositor), _, _) => compositor.encode(encoder, source, view, format),
                (None, Some(pipeline), _) => pipeline.encode(device, encoder, source, view),
                (None, None, Some(copy)) => copy.copy(device, encoder, source, view, color_lut),
                (None, None, None) => blitter.copy(device, encoder, source, view),
            },
        );
        self.target.hdr_pass = None;
        if composite {
            // The retained frame holds the layers black: no later frame may
            // repair it sparsely, whether or not it composites.
            self.target.last_successful_revision = None;
        }
        let rendered = rendered.and_then(|output| self.take_device_error().map_or(Ok(output), Err));
        if let Some(observer) = &mut self.dispatch_stage_observer {
            observer.record(
                &mut self.target,
                damage.revision,
                rendered.is_ok(),
                previous_dispatch_submission,
                self.xrgb_history_generation,
            );
        }
        self.last_phase_timing = rendered.as_ref().ok().and(timing);
        let (_, admitted) = rendered?;
        Ok(GpuFrameStats {
            revision: damage.revision,
            damage_admitted: admitted,
            render_attempts: self.target.last_render_attempts,
            sparse_attempts: self.target.last_sparse_render_attempts,
            full_attempts: self.target.last_full_render_attempts,
            completion_markers: self.target.completion_markers,
            framebuffer_copy_bytes: 0,
            framebuffer_staging_bytes: 0,
        })
    }

    fn next_xrgb_history_generation() -> Option<u64> {
        NEXT_XRGB_HISTORY_GENERATION
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |generation| {
                generation.checked_add(1)
            })
            .ok()
    }

    fn bump_xrgb_history_epoch(&mut self) {
        self.xrgb_history_epoch = self
            .xrgb_history_epoch
            .and_then(|epoch| epoch.checked_add(1));
    }

    /// Wait for each Linux completion phase until the GPU actually finishes, bounded only by a
    /// generous backstop, instead of the production one-second allowance.
    ///
    /// On a shared CI host llvmpipe competes with every other test for CPU, so a frame whose
    /// pixels a fixture checks can legitimately miss the per-phase budget. Fixtures that assert
    /// what is rendered, not how fast, opt in here; device loss still fails immediately.
    #[cfg(test)]
    pub(crate) fn wait_for_gpu_completion_for_test(&mut self) {
        self.target.phase_wait_override = Some(tilo_test_env::hang_bound());
    }

    /// Compile the renderer's full and sparse pipelines outside any frame deadline, then start
    /// over from an invalidated target and a new history epoch.
    ///
    /// llvmpipe builds compute variants at first dispatch. On a cold shader cache that takes
    /// longer than the Linux per-phase fence allowance, so a fixture asserting on
    /// deadline-bound frames warms the renderer first instead of timing the compiler.
    #[cfg(test)]
    pub(crate) fn warm_renderer_for_test(&mut self) {
        use vello::kurbo::{Affine, Rect};
        use vello::peniko::Fill;

        let (width, height) = self.target.size;
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgb8(0x80, 0x80, 0x80),
            None,
            &Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
        );
        let deadline = tilo_test_env::hang_deadline();
        self.target
            .render_until(
                &scene,
                SceneDamage {
                    revision: 1,
                    previous_revision: None,
                    regions: None,
                },
                deadline,
            )
            .expect("warm-up full render");
        let region = [vello::FineDamageRect {
            x: 0,
            y: 0,
            width: width.min(16),
            height: height.min(16),
        }];
        self.target
            .render_until(
                &scene,
                SceneDamage {
                    revision: 2,
                    previous_revision: Some(1),
                    regions: Some(&region),
                },
                deadline,
            )
            .expect("warm-up sparse render");
        self.invalidate_retained();
        self.bump_xrgb_history_epoch();
    }

    /// Refuse to submit another frame while a timed-out software-rendered submission drains.
    /// Returning before `render_normal` is called leaves the caller's dumb buffer untouched, so
    /// KMS continues showing the last complete frame instead of exposing a half-painted one.
    pub(super) fn check_recovery_backoff(&mut self) -> Result<(), BridgeError> {
        let Some(not_before) = self.recovery_not_before else {
            return Ok(());
        };
        let now = Instant::now();
        if now < not_before {
            let retry_in_ms = not_before
                .saturating_duration_since(now)
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX);
            return Err(BridgeError::RecoveryBackoff { retry_in_ms });
        }
        self.recovery_not_before = None;
        self.recovery_backoff = RECOVERY_BACKOFF_INITIAL;
        Ok(())
    }

    /// Completion timeouts are recoverable, but only after an explicit cool-down. Other errors
    /// retain their existing fail-closed behaviour and may be retried on the next frame.
    pub(super) fn note_render_failure(&mut self, error: &BridgeError) {
        if !matches!(error, BridgeError::CompletionTimeout { .. }) {
            self.recovery_not_before = None;
            self.recovery_backoff = RECOVERY_BACKOFF_INITIAL;
            return;
        }
        let delay = self.recovery_backoff;
        self.recovery_not_before = Instant::now().checked_add(delay);
        self.recovery_backoff = delay
            .checked_mul(2)
            .unwrap_or(RECOVERY_BACKOFF_MAX)
            .min(RECOVERY_BACKOFF_MAX);
        tracing::warn!(
            ?delay,
            %error,
            "DRM paint completion timed out; preserving the last scanout and backing off renderer recovery"
        );
    }

    /// Identity required by an opt-in XRGB destination history. A saturated epoch is deliberately
    /// unavailable so stale destination state can never become valid again through wraparound.
    #[doc(hidden)]
    pub fn xrgb_history_identity(
        &self,
        dst_pitch: usize,
        targets: usize,
    ) -> Option<XrgbHistoryIdentity> {
        let generation = self.xrgb_history_generation?;
        let epoch = self.xrgb_history_epoch?;
        let (width, height) = self.target.size;
        let tight = usize::try_from(width).ok()?.checked_mul(4)?;
        if width == 0 || height == 0 || dst_pitch < tight || !(1..=2).contains(&targets) {
            return None;
        }
        Some(XrgbHistoryIdentity {
            bridge_generation: generation,
            bridge_epoch: epoch,
            width,
            height,
            pitch: dst_pitch,
            targets,
            scanout_epoch: 0,
        })
    }

    /// Whether this bridge can participate in the opt-in XRGB destination history.
    ///
    /// A saturated process identity or content epoch disables only tracking. Callers must retain
    /// their established complete-output path in that case.
    #[doc(hidden)]
    #[must_use]
    pub fn xrgb_history_available(&self) -> bool {
        self.xrgb_history_generation.is_some() && self.xrgb_history_epoch.is_some()
    }

    /// Enable wall-clock phase collection for diagnostic runs. This deliberately defaults to
    /// false so normal compositor paints do not take timestamps or retain timing state.
    pub fn set_phase_timing_enabled(&mut self, enabled: bool) {
        self.phase_timing_enabled = enabled;
        self.last_phase_timing = None;
    }

    /// Adapter opened by this bridge's `RenderContext`.
    ///
    /// # Errors
    /// Returns [`BridgeError::InvalidDevice`] if the stored device identifier is invalid.
    pub fn adapter_info(&self) -> Result<BridgeAdapterInfo, BridgeError> {
        let dev_handle = self.context.devices.get(self.dev_id).ok_or_else(|| {
            BridgeError::InvalidDevice(format!("wgpu device id {} out of range", self.dev_id))
        })?;
        let info = dev_handle.adapter.get_info();
        Ok(BridgeAdapterInfo {
            name: info.name,
            backend: format!("{:?}", info.backend),
            device_type: format!("{:?}", info.device_type),
            driver: info.driver,
            vendor: info.vendor,
            device: info.device,
            pci_bus_id: info.device_pci_bus_id,
            #[cfg(target_os = "linux")]
            drm_device: ui::dmabuf_import::vulkan_drm_device_identity(&dev_handle.device),
        })
    }

    /// Latest diagnostic timing. `None` means profiling was not enabled for that paint.
    pub fn last_phase_timing(&self) -> Option<BridgePhaseTiming> {
        self.last_phase_timing
    }

    /// Consume the completed paint phases so a skipped subsequent frame cannot reuse them.
    pub fn take_phase_timing(&mut self) -> Option<BridgePhaseTiming> {
        self.last_phase_timing.take()
    }

    /// Current opt-in Vello per-dispatch timestamp diagnostic state.
    ///
    /// This is a deliberately thin diagnostic facade: ordinary DRM paints never call it or
    /// allocate timestamp bookkeeping. A benchmark that opts in to Vello timestamps should read
    /// [`Self::gpu_dispatch_timestamp_latest_submission_id`] after a completed paint, then take
    /// the sample and compare its submission id exactly rather than assigning a delayed sample to
    /// a different frame.
    #[doc(hidden)]
    pub fn gpu_dispatch_timestamp_status(&mut self) -> vello::GpuDispatchTimestampStatus {
        self.target.renderer.gpu_dispatch_timestamp_status()
    }

    /// Consume the one bounded Vello per-dispatch timestamp sample, if its asynchronous map has
    /// completed. Diagnostic callers must match [`vello::GpuDispatchTimestampSample::submission_id`]
    /// to [`Self::gpu_dispatch_timestamp_latest_submission_id`].
    #[doc(hidden)]
    pub fn take_gpu_dispatch_timestamp_sample(
        &mut self,
    ) -> Option<vello::GpuDispatchTimestampSample> {
        self.target.renderer.take_gpu_dispatch_timestamp_sample()
    }

    /// Identity assigned to the latest Vello recording when opt-in per-dispatch timestamps are
    /// available. The id advances even when the bounded diagnostic readback slot was busy, so a
    /// missing sample must remain missing rather than being attributed to a newer paint.
    #[doc(hidden)]
    pub fn gpu_dispatch_timestamp_latest_submission_id(&self) -> Option<u64> {
        self.target
            .renderer
            .gpu_dispatch_timestamp_latest_submission_id()
    }

    /// Bytes allocated exclusively for opt-in per-dispatch timestamp resolve/readback buffers.
    #[doc(hidden)]
    pub fn gpu_dispatch_timestamp_buffer_bytes(&self) -> u64 {
        self.target.renderer.gpu_dispatch_timestamp_buffer_bytes()
    }

    /// Effective initialization-time direct path workgroup count, exposed only for an explicit
    /// diagnostic benchmark header. Normal paint behavior does not query or alter it.
    #[doc(hidden)]
    pub fn direct_path_workgroups(&self) -> u32 {
        self.target.renderer.direct_path_workgroups()
    }

    /// Exact CPU upload bytes from the most recently encoded Vello recording. This borrows
    /// renderer-owned diagnostic state and is valid only until the next render starts.
    #[doc(hidden)]
    pub fn diagnostic_packed_scene(&self) -> &[u8] {
        self.target.renderer.diagnostic_packed_scene()
    }

    /// Exact resolver-expanded geometry inputs from the latest ordinary recording.
    ///
    /// Profile-only callers use this as an input-opportunity diagnostic; it does not retain GPU
    /// geometry or infer current bump counters from the ordinary settle result.
    #[doc(hidden)]
    pub fn diagnostic_geometry_input(
        &self,
    ) -> Result<vello::GeometryInput<'_>, vello::GeometryInputError> {
        self.target.renderer.diagnostic_geometry_input()
    }

    /// Render `scene` into the offscreen texture and return a slice of the tight-packed RGBA bytes
    /// (`width * height * 4`). The slice's lifetime is tied to `&mut self`; callers must clone bytes out before the next `paint`.
    ///
    /// # Errors
    /// Returns a [`BridgeError`] if the vello render, the texture→buffer copy, or the readback
    /// buffer mapping fails. A readback row that doesn't fit the mapped buffer (shouldn't happen
    /// given `allocate_targets`, but guarded rather than trusted) is logged and truncates the
    /// returned buffer instead of panicking.
    pub fn paint(&mut self, scene: &Scene) -> Result<&[u8], BridgeError> {
        self.last_phase_timing = None;
        self.check_recovery_backoff()?;
        // This API intentionally has no revision. It always renders densely and cannot seed a
        // later versioned sparse frame, even though it refreshes the retained texture/staging.
        self.invalidate_retained();
        if !WARNED_CPU_FALLBACK.swap(true, Ordering::AcqRel) {
            tracing::warn!(
                "DRM paint: CPU fallback active — vello → wgpu readback → RGBA bytes; \
                 zero-copy DMA-BUF interop is the Phase 5.3 follow-up. \
                 Expect ~10ms+ per frame for a 1080p output."
            );
        }

        let (width, height) = self.target.size;
        let padded_bytes_per_row = self.padded_bytes_per_row;
        let mut timing = self.phase_timing_enabled.then(BridgePhaseTiming::default);
        self.reset_render_attempts();
        let (mapped, _) = match self.render_normal(scene, None, timing.as_mut()) {
            Ok(result) => result,
            Err(error) => {
                self.note_render_failure(&error);
                return Err(error);
            }
        };

        // Block-map the readback buffer and copy into the tight `self.readback`, stripping row padding.
        let tight_row = (width as usize).saturating_mul(4);
        let needed = tight_row.saturating_mul(height as usize);
        let cpu_copy_started = self.phase_timing_enabled.then(Instant::now);
        self.readback.clear();
        self.readback.reserve(needed);
        for row in 0..height as usize {
            let start = row.saturating_mul(padded_bytes_per_row as usize);
            let end = start.saturating_add(tight_row);
            let Some(src_row) = mapped.get(start..end) else {
                tracing::warn!(
                    "DRM paint: readback row {row} out of the mapped buffer's bounds; truncating frame"
                );
                break;
            };
            self.readback.extend_from_slice(src_row);
        }
        drop(mapped);
        self.readback_buf.unmap();

        if let (Some(timing), Some(started)) = (timing.as_mut(), cpu_copy_started) {
            timing.cpu_copy_swizzle_us = started.elapsed().as_micros();
            timing.cpu_output_bytes = u64::try_from(self.readback.len()).unwrap_or(u64::MAX);
        }
        self.last_phase_timing = timing;

        if self.readback.len() == needed {
            self.mark_retained_complete(None);
        }

        Ok(&self.readback)
    }

    /// [`paint`](Self::paint) for a frame with HDR layers: the tight RGBA render with every
    /// layer image black, and the render with each in its channel colour, for
    /// [`crate::shell::hdr_surface::composite_hdr_pixel`].
    ///
    /// # Errors
    /// Fails like [`paint`](Self::paint), or when the second render cannot be read back.
    pub fn paint_with_hdr_layers(
        &mut self,
        scene: &Scene,
        hdr_layers: &[HdrLayer],
    ) -> Result<(&[u8], Vec<u8>), BridgeError> {
        {
            let (device, queue) = (&self.target.device, &self.target.queue);
            let compositor = self
                .hdr_compositor
                .get_or_insert_with(|| HdrCompositor::new(device, queue));
            self.target.hdr_pass = Some(compositor.prepare(self.target.size, hdr_layers, None));
        }
        let painted = self.paint(scene).map(|_| ());
        self.target.hdr_pass = None;
        painted?;
        let second = self
            .hdr_compositor
            .as_ref()
            .ok_or_else(|| BridgeError::Render("HDR compositor missing".to_owned()))?
            .read_second(LINUX_FENCE_WAIT)
            .map_err(BridgeError::Render)?;
        Ok((&self.readback, second))
    }

    /// Render `scene` and write it **directly** into an XRGB8888 destination (little-endian
    /// `[B, G, R, X]`), fusing the wgpu padding-strip and the RGBA→XRGB swizzle into one pass.
    ///
    /// [`paint`](Self::paint) exposes tight RGBA bytes; this method writes the mapped readback as
    /// XRGB into the caller's destination, which is normally a DRM dumb-buffer mmap.
    ///
    /// `dst_pitch` may exceed `width * 4` for hardware pitch alignment; rows/columns are clamped to what `dst` can hold, so a short destination is logged-and-truncated rather than panicking.
    ///
    /// # Errors
    /// Returns a [`BridgeError`] under the same conditions as [`paint`](Self::paint).
    pub fn paint_into_xrgb8888(
        &mut self,
        scene: &Scene,
        dst: &mut [u8],
        dst_pitch: usize,
    ) -> Result<(), BridgeError> {
        self.last_phase_timing = None;
        self.check_recovery_backoff()?;
        self.invalidate_retained();
        if !WARNED_CPU_FALLBACK.swap(true, Ordering::AcqRel) {
            tracing::warn!(
                "DRM paint: CPU fallback active — vello → wgpu readback → XRGB8888 \
                 (direct mapped RGBA→XRGB swizzle); \
                 zero-copy DMA-BUF interop remains a follow-up. \
                 Expect ~several ms per frame for a 1080p output."
            );
        }

        let (width, height) = self.target.size;
        let padded_bytes_per_row = self.padded_bytes_per_row;
        let mut timing = self.phase_timing_enabled.then(BridgePhaseTiming::default);
        self.reset_render_attempts();
        let (mapped, _) = match self.render_normal(scene, None, timing.as_mut()) {
            Ok(result) => result,
            Err(error) => {
                self.note_render_failure(&error);
                return Err(error);
            }
        };

        let cpu_copy_started = self.phase_timing_enabled.then(Instant::now);
        if dst_pitch == 0 {
            drop(mapped);
            self.readback_buf.unmap();
            if let (Some(timing), Some(started)) = (timing.as_mut(), cpu_copy_started) {
                timing.cpu_copy_swizzle_us = started.elapsed().as_micros();
            }
            self.last_phase_timing = timing;
            self.mark_retained_complete(None);
            return Ok(());
        }
        let cpu_output_bytes = Self::swizzle_mapped_xrgb8888(
            MappedXrgbSource {
                bytes: &mapped,
                padded_bytes_per_row: padded_bytes_per_row as usize,
                width: width as usize,
                height: height as usize,
            },
            dst,
            dst_pitch,
            self.phase_timing_enabled,
        );
        drop(mapped);
        self.readback_buf.unmap();
        if let (Some(timing), Some(started)) = (timing.as_mut(), cpu_copy_started) {
            timing.cpu_copy_swizzle_us = started.elapsed().as_micros();
            timing.cpu_output_bytes = cpu_output_bytes;
        }
        self.last_phase_timing = timing;
        self.mark_retained_complete(None);
        Ok(())
    }

    #[inline]
    pub(super) fn invalidate_retained(&mut self) {
        self.target.retained_target_valid = false;
        self.retained_staging_valid = false;
        self.staging_content = None;
        self.target.last_successful_revision = None;
        self.target.owed_damage.clear();
    }

    #[inline]
    pub(super) fn mark_retained_complete(&mut self, revision: Option<u64>) {
        self.target.retained_target_valid = true;
        self.target.finished_frames = self.target.finished_frames.saturating_add(1);
        self.retained_staging_valid = true;
        self.staging_content = Some((self.target.geometry_generation, revision));
        self.target.last_successful_revision = revision;
    }

    #[inline]
    pub(super) fn reset_render_attempts(&mut self) {
        self.target.reset_attempts();
    }

    pub(super) fn render_normal(
        &mut self,
        scene: &Scene,
        damage: Option<SceneDamage<'_>>,
        timing: Option<&mut BridgePhaseTiming>,
    ) -> Result<(wgpu::BufferView, bool), BridgeError> {
        if self.context.devices.get(self.dev_id).is_none() {
            self.invalidate_retained();
            return Err(BridgeError::InvalidDevice(format!(
                "wgpu device id {} out of range",
                self.dev_id
            )));
        }
        self.take_device_error();
        if self.target.poisoned {
            let (width, height) = self.target.size;
            self.reconstruct_target(width, height)?;
        }
        let partial = self.retained_staging_valid
            && self.staging_content
                == Some((
                    self.target.geometry_generation,
                    self.target.last_successful_revision,
                ));
        let result = self.target.linux_frame_policy().and_then(|policy| {
            self.target.render(
                scene,
                damage,
                FrameOutput::Xrgb {
                    buffer: &self.readback_buf,
                    stride: self.padded_bytes_per_row,
                    partial,
                },
                policy,
                timing,
                |_, _, _| {},
            )
        });
        // The bridge publishes only after CPU output succeeds as well. A swizzle error remains
        // its deliberately stricter invalidate-both boundary, independent of GPU completion.
        self.invalidate_retained();
        let (mapped, admitted) = result?;
        if let Some(error) = self.take_device_error() {
            drop(mapped);
            self.readback_buf.unmap();
            return Err(error);
        }
        mapped
            .map(|mapped| (mapped, admitted))
            .ok_or_else(|| BridgeError::Map("XRGB output missing mapped framebuffer".to_owned()))
    }

    /// Take an uncaptured wgpu error raised since the last check, poisoning the target so the
    /// next render rebuilds it.
    fn take_device_error(&mut self) -> Option<BridgeError> {
        let error = self.device_errors.take()?;
        tracing::error!(%error, "DRM paint: wgpu reported an error; rebuilding the renderer");
        self.target.poisoned = true;
        Some(BridgeError::Render(format!("wgpu: {error}")))
    }

    /// Replace the target with a fresh renderer at `width` x `height` on the same device,
    /// keeping its clear colour. This is the one way out of a poisoned target.
    pub(super) fn reconstruct_target(
        &mut self,
        width: u32,
        height: u32,
    ) -> Result<(), BridgeError> {
        let color = self.target.base_color;
        let mut target = RetainedGpuTarget::new(
            self.context.devices.get(self.dev_id).ok_or_else(|| {
                BridgeError::InvalidDevice(
                    "renderer reconstruction lost its device handle".to_owned(),
                )
            })?,
            width,
            height,
        )?;
        target.set_base_color(color);
        target.present_generation = self.target.present_generation;
        self.target = target;
        self.invalidate_output_surfaces();
        (self.readback_buf, self.padded_bytes_per_row) =
            Self::allocate_staging(&self.target.device, width, height);
        self.readback.clear();
        self.xrgb_history_generation = Self::next_xrgb_history_generation();
        self.bump_xrgb_history_epoch();
        self.invalidate_retained();
        Ok(())
    }

    /// Append one complete readback copy for a shared-coarse diagnostic output. Diagnostics use
    /// full staging images so no unwritten row/pitch bytes can influence their comparison.
    pub(super) fn append_full_copy(
        encoder: &mut wgpu::CommandEncoder,
        texture: &wgpu::Texture,
        readback_buf: &wgpu::Buffer,
        padded_bytes_per_row: u32,
        width: u32,
        height: u32,
    ) {
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: readback_buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
    }

    pub(super) fn record_sparse_shader_selected_attempts(
        stats: &vello::SparseRenderStats,
        canonical_segment_order_sparse_attempts: Option<&mut usize>,
        area_ppt8_sparse_attempts: Option<&mut usize>,
        clip_single_group_sparse_attempts: Option<&mut usize>,
    ) {
        if stats.canonical_segment_order_shader_selected
            && let Some(selected_attempts) = canonical_segment_order_sparse_attempts
        {
            *selected_attempts = selected_attempts.saturating_add(1);
        }
        if stats.area_ppt8_shader_selected
            && let Some(selected_attempts) = area_ppt8_sparse_attempts
        {
            *selected_attempts = selected_attempts.saturating_add(1);
        }
        if stats.clip_single_group_shader_selected
            && let Some(selected_attempts) = clip_single_group_sparse_attempts
        {
            *selected_attempts = selected_attempts.saturating_add(1);
        }
    }

    pub(super) fn sparse_damage_readback_bytes(damage: &[vello::FineDamageRect]) -> u64 {
        damage.iter().fold(0_u64, |bytes, rect| {
            bytes.saturating_add(
                u64::from(rect.width)
                    .saturating_mul(u64::from(rect.height))
                    .saturating_mul(4),
            )
        })
    }

    /// Map the staged `readback_buf` for CPU reads, blocking until the GPU copy completes. The
    /// returned view owns a buffer handle; the caller must drop it before `readback_buf.unmap()`.
    pub(super) fn map_readback(
        context: &RenderContext,
        dev_id: usize,
        readback_buf: &wgpu::Buffer,
    ) -> Result<wgpu::BufferView, BridgeError> {
        let dev_handle = context.devices.get(dev_id).ok_or_else(|| {
            BridgeError::InvalidDevice(format!("wgpu device id {dev_id} out of range"))
        })?;
        let device = &dev_handle.device;
        Self::map_device_readback(device, readback_buf)
    }

    pub(super) fn map_device_readback(
        device: &wgpu::Device,
        readback_buf: &wgpu::Buffer,
    ) -> Result<wgpu::BufferView, BridgeError> {
        let slice = readback_buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            tx.send(r).ok();
        });
        // Bounded, not indefinite: a fence in flight across a system suspend/resume can never signal,
        // and an unbounded wait would wedge the paint path forever — the same freeze the winit
        // presenter's post-present poll had (see `backend/winit.rs`). A timeout drops this frame through the existing error path; the next one retries.
        let waited = device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(std::time::Duration::from_secs(1)),
        });
        // Timeouts are typed the same as on the frame path, so a caller that feeds them to the
        // recovery back-off backs off for the same reasons.
        if let Err(e) = waited {
            // Cancels an in-flight map request (or releases a map that raced to completion) so
            // the next production frame can retry instead of seeing this buffer as pending.
            readback_buf.unmap();
            return Err(match e {
                wgpu::PollError::Timeout => BridgeError::CompletionTimeout {
                    phase: CompletionPhase::Fence,
                },
                other @ wgpu::PollError::WrongSubmissionIndex(..) => {
                    BridgeError::Map(format!("fence wait: {other:?}"))
                }
            });
        }
        match rx.recv_timeout(std::time::Duration::from_secs(1)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                readback_buf.unmap();
                return Err(BridgeError::Map(format!("{e:?}")));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                readback_buf.unmap();
                return Err(BridgeError::CompletionTimeout {
                    phase: CompletionPhase::MapCallback,
                });
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                readback_buf.unmap();
                return Err(BridgeError::Map(
                    "map_async callback dropped without reporting".to_owned(),
                ));
            }
        }
        Ok(slice.get_mapped_range())
    }

    /// Map only a capture's logical range: download buffers come from a size-class pool and can
    /// be physically larger than the submitted PTCL or segment resource.
    pub(super) fn map_buffer_range(
        context: &RenderContext,
        dev_id: usize,
        buffer: &wgpu::Buffer,
        logical_bytes: u64,
    ) -> Result<wgpu::BufferView, BridgeError> {
        let dev_handle = context.devices.get(dev_id).ok_or_else(|| {
            BridgeError::InvalidDevice(format!("wgpu device id {dev_id} out of range"))
        })?;
        let device = &dev_handle.device;
        let slice = buffer.slice(..logical_bytes);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            tx.send(result).ok();
        });
        let waited = device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(std::time::Duration::from_secs(1)),
        });
        if let Err(error) = waited {
            buffer.unmap();
            return Err(BridgeError::Map(format!("fence wait: {error:?}")));
        }
        match rx.recv_timeout(std::time::Duration::from_secs(1)) {
            Ok(Ok(())) => Ok(slice.get_mapped_range()),
            Ok(Err(error)) => {
                buffer.unmap();
                Err(BridgeError::Map(format!("{error:?}")))
            }
            Err(error) => {
                buffer.unmap();
                Err(BridgeError::Map(format!("map_async channel: {error}")))
            }
        }
    }

    /// Resize the offscreen target, reallocating texture + readback buffer; returns `Ok(())` even when the new size matches the current (no-op).
    ///
    /// # Errors
    /// Returns [`BridgeError::InvalidSize`] if `width` or `height` is `0`, or
    /// [`BridgeError::InvalidDevice`] if the bridge's device id is out of range (defensive;
    /// cannot happen in practice).
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), BridgeError> {
        // Treat even a rejected/no-op resize request as a sparse boundary. This keeps a caller
        // that changed its output model but failed to complete reallocation from reusing an
        // ambiguous target on the next frame.
        self.invalidate_retained();
        self.invalidate_output_surfaces();
        self.bump_xrgb_history_epoch();
        // A poisoned target refuses to resize; rebuilding it at the new size is what a failed
        // frame followed by a display change needs. Refusing instead left the presenter asking
        // again every frame and presenting nothing.
        self.take_device_error();
        if self.target.poisoned {
            return self.reconstruct_target(width, height);
        }
        let old_size = self.target.size;
        self.target.resize(width, height)?;
        if old_size == self.target.size {
            return Ok(());
        }
        (self.readback_buf, self.padded_bytes_per_row) =
            Self::allocate_staging(&self.target.device, width, height);
        // Reset the tight buffer for the next `paint`; the new size may be larger, so clearing is cleaner than keeping capacity.
        self.readback.clear();
        Ok(())
    }

    /// Why this bridge's device was lost, once it has been: wgpu reported it, or work the GPU
    /// was given stopped finishing (`ui::gpu::LossWatch`). Asked before every frame, so work
    /// still in flight then is what the next ask finds finished or not; this never waits for
    /// the GPU. Nothing recovers a lost device but a new bridge ([`super::retire_lost_bridge`]).
    pub fn device_lost(&mut self) -> Option<String> {
        #[cfg(target_os = "linux")]
        self.apply_injected_fault();
        let lost = self.loss_watch.lost(&self.target.device);
        if lost.is_none() {
            self.loss_watch.unfinished(&self.target.queue);
        }
        lost
    }

    /// Suffer the fault a development session's test control injected, if any
    /// ([`super::InjectedGpuFault`]), on this bridge's own device.
    #[cfg(target_os = "linux")]
    fn apply_injected_fault(&self) {
        if let Some(fault) = super::injected_gpu_fault(ui::gpu::software_fallback().is_some()) {
            apply_injected_fault(&self.target.device, fault);
        }
    }

    /// Whether this bridge's device finished at least one frame.
    pub fn finished_frames(&self) -> bool {
        self.target.finished_frames > 0
    }

    /// The adapter this bridge renders on.
    pub fn adapter(&self) -> &wgpu::AdapterInfo {
        &self.adapter
    }

    /// Current (width, height); useful in tests and for comparing against the connector's mode size before resizing.
    pub fn size(&self) -> (u32, u32) {
        self.target.size
    }

    /// Vello submissions used by the latest paint, including any sparse attempt and bounded
    /// dense bump-budget recovery retries.
    pub fn last_render_attempts(&self) -> usize {
        self.target.last_render_attempts
    }

    /// Sparse Vello submissions used by the latest paint. This remains zero for every legacy
    /// full-render API and for conservative versioned fallbacks.
    pub fn last_sparse_render_attempts(&self) -> usize {
        self.target.last_sparse_render_attempts
    }

    /// Forget every retained frame, as a new bridge has none: the next versioned paint renders
    /// dense. Lets a test reuse one bridge (and its compiled pipelines) for scenes that share
    /// no history.
    #[cfg(test)]
    pub(crate) fn forget_retained_for_test(&mut self) {
        self.invalidate_retained();
    }

    /// The image atlas and the CPU pixels its residents keep alive.
    pub fn image_cache_stats(&self) -> vello::ImageCacheStats {
        self.target.vello().image_cache_stats()
    }

    /// GPU allocations the renderer accounts for, the image atlas included.
    pub fn renderer_memory_stats(&self) -> vello::RendererMemoryStats {
        self.target.vello().memory_stats()
    }

    /// After each frame. When a window closed since the last
    /// frame, the image atlas comes down within the next few frames rather
    /// than after sixty: a desktop that goes idle after a close draws too
    /// few to bring it down the slow way, and kept up to 256 MiB of atlas.
    /// When the atlas did come down, so did the scene: the buffers pooled for
    /// the larger one go too (the next frame pools what it needs), and a trim
    /// gives the memory back to the kernel. The renderer's scratch (pooled
    /// buffers and bump reservation, sized for the busiest scene it drew)
    /// comes down the same way, within a few frames of a close; when the
    /// reservation shrank, a trim gives that back too. Cheap enough for
    /// every frame.
    pub fn after_paint(&mut self) {
        let closed = crate::shell::heap_trim::windows_closed();
        if closed != self.windows_closed_seen {
            self.windows_closed_seen = closed;
            self.target.renderer.shrink_image_atlas_soon();
            self.target.renderer.shrink_scratch_soon();
        }
        let atlas_before = self.logged_memory.map(|mark| mark[0]);
        let atlas_now = u64::from(self.target.vello().image_cache_stats().atlas_side);
        let shrinks = self.target.vello().scratch_shrinks();
        let scratch_shrank = shrinks != self.scratch_shrinks_seen;
        self.scratch_shrinks_seen = shrinks;
        if atlas_before.is_some_and(|was| atlas_now < was) {
            self.target.renderer.trim_pooled_buffers();
            crate::shell::heap_trim::request();
        } else if scratch_shrank {
            crate::shell::heap_trim::request();
        }
        self.log_memory_changes();
    }

    /// Warm the layers the target composites a lone output's strip pans
    /// from, at rest ([`RetainedGpuTarget::warm_layers`]): the pixels
    /// rasterised, `None` when the target cannot present these layers.
    ///
    /// # Errors
    /// As [`RetainedGpuTarget::warm_layers`], or a device error it left.
    pub fn warm_layers(
        &mut self,
        cache: &crate::shell::frame::FrameCache,
        deadline: Instant,
        keep_going: &dyn Fn() -> bool,
    ) -> Result<Option<u64>, BridgeError> {
        if let Some(error) = self.take_device_error() {
            return Err(error);
        }
        let warmed = self.target.warm_layers(cache, deadline, keep_going);
        let warmed = warmed.and_then(|pixels| self.take_device_error().map_or(Ok(pixels), Err));
        self.log_memory_changes();
        warmed
    }

    /// Log one `RENDERER-MEMORY` line when the image atlas changes size or
    /// the renderer's GPU allocations move by 8 MiB or more since the last
    /// line, so the session log tells which of them a desktop with no window
    /// left still holds.
    fn log_memory_changes(&mut self) {
        const STEP: u64 = 8 << 20;
        let memory = self.target.vello().memory_stats();
        let images = self.target.vello().image_cache_stats();
        let mark = [
            u64::from(images.atlas_side),
            memory.image_bytes,
            memory.pooled_buffer_bytes,
            memory.live_buffer_bytes,
            memory.bump_budget_bytes,
            memory.sparse_upload_bytes,
        ];
        let moved = self.logged_memory.is_none_or(|last| {
            last[0] != mark[0]
                || last
                    .iter()
                    .zip(mark)
                    .skip(1)
                    .any(|(was, now)| was.abs_diff(now) >= STEP)
        });
        if !moved {
            return;
        }
        self.logged_memory = Some(mark);
        let mib = |bytes: u64| bytes / (1 << 20);
        // A measurement line for the session log, like WORKSPACE-SWITCH.
        eprintln!(
            "RENDERER-MEMORY atlas_side={} image_mib={} pooled_mib={} live_mib={} bump_mib={} \
             sparse_upload_mib={} resident_images={}",
            images.atlas_side,
            mib(memory.image_bytes),
            mib(memory.pooled_buffer_bytes),
            mib(memory.live_buffer_bytes),
            mib(memory.bump_budget_bytes),
            mib(memory.sparse_upload_bytes),
            images.resident,
        );
    }

    /// Dense Vello submissions used by the latest paint, including bump-budget retries.
    pub fn last_full_render_attempts(&self) -> usize {
        self.target.last_full_render_attempts
    }

    /// Full render attempts from the latest paint which actually recorded canonical segment
    /// ordering. A selected pass is not evidence of completion or performance improvement.
    #[doc(hidden)]
    pub fn last_canonical_segment_order_full_attempts(&self) -> usize {
        self.target.last_canonical_segment_order_full_attempts
    }

    /// Sparse render attempts from the latest paint which actually recorded canonical segment
    /// ordering, including an overflowed sparse attempt before dense recovery.
    #[doc(hidden)]
    pub fn last_canonical_segment_order_sparse_attempts(&self) -> usize {
        self.target.last_canonical_segment_order_sparse_attempts
    }

    /// Full render attempts from the latest paint that actually selected Area PPT8.
    #[doc(hidden)]
    pub fn last_area_ppt8_full_attempts(&self) -> usize {
        self.target.last_area_ppt8_full_attempts
    }

    /// Sparse render attempts from the latest paint that actually selected Area PPT8.
    #[doc(hidden)]
    pub fn last_area_ppt8_sparse_attempts(&self) -> usize {
        self.target.last_area_ppt8_sparse_attempts
    }

    /// Full render attempts from the latest paint that selected single-workgroup clip leaf.
    #[doc(hidden)]
    pub fn last_clip_single_group_full_attempts(&self) -> usize {
        self.target.last_clip_single_group_full_attempts
    }

    /// Sparse render attempts from the latest paint that selected single-workgroup clip leaf.
    #[doc(hidden)]
    pub fn last_clip_single_group_sparse_attempts(&self) -> usize {
        self.target.last_clip_single_group_sparse_attempts
    }

    /// Actual sparse coarse-dispatch selection from the latest paint, if sparse submission was
    /// reached. This is present even when a sparse overflow forced a later dense recovery.
    pub fn last_sparse_render_stats(&self) -> Option<vello::SparseRenderStats> {
        self.target.last_sparse_render_stats
    }

    /// Whether path-level sparse tile bounds were requested for this renderer.
    #[doc(hidden)]
    pub fn sparse_tile_bounds_requested(&self) -> bool {
        self.target.renderer.sparse_tile_bounds_requested()
    }

    /// Whether this renderer can apply path-level sparse tile bounds.
    #[doc(hidden)]
    pub fn sparse_tile_bounds_available(&self) -> bool {
        self.target.renderer.sparse_tile_bounds_available()
    }

    /// Whether sparse Area fine quad skipping was requested for this renderer.
    #[doc(hidden)]
    pub fn sparse_fine_quad_skip_requested(&self) -> bool {
        self.target.renderer.sparse_fine_quad_skip_requested()
    }

    /// Whether sparse Area fine quad skipping can be selected for this renderer.
    #[doc(hidden)]
    pub fn sparse_fine_quad_skip_available(&self) -> bool {
        self.target.renderer.sparse_fine_quad_skip_available()
    }

    /// Whether sparse coarse opaque pruning was requested for this renderer.
    #[doc(hidden)]
    pub fn sparse_opaque_prune_requested(&self) -> bool {
        self.target.renderer.sparse_opaque_prune_requested()
    }

    /// Whether sparse coarse opaque pruning can select its shader for this renderer.
    #[doc(hidden)]
    pub fn sparse_opaque_prune_available(&self) -> bool {
        self.target.renderer.sparse_opaque_prune_available()
    }

    /// Whether sparse solid-fuse selection was requested for this renderer.
    #[doc(hidden)]
    pub fn sparse_solid_fuse_requested(&self) -> bool {
        self.target.renderer.sparse_solid_fuse_requested()
    }

    /// Whether sparse solid-fuse selection can be used by this renderer.
    #[doc(hidden)]
    pub fn sparse_solid_fuse_available(&self) -> bool {
        self.target.renderer.sparse_solid_fuse_available()
    }

    /// Whether sparse segment-row masking was requested for this renderer.
    #[doc(hidden)]
    pub fn sparse_segment_row_mask_requested(&self) -> bool {
        self.target.renderer.sparse_segment_row_mask_requested()
    }

    /// Whether sparse segment-row masking can be selected by this renderer.
    #[doc(hidden)]
    pub fn sparse_segment_row_mask_available(&self) -> bool {
        self.target.renderer.sparse_segment_row_mask_available()
    }

    /// Render with a fixed per-tile segment order, so repeated paints of one
    /// scene are bit-identical. For exact-pixel oracles.
    ///
    /// # Errors
    /// Returns [`BridgeError::Renderer`] if the shader set fails to compile.
    #[doc(hidden)]
    pub fn enable_canonical_segment_order(&mut self) -> Result<(), BridgeError> {
        self.invalidate_retained();
        let RetainedGpuTarget {
            device, renderer, ..
        } = &mut self.target;
        renderer
            .enable_canonical_segment_order(device)
            .map_err(|e| BridgeError::Renderer(format!("{e:?}")))
    }

    /// Whether canonical segment ordering was requested for this renderer.
    #[doc(hidden)]
    pub fn canonical_segment_order_requested(&self) -> bool {
        self.target.renderer.canonical_segment_order_requested()
    }

    /// Whether this renderer has the canonical segment-ordering pass available.
    #[doc(hidden)]
    pub fn canonical_segment_order_available(&self) -> bool {
        self.target.renderer.canonical_segment_order_available()
    }

    /// Whether Area PPT8 was requested for this renderer.
    #[doc(hidden)]
    pub fn area_ppt8_requested(&self) -> bool {
        self.target.renderer.area_ppt8_requested()
    }

    /// Whether this renderer has the ordinary Area PPT8 full shader available.
    #[doc(hidden)]
    pub fn area_ppt8_available(&self) -> bool {
        self.target.renderer.area_ppt8_available()
    }

    /// Whether single-workgroup clip leaf was requested for this renderer.
    #[doc(hidden)]
    pub fn clip_single_group_requested(&self) -> bool {
        self.target.renderer.clip_single_group_requested()
    }

    /// Whether the single-workgroup clip leaf permutation is available.
    #[doc(hidden)]
    pub fn clip_single_group_available(&self) -> bool {
        self.target.renderer.clip_single_group_available()
    }

    /// Allocate (texture, view, `readback_buf`, `padded_bytes_per_row`) for the given size; shared by `new` and `resize`.
    pub(super) fn allocate_targets(
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> (wgpu::Texture, wgpu::TextureView, wgpu::Buffer, u32) {
        let (texture, view) = RetainedGpuTarget::allocate_texture(device, width, height);
        let (readback_buf, padded_bytes_per_row) = Self::allocate_staging(device, width, height);
        (texture, view, readback_buf, padded_bytes_per_row)
    }

    pub(super) fn allocate_staging(
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> (wgpu::Buffer, u32) {
        let bytes_per_pixel = 4u32;
        let unpadded = width.saturating_mul(bytes_per_pixel);
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = unpadded.div_ceil(align).saturating_mul(align);
        let readback_size = u64::from(padded_bytes_per_row).saturating_mul(u64::from(height));
        let readback_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tilo-desktop-drm-paint-readback"),
            size: readback_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        (readback_buf, padded_bytes_per_row)
    }
}

/// Have `device` suffer `fault` through the vendored HAL's switches
/// (`vendor/wgpu-hal/TILO-PATCH.md`).
#[cfg(target_os = "linux")]
fn apply_injected_fault(device: &wgpu::Device, fault: super::InjectedGpuFault) {
    // SAFETY: the hal device is only flagged; no raw handle leaves this block.
    let Some(hal) = (unsafe { device.as_hal::<wgpu::hal::api::Vulkan>() }) else {
        tracing::warn!(?fault, "an injected GPU fault needs a Vulkan device");
        return;
    };
    tracing::warn!(
        ?fault,
        "applying an injected GPU fault to the renderer's device"
    );
    match fault {
        super::InjectedGpuFault::Reset | super::InjectedGpuFault::Dead => {
            hal.simulate_device_loss();
        }
        super::InjectedGpuFault::Hang => hal.simulate_hang(),
    }
}
