//! The service's device: opened once when the service starts, warm for
//! every app of the workspace after that. Every frame is drawn on it; a
//! software adapter (a machine with no GPU, or none the service may use)
//! rasterizes the frame on the CPU inside the same device and its buffers
//! are shared memory rather than dma-bufs.
//!
//! On a software adapter the device's pipelines are compiled before the
//! service says it is ready, on extra devices from the session's budget
//! ([`crate::warmup`]).

use vello::wgpu;
use vello::{AaSupport, Renderer, RendererOptions};

/// The device, its renderer and the allocator its buffers come from.
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub renderer: Renderer,
    /// The adapter rasterizes in software (no GPU in this machine, or none
    /// the service may use): frames go into shared memory, not dma-bufs.
    pub software: bool,
    pub adapter: wgpu::AdapterInfo,
    loss: ui::gpu::DeviceLossMonitor,
    #[cfg(target_os = "linux")]
    pub allocator: Option<crate::buffers::Allocator>,
    /// Keeps the instance and the device handle alive.
    _context: ui::gpu::RenderContext,
}

/// Why the device could not be opened.
#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    #[error("no adapter")]
    NoAdapter,
    #[error("the renderer could not start: {0}")]
    Renderer(String),
    #[error("the buffer allocator could not open {path}: {error}")]
    Allocator { path: String, error: std::io::Error },
}

impl Gpu {
    /// Open the device the compositor composites on, whose render node is
    /// `render_node`, build every vello pipeline now (the service exists to
    /// have them warm), and open a GBM allocator on the same node.
    pub fn open(render_node: Option<&std::path::Path>) -> Result<Self, GpuError> {
        let mut context = ui::gpu::RenderContext::new();
        let index = pollster::block_on(context.device(None)).ok_or(GpuError::NoAdapter)?;
        let handle = context.devices.get(index).ok_or(GpuError::NoAdapter)?;
        let device = handle.device.clone();
        let queue = handle.queue.clone();
        let adapter = handle.adapter.get_info();
        let loss = handle.loss_monitor();
        let software = ui::gpu::rasterizes_on_cpu(&adapter);
        let renderer = new_renderer(&device)?;
        #[cfg(target_os = "linux")]
        if software {
            // Lavapipe compiles each pipeline on its first dispatch: on a cold shader cache,
            // seconds of one-at-a-time work a first frame drawn by the renderer would wait on.
            let slots = crate::warmup::acquire();
            crate::warmup::precompile(&renderer, (&device, &queue), slots.devices(), &|| {
                handle.open_peer()
            });
            drop(slots);
        }
        #[cfg(target_os = "linux")]
        let allocator = match render_node.filter(|_| !software) {
            Some(path) => {
                Some(
                    crate::buffers::allocator(path).map_err(|error| GpuError::Allocator {
                        path: path.display().to_string(),
                        error,
                    })?,
                )
            }
            None => None,
        };
        #[cfg(not(target_os = "linux"))]
        let _ = render_node;
        Ok(Self {
            device,
            queue,
            renderer,
            software,
            adapter,
            loss,
            #[cfg(target_os = "linux")]
            allocator,
            _context: context,
        })
    }

    /// Why the device was lost, once it was.
    pub fn lost(&self) -> Option<String> {
        self.loss.reason()
    }

    /// Wait until everything submitted finished, and say whether it did. A GPU that finishes
    /// nothing for [`ui::gpu::NOT_PROGRESSING_AFTER`] has stopped, and nothing else would ever
    /// say so: its device is declared lost ([`Self::lost`]).
    pub fn finish(&self) -> bool {
        let waited = ui::gpu::NOT_PROGRESSING_AFTER;
        match self.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(waited),
        }) {
            Ok(_) => true,
            Err(wgpu::PollError::Timeout) => {
                ui::gpu::LossWatch::new(self.loss.clone()).stopped(self.queue.clone(), waited);
                false
            }
            Err(_) => self.lost().is_none(),
        }
    }

    /// A new renderer on the same device, after the old one's state stopped
    /// being trustworthy.
    ///
    /// # Errors
    ///
    /// When the pipelines cannot be built.
    pub fn rebuild_renderer(&mut self) -> Result<(), GpuError> {
        self.renderer = new_renderer(&self.device)?;
        Ok(())
    }

    /// Whether frames go into dma-bufs.
    pub fn dmabufs(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            !self.software && self.allocator.is_some()
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }
}

