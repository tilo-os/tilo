//! Artwork for applications Tilo did not package.
//!
//! A host application names its icon (`Icon=` in its desktop entry); a
//! window names only its `app_id`. This module turns either into pixels for
//! the shell's chrome — the tab pill, the palette's application rows, the
//! tray's fallback — preferring a window's own `xdg_toplevel_icon_v1` icon,
//! and otherwise through the freedesktop icon-theme lookup in
//! [`desktop_entry::icon_theme`]. PNGs are decoded and downscaled, SVGs are
//! rasterised by `resvg` at the device size the chrome paints them at, and
//! both are cached, since chrome asks every frame. All of it runs on the
//! resolver's own thread ([`Resolver`]), never on the compositor's loop.
//!
//! An icon that cannot be found or decoded is simply absent: every caller
//! keeps its own glyph or letter badge as the fallback.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};

use desktop_entry::icon_theme::{self, IconFile, IconFormat, IconLookup};
use ui::icons::{AppImage, AppImageSource};
use vello::peniko::{Blob, ImageData};

use crate::shell::host_apps::{self};
use tilo_lock::MutexRecover as _;

/// Icon files larger than this are not read: an icon is kilobytes, and a
/// hostile or broken theme must not make the compositor parse megabytes.
const MAX_SOURCE_BYTES: u64 = 4 * 1024 * 1024;

/// Largest decoded edge accepted from a PNG.
const MAX_DECODED_EDGE: u32 = 1024;

/// Rasterisations kept before the cache is dropped and rebuilt.
const RASTER_CACHE_LIMIT: usize = 512;

/// Size a name is probed at to decide whether an app has artwork at all.
const PROBE_PX: u32 = 32;

/// Straight-alpha RGBA pixels, row-major.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<Vec<u8>>,
}

impl Raster {
    fn image_data(&self) -> ImageData {
        ui::raster::rgba8(Blob::new(self.rgba.clone()), self.width, self.height)
    }
}

/// Decode `file` for painting `device_px` square: an SVG is rendered at that
/// size, a PNG larger than it is downscaled to it.
pub fn rasterize(file: &IconFile, device_px: u32) -> Option<Raster> {
    let device_px = device_px.clamp(1, ui::icons::MAX_DEVICE_PX);
    let length = std::fs::metadata(&file.path).ok()?.len();
    if length > MAX_SOURCE_BYTES {
        return None;
    }
    let bytes = std::fs::read(&file.path).ok()?;
    match file.format {
        IconFormat::Png => rasterize_png(&bytes, device_px),
        IconFormat::Svg => rasterize_svg(&bytes, device_px),
    }
}

fn rasterize_png(bytes: &[u8], device_px: u32) -> Option<Raster> {
    let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Png).ok()?;
    let (width, height) = (image.width(), image.height());
    if width == 0 || height == 0 || width > MAX_DECODED_EDGE || height > MAX_DECODED_EDGE {
        return None;
    }
    let image = if width.max(height) > device_px {
        // Sampling a 256 px bitmap straight into 14 px aliases badly; a
        // filtered downscale is what every toolkit paints.
        image.resize(device_px, device_px, image::imageops::FilterType::Triangle)
    } else {
        image
    };
    let rgba = image.into_rgba8();
    let (width, height) = rgba.dimensions();
    Some(Raster {
        width,
        height,
        rgba: Arc::new(rgba.into_raw()),
    })
}

fn rasterize_svg(bytes: &[u8], device_px: u32) -> Option<Raster> {
    use resvg::{tiny_skia, usvg};
    let tree = usvg::Tree::from_data(bytes, &usvg::Options::default()).ok()?;
    let size = tree.size();
    let longest = size.width().max(size.height());
    if !longest.is_finite() || longest <= 0.0 {
        return None;
    }
    let edge = f32::from(u16::try_from(device_px).ok()?);
    let scale = edge / longest;
    let width = dimension(size.width() * scale)?;
    let height = dimension(size.height() * scale)?;
    let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let rgba = pixmap
        .pixels()
        .iter()
        .flat_map(|pixel| {
            let color = pixel.demultiply();
            [color.red(), color.green(), color.blue(), color.alpha()]
        })
        .collect();
    Some(Raster {
        width,
        height,
        rgba: Arc::new(rgba),
    })
}

/// A rendered edge in whole pixels, at least one.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a finite edge checked to lie in 1..=MAX_DEVICE_PX"
)]
fn dimension(edge: f32) -> Option<u32> {
    let edge = edge.ceil();
    if !edge.is_finite() || edge < 1.0 {
        return Some(1);
    }
    (edge <= f32::from(u16::try_from(ui::icons::MAX_DEVICE_PX).ok()?)).then_some(edge as u32)
}

