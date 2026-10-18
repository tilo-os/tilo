//! wgpu state, asked for in a size this machine can give.
//!
//! This module exists for one line: `RenderContext::new_device` requests the
//! device with [`wgpu::MemoryHints::Manual`] instead of the default.
//!
//! `vello::util::RenderContext` defaults to `MemoryHints::Performance` — right
//! for a game alone on a discrete card, wrong for a Tilo session where a dozen
//! clients share one integrated GPU whose memory is the machine's memory.
//! Measured on an idle file manager: 488 MB across 459 buffers, of which
//! 448 MB was three suballocator chunks (256 + 128 + 64 MB) and only 20 MB
//! was actual content. Thirteen clients paying that is the whole 16 GB GTT
//! window, and a GPU allocation that can neither be served nor fail takes
//! the box with it — no OOM, no panic, just a freeze.
//!
//! Not a patched vello: `RenderSurface`'s fields are public and
//! `Renderer::new` takes a plain `&Device`, so only `DeviceHandle` needed
//! reimplementing, for its private `adapter` field. The one change to
//! `vello::util` is that `RenderSurface::blitter` is optional, so a surface
//! the target reaches by a texture copy compiles no blit pipeline
//! (`vendor/PATCHES.md`); this file stays close to `vello::util`'s own logic
//! so the two can be compared when vello moves.
//!
//! It also decides which backends the instance is made with
//! (`default_backends`): without OpenGL whenever OpenGL could not be
//! chosen anyway, because wgpu initialises every backend it is asked for.

use std::sync::{Arc, Mutex};

use tilo_lock::MutexRecover as _;
use vello::util::RenderSurface;
use wgpu::{
    Adapter, Device, Instance, Limits, Queue, Surface, SurfaceConfiguration, SurfaceTarget,
    Texture, TextureFormat, TextureView, util::TextureBlitter,
};

/// An adapter and the device opened on it.
///
/// vello's own `DeviceHandle` keeps `adapter` private, which is the only reason
/// this type exists rather than being reused.
pub struct DeviceHandle {
    pub adapter: Adapter,
    pub device: Device,
    pub queue: Queue,
    loss: DeviceLossMonitor,
    /// Whether a window surface has been reported to use this adapter: a
    /// device opened ahead of its window is reported when one first takes it.
    surface_reported: bool,
    #[cfg(all(windows, feature = "dx12-shader-cache"))]
    shader_cache: Option<crate::dx12_shader_cache::CacheSession>,
}

/// Shared, read-only observation of the context-owned device loss callback.
#[derive(Clone, Default)]
pub struct DeviceLossMonitor(Arc<Mutex<Option<String>>>);

impl DeviceLossMonitor {
    /// Actual device loss reason, when the context callback has observed one.
    pub fn reason(&self) -> Option<String> {
        self.0.lock_recover().clone()
    }
}

impl DeviceHandle {
    #[cfg(all(windows, feature = "dx12-shader-cache"))]
    pub(crate) fn finish_shader_cache(&self) {
        crate::dx12_shader_cache::report(&self.device);
        if let Some(cache) = &self.shader_cache {
            cache.finish(&self.device);
        }
    }

    /// Observe loss without replacing the context-owned callback.
    pub fn loss_monitor(&self) -> DeviceLossMonitor {
        self.loss.clone()
    }

    /// A method as well as a field, because vello's handle offers one and call
    /// sites were written against it.
    pub fn adapter(&self) -> &Adapter {
        &self.adapter
    }

