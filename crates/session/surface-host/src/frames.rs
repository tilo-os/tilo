//! Where a hosted surface's frame is drawn.
//!
//! A surface app's scene is drawn the way every toolkit app's frame is: by
//! the toolkit's own renderer ([`ui::offscreen::OffscreenRenderer`]), and
//! delivered as an app's frame is, in dma-bufs on the compositor's GPU
//! (`tilo_render_service::buffers`) that the compositor composites straight
//! from: the pixels never leave the GPU. What the frame shows (which cells
//! hold anything, which are opaque, which pixels take the pointer) is read on
//! the GPU too, and only that comes back. Where the machine has no GPU the
//! compositor reads them from, the toolkit's CPU rasterizer draws the frame,
//! exactly as it draws a normal app's frame there. A device lost under the
//! renderer gives way to the same CPU rasterizer, so a GPU that stops leaves
//! the surface slower rather than stopped. Only what changed is drawn, so a
//! moved cursor, a new time or a resized panel costs what it changed rather
//! than the whole surface.

use std::sync::Arc;

use tilo_render_protocol::control::DmabufFormat;
use tilo_render_service::buffers::{self as delivery, Allocator, BufferSet, History, Memory};
use tilo_session_core::surface::{CELL, HostBuffer, HostBuffers, HostFrame, Region};

use ui::offscreen::OffscreenRenderer;
use vello::Scene;
use vello::kurbo::Rect;
use vello::peniko::Color;
use vello::wgpu::{self, AdapterInfo};

/// The colour a surface draws on: nothing, so the desktop shows through
/// wherever the app painted nothing.
const TRANSPARENT: Color = Color::TRANSPARENT;

/// Cores the CPU rasterizer uses where it has them.
const RASTER_THREADS: u16 = 2;

/// One hosted surface's frames: what it drew last, and what draws its next.
pub(crate) struct Frames {
    size: (u32, u32),
    /// What draws them, or `None` where this build's toolkit has no
    /// rasterizer to draw a surface with.
    target: Option<Target>,
    /// Whether [`Self::target`] was chosen: as the first frame is drawn,
    /// once the compositor said where it composites from.
    chosen: bool,
    /// What the compositor draws: the surface's pixels, changed where it
    /// changed.
    frame: HostFrame,
}

impl Frames {
    /// Frames for a surface of `size` physical pixels. Nothing is drawn
    /// before [`Self::present`].
    pub(crate) fn new(size: (u32, u32)) -> Self {
        // The first surface comes before the compositor's display; which GPU
        // draws is chosen with the first frame, once the display is up.
        ui::offscreen::load_drivers();
        Self {
            size,
            target: None,
            chosen: false,
            frame: HostFrame::new(size),
        }
    }

    /// The surface is now `size` physical pixels: the frames drawn for the
    /// old size are not its own.
    pub(crate) fn resize(&mut self, size: (u32, u32)) {
        if size == self.size {
            return;
        }
        self.size = size;
        self.target = None;
        self.chosen = false;
        self.frame = HostFrame::new(size);
    }

    /// The frame as the compositor shows it.
    pub(crate) fn frame(&self) -> HostFrame {
        self.frame.clone()
    }

    pub(crate) const fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Whether the last frame drawn shows anything at one pixel: where it
    /// shows nothing, the pointer reaches what lies beneath.
    pub(crate) fn covers(&self, x: u32, y: u32) -> bool {
        match &self.target {
            Some(Target::Gpu(gpu)) => gpu.covers(x, y),
            _ => self.frame.alpha(x, y).is_some_and(|alpha| alpha > 0),
        }
    }

