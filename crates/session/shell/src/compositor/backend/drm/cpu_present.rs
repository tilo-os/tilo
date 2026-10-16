//! Desktop rendering on the CPU, for GPUs that cannot run vello.
//!
//! vello's compute pipeline faults on Mali Bifrost under panvk: every dispatch
//! page-faults and a frame takes seconds, with black tiles where work was
//! lost. Such a device renders the scene with [`tilo_cpu_raster`] into one
//! premultiplied RGBA canvas held in cached memory, and the scanout copies
//! the changed rectangles into its dumb buffers.
//!
//! The canvas keeps the desktop between frames. Which of its tiles hold the
//! current scene is tracked per 16 pixel tile. The shell's damage regions
//! narrow what is compared; the decoded draws decide what changed
//! ([`tilo_cpu_raster::CpuRasterizer::changes`]), so a clock tick redraws its
//! digits, and a pan moves the pixels it only moved and draws what it
//! reveals.
//!
//! Only the primary plane is written. Video pictures reach the screen through
//! the scene; an overlay plane for them (VOP2 has two that take NV12) would
//! be another plane in the flip, outside this canvas.
//!
//! What changed on screen since the last presented frame accumulates in
//! [`CpuRenderer::pending`]. The scanout's per-buffer destination history
//! ([`super::xrgb_history`]) turns that into the rectangles each dumb buffer
//! is missing.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    reason = "pixel and tile arithmetic on a canvas whose sides fit u16 (vello_cpu's limit), checked when it is allocated"
)]

use std::sync::OnceLock;
use std::time::Duration;
use std::time::Instant;

use tilo_cpu_raster::{CpuRasterizer, Options, Region, SceneChanges, Target};
use vello::FineDamageRect;
use vello::Scene;
use vello::peniko::Color;

/// Canvas validity is tracked in tiles of this edge, the same 16 pixels the
/// shell snaps its damage to.
const TILE: u32 = 16;

/// Copies of at least this many pixels are split across threads: one
/// Cortex-A55 core does not saturate the memory bus.
const PARALLEL_COPY_PIXELS: u64 = 256 * 1024;

/// More pending rectangles than this are presented as one full copy.
const MAX_PENDING: usize = 48;

/// Environment switch: `cpu` forces the CPU path, `gpu` forbids it.
pub const RENDERER_ENV: &str = "TILO_DRM_RENDERER";

/// Why this session renders the desktop on the CPU, or `None` to use vello
/// on the GPU. Decided once per process, before any GPU device is opened.
pub fn cpu_render_reason() -> Option<&'static str> {
    static REASON: OnceLock<Option<String>> = OnceLock::new();
    REASON
        .get_or_init(|| {
            let env = std::env::var(RENDERER_ENV).ok();
            let nodes = render_node_compatibles();
            let reason = select_cpu_render(env.as_deref(), &nodes);
            eprintln!(
                "tilo compositor: DRM renderer={} reason={:?} {RENDERER_ENV}={:?} render_nodes={:?}",
                if reason.is_some() { "cpu" } else { "gpu" },
                reason.as_deref().unwrap_or("vello on the GPU"),
                env.as_deref().unwrap_or(""),
                nodes,
            );
            reason
        })
        .as_deref()
}

/// Devicetree compatibles of every DRM render node. A PCI GPU has no
/// devicetree node and reports an empty string.
fn render_node_compatibles() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir("/sys/class/drm") else {
        return Vec::new();
    };
    let mut nodes: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("renderD"))
        .map(|entry| {
            std::fs::read(entry.path().join("device/of_node/compatible"))
                .map(|bytes| {
                    String::from_utf8_lossy(&bytes)
                        .split('\0')
                        .filter(|part| !part.is_empty())
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_default()
        })
        .collect();
    nodes.sort();
    nodes
}

/// The explicit switch wins. Otherwise the CPU renders when every render node
/// is a Mali Bifrost GPU, whose only Vulkan driver (panvk) cannot run vello.
/// No render node at all keeps the GPU path and its software adapter.
fn select_cpu_render(env: Option<&str>, compatibles: &[String]) -> Option<String> {
    match env {
        Some("cpu") => return Some(format!("{RENDERER_ENV}=cpu")),
        Some("gpu") => return None,
        _ => {}
    }
    let bifrost = |compatible: &String| {
        compatible
            .split(',')
            .collect::<Vec<_>>()
            .windows(2)
            .any(|pair| pair == ["arm", "mali-bifrost"])
    };
    (!compatibles.is_empty() && compatibles.iter().all(bifrost))
        .then(|| "Mali Bifrost GPU: vello's compute pipeline faults under panvk".to_owned())
}

/// One frame as the CPU renderer sees it.
#[derive(Clone, Copy)]
pub struct CpuFrame<'a> {
    pub scene: &'a Scene,
    /// Zero for a frame without a damage relation to its predecessor.
    pub revision: u64,
    pub previous_revision: Option<u64>,
    pub regions: Option<&'a [FineDamageRect]>,
}

