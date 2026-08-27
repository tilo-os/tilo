//! wgpu state, asked for in a size this machine can give.
//!
//! This module exists for one line: [`RenderContext::new_device`] requests the
//! device with [`wgpu::MemoryHints::Manual`] instead of the default.
//!
//! `vello::util::RenderContext` defaults to `MemoryHints::Performance` — right
//! for a game alone on a discrete card, wrong for a qOS session where a dozen
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
}

impl DeviceHandle {
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
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
        format: TextureFormat::Rgba8Unorm,
        view_formats: &[],
    });
    let target_view = target_texture.create_view(&wgpu::TextureViewDescriptor::default());
    (target_texture, target_view)
}

impl RenderContext {
    pub fn new() -> Self {
        // Every field spelled out: `InstanceDescriptor` has no `Default`, and
        // these are the values vello would have used.
        let instance = Instance::new(wgpu::InstanceDescriptor {
            display: None,
            backends: wgpu::Backends::from_env().unwrap_or_default(),
            flags: wgpu::InstanceFlags::from_build_config().with_env(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            backend_options: wgpu::BackendOptions::from_env_or_default(),
        });
        Self {
            instance,
            devices: Vec::new(),
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
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: None,
                required_features: features & maybe_features,
                required_limits: Limits::default(),
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
        self.devices.push(DeviceHandle {
            adapter,
            device,
            queue,
        });
        Some(self.devices.len() - 1)
    }

    /// A surface for `window`, rendered at `width` x `height`.
    pub async fn create_surface<'w>(
        &mut self,
        window: impl Into<SurfaceTarget<'w>>,
        width: u32,
        height: u32,
        present_mode: wgpu::PresentMode,
    ) -> Result<RenderSurface<'w>, CreateSurfaceError> {
        let surface = self
            .instance
            .create_surface(window.into())
            .map_err(|_| CreateSurfaceError::Surface)?;
        let dev_id = self
            .device(Some(&surface))
            .await
            .ok_or(CreateSurfaceError::NoCompatibleDevice)?;
        let handle = &self.devices[dev_id];
        let capabilities = surface.get_capabilities(&handle.adapter);
        let format = capabilities
            .formats
            .into_iter()
            .find(|it| matches!(it, TextureFormat::Rgba8Unorm | TextureFormat::Bgra8Unorm))
            .ok_or(CreateSurfaceError::UnsupportedSurfaceFormat)?;

        let config = SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width,
            height,
            present_mode,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
        };
        let (target_texture, target_view) = create_targets(width, height, &handle.device);
        let surface = RenderSurface {
            surface,
            config,
            dev_id,
            format,
            target_texture,
            target_view,
            blitter: TextureBlitter::new(&handle.device, format),
        };
        self.configure_surface(&surface);
        Ok(surface)
    }

    /// # Panics
    /// If `width` or `height` is zero.
    pub fn resize_surface(&self, surface: &mut RenderSurface<'_>, width: u32, height: u32) {
        let (texture, view) = create_targets(width, height, &self.devices[surface.dev_id].device);
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
        let device = &self.devices[surface.dev_id].device;
        surface.surface.configure(device, &surface.config);
    }
}

/// Why a surface could not be made.
#[derive(Debug, thiserror::Error)]
pub enum CreateSurfaceError {
    #[error("could not create a wgpu surface for this window")]
    Surface,
    #[error("no GPU adapter can present to this surface")]
    NoCompatibleDevice,
    #[error("this surface offers no format vello can render")]
    UnsupportedSurfaceFormat,
}
