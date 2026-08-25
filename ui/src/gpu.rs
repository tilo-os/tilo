//! wgpu state, asked for in a size this machine can actually give.
//!
//! This exists for one line: the device is requested with
//! [`wgpu::MemoryHints::MemoryUsage`] rather than the default.
//!
//! # Why that line is worth a module
//!
//! The machine kept freezing. Not crashing -- freezing, hard, four times
//! in an evening, with nothing written down: no GPU reset, no hung task,
//! no OOM, no panic, three journals stopping mid-sentence. Sampling GPU
//! memory every 500ms and `fsync`ing each line got past that, because a
//! line on the platter survives the power button:
//!
//! ```text
//! 01:46:38  GTT 12357 MB   avail 9892 MB
//! 01:47:17  GTT 15643 MB   avail 6419 MB   <- last line
//! ```
//!
//! GTT is system memory mapped for the GPU; this machine's window onto it
//! is 16404 MB. The plateau at 15643 is that window full. System RAM was
//! fine and swap was untouched, so nothing was out of memory except the
//! GPU -- and an allocation that can neither be served nor fail takes the
//! box with it, which is why the kernel never got to complain.
//!
//! Nothing leaked. A hundred and twenty snapshots moved GTT by zero, and
//! an idle application held flat for a minute. What cost was simply
//! *existing*: one file manager, one window, nothing happening --
//!
//! ```text
//! pid 69707 file-explorer: 488 MB across 459 buffers
//!      256.00 MB x 1  ->  256 MB
//!      128.00 MB x 1  ->  128 MB
//!       64.00 MB x 1  ->   64 MB
//!        0.08 MB x 259 ->  20 MB   <- the application's actual content
//! ```
//!
//! 448 of 488 MB in three buffers nobody asked for. They are suballocator
//! chunks, and their sizes come from `wgpu-hal`'s Vulkan allocator config
//! (`vulkan/adapter.rs`), which reads them off the memory hint:
//!
//! | | `Performance` | `MemoryUsage` |
//! |---|---|---|
//! | first chunk | 128 MB | 8 MB |
//! | largest chunk | 512 MB | 64 MB |
//!
//! `vello::util::RenderContext` builds its device with
//! `..Default::default()`, and that default is `Performance`. It is the
//! right default for a game with one device on a discrete card with
//! gigabytes to spare. A qOS session is the opposite: a dozen small
//! clients sharing one integrated GPU whose memory is the machine's
//! memory. Thirteen clients paying ~450 MB of chunk padding each is the
//! entire 16 GB window, and the thirteenth client is a person opening a
//! photograph.
//!
//! # Why this is not a patched vello
//!
//! It does not need to be. `RenderSurface`'s fields are all public and
//! `Renderer::new` takes a plain `&Device`, so everything vello wants can
//! be built from outside it. What could not be reused is `DeviceHandle`,
//! whose `adapter` field is private -- so the handle is ours and the
//! surface stays vello's. vello is an unmodified dependency.
//!
//! The rest of this file is `vello::util`'s own logic, kept deliberately
//! close to it so the two can be compared line by line when vello moves.

use vello::util::RenderSurface;
use wgpu::{
    Adapter, Device, Instance, Limits, Queue, Surface, SurfaceConfiguration, SurfaceTarget,
    Texture, TextureFormat, TextureView, util::TextureBlitter,
};

/// An adapter and the device opened on it.
///
/// vello's own `DeviceHandle` keeps `adapter` private, which is the only
/// reason this type exists rather than being reused.
pub struct DeviceHandle {
    pub adapter: Adapter,
    pub device: Device,
    pub queue: Queue,
}

impl DeviceHandle {
    /// The adapter this device was opened on.
    ///
    /// A method as well as a field, because vello's handle offers one and
    /// call sites were written against it.
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

/// What the surface is rendered into before being blitted to it.
///
/// vello renders with a compute shader, which cannot bind a surface
/// texture on most backends, so the intermediate is not optional.
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
        // Every field spelled out: `InstanceDescriptor` has no
        // `Default`, and these are the values vello would have used.
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

    /// Index of a device that can present to `compatible_surface`,
    /// opening one if none of the existing devices can.
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
                // The whole reason this module exists. See the header:
                // the default is `Performance`, which reserves in 128MB
                // chunks and grows to 512MB, and a dozen clients doing
                // that on a shared integrated GPU is the machine.
                memory_hints: wgpu::MemoryHints::MemoryUsage,
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