    /// The same driver's device on a fresh instance of this adapter's backend, with this
    /// device's features and limits. Blocks while the device opens.
    ///
    /// Not a second device on this adapter: Mesa's CPU Vulkan driver compiles shaders for every
    /// device of one physical device one at a time, so work meant to run beside this device's
    /// needs a physical device of its own.
    pub fn open_peer(&self) -> Option<(Device, Queue)> {
        let info = self.adapter.get_info();
        let backends = wgpu::Backends::from(info.backend);
        let instance = Instance::new(wgpu::InstanceDescriptor {
            display: None,
            backends,
            flags: wgpu::InstanceFlags::from_build_config().with_env(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            backend_options: wgpu::BackendOptions::from_env_or_default(),
        });
        let adapter = pollster::block_on(instance.enumerate_adapters(backends))
            .into_iter()
            .find(|adapter| {
                let peer = adapter.get_info();
                (peer.name.as_str(), peer.driver.as_str(), peer.device_type)
                    == (info.name.as_str(), info.driver.as_str(), info.device_type)
            })?;
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: None,
            required_features: self.device.features(),
            required_limits: self.device.limits(),
            memory_hints: MEMORY_HINTS,
            ..Default::default()
        }))
        .ok()
    }
}

/// See the module header for the default's cost. `MemoryUsage` (8 MB first block, 64 MB
/// largest) is still not enough, because the block size doubles with every block a process opens
/// (`min << active_general_blocks`, capped at the maximum): one idle app held
/// 4+8+16+32+64 = 124 MB of blocks to suballocate ~50 MB of buffers out of.
///
/// Fixed 4 MB blocks stop the doubling. Anything larger gets a dedicated allocation of its own
/// exact size instead (`gpu_allocator`'s `requires_personal_block`), so big buffers cost what
/// they are. 4 MB is the floor wgpu clamps to.
const MEMORY_HINTS: wgpu::MemoryHints = wgpu::MemoryHints::Manual {
    suballocated_device_memory_block_size: (4 << 20)..(4 << 20),
};

/// wgpu state for one process, with a memory hint that fits a shared GPU.
pub struct RenderContext {
    pub instance: Instance,
    pub devices: Vec<DeviceHandle>,
    /// Backends to start `instance` again with if its own offer no adapter
    /// at all, before any device or surface came from it (`default_backends`).
    fallback: Option<wgpu::Backends>,
    backend_options: wgpu::BackendOptions,
    #[cfg(all(windows, feature = "dx12-shader-cache"))]
    compiler_identity: Option<Arc<crate::dx12_shader_cache::CompilerIdentity>>,
}

impl Default for RenderContext {
    fn default() -> Self {
        Self::new()
    }
}

/// What the surface is rendered into before being blitted to it. vello renders
/// with a compute shader, which cannot bind a surface texture on most backends,
/// so the intermediate is not optional.
fn create_targets(width: u32, height: u32, device: &Device) -> (Texture, TextureView) {
    let target_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("quick-ui-surface-target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        usage: wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST,
        format: TextureFormat::Rgba8Unorm,
        view_formats: &[],
    });
    let target_view = target_texture.create_view(&wgpu::TextureViewDescriptor::default());
    (target_texture, target_view)
}

/// wgpu's defaults, except the two a renderer's scratch buffers are bound
/// against, which come from the adapter.
///
/// The default `max_storage_buffer_binding_size` is 128 MiB whatever the
/// hardware can do, and vello binds each bump buffer whole. A 5120x2880
/// output legitimately needs more than that; asking for what the adapter
/// supports (4 GiB on RADV) costs nothing until a buffer is allocated.
fn required_limits(adapter: &Limits) -> Limits {
    Limits {
        max_storage_buffer_binding_size: adapter.max_storage_buffer_binding_size,
        max_buffer_size: adapter.max_buffer_size,
        ..Limits::default()
    }
}

/// The backends a context's instance is made with, and those it is made
/// again with when the first offer no adapter at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BackendChoice {
    pub(crate) primary: wgpu::Backends,
    pub(crate) fallback: Option<wgpu::Backends>,
}

impl BackendChoice {
    const fn only(backends: wgpu::Backends) -> Self {
        Self {
            primary: backends,
            fallback: None,
        }
    }
}

