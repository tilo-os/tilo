//! Window icons an application sets for itself, through `xdg_toplevel_icon_v1`.
//!
//! A window may name a theme icon or hand over `wl_shm` pixels — how an
//! `AppImage`, a game or a browser profile window that no desktop entry
//! describes still gets its own artwork in its tab. The icon is
//! double-buffered on the surface; on each commit the current one is read
//! here, and what it resolves to is kept per surface for
//! [`crate::app_icons::for_window`] to prefer over the desktop entry's.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor as wl_compositor;
use smithay::wayland::xdg_toplevel_icon::{ToplevelIconCachedState, XdgToplevelIconManager};
use ui::icons::{AppImage, AppImageSource};
use vello::peniko::{Blob, ImageAlphaType, ImageData, ImageFormat};

/// Edges the compositor tells clients it prefers: the tab pill at 1× to 3×.
pub const PREFERRED_SIZES: [i32; 3] = [16, 32, 48];

/// Largest icon buffer edge kept; the protocol has no limit of its own.
const MAX_EDGE: u32 = 512;

/// The `xdg_toplevel_icon_manager_v1` global, telling clients the sizes the
/// tab pill paints at.
pub fn manager(
    display: &smithay::reexports::wayland_server::DisplayHandle,
) -> XdgToplevelIconManager {
    let mut manager = XdgToplevelIconManager::new::<crate::compositor::state::State>(display);
    for size in PREFERRED_SIZES {
        manager.add_icon_size(size);
    }
    manager
}

/// What a window's current icon is.
#[derive(Debug, Clone)]
pub enum ToplevelIcon {
    /// A theme icon name, resolved like a desktop entry's `Icon=`.
    Named(String),
    /// Client pixels, one image per size the client offered.
    Pixels(Arc<Vec<ImageData>>),
}

#[derive(Clone)]
struct Entry {
    /// The protocol icon object it came from: a new `set_icon` is a new
    /// object, so an unchanged commit is recognised without re-reading.
    object: u32,
    serial: u64,
    icon: ToplevelIcon,
}

fn store() -> &'static Mutex<HashMap<u64, Entry>> {
    static STORE: OnceLock<Mutex<HashMap<u64, Entry>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_serial() -> u64 {
    static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Record `icon` for `surface_id`, or forget it with `None`. Returns whether
/// that changed what the window shows.
pub fn set(surface_id: u64, object: u32, icon: Option<ToplevelIcon>) -> bool {
    let mut store = store().lock().unwrap_or_else(PoisonError::into_inner);
    match icon {
        Some(icon) => {
            if store
                .get(&surface_id)
                .is_some_and(|entry| entry.object == object)
            {
                return false;
            }
            store.insert(
                surface_id,
                Entry {
                    object,
                    serial: next_serial(),
                    icon,
                },
            );
            true
        }
        None => store.remove(&surface_id).is_some(),
    }
}

/// Forget a destroyed window's icon.
pub fn forget(surface_id: u64) {
    store()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&surface_id);
}

/// The icon a window set for itself, if any.
pub fn current(surface_id: u64) -> Option<ToplevelIcon> {
    store()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&surface_id)
        .map(|entry| entry.icon.clone())
}

/// Client pixels as an [`AppImage`]: the offered size nearest at-or-above
/// the painted one, else the largest.
struct ClientPixels {
    key: String,
    images: Arc<Vec<ImageData>>,
}

impl AppImageSource for ClientPixels {
    fn key(&self) -> &str {
        &self.key
    }

    fn pixels(&self, device_px: u32) -> Option<ImageData> {
        self.images
            .iter()
            .filter(|image| image.width >= device_px)
            .min_by_key(|image| image.width)
            .or_else(|| self.images.iter().max_by_key(|image| image.width))
            .cloned()
    }
}

/// The artwork for a window's own icon: pixels directly, a name through the
/// icon theme.
pub fn app_image(surface_id: u64) -> Option<AppImage> {
    let entry = store()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&surface_id)
        .cloned()?;
    match entry.icon {
        ToplevelIcon::Named(name) => crate::app_icons::themed(&name),
        ToplevelIcon::Pixels(images) => Some(AppImage::new(ClientPixels {
            key: format!("toplevel:{surface_id}:{}", entry.serial),
            images,
        })),
    }
}

/// Read `surface`'s committed icon into the store. Returns whether the
/// window's icon changed, so the caller can repaint its tab.
pub fn sync(surface: &WlSurface, surface_id: u64) -> bool {
    let read = wl_compositor::with_states(surface, |states| {
        let current = states
            .cached_state
            .get::<ToplevelIconCachedState>()
            .current()
            .clone();
        (
            current.icon_name().map(str::to_string),
            current.buffers().to_vec(),
        )
    });
    let (name, buffers) = read;
    if name.is_none() && buffers.is_empty() {
        return set(surface_id, 0, None);
    }
    // Every buffer and name of one icon object share its identity; the first
    // buffer's protocol id stands for it when there is no name to compare.
    let object = buffers
        .first()
        .map(|(buffer, _)| buffer.id().protocol_id())
        .unwrap_or_default()
        ^ name.as_deref().map_or(0, fold_name);
    let images: Vec<ImageData> = buffers
        .iter()
        .filter_map(|(buffer, _scale)| client_image(buffer))
        .collect();
    let icon = if images.is_empty() {
        name.map(ToplevelIcon::Named)
    } else {
        Some(ToplevelIcon::Pixels(Arc::new(images)))
    };
    set(surface_id, object, icon)
}

fn fold_name(name: &str) -> u32 {
    name.bytes().fold(0x811c_9dc5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    })
}

fn client_image(
    buffer: &smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer,
) -> Option<ImageData> {
    let mut rgba = Vec::new();
    let (width, height) =
        crate::compositor::surface_readback::buffer_to_rgba(buffer, &mut rgba).ok()?;
    if width == 0 || width != height || width > MAX_EDGE {
        return None;
    }
    Some(ImageData {
        data: Blob::new(Arc::new(rgba)),
        format: ImageFormat::Rgba8,
        // `wl_shm` ARGB is premultiplied.
        alpha_type: ImageAlphaType::AlphaPremultiplied,
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(edge: u32) -> ImageData {
        ImageData {
            data: Blob::new(Arc::new(vec![
                0;
                usize::try_from(edge.pow(2).saturating_mul(4))
                    .unwrap()
            ])),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::AlphaPremultiplied,
            width: edge,
            height: edge,
        }
    }

    #[test]
    fn the_offered_size_nearest_above_the_painted_one_is_used() {
        let surface = 0x7011_0001;
        let pixels = ToplevelIcon::Pixels(Arc::new(vec![square(16), square(48), square(32)]));
        assert!(set(surface, 7, Some(pixels.clone())));
        assert!(
            !set(surface, 7, Some(pixels)),
            "same icon object, no change"
        );
        let image = app_image(surface).expect("the window's icon");
        assert_eq!(image.pixels(14).map(|p| p.width), Some(16));
        assert_eq!(image.pixels(28).map(|p| p.width), Some(32));
        assert_eq!(image.pixels(96).map(|p| p.width), Some(48));
        assert!(image.key().starts_with("toplevel:"));
        forget(surface);
        assert!(app_image(surface).is_none());
    }
}