/// What one frame cost, for the session log.
#[derive(Clone, Debug, Default)]
pub struct CpuFrameStats {
    pub fill: &'static str,
    pub revision: u64,
    pub previous_revision: Option<u64>,
    /// The pointer is drawn into the frame: there is no cursor plane.
    pub software_cursor: bool,
    pub decode: Duration,
    pub raster: Duration,
    pub raster_px: u64,
    pub raster_regions: usize,
    /// Pixels moved within the canvas instead of drawn (a pan or scroll).
    pub moved_px: u64,
    pub move_time: Duration,
    /// The whole-pixel offset the change moved by, or why it is no move.
    pub motion: String,
    pub copy: Duration,
    pub copy_px: u64,
    pub copy_rects: usize,
    pub flip_wait: Duration,
    /// Queue-to-vblank time of the flip the previous frame queued.
    pub previous_flip_latency: Option<Duration>,
    pub presented: bool,
    pub total: Duration,
}

impl CpuFrameStats {
    /// Key=value fields appended to the `FRAME-LIFECYCLE` line.
    pub fn log_fields(&self) -> String {
        format!(
            "renderer=cpu revision={} previous_revision={} cursor={} fill={} decode_us={} raster_us={} raster_px={} raster_regions={} motion={:?} moved_px={} move_us={} copy_us={} copy_px={} copy_rects={} flip_wait_us={} prev_flip_latency_us={} presented={} cpu_total_us={}",
            self.revision,
            self.previous_revision
                .map_or_else(|| "none".to_owned(), |revision| revision.to_string()),
            if self.software_cursor {
                "software"
            } else {
                "plane"
            },
            self.fill,
            self.decode.as_micros(),
            self.raster.as_micros(),
            self.raster_px,
            self.raster_regions,
            self.motion,
            self.moved_px,
            self.move_time.as_micros(),
            self.copy.as_micros(),
            self.copy_px,
            self.copy_rects,
            self.flip_wait.as_micros(),
            self.previous_flip_latency.map_or_else(
                || "none".to_owned(),
                |latency| latency.as_micros().to_string()
            ),
            self.presented,
            self.total.as_micros(),
        )
    }

    /// Whether the frame changed anything worth a log line.
    pub fn did_work(&self) -> bool {
        self.raster_px > 0 || self.moved_px > 0 || self.copy_px > 0
    }
}

/// The CPU renderer's retained canvas and the screen damage not yet
/// presented.
pub struct CpuRenderer {
    raster: CpuRasterizer,
    canvas: Vec<u8>,
    width: u32,
    height: u32,
    screen: (u32, u32),
    /// Bumped whenever the canvas is reallocated; part of the scanout
    /// history identity, so no dumb buffer claims pixels of an old canvas.
    generation: u64,
    /// Scene revision the valid tiles hold; `None` when unversioned.
    revision: Option<u64>,
    tiles_x: u32,
    tiles_y: u32,
    valid: Vec<bool>,
    want: Vec<bool>,
    /// Screen damage since the last presented frame.
    pending: Vec<FineDamageRect>,
    pending_full: bool,
    /// Present counter, the revision the scanout history chains on.
    presented_revision: u64,
    stats: CpuFrameStats,
    /// The last finished frame that changed pixels, until it is logged.
    finished: Option<CpuFrameStats>,
    started: Option<Instant>,
    copy_measured: bool,
    /// The rasterizer holds a decoded scene.
    prepared: bool,
    copy_threads: usize,
}

impl std::fmt::Debug for CpuRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CpuRenderer")
            .field("canvas", &(self.width, self.height))
            .field("screen", &self.screen)
            .finish_non_exhaustive()
    }
}