/// The backends a context starts with when `WGPU_BACKEND` names none.
///
/// Windows uses DX12 alone: probing every backend can enter optional Vulkan
/// translation-layer drivers before a working DX12 adapter is selected.
/// Every Android device the app installs on has Vulkan 1.1. A native process
/// elsewhere leaves OpenGL out whenever it could not be chosen anyway
/// (`native::gpu_backends`).
fn default_backends() -> BackendChoice {
    if cfg!(target_os = "windows") {
        BackendChoice::only(wgpu::Backends::DX12)
    } else if cfg!(target_os = "android") {
        BackendChoice::only(wgpu::Backends::VULKAN)
    } else {
        native_backends()
    }
}

#[cfg(feature = "native")]
fn native_backends() -> BackendChoice {
    crate::native::gpu_backends::default_choice()
}

#[cfg(not(feature = "native"))]
fn native_backends() -> BackendChoice {
    BackendChoice::only(wgpu::Backends::all())
}

/// Whether `WGPU_ADAPTER_NAME` picks the adapter (`RenderContext::request_adapter`).
#[cfg(feature = "native")]
fn adapter_named_by_env() -> bool {
    crate::native::gpu_backends::adapter_named()
}

/// Without a process environment to read, assume it may.
#[cfg(not(feature = "native"))]
fn adapter_named_by_env() -> bool {
    true
}

/// An instance of `backends`, with every field spelled out:
/// `InstanceDescriptor` has no `Default`, and these are the values vello
/// would have used.
fn create_instance(backends: wgpu::Backends, backend_options: &wgpu::BackendOptions) -> Instance {
    let instance = Instance::new(wgpu::InstanceDescriptor {
        display: None,
        backends,
        flags: {
            let flags = wgpu::InstanceFlags::from_build_config();
            // The Android emulator's Vulkan driver crashes in
            // vkSetDebugUtilsObjectNameEXT; `WGPU_DEBUG=1` still asks
            // for object labels.
            #[cfg(target_os = "android")]
            let flags = flags.difference(wgpu::InstanceFlags::DEBUG);
            flags.with_env()
        },
        memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        backend_options: backend_options.clone(),
    });
    tilo_startup_metrics::mark("gpu-instance-created");
    instance
}

impl RenderContext {
    /// A context on the backends `WGPU_BACKEND` names, or this platform's
    /// `default_backends`. `WGPU_BACKEND` stays available for explicit
    /// diagnostics and alternate drivers, and is never second-guessed.
    pub fn new() -> Self {
        Self::with_backends(
            wgpu::Backends::from_env().map_or_else(default_backends, BackendChoice::only),
        )
    }

    pub(crate) fn with_backends(choice: BackendChoice) -> Self {
        let backend_options = wgpu::BackendOptions::from_env_or_default();
        #[cfg(windows)]
        let backend_options = {
            let mut options = backend_options;
            // Prefer the pinned app-local compiler, independent of the
            // working directory or ambient SDK/PATH. Keep an explicit
            // diagnostic compiler override and unbundled wgpu fallback.
            if wgpu::Dx12Compiler::from_env().is_none()
                && let Some(compiler) = std::env::current_exe()
                    .ok()
                    .and_then(|exe| exe.parent().map(|path| path.join("dxcompiler.dll")))
                    .filter(|path| path.is_file())
            {
                options.dx12.shader_compiler = wgpu::Dx12Compiler::DynamicDxc {
                    dxc_path: compiler.to_string_lossy().into_owned(),
                };
            }
            options
        };
        #[cfg(all(windows, feature = "dx12-shader-cache"))]
        let compiler_identity =
            crate::dx12_shader_cache::CompilerIdentity::open(&backend_options.dx12.shader_compiler);
        #[cfg(all(windows, feature = "dx12-shader-cache"))]
        let backend_options = {
            let mut options = backend_options;
            if let Some(compiler) = &compiler_identity {
                options.dx12.shader_compiler = compiler.compiler();
            }
            options
        };
        Self {
            instance: create_instance(choice.primary, &backend_options),
            devices: Vec::new(),
            fallback: choice.fallback,
            backend_options,
            #[cfg(all(windows, feature = "dx12-shader-cache"))]
            compiler_identity,
        }
    }