    /// Draw `scene` (physical pixels) where `damage` says it changed (`None`:
    /// all of it) and hand the frame what was drawn, telling it what
    /// changed.
    pub(crate) fn present(&mut self, scene: &Scene, damage: Option<&[Rect]>) {
        if !self.chosen {
            self.chosen = true;
            self.target = Target::new(self.size);
            self.frame = frame_of(self.target.as_ref(), self.size);
        }
        // A device that stopped working is noticed where the frames are
        // drawn, and the frames go on: on the software rasteriser where the
        // GPU cannot be brought back, so the surface stays there rather than
        // drawing into a dead device.
        if let Some(reason) = self.target.as_ref().and_then(Target::device_loss) {
            self.target = self
                .target
                .take()
                .and_then(|target| target.after_loss(&reason, self.size));
            self.frame = frame_of(self.target.as_ref(), self.size);
        }
        match self.target.as_mut() {
            Some(Target::Gpu(gpu)) => gpu.draw(scene, damage, &self.frame),
            Some(Target::Cpu(cpu)) => {
                let (width, height) = cpu.size();
                let asked = damage.map(|rects| {
                    regions(rects, width, height)
                        .into_iter()
                        .map(|region| (region.x, region.y, region.width, region.height))
                        .collect::<Vec<_>>()
                });
                if let Some(drawn) = cpu.draw(scene, asked.as_deref()) {
                    let drawn: Vec<Region> = drawn
                        .into_iter()
                        .map(|(x, y, width, height)| Region::new(x, y, width, height))
                        .collect();
                    self.frame.set_pixels(cpu.pixels(), &drawn);
                }
            }
            None => {}
        }
    }
}

/// The frame a surface drawing on `target` hands the compositor: its dma-bufs
/// where it draws on the GPU, its pixels otherwise.
fn frame_of(target: Option<&Target>, size: (u32, u32)) -> HostFrame {
    match target {
        Some(Target::Gpu(gpu)) => HostFrame::on_gpu(size, Arc::clone(&gpu.shared)),
        _ => HostFrame::new(size),
    }
}

/// Where a surface's frames are drawn: on the GPU, or on the toolkit's CPU
/// rasterizer where the machine has no GPU to draw them on.
enum Target {
    Gpu(Box<Gpu>),
    Cpu(Box<ui::offscreen::CpuSurface>),
}

impl Target {
    /// The drawing `size` frames: on the GPU where the machine has one to
    /// draw on and the compositor composites from buffers on it, on the CPU
    /// otherwise; `None` where this build's toolkit has no rasterizer to draw
    /// one with.
    fn new(size: (u32, u32)) -> Option<Self> {
        let node = tilo_compositor::gpu_frames_node().filter(|node| {
            // The renderer draws on the GPU the compositor composites on:
            // memory one GPU allocated is not another's to draw into.
            prefer_render_node(node);
            draws_on_gpu()
        });
        if let Some(gpu) = node.and_then(|node| Gpu::new(size, node)) {
            return Some(Self::Gpu(Box::new(gpu)));
        }
        tracing::info!("surface apps' frames are drawn on the CPU: no GPU to draw them on");
        Self::cpu(size)
    }

    /// The toolkit's CPU rasterizer drawing a `size` surface: what a machine
    /// with no GPU to draw on draws with, and what a surface draws with once
    /// its GPU cannot be brought back. `None` where this build has no
    /// rasterizer.
    fn cpu(size: (u32, u32)) -> Option<Self> {
        let canvas = ui::offscreen::CpuSurface::new(size.0, size.1, RASTER_THREADS)?;
        Some(Self::Cpu(Box::new(canvas)))
    }

    /// Why this target's GPU device is gone, while it is: what the next frame
    /// is not drawn on.
    fn device_loss(&self) -> Option<String> {
        match self {
            Self::Gpu(gpu) => gpu.renderer.device_loss(),
            Self::Cpu(_) => None,
        }
    }

    /// What draws a `size` surface after this target lost its GPU device for
    /// `reason`: the answer `ui::gpu::recover` decides, and the drawing that
    /// fits it — the CPU rasterizer once the GPU cannot be brought back, the
    /// GPU again after a reset that left it working.
    fn after_loss(self, reason: &str, size: (u32, u32)) -> Option<Self> {
        let Self::Gpu(gpu) = self else {
            return Self::new(size);
        };
        let recovery = ui::gpu::recover(&gpu.adapter, gpu.finished_frames, reason);
        tracing::error!(%reason, ?recovery, "a hosted surface's GPU device was lost");
        match recovery {
            // The GPU was reset and works again: open a renderer on it. A
            // device that stays lost ends here again on the next pass, then
            // with no frame finished and gives way to the CPU.
            ui::gpu::Recovery::SameAdapter => Self::new(size),
            ui::gpu::Recovery::Software => Self::cpu(size),
        }
    }
}

