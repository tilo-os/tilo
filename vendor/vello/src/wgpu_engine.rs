// Copyright 2023 the Vello Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::num::NonZeroU64;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use smallvec::SmallVec;
use wgpu::{
    BindGroup, BindGroupLayout, Buffer, BufferUsages, CommandEncoder, CommandEncoderDescriptor,
    ComputePassDescriptor, ComputePipeline, Device, PipelineCache, PipelineCompilationOptions,
    QuerySet, Queue, Texture, TextureAspect, TextureUsages, TextureView, TextureViewDimension,
};

use crate::{
    Error, GpuDispatchStageSample, GpuDispatchTimestampSample, GpuDispatchTimestampStatus,
    GpuTimestampSample, GpuTimestampStatus, Result,
    low_level::{BufferProxy, Command, ImageProxy, Recording, ResourceId, ResourceProxy, ShaderId},
    recording::BindType,
};
use vello_shaders::cpu::CpuBinding;

#[derive(Clone)]
struct UninitialisedShader {
    wgsl: Cow<'static, str>,
    label: &'static str,
    entries: Vec<wgpu::BindGroupLayoutEntry>,
    shader_id: ShaderId,
}

/// What a compute pipeline is created from, created before the pipeline.
struct ComputePipelineParts {
    shader_module: wgpu::ShaderModule,
    bind_group_layout: BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    #[cfg(not(target_arch = "wasm32"))]
    entries: Vec<wgpu::BindGroupLayoutEntry>,
    #[cfg(not(target_arch = "wasm32"))]
    source_len: usize,
}

impl ComputePipelineParts {
    fn new(
        device: &Device,
        label: &str,
        wgsl: Cow<'_, str>,
        entries: Vec<wgpu::BindGroupLayoutEntry>,
    ) -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        let source_len = wgsl.len();
        // SAFETY: We only call this with trusted shaders (written by Vello developers)
        let shader_module = unsafe {
            device.create_shader_module_trusted(
                wgpu::ShaderModuleDescriptor {
                    label: Some(label),
                    source: wgpu::ShaderSource::Wgsl(wgsl),
                },
                wgpu::ShaderRuntimeChecks::unchecked(),
            )
        };
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &entries,
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        Self {
            shader_module,
            bind_group_layout,
            pipeline_layout,
            #[cfg(not(target_arch = "wasm32"))]
            entries,
            #[cfg(not(target_arch = "wasm32"))]
            source_len,
        }
    }

    fn descriptor<'a>(
        &'a self,
        label: &'a str,
        cache: Option<&'a PipelineCache>,
    ) -> wgpu::ComputePipelineDescriptor<'a> {
        wgpu::ComputePipelineDescriptor {
            label: Some(label),
            layout: Some(&self.pipeline_layout),
            module: &self.shader_module,
            entry_point: None,
            compilation_options: PipelineCompilationOptions {
                zero_initialize_workgroup_memory: false,
                ..Default::default()
            },
            cache,
        }
    }

    fn into_shader(self, pipeline: ComputePipeline) -> WgpuShader {
        WgpuShader {
            pipeline: PipelineState::Compute(pipeline),
            bind_group_layout: self.bind_group_layout,
            #[cfg(not(target_arch = "wasm32"))]
            entries: self.entries,
            #[cfg(not(target_arch = "wasm32"))]
            source_len: self.source_len,
        }
    }
}

/// Pipelines a renderer registered but did not build before its first frame
/// (Tilo patch, `Renderer::new_deferring`).
#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
struct DeferredBuild {
    /// Which registrations defer while `full_shaders` runs; `None` after.
    mode: crate::PipelineDeferral,
    /// Set by `full_shaders` around registrations a full frame on the
    /// default path never dispatches.
    unused: bool,
    /// Every deferred registration, kept so one can still be built here when
    /// no builder has it (not started, or stopped).
    specs: Vec<UninitialisedShader>,
    /// Deferred shaders whose pipeline is not installed yet.
    pending: HashSet<usize>,
    /// Pipelines the background builder finished, in its order.
    built: Option<Receiver<(ShaderId, WgpuShader)>>,
    started: bool,
    /// Pipelines a dispatch needed before any builder had them.
    built_inline: u64,
}

#[cfg(not(target_arch = "wasm32"))]
fn defers_registration(deferred: &DeferredBuild) -> bool {
    match deferred.mode {
        crate::PipelineDeferral::None => false,
        crate::PipelineDeferral::Unused => deferred.unused,
        crate::PipelineDeferral::All => true,
    }
}

#[derive(Default)]
pub(crate) struct WgpuEngine {
    shaders: Vec<Shader>,
    pool: ResourcePool,
    bind_map: BindMap,
    downloads: HashMap<ResourceId, Buffer>,
    bind_group_cache: HashMap<BindGroupKey, BindGroup>,
    /// Shaders registered but not yet compiled, while compilation is
    /// deferred to [`Self::build_shaders_if_needed`] or
    /// [`Self::build_shaders_async`].
    shaders_to_initialise: Option<Vec<UninitialisedShader>>,
    #[cfg(not(target_arch = "wasm32"))]
    deferred: DeferredBuild,
    pub(crate) use_cpu: bool,
    /// Overrides from a specific `Image::data`'s [`id`](peniko::Blob::id) to a wgpu `Texture`.
    ///
    /// The `Texture` should have the same size as the `Image`.
    pub(crate) image_overrides: HashMap<u64, wgpu::TexelCopyTextureInfoBase<Texture>>,
    /// Images drawn into the atlas from a texture through a render pass
    /// (Tilo patch): any colour format, optionally opaque.
    pub(crate) image_sources: crate::image_source::ImageSources,
    pipeline_cache: Option<PipelineCache>,
    /// Compute-pass batching policy: mode 2 (including deferred frees) is the
    /// production default; explicit modes 0 and 1 remain available for A/B runs.
    compute_batch: u8,
    /// Logging is deliberately separate from enablement so timed A/B runs do
    /// not emit one line for every encoded pass.
    compute_batch_log: bool,
    gpu_timestamps: GpuTimestampTracker,
    gpu_dispatch_timestamps: GpuDispatchTimestampTracker,
    cpu_pathtag: bool,
    cpu_pathtag_reuse: bool,
    cpu_pathtag_pool: CpuPathtagPool,
    cpu_metadata: bool,
    cpu_metadata_pool: CpuMetadataPool,
    cpu_draw_prefix: bool,
    cpu_draw_prefix_pool: CpuDrawPrefixPool,
    direct_path_dispatch: bool,
    /// Fixed direct-dispatch grid selected once at renderer initialization.
    direct_path_workgroups: u32,
    /// Reuses one explicitly-zeroed image when a scene has no gradient ramps.
    ///
    /// This is deliberately an engine-owned exception to the otherwise
    /// transient image lifetime: its texture lives in `bind_map.image_map` so
    /// fine-pass bind-group keys retain the same view across redraws.
    zero_gradient_reuse: bool,
    zero_gradient: Option<ImageProxy>,
    #[cfg(test)]
    zero_gradient_allocations: u64,
    sparse_fine_enabled: bool,
    sparse_fine_available: bool,
    sparse_coarse_enabled: bool,
    sparse_coarse_active_bins_enabled: bool,
    sparse_coarse_adaptive_enabled: bool,
    sparse_tile_bounds_enabled: bool,
    sparse_fine_quad_skip_enabled: bool,
    sparse_opaque_prune_enabled: bool,
    sparse_solid_fuse_enabled: bool,
    sparse_segment_row_mask_enabled: bool,
    canonical_segment_order_enabled: bool,
    area_ppt8_enabled: bool,
    clip_single_group_enabled: bool,
}

const GPU_TIMESTAMP_PASS_LIMIT: u32 = 64;
const GPU_TIMESTAMP_QUERY_COUNT: u32 = GPU_TIMESTAMP_PASS_LIMIT * 2;
const GPU_TIMESTAMP_BYTES: u64 = GPU_TIMESTAMP_QUERY_COUNT as u64 * 8;
const CPU_PATHTAG_REUSE_SLOT_BYTES: usize = 1 << 20;
const CPU_PATHTAG_REDUCED_LABEL: &str = "vello.reduced_buf";
const CPU_PATHTAG_MONOID_LABEL: &str = "vello.tagmonoid_buf";
const CPU_METADATA_PATH_BBOX_SLOT_BYTES: usize = 1 << 20;
const CPU_METADATA_DRAW_REDUCED_SLOT_BYTES: usize = 4096;
const CPU_METADATA_PATH_BBOX_LABEL: &str = "vello.path_bbox_buf";
const CPU_METADATA_DRAW_REDUCED_LABEL: &str = "vello.draw_reduced_buf";
const CPU_DRAW_PREFIX_SLOT_BYTES: usize = 1 << 20;
const CPU_DRAW_PREFIX_LABEL: &str = "vello.draw_monoid_buf";
const DIRECT_PATH_WORKGROUPS_DEFAULT: u32 = 128;

/// Production performance features are on unless explicitly disabled.
///
/// Keeping this policy pure lets tests cover the release defaults without
/// mutating process-global environment state.
fn default_enabled_from_value(value: Option<&OsStr>) -> bool {
    !matches!(value, Some(value) if value == "0")
}

pub(crate) fn default_enabled_from_env(name: &str) -> bool {
    default_enabled_from_value(std::env::var_os(name).as_deref())
}

fn compute_batch_from_value(value: Option<&OsStr>) -> u8 {
    match value {
        Some(value) if value == "0" => 0,
        // Retain the former conservative mode for an explicit A/B override.
        Some(value) if value == "1" => 1,
        Some(value) if value == "2" => 2,
        // The production default is the mode that also folds deferred frees.
        _ => 2,
    }
}

/// Parse the bounded, initialization-only direct path-dispatch diagnostic.
///
/// Invalid values deliberately retain the production grid rather than making
/// an environment typo change rendering work or select an empty dispatch.
fn direct_path_workgroups_from_value(value: Option<&OsStr>) -> u32 {
    value
        .and_then(OsStr::to_str)
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|&workgroups| (1..=DIRECT_PATH_WORKGROUPS_DEFAULT).contains(&workgroups))
        .unwrap_or(DIRECT_PATH_WORKGROUPS_DEFAULT)
}

fn sparse_coarse_active_bins_from_value(value: Option<&OsStr>) -> bool {
    value.is_some_and(|value| value == "1")
}

fn sparse_tile_bounds_from_value(value: Option<&OsStr>) -> bool {
    value.is_some_and(|value| value == "1")
}

fn sparse_fine_quad_skip_from_value(value: Option<&OsStr>) -> bool {
    value.is_some_and(|value| value == "1")
}

fn sparse_opaque_prune_from_value(value: Option<&OsStr>) -> bool {
    value.is_some_and(|value| value == "1")
}

fn sparse_solid_fuse_from_value(value: Option<&OsStr>) -> bool {
    value.is_some_and(|value| value == "1")
}

fn sparse_segment_row_mask_from_value(value: Option<&OsStr>) -> bool {
    value.is_some_and(|value| value == "1")
}

fn canonical_segment_order_from_value(value: Option<&OsStr>) -> bool {
    value.is_some_and(|value| value == "1")
}

fn area_ppt8_from_value(value: Option<&OsStr>) -> bool {
    value.is_some_and(|value| value == "1")
}

