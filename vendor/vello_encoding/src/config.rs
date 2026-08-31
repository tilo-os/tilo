// Copyright 2023 the Vello Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::SegmentCount;

use super::{
    BinHeader, Clip, ClipBbox, ClipBic, ClipElement, DrawBbox, DrawMonoid, Layout, LineSoup, Path,
    PathBbox, PathMonoid, PathSegment, Tile,
};
use bytemuck::{Pod, Zeroable};

const TILE_WIDTH: u32 = 16;
const TILE_HEIGHT: u32 = 16;

// TODO: Obtain these from the vello_shaders crate
pub(crate) const PATH_REDUCE_WG: u32 = 256;
const PATH_BBOX_WG: u32 = 256;
const FLATTEN_WG: u32 = 256;
const CLIP_REDUCE_WG: u32 = 256;

/// Counters for tracking dynamic allocation on the GPU.
///
/// This must be kept in sync with the struct in `shader/shared/bump.wgsl`
#[derive(Clone, Copy, Debug, Default, Zeroable, Pod)]
#[repr(C)]
pub struct BumpAllocators {
    pub failed: u32,
    // Final needed dynamic size of the buffers. If any of these are larger
    // than the corresponding `_size` element reallocation needs to occur.
    pub binning: u32,
    pub ptcl: u32,
    pub tile: u32,
    pub seg_counts: u32,
    pub segments: u32,
    pub blend: u32,
    pub lines: u32,
}

#[derive(Default)]
pub struct BumpAllocatorMemory {
    pub total: u32,
    pub binning: BufferSize<u32>,
    pub ptcl: BufferSize<u32>,
    pub tile: BufferSize<Tile>,
    pub seg_counts: BufferSize<SegmentCount>,
    pub segments: BufferSize<PathSegment>,
    pub lines: BufferSize<LineSoup>,
}

impl BumpAllocators {
    pub fn memory(&self) -> BumpAllocatorMemory {
        let binning = BufferSize::new(self.binning);
        let ptcl = BufferSize::new(self.ptcl);
        let tile = BufferSize::new(self.tile);
        let seg_counts = BufferSize::new(self.seg_counts);
        let segments = BufferSize::new(self.segments);
        let lines = BufferSize::new(self.lines);
        BumpAllocatorMemory {
            total: binning.size_in_bytes()
                + ptcl.size_in_bytes()
                + tile.size_in_bytes()
                + seg_counts.size_in_bytes()
                + segments.size_in_bytes()
                + lines.size_in_bytes(),
            binning,
            ptcl,
            tile,
            seg_counts,
            segments,
            lines,
        }
    }
}

impl std::fmt::Display for BumpAllocatorMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "\n \
                 \tTotal:\t\t\t{} bytes ({:.2} KB | {:.2} MB)\n\
                 \tBinning\t\t\t{} elements ({} bytes)\n\
                 \tPTCL\t\t\t{} elements ({} bytes)\n\
                 \tTile:\t\t\t{} elements ({} bytes)\n\
                 \tSegment Counts:\t\t{} elements ({} bytes)\n\
                 \tSegments:\t\t{} elements ({} bytes)\n\
                 \tLines:\t\t\t{} elements ({} bytes)",
            self.total,
            self.total as f32 / (1 << 10) as f32,
            self.total as f32 / (1 << 20) as f32,
            self.binning.len(),
            self.binning.size_in_bytes(),
            self.ptcl.len(),
            self.ptcl.size_in_bytes(),
            self.tile.len(),
            self.tile.size_in_bytes(),
            self.seg_counts.len(),
            self.seg_counts.size_in_bytes(),
            self.segments.len(),
            self.segments.size_in_bytes(),
            self.lines.len(),
            self.lines.size_in_bytes()
        )
    }
}

/// Storage of indirect dispatch size values.
///
/// The original plan was to reuse [`BumpAllocators`], but the WebGPU compatible
/// usage list rules forbid that being used as indirect counts while also
/// bound as writable.
#[derive(Clone, Copy, Debug, Default, Zeroable, Pod)]
#[repr(C)]
pub struct IndirectCount {
    pub count_x: u32,
    pub count_y: u32,
    pub count_z: u32,
    pub pad0: u32,
}

/// Uniform render configuration data used by all GPU stages.
///
/// This data structure must be kept in sync with the definition in
/// `shaders/shared/config.wgsl`.
#[derive(Clone, Copy, Debug, Default, Zeroable, Pod)]
#[repr(C)]
pub struct ConfigUniform {
    /// Width of the scene in tiles.
    pub width_in_tiles: u32,
    /// Height of the scene in tiles.
    pub height_in_tiles: u32,
    /// Width of the target in pixels.
    pub target_width: u32,
    /// Height of the target in pixels.
    pub target_height: u32,
    /// The base background color applied to the target before any blends.
    pub base_color: u32,
    /// Layout of packed scene data.
    pub layout: Layout,
    /// Size of line soup buffer allocation (in [`LineSoup`]s)
    pub lines_size: u32,
    /// Size of binning buffer allocation (in `u32`s).
    pub binning_size: u32,
    /// Size of tile buffer allocation (in [`Tile`]s).
    pub tiles_size: u32,
    /// Size of segment count buffer allocation (in [`SegmentCount`]s).
    pub seg_counts_size: u32,
    /// Size of segment buffer allocation (in [`PathSegment`]s).
    pub segments_size: u32,
    /// Size of blend spill buffer (in `u32` pixels).
    // TODO: Maybe store in TILE_WIDTH * TILE_HEIGHT blocks of pixels instead?
    pub blend_size: u32,
    /// Size of per-tile command list buffer allocation (in `u32`s).
    pub ptcl_size: u32,
}