    /// Before the first device or surface comes from `instance`: if it may
    /// still be replaced and offers no adapter at all, make it again with the
    /// fallback backends, once. Nothing of the first instance exists yet, so
    /// nothing of it can meet the new one; afterwards the instance stays.
    /// Free once settled.
    async fn settle_backends(&mut self) {
        let Some(fallback) = self.fallback.take() else {
            return;
        };
        if self.devices.is_empty()
            && self
                .instance
                .enumerate_adapters(wgpu::Backends::all())
                .await
                .is_empty()
        {
            tilo_startup_metrics::mark("gpu-instance-fallback");
            self.instance = create_instance(fallback, &self.backend_options);
        }
    }

    /// The adapter for a new device. The first device decides the instance
    /// for good (`settle_backends`).
    ///
    /// Asking wgpu for an adapter enumerates every adapter, and so does
    /// settling. So the first request goes to the primary instance at once,
    /// and the instance is settled (and the request repeated) only when that
    /// finds nothing. With `WGPU_ADAPTER_NAME` set it is settled first
    /// instead: wgpu then panics on an instance that offers no adapter
    /// rather than failing.
    async fn request_adapter(
        &mut self,
        compatible_surface: Option<&Surface<'_>>,
    ) -> Option<wgpu::Adapter> {
        if self.fallback.is_some()
            && self.devices.is_empty()
            && !adapter_named_by_env()
            && let Ok(adapter) = wgpu::util::initialize_adapter_from_env_or_default(
                &self.instance,
                compatible_surface,
            )
            .await
        {
            // It offers an adapter, so it stays.
            self.fallback = None;
            return Some(adapter);
        }
        self.settle_backends().await;
        wgpu::util::initialize_adapter_from_env_or_default(&self.instance, compatible_surface)
            .await
            .ok()
    }

    /// Index of a device that can present to `compatible_surface`, opening one
    /// if none of the existing devices can.
    pub async fn device(&mut self, compatible_surface: Option<&Surface<'_>>) -> Option<usize> {
        let compatible = match compatible_surface {
            Some(s) => self
                .devices
                .iter()
                .enumerate()
                .find(|(_, d)| d.adapter.is_surface_supported(s))
                .map(|(i, _)| i),
            None => (!self.devices.is_empty()).then_some(0),
        };
        if compatible.is_none() {
            return self.new_device(compatible_surface).await;
        }
        compatible
    }