fn clip_single_group_from_value(value: Option<&OsStr>) -> bool {
    value.is_some_and(|value| value == "1")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SparseCoarsePolicy {
    Dense,
    Legacy,
    Active,
    Adaptive,
}

fn sparse_coarse_policy(
    coarse: Option<&OsStr>,
    active_bins: Option<&OsStr>,
    auto: Option<&OsStr>,
) -> SparseCoarsePolicy {
    match coarse {
        Some(value) if value == "1" => {
            if sparse_coarse_active_bins_from_value(active_bins) {
                SparseCoarsePolicy::Active
            } else {
                SparseCoarsePolicy::Legacy
            }
        }
        Some(_) => SparseCoarsePolicy::Dense,
        None if auto.is_some_and(|value| value == "1") => SparseCoarsePolicy::Adaptive,
        None => SparseCoarsePolicy::Dense,
    }
}

#[derive(Default)]
struct CpuPathtagPool {
    reduced: Option<Vec<u8>>,
    monoid: Option<Vec<u8>>,
}

impl CpuPathtagPool {
    fn take(&mut self, label: &str, size: usize) -> Option<Vec<u8>> {
        if size > CPU_PATHTAG_REUSE_SLOT_BYTES {
            return None;
        }
        let slot = match label {
            CPU_PATHTAG_REDUCED_LABEL => &mut self.reduced,
            CPU_PATHTAG_MONOID_LABEL => &mut self.monoid,
            _ => return None,
        };
        let mut bytes = slot.take()?;
        if bytes.capacity() < size {
            return None;
        }
        bytes.clear();
        bytes.resize(size, 0);
        Some(bytes)
    }

    fn put(&mut self, label: &str, bytes: Vec<u8>) {
        if bytes.capacity() > CPU_PATHTAG_REUSE_SLOT_BYTES {
            return;
        }
        let slot = match label {
            CPU_PATHTAG_REDUCED_LABEL => &mut self.reduced,
            CPU_PATHTAG_MONOID_LABEL => &mut self.monoid,
            _ => return,
        };
        if slot.is_none() {
            *slot = Some(bytes);
        }
    }

    fn retained_bytes(&self) -> u64 {
        [self.reduced.as_ref(), self.monoid.as_ref()]
            .into_iter()
            .flatten()
            .map(|bytes| u64::try_from(bytes.capacity()).unwrap_or(u64::MAX))
            .sum()
    }

    fn clear(&mut self) {
        self.reduced = None;
        self.monoid = None;
    }
}

#[derive(Default)]
struct CpuMetadataPool {
    path_bbox: Option<Vec<u8>>,
    draw_reduced: Option<Vec<u8>>,
}

impl CpuMetadataPool {
    fn take(&mut self, label: &str, size: usize) -> Option<Vec<u8>> {
        let (slot, limit) = match label {
            CPU_METADATA_PATH_BBOX_LABEL => {
                (&mut self.path_bbox, CPU_METADATA_PATH_BBOX_SLOT_BYTES)
            }
            CPU_METADATA_DRAW_REDUCED_LABEL => {
                (&mut self.draw_reduced, CPU_METADATA_DRAW_REDUCED_SLOT_BYTES)
            }
            _ => return None,
        };
        if size > limit {
            return None;
        }
        let mut bytes = slot.take()?;
        if bytes.capacity() < size {
            return None;
        }
        bytes.clear();
        bytes.resize(size, 0);
        Some(bytes)
    }

    fn put(&mut self, label: &str, bytes: Vec<u8>) {
        let (slot, limit) = match label {
            CPU_METADATA_PATH_BBOX_LABEL => {
                (&mut self.path_bbox, CPU_METADATA_PATH_BBOX_SLOT_BYTES)
            }
            CPU_METADATA_DRAW_REDUCED_LABEL => {
                (&mut self.draw_reduced, CPU_METADATA_DRAW_REDUCED_SLOT_BYTES)
            }
            _ => return,
        };
        if bytes.capacity() <= limit && slot.is_none() {
            *slot = Some(bytes);
        }
    }

    fn retained_bytes(&self) -> u64 {
        [self.path_bbox.as_ref(), self.draw_reduced.as_ref()]
            .into_iter()
            .flatten()
            .map(|bytes| u64::try_from(bytes.capacity()).unwrap_or(u64::MAX))
            .sum()
    }

    fn clear(&mut self) {
        self.path_bbox = None;
        self.draw_reduced = None;
    }
}

#[derive(Default)]
struct CpuDrawPrefixPool {
    draw_monoid: Option<Vec<u8>>,
}

impl CpuDrawPrefixPool {
    fn take(&mut self, label: &str, size: usize) -> Option<Vec<u8>> {
        if label != CPU_DRAW_PREFIX_LABEL || size > CPU_DRAW_PREFIX_SLOT_BYTES {
            return None;
        }
        let mut bytes = self.draw_monoid.take()?;
        if bytes.capacity() < size {
            return None;
        }
        bytes.clear();
        bytes.resize(size, 0);
        Some(bytes)
    }

    fn put(&mut self, label: &str, bytes: Vec<u8>) {
        if label == CPU_DRAW_PREFIX_LABEL
            && bytes.capacity() <= CPU_DRAW_PREFIX_SLOT_BYTES
            && self.draw_monoid.is_none()
        {
            self.draw_monoid = Some(bytes);
        }
    }

    fn retained_bytes(&self) -> u64 {
        self.draw_monoid.as_ref().map_or(0, |bytes| {
            u64::try_from(bytes.capacity()).unwrap_or(u64::MAX)
        })
    }

    fn clear(&mut self) {
        self.draw_monoid = None;
    }
}

struct TimestampWrites {
    query_set: QuerySet,
    beginning: u32,
    end: u32,
}

enum TimestampMapState {
    Idle,
    Pending {
        submission_id: u64,
        pass_count: u32,
        receiver: Receiver<std::result::Result<(), wgpu::BufferAsyncError>>,
    },
    Ready(GpuTimestampSample),
    MapFailed {
        submission_id: u64,
    },
    Invalid {
        submission_id: u64,
    },
    Overflowed {
        submission_id: u64,
        pass_count: u32,
    },
}

struct TimestampResources {
    query_set: QuerySet,
    resolve: Buffer,
    read: Buffer,
    timestamp_period_ns: f32,
    pass_count: u32,
    overflowed: bool,
    next_submission_id: u64,
    latest_submission_id: Option<u64>,
    map_state: TimestampMapState,
}

enum GpuTimestampTracker {
    Disabled,
    Requested,
    Unsupported,
    Active(TimestampResources),
}

/// Opt-in timestamps around individual GPU dispatches. This deliberately
/// wraps the existing bounded timestamp lifecycle so it cannot create a
/// second in-flight map or an additional submission.
enum GpuDispatchTimestampTracker {
    Disabled,
    Requested,
    Unsupported,
    Active(Box<DispatchTimestampResources>),
}

struct DispatchTimestampResources {
    inner: GpuTimestampTracker,
    stage_ids: [u16; GPU_TIMESTAMP_PASS_LIMIT as usize],
    stage_count: u32,
}

fn timestamp_slot_available(map_state: &TimestampMapState) -> bool {
    !matches!(map_state, TimestampMapState::Pending { .. })
}

impl Default for GpuTimestampTracker {
    fn default() -> Self {
        Self::Disabled
    }
}

impl Default for GpuDispatchTimestampTracker {
    fn default() -> Self {
        Self::Disabled
    }
}

enum BatchedDispatch {
    Direct {
        stage_id: u16,
        pipeline: ComputePipeline,
        bind_group: BindGroup,
        workgroups: (u32, u32, u32),
    },
    Indirect {
        stage_id: u16,
        pipeline: ComputePipeline,
        bind_group: BindGroup,
        buffer: Buffer,
        offset: u64,
    },
}

const COMPUTE_BATCH_LIMIT: usize = 16;

fn gpu_dispatch_stage_id(label: &str) -> u16 {
    match label {
        "vello.pathtag_reduce" => 1,
        "vello.pathtag_reduce2" => 2,
        "vello.pathtag_scan1" => 3,
        "vello.pathtag_scan_small" => 4,
        "vello.pathtag_scan_large" => 5,
        "vello.bbox_clear" => 6,
        "vello.flatten" => 7,
        "vello.draw_reduce" => 8,
        "vello.draw_leaf" => 9,
        "vello.clip_reduce" => 10,
        "vello.clip_leaf" | "vello.clip_leaf_single_group" => 11,
        "vello.binning" => 12,
        "vello.tile_alloc" | "vello.tile_alloc_sparse" => 13,
        "vello.path_count_setup" => 14,
        "vello.path_count" => 15,
        "vello.backdrop_dyn" => 16,
        "vello.coarse" | "vello.coarse_opaque_prune" | "vello.coarse_solid_fuse" => 17,
        "vello.path_tiling_setup" => 18,
        "vello.path_tiling" | "vello.path_tiling_segment_row_mask" => 19,
        "vello.fine_area" | "vello.fine_area_ppt8" => 20,
        "vello.fine_msaa8" => 21,
        "vello.fine_msaa16" => 22,
        "vello.fine_sparse_area"
        | "vello.fine_sparse_area_quad_skip"
        | "vello.fine_sparse_area_solid_fuse"
        | "vello.fine_sparse_area_segment_row_mask"
        | "vello.fine_sparse_area_ppt8" => 23,
        "vello.coarse_sparse"
        | "vello.coarse_sparse_opaque_prune"
        | "vello.coarse_sparse_solid_fuse" => 24,
        "vello.draw_leaf_prefix" => 25,
        "vello.path_count_direct" => 26,
        "vello.path_tiling_direct" | "vello.path_tiling_direct_segment_row_mask" => 27,
        "vello.coarse_sparse_active"
        | "vello.coarse_sparse_active_opaque_prune"
        | "vello.coarse_sparse_active_solid_fuse" => 28,
        _ => 0,
    }
}

fn is_cpu_pathtag_shader(label: &str) -> bool {
    matches!(label, "vello.pathtag_reduce" | "vello.pathtag_scan_small")
}

fn is_cpu_metadata_shader(label: &str) -> bool {
    matches!(label, "vello.bbox_clear" | "vello.draw_reduce")
}

fn compute_batch_boundary(command: &Command, include_frees: bool) -> bool {
    match command {
        Command::Dispatch(..) | Command::DispatchIndirect(..) => false,
        Command::FreeBuffer(..) | Command::FreeImage(..) => !include_frees,
        _ => true,
    }
}

fn record_deferred_free(
    command: &Command,
    free_bufs: &mut HashSet<ResourceId>,
    free_images: &mut HashSet<ResourceId>,
) -> bool {
    match command {
        Command::FreeBuffer(proxy) => {
            free_bufs.insert(proxy.id);
            true
        }
        Command::FreeImage(proxy) => {
            free_images.insert(proxy.id);
            true
        }
        _ => false,
    }
}

enum PipelineState {
    Compute(ComputePipeline),
    #[cfg(feature = "debug_layers")]
    Render(wgpu::RenderPipeline),
}

struct WgpuShader {
    pipeline: PipelineState,
    bind_group_layout: BindGroupLayout,
    /// The layout's entries, so [`WgpuEngine::dispatch_empty`] can bind stand-in resources.
    #[cfg(not(target_arch = "wasm32"))]
    entries: Vec<wgpu::BindGroupLayoutEntry>,
    /// Length of the WGSL source, a proxy for how long a driver takes to compile it.
    #[cfg(not(target_arch = "wasm32"))]
    source_len: usize,
}

const BIND_GROUP_CACHE_LIMIT: usize = 128;

#[derive(Clone, Hash, PartialEq, Eq)]
enum BoundResource {
    Buffer(Buffer, u64, Option<NonZeroU64>),
    Image(TextureView),
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct BindGroupKey {
    shader: ShaderId,
    resources: SmallVec<[BoundResource; 8]>,
}

pub(crate) enum CpuShaderType {
    Present(fn(u32, &[CpuBinding<'_>])),
    Missing,
    Skipped,
}

struct CpuShader {
    shader: fn(u32, &[CpuBinding<'_>]),
}

enum ShaderKind<'a> {
    Wgpu(&'a WgpuShader),
    Cpu(&'a CpuShader),
}

struct Shader {
    label: &'static str,
    wgpu: Option<WgpuShader>,
    cpu: Option<CpuShader>,
    cpu_pathtag: Option<CpuShader>,
    cpu_metadata: Option<CpuShader>,
}

impl Shader {
    fn select(&self, cpu_pathtag_enabled: bool, cpu_metadata_enabled: bool) -> ShaderKind<'_> {
        if let Some(cpu) = self.cpu.as_ref() {
            ShaderKind::Cpu(cpu)
        } else if cpu_pathtag_enabled && let Some(cpu) = self.cpu_pathtag.as_ref() {
            ShaderKind::Cpu(cpu)
        } else if cpu_metadata_enabled && let Some(cpu) = self.cpu_metadata.as_ref() {
            ShaderKind::Cpu(cpu)
        } else if let Some(wgpu) = self.wgpu.as_ref() {
            ShaderKind::Wgpu(wgpu)
        } else {
            panic!("no available shader for {}", self.label)
        }
    }
}

pub(crate) enum ExternalResource<'a> {
    Buffer(BufferProxy, &'a Buffer),
    Image(ImageProxy, &'a TextureView),
}

/// A buffer can exist either on the GPU or on CPU.
enum MaterializedBuffer {
    Gpu(Buffer),
    Cpu(RefCell<Vec<u8>>),
}

struct BindMapBuffer {
    buffer: MaterializedBuffer,
    label: &'static str,
}

#[derive(Default)]
struct BindMap {
    buf_map: HashMap<ResourceId, BindMapBuffer>,
    image_map: HashMap<ResourceId, (Texture, TextureView)>,
    pending_clears: HashSet<ResourceId>,
    #[cfg(test)]
    zero_gradient_id: Option<ResourceId>,
    #[cfg(test)]
    zero_gradient_bind_group_cache_hits: u64,
}

#[derive(Hash, PartialEq, Eq)]
struct BufferProperties {
    size: u64,
    usages: BufferUsages,
    name: &'static str,
}

#[derive(Default)]
struct ResourcePool {
    /// Free buffers per size class, each with the recording that returned it.
    bufs: HashMap<BufferProperties, Vec<(Buffer, u64)>>,
    /// Recordings run so far: the clock free buffers are stamped with.
    recordings: u64,
    /// Destroy free buffers no recording has taken for this many recordings;
    /// `None` keeps them until the pool is cleared (upstream).
    keep: Option<u64>,
    /// Recordings left in which [`POOL_KEEP_SOON`] applies instead.
    soon: u64,
}

/// Recordings a free buffer may go untaken before it is destroyed, in an
/// engine that opted in ([`WgpuEngine::set_pool_keep`]). A size class every
/// frame asks for is taken every frame; one a larger scene asked for is not
/// asked for again once the scene got smaller.
pub(crate) const POOL_KEEP_RECORDINGS: u64 = 120;
/// The same for the recordings after [`WgpuEngine::shrink_pool_soon`]: a
/// window closed, and an idle desktop draws few frames after it. Still a few
/// frames' worth: a frame may record a blur, a second pass and each output.
const POOL_KEEP_SOON: u64 = 16;
/// Recordings a soon request stays live for.
const POOL_SOON_RECORDINGS: u64 = 64;

/// The transient bind map contains short-lifetime resources.
///
/// In particular, it has resources scoped to a single call of
/// `run_recording()`, including external resources and also buffer
/// uploads.
#[derive(Default)]
struct TransientBindMap<'a> {
    bufs: HashMap<ResourceId, TransientBuf<'a>>,
    // TODO: create transient image type
    images: HashMap<ResourceId, &'a TextureView>,
}

enum TransientBuf<'a> {
    Cpu(&'a [u8]),
    Gpu(&'a Buffer),
}

impl GpuTimestampTracker {
    fn requested() -> Self {
        Self::Requested
    }

    fn initialize(&mut self, device: &Device) {
        if matches!(self, Self::Requested) {
            if !device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
                *self = Self::Unsupported;
                return;
            }
            *self = Self::Active(TimestampResources {
                query_set: device.create_query_set(&wgpu::QuerySetDescriptor {
                    label: Some("vello GPU timestamps"),
                    ty: wgpu::QueryType::Timestamp,
                    count: GPU_TIMESTAMP_QUERY_COUNT,
                }),
                resolve: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("vello GPU timestamp resolve"),
                    size: GPU_TIMESTAMP_BYTES,
                    usage: BufferUsages::QUERY_RESOLVE | BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                }),
                read: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("vello GPU timestamp readback"),
                    size: GPU_TIMESTAMP_BYTES,
                    usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                }),
                timestamp_period_ns: 0.0,
                pass_count: 0,
                overflowed: false,
                next_submission_id: 0,
                latest_submission_id: None,
                map_state: TimestampMapState::Idle,
            });
        }
    }

    fn begin_recording(&mut self, device: &Device, queue: &Queue) {
        self.initialize(device);
        self.poll_ready();
        if let Self::Active(resources) = self {
            resources.next_submission_id = resources.next_submission_id.saturating_add(1);
            resources.latest_submission_id = Some(resources.next_submission_id);
            // This is a latest-recording diagnostic: a ready sample that the
            // caller did not consume must not permanently block later samples.
            // A pending map is the sole state that owns the readback buffer.
            if timestamp_slot_available(&resources.map_state) {
                resources.map_state = TimestampMapState::Idle;
            }
        }
        if let Self::Active(resources) = self
            && matches!(&resources.map_state, TimestampMapState::Idle)
        {
            resources.pass_count = 0;
            resources.overflowed = false;
            resources.timestamp_period_ns = queue.get_timestamp_period();
        }
    }

    fn reserve_pass(&mut self) -> Option<TimestampWrites> {
        let Self::Active(resources) = self else {
            return None;
        };
        if !matches!(&resources.map_state, TimestampMapState::Idle) {
            return None;
        }
        resources.pass_count = resources.pass_count.saturating_add(1);
        if resources.pass_count > GPU_TIMESTAMP_PASS_LIMIT {
            resources.overflowed = true;
            return None;
        }
        let beginning = resources.pass_count.saturating_sub(1).saturating_mul(2);
        Some(TimestampWrites {
            query_set: resources.query_set.clone(),
            beginning,
            end: beginning.saturating_add(1),
        })
    }

    fn finish_recording(&mut self, encoder: &mut CommandEncoder) -> bool {
        let Self::Active(resources) = self else {
            return false;
        };
        if !matches!(&resources.map_state, TimestampMapState::Idle) || resources.pass_count == 0 {
            return false;
        }
        let submission_id = resources
            .latest_submission_id
            .expect("recording has an identity");
        if resources.overflowed {
            resources.map_state = TimestampMapState::Overflowed {
                submission_id,
                pass_count: resources.pass_count,
            };
            return false;
        }
        let byte_count = u64::from(resources.pass_count).saturating_mul(16);
        encoder.resolve_query_set(
            &resources.query_set,
            0..resources.pass_count.saturating_mul(2),
            &resources.resolve,
            0,
        );
        encoder.copy_buffer_to_buffer(&resources.resolve, 0, &resources.read, 0, byte_count);
        true
    }

    fn map_after_submit(&mut self) {
        let Self::Active(resources) = self else {
            return;
        };
        if !matches!(&resources.map_state, TimestampMapState::Idle)
            || resources.pass_count == 0
            || resources.overflowed
        {
            return;
        }
        let submission_id = resources
            .latest_submission_id
            .expect("recording has an identity");
        let pass_count = resources.pass_count;
        let (sender, receiver) = mpsc::channel();
        resources
            .read
            .slice(..u64::from(pass_count).saturating_mul(16))
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        resources.map_state = TimestampMapState::Pending {
            submission_id,
            pass_count,
            receiver,
        };
    }

    fn poll_ready(&mut self) {
        let Self::Active(resources) = self else {
            return;
        };
        let TimestampMapState::Pending {
            submission_id,
            pass_count,
            ..
        } = &resources.map_state
        else {
            return;
        };
        let submission_id = *submission_id;
        let pass_count = *pass_count;
        let result = match &resources.map_state {
            TimestampMapState::Pending { receiver, .. } => receiver.try_recv(),
            _ => unreachable!(),
        };
        match result {
            Ok(Ok(())) => {
                let bytes = resources
                    .read
                    .slice(..u64::from(pass_count).saturating_mul(16))
                    .get_mapped_range()
                    .to_vec();
                resources.read.unmap();
                resources.map_state = decode_timestamp_sample(
                    submission_id,
                    pass_count,
                    resources.timestamp_period_ns,
                    bytes,
                )
                .map(TimestampMapState::Ready)
                .unwrap_or(TimestampMapState::Invalid { submission_id });
            }
            Ok(Err(_)) | Err(TryRecvError::Disconnected) => {
                resources.map_state = TimestampMapState::MapFailed { submission_id };
            }
            Err(TryRecvError::Empty) => {}
        }
    }

    fn status(&mut self) -> GpuTimestampStatus {
        self.poll_ready();
        match self {
            Self::Disabled => GpuTimestampStatus::Disabled,
            Self::Requested | Self::Unsupported => GpuTimestampStatus::Unsupported,
            Self::Active(resources) => match &resources.map_state {
                TimestampMapState::Idle => GpuTimestampStatus::Idle,
                TimestampMapState::Pending { submission_id, .. } => GpuTimestampStatus::Pending {
                    submission_id: *submission_id,
                },
                TimestampMapState::Ready(sample) => GpuTimestampStatus::Ready {
                    submission_id: sample.submission_id,
                    pass_count: sample.pass_count,
                },
                TimestampMapState::MapFailed { submission_id } => GpuTimestampStatus::MapFailed {
                    submission_id: *submission_id,
                },
                TimestampMapState::Invalid { submission_id } => GpuTimestampStatus::Invalid {
                    submission_id: *submission_id,
                },
                TimestampMapState::Overflowed {
                    submission_id,
                    pass_count,
                } => GpuTimestampStatus::Overflowed {
                    submission_id: *submission_id,
                    pass_count: *pass_count,
                },
            },
        }
    }

    fn take_sample(&mut self) -> Option<GpuTimestampSample> {
        self.poll_ready();
        let Self::Active(resources) = self else {
            return None;
        };
        if !matches!(&resources.map_state, TimestampMapState::Ready(_)) {
            return None;
        }
        let TimestampMapState::Ready(sample) =
            std::mem::replace(&mut resources.map_state, TimestampMapState::Idle)
        else {
            unreachable!();
        };
        Some(sample)
    }

    fn latest_submission_id(&self) -> Option<u64> {
        match self {
            Self::Active(resources) => resources.latest_submission_id,
            _ => None,
        }
    }

    fn buffer_bytes(&self) -> u64 {
        match self {
            Self::Active(_) => GPU_TIMESTAMP_BYTES.saturating_mul(2),
            _ => 0,
        }
    }
}

impl GpuDispatchTimestampTracker {
    fn requested() -> Self {
        Self::Requested
    }

    fn initialize(&mut self, device: &Device) {
        if !matches!(self, Self::Requested) {
            return;
        }
        let required =
            wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES;
        if !device.features().contains(required) {
            *self = Self::Unsupported;
            return;
        }
        let mut inner = GpuTimestampTracker::requested();
        inner.initialize(device);
        if matches!(inner, GpuTimestampTracker::Active(_)) {
            *self = Self::Active(Box::new(DispatchTimestampResources {
                inner,
                stage_ids: [0; GPU_TIMESTAMP_PASS_LIMIT as usize],
                stage_count: 0,
            }));
        } else {
            *self = Self::Unsupported;
        }
    }

    fn begin_recording(&mut self, device: &Device, queue: &Queue) {
        self.initialize(device);
        let Self::Active(resources) = self else {
            return;
        };
        resources.inner.begin_recording(device, queue);
        // A pending map owns both the readback buffer and its stage IDs. The
        // wrapped tracker leaves it pending; do the same here. Terminal
        // states are reset by `begin_recording`, so their old IDs may be
        // discarded under the documented latest-recording policy.
        if let GpuTimestampTracker::Active(inner) = &resources.inner
            && matches!(&inner.map_state, TimestampMapState::Idle)
        {
            resources.stage_ids.fill(0);
            resources.stage_count = 0;
        }
    }

    fn reserve_dispatch(&mut self, stage_id: u16) -> Option<TimestampWrites> {
        let Self::Active(resources) = self else {
            return None;
        };
        let writes = resources.inner.reserve_pass()?;
        let slot = usize::try_from(writes.beginning.saturating_div(2)).ok()?;
        let stage = resources.stage_ids.get_mut(slot)?;
        *stage = stage_id;
        resources.stage_count = resources
            .stage_count
            .max(writes.beginning.saturating_div(2).checked_add(1)?);
        Some(writes)
    }

    fn stage_id_for_label(&self, label: &str) -> u16 {
        matches!(self, Self::Active(_))
            .then(|| gpu_dispatch_stage_id(label))
            .unwrap_or(0)
    }

    fn finish_recording(&mut self, encoder: &mut CommandEncoder) -> bool {
        let Self::Active(resources) = self else {
            return false;
        };
        resources.inner.finish_recording(encoder)
    }

    fn map_after_submit(&mut self) {
        if let Self::Active(resources) = self {
            resources.inner.map_after_submit();
        }
    }

    fn status(&mut self) -> GpuDispatchTimestampStatus {
        match self {
            Self::Disabled => GpuDispatchTimestampStatus::Disabled,
            Self::Requested | Self::Unsupported => GpuDispatchTimestampStatus::Unsupported,
            Self::Active(resources) => match resources.inner.status() {
                GpuTimestampStatus::Disabled | GpuTimestampStatus::Unsupported => {
                    GpuDispatchTimestampStatus::Unsupported
                }
                GpuTimestampStatus::Idle => GpuDispatchTimestampStatus::Idle,
                GpuTimestampStatus::Pending { submission_id } => {
                    GpuDispatchTimestampStatus::Pending { submission_id }
                }
                GpuTimestampStatus::Ready { submission_id, .. } => {
                    GpuDispatchTimestampStatus::Ready {
                        submission_id,
                        stage_count: resources.stage_count,
                    }
                }
                GpuTimestampStatus::MapFailed { submission_id } => {
                    GpuDispatchTimestampStatus::MapFailed { submission_id }
                }
                GpuTimestampStatus::Invalid { submission_id } => {
                    GpuDispatchTimestampStatus::Invalid { submission_id }
                }
                GpuTimestampStatus::Overflowed { submission_id, .. } => {
                    GpuDispatchTimestampStatus::Overflowed {
                        submission_id,
                        stage_count: resources.stage_count,
                    }
                }
            },
        }
    }

    fn take_sample(&mut self) -> Option<GpuDispatchTimestampSample> {
        let Self::Active(resources) = self else {
            return None;
        };
        let sample = resources.inner.take_sample()?;
        let submission_id = sample.submission_id;
        let decoded =
            decode_dispatch_timestamp_sample(&sample, &resources.stage_ids, resources.stage_count);
        if decoded.is_none() {
            mark_dispatch_timestamp_invalid(resources, submission_id);
        }
        decoded
    }

    fn latest_submission_id(&self) -> Option<u64> {
        match self {
            Self::Active(resources) => resources.inner.latest_submission_id(),
            _ => None,
        }
    }

    fn buffer_bytes(&self) -> u64 {
        match self {
            Self::Active(resources) => resources.inner.buffer_bytes(),
            _ => 0,
        }
    }
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "finite, bounded non-negative timestamp durations are validated before compact diagnostic aggregation"
)]
fn decode_dispatch_timestamp_sample(
    sample: &GpuTimestampSample,
    stage_ids: &[u16; GPU_TIMESTAMP_PASS_LIMIT as usize],
    expected_stage_count: u32,
) -> Option<GpuDispatchTimestampSample> {
    let expected_bytes = usize::try_from(sample.pass_count)
        .ok()
        .and_then(|count| count.checked_mul(16));
    if sample.pass_count == 0
        || sample.pass_count > GPU_TIMESTAMP_PASS_LIMIT
        || sample.pass_count != expected_stage_count
        || expected_bytes != Some(sample.timestamp_bytes.len())
        || !sample.timestamp_period_ns.is_finite()
        || sample.timestamp_period_ns <= 0.0
    {
        return None;
    }
    let mut by_stage = [GpuDispatchStageSample::default(); 32];
    for (pair, stage_id) in sample
        .timestamp_bytes
        .chunks_exact(16)
        .zip(stage_ids.iter().copied())
    {
        let beginning_bytes: [u8; 8] = pair.get(..8)?.try_into().ok()?;
        let end_bytes: [u8; 8] = pair.get(8..)?.try_into().ok()?;
        let beginning = u64::from_le_bytes(beginning_bytes);
        let end = u64::from_le_bytes(end_bytes);
        let ticks = end.checked_sub(beginning)?;
        let ns = ticks as f64 * f64::from(sample.timestamp_period_ns);
        if !ns.is_finite() || ns < 0.0 || ns >= u128::MAX as f64 {
            return None;
        }
        let bucket = match stage_id {
            1..=31 => usize::from(stage_id),
            _ => 0,
        };
        let aggregate = &mut by_stage[bucket];
        aggregate.stage_id = if bucket == 0 { 0 } else { stage_id };
        let dispatch_count = aggregate.dispatch_count.checked_add(1)?;
        let duration_ns = aggregate.duration_ns.checked_add(ns as u128)?;
        aggregate.dispatch_count = dispatch_count;
        aggregate.duration_ns = duration_ns;
    }
    let mut stages = [GpuDispatchStageSample::default(); 32];
    let mut stage_count = 0usize;
    for stage in by_stage
        .into_iter()
        .filter(|stage| stage.dispatch_count != 0)
    {
        let slot = stages.get_mut(stage_count)?;
        *slot = stage;
        stage_count = stage_count.checked_add(1)?;
    }
    Some(GpuDispatchTimestampSample {
        submission_id: sample.submission_id,
        stage_count: u32::try_from(stage_count).ok()?,
        stages,
    })
}

fn mark_dispatch_timestamp_invalid(resources: &mut DispatchTimestampResources, submission_id: u64) {
    if let GpuTimestampTracker::Active(inner) = &mut resources.inner {
        inner.map_state = TimestampMapState::Invalid { submission_id };
    }
}

fn decode_timestamp_sample(
    submission_id: u64,
    pass_count: u32,
    timestamp_period_ns: f32,
    timestamp_bytes: Vec<u8>,
) -> Option<GpuTimestampSample> {
    if !timestamp_period_ns.is_finite()
        || timestamp_period_ns <= 0.0
        || pass_count == 0
        || pass_count > GPU_TIMESTAMP_PASS_LIMIT
        || timestamp_bytes.len() != (pass_count as usize).saturating_mul(16)
    {
        return None;
    }
    let mut pairs = timestamp_bytes.chunks_exact(16);
    let first_pair = pairs.next()?;
    let first = u64::from_le_bytes(first_pair[..8].try_into().ok()?);
    let mut last = 0;
    let mut summed_pass_ticks = 0u64;
    for pair in std::iter::once(first_pair).chain(pairs) {
        let beginning = u64::from_le_bytes(pair[..8].try_into().ok()?);
        let end = u64::from_le_bytes(pair[8..].try_into().ok()?);
        summed_pass_ticks = summed_pass_ticks.checked_add(end.checked_sub(beginning)?)?;
        last = end;
    }
    let span_ticks = last.checked_sub(first)?;
    Some(GpuTimestampSample {
        submission_id,
        pass_count,
        span_ticks,
        summed_pass_ticks,
        timestamp_period_ns,
        span_ns: span_ticks as f64 * f64::from(timestamp_period_ns),
        summed_pass_ns: summed_pass_ticks as f64 * f64::from(timestamp_period_ns),
        timestamp_bytes,
    })
}

impl WgpuEngine {
    /// Dispatch timestamps are deliberately unavailable until the opt-in
    /// inside-pass tracker has reserved a query slot for a recording.
    pub fn gpu_dispatch_timestamp_status(&mut self) -> GpuDispatchTimestampStatus {
        self.gpu_dispatch_timestamps.status()
    }

    pub fn take_gpu_dispatch_timestamp_sample(&mut self) -> Option<GpuDispatchTimestampSample> {
        self.gpu_dispatch_timestamps.take_sample()
    }

    pub fn gpu_dispatch_timestamp_latest_submission_id(&self) -> Option<u64> {
        self.gpu_dispatch_timestamps.latest_submission_id()
    }

    pub fn gpu_dispatch_timestamp_buffer_bytes(&self) -> u64 {
        self.gpu_dispatch_timestamps.buffer_bytes()
    }
    #[expect(
        clippy::print_stderr,
        reason = "an explicitly requested A/B diagnostic must work without application logging"
    )]
    pub fn new(use_cpu: bool, pipeline_cache: Option<PipelineCache>) -> Self {
        let compute_batch =
            compute_batch_from_value(std::env::var_os("VELLO_COMPUTE_BATCH").as_deref());
        let compute_batch_log =
            std::env::var_os("VELLO_COMPUTE_BATCH_LOG").is_some_and(|value| value == "1");
        let gpu_timestamps = std::env::var_os("VELLO_GPU_TIMESTAMPS")
            .is_some_and(|value| value == "1")
            .then(GpuTimestampTracker::requested)
            .unwrap_or_default();
        let dispatch_timestamps_requested =
            std::env::var_os("VELLO_GPU_DISPATCH_TIMESTAMPS").is_some_and(|value| value == "1");
        let gpu_dispatch_timestamps = if dispatch_timestamps_requested
            && std::env::var_os("VELLO_GPU_TIMESTAMPS").is_some_and(|value| value == "1")
            && !use_cpu
            && !cfg!(feature = "wgpu-profiler")
        {
            GpuDispatchTimestampTracker::requested()
        } else if dispatch_timestamps_requested {
            GpuDispatchTimestampTracker::Unsupported
        } else {
            GpuDispatchTimestampTracker::Disabled
        };
        let sparse_fine_enabled = default_enabled_from_env("VELLO_SPARSE_FINE");
        let sparse_coarse_policy = sparse_coarse_policy(
            std::env::var_os("VELLO_SPARSE_COARSE").as_deref(),
            std::env::var_os("VELLO_SPARSE_COARSE_ACTIVE_BINS").as_deref(),
            std::env::var_os("VELLO_SPARSE_COARSE_AUTO").as_deref(),
        );
        let sparse_coarse_enabled = !matches!(sparse_coarse_policy, SparseCoarsePolicy::Dense);
        let sparse_coarse_active_bins_enabled = matches!(
            sparse_coarse_policy,
            SparseCoarsePolicy::Active | SparseCoarsePolicy::Adaptive
        );
        let sparse_coarse_adaptive_enabled =
            matches!(sparse_coarse_policy, SparseCoarsePolicy::Adaptive);
        let sparse_tile_bounds_enabled =
            sparse_tile_bounds_from_value(std::env::var_os("VELLO_SPARSE_TILE_BOUNDS").as_deref());
        let sparse_fine_quad_skip_enabled = sparse_fine_quad_skip_from_value(
            std::env::var_os("VELLO_SPARSE_FINE_QUAD_SKIP").as_deref(),
        );
        let sparse_opaque_prune_enabled = sparse_opaque_prune_from_value(
            std::env::var_os("VELLO_SPARSE_OPAQUE_PRUNE").as_deref(),
        );
        let sparse_solid_fuse_enabled =
            sparse_solid_fuse_from_value(std::env::var_os("VELLO_SPARSE_SOLID_FUSE").as_deref());
        let sparse_segment_row_mask_enabled = sparse_segment_row_mask_from_value(
            std::env::var_os("VELLO_SPARSE_SEGMENT_ROW_MASK").as_deref(),
        );
        let canonical_segment_order_enabled = canonical_segment_order_from_value(
            std::env::var_os("VELLO_CANONICAL_SEGMENT_ORDER").as_deref(),
        );
        let area_ppt8_enabled =
            area_ppt8_from_value(std::env::var_os("VELLO_AREA_PPT8").as_deref());
        let clip_single_group_enabled =
            clip_single_group_from_value(std::env::var_os("VELLO_CLIP_SINGLE_GROUP").as_deref());
        if compute_batch_log {
            let state = if compute_batch == 0 {
                "disabled (set VELLO_COMPUTE_BATCH=1 or 2 to enable)"
            } else if use_cpu {
                "requested, but CPU dispatches remain unbatched"
            } else if cfg!(feature = "wgpu-profiler") {
                "requested, but profiler builds retain one query per dispatch"
            } else if compute_batch == 2 {
                "enabled with deferred free bookkeeping"
            } else {
                "enabled conservatively"
            };
            eprintln!(
                "vello: compute dispatch batching {state} (max {COMPUTE_BATCH_LIMIT} per pass)"
            );
        }
        let cpu_pathtag = default_enabled_from_env("VELLO_CPU_PATHTAG");
        let cpu_pathtag_reuse = !use_cpu && default_enabled_from_env("VELLO_CPU_PATHTAG_REUSE");
        let cpu_metadata = !use_cpu && default_enabled_from_env("VELLO_CPU_METADATA");
        let cpu_draw_prefix = !use_cpu && default_enabled_from_env("VELLO_CPU_DRAW_PREFIX");
        let direct_path_dispatch = default_enabled_from_env("VELLO_DIRECT_PATH_DISPATCH");
        let direct_path_workgroups = direct_path_workgroups_from_value(
            std::env::var_os("VELLO_DIRECT_PATH_WORKGROUPS").as_deref(),
        );
        let zero_gradient_reuse = default_enabled_from_env("VELLO_ZERO_GRADIENT_REUSE");
        Self {
            use_cpu,
            pipeline_cache,
            compute_batch,
            compute_batch_log,
            gpu_timestamps,
            gpu_dispatch_timestamps,
            cpu_pathtag,
            cpu_pathtag_reuse,
            cpu_metadata,
            cpu_draw_prefix,
            direct_path_dispatch,
            direct_path_workgroups,
            zero_gradient_reuse,
            sparse_fine_enabled,
            sparse_fine_available: sparse_fine_enabled,
            sparse_coarse_enabled,
            sparse_coarse_active_bins_enabled,
            sparse_coarse_adaptive_enabled,
            sparse_tile_bounds_enabled,
            sparse_fine_quad_skip_enabled,
            sparse_opaque_prune_enabled,
            sparse_solid_fuse_enabled,
            sparse_segment_row_mask_enabled,
            canonical_segment_order_enabled,
            area_ppt8_enabled,
            clip_single_group_enabled,
            ..Default::default()
        }
    }

    pub fn initialize_gpu_timestamps(&mut self, device: &Device) {
        self.gpu_timestamps.initialize(device);
        self.gpu_dispatch_timestamps.initialize(device);
    }

    pub fn gpu_timestamp_status(&mut self) -> GpuTimestampStatus {
        self.gpu_timestamps.status()
    }

    pub fn take_gpu_timestamp_sample(&mut self) -> Option<GpuTimestampSample> {
        self.gpu_timestamps.take_sample()
    }

    pub fn gpu_timestamp_latest_submission_id(&self) -> Option<u64> {
        self.gpu_timestamps.latest_submission_id()
    }

    pub fn gpu_timestamp_buffer_bytes(&self) -> u64 {
        self.gpu_timestamps.buffer_bytes()
    }

    pub fn set_compute_pass_batching(&mut self, enabled: bool) {
        self.compute_batch = u8::from(enabled);
    }

    /// Whether shared compute-pass encoding is effective for new recordings.
    ///
    /// The release default is mode 2; full CPU and profiler builds retain
    /// their existing unbatched execution paths.
    pub fn compute_pass_batching_enabled(&self) -> bool {
        self.compute_batch != 0 && !self.use_cpu && !cfg!(feature = "wgpu-profiler")
    }

    pub fn cpu_pathtag_enabled(&self) -> bool {
        self.cpu_pathtag
    }

    pub fn cpu_pathtag_reuse_enabled(&self) -> bool {
        self.cpu_pathtag_reuse_active()
    }

    pub(crate) fn cpu_pathtag_pool_retained_bytes(&self) -> u64 {
        self.cpu_pathtag_pool.retained_bytes()
    }

    pub fn cpu_metadata_enabled(&self) -> bool {
        self.cpu_metadata_active()
    }

    pub(crate) fn cpu_metadata_pool_retained_bytes(&self) -> u64 {
        self.cpu_metadata_pool.retained_bytes()
    }

    pub fn set_cpu_metadata_enabled(&mut self, enabled: bool) {
        self.cpu_metadata = enabled && !self.use_cpu;
        if !self.cpu_metadata_active() {
            self.cpu_metadata_pool.clear();
        }
    }

    pub fn cpu_draw_prefix_enabled(&self) -> bool {
        self.cpu_draw_prefix_active()
    }

    pub(crate) fn cpu_draw_prefix_pool_retained_bytes(&self) -> u64 {
        self.cpu_draw_prefix_pool.retained_bytes()
    }

    pub(crate) fn cpu_draw_prefix_supported(&self, bytes: u64) -> bool {
        self.cpu_draw_prefix_active()
            && usize::try_from(bytes).is_ok_and(|bytes| bytes <= CPU_DRAW_PREFIX_SLOT_BYTES)
    }

    pub fn set_cpu_draw_prefix_enabled(&mut self, enabled: bool) {
        self.cpu_draw_prefix = enabled && !self.use_cpu;
        if !self.cpu_draw_prefix_active() {
            self.cpu_draw_prefix_pool.clear();
        }
    }

    /// Whether the optional direct GPU path-dispatch variants may be selected.
    ///
    /// Full CPU rendering never uses GPU dispatch variants, even when this
    /// experiment was requested at construction time.
    pub fn direct_path_dispatch_enabled(&self) -> bool {
        self.direct_path_dispatch && !self.use_cpu
    }

    /// Effective nonzero workgroup count for both direct path stages.
    pub fn direct_path_workgroups(&self) -> u32 {
        self.direct_path_workgroups
    }

    /// Whether no-ramp fine passes reuse the engine-owned zero gradient.
    ///
    /// This is enabled by default and can be disabled only with
    /// `VELLO_ZERO_GRADIENT_REUSE=0` for an A/B comparison.
    pub fn zero_gradient_reuse_enabled(&self) -> bool {
        self.zero_gradient_reuse
    }

    /// Return the stable, explicitly-zeroed fallback gradient image.
    ///
    /// The texture is deliberately retained by `BindMap` for the engine's
    /// lifetime. It must not be released by a frame recording.
    pub(crate) fn ensure_zero_gradient(&mut self, device: &Device, queue: &Queue) -> ImageProxy {
        if let Some(proxy) = self.zero_gradient {
            return proxy;
        }
        let proxy = ImageProxy::new(1, 1, crate::recording::ImageFormat::Rgba8);
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vello.zero_gradient"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
            format,
            view_formats: &[],
        });
        let texture_view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("vello.zero_gradient_view"),
            dimension: Some(TextureViewDimension::D2),
            usage: None,
            aspect: TextureAspect::All,
            mip_level_count: None,
            base_mip_level: 0,
            base_array_layer: 0,
            array_layer_count: None,
            format: Some(format),
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: TextureAspect::All,
            },
            &[0, 0, 0, 0],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        self.bind_map.insert_image(proxy.id, texture, texture_view);
        self.zero_gradient = Some(proxy);
        #[cfg(test)]
        {
            self.bind_map.zero_gradient_id = Some(proxy.id);
            self.zero_gradient_allocations += 1;
        }
        proxy
    }

    #[cfg(test)]
    pub(crate) fn zero_gradient_test_stats(&self) -> (u64, u64) {
        (
            self.zero_gradient_allocations,
            self.bind_map.zero_gradient_bind_group_cache_hits,
        )
    }

    #[cfg(test)]
    pub(crate) fn zero_gradient_test_image_state(&self) -> (Option<ResourceId>, u64, usize, u64) {
        let fallback_id = self.zero_gradient.map(|proxy| proxy.id);
        let fallback_bytes = fallback_id
            .and_then(|id| self.bind_map.image_map.get(&id))
            .map(|(texture, _)| {
                u64::from(texture.width())
                    .saturating_mul(u64::from(texture.height()))
                    .saturating_mul(u64::from(texture.depth_or_array_layers()))
                    .saturating_mul(u64::from(
                        texture.format().block_copy_size(None).unwrap_or(4),
                    ))
            })
            .unwrap_or(0);
        let other_bytes = self
            .bind_map
            .image_map
            .iter()
            .filter(|(id, _)| Some(**id) != fallback_id)
            .map(|(_, (texture, _))| {
                u64::from(texture.width())
                    .saturating_mul(u64::from(texture.height()))
                    .saturating_mul(u64::from(texture.depth_or_array_layers()))
                    .saturating_mul(u64::from(
                        texture.format().block_copy_size(None).unwrap_or(4),
                    ))
            })
            .sum();
        (
            fallback_id,
            fallback_bytes,
            self.bind_map.image_map.len(),
            other_bytes,
        )
    }

    #[cfg(test)]
    pub(crate) fn set_zero_gradient_reuse_for_test(&mut self, enabled: bool) {
        self.zero_gradient_reuse = enabled;
    }

    /// Toggle optional direct GPU path-dispatch variants for A/B comparison.
    pub fn set_direct_path_dispatch_enabled(&mut self, enabled: bool) {
        self.direct_path_dispatch = enabled;
    }

    pub fn set_cpu_pathtag_enabled(&mut self, enabled: bool) {
        self.cpu_pathtag = enabled;
        if !enabled {
            self.cpu_pathtag_pool.clear();
            self.cpu_metadata_pool.clear();
            self.cpu_draw_prefix_pool.clear();
        }
    }

    fn cpu_pathtag_reuse_active(&self) -> bool {
        self.cpu_pathtag && self.cpu_pathtag_reuse && !self.use_cpu
    }

    fn cpu_metadata_active(&self) -> bool {
        self.cpu_pathtag && self.cpu_metadata && !self.use_cpu
    }

    fn cpu_draw_prefix_active(&self) -> bool {
        self.cpu_pathtag && self.cpu_draw_prefix && !self.use_cpu
    }

    pub fn sparse_fine_enabled(&self) -> bool {
        self.sparse_fine_enabled
    }

    pub fn sparse_coarse_enabled(&self) -> bool {
        self.sparse_coarse_enabled
    }

    pub fn sparse_coarse_active_bins_enabled(&self) -> bool {
        self.sparse_coarse_active_bins_enabled
    }

    pub fn sparse_coarse_adaptive_enabled(&self) -> bool {
        self.sparse_coarse_adaptive_enabled
    }

    pub fn sparse_tile_bounds_enabled(&self) -> bool {
        self.sparse_tile_bounds_enabled
    }

    pub fn sparse_fine_quad_skip_enabled(&self) -> bool {
        self.sparse_fine_quad_skip_enabled
    }

    pub fn sparse_opaque_prune_enabled(&self) -> bool {
        self.sparse_opaque_prune_enabled
    }

    pub fn sparse_solid_fuse_enabled(&self) -> bool {
        self.sparse_solid_fuse_enabled
    }

    pub fn sparse_segment_row_mask_enabled(&self) -> bool {
        self.sparse_segment_row_mask_enabled
    }

    pub fn canonical_segment_order_enabled(&self) -> bool {
        self.canonical_segment_order_enabled
    }

    pub fn area_ppt8_enabled(&self) -> bool {
        self.area_ppt8_enabled
    }

    pub fn clip_single_group_enabled(&self) -> bool {
        self.clip_single_group_enabled
    }

    #[cfg(test)]
    pub(crate) fn enable_area_ppt8_fixture(&mut self) {
        self.area_ppt8_enabled = true;
    }

    #[cfg(test)]
    pub(crate) fn enable_clip_single_group_fixture(&mut self) {
        self.clip_single_group_enabled = true;
    }

    #[cfg(test)]
    pub(crate) fn enable_sparse_tile_bounds_fixture(&mut self, adaptive: bool) {
        self.sparse_fine_enabled = true;
        self.sparse_fine_available = true;
        self.sparse_coarse_enabled = true;
        self.sparse_coarse_active_bins_enabled = true;
        self.sparse_coarse_adaptive_enabled = adaptive;
        self.sparse_tile_bounds_enabled = true;
        self.sparse_fine_quad_skip_enabled = true;
    }

    #[cfg(test)]
    pub(crate) fn enable_sparse_opaque_prune_fixture(&mut self) {
        self.sparse_fine_enabled = true;
        self.sparse_fine_available = true;
        self.sparse_opaque_prune_enabled = true;
    }

    #[cfg(test)]
    pub(crate) fn enable_sparse_solid_fuse_fixture(&mut self) {
        self.sparse_fine_enabled = true;
        self.sparse_fine_available = true;
        self.sparse_solid_fuse_enabled = true;
    }

    #[cfg(test)]
    pub(crate) fn enable_sparse_segment_row_mask_fixture(&mut self, direct: bool) {
        self.sparse_fine_enabled = true;
        self.sparse_fine_available = true;
        self.sparse_coarse_enabled = true;
        self.sparse_coarse_active_bins_enabled = true;
        self.sparse_tile_bounds_enabled = true;
        self.direct_path_dispatch = direct;
        self.sparse_segment_row_mask_enabled = true;
    }

    pub(crate) fn enable_canonical_segment_order(&mut self) {
        self.canonical_segment_order_enabled = true;
    }

    #[cfg(test)]
    pub(crate) fn enable_canonical_segment_order_fixture(&mut self) {
        self.enable_canonical_segment_order();
    }

    pub fn set_sparse_fine_enabled(&mut self, enabled: bool) {
        self.sparse_fine_enabled = enabled && self.sparse_fine_available;
    }

    fn bindings_have_pending_clear(&self, bindings: &[ResourceProxy]) -> bool {
        bindings.iter().any(|resource| match resource {
            ResourceProxy::Buffer(proxy)
            | ResourceProxy::BufferRange {
                proxy,
                offset: _,
                size: _,
            } => self.bind_map.pending_clears.contains(&proxy.id),
            ResourceProxy::Image(_) => false,
        })
    }

    /// Enable creating any remaining shaders in parallel
    #[cfg(not(target_arch = "wasm32"))]
    pub fn use_parallel_initialisation(&mut self) {
        self.defer_shader_initialisation();
    }

    /// Register shaders without compiling them until a later build.
    pub fn defer_shader_initialisation(&mut self) {
        if self.shaders_to_initialise.is_some() {
            return;
        }
        self.shaders_to_initialise = Some(Vec::new());
    }

    /// Compile every deferred shader through the device's asynchronous
    /// pipeline creation (Tilo patch): every compile starts before the first
    /// is awaited, so a WebGPU browser compiles them in parallel off its GPU
    /// process's main thread. Elsewhere each is created synchronously.
    pub async fn build_shaders_async(&mut self, device: &Device) {
        let Some(new_shaders) = self.shaders_to_initialise.take() else {
            return;
        };
        let pending = new_shaders
            .into_iter()
            .map(|it| {
                let pipeline = Self::start_compute_pipeline(
                    device,
                    it.label,
                    it.wgsl,
                    it.entries,
                    self.pipeline_cache.as_ref(),
                );
                (it.shader_id, pipeline)
            })
            .collect::<Vec<_>>();
        for (id, pipeline) in pending {
            self.shaders[id.0].wgpu = Some(pipeline.await);
        }
    }

    /// Which registrations defer their pipeline (Tilo patch). `full_shaders`
    /// runs between setting a mode and resetting it to `None`.
    pub(crate) fn set_pipeline_deferral(&mut self, mode: crate::PipelineDeferral) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.deferred.mode = mode;
        }
        #[cfg(target_arch = "wasm32")]
        let _ = mode;
    }

    /// Mark the registrations that follow as ones a full frame on the
    /// default path never dispatches, until called with `false`.
    pub(crate) fn defer_unused(&mut self, unused: bool) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.deferred.unused = unused;
        }
        #[cfg(target_arch = "wasm32")]
        let _ = unused;
    }

    /// Build every deferred pipeline on one background thread, largest
    /// first; frames install them as they arrive. `built` runs after the
    /// last, or at once when none is pending. Once only.
    pub(crate) fn start_deferred_pipelines(
        &mut self,
        device: &Device,
        built: Box<dyn FnOnce() + Send>,
    ) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.deferred.started {
                return;
            }
            self.deferred.started = true;
            if self.deferred.pending.is_empty() {
                built();
                return;
            }
            let mut work: Vec<UninitialisedShader> = self
                .deferred
                .specs
                .iter()
                .filter(|spec| self.deferred.pending.contains(&spec.shader_id.0))
                .cloned()
                .collect();
            work.sort_by_key(|spec| std::cmp::Reverse(spec.wgsl.len()));
            let (sender, receiver) = mpsc::channel();
            let device = device.clone();
            let pipeline_cache = self.pipeline_cache.clone();
            let spawned = std::thread::Builder::new()
                .name("vello-deferred-pipelines".into())
                .spawn(move || {
                    for spec in work {
                        let shader = Self::create_compute_pipeline(
                            &device,
                            spec.label,
                            spec.wgsl,
                            spec.entries,
                            pipeline_cache.as_ref(),
                        );
                        if sender.send((spec.shader_id, shader)).is_err() {
                            return;
                        }
                    }
                    built();
                });
            if spawned.is_ok() {
                self.deferred.built = Some(receiver);
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = device;
            built();
        }
    }

    /// Deferred pipelines not installed yet, and how many a dispatch had to
    /// build itself.
    pub(crate) fn deferred_pipeline_counts(&mut self) -> (usize, u64) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.receive_deferred();
            (self.deferred.pending.len(), self.deferred.built_inline)
        }
        #[cfg(target_arch = "wasm32")]
        (0, 0)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn install_deferred(&mut self, id: ShaderId, shader: WgpuShader) {
        if self.deferred.pending.remove(&id.0) {
            self.shaders[id.0].wgpu = Some(shader);
        }
    }

    /// Install whatever the builder has finished, without waiting.
    #[cfg(not(target_arch = "wasm32"))]
    fn receive_deferred(&mut self) {
        while let Some(built) = self.deferred.built.as_ref() {
            match built.try_recv() {
                Ok((id, shader)) => self.install_deferred(id, shader),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => self.deferred.built = None,
            }
        }
    }

    /// Every deferred GPU pipeline `recording` dispatches is installed before
    /// it is encoded: from the builder (waiting for it when it is still
    /// running), else built here. A dispatch never runs without its pipeline.
    #[cfg(not(target_arch = "wasm32"))]
    fn ensure_deferred(&mut self, device: &Device, recording: &Recording) {
        if self.deferred.pending.is_empty() {
            return;
        }
        self.receive_deferred();
        for command in &recording.commands {
            let (Command::Dispatch(id, ..) | Command::DispatchIndirect(id, ..)) = command else {
                continue;
            };
            if self.shaders[id.0].cpu.is_some() || !self.deferred.pending.contains(&id.0) {
                continue;
            }
            while self.deferred.pending.contains(&id.0) {
                let Some(built) = self.deferred.built.as_ref() else {
                    break;
                };
                match built.recv() {
                    Ok((built_id, shader)) => self.install_deferred(built_id, shader),
                    Err(_) => self.deferred.built = None,
                }
            }
            if self.deferred.pending.contains(&id.0)
                && let Some(spec) = self
                    .deferred
                    .specs
                    .iter()
                    .find(|spec| spec.shader_id.0 == id.0)
                    .cloned()
            {
                let shader = Self::create_compute_pipeline(
                    device,
                    spec.label,
                    spec.wgsl,
                    spec.entries,
                    self.pipeline_cache.as_ref(),
                );
                self.deferred.built_inline += 1;
                self.install_deferred(*id, shader);
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    /// Initialise (in parallel) any shaders which are yet to be created
    pub fn build_shaders_if_needed(
        &mut self,
        device: &Device,
        num_threads: Option<std::num::NonZeroUsize>,
    ) {
        use std::num::NonZeroUsize;

        if let Some(mut new_shaders) = self.shaders_to_initialise.take() {
            // Longest source first: each worker takes the next one as it
            // finishes, so the largest compiles (the fine stage, flatten,
            // coarse) start at once instead of last.
            new_shaders.sort_by_key(|shader| std::cmp::Reverse(shader.wgsl.len()));
            let num_threads = num_threads
                .map(NonZeroUsize::get)
                .unwrap_or_else(|| {
                    // Fallback onto a heuristic. This tries to not to use all threads.
                    // We keep the main thread blocked and not doing much whilst this is running,
                    // so we broadly leave two cores unused at the point of maximum parallelism
                    // (This choice is arbitrary, and could be tuned, although a 'proper' threadpool
                    // should probably be used instead)
                    std::thread::available_parallelism().map_or(2, |it| it.get().max(4) - 2)
                })
                .min(new_shaders.len());
            log::info!("Initialising shader modules in parallel using {num_threads} threads");
            let remainder = new_shaders.split_off(num_threads);
            let (tx, rx) = mpsc::channel::<(ShaderId, WgpuShader)>();

            // We expect each initialisation to take much longer than acquiring a lock, so we just
            // use a mutex for our work queue
            let work_queue = std::sync::Mutex::new(remainder.into_iter());
            let work_queue = &work_queue;
            std::thread::scope(|scope| {
                let pipeline_cache = self.pipeline_cache.as_ref();
                let tx = tx;
                new_shaders
                    .into_iter()
                    .map(|it| {
                        let tx = tx.clone();
                        std::thread::Builder::new()
                            .name("Vello shader initialisation worker thread".into())
                            .spawn_scoped(scope, move || {
                                let shader = Self::create_compute_pipeline(
                                    device,
                                    it.label,
                                    it.wgsl,
                                    it.entries,
                                    pipeline_cache,
                                );
                                // We know the rx can only be closed if all the tx references are dropped
                                tx.send((it.shader_id, shader)).unwrap();
                                while let Ok(mut guard) = work_queue.lock() {
                                    if let Some(value) = guard.next() {
                                        drop(guard);
                                        let shader = Self::create_compute_pipeline(
                                            device,
                                            value.label,
                                            value.wgsl,
                                            value.entries,
                                            pipeline_cache,
                                        );
                                        tx.send((value.shader_id, shader)).unwrap();
                                    } else {
                                        break;
                                    }
                                }
                                // Another thread panicked or we finished.
                                // If another thread panicked, we ignore that here and finish our processing
                                drop(tx);
                            })
                            .expect("failed to spawn thread");
                    })
                    .for_each(drop);
                // Drop the initial sender, to mean that there will be no more senders if and only if all other threads have finished
                drop(tx);

                while let Ok((id, value)) = rx.recv() {
                    self.shaders[id.0].wgpu = Some(value);
                }
            });
        }
    }

    /// Add a shader.
    ///
    /// This function is somewhat limited, it doesn't apply a label, only allows one bind group,
    /// doesn't support push constants, and entry point is hardcoded as "main".
    ///
    /// Maybe should do template instantiation here? But shader compilation pipeline feels maybe
    /// a bit separate.
    pub fn add_compute_shader(
        &mut self,
        device: &Device,
        label: &'static str,
        wgsl: Cow<'static, str>,
        layout: &[BindType],
        cpu_shader: CpuShaderType,
    ) -> ShaderId {
        let mut add = |shader| {
            let id = self.shaders.len();
            self.shaders.push(shader);
            ShaderId(id)
        };
        let cpu_pathtag = if is_cpu_pathtag_shader(label) {
            match &cpu_shader {
                CpuShaderType::Present(shader) => Some(CpuShader { shader: *shader }),
                CpuShaderType::Missing | CpuShaderType::Skipped => None,
            }
        } else {
            None
        };
        let cpu_metadata = if is_cpu_metadata_shader(label) {
            match &cpu_shader {
                CpuShaderType::Present(shader) => Some(CpuShader { shader: *shader }),
                CpuShaderType::Missing | CpuShaderType::Skipped => None,
            }
        } else {
            None
        };

        if self.use_cpu {
            match cpu_shader {
                CpuShaderType::Present(shader) => {
                    return add(Shader {
                        wgpu: None,
                        cpu: Some(CpuShader { shader }),
                        cpu_pathtag: None,
                        cpu_metadata: None,
                        label,
                    });
                }
                // This shader is unused in CPU mode, create a dummy shader
                CpuShaderType::Skipped => {
                    return add(Shader {
                        wgpu: None,
                        cpu: None,
                        cpu_pathtag: None,
                        cpu_metadata: None,
                        label,
                    });
                }
                // Create a GPU shader as we don't have a CPU shader
                CpuShaderType::Missing => {}
            }
        }

        let entries = Self::create_bind_group_layout_entries(
            layout.iter().map(|b| (*b, wgpu::ShaderStages::COMPUTE)),
        );
        #[cfg(not(target_arch = "wasm32"))]
        if defers_registration(&self.deferred) {
            let id = add(Shader {
                label,
                wgpu: None,
                cpu: None,
                cpu_pathtag,
                cpu_metadata,
            });
            self.deferred.pending.insert(id.0);
            self.deferred.specs.push(UninitialisedShader {
                wgsl,
                label,
                entries,
                shader_id: id,
            });
            return id;
        }
        if let Some(uninit) = self.shaders_to_initialise.as_mut() {
            let id = add(Shader {
                label,
                wgpu: None,
                cpu: None,
                cpu_pathtag,
                cpu_metadata,
            });
            uninit.push(UninitialisedShader {
                wgsl,
                label,
                entries,
                shader_id: id,
            });
            return id;
        }
        let wgpu = Self::create_compute_pipeline(
            device,
            label,
            wgsl,
            entries,
            self.pipeline_cache.as_ref(),
        );
        add(Shader {
            wgpu: Some(wgpu),
            cpu: None,
            cpu_pathtag,
            cpu_metadata,
            label,
        })
    }

    /// Register a CPU-only dispatch stage. Callers must record it only while
    /// their associated opt-in path is active.
    pub(crate) fn add_cpu_shader(
        &mut self,
        label: &'static str,
        shader: fn(u32, &[CpuBinding<'_>]),
    ) -> ShaderId {
        let id = self.shaders.len();
        self.shaders.push(Shader {
            label,
            wgpu: None,
            cpu: Some(CpuShader { shader }),
            cpu_pathtag: None,
            cpu_metadata: None,
        });
        ShaderId(id)
    }

    #[cfg(feature = "debug_layers")]
    pub fn add_render_shader(
        &mut self,
        device: &Device,
        label: &'static str,
        module: &wgpu::ShaderModule,
        vertex_main: &'static str,
        fragment_main: &'static str,
        topology: wgpu::PrimitiveTopology,
        color_attachment: wgpu::ColorTargetState,
        vertex_buffer: Option<wgpu::VertexBufferLayout<'_>>,
        bind_layout: &[(BindType, wgpu::ShaderStages)],
    ) -> ShaderId {
        let entries = Self::create_bind_group_layout_entries(bind_layout.iter().copied());
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &entries,
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module,
                entry_point: Some(vertex_main),
                buffers: vertex_buffer.as_slice(),
                compilation_options: PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module,
                entry_point: Some(fragment_main),
                targets: &[Some(color_attachment)],
                compilation_options: PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: Some(wgpu::Face::Back),
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: self.pipeline_cache.as_ref(),
        });
        let id = self.shaders.len();
        self.shaders.push(Shader {
            wgpu: Some(WgpuShader {
                pipeline: PipelineState::Render(pipeline),
                bind_group_layout,
                #[cfg(not(target_arch = "wasm32"))]
                entries,
                #[cfg(not(target_arch = "wasm32"))]
                source_len: 0,
            }),
            cpu: None,
            cpu_pathtag: None,
            cpu_metadata: None,
            label,
        });
        ShaderId(id)
    }

    #[expect(
        clippy::print_stderr,
        reason = "an explicitly requested diagnostic must report actual shared-pass sizes"
    )]
    fn encode_compute_batch(
        &mut self,
        commands: &[Command],
        transient_map: &mut TransientBindMap<'_>,
        device: &Device,
        queue: &Queue,
        encoder: &mut CommandEncoder,
        free_bufs: &mut HashSet<ResourceId>,
        free_images: &mut HashSet<ResourceId>,
        include_frees: bool,
    ) -> Result<usize> {
        let cpu_pathtag_reuse_active = self.cpu_pathtag_reuse_active();
        let cpu_metadata_active = self.cpu_metadata_active();
        let cpu_draw_prefix_active = self.cpu_draw_prefix_active();
        let mut prepared = SmallVec::<[BatchedDispatch; COMPUTE_BATCH_LIMIT]>::new();
        let mut consumed = 0usize;
        for (command_count, command) in commands.iter().enumerate() {
            // Mode 1 intentionally retains its original 16-command scan
            // limit. Mode 2 folds free bookkeeping into a run, so its inline
            // storage instead needs to be bounded by actual dispatches.
            if !include_frees && command_count == COMPUTE_BATCH_LIMIT {
                break;
            }
            if compute_batch_boundary(command, include_frees) {
                break;
            }
            if include_frees {
                // The fixed inline storage bounds GPU dispatches, rather than
                // zero-cost bookkeeping commands between them.
                match command {
                    Command::Dispatch(_, workgroups, _)
                        if prepared.len() == COMPUTE_BATCH_LIMIT =>
                    {
                        let (x, y, z) = *workgroups;
                        if x != 0 && y != 0 && z != 0 {
                            break;
                        }
                    }
                    Command::DispatchIndirect(..) if prepared.len() == COMPUTE_BATCH_LIMIT => {
                        break;
                    }
                    _ => {}
                }
            }
            match command {
                Command::Dispatch(shader_id, workgroups, bindings) => {
                    // `create_bind_group` may encode a lazy clear. Materializing these bindings
                    // while earlier dispatches are still prepared would encode that clear before
                    // them. End this run so it emits earlier dispatches first; then a fresh pass
                    // encodes this command's clear followed by this command's dispatch.
                    if !prepared.is_empty() && self.bindings_have_pending_clear(bindings) {
                        break;
                    }
                    let (x, y, z) = *workgroups;
                    if x == 0 || y == 0 || z == 0 {
                        consumed = consumed.saturating_add(1);
                        continue;
                    }
                    let (stage_id, pipeline, bind_group_layout) = {
                        let shader = &self.shaders[shader_id.0];
                        let ShaderKind::Wgpu(wgpu_shader) =
                            shader.select(self.cpu_pathtag, self.cpu_metadata_active())
                        else {
                            break;
                        };
                        #[cfg_attr(
                            not(feature = "debug_layers"),
                            expect(
                                irrefutable_let_patterns,
                                reason = "Render shaders are only enabled if we have the debug pipeline"
                            )
                        )]
                        let PipelineState::Compute(pipeline) = &wgpu_shader.pipeline else {
                            panic!("cannot issue a dispatch with a render pipeline");
                        };
                        (
                            self.gpu_dispatch_timestamps
                                .stage_id_for_label(shader.label),
                            pipeline.clone(),
                            wgpu_shader.bind_group_layout.clone(),
                        )
                    };
                    let bind_group = transient_map.create_bind_group(
                        &mut self.bind_map,
                        &mut self.pool,
                        cpu_pathtag_reuse_active.then_some(&mut self.cpu_pathtag_pool),
                        cpu_metadata_active.then_some(&mut self.cpu_metadata_pool),
                        cpu_draw_prefix_active.then_some(&mut self.cpu_draw_prefix_pool),
                        &mut self.bind_group_cache,
                        device,
                        queue,
                        encoder,
                        *shader_id,
                        &bind_group_layout,
                        bindings,
                    );
                    prepared.push(BatchedDispatch::Direct {
                        stage_id,
                        pipeline,
                        bind_group,
                        workgroups: *workgroups,
                    });
                }
                Command::DispatchIndirect(shader_id, proxy, offset, bindings) => {
                    // See the direct-dispatch case above: flush prepared dispatches before
                    // materializing a binding whose lazy clear must precede this dispatch.
                    if !prepared.is_empty() && self.bindings_have_pending_clear(bindings) {
                        break;
                    }
                    let (stage_id, pipeline, bind_group_layout) = {
                        let shader = &self.shaders[shader_id.0];
                        let ShaderKind::Wgpu(wgpu_shader) =
                            shader.select(self.cpu_pathtag, self.cpu_metadata_active())
                        else {
                            break;
                        };
                        #[cfg_attr(
                            not(feature = "debug_layers"),
                            expect(
                                irrefutable_let_patterns,
                                reason = "Render shaders are only enabled if we have the debug pipeline"
                            )
                        )]
                        let PipelineState::Compute(pipeline) = &wgpu_shader.pipeline else {
                            panic!("cannot issue a dispatch with a render pipeline");
                        };
                        (
                            self.gpu_dispatch_timestamps
                                .stage_id_for_label(shader.label),
                            pipeline.clone(),
                            wgpu_shader.bind_group_layout.clone(),
                        )
                    };
                    let bind_group = transient_map.create_bind_group(
                        &mut self.bind_map,
                        &mut self.pool,
                        cpu_pathtag_reuse_active.then_some(&mut self.cpu_pathtag_pool),
                        cpu_metadata_active.then_some(&mut self.cpu_metadata_pool),
                        cpu_draw_prefix_active.then_some(&mut self.cpu_draw_prefix_pool),
                        &mut self.bind_group_cache,
                        device,
                        queue,
                        encoder,
                        *shader_id,
                        &bind_group_layout,
                        bindings,
                    );
                    transient_map.materialize_gpu_buf_for_indirect(
                        &mut self.bind_map,
                        &mut self.pool,
                        cpu_pathtag_reuse_active.then_some(&mut self.cpu_pathtag_pool),
                        cpu_metadata_active.then_some(&mut self.cpu_metadata_pool),
                        cpu_draw_prefix_active.then_some(&mut self.cpu_draw_prefix_pool),
                        device,
                        queue,
                        proxy,
                    );
                    let buffer = self
                        .bind_map
                        .get_gpu_buf(proxy.id)
                        .ok_or(Error::UnavailableBufferUsed(
                            proxy.name,
                            "indirect dispatch",
                        ))?
                        .clone();
                    prepared.push(BatchedDispatch::Indirect {
                        stage_id,
                        pipeline,
                        bind_group,
                        buffer,
                        offset: *offset,
                    });
                }
                Command::FreeBuffer(..) | Command::FreeImage(..) => {
                    let recorded = record_deferred_free(command, free_bufs, free_images);
                    debug_assert!(recorded);
                }
                _ => unreachable!("batch boundary must have been handled before matching"),
            }
            consumed = consumed.saturating_add(1);
        }
        if !prepared.is_empty() {
            if self.compute_batch_log {
                eprintln!(
                    "vello: encoded shared compute pass with {} dispatches",
                    prepared.len()
                );
            }
            let timestamps = self.gpu_timestamps.reserve_pass();
            let timestamp_writes =
                timestamps
                    .as_ref()
                    .map(|timestamps| wgpu::ComputePassTimestampWrites {
                        query_set: &timestamps.query_set,
                        beginning_of_pass_write_index: Some(timestamps.beginning),
                        end_of_pass_write_index: Some(timestamps.end),
                    });
            let mut pass = encoder.begin_compute_pass(&ComputePassDescriptor {
                label: None,
                timestamp_writes,
            });
            for dispatch in prepared {
                match dispatch {
                    BatchedDispatch::Direct {
                        stage_id,
                        pipeline,
                        bind_group,
                        workgroups,
                    } => {
                        let dispatch_timestamps =
                            self.gpu_dispatch_timestamps.reserve_dispatch(stage_id);
                        if let Some(timestamps) = dispatch_timestamps.as_ref() {
                            pass.write_timestamp(&timestamps.query_set, timestamps.beginning);
                        }
                        pass.set_pipeline(&pipeline);
                        pass.set_bind_group(0, &bind_group, &[]);
                        pass.dispatch_workgroups(workgroups.0, workgroups.1, workgroups.2);
                        if let Some(timestamps) = dispatch_timestamps.as_ref() {
                            pass.write_timestamp(&timestamps.query_set, timestamps.end);
                        }
                    }
                    BatchedDispatch::Indirect {
                        stage_id,
                        pipeline,
                        bind_group,
                        buffer,
                        offset,
                    } => {
                        let dispatch_timestamps =
                            self.gpu_dispatch_timestamps.reserve_dispatch(stage_id);
                        if let Some(timestamps) = dispatch_timestamps.as_ref() {
                            pass.write_timestamp(&timestamps.query_set, timestamps.beginning);
                        }
                        pass.set_pipeline(&pipeline);
                        pass.set_bind_group(0, &bind_group, &[]);
                        pass.dispatch_workgroups_indirect(&buffer, offset);
                        if let Some(timestamps) = dispatch_timestamps.as_ref() {
                            pass.write_timestamp(&timestamps.query_set, timestamps.end);
                        }
                    }
                }
            }
        }
        Ok(consumed)
    }

    pub fn run_recording(
        &mut self,
        device: &Device,
        queue: &Queue,
        recording: &Recording,
        external_resources: &[ExternalResource<'_>],
        label: &'static str,
        #[cfg(feature = "wgpu-profiler")] profiler: &mut wgpu_profiler::GpuProfiler,
    ) -> Result<()> {
        self.run_recording_with_post_render(
            device,
            queue,
            recording,
            external_resources,
            label,
            |_| {},
            #[cfg(feature = "wgpu-profiler")]
            profiler,
        )
    }

    /// Encode a recording and caller commands into one command buffer.
    ///
    /// `post_render` runs after every Vello command is encoded and before the
    /// command buffer is submitted, preserving command ordering without a
    /// second queue submission.
    pub fn run_recording_with_post_render<F>(
        &mut self,
        device: &Device,
        queue: &Queue,
        recording: &Recording,
        external_resources: &[ExternalResource<'_>],
        label: &'static str,
        post_render: F,
        #[cfg(feature = "wgpu-profiler")] profiler: &mut wgpu_profiler::GpuProfiler,
    ) -> Result<()>
    where
        F: FnOnce(&mut CommandEncoder),
    {
        self.run_recording_with_post_render_checked(
            device,
            queue,
            recording,
            external_resources,
            label,
            post_render,
            || true,
            #[cfg(feature = "wgpu-profiler")]
            profiler,
        )
    }

    /// The gate runs after encoding, immediately before command-buffer submission.
    /// Rejection leaves encoded resources unusable: the owning renderer must be dropped.
    pub fn run_recording_with_post_render_checked<F, G>(
        &mut self,
        device: &Device,
        queue: &Queue,
        recording: &Recording,
        external_resources: &[ExternalResource<'_>],
        label: &'static str,
        post_render: F,
        before_submit: G,
        #[cfg(feature = "wgpu-profiler")] profiler: &mut wgpu_profiler::GpuProfiler,
    ) -> Result<()>
    where
        F: FnOnce(&mut CommandEncoder),
        G: FnOnce() -> bool,
    {
        self.run_recording_with_post_render_checked_indexed(
            device,
            queue,
            recording,
            external_resources,
            label,
            post_render,
            before_submit,
            #[cfg(feature = "wgpu-profiler")]
            profiler,
        )?;
        Ok(())
    }

    /// The indexed form exposes the exact submission containing the render
    /// and caller-appended commands. Most callers intentionally discard it.
    #[doc(hidden)]
    pub fn run_recording_with_post_render_checked_indexed<F, G>(
        &mut self,
        device: &Device,
        queue: &Queue,
        recording: &Recording,
        external_resources: &[ExternalResource<'_>],
        label: &'static str,
        post_render: F,
        before_submit: G,
        #[cfg(feature = "wgpu-profiler")] profiler: &mut wgpu_profiler::GpuProfiler,
    ) -> Result<wgpu::SubmissionIndex>
    where
        F: FnOnce(&mut CommandEncoder),
        G: FnOnce() -> bool,
    {
        #[cfg(not(target_arch = "wasm32"))]
        self.ensure_deferred(device, recording);
        let cpu_pathtag_reuse_active = self.cpu_pathtag_reuse_active();
        let cpu_metadata_active = self.cpu_metadata_active();
        let cpu_draw_prefix_active = self.cpu_draw_prefix_active();
        let mut free_bufs: HashSet<ResourceId> = HashSet::default();
        let mut free_images: HashSet<ResourceId> = HashSet::default();
        let mut transient_map = TransientBindMap::new(external_resources);
        self.gpu_timestamps.begin_recording(device, queue);
        self.gpu_dispatch_timestamps.begin_recording(device, queue);

        let mut encoder =
            device.create_command_encoder(&CommandEncoderDescriptor { label: Some(label) });
        #[cfg(feature = "wgpu-profiler")]
        let query = profiler.begin_query(label, &mut encoder);
        let mut command_index = 0usize;
        while command_index < recording.commands.len() {
            if self.compute_batch != 0 && !self.use_cpu && !cfg!(feature = "wgpu-profiler") {
                let consumed = self.encode_compute_batch(
                    &recording.commands[command_index..],
                    &mut transient_map,
                    device,
                    queue,
                    &mut encoder,
                    &mut free_bufs,
                    &mut free_images,
                    self.compute_batch == 2,
                )?;
                if consumed != 0 {
                    command_index = command_index.saturating_add(consumed);
                    continue;
                }
            }
            let command = &recording.commands[command_index];
            match command {
                Command::Upload(buf_proxy, bytes) => {
                    transient_map
                        .bufs
                        .insert(buf_proxy.id, TransientBuf::Cpu(bytes));
                    // TODO: restrict VERTEX usage to "debug_layers" feature?
                    let usage = BufferUsages::COPY_SRC
                        | BufferUsages::COPY_DST
                        | BufferUsages::STORAGE
                        | BufferUsages::VERTEX;
                    let buf = self
                        .pool
                        .get_buf(buf_proxy.size, buf_proxy.name, usage, device);
                    // TODO: if buffer is newly created, might be better to make it mapped at creation
                    // and copy. However, we expect reuse will be most common.
                    queue.write_buffer(&buf, 0, bytes);
                    self.bind_map.insert_buf(buf_proxy, buf);
                }
                Command::UploadUniform(buf_proxy, bytes) => {
                    transient_map
                        .bufs
                        .insert(buf_proxy.id, TransientBuf::Cpu(bytes));
                    let usage = BufferUsages::UNIFORM | BufferUsages::COPY_DST;
                    // Same consideration as above
                    let buf = self
                        .pool
                        .get_buf(buf_proxy.size, buf_proxy.name, usage, device);
                    queue.write_buffer(&buf, 0, bytes);
                    self.bind_map.insert_buf(buf_proxy, buf);
                }
                Command::UploadImage(image_proxy, bytes) => {
                    let format = image_proxy.format.to_wgpu();
                    let block_size = format
                        .block_copy_size(None)
                        .expect("ImageFormat must have a valid block size");
                    let texture = device.create_texture(&wgpu::TextureDescriptor {
                        label: None,
                        size: wgpu::Extent3d {
                            width: image_proxy.width,
                            height: image_proxy.height,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
                        format,
                        view_formats: &[],
                    });
                    let texture_view = texture.create_view(&wgpu::TextureViewDescriptor {
                        label: None,
                        dimension: Some(TextureViewDimension::D2),
                        usage: None,
                        aspect: TextureAspect::All,
                        mip_level_count: None,
                        base_mip_level: 0,
                        base_array_layer: 0,
                        array_layer_count: None,
                        format: Some(format),
                    });
                    queue.write_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: &texture,
                            mip_level: 0,
                            origin: wgpu::Origin3d { x: 0, y: 0, z: 0 },
                            aspect: TextureAspect::All,
                        },
                        bytes,
                        wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(image_proxy.width * block_size),
                            rows_per_image: None,
                        },
                        wgpu::Extent3d {
                            width: image_proxy.width,
                            height: image_proxy.height,
                            depth_or_array_layers: 1,
                        },
                    );
                    self.bind_map
                        .insert_image(image_proxy.id, texture, texture_view);
                }
                Command::WriteImage(proxy, [x, y], image) => {
                    let (texture, _) = self.bind_map.get_or_create_image(*proxy, device);
                    let format = proxy.format.to_wgpu();
                    let block_size = format
                        .block_copy_size(None)
                        .expect("ImageFormat must have a valid block size");
                    let drawn = self.image_sources.write(
                        device,
                        &mut encoder,
                        image.data.id(),
                        &crate::image_source::AtlasSlot {
                            atlas: texture,
                            at: [*x, *y],
                            size: [image.width, image.height],
                        },
                    );
                    if drawn {
                        // Drawn from its `ImageSource`: the one copy.
                    } else if let Some(overrider) = self.image_overrides.get(&image.data.id()) {
                        encoder.copy_texture_to_texture(
                            wgpu::TexelCopyTextureInfo {
                                texture: &overrider.texture,
                                mip_level: overrider.mip_level,
                                origin: overrider.origin,
                                aspect: overrider.aspect,
                            },
                            wgpu::TexelCopyTextureInfo {
                                texture,
                                mip_level: 0,
                                origin: wgpu::Origin3d { x: *x, y: *y, z: 0 },
                                aspect: TextureAspect::All,
                            },
                            wgpu::Extent3d {
                                width: image.width,
                                height: image.height,
                                depth_or_array_layers: 1,
                            },
                        );
                    } else {
                        let expected = (image.width as usize)
                            .saturating_mul(image.height as usize)
                            .saturating_mul(block_size as usize);
                        // An image whose pixels live in another renderer's
                        // texture: ask the registered source for them, and
                        // draw it transparent rather than abort when there
                        // is none.
                        let fetched;
                        let data = if image.data.is_empty() && expected != 0 {
                            fetched = crate::external_image_pixels(image.data.id())
                                .filter(|pixels| pixels.len() >= expected)
                                .unwrap_or_else(|| {
                                    log::warn!(
                                        "image {} has no pixels for this renderer; drawing it transparent",
                                        image.data.id()
                                    );
                                    vec![0; expected]
                                });
                            fetched.as_slice()
                        } else {
                            image.data.data()
                        };
                        queue.write_texture(
                            wgpu::TexelCopyTextureInfo {
                                texture,
                                mip_level: 0,
                                origin: wgpu::Origin3d { x: *x, y: *y, z: 0 },
                                aspect: TextureAspect::All,
                            },
                            data,
                            wgpu::TexelCopyBufferLayout {
                                offset: 0,
                                bytes_per_row: Some(image.width * block_size),
                                rows_per_image: None,
                            },
                            wgpu::Extent3d {
                                width: image.width,
                                height: image.height,
                                depth_or_array_layers: 1,
                            },
                        );
                    }
                }
                Command::Dispatch(shader_id, wg_size, bindings) => {
                    let (x, y, z) = *wg_size;
                    // println!("dispatching {:?} with {} bindings", wg_size, bindings.len());
                    let shader = &self.shaders[shader_id.0];
                    match shader.select(self.cpu_pathtag, self.cpu_metadata_active()) {
                        ShaderKind::Cpu(cpu_shader) => {
                            // The current strategy is to run the CPU shader synchronously. This
                            // works because there is currently the added constraint that data
                            // can only flow from CPU to GPU, not the other way around. If and
                            // when we implement that, we will need to defer the execution. Of
                            // course, we will also need to wire up more async synchronization
                            // mechanisms, as the CPU dispatch can't run until the preceding
                            // command buffer submission completes (and, in WebGPU, the async
                            // mapping operations on the buffers completes).
                            let resources = transient_map.create_cpu_resources(
                                &mut self.bind_map,
                                cpu_pathtag_reuse_active.then_some(&mut self.cpu_pathtag_pool),
                                cpu_metadata_active.then_some(&mut self.cpu_metadata_pool),
                                cpu_draw_prefix_active.then_some(&mut self.cpu_draw_prefix_pool),
                                bindings,
                            );
                            (cpu_shader.shader)(x, &resources);
                        }
                        ShaderKind::Wgpu(wgpu_shader) => {
                            // Workaround for https://github.com/linebender/vello/issues/637
                            if x == 0 || y == 0 || z == 0 {
                                command_index = command_index.saturating_add(1);
                                continue;
                            }
                            let stage_id = self
                                .gpu_dispatch_timestamps
                                .stage_id_for_label(self.shaders[shader_id.0].label);
                            let dispatch_timestamps =
                                self.gpu_dispatch_timestamps.reserve_dispatch(stage_id);
                            let bind_group = transient_map.create_bind_group(
                                &mut self.bind_map,
                                &mut self.pool,
                                cpu_pathtag_reuse_active.then_some(&mut self.cpu_pathtag_pool),
                                cpu_metadata_active.then_some(&mut self.cpu_metadata_pool),
                                cpu_draw_prefix_active.then_some(&mut self.cpu_draw_prefix_pool),
                                &mut self.bind_group_cache,
                                device,
                                queue,
                                &mut encoder,
                                *shader_id,
                                &wgpu_shader.bind_group_layout,
                                bindings,
                            );
                            let timestamps = self.gpu_timestamps.reserve_pass();
                            let timestamp_writes = timestamps.as_ref().map(|timestamps| {
                                wgpu::ComputePassTimestampWrites {
                                    query_set: &timestamps.query_set,
                                    beginning_of_pass_write_index: Some(timestamps.beginning),
                                    end_of_pass_write_index: Some(timestamps.end),
                                }
                            });
                            let mut cpass = encoder.begin_compute_pass(&ComputePassDescriptor {
                                label: None,
                                timestamp_writes,
                            });
                            if let Some(timestamps) = dispatch_timestamps.as_ref() {
                                cpass.write_timestamp(&timestamps.query_set, timestamps.beginning);
                            }
                            #[cfg(feature = "wgpu-profiler")]
                            let query = profiler
                                .begin_query(shader.label, &mut cpass)
                                .with_parent(Some(&query));
                            #[cfg_attr(
                                not(feature = "debug_layers"),
                                expect(
                                    irrefutable_let_patterns,
                                    reason = "Render shaders are only enabled if we have the debug pipeline"
                                )
                            )]
                            let PipelineState::Compute(pipeline) = &wgpu_shader.pipeline else {
                                panic!("cannot issue a dispatch with a render pipeline");
                            };
                            cpass.set_pipeline(pipeline);
                            cpass.set_bind_group(0, &bind_group, &[]);
                            cpass.dispatch_workgroups(x, y, z);
                            if let Some(timestamps) = dispatch_timestamps.as_ref() {
                                cpass.write_timestamp(&timestamps.query_set, timestamps.end);
                            }
                            #[cfg(feature = "wgpu-profiler")]
                            profiler.end_query(&mut cpass, query);
                        }
                    }
                }
                Command::DispatchIndirect(shader_id, proxy, offset, bindings) => {
                    let shader = &self.shaders[shader_id.0];
                    match shader.select(self.cpu_pathtag, self.cpu_metadata_active()) {
                        ShaderKind::Cpu(cpu_shader) => {
                            // Same consideration as above about running the CPU shader synchronously.
                            let n_wg;
                            if let CpuBinding::BufferRW(b) = self.bind_map.get_cpu_buf(proxy.id) {
                                let slice = b.borrow();
                                let indirect: &[u32] = bytemuck::cast_slice(&slice);
                                n_wg = indirect[0];
                            } else {
                                panic!("indirect buffer missing from bind map");
                            }
                            let resources = transient_map.create_cpu_resources(
                                &mut self.bind_map,
                                cpu_pathtag_reuse_active.then_some(&mut self.cpu_pathtag_pool),
                                cpu_metadata_active.then_some(&mut self.cpu_metadata_pool),
                                cpu_draw_prefix_active.then_some(&mut self.cpu_draw_prefix_pool),
                                bindings,
                            );
                            (cpu_shader.shader)(n_wg, &resources);
                        }
                        ShaderKind::Wgpu(wgpu_shader) => {
                            let stage_id = self
                                .gpu_dispatch_timestamps
                                .stage_id_for_label(self.shaders[shader_id.0].label);
                            let dispatch_timestamps =
                                self.gpu_dispatch_timestamps.reserve_dispatch(stage_id);
                            let bind_group = transient_map.create_bind_group(
                                &mut self.bind_map,
                                &mut self.pool,
                                cpu_pathtag_reuse_active.then_some(&mut self.cpu_pathtag_pool),
                                cpu_metadata_active.then_some(&mut self.cpu_metadata_pool),
                                cpu_draw_prefix_active.then_some(&mut self.cpu_draw_prefix_pool),
                                &mut self.bind_group_cache,
                                device,
                                queue,
                                &mut encoder,
                                *shader_id,
                                &wgpu_shader.bind_group_layout,
                                bindings,
                            );
                            transient_map.materialize_gpu_buf_for_indirect(
                                &mut self.bind_map,
                                &mut self.pool,
                                cpu_pathtag_reuse_active.then_some(&mut self.cpu_pathtag_pool),
                                cpu_metadata_active.then_some(&mut self.cpu_metadata_pool),
                                cpu_draw_prefix_active.then_some(&mut self.cpu_draw_prefix_pool),
                                device,
                                queue,
                                proxy,
                            );
                            let timestamps = self.gpu_timestamps.reserve_pass();
                            let timestamp_writes = timestamps.as_ref().map(|timestamps| {
                                wgpu::ComputePassTimestampWrites {
                                    query_set: &timestamps.query_set,
                                    beginning_of_pass_write_index: Some(timestamps.beginning),
                                    end_of_pass_write_index: Some(timestamps.end),
                                }
                            });
                            let mut cpass = encoder.begin_compute_pass(&ComputePassDescriptor {
                                label: None,
                                timestamp_writes,
                            });
                            if let Some(timestamps) = dispatch_timestamps.as_ref() {
                                cpass.write_timestamp(&timestamps.query_set, timestamps.beginning);
                            }
                            #[cfg(feature = "wgpu-profiler")]
                            let query = profiler
                                .begin_query(shader.label, &mut cpass)
                                .with_parent(Some(&query));
                            #[cfg_attr(
                                not(feature = "debug_layers"),
                                expect(
                                    irrefutable_let_patterns,
                                    reason = "Render shaders are only enabled if we have the debug pipeline"
                                )
                            )]
                            let PipelineState::Compute(pipeline) = &wgpu_shader.pipeline else {
                                panic!("cannot issue a dispatch with a render pipeline");
                            };
                            cpass.set_pipeline(pipeline);
                            cpass.set_bind_group(0, &bind_group, &[]);
                            let buf = self.bind_map.get_gpu_buf(proxy.id).ok_or(
                                Error::UnavailableBufferUsed(proxy.name, "indirect dispatch"),
                            )?;
                            cpass.dispatch_workgroups_indirect(buf, *offset);
                            if let Some(timestamps) = dispatch_timestamps.as_ref() {
                                cpass.write_timestamp(&timestamps.query_set, timestamps.end);
                            }
                            #[cfg(feature = "wgpu-profiler")]
                            profiler.end_query(&mut cpass, query);
                        }
                    }
                }
                #[cfg(feature = "debug_layers")]
                Command::Draw(draw_params) => {
                    let shader = &self.shaders[draw_params.shader_id.0];
                    #[cfg(feature = "wgpu-profiler")]
                    let label = shader.label;
                    let ShaderKind::Wgpu(shader) =
                        shader.select(self.cpu_pathtag, self.cpu_metadata_active())
                    else {
                        panic!("a render pass does not have a CPU equivalent");
                    };
                    let bind_group = transient_map.create_bind_group(
                        &mut self.bind_map,
                        &mut self.pool,
                        cpu_pathtag_reuse_active.then_some(&mut self.cpu_pathtag_pool),
                        cpu_metadata_active.then_some(&mut self.cpu_metadata_pool),
                        cpu_draw_prefix_active.then_some(&mut self.cpu_draw_prefix_pool),
                        &mut self.bind_group_cache,
                        device,
                        queue,
                        &mut encoder,
                        draw_params.shader_id,
                        &shader.bind_group_layout,
                        &draw_params.resources,
                    );
                    let render_target = transient_map
                        .materialize_external_image_for_render_pass(&draw_params.target);
                    let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: None,
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: render_target,
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: match draw_params.clear_color {
                                    Some(c) => wgpu::LoadOp::Clear(wgpu::Color {
                                        r: c[0] as f64,
                                        g: c[1] as f64,
                                        b: c[2] as f64,
                                        a: c[3] as f64,
                                    }),
                                    None => wgpu::LoadOp::Load,
                                },
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        occlusion_query_set: None,
                        timestamp_writes: None,
                        multiview_mask: None,
                    });
                    #[cfg(feature = "wgpu-profiler")]
                    let query = profiler
                        .begin_query(label, &mut rpass)
                        .with_parent(Some(&query));
                    let PipelineState::Render(pipeline) = &shader.pipeline else {
                        panic!("cannot issue a draw with a compute pipeline");
                    };
                    rpass.set_pipeline(pipeline);
                    if let Some(proxy) = draw_params.vertex_buffer {
                        // TODO: need a way to materialize a CPU initialized buffer. For now assume
                        // buffer exists? Also, need to materialize this buffer with vertex usage
                        let buf = self
                            .bind_map
                            .get_gpu_buf(proxy.id)
                            .ok_or(Error::UnavailableBufferUsed(proxy.name, "draw"))?;
                        rpass.set_vertex_buffer(0, buf.slice(..));
                    }
                    rpass.set_bind_group(0, &bind_group, &[]);
                    rpass.draw(0..draw_params.vertex_count, 0..draw_params.instance_count);
                    #[cfg(feature = "wgpu-profiler")]
                    profiler.end_query(&mut rpass, query);
                }
                Command::Download(proxy) => {
                    let src_buf = self
                        .bind_map
                        .get_gpu_buf(proxy.id)
                        .ok_or(Error::UnavailableBufferUsed(proxy.name, "download"))?;
                    let usage = BufferUsages::MAP_READ | BufferUsages::COPY_DST;
                    let buf = self.pool.get_buf(proxy.size, "download", usage, device);
                    encoder.copy_buffer_to_buffer(src_buf, 0, &buf, 0, proxy.size);
                    self.downloads.insert(proxy.id, buf);
                }
                Command::Clear(proxy, offset, size) => {
                    if let Some(buf) = self.bind_map.get_buf(*proxy) {
                        match &buf.buffer {
                            MaterializedBuffer::Gpu(b) => encoder.clear_buffer(b, *offset, *size),
                            MaterializedBuffer::Cpu(b) => {
                                let mut slice = &mut b.borrow_mut()[*offset as usize..];
                                if let Some(size) = size {
                                    slice = &mut slice[..*size as usize];
                                }
                                slice.fill(0);
                            }
                        }
                    } else {
                        self.bind_map.pending_clears.insert(proxy.id);
                    }
                }
                Command::FreeBuffer(proxy) => {
                    free_bufs.insert(proxy.id);
                }
                Command::FreeImage(proxy) => {
                    free_images.insert(proxy.id);
                }
            }
            command_index = command_index.saturating_add(1);
        }
        #[cfg(feature = "wgpu-profiler")]
        profiler.end_query(&mut encoder, query);
        // TODO: This only actually needs to happen once per frame, but run_recording happens two or three times
        #[cfg(feature = "wgpu-profiler")]
        profiler.resolve_queries(&mut encoder);
        let timestamp_readback = self.gpu_timestamps.finish_recording(&mut encoder);
        let dispatch_timestamp_readback =
            self.gpu_dispatch_timestamps.finish_recording(&mut encoder);
        post_render(&mut encoder);
        let command_buffer = encoder.finish();
        if !before_submit() {
            // begin_recording assigned diagnostic identities before encoding. They do not
            // name submissions when this gate rejects the command buffer. The renderer is
            // reconstruct-only after abort, so discard both trackers (including older maps)
            // rather than exposing an identity/sample from an unusable recording.
            self.gpu_timestamps = GpuTimestampTracker::Disabled;
            self.gpu_dispatch_timestamps = GpuDispatchTimestampTracker::Disabled;
            return Err(Error::SubmissionAborted);
        }
        let submission = queue.submit(Some(command_buffer));
        if timestamp_readback {
            self.gpu_timestamps.map_after_submit();
        }
        if dispatch_timestamp_readback {
            self.gpu_dispatch_timestamps.map_after_submit();
        }
        for id in free_bufs {
            if let Some(buf) = self.bind_map.buf_map.remove(&id) {
                match buf.buffer {
                    MaterializedBuffer::Gpu(gpu_buf) => {
                        let props = BufferProperties {
                            size: gpu_buf.size(),
                            usages: gpu_buf.usage(),
                            name: buf.label,
                        };
                        self.pool.put(props, gpu_buf);
                    }
                    MaterializedBuffer::Cpu(cpu_buf) => match buf.label {
                        CPU_PATHTAG_REDUCED_LABEL | CPU_PATHTAG_MONOID_LABEL
                            if self.cpu_pathtag_reuse_active() =>
                        {
                            self.cpu_pathtag_pool.put(buf.label, cpu_buf.into_inner());
                        }
                        CPU_METADATA_PATH_BBOX_LABEL | CPU_METADATA_DRAW_REDUCED_LABEL
                            if self.cpu_metadata_active() =>
                        {
                            self.cpu_metadata_pool.put(buf.label, cpu_buf.into_inner());
                        }
                        CPU_DRAW_PREFIX_LABEL if self.cpu_draw_prefix_active() => {
                            self.cpu_draw_prefix_pool
                                .put(buf.label, cpu_buf.into_inner());
                        }
                        _ => {}
                    },
                }
            }
        }
        let zero_gradient_id = self.zero_gradient.map(|proxy| proxy.id);
        for id in free_images {
            // This is an engine-owned image, not a per-recording allocation.
            // Render normally avoids recording this free; keep the guard so a
            // future cleanup path cannot invalidate its stable bind-group key.
            if Some(id) == zero_gradient_id {
                continue;
            }
            if let Some((texture, _view)) = self.bind_map.image_map.remove(&id) {
                // TODO: have a pool to avoid needless re-allocation
                // Release it once the submitted work is done; dropping alone
                // leaves a browser's GPU memory to its garbage collector.
                texture.destroy();
            }
        }
        self.release_unused_pool();
        Ok(submission)
    }

    /// Destroy the free buffers [`ResourcePool::end_recording`] found unused,
    /// with the cached bind groups that name them.
    fn release_unused_pool(&mut self) {
        let unused = self.pool.end_recording();
        if unused.is_empty() {
            return;
        }
        self.bind_group_cache.retain(|key, _| {
            !key.resources.iter().any(|resource| {
                matches!(resource, BoundResource::Buffer(bound, _, _) if unused.contains(bound))
            })
        });
        for buffer in unused {
            buffer.destroy();
        }
    }

    /// Destroy free buffers no recording has taken for `keep` recordings
    /// (see [`POOL_KEEP_RECORDINGS`]); `None` keeps them until
    /// [`Self::clear_pool`], as upstream does.
    pub(crate) fn set_pool_keep(&mut self, keep: Option<u64>) {
        self.pool.keep = keep;
    }

    /// The scene got smaller for good: for the next few recordings, free
    /// buffers go after [`POOL_KEEP_SOON`] recordings untaken.
    pub(crate) fn shrink_pool_soon(&mut self) {
        if self.pool.keep.is_some() {
            self.pool.soon = POOL_SOON_RECORDINGS;
        }
    }

    pub fn get_download(&self, buf: BufferProxy) -> Option<&Buffer> {
        self.downloads.get(&buf.id)
    }

    pub fn take_download(&mut self, buf: BufferProxy) -> Option<Buffer> {
        self.downloads.remove(&buf.id)
    }

    /// Drop every buffer the pool is holding for reuse.
    ///
    /// The pool keeps a free list per size class and never gives one up,
    /// which is right while the sizes asked for stay the same. When the
    /// bump reservation changes ([`crate::bump_budget`]) the old classes
    /// stop being asked for, and without this they would be kept for the
    /// life of the process — the memory this whole exercise is about.
    /// Buffers in use are not in the pool, so this cannot pull one out
    /// from under a frame in flight.
    pub fn clear_pool(&mut self) {
        self.bind_group_cache.clear();
        // On WebGPU a dropped buffer lives until JavaScript collects it, so a
        // cleared pool would stay resident beside its replacements. destroy()
        // waits for submitted work that still uses the buffer.
        for (buffer, _) in self.pool.bufs.drain().flat_map(|(_, buffers)| buffers) {
            buffer.destroy();
        }
        self.cpu_pathtag_pool.clear();
        self.cpu_metadata_pool.clear();
        self.cpu_draw_prefix_pool.clear();
    }

    /// Remove cached bind groups that keep an external texture view alive.
    ///
    /// Damage renderers replace scratch textures independently of Vello's
    /// reusable buffer pool. Evicting only entries that reference the retired
    /// view releases that texture without throwing away unrelated bind groups
    /// or pooled buffers.
    pub fn invalidate_texture_bindings(&mut self, texture: &TextureView) {
        self.bind_group_cache.retain(|key, _| {
            !key.resources
                .iter()
                .any(|resource| matches!(resource, BoundResource::Image(view) if view == texture))
        });
    }

    /// Remove cached bind groups that retain an external buffer being
    /// replaced independently of Vello's normal resource pool.
    pub fn invalidate_buffer_bindings(&mut self, buffer: &Buffer) {
        self.bind_group_cache.retain(|key, _| {
            !key.resources.iter().any(|resource| {
                matches!(resource, BoundResource::Buffer(bound, _, _) if bound == buffer)
            })
        });
    }

    pub(crate) fn tracked_memory_bytes(&self) -> (u64, u64, u64) {
        let pooled = self
            .pool
            .bufs
            .iter()
            .map(|(properties, buffers)| {
                properties
                    .size
                    .saturating_mul(u64::try_from(buffers.len()).unwrap_or(u64::MAX))
            })
            .sum();
        let live_buffers = self
            .bind_map
            .buf_map
            .values()
            .map(|entry| match &entry.buffer {
                MaterializedBuffer::Gpu(buffer) => buffer.size(),
                MaterializedBuffer::Cpu(buffer) => {
                    u64::try_from(buffer.borrow().len()).unwrap_or(u64::MAX)
                }
            })
            .chain(self.downloads.values().map(Buffer::size))
            .sum();
        let images = self
            .bind_map
            .image_map
            .values()
            .map(|(texture, _)| {
                let bytes_per_block = texture.format().block_copy_size(None).unwrap_or(4);
                u64::from(texture.width())
                    .saturating_mul(u64::from(texture.height()))
                    .saturating_mul(u64::from(texture.depth_or_array_layers()))
                    .saturating_mul(u64::from(bytes_per_block))
            })
            .sum();
        (pooled, live_buffers, images)
    }

    /// Drop a completed download.
    pub fn free_download(&mut self, buf: BufferProxy) {
        if let Some(buffer) = self.downloads.remove(&buf.id) {
            buffer.destroy();
        }
    }

    /// Recycle the fixed-size adaptive bump-counter download.
    ///
    /// The caller must have successfully mapped and then unmapped this
    /// buffer. Keeping that state transition at the mapping call site avoids
    /// attempting to unmap a failed mapping (or unmapping twice), both of
    /// which are validation errors on wgpu.
    pub fn recycle_bump_download(&mut self, buf: BufferProxy) {
        if buf.name != "vello.bump_buf" {
            self.free_download(buf);
            return;
        }
        if let Some(buffer) = self.downloads.remove(&buf.id) {
            let props = BufferProperties {
                size: buffer.size(),
                usages: buffer.usage(),
                name: "download",
            };
            self.pool.put(props, buffer);
        }
    }

    fn create_bind_group_layout_entries(
        layout: impl Iterator<Item = (BindType, wgpu::ShaderStages)>,
    ) -> Vec<wgpu::BindGroupLayoutEntry> {
        layout
            .enumerate()
            .map(|(i, (bind_type, visibility))| match bind_type {
                BindType::Buffer | BindType::BufReadOnly => wgpu::BindGroupLayoutEntry {
                    binding: i as u32,
                    visibility,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage {
                            read_only: bind_type == BindType::BufReadOnly,
                        },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                BindType::Uniform => wgpu::BindGroupLayoutEntry {
                    binding: i as u32,
                    visibility,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                BindType::Image(format) | BindType::ImageRead(format) => {
                    wgpu::BindGroupLayoutEntry {
                        binding: i as u32,
                        visibility,
                        ty: if bind_type == BindType::ImageRead(format) {
                            wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                                view_dimension: TextureViewDimension::D2,
                                multisampled: false,
                            }
                        } else {
                            wgpu::BindingType::StorageTexture {
                                access: wgpu::StorageTextureAccess::WriteOnly,
                                format: format.to_wgpu(),
                                view_dimension: TextureViewDimension::D2,
                            }
                        },
                        count: None,
                    }
                }
            })
            .collect::<Vec<_>>()
    }

    fn create_compute_pipeline(
        device: &Device,
        label: &str,
        wgsl: Cow<'_, str>,
        entries: Vec<wgpu::BindGroupLayoutEntry>,
        cache: Option<&PipelineCache>,
    ) -> WgpuShader {
        let parts = ComputePipelineParts::new(device, label, wgsl, entries);
        let pipeline = device.create_compute_pipeline(&parts.descriptor(label, cache));
        parts.into_shader(pipeline)
    }

    /// [`Self::create_compute_pipeline`] through the device's asynchronous
    /// pipeline creation. The compile starts now, before the future is
    /// polled.
    fn start_compute_pipeline(
        device: &Device,
        label: &str,
        wgsl: Cow<'_, str>,
        entries: Vec<wgpu::BindGroupLayoutEntry>,
        cache: Option<&PipelineCache>,
    ) -> impl Future<Output = WgpuShader> + 'static {
        let parts = ComputePipelineParts::new(device, label, wgsl, entries);
        let pipeline = device.create_compute_pipeline_async(&parts.descriptor(label, cache));
        async move { parts.into_shader(pipeline.await) }
    }

    /// The compute shaders [`Self::dispatch_empty`] can run, largest source first, so that
    /// workers taking them in turn finish the longest compilations early.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn precompile_order(&self) -> Vec<usize> {
        let mut order: Vec<(usize, usize)> = self
            .shaders
            .iter()
            .enumerate()
            .filter_map(|(index, shader)| match shader.wgpu.as_ref()? {
                WgpuShader {
                    pipeline: PipelineState::Compute(_),
                    source_len,
                    ..
                } => Some((index, *source_len)),
                #[cfg(feature = "debug_layers")]
                _ => None,
            })
            .collect();
        order.sort_by(|a, b| b.1.cmp(&a.1));
        order.into_iter().map(|(index, _)| index).collect()
    }

    /// Dispatch shader `index`'s compute pipeline once over an empty grid, with stand-in
    /// resources of its layout's types, and wait for the device to finish.
    ///
    /// No invocation runs. What it buys is the driver's own work for a first dispatch: Mesa's
    /// CPU Vulkan driver compiles a pipeline's machine code then, not when the pipeline is
    /// created, and keeps the result in its shader disk cache.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn dispatch_empty(&self, device: &Device, queue: &Queue, index: usize) {
        let Some(WgpuShader {
            pipeline: PipelineState::Compute(pipeline),
            bind_group_layout,
            entries,
            ..
        }) = self
            .shaders
            .get(index)
            .and_then(|shader| shader.wgpu.as_ref())
        else {
            return;
        };
        let resources: Vec<BoundResource> = entries
            .iter()
            .map(|entry| Self::stand_in_resource(device, entry))
            .collect();
        let bindings: Vec<wgpu::BindGroupEntry<'_>> = resources
            .iter()
            .zip(entries)
            .map(|(resource, entry)| wgpu::BindGroupEntry {
                binding: entry.binding,
                resource: match resource {
                    BoundResource::Buffer(buffer, _, _) => buffer.as_entire_binding(),
                    BoundResource::Image(view) => wgpu::BindingResource::TextureView(view),
                },
            })
            .collect();
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: bind_group_layout,
            entries: &bindings,
        });
        let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_compute_pass(&ComputePassDescriptor::default());
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(0, 0, 0);
        }
        queue.submit(Some(encoder.finish()));
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn stand_in_resource(device: &Device, entry: &wgpu::BindGroupLayoutEntry) -> BoundResource {
        // Large enough for any fixed-size part a Vello shader declares; never read.
        const BUFFER_SIZE: u64 = 1 << 14;
        let texture = |format, usage| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: None,
                    size: wgpu::Extent3d {
                        width: 1,
                        height: 1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        match entry.ty {
            wgpu::BindingType::Buffer { ty, .. } => {
                let usage = match ty {
                    wgpu::BufferBindingType::Uniform => BufferUsages::UNIFORM,
                    wgpu::BufferBindingType::Storage { .. } => BufferUsages::STORAGE,
                };
                let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size: BUFFER_SIZE,
                    usage,
                    mapped_at_creation: false,
                });
                BoundResource::Buffer(buffer, 0, None)
            }
            wgpu::BindingType::StorageTexture { format, .. } => {
                BoundResource::Image(texture(format, TextureUsages::STORAGE_BINDING))
            }
            // Every sampled image Vello binds is `ImageFormat::Rgba8`.
            _ => BoundResource::Image(texture(
                wgpu::TextureFormat::Rgba8Unorm,
                TextureUsages::TEXTURE_BINDING,
            )),
        }
    }
}