/// CPU side setup and configuration.
#[derive(Default)]
pub struct RenderConfig {
    /// GPU side configuration.
    pub gpu: ConfigUniform,
    /// Workgroup counts for all compute pipelines.
    pub workgroup_counts: WorkgroupCounts,
    /// Sizes of all buffer resources.
    pub buffer_sizes: BufferSizes,
}

impl RenderConfig {
    pub fn new(
        layout: &Layout,
        width: u32,
        height: u32,
        base_color: &peniko::Color,
        budget: &BumpBudget,
    ) -> Self {
        let new_width = width.next_multiple_of(TILE_WIDTH);
        let new_height = height.next_multiple_of(TILE_HEIGHT);
        let width_in_tiles = new_width / TILE_WIDTH;
        let height_in_tiles = new_height / TILE_HEIGHT;
        let n_path_tags = layout.path_tags_size();
        let workgroup_counts =
            WorkgroupCounts::new(layout, width_in_tiles, height_in_tiles, n_path_tags);
        let buffer_sizes = BufferSizes::new(
            layout,
            &workgroup_counts,
            width_in_tiles,
            height_in_tiles,
            budget,
        );
        Self {
            gpu: ConfigUniform {
                width_in_tiles,
                height_in_tiles,
                target_width: width,
                target_height: height,
                base_color: base_color.premultiply().to_rgba8().to_u32(),
                lines_size: buffer_sizes.lines.len(),
                binning_size: buffer_sizes.bin_data.len() - layout.bin_data_start,
                tiles_size: buffer_sizes.tiles.len(),
                seg_counts_size: buffer_sizes.seg_counts.len(),
                segments_size: buffer_sizes.segments.len(),
                blend_size: buffer_sizes.blend_spill.len(),
                ptcl_size: buffer_sizes.ptcl.len(),
                layout: *layout,
            },
            workgroup_counts,
            buffer_sizes,
        }
    }
}

/// Type alias for a workgroup size.
pub type WorkgroupSize = (u32, u32, u32);

/// Computed sizes for all dispatches.
#[derive(Copy, Clone, Debug, Default)]
pub struct WorkgroupCounts {
    pub use_large_path_scan: bool,
    pub path_reduce: WorkgroupSize,
    pub path_reduce2: WorkgroupSize,
    pub path_scan1: WorkgroupSize,
    pub path_scan: WorkgroupSize,
    pub bbox_clear: WorkgroupSize,
    pub flatten: WorkgroupSize,
    pub draw_reduce: WorkgroupSize,
    pub draw_leaf: WorkgroupSize,
    pub clip_reduce: WorkgroupSize,
    pub clip_leaf: WorkgroupSize,
    pub binning: WorkgroupSize,
    pub tile_alloc: WorkgroupSize,
    pub path_count_setup: WorkgroupSize,
    // Note: `path_count` must use an indirect dispatch
    pub backdrop: WorkgroupSize,
    pub coarse: WorkgroupSize,
    pub path_tiling_setup: WorkgroupSize,
    // Note: `path_tiling` must use an indirect dispatch
    pub fine: WorkgroupSize,
}

impl WorkgroupCounts {
    pub fn new(
        layout: &Layout,
        width_in_tiles: u32,
        height_in_tiles: u32,
        n_path_tags: u32,
    ) -> Self {
        let n_paths = layout.n_paths;
        let n_draw_objects = layout.n_draw_objects;
        let n_clips = layout.n_clips;
        let path_tag_padded = align_up(n_path_tags, 4 * PATH_REDUCE_WG);
        let path_tag_wgs = path_tag_padded / (4 * PATH_REDUCE_WG);
        let use_large_path_scan = path_tag_wgs > PATH_REDUCE_WG;
        let reduced_size = if use_large_path_scan {
            align_up(path_tag_wgs, PATH_REDUCE_WG)
        } else {
            path_tag_wgs
        };
        let draw_object_wgs = n_draw_objects.div_ceil(PATH_BBOX_WG);
        let draw_monoid_wgs = draw_object_wgs.min(PATH_BBOX_WG);
        let flatten_wgs = n_path_tags.div_ceil(FLATTEN_WG);
        let clip_reduce_wgs = n_clips.saturating_sub(1) / CLIP_REDUCE_WG;
        let clip_wgs = n_clips.div_ceil(CLIP_REDUCE_WG);
        let path_wgs = n_paths.div_ceil(PATH_BBOX_WG);
        let width_in_bins = width_in_tiles.div_ceil(16);
        let height_in_bins = height_in_tiles.div_ceil(16);
        Self {
            use_large_path_scan,
            path_reduce: (path_tag_wgs, 1, 1),
            path_reduce2: (PATH_REDUCE_WG, 1, 1),
            path_scan1: (reduced_size / PATH_REDUCE_WG, 1, 1),
            path_scan: (path_tag_wgs, 1, 1),
            bbox_clear: (draw_object_wgs, 1, 1),
            flatten: (flatten_wgs, 1, 1),
            draw_reduce: (draw_monoid_wgs, 1, 1),
            draw_leaf: (draw_monoid_wgs, 1, 1),
            clip_reduce: (clip_reduce_wgs, 1, 1),
            clip_leaf: (clip_wgs, 1, 1),
            binning: (draw_object_wgs, 1, 1),
            tile_alloc: (path_wgs, 1, 1),
            path_count_setup: (1, 1, 1),
            backdrop: (path_wgs, 1, 1),
            coarse: (width_in_bins, height_in_bins, 1),
            path_tiling_setup: (1, 1, 1),
            fine: (width_in_tiles, height_in_tiles, 1),
        }
    }
}

