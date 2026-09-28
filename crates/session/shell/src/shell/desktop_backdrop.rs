//! The desktop backdrop with this device's private wallpapers.
//!
//! Drawing is the shared [`DesktopBackdrop`]; this module supplies the
//! wallpaper each workspace shows, read from Tilo's own 0600 registry and
//! cache (the portal's preview file first). Wallpaper is per device.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};
pub use tilo_desktop::backdrop::DesktopBackdrop;
use vello::peniko::{Blob, ImageAlphaType, ImageData, ImageFormat as Format};

/// The backdrop for one local workspace, showing its native wallpaper.
pub fn native_backdrop(workspace_id: impl Into<String>) -> DesktopBackdrop {
    DesktopBackdrop::for_workspace(workspace_id).with_wallpaper_provider(native_wallpaper)
}

/// The wallpaper workspace `workspace_id` shows on this device, if any.
pub fn native_wallpaper(workspace_id: &str) -> Option<ImageData> {
    #[cfg(test)]
    crate::shell::test_support::ambient_start("workspace wallpaper lookup");
    crate::portal::desktop_services::wallpaper_preview_file_for_workspace(workspace_id)
        .and_then(|path| read_normalised_wallpaper(&path))
        .or_else(|| wallpaper_image_for_workspace(workspace_id, "background"))
}

#[derive(Default)]
struct WallpaperRenderCache {
    checked: Option<Instant>,
    stamp: Option<SystemTime>,
    records: Vec<crate::portal::desktop_services::WallpaperRecord>,
    images: HashMap<String, ImageData>,
}

fn wallpaper_render_cache() -> &'static Mutex<WallpaperRenderCache> {
    static CACHE: OnceLock<Mutex<WallpaperRenderCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(WallpaperRenderCache::default()))
}

/// Return a normalised, private wallpaper image for the named local workspace.
/// The cache only reads Tilo's 0600 registry and cache directory; it never
/// dereferences the original document URI.
pub fn wallpaper_image_for_workspace(workspace_id: &str, target: &str) -> Option<ImageData> {
    let path = crate::portal::desktop_services::wallpaper_state_path();
    let cache_dir = crate::portal::desktop_services::wallpaper_cache_dir_for_renderer(&path);
    let mut cache = wallpaper_render_cache().lock().ok()?;
    let now = Instant::now();
    if cache
        .checked
        .is_none_or(|checked| now.duration_since(checked) >= Duration::from_millis(250))
    {
        cache.checked = Some(now);
        let stamp = std::fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .ok();
        if stamp != cache.stamp {
            cache.stamp = stamp;
            cache.records = std::fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default();
            cache.images.clear();
        }
    }
    let file = crate::portal::desktop_services::wallpaper_record_for(
        &cache.records,
        workspace_id,
        target,
    )?
    .cache_file
    .clone();
    if !safe_cache_name(&file) {
        return None;
    }
    if !cache.images.contains_key(&file) {
        let image = read_normalised_wallpaper(&cache_dir.join(&file))?;
        cache.images.insert(file.clone(), image);
    }
    cache.images.get(&file).cloned()
}

fn safe_cache_name(name: &str) -> bool {
    name.len() == 68
        && std::path::Path::new(name)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
        && name
            .as_bytes()
            .get(..64)
            .is_some_and(|hash| hash.iter().all(u8::is_ascii_hexdigit))
}

fn read_normalised_wallpaper(path: &std::path::Path) -> Option<ImageData> {
    let decoded = ui::png::read_rgba8(path).ok()?;
    Some(ImageData {
        data: Blob::new(Arc::new(decoded.rgba)),
        format: Format::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width: decoded.width,
        height: decoded.height,
    })
}
