//! What an X11 window says about its own program, and the focus X gives it.
//!
//! An X11 window carries its program's identity as window properties: its
//! class, the desktop start-up it belongs to and its icons. Those are read
//! here, on a connection of the session's own, and kept per window until the
//! window changes or goes away.
//!
//! Xwayland runs with a window manager, so the X input focus is the window
//! manager's to give: [`X11Windows::follow_keyboard`] keeps it on the X11
//! window that has the session's keyboard focus.

use std::collections::HashMap;

use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::xwayland::X11Surface;
use smithay::xwayland::xwm::X11Window;
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt as _, InputFocus, PropMode};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use crate::state::State;
use crate::{Error, io};

/// The biggest edge of an icon taken from a window: larger ones are there
/// for docks and file managers, and cost memory a tab does not need.
const MAX_EDGE: u32 = 512;

/// The most 32-bit units `_NET_WM_ICON` may hold: every size up to
/// [`MAX_EDGE`] square is far below it, and a hostile client cannot make the
/// session read a property that big.
const MAX_ICON_UNITS: u32 = 1 << 19;

/// What an X11 window says about its program.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Identity {
    /// The process id of the program that owns the window, from the X
    /// server's own knowledge of its clients: `_NET_WM_PID` is a claim by
    /// the client and is never used.
    pub pid: Option<u32>,
    /// `WM_CLASS`'s instance, the program's own name.
    pub instance: Option<String>,
    /// `WM_CLASS`'s class, the program's name for grouping windows.
    pub class: Option<String>,
    /// The desktop start-up that opened it (`_NET_STARTUP_ID`).
    pub startup_id: Option<String>,
    /// The largest of the window's icons, when it has one.
    pub icon: Option<Icon>,
}

/// An image an X11 window offers as its icon, in premultiplied RGBA.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Icon {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height` pixels, four bytes each.
    pub pixels: Vec<u8>,
}

/// The session's own X11 connection to Xwayland, and what it learned about
/// each window.
pub(crate) struct X11Windows {
    connection: RustConnection,
    icon: Atom,
    identities: HashMap<X11Window, Identity>,
    /// The window X input focus was last set to.
    focused: Option<X11Window>,
}

impl X11Windows {
    /// Connect to the X11 display `display`.
    pub(crate) fn connect(display: u32) -> Result<Self, Error> {
        let (connection, _) = x11rb::connect(Some(&format!(":{display}"))).map_err(io)?;
        let icon = connection
            .intern_atom(false, b"_NET_WM_ICON")
            .map_err(io)?
            .reply()
            .map_err(io)?
            .atom;
        Ok(Self {
            connection,
            icon,
            identities: HashMap::new(),
            focused: None,
        })
    }

    /// Read and keep what `window` says about its program.
    pub(crate) fn learn(&mut self, window: &X11Surface) {
        let id = window.window_id();
        let identity = Identity {
            pid: window.get_client_pid().ok().filter(|pid| *pid != 0),
            instance: Some(window.instance()).filter(|name| !name.is_empty()),
            class: Some(window.class()).filter(|name| !name.is_empty()),
            startup_id: window.startup_id().filter(|id| !id.is_empty()),
            icon: self.icon(id),
        };
        self.identities.insert(id, identity);
    }

    /// Drop what was learned about `window`.
    pub(crate) fn forget(&mut self, window: X11Window) {
        self.identities.remove(&window);
        if self.focused == Some(window) {
            self.focused = None;
        }
    }

    /// What `window` says about its program, once [`Self::learn`] ran.
    pub(crate) fn identity(&self, window: X11Window) -> Option<&Identity> {
        self.identities.get(&window)
    }

    /// Give X input focus to the X11 window the keyboard focus is on, and
    /// take it away when the focus is on anything else. Xwayland leaves the
    /// focus to the window manager, so without this a window never sees a
    /// `FocusIn` while the keyboard already reaches it.
    pub(crate) fn follow_keyboard(&mut self, state: &State) {
        let focus = state
            .seat
            .get_keyboard()
            .and_then(|keyboard| keyboard.current_focus());
        let window = focus.as_ref().and_then(|focus| {
            state
                .space
                .elements()
                .filter_map(|window| window.x11_surface())
                .find(|window| window.wl_surface().as_ref() == Some(focus))
                .map(X11Surface::window_id)
        });
        if window == self.focused {
            return;
        }
        let target = window.unwrap_or(x11rb::NONE);
        let set = self
            .connection
            .set_input_focus(InputFocus::NONE, target, x11rb::CURRENT_TIME)
            .map_err(io)
            .and_then(|cookie| cookie.check().map_err(io))
            .and_then(|()| self.connection.flush().map_err(io));
        if set.is_ok() {
            self.focused = window;
        }
    }

    /// The window's largest usable icon from `_NET_WM_ICON`.
    fn icon(&self, window: X11Window) -> Option<Icon> {
        let reply = self
            .connection
            .get_property(
                false,
                window,
                self.icon,
                AtomEnum::CARDINAL,
                0,
                MAX_ICON_UNITS,
            )
            .ok()?
            .reply()
            .ok()?;
        let data: Vec<u32> = reply.value32()?.collect();
        icon(&data)
    }
}