    async fn new_device(&mut self, compatible_surface: Option<&Surface<'_>>) -> Option<usize> {
        tilo_startup_metrics::mark("gpu-adapter-request-start");
        let adapter = self.request_adapter(compatible_surface).await?;
        tilo_startup_metrics::mark("gpu-adapter-request-done");
        if compatible_surface.is_some() {
            tilo_startup_metrics::mark("gpu-adapter-report-start");
            report_adapter(&self.instance, &adapter).await;
            tilo_startup_metrics::mark("gpu-adapter-report-done");
        }
        let features = adapter.features();
        let maybe_features = wgpu::Features::CLEAR_TEXTURE | wgpu::Features::PIPELINE_CACHE;
        // Timestamp queries allocate profile-only GPU resources and are not a
        // baseline rendering requirement. Opt in explicitly, and leave the
        // ordinary feature request bit-for-bit unchanged.
        let diagnostics = &crate::host::environment().diagnostics;
        let gpu_timestamps_requested = diagnostics.gpu_timestamps;
        let dispatch_timestamps_requested = diagnostics.gpu_dispatch_timestamps;
        let timestamp_feature = wgpu::Features::TIMESTAMP_QUERY;
        let inside_pass_feature = wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES;
        let timestamp_features = if gpu_timestamps_requested && features.contains(timestamp_feature)
        {
            if dispatch_timestamps_requested && features.contains(inside_pass_feature) {
                timestamp_feature | inside_pass_feature
            } else {
                if dispatch_timestamps_requested {
                    eprintln!(
                        "tilo: VELLO_GPU_DISPATCH_TIMESTAMPS=1 requested, but this adapter does not support TIMESTAMP_QUERY_INSIDE_PASSES"
                    );
                }
                timestamp_feature
            }
        } else {
            if gpu_timestamps_requested {
                eprintln!(
                    "tilo: VELLO_GPU_TIMESTAMPS=1 requested, but this adapter does not support TIMESTAMP_QUERY"
                );
            }
            if dispatch_timestamps_requested {
                eprintln!(
                    "tilo: VELLO_GPU_DISPATCH_TIMESTAMPS=1 requires VELLO_GPU_TIMESTAMPS=1 with TIMESTAMP_QUERY"
                );
            }
            wgpu::Features::empty()
        };
        tilo_startup_metrics::mark("gpu-device-request-start");
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: None,
                required_features: (features & maybe_features) | timestamp_features,
                required_limits: required_limits(&adapter.limits()),
                memory_hints: MEMORY_HINTS,
                ..Default::default()
            })
            .await
            .ok()?;
        tilo_startup_metrics::mark("gpu-device-request-done");
        let loss = DeviceLossMonitor::default();
        let observer = loss.clone();
        device.set_device_lost_callback(move |reason, message| {
            let mut recorded = observer.0.lock_recover();
            if recorded.is_none() {
                *recorded = Some(format!("{reason:?}: {message}"));
            }
        });
        #[cfg(all(windows, feature = "dx12-shader-cache"))]
        let shader_cache =
            crate::dx12_shader_cache::CacheSession::load(&device, self.compiler_identity.clone());
        self.devices.push(DeviceHandle {
            adapter,
            device,
            queue,
            loss,
            surface_reported: compatible_surface.is_some(),
            #[cfg(all(windows, feature = "dx12-shader-cache"))]
            shader_cache,
        });
        Some(self.devices.len().saturating_sub(1))
    }

    async fn surface_device(
        &mut self,
        surface: &Surface<'_>,
    ) -> Option<(usize, wgpu::SurfaceCapabilities)> {
        // is_surface_supported performs a complete capabilities query. Reuse
        // its result for configuration when an existing device can present.
        for (id, handle) in self.devices.iter_mut().enumerate() {
            let capabilities = surface_capabilities(surface, &handle.adapter);
            if !capabilities.formats.is_empty() {
                // A device prepared without a surface (the startup renderer)
                // still names the adapter its first window renders on: GPU
                // health and the startup gate read that line.
                if !handle.surface_reported {
                    handle.surface_reported = true;
                    report_adapter(&self.instance, &handle.adapter).await;
                }
                return Some((id, capabilities));
            }
        }
        // Preserve surface-aware selection and explicit adapter preferences
        // for a fresh or incompatible device, including non-Vulkan backends.
        let id = self.new_device(Some(surface)).await?;
        let handle = self.devices.get(id)?;
        Some((id, surface_capabilities(surface, &handle.adapter)))
    }

    /// A surface for `window`, rendered at `width` x `height`.
    ///
    /// # Errors
    /// Returns [`CreateSurfaceError::Surface`] if wgpu cannot create a surface for
    /// `window`, [`CreateSurfaceError::NoCompatibleDevice`] if no adapter can present
    /// to it, or [`CreateSurfaceError::UnsupportedSurfaceFormat`] if the surface
    /// offers neither `Rgba8Unorm` nor `Bgra8Unorm`.
    pub async fn create_surface<'w>(
        &mut self,
        window: impl Into<SurfaceTarget<'w>>,
        width: u32,
        height: u32,
        present_mode: wgpu::PresentMode,
    ) -> Result<RenderSurface<'w>, CreateSurfaceError> {
        let surface = self
            .create_surface_unconfigured(window, width, height, present_mode)
            .await?;
        tilo_startup_metrics::mark("surface-initial-configure-start");
        self.configure_surface(&surface);
        tilo_startup_metrics::mark("surface-initial-configure-done");
        Ok(surface)
    }

    /// Build a surface and render targets without creating its swapchain.
    ///
    /// A caller that adjusts alpha or other configuration must configure the
    /// returned surface before acquiring a frame. Native windows do that once
    /// inside their existing validation error scope.
    ///
    /// # Errors
    /// Returns the same surface, device and format errors as [`Self::create_surface`].
    pub async fn create_surface_unconfigured<'w>(
        &mut self,
        window: impl Into<SurfaceTarget<'w>>,
        width: u32,
        height: u32,
        present_mode: wgpu::PresentMode,
    ) -> Result<RenderSurface<'w>, CreateSurfaceError> {
        self.settle_backends().await;
        tilo_startup_metrics::mark("surface-native-create-start");
        let surface = self
            .instance
            .create_surface(window.into())
            .map_err(CreateSurfaceError::Surface)?;
        tilo_startup_metrics::mark("surface-native-create-done");
        tilo_startup_metrics::mark("surface-device-select-start");
        let (dev_id, capabilities) = self
            .surface_device(&surface)
            .await
            .ok_or(CreateSurfaceError::NoCompatibleDevice)?;
        tilo_startup_metrics::mark("surface-device-select-done");
        let handle = self
            .devices
            .get(dev_id)
            .ok_or(CreateSurfaceError::NoCompatibleDevice)?;
        tilo_startup_metrics::mark("surface-capabilities-start");
        // The intermediate target is RGBA8. Prefer a matching surface format
        // so capable backends can present it with a texture copy instead of
        // compiling a fullscreen blit shader for every new surface.
        let format = if capabilities.formats.contains(&TextureFormat::Rgba8Unorm) {
            TextureFormat::Rgba8Unorm
        } else if capabilities.formats.contains(&TextureFormat::Bgra8Unorm) {
            TextureFormat::Bgra8Unorm
        } else {
            return Err(CreateSurfaceError::UnsupportedSurfaceFormat);
        };
        let direct_copy = format == TextureFormat::Rgba8Unorm
            && capabilities.usages.contains(wgpu::TextureUsages::COPY_DST);
        tilo_startup_metrics::mark("surface-capabilities-done");

        let config = SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | if direct_copy {
                    wgpu::TextureUsages::COPY_DST
                } else {
                    wgpu::TextureUsages::empty()
                },
            format,
            width,
            height,
            present_mode,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
        };
        tilo_startup_metrics::mark("surface-targets-create-start");
        let (target_texture, target_view) = create_targets(width, height, &handle.device);
        tilo_startup_metrics::mark("surface-targets-create-done");
        // A surface the target reaches by a texture copy never blits: its
        // blit pipeline would be compiled on every window's way to its first
        // frame for nothing.
        tilo_startup_metrics::mark("surface-blitter-create-start");
        let blitter = (!direct_copy).then(|| TextureBlitter::new(&handle.device, format));
        tilo_startup_metrics::mark("surface-blitter-create-done");
        let surface = RenderSurface {
            surface,
            config,
            dev_id,
            format,
            target_texture,
            target_view,
            blitter,
        };
        Ok(surface)
    }

    /// # Panics
    /// If `width` or `height` is zero.
    pub fn resize_surface(&self, surface: &mut RenderSurface<'_>, width: u32, height: u32) {
        let Some(handle) = self.devices.get(surface.dev_id) else {
            return;
        };
        // Release the old target before allocating the new one: on WebGPU a
        // dropped texture stays resident until JavaScript collects it.
        surface.target_texture.destroy();
        let (texture, view) = create_targets(width, height, &handle.device);
        surface.target_texture = texture;
        surface.target_view = view;
        surface.config.width = width;
        surface.config.height = height;
        self.configure_surface(surface);
    }

    pub fn configure_surface(&self, surface: &RenderSurface<'_>) {
        let Some(handle) = self.devices.get(surface.dev_id) else {
            return;
        };
        surface.surface.configure(&handle.device, &surface.config);
    }
}