/// Where a name, a probe or a rasterisation stands.
#[derive(Clone)]
enum Slot<T> {
    /// Queued on, or running on, the resolver's thread.
    Pending,
    Ready(T),
}

/// What the resolver knows, valid for one generation of the application
/// scan: installing or removing an application is when icons appear.
#[derive(Default)]
struct Known {
    generation: u64,
    /// Theme name a window's `app_id` or a desktop-file id resolves to.
    names: HashMap<String, Slot<Option<String>>>,
    /// Whether any theme has artwork for a theme name.
    probes: HashMap<String, Slot<bool>>,
    /// A theme name rasterised at a device size.
    pixels: HashMap<(String, u32), Slot<Option<ImageData>>>,
}

type Job = Box<dyn FnOnce(&Resolver) + Send>;

/// Finds and decodes application artwork on a thread of its own.
///
/// Chrome asks for icons on the compositor's loop: each tab pill, palette
/// row and window preview, every time it is built and painted. Finding an
/// icon walks the theme directories (a `statx` per candidate), naming one
/// reads the application scan, and decoding reads the file; under I/O
/// pressure any of these blocks for seconds, and the desktop froze in
/// them. So the loop only reads what is known: something not known yet is
/// queued here and is absent meanwhile, which every caller already paints
/// as its glyph or letter badge. Once an answer adds artwork, the loop is
/// woken and told ([`take_arrived`]) to build its chrome again.
struct Resolver {
    lookup: Box<dyn Fn() -> Arc<IconLookup> + Send + Sync>,
    generation: Box<dyn Fn() -> u64 + Send + Sync>,
    known: Mutex<Known>,
    jobs: Mutex<Option<mpsc::Sender<Job>>>,
    arrived: AtomicBool,
    wake: Mutex<Option<crate::session_runtime::Wake>>,
}

