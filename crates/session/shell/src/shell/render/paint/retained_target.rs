//! The retained Vello target: its construction, layered and whole-scene renders, and the
//! completion waits every submission goes through.

use std::time::{Duration, Instant};

use vello::peniko::Color;
use vello::{AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, Scene, wgpu};

#[cfg(test)]
use super::WaitPhase;
use super::{
    BUMP_SETTLEMENT_WAIT, BridgeAdapterInfo, BridgeError, BridgePhaseTiming,
    COUNTER_SETTLEMENT_ROUNDS, CompletionPhase, CompletionPolicy, FrameOutput, GpuFrameStats,
    LINUX_FENCE_WAIT, LINUX_FRAME_BUDGET, LINUX_MAP_CALLBACK_WAIT, MAX_VELLO_BINS, PIPELINE_WARMUP,
    RESILIENT_RENDER_ATTEMPTS, RetainedGpuTarget, SceneDamage, VELLO_BIN_PIXELS, VelloDrmBridge,
};
use crate::shell::render::{strip_layers, warmup};

/// Whether this process has published its pipelines for apps (see [`ui::pipeline_seed`]).
static PIPELINE_SEED_PUBLISHED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// An empty pipeline cache for the first renderer this session builds, which then becomes the
/// seed every app starts from. Later renderers compile exactly the same pipelines, so one seed
/// per session is all there is to publish. Never in unit tests: they must not write into the
/// real session's runtime directory.
fn pipeline_seed_to_publish(gpu: &ui::gpu::DeviceHandle) -> Option<wgpu::PipelineCache> {
    if cfg!(test) || PIPELINE_SEED_PUBLISHED.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return None;
    }
    ui::pipeline_seed::empty(gpu)
}

/// Replace the seed for `gpu` under `dir` atomically, readable by every app and writable by the
/// session only. `Ok(false)` when the backend has no cache data to share.
fn write_pipeline_seed(
    gpu: &ui::gpu::DeviceHandle,
    cache: &wgpu::PipelineCache,
    dir: &std::path::Path,
) -> std::io::Result<bool> {
    let Some(file) = ui::pipeline_seed::seed_file(dir, &gpu.adapter.get_info()) else {
        return Ok(false);
    };
    let Some(data) = cache.get_data() else {
        return Ok(false);
    };
    std::fs::create_dir_all(dir)?;
    let staging = file.with_extension("tmp");
    std::fs::write(&staging, &data)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o644))?;
    }
    std::fs::rename(&staging, &file)?;
    Ok(true)
}

/// Write the session's pipelines where the launcher binds them, read-only, into every app.
fn publish_pipeline_seed(gpu: &ui::gpu::DeviceHandle, cache: &wgpu::PipelineCache) {
    let dir = crate::shell::launcher::runtime_dir()
        .join("tilo")
        .join(launcher::gpu::PIPELINE_SEED_DIR_NAME);
    match write_pipeline_seed(gpu, cache, &dir) {
        Ok(true) => tracing::info!(
            target: "shell::render",
            dir = %dir.display(),
            "published the session's vello pipelines for apps to start from"
        ),
        Ok(false) => {}
        Err(error) => tracing::warn!(
            target: "shell::render",
            dir = %dir.display(),
            %error,
            "could not publish the session's vello pipelines; apps compile their own"
        ),
    }
}

impl RetainedGpuTarget {
    /// Construct an offscreen target on the supplied context-owned device and queue.
    ///
    /// # Errors
    /// Returns a size or renderer error if construction cannot complete.
    pub fn new(gpu: &ui::gpu::DeviceHandle, width: u32, height: u32) -> Result<Self, BridgeError> {
        Self::validate_size(&gpu.device, width, height)?;
        if let Some(reason) = gpu.loss_monitor().reason() {
            return Err(BridgeError::DeviceLost(reason));
        }
        let (texture, view) = Self::allocate_texture(&gpu.device, width, height);
        #[cfg(target_os = "linux")]
        crate::shader_seed::observe_device(gpu.adapter.get_info());
        let seed = pipeline_seed_to_publish(gpu);
        let mut renderer = Renderer::new(
            &gpu.device,
            RendererOptions {
                use_cpu: false,
                // Every frame renders with area anti-aliasing; MSAA pipelines would go unused.
                antialiasing_support: AaSupport::area_only(),
                num_init_threads: None,
                pipeline_cache: seed.clone(),
            },
        )
        .map_err(|e| BridgeError::Renderer(format!("{e:?}")))?;
        // Atomic tile allocation otherwise sums Area coverage in GPU scheduling order.
        // The same unchanged desktop can then round to different pixels in dense and
        // retained renders. Keep their segment order stable on the adaptive shell path;
        // Vello still preserves its pinned-budget and overflow-validation guards.
        renderer
            .enable_canonical_segment_order(&gpu.device)
            .map_err(|e| BridgeError::Renderer(format!("{e:?}")))?;
        // Once the desktop got lighter (windows closed) and stayed so, the
        // scratch its busiest scene grew comes back: the pool drops buffers
        // no frame takes and the bump reservation comes down, with headroom.
        renderer.set_shrink_scratch(true);
        if let Some(seed) = seed {
            publish_pipeline_seed(gpu, &seed);
        }
        let mut target = Self {
            device: gpu.device.clone(),
            queue: gpu.queue.clone(),
            renderer,
            client_frames: crate::shell::gpu_frames::AppliedFrames::default(),
            present_generation: None,

            texture,
            view,
            band: None,
            loss: gpu.loss_monitor(),
            poisoned: false,
            geometry_generation: 1,
            completion_markers: 0,
            #[cfg(test)]
            completion_test: None,
            #[cfg(test)]
            phase_wait_override: None,
            size: (width, height),
            last_frame_full: false,
            base_color: Color::from_rgb8(0x1A, 0x1B, 0x26),
            retained_target_valid: false,
            last_successful_revision: None,
            owed_damage: Vec::new(),
            last_render_attempts: 0,
            last_sparse_render_attempts: 0,
            last_full_render_attempts: 0,
            last_canonical_segment_order_full_attempts: 0,
            last_canonical_segment_order_sparse_attempts: 0,
            hdr_pass: None,
            backdrop: None,
            backdrop_blur: None,
            last_area_ppt8_full_attempts: 0,
            last_area_ppt8_sparse_attempts: 0,
            last_clip_single_group_full_attempts: 0,
            last_clip_single_group_sparse_attempts: 0,
            last_sparse_render_stats: None,
            strip_layers: None,
            last_layer_work: None,
            staged_copies: None,
            raster_override: None,
            copy_scratch: ui::scroll_copy::CopyScratch::default(),
            last_scroll_copy_work: None,
        };
        if gpu.adapter.get_info().device_type == wgpu::DeviceType::Cpu {
            // Lavapipe compiles each pipeline on its first dispatch: on a cold shader cache,
            // seconds of one-at-a-time work the warm-up below would add to the first frame.
            // Extra devices come from the session's shared budget; without any, this device
            // compiles in place.
            let slots = warmup::acquire();
            target.renderer.precompile_pipelines(
                &target.device,
                &target.queue,
                slots.len(),
                &|| gpu.open_peer(),
            );
            drop(slots);
        }
        target.warm_pipelines()?;
        Ok(target)
    }