/// Put `surface`'s render target into the acquired `texture` of its
/// swapchain: a texture copy when the surface was configured for one, else
/// its blit.
pub fn draw_target_to_surface(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    surface: &RenderSurface<'_>,
    texture: &Texture,
) {
    match surface.blitter.as_ref() {
        Some(blitter) => blitter.copy(
            device,
            encoder,
            &surface.target_view,
            &texture.create_view(&wgpu::TextureViewDescriptor::default()),
        ),
        None => encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &surface.target_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: surface.config.width,
                height: surface.config.height,
                depth_or_array_layers: 1,
            },
        ),
    }
}

/// Prefix of every line a window prints about the adapter it renders on.
/// The compositor reads app stderr for it; session checks grep for it.
pub const ADAPTER_LINE_PREFIX: &str = "tilo-gpu:";

/// What follows [`ADAPTER_LINE_PREFIX`] when a window presents through a CPU
/// adapter on a machine that has a GPU.
pub const SOFTWARE_FALLBACK_MESSAGE: &str =
    "window renders on a CPU adapter although a GPU is present";

/// The one line naming a window's adapter, e.g.
/// `tilo-gpu: window adapter "Turnip Adreno (TM) X1-85" type=IntegratedGpu backend=Vulkan driver="turnip Mesa driver"`.
fn adapter_line(info: &wgpu::AdapterInfo) -> String {
    format!(
        "{ADAPTER_LINE_PREFIX} window adapter {:?} type={:?} backend={:?} driver={:?}",
        info.name, info.device_type, info.backend, info.driver
    )
}