impl Resolver {
    fn new(
        lookup: impl Fn() -> Arc<IconLookup> + Send + Sync + 'static,
        generation: impl Fn() -> u64 + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            lookup: Box::new(lookup),
            generation: Box::new(generation),
            known: Mutex::new(Known::default()),
            jobs: Mutex::new(None),
            arrived: AtomicBool::new(false),
            wake: Mutex::new(None),
        })
    }

    /// What is known, for the current generation.
    fn known(&self) -> std::sync::MutexGuard<'_, Known> {
        let generation = (self.generation)();
        let mut known = self.known.lock_recover();
        if known.generation != generation {
            *known = Known {
                generation,
                ..Known::default()
            };
        }
        known
    }

    /// Run `job` on the resolver's thread, started on first use. The
    /// thread holds the resolver weakly and ends with it.
    fn submit(self: &Arc<Self>, job: Job) {
        let mut jobs = self.jobs.lock_recover();
        if jobs.is_none() {
            let (sender, receiver) = mpsc::channel::<Job>();
            let resolver = Arc::downgrade(self);
            let spawned = std::thread::Builder::new()
                .name("app-icons".into())
                .spawn(move || {
                    while let Ok(job) = receiver.recv() {
                        let Some(resolver) = resolver.upgrade() else {
                            return;
                        };
                        job(&resolver);
                    }
                });
            if let Err(error) = spawned {
                // Without its thread nothing resolves: icons stay absent and
                // the glyphs stand in, rather than the loop reading the disk.
                tracing::warn!(%error, "app icons: could not start the resolver thread");
                return;
            }
            *jobs = Some(sender);
        }
        if let Some(sender) = jobs.as_ref() {
            let _ = sender.send(job);
        }
    }

    /// Record an answer worked out for `generation`; `artwork` says whether
    /// it gives the loop something new to paint.
    fn answer(&self, generation: u64, artwork: bool, record: impl FnOnce(&mut Known)) {
        let mut known = self.known();
        if known.generation != generation {
            // The scan changed meanwhile: a later ask resolves anew.
            return;
        }
        record(&mut known);
        drop(known);
        if artwork {
            self.arrived.store(true, Ordering::Release);
            let wake = self.wake.lock_recover().clone();
            if let Some(wake) = wake {
                wake();
            }
        }
    }

    /// The artwork for theme name `name`: `None` when no theme has it, or
    /// while that is being found out.
    fn themed(self: &Arc<Self>, name: &str) -> Option<AppImage> {
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        let generation = {
            let mut known = self.known();
            match known.probes.get(name) {
                Some(Slot::Ready(true)) => {
                    return Some(AppImage::new(ThemedIcon {
                        key: format!("theme:{name}"),
                        name: name.to_string(),
                        resolver: Arc::clone(self),
                    }));
                }
                Some(Slot::Ready(false) | Slot::Pending) => return None,
                None => {}
            }
            known.probes.insert(name.to_string(), Slot::Pending);
            known.generation
        };
        let name = name.to_string();
        self.submit(Box::new(move |resolver| {
            let found = (resolver.lookup)().find(&name, PROBE_PX, 1).is_some();
            resolver.answer(generation, found, |known| {
                known.probes.insert(name, Slot::Ready(found));
            });
        }));
        None
    }

    /// `name` rasterised `device_px` square, looked up again per size so the
    /// theme's best directory for it is used; `None` until it is decoded.
    fn pixels(self: &Arc<Self>, name: &str, device_px: u32) -> Option<ImageData> {
        let key = (name.to_string(), device_px);
        let generation = {
            let mut known = self.known();
            match known.pixels.get(&key) {
                Some(Slot::Ready(image)) => return image.clone(),
                Some(Slot::Pending) => return None,
                None => {}
            }
            if known.pixels.len() >= RASTER_CACHE_LIMIT {
                known.pixels.retain(|_, slot| matches!(slot, Slot::Pending));
            }
            known.pixels.insert(key.clone(), Slot::Pending);
            known.generation
        };
        self.submit(Box::new(move |resolver| {
            let image = (resolver.lookup)()
                .find(&key.0, key.1, 1)
                .and_then(|file| rasterize(&file, key.1))
                .map(|raster| raster.image_data());
            resolver.answer(generation, image.is_some(), |known| {
                known.pixels.insert(key, Slot::Ready(image));
            });
        }));
        None
    }

    /// The theme name `key` resolves to through `resolve`, which reads the
    /// application scan, so it runs on the resolver's thread; that thread
    /// also finds out whether the name has artwork.
    fn named(
        self: &Arc<Self>,
        key: String,
        resolve: impl FnOnce() -> Option<String> + Send + 'static,
    ) -> Option<AppImage> {
        let generation = {
            let mut known = self.known();
            match known.names.get(&key) {
                Some(Slot::Ready(name)) => {
                    let name = name.clone();
                    drop(known);
                    return name.and_then(|name| self.themed(&name));
                }
                Some(Slot::Pending) => return None,
                None => {}
            }
            known.names.insert(key.clone(), Slot::Pending);
            known.generation
        };
        self.submit(Box::new(move |resolver| {
            let name = resolve()
                .map(|name| name.trim().to_string())
                .filter(|name| !name.is_empty());
            let found = name
                .as_deref()
                .is_some_and(|name| (resolver.lookup)().find(name, PROBE_PX, 1).is_some());
            resolver.answer(generation, found, |known| {
                if let Some(name) = name.clone() {
                    known.probes.insert(name, Slot::Ready(found));
                }
                known.names.insert(key, Slot::Ready(name));
            });
        }));
        None
    }

    /// Wait until everything queued so far has been answered.
    #[cfg(test)]
    fn settle(self: &Arc<Self>) {
        let (done, finished) = mpsc::channel();
        self.submit(Box::new(move |_| {
            let _ = done.send(());
        }));
        let _ = finished.recv();
    }
}

fn resolver() -> &'static Arc<Resolver> {
    static RESOLVER: OnceLock<Arc<Resolver>> = OnceLock::new();
    RESOLVER
        .get_or_init(|| Resolver::new(icon_theme::system_lookup, host_apps::host_apps_generation))
}

/// Wake the compositor's loop when artwork it asked for has arrived.
pub fn set_wake(wake: crate::session_runtime::Wake) {
    *resolver().wake.lock_recover() = Some(wake);
}

/// Whether artwork the chrome asked for has arrived since the last call:
/// the chrome built without it should be built again.
pub fn take_arrived() -> bool {
    resolver().arrived.swap(false, Ordering::AcqRel)
}

/// An icon-theme name, looked up again for each device size so the theme's
/// best directory for that size is used.
struct ThemedIcon {
    name: String,
    key: String,
    resolver: Arc<Resolver>,
}

impl AppImageSource for ThemedIcon {
    fn key(&self) -> &str {
        &self.key
    }

    fn pixels(&self, device_px: u32) -> Option<ImageData> {
        self.resolver.pixels(&self.name, device_px)
    }
}

/// The artwork for icon-theme name (or absolute path) `name`, or `None`
/// when no theme has it or while that is being found out. Never touches
/// the disk on the calling thread.
pub fn themed(name: &str) -> Option<AppImage> {
    resolver().themed(name)
}