impl BindMap {
    fn insert_buf(&mut self, proxy: &BufferProxy, buffer: Buffer) {
        self.buf_map.insert(
            proxy.id,
            BindMapBuffer {
                buffer: MaterializedBuffer::Gpu(buffer),
                label: proxy.name,
            },
        );
    }

    /// Get a buffer, only if it's on GPU.
    fn get_gpu_buf(&self, id: ResourceId) -> Option<&Buffer> {
        self.buf_map.get(&id).and_then(|b| match &b.buffer {
            MaterializedBuffer::Gpu(b) => Some(b),
            _ => None,
        })
    }

    /// Get a CPU buffer.
    ///
    /// Panics if buffer is not present or is on GPU.
    fn get_cpu_buf(&self, id: ResourceId) -> CpuBinding<'_> {
        match &self.buf_map[&id].buffer {
            MaterializedBuffer::Cpu(b) => CpuBinding::BufferRW(b),
            _ => panic!("getting cpu buffer, but it's on gpu"),
        }
    }

    fn materialize_cpu_buf(
        &mut self,
        buf: &BufferProxy,
        cpu_pathtag_pool: Option<&mut CpuPathtagPool>,
        cpu_metadata_pool: Option<&mut CpuMetadataPool>,
        cpu_draw_prefix_pool: Option<&mut CpuDrawPrefixPool>,
    ) {
        self.buf_map.entry(buf.id).or_insert_with(|| {
            let bytes = cpu_pathtag_pool
                .and_then(|pool| pool.take(buf.name, buf.size as usize))
                .or_else(|| {
                    cpu_metadata_pool.and_then(|pool| pool.take(buf.name, buf.size as usize))
                })
                .or_else(|| {
                    cpu_draw_prefix_pool.and_then(|pool| pool.take(buf.name, buf.size as usize))
                })
                .unwrap_or_else(|| vec![0; buf.size as usize]);
            let buffer = MaterializedBuffer::Cpu(RefCell::new(bytes));
            BindMapBuffer {
                buffer,
                // TODO: do we need to cfg this?
                label: buf.name,
            }
        });
    }

    fn insert_image(&mut self, id: ResourceId, image: Texture, image_view: TextureView) {
        self.image_map.insert(id, (image, image_view));
    }

    fn get_buf(&mut self, proxy: BufferProxy) -> Option<&BindMapBuffer> {
        self.buf_map.get(&proxy.id)
    }

    fn get_or_create_image(
        &mut self,
        proxy: ImageProxy,
        device: &Device,
    ) -> &(Texture, TextureView) {
        match self.image_map.entry(proxy.id) {
            Entry::Occupied(occupied) => occupied.into_mut(),
            Entry::Vacant(vacant) => {
                let format = proxy.format.to_wgpu();
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: None,
                    size: wgpu::Extent3d {
                        width: proxy.width,
                        height: proxy.height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    // Render attachment too: an `ImageSource` is drawn in.
                    usage: TextureUsages::TEXTURE_BINDING
                        | TextureUsages::COPY_DST
                        | TextureUsages::RENDER_ATTACHMENT,
                    format,
                    view_formats: &[],
                });
                let texture_view = texture.create_view(&wgpu::TextureViewDescriptor {
                    label: None,
                    usage: None,
                    dimension: Some(TextureViewDimension::D2),
                    aspect: TextureAspect::All,
                    mip_level_count: None,
                    base_mip_level: 0,
                    base_array_layer: 0,
                    array_layer_count: None,
                    format: Some(proxy.format.to_wgpu()),
                });
                vacant.insert((texture, texture_view))
            }
        }
    }
}

