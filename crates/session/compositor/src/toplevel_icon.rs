//! Window icons an application sets for itself, through `xdg_toplevel_icon_v1`.
//!
//! A window may name a theme icon or hand over `wl_shm` pixels — how an
//! `AppImage`, a game or a browser profile window that no desktop entry
//! describes still gets its own artwork in its tab. The icon is
//! double-buffered on the surface; on each commit the pending one is applied
//! here, and what it resolves to rides along with the window's facts
//! ([`crate::policy::WindowInfo::icon`]).
//!
//! The protocol is served here rather than by smithay 0.7, whose cached icon
//! state keeps the protocol objects: a client that destroys its icon object
//! and then the buffer (legal, and what Chromium does on every window) both
//! loses its icon and is disconnected with `no_buffer`. Pixels are copied at
//! `add_buffer` — the protocol forbids changing them afterwards — so a set
//! icon outlives its protocol objects.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use smithay::reexports::wayland_protocols::xdg::toplevel_icon::v1::server::{
    xdg_toplevel_icon_manager_v1::{self, XdgToplevelIconManagerV1},
    xdg_toplevel_icon_v1::{self, XdgToplevelIconV1},
};
use smithay::reexports::wayland_server::backend::{ClientId, ObjectId};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::wayland::compositor::with_states;
use tilo_lock::MutexRecover as _;

use crate::state::State;

/// Edges the compositor tells clients it prefers: the tab pill at 1× to 3×.
pub const PREFERRED_SIZES: [i32; 3] = [16, 32, 48];

/// Largest icon edge kept; the protocol has no limit of its own.
const MAX_EDGE: u32 = 512;

const MANAGER_VERSION: u32 = 1;

/// One icon size a window offered, in premultiplied RGBA.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IconImage {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels; a window icon is square.
    pub height: u32,
    /// `width * height` pixels, four bytes each.
    pub pixels: Arc<Vec<u8>>,
}

/// What a window's own icon is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IconArtwork {
    /// A theme icon name, resolved like a desktop entry's `Icon=`.
    Named(String),
    /// The pixels the window handed over, one per size it offered.
    Pixels(Arc<[IconImage]>),
}

/// The icon a window set for itself, as its last commit left it.
#[derive(Debug)]
pub struct WindowIcon {
    serial: u64,
    artwork: IconArtwork,
}

impl WindowIcon {
    /// Distinguishes this icon from every other one set while the
    /// compositor runs: a window's icon changed exactly when this changed.
    pub const fn serial(&self) -> u64 {
        self.serial
    }

    /// What to draw, or the name of what to draw.
    pub const fn artwork(&self) -> &IconArtwork {
        &self.artwork
    }
}

impl PartialEq for WindowIcon {
    fn eq(&self, other: &Self) -> bool {
        self.serial == other.serial
    }
}

impl Eq for WindowIcon {}

/// The icon each window shows, by [`WindowInfo::id`].
#[derive(Debug, Default)]
pub struct Icons {
    shown: std::collections::HashMap<u64, Arc<WindowIcon>>,
}

impl Icons {
    /// The icon `window` shows, if it set one.
    pub fn of(&self, window: u64) -> Option<Arc<WindowIcon>> {
        self.shown.get(&window).cloned()
    }

    /// Record `icon` as what `window` shows; returns whether that changed.
    pub(crate) fn set(&mut self, window: u64, icon: Option<Arc<WindowIcon>>) -> bool {
        match icon {
            Some(icon) => {
                if self
                    .shown
                    .get(&window)
                    .is_some_and(|shown| same(Some(shown), Some(&icon)))
                {
                    return false;
                }
                self.shown.insert(window, icon);
                true
            }
            None => self.shown.remove(&window).is_some(),
        }
    }
}

fn next_serial() -> u64 {
    static SERIAL: AtomicU64 = AtomicU64::new(1);
    SERIAL.fetch_add(1, Ordering::Relaxed)
}

