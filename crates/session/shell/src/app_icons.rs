//! Artwork for applications Tilo did not package.
//!
//! A host application names its icon (`Icon=` in its desktop entry); a
//! window names only its `app_id`. This module turns either into pixels for
//! the shell's chrome — the tab pill, the palette's application rows, the
//! tray's fallback — preferring a window's own `xdg_toplevel_icon_v1` icon,
//! and otherwise through the freedesktop icon-theme lookup in
//! [`desktop_entry::icon_theme`]. PNGs are decoded and downscaled, SVGs are
//! rasterised by `resvg` at the device size the chrome paints them at, and
//! both are cached, since chrome asks every frame.
//!
//! An icon that cannot be found or decoded is simply absent: every caller
//! keeps its own glyph or letter badge as the fallback.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use desktop_entry::icon_theme::{self, IconFile, IconFormat, IconLookup};
use ui::icons::{AppImage, AppImageSource};
use vello::peniko::{Blob, ImageAlphaType, ImageData, ImageFormat};

use crate::shell::host_apps::{self};

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
        ImageData {
            data: Blob::new(self.rgba.clone()),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: self.width,
            height: self.height,
        }
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

type RasterKey = (PathBuf, u32);

fn raster_cache() -> &'static Mutex<HashMap<RasterKey, Option<ImageData>>> {
    static CACHE: OnceLock<Mutex<HashMap<RasterKey, Option<ImageData>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cached_image(file: &IconFile, device_px: u32) -> Option<ImageData> {
    let key = (file.path.clone(), device_px);
    if let Some(hit) = raster_cache()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&key)
    {
        return hit.clone();
    }
    let image = rasterize(file, device_px).map(|raster| raster.image_data());
    let mut cache = raster_cache()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if cache.len() >= RASTER_CACHE_LIMIT {
        cache.clear();
    }
    cache.insert(key, image.clone());
    image
}

/// An icon-theme name, looked up again for each device size so the theme's
/// best directory for that size is used.
struct ThemedIcon {
    name: String,
    key: String,
    lookup: Arc<IconLookup>,
}

impl AppImageSource for ThemedIcon {
    fn key(&self) -> &str {
        &self.key
    }

    fn pixels(&self, device_px: u32) -> Option<ImageData> {
        let file = self.lookup.find(&self.name, device_px, 1)?;
        cached_image(&file, device_px)
    }
}

/// The artwork for icon-theme name (or absolute path) `name`, or `None`
/// when no theme has it.
pub fn themed(name: &str) -> Option<AppImage> {
    themed_in(icon_theme::system_lookup(), name)
}

fn themed_in(lookup: Arc<IconLookup>, name: &str) -> Option<AppImage> {
    let name = name.trim();
    lookup.find(name, PROBE_PX, 1)?;
    Some(AppImage::new(ThemedIcon {
        key: format!("theme:{name}"),
        name: name.to_string(),
        lookup,
    }))
}

/// Icon names already resolved, keyed by what asked, and valid for one
/// generation of the application scan.
struct NameMemo {
    generation: u64,
    names: HashMap<String, Option<String>>,
}

fn memoised_name(key: String, resolve: impl FnOnce() -> Option<String>) -> Option<String> {
    static MEMO: OnceLock<Mutex<NameMemo>> = OnceLock::new();
    let memo = MEMO.get_or_init(|| {
        Mutex::new(NameMemo {
            generation: 0,
            names: HashMap::new(),
        })
    });
    let generation = host_apps::host_apps_generation();
    {
        let mut guard = memo.lock().unwrap_or_else(PoisonError::into_inner);
        if guard.generation != generation {
            guard.generation = generation;
            guard.names.clear();
        }
        if let Some(hit) = guard.names.get(&key) {
            return hit.clone();
        }
    }
    // Resolved outside the lock: it scans the application directories.
    let name = resolve();
    memo.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .names
        .insert(key, name.clone());
    name
}

/// The artwork of the host application with desktop-file id `desktop_id`.
pub fn for_desktop_id(desktop_id: &str) -> Option<AppImage> {
    memoised_name(format!("desktop:{desktop_id}"), || {
        host_apps::find(desktop_id).and_then(|app| app.icon)
    })
    .and_then(|name| themed(&name))
}

/// The artwork for a window announcing `app_id`: its desktop entry's icon,
/// else a theme icon named like the `app_id` itself — what most Wayland
/// applications with no desktop entry of their own install.
pub fn for_window_app_id(app_id: &str) -> Option<AppImage> {
    let app_id = app_id.trim();
    if app_id.is_empty() {
        return None;
    }
    memoised_name(format!("window:{app_id}"), || {
        host_apps::app_for_window_id(app_id)
            .and_then(|app| app.icon)
            .or_else(|| Some(app_id.to_string()))
    })
    .and_then(|name| themed(&name))
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

    #[test]
    fn a_themed_icon_rasterises_per_device_size_from_the_flatpak_export() {
        let dir = tempfile::tempdir().unwrap();
        let exports = dir.path().join("flatpak/exports/share/icons");
        png(&exports.join("hicolor/16x16/apps/org.example.App.png"), 16);
        svg(&exports.join("hicolor/scalable/apps/org.example.App.svg"));
        let lookup = Arc::new(IconLookup::new(
            IconSearchPath {
                bases: vec![exports],
                pixmaps: Vec::new(),
            },
            "Adwaita",
        ));
        let image = themed_in(lookup.clone(), "org.example.App").expect("found");
        assert_eq!(image.key(), "theme:org.example.App");
        let small = image.pixels(16).unwrap();
        assert_eq!(small.width, 16);
        // No 28 px bitmap: the scalable SVG is rendered at 28.
        let large = image.pixels(28).unwrap();
        assert_eq!(large.width, 28);
        assert!(themed_in(lookup, "org.example.Missing").is_none());
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