impl Default for CpuRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl CpuRenderer {
    pub fn new() -> Self {
        let threads = std::thread::available_parallelism()
            .map_or(1, std::num::NonZeroUsize::get)
            .min(4);
        let threads = u16::try_from(threads).unwrap_or(1);
        eprintln!("tilo compositor: CPU renderer threads={threads}");
        Self {
            raster: CpuRasterizer::new(Options {
                threads,
                ..Options::default()
            }),
            canvas: Vec::new(),
            width: 0,
            height: 0,
            screen: (0, 0),
            generation: 0,
            revision: None,
            tiles_x: 0,
            tiles_y: 0,
            valid: Vec::new(),
            want: Vec::new(),
            pending: Vec::new(),
            pending_full: true,
            presented_revision: 0,
            stats: CpuFrameStats::default(),
            finished: None,
            started: None,
            copy_measured: false,
            prepared: false,
            copy_threads: usize::from(threads.max(1)),
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn presented_revision(&self) -> u64 {
        self.presented_revision
    }

    pub fn stats(&self) -> CpuFrameStats {
        self.stats.clone()
    }

    /// Screen damage to present, `None` for the whole screen.
    pub fn pending(&self) -> Option<&[FineDamageRect]> {
        (!self.pending_full).then_some(self.pending.as_slice())
    }

    /// Anything changed on screen since the last present.
    pub fn has_pending(&self) -> bool {
        self.pending_full || !self.pending.is_empty()
    }

    /// Add screen damage the scanout found itself, such as a software
    /// cursor that moved.
    pub fn add_pending(&mut self, rect: FineDamageRect) {
        self.push_pending(rect);
    }

    /// Everything on screen must be copied again, as after a mode change.
    pub fn invalidate_screen(&mut self) {
        self.pending_full = true;
        self.pending.clear();
    }

    /// The frame was presented: its damage is now in the scanout history.
    pub fn mark_presented(&mut self, revision: u64) {
        self.presented_revision = revision;
        self.pending.clear();
        self.pending_full = false;
    }

    /// Retain pixels for retry when the scanout did not accept publication.
    /// This records acceptance, not physical display completion.
    pub fn finish_present(&mut self, revision: u64, accepted: bool) {
        self.stats.presented = accepted;
        if accepted {
            self.mark_presented(revision);
        } else {
            self.invalidate_screen();
        }
    }

    pub fn record_present(&mut self, copy: Duration, copy_px: u64, copy_rects: usize) {
        self.stats.copy = copy;
        self.stats.copy_px = copy_px;
        self.stats.copy_rects = copy_rects;
        self.stats.presented = true;
    }

    pub fn record_cursor(&mut self, software: bool) {
        self.stats.software_cursor = software;
    }

    pub fn record_flip_wait(&mut self, wait: Duration, latency: Option<Duration>) {
        self.stats.flip_wait = wait;
        self.stats.previous_flip_latency = latency;
    }

    pub fn finish_frame(&mut self) {
        if let Some(started) = self.started.take() {
            self.stats.total = started.elapsed();
        }
        if self.stats.did_work() {
            self.finished = Some(self.stats.clone());
        }
    }

    /// The last frame that changed pixels, once.
    pub fn take_finished(&mut self) -> Option<CpuFrameStats> {
        self.finished.take()
    }

    /// Bring the canvas up to date with `frame` for a `screen`-sized
    /// output and record what changed on screen.
    pub fn update(&mut self, frame: CpuFrame<'_>, screen: (u32, u32)) {
        self.started = Some(Instant::now());
        self.stats = CpuFrameStats {
            revision: frame.revision,
            previous_revision: frame.previous_revision,
            ..CpuFrameStats::default()
        };
        if screen != (self.width, self.height) {
            self.resize(screen);
        }

        let chained = frame.revision != 0
            && self.revision.is_some()
            && frame.previous_revision == self.revision;
        // The shell's damage: `None` when it does not know what changed.
        let shell_damage: Option<&[FineDamageRect]> = if chained {
            self.stats.fill = if frame.regions.is_some() {
                "damage"
            } else {
                "full"
            };
            frame.regions
        } else {
            self.stats.fill = "full";
            None
        };
        // A reused scene is the one decoded last; anything else is decoded
        // now, so it can be rendered and compared with its predecessor.
        if shell_damage.is_none_or(|rects| !rects.is_empty()) || !self.prepared {
            let started = Instant::now();
            self.raster.prepare(frame.scene.encoding());
            self.stats.decode = started.elapsed();
            self.prepared = true;
        }
        let mut moved = Vec::new();
        match shell_damage {
            Some([]) => {}
            // The shell's rectangles bound a section; the changed draws inside
            // it are usually far smaller (a clock's digits in the top bar).
            Some(rects) => {
                let area = |w: u32, h: u32| u64::from(w) * u64::from(h);
                let shell_area: u64 = rects.iter().map(|r| area(r.width, r.height)).sum();
                match self
                    .raster
                    .changes(self.width, self.height)
                    .filter(|changes| {
                        changes
                            .regions
                            .iter()
                            .map(|r| area(r.width, r.height))
                            .sum::<u64>()
                            < shell_area
                    }) {
                    Some(changes) => {
                        self.stats.fill = "damage-diff";
                        moved = self.apply_changes(changes);
                    }
                    None => {
                        for rect in rects {
                            self.invalidate(*rect);
                        }
                    }
                }
            }
            // Where the shell cannot say, the decoded draws can: the valid
            // tiles hold the scene decoded before this one, and only pixels
            // under a draw that differs between the two can change.
            None => {
                if let Some(changes) = self.raster.changes(self.width, self.height) {
                    self.stats.fill = "diff";
                    moved = self.apply_changes(changes);
                } else {
                    self.valid.fill(false);
                    self.invalidate_screen();
                }
            }
        }
        self.revision = (frame.revision != 0).then_some(frame.revision);

        for region in &moved {
            self.push_pending(region_rect(*region));
        }
        let regions = self.invalid_regions();
        if regions.is_empty() {
            return;
        }
        let started = Instant::now();
        let (width, height) = (self.width, self.height);
        let Ok(mut target) = Target::new(&mut self.canvas, width, height, width as usize * 4)
        else {
            return;
        };
        // The produced scene owns the desktop ground. Clear invalid canvas
        // regions to transparent before replaying that scene.
        self.raster
            .render_prepared(&mut target, Some(&regions), Color::TRANSPARENT);
        self.stats.raster = started.elapsed();
        self.stats.raster_regions = regions.len();
        for region in &regions {
            self.stats.raster_px = self
                .stats
                .raster_px
                .saturating_add(u64::from(region.width) * u64::from(region.height));
            self.mark_valid(*region);
            self.push_pending(region_rect(*region));
        }
    }

    /// Invalidate what changed, after moving the pixels a pan or scroll
    /// only moved. Returns the regions that were moved and are now valid.
    fn apply_changes(&mut self, changes: SceneChanges) -> Vec<Region> {
        let mut moved = Vec::new();
        self.stats.motion = match (&changes.motion, &changes.not_a_motion) {
            (Some(motion), _) => format!(
                "{},{} opaque={} obstacles={}",
                motion.dx,
                motion.dy,
                motion.opaque.len(),
                motion.obstacles.len()
            ),
            (None, Some(reason)) => reason.clone(),
            (None, None) => String::new(),
        };
        if let Some(motion) = &changes.motion {
            let (dx, dy) = (i64::from(motion.dx), i64::from(motion.dy));
            for run in motion.movable(self.width, self.height, TILE) {
                // Tile by tile: the source pixels must all be valid.
                let mut x = run.x;
                while x < run.x + run.width {
                    let width = TILE.min(run.x + run.width - x);
                    let source = FineDamageRect {
                        x: u32::try_from(i64::from(x) - dx).unwrap_or(u32::MAX),
                        y: u32::try_from(i64::from(run.y) - dy).unwrap_or(u32::MAX),
                        width,
                        height: run.height,
                    };
                    if self.all_valid(source) {
                        match moved.last_mut() {
                            Some(Region {
                                x: px,
                                y,
                                width: pw,
                                height,
                            }) if *y == run.y && *height == run.height && *px + *pw == x => {
                                *pw += width;
                            }
                            _ => moved.push(Region::new(x, run.y, width, run.height)),
                        }
                    }
                    x += width;
                }
            }
            if !moved.is_empty() {
                let (width, height) = (self.width, self.height);
                if let Ok(mut target) =
                    Target::new(&mut self.canvas, width, height, width as usize * 4)
                {
                    let started = Instant::now();
                    tilo_cpu_raster::move_pixels(&mut target, &moved, motion.dx, motion.dy);
                    self.stats.move_time = started.elapsed();
                } else {
                    moved.clear();
                }
            }
        }
        for region in changes.regions {
            self.invalidate(region_rect(region));
        }
        for region in &moved {
            self.mark_valid(*region);
            self.stats.moved_px = self
                .stats
                .moved_px
                .saturating_add(u64::from(region.width) * u64::from(region.height));
        }
        moved
    }

    /// Every tile under `rect` holds the current canvas.
    fn all_valid(&self, rect: FineDamageRect) -> bool {
        if rect.x.saturating_add(rect.width) > self.width
            || rect.y.saturating_add(rect.height) > self.height
        {
            return false;
        }
        let (x0, y0, x1, y1) = self.tile_span(rect);
        (y0..y1).all(|ty| {
            let row = (ty * self.tiles_x) as usize;
            self.valid
                .get(row + x0 as usize..row + x1 as usize)
                .is_some_and(|tiles| tiles.iter().all(|valid| *valid))
        })
    }

    fn resize(&mut self, (width, height): (u32, u32)) {
        self.width = width;
        self.height = height;
        self.screen = (width, height);
        self.canvas = vec![0; width as usize * height as usize * 4];
        self.tiles_x = width.div_ceil(TILE);
        self.tiles_y = height.div_ceil(TILE);
        let tiles = self.tiles_x as usize * self.tiles_y as usize;
        self.valid = vec![false; tiles];
        self.want = vec![false; tiles];
        self.revision = None;
        self.generation = self.generation.wrapping_add(1);
        self.invalidate_screen();
    }

    fn tile_span(&self, rect: FineDamageRect) -> (u32, u32, u32, u32) {
        let x0 = (rect.x / TILE).min(self.tiles_x);
        let y0 = (rect.y / TILE).min(self.tiles_y);
        let x1 = rect
            .x
            .saturating_add(rect.width)
            .div_ceil(TILE)
            .min(self.tiles_x);
        let y1 = rect
            .y
            .saturating_add(rect.height)
            .div_ceil(TILE)
            .min(self.tiles_y);
        (x0, y0, x1, y1)
    }

    fn invalidate(&mut self, rect: FineDamageRect) {
        let (x0, y0, x1, y1) = self.tile_span(rect);
        for ty in y0..y1 {
            let row = (ty * self.tiles_x) as usize;
            if let Some(tiles) = self.valid.get_mut(row + x0 as usize..row + x1 as usize) {
                tiles.fill(false);
            }
        }
    }

    fn mark_valid(&mut self, region: Region) {
        let (x0, y0, x1, y1) = self.tile_span(region_rect(region));
        for ty in y0..y1 {
            let row = (ty * self.tiles_x) as usize;
            if let Some(tiles) = self.valid.get_mut(row + x0 as usize..row + x1 as usize) {
                tiles.fill(true);
            }
        }
    }

    /// Invalid tiles, merged into rectangles: runs along a tile row, joined
    /// with identical runs in the rows below.
    fn invalid_regions(&mut self) -> Vec<Region> {
        for (want, valid) in self.want.iter_mut().zip(&self.valid) {
            *want = !*valid;
        }
        // (x0, x1, y0, y1) in tiles.
        let mut open: Vec<(u32, u32, u32, u32)> = Vec::new();
        let mut closed: Vec<(u32, u32, u32, u32)> = Vec::new();
        for ty in 0..self.tiles_y {
            let row = (ty * self.tiles_x) as usize;
            let mut runs = Vec::new();
            let mut tx = 0;
            while tx < self.tiles_x {
                if self.want.get(row + tx as usize).copied().unwrap_or(false) {
                    let start = tx;
                    while tx < self.tiles_x
                        && self.want.get(row + tx as usize).copied().unwrap_or(false)
                    {
                        tx += 1;
                    }
                    runs.push((start, tx));
                } else {
                    tx += 1;
                }
            }
            let mut next_open = Vec::with_capacity(runs.len());
            for (start, end) in runs {
                if let Some(position) = open
                    .iter()
                    .position(|&(x0, x1, _, _)| x0 == start && x1 == end)
                {
                    let (x0, x1, y0, _) = open.swap_remove(position);
                    next_open.push((x0, x1, y0, ty + 1));
                } else {
                    next_open.push((start, end, ty, ty + 1));
                }
            }
            closed.append(&mut open);
            open = next_open;
        }
        closed.append(&mut open);
        closed
            .into_iter()
            .map(|(x0, x1, y0, y1)| {
                let x = x0 * TILE;
                let y = y0 * TILE;
                Region::new(
                    x,
                    y,
                    (x1 * TILE).min(self.width) - x,
                    (y1 * TILE).min(self.height) - y,
                )
            })
            .collect()
    }

    fn push_pending(&mut self, rect: FineDamageRect) {
        if self.pending_full || rect.width == 0 || rect.height == 0 {
            return;
        }
        let Some(rect) = intersect(rect, full_rect(self.screen)) else {
            return;
        };
        if self
            .pending
            .iter()
            .any(|existing| contains(*existing, rect))
        {
            return;
        }
        self.pending.retain(|existing| !contains(rect, *existing));
        if self.pending.len() >= MAX_PENDING {
            self.invalidate_screen();
            return;
        }
        self.pending.push(rect);
    }

    /// Write `rect` of the screen into an XRGB8888 destination with `pitch`
    /// bytes per row. Returns the pixels written.
    pub fn copy_to_xrgb(&self, dst: &mut [u8], pitch: usize, rect: FineDamageRect) -> u64 {
        let Some(rect) = intersect(rect, full_rect(self.screen)) else {
            return 0;
        };
        let area = u64::from(rect.width) * u64::from(rect.height);
        let threads = if area >= PARALLEL_COPY_PIXELS {
            self.copy_threads
        } else {
            1
        };
        let first = rect.y as usize * pitch;
        let Some(band) = dst.get_mut(first..first + rect.height as usize * pitch) else {
            return 0;
        };
        let canvas = Canvas {
            bytes: &self.canvas,
            stride: self.width as usize * 4,
        };
        if threads <= 1 {
            canvas.copy_rows(band, pitch, rect, rect.y);
            return area;
        }
        let rows_per_thread = (rect.height as usize).div_ceil(threads);
        std::thread::scope(|scope| {
            for (index, rows) in band.chunks_mut(rows_per_thread * pitch).enumerate() {
                let y = rect.y + (index * rows_per_thread) as u32;
                scope.spawn(move || canvas.copy_rows(rows, pitch, rect, y));
            }
        });
        area
    }

    /// The whole screen as tightly packed RGBA, for outputs the scanout
    /// cannot take sparse copies for (several outputs, rotation, colour
    /// management).
    pub fn screen_rgba(&self) -> &[u8] {
        &self.canvas
    }

    /// Time a full-screen copy into cached memory, once, next to the copy
    /// into the scanout buffer, to tell whether writing the (write-combined)
    /// dumb buffer directly costs more than a staging copy would.
    pub fn measure_cached_copy_once(&mut self, dumb_full_copy: Duration) {
        if self.copy_measured {
            return;
        }
        self.copy_measured = true;
        let (width, height) = self.screen;
        let pitch = width as usize * 4;
        let mut staging = vec![0u8; pitch * height as usize];
        // Fault the staging pages in first, as a kept buffer would be.
        self.copy_to_xrgb(&mut staging, pitch, full_rect(self.screen));
        let started = Instant::now();
        self.copy_to_xrgb(&mut staging, pitch, full_rect(self.screen));
        let cached = started.elapsed();
        eprintln!(
            "tilo compositor: CPU scanout copy of {width}x{height}: dumb_buffer_us={} cached_staging_us={}",
            dumb_full_copy.as_micros(),
            cached.as_micros(),
        );
    }
}

/// Premultiplied RGBA over an opaque base is opaque: XRGB8888 is the same
/// pixel with red and blue swapped.
/// The canvas pixels, shareable with copy threads.
#[derive(Clone, Copy)]
struct Canvas<'a> {
    bytes: &'a [u8],
    stride: usize,
}

impl Canvas<'_> {
    /// Swizzle the columns of `rect` for the rows in `rows`, which start at
    /// screen row `y`.
    fn copy_rows(self, rows: &mut [u8], pitch: usize, rect: FineDamageRect, y: u32) {
        let width = rect.width as usize * 4;
        for (index, row) in rows.chunks_mut(pitch).enumerate() {
            let src_start = (y as usize + index) * self.stride + rect.x as usize * 4;
            let dst_start = rect.x as usize * 4;
            if let (Some(src), Some(out)) = (
                self.bytes.get(src_start..src_start + width),
                row.get_mut(dst_start..dst_start + width),
            ) {
                swizzle_row(src, out);
            }
        }
    }
}