const SIZE_CLASS_BITS: u32 = 1;

impl ResourcePool {
    /// Get a buffer from the pool or create one.
    fn get_buf(
        &mut self,
        size: u64,
        name: &'static str,
        usage: BufferUsages,
        device: &Device,
    ) -> Buffer {
        let rounded_size = Self::size_class(size, SIZE_CLASS_BITS);
        let props = BufferProperties {
            size: rounded_size,
            usages: usage,
            name,
        };
        if let Some(buf_vec) = self.bufs.get_mut(&props)
            && let Some((buf, _)) = buf_vec.pop()
        {
            return buf;
        }
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(name),
            size: rounded_size,
            usage,
            mapped_at_creation: false,
        })
    }

    /// Keep `buf` for reuse, stamped with the current recording.
    fn put(&mut self, props: BufferProperties, buf: Buffer) {
        self.bufs
            .entry(props)
            .or_default()
            .push((buf, self.recordings));
    }

    /// A recording finished: take out every free buffer no recording has
    /// taken within the keep window, for the caller to destroy. Buffers are
    /// taken from the end of their list, so the ones left at its start are
    /// the class's surplus and go first.
    fn end_recording(&mut self) -> Vec<Buffer> {
        self.recordings += 1;
        let keep = if self.soon > 0 {
            self.soon -= 1;
            Some(POOL_KEEP_SOON)
        } else {
            self.keep
        };
        let Some(keep) = keep else {
            return Vec::new();
        };
        let oldest = self.recordings.saturating_sub(keep);
        let mut unused = Vec::new();
        self.bufs.retain(|_, bufs| {
            unused.extend(
                bufs.extract_if(.., |(_, returned)| *returned < oldest)
                    .map(|(buf, _)| buf),
            );
            !bufs.is_empty()
        });
        unused
    }

    /// Quantize a size up to the nearest size class.
    fn size_class(x: u64, bits: u32) -> u64 {
        if x > 1 << bits {
            let a = (x - 1).leading_zeros();
            let b = (x - 1) | (((u64::MAX / 2) >> bits) >> a);
            b + 1
        } else {
            1 << bits
        }
    }
}