fn surface_capabilities(surface: &Surface<'_>, adapter: &Adapter) -> wgpu::SurfaceCapabilities {
    tilo_startup_metrics::mark("gpu-surface-capabilities-start");
    let capabilities = surface.get_capabilities(adapter);
    tilo_startup_metrics::mark("gpu-surface-capabilities-done");
    capabilities
}

/// The hardware adapters a window passed over for a CPU one, or nothing when
/// the choice was not a fallback.
///
/// Mesa's hardware Vulkan drivers present on Wayland only through
/// `zwp_linux_dmabuf_v1`; without it they report the surface lost and the
/// only adapter left that can present is lavapipe, which rasterises every
/// frame on the CPU and JIT-compiles every shader on the first one. That cost
/// seconds per launch on a Snapdragon laptop and nothing reported it.
fn passed_over_hardware(
    chosen: wgpu::DeviceType,
    available: impl IntoIterator<Item = (wgpu::DeviceType, String)>,
) -> Vec<String> {
    if chosen != wgpu::DeviceType::Cpu {
        return Vec::new();
    }
    available
        .into_iter()
        .filter(|(kind, _)| {
            matches!(
                kind,
                wgpu::DeviceType::IntegratedGpu
                    | wgpu::DeviceType::DiscreteGpu
                    | wgpu::DeviceType::VirtualGpu
                    | wgpu::DeviceType::Other
            )
        })
        .map(|(_, name)| name)
        .collect()
}

/// Name the adapter a window renders on, and call it an error when it is a
/// software fallback. Enumerating again costs a few milliseconds, so it only
/// happens once the choice is already a CPU adapter.
async fn report_adapter(instance: &Instance, chosen: &Adapter) {
    let info = chosen.get_info();
    crate::diag!("{}", adapter_line(&info));
    if info.device_type != wgpu::DeviceType::Cpu {
        return;
    }
    let available = instance
        .enumerate_adapters(wgpu::Backends::all())
        .await
        .into_iter()
        .map(|adapter| {
            let other = adapter.get_info();
            (
                other.device_type,
                format!("{} ({:?})", other.name, other.backend),
            )
        });
    let hardware = passed_over_hardware(info.device_type, available);
    if hardware.is_empty() {
        return;
    }
    // Plain stderr, not only `tracing`: an app's subscriber prints errors,
    // but the compositor matches this exact line to raise it in GPU health.
    crate::diag!(
        "{ADAPTER_LINE_PREFIX} {SOFTWARE_FALLBACK_MESSAGE}: {:?} chosen over {} \
         because none of them can present to this window (does the compositor \
         advertise zwp_linux_dmabuf_v1 on their render node?)",
        info.name,
        hardware.join(", ")
    );
}

