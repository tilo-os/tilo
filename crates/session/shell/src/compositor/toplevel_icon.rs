//! Window icons an application sets for itself, through `xdg_toplevel_icon_v1`.
//!
//! A window may name a theme icon or hand over `wl_shm` pixels — how an
//! `AppImage`, a game or a browser profile window that no desktop entry
//! describes still gets its own artwork in its tab. The icon is
//! double-buffered on the surface; on each commit the pending one is applied
//! here, and what it resolves to is kept per surface for
//! [`crate::app_icons::for_window`] to prefer over the desktop entry's.
//!
//! The protocol is served here rather than by smithay 0.7, whose
//! `ShmBufferUserData::remove_destruction_hook` removes every hook except the
//! one asked for: a client that destroys its icon object and then the buffer
//! (legal, and what Chromium does on every window) was disconnected with
//! `no_buffer`. Pixels are copied at `add_buffer` — the protocol forbids
//! changing them afterwards — so a set icon outlives its protocol objects.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use smithay::reexports::wayland_protocols::xdg::toplevel_icon::v1::server::{
    xdg_toplevel_icon_manager_v1::{self, XdgToplevelIconManagerV1},
    xdg_toplevel_icon_v1::{self, XdgToplevelIconV1},
};
use smithay::reexports::wayland_server::backend::{ClientId, ObjectId};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::wayland::compositor as wl_compositor;
use ui::icons::{AppImage, AppImageSource};
use vello::peniko::{Blob, ImageAlphaType, ImageData, ImageFormat};

use crate::compositor::state::State;

/// Edges the compositor tells clients it prefers: the tab pill at 1× to 3×.
pub const PREFERRED_SIZES: [i32; 3] = [16, 32, 48];

/// Largest icon buffer edge kept; the protocol has no limit of its own.
const MAX_EDGE: u32 = 512;

const MANAGER_VERSION: u32 = 1;

/// The `xdg_toplevel_icon_manager_v1` global and the icon buffers clients
/// must keep alive while their icon object lives.
#[derive(Debug)]
pub struct ToplevelIconManager {
    /// `(buffer, icon)` for every buffer added to a live icon object.
    watched: Vec<(ObjectId, XdgToplevelIconV1)>,
}

/// Advertise the `xdg_toplevel_icon_manager_v1` global, telling clients the
/// sizes the tab pill paints at.
pub fn manager(display: &DisplayHandle) -> ToplevelIconManager {
    display.create_global::<State, XdgToplevelIconManagerV1, ()>(MANAGER_VERSION, ());
    ToplevelIconManager {
        watched: Vec::new(),
    }
}

impl ToplevelIconManager {
    /// A client destroyed `buffer`: an icon object still holding it is the
    /// protocol's `no_buffer` error.
    pub fn buffer_destroyed(&mut self, buffer: &WlBuffer) {
        let id = buffer.id();
        let mut offenders = Vec::new();
        self.watched.retain(|(watched, icon)| {
            if *watched != id {
                return true;
            }
            if icon.is_alive() {
                offenders.push(icon.clone());
            }
            false
        });
        for icon in offenders {
            icon.post_error(
                xdg_toplevel_icon_v1::Error::NoBuffer,
                "an icon buffer was destroyed before its xdg_toplevel_icon_v1",
            );
        }
    }

    fn watch(&mut self, buffer: &WlBuffer, icon: &XdgToplevelIconV1) {
        self.watched.push((buffer.id(), icon.clone()));
    }

    fn unwatch_buffer(&mut self, buffer: &ObjectId, icon: &ObjectId) {
        self.watched
            .retain(|(watched, owner)| watched != buffer || owner.id() != *icon);
    }

    fn unwatch_icon(&mut self, icon: &ObjectId) {
        self.watched.retain(|(_, owner)| owner.id() != *icon);
    }
}

/// What one icon object holds: pixels copied at `add_buffer`, and a name.
#[derive(Debug, Default)]
struct IconContents {
    name: Option<String>,
    /// `(buffer, scale, pixels)`; the buffer id only identifies a replaced
    /// size so its no-longer-needed buffer stops being watched.
    images: Vec<(ObjectId, i32, ImageData)>,
}

/// User data of an `xdg_toplevel_icon_v1`.
#[derive(Debug, Default)]
pub struct IconObject {
    contents: Mutex<IconContents>,
    /// Set by `set_icon`; from then on the icon is immutable.
    frozen: OnceLock<Option<Arc<FrozenIcon>>>,
}

/// An icon as assigned to a toplevel.
#[derive(Debug)]
struct FrozenIcon {
    serial: u64,
    icon: ToplevelIcon,
}

impl IconObject {
    fn freeze(&self) -> Option<Arc<FrozenIcon>> {
        self.frozen
            .get_or_init(|| {
                let contents = std::mem::take(
                    &mut *self.contents.lock().unwrap_or_else(PoisonError::into_inner),
                );
                let icon = if contents.images.is_empty() {
                    contents.name.map(ToplevelIcon::Named)
                } else {
                    Some(ToplevelIcon::Pixels(Arc::new(
                        contents
                            .images
                            .into_iter()
                            .map(|(_, _, image)| image)
                            .collect(),
                    )))
                };
                icon.map(|icon| {
                    Arc::new(FrozenIcon {
                        serial: next_serial(),
                        icon,
                    })
                })
            })
            .clone()
    }

