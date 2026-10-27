//! The service's device: opened once when the service starts, warm for
//! every app of the room after that.

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
    pub allocator: Option<gbm::Device<std::fs::File>>,
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
        let software = ui::render_thread::rasterizes_on_cpu(&adapter);
        let renderer = new_renderer(&device)?;
        #[cfg(target_os = "linux")]
        let allocator = match render_node.filter(|_| !software) {
            Some(path) => {
                let file = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(path)
                    .map_err(|error| GpuError::Allocator {
                        path: path.display().to_string(),
                        error,
                    })?;
                Some(gbm::Device::new(file).map_err(|error| GpuError::Allocator {
                    path: path.display().to_string(),
                    error,
                })?)
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
}
