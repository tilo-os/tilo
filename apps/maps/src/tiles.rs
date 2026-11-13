//! Non-blocking OSM raster tiles. The UI thread never touches the network or decodes a PNG:
//! [`visible_tiles`](Self::visible_tiles) picks the tiles, a worker caches, fetches and decodes off
//! the UI thread, and [`drain`](Self::drain) folds finished tiles in between frames. A fresh zoom
//! draws a cached ancestor scaled under the hole, so imagery never blanks.

use std::collections::HashMap;
use std::num::NonZeroUsize;

use lru::LruCache;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;
use ui::time::Instant;

use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{Color, Fill, ImageBrush, ImageData as PenikoImage};

use ui::theme::Theme;

use crate::geo;
use crate::model::LatLon;

/// Slippy-map tile edge length in pixels (matches `geo::TILE_SIZE`).
pub const TILE_PX: f64 = 256.0;

/// Decoded tiles kept resident: each is 256×256×4, so 512 tiles is about 128 MiB, far more than a
/// screen needs.
/// How many decoded tiles we keep resident in RAM. Each tile is 256×256×4 ≈
/// 256 KiB, so 512 tiles ≈ 128 MiB worst case — comfortable, and far more
/// than any single viewport needs (a 4K screen covers < 150 tiles).
const MEM_CACHE_CAP: NonZeroUsize = match NonZeroUsize::new(512) {
    Some(cap) => cap,
    None => NonZeroUsize::MIN,
};

/// First retry delay for a tile whose fetch failed. The delay doubles on each consecutive failure
/// (see [`FAIL_BACKOFF_MAX`]).
const FAIL_BACKOFF_MIN: Duration = Duration::from_secs(2);
/// Ceiling for the failed-tile backoff. A tile server that is persistently unreachable is retried
/// at most this often, never per-frame.
const FAIL_BACKOFF_MAX: Duration = Duration::from_secs(300);
/// Cap on the negative cache, so a long pan across an unreachable region cannot grow it without
/// bound.
const FAIL_CACHE_CAP: usize = 4096;

/// A failed fetch: no further request for that tile until `retry_at`, backing off exponentially, so
/// an unreachable server is not hammered and the loop can idle.
#[derive(Clone, Copy)]
struct FailedFetch {
    retry_at: Instant,
    /// The current backoff window, doubling up to [`FAIL_BACKOFF_MAX`].
    delay: Duration,
    fault: TileFault,
}

/// The OSM Standard raster, used under the OSMF tile policy: an identifying `USER_AGENT`, visible
/// attribution, a local cache, and no bulk download or prefetch, so offline areas are refused
/// ([`allows_prefetch`](Self::allows_prefetch)). The server name must stay in the manifest's
/// `[sandbox] net` allowlist. The OpenStreetMap Standard raster, used under the OSMF Tile Usage
/// Policy (<https://operations.osmfoundation.org/policies/tiles/>): an identifying `USER_AGENT`,
/// attribution visible on the map, a persistent local cache, and no bulk download or prefetch — so
/// offline areas are refused ([`TileLayer::allows_prefetch`]).
pub const TILE_SERVER: &str = "https://tile.openstreetmap.org";

/// The credit [`TILE_SERVER`]'s licence requires on the map.
pub const TILE_ATTRIBUTION: &str = "© OpenStreetMap contributors";

/// Cache subdirectory for [`TILE_SERVER`]; earlier builds cached CARTO trees beside it, whose tiles
/// are never read.
/// Cache subdirectory for [`TILE_SERVER`] under the style-agnostic cache root.
/// Earlier builds cached CARTO's `light`/`dark` trees beside it; those hold
/// "API key required" placeholders and are never read.
const TILE_SERVER_CACHE: &str = "osm";

/// The tone the raster is drawn in, from the OS theme. The server has one rendering;
/// [`TileStyle::Dark`] re-tones those tiles on decode, so both styles share one download and one
/// cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileStyle {
    Light,
    Dark,
}

impl TileStyle {
    /// Re-tone decoded RGBA8 pixels in place for the dark style: invert lightness, keep hue, soften
    /// contrast.
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

    pub fn from_theme(theme: &Theme) -> Self {
        let c = theme.tokens.colors.surfaces.content.to_rgba8();
        let lum = 0.2126 * f64::from(c.r) + 0.7152 * f64::from(c.g) + 0.0722 * f64::from(c.b);
        if lum < 128.0 {
            TileStyle::Dark
        } else {
            TileStyle::Light
        }
    }
}