/// Every pipeline built now: the service exists to have them warm when an
/// app's first frame arrives.
fn new_renderer(device: &wgpu::Device) -> Result<Renderer, GpuError> {
    Renderer::new(
        device,
        RendererOptions {
            use_cpu: false,
            antialiasing_support: AaSupport::area_only(),
            num_init_threads: std::num::NonZeroUsize::new(2),
            pipeline_cache: None,
        },
    )
    .map_err(|error| GpuError::Renderer(error.to_string()))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// A GPU whose firmware stopped answering finishes nothing and reports
    /// nothing: the service used to wait for it for ever, and every app it
    /// drew for froze with it. Tests run on lavapipe, never a real GPU.
    #[test]
    fn a_gpu_that_stops_finishing_work_ends_the_wait_as_a_lost_device() {
        let gpu = Gpu::open(None).expect("a wgpu adapter");
        assert!(gpu.finish(), "a working GPU finishes");
        // SAFETY: the hal device is only flagged; no raw handle leaves this block.
        unsafe { gpu.device.as_hal::<wgpu::hal::api::Vulkan>() }
            .expect("tests render through Vulkan")
            .simulate_hang();
        let encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        gpu.queue.submit([encoder.finish()]);
        assert!(!gpu.finish(), "work that never finishes ends the wait");
        assert!(
            gpu.lost()
                .is_some_and(|reason| reason.contains("stopped finishing work")),
            "the device is lost, so the service ends and is started again"
        );
    }

    const MIB: u64 = 1 << 20;

    /// Map `bytes` of fresh device memory, write all of it, and free it.
    #[allow(
        clippy::expect_used,
        reason = "a test helper: a failure is the failing test"
    )]
    fn touch_and_free(device: &wgpu::Device, bytes: u64) {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("touched then freed"),
            size: bytes,
            usage: wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: true,
        });
        let len = usize::try_from(bytes).expect("a buffer this process can map");
        buffer
            .slice(..)
            .get_mapped_range_mut()
            .copy_from_slice(&vec![0x5A; len]);
        buffer.unmap();
        drop(buffer);
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
    }

    /// Shmem the CPU driver's device memory holds in this process: lavapipe
    /// keeps every device's memory in a memfd named `allocation fd`, whose
    /// pages stay charged to the process until they are punched out of it.
    #[allow(
        clippy::expect_used,
        reason = "a test helper: a failure is the failing test"
    )]
    fn driver_memory_bytes() -> u64 {
        use std::os::unix::fs::MetadataExt as _;
        let mut seen = std::collections::BTreeSet::new();
        std::fs::read_dir("/proc/self/fd")
            .expect("the process's descriptors")
            .filter_map(Result::ok)
            .filter(|entry| {
                std::fs::read_link(entry.path())
                    .is_ok_and(|target| target.as_os_str() == "/memfd:allocation fd (deleted)")
            })
            .filter_map(|entry| std::fs::metadata(entry.path()).ok())
            .filter(|meta| seen.insert(meta.ino()))
            .map(|meta| meta.blocks().saturating_mul(512))
            .sum()
    }

    /// The memory a closed window's buffers and textures held on a software
    /// GPU goes back to the kernel when they are freed; it is not kept
    /// charged to the service until it exits.
    #[test]
    fn device_memory_that_is_freed_goes_back_to_the_kernel() {
        let gpu = Gpu::open(None).expect("a wgpu adapter");
        assert!(gpu.software, "tests run on lavapipe");
        let baseline = driver_memory_bytes();
        // Larger than a suballocation block, so each is an allocation of its
        // own, freed with it: what closing a window does with its target.
        let live = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("a window's target"),
            size: wgpu::Extent3d {
                width: 2048,
                height: 2048,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        gpu.queue.write_texture(
            live.as_image_copy(),
            &vec![0x5A; 16 << 20],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(2048 * 4),
                rows_per_image: None,
            },
            live.size(),
        );
        gpu.queue.submit([]);
        assert!(gpu.finish());
        let held = driver_memory_bytes();
        assert!(
            held >= baseline.saturating_add(15 * MIB),
            "a live texture's pages are charged to the driver's memory: {baseline} -> {held}"
        );
        drop(live);
        for _ in 0..4 {
            touch_and_free(&gpu.device, 24 * MIB);
        }
        assert!(gpu.finish());
        let freed = driver_memory_bytes();
        assert!(
            freed <= baseline.saturating_add(MIB),
            "freed device memory stayed charged: {baseline} before, {held} with the texture, \
             {freed} after it and 96 MiB of buffers were freed"
        );
    }

    /// The warm-up's extra devices compile at the same time: each is opened
    /// while the others are, on a thread of its own, never one after another.
    #[test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the arrival guard is handed to the condvar's wait, which releases it"
    )]
    fn the_warm_up_compiles_on_its_extra_devices_at_once() {
        use std::sync::{Condvar, Mutex};
        use std::time::Duration;

        const HELPERS: usize = 3;
        let mut context = ui::gpu::RenderContext::new();
        let index = pollster::block_on(context.device(None)).expect("a wgpu adapter");
        let handle = &context.devices[index];
        let renderer = new_renderer(&handle.device).expect("the renderer builds");
        // Each helper waits at the door until every other helper is there too:
        // helpers opened one after another would never all meet.
        let arrived = Mutex::new(0_usize);
        let all_here = Condvar::new();
        let met = Mutex::new(0_usize);
        crate::warmup::precompile(&renderer, (&handle.device, &handle.queue), HELPERS, &|| {
            let mut count = arrived.lock().expect("not poisoned");
            *count += 1;
            all_here.notify_all();
            let all_met = !all_here
                .wait_timeout_while(count, Duration::from_secs(30), |count| *count < HELPERS)
                .expect("not poisoned")
                .1
                .timed_out();
            if all_met {
                *met.lock().expect("not poisoned") += 1;
            }
            handle.open_peer()
        });
        assert_eq!(
            *met.lock().expect("not poisoned"),
            HELPERS,
            "every extra device was opened while the others were"
        );
    }
}
