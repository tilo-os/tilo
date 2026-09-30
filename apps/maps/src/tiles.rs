//! Non-blocking OSM raster-tile layer.
//!
//! This is the heart of the map's smoothness. The UI thread must NEVER touch
//! the network or decode a PNG: both are slow and would stall the frame,
//! making zoom and pan feel laggy. Instead:
//!
//! * [`TileLayer::visible_tiles`] computes which `z/x/y` tiles cover the
//!   current viewport — pure arithmetic, runs every frame.
//! * For any tile that is not already decoded in memory, the layer sends a
//!   `TileRequest` over a channel to a background `worker` thread and
//!   *returns immediately*. The frame is drawn from whatever is cached.
//! * The worker checks the on-disk cache first, then fetches over HTTP and
//!   decodes the PNG to RGBA — all off the UI thread — and ships the decoded
//!   pixels back over a channel.
//! * [`TileLayer::drain`] folds finished tiles into the in-memory store
//!   between frames; the next frame swaps them in.
//!
//! Because zoom changes the integer tile-z, a freshly-zoomed view usually has
//! no exact-z tiles cached yet. To keep zoom feeling *instant*, draw scales an
//! already-cached ancestor (lower-z) tile under the hole as a placeholder, so
//! the user always sees imagery — sharp where cached, soft where pending —
//! never a blank frame.

use std::collections::HashMap;
use std::num::NonZeroUsize;

use lru::LruCache;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;
use ui::time::Instant;

use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{
    Blob, Color, Fill, ImageAlphaType, ImageBrush, ImageData as PenikoImage, ImageFormat as Format,
};

use ui::theme::Theme;

use crate::geo;
use crate::model::LatLon;

/// Slippy-map tile edge length in pixels (matches `geo::TILE_SIZE`).
pub const TILE_PX: f64 = 256.0;

/// How many decoded tiles we keep resident in RAM. Each tile is 256×256×4 ≈
/// 256 KiB, so 512 tiles ≈ 128 MiB worst case — comfortable, and far more
/// than any single viewport needs (a 4K screen covers < 150 tiles).
const MEM_CACHE_CAP: NonZeroUsize = match NonZeroUsize::new(512) {
    Some(cap) => cap,
    None => NonZeroUsize::MIN,
};

/// First retry delay for a tile whose fetch failed. The delay doubles on each
/// consecutive failure (see [`FAIL_BACKOFF_MAX`]).
const FAIL_BACKOFF_MIN: Duration = Duration::from_secs(2);
/// Ceiling for the failed-tile backoff. A tile server that is persistently
/// unreachable is retried at most this often, never per-frame.
const FAIL_BACKOFF_MAX: Duration = Duration::from_secs(300);
/// Cap on the negative-cache size so a long pan across an unreachable region
/// can't grow it without bound.
const FAIL_CACHE_CAP: usize = 4096;

/// Negative-cache entry for a tile whose fetch failed. It suppresses further
/// requests for that tile until `retry_at`, backing off exponentially so an
/// unreachable server is not hammered and the draw loop can idle instead of
/// re-requesting the same doomed tile every frame.
#[derive(Clone, Copy)]
struct FailedFetch {
    /// Earliest instant at which the tile may be requested again.
    retry_at: Instant,
    /// Current backoff window; doubles per failure up to [`FAIL_BACKOFF_MAX`].
    delay: Duration,
    /// Why the most recent fetch failed.
    fault: TileFault,
}

/// The OpenStreetMap Standard raster, used under the OSMF Tile Usage Policy
/// (<https://operations.osmfoundation.org/policies/tiles/>): an identifying
/// [`USER_AGENT`], attribution visible on the map, a persistent local cache, and
/// no bulk download or prefetch — so offline areas are refused
/// ([`TileLayer::allows_prefetch`]). The host must stay in the manifest's
/// `[sandbox] net` allowlist.
pub const TILE_SERVER: &str = "https://tile.openstreetmap.org";

/// The credit [`TILE_SERVER`]'s licence requires on the map.
pub const TILE_ATTRIBUTION: &str = "© OpenStreetMap contributors";

/// Cache subdirectory for [`TILE_SERVER`] under the style-agnostic cache root.
/// Earlier builds cached CARTO's `light`/`dark` trees beside it; those hold
/// "API key required" placeholders and are never read.
const TILE_SERVER_CACHE: &str = "osm";

/// The tone the raster is drawn in, resolved from the active OS [`Theme`] so the
/// basemap tracks it and a view is never a mix of light and dark. The server
/// has one (light) rendering; [`TileStyle::Dark`] re-tones those same tiles on
/// decode, so both styles share one download and one on-disk cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileStyle {
    Light,
    Dark,
}

impl TileStyle {
    /// Re-tone decoded RGBA8 tile pixels for this style in place. Dark inverts
    /// lightness while keeping hue (invert, then rotate hue by 180°) and softens
    /// contrast, so pale land turns deep, roads turn into light ribbons, and
    /// water and parks keep their blue and green.
    fn retone(self, rgba: &mut [u8]) {
        // CSS `hue-rotate(180deg)` in linear form.
        const HUE_180: [[f32; 3]; 3] = [
            [-0.574, 1.430, 0.144],
            [0.426, 0.430, 0.144],
            [0.426, 1.430, -0.856],
        ];
        const CONTRAST: f32 = 0.85;
        if self == TileStyle::Light {
            return;
        }
        for px in rgba.chunks_exact_mut(4) {
            let [r, g, b, _] = *px else { continue };
            let inv = [
                255.0 - f32::from(r),
                255.0 - f32::from(g),
                255.0 - f32::from(b),
            ];
            for (channel, row) in px.iter_mut().zip(HUE_180) {
                let rotated = row[0] * inv[0] + row[1] * inv[1] + row[2] * inv[2];
                let softened = (rotated - 128.0) * CONTRAST + 128.0;
                *channel = softened.round().clamp(0.0, 255.0) as u8;
            }
        }
    }

    /// Resolve the basemap style from the active theme by the lightness of its
    /// base surface — robust across every named/custom theme (and the two
    /// high-contrast tracks), not just the two built-ins.
    pub fn from_theme(theme: &Theme) -> Self {
        let c = theme.tokens.colors.surfaces.base.to_rgba8();
        let lum = 0.2126 * f64::from(c.r) + 0.7152 * f64::from(c.g) + 0.0722 * f64::from(c.b);
        if lum < 128.0 {
            TileStyle::Dark
        } else {
            TileStyle::Light
        }
    }
}

/// Identifies Tilo Maps to the tile server, as its usage policy requires; a
/// library default User-Agent is blocked.
pub(crate) const USER_AGENT: &str = concat!(
    "Tilo-Maps/",
    env!("CARGO_PKG_VERSION"),
    " (+https://code.tilo.so/tilo/tilo)"
);

/// Why a tile fetch failed, so the map can tell the user whether imagery is
/// unreachable or being refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileFault {
    /// No response: offline, egress blocked, or the connection failed.
    Unreachable,
    /// The server answered with an error status, a non-PNG body, or bytes that
    /// do not decode as a tile — imagery is being refused, not merely delayed.
    Rejected,
}

/// A tile coordinate in the slippy-map scheme.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileId {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

impl TileId {
    pub fn new(z: u8, x: u32, y: u32) -> Self {
        Self { z, x, y }
    }

    /// Number of tiles per axis at this zoom (`2^z`).
    fn n(z: u8) -> u32 {
        1u32 << z
    }

    /// The parent tile one zoom out that contains this one, if any.
    pub fn parent(&self) -> Option<TileId> {
        if self.z == 0 {
            None
        } else {
            Some(TileId::new(self.z - 1, self.x / 2, self.y / 2))
        }
    }

    /// Relative path under the cache root: `z/x/y.png`.
    fn cache_rel(&self) -> PathBuf {
        PathBuf::from(format!("{}/{}/{}.png", self.z, self.x, self.y))
    }

    /// [`Self::cache_rel`] inside the cache's [`tilo_files::Dir`].
    fn cache_vpath(&self) -> Option<tilo_files::VPath> {
        tilo_files::VPath::new(&format!("{}/{}/{}.png", self.z, self.x, self.y)).ok()
    }

    fn url(&self, base: &str) -> String {
        format!("{base}/{}/{}/{}.png", self.z, self.x, self.y)
    }
}

/// A decoded, GPU-ready tile image plus its source size.
#[derive(Clone)]
pub struct DecodedTile {
    pub image: PenikoImage,
}

/// Work item handed to the background worker.
enum Job {
    /// Fetch+decode for the live viewport (cache, then network).
    Fetch(TileId),
    /// Bulk download for offline use — same path, but counted for progress.
    Download(TileId),
}

/// Result shipped back from the worker.
enum Done {
    /// A tile finished: decoded RGBA pixels, or why it failed.
    Tile {
        id: TileId,
        rgba: Result<(Vec<u8>, u32, u32), TileFault>,
    },
    /// One bulk-download item finished (success flag for progress accounting).
    Download { ok: bool },
}