/// Identifies Tilo Maps to the tile server, as its usage policy requires; a library default
/// User-Agent is blocked.
pub(crate) const USER_AGENT: &str = concat!(
    "Tilo-Maps/",
    env!("CARGO_PKG_VERSION"),
    " (+https://code.tilo.so/tilo/tilo)"
);

/// Why a fetch failed, so the map can say whether imagery is unreachable or refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileFault {
    Unreachable,
    Rejected,
}

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

    fn n(z: u8) -> u32 {
        1u32 << z
    }

    pub fn parent(&self) -> Option<TileId> {
        if self.z == 0 {
            None
        } else {
            Some(TileId::new(self.z - 1, self.x / 2, self.y / 2))
        }
    }

    fn cache_rel(&self) -> PathBuf {
        PathBuf::from(format!("{}/{}/{}.png", self.z, self.x, self.y))
    }

    fn cache_vpath(&self) -> Option<tilo_files::VPath> {
        tilo_files::VPath::new(&format!("{}/{}/{}.png", self.z, self.x, self.y)).ok()
    }

    fn url(&self, base: &str) -> String {
        format!("{base}/{}/{}/{}.png", self.z, self.x, self.y)
    }
}

#[derive(Clone)]
pub struct DecodedTile {
    pub image: PenikoImage,
}

enum Job {
    Fetch(TileId),
    Download(TileId),
}