fn swizzle_row(src: &[u8], dst: &mut [u8]) {
    // Whole words when both rows are word aligned, which the allocator and
    // every 4-byte pixel offset give; the loop then vectorizes.
    // SAFETY: any bit pattern is a valid u32.
    let (src_head, src_words, src_tail) = unsafe { src.align_to::<u32>() };
    // SAFETY: as above.
    let (dst_head, dst_words, dst_tail) = unsafe { dst.align_to_mut::<u32>() };
    if src_head.is_empty() && dst_head.is_empty() && src_tail.is_empty() && dst_tail.is_empty() {
        for (s, d) in src_words.iter().zip(dst_words.iter_mut()) {
            let pixel = u32::from_le(*s);
            *d = (0xFF00_0000 | ((pixel & 0xFF) << 16) | (pixel & 0xFF00) | ((pixel >> 16) & 0xFF))
                .to_le();
        }
        return;
    }
    for (s, d) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
        if let [red, green, blue, _] = *s {
            d.copy_from_slice(&[blue, green, red, 0xFF]);
        }
    }
}

fn full_rect((width, height): (u32, u32)) -> FineDamageRect {
    FineDamageRect {
        x: 0,
        y: 0,
        width,
        height,
    }
}

fn region_rect(region: Region) -> FineDamageRect {
    FineDamageRect {
        x: region.x,
        y: region.y,
        width: region.width,
        height: region.height,
    }
}