/// Typed buffer size primitive.
#[derive(Copy, Clone, Eq, Default, Debug)]
pub struct BufferSize<T: Sized> {
    len: u32,
    _phantom: std::marker::PhantomData<T>,
}

impl<T: Sized> BufferSize<T> {
    /// Creates a new buffer size from number of elements.
    pub const fn new(len: u32) -> Self {
        Self {
            // Each buffer binding must be large enough to hold at least one element to avoid
            // triggering validation errors.
            //
            // Note: not using `Ord::max` here because it doesn't support const eval yet (except
            // in nightly)
            len: if len > 0 { len } else { 1 },
            _phantom: std::marker::PhantomData,
        }
    }

    /// Creates a new buffer size from size in bytes.
    pub const fn from_size_in_bytes(size: u32) -> Self {
        Self::new(size / size_of::<T>() as u32)
    }

    /// Returns the number of elements.
    #[expect(clippy::len_without_is_empty, reason = "The buffer can never be empty")]
    pub const fn len(self) -> u32 {
        self.len
    }

    /// Returns the size in bytes.
    pub const fn size_in_bytes(self) -> u32 {
        size_of::<T>() as u32 * self.len
    }

    /// Returns the size in bytes aligned up to the given value.
    pub const fn aligned_in_bytes(self, alignment: u32) -> u32 {
        align_up(self.size_in_bytes(), alignment)
    }
}

impl<T: Sized> PartialEq for BufferSize<T> {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len
    }
}

impl<T: Sized> PartialOrd for BufferSize<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.len.partial_cmp(&other.len)
    }
}

/// Computed sizes for all buffers.
#[derive(Copy, Clone, Debug, Default)]
pub struct BufferSizes {
    // Known size buffers
    pub path_reduced: BufferSize<PathMonoid>,
    pub path_reduced2: BufferSize<PathMonoid>,
    pub path_reduced_scan: BufferSize<PathMonoid>,
    pub path_monoids: BufferSize<PathMonoid>,
    pub path_bboxes: BufferSize<PathBbox>,
    pub draw_reduced: BufferSize<DrawMonoid>,
    pub draw_monoids: BufferSize<DrawMonoid>,
    pub info: BufferSize<u32>,
    pub clip_inps: BufferSize<Clip>,
    pub clip_els: BufferSize<ClipElement>,
    pub clip_bics: BufferSize<ClipBic>,
    pub clip_bboxes: BufferSize<ClipBbox>,
    pub draw_bboxes: BufferSize<DrawBbox>,
    pub bump_alloc: BufferSize<BumpAllocators>,
    pub indirect_count: BufferSize<IndirectCount>,
    pub bin_headers: BufferSize<BinHeader>,
    pub paths: BufferSize<Path>,
    // Bump allocated buffers
    pub lines: BufferSize<LineSoup>,
    pub bin_data: BufferSize<u32>,
    pub tiles: BufferSize<Tile>,
    pub seg_counts: BufferSize<SegmentCount>,
    pub segments: BufferSize<PathSegment>,
    pub blend_spill: BufferSize<u32>,
    pub ptcl: BufferSize<u32>,
}

