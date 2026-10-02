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
//! reimplementing, for its private `adapter` field. vello stays unmodified,
//! and this file stays close to `vello::util`'s own logic so the two can be
//! compared when vello moves.

use std::sync::{Arc, Mutex};

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
    #[cfg(all(windows, feature = "dx12-shader-cache"))]
    shader_cache: Option<crate::dx12_shader_cache::CacheSession>,
}

/// Shared, read-only observation of the context-owned device loss callback.
#[derive(Clone, Default)]
pub struct DeviceLossMonitor(Arc<Mutex<Option<String>>>);

impl DeviceLossMonitor {
    /// Actual device loss reason, when the context callback has observed one.
    pub fn reason(&self) -> Option<String> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
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
}

/// wgpu state for one process, with a memory hint that fits a shared GPU.
pub struct RenderContext {
    pub instance: Instance,
    pub devices: Vec<DeviceHandle>,
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

impl RenderContext {
    pub fn new() -> Self {
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
        // Every field spelled out: `InstanceDescriptor` has no `Default`, and
        // these are the values vello would have used.
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
        let instance = Instance::new(wgpu::InstanceDescriptor {
            display: None,
            // Use the host's native graphics API on Windows. Probing every
            // backend can enter optional Vulkan translation-layer drivers
            // before a working DX12 adapter is selected. Keep WGPU_BACKEND
            // available for explicit diagnostics and alternate drivers.
            backends: wgpu::Backends::from_env().unwrap_or_else(|| {
                if cfg!(target_os = "windows") {
                    wgpu::Backends::DX12
                } else if cfg!(target_os = "android") {
                    // Every device the app installs on has Vulkan 1.1.
                    wgpu::Backends::VULKAN
                } else {
                    wgpu::Backends::default()
                }
            }),
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
            backend_options,
        });
        Self {
            instance,
            devices: Vec::new(),
            #[cfg(all(windows, feature = "dx12-shader-cache"))]
            compiler_identity,
        }
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
        let adapter =
            wgpu::util::initialize_adapter_from_env_or_default(&self.instance, compatible_surface)
                .await
                .ok()?;
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
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: None,
                required_features: (features & maybe_features) | timestamp_features,
                required_limits: required_limits(&adapter.limits()),
                // See the module header for the default's cost. `MemoryUsage`
                // (8 MB first block, 64 MB largest) is still not enough,
                // because the block size doubles with every block a process
                // opens (`min << active_general_blocks`, capped at the
                // maximum): one idle app held 4+8+16+32+64 = 124 MB of blocks
                // to suballocate ~50 MB of buffers out of.
                //
                // Fixed 4 MB blocks stop the doubling. Anything larger gets a
                // dedicated allocation of its own exact size instead
                // (`gpu_allocator`'s `requires_personal_block`), so big
                // buffers cost what they are. 4 MB is the floor wgpu clamps to.
                memory_hints: wgpu::MemoryHints::Manual {
                    suballocated_device_memory_block_size: (4 << 20)..(4 << 20),
                },
                ..Default::default()
            })
            .await
            .ok()?;
        let loss = DeviceLossMonitor::default();
        let observer = loss.clone();
        device.set_device_lost_callback(move |reason, message| {
            let mut recorded = observer
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
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
            #[cfg(all(windows, feature = "dx12-shader-cache"))]
            shader_cache,
        });
        Some(self.devices.len().saturating_sub(1))
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
        tilo_startup_metrics::mark("surface-native-create-start");
        let surface = self
            .instance
            .create_surface(window.into())
            .map_err(CreateSurfaceError::Surface)?;
        tilo_startup_metrics::mark("surface-native-create-done");
        tilo_startup_metrics::mark("surface-device-select-start");
        let dev_id = self
            .device(Some(&surface))
            .await
            .ok_or(CreateSurfaceError::NoCompatibleDevice)?;
        tilo_startup_metrics::mark("surface-device-select-done");
        let handle = self
            .devices
            .get(dev_id)
            .ok_or(CreateSurfaceError::NoCompatibleDevice)?;
        tilo_startup_metrics::mark("surface-capabilities-start");
        let capabilities = surface.get_capabilities(&handle.adapter);
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
        tilo_startup_metrics::mark("surface-blitter-create-start");
        let blitter = TextureBlitter::new(&handle.device, format);
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
        tilo_startup_metrics::mark("surface-initial-configure-start");
        self.configure_surface(&surface);
        tilo_startup_metrics::mark("surface-initial-configure-done");
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

    pub fn set_present_mode(
        &self,
        surface: &mut RenderSurface<'_>,
        present_mode: wgpu::PresentMode,
    ) {
        surface.config.present_mode = present_mode;
        self.configure_surface(surface);
    }

    pub fn configure_surface(&self, surface: &RenderSurface<'_>) {
        let Some(handle) = self.devices.get(surface.dev_id) else {
            return;
        };
        surface.surface.configure(&handle.device, &surface.config);
    }
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
    use super::create_targets;
    use crate::offscreen::shared_gpu;

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