    /// Run the full and the sparse render paths once so every frame deadline measures rendering
    /// alone. Mesa's CPU Vulkan driver (lavapipe, the DRM CPU-fallback adapter) compiles each
    /// compute pipeline on its first dispatch, not at creation. With a cold shader cache (first
    /// boot, or a fresh CI home) that compilation ran inside the first frame's one-second fence
    /// allowance and failed it, and the first sparse frame paid it again.
    ///
    /// # Errors
    /// Fails when the device cannot complete the warm-up frames within [`PIPELINE_WARMUP`].
    fn warm_pipelines(&mut self) -> Result<(), BridgeError> {
        use vello::kurbo::{Affine, Rect};
        use vello::peniko::{BlendMode, Fill};

        let (width, height) = self.size;
        let full = Rect::new(0.0, 0.0, f64::from(width), f64::from(height));
        let inset = full.inset(-f64::from(width.min(height)) / 4.0);
        // The picture is never shown, so it reuses the base colour rather than stating its own.
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            self.base_color,
            None,
            &full,
        );
        // A clip layer adds the clip reduce/leaf stages ordinary shell frames use.
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            Affine::IDENTITY,
            &inset,
        );
        scene.fill(
            Fill::EvenOdd,
            Affine::IDENTITY,
            self.base_color.with_alpha(0.5),
            None,
            &full,
        );
        scene.pop_layer();
        let damage = [vello::FineDamageRect {
            x: 0,
            y: 0,
            width: width.min(16),
            height: height.min(16),
        }];
        let deadline = Instant::now()
            .checked_add(PIPELINE_WARMUP)
            .ok_or_else(|| BridgeError::Renderer("warm-up deadline out of range".to_owned()))?;
        for damage in [
            SceneDamage {
                revision: 1,
                previous_revision: None,
                regions: None,
            },
            SceneDamage {
                revision: 2,
                previous_revision: Some(1),
                regions: Some(&damage),
            },
        ] {
            self.render_until(&scene, damage, deadline)?;
        }
        // The warm-up picture is not content: the first real frame renders in full.
        self.invalidate();
        self.reset_attempts();
        Ok(())
    }

    /// Change the clear color, invalidating retained content only when it changes.
    pub fn set_base_color(&mut self, color: Color) {
        if self.base_color != color {
            self.base_color = color;
            self.invalidate();
            // The backdrop layer is rendered over the base colour.
            if let Some(layers) = self.strip_layers.as_mut() {
                layers.shown = None;
            }
        }
    }

    /// Resize the target. Even a rejected or no-op resize invalidates its content relation.
    ///
    /// # Errors
    /// Rejects zero/oversized extents and poisoned or lost devices before allocation.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), BridgeError> {
        self.invalidate();
        self.check_health()?;
        Self::validate_size(&self.device, width, height)?;
        if (width, height) != self.size {
            (self.texture, self.view) = Self::allocate_texture(&self.device, width, height);
            self.band = None;
            // Pooled scratch is sized for the old extent; a session that
            // changed mode never renders at it again.
            self.renderer.trim_pooled_buffers();
            self.size = (width, height);
            self.geometry_generation = self.geometry_generation.saturating_add(1);
        }
        Ok(())
    }

    pub(super) fn validate_size(
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> Result<(), BridgeError> {
        let limit = device.limits().max_texture_dimension_2d;
        if width == 0 || height == 0 || width > limit || height > limit {
            return Err(BridgeError::InvalidSize(format!(
                "{width}x{height}, device limit {limit}"
            )));
        }
        Ok(())
    }

    fn invalidate(&mut self) {
        self.retained_target_valid = false;
        self.last_successful_revision = None;
        self.owed_damage.clear();
    }

    /// `regions` together with the damage owed by frames skipped since the last render,
    /// which this takes; `None` when nothing is owed or the frame renders in full anyway.
    pub(super) fn take_owed_damage(
        &mut self,
        regions: Option<&[vello::FineDamageRect]>,
    ) -> Option<Vec<vello::FineDamageRect>> {
        let owed = std::mem::take(&mut self.owed_damage);
        regions
            .filter(|_| !owed.is_empty())
            .map(|regions| [regions, owed.as_slice()].concat())
    }

    pub(super) fn check_health(&self) -> Result<(), BridgeError> {
        if let Some(reason) = self.loss.reason() {
            return Err(BridgeError::DeviceLost(reason));
        }
        if self.poisoned {
            return Err(BridgeError::RecreateRequired);
        }
        Ok(())
    }

    fn check_live(&self, policy: CompletionPolicy) -> Result<(), BridgeError> {
        self.check_health()?;
        if self.now() >= policy.deadline() {
            return Err(policy.timed_out(CompletionPhase::FrameBudget));
        }
        Ok(())
    }

    #[allow(
        clippy::unused_self,
        reason = "the test clock observes this actual target instance"
    )]
    fn now(&self) -> Instant {
        #[cfg(test)]
        if let Some(clock) = &self.completion_test {
            return clock.now();
        }
        Instant::now()
    }

    /// A Linux allowance: `nominal` outside correctness fixtures.
    #[allow(
        clippy::unused_self,
        reason = "correctness fixtures replace the allowance on this actual target instance"
    )]
    fn linux_allowance(&self, nominal: Duration) -> Duration {
        #[cfg(test)]
        if let Some(wait) = self.phase_wait_override {
            return wait;
        }
        nominal
    }

    /// The Linux policy for a frame starting now: its per-phase allowances share one
    /// [`LINUX_FRAME_BUDGET`] across every attempt and band.
    pub(super) fn linux_frame_policy(&self) -> Result<CompletionPolicy, BridgeError> {
        self.now()
            .checked_add(self.linux_allowance(LINUX_FRAME_BUDGET))
            .map(|frame_deadline| CompletionPolicy::LinuxPerPhase { frame_deadline })
            .ok_or_else(|| BridgeError::DevicePoll("frame deadline out of range".to_owned()))
    }

    /// How long one completion phase may block now: its own allowance on Linux, never past
    /// the frame's deadline.
    fn phase_wait(&self, policy: CompletionPolicy, nominal: Duration) -> Duration {
        let remaining = policy.deadline().saturating_duration_since(self.now());
        match policy {
            CompletionPolicy::Absolute(_) => remaining,
            CompletionPolicy::LinuxPerPhase { .. } => self.linux_allowance(nominal).min(remaining),
        }
    }

    fn eligible_sparse_damage(
        &self,
        damage: SceneDamage<'_>,
    ) -> Option<Vec<vello::FineDamageRect>> {
        if self.last_successful_revision != Some(damage.previous_revision?)
            || !self.retained_target_valid
            || !self.renderer.sparse_fine_supported()
            || Self::band_rows(self.size).is_some()
        {
            return None;
        }
        VelloDrmBridge::normalize_damage(damage.regions?, self.size)
    }

    /// Render under one caller deadline, with no framebuffer staging/copy/map/swizzle.
    /// Tiny Vello bump downloads remain mandatory recovery metadata, independently of pixels.
    ///
    /// # Errors
    /// A failure hides the view. Unresolved or failed submitted work poisons this target;
    /// reconstruct it before reuse (a lost device requires a new context-owned device).
    pub fn render_until(
        &mut self,
        scene: &Scene,
        damage: SceneDamage<'_>,
        deadline: Instant,
    ) -> Result<GpuFrameStats, BridgeError> {
        let (_, admitted) = self.render(
            scene,
            Some(damage),
            FrameOutput::GpuOnly,
            CompletionPolicy::Absolute(deadline),
            None,
            |_, _, _| {},
        )?;
        Ok(GpuFrameStats {
            revision: damage.revision,
            damage_admitted: admitted,
            render_attempts: self.last_render_attempts,
            sparse_attempts: self.last_sparse_render_attempts,
            full_attempts: self.last_full_render_attempts,
            completion_markers: self.completion_markers,
            framebuffer_copy_bytes: 0,
            framebuffer_staging_bytes: 0,
        })
    }

    /// Whether [`Self::render_layers_until`] can present `cache`: a target
    /// rendered whole, in one piece, of the size the layers are drawn at.
    pub fn layers_eligible(&self, cache: &crate::shell::frame::FrameCache) -> bool {
        !cache.strips.is_empty()
            && cache.strips.len() == cache.layers.strips.len()
            && Self::band_rows(self.size).is_none()
            && cache.width >= self.size.0
            && self.hdr_pass.is_none()
            && self.backdrop.is_none()
    }

    /// Render a frame that came as layers ([`crate::shell::frame::FramePaint::cache`])
    /// by compositing retained layer textures: a strip pan copies what stays
    /// on screen and rasterises only what it uncovered (see
    /// [`strip_layers`]). `post_render` then reads the composited
    /// target, as after [`Self::render_until`]. Waits for completion.
    ///
    /// # Errors
    /// As [`Self::render_until`]; [`BridgeError::InvalidSize`] when the
    /// layers are not [`Self::layers_eligible`]. A failure forgets the
    /// layers, so the next layered frame rasterises them whole.
    pub fn render_layers_until(
        &mut self,
        cache: &crate::shell::frame::FrameCache,
        revision: u64,
        deadline: Instant,
        post_render: impl Fn(&wgpu::Device, &mut wgpu::CommandEncoder, &wgpu::TextureView),
    ) -> Result<GpuFrameStats, BridgeError> {
        if !self.layers_eligible(cache) {
            return Err(BridgeError::InvalidSize(format!(
                "layers {} wide with {} strips cannot be composited into a {}x{} target",
                cache.width,
                cache.strips.len(),
                self.size.0,
                self.size.1
            )));
        }
        let policy = CompletionPolicy::Absolute(deadline);
        crate::shell::gpu_frames::apply(
            &mut self.renderer,
            self.present_generation,
            &mut self.client_frames,
        );
        self.reset_attempts();
        self.check_live(policy)?;
        if self
            .strip_layers
            .as_ref()
            .is_none_or(|layers| layers.size != self.size)
        {
            self.strip_layers = Some(strip_layers::StripLayers::new(&self.device, self.size));
        }
        let shown = self.strip_layers.as_ref().and_then(|layers| {
            layers.shown.as_ref().map(|shown| {
                let current = self.retained_target_valid
                    && shown.revision.is_some()
                    && self.last_successful_revision == shown.revision;
                (shown, current)
            })
        });
        let work = match shown {
            Some((shown, current)) => strip_layers::plan(Some(shown), cache, self.size, current),
            None => strip_layers::plan(None, cache, self.size, false),
        };
        // The target holds this frame only once the composite lands.
        self.invalidate();
        let result = self.run_layer_work(cache, &work, policy, post_render);
        match &result {
            Ok(()) => {
                self.retained_target_valid = true;
                self.last_successful_revision = Some(revision);
                if let Some(layers) = self.strip_layers.as_mut() {
                    layers.shown = Some(strip_layers::Shown::of(cache, revision));
                }
            }
            Err(error) => {
                if let Some(layers) = self.strip_layers.as_mut() {
                    layers.shown = None;
                }
                if !matches!(error, BridgeError::RecoveryExhausted { .. }) {
                    self.poisoned = true;
                }
            }
        }
        self.last_layer_work = Some(work);
        result?;
        Ok(GpuFrameStats {
            revision,
            damage_admitted: true,
            render_attempts: self.last_render_attempts,
            sparse_attempts: self.last_sparse_render_attempts,
            full_attempts: self.last_full_render_attempts,
            completion_markers: self.completion_markers,
            framebuffer_copy_bytes: 0,
            framebuffer_staging_bytes: 0,
        })
    }

    /// Bring the layer textures up to `cache` without compositing it: the
    /// desktop at rest drawn as layers
    /// ([`crate::shell::frame::FrameBuilder::layers_at_rest`]), so that the
    /// first frame of the next strip pan copies and composites instead of
    /// rasterising every layer. Rasterises only the layers that differ from
    /// what the textures hold, a whole one in bands
    /// ([`strip_layers::warm_bands`]), and stops between two renders
    /// as soon as `keep_going` says a frame waits, keeping what it finished.
    /// The output target is left alone. Returns the pixels rasterised, or
    /// `None` when the layers cannot present `cache`.
    ///
    /// # Errors
    /// As [`Self::render_layers_until`]; a failure forgets the layers.
    pub fn warm_layers(
        &mut self,
        cache: &crate::shell::frame::FrameCache,
        deadline: Instant,
        keep_going: &dyn Fn() -> bool,
    ) -> Result<Option<u64>, BridgeError> {
        if self.poisoned || !self.layers_eligible(cache) {
            return Ok(None);
        }
        let policy = CompletionPolicy::Absolute(deadline);
        crate::shell::gpu_frames::apply(
            &mut self.renderer,
            self.present_generation,
            &mut self.client_frames,
        );
        self.reset_attempts();
        self.check_live(policy)?;
        if self
            .strip_layers
            .as_ref()
            .is_none_or(|layers| layers.size != self.size)
        {
            self.strip_layers = Some(strip_layers::StripLayers::new(&self.device, self.size));
        }
        if let Some(layers) = self.strip_layers.as_mut() {
            layers.prime_composite(&self.device, &self.queue);
        }
        let shown = self
            .strip_layers
            .as_ref()
            .and_then(|layers| layers.shown.as_ref());
        let work = strip_layers::plan(shown, cache, self.size, false);
        if work.rasterises_nothing() {
            return Ok(Some(0));
        }
        let mut shown = self
            .strip_layers
            .as_mut()
            .and_then(|layers| layers.shown.take())
            .unwrap_or_else(strip_layers::Shown::unknown);
        // The output keeps the frame it shows, which these textures no
        // longer composite to.
        shown.revision = None;
        let warmed = self.warm_into(&mut shown, cache, &work, policy, keep_going);
        // The pan's first frame renders the scenes the warm just rendered:
        // the scratch they left pooled is the scratch it needs, so it must
        // not be trimmed as a full frame's would be. What no render takes
        // again ages out of the pool (`Renderer::set_shrink_scratch`).
        self.last_frame_full = false;
        if let Some(layers) = self.strip_layers.as_mut() {
            layers.shown = warmed.is_ok().then_some(shown);
        }
        match warmed {
            Ok(pixels) => Ok(Some(pixels)),
            Err(error) => {
                if !matches!(error, BridgeError::RecoveryExhausted { .. }) {
                    self.poisoned = true;
                }
                Err(error)
            }
        }
    }

    /// [`Self::warm_layers`]' renders, noting in `shown` each layer as it
    /// is done. Returns the pixels rasterised.
    fn warm_into(
        &mut self,
        shown: &mut strip_layers::Shown,
        cache: &crate::shell::frame::FrameCache,
        work: &strip_layers::LayerWork,
        policy: CompletionPolicy,
        keep_going: &dyn Fn() -> bool,
    ) -> Result<u64, BridgeError> {
        use strip_layers::{Layer, WorkspaceWork};
        let bands =
            strip_layers::warm_bands(self.size).filter(|_| self.renderer.sparse_fine_supported());
        let whole = u64::from(self.size.0).saturating_mul(u64::from(self.size.1));
        let mut pixels = 0_u64;
        for (layer, wanted) in [
            (Layer::Backdrop, work.backdrop),
            (Layer::Chrome, work.chrome),
        ] {
            if !wanted {
                continue;
            }
            if !keep_going() {
                return Ok(pixels);
            }
            // A chrome texture that holds the drawing the chrome changed
            // from takes only the tiles that changed (a clock tick).
            let damaged = work
                .chrome_tiles
                .clone()
                .filter(|_| layer == Layer::Chrome && self.renderer.sparse_fine_supported());
            shown.forget(layer);
            let (scene, base) = if layer == Layer::Backdrop {
                (&cache.layers.backdrop, self.base_color)
            } else {
                (&cache.layers.chrome, Color::TRANSPARENT)
            };
            if let Some(rects) = damaged {
                let area = rects
                    .iter()
                    .map(|rect| u64::from(rect.width).saturating_mul(u64::from(rect.height)))
                    .fold(0, u64::saturating_add);
                if !rects.is_empty() {
                    self.render_layer(layer, scene, Some(rects), base, policy)?;
                }
                shown.record(layer, cache);
                pixels = pixels.saturating_add(area);
                continue;
            }
            let done =
                self.warm_whole(layer, (scene, base), bands.as_deref(), policy, keep_going)?;
            if !done {
                return Ok(pixels);
            }
            shown.record(layer, cache);
            pixels = pixels.saturating_add(whole);
        }
        match (work.wheel, cache.layers.wheel.as_ref()) {
            (Some(area), Some(wheel)) => {
                if !keep_going() {
                    return Ok(pixels);
                }
                shown.forget(Layer::Wheel);
                if let Some(layers) = self.strip_layers.as_mut() {
                    layers.size_wheel(&self.device, (wheel.width, wheel.height));
                }
                self.render_layer(Layer::Wheel, &wheel.scene, None, Color::TRANSPARENT, policy)?;
                shown.record(Layer::Wheel, cache);
                pixels = pixels.saturating_add(area);
            }
            // The wheel's texture holds what it shows, or no wheel shows.
            _ => shown.record(Layer::Wheel, cache),
        }
        if work.workspace == WorkspaceWork::Unchanged || !keep_going() {
            return Ok(pixels);
        }
        shown.forget(Layer::Workspace);
        let strips =
            self.warm_strips(cache, &work.workspace, bands.as_deref(), policy, keep_going)?;
        if let Some(strips) = strips {
            shown.record(Layer::Workspace, cache);
            pixels = pixels.saturating_add(strips);
        }
        Ok(pixels)
    }

    /// [`Self::warm_layers`]' work on the strips texture: the pixels it
    /// rasterised, or `None` when it stopped for a frame before the texture
    /// held `cache`'s strips.
    fn warm_strips(
        &mut self,
        cache: &crate::shell::frame::FrameCache,
        work: &strip_layers::WorkspaceWork,
        bands: Option<&[vello::FineDamageRect]>,
        policy: CompletionPolicy,
        keep_going: &dyn Fn() -> bool,
    ) -> Result<Option<u64>, BridgeError> {
        use strip_layers::{Layer, WorkspaceWork};
        let mut scene = Scene::new();
        strip_layers::append_panned(&mut scene, &cache.layers, &cache.strips);
        match work {
            WorkspaceWork::Unchanged => Ok(Some(0)),
            WorkspaceWork::Whole => {
                let whole = u64::from(self.size.0).saturating_mul(u64::from(self.size.1));
                let done = self.warm_whole(
                    Layer::Workspace,
                    (&scene, Color::TRANSPARENT),
                    bands,
                    policy,
                    keep_going,
                )?;
                Ok(done.then_some(whole))
            }
            WorkspaceWork::Pan { copies, uncovered } => {
                if !copies.is_empty() {
                    let mut encoder =
                        self.device
                            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                                label: Some("tilo-strip-layer-warm-pan"),
                            });
                    if let Some(layers) = self.strip_layers.as_mut() {
                        layers.encode_copies(&self.device, &mut encoder, copies);
                    }
                    self.queue.submit([encoder.finish()]);
                }
                if !uncovered.is_empty() {
                    let regions = self
                        .renderer
                        .sparse_fine_supported()
                        .then(|| uncovered.clone());
                    self.render_layer(
                        Layer::Workspace,
                        &scene,
                        regions,
                        Color::TRANSPARENT,
                        policy,
                    )?;
                }
                Ok(Some(
                    uncovered
                        .iter()
                        .map(|rect| u64::from(rect.width).saturating_mul(u64::from(rect.height)))
                        .fold(0, u64::saturating_add),
                ))
            }
        }
    }

    /// Rasterise `scene` over `base` into all of `layer`, band by band when
    /// `bands` cuts it, asking `keep_going` between two bands. Whether the
    /// whole layer got drawn.
    fn warm_whole(
        &mut self,
        layer: strip_layers::Layer,
        (scene, base): (&Scene, Color),
        bands: Option<&[vello::FineDamageRect]>,
        policy: CompletionPolicy,
        keep_going: &dyn Fn() -> bool,
    ) -> Result<bool, BridgeError> {
        let Some(bands) = bands else {
            self.render_layer(layer, scene, None, base, policy)?;
            return Ok(true);
        };
        for (index, band) in bands.iter().enumerate() {
            if index > 0 && !keep_going() {
                return Ok(false);
            }
            self.render_layer(layer, scene, Some(vec![*band]), base, policy)?;
        }
        Ok(true)
    }

    /// The target's pixels, RGBA, row by row: what the final pass reads.
    #[cfg(test)]
    pub(crate) fn read_rgba_for_test(&self) -> Vec<u8> {
        let (width, height) = self.size;
        let row = width.saturating_mul(4);
        let padded = row
            .div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            .saturating_mul(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tilo-target-readback-for-test"),
            size: u64::from(padded).saturating_mul(u64::from(height)),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_texture_to_buffer(
            self.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);
        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            tx.send(result).ok();
        });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(tilo_test_env::hang_bound()),
            })
            .expect("the readback copy completes");
        rx.recv().expect("map callback").expect("map succeeds");
        let mapped = slice.get_mapped_range();
        let (padded, row) = (
            usize::try_from(padded).expect("row pitch fits usize"),
            usize::try_from(row).expect("row bytes fit usize"),
        );
        mapped
            .chunks_exact(padded)
            .flat_map(|line| &line[..row])
            .copied()
            .collect()
    }

    /// The work the latest [`Self::render_layers_until`] planned.
    #[cfg(test)]
    pub(crate) fn last_layer_work_for_test(&self) -> Option<&strip_layers::LayerWork> {
        self.last_layer_work.as_ref()
    }

    /// Keep the frame `damage` describes without rendering it, as the DRM
    /// presenter keeps a lone output's image for a frame proven unchanged
    /// (empty damage) relative to the one this target holds: the target then
    /// holds this revision. `false`, and nothing kept, for any other frame.
    pub fn keep_unchanged(&mut self, damage: SceneDamage<'_>) -> bool {
        let unchanged = damage.regions.is_some_and(<[_]>::is_empty);
        let holds_previous = damage.previous_revision.is_some()
            && self.last_successful_revision == damage.previous_revision;
        if !unchanged
            || !holds_previous
            || !self.retained_target_valid
            || !self.owed_damage.is_empty()
        {
            return false;
        }
        if let Some(previous) = damage.previous_revision {
            self.layers_kept(previous, damage.revision);
        }
        self.last_successful_revision = Some(damage.revision);
        true
    }

    /// The target went from `previous` to `revision` without a pixel
    /// changing: layers it was composited from as `previous` are still what
    /// it shows, so the next pan composites only what moves.
    pub(super) fn layers_kept(&mut self, previous: u64, revision: u64) {
        if let Some(shown) = self
            .strip_layers
            .as_mut()
            .and_then(|layers| layers.shown.as_mut())
            && shown.revision == Some(previous)
        {
            shown.revision = Some(revision);
        }
    }

    /// Hand the next render of frame `revision` the copies its producer
    /// planned ([`crate::shell::frame::FramePaint::scroll_copies`]): when
    /// that render is sparse against the frame this target holds, it moves
    /// those pixels within the target first and rasterises only the rest.
    /// Any other render ignores them; `None` drops what was staged.
    pub fn stage_scroll_copies(
        &mut self,
        revision: Option<u64>,
        copies: Option<&crate::shell::frame::ScrollCopies>,
    ) {
        self.staged_copies = revision.zip(copies.cloned());
    }

    /// The pixels the latest render rasterised and copied, when it made
    /// scroll copies ([`Self::stage_scroll_copies`]), for counters.
    pub fn last_scroll_copy_work(&self) -> Option<(u64, u64)> {
        self.last_scroll_copy_work
    }

    /// The changed regions of a frame that makes `copies`, sparse against
    /// the frame this target holds, after making the copies within it: what
    /// the render reads out (every pixel the frame changes), with the
    /// rasterised regions set aside for it. `None`, with nothing copied,
    /// when the frame is not sparse-eligible or a copy would leave the
    /// target: it then renders as it would without copies.
    fn make_scroll_copies(
        &mut self,
        damage: SceneDamage<'_>,
        copies: &crate::shell::frame::ScrollCopies,
    ) -> Option<Vec<vello::FineDamageRect>> {
        if self.last_successful_revision != Some(damage.previous_revision?)
            || !self.retained_target_valid
            || !self.renderer.sparse_fine_supported()
            || Self::band_rows(self.size).is_some()
            || copies.copies.is_empty()
        {
            return None;
        }
        // What is rasterised stays under half the target, as any sparse
        // frame's damage; what the copies move may be more.
        let raster = VelloDrmBridge::normalize_damage(&copies.raster, self.size)?;
        let changed = VelloDrmBridge::normalize_rects(damage.regions?, self.size)?;
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("tilo-scroll-copies"),
            });
        if !ui::scroll_copy::encode_copies(
            &self.device,
            &mut encoder,
            &self.texture,
            &mut self.copy_scratch,
            &copies.copies,
        ) {
            return None;
        }
        self.queue.submit([encoder.finish()]);
        let area = |rect: &vello::FineDamageRect| {
            u64::from(rect.width).saturating_mul(u64::from(rect.height))
        };
        let moved_pixels = copies
            .copies
            .iter()
            .map(|copy| u64::from(copy.width).saturating_mul(u64::from(copy.height)))
            .fold(0_u64, u64::saturating_add);
        let rasterised = raster.iter().map(area).fold(0_u64, u64::saturating_add);
        self.last_scroll_copy_work = Some((rasterised, moved_pixels));
        self.raster_override = Some(raster);
        Some(changed)
    }

    /// The work the latest [`Self::render_layers_until`] did, for counters.
    pub fn last_layer_raster_pixels(&self) -> Option<(u64, u64)> {
        self.last_layer_work
            .as_ref()
            .map(|work| (work.raster_pixels(self.size), work.composite_pixels()))
    }

    fn run_layer_work(
        &mut self,
        cache: &crate::shell::frame::FrameCache,
        work: &strip_layers::LayerWork,
        policy: CompletionPolicy,
        post_render: impl Fn(&wgpu::Device, &mut wgpu::CommandEncoder, &wgpu::TextureView),
    ) -> Result<(), BridgeError> {
        use strip_layers::{Layer, WorkspaceWork};
        let transparent = Color::TRANSPARENT;
        if work.backdrop {
            let base = self.base_color;
            self.render_layer(Layer::Backdrop, &cache.layers.backdrop, None, base, policy)?;
        }
        // The chrome's changed tiles, or all of it.
        let chrome_tiles = work
            .chrome_tiles
            .clone()
            .filter(|_| self.renderer.sparse_fine_supported());
        if work.chrome && chrome_tiles.as_ref().is_none_or(|tiles| !tiles.is_empty()) {
            self.render_layer(
                Layer::Chrome,
                &cache.layers.chrome,
                chrome_tiles,
                transparent,
                policy,
            )?;
        }
        if let (Some(_), Some(wheel)) = (work.wheel, cache.layers.wheel.as_ref()) {
            if let Some(layers) = self.strip_layers.as_mut() {
                layers.size_wheel(&self.device, (wheel.width, wheel.height));
            }
            self.render_layer(Layer::Wheel, &wheel.scene, None, transparent, policy)?;
        }
        let panned = || {
            let mut scene = Scene::new();
            strip_layers::append_panned(&mut scene, &cache.layers, &cache.strips);
            scene
        };
        match &work.workspace {
            WorkspaceWork::Unchanged => {}
            WorkspaceWork::Whole => {
                let scene = panned();
                self.render_layer(Layer::Workspace, &scene, None, transparent, policy)?;
            }
            WorkspaceWork::Pan { copies, uncovered } => {
                if !copies.is_empty() {
                    let mut encoder =
                        self.device
                            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                                label: Some("tilo-strip-layer-pan"),
                            });
                    if let Some(layers) = self.strip_layers.as_mut() {
                        layers.encode_copies(&self.device, &mut encoder, copies);
                    }
                    self.queue.submit([encoder.finish()]);
                }
                if !uncovered.is_empty() {
                    let scene = panned();
                    let regions = self
                        .renderer
                        .sparse_fine_supported()
                        .then(|| uncovered.clone());
                    self.render_layer(Layer::Workspace, &scene, regions, transparent, policy)?;
                }
            }
        }
        self.check_live(policy)?;
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("tilo-strip-layer-composite"),
            });
        if let (Some(layers), Some(rect)) = (self.strip_layers.as_ref(), work.composite) {
            layers.encode_composite(
                strip_layers::CompositeTarget {
                    device: &self.device,
                    queue: &self.queue,
                    encoder: &mut encoder,
                    output: &self.view,
                },
                cache.layers.wheel.as_ref(),
                rect,
            );
        }
        post_render(&self.device, &mut encoder, &self.view);
        let submission = self.queue.submit([encoder.finish()]);
        self.wait_submission(submission, policy)
    }

    /// Render `scene` into one layer's texture, through the target's own
    /// render path (its recovery from vello's budget overflow included):
    /// the layer's texture stands in for the target's for the render.
    fn render_layer(
        &mut self,
        layer: strip_layers::Layer,
        scene: &Scene,
        regions: Option<Vec<vello::FineDamageRect>>,
        base_color: Color,
        policy: CompletionPolicy,
    ) -> Result<(), BridgeError> {
        self.swap_layer(layer);
        let base_color = std::mem::replace(&mut self.base_color, base_color);
        // Each layer has the recovery attempts a frame has.
        let earlier_attempts = std::mem::take(&mut self.last_render_attempts);
        let rendered = self.render_attempts(
            scene,
            regions,
            FrameOutput::GpuOnly,
            policy,
            None,
            |_, _, _| {},
        );
        self.last_render_attempts = self.last_render_attempts.saturating_add(earlier_attempts);
        self.base_color = base_color;
        self.swap_layer(layer);
        rendered.map(|_| ())
    }

    /// Exchange the target's texture with `layer`'s, size included: the
    /// wheel's texture is the wheel's size, not the output's.
    fn swap_layer(&mut self, layer: strip_layers::Layer) {
        if let Some(layers) = self.strip_layers.as_mut() {
            let slot = layers.texture_mut(layer);
            std::mem::swap(&mut self.texture, &mut slot.texture);
            std::mem::swap(&mut self.view, &mut slot.view);
            std::mem::swap(&mut self.size, &mut slot.size);
        }
    }

    /// Completed output for read-only sampling, never a display presentation claim.
    pub fn completed_view(&self) -> Option<&wgpu::TextureView> {
        (self.retained_target_valid && !self.poisoned && self.loss.reason().is_none())
            .then_some(&self.view)
    }

    /// Current native pixel extent.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    pub(super) fn reset_attempts(&mut self) {
        self.last_render_attempts = 0;
        self.last_sparse_render_attempts = 0;
        self.last_full_render_attempts = 0;
        self.last_canonical_segment_order_full_attempts = 0;
        self.last_canonical_segment_order_sparse_attempts = 0;
        self.last_area_ppt8_full_attempts = 0;
        self.last_area_ppt8_sparse_attempts = 0;
        self.last_clip_single_group_full_attempts = 0;
        self.last_clip_single_group_sparse_attempts = 0;
        self.last_sparse_render_stats = None;
        self.completion_markers = 0;
    }

    pub(super) fn render<F>(
        &mut self,
        scene: &Scene,
        damage: Option<SceneDamage<'_>>,
        output: FrameOutput<'_>,
        policy: CompletionPolicy,
        timing: Option<&mut BridgePhaseTiming>,
        post_render: F,
    ) -> Result<(Option<wgpu::BufferView>, bool), BridgeError>
    where
        F: Fn(&wgpu::Device, &mut wgpu::CommandEncoder, &wgpu::TextureView),
    {
        crate::shell::gpu_frames::apply(
            &mut self.renderer,
            self.present_generation,
            &mut self.client_frames,
        );
        self.raster_override = None;
        self.last_scroll_copy_work = None;
        // The frame's scroll copies, when it is the one they were planned for.
        let staged = self
            .staged_copies
            .take()
            .filter(|(revision, _)| damage.is_some_and(|damage| damage.revision == *revision));
        let copied = staged
            .as_ref()
            .zip(damage)
            .and_then(|((_, copies), damage)| self.make_scroll_copies(damage, copies));
        let sparse =
            copied.or_else(|| damage.and_then(|damage| self.eligible_sparse_damage(damage)));
        self.invalidate();
        self.reset_attempts();
        self.check_live(policy)?;
        // Once encoding starts, even a pre-submit abort leaves renderer-owned downloads and
        // timestamp bookkeeping unusable. Clear poison only after both completion facts hold.
        let result = match Self::band_rows(self.size).filter(|_| sparse.is_none()) {
            Some(rows) => self.render_banded(scene, rows, output, policy, timing, post_render),
            None => self.render_attempts(scene, sparse, output, policy, timing, post_render),
        };
        if let Some(pass) = self.hdr_pass.as_ref() {
            // Later renders (and captures on this renderer) draw the images
            // as they are again.
            for substitution in &pass.substitutions {
                self.renderer.override_image(&substitution.image, None);
            }
        }
        if let Some(blur) = self.backdrop_blur.as_mut() {
            blur.release(&mut self.renderer);
        }
        match &result {
            Ok(_) => {
                #[cfg(target_os = "linux")]
                crate::shader_seed::after_frame();
                self.retained_target_valid = true;
                self.last_successful_revision = damage.map(|damage| damage.revision);
            }
            // Every attempt completed and settled its counters, so the renderer is consistent
            // and holds the budget those attempts grew. Rebuilding it would start the next
            // frame from the initial budget and overflow the same way forever.
            Err(BridgeError::RecoveryExhausted { .. }) => {}
            Err(_) => self.poisoned = true,
        }
        result
    }

    /// The height of the horizontal bands a target of `size` must be rendered in, or `None` when
    /// it renders whole.
    ///
    /// Vello bins the target into 256x256-pixel bins, and its binning and coarse stages keep one
    /// 256-entry bitmap per workgroup. A target with more bins than that is not refused: the
    /// indices past 255 are clamped onto other bins, and whatever falls there is drawn wrong or
    /// not at all. Two outputs side by side reach it easily -- 5120x2880 beside 1920x1080 is a
    /// 7040x2880 target of 336 bins -- and so does a single 6K or 8K panel.
    pub(super) fn band_rows((width, height): (u32, u32)) -> Option<u32> {
        let bins_x = width.div_ceil(VELLO_BIN_PIXELS);
        let bins_y = height.div_ceil(VELLO_BIN_PIXELS);
        if bins_x.saturating_mul(bins_y) <= MAX_VELLO_BINS {
            return None;
        }
        // bins_x is at least 1 for any positive width; zero width never reaches banding.
        let bins_per_band = MAX_VELLO_BINS.checked_div(bins_x).unwrap_or(1).max(1);
        Some(bins_per_band.saturating_mul(VELLO_BIN_PIXELS))
    }

    fn band_target(&mut self, rows: u32) -> (wgpu::Texture, wgpu::TextureView) {
        if let Some((texture, view, band_rows)) = &self.band
            && *band_rows == rows
        {
            return (texture.clone(), view.clone());
        }
        let (texture, view) = Self::allocate_texture(&self.device, self.size.0, rows);
        self.band = Some((texture.clone(), view.clone(), rows));
        (texture, view)
    }

    /// Dense render of a target past [`MAX_VELLO_BINS`], one band of `rows` at a time.
    ///
    /// Each band renders the scene translated up by its offset into a band-sized texture, which
    /// is copied into place in the full target; the full target's output copy and `post_render`
    /// follow the last band, so callers see exactly what a whole render would produce. Each band
    /// has its own bounded recovery, and every one is a smaller viewport, so its bump buffers are
    /// smaller too.
    #[allow(
        clippy::too_many_lines,
        reason = "one ordered band/recovery transaction serves both output paths"
    )]
    fn render_banded(
        &mut self,
        scene: &Scene,
        rows: u32,
        output: FrameOutput<'_>,
        policy: CompletionPolicy,
        mut timing: Option<&mut BridgePhaseTiming>,
        post_render: impl Fn(&wgpu::Device, &mut wgpu::CommandEncoder, &wgpu::TextureView),
    ) -> Result<(Option<wgpu::BufferView>, bool), BridgeError> {
        use vello::kurbo::Affine;

        let (width, height) = self.size;
        let (band_texture, band_view) = self.band_target(rows);
        // Every band draws the same substituted placeholders.
        self.prepare_backdrop(timing.as_deref_mut());
        let mut top = 0;
        while top < height {
            let band_height = rows.min(height.saturating_sub(top));
            let last = top.saturating_add(band_height) >= height;
            let mut band_scene = Scene::new();
            band_scene.append(scene, Some(Affine::translate((0.0, -f64::from(top)))));
            let mut attempts = 0;
            let mapped = loop {
                if attempts == RESILIENT_RENDER_ATTEMPTS {
                    return Err(BridgeError::RecoveryExhausted {
                        attempts: self.last_render_attempts,
                    });
                }
                attempts = attempts.saturating_add(1);
                self.check_live(policy)?;
                let submit_started = timing.as_ref().map(|_| Instant::now());
                let params = RenderParams {
                    base_color: self.base_color,
                    width,
                    height: band_height,
                    antialiasing_method: AaConfig::Area,
                };
                let loss = self.loss.clone();
                let before_submit =
                    || loss.reason().is_none() && Instant::now() < policy.deadline();
                let append = |encoder: &mut wgpu::CommandEncoder| {
                    encoder.copy_texture_to_texture(
                        band_texture.as_image_copy(),
                        wgpu::TexelCopyTextureInfo {
                            texture: &self.texture,
                            mip_level: 0,
                            origin: wgpu::Origin3d { x: 0, y: top, z: 0 },
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::Extent3d {
                            width,
                            height: band_height,
                            depth_or_array_layers: 1,
                        },
                    );
                    if last {
                        output.append(encoder, &self.texture, self.size, None);
                        post_render(&self.device, encoder, &self.view);
                    }
                };
                let submission = match self
                    .renderer
                    .render_to_texture_with_post_render_checked_indexed(
                        &self.device,
                        &self.queue,
                        &band_scene,
                        &band_view,
                        &params,
                        append,
                        before_submit,
                    ) {
                    Ok(submission) => submission,
                    Err(vello::Error::SubmissionAborted) => {
                        self.check_live(policy)?;
                        return Err(BridgeError::Render(
                            "submission declined after encoding".to_owned(),
                        ));
                    }
                    Err(error) => return Err(BridgeError::Render(format!("{error:?}"))),
                };
                self.last_render_attempts = self.last_render_attempts.saturating_add(1);
                self.last_full_render_attempts = self.last_full_render_attempts.saturating_add(1);
                if let (Some(timing), Some(started)) = (timing.as_deref_mut(), submit_started) {
                    timing.submit_us = timing
                        .submit_us
                        .saturating_add(started.elapsed().as_micros());
                    if last {
                        timing.readback_bytes = timing
                            .readback_bytes
                            .saturating_add(output.copy_bytes(self.size, None));
                    }
                }
                self.check_live(policy)?;
                let map_started = timing.as_ref().map(|_| Instant::now());
                let mapped = match output {
                    FrameOutput::Xrgb { buffer, .. } if last => {
                        Some(self.map_framebuffer(buffer, policy)?)
                    }
                    FrameOutput::GpuOnly if matches!(policy, CompletionPolicy::Absolute(_)) => {
                        self.wait_submission(submission.clone(), policy)?;
                        None
                    }
                    // Counter settlement below waits on this band's submission.
                    _ => None,
                };
                if let (Some(timing), Some(started)) = (timing.as_deref_mut(), map_started) {
                    timing.completed_readback_map_us = timing
                        .completed_readback_map_us
                        .saturating_add(started.elapsed().as_micros());
                }
                let settle_started = timing.as_ref().map(|_| Instant::now());
                let settled =
                    self.settle_counters(policy, Some(&submission))
                        .and_then(|overflowed| {
                            self.check_live(policy)?;
                            Ok(overflowed)
                        });
                if let (Some(timing), Some(started)) = (timing.as_deref_mut(), settle_started) {
                    timing.settle_us = timing
                        .settle_us
                        .saturating_add(started.elapsed().as_micros());
                }
                match settled {
                    Ok(false) => break mapped,
                    Ok(true) => {
                        if mapped.is_some() {
                            drop(mapped);
                            output.unmap();
                        }
                    }
                    Err(error) => {
                        if mapped.is_some() {
                            drop(mapped);
                            output.unmap();
                        }
                        return Err(error);
                    }
                }
            };
            if last {
                return Ok((mapped, false));
            }
            top = top.saturating_add(band_height);
        }
        Err(BridgeError::InvalidSize(format!("{width}x{height}")))
    }

    /// Render, blur and substitute the backdrops of the frame about to be
    /// rendered, when it has any.
    fn prepare_backdrop(&mut self, timing: Option<&mut BridgePhaseTiming>) {
        let Some(frame) = self.backdrop.as_deref().filter(|frame| !frame.is_empty()) else {
            return;
        };
        let stats = self
            .backdrop_blur
            .get_or_insert_with(|| ui::backdrop::BackdropBlur::new(&self.device))
            .prepare(
                &self.device,
                &self.queue,
                &mut self.renderer,
                frame,
                self.size,
                self.base_color,
            );
        if let Some(timing) = timing {
            timing.backdrop_us = timing
                .backdrop_us
                .saturating_add(u128::from(stats.encode_us));
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one ordered render/recovery transaction serves both output paths"
    )]
    fn render_attempts(
        &mut self,
        scene: &Scene,
        mut sparse: Option<Vec<vello::FineDamageRect>>,
        output: FrameOutput<'_>,
        policy: CompletionPolicy,
        mut timing: Option<&mut BridgePhaseTiming>,
        post_render: impl Fn(&wgpu::Device, &mut wgpu::CommandEncoder, &wgpu::TextureView),
    ) -> Result<(Option<wgpu::BufferView>, bool), BridgeError> {
        // A full frame (a lock screen, an overlay's first paint) leaves
        // full-target scratch pooled; damage frames after it never reuse it.
        let full = sparse.is_none();
        if !full && self.last_frame_full {
            self.renderer.trim_pooled_buffers();
        }
        self.last_frame_full = full;
        // After scroll copies only what they did not move is rasterised;
        // every changed pixel is still read out.
        let mut raster = self.raster_override.take();
        while self.last_render_attempts < RESILIENT_RENDER_ATTEMPTS {
            self.check_live(policy)?;
            let regions = sparse.take();
            let submit_started = timing.as_ref().map(|_| Instant::now());
            let params = RenderParams {
                base_color: self.base_color,
                width: self.size.0,
                height: self.size.1,
                antialiasing_method: AaConfig::Area,
            };
            self.prepare_backdrop(timing.as_deref_mut());
            if let Some(pass) = self.hdr_pass.as_ref() {
                for substitution in &pass.substitutions {
                    self.renderer
                        .override_image(&substitution.image, Some(substitution.lit.clone()));
                }
                let second = self.renderer.render_to_texture(
                    &self.device,
                    &self.queue,
                    scene,
                    &pass.second_view,
                    &params,
                );
                for substitution in &pass.substitutions {
                    self.renderer
                        .override_image(&substitution.image, Some(substitution.black.clone()));
                }
                second.map_err(|error| {
                    BridgeError::Render(format!("HDR layer weights: {error:?}"))
                })?;
            }
            let loss = self.loss.clone();
            #[cfg(test)]
            let gate_now = self.completion_test.as_mut().map(|clock| {
                clock.advance(clock.encode_elapsed);
                clock.now()
            });
            let before_submit = || {
                #[cfg(test)]
                let now = gate_now.unwrap_or_else(Instant::now);
                #[cfg(not(test))]
                let now = Instant::now();
                loss.reason().is_none() && now < policy.deadline()
            };
            let append = |encoder: &mut wgpu::CommandEncoder| {
                output.append(encoder, &self.texture, self.size, regions.as_deref());
                post_render(&self.device, encoder, &self.view);
            };
            let rasterised = raster.take();
            let submitted = if let Some(regions) = regions.as_deref() {
                self.renderer
                    .render_to_texture_with_damage_and_post_render_checked(
                        &self.device,
                        &self.queue,
                        scene,
                        &self.view,
                        &params,
                        rasterised.as_deref().unwrap_or(regions),
                        append,
                        before_submit,
                    )
                    .map(|stats| (stats, None))
            } else {
                self.renderer
                    .render_to_texture_with_post_render_checked_indexed(
                        &self.device,
                        &self.queue,
                        scene,
                        &self.view,
                        &params,
                        append,
                        before_submit,
                    )
                    .map(|submission| (None, Some(submission)))
            };
            let (stats, submission_index) = match submitted {
                Ok(submitted) => submitted,
                Err(vello::Error::SubmissionAborted) => {
                    self.check_live(policy)?;
                    return Err(BridgeError::Render(
                        "submission declined after encoding".to_owned(),
                    ));
                }
                Err(error) => return Err(BridgeError::Render(format!("{error:?}"))),
            };
            if regions.is_some() && stats.is_none() {
                // Unsupported sparse rendering encoded/submitted nothing; dense still owns all four attempts.
                continue;
            }
            self.last_render_attempts = self.last_render_attempts.saturating_add(1);
            let admitted = regions.is_some();
            if let Some(stats) = stats {
                self.last_sparse_render_attempts =
                    self.last_sparse_render_attempts.saturating_add(1);
                VelloDrmBridge::record_sparse_shader_selected_attempts(
                    &stats,
                    Some(&mut self.last_canonical_segment_order_sparse_attempts),
                    Some(&mut self.last_area_ppt8_sparse_attempts),
                    Some(&mut self.last_clip_single_group_sparse_attempts),
                );
                self.last_sparse_render_stats = Some(stats);
            } else {
                self.last_full_render_attempts = self.last_full_render_attempts.saturating_add(1);
                self.last_canonical_segment_order_full_attempts = self
                    .last_canonical_segment_order_full_attempts
                    .saturating_add(usize::from(
                        self.renderer.last_canonical_segment_order_selected(),
                    ));
                self.last_area_ppt8_full_attempts = self
                    .last_area_ppt8_full_attempts
                    .saturating_add(usize::from(self.renderer.last_area_ppt8_selected()));
                self.last_clip_single_group_full_attempts = self
                    .last_clip_single_group_full_attempts
                    .saturating_add(usize::from(self.renderer.last_clip_single_group_selected()));
            }
            if let (Some(timing), Some(started)) = (timing.as_deref_mut(), submit_started) {
                timing.submit_us = timing
                    .submit_us
                    .saturating_add(started.elapsed().as_micros());
                timing.readback_bytes = timing
                    .readback_bytes
                    .saturating_add(output.copy_bytes(self.size, regions.as_deref()));
            }
            self.check_live(policy)?;
            let map_started = timing.as_ref().map(|_| Instant::now());
            let mapped = match output {
                FrameOutput::GpuOnly => {
                    // The sparse path does not report its submission; an
                    // empty submit queued behind it completes no earlier.
                    let submission = submission_index.clone().unwrap_or_else(|| {
                        self.queue.submit(std::iter::empty::<wgpu::CommandBuffer>())
                    });
                    self.wait_submission(submission, policy)?;
                    None
                }
                FrameOutput::Xrgb { buffer, .. } => Some(self.map_framebuffer(buffer, policy)?),
            };
            if let (Some(timing), Some(started)) = (timing.as_deref_mut(), map_started) {
                timing.completed_readback_map_us = timing
                    .completed_readback_map_us
                    .saturating_add(started.elapsed().as_micros());
            }
            let settle_started = timing.as_ref().map(|_| Instant::now());
            // Without the sparse path's index, settlement waits on the latest submission.
            let settled = self
                .settle_counters(policy, submission_index.as_ref())
                .and_then(|overflowed| {
                    self.check_live(policy)?;
                    Ok(overflowed)
                });
            if let (Some(timing), Some(started)) = (timing.as_deref_mut(), settle_started) {
                timing.settle_us = timing
                    .settle_us
                    .saturating_add(started.elapsed().as_micros());
            }
            match settled {
                Ok(false) => return Ok((mapped, admitted)),
                Ok(true) => {
                    drop(mapped);
                    output.unmap();
                }
                Err(error) => {
                    drop(mapped);
                    output.unmap();
                    return Err(error);
                }
            }
        }
        Err(BridgeError::RecoveryExhausted {
            attempts: self.last_render_attempts,
        })
    }

    fn wait_submission(
        &mut self,
        submission: wgpu::SubmissionIndex,
        policy: CompletionPolicy,
    ) -> Result<(), BridgeError> {
        self.check_live(policy)?;
        let CompletionPolicy::Absolute(deadline) = policy else {
            return Err(BridgeError::DevicePoll(
                "LinuxPerPhase cannot request a GPU-only marker".to_owned(),
            ));
        };
        self.completion_markers = self.completion_markers.saturating_add(1);
        let remaining = deadline.saturating_duration_since(self.now());
        if remaining.is_zero() {
            return Err(BridgeError::DeadlineExceeded);
        }
        #[cfg(test)]
        self.before_wait(WaitPhase::Marker, remaining)?;
        let waited = self.device.poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: Some(remaining),
        });
        // PollError itself cannot identify device loss. Prefer the context callback's
        // actual reason even when the same poll also reported a fence error.
        self.check_health()?;
        waited.map_err(|error| match error {
            wgpu::PollError::Timeout => BridgeError::DeadlineExceeded,
            other @ wgpu::PollError::WrongSubmissionIndex(..) => {
                BridgeError::DevicePoll(format!("GPU-only submission: {other:?}"))
            }
        })?;
        self.check_live(policy)
    }

    #[cfg_attr(
        not(test),
        allow(
            clippy::needless_pass_by_ref_mut,
            reason = "tests inject elapsed time at the real framebuffer waits"
        )
    )]
    fn map_framebuffer(
        &mut self,
        buffer: &wgpu::Buffer,
        policy: CompletionPolicy,
    ) -> Result<wgpu::BufferView, BridgeError> {
        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        #[cfg(test)]
        let rx = match self.completion_test.as_ref() {
            Some(test) if test.disconnect_map_callback => std::sync::mpsc::channel().1,
            _ => rx,
        };
        let result = (|| {
            let fence_wait = self.phase_wait(policy, LINUX_FENCE_WAIT);
            #[cfg(test)]
            self.before_wait(WaitPhase::Fence, fence_wait)?;
            let waited = self.device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(fence_wait),
            });
            self.check_health()?;
            waited.map_err(|error| match error {
                wgpu::PollError::Timeout => policy.timed_out(CompletionPhase::Fence),
                other @ wgpu::PollError::WrongSubmissionIndex(..) => {
                    BridgeError::Map(format!("fence wait: {other:?}"))
                }
            })?;
            // A fresh callback allowance follows the independent fence allowance.
            let callback_wait = self.phase_wait(policy, LINUX_MAP_CALLBACK_WAIT);
            #[cfg(test)]
            self.before_wait(WaitPhase::MapCallback, callback_wait)?;
            rx.recv_timeout(callback_wait)
                .map_err(|error| match error {
                    std::sync::mpsc::RecvTimeoutError::Timeout => {
                        policy.timed_out(CompletionPhase::MapCallback)
                    }
                    // The callback can no longer report: wgpu dropped it, as a lost device does.
                    // Nothing is left to drain, so this fails closed instead of backing off.
                    std::sync::mpsc::RecvTimeoutError::Disconnected => {
                        BridgeError::Map("map_async callback dropped without reporting".to_owned())
                    }
                })?
                .map_err(|error| BridgeError::Map(format!("map_async: {error:?}")))?;
            self.check_health()?;
            Ok(slice.get_mapped_range())
        })();
        if result.is_err() {
            buffer.unmap();
        }
        result
    }

    /// Wait until Vello's bump counters for the frame just submitted are read back, and report
    /// whether it overflowed.
    ///
    /// Each round polls the counters once and, while they are pending, blocks on `submission`
    /// (the latest submission when `None`) for the rest of the phase allowance: the device
    /// wakes this thread when the work completes, rather than a sleep loop sampling it.
    pub(super) fn settle_counters(
        &mut self,
        policy: CompletionPolicy,
        submission: Option<&wgpu::SubmissionIndex>,
    ) -> Result<bool, BridgeError> {
        let deadline = self
            .now()
            .checked_add(self.phase_wait(policy, BUMP_SETTLEMENT_WAIT))
            .ok_or_else(|| BridgeError::BumpReadback("counter deadline out of range".to_owned()))?;
        for _ in 0..COUNTER_SETTLEMENT_ROUNDS {
            self.check_health()?;
            if self.now() >= deadline {
                tracing::warn!(
                    ?policy,
                    phase = "counter settlement",
                    "completion deadline exceeded"
                );
                return Err(policy.timed_out(CompletionPhase::CounterSettlement));
            }
            let result = self.renderer.poll_bump_budget(&self.device);
            #[cfg(test)]
            let result = if let Some(test) = self.completion_test.as_mut() {
                test.counter_remaining
                    .push(deadline.saturating_duration_since(test.now()));
                test.counters.pop_front().unwrap_or(result)
            } else {
                result
            };
            self.check_health()?;
            if self.now() >= deadline {
                continue;
            }
            match result.map_err(|error| match error {
                vello::BumpBudgetError::Poll(message) => {
                    BridgeError::DevicePoll(format!("{policy:?} counter settlement: {message}"))
                }
                other => {
                    BridgeError::BumpReadback(format!("{policy:?} counter settlement: {other:?}"))
                }
            })? {
                vello::BumpBudgetPoll::Complete { overflowed } => return Ok(overflowed),
                vello::BumpBudgetPoll::Pending => {
                    self.wait_counters(policy, submission, deadline)?;
                }
            }
        }
        self.check_health()?;
        if self.now() >= deadline {
            return Err(policy.timed_out(CompletionPhase::CounterSettlement));
        }
        // Every round's wait completed yet the readback stayed pending: nothing is left in
        // flight to drain, so this is not a timeout.
        Err(BridgeError::BumpReadback(format!(
            "counter readback still pending after {COUNTER_SETTLEMENT_ROUNDS} completed waits"
        )))
    }

    /// Block until `submission` completes (wgpu runs its map callbacks, the counters' among
    /// them, before returning) or the settlement `deadline` passes.
    #[cfg_attr(
        not(test),
        allow(
            clippy::needless_pass_by_ref_mut,
            reason = "tests inject elapsed time at the real counter wait"
        )
    )]
    fn wait_counters(
        &mut self,
        policy: CompletionPolicy,
        submission: Option<&wgpu::SubmissionIndex>,
        deadline: Instant,
    ) -> Result<(), BridgeError> {
        let remaining = deadline.saturating_duration_since(self.now());
        if remaining.is_zero() {
            return Ok(());
        }
        #[cfg(test)]
        if let Some(test) = self.completion_test.as_mut() {
            test.counter_waits.push(remaining);
            test.advance(test.pending_elapsed.min(remaining));
        }
        let waited = self.device.poll(wgpu::PollType::Wait {
            submission_index: submission.cloned(),
            timeout: Some(remaining),
        });
        self.check_health()?;
        waited.map_err(|error| match error {
            wgpu::PollError::Timeout => policy.timed_out(CompletionPhase::CounterSettlement),
            other @ wgpu::PollError::WrongSubmissionIndex(..) => {
                BridgeError::DevicePoll(format!("{policy:?} counter settlement: {other:?}"))
            }
        })?;
        Ok(())
    }

    #[cfg(test)]
    fn before_wait(&mut self, phase: WaitPhase, requested: Duration) -> Result<(), BridgeError> {
        #[cfg(test)]
        if let Some(test) = self.completion_test.as_mut() {
            test.waits.push((phase, requested));
            if phase == WaitPhase::Marker && test.destroy_at_marker {
                self.device.destroy();
                let _ = self.device.poll(wgpu::PollType::Poll);
            }
            if test.failure == Some(phase) {
                return Err(match phase {
                    WaitPhase::Marker => BridgeError::DeadlineExceeded,
                    WaitPhase::Fence => BridgeError::CompletionTimeout {
                        phase: CompletionPhase::Fence,
                    },
                    WaitPhase::MapCallback => BridgeError::CompletionTimeout {
                        phase: CompletionPhase::MapCallback,
                    },
                });
            }
            test.advance(
                match phase {
                    WaitPhase::Fence | WaitPhase::Marker => test.fence_elapsed,
                    WaitPhase::MapCallback => test.callback_elapsed,
                }
                .min(requested),
            );
        }
        Ok(())
    }

    /// The Vello renderer underneath, for its feature and diagnostic
    /// queries.
    #[doc(hidden)]
    pub fn vello(&self) -> &Renderer {
        &self.renderer
    }

    /// The Vello renderer underneath, for the queries that take samples.
    #[doc(hidden)]
    pub fn vello_mut(&mut self) -> &mut Renderer {
        &mut self.renderer
    }

    /// Actual statistics from the latest normal render transaction.
    pub fn last_render_attempts(&self) -> usize {
        self.last_render_attempts
    }

    /// Actual statistics from the latest normal render transaction.
    pub fn last_sparse_render_attempts(&self) -> usize {
        self.last_sparse_render_attempts
    }

    /// Actual statistics from the latest normal render transaction.
    pub fn last_full_render_attempts(&self) -> usize {
        self.last_full_render_attempts
    }

    /// Actual statistics from the latest normal render transaction.
    pub fn last_sparse_render_stats(&self) -> Option<vello::SparseRenderStats> {
        self.last_sparse_render_stats
    }

    /// Adapter identity for the actual same-device renderer.
    pub fn adapter_info(&self) -> BridgeAdapterInfo {
        let info = self.device.adapter_info();
        BridgeAdapterInfo {
            name: info.name,
            backend: format!("{:?}", info.backend),
            device_type: format!("{:?}", info.device_type),
            driver: info.driver,
            vendor: info.vendor,
            device: info.device,
            pci_bus_id: info.device_pci_bus_id,
            #[cfg(target_os = "linux")]
            drm_device: ui::dmabuf_import::vulkan_drm_device_identity(&self.device),
        }
    }

    pub(super) fn allocate_texture(
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> (wgpu::Texture, wgpu::TextureView) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tilo-desktop-drm-paint-target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        (texture, view)
    }
}