/// The DRM render node the compositor composites on: every device this
/// process opens is that GPU's, so what it draws is handed over in that GPU's
/// memory.
static COMPOSITORS_GPU: std::sync::OnceLock<(u32, u32)> = std::sync::OnceLock::new();

/// Open this process's devices on the compositor's GPU: `node` is the render
/// node it composites from (`/dev/dri/renderD128`).
fn prefer_render_node(node: &std::path::Path) {
    let Some(numbers) = tilo_presentation::device_numbers(node) else {
        return;
    };
    let _ = COMPOSITORS_GPU.set(numbers);
    ui::gpu::prefer_adapter(on_compositors_gpu);
}

/// Whether `adapter` is the GPU behind the compositor's render node.
fn on_compositors_gpu(adapter: &wgpu::Adapter) -> bool {
    COMPOSITORS_GPU.get().is_some_and(|node| {
        ui::dmabuf_import::vulkan_drm_adapter_identity(adapter)
            .is_some_and(|identity| identity.render == Some(*node))
    })
}

/// The renderer, the buffers its frames are delivered in and what the last
/// frame shows.
struct Gpu {
    renderer: OffscreenRenderer,
    /// The adapter this renderer draws on: what a loss of its device is
    /// decided by ([`ui::gpu::recover`]).
    adapter: AdapterInfo,
    /// Whether this renderer finished at least one frame before the loss: a
    /// device that did was reset and works again, one that did not is on a
    /// GPU that cannot be brought back.
    finished_frames: bool,
    /// The dma-bufs frames are delivered in, imported on the renderer's
    /// device; dropped before their allocator.
    buffers: BufferSet,
    /// The same buffers as the compositor imports them.
    shared: Arc<HostBuffers>,
    /// What the last frames changed, for a buffer that missed them.
    history: History,
    coverage: Coverage,
    /// Which pixels of the last frame show anything: a bit each, `words`
    /// words a row.
    mask: Vec<u32>,
    words: u32,
    _allocator: Allocator,
}

impl Gpu {
    /// The renderer for a surface of `size` physical pixels and its buffers
    /// on the render node `node`, if the toolkit has an adapter to draw it on
    /// and the buffers import there: an emulated adapter is no GPU, and the
    /// toolkit rasterizes on the CPU there too.
    fn new(size: (u32, u32), node: &std::path::Path) -> Option<Self> {
        if size.0 == 0 || size.1 == 0 {
            return None;
        }
        let renderer = match OffscreenRenderer::try_new_live(size.0, size.1) {
            Ok(renderer) => renderer,
            Err(error) => {
                tracing::warn!(%error, "a hosted surface has no renderer to draw on");
                return None;
            }
        };
        let adapter = renderer
            .adapter_info()
            .filter(|info| !ui::gpu::rasterizes_on_cpu(info))?;
        let (device, _, _) = renderer.target();
        let Some(numbers) = tilo_presentation::device_numbers(node) else {
            tracing::warn!(?node, "hosted frames: no device node to draw on");
            return None;
        };
        if !ui::dmabuf_import::is_on_render_node(&device, numbers) {
            tracing::warn!(adapter = %adapter.name, ?node, "hosted frames: the renderer is not on the compositor's GPU");
            return None;
        }
        tracing::info!(adapter = %adapter.name, ?node, "hosted frames are drawn on the GPU");
        let allocator = delivery::allocator(node)
            .map_err(|error| tracing::warn!(%error, ?node, "hosted frames: no allocator"))
            .ok()?;
        let buffers = BufferSet::dmabufs(
            (&device, &allocator),
            size,
            DmabufFormat {
                fourcc: delivery::DRM_FORMAT_ABGR8888,
                modifier: delivery::DRM_FORMAT_MOD_LINEAR,
            },
            0,
        )
        .map_err(|error| tracing::warn!(%error, "hosted frames: no buffers on the GPU"))
        .ok()?;
        let shared = Arc::new(HostBuffers {
            fourcc: delivery::DRM_FORMAT_ABGR8888,
            modifier: delivery::DRM_FORMAT_MOD_LINEAR,
            buffers: buffers
                .buffers
                .iter()
                .zip(&buffers.planes)
                .map(|(buffer, plane)| {
                    Some(HostBuffer {
                        fd: buffer.fd().try_clone_to_owned().ok()?,
                        offset: plane.offset,
                        stride: plane.stride,
                    })
                })
                .collect::<Option<_>>()?,
        });
        let words = size.0.div_ceil(32);
        Some(Self {
            coverage: Coverage::new(&device, size),
            renderer,
            adapter,
            finished_frames: false,
            buffers,
            shared,
            history: History::default(),
            mask: vec![0; (words as usize).saturating_mul(size.1 as usize)],
            words,
            _allocator: allocator,
        })
    }