impl BindMapBuffer {
    // Upload a buffer from CPU to GPU if needed.
    //
    // Note data flow is one way only, from CPU to GPU. Once this method is
    // called, the buffer is no longer materialized on CPU, and cannot be
    // accessed from a CPU shader.
    fn upload_if_needed(
        &mut self,
        proxy: &BufferProxy,
        device: &Device,
        queue: &Queue,
        pool: &mut ResourcePool,
        cpu_pathtag_pool: Option<&mut CpuPathtagPool>,
        cpu_metadata_pool: Option<&mut CpuMetadataPool>,
        cpu_draw_prefix_pool: Option<&mut CpuDrawPrefixPool>,
    ) {
        if let MaterializedBuffer::Cpu(cpu_buf) = &self.buffer {
            let usage = BufferUsages::COPY_SRC
                | BufferUsages::COPY_DST
                | BufferUsages::STORAGE
                | BufferUsages::INDIRECT;
            let buf = pool.get_buf(proxy.size, proxy.name, usage, device);
            queue.write_buffer(&buf, 0, &cpu_buf.borrow());
            match self.label {
                CPU_PATHTAG_REDUCED_LABEL | CPU_PATHTAG_MONOID_LABEL => {
                    if let Some(cpu_pathtag_pool) = cpu_pathtag_pool {
                        cpu_pathtag_pool.put(self.label, cpu_buf.take());
                    }
                }
                CPU_METADATA_PATH_BBOX_LABEL | CPU_METADATA_DRAW_REDUCED_LABEL => {
                    if let Some(cpu_metadata_pool) = cpu_metadata_pool {
                        cpu_metadata_pool.put(self.label, cpu_buf.take());
                    }
                }
                CPU_DRAW_PREFIX_LABEL => {
                    if let Some(cpu_draw_prefix_pool) = cpu_draw_prefix_pool {
                        cpu_draw_prefix_pool.put(self.label, cpu_buf.take());
                    }
                }
                _ => {}
            }
            self.buffer = MaterializedBuffer::Gpu(buf);
        }
    }
}