/// The artwork of the host application with desktop-file id `desktop_id`.
pub fn for_desktop_id(desktop_id: &str) -> Option<AppImage> {
    let id = desktop_id.to_string();
    resolver().named(format!("desktop:{desktop_id}"), move || {
        host_apps::find(&id).and_then(|app| app.icon)
    })
}

/// The artwork for a window announcing `app_id`: its desktop entry's icon,
/// else a theme icon named like the `app_id` itself — what most Wayland
/// applications with no desktop entry of their own install.
pub fn for_window_app_id(app_id: &str) -> Option<AppImage> {
    let app_id = app_id.trim();
    if app_id.is_empty() {
        return None;
    }
    let id = app_id.to_string();
    resolver().named(format!("window:{app_id}"), move || {
        host_apps::app_for_window_id(&id)
            .and_then(|app| app.icon)
            .or(Some(id))
    })
}

/// The artwork for window `surface_id`: the icon it set for itself through
/// `xdg_toplevel_icon_v1`, else its application's.
pub fn for_window(surface_id: u64, app_id: &str) -> Option<AppImage> {
    #[cfg(target_os = "linux")]
    if let Some(own) = crate::compositor::toplevel_icon::app_image(surface_id) {
        return Some(own);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = surface_id;
    for_window_app_id(app_id)
}

/// The artwork a tab for `tile` shows in place of its glyph.
pub fn for_tile(tile: &crate::shell::state::TileContent) -> Option<AppImage> {
    match tile {
        crate::shell::state::TileContent::Foreign { app_id, surface_id } => {
            for_window(*surface_id, app_id)
        }
        crate::shell::state::TileContent::LocalApp { app_id, .. }
        | crate::shell::state::TileContent::PeerWindow { app_id, .. }
        | crate::shell::state::TileContent::HostedWindow { app_id, .. } => {
            for_window_app_id(app_id)
        }
        crate::shell::state::TileContent::AppPlaceholder { .. }
        | crate::shell::state::TileContent::Stub { .. }
        | crate::shell::state::TileContent::WorkspaceActivity => None,
    }
}

/// The glyph standing in for a tab's app when it has no artwork: the app's
/// own symbol for a Tilo app, the generic window for anything else.
pub fn glyph_for_tile(tile: &crate::shell::state::TileContent) -> ui::icons::Icon {
    match tile {
        crate::shell::state::TileContent::LocalApp { app_id, .. }
        | crate::shell::state::TileContent::PeerWindow { app_id, .. }
        | crate::shell::state::TileContent::HostedWindow { app_id, .. } => {
            ui::icons::app_glyph(app_id, None)
        }
        _ => ui::icons::Icon::AppWindow,
    }
}

/// A named icon for the tray, looked up first under the item's own
/// `theme_path`, rasterised `device_px` square.
pub fn tray_raster(name: &str, theme_path: Option<&str>, device_px: u32) -> Option<Raster> {
    let file = match theme_path.map(str::trim).filter(|path| !path.is_empty()) {
        Some(path) => IconLookup::new(
            icon_theme::IconSearchPath::system().with_first_base(PathBuf::from(path)),
            icon_theme::DEFAULT_THEME,
        )
        .find(name, device_px, 1)?,
        None => icon_theme::system_lookup().find(name, device_px, 1)?,
    };
    rasterize(&file, device_px)
}

#[cfg(test)]
mod tests {
    use super::*;
    use desktop_entry::icon_theme::IconSearchPath;

    const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><rect width="16" height="16" fill="#ff0000"/></svg>"##;

    fn png(path: &std::path::Path, edge: u32) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        image::RgbaImage::from_pixel(edge, edge, image::Rgba([0, 0, 255, 255]))
            .save_with_format(path, image::ImageFormat::Png)
            .unwrap();
    }

    fn svg(path: &std::path::Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, SVG).unwrap();
    }

    #[test]
    fn an_svg_is_rendered_at_the_requested_device_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.svg");
        svg(&path);
        let file = IconFile {
            path,
            format: IconFormat::Svg,
        };
        for px in [14, 28, 21] {
            let raster = rasterize(&file, px).expect("renders");
            assert_eq!((raster.width, raster.height), (px, px));
            assert_eq!(raster.rgba.get(0..4), Some(&[255, 0, 0, 255][..]));
        }
    }

    #[test]
    fn a_large_png_is_downscaled_and_a_small_one_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.png");
        png(&path, 256);
        let file = IconFile {
            path,
            format: IconFormat::Png,
        };
        let raster = rasterize(&file, 28).unwrap();
        assert_eq!((raster.width, raster.height), (28, 28));
        assert_eq!(raster.rgba.get(0..4), Some(&[0, 0, 255, 255][..]));
        let small = rasterize(&file, 512).unwrap();
        assert_eq!(small.width, 256);
    }

    #[test]
    fn a_broken_icon_is_absent_rather_than_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.svg");
        std::fs::write(&path, "not svg").unwrap();
        let file = IconFile {
            path,
            format: IconFormat::Svg,
        };
        assert_eq!(rasterize(&file, 16), None);
    }

    /// A resolver over `bases` only, at a fixed scan generation.
    fn resolver_over(bases: Vec<PathBuf>) -> Arc<Resolver> {
        let lookup = Arc::new(IconLookup::new(
            IconSearchPath {
                bases,
                pixmaps: Vec::new(),
            },
            "Adwaita",
        ));
        Resolver::new(move || Arc::clone(&lookup), || 1)
    }

    #[test]
    fn a_themed_icon_rasterises_per_device_size_from_the_flatpak_export() {
        let dir = tempfile::tempdir().unwrap();
        let exports = dir.path().join("flatpak/exports/share/icons");
        png(&exports.join("hicolor/16x16/apps/org.example.App.png"), 16);
        svg(&exports.join("hicolor/scalable/apps/org.example.App.svg"));
        let resolver = resolver_over(vec![exports]);
        assert!(
            resolver.themed("org.example.App").is_none(),
            "not known yet"
        );
        assert!(resolver.themed("org.example.Missing").is_none());
        resolver.settle();
        let image = resolver.themed("org.example.App").expect("found");
        assert_eq!(image.key(), "theme:org.example.App");
        assert!(image.pixels(16).is_none(), "not decoded yet");
        assert!(image.pixels(28).is_none(), "not decoded yet");
        resolver.settle();
        let small = image.pixels(16).unwrap();
        assert_eq!(small.width, 16);
        // No 28 px bitmap: the scalable SVG is rendered at 28.
        let large = image.pixels(28).unwrap();
        assert_eq!(large.width, 28);
        assert!(resolver.themed("org.example.Missing").is_none());
    }

    /// The compositor's loop asks for icons while it builds its chrome. A
    /// theme directory stuck in the disk (a `statx` that does not return)
    /// must not hold it: the ask comes back at once with nothing, the glyph
    /// stands in, and the loop is woken to build again once the artwork is
    /// there.
    #[test]
    fn an_icon_ask_never_waits_for_a_stuck_disk() {
        let dir = tempfile::tempdir().unwrap();
        let exports = dir.path().join("icons");
        png(&exports.join("hicolor/32x32/apps/org.example.Slow.png"), 32);
        let resolver = resolver_over(vec![exports]);
        let woken = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&woken);
        *resolver.wake.lock().unwrap() =
            Some(Arc::new(move || flag.store(true, Ordering::Release)));

        // The resolver's thread is stuck in the disk.
        let (release, stuck) = mpsc::channel::<()>();
        let (started, running) = mpsc::channel::<()>();
        resolver.submit(Box::new(move |_| {
            started.send(()).unwrap();
            let _ = stuck.recv();
        }));
        running.recv().unwrap();

        let asked = std::time::Instant::now();
        assert!(resolver.themed("org.example.Slow").is_none());
        assert!(
            resolver
                .named("window:org.example.Slow".into(), || Some(
                    "org.example.Slow".into()
                ))
                .is_none()
        );
        let waited = asked.elapsed();
        assert!(
            waited < std::time::Duration::from_millis(500),
            "asking for an icon waited {waited:?} for the disk"
        );
        assert!(!resolver.arrived.load(Ordering::Acquire));
        assert!(!woken.load(Ordering::Acquire));

        // The disk answers: the loop is woken and told to build again.
        release.send(()).unwrap();
        resolver.settle();
        assert!(resolver.arrived.swap(false, Ordering::AcqRel));
        assert!(woken.load(Ordering::Acquire));
        let image = resolver
            .named("window:org.example.Slow".into(), || {
                unreachable!("resolved once")
            })
            .expect("found");
        assert!(image.pixels(32).is_none());
        resolver.settle();
        assert_eq!(image.pixels(32).unwrap().width, 32);
    }

    #[test]
    fn a_tray_item_theme_path_is_searched_first() {
        let dir = tempfile::tempdir().unwrap();
        png(&dir.path().join("steam_tray_mono.png"), 22);
        let raster = tray_raster("steam_tray_mono", dir.path().to_str(), 44).expect("found");
        assert_eq!(raster.width, 22);
        assert!(tray_raster("../etc/passwd", dir.path().to_str(), 44).is_none());
    }
}