    /// Whether the last frame shows anything at one pixel.
    fn covers(&self, x: u32, y: u32) -> bool {
        if x >= self.renderer.width || y >= self.renderer.height {
            return false;
        }
        let word = (y as usize)
            .saturating_mul(self.words as usize)
            .saturating_add((x / 32) as usize);
        self.mask
            .get(word)
            .is_some_and(|bits| bits & (1 << (x % 32)) != 0)
    }

    /// Draw `scene` where `damage` says it changed, copy what a free buffer
    /// lacks of it there and show that buffer in `frame`, with what the frame
    /// shows now.
    fn draw(&mut self, scene: &Scene, damage: Option<&[Rect]>, frame: &HostFrame) {
        let renderer = &mut self.renderer;
        let (width, height) = (renderer.width, renderer.height);
        let changed = match damage.map(|rects| regions(rects, width, height)) {
            Some(changed) if changed.is_empty() => return,
            Some(changed) => {
                let rects: Vec<Rect> = changed
                    .iter()
                    .map(|region| {
                        Rect::new(
                            f64::from(region.x),
                            f64::from(region.y),
                            f64::from(region.x.saturating_add(region.width)),
                            f64::from(region.y.saturating_add(region.height)),
                        )
                    })
                    .collect();
                renderer.render_damage_regions_gpu_wait(scene, TRANSPARENT, &rects);
                changed
            }
            None => {
                renderer.render_gpu_wait(scene, TRANSPARENT);
                vec![Region::new(0, 0, width, height)]
            }
        };
        self.history.push(changed.iter().map(delivered).collect());
        // The buffers the compositor drew from last may still be read: the
        // frame goes into another, which three buffers always leave.
        let in_use = frame.buffers_in_use();
        for (index, buffer) in self.buffers.buffers.iter_mut().enumerate() {
            buffer.busy = in_use.contains(&Some(index));
        }
        let Some(index) = self.buffers.free() else {
            return;
        };
        let latest = self.history.latest();
        let Some(buffer) = self.buffers.buffers.get_mut(index) else {
            return;
        };
        let Memory::Dmabuf(dmabuf) = &buffer.memory else {
            return;
        };
        let copy = self
            .history
            .missed(buffer.holds, delivered(&Region::new(0, 0, width, height)));
        let (device, queue, texture) = self.renderer.target();
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("hosted-frame-deliver"),
        });
        dmabuf.copy_from(&mut encoder, texture, &copy);
        let Some(area) = covered(&changed).map(|area| cell_area(area, (width, height))) else {
            return;
        };
        let read = self
            .coverage
            .read((&device, &queue), &mut encoder, texture, area, self.words);
        queue.submit([encoder.finish()]);
        buffer.holds = Some(latest);
        let Some(read) = read.wait(&device) else {
            return;
        };
        self.finished_frames = true;
        // The rows of the area, into the frame's mask.
        let across = (area.width / 32) as usize;
        let first = (area.x / 32) as usize;
        let stride = self.words as usize;
        for (row, words) in read.mask.chunks(across.max(1)).enumerate() {
            let y = (area.y as usize).saturating_add(row);
            if y >= height as usize {
                break;
            }
            let start = y.saturating_mul(stride).saturating_add(first);
            let fits = words.len().min(stride.saturating_sub(first));
            if let (Some(to), Some(from)) = (
                self.mask.get_mut(start..start.saturating_add(fits)),
                words.get(..fits),
            ) {
                to.copy_from_slice(from);
            }
        }
        frame.record_cells(area, &read.cells);
        frame.show_buffer(index, &changed);
    }
}