impl<'a> TransientBindMap<'a> {
    /// Create new transient bind map, seeded from external resources
    fn new(external_resources: &'a [ExternalResource<'_>]) -> Self {
        let mut bufs = HashMap::default();
        let mut images = HashMap::default();
        for resource in external_resources {
            match resource {
                ExternalResource::Buffer(proxy, gpu_buf) => {
                    bufs.insert(proxy.id, TransientBuf::Gpu(gpu_buf));
                }
                ExternalResource::Image(proxy, gpu_image) => {
                    images.insert(proxy.id, *gpu_image);
                }
            }
        }
        TransientBindMap { bufs, images }
    }

    fn materialize_gpu_buf_for_indirect(
        &mut self,
        bind_map: &mut BindMap,
        pool: &mut ResourcePool,
        cpu_pathtag_pool: Option<&mut CpuPathtagPool>,
        cpu_metadata_pool: Option<&mut CpuMetadataPool>,
        cpu_draw_prefix_pool: Option<&mut CpuDrawPrefixPool>,
        device: &Device,
        queue: &Queue,
        buf: &BufferProxy,
    ) {
        if !self.bufs.contains_key(&buf.id)
            && let Some(b) = bind_map.buf_map.get_mut(&buf.id)
        {
            b.upload_if_needed(
                buf,
                device,
                queue,
                pool,
                cpu_pathtag_pool,
                cpu_metadata_pool,
                cpu_draw_prefix_pool,
            );
        }
    }

    #[cfg(feature = "debug_layers")]
    fn materialize_external_image_for_render_pass(&mut self, proxy: &ImageProxy) -> &TextureView {
        // TODO: Maybe this should support instantiating a transient texture. Right now all render
        // passes target a `SurfaceTexture`, so supporting external textures is sufficient.
        self.images
            .get(&proxy.id)
            .expect("texture not materialized")
    }

    fn create_bind_group(
        &mut self,
        bind_map: &mut BindMap,
        pool: &mut ResourcePool,
        mut cpu_pathtag_pool: Option<&mut CpuPathtagPool>,
        mut cpu_metadata_pool: Option<&mut CpuMetadataPool>,
        mut cpu_draw_prefix_pool: Option<&mut CpuDrawPrefixPool>,
        bind_group_cache: &mut HashMap<BindGroupKey, BindGroup>,
        device: &Device,
        queue: &Queue,
        encoder: &mut CommandEncoder,
        shader: ShaderId,
        layout: &BindGroupLayout,
        bindings: &[ResourceProxy],
    ) -> BindGroup {
        for proxy in bindings {
            match proxy {
                ResourceProxy::Buffer(proxy)
                | ResourceProxy::BufferRange {
                    proxy,
                    offset: _,
                    size: _,
                } => {
                    if self.bufs.contains_key(&proxy.id) {
                        continue;
                    }
                    match bind_map.buf_map.entry(proxy.id) {
                        Entry::Vacant(v) => {
                            // TODO: only some buffers will need indirect & vertex, but does it hurt?
                            let usage = BufferUsages::COPY_SRC
                                | BufferUsages::COPY_DST
                                | BufferUsages::STORAGE
                                | BufferUsages::INDIRECT
                                | BufferUsages::VERTEX;
                            let buf = pool.get_buf(proxy.size, proxy.name, usage, device);
                            if bind_map.pending_clears.remove(&proxy.id) {
                                encoder.clear_buffer(&buf, 0, None);
                            }
                            v.insert(BindMapBuffer {
                                buffer: MaterializedBuffer::Gpu(buf),
                                label: proxy.name,
                            });
                        }
                        Entry::Occupied(mut o) => {
                            o.get_mut().upload_if_needed(
                                proxy,
                                device,
                                queue,
                                pool,
                                cpu_pathtag_pool.as_deref_mut(),
                                cpu_metadata_pool.as_deref_mut(),
                                cpu_draw_prefix_pool.as_deref_mut(),
                            );
                        }
                    }
                }
                ResourceProxy::Image(proxy) => {
                    if self.images.contains_key(&proxy.id) {
                        continue;
                    }
                    if let Entry::Vacant(v) = bind_map.image_map.entry(proxy.id) {
                        let format = proxy.format.to_wgpu();
                        let texture = device.create_texture(&wgpu::TextureDescriptor {
                            label: None,
                            size: wgpu::Extent3d {
                                width: proxy.width,
                                height: proxy.height,
                                depth_or_array_layers: 1,
                            },
                            mip_level_count: 1,
                            sample_count: 1,
                            dimension: wgpu::TextureDimension::D2,
                            usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
                            format,
                            view_formats: &[],
                        });
                        let texture_view = texture.create_view(&wgpu::TextureViewDescriptor {
                            label: None,
                            usage: None,
                            dimension: Some(TextureViewDimension::D2),
                            aspect: TextureAspect::All,
                            mip_level_count: None,
                            base_mip_level: 0,
                            base_array_layer: 0,
                            array_layer_count: None,
                            format: Some(proxy.format.to_wgpu()),
                        });
                        v.insert((texture, texture_view));
                    }
                }
            }
        }
        let resources = bindings
            .iter()
            .map(|proxy| match proxy {
                ResourceProxy::Buffer(proxy) => {
                    let buffer = match self.bufs.get(&proxy.id) {
                        Some(TransientBuf::Gpu(buffer)) => *buffer,
                        _ => bind_map
                            .get_gpu_buf(proxy.id)
                            .expect("materialized bind-group buffer"),
                    };
                    BoundResource::Buffer(buffer.clone(), 0, None)
                }
                ResourceProxy::BufferRange {
                    proxy,
                    offset,
                    size,
                } => {
                    let buffer = match self.bufs.get(&proxy.id) {
                        Some(TransientBuf::Gpu(buffer)) => *buffer,
                        _ => bind_map
                            .get_gpu_buf(proxy.id)
                            .expect("materialized ranged bind-group buffer"),
                    };
                    BoundResource::Buffer(buffer.clone(), *offset, NonZeroU64::new(*size))
                }
                ResourceProxy::Image(proxy) => {
                    let view = self
                        .images
                        .get(&proxy.id)
                        .copied()
                        .or_else(|| bind_map.image_map.get(&proxy.id).map(|value| &value.1))
                        .expect("materialized bind-group image");
                    BoundResource::Image(view.clone())
                }
            })
            .collect::<SmallVec<[_; 8]>>();
        let key = BindGroupKey { shader, resources };
        if let Some(cached) = bind_group_cache.get(&key) {
            #[cfg(test)]
            if bindings.iter().any(|resource| {
                matches!(resource, ResourceProxy::Image(proxy) if Some(proxy.id) == bind_map.zero_gradient_id)
            }) {
                bind_map.zero_gradient_bind_group_cache_hits += 1;
            }
            return cached.clone();
        }
        let entries = bindings
            .iter()
            .enumerate()
            .map(|(i, proxy)| match proxy {
                ResourceProxy::Buffer(proxy) => {
                    let buf = match self.bufs.get(&proxy.id) {
                        Some(TransientBuf::Gpu(b)) => b,
                        _ => bind_map.get_gpu_buf(proxy.id).unwrap(),
                    };
                    wgpu::BindGroupEntry {
                        binding: i as u32,
                        resource: buf.as_entire_binding(),
                    }
                }
                ResourceProxy::BufferRange {
                    proxy,
                    offset,
                    size,
                } => {
                    let buf = match self.bufs.get(&proxy.id) {
                        Some(TransientBuf::Gpu(b)) => b,
                        _ => bind_map.get_gpu_buf(proxy.id).unwrap(),
                    };
                    wgpu::BindGroupEntry {
                        binding: i as u32,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: buf,
                            offset: *offset,
                            size: NonZeroU64::new(*size),
                        }),
                    }
                }
                ResourceProxy::Image(proxy) => {
                    let view = self
                        .images
                        .get(&proxy.id)
                        .copied()
                        .or_else(|| bind_map.image_map.get(&proxy.id).map(|v| &v.1))
                        .unwrap();
                    wgpu::BindGroupEntry {
                        binding: i as u32,
                        resource: wgpu::BindingResource::TextureView(view),
                    }
                }
            })
            .collect::<SmallVec<[_; 8]>>();
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout,
            entries: &entries,
        });
        if bind_group_cache.len() >= BIND_GROUP_CACHE_LIMIT {
            bind_group_cache.clear();
        }
        bind_group_cache.insert(key, bind_group.clone());
        bind_group
    }

    fn create_cpu_resources(
        &self,
        bind_map: &'a mut BindMap,
        mut cpu_pathtag_pool: Option<&mut CpuPathtagPool>,
        mut cpu_metadata_pool: Option<&mut CpuMetadataPool>,
        mut cpu_draw_prefix_pool: Option<&mut CpuDrawPrefixPool>,
        bindings: &[ResourceProxy],
    ) -> Vec<CpuBinding<'_>> {
        // First pass is mutable; create buffers as needed
        for resource in bindings {
            match resource {
                ResourceProxy::Buffer(proxy)
                | ResourceProxy::BufferRange {
                    proxy,
                    offset: _,
                    size: _,
                } => match self.bufs.get(&proxy.id) {
                    Some(TransientBuf::Cpu(_)) => (),
                    Some(TransientBuf::Gpu(_)) => panic!("buffer was already materialized on GPU"),
                    _ => bind_map.materialize_cpu_buf(
                        proxy,
                        cpu_pathtag_pool.as_deref_mut(),
                        cpu_metadata_pool.as_deref_mut(),
                        cpu_draw_prefix_pool.as_deref_mut(),
                    ),
                },
                ResourceProxy::Image(_) => todo!(),
            }
        }
        // Second pass takes immutable references
        bindings
            .iter()
            .map(|resource| match resource {
                ResourceProxy::Buffer(buf) => match self.bufs.get(&buf.id) {
                    Some(TransientBuf::Cpu(b)) => CpuBinding::Buffer(b),
                    _ => bind_map.get_cpu_buf(buf.id),
                },
                ResourceProxy::BufferRange { .. } => todo!(),
                ResourceProxy::Image(_) => todo!(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparse_segment_row_mask_requires_exact_opt_in_value() {
        assert!(sparse_segment_row_mask_from_value(Some(OsStr::new("1"))));
        for value in ["", "0", "true", "01", "2"] {
            assert!(
                !sparse_segment_row_mask_from_value(Some(OsStr::new(value))),
                "{value:?} must not enable the off-default row-mask experiment"
            );
        }
        assert!(!sparse_segment_row_mask_from_value(None));
    }

    #[test]
    fn canonical_segment_order_requires_exact_opt_in_value() {
        assert!(canonical_segment_order_from_value(Some(OsStr::new("1"))));
        for value in ["", "0", "true", "01", "2"] {
            assert!(
                !canonical_segment_order_from_value(Some(OsStr::new(value))),
                "{value:?} must not enable canonical segment ordering"
            );
        }
        assert!(!canonical_segment_order_from_value(None));
    }

    #[test]
    fn area_ppt8_requires_exact_opt_in_value() {
        assert!(area_ppt8_from_value(Some(OsStr::new("1"))));
        for value in ["", "0", "true", "01", "2"] {
            assert!(
                !area_ppt8_from_value(Some(OsStr::new(value))),
                "{value:?} must not enable the off-default Area PPT8 experiment"
            );
        }
        assert!(!area_ppt8_from_value(None));
    }

    #[test]
    fn clip_single_group_requires_exact_opt_in_value() {
        assert!(clip_single_group_from_value(Some(OsStr::new("1"))));
        for value in ["", "0", "true", "01", "2"] {
            assert!(
                !clip_single_group_from_value(Some(OsStr::new(value))),
                "{value:?} must not enable the off-default clip single-group experiment"
            );
        }
        assert!(!clip_single_group_from_value(None));
    }

    #[test]
    fn clip_single_group_keeps_the_clip_leaf_timestamp_stage() {
        assert_eq!(gpu_dispatch_stage_id("vello.clip_leaf"), 11);
        assert_eq!(gpu_dispatch_stage_id("vello.clip_leaf_single_group"), 11);
    }

    #[test]
    fn cpu_pathtag_shader_labels_are_exact() {
        assert!(is_cpu_pathtag_shader("vello.pathtag_reduce"));
        assert!(is_cpu_pathtag_shader("vello.pathtag_scan_small"));
        assert!(!is_cpu_pathtag_shader("vello.pathtag_reduce2"));
        assert!(!is_cpu_pathtag_shader("vello.pathtag_scan1"));
        assert!(!is_cpu_pathtag_shader("pathtag_reduce"));
    }

    #[test]
    fn cpu_metadata_shader_labels_are_exact() {
        assert!(is_cpu_metadata_shader("vello.bbox_clear"));
        assert!(is_cpu_metadata_shader("vello.draw_reduce"));
        assert!(!is_cpu_metadata_shader("vello.draw_leaf"));
        assert!(!is_cpu_metadata_shader("vello.pathtag_reduce"));
    }

    #[test]
    fn cpu_pathtag_pool_reuses_only_known_zeroed_slots() {
        let mut pool = CpuPathtagPool::default();
        let initial = vec![0xaa; 16];
        let initial_capacity = initial.capacity();
        pool.put(CPU_PATHTAG_REDUCED_LABEL, initial);
        assert_eq!(
            pool.retained_bytes(),
            u64::try_from(initial_capacity).unwrap()
        );

        let bytes = pool.take(CPU_PATHTAG_REDUCED_LABEL, 12).unwrap();
        assert_eq!(bytes.len(), 12);
        assert!(bytes.iter().all(|byte| *byte == 0));
        assert_eq!(pool.retained_bytes(), 0);

        let larger_size = bytes.capacity().saturating_add(1);
        pool.put(CPU_PATHTAG_REDUCED_LABEL, bytes);
        assert!(pool.take(CPU_PATHTAG_REDUCED_LABEL, larger_size).is_none());
        assert_eq!(pool.retained_bytes(), 0);

        pool.put("vello.other_buf", vec![0; 16]);
        assert!(pool.take("vello.other_buf", 16).is_none());
        assert_eq!(pool.retained_bytes(), 0);
    }

    #[test]
    fn cpu_pathtag_pool_caps_oversize_slots_and_clears() {
        let mut pool = CpuPathtagPool::default();
        pool.put(
            CPU_PATHTAG_REDUCED_LABEL,
            vec![0; CPU_PATHTAG_REUSE_SLOT_BYTES + 1],
        );
        assert_eq!(pool.retained_bytes(), 0);

        pool.put(
            CPU_PATHTAG_REDUCED_LABEL,
            vec![0; CPU_PATHTAG_REUSE_SLOT_BYTES],
        );
        pool.put(
            CPU_PATHTAG_MONOID_LABEL,
            vec![0; CPU_PATHTAG_REUSE_SLOT_BYTES],
        );
        assert_eq!(
            pool.retained_bytes(),
            u64::try_from(CPU_PATHTAG_REUSE_SLOT_BYTES * 2).unwrap()
        );
        pool.clear();
        assert_eq!(pool.retained_bytes(), 0);
    }

    #[test]
    fn cpu_metadata_pool_reuses_zeroed_bounded_slots() {
        let mut pool = CpuMetadataPool::default();
        pool.put(CPU_METADATA_PATH_BBOX_LABEL, vec![0xab; 64]);
        let bytes = pool.take(CPU_METADATA_PATH_BBOX_LABEL, 48).unwrap();
        assert_eq!(bytes.len(), 48);
        assert!(bytes.iter().all(|byte| *byte == 0));
        pool.put(CPU_METADATA_PATH_BBOX_LABEL, bytes);

        pool.put(
            CPU_METADATA_DRAW_REDUCED_LABEL,
            vec![0; CPU_METADATA_DRAW_REDUCED_SLOT_BYTES + 1],
        );
        assert!(
            pool.take(
                CPU_METADATA_DRAW_REDUCED_LABEL,
                CPU_METADATA_DRAW_REDUCED_SLOT_BYTES
            )
            .is_none()
        );
        assert!(pool.take("vello.other_buf", 16).is_none());
        assert!(
            pool.retained_bytes()
                <= u64::try_from(
                    CPU_METADATA_PATH_BBOX_SLOT_BYTES + CPU_METADATA_DRAW_REDUCED_SLOT_BYTES
                )
                .unwrap()
        );
    }

    #[test]
    fn disabling_cpu_pathtags_disables_metadata_and_clears_its_pool() {
        let mut engine = WgpuEngine {
            cpu_pathtag: true,
            cpu_metadata: true,
            ..Default::default()
        };
        engine
            .cpu_metadata_pool
            .put(CPU_METADATA_DRAW_REDUCED_LABEL, vec![0; 16]);
        assert!(engine.cpu_metadata_enabled());
        assert_ne!(engine.cpu_metadata_pool_retained_bytes(), 0);

        engine.set_cpu_pathtag_enabled(false);
        assert!(!engine.cpu_metadata_enabled());
        assert_eq!(engine.cpu_metadata_pool_retained_bytes(), 0);
    }

    #[test]
    fn cpu_draw_prefix_pool_is_bounded_and_zeroed() {
        let mut pool = CpuDrawPrefixPool::default();
        pool.put(CPU_DRAW_PREFIX_LABEL, vec![0xcd; 64]);
        let bytes = pool.take(CPU_DRAW_PREFIX_LABEL, 48).unwrap();
        assert_eq!(bytes.len(), 48);
        assert!(bytes.iter().all(|byte| *byte == 0));
        pool.put(CPU_DRAW_PREFIX_LABEL, bytes);

        pool.put("vello.other_buf", vec![0; 16]);
        assert!(pool.take("vello.other_buf", 16).is_none());
        pool.put(
            CPU_DRAW_PREFIX_LABEL,
            vec![0; CPU_DRAW_PREFIX_SLOT_BYTES + 1],
        );
        assert!(pool.retained_bytes() <= u64::try_from(CPU_DRAW_PREFIX_SLOT_BYTES).unwrap());
        pool.clear();
        assert_eq!(pool.retained_bytes(), 0);
    }

    #[test]
    fn disabling_cpu_pathtags_disables_draw_prefix_and_clears_its_pool() {
        let mut engine = WgpuEngine {
            cpu_pathtag: true,
            cpu_draw_prefix: true,
            ..Default::default()
        };
        engine
            .cpu_draw_prefix_pool
            .put(CPU_DRAW_PREFIX_LABEL, vec![0; 16]);
        assert!(engine.cpu_draw_prefix_enabled());
        assert!(engine.cpu_draw_prefix_supported(0));
        assert!(engine.cpu_draw_prefix_supported(16));
        assert!(
            engine.cpu_draw_prefix_supported(u64::try_from(CPU_DRAW_PREFIX_SLOT_BYTES).unwrap())
        );
        assert!(
            !engine
                .cpu_draw_prefix_supported(u64::try_from(CPU_DRAW_PREFIX_SLOT_BYTES + 1).unwrap())
        );
        assert!(
            !engine
                .cpu_draw_prefix_supported(u64::try_from(CPU_DRAW_PREFIX_SLOT_BYTES + 16).unwrap())
        );

        engine.set_cpu_pathtag_enabled(false);
        assert!(!engine.cpu_draw_prefix_enabled());
        assert_eq!(engine.cpu_draw_prefix_pool_retained_bytes(), 0);
    }

    #[test]
    fn production_defaults_require_an_explicit_zero_to_disable() {
        assert!(default_enabled_from_value(None));
        assert!(!default_enabled_from_value(Some(OsStr::new("0"))));
        assert!(default_enabled_from_value(Some(OsStr::new("1"))));
        assert!(default_enabled_from_value(Some(OsStr::new("unexpected"))));

        assert_eq!(compute_batch_from_value(None), 2);
        assert_eq!(compute_batch_from_value(Some(OsStr::new("0"))), 0);
        assert_eq!(compute_batch_from_value(Some(OsStr::new("1"))), 1);
        assert_eq!(compute_batch_from_value(Some(OsStr::new("2"))), 2);
        assert_eq!(compute_batch_from_value(Some(OsStr::new("unexpected"))), 2);
    }

    #[test]
    fn zero_gradient_reuse_uses_the_default_on_exact_zero_off_policy() {
        assert!(default_enabled_from_value(None));
        assert!(!default_enabled_from_value(Some(OsStr::new("0"))));
        assert!(default_enabled_from_value(Some(OsStr::new("00"))));
    }

    #[test]
    fn direct_path_workgroups_accepts_only_the_bounded_integer_override() {
        assert_eq!(
            direct_path_workgroups_from_value(None),
            DIRECT_PATH_WORKGROUPS_DEFAULT
        );
        assert_eq!(direct_path_workgroups_from_value(Some(OsStr::new("1"))), 1);
        assert_eq!(direct_path_workgroups_from_value(Some(OsStr::new("8"))), 8);
        assert_eq!(
            direct_path_workgroups_from_value(Some(OsStr::new("128"))),
            128
        );
        for invalid in ["0", "129", "-1", "1.5", "unexpected"] {
            assert_eq!(
                direct_path_workgroups_from_value(Some(OsStr::new(invalid))),
                DIRECT_PATH_WORKGROUPS_DEFAULT,
                "invalid diagnostic override {invalid:?} must retain the production grid"
            );
        }
    }

    #[test]
    fn sparse_coarse_active_bins_requires_exact_opt_in() {
        assert!(!sparse_coarse_active_bins_from_value(None));
        assert!(sparse_coarse_active_bins_from_value(Some(OsStr::new("1"))));
        for disabled in ["0", "01", "true", "unexpected"] {
            assert!(
                !sparse_coarse_active_bins_from_value(Some(OsStr::new(disabled))),
                "only exact 1 enables the diagnostic permutation"
            );
        }
    }

    #[test]
    fn sparse_tile_bounds_requires_exact_opt_in() {
        assert!(!sparse_tile_bounds_from_value(None));
        assert!(sparse_tile_bounds_from_value(Some(OsStr::new("1"))));
        for disabled in ["0", "01", "true", "unexpected"] {
            assert!(
                !sparse_tile_bounds_from_value(Some(OsStr::new(disabled))),
                "only exact 1 enables the bounded allocation experiment"
            );
        }
    }

    #[test]
    fn sparse_fine_quad_skip_requires_exact_opt_in() {
        assert!(!sparse_fine_quad_skip_from_value(None));
        assert!(sparse_fine_quad_skip_from_value(Some(OsStr::new("1"))));
        for disabled in ["0", "01", "true", "unexpected"] {
            assert!(!sparse_fine_quad_skip_from_value(Some(OsStr::new(
                disabled
            ))));
        }
    }

    #[test]
    fn sparse_opaque_prune_requires_exact_opt_in() {
        assert!(!sparse_opaque_prune_from_value(None));
        assert!(sparse_opaque_prune_from_value(Some(OsStr::new("1"))));
        for disabled in ["0", "01", "true", "unexpected"] {
            assert!(!sparse_opaque_prune_from_value(Some(OsStr::new(disabled))));
        }
    }

    #[test]
    fn sparse_solid_fuse_requires_exact_opt_in() {
        assert!(!sparse_solid_fuse_from_value(None));
        assert!(sparse_solid_fuse_from_value(Some(OsStr::new("1"))));
        for disabled in ["0", "01", "true", "unexpected"] {
            assert!(!sparse_solid_fuse_from_value(Some(OsStr::new(disabled))));
        }
    }

    #[test]
    fn sparse_coarse_policy_has_explicit_precedence() {
        assert_eq!(
            sparse_coarse_policy(None, None, None),
            SparseCoarsePolicy::Dense
        );
        assert_eq!(
            sparse_coarse_policy(None, None, Some(OsStr::new("1"))),
            SparseCoarsePolicy::Adaptive
        );
        assert_eq!(
            sparse_coarse_policy(Some(OsStr::new("1")), Some(OsStr::new("1")), None),
            SparseCoarsePolicy::Active
        );
        assert_eq!(
            sparse_coarse_policy(Some(OsStr::new("1")), None, Some(OsStr::new("1"))),
            SparseCoarsePolicy::Legacy
        );
        for invalid in ["0", "unexpected"] {
            assert_eq!(
                sparse_coarse_policy(Some(OsStr::new(invalid)), None, Some(OsStr::new("1"))),
                SparseCoarsePolicy::Dense
            );
        }
        assert_eq!(
            sparse_coarse_policy(None, None, Some(OsStr::new("unexpected"))),
            SparseCoarsePolicy::Dense
        );
    }

    #[test]
    fn promoted_gpu_paths_retain_full_cpu_and_profiler_fallbacks() {
        let cpu_engine = WgpuEngine {
            compute_batch: 2,
            direct_path_dispatch: true,
            use_cpu: true,
            ..Default::default()
        };
        assert!(!cpu_engine.compute_pass_batching_enabled());
        assert!(!cpu_engine.direct_path_dispatch_enabled());

        let gpu_engine = WgpuEngine {
            compute_batch: 2,
            ..Default::default()
        };
        assert_eq!(
            gpu_engine.compute_pass_batching_enabled(),
            !cfg!(feature = "wgpu-profiler")
        );
    }

    #[test]
    fn direct_path_dispatch_toggle_excludes_full_cpu() {
        let mut engine = WgpuEngine::default();
        assert!(!engine.direct_path_dispatch_enabled());

        engine.set_direct_path_dispatch_enabled(true);
        assert!(engine.direct_path_dispatch_enabled());
        engine.set_direct_path_dispatch_enabled(false);
        assert!(!engine.direct_path_dispatch_enabled());

        let mut cpu_engine = WgpuEngine {
            use_cpu: true,
            direct_path_dispatch: true,
            ..Default::default()
        };
        assert!(!cpu_engine.direct_path_dispatch_enabled());
        cpu_engine.set_direct_path_dispatch_enabled(true);
        assert!(!cpu_engine.direct_path_dispatch_enabled());
    }

    #[test]
    fn direct_path_dispatch_timestamp_labels_are_exact() {
        assert_eq!(gpu_dispatch_stage_id("vello.path_count_direct"), 26);
        assert_eq!(gpu_dispatch_stage_id("vello.path_tiling_direct"), 27);
        assert_eq!(
            gpu_dispatch_stage_id("vello.path_tiling_direct_segment_row_mask"),
            27
        );
        assert_eq!(gpu_dispatch_stage_id("vello.coarse_sparse_active"), 28);
        assert_eq!(gpu_dispatch_stage_id("vello.coarse_opaque_prune"), 17);
        assert_eq!(gpu_dispatch_stage_id("vello.coarse_solid_fuse"), 17);
        assert_eq!(
            gpu_dispatch_stage_id("vello.coarse_sparse_opaque_prune"),
            24
        );
        assert_eq!(
            gpu_dispatch_stage_id("vello.coarse_sparse_active_opaque_prune"),
            28
        );
        assert_eq!(gpu_dispatch_stage_id("vello.coarse_sparse_solid_fuse"), 24);
        assert_eq!(
            gpu_dispatch_stage_id("vello.coarse_sparse_active_solid_fuse"),
            28
        );
        assert_eq!(gpu_dispatch_stage_id("vello.tile_alloc_sparse"), 13);
        assert_eq!(
            gpu_dispatch_stage_id("vello.fine_sparse_area_quad_skip"),
            23
        );
        assert_eq!(
            gpu_dispatch_stage_id("vello.fine_sparse_area_solid_fuse"),
            23
        );
        assert_eq!(gpu_dispatch_stage_id("vello.path_count"), 15);
        assert_eq!(gpu_dispatch_stage_id("vello.path_tiling"), 19);
        assert_eq!(
            gpu_dispatch_stage_id("vello.path_tiling_segment_row_mask"),
            19
        );
        assert_eq!(
            gpu_dispatch_stage_id("vello.fine_sparse_area_segment_row_mask"),
            23
        );
        assert_eq!(gpu_dispatch_stage_id("vello.fine_area_ppt8"), 20);
        assert_eq!(gpu_dispatch_stage_id("vello.fine_sparse_area_ppt8"), 23);
        assert_eq!(gpu_dispatch_stage_id("vello.path_count_directly"), 0);
    }

    #[test]
    fn pending_buffer_clear_ends_a_prepared_compute_run() {
        let mut engine = WgpuEngine::default();
        let pending = BufferProxy::new(4, "pending");
        let other = BufferProxy::new(4, "other");
        engine.bind_map.pending_clears.insert(pending.id);

        assert!(
            engine.bindings_have_pending_clear(&[ResourceProxy::BufferRange {
                proxy: pending,
                offset: 0,
                size: 4,
            }])
        );
        assert!(!engine.bindings_have_pending_clear(&[ResourceProxy::Buffer(other)]));
    }

    #[test]
    fn deferred_frees_extend_only_the_second_batching_mode() {
        let buffer = BufferProxy::new(4, "buffer");
        let image = ImageProxy::new(1, 1, crate::low_level::ImageFormat::Rgba8);
        let free_buffer = Command::FreeBuffer(buffer);
        let free_image = Command::FreeImage(image);
        let clear = Command::Clear(buffer, 0, None);

        assert!(compute_batch_boundary(&free_buffer, false));
        assert!(compute_batch_boundary(&free_image, false));
        assert!(!compute_batch_boundary(&free_buffer, true));
        assert!(!compute_batch_boundary(&free_image, true));
        assert!(compute_batch_boundary(&clear, true));

        let mut free_bufs = HashSet::default();
        let mut free_images = HashSet::default();
        assert!(record_deferred_free(
            &free_buffer,
            &mut free_bufs,
            &mut free_images
        ));
        assert!(record_deferred_free(
            &free_image,
            &mut free_bufs,
            &mut free_images
        ));
        assert!(!record_deferred_free(
            &clear,
            &mut free_bufs,
            &mut free_images
        ));
        assert!(free_bufs.contains(&buffer.id));
        assert!(free_images.contains(&image.id));
    }

    #[test]
    fn timestamp_decoder_reports_span_and_sum_for_complete_monotonic_pairs() {
        let bytes = [10u64, 16, 20, 29]
            .into_iter()
            .flat_map(u64::to_le_bytes)
            .collect();
        let sample = decode_timestamp_sample(7, 2, 0.5, bytes).unwrap();

        assert_eq!(sample.submission_id, 7);
        assert_eq!(sample.span_ticks, 19);
        assert_eq!(sample.summed_pass_ticks, 15);
        assert_eq!(sample.span_ns, 9.5);
        assert_eq!(sample.summed_pass_ns, 7.5);
    }

    #[test]
    fn timestamp_decoder_rejects_non_monotonic_pairs() {
        let bytes = [10u64, 9].into_iter().flat_map(u64::to_le_bytes).collect();
        assert!(decode_timestamp_sample(1, 1, 1.0, bytes).is_none());
    }

    #[test]
    fn timestamp_lifecycle_only_preserves_a_pending_map() {
        let (_sender, receiver) = mpsc::channel();
        assert!(!timestamp_slot_available(&TimestampMapState::Pending {
            submission_id: 1,
            pass_count: 1,
            receiver,
        }));
        assert!(timestamp_slot_available(&TimestampMapState::MapFailed {
            submission_id: 1,
        }));
        assert!(timestamp_slot_available(&TimestampMapState::Overflowed {
            submission_id: 1,
            pass_count: GPU_TIMESTAMP_PASS_LIMIT + 1,
        }));
    }

    #[test]
    fn timestamp_decoder_rejects_invalid_metadata() {
        let bytes: Vec<u8> = [1u64, 2].into_iter().flat_map(u64::to_le_bytes).collect();
        assert!(decode_timestamp_sample(1, 1, 0.0, bytes.clone()).is_none());
        assert!(decode_timestamp_sample(1, 1, f32::NAN, bytes).is_none());
    }

    fn dispatch_timestamp_source(pairs: &[u64]) -> GpuTimestampSample {
        GpuTimestampSample {
            submission_id: 9,
            pass_count: u32::try_from(pairs.len().saturating_div(2)).unwrap_or(0),
            span_ticks: 0,
            summed_pass_ticks: 0,
            timestamp_period_ns: 1.0,
            span_ns: 0.0,
            summed_pass_ns: 0.0,
            timestamp_bytes: pairs.iter().flat_map(|tick| tick.to_le_bytes()).collect(),
        }
    }

    #[test]
    fn dispatch_timestamp_decoder_aggregates_repeated_and_unknown_stages() {
        let sample = dispatch_timestamp_source(&[10, 13, 20, 27, 30, 35]);
        let mut stage_ids = [0; GPU_TIMESTAMP_PASS_LIMIT as usize];
        stage_ids[..3].copy_from_slice(&[17, 17, 99]);
        let decoded = decode_dispatch_timestamp_sample(&sample, &stage_ids, 3).unwrap();

        assert_eq!(decoded.stage_count, 2);
        assert_eq!(decoded.stages[0].stage_id, 0);
        assert_eq!(decoded.stages[0].dispatch_count, 1);
        assert_eq!(decoded.stages[0].duration_ns, 5);
        assert_eq!(decoded.stages[1].stage_id, 17);
        assert_eq!(decoded.stages[1].dispatch_count, 2);
        assert_eq!(decoded.stages[1].duration_ns, 10);
    }

    #[test]
    fn dispatch_timestamp_decoder_rejects_bad_pairs_and_stage_counts() {
        let sample = dispatch_timestamp_source(&[10, 9]);
        let stage_ids = [17; GPU_TIMESTAMP_PASS_LIMIT as usize];
        assert!(decode_dispatch_timestamp_sample(&sample, &stage_ids, 1).is_none());

        let sample = dispatch_timestamp_source(&[10, 12]);
        assert!(decode_dispatch_timestamp_sample(&sample, &stage_ids, 2).is_none());
    }

    #[test]
    fn dispatch_timestamp_decoder_rejects_zero_and_over_limit_pass_counts() {
        let stage_ids = [17; GPU_TIMESTAMP_PASS_LIMIT as usize];
        let zero = dispatch_timestamp_source(&[]);
        assert!(decode_dispatch_timestamp_sample(&zero, &stage_ids, 0).is_none());

        let ticks: Vec<u64> = (0..130).collect();
        let oversized = dispatch_timestamp_source(&ticks);
        assert_eq!(oversized.pass_count, GPU_TIMESTAMP_PASS_LIMIT + 1);
        assert!(
            decode_dispatch_timestamp_sample(&oversized, &stage_ids, GPU_TIMESTAMP_PASS_LIMIT + 1,)
                .is_none()
        );
    }
}