impl BufferSizes {
    /// Sizes for a scene of this `layout`, at a viewport of
    /// `width_in_tiles` x `height_in_tiles`, given a `budget` of
    /// bump-allocated scratch.
    ///
    /// The known-size buffers are functions of the scene and are
    /// upstream's own code. The bump-allocated ones are the reservation
    /// this fork exists to change: see [`BumpBudget`].
    pub fn new(
        layout: &Layout,
        workgroups: &WorkgroupCounts,
        width_in_tiles: u32,
        height_in_tiles: u32,
        budget: &BumpBudget,
    ) -> Self {
        let n_paths = layout.n_paths;
        let n_draw_objects = layout.n_draw_objects;
        let n_clips = layout.n_clips;
        let path_tag_wgs = workgroups.path_reduce.0;
        let reduced_size = if workgroups.use_large_path_scan {
            align_up(path_tag_wgs, PATH_REDUCE_WG)
        } else {
            path_tag_wgs
        };
        let path_reduced = BufferSize::new(reduced_size);
        let path_reduced2 = BufferSize::new(PATH_REDUCE_WG);
        let path_reduced_scan = BufferSize::new(reduced_size);
        let path_monoids = BufferSize::new(path_tag_wgs * PATH_REDUCE_WG);
        let path_bboxes = BufferSize::new(n_paths);
        let binning_wgs = workgroups.binning.0;
        let draw_monoid_wgs = workgroups.draw_reduce.0;
        let draw_reduced = BufferSize::new(draw_monoid_wgs);
        let draw_monoids = BufferSize::new(n_draw_objects);
        let info = BufferSize::new(layout.bin_data_start);
        let clip_inps = BufferSize::new(n_clips);
        let clip_els = BufferSize::new(n_clips);
        let clip_bics = BufferSize::new(n_clips / CLIP_REDUCE_WG);
        let clip_bboxes = BufferSize::new(n_clips);
        let draw_bboxes = BufferSize::new(n_paths);
        let bump_alloc = BufferSize::new(1);
        let indirect_count = BufferSize::new(1);
        let bin_headers = BufferSize::new(binning_wgs * 256);
        let n_paths_aligned = align_up(n_paths, 256);
        let paths = BufferSize::new(n_paths_aligned);

        // The following buffer sizes have been hand picked to accommodate the vello test scenes as
        // well as paris-30k. These should instead get derived from the scene layout using
        // reasonable heuristics.
        //
        // ---- qOS: derived from the scene, and then from measurement ----
        //
        // Upstream's own comment above says these constants are picked
        // for a 30,000-path stress scene and ought to be derived. They
        // come to about 165 MB, per renderer, and a renderer is per
        // process -- so a file manager showing four rows reserves what a
        // street map of Paris would, and a session of a dozen such
        // applications is two gigabytes of an integrated GPU's shared
        // memory before anything is drawn. On this machine, whose window
        // onto that memory is 16404 MB, it was four hard freezes in an
        // evening.
        //
        // So they come from the `budget` instead, and the budget is
        // measured: `BumpAllocators` -- which the GPU already writes and
        // `Renderer` now reads back every frame -- says what the frame
        // actually used, and `BumpBudget::observe` grows to cover it.
        //
        // What the budget may not do is go below what the *viewport*
        // structurally requires, which is the floor applied here:
        //
        //  * `ptcl` is indexed as `tile_ix * PTCL_INITIAL_ALLOC` before
        //    any bump allocation happens (`coarse.wgsl`), so the first
        //    `n_tiles * 64` words are spoken for whatever the scene is.
        //    Measured usage never counts them, and that is exactly how a
        //    blind fraction of upstream's constant goes wrong: at 3840x2160
        //    the floor alone is 2.07M words inside a `1 << 21` buffer,
        //    leaving 23k words to allocate from and dropping most of the
        //    screen's commands silently.
        //  * `tiles` takes one entry per tile per path covering it, so a
        //    single full-screen rectangle is already `n_tiles` of them.
        //  * `bin_data` shares its buffer with the draw-info region: the
        //    config's `binning_size` is `bin_data - bin_data_start`,
        //    which on a u32 underflows to near four billion if the buffer
        //    is ever sized below the scene's own info block.
        let n_tiles = width_in_tiles.max(1) * height_in_tiles.max(1);
        let floor = BumpBudget::floor_for_viewport(n_tiles);
        let bin_data = BufferSize::new(
            layout
                .bin_data_start
                .saturating_add(budget.binning.max(floor.binning)),
        );
        let tiles = BufferSize::new(budget.tiles.max(floor.tiles));
        let lines = BufferSize::new(budget.lines.max(floor.lines));
        let seg_counts = BufferSize::new(budget.seg_counts.max(floor.seg_counts));
        let segments = BufferSize::new(budget.segments.max(floor.segments));
        // 16 * 16 (1 << 8) is one blend spill, so this allows for 1024 spills.
        let blend_spill = BufferSize::new(budget.blend_spill.max(floor.blend_spill));
        let ptcl = BufferSize::new(
            n_tiles
                .saturating_mul(PTCL_INITIAL_ALLOC)
                .saturating_add(budget.ptcl.max(floor.ptcl)),
        );
        Self {
            path_reduced,
            path_reduced2,
            path_reduced_scan,
            path_monoids,
            path_bboxes,
            draw_reduced,
            draw_monoids,
            info,
            clip_inps,
            clip_els,
            clip_bics,
            clip_bboxes,
            draw_bboxes,
            bump_alloc,
            indirect_count,
            lines,
            bin_headers,
            paths,
            bin_data,
            tiles,
            seg_counts,
            segments,
            blend_spill,
            ptcl,
        }
    }
}

/// Words of `ptcl` reserved per tile before any allocation happens, from
/// `PTCL_INITIAL_ALLOC` in `shader/shared/ptcl.wgsl`. Kept in step by
/// hand, like the other shader constants in this file.
const PTCL_INITIAL_ALLOC: u32 = 64;