/// Snapshot of an in-progress offline download.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DownloadProgress {
    pub total: u32,
    pub done: u32,
    pub failed: u32,
}

impl DownloadProgress {
    pub fn is_active(&self) -> bool {
        self.total > 0 && self.done < self.total
    }

    pub fn fraction(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            f64::from(self.done) / f64::from(self.total)
        }
    }
}

/// The non-blocking tile layer owned by the app.
pub struct TileLayer {
    /// Decoded tiles resident in memory, keyed by `z/x/y`, least recently
    /// used evicted first.
    mem: LruCache<TileId, DecodedTile>,
    /// Tiles already requested and awaiting the worker, so we don't enqueue
    /// the same tile twice per frame.
    inflight: std::collections::HashSet<TileId>,
    /// Tiles whose most recent fetch failed, with an exponential backoff. A
    /// tile in here is not re-requested until its backoff elapses — this is
    /// what stops a doomed tile from being re-fetched every frame (which would
    /// both spin the CPU and hammer the public tile server).
    failed: HashMap<TileId, FailedFetch>,
    /// Channel to the worker; `None` in headless/offline mode (no worker).
    job_tx: Option<Sender<Job>>,
    /// Finished tiles + download ticks arrive here.
    done_rx: Option<Receiver<Done>>,
    /// Wakes the app when the worker delivers, so an in-flight fetch needs no
    /// frame loop polling for it.
    wake: ui::wake::WakeHandle,
    /// The active basemap style: the tone the worker decodes tiles into, so the
    /// resident tiles always match the theme.
    style: TileStyle,
    /// The tile server the worker fetches from.
    base_url: String,
    /// Whether the tile server's terms permit fetching tiles nobody is viewing
    /// (offline areas). [`TILE_SERVER`]'s do not.
    allows_prefetch: bool,
    /// The cache root this layer owns (the live app), which
    /// [`clear_cache`](Self::clear_cache) removes whole, including trees left
    /// by earlier tile servers. `None` for the hermetic test/offline layers,
    /// whose `cache_dir` is exactly as given.
    cache_root: Option<PathBuf>,
    /// On-disk cache of raw server tiles (`z/x/y.png` beneath it), shared by
    /// both styles.
    cache_dir: PathBuf,
    /// Live offline-download progress.
    progress: DownloadProgress,
}

impl TileLayer {
    /// Construct a live tile layer that spawns a worker thread. Its cache is
    /// style-scoped under the default root, so light and dark tiles never
    /// collide; the style is set to match the theme on the first `set_style`.
    pub fn new() -> Self {
        Self::live_with_root(default_cache_dir())
    }

    /// A live tile layer rooted at `root`, fetching from [`TILE_SERVER`] into
    /// `root/osm/z/x/y.png`. Starts in [`TileStyle::Light`]; the app calls
    /// [`set_style`](Self::set_style) with the theme's style each draw, which
    /// re-points the worker's decode on a change.
    pub fn live_with_root(root: PathBuf) -> Self {
        let mut layer = Self::spawn_worker(root.join(TILE_SERVER_CACHE), TILE_SERVER.to_string());
        layer.cache_root = Some(root);
        layer
    }

    /// Construct a live tile layer rooted at exactly `cache_dir` (no style
    /// subdir). The worker runs off the UI thread. Used by the tile unit tests
    /// to point the cache at a temp dir and drive the raw fetch path.
    pub fn with_cache_dir(cache_dir: PathBuf) -> Self {
        Self::with_cache_dir_and_base(cache_dir, TILE_SERVER.to_string())
    }

    /// Live tile layer at an explicit dir with an explicit tile-server base URL.
    /// A custom base lets a test exercise the real fetch→fail→negative-cache
    /// path against a local (closed-port) server without touching the public one.
    fn with_cache_dir_and_base(cache_dir: PathBuf, base_url: String) -> Self {
        Self::spawn_worker(cache_dir, base_url)
    }

    /// Spawn the background worker for `cache_dir` + `base_url` and wrap it in a
    /// fresh layer (style-agnostic root left `None`; the caller sets it).
    fn spawn_worker(cache_dir: PathBuf, base_url: String) -> Self {
        let mut layer = Self::offline_with_cache_dir(cache_dir);
        layer.base_url = base_url;
        layer.respawn_worker();
        layer
    }

    /// (Re)start the background worker for the current cache dir, server and
    /// style. The previous worker exits when its channel drops, discarding any
    /// in-flight result with it. A spawn failure (OS resource exhaustion)
    /// degrades to no worker rather than crashing the app — the layer then
    /// behaves like an offline one, serving whatever is already resident.
    fn respawn_worker(&mut self) {
        let (job_tx, job_rx) = channel::<Job>();
        let (done_tx, done_rx) = channel::<Done>();
        let dir = self.cache_dir.clone();
        let base_url = self.base_url.clone();
        let style = self.style;
        let wake = self.wake.clone();
        let spawned = ui::rt::spawn_thread("maps-tiles", move || {
            worker(&job_rx, &done_tx, &wake, &dir, &base_url, style);
        })
        .is_ok();
        self.job_tx = spawned.then_some(job_tx);
        self.done_rx = spawned.then_some(done_rx);
    }

    /// Point the layer at the basemap `style` for the active theme. A no-op when
    /// unchanged; on a change it drops the now-wrong-tone resident tiles and
    /// re-spawns the worker to decode in the new tone (the raw tiles on disk
    /// serve both styles, so nothing is downloaded again). Offline layers just
    /// record the style. Cheap on the steady state, so the app calls it each draw.
    pub fn set_style(&mut self, style: TileStyle) {
        if self.style == style {
            return;
        }
        self.style = style;
        // Wrong-tone imagery must not linger through the switch.
        self.mem.clear();
        self.inflight.clear();
        self.failed.clear();
        // Live layers only; offline layers have no `job_tx` and simply keep
        // serving from the — now cleared — set.
        if self.job_tx.is_some() {
            self.respawn_worker();
        }
    }

    /// The active basemap style.
    pub fn style(&self) -> TileStyle {
        self.style
    }

    /// Construct an inert layer with no worker thread and no network — used by
    /// `--shots` and tests so nothing ever blocks or hits the wire.
    pub fn offline() -> Self {
        Self::offline_with_cache_dir(default_cache_dir())
    }

    /// An inert (no-worker, no-network) layer rooted at an explicit cache
    /// directory. Lets a cache-management test drive `clear_cache` against a
    /// disposable temp dir instead of the user's real default cache.
    pub fn offline_with_cache_dir(cache_dir: PathBuf) -> Self {
        Self {
            mem: LruCache::new(MEM_CACHE_CAP),
            inflight: std::collections::HashSet::new(),
            failed: HashMap::new(),
            job_tx: None,
            done_rx: None,
            wake: ui::wake::WakeHandle::new(),
            style: TileStyle::Light,
            base_url: TILE_SERVER.to_string(),
            allows_prefetch: false,
            cache_root: None,
            cache_dir,
            progress: DownloadProgress::default(),
        }
    }

    pub fn cache_dir(&self) -> &std::path::Path {
        &self.cache_dir
    }

    pub fn progress(&self) -> DownloadProgress {
        self.progress
    }

    /// Number of decoded tiles currently resident in RAM.
    pub fn mem_len(&self) -> usize {
        self.mem.len()
    }

    /// Integer zoom used for tile selection at a fractional map zoom. Tiles
    /// exist at integer z; we round to the nearest so detail tracks the view.
    pub fn tile_zoom(map_zoom: f64) -> u8 {
        map_zoom.round().clamp(0.0, 19.0) as u8
    }

    /// Compute the `TileId`s whose footprint intersects `viewport` for the
    /// given camera. Pure arithmetic — safe to call every frame.
    pub fn visible_tiles(
        center: LatLon,
        map_zoom: f64,
        view_center: vello::kurbo::Point,
        viewport: Rect,
    ) -> Vec<(TileId, Rect)> {
        let z = Self::tile_zoom(map_zoom);
        // Scale between the integer tile pyramid and the fractional view.
        let scale = 2f64.powf(map_zoom - f64::from(z));
        let tile_screen = TILE_PX * scale;
        let n = TileId::n(z);

        // World-pixel position of the viewport centre at integer zoom z.
        let (cwx, cwy) = geo::lonlat_to_world(center, f64::from(z));
        // Top-left of the viewport in world-pixel space (scaled).
        let left_world = cwx - (view_center.x - viewport.x0) / scale;
        let top_world = cwy - (view_center.y - viewport.y0) / scale;

        let first_x = (left_world / TILE_PX).floor() as i64;
        let first_y = (top_world / TILE_PX).floor() as i64;
        let cols = (viewport.width() / tile_screen).ceil() as i64 + 2;
        let rows = (viewport.height() / tile_screen).ceil() as i64 + 2;

        let mut out = Vec::with_capacity((cols * rows).max(0) as usize);
        for ty in first_y..first_y + rows {
            for tx in first_x..first_x + cols {
                // Wrap X around the antimeridian; clamp Y (no vertical wrap).
                if ty < 0 || ty >= i64::from(n) {
                    continue;
                }
                let wx = tx.rem_euclid(i64::from(n)) as u32;
                let id = TileId::new(z, wx, ty as u32);
                // Screen rect of this tile: its world origin minus camera, /scale.
                let world_x = tx as f64 * TILE_PX;
                let world_y = ty as f64 * TILE_PX;
                let sx = view_center.x + (world_x - cwx) * scale;
                let sy = view_center.y + (world_y - cwy) * scale;
                let rect = Rect::new(sx, sy, sx + tile_screen, sy + tile_screen);
                out.push((id, rect));
            }
        }
        out
    }