fn intersect(a: FineDamageRect, b: FineDamageRect) -> Option<FineDamageRect> {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = a.x.saturating_add(a.width).min(b.x.saturating_add(b.width));
    let y1 =
        a.y.saturating_add(a.height)
            .min(b.y.saturating_add(b.height));
    (x1 > x0 && y1 > y0).then(|| FineDamageRect {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
    })
}

fn right(rect: FineDamageRect) -> u32 {
    rect.x.saturating_add(rect.width)
}

fn bottom(rect: FineDamageRect) -> u32 {
    rect.y.saturating_add(rect.height)
}

fn contains(outer: FineDamageRect, inner: FineDamageRect) -> bool {
    outer.x <= inner.x
        && outer.y <= inner.y
        && right(outer) >= right(inner)
        && bottom(outer) >= bottom(inner)
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "fixtures index small, fixed buffers"
)]
mod tests {
    use super::*;
    use vello::kurbo::{Affine, Rect};

    fn rect(x: u32, y: u32, width: u32, height: u32) -> FineDamageRect {
        FineDamageRect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn bifrost_render_nodes_select_the_cpu_and_the_switch_wins() {
        let bifrost = vec!["rockchip,rk3568-mali,arm,mali-bifrost".to_owned()];
        assert!(select_cpu_render(None, &bifrost).is_some());
        assert!(select_cpu_render(Some("gpu"), &bifrost).is_none());
        assert!(select_cpu_render(Some("cpu"), &[String::new()]).is_some());
        let valhall = vec!["rockchip,rk3588-mali,arm,mali-valhall-csf".to_owned()];
        assert!(select_cpu_render(None, &valhall).is_none());
        let mixed = vec![bifrost[0].clone(), String::new()];
        assert!(
            select_cpu_render(None, &mixed).is_none(),
            "another GPU next to the Mali can still run vello"
        );
        assert!(select_cpu_render(None, &[]).is_none());
    }

    fn scene(width: f64, height: f64, color: Color, bar: Option<(Rect, Color)>) -> Scene {
        let mut scene = Scene::new();
        scene.fill(
            vello::peniko::Fill::NonZero,
            Affine::IDENTITY,
            color,
            None,
            &Rect::new(0.0, 0.0, width, height),
        );
        if let Some((rect, color)) = bar {
            scene.fill(
                vello::peniko::Fill::NonZero,
                Affine::IDENTITY,
                color,
                None,
                &rect,
            );
        }
        scene
    }

    fn pixel(renderer: &CpuRenderer, x: u32, y: u32) -> [u8; 4] {
        let pitch = renderer.screen.0 as usize * 4;
        let mut out = vec![0u8; pitch * renderer.screen.1 as usize];
        renderer.copy_to_xrgb(&mut out, pitch, full_rect(renderer.screen));
        let at = y as usize * pitch + x as usize * 4;
        [out[at], out[at + 1], out[at + 2], out[at + 3]]
    }

    #[test]
    fn damage_rasterizes_only_its_tiles_and_reports_them_as_screen_damage() {
        let mut renderer = CpuRenderer::new();
        let first = scene(64.0, 48.0, Color::from_rgb8(0, 0, 255), None);
        renderer.update(
            CpuFrame {
                scene: &first,
                revision: 1,
                previous_revision: None,
                regions: None,
            },
            (64, 48),
        );
        assert_eq!(renderer.stats().raster_px, 64 * 48);
        assert!(renderer.pending().is_none(), "a first frame is a full copy");
        renderer.mark_presented(1);

        let bar = Rect::new(16.0, 16.0, 32.0, 32.0);
        let second = scene(
            64.0,
            48.0,
            Color::from_rgb8(0, 0, 255),
            Some((bar, Color::from_rgb8(255, 0, 0))),
        );
        let regions = [rect(16, 16, 16, 16)];
        renderer.update(
            CpuFrame {
                scene: &second,
                revision: 2,
                previous_revision: Some(1),
                regions: Some(&regions),
            },
            (64, 48),
        );
        assert_eq!(renderer.stats().raster_px, 16 * 16);
        assert_eq!(renderer.pending(), Some(regions.as_slice()));
        // XRGB8888 little endian: blue, green, red, x.
        assert_eq!(pixel(&renderer, 20, 20), [0, 0, 255, 255]);
        assert_eq!(pixel(&renderer, 2, 2), [255, 0, 0, 255]);
    }

    #[test]
    fn without_shell_damage_the_changed_draws_decide() {
        let mut renderer = CpuRenderer::new();
        let blue = Color::from_rgb8(0, 0, 255);
        let first = scene(64.0, 64.0, blue, None);
        let unversioned = |scene| CpuFrame {
            scene,
            revision: 0,
            previous_revision: None,
            regions: None,
        };
        renderer.update(unversioned(&first), (64, 64));
        assert_eq!(renderer.stats().raster_px, 64 * 64);
        renderer.mark_presented(1);

        renderer.update(unversioned(&first), (64, 64));
        assert_eq!(renderer.stats().raster_px, 0, "the same scene again");
        assert!(!renderer.has_pending());

        let bar = Rect::new(36.0, 36.0, 44.0, 44.0);
        let second = scene(64.0, 64.0, blue, Some((bar, Color::from_rgb8(255, 0, 0))));
        renderer.update(unversioned(&second), (64, 64));
        assert_eq!(renderer.stats().fill, "diff");
        assert_eq!(
            renderer.stats().raster_px,
            16 * 16,
            "the tile under the new bar"
        );
        assert_eq!(pixel(&renderer, 40, 40), [0, 0, 255, 255]);
        assert_eq!(pixel(&renderer, 20, 20), [255, 0, 0, 255]);
    }

    #[test]
    fn the_scene_owns_its_ground_and_removed_draws_leave_transparent_pixels() {
        let mut renderer = CpuRenderer::new();
        let mut picture = Scene::new();
        picture.fill(
            vello::peniko::Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgb8(0, 0, 255),
            None,
            &Rect::new(16.0, 16.0, 32.0, 32.0),
        );
        renderer.update(
            CpuFrame {
                scene: &picture,
                revision: 1,
                previous_revision: None,
                regions: None,
            },
            (64, 48),
        );
        assert_eq!(&renderer.canvas[..4], &[0, 0, 0, 0]);
        assert_eq!(pixel(&renderer, 20, 20), [255, 0, 0, 255]);
        renderer.mark_presented(1);

        renderer.update(
            CpuFrame {
                scene: &Scene::new(),
                revision: 2,
                previous_revision: Some(1),
                regions: None,
            },
            (64, 48),
        );
        assert!(
            renderer.canvas.iter().all(|byte| *byte == 0),
            "removing a draw clears its retained pixels without inventing a ground"
        );
        assert_eq!(renderer.stats().fill, "diff");
        assert_eq!(renderer.stats().raster_px, 16 * 16);
    }

    #[test]
    fn rejected_publication_keeps_cached_pixels_pending_until_accepted() {
        let mut renderer = CpuRenderer::new();
        let picture = scene(64.0, 48.0, Color::from_rgb8(0, 0, 255), None);
        let frame = CpuFrame {
            scene: &picture,
            revision: 1,
            previous_revision: None,
            regions: None,
        };
        renderer.update(frame, (64, 48));
        let pixels = screen(&renderer);
        renderer.finish_present(1, false);
        assert!(renderer.has_pending());
        assert!(
            renderer.pending().is_none(),
            "retry the complete destination"
        );
        assert_eq!(renderer.presented_revision(), 0);
        assert!(!renderer.stats().presented);

        renderer.update(frame, (64, 48));
        assert_eq!(renderer.stats().raster_px, 0, "reuse the prepared canvas");
        assert_eq!(screen(&renderer), pixels);
        assert!(
            renderer.has_pending(),
            "an unchanged scene must retry its pixels"
        );
        renderer.finish_present(1, true);
        assert!(!renderer.has_pending());
        assert_eq!(renderer.presented_revision(), 1);
        assert!(renderer.stats().presented);
    }

    fn screen(renderer: &CpuRenderer) -> Vec<u8> {
        let pitch = renderer.screen.0 as usize * 4;
        let mut out = vec![0u8; pitch * renderer.screen.1 as usize];
        renderer.copy_to_xrgb(&mut out, pitch, full_rect(renderer.screen));
        out
    }

    #[test]
    fn a_pan_moves_canvas_pixels_instead_of_drawing_them() {
        let pan = |x: f64| {
            let mut scene = scene(256.0, 128.0, Color::from_rgb8(10, 10, 20), None);
            scene.fill(
                vello::peniko::Fill::NonZero,
                Affine::IDENTITY,
                Color::from_rgb8(60, 70, 80),
                None,
                &vello::kurbo::RoundedRect::new(x, 16.0, x + 128.0, 112.0, 8.0),
            );
            scene
        };
        let unversioned = |scene| CpuFrame {
            scene,
            revision: 0,
            previous_revision: None,
            regions: None,
        };
        let mut renderer = CpuRenderer::new();
        let (first, second) = (pan(32.0), pan(64.0));
        renderer.update(unversioned(&first), (256, 128));
        renderer.mark_presented(1);
        renderer.update(unversioned(&second), (256, 128));
        assert!(
            renderer.stats().moved_px >= 64 * 64,
            "{:?}",
            renderer.stats()
        );
        assert!(renderer.stats().raster_px < 256 * 128 / 2);

        let mut fresh = CpuRenderer::new();
        fresh.update(unversioned(&second), (256, 128));
        let (moved, drawn) = (screen(&renderer), screen(&fresh));
        let worst = moved
            .iter()
            .zip(&drawn)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        assert!(
            worst <= 1,
            "a moved frame equals a drawn one, off by {worst}"
        );
    }
}