/// Floors for the measured buffers, in elements.
///
/// The budget grows to fit, but growth is a frame behind, so the floor
/// is what the *first* frame gets — and a first frame is the compositor
/// painting a whole desktop, not a blank window. These come from
/// measurement rather than taste: the shell's scene catalogue, all 28
/// scenes, rendered with the counters logged.
///
/// | buffer     | peak at 1920x1080 | per tile | peak at 3840x2160 | per tile |
/// |------------|-------------------|----------|-------------------|----------|
/// | lines      | 27943             | -        | 29377             | -        |
/// | binning    | 1833              | 0.22     | 5592              | 0.17     |
/// | tiles      | 115406            | 14.1     | 439039            | 13.5     |
/// | seg_counts | 54177             | 6.6      | 123122            | 3.8      |
/// | segments   | 54177             | -        | 123122            | -        |
/// | blend      | 0                 | -        | 0                 | -        |
/// | ptcl (dyn) | 659200            | 80.8     | 1361664           | 42.0     |
///
/// So the ones that track screen area are written per tile with roughly
/// twice the measured headroom, and the ones that track how much *stuff*
/// is on screen -- lines, and the binning list -- are flat. At 1920x1080
/// that is 13 MB reserved against upstream's 165 MB; at 3840x2160, 37 MB,
/// where upstream's ptcl constant is itself only four times the
/// structural floor.
///
/// The 28-scene catalogue above has no *foreign* surface in it, and a
/// composited Wayland client is not a flat fill: `foreign.rs` draws it
/// as `push_layer(rounded-rect clip) + draw_image + pop_layer`, and a
/// blend layer around a sampled image costs far more per-tile `ptcl`
/// than the vector UI the catalogue was measured from. Compositing a
/// client for the first time -- `client_buffer_pixels_are_composited_
/// into_the_tab_rect` in `shell/tests/foreign_surface_compositing.rs`,
/// a 1310x768 tab in a 1600x900 (5700-tile) viewport -- asked for
/// 1,085,696 `ptcl` words, ~190/tile against every tile in the
/// viewport, more than the old floor's 96/tile reserved in total. That
/// is the first frame a client ever presents: there is no prior frame
/// for growth to have learned from, so the floor is the only thing
/// standing between that frame and a silently missing window. Chrome
/// (the sidebar, the top bar) is permanent screen furniture in this
/// shell, so a maximized single tab cannot exceed roughly the 70% tile
/// coverage this measurement already has; 320/tile keeps clear of it
/// with room over both measurement noise and a second simultaneous
/// composited surface.
/// Path segments scale with how much is drawn, so this floor scales with the
/// viewport like the tile-indexed ones. A flat constant cannot cover a large or
/// high-DPI surface: a subtree cache at scale 2 measured 105,450 lines against
/// the old flat 65,536, and the overflow drops geometry silently.
const FLOOR_LINES_PER_TILE: u32 = 16;
const FLOOR_LINES_MIN: u32 = 1 << 16;
const FLOOR_BINNING: u32 = 1 << 14;
const FLOOR_BLEND_SPILL: u32 = 1 << 14;
const FLOOR_TILES_PER_TILE: u32 = 24;
const FLOOR_SEG_COUNTS_PER_TILE: u32 = 8;
const FLOOR_SEG_COUNTS_MIN: u32 = 1 << 17;
const FLOOR_PTCL_PER_TILE: u32 = 320;
const FLOOR_PTCL_MIN: u32 = 1 << 17;

/// How much bump-allocated scratch a renderer reserves, in elements.
///
/// Every field here is a count the GPU also reports back through
/// [`BumpAllocators`], which is what makes the reservation measurable
/// rather than guessed: [`BumpBudget::observe`] takes what a frame asked
/// for and grows the budget to cover it.
///
/// Growth is one-way within a process. A window that once drew a map
/// keeps room to draw a map, on the grounds that it probably will again,
/// and that handing the memory back costs a reallocation of every buffer
/// at the moment the user is busiest.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BumpBudget {
    /// Flattened line segments ([`LineSoup`]).
    pub lines: u32,
    /// Words of per-bin draw-object lists, not counting the draw-info
    /// region they share a buffer with.
    pub binning: u32,
    /// Per-path tile allocations ([`Tile`]).
    pub tiles: u32,
    /// Line/tile crossings counted by `path_count` ([`SegmentCount`]).
    pub seg_counts: u32,
    /// Per-tile path segments ([`PathSegment`]).
    pub segments: u32,
    /// Words of spilled blend scratch.
    pub blend_spill: u32,
    /// Words of per-tile command list beyond the structural
    /// `n_tiles * PTCL_INITIAL_ALLOC` floor.
    pub ptcl: u32,
}

impl BumpBudget {
    /// Upstream vello's constants, kept for reference and as the scale
    /// the ceiling below is expressed in.
    pub const UPSTREAM: Self = Self {
        lines: 1 << 21,
        binning: 1 << 18,
        tiles: 1 << 21,
        seg_counts: 1 << 21,
        segments: 1 << 21,
        blend_spill: 1 << 20,
        ptcl: 1 << 23,
    };