    /// Fold any finished tiles/download ticks into the store. Call once per
    /// frame (cheap, non-blocking `try_recv`).
    pub fn drain(&mut self) {
        // Collect first so we don't hold an immutable borrow of `done_rx`
        // while mutating `self` via `insert`.
        let mut finished: Vec<Done> = Vec::new();
        if let Some(rx) = &self.done_rx {
            while let Ok(msg) = rx.try_recv() {
                finished.push(msg);
            }
        }
        for msg in finished {
            match msg {
                Done::Tile { id, rgba } => {
                    self.inflight.remove(&id);
                    match rgba {
                        Ok((rgba, w, h)) => {
                            // A successful fetch clears any negative-cache entry
                            // so a recovered tile loads normally next time.
                            self.failed.remove(&id);
                            if let Some(img) = make_image(rgba, w, h) {
                                self.insert(id, DecodedTile { image: img });
                            }
                        }
                        Err(fault) => self.note_failure(id, fault),
                    }
                }
                Done::Download { ok } => {
                    self.progress.done += 1;
                    if !ok {
                        self.progress.failed += 1;
                    }
                }
            }
        }
    }

    /// Look up a decoded tile, marking it most-recently-used.
    pub fn get(&mut self, id: TileId) -> Option<&PenikoImage> {
        self.mem.get(&id).map(|t| &t.image)
    }

    /// Has this tile been requested but not yet returned?
    pub fn is_inflight(&self, id: TileId) -> bool {
        self.inflight.contains(&id)
    }

    /// Are any tiles currently awaiting the worker?
    pub fn has_inflight(&self) -> bool {
        !self.inflight.is_empty()
    }

    /// Whether at least one recent tile fetch failed and is backing off. Lets
    /// the map distinguish "still loading imagery" from "imagery unreachable"
    /// (egress blocked / offline) for the loading affordance.
    pub fn has_failures(&self) -> bool {
        !self.failed.is_empty()
    }

    /// The most telling fault among the failing tiles: [`TileFault::Rejected`]
    /// when the server refused any of them, so a refusing server is never
    /// reported as the user merely being offline.
    pub fn failure(&self) -> Option<TileFault> {
        let mut faults = self.failed.values().map(|f| f.fault);
        let first = faults.next()?;
        Some(
            if first == TileFault::Rejected || faults.any(|f| f == TileFault::Rejected) {
                TileFault::Rejected
            } else {
                TileFault::Unreachable
            },
        )
    }

    /// Whether the tile server permits fetching tiles nobody is viewing, which
    /// offline areas need. [`TILE_SERVER`]'s policy forbids it, so the app
    /// refuses to save areas rather than scrape the community server.
    pub fn allows_prefetch(&self) -> bool {
        self.allows_prefetch
    }

    /// Request `id` from the worker if it isn't already resident, inflight, or
    /// suppressed by the failure backoff. Returns immediately — the UI thread
    /// never blocks on I/O or decode.
    pub fn request(&mut self, id: TileId) {
        if self.mem.contains(&id) || self.inflight.contains(&id) {
            return;
        }
        // Negative cache: a tile whose fetch recently failed is not requested
        // again until its backoff elapses. Without this, `draw_tiles` would
        // re-request every non-resident tile every frame, so an unfetchable
        // tile would spin the loop (it never idles while `has_inflight`) and
        // hammer the tile server. The entry is kept until a later fetch
        // succeeds, so repeated failures keep doubling the delay.
        if let Some(f) = self.failed.get(&id)
            && ui::time::now() < f.retry_at
        {
            return;
        }
        if let Some(tx) = &self.job_tx
            && tx.send(Job::Fetch(id)).is_ok()
        {
            self.inflight.insert(id);
        }
    }

    /// Record a failed fetch, extending this tile's backoff. The first failure
    /// waits [`FAIL_BACKOFF_MIN`]; each consecutive failure doubles the window
    /// up to [`FAIL_BACKOFF_MAX`].
    fn note_failure(&mut self, id: TileId, fault: TileFault) {
        let now = ui::time::now();
        let delay = match self.failed.get(&id) {
            Some(f) => (f.delay * 2).min(FAIL_BACKOFF_MAX),
            None => FAIL_BACKOFF_MIN,
        };
        self.failed.insert(
            id,
            FailedFetch {
                retry_at: now + delay,
                delay,
                fault,
            },
        );
        if self.failed.len() > FAIL_CACHE_CAP {
            // Entries whose backoff already elapsed no longer suppress a
            // request, so dropping them keeps the map bounded without causing a
            // re-request storm (only visible tiles are ever requested).
            self.failed.retain(|_, f| f.retry_at > now);
            if self.failed.len() > FAIL_CACHE_CAP {
                self.failed.clear();
            }
        }
    }