/// `region` as the delivery's own.
const fn delivered(region: &Region) -> delivery::Region {
    delivery::Region {
        x: region.x,
        y: region.y,
        width: region.width,
        height: region.height,
    }
}

/// `area` grown to whole mask words across (32 pixels) and whole cells down:
/// what the coverage pass reads, which may run past the frame's right and
/// bottom edge.
fn cell_area(area: Region, (width, height): (u32, u32)) -> Region {
    let left = (area.x.min(width) / 32).saturating_mul(32);
    let top = area
        .y
        .min(height)
        .checked_div(CELL)
        .unwrap_or(0)
        .saturating_mul(CELL);
    let right = area
        .x
        .saturating_add(area.width)
        .min(width)
        .div_ceil(32)
        .saturating_mul(32);
    let bottom = area
        .y
        .saturating_add(area.height)
        .min(height)
        .div_ceil(CELL)
        .saturating_mul(CELL);
    Region::new(
        left,
        top,
        right.saturating_sub(left),
        bottom.saturating_sub(top),
    )
}

/// The pass that reads what a frame shows where it is drawn
/// (`coverage.wgsl`), and its buffers, sized for the whole frame.
struct Coverage {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    area: wgpu::Buffer,
    mask: wgpu::Buffer,
    cells: wgpu::Buffer,
    /// Where both are copied for the CPU to read.
    readback: wgpu::Buffer,
}

/// What the coverage pass read of an area, once the GPU finished.
struct Read {
    /// A bit per pixel, a word per 32 pixels, the area's rows one after another.
    mask: Vec<u32>,
    /// A cell's state per 8x8 pixels, the area's rows one after another.
    cells: Vec<u8>,
}

/// A coverage read submitted: what [`Pending::wait`] maps.
struct Pending<'a> {
    readback: &'a wgpu::Buffer,
    mask_bytes: u64,
    cells_bytes: u64,
}