enum Done {
    Tile {
        id: TileId,
        rgba: Result<(Vec<u8>, u32, u32), TileFault>,
    },
    Download {
        ok: bool,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DownloadProgress {
    pub total: u32,
    pub done: u32,
    pub failed: u32,
    pub stopped: bool,
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

pub struct TileLayer {
    mem: LruCache<TileId, DecodedTile>,
    inflight: std::collections::HashSet<TileId>,
    /// Tiles whose last fetch failed, with an exponential backoff, so a doomed tile is not
    /// re-requested every frame.
    failed: HashMap<TileId, FailedFetch>,
    job_tx: Option<Sender<Job>>,
    done_rx: Option<Receiver<Done>>,
    /// Where no thread may block (a browser page), tiles are fetched as runtime tasks and arrive on
    /// the same channel.
    tasks: Option<TaskFetcher>,
    /// Wakes the app when the worker delivers, so an in-flight fetch needs no frame polling.
    wake: ui::wake::WakeHandle,
    style: TileStyle,
    base_url: String,
    allows_prefetch: bool,
    /// The cache root this layer owns and [`clear_cache`](Self::clear_cache) removes whole; `None`
    /// for a test layer, whose `cache_dir` is exactly as given.
    cache_root: Option<PathBuf>,
    /// Raw server tiles on disk (`z/x/y.png`), shared by both styles.
    cache_dir: PathBuf,
    progress: DownloadProgress,
    /// Set by [`stop_download`](Self::stop_download): the worker checks it before each tile it
    /// would fetch, so a wrong city is not waited out.
    stop: Arc<AtomicBool>,
}

impl TileLayer {
    pub fn new() -> Self {
        Self::live_with_root(default_cache_dir())
    }

    pub fn live_with_root(root: PathBuf) -> Self {
        let mut layer = Self::spawn_worker(root.join(TILE_SERVER_CACHE), TILE_SERVER.to_string());
        layer.cache_root = Some(root);
        layer
    }

    pub fn with_cache_dir(cache_dir: PathBuf) -> Self {
        Self::with_cache_dir_and_base(cache_dir, TILE_SERVER.to_string())
    }

    fn with_cache_dir_and_base(cache_dir: PathBuf, base_url: String) -> Self {
        Self::spawn_worker(cache_dir, base_url)
    }

    fn spawn_worker(cache_dir: PathBuf, base_url: String) -> Self {
        let mut layer = Self::offline_with_cache_dir(cache_dir);
        layer.base_url = base_url;
        layer.respawn_worker();
        layer
    }

    /// (Re)start the worker for the current cache dir, server and style; the previous one exits
    /// when its channel drops, and a spawn failure degrades to no worker instead of crashing.
    fn respawn_worker(&mut self) {
        let (job_tx, job_rx) = channel::<Job>();
        let (done_tx, done_rx) = channel::<Done>();
        self.tasks = None;
        // Where no thread may block (a browser page) there is no worker: each tile is a runtime
        // task.
        if !ui::rt::available(ui::rt::Feature::BlockOn) {
            self.job_tx = None;
            self.tasks = TaskFetcher::new(done_tx, self.wake.clone(), &self.base_url, self.style);
            self.done_rx = self.tasks.is_some().then_some(done_rx);
            return;
        }
        let dir = self.cache_dir.clone();
        let base_url = self.base_url.clone();
        let style = self.style;
        let wake = self.wake.clone();
        let stop = Arc::clone(&self.stop);
        let spawned = ui::rt::spawn_thread("maps-tiles", move || {
            worker(&job_rx, &done_tx, &wake, &dir, &base_url, style, &stop);
        })
        .is_ok();
        self.job_tx = spawned.then_some(job_tx);
        self.done_rx = spawned.then_some(done_rx);
    }

    /// Whether this layer can fetch at all; an inert layer, or one whose worker did not start,
    /// shows only resident tiles.
    pub fn can_fetch(&self) -> bool {
        self.job_tx.is_some() || self.tasks.is_some()
    }

    pub fn set_style(&mut self, style: TileStyle) {
        if self.style == style {
            return;
        }
        self.style = style;
        // Wrong-tone imagery must not linger through a style switch.
        self.mem.clear();
        self.inflight.clear();
        self.failed.clear();
        if self.job_tx.is_some() {
            self.respawn_worker();
        }
    }

    /// An inert layer: no worker, no network, for `--shots` and tests.
    pub fn offline() -> Self {
        Self::offline_with_cache_dir(default_cache_dir())
    }

    /// An inert layer rooted at `cache_dir`, so a cache test drives `clear_cache` on a temp dir.
    pub fn offline_with_cache_dir(cache_dir: PathBuf) -> Self {
        Self {
            mem: LruCache::new(MEM_CACHE_CAP),
            inflight: std::collections::HashSet::new(),
            failed: HashMap::new(),
            job_tx: None,
            done_rx: None,
            tasks: None,
            wake: ui::wake::WakeHandle::new(),
            style: TileStyle::Light,
            base_url: TILE_SERVER.to_string(),
            allows_prefetch: false,
            cache_root: None,
            cache_dir,
            progress: DownloadProgress::default(),
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn cache_dir(&self) -> &std::path::Path {
        &self.cache_dir
    }

    pub fn progress(&self) -> DownloadProgress {
        self.progress
    }

    pub fn mem_len(&self) -> usize {
        self.mem.len()
    }

    pub fn tile_zoom(map_zoom: f64) -> u8 {
        map_zoom.round().clamp(0.0, 19.0) as u8
    }

    pub fn visible_tiles(
        center: LatLon,
        map_zoom: f64,
        view_center: vello::kurbo::Point,
        viewport: Rect,
    ) -> Vec<(TileId, Rect)> {
        let z = Self::tile_zoom(map_zoom);
        let scale = 2f64.powf(map_zoom - f64::from(z));
        let tile_screen = TILE_PX * scale;
        let n = TileId::n(z);

        let (cwx, cwy) = geo::lonlat_to_world(center, f64::from(z));
        let left_world = cwx - (view_center.x - viewport.x0) / scale;
        let top_world = cwy - (view_center.y - viewport.y0) / scale;

        let first_x = (left_world / TILE_PX).floor() as i64;
        let first_y = (top_world / TILE_PX).floor() as i64;
        let cols = (viewport.width() / tile_screen).ceil() as i64 + 2;
        let rows = (viewport.height() / tile_screen).ceil() as i64 + 2;

        let mut out = Vec::with_capacity((cols * rows).max(0) as usize);
        for ty in first_y..first_y + rows {
            for tx in first_x..first_x + cols {
                // Wrap X at the antimeridian; Y does not wrap.
                if ty < 0 || ty >= i64::from(n) {
                    continue;
                }
                let wx = tx.rem_euclid(i64::from(n)) as u32;
                let id = TileId::new(z, wx, ty as u32);
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

    pub fn drain(&mut self) {
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
                            // A successful fetch drops the negative-cache entry.
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

    pub fn get(&mut self, id: TileId) -> Option<&PenikoImage> {
        self.mem.get(&id).map(|t| &t.image)
    }

    pub fn is_inflight(&self, id: TileId) -> bool {
        self.inflight.contains(&id)
    }

    pub fn has_inflight(&self) -> bool {
        !self.inflight.is_empty()
    }

    pub fn has_failures(&self) -> bool {
        !self.failed.is_empty()
    }

    /// The most telling fault among failing tiles: `Rejected` when the server refused any, so a
    /// refusing server is never reported as being offline.
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

    /// Whether the server permits fetching tiles nobody views, which offline areas need;
    /// [`TILE_SERVER`] forbids it, so the app refuses to save areas rather than scrape a community
    /// server.
    pub fn allows_prefetch(&self) -> bool {
        self.allows_prefetch
    }

    /// Request `id` unless it is resident, in flight, or suppressed by the backoff; returns at
    /// once.
    pub fn request(&mut self, id: TileId) {
        if self.mem.contains(&id) || self.inflight.contains(&id) {
            return;
        }
        // Not requested again until the backoff elapses. Without this every non-resident tile would
        // be re-requested every frame, spinning the loop and hammering the server; a later success
        // drops the entry.
        if let Some(f) = self.failed.get(&id)
            && ui::time::now() < f.retry_at
        {
            return;
        }
        if let Some(tx) = &self.job_tx
            && tx.send(Job::Fetch(id)).is_ok()
        {
            self.inflight.insert(id);
        } else if let Some(tasks) = &self.tasks {
            tasks.fetch(id);
            self.inflight.insert(id);
        }
    }

    /// Note a failed fetch, doubling this tile's backoff up to [`FAIL_BACKOFF_MAX`].
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
            self.failed.retain(|_, f| f.retry_at > now);
            if self.failed.len() > FAIL_CACHE_CAP {
                self.failed.clear();
            }
        }
    }

    /// The nearest cached ancestor covering `id` and how many zoom levels up it is, for a zoom/pan
    /// placeholder.
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

    /// Download every tile covering `region` across `z0..=z1` into the cache, progress streaming
    /// back through `drain`; returns the tiles queued, none when prefetching is forbidden.
    pub fn start_download(&mut self, region: GeoBounds, z0: u8, z1: u8) -> u32 {
        if !self.allows_prefetch {
            return 0;
        }
        let ids = region.tiles_in_range(z0, z1);
        let total = ids.len() as u32;
        self.stop.store(false, Ordering::Relaxed);
        self.progress = DownloadProgress {
            total,
            done: 0,
            failed: 0,
            stopped: false,
        };
        if let Some(tx) = &self.job_tx {
            for id in ids {
                let _ = tx.send(Job::Download(id));
            }
        } else {
            self.progress.done = total;
        }
        total
    }

    /// Stop the download: the worker fetches nothing more, the progress is what arrived, and the
    /// tiles on disk stay. Returns whether a download was running.
    pub fn stop_download(&mut self) -> bool {
        if !self.progress.is_active() {
            return false;
        }
        self.stop.store(true, Ordering::Relaxed);
        self.progress.done = self.progress.total;
        self.progress.stopped = true;
        true
    }

    pub fn estimate_download(region: GeoBounds, z0: u8, z1: u8) -> u32 {
        region.tiles_in_range(z0, z1).len() as u32
    }

    /// Delete the whole on-disk cache and RAM, returning bytes freed: the style-agnostic root for
    /// the live layer, exactly `cache_dir` for a test layer.
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

    /// Delete tiles from RAM and disk, returning bytes freed; the caller excludes any tile a region
    /// it keeps still covers.
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

    /// Bytes the on-disk cache occupies: all styles for the live layer, exactly `cache_dir` for a
    /// test layer.
    pub fn cache_bytes(&self) -> u64 {
        dir_size(self.cache_root.as_deref().unwrap_or(&self.cache_dir))
    }

    fn insert(&mut self, id: TileId, tile: DecodedTile) {
        self.mem.put(id, tile);
    }

    /// A live layer fetching from `base_url`, so a test drives the real fetch, failure and backoff
    /// path against a local endpoint.
    #[cfg(test)]
    fn with_cache_dir_url(cache_dir: PathBuf, base_url: String) -> Self {
        let mut layer = Self::with_cache_dir_and_base(cache_dir, base_url);
        layer.allows_prefetch = true;
        layer
    }

    #[cfg(test)]
    pub(crate) fn allow_prefetch_for_test(&mut self) {
        self.allows_prefetch = true;
    }

    #[cfg(test)]
    pub(crate) fn cache_path(&self, id: TileId) -> PathBuf {
        self.cache_dir.join(id.cache_rel())
    }

    #[cfg(test)]
    pub(crate) fn insert_solid_tile(&mut self, id: TileId) {
        let img = make_image(vec![80u8; 256 * 256 * 4], 256, 256).expect("solid tile");
        self.insert(id, DecodedTile { image: img });
    }

    #[cfg(test)]
    fn failed_delay(&self, id: TileId) -> Option<Duration> {
        self.failed.get(&id).map(|f| f.delay)
    }

    #[cfg(test)]
    fn is_backing_off(&self, id: TileId) -> bool {
        self.failed
            .get(&id)
            .is_some_and(|f| ui::time::now() < f.retry_at)
    }
}

impl Default for TileLayer {
    fn default() -> Self {
        Self::offline()
    }
}

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

fn worker(
    rx: &Receiver<Job>,
    tx: &Sender<Done>,
    wake: &ui::wake::WakeHandle,
    cache_dir: &std::path::Path,
    base_url: &str,
    style: TileStyle,
    stop: &AtomicBool,
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
                if stop.load(Ordering::Relaxed) {
                    continue;
                }
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

#[derive(Clone)]
struct TaskFetcher {
    client: tilo_net::Client,
    done: Sender<Done>,
    wake: ui::wake::WakeHandle,
    base_url: String,
    style: TileStyle,
}

impl TaskFetcher {
    fn new(
        done: Sender<Done>,
        wake: ui::wake::WakeHandle,
        base_url: &str,
        style: TileStyle,
    ) -> Option<Self> {
        ui::rt::try_app().ok()?;
        let client = tilo_net::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .inspect_err(|e| tracing::warn!("maps tile HTTP client failed to build: {e}"))
            .ok()?;
        Some(Self {
            client,
            done,
            wake,
            base_url: base_url.to_string(),
            style,
        })
    }

    fn fetch(&self, id: TileId) {
        let fetcher = self.clone();
        let Ok(runtime) = ui::rt::try_app() else {
            return;
        };
        drop(runtime.spawn(async move {
            let rgba = fetch_tile_bytes(&fetcher.client, &fetcher.base_url, id)
                .await
                .and_then(|bytes| {
                    decode_png(&bytes).map_err(|e| {
                        tracing::warn!("maps tile {id:?} does not decode as PNG: {e}");
                        TileFault::Rejected
                    })
                })
                .map(|(mut rgba, w, h)| {
                    fetcher.style.retone(&mut rgba);
                    (rgba, w, h)
                });
            if fetcher.done.send(Done::Tile { id, rgba }).is_ok() {
                fetcher.wake.wake();
            }
        }));
    }
}

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

fn fetch_tile(
    client: Option<&tilo_net::Client>,
    base_url: &str,
    id: TileId,
) -> Result<Vec<u8>, TileFault> {
    let client = client.ok_or(TileFault::Unreachable)?;
    ui::rt::app()
        .block_on(fetch_tile_bytes(client, base_url, id))
        .map_err(|_| TileFault::Unreachable)?
}

async fn fetch_tile_bytes(
    client: &tilo_net::Client,
    base_url: &str,
    id: TileId,
) -> Result<Vec<u8>, TileFault> {
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
}

fn cache_files(root: &std::path::Path) -> Option<tilo_files::Dir> {
    tilo_files::Dir::open(tilo_files::Location::Native(root.to_path_buf())).ok()
}

fn write_cache(dir: &tilo_files::Dir, path: &tilo_files::VPath, bytes: &[u8]) {
    let _ = tilo_files::blocking::write(dir, path, bytes);
}

fn make_image(rgba: Vec<u8>, w: u32, h: u32) -> Option<PenikoImage> {
    if rgba.len() != (w as usize) * (h as usize) * 4 {
        return None;
    }
    Some(ui::raster::rgba8(rgba, w, h))
}

fn decode_png(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    let decoded = ui::png::decode_rgba8(bytes).map_err(|e| e.to_string())?;
    Ok((decoded.rgba, decoded.width, decoded.height))
}

fn default_cache_dir() -> PathBuf {
    tilo_paths::xdg::cache_home()
        .or_else(tilo_paths::temp_dir)
        .unwrap_or_default()
        .join("tilo-maps/tiles")
}

fn dir_size(dir: &std::path::Path) -> u64 {
    cache_files(dir).map_or(0, |files| tree_size(&files, &tilo_files::VPath::root()))
}

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
    let span = 1u32 << levels;
    let cell_x = child.x - ancestor.x * span;
    let cell_y = child.y - ancestor.y * span;
    let src_w = f64::from(img.width) / scale;
    let src_h = f64::from(img.height) / scale;
    let src_x = f64::from(cell_x) * src_w;
    let src_y = f64::from(cell_y) * src_h;

    let sx = dst.width() / src_w;
    let sy = dst.height() / src_h;
    let brush_transform = Affine::translate((dst.x0, dst.y0))
        * Affine::scale_non_uniform(sx, sy)
        * Affine::translate((-src_x, -src_y));
    let brush = ImageBrush::new(img.clone());
    scene.fill(
        Fill::NonZero,
        transform,
        &brush,
        Some(brush_transform),
        &dst,
    );
}

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
        if painted {
            return true;
        }
    }
    false
}

pub fn paint_placeholder(scene: &mut Scene, transform: Affine, dst: Rect, color: Color) {
    scene.fill(Fill::NonZero, transform, color, None, &dst);
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::kurbo::Point;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn tile_addressing_decoding_and_label_free_styles() {
        {
            assert_eq!(TileLayer::tile_zoom(14.4), 14);
            assert_eq!(TileLayer::tile_zoom(14.6), 15);
            assert_eq!(TileLayer::tile_zoom(-3.0), 0);
            assert_eq!(TileLayer::tile_zoom(99.0), 19);
        }

        {
            let t = TileId::new(14, 8800, 5680);
            let p = t.parent().unwrap();
            assert_eq!(p, TileId::new(13, 4400, 2840));
            assert!(TileId::new(0, 0, 0).parent().is_none());
        }

        {
            let center = LatLon::new(48.1371, 11.5754);
            let vp = Rect::new(0.0, 0.0, 1280.0, 800.0);
            let vc = Point::new(640.0, 400.0);
            let tiles = TileLayer::visible_tiles(center, 14.0, vc, vp);
            assert!(!tiles.is_empty());
            assert!(tiles.len() >= 20, "got {}", tiles.len());
            let n = 1u32 << 14;
            for (id, rect) in &tiles {
                assert_eq!(id.z, 14);
                assert!(id.x < n && id.y < n);
                assert!(rect.width() > 200.0 && rect.width() < 320.0);
            }
            let covers_center = tiles.iter().any(|(_, r)| r.contains(vc));
            assert!(covers_center);
        }

        {
            let b = GeoBounds::new(LatLon::new(48.10, 11.55), LatLon::new(48.18, 11.62));
            let small = b.tiles_in_range(12, 12).len();
            let big = b.tiles_in_range(12, 14).len();
            assert!(big > small);
            assert!(small >= 1);
        }

        {
            let (rgba, w, h) =
                decode_png(&INDEXED_PNG_2X2).expect("indexed PNG must decode, not error");
            assert_eq!((w, h), (2, 2));
            assert_eq!(rgba.len(), 2 * 2 * 4, "expanded to RGBA8");
            assert_eq!(&rgba[0..4], &[255, 0, 0, 255]);
            assert_eq!(&rgba[4..8], &[0, 255, 0, 255]);
        }

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

        {
            assert_eq!(TileStyle::from_theme(&Theme::dark()), TileStyle::Dark);
            assert_eq!(TileStyle::from_theme(&Theme::light()), TileStyle::Light);
        }
    }

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
        /// The wall clock alone does not keep parallel tests apart: a capture test freezes it
        /// process-wide, and macOS reads it in whole microseconds. A per-process sequence number
        /// does.
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let mut dir = std::env::temp_dir();
        let uniq = format!(
            "tilo-maps-test-{}-{}-{}",
            std::process::id(),
            ui::time::try_unix_now_ns().unwrap(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        dir.push(uniq);
        dir
    }

    #[test]
    fn tile_requests_never_block_and_the_resident_set_is_bounded() {
        {
            let cache = temp_cache();
            let id = TileId::new(14, 8800, 5680);
            let server = HeldServer::png(solid_png(256, 256, [10, 20, 30, 255]));
            let mut layer = TileLayer::with_cache_dir_url(cache.clone(), server.base.clone());

            layer.request(id);
            assert!(layer.is_inflight(id));
            assert!(layer.get(id).is_none());
            server.release();

            tilo_test_env::wait_until("the decoded tile to arrive via the channel", || {
                layer.drain();
                layer.get(id).is_some()
            });
            assert!(!layer.is_inflight(id));

            let _ = std::fs::remove_dir_all(&cache);
        }

        {
            let cache = temp_cache();
            let id = TileId::new(12, 2200, 1420);
            let path = cache.join(id.cache_rel());
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, solid_png(64, 64, [1, 2, 3, 255])).unwrap();

            let mut layer = TileLayer::with_cache_dir(cache.clone());
            layer.request(id);
            assert!(layer.is_inflight(id));
            layer.request(id);

            tilo_test_env::wait_until("the tile to become resident", || {
                layer.drain();
                layer.get(id).is_some()
            });
            layer.request(id);
            assert!(!layer.is_inflight(id));

            let _ = std::fs::remove_dir_all(&cache);
        }

        {
            let mut layer = TileLayer::offline();
            let id = TileId::new(14, 100, 100);
            layer.request(id);
            layer.drain();
            assert!(!layer.is_inflight(id));
            assert!(layer.get(id).is_none());
            assert!(!layer.has_inflight());
        }

        {
            let mut layer = TileLayer::offline();
            let img = make_image(vec![0u8; 4 * 4 * 4], 4, 4).unwrap();
            for i in 0..(MEM_CACHE_CAP.get() as u32 + 50) {
                layer.insert(TileId::new(14, i, 0), DecodedTile { image: img.clone() });
            }
            assert!(layer.mem_len() <= MEM_CACHE_CAP.get());
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
        {
            let cache = temp_cache();
            let bounds = GeoBounds::new(LatLon::new(48.13, 11.57), LatLon::new(48.15, 11.59));
            let (z0, z1) = (13u8, 14u8);
            for id in bounds.tiles_in_range(z0, z1) {
                let p = cache.join(id.cache_rel());
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(&p, solid_png(32, 32, [9, 9, 9, 255])).unwrap();
            }
            let mut layer = TileLayer::with_cache_dir_url(cache.clone(), closed_local_base());

            let est = TileLayer::estimate_download(bounds, z0, z1);
            let queued = layer.start_download(bounds, z0, z1);
            assert_eq!(queued, est);
            assert!(queued > 1);
            assert!(layer.progress().is_active() || est == 0);

            tilo_test_env::wait_until("the download to complete via the channel", || {
                layer.drain();
                !layer.progress().is_active() && layer.progress().done >= queued
            });
            let prog = layer.progress();
            assert_eq!(prog.done, queued, "download did not complete via channel");
            assert_eq!(prog.failed, 0, "cached tiles should not fail");

            let _ = std::fs::remove_dir_all(&cache);
        }

        {
            let cache = temp_cache();
            let server = HeldServer::png(solid_png(256, 256, [30, 60, 90, 255]));
            let mut layer = TileLayer::with_cache_dir_url(cache.clone(), server.base.clone());

            let bounds = GeoBounds::new(LatLon::new(48.13, 11.55), LatLon::new(48.16, 11.58));
            let (z0, z1) = (13u8, 14u8);
            let ids = bounds.tiles_in_range(z0, z1);
            assert!(ids.len() >= 3, "expected several tiles, got {}", ids.len());
            for id in &ids {
                assert!(!cache.join(id.cache_rel()).exists());
            }

            let queued = layer.start_download(bounds, z0, z1);
            assert_eq!(queued, ids.len() as u32);
            layer.drain();
            assert!(layer.progress().is_active());
            assert_eq!(layer.progress().done, 0);
            server.release();

            tilo_test_env::wait_until("the download to complete", || {
                layer.drain();
                !layer.progress().is_active() && layer.progress().done >= queued
            });
            assert_eq!(layer.progress().done, queued, "download did not complete");
            assert_eq!(
                layer.progress().failed,
                0,
                "the mock server serves every tile"
            );

            for id in &ids {
                let path = cache.join(id.cache_rel());
                assert!(path.exists(), "tile {id:?} was not persisted to {path:?}");
                let bytes = std::fs::read(&path).unwrap();
                assert!(decode_png(&bytes).is_ok(), "persisted tile is a valid PNG");
            }

            let _ = std::fs::remove_dir_all(&cache);
        }

        {
            let cache = temp_cache();
            let id = TileId::new(16, 35210, 22696);
            let path = cache.join(id.cache_rel());
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, solid_png(256, 256, [12, 34, 56, 255])).unwrap();

            let mut layer = TileLayer::with_cache_dir_url(cache.clone(), closed_local_base());
            layer.request(id);

            tilo_test_env::wait_until(
                "the cached tile to load offline (disk-first), with no network",
                || {
                    layer.drain();
                    layer.get(id).is_some()
                },
            );
            assert!(
                !layer.has_failures(),
                "a disk cache hit must not be recorded as a fetch failure"
            );

            let _ = std::fs::remove_dir_all(&cache);
        }

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

    fn closed_local_base() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener); // free the port so subsequent connects are refused
        format!("http://127.0.0.1:{port}")
    }

    #[test]
    fn failed_tiles_are_negative_cached_with_capped_exponential_backoff() {
        ui::time::testing::with_monotonic_time(ui::time::now(), || {
            let cache = temp_cache();
            let mut layer = TileLayer::with_cache_dir_url(cache.clone(), closed_local_base());
            let id = TileId::new(14, 8801, 5681);

            layer.request(id);
            assert!(layer.is_inflight(id), "first request should be in-flight");

            tilo_test_env::wait_until("the failed fetch to clear in-flight", || {
                layer.drain();
                !layer.is_inflight(id)
            });
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
        });

        {
            let mut layer = TileLayer::offline();
            let id = TileId::new(12, 1, 2);
            layer.note_failure(id, TileFault::Unreachable);
            assert_eq!(layer.failed_delay(id), Some(FAIL_BACKOFF_MIN));
            layer.note_failure(id, TileFault::Unreachable);
            assert_eq!(layer.failed_delay(id), Some(FAIL_BACKOFF_MIN * 2));
            layer.note_failure(id, TileFault::Unreachable);
            assert_eq!(layer.failed_delay(id), Some(FAIL_BACKOFF_MIN * 4));

            layer.failed.remove(&id);
            let img = make_image(vec![0u8; 4 * 4 * 4], 4, 4).unwrap();
            layer.insert(id, DecodedTile { image: img });
            assert!(
                layer.failed_delay(id).is_none(),
                "success clears the backoff"
            );
            assert!(!layer.is_backing_off(id));
        }

        {
            let mut layer = TileLayer::offline();
            let id = TileId::new(10, 3, 4);
            for _ in 0..40 {
                layer.note_failure(id, TileFault::Unreachable);
            }
            assert_eq!(layer.failed_delay(id), Some(FAIL_BACKOFF_MAX));
        }
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

    struct HeldServer {
        base: String,
        gate: std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
    }

    impl HeldServer {
        fn png(body: Vec<u8>) -> Self {
            use std::io::{Read, Write};
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let gate =
                std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
            let server_gate = gate.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { continue };
                    let mut buf = [0u8; 1024];
                    let _ = stream.read(&mut buf);
                    let (released, wake) = &*server_gate;
                    let mut open = released.lock().unwrap();
                    while !*open {
                        open = wake.wait(open).unwrap();
                    }
                    drop(open);
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(&body);
                    let _ = stream.flush();
                }
            });
            Self {
                base: format!("http://127.0.0.1:{port}"),
                gate,
            }
        }

        fn release(&self) {
            let (released, wake) = &*self.gate;
            *released.lock().unwrap() = true;
            wake.notify_all();
        }
    }

    fn settle(layer: &mut TileLayer, id: TileId) {
        layer.request(id);
        tilo_test_env::wait_until(format!("the worker's answer for {id:?}"), || {
            layer.drain();
            !layer.is_inflight(id)
        });
    }

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

        let mut layer = TileLayer::offline();
        layer.note_failure(TileId::new(3, 1, 1), TileFault::Unreachable);
        assert_eq!(layer.failure(), Some(TileFault::Unreachable));
        layer.note_failure(TileId::new(3, 2, 1), TileFault::Rejected);
        layer.note_failure(TileId::new(3, 3, 1), TileFault::Unreachable);
        assert_eq!(layer.failure(), Some(TileFault::Rejected));
    }

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