/// The largest square icon in a `_NET_WM_ICON` value that is small enough to
/// keep, as premultiplied RGBA.
fn icon(data: &[u32]) -> Option<Icon> {
    let mut best: Option<Icon> = None;
    let mut offset = 0_usize;
    while let Some(header) = data.get(offset..offset.checked_add(2)?) {
        let (width, height) = (header[0], header[1]);
        let pixels = width.checked_mul(height)?;
        offset = offset.checked_add(2)?;
        let end = offset.checked_add(usize::try_from(pixels).ok()?)?;
        let Some(pixels) = data.get(offset..end) else {
            // The value was cut off: nothing after it can be trusted.
            return best;
        };
        offset = end;
        if width != height || width == 0 || width > MAX_EDGE {
            continue;
        }
        if best.as_ref().is_some_and(|best| best.width >= width) {
            continue;
        }
        best = Some(Icon {
            width,
            height,
            pixels: premultiplied(pixels),
        });
    }
    best
}

/// ARGB pixels, as the X server stores them, to premultiplied RGBA.
fn premultiplied(pixels: &[u32]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(pixels.len().saturating_mul(4));
    for pixel in pixels {
        let alpha = (pixel >> 24) as u8;
        let scale = |channel: u32| -> u8 {
            let channel = u32::from((channel & 0xFF) as u8);
            u8::try_from((channel * u32::from(alpha) / 255) & 0xFF).unwrap_or(0)
        };
        rgba.push(scale(pixel >> 16));
        rgba.push(scale(pixel >> 8));
        rgba.push(scale(*pixel));
        rgba.push(alpha);
    }
    rgba
}

/// The `RESOURCE_MANAGER` text `resources` with `key` set to `value`, every
/// other line kept.
pub(crate) fn with_resource(resources: &str, key: &str, value: &str) -> String {
    let mut merged: String = resources
        .lines()
        .filter(|line| {
            line.split_once(':')
                .is_none_or(|(name, _)| name.trim() != key)
        })
        .flat_map(|line| [line, "\n"])
        .collect();
    merged.push_str(key);
    merged.push_str(":\t");
    merged.push_str(value);
    merged.push('\n');
    merged
}

/// Write `value` under `key` in the root window's `RESOURCE_MANAGER`.
pub(crate) fn set_resource(
    connection: &RustConnection,
    key: &str,
    value: &str,
) -> Result<(), Error> {
    let root = connection
        .setup()
        .roots
        .first()
        .ok_or_else(|| io("the X server has no screen"))?
        .root;
    let existing = connection
        .get_property(
            false,
            root,
            AtomEnum::RESOURCE_MANAGER,
            AtomEnum::STRING,
            0,
            u32::MAX,
        )
        .map_err(io)?
        .reply()
        .map_err(io)?
        .value;
    let merged = with_resource(&String::from_utf8_lossy(&existing), key, value);
    connection
        .change_property8(
            PropMode::REPLACE,
            root,
            AtomEnum::RESOURCE_MANAGER,
            AtomEnum::STRING,
            merged.as_bytes(),
        )
        .map_err(io)?;
    connection.flush().map_err(io)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_largest_square_icon_is_kept_as_premultiplied_rgba() {
        let data = [
            2,
            2,
            0x80FF_0000,
            0x0000_FF00,
            0x0000_00FF,
            0xFFFF_FFFF, // 2x2
            1,
            1,
            0xFFFF_FFFF, // 1x1
        ];
        let icon = icon(&data).expect("an icon");
        assert_eq!((icon.width, icon.height), (2, 2));
        assert_eq!(
            icon.pixels,
            vec![
                128, 0, 0, 128, // half-transparent red, premultiplied
                0, 0, 0, 0, // transparent green
                0, 0, 0, 0, // transparent blue
                255, 255, 255, 255, // opaque white
            ]
        );
    }

    #[test]
    fn icons_that_are_not_square_are_skipped() {
        let data = [3, 2, 1, 2, 3, 4, 5, 6, 1, 1, 0xFF00_0000];
        let icon = icon(&data).expect("an icon");
        assert_eq!((icon.width, icon.height), (1, 1));
    }

    #[test]
    fn a_cut_off_icon_value_yields_nothing_after_it() {
        let data = [8, 8, 1, 2, 3];
        assert_eq!(icon(&data), None);
    }

    #[test]
    fn icons_larger_than_a_tab_needs_are_skipped() {
        let data = [MAX_EDGE + 1, MAX_EDGE + 1];
        assert_eq!(icon(&data), None);
    }

    #[test]
    fn a_resource_replaces_only_its_own_line() {
        assert_eq!(
            with_resource("Xcursor.theme:\tAdwaita\nXft.dpi:\t96", "Xft.dpi", "192"),
            "Xcursor.theme:\tAdwaita\nXft.dpi:\t192\n"
        );
        assert_eq!(with_resource("", "Xft.dpi", "192"), "Xft.dpi:\t192\n");
    }
}