/// Whether two icons are the same picture, so a window that sets the icon
/// it already had changes nothing.
fn same(left: Option<&Arc<WindowIcon>>, right: Option<&Arc<WindowIcon>>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => Arc::ptr_eq(left, right) || left.artwork == right.artwork,
        (None, Some(_)) | (Some(_), None) => false,
    }
}

/// An X11 window's `_NET_WM_ICON`, as what a window shows.
pub(crate) fn x11_icon(icon: &crate::x11_windows::X11Icon) -> Arc<WindowIcon> {
    Arc::new(WindowIcon {
        serial: next_serial(),
        artwork: IconArtwork::Pixels(Arc::from([IconImage {
            width: icon.width,
            height: icon.height,
            pixels: Arc::new(icon.pixels.clone()),
        }])),
    })
}

/// The `xdg_toplevel_icon_manager_v1` global and the icon buffers clients
/// must keep alive while their icon object lives.
#[derive(Debug)]
pub struct ToplevelIconManager {
    /// `(buffer, icon)` for every buffer added to a live icon object.
    watched: Vec<(ObjectId, XdgToplevelIconV1)>,
}

impl ToplevelIconManager {
    /// Advertise the `xdg_toplevel_icon_manager_v1` global, telling clients
    /// the sizes the tab pill paints at.
    pub fn new(display: &DisplayHandle) -> Self {
        display.create_global::<State, XdgToplevelIconManagerV1, ()>(MANAGER_VERSION, ());
        Self {
            watched: Vec::new(),
        }
    }

