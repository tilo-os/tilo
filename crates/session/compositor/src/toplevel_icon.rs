//! Window icons an application sets for itself, through `xdg_toplevel_icon_v1`.
//!
//! A window may name a theme icon or hand over `wl_shm` pixels — how an
//! `AppImage`, a game or a browser profile window that no desktop entry
//! describes still gets its own artwork in its tab. The protocol is served by
//! smithay, which keeps what a window's icon holds on its surface, applied on
//! each commit; here that becomes the artwork a tab paints and rides along
//! with the window's facts ([`crate::policy::WindowInfo::icon`]).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::XdgToplevel;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::with_states;
use smithay::wayland::xdg_toplevel_icon::{
    ToplevelIconBuffer, ToplevelIconCachedState, XdgToplevelIconHandler, XdgToplevelIconManager,
};

use crate::state::State;

/// Edges the compositor tells clients it prefers: the tab pill at 1× to 3×.
const PREFERRED_SIZES: [i32; 3] = [16, 32, 48];

/// Largest icon edge kept; the protocol has no limit of its own.
const MAX_EDGE: u32 = 512;

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

/// A window set an icon for itself; it takes effect when the window's
/// surface commits.
#[derive(Debug, Default)]
struct PendingIcon(AtomicBool);

impl PendingIcon {
    /// Whether an icon was set since this was last asked, clearing it.
    fn take(&self) -> bool {
        self.0.swap(false, Ordering::Relaxed)
    }
}

impl XdgToplevelIconHandler for State {
    fn set_icon(&mut self, _toplevel: XdgToplevel, wl_surface: WlSurface) {
        with_states(&wl_surface, |states| {
            states
                .data_map
                .get_or_insert_threadsafe(PendingIcon::default)
                .0
                .store(true, Ordering::Relaxed);
        });
    }
}

/// Advertise the `xdg_toplevel_icon_manager_v1` global, telling clients the
/// sizes the tab pill paints at.
pub(crate) fn advertise(display: &DisplayHandle) {
    let mut manager = XdgToplevelIconManager::new::<State>(display);
    for size in PREFERRED_SIZES {
        manager.add_icon_size(size);
    }
}

/// Apply the icon `surface`'s commit made current: the window it belongs to
/// and the icon it now shows, when this commit changed it.
pub(crate) fn committed(surface: &WlSurface) -> Option<(u64, Option<Arc<WindowIcon>>)> {
    let pending = with_states(surface, |states| {
        states
            .data_map
            .get::<PendingIcon>()
            .is_some_and(PendingIcon::take)
    });
    if !pending {
        return None;
    }
    let window = crate::state::window_id_of(surface)?;
    let artwork = with_states(surface, |states| {
        let mut cached = states.cached_state.get::<ToplevelIconCachedState>();
        artwork(cached.current())
    });
    let icon = artwork.map(|artwork| {
        Arc::new(WindowIcon {
            serial: next_serial(),
            artwork,
        })
    });
    Some((window, icon))
}

/// What to draw for the icon a window has, or nothing when it set none.
fn artwork(state: &ToplevelIconCachedState) -> Option<IconArtwork> {
    let icon = state.icon()?;
    let images: Vec<IconImage> = icon.buffers().iter().filter_map(image).collect();
    if images.is_empty() {
        return icon
            .icon_name()
            .map(|name| IconArtwork::Named(name.to_owned()));
    }
    Some(IconArtwork::Pixels(Arc::from(images)))
}

/// One size of an icon as premultiplied RGBA, or nothing for pixels this
/// compositor does not paint.
fn image(buffer: &ToplevelIconBuffer) -> Option<IconImage> {
    let edge = |edge: i32| u32::try_from(edge).ok().filter(|edge| *edge > 0);
    let width = edge(buffer.width())?;
    let height = edge(buffer.height())?;
    if width != height || width > MAX_EDGE {
        return None;
    }
    let alpha = match buffer.format() {
        wl_shm::Format::Argb8888 => None,
        wl_shm::Format::Xrgb8888 => Some(0xFF),
        _ => return None,
    };
    let row = usize::try_from(width).ok()?.checked_mul(4)?;
    let stride = usize::try_from(buffer.stride()).ok()?;
    if stride < row {
        return None;
    }
    let pixels = buffer.pixels();
    let mut rgba = Vec::with_capacity(row.checked_mul(usize::try_from(height).ok()?)?);
    for line in 0..usize::try_from(height).ok()? {
        let start = line.checked_mul(stride)?;
        let line = pixels.get(start..start.checked_add(row)?)?;
        for pixel in line.chunks_exact(4) {
            let [blue, green, red, opacity] = <[u8; 4]>::try_from(pixel).ok()?;
            rgba.push(red);
            rgba.push(green);
            rgba.push(blue);
            rgba.push(alpha.unwrap_or(opacity));
        }
    }
    Some(IconImage {
        width,
        height,
        pixels: Arc::new(rgba),
    })
}