    /// What a renderer starts from, before it has seen a frame: zero,
    /// with [`BufferSizes`] applying the viewport floor underneath. A
    /// renderer that never draws anything reserves what the floor says
    /// and no more.
    pub const INITIAL: Self = Self {
        lines: 0,
        binning: 0,
        tiles: 0,
        seg_counts: 0,
        segments: 0,
        blend_spill: 0,
        ptcl: 0,
    };

    /// The least a viewport of `n_tiles` can be rendered with, per the
    /// measurements at the constants above.
    pub fn floor_for_viewport(n_tiles: u32) -> Self {
        Self {
            lines: n_tiles
                .saturating_mul(FLOOR_LINES_PER_TILE)
                .max(FLOOR_LINES_MIN),
            binning: FLOOR_BINNING,
            tiles: n_tiles.saturating_mul(FLOOR_TILES_PER_TILE),
            seg_counts: n_tiles
                .saturating_mul(FLOOR_SEG_COUNTS_PER_TILE)
                .max(FLOOR_SEG_COUNTS_MIN),
            segments: n_tiles
                .saturating_mul(FLOOR_SEG_COUNTS_PER_TILE)
                .max(FLOOR_SEG_COUNTS_MIN),
            blend_spill: FLOOR_BLEND_SPILL,
            ptcl: n_tiles
                .saturating_mul(FLOOR_PTCL_PER_TILE)
                .max(FLOOR_PTCL_MIN),
        }
    }

    /// Nothing grows past this -- four times upstream's reservation. No
    /// user interface reaches it; a scene that did would have rendered
    /// wrong on upstream too, and this way the renderer can say so
    /// instead of reserving without limit.
    pub const CEILING: Self = Self {
        lines: Self::UPSTREAM.lines << 2,
        binning: Self::UPSTREAM.binning << 2,
        tiles: Self::UPSTREAM.tiles << 2,
        seg_counts: Self::UPSTREAM.seg_counts << 2,
        segments: Self::UPSTREAM.segments << 2,
        blend_spill: Self::UPSTREAM.blend_spill << 2,
        ptcl: Self::UPSTREAM.ptcl << 2,
    };

    /// Grow to cover what a frame reported using, with headroom.
    ///
    /// Returns `true` if anything changed, which is the caller's cue to
    /// drop pooled buffers of the sizes that are no longer asked for.
    ///
    /// The headroom matters because this check is a frame behind: on a
    /// scene that grows steadily, reserving exactly what the last frame
    /// used means the next one overflows. Doubling it means a scene has
    /// to double in complexity between two consecutive frames to catch
    /// this out -- and even then the frame after it is correct, which is
    /// more than either upstream's constants or a blind fraction of them
    /// can say.
    pub fn observe(&mut self, used: &BumpAllocators) -> bool {
        let mut changed = false;
        let mut grow = |field: &mut u32, used: u32, ceiling: u32| {
            let want = used.saturating_mul(2).max(used).min(ceiling);
            if want > *field {
                *field = want;
                changed = true;
            }
        };
        grow(&mut self.lines, used.lines, Self::CEILING.lines);
        grow(&mut self.binning, used.binning, Self::CEILING.binning);
        grow(&mut self.tiles, used.tile, Self::CEILING.tiles);
        grow(
            &mut self.seg_counts,
            used.seg_counts,
            Self::CEILING.seg_counts,
        );
        grow(&mut self.segments, used.segments, Self::CEILING.segments);
        grow(&mut self.blend_spill, used.blend, Self::CEILING.blend_spill);
        grow(&mut self.ptcl, used.ptcl, Self::CEILING.ptcl);
        changed
    }

    /// Bytes this budget reserves, not counting the viewport floors.
    pub fn size_in_bytes(&self) -> u64 {
        u64::from(self.lines) * size_of::<LineSoup>() as u64
            + u64::from(self.binning) * 4
            + u64::from(self.tiles) * size_of::<Tile>() as u64
            + u64::from(self.seg_counts) * size_of::<SegmentCount>() as u64
            + u64::from(self.segments) * size_of::<PathSegment>() as u64
            + u64::from(self.blend_spill) * 4
            + u64::from(self.ptcl) * 4
    }
}

impl Default for BumpBudget {
    fn default() -> Self {
        Self::INITIAL
    }
}

const fn align_up(len: u32, alignment: u32) -> u32 {
    len + (len.wrapping_neg() & (alignment - 1))
}

#[cfg(test)]
mod tests {
    use super::{BufferSizes, BumpAllocators, BumpBudget, PTCL_INITIAL_ALLOC, RenderConfig};
    use crate::{Layout, WorkgroupCounts};

