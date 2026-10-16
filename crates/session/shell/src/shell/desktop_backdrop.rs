//! The desktop backdrop with this device's private wallpapers.
//!
//! Drawing is the shared [`DesktopBackdrop`]; this module supplies the
//! wallpaper each workspace shows, read from Tilo's own 0600 registry and
//! cache (the portal's preview file first). Wallpaper is per device.
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};
pub use tilo_desktop::backdrop::DesktopBackdrop;
use vello::peniko::ImageData;

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

/// What a workspace's wallpaper is, as far as its temperature is concerned:
/// the preview file and its age, and the age of the wallpaper registry.
/// Cheap to read every few seconds; the image is decoded only when it moves.
type WallpaperStamp = (
    Option<std::path::PathBuf>,
    Option<SystemTime>,
    Option<SystemTime>,
);

fn wallpaper_stamp(workspace_id: &str) -> WallpaperStamp {
    let modified = |path: &std::path::Path| std::fs::metadata(path).ok()?.modified().ok();
    let preview =
        crate::portal::desktop_services::wallpaper_preview_file_for_workspace(workspace_id);
    let preview_modified = preview.as_deref().and_then(modified);
    let registry_modified = modified(&crate::portal::desktop_services::wallpaper_state_path());
    (preview, preview_modified, registry_modified)
}

/// The neutral temperature workspace `workspace_id` takes on this device:
/// its wallpaper's dominant hue ([`ui::workspace_hue`]), or `None` for a
/// workspace whose wallpaper has none, which stays neutral. Decodes the
/// wallpaper only when it changed since the last call for that workspace.
pub fn native_workspace_hue(workspace_id: &str) -> Option<f32> {
    type KnownHues = HashMap<String, (WallpaperStamp, Option<f32>)>;
    static HUES: OnceLock<Mutex<KnownHues>> = OnceLock::new();
    let stamp = wallpaper_stamp(workspace_id);
    let hues = HUES.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(cache) = hues.lock()
        && let Some((known, hue)) = cache.get(workspace_id)
        && *known == stamp
    {
        return *hue;
    }
    let hue = native_wallpaper(workspace_id).and_then(|image| ui::workspace_hue(image.data.data()));
    if let Ok(mut cache) = hues.lock() {
        cache.insert(workspace_id.to_owned(), (stamp, hue));
    }
    hue
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
    Some(ui::raster::rgba8(
        decoded.rgba,
        decoded.width,
        decoded.height,
    ))
}
