//! UI render worker thread.
//!
//! Moves scene encode, swapchain acquire, blit, submit, present, and poll
//! off the main winit thread onto a dedicated `ui-render` worker. Main keeps
//! the widget tree + scene encoding; the worker owns the wgpu
//! `RenderContext`, `RenderSurface<'static>`, and `vello::Renderer`.
//!
//! Three channels: `render_tx`/`rx` (capacity 1 — blocks main until the
//! worker drains the previous frame) moves an encoded `Scene` + dims +
//! base colour; `scene_pool_tx`/`scene_pool` (worker → main, capacity 2,
//! pre-seeded) recycles each `Scene`'s buffer instead of reallocating;
//! `outcome_tx`/`frame_complete` (capacity 8) reports per-frame outcomes so
//! main can re-request a redraw on Outdated/Suboptimal/Occluded/Timeout.
//!
//! The worker resizes the surface itself — main never touches it after
//! handing it off at spawn — so wgpu state never crosses threads.
//!
//! On `exiting`, the App sends `RenderRequest::Shutdown` and `join()`s the
//! worker, which drops its `RenderSurface<'static>` before the `Window` is
//! dropped — required or wgpu panics on a surface outliving its window.

use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::JoinHandle;

use crate::gpu::RenderContext;
use crate::offscreen::{damage_texture_extent, damage_texture_needs_replacement};
use vello::kurbo::{Affine, Rect};
use vello::peniko::Color;
use vello::util::RenderSurface;
use vello::{RenderParams, Renderer, Scene};

/// A hook run on the render thread *after* vello encodes the frame into its
/// intermediate texture and *before* that texture is blit to the surface.
/// It may composite extra GPU content into `target` — used by the browser
/// to draw its zero-copy `WebView` texture (which vello cannot sample) over
/// the transparent region the chrome leaves for it.
pub trait FramePostProcess: Send {
    fn post_render(
        &mut self,
        device: &vello::wgpu::Device,
        queue: &vello::wgpu::Queue,
        target: &vello::wgpu::TextureView,
        width: u32,
        height: u32,
    );
}

static POST_PROCESSOR: std::sync::Mutex<Option<Box<dyn FramePostProcess>>> =
    std::sync::Mutex::new(None);
static POST_PROCESSOR_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Called by an app before its window comes up. A no-op for apps that
/// don't set one, so the shared render path is unchanged for everyone else.
pub fn set_frame_post_processor(p: Box<dyn FramePostProcess>) {
    // A poisoned lock means some other caller already panicked while
    // holding it; there's nothing safe left to do but skip installing the
    // processor rather than taking this caller down too.
    if let Ok(mut guard) = POST_PROCESSOR.lock() {
        *guard = Some(p);
        POST_PROCESSOR_ACTIVE.store(true, std::sync::atomic::Ordering::Release);
    }
}

/// Compile-time `Send` check: if a future vello/wgpu version regresses the
/// `Send` impl on `Renderer`/`Scene`, this fails to compile rather than
/// failing at thread spawn. Hand-rolled to avoid a dep on
/// `static_assertions` for one macro.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<Renderer>();
    assert_send::<Scene>();
    assert_send::<RenderSurface<'static>>();
    assert_send::<RenderContext>();
};