    /// A layout with nothing in it. The bump-allocated sizes do not
    /// depend on the scene, only on the viewport and the budget, so this
    /// is enough to exercise them.
    fn sizes(width: u32, height: u32, budget: &BumpBudget) -> BufferSizes {
        let layout = Layout {
            bin_data_start: 4096,
            ..Layout::default()
        };
        let (w_tiles, h_tiles) = (width.div_ceil(16), height.div_ceil(16));
        let wgs = WorkgroupCounts::new(&layout, w_tiles, h_tiles, 0);
        BufferSizes::new(&layout, &wgs, w_tiles, h_tiles, budget)
    }

    /// `coarse.wgsl` indexes ptcl as `tile_ix * PTCL_INITIAL_ALLOC`
    /// before it allocates anything, so that much of the buffer is
    /// spoken for whatever the scene is. Sizing the buffer as a fraction
    /// of a constant -- rather than from the viewport -- leaves a 4K
    /// screen with a few thousand words to draw a desktop from, and the
    /// overflow is silent.
    #[test]
    fn ptcl_has_room_to_allocate_from_at_every_resolution() {
        for (w, h) in [(800, 600), (1920, 1080), (2560, 1440), (3840, 2160)] {
            let s = sizes(w, h, &BumpBudget::INITIAL);
            let structural = w.div_ceil(16) * h.div_ceil(16) * PTCL_INITIAL_ALLOC;
            let dynamic = s.ptcl.len() - structural;
            assert!(
                dynamic >= structural / 2,
                "{w}x{h}: {dynamic} words to allocate from, under a floor of {structural}",
            );
        }
    }

    /// A composited Wayland client is not a flat fill: the compositor
    /// draws it as `push_layer(rounded-rect clip) + draw_image +
    /// pop_layer`, which costs far more `ptcl` per tile than the vector
    /// UI the floor's per-tile constant was calibrated from. This is
    /// the exact ask a real first frame of `shell`'s
    /// `client_buffer_pixels_are_composited_into_the_tab_rect` made
    /// (`VELLO_BUMP_LOG=1`, a 1310x768 tab in a 1600x900 viewport): a
    /// regression test, not a synthetic bound, because a synthetic one
    /// would have missed this exact shape of scene the way the
    /// catalogue did the first time.
    ///
    /// The first frame a client ever presents has no earlier frame for
    /// `BumpBudget::observe` to have grown from, so the static floor is
    /// the only thing standing between that frame and a client's
    /// window rendering with its content silently missing. Below the
    /// floor this raised, that is exactly what happened: `failed=0x10`
    /// (the `ptcl` bit), and the composited pixels never reached the
    /// screen.
    #[test]
    fn ptcl_floor_covers_a_clipped_foreign_surface_composite_first_frame() {
        let width_in_tiles = 1600_u32.div_ceil(16);
        let height_in_tiles = 900_u32.div_ceil(16);
        let n_tiles = width_in_tiles * height_in_tiles;
        // BumpAllocators.ptcl measured on that frame; already excludes
        // the structural `n_tiles * PTCL_INITIAL_ALLOC` term (see
        // `ptcl_reserves_the_structural_floor_on_top_of_the_heuristic_one`),
        // so it compares directly against the heuristic floor alone.
        let measured_ptcl_words = 1_085_696;
        let floor = BumpBudget::floor_for_viewport(n_tiles);
        assert!(
            floor.ptcl >= measured_ptcl_words,
            "floor of {} ptcl words does not cover the {measured_ptcl_words} a real clipped \
             client composite asked for at {n_tiles} tiles -- the first frame a client \
             presents would overflow ptcl and render with its pixels silently missing",
            floor.ptcl,
        );
    }

    /// A cached subtree redrawn at scale 2 measured 105,450 lines against
    /// a flat floor of 65,536, overflowed (`failed=0x4`) and lost geometry.
    ///
    /// The lesson is the shape of the floor rather than its size: every other
    /// floor here scales with `n_tiles` and this one did not. Segment count
    /// follows how much is drawn, so a flat constant covers a small window and
    /// silently under-provisions a large or high-DPI one -- the case least
    /// likely to be measured and most likely to be somebody's actual desktop.
    #[test]
    fn lines_floor_scales_with_the_viewport_it_has_to_cover() {
        let n_tiles = 1920_u32.div_ceil(16) * 1080_u32.div_ceil(16);
        let measured_lines = 105_450;
        let floor = BumpBudget::floor_for_viewport(n_tiles);
        assert!(
            floor.lines >= measured_lines,
            "floor of {} lines does not cover the {measured_lines} a subtree cache at \
             scale 2 asked for at {n_tiles} tiles",
            floor.lines,
        );
        // And the viewport is what decides: a desktop must reserve more than a
        // small window, or the floor is flat in disguise.
        let small = BumpBudget::floor_for_viewport(320_u32.div_ceil(16) * 240_u32.div_ceil(16));
        assert!(
            floor.lines > small.lines,
            "a desktop reserves {} lines and a 320x240 window {} -- the floor is not \
             scaling with the viewport",
            floor.lines,
            small.lines,
        );
    }