    /// A client destroyed `buffer`: an icon object still holding it is the
    /// protocol's `no_buffer` error.
    pub(crate) fn buffer_destroyed(&mut self, buffer: &WlBuffer) {
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
    /// `(buffer, scale, image)`; the buffer id only identifies a replaced
    /// size so its no-longer-needed buffer stops being watched.
    images: Vec<(ObjectId, i32, IconImage)>,
}

/// User data of an `xdg_toplevel_icon_v1`.
#[derive(Debug, Default)]
pub struct IconObject {
    contents: Mutex<IconContents>,
    /// Set by `set_icon`; from then on the icon is immutable.
    frozen: std::sync::OnceLock<Option<Arc<WindowIcon>>>,
}

impl IconObject {
    /// The icon this object holds, or `None` when it holds neither pixels
    /// nor a name. Only the first answer counts: an icon is immutable once
    /// it is given to a toplevel.
    fn freeze(&self) -> Option<Arc<WindowIcon>> {
        self.frozen
            .get_or_init(|| {
                let contents = std::mem::take(&mut *self.contents.lock_recover());
                let artwork = if contents.images.is_empty() {
                    contents.name.map(IconArtwork::Named)?
                } else {
                    IconArtwork::Pixels(
                        contents
                            .images
                            .into_iter()
                            .map(|(_, _, image)| image)
                            .collect(),
                    )
                };
                Some(Arc::new(WindowIcon {
                    serial: next_serial(),
                    artwork,
                }))
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
    Set(Arc<WindowIcon>),
    /// A null icon, or one with neither pixels nor a name.
    Reset,
}

/// A toplevel's icon change, applied on its surface's next commit.
#[derive(Debug, Default)]
struct PendingIcon(Mutex<Option<IconChange>>);

/// Apply the icon `surface`'s commit made current: the window it belongs to
/// and the icon it now shows, when the commit changed it.
pub(crate) fn committed(surface: &WlSurface) -> Option<(u64, Option<Arc<WindowIcon>>)> {
    let pending = with_states(surface, |states| {
        states
            .data_map
            .get::<PendingIcon>()
            .and_then(|pending| pending.0.lock_recover().take())
    });
    let icon = match pending? {
        IconChange::Reset => None,
        IconChange::Set(icon) => Some(icon),
    };
    Some((crate::state::window_id_of(surface)?, icon))
}

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
                with_states(&surface, |states| {
                    states
                        .data_map
                        .insert_if_missing_threadsafe(PendingIcon::default);
                    if let Some(pending) = states.data_map.get::<PendingIcon>() {
                        *pending.0.lock_recover() = Some(change);
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
                data.contents.lock_recover().name = Some(icon_name);
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
                state.toplevel_icon.watch(&buffer, icon);
                let Some(image) = buffer_image(&buffer) else {
                    return;
                };
                let mut contents = data.contents.lock_recover();
                let replaced = contents
                    .images
                    .iter()
                    .position(|(_, s, existing)| *s == scale && existing.width == image.width);
                if let Some(at) = replaced {
                    let (old, _, _) = contents.images.remove(at);
                    state.toplevel_icon.unwatch_buffer(&old, &icon.id());
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
        state.toplevel_icon.unwatch_icon(&icon.id());
    }
}

/// The pixels of a square `wl_shm` icon buffer, in premultiplied RGBA.
fn buffer_image(buffer: &WlBuffer) -> Option<IconImage> {
    smithay::wayland::shm::with_buffer_contents(buffer, |pointer, length, data| {
        let (Ok(width), Ok(height), Ok(stride), Ok(offset)) = (
            u32::try_from(data.width),
            u32::try_from(data.height),
            u32::try_from(data.stride),
            u32::try_from(data.offset),
        ) else {
            return None;
        };
        if width == 0 || width != height || width > MAX_EDGE {
            return None;
        }
        let alpha = match data.format {
            wl_shm::Format::Argb8888 => None,
            wl_shm::Format::Xrgb8888 => Some(0xFF),
            _ => return None,
        };
        let row = width.checked_mul(4)?;
        if stride < row {
            return None;
        }
        let end = offset.checked_add(stride.checked_mul(height)?)?;
        if usize::try_from(end).ok()? > length {
            return None;
        }
        // SAFETY: the pool is mapped for `length` bytes and every range read
        // below ends within it.
        let bytes = unsafe { std::slice::from_raw_parts(pointer, length) };
        let mut pixels = Vec::with_capacity(usize::try_from(row.checked_mul(height)?).ok()?);
        for y in 0..height {
            let start = usize::try_from(offset.checked_add(y.checked_mul(stride)?)?).ok()?;
            let line = bytes.get(start..start.checked_add(usize::try_from(row).ok()?)?)?;
            for pixel in line.chunks_exact(4) {
                let [blue, green, red, opacity] = <[u8; 4]>::try_from(pixel).ok()?;
                pixels.push(red);
                pixels.push(green);
                pixels.push(blue);
                pixels.push(alpha.unwrap_or(opacity));
            }
        }
        Some(IconImage {
            width,
            height,
            pixels: Arc::new(pixels),
        })
    })
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(edge: u32) -> IconImage {
        IconImage {
            width: edge,
            height: edge,
            pixels: Arc::new(vec![
                0;
                usize::try_from(
                    edge.saturating_mul(edge).saturating_mul(4)
                )
                .unwrap()
            ]),
        }
    }

    #[test]
    fn an_icon_object_keeps_what_it_was_given_and_never_changes_again() {
        let object = IconObject::default();
        object.contents.lock().unwrap().name = Some("game".to_owned());
        object.contents.lock().unwrap().images = vec![
            (ObjectId::null(), 1, image(16)),
            (ObjectId::null(), 1, image(32)),
        ];
        let first = object.freeze().expect("pixels win over a name");
        assert!(matches!(first.artwork(), IconArtwork::Pixels(images) if images.len() == 2));
        assert!(object.is_frozen());
        object.contents.lock().unwrap().name = Some("other".to_owned());
        let second = object.freeze().expect("still the same icon");
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first, second);
    }

    #[test]
    fn an_icon_with_neither_pixels_nor_a_name_shows_nothing() {
        assert!(IconObject::default().freeze().is_none());
    }
}