    /// Whether any already-cached tile lies within `max_depth` zoom levels
    /// *below* `parent` (i.e. a descendant/child tile). Read-only — used to
    /// decide whether a zoom-out hole can be filled from sharper child tiles
    /// that were resident at the previous, deeper zoom.
    pub fn has_descendants(&self, parent: TileId, max_depth: u8) -> bool {
        for depth in 1..=max_depth {
            let cz = u32::from(parent.z) + u32::from(depth);
            if cz > 19 {
                break;
            }
            let cz = cz as u8;
            let span = 1u32 << depth;
            let base_x = parent.x << depth;
            let base_y = parent.y << depth;
            for j in 0..span {
                for i in 0..span {
                    if self.mem.contains(&TileId::new(cz, base_x + i, base_y + j)) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Find the nearest already-cached ancestor tile that covers `id`, for use
    /// as an instant zoom/pan placeholder. Returns the ancestor and how many
    /// zoom levels up it is (so the caller can sub-sample its pixels).
    pub fn ancestor_placeholder(&mut self, id: TileId) -> Option<(TileId, u8)> {
        let mut cur = id;
        let mut up = 0u8;
        while let Some(p) = cur.parent() {
            up += 1;
            if self.mem.promote(&p) {
                return Some((p, up));
            }
            cur = p;
            if up > 8 {
                break;
            }
        }
        None
    }

    /// Begin downloading every tile covering `region` across `z0..=z1` into the
    /// on-disk cache. Non-blocking: the worker streams progress back via
    /// `drain`. Returns the number of tiles queued — none when the tile server
    /// forbids prefetching ([`allows_prefetch`](Self::allows_prefetch)).
    pub fn start_download(&mut self, region: GeoBounds, z0: u8, z1: u8) -> u32 {
        if !self.allows_prefetch {
            return 0;
        }
        let ids = region.tiles_in_range(z0, z1);
        let total = ids.len() as u32;
        self.progress = DownloadProgress {
            total,
            done: 0,
            failed: 0,
        };
        if let Some(tx) = &self.job_tx {
            for id in ids {
                let _ = tx.send(Job::Download(id));
            }
        } else {
            // Offline layer: nothing to do, mark complete so the UI settles.
            self.progress.done = total;
        }
        total
    }

    /// Estimate how many tiles a download of `region` across `z0..=z1` covers,
    /// without enqueuing anything — for the pre-download confirmation.
    pub fn estimate_download(region: GeoBounds, z0: u8, z1: u8) -> u32 {
        region.tiles_in_range(z0, z1).len() as u32
    }

    /// Delete the entire on-disk tile cache and clear RAM. Returns bytes freed.
    /// For the live layer this is the whole style-agnostic root (both light and
    /// dark tiles); for a test/offline layer it is exactly its `cache_dir`.
    pub fn clear_cache(&mut self) -> u64 {
        self.mem.clear();
        self.inflight.clear();
        self.failed.clear();
        self.progress = DownloadProgress::default();
        let target = self
            .cache_root
            .clone()
            .unwrap_or_else(|| self.cache_dir.clone());
        let freed = dir_size(&target);
        if let Ok((parent, name)) = tilo_files::file_at(&target) {
            let _ = tilo_files::blocking::remove_all(&parent, &name);
        }
        freed
    }

    /// Delete specific tiles from RAM and the on-disk cache — used to remove a
    /// downloaded offline region. Returns the number of bytes freed on disk.
    /// The caller is responsible for excluding any tile still covered by a
    /// region it means to keep, so shared imagery isn't dropped.
    pub fn delete_tiles(&mut self, ids: &[TileId]) -> u64 {
        let mut freed = 0u64;
        let cache = cache_files(&self.cache_dir);
        for id in ids {
            self.mem.pop(id);
            let (Some(cache), Some(path)) = (&cache, id.cache_vpath()) else {
                continue;
            };
            if let Ok(meta) = tilo_files::blocking::metadata(cache, &path) {
                freed += meta.len;
            }
            let _ = tilo_files::blocking::remove(cache, &path);
        }
        freed
    }

    /// Total bytes the on-disk cache currently occupies (all styles, for the
    /// live layer; exactly `cache_dir` for a test/offline layer).
    pub fn cache_bytes(&self) -> u64 {
        dir_size(self.cache_root.as_deref().unwrap_or(&self.cache_dir))
    }

    // --- internal ----------------------------------------------------------
    fn insert(&mut self, id: TileId, tile: DecodedTile) {
        self.mem.put(id, tile);
    }

    // --- test hooks --------------------------------------------------------
    /// A live layer whose worker fetches from `base_url` instead of the public
    /// OSM server — so a test can drive the real fetch→fail→negative-cache path
    /// against a local (closed-port) endpoint without any external traffic.
    /// The local server is the test's own, so it permits prefetching.
    #[cfg(test)]
    fn with_cache_dir_url(cache_dir: PathBuf, base_url: String) -> Self {
        let mut layer = Self::with_cache_dir_and_base(cache_dir, base_url);
        layer.allows_prefetch = true;
        layer
    }

    /// Stand in for a tile server whose terms permit offline areas, so the
    /// offline-area flow can be exercised end to end.
    #[cfg(test)]
    pub(crate) fn allow_prefetch_for_test(&mut self) {
        self.allows_prefetch = true;
    }

    /// The on-disk path a tile would occupy under this layer's cache root.
    /// Crate-visible so an app-level test can seed/inspect cached tiles by id
    /// (the `z/x/y.png` layout is otherwise private).
    #[cfg(test)]
    pub(crate) fn cache_path(&self, id: TileId) -> PathBuf {
        self.cache_dir.join(id.cache_rel())
    }

    /// Seed a solid 256×256 decoded tile straight into the resident set, for
    /// tests that need cached imagery without a worker or the network (e.g. the
    /// canvas zoom-reuse test). Crate-visible so `canvas`'s tests can use it.
    #[cfg(test)]
    pub(crate) fn insert_solid_tile(&mut self, id: TileId) {
        let img = make_image(vec![80u8; 256 * 256 * 4], 256, 256).expect("solid tile");
        self.insert(id, DecodedTile { image: img });
    }

    /// The current backoff window recorded for a failed tile, if any.
    #[cfg(test)]
    fn failed_delay(&self, id: TileId) -> Option<Duration> {
        self.failed.get(&id).map(|f| f.delay)
    }

    /// Whether a tile is currently suppressed by the failure backoff.
    #[cfg(test)]
    fn is_backing_off(&self, id: TileId) -> bool {
        self.failed
            .get(&id)
            .is_some_and(|f| Instant::now() < f.retry_at)
    }
}

impl Default for TileLayer {
    fn default() -> Self {
        Self::offline()
    }
}

/// A lat/lon bounding box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeoBounds {
    pub min: LatLon,
    pub max: LatLon,
}

impl GeoBounds {
    pub fn new(a: LatLon, b: LatLon) -> Self {
        Self {
            min: LatLon::new(a.lat.min(b.lat), a.lon.min(b.lon)),
            max: LatLon::new(a.lat.max(b.lat), a.lon.max(b.lon)),
        }
    }

    /// Every tile (across `z0..=z1`) whose footprint intersects the box.
    pub fn tiles_in_range(&self, z0: u8, z1: u8) -> Vec<TileId> {
        let (z0, z1) = (z0.min(z1), z0.max(z1).min(19));
        let mut out = Vec::new();
        for z in z0..=z1 {
            let n = TileId::n(z);
            let (x0, y0) = lonlat_to_tile(self.max.lat, self.min.lon, z);
            let (x1, y1) = lonlat_to_tile(self.min.lat, self.max.lon, z);
            let (xa, xb) = (x0.min(x1), x0.max(x1));
            let (ya, yb) = (y0.min(y1), y0.max(y1));
            for y in ya..=yb.min(n.saturating_sub(1)) {
                for x in xa..=xb.min(n.saturating_sub(1)) {
                    out.push(TileId::new(z, x, y));
                }
            }
        }
        out
    }
}

/// Tile column/row containing a coordinate at integer zoom `z`.
fn lonlat_to_tile(lat: f64, lon: f64, z: u8) -> (u32, u32) {
    let n = f64::from(TileId::n(z));
    let x = ((lon + 180.0) / 360.0 * n).floor();
    let lat_rad = lat.clamp(-85.051_128_78, 85.051_128_78).to_radians();
    let y = ((1.0 - (lat_rad.tan() + 1.0 / lat_rad.cos()).ln() / std::f64::consts::PI) / 2.0 * n)
        .floor();
    let nm = TileId::n(z).saturating_sub(1);
    (
        (x as i64).clamp(0, i64::from(nm)) as u32,
        (y as i64).clamp(0, i64::from(nm)) as u32,
    )
}

/// Background worker: services fetch + download jobs entirely off the UI
/// thread. Disk cache first, then network; decode happens here too.
fn worker(
    rx: &Receiver<Job>,
    tx: &Sender<Done>,
    wake: &ui::wake::WakeHandle,
    cache_dir: &std::path::Path,
    base_url: &str,
    style: TileStyle,
) {
    let client = match tilo_net::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!("maps tile HTTP client failed to build: {e}");
            None
        }
    };
    while let Ok(job) = rx.recv() {
        match job {
            Job::Fetch(id) => {
                let rgba =
                    load_tile(client.as_ref(), cache_dir, base_url, id).map(|(mut rgba, w, h)| {
                        style.retone(&mut rgba);
                        (rgba, w, h)
                    });
                if tx.send(Done::Tile { id, rgba }).is_err() {
                    break;
                }
                wake.wake();
            }
            Job::Download(id) => {
                // For downloads we only need the bytes on disk; decoding is the
                // live path's concern. Skip if already cached.
                let path = cache_dir.join(id.cache_rel());
                let ok = if path.exists() {
                    true
                } else {
                    fetch_to_disk(client.as_ref(), cache_dir, base_url, id)
                };
                if tx.send(Done::Download { ok }).is_err() {
                    break;
                }
                wake.wake();
            }
        }
    }
}

/// Load a tile's RGBA pixels: disk cache → network. Writes fetched bytes to
/// disk so re-views and zoom are instant.
fn load_tile(
    client: Option<&tilo_net::Client>,
    cache_dir: &std::path::Path,
    base_url: &str,
    id: TileId,
) -> Result<(Vec<u8>, u32, u32), TileFault> {
    let cache = cache_files(cache_dir).zip(id.cache_vpath());
    if let Some((dir, path)) = &cache
        && let Ok(bytes) = tilo_files::blocking::read(dir, path)
        && let Ok(t) = decode_png(&bytes)
    {
        return Ok(t);
    }
    let bytes = fetch_tile(client, base_url, id)?;
    let decoded = decode_png(&bytes).map_err(|e| {
        tracing::warn!("maps tile {id:?} does not decode as PNG: {e}");
        TileFault::Rejected
    })?;
    if let Some((dir, path)) = &cache {
        write_cache(dir, path, &bytes);
    }
    Ok(decoded)
}

/// Fetch a tile to the on-disk cache only (no decode) — for bulk downloads.
fn fetch_to_disk(
    client: Option<&tilo_net::Client>,
    cache_dir: &std::path::Path,
    base_url: &str,
    id: TileId,
) -> bool {
    match fetch_tile(client, base_url, id) {
        Ok(bytes) if decode_png(&bytes).is_ok() => {
            if let (Some(dir), Some(path)) = (cache_files(cache_dir), id.cache_vpath()) {
                write_cache(&dir, &path, &bytes);
            }
            true
        }
        _ => false,
    }
}

/// GET one tile's bytes, accepting only a successful `image/png` answer: an
/// error status or an HTML/JSON error page is the server refusing imagery.
fn fetch_tile(
    client: Option<&tilo_net::Client>,
    base_url: &str,
    id: TileId,
) -> Result<Vec<u8>, TileFault> {
    let client = client.ok_or(TileFault::Unreachable)?;
    // The worker thread owns no runtime; each tile waits on the app's.
    ui::rt::app()
        .block_on(async {
            let resp = client
                .get(id.url(base_url))
                .send()
                .await
                .map_err(|_| TileFault::Unreachable)?;
            let status = resp.status();
            if !status.is_success() {
                tracing::warn!("maps tile server answered {status} for {id:?}");
                return Err(TileFault::Rejected);
            }
            let content_type = resp
                .headers()
                .get(tilo_net::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            if !content_type.starts_with("image/png") {
                tracing::warn!("maps tile server sent {content_type:?} for {id:?}, not a PNG tile");
                return Err(TileFault::Rejected);
            }
            resp.bytes()
                .await
                .map(|bytes| bytes.to_vec())
                .map_err(|_| TileFault::Unreachable)
        })
        .map_err(|_| TileFault::Unreachable)?
}

/// The on-disk tile cache rooted at `root`.
fn cache_files(root: &std::path::Path) -> Option<tilo_files::Dir> {
    tilo_files::Dir::open(tilo_files::Location::Native(root.to_path_buf())).ok()
}

/// A fetched tile is only a cache entry: it is written in place, without the
/// cost of a crash-safe replacement, and fetched again if a crash tore it.
fn write_cache(dir: &tilo_files::Dir, path: &tilo_files::VPath, bytes: &[u8]) {
    let _ = tilo_files::blocking::write(dir, path, bytes);
}

/// Wrap RGBA bytes in a `peniko::ImageData` for painting.
fn make_image(rgba: Vec<u8>, w: u32, h: u32) -> Option<PenikoImage> {
    if rgba.len() != (w as usize) * (h as usize) * 4 {
        return None;
    }
    Some(PenikoImage {
        data: Blob::new(Arc::new(rgba)),
        format: Format::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width: w,
        height: h,
    })
}

/// Decode a PNG to RGBA8. Errors are stringly-typed; the caller only cares
/// success vs. failure. OSM's standard tiles are 8-bit *palette* (indexed)
/// PNGs; palette, `tRNS`, sub-8-bit and 16-bit sources all normalise to RGBA8.
fn decode_png(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    let decoded = ui::png::decode_rgba8(bytes).map_err(|e| e.to_string())?;
    Ok((decoded.rgba, decoded.width, decoded.height))
}

/// Default on-disk cache root. Honours `XDG_CACHE_HOME`, falling back to a
/// temp dir. The page has neither: its root is a bare relative path that
/// tilo-files refuses there, so tiles stay in memory.
fn default_cache_dir() -> PathBuf {
    tilo_paths::xdg::cache_home()
        .or_else(tilo_paths::temp_dir)
        .unwrap_or_default()
        .join("tilo-maps/tiles")
}

/// Recursively sum file sizes under `dir` (bytes). Cheap enough for the
/// manage-cache view; the cache is a shallow `z/x/y` tree.
fn dir_size(dir: &std::path::Path) -> u64 {
    cache_files(dir).map_or(0, |files| tree_size(&files, &tilo_files::VPath::root()))
}

/// Bytes of the files below `path`; a symbolic link is not followed.
fn tree_size(files: &tilo_files::Dir, path: &tilo_files::VPath) -> u64 {
    let Ok(entries) = tilo_files::blocking::list(files, path) else {
        return 0;
    };
    let mut total = 0;
    for entry in entries {
        let Ok(name) = tilo_files::VPath::new(&entry.name) else {
            continue;
        };
        let child = path.join(&name);
        let Ok(meta) = tilo_files::blocking::metadata(files, &child) else {
            continue;
        };
        if meta.symlink {
            continue;
        }
        if meta.kind == tilo_files::Kind::Dir {
            total += tree_size(files, &child);
        } else {
            total += meta.len;
        }
    }
    total
}

/// Paint one decoded tile into `dst` (a screen rect). The tile is scaled to
/// exactly fill `dst` via a brush transform — same idiom as the `ui` image
/// widget. `opacity < 1.0` is supported via a push/pop layer.
pub fn paint_tile(scene: &mut Scene, transform: Affine, img: &PenikoImage, dst: Rect) {
    if dst.width() <= 0.0 || dst.height() <= 0.0 {
        return;
    }
    let iw = f64::from(img.width.max(1));
    let ih = f64::from(img.height.max(1));
    let sx = dst.width() / iw;
    let sy = dst.height() / ih;
    let brush_transform = Affine::translate((dst.x0, dst.y0)) * Affine::scale_non_uniform(sx, sy);
    let brush = ImageBrush::new(img.clone());
    scene.fill(
        Fill::NonZero,
        transform,
        &brush,
        Some(brush_transform),
        &dst,
    );
}

/// Paint only the sub-quadrant of an ancestor `img` that corresponds to the
/// descendant tile `levels` zoom-steps below it, stretched across `dst`. This
/// is how a lower-z tile fills a higher-z hole as a crisp-ish placeholder.
pub fn paint_ancestor(
    scene: &mut Scene,
    transform: Affine,
    img: &PenikoImage,
    ancestor: TileId,
    child: TileId,
    levels: u8,
    dst: Rect,
) {
    if dst.width() <= 0.0 || dst.height() <= 0.0 || levels == 0 {
        paint_tile(scene, transform, img, dst);
        return;
    }
    let scale = f64::from(1u32 << levels);
    // Which sub-cell of the ancestor does `child` fall into?
    let span = 1u32 << levels;
    let cell_x = child.x - ancestor.x * span;
    let cell_y = child.y - ancestor.y * span;
    let src_w = f64::from(img.width) / scale;
    let src_h = f64::from(img.height) / scale;
    let src_x = f64::from(cell_x) * src_w;
    let src_y = f64::from(cell_y) * src_h;

    // Brush transform maps dst-space → image-space: place src origin at dst.
    let sx = dst.width() / src_w;
    let sy = dst.height() / src_h;
    let brush_transform = Affine::translate((dst.x0, dst.y0))
        * Affine::scale_non_uniform(sx, sy)
        * Affine::translate((-src_x, -src_y));
    let brush = ImageBrush::new(img.clone());
    // The fill geometry is `dst`, so only pixels inside the tile rect are
    // painted — no clip layer is needed to bound the stretched ancestor. A
    // fresh zoom-in fills *every* visible tile from an ancestor, so dropping the
    // per-tile push/pop_layer removes a clip layer per tile from the hottest
    // frames (the whole tile pass is already clipped to the viewport upstream).
    scene.fill(
        Fill::NonZero,
        transform,
        &brush,
        Some(brush_transform),
        &dst,
    );
}

/// Fill a zoom-out hole from *cached descendant* (higher-z, sharper) tiles.
///
/// When the view zooms out, the target integer-z tiles are usually not cached
/// yet, and [`TileLayer::ancestor_placeholder`] can't help (there is no cached
/// lower-z tile). But the child tiles that were resident at the previous,
/// deeper zoom still cover `dst` — each child maps to a sub-cell of `parent`.
/// Painting those children into their sub-rects keeps the map showing crisp
/// imagery through the zoom-out instead of blanking to the land colour.
///
/// Tries depth 1 (the 2×2 direct children) first; if none of those are cached
/// it falls back to depth 2 (the 4×4 grandchildren), covering a two-step zoom
/// out. Returns whether it painted anything.
pub fn paint_descendants(
    scene: &mut Scene,
    transform: Affine,
    layer: &mut TileLayer,
    parent: TileId,
    dst: Rect,
    max_depth: u8,
) -> bool {
    if dst.width() <= 0.0 || dst.height() <= 0.0 {
        return false;
    }
    for depth in 1..=max_depth {
        let cz = u32::from(parent.z) + u32::from(depth);
        if cz > 19 {
            break;
        }
        let cz = cz as u8;
        let span = 1u32 << depth; // children per axis at this depth
        let cw = dst.width() / f64::from(span);
        let ch = dst.height() / f64::from(span);
        let base_x = parent.x << depth;
        let base_y = parent.y << depth;
        let mut painted = false;
        for j in 0..span {
            for i in 0..span {
                let cid = TileId::new(cz, base_x + i, base_y + j);
                if let Some(img) = layer.get(cid).cloned() {
                    let sx = dst.x0 + f64::from(i) * cw;
                    let sy = dst.y0 + f64::from(j) * ch;
                    paint_tile(scene, transform, &img, Rect::new(sx, sy, sx + cw, sy + ch));
                    painted = true;
                }
            }
        }
        // Once a depth yields coverage, stop — deeper children would only
        // overpaint the same region more expensively.
        if painted {
            return true;
        }
    }
    false
}

/// A flat fill used under pending tiles so a never-seen area shows the map
/// land colour instead of a transparent hole.
pub fn paint_placeholder(scene: &mut Scene, transform: Affine, dst: Rect, color: Color) {
    scene.fill(Fill::NonZero, transform, color, None, &dst);
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::kurbo::Point;

    /// Tile addressing (zoom, parent, visible set, bounds), indexed-PNG decoding, and
    /// the label-free, theme-tracking basemap style.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn tile_addressing_decoding_and_label_free_styles() {
        // Phase `tile_zoom_rounds_and_clamps`.
        {
            assert_eq!(TileLayer::tile_zoom(14.4), 14);
            assert_eq!(TileLayer::tile_zoom(14.6), 15);
            assert_eq!(TileLayer::tile_zoom(-3.0), 0);
            assert_eq!(TileLayer::tile_zoom(99.0), 19);
        }

        // Phase `parent_walks_up_the_pyramid`.
        {
            let t = TileId::new(14, 8800, 5680);
            let p = t.parent().unwrap();
            assert_eq!(p, TileId::new(13, 4400, 2840));
            assert!(TileId::new(0, 0, 0).parent().is_none());
        }

        // Phase `visible_tiles_cover_viewport_and_are_in_range`.
        {
            let center = LatLon::new(48.1371, 11.5754);
            let vp = Rect::new(0.0, 0.0, 1280.0, 800.0);
            let vc = Point::new(640.0, 400.0);
            let tiles = TileLayer::visible_tiles(center, 14.0, vc, vp);
            assert!(!tiles.is_empty());
            // A 1280×800 viewport needs at least a 6×4 tile block.
            assert!(tiles.len() >= 20, "got {}", tiles.len());
            let n = 1u32 << 14;
            for (id, rect) in &tiles {
                assert_eq!(id.z, 14);
                assert!(id.x < n && id.y < n);
                assert!(rect.width() > 200.0 && rect.width() < 320.0);
            }
            // The tile under the viewport centre must contain the centre point.
            let covers_center = tiles.iter().any(|(_, r)| r.contains(vc));
            assert!(covers_center);
        }

        // Phase `geobounds_tile_count_grows_with_zoom`.
        {
            let b = GeoBounds::new(LatLon::new(48.10, 11.55), LatLon::new(48.18, 11.62));
            let small = b.tiles_in_range(12, 12).len();
            let big = b.tiles_in_range(12, 14).len();
            assert!(big > small);
            // Range is inclusive and non-empty.
            assert!(small >= 1);
        }

        // Phase `decode_png_expands_indexed_palette`.
        // Indexed PNGs must decode to RGBA rather than erroring — the hermetic
        // guard for the bug where every (indexed) OSM tile silently failed to
        // decode and the map read as "imagery unavailable".
        {
            let (rgba, w, h) =
                decode_png(&INDEXED_PNG_2X2).expect("indexed PNG must decode, not error");
            assert_eq!((w, h), (2, 2));
            assert_eq!(rgba.len(), 2 * 2 * 4, "expanded to RGBA8");
            // Index 0 → red, fully opaque.
            assert_eq!(&rgba[0..4], &[255, 0, 0, 255]);
            // Index 1 → green.
            assert_eq!(&rgba[4..8], &[0, 255, 0, 255]);
        }

        // Phase `tile_server_is_osm_standard_identified_and_sandbox_allowed`.
        // The worker fetches the OSM Standard raster at the exact URL its usage
        // policy names, identifies itself, and the host is one the app's sandbox
        // lets it reach. CARTO's host never was, and it now answers every tile
        // with an "API key required" placeholder.
        {
            let id = TileId::new(14, 8800, 5680);
            assert_eq!(
                id.url(TILE_SERVER),
                "https://tile.openstreetmap.org/14/8800/5680.png"
            );
            assert!(
                USER_AGENT.starts_with("Tilo-Maps/") && USER_AGENT.contains("(+https://"),
                "the tile server blocks unidentified clients: {USER_AGENT}"
            );
            let net = include_str!("../tilo-app.toml")
                .split("\n[")
                .find(|table| table.starts_with("sandbox.net]"))
                .expect("a [sandbox.net] allowlist");
            let host = TILE_SERVER.trim_start_matches("https://");
            assert!(
                net.lines()
                    .any(|line| line.trim() == format!("\"{host}\",")),
                "the sandbox must allow the tile server {host}: {net}"
            );
        }

        // Phase `dark_style_retones_the_light_raster`.
        // Both styles decode the one raster the server renders: light passes it
        // through, dark turns the pale land deep and the black labels light,
        // while water stays blue and parks green.
        {
            let osm = [
                [242, 239, 233, 255], // land
                [170, 211, 223, 255], // water
                [200, 250, 204, 255], // park
                [0, 0, 0, 255],       // label
            ];
            let mut light = osm.concat();
            TileStyle::Light.retone(&mut light);
            assert_eq!(
                light,
                osm.concat(),
                "the light style is the raster as served"
            );

            let mut dark = osm.concat();
            TileStyle::Dark.retone(&mut dark);
            let px = |i: usize| {
                [
                    dark[i * 4],
                    dark[i * 4 + 1],
                    dark[i * 4 + 2],
                    dark[i * 4 + 3],
                ]
            };
            let luma = |c: [u8; 4]| {
                0.2126 * f64::from(c[0]) + 0.7152 * f64::from(c[1]) + 0.0722 * f64::from(c[2])
            };
            let [land, water, park, label] = [px(0), px(1), px(2), px(3)];
            assert!(luma(land) < 48.0, "dark land is deep: {land:?}");
            assert!(luma(label) > 200.0, "dark labels are light: {label:?}");
            assert!(
                water[2] > water[0] && water[2] > luma(land) as u8,
                "water stays blue: {water:?}"
            );
            assert!(
                park[1] > park[0] && park[1] > park[2],
                "parks stay green: {park:?}"
            );
            assert!(
                dark.chunks_exact(4).all(|c| c[3] == 255),
                "alpha is untouched"
            );
        }

        // Phase `style_tracks_theme`.
        // The basemap style tracks the theme: a dark chrome theme resolves to the
        // dark basemap, a light theme to the light one — the wiring that kills the
        // mixed-tone view.
        {
            assert_eq!(TileStyle::from_theme(&Theme::dark()), TileStyle::Dark);
            assert_eq!(TileStyle::from_theme(&Theme::light()), TileStyle::Light);
        }
    }

    // --- Non-blocking guarantees -------------------------------------------

    /// Encode a solid-colour RGBA tile to PNG bytes (so a test can seed the
    /// on-disk cache without any network).
    fn solid_png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
        ui::png::encode_rgba8(w, h, &rgba.repeat((w * h) as usize)).unwrap()
    }