    /// The structural `n_tiles * PTCL_INITIAL_ALLOC` term is reserved on
    /// top of the budget/floor, not folded into it.
    ///
    /// `floor_for_viewport`'s own per-tile headroom happens to be 1.5x
    /// `PTCL_INITIAL_ALLOC`, so at the `INITIAL` budget the test above
    /// still passes -- at exactly zero margin -- even with the
    /// structural term removed entirely. This isolates the claim: the
    /// reservation must clear the heuristic floor *plus* the structural
    /// one, which only holding both terms produces.
    #[test]
    fn ptcl_reserves_the_structural_floor_on_top_of_the_heuristic_one() {
        for (w, h) in [(800, 600), (1920, 1080), (3840, 2160)] {
            let s = sizes(w, h, &BumpBudget::INITIAL);
            let n_tiles = w.div_ceil(16) * h.div_ceil(16);
            let structural = n_tiles * PTCL_INITIAL_ALLOC;
            let heuristic_floor = BumpBudget::floor_for_viewport(n_tiles).ptcl;
            assert!(
                s.ptcl.len() >= structural + heuristic_floor,
                "{w}x{h}: reserved {}, expected the structural floor {structural} \
                 plus the heuristic one {heuristic_floor} = {}",
                s.ptcl.len(),
                structural + heuristic_floor,
            );
        }
    }

    /// The config hands the shaders `bin_data - bin_data_start` as the
    /// binning size. On a u32 that underflows to near four billion if
    /// the buffer is ever smaller than the scene's own draw-info block,
    /// and the shaders then believe they have unlimited room.
    #[test]
    fn bin_data_always_clears_the_info_block() {
        let layout = Layout {
            bin_data_start: 1 << 20,
            ..Layout::default()
        };
        let wgs = WorkgroupCounts::new(&layout, 120, 68, 0);
        let s = BufferSizes::new(&layout, &wgs, 120, 68, &BumpBudget::INITIAL);
        assert!(
            s.bin_data.len() > layout.bin_data_start,
            "bin_data {} does not clear bin_data_start {}",
            s.bin_data.len(),
            layout.bin_data_start,
        );
        let cfg = RenderConfig::new(
            &layout,
            1920,
            1080,
            &peniko::Color::BLACK,
            &BumpBudget::INITIAL,
        );
        assert!(
            cfg.gpu.binning_size < s.bin_data.len(),
            "binning_size {} underflowed",
            cfg.gpu.binning_size,
        );
    }

    /// Growth covers what a frame asked for, including the part that did
    /// not fit -- the shaders bump the counter first and check after, so
    /// the reported count is the need, not the fit.
    #[test]
    fn growth_covers_an_overflowed_frame() {
        let mut budget = BumpBudget::INITIAL;
        let overflowed = BumpAllocators {
            failed: 0x2,
            binning: 4_000,
            ptcl: 700_000,
            tile: 500_000,
            seg_counts: 90_000,
            segments: 90_000,
            blend: 0,
            lines: 40_000,
        };
        assert!(budget.observe(&overflowed), "should have grown");
        assert!(budget.tiles >= overflowed.tile);
        assert!(budget.ptcl >= overflowed.ptcl);
        assert!(budget.lines >= overflowed.lines);
        // And it settles: a second frame like the first asks for nothing
        // more, so the buffers stop being reallocated.
        assert!(
            !budget.observe(&overflowed),
            "a repeat of the same frame should not grow again"
        );
    }

    /// Growth is one-way and bounded.
    #[test]
    fn growth_stops_at_the_ceiling() {
        let mut budget = BumpBudget::INITIAL;
        let huge = BumpAllocators {
            failed: 0x1f,
            binning: u32::MAX,
            ptcl: u32::MAX,
            tile: u32::MAX,
            seg_counts: u32::MAX,
            segments: u32::MAX,
            blend: u32::MAX,
            lines: u32::MAX,
        };
        budget.observe(&huge);
        assert_eq!(budget.tiles, BumpBudget::CEILING.tiles);
        assert_eq!(budget.ptcl, BumpBudget::CEILING.ptcl);
        let quiet = BumpAllocators::default();
        assert!(
            !budget.observe(&quiet),
            "a quiet frame must not shrink the reservation mid-session"
        );
    }

    /// The whole point: a desktop-sized viewport reserves a fraction of
    /// what upstream's constants do.
    #[test]
    fn a_desktop_viewport_reserves_far_less_than_upstream() {
        let ours = sizes(1920, 1080, &BumpBudget::INITIAL);
        let theirs = sizes(1920, 1080, &BumpBudget::UPSTREAM);
        let bytes = |s: &BufferSizes| -> u64 {
            u64::from(s.lines.size_in_bytes())
                + u64::from(s.bin_data.size_in_bytes())
                + u64::from(s.tiles.size_in_bytes())
                + u64::from(s.seg_counts.size_in_bytes())
                + u64::from(s.segments.size_in_bytes())
                + u64::from(s.blend_spill.size_in_bytes())
                + u64::from(s.ptcl.size_in_bytes())
        };
        let (ours, theirs) = (bytes(&ours), bytes(&theirs));
        assert!(
            ours * 8 < theirs,
            "reserved {ours} bytes against upstream's {theirs}; expected under an eighth",
        );
    }
}