/// Why a surface could not be made.
#[derive(Debug, thiserror::Error)]
pub enum CreateSurfaceError {
    #[error("could not create a wgpu surface for this window: {0}")]
    Surface(#[source] wgpu::CreateSurfaceError),
    #[error("no GPU adapter can present to this surface")]
    NoCompatibleDevice,
    #[error("this surface offers no format vello can render")]
    UnsupportedSurfaceFormat,
}

#[cfg(test)]
mod tests {
    use super::{BackendChoice, RenderContext, create_targets, passed_over_hardware};
    use wgpu::Backends;

    #[test]
    fn a_context_whose_backends_offer_no_adapter_starts_again_on_its_fallback_once() {
        // The production fallback, from an instance that can offer nothing to
        // the backends of this machine's real adapter.
        let mut context = RenderContext::with_backends(BackendChoice {
            primary: Backends::empty(),
            fallback: Some(Backends::all()),
        });
        let index = pollster::block_on(context.device(None))
            .expect("the fallback instance opens this machine's adapter");
        assert_eq!(index, 0);
        assert!(
            context.fallback.is_none(),
            "an instance is only ever made again once"
        );

        // An instance that offers an adapter is kept: falling back here
        // would leave this context with no backend at all.
        let mut kept = RenderContext::with_backends(BackendChoice {
            primary: Backends::all(),
            fallback: Some(Backends::empty()),
        });
        assert!(
            pollster::block_on(kept.device(None)).is_some(),
            "an instance with an adapter is never replaced"
        );
        assert!(kept.fallback.is_none());
    }
    use crate::offscreen::shared_gpu;
    use wgpu::DeviceType;

    #[test]
    fn a_cpu_adapter_beside_a_gpu_is_reported_as_a_fallback() {
        let adapters = || {
            [
                (DeviceType::IntegratedGpu, "Turnip Adreno X1-85".to_owned()),
                (DeviceType::Cpu, "llvmpipe".to_owned()),
            ]
        };
        assert_eq!(
            passed_over_hardware(DeviceType::Cpu, adapters()),
            ["Turnip Adreno X1-85"]
        );
        assert!(passed_over_hardware(DeviceType::IntegratedGpu, adapters()).is_empty());
        assert!(
            passed_over_hardware(DeviceType::Cpu, [(DeviceType::Cpu, "llvmpipe".to_owned())])
                .is_empty(),
            "a machine with only a software adapter has nothing to fall back from"
        );
    }

    #[test]
    fn gpu_context_shares_device_loss_callbacks_and_accepts_native_damage() {
        // Case `device_loss_observers_share_the_context_callback`.
        {
            let mut context = super::RenderContext::new();
            let index = pollster::block_on(context.device(None)).expect("real GPU adapter");
            let handle = &context.devices[index];
            let first = handle.loss_monitor();
            let second = handle.loss_monitor();
            assert!(first.reason().is_none());
            handle.device.destroy();
            let _ = handle.device.poll(wgpu::PollType::Poll);
            assert!(
                first.reason().is_some(),
                "context must record actual destroyed-device loss"
            );
            assert_eq!(first.reason(), second.reason());
        }

        // Case `live_retained_target_accepts_native_damage_copy`.
        {
            let gpu = shared_gpu().expect("shared GPU context for live-target copy test");
            let (target, _) = create_targets(4, 4, &gpu.device);
            assert!(target.usage().contains(wgpu::TextureUsages::COPY_DST));
            let source = gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("gpu-test-copy-source"),
                size: wgpu::Extent3d {
                    width: 4,
                    height: 4,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
            let mut encoder = gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("gpu-test-copy-into-live-target"),
                });
            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &source,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &target,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: 4,
                    height: 4,
                    depth_or_array_layers: 1,
                },
            );
            gpu.queue.submit([encoder.finish()]);
            assert!(
                pollster::block_on(scope.pop()).is_none(),
                "live target rejected COPY_DST damage copy"
            );
        }
    }
}