impl Coverage {
    fn new(device: &wgpu::Device, size: (u32, u32)) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("hosted-frame-coverage"),
            source: wgpu::ShaderSource::Wgsl(include_str!("coverage.wgsl").into()),
        });
        let storage = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: false },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("hosted-frame-coverage"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                storage(2),
                storage(3),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("hosted-frame-coverage"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("hosted-frame-coverage"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        // The most a whole frame needs: its width in words, its height in
        // whole cells.
        let words = u64::from(size.0.div_ceil(32));
        let rows = u64::from(size.1.div_ceil(CELL).saturating_mul(CELL));
        let mask_bytes = words.saturating_mul(rows).saturating_mul(4);
        let cells_bytes = words
            .saturating_mul(4)
            .saturating_mul(rows.checked_div(u64::from(CELL)).unwrap_or(0))
            .saturating_mul(4);
        let buffer = |label, size, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let written = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC;
        Self {
            pipeline,
            layout,
            area: buffer(
                "hosted-frame-coverage-area",
                16,
                wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            ),
            mask: buffer("hosted-frame-coverage-mask", mask_bytes.max(4), written),
            cells: buffer("hosted-frame-coverage-cells", cells_bytes.max(4), written),
            readback: buffer(
                "hosted-frame-coverage-readback",
                mask_bytes.saturating_add(cells_bytes).max(8),
                wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            ),
        }
    }

    /// Record reading `area` (from [`cell_area`]) of `texture`, the frame
    /// `words` mask words wide.
    fn read<'a>(
        &'a self,
        (device, queue): (&wgpu::Device, &wgpu::Queue),
        encoder: &mut wgpu::CommandEncoder,
        texture: &wgpu::Texture,
        area: Region,
        words: u32,
    ) -> Pending<'a> {
        let across = (area.width / 32).min(words);
        let down = area.height / CELL;
        let cells_across = across.saturating_mul(32 / CELL);
        let uniform: [u32; 4] = [area.x, area.y, across, cells_across];
        let bytes: Vec<u8> = uniform
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect();
        // A queue write lands ahead of the submission the pass is in.
        queue.write_buffer(&self.area, 0, &bytes);
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("hosted-frame-coverage"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.area.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.mask.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.cells.as_entire_binding(),
                },
            ],
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("hosted-frame-coverage"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(across, down, 1);
        }
        let mask_bytes = u64::from(across)
            .saturating_mul(u64::from(area.height))
            .saturating_mul(4);
        let cells_bytes = u64::from(cells_across)
            .saturating_mul(u64::from(down))
            .saturating_mul(4);
        if mask_bytes > 0 {
            encoder.copy_buffer_to_buffer(&self.mask, 0, &self.readback, 0, mask_bytes);
        }
        if cells_bytes > 0 {
            encoder.copy_buffer_to_buffer(&self.cells, 0, &self.readback, mask_bytes, cells_bytes);
        }
        Pending {
            readback: &self.readback,
            mask_bytes,
            cells_bytes,
        }
    }
}

impl Pending<'_> {
    /// Wait for the GPU and take what it read: `None` where the device could
    /// not finish it.
    fn wait(self, device: &wgpu::Device) -> Option<Read> {
        let total = self.mask_bytes.saturating_add(self.cells_bytes);
        if total == 0 {
            return Some(Read {
                mask: Vec::new(),
                cells: Vec::new(),
            });
        }
        let slice = self.readback.slice(..total);
        let (sent, mapped) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sent.send(result);
        });
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        mapped.recv().ok()?.ok()?;
        let bytes = slice.get_mapped_range();
        let words = |from: usize, to: usize| -> Vec<u32> {
            bytes
                .get(from..to)
                .unwrap_or_default()
                .chunks_exact(4)
                .filter_map(|word| <[u8; 4]>::try_from(word).ok().map(u32::from_le_bytes))
                .collect()
        };
        let mask_end = usize::try_from(self.mask_bytes).ok()?;
        let end = usize::try_from(total).ok()?;
        let read = Read {
            mask: words(0, mask_end),
            cells: words(mask_end, end)
                .into_iter()
                .map(|cell| u8::try_from(cell).unwrap_or(1))
                .collect(),
        };
        drop(bytes);
        self.readback.unmap();
        Some(read)
    }
}

/// The smallest region holding every one of `regions`.
fn covered(regions: &[Region]) -> Option<Region> {
    let left = regions.iter().map(|region| region.x).min()?;
    let top = regions.iter().map(|region| region.y).min()?;
    let right = regions
        .iter()
        .map(|region| region.x.saturating_add(region.width))
        .max()?;
    let bottom = regions
        .iter()
        .map(|region| region.y.saturating_add(region.height))
        .max()?;
    Some(Region::new(
        left,
        top,
        right.saturating_sub(left),
        bottom.saturating_sub(top),
    ))
}

/// Whether hosted surfaces' frames are drawn on a GPU here: never on a
/// machine whose adapter rasterizes on the CPU. The adapter question opens the
/// toolkit's device, so it is asked once per process rather than per surface.
fn draws_on_gpu() -> bool {
    static DRAWS_ON_GPU: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DRAWS_ON_GPU.get_or_init(|| {
        ui::offscreen::shared_adapter_info()
            .is_some_and(|adapter| !ui::gpu::rasterizes_on_cpu(&adapter))
    })
}