    fn is_frozen(&self) -> bool {
        self.frozen.get().is_some()
    }
}

/// What `set_icon` asked for a toplevel.
#[derive(Debug)]
enum IconChange {
    Set(Arc<FrozenIcon>),
    /// A null icon, or one with neither pixels nor a name.
    Reset,
}

/// A toplevel's icon change, applied on its surface's next commit.
#[derive(Debug, Default)]
struct PendingIcon(Mutex<Option<IconChange>>);

impl GlobalDispatch<XdgToplevelIconManagerV1, ()> for State {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<XdgToplevelIconManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let manager = data_init.init(resource, ());
        for size in PREFERRED_SIZES {
            manager.icon_size(size);
        }
        manager.done();
    }
}

impl Dispatch<XdgToplevelIconManagerV1, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        _manager: &XdgToplevelIconManagerV1,
        request: xdg_toplevel_icon_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            xdg_toplevel_icon_manager_v1::Request::CreateIcon { id } => {
                data_init.init(id, IconObject::default());
            }
            xdg_toplevel_icon_manager_v1::Request::SetIcon { toplevel, icon } => {
                let Some(surface) = state
                    .xdg_shell_state
                    .get_toplevel(&toplevel)
                    .map(|toplevel| toplevel.wl_surface().clone())
                else {
                    return;
                };
                let change = icon
                    .as_ref()
                    .and_then(|icon| icon.data::<IconObject>())
                    .and_then(IconObject::freeze)
                    .map_or(IconChange::Reset, IconChange::Set);
                wl_compositor::with_states(&surface, |states| {
                    states
                        .data_map
                        .insert_if_missing_threadsafe(PendingIcon::default);
                    if let Some(pending) = states.data_map.get::<PendingIcon>() {
                        *pending.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(change);
                    }
                });
            }
            _ => {}
        }
    }
}

impl Dispatch<XdgToplevelIconV1, IconObject> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        icon: &XdgToplevelIconV1,
        request: xdg_toplevel_icon_v1::Request,
        data: &IconObject,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let immutable = || {
            icon.post_error(
                xdg_toplevel_icon_v1::Error::Immutable,
                "the icon has already been assigned to a toplevel",
            );
        };
        match request {
            xdg_toplevel_icon_v1::Request::SetName { icon_name } => {
                if data.is_frozen() {
                    immutable();
                    return;
                }
                data.contents
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .name = Some(icon_name);
            }
            xdg_toplevel_icon_v1::Request::AddBuffer { buffer, scale } => {
                if data.is_frozen() {
                    immutable();
                    return;
                }
                let edges = smithay::wayland::shm::with_buffer_contents(&buffer, |_, _, shm| {
                    (shm.width, shm.height)
                });
                let Ok((width, height)) = edges else {
                    icon.post_error(
                        xdg_toplevel_icon_v1::Error::InvalidBuffer,
                        "an icon buffer must be backed by wl_shm",
                    );
                    return;
                };
                if width != height {
                    icon.post_error(
                        xdg_toplevel_icon_v1::Error::InvalidBuffer,
                        "an icon buffer must be square",
                    );
                    return;
                }
                state.toplevel_icon_manager.watch(&buffer, icon);
                let Some(image) = client_image(&buffer) else {
                    return;
                };
                let mut contents = data.contents.lock().unwrap_or_else(PoisonError::into_inner);
                let replaced = contents
                    .images
                    .iter()
                    .position(|(_, s, existing)| *s == scale && existing.width == image.width);
                if let Some(at) = replaced {
                    let (old, _, _) = contents.images.remove(at);
                    state.toplevel_icon_manager.unwatch_buffer(&old, &icon.id());
                }
                contents.images.push((buffer.id(), scale, image));
            }
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        icon: &XdgToplevelIconV1,
        _data: &IconObject,
    ) {
        state.toplevel_icon_manager.unwatch_icon(&icon.id());
    }
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
    object: u64,
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
pub fn set(surface_id: u64, object: u64, icon: Option<ToplevelIcon>) -> bool {
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

/// Apply the icon `surface`'s commit made current. Returns whether the
/// window's icon changed, so the caller can repaint its tab.
pub fn sync(surface: &WlSurface, surface_id: u64) -> bool {
    let pending = wl_compositor::with_states(surface, |states| {
        states.data_map.get::<PendingIcon>().and_then(|pending| {
            pending
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
        })
    });
    match pending {
        None => false,
        Some(IconChange::Reset) => set(surface_id, 0, None),
        Some(IconChange::Set(frozen)) => set(surface_id, frozen.serial, Some(frozen.icon.clone())),
    }
}

fn client_image(buffer: &WlBuffer) -> Option<ImageData> {
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