    /// A 2×2 indexed-colour (palette) PNG — the format the OSM tile server
    /// actually serves — so the decode path can be checked hermetically.
    /// Palette: index 0 = red, index 1 = green; each row is `[0, 1]`.
    const INDEXED_PNG_2X2: [u8; 89] = [
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x08, 0x03, 0x00, 0x00, 0x00, 0x45,
        0x68, 0xfd, 0x16, 0x00, 0x00, 0x00, 0x06, 0x50, 0x4c, 0x54, 0x45, 0xff, 0x00, 0x00, 0x00,
        0xff, 0x00, 0xd2, 0x87, 0xef, 0x71, 0x00, 0x00, 0x00, 0x0e, 0x49, 0x44, 0x41, 0x54, 0x78,
        0xda, 0x63, 0x60, 0x60, 0x64, 0x60, 0x60, 0x04, 0x00, 0x00, 0x0b, 0x00, 0x03, 0x4f, 0x41,
        0x36, 0xf9, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    fn temp_cache() -> PathBuf {
        let mut dir = std::env::temp_dir();
        let uniq = format!(
            "tilo-maps-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        dir.push(uniq);
        dir
    }

    /// Non-blocking guarantees: requests return at once and arrive via drain,
    /// duplicates are no-ops, the offline layer never requests, and LRU bounds memory.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn tile_requests_never_block_and_the_resident_set_is_bounded() {
        // Phase `request_returns_immediately_results_arrive_via_channel`.
        // `request()` must return *immediately* (no blocking on I/O or decode) and
        // the decoded tile must arrive asynchronously via `drain()`. This is the
        // core non-blocking guarantee that makes zoom/pan smooth.
        {
            let cache = temp_cache();
            let id = TileId::new(14, 8800, 5680);
            // Seed the on-disk cache so the worker never needs the network.
            let path = cache.join(id.cache_rel());
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, solid_png(256, 256, [10, 20, 30, 255])).unwrap();

            let mut layer = TileLayer::with_cache_dir(cache.clone());

            // The request call itself must be effectively instant — assert it does
            // not perform the (slow) decode synchronously.
            let t0 = std::time::Instant::now();
            layer.request(id);
            assert!(
                t0.elapsed() < std::time::Duration::from_millis(20),
                "request() blocked for {:?} — it must not fetch/decode on the caller",
                t0.elapsed()
            );
            // Immediately after requesting, the tile is in-flight, NOT resident.
            assert!(layer.is_inflight(id));
            assert!(layer.get(id).is_none());

            // Poll drain() until the worker delivers the decoded tile.
            let mut got = false;
            let deadline = tilo_test_env::hang_deadline();
            while std::time::Instant::now() < deadline {
                layer.drain();
                if layer.get(id).is_some() {
                    got = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert!(got, "decoded tile never arrived via the channel");
            // And it is no longer in-flight once delivered.
            assert!(!layer.is_inflight(id));

            let _ = std::fs::remove_dir_all(&cache);
        }

        // Phase `duplicate_request_is_a_noop`.
        // A second `request()` for an already-in-flight or resident tile must be a
        // no-op — it must not enqueue duplicate work or block.
        {
            let cache = temp_cache();
            let id = TileId::new(12, 2200, 1420);
            let path = cache.join(id.cache_rel());
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, solid_png(64, 64, [1, 2, 3, 255])).unwrap();

            let mut layer = TileLayer::with_cache_dir(cache.clone());
            layer.request(id);
            assert!(layer.is_inflight(id));
            // Requesting again while in-flight changes nothing and does not block.
            let t0 = std::time::Instant::now();
            layer.request(id);
            assert!(t0.elapsed() < std::time::Duration::from_millis(20));

            let deadline = tilo_test_env::hang_deadline();
            while std::time::Instant::now() < deadline && layer.get(id).is_none() {
                layer.drain();
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert!(layer.get(id).is_some());
            // Now resident: a fresh request must stay a no-op (not re-enqueue).
            layer.request(id);
            assert!(!layer.is_inflight(id));

            let _ = std::fs::remove_dir_all(&cache);
        }

        // Phase `offline_layer_never_requests`.
        // The offline layer (no worker) must never block or enqueue anything, so
        // `--shots` and tests are hermetic.
        {
            let mut layer = TileLayer::offline();
            let id = TileId::new(14, 100, 100);
            let t0 = std::time::Instant::now();
            layer.request(id);
            layer.drain();
            assert!(t0.elapsed() < std::time::Duration::from_millis(20));
            assert!(!layer.is_inflight(id));
            assert!(layer.get(id).is_none());
            assert!(!layer.has_inflight());
        }

        // Phase `mem_cache_evicts_beyond_capacity`.
        // LRU eviction keeps the resident set bounded so memory can't grow without
        // limit during a long pan.
        {
            let mut layer = TileLayer::offline();
            let img = make_image(vec![0u8; 4 * 4 * 4], 4, 4).unwrap();
            for i in 0..(MEM_CACHE_CAP.get() as u32 + 50) {
                layer.insert(TileId::new(14, i, 0), DecodedTile { image: img.clone() });
            }
            assert!(layer.mem_len() <= MEM_CACHE_CAP.get());
            // The most-recently-inserted tile must still be resident.
            assert!(
                layer
                    .mem
                    .contains(&TileId::new(14, MEM_CACHE_CAP.get() as u32 + 49, 0))
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn offline_region_download_persists_renders_and_deletes_tiles() {
        // Phase `offline_download_streams_progress_without_blocking`.
        // Starting an offline download enqueues exactly the estimated number of
        // tiles and the worker streams progress back via `drain()` — never
        // blocking the caller. Seeds the cache so no network is needed.
        {
            let cache = temp_cache();
            let bounds = GeoBounds::new(LatLon::new(48.13, 11.57), LatLon::new(48.15, 11.59));
            let (z0, z1) = (13u8, 14u8);
            // Pre-seed every tile in range so the worker hits disk, not the wire.
            for id in bounds.tiles_in_range(z0, z1) {
                let p = cache.join(id.cache_rel());
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(&p, solid_png(32, 32, [9, 9, 9, 255])).unwrap();
            }
            // A local server that permits prefetching; every tile is a disk hit.
            let mut layer = TileLayer::with_cache_dir_url(cache.clone(), closed_local_base());

            let est = TileLayer::estimate_download(bounds, z0, z1);
            let t0 = std::time::Instant::now();
            let queued = layer.start_download(bounds, z0, z1);
            assert!(
                t0.elapsed() < std::time::Duration::from_millis(50),
                "start_download blocked for {:?}",
                t0.elapsed()
            );
            assert_eq!(queued, est);
            assert!(queued > 1);
            assert!(layer.progress().is_active() || est == 0);

            let deadline = tilo_test_env::hang_deadline();
            while std::time::Instant::now() < deadline {
                layer.drain();
                if !layer.progress().is_active() && layer.progress().done >= queued {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let prog = layer.progress();
            assert_eq!(prog.done, queued, "download did not complete via channel");
            assert_eq!(prog.failed, 0, "cached tiles should not fail");

            let _ = std::fs::remove_dir_all(&cache);
        }

        // Phase `download_fetches_and_persists_tiles_to_disk`.
        // The decisive offline-download real-path test: `start_download` must
        // actually **fetch tiles over HTTP and persist the PNGs to the on-disk
        // cache** (not just count). Runs the full production path —
        // `start_download` → worker → HTTP GET → decode → `write_cache` → drain — and
        // then asserts the exact `z/x/y.png` files exist on disk.
        {
            let cache = temp_cache();
            let base = serve_png(solid_png(256, 256, [30, 60, 90, 255]));
            let mut layer = TileLayer::with_cache_dir_url(cache.clone(), base);

            // A small area across two zooms → a handful of real tiles.
            let bounds = GeoBounds::new(LatLon::new(48.13, 11.55), LatLon::new(48.16, 11.58));
            let (z0, z1) = (13u8, 14u8);
            let ids = bounds.tiles_in_range(z0, z1);
            assert!(ids.len() >= 3, "expected several tiles, got {}", ids.len());
            // None are on disk yet.
            for id in &ids {
                assert!(!cache.join(id.cache_rel()).exists());
            }

            let queued = layer.start_download(bounds, z0, z1);
            assert_eq!(queued, ids.len() as u32);

            let deadline = tilo_test_env::hang_deadline();
            while Instant::now() < deadline {
                layer.drain();
                if !layer.progress().is_active() && layer.progress().done >= queued {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(layer.progress().done, queued, "download did not complete");
            assert_eq!(
                layer.progress().failed,
                0,
                "the mock server serves every tile"
            );

            // Every queued tile is now a real PNG on disk — that is what makes the
            // region available offline.
            for id in &ids {
                let path = cache.join(id.cache_rel());
                assert!(path.exists(), "tile {id:?} was not persisted to {path:?}");
                let bytes = std::fs::read(&path).unwrap();
                assert!(decode_png(&bytes).is_ok(), "persisted tile is a valid PNG");
            }

            let _ = std::fs::remove_dir_all(&cache);
        }

        // Phase `cached_region_renders_offline_without_network`.
        // A downloaded region must **render offline**: with the tiles already on
        // disk and the network unreachable (a refused local port), a live layer
        // still serves the tile from the cache — `load_tile` reads disk *before*
        // the network, and a disk hit records no failure. This is the guarantee
        // that a pre-downloaded area works with no connection.
        {
            let cache = temp_cache();
            let id = TileId::new(16, 35210, 22696);
            // Persist the tile as if a prior download had written it.
            let path = cache.join(id.cache_rel());
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, solid_png(256, 256, [12, 34, 56, 255])).unwrap();

            // Network is definitively down (closed port), i.e. offline.
            let mut layer = TileLayer::with_cache_dir_url(cache.clone(), closed_local_base());
            layer.request(id);

            let deadline = tilo_test_env::hang_deadline();
            while Instant::now() < deadline && layer.get(id).is_none() {
                layer.drain();
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(
                layer.get(id).is_some(),
                "a cached tile must load offline (disk-first), with no network"
            );
            assert!(
                !layer.has_failures(),
                "a disk cache hit must not be recorded as a fetch failure"
            );

            let _ = std::fs::remove_dir_all(&cache);
        }

        // Phase `delete_tiles_removes_from_disk_and_reports_freed`.
        // Removing a downloaded region deletes exactly its tiles from disk and
        // reports the bytes freed, without touching tiles it wasn't asked to drop.
        {
            let cache = temp_cache();
            let doomed = TileId::new(15, 17600, 11360);
            let kept = TileId::new(15, 17601, 11360);
            for id in [doomed, kept] {
                let p = cache.join(id.cache_rel());
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(&p, solid_png(64, 64, [7, 7, 7, 255])).unwrap();
            }
            let mut layer = TileLayer::offline_with_cache_dir(cache.clone());

            let freed = layer.delete_tiles(&[doomed]);
            assert!(freed > 0, "deleting a real tile frees its bytes");
            assert!(
                !cache.join(doomed.cache_rel()).exists(),
                "the tile is gone from disk"
            );
            assert!(
                cache.join(kept.cache_rel()).exists(),
                "an untouched tile is kept"
            );

            let _ = std::fs::remove_dir_all(&cache);
        }
    }

    // --- Failed-tile negative cache ----------------------------------------

    /// A closed local port: connecting is refused immediately, so a fetch fails
    /// fast and hermetically — no external server is ever contacted.
    fn closed_local_base() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener); // free the port so subsequent connects are refused
        format!("http://127.0.0.1:{port}")
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn failed_tiles_are_negative_cached_with_capped_exponential_backoff() {
        // Phase `failed_fetch_is_negative_cached_and_suppresses_refetch`.
        // The core of the fix: a tile whose *real* fetch fails must be
        // negative-cached, so a subsequent `request()` (what `draw_tiles` issues
        // every frame for each non-resident tile) is suppressed and does NOT
        // enqueue new work or mark the tile in-flight. This is what stops the
        // per-frame refetch spin and the tile-server hammering. Exercises the full
        // production path: request → worker → HTTP failure → drain → suppression.
        {
            let cache = temp_cache();
            let mut layer = TileLayer::with_cache_dir_url(cache.clone(), closed_local_base());
            let id = TileId::new(14, 8801, 5681);

            // First request is dispatched and marked in-flight.
            layer.request(id);
            assert!(layer.is_inflight(id), "first request should be in-flight");

            // Wait for the (fast) connection-refused failure to arrive via drain.
            let deadline = tilo_test_env::hang_deadline();
            while Instant::now() < deadline && layer.is_inflight(id) {
                layer.drain();
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(
                !layer.is_inflight(id),
                "a failed fetch must clear in-flight"
            );
            assert!(layer.get(id).is_none(), "a failed tile is not resident");
            assert!(
                layer.is_backing_off(id),
                "a failed tile must be negative-cached"
            );
            assert_eq!(layer.failed_delay(id), Some(FAIL_BACKOFF_MIN));
            assert_eq!(
                layer.failure(),
                Some(TileFault::Unreachable),
                "a refused connection is unreachable imagery, not a refusal"
            );

            // The decisive assertion: re-requesting the doomed tile is a no-op while
            // it is backing off — no new job, nothing in flight. `has_inflight()`
            // therefore stays false, which is what lets the app's frame loop idle.
            layer.request(id);
            assert!(
                !layer.is_inflight(id),
                "a backed-off tile must not be re-requested"
            );
            assert!(
                !layer.has_inflight(),
                "no work should be in flight after a suppressed request — the loop can idle"
            );

            let _ = std::fs::remove_dir_all(&cache);
        }

        // Phase `failure_backoff_grows_and_clears_on_success`.
        // The backoff grows exponentially (bounded) across consecutive failures so
        // a persistently-unreachable server is retried ever more rarely, and a
        // later success clears the entry entirely.
        {
            let mut layer = TileLayer::offline();
            let id = TileId::new(12, 1, 2);
            layer.note_failure(id, TileFault::Unreachable);
            assert_eq!(layer.failed_delay(id), Some(FAIL_BACKOFF_MIN));
            layer.note_failure(id, TileFault::Unreachable);
            assert_eq!(layer.failed_delay(id), Some(FAIL_BACKOFF_MIN * 2));
            layer.note_failure(id, TileFault::Unreachable);
            assert_eq!(layer.failed_delay(id), Some(FAIL_BACKOFF_MIN * 4));

            // A resident tile (success) clears the negative cache. Simulate the
            // success branch of `drain` by inserting the decoded tile and dropping
            // the failure entry as that branch does.
            layer.failed.remove(&id);
            let img = make_image(vec![0u8; 4 * 4 * 4], 4, 4).unwrap();
            layer.insert(id, DecodedTile { image: img });
            assert!(
                layer.failed_delay(id).is_none(),
                "success clears the backoff"
            );
            assert!(!layer.is_backing_off(id));
        }

        // Phase `failure_backoff_is_capped`.
        // The backoff is capped so it never grows unbounded, even after many
        // consecutive failures.
        {
            let mut layer = TileLayer::offline();
            let id = TileId::new(10, 3, 4);
            for _ in 0..40 {
                layer.note_failure(id, TileFault::Unreachable);
            }
            assert_eq!(layer.failed_delay(id), Some(FAIL_BACKOFF_MAX));
        }
    }

    // --- Label-free base style + offline region download -------------------

    /// A one-shot local HTTP/1.1 server that answers every request with the
    /// same PNG bytes, so the *real* fetch→decode→persist path can be exercised
    /// hermetically (no external server). Returns the base URL.
    fn serve_png(png: Vec<u8>) -> String {
        serve("200 OK", "image/png", png)
    }

    /// A local HTTP/1.1 server answering every request with `status`,
    /// `content_type` and `body`. Returns the base URL.
    fn serve(status: &'static str, content_type: &'static str, body: Vec<u8>) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                // Read (and discard) the request head — one read covers a GET.
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
                let _ = stream.flush();
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    /// Request `id` and pump `drain` until the worker answers.
    fn settle(layer: &mut TileLayer, id: TileId) {
        layer.request(id);
        let deadline = tilo_test_env::hang_deadline();
        while Instant::now() < deadline && layer.is_inflight(id) {
            layer.drain();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            !layer.is_inflight(id),
            "the worker never answered for {id:?}"
        );
    }

    /// A server that refuses imagery — an error status, or a success carrying
    /// something other than a PNG — yields a rejected failure the map can
    /// report, never a resident tile or a cached file.
    #[test]
    fn refused_tiles_are_rejected_failures_not_imagery() {
        let id = TileId::new(14, 8802, 5682);
        for (status, content_type, body) in [
            (
                "403 Forbidden",
                "image/png",
                solid_png(256, 256, [200, 0, 0, 255]),
            ),
            (
                "200 OK",
                "text/html",
                b"<html>API key required</html>".to_vec(),
            ),
            ("200 OK", "image/png", b"not a png".to_vec()),
        ] {
            let cache = temp_cache();
            let base = serve(status, content_type, body);
            let mut layer = TileLayer::with_cache_dir_url(cache.clone(), base);
            settle(&mut layer, id);
            assert!(
                layer.get(id).is_none(),
                "{status} {content_type}: not resident"
            );
            assert_eq!(
                layer.failure(),
                Some(TileFault::Rejected),
                "{status} {content_type}: the server refused the tile"
            );
            assert!(
                !layer.cache_path(id).exists(),
                "{status} {content_type}: a refused answer is never cached"
            );
            let _ = std::fs::remove_dir_all(&cache);
        }

        // A refusal outweighs unreachable tiles, so a refusing server is not
        // reported as the user being offline.
        let mut layer = TileLayer::offline();
        layer.note_failure(TileId::new(3, 1, 1), TileFault::Unreachable);
        assert_eq!(layer.failure(), Some(TileFault::Unreachable));
        layer.note_failure(TileId::new(3, 2, 1), TileFault::Rejected);
        layer.note_failure(TileId::new(3, 3, 1), TileFault::Unreachable);
        assert_eq!(layer.failure(), Some(TileFault::Rejected));
    }

    /// The OSM tile server forbids prefetching, so a download on its layer
    /// queues nothing and reads nothing from the network.
    #[test]
    fn the_osm_layer_refuses_offline_downloads() {
        let cache = temp_cache();
        let mut layer = TileLayer::with_cache_dir(cache.clone());
        assert!(!layer.allows_prefetch());
        let bounds = GeoBounds::new(LatLon::new(48.13, 11.57), LatLon::new(48.15, 11.59));
        assert_eq!(layer.start_download(bounds, 13, 14), 0);
        assert_eq!(layer.progress(), DownloadProgress::default());
        assert!(!cache.exists(), "nothing was fetched into the cache");
        assert!(!TileLayer::offline().allows_prefetch());
    }

    /// The worker decodes in the active style: a dark theme gets the served
    /// tile re-toned, from the same cached bytes the light style reads.
    #[test]
    fn the_worker_decodes_in_the_active_style() {
        let cache = temp_cache();
        let id = TileId::new(14, 8803, 5683);
        let path = cache.join(id.cache_rel());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, solid_png(256, 256, [242, 239, 233, 255])).unwrap();
        let mut layer = TileLayer::with_cache_dir_url(cache.clone(), closed_local_base());

        let first_pixel = |layer: &mut TileLayer| {
            let data = layer.get(id).expect("tile resident").data.data().to_vec();
            [data[0], data[1], data[2], data[3]]
        };
        settle(&mut layer, id);
        assert_eq!(first_pixel(&mut layer), [242, 239, 233, 255]);

        layer.set_style(TileStyle::Dark);
        assert!(
            layer.get(id).is_none(),
            "the light tile is dropped on the switch"
        );
        settle(&mut layer, id);
        let mut expected = vec![242, 239, 233, 255];
        TileStyle::Dark.retone(&mut expected);
        assert_eq!(first_pixel(&mut layer).to_vec(), expected);
        assert!(
            !layer.has_failures(),
            "both styles read the one cached tile"
        );
        let _ = std::fs::remove_dir_all(&cache);
    }
}