/// `rects` as whole-pixel regions of a `width` by `height` target: rounded
/// out, clipped, and merged where they overlap so no pixel is drawn twice.
pub(crate) fn regions(rects: &[Rect], width: u32, height: u32) -> Vec<Region> {
    let bounds = Rect::new(0.0, 0.0, f64::from(width), f64::from(height));
    let mut merged: Vec<Rect> = Vec::new();
    for rect in rects {
        let mut rect = rect.expand().intersect(bounds);
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            continue;
        }
        while let Some(index) = merged
            .iter()
            .position(|other| other.intersect(rect).area() > 0.0)
        {
            rect = rect.union(merged.swap_remove(index));
        }
        merged.push(rect);
    }
    merged
        .into_iter()
        .map(|rect| {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "whole-pixel rectangles clipped to the target"
            )]
            let region = Region::new(
                rect.x0 as u32,
                rect.y0 as u32,
                rect.width() as u32,
                rect.height() as u32,
            );
            region
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::kurbo::Affine;
    use vello::peniko::Fill;

    #[test]
    fn damage_is_rounded_out_clipped_and_merged() {
        let rects = [
            Rect::new(1.5, 1.5, 4.2, 4.2),
            Rect::new(3.0, 3.0, 6.0, 6.0),
            Rect::new(90.0, 90.0, 120.0, 120.0),
            Rect::new(-5.0, -5.0, -1.0, -1.0),
        ];
        let mut out = regions(&rects, 100, 100);
        out.sort_by_key(|region| (region.x, region.y));
        assert_eq!(
            out,
            vec![Region::new(1, 1, 5, 5), Region::new(90, 90, 10, 10)]
        );
    }

    #[test]
    fn the_coverage_read_covers_every_region_that_changed() {
        assert_eq!(
            covered(&[
                Region::new(10, 5, 4, 4),
                Region::new(20, 9, 2, 3),
                Region::new(12, 6, 1, 1),
            ]),
            Some(Region::new(10, 5, 12, 7)),
            "a typing frame reads what the glyph's box shows, not the whole surface"
        );
        assert_eq!(covered(&[]), None);
    }

    /// A hosted surface's device can be lost under it. The surface then draws
    /// its frames where a machine with no GPU draws them, so the person still
    /// sees everything, only slower, instead of a surface that stops or a
    /// process that panics on a dead device.
    ///
    /// The decision itself is the toolkit's (`ui::gpu::recover`, tested in
    /// `ui-core`): this drives what follows it for a surface, from the loss
    /// of a device that never finished a frame, which cannot be brought back.
    #[test]
    fn a_gpu_given_up_leaves_the_surface_drawing_on_the_cpu() {
        ui::gpu::reset_software_fallback_for_test();
        let adapter = AdapterInfo {
            name: "Adreno X1-85".to_owned(),
            vendor: 0,
            device: 0,
            device_type: vello::wgpu::DeviceType::IntegratedGpu,
            device_pci_bus_id: String::new(),
            driver: String::new(),
            driver_info: String::new(),
            backend: vello::wgpu::Backend::Vulkan,
            subgroup_min_size: 4,
            subgroup_max_size: 128,
            transient_saves_memory: false,
        };
        assert_eq!(
            ui::gpu::recover(&adapter, false, "Device is lost"),
            ui::gpu::Recovery::Software,
            "a device lost before its first frame is on a GPU that cannot come back"
        );

        let size = (64, 48);
        let mut frames = Frames::new(size);
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::WHITE,
            None,
            &Rect::new(8.0, 8.0, 56.0, 40.0),
        );
        frames.present(&scene, None);
        assert!(
            matches!(frames.target, Some(Target::Cpu(_))),
            "the surface draws on the CPU rasterizer once its GPU is given up"
        );
        assert!(
            frames.covers(32, 24),
            "the frames are still drawn, in software"
        );
        ui::gpu::reset_software_fallback_for_test();
    }
}