#[allow(
    clippy::large_enum_variant,
    reason = "keeping Scene inline removes a heap allocation on every frame; the bounded channel reserves only one slot"
)]
pub enum RenderRequest {
    Frame {
        // Inline deliberately. `Scene` is a collection of Vec/Arc handles:
        // moving it through the bounded channel does not copy their backing
        // data, while boxing it allocated and freed one heap object per frame.
        scene: Scene,
        width: u32,
        height: u32,
        base_color: Color,
        /// Physical-pixel rectangles. Empty means the retained target is
        /// already current and only needs presenting.
        damage_regions: Vec<Rect>,
        /// Resize, relayout, structure, or untracked overlay invalidation.
        full_redraw: bool,
    },
    /// Drain the channel and exit the worker loop. Sent from
    /// `AppHandler::exiting`; the App then `join()`s the handle.
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameOutcome {
    Presented,
    /// Surface was outdated or suboptimal; worker reconfigured it and
    /// skipped the frame. Main should redraw.
    Reconfigured,
    /// Window is occluded or compositor refused the frame; main should
    /// redraw (winit will throttle if still occluded).
    Skipped,
    /// No retry will help; the worker stops after emitting this and main
    /// treats it as "close the window". Ordinary end of session, not a
    /// fault — the compositor exiting destroys the surface mid-frame, and
    /// wgpu's uncaptured-validation-error handler is `panic!`, which under
    /// `panic = "abort"` killed the process. This variant avoids that.
    SurfaceGone,
}

pub struct RenderThreadHandle {
    pub render_tx: SyncSender<RenderRequest>,
    pub scene_pool: Receiver<Scene>,
    pub damage_pool: Receiver<Vec<Rect>>,
    pub frame_complete: Receiver<FrameOutcome>,
    join: Option<JoinHandle<()>>,
}

struct DamageTarget {
    width: u32,
    height: u32,
    texture: vello::wgpu::Texture,
    view: vello::wgpu::TextureView,
}

#[derive(Default)]
struct DamageCache {
    targets: Vec<DamageTarget>,
    translated_scene: Scene,
    last_region_count: Option<usize>,
}

impl RenderThreadHandle {
    /// Idempotent: a second call is a no-op (the `JoinHandle` is taken on
    /// the first). The worker drops its `RenderSurface` before this
    /// returns — it must die before the `Window` it was created against,
    /// or wgpu may panic on Vulkan backends.
    pub fn shutdown_and_join(&mut self) {
        // Best-effort: the worker may already have crashed and dropped the
        // receiver, in which case the send errors and join yields immediately.
        let _ = self.render_tx.send(RenderRequest::Shutdown);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

/// Channel capacities are documented at the module level.
///
/// # Errors
///
/// Returns an error if the OS refuses to spawn the `ui-render` thread (e.g.
/// the process is out of resources for new threads).
pub fn spawn_render_thread(
    render_cx: RenderContext,
    mut surface: RenderSurface<'static>,
    mut renderer: Renderer,
) -> Result<RenderThreadHandle, String> {
    let (req_tx, rx) = mpsc::sync_channel::<RenderRequest>(1);
    let (scene_pool_tx, scene_pool) = mpsc::sync_channel::<Scene>(2);
    let (damage_pool_tx, damage_pool) = mpsc::sync_channel::<Vec<Rect>>(2);
    let (outcome_tx, frame_complete) = mpsc::sync_channel::<FrameOutcome>(8);

    // Seed the pool so main finds a Scene buffer ready on the first frame
    // instead of allocating. The receiver can't have been dropped yet (the
    // handle that would own it isn't constructed until below), so a send
    // failure here is unreachable; if it somehow happened anyway, skipping
    // the seed only costs one extra allocation on an early frame.
    let _ = scene_pool_tx.send(Scene::new());
    let _ = scene_pool_tx.send(Scene::new());
    let _ = damage_pool_tx.send(Vec::with_capacity(8));
    let _ = damage_pool_tx.send(Vec::with_capacity(8));

    let pool_tx = scene_pool_tx.clone();
    let join = std::thread::Builder::new()
        .name("ui-render".into())
        .spawn(move || {
            // `QUICK_UI_PRESENT=1` logs per-frame encode/acquire/present timings
            // — a slow compositor/present path shows up as a large acquire or
            // present, invisible to offscreen benches. Off by default.
            let present_dbg = std::env::var("QUICK_UI_PRESENT").is_ok();
            let mut frame_ix: u64 = 0;
            let mut damage_cache = DamageCache::default();
            let mut target_valid = false;
            while let Ok(msg) = rx.recv() {
                let (mut scene, width, height, base_color, mut damage_regions, full_redraw) =
                    match msg {
                        RenderRequest::Frame {
                            scene,
                            width,
                            height,
                            base_color,
                            damage_regions,
                            full_redraw,
                        } => (
                            scene,
                            width,
                            height,
                            base_color,
                            damage_regions,
                            full_redraw,
                        ),
                        RenderRequest::Shutdown => break,
                    };

                // Resize on the worker so main never touches wgpu state after
                // spawn — simpler than an explicit Resize variant, and keeps
                // every wgpu call on the thread that owns the surface.
                if surface.config.width != width || surface.config.height != height {
                    if let Err(err) =
                        resize_surface_checked(&render_cx, &mut surface, width, height)
                    {
                        // Re-attempting the same reconfigure on a destroyed
                        // surface fails identically forever.
                        tracing::info!(%err, "drawing surface is gone; stopping the render thread");
                        let _ = outcome_tx.send(FrameOutcome::SurfaceGone);
                        break;
                    }
                    target_valid = false;
                    damage_cache = DamageCache::default();
                    renderer.trim_pooled_buffers();
                }

                let outcome = render_one_frame(
                    &render_cx,
                    &surface,
                    &mut renderer,
                    &scene,
                    width,
                    height,
                    base_color,
                    &damage_regions,
                    full_redraw || !target_valid,
                    &mut damage_cache,
                    present_dbg,
                    frame_ix,
                );
                // A per-session frame counter; bounded by wall-clock session
                // length at typical refresh rates, nowhere near `u64::MAX`.
                frame_ix = frame_ix.saturating_add(1);
                target_valid = outcome == FrameOutcome::Presented;

                // `Scene::reset` clears the encoding without freeing the
                // backing vecs, so main avoids a reallocation on reuse.
                scene.reset();
                damage_regions.clear();
                // Non-blocking: at most one Scene is outstanding on main (the
                // one being built), so the pool (capacity 2) never fills. If
                // main has shut down and the receiver is gone, dropping is fine.
                let _ = pool_tx.send(scene);
                let _ = damage_pool_tx.send(damage_regions);
                let _ = outcome_tx.send(outcome);
                if outcome == FrameOutcome::SurfaceGone {
                    break;
                }
            }
            // Worker exits — `surface`, `renderer`, `render_cx` drop here in
            // that order, which is the order wgpu requires (surface first
            // releases the swapchain on the instance `render_cx` owns).
        })
        .map_err(|e| format!("could not spawn the ui-render thread: {e}"))?;
    // pool_tx (the worker's clone) keeps the pool alive; main never sends
    // into it, so the original can be dropped.
    drop(scene_pool_tx);

    Ok(RenderThreadHandle {
        render_tx: req_tx,
        scene_pool,
        damage_pool,
        frame_complete,
        join: Some(join),
    })
}

/// `Surface::configure` returns no `Result` — failures go to the device's
/// error sink and panic with no matching error scope. Pushing the scope
/// here, around this one call, keeps the catch narrow: every other wgpu
/// misuse still panics.
///
/// The failure reads `Surface does not support the adapter's queue family`,
/// which is misleading: wgpu-core reports `UnsupportedQueueFamily`
/// whenever `get_capabilities` errors, and the Vulkan backend returns that
/// for `ERROR_SURFACE_LOST_KHR` too — the actual case here.
fn configure_surface_checked(
    render_cx: &RenderContext,
    surface: &RenderSurface<'static>,
) -> Result<(), String> {
    let Some(device_handle) = render_cx.devices.get(surface.dev_id) else {
        return Err("render device index out of range".to_string());
    };
    let scope = device_handle
        .device
        .push_error_scope(vello::wgpu::ErrorFilter::Validation);
    render_cx.configure_surface(surface);
    match pollster::block_on(scope.pop()) {
        Some(err) => Err(err.to_string()),
        None => Ok(()),
    }
}

/// [`configure_surface_checked`] for the resize path, which reconfigures after
/// swapping the intermediate targets.
fn resize_surface_checked(
    render_cx: &RenderContext,
    surface: &mut RenderSurface<'static>,
    width: u32,
    height: u32,
) -> Result<(), String> {
    let Some(device_handle) = render_cx.devices.get(surface.dev_id) else {
        return Err("render device index out of range".to_string());
    };
    let scope = device_handle
        .device
        .push_error_scope(vello::wgpu::ErrorFilter::Validation);
    render_cx.resize_surface(surface, width, height);
    match pollster::block_on(scope.pop()) {
        Some(err) => Err(err.to_string()),
        None => Ok(()),
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::expect_used,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "damage bounds are clamped to the u32 surface before narrowing; one bounded GPU transaction keeps retained-target correctness visible in one place"
)]
fn rasterize_damage(
    device: &vello::wgpu::Device,
    queue: &vello::wgpu::Queue,
    surface: &RenderSurface<'static>,
    renderer: &mut Renderer,
    scene: &Scene,
    width: u32,
    height: u32,
    base_color: Color,
    damage_regions: &[Rect],
    full_redraw: bool,
    damage_cache: &mut DamageCache,
) -> Result<u64, vello::Error> {
    let full_pixels = u64::from(width).saturating_mul(u64::from(height));
    let render_full = |renderer: &mut Renderer| {
        renderer.render_to_texture(
            device,
            queue,
            scene,
            &surface.target_view,
            &RenderParams {
                base_color,
                width,
                height,
                antialiasing_method: vello::AaConfig::Area,
            },
        )
    };
    if full_redraw {
        if damage_cache
            .last_region_count
            .is_some_and(|previous| previous != 0)
        {
            renderer.trim_pooled_buffers();
        }
        damage_cache.targets.clear();
        damage_cache.last_region_count = Some(0);
        render_full(renderer)?;
        return Ok(full_pixels);
    }
    if damage_regions.is_empty() {
        return Ok(0);
    }

    let mut damage_pixels = 0u64;
    let mut damage_region_count = 0usize;
    for rect in damage_regions {
        let x0 = rect.x0.floor().max(0.0).min(f64::from(width)) as u32;
        let y0 = rect.y0.floor().max(0.0).min(f64::from(height)) as u32;
        let x1 = rect.x1.ceil().max(0.0).min(f64::from(width)) as u32;
        let y1 = rect.y1.ceil().max(0.0).min(f64::from(height)) as u32;
        let region_width = x1.saturating_sub(x0);
        let region_height = y1.saturating_sub(y0);
        damage_pixels = damage_pixels
            .saturating_add(u64::from(region_width).saturating_mul(u64::from(region_height)));
        damage_region_count = damage_region_count
            .saturating_add(usize::from(region_width != 0 && region_height != 0));
    }
    // A large union costs less as one Vello pass and must not retain a second
    // near-window-sized texture. This is both the time and memory bound.
    if damage_pixels == 0 || damage_pixels >= full_pixels.saturating_div(2) {
        if damage_cache
            .last_region_count
            .is_some_and(|previous| previous != 0)
        {
            renderer.trim_pooled_buffers();
        }
        damage_cache.targets.clear();
        damage_cache.last_region_count = Some(0);
        render_full(renderer)?;
        return Ok(full_pixels);
    }

    if damage_cache
        .last_region_count
        .is_some_and(|previous| previous != damage_region_count)
    {
        renderer.trim_pooled_buffers();
    }
    damage_cache.last_region_count = Some(damage_region_count);

    let texture_limit = device.limits().max_texture_dimension_2d;

    let mut target_index = 0usize;
    for rect in damage_regions {
        let x0 = rect.x0.floor().max(0.0).min(f64::from(width)) as u32;
        let y0 = rect.y0.floor().max(0.0).min(f64::from(height)) as u32;
        let x1 = rect.x1.ceil().max(0.0).min(f64::from(width)) as u32;
        let y1 = rect.y1.ceil().max(0.0).min(f64::from(height)) as u32;
        let region_width = x1.saturating_sub(x0);
        let region_height = y1.saturating_sub(y0);
        if region_width == 0 || region_height == 0 {
            continue;
        }
        let recreate = damage_cache.targets.get(target_index).is_none_or(|target| {
            damage_texture_needs_replacement(
                target.width,
                target.height,
                region_width,
                region_height,
            )
        });
        if recreate {
            if let Some(target) = damage_cache.targets.get(target_index) {
                renderer.invalidate_texture_bindings(&target.view);
            }
            let allocated_width = damage_texture_extent(region_width, texture_limit);
            let allocated_height = damage_texture_extent(region_height, texture_limit);
            let texture = device.create_texture(&vello::wgpu::TextureDescriptor {
                label: Some("ui-native-damage-target"),
                size: vello::wgpu::Extent3d {
                    width: allocated_width,
                    height: allocated_height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: vello::wgpu::TextureDimension::D2,
                format: vello::wgpu::TextureFormat::Rgba8Unorm,
                usage: vello::wgpu::TextureUsages::STORAGE_BINDING
                    | vello::wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&vello::wgpu::TextureViewDescriptor::default());
            let replacement = DamageTarget {
                width: allocated_width,
                height: allocated_height,
                texture,
                view,
            };
            if let Some(target) = damage_cache.targets.get_mut(target_index) {
                *target = replacement;
            } else {
                damage_cache.targets.push(replacement);
            }
        }
        let target = damage_cache
            .targets
            .get(target_index)
            .expect("damage target was inserted above");
        damage_cache.translated_scene.reset();
        damage_cache.translated_scene.append(
            scene,
            Some(Affine::translate((-(f64::from(x0)), -(f64::from(y0))))),
        );
        renderer.render_to_texture(
            device,
            queue,
            &damage_cache.translated_scene,
            &target.view,
            &RenderParams {
                base_color,
                width: region_width,
                height: region_height,
                antialiasing_method: vello::AaConfig::Area,
            },
        )?;
        let mut encoder = device.create_command_encoder(&vello::wgpu::CommandEncoderDescriptor {
            label: Some("ui-copy-native-damage"),
        });
        encoder.copy_texture_to_texture(
            vello::wgpu::TexelCopyTextureInfo {
                texture: &target.texture,
                mip_level: 0,
                origin: vello::wgpu::Origin3d::ZERO,
                aspect: vello::wgpu::TextureAspect::All,
            },
            vello::wgpu::TexelCopyTextureInfo {
                texture: &surface.target_texture,
                mip_level: 0,
                origin: vello::wgpu::Origin3d { x: x0, y: y0, z: 0 },
                aspect: vello::wgpu::TextureAspect::All,
            },
            vello::wgpu::Extent3d {
                width: region_width,
                height: region_height,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
        target_index = target_index.saturating_add(1);
    }
    damage_cache.targets.truncate(target_index);
    Ok(damage_pixels)
}

// One call site: this is the whole worker's per-frame encode+present step,
// and each parameter is a distinct, independently necessary piece of GPU
// state or frame metadata. Bundling would trade nine named parameters for
// an equally large ad-hoc struct with no behavioural change.
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "one render-thread frame transaction; splitting it would separate acquire/encode/present state and timings"
)]
fn render_one_frame(
    render_cx: &RenderContext,
    surface: &RenderSurface<'static>,
    renderer: &mut Renderer,
    scene: &Scene,
    width: u32,
    height: u32,
    base_color: Color,
    damage_regions: &[Rect],
    full_redraw: bool,
    damage_cache: &mut DamageCache,
    present_dbg: bool,
    frame_ix: u64,
) -> FrameOutcome {
    use std::time::Instant;
    let Some(device_handle) = render_cx.devices.get(surface.dev_id) else {
        tracing::info!("render device index out of range; stopping the render thread");
        return FrameOutcome::SurfaceGone;
    };

    // Acquire before encoding. An occluded, timed-out, outdated, or destroyed
    // swapchain cannot consume the frame; discovering that only after Vello
    // packed and uploaded the scene wasted the entire GPU pass.
    let t_acquire = present_dbg.then(Instant::now);
    let surface_texture = match surface.surface.get_current_texture() {
        vello::wgpu::CurrentSurfaceTexture::Success(t) => t,
        vello::wgpu::CurrentSurfaceTexture::Outdated
        | vello::wgpu::CurrentSurfaceTexture::Suboptimal(_) => {
            // Surface drifted (resize, scale change). Reconfigure
            // and ask main to redraw.
            if let Err(err) = configure_surface_checked(render_cx, surface) {
                tracing::info!(%err, "drawing surface is gone; abandoning the frame");
                return FrameOutcome::SurfaceGone;
            }
            return FrameOutcome::Reconfigured;
        }
        vello::wgpu::CurrentSurfaceTexture::Occluded
        | vello::wgpu::CurrentSurfaceTexture::Timeout => {
            return FrameOutcome::Skipped;
        }
        vello::wgpu::CurrentSurfaceTexture::Lost
        | vello::wgpu::CurrentSurfaceTexture::Validation => {
            // Same shutdown race as the reconfigure above; panicking here
            // used to abort the process for an ordinary end of session.
            tracing::info!("drawing surface is no longer usable; stopping the render thread");
            return FrameOutcome::SurfaceGone;
        }
    };

    let acquire_us = t_acquire.map(|t| t.elapsed().as_secs_f64() * 1e6);

    let t_encode = present_dbg.then(Instant::now);
    let raster_pixels = match rasterize_damage(
        &device_handle.device,
        &device_handle.queue,
        surface,
        renderer,
        scene,
        width,
        height,
        base_color,
        damage_regions,
        full_redraw || POST_PROCESSOR_ACTIVE.load(std::sync::atomic::Ordering::Acquire),
        damage_cache,
    ) {
        Ok(pixels) => pixels,
        Err(err) => {
            // A programmer bug (bad encoding / ran out of GPU resources); log and
            // skip rather than panic, which would tear down the app with no
            // chance for main to clean up.
            tracing::error!(?err, "vello render_to_texture failed");
            return FrameOutcome::Skipped;
        }
    };

    // Composite any post-processor content (the browser's zero-copy WebView
    // texture) into the intermediate texture before it goes to the surface.
    if POST_PROCESSOR_ACTIVE.load(std::sync::atomic::Ordering::Acquire)
        && let Ok(mut guard) = POST_PROCESSOR.lock()
        && let Some(p) = guard.as_mut()
    {
        p.post_render(
            &device_handle.device,
            &device_handle.queue,
            &surface.target_view,
            width,
            height,
        );
    }

    let encode_us = t_encode.map(|t| t.elapsed().as_secs_f64() * 1e6);

    let t_submit = present_dbg.then(Instant::now);
    let mut encoder =
        device_handle
            .device
            .create_command_encoder(&vello::wgpu::CommandEncoderDescriptor {
                label: Some("ui-render-thread blit-to-surface"),
            });
    surface.blitter.copy(
        &device_handle.device,
        &mut encoder,
        &surface.target_view,
        &surface_texture
            .texture
            .create_view(&vello::wgpu::TextureViewDescriptor::default()),
    );
    device_handle.queue.submit([encoder.finish()]);

    let submit_us = t_submit.map(|t| t.elapsed().as_secs_f64() * 1e6);

    let t_present = present_dbg.then(Instant::now);
    surface_texture.present();
    if present_dbg {
        let present_us = t_present.map_or(0.0, |t| t.elapsed().as_secs_f64() * 1e6);
        let encoding = scene.encoding();
        let scene_bytes = std::mem::size_of_val(encoding.path_tags.as_slice())
            .saturating_add(std::mem::size_of_val(encoding.path_data.as_slice()))
            .saturating_add(std::mem::size_of_val(encoding.draw_tags.as_slice()))
            .saturating_add(std::mem::size_of_val(encoding.draw_data.as_slice()))
            .saturating_add(std::mem::size_of_val(encoding.transforms.as_slice()))
            .saturating_add(std::mem::size_of_val(encoding.styles.as_slice()))
            .saturating_add(std::mem::size_of_val(encoding.resources.patches.as_slice()))
            .saturating_add(std::mem::size_of_val(
                encoding.resources.color_stops.as_slice(),
            ))
            .saturating_add(std::mem::size_of_val(encoding.resources.glyphs.as_slice()))
            .saturating_add(std::mem::size_of_val(
                encoding.resources.glyph_runs.as_slice(),
            ))
            .saturating_add(std::mem::size_of_val(
                encoding.resources.normalized_coords.as_slice(),
            ));
        // The present pass samples every RGBA8 pixel from Vello's target into
        // the swapchain. This is GPU-local traffic, not a CPU readback.
        let blit_bytes = u64::from(width)
            .saturating_mul(u64::from(height))
            .saturating_mul(4);
        eprintln!(
            "present-frame {frame_ix}: acquire={:.0}us encode={:.0}us submit={:.0}us present={:.0}us scene={}B paths={} draw_tags={} raster_pixels={} gpu_blit={}B ({}x{})",
            acquire_us.unwrap_or(0.0),
            encode_us.unwrap_or(0.0),
            submit_us.unwrap_or(0.0),
            present_us,
            scene_bytes,
            encoding.n_paths,
            encoding.draw_tags.len(),
            raster_pixels,
            blit_bytes,
            width,
            height,
        );
    }
    // `Poll` is the non-blocking variant — nudges the queue without stalling.
    let _ = device_handle.device.poll(vello::wgpu::PollType::Poll);
    FrameOutcome::Presented
}
