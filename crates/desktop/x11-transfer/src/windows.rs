//! An X11 connection of its own that follows an X11 display's windows for what an X11 server for
//! Wayland does not carry across: a window's own icon (`_NET_WM_ICON`) and whether its program
//! still answers (`_NET_WM_PING`).
//!
//! It watches the root's children map and unmap (`SubstructureNotify`, which any number of
//! clients may select beside the window manager's redirect) and each mapped window's properties.
//! A client answers `_NET_WM_PING` by sending the message back to the root with
//! `SubstructureNotify | SubstructureRedirect`, so the answer reaches this connection as well as
//! the window manager's.
//!
//! Every call may wait for the X11 server's answer: an owner whose loop must never stall on X11
//! keeps the watch on a thread of its own.

use std::collections::HashMap;
use std::os::fd::{AsFd as _, OwnedFd};

use x11rb::connection::Connection as _;
use x11rb::errors::ReplyOrIdError;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ChangeWindowAttributesAux, ClientMessageEvent, ConnectionExt as _, EventMask,
    MapState, Window,
};
use x11rb::rust_connection::RustConnection;

x11rb::atom_manager! {
    /// The atoms the watch uses, interned once.
    Atoms: AtomsCookie {
        WM_PROTOCOLS,
        UTF8_STRING,
        _NET_WM_NAME,
        _NET_WM_ICON,
        _NET_WM_PING,
    }
}

/// The largest side of an icon image kept; bigger ones are left out.
const MAX_ICON_SIDE: u32 = 512;

/// One image of a window's icon: `size` × `size` pixels, premultiplied ARGB in the byte order of
/// Wayland's `argb8888` (blue, green, red, alpha), rows without padding.
pub struct IconImage {
    pub size: u32,
    pub pixels: Vec<u8>,
}

/// What happened to the display's windows.
pub enum WindowEvent {
    /// A top-level window was mapped: its class (`WM_CLASS`), title and icon, as an X11 server
    /// for Wayland names its toplevel from the first two.
    Mapped {
        window: Window,
        class: String,
        title: String,
        icon: Vec<IconImage>,
    },
    /// A mapped window's icon changed.
    Icon {
        window: Window,
        icon: Vec<IconImage>,
    },
    /// The window was unmapped or destroyed.
    Unmapped { window: Window },
    /// Every window pinged with this stamp answered, or went.
    Answered { stamp: u32 },
}

struct Watched {
    /// It lists `_NET_WM_PING` in `WM_PROTOCOLS`.
    pingable: bool,
}

/// The connection and the windows it follows.
pub struct WindowWatch {
    conn: RustConnection,
    atoms: Atoms,
    root: Window,
    windows: HashMap<Window, Watched>,
    /// The ping out, and the windows that have not answered it.
    ping: Option<(u32, Vec<Window>)>,
    /// Events read while waiting for a reply, handled with the next ones.
    pending: Vec<WindowEvent>,
}

impl WindowWatch {
    /// Connect to the X11 display `:display` (see [`crate::bridge::XdndBridge::connect`] for
    /// `cookie`) and take in the windows already mapped.
    ///
    /// # Errors
    ///
    /// The display refused or failed the connection.
    pub fn connect(display: u32, cookie: Option<&[u8]>) -> Result<Self, String> {
        let (conn, screen) = crate::bridge::connect(display, cookie)?;
        Self::new(conn, screen).map_err(|error| error.to_string())
    }

    fn new(conn: RustConnection, screen: usize) -> Result<Self, ReplyOrIdError> {
        let root = conn
            .setup()
            .roots
            .get(screen)
            .map(|screen| screen.root)
            .ok_or(ReplyOrIdError::IdsExhausted)?;
        let atoms = Atoms::new(&conn)?.reply()?;
        conn.change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::SUBSTRUCTURE_NOTIFY),
        )?
        .check()?;
        let children = conn.query_tree(root)?.reply()?.children;
        let mut watch = Self {
            conn,
            atoms,
            root,
            windows: HashMap::new(),
            ping: None,
            pending: Vec::new(),
        };
        for window in children {
            let viewable =
                watch
                    .conn
                    .get_window_attributes(window)?
                    .reply()
                    .is_ok_and(|attributes| {
                        attributes.map_state == MapState::VIEWABLE && !attributes.override_redirect
                    });
            if viewable {
                watch.mapped(window);
            }
        }
        Ok(watch)
    }

    /// The connection's descriptor, readable when events arrived.
    ///
    /// # Errors
    ///
    /// The descriptor could not be duplicated.
    pub fn fd(&self) -> std::io::Result<OwnedFd> {
        self.conn.stream().as_fd().try_clone_to_owned()
    }

    /// Ask every program with a window that answers pings whether it is alive, one window per
    /// program. Gives whether any was asked; [`WindowEvent::Answered`] tells when all answered.
    /// A ping already out is answered with this stamp instead.
    pub fn ping(&mut self, stamp: u32) -> bool {
        if let Some((out, _)) = &mut self.ping {
            *out = stamp;
            return true;
        }
        let mask = self.conn.setup().resource_id_mask;
        let mut programs = Vec::new();
        let mut asked = Vec::new();
        for (window, watched) in &self.windows {
            let program = window & !mask;
            if !watched.pingable || programs.contains(&program) {
                continue;
            }
            programs.push(program);
            let message = ClientMessageEvent::new(
                32,
                *window,
                self.atoms.WM_PROTOCOLS,
                [self.atoms._NET_WM_PING, stamp, *window, 0, 0],
            );
            if self
                .conn
                .send_event(false, *window, EventMask::NO_EVENT, message)
                .is_ok()
            {
                asked.push(*window);
            }
        }
        let _ = self.conn.flush();
        if asked.is_empty() {
            return false;
        }
        self.ping = Some((stamp, asked));
        true
    }

    /// Handle what arrived on the connection.
    pub fn events(&mut self) -> Vec<WindowEvent> {
        loop {
            match self.conn.poll_for_event() {
                Ok(Some(event)) => self.event(&event),
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!(%error, "X11 window connection failed");
                    break;
                }
            }
        }
        let _ = self.conn.flush();
        std::mem::take(&mut self.pending)
    }

    fn event(&mut self, event: &Event) {
        match event {
            Event::MapNotify(event) if event.event == self.root && !event.override_redirect => {
                self.mapped(event.window);
            }
            Event::UnmapNotify(event) if event.event == self.root => self.gone(event.window),
            Event::DestroyNotify(event) if event.event == self.root => self.gone(event.window),
            Event::PropertyNotify(event)
                if event.atom == self.atoms._NET_WM_ICON
                    && self.windows.contains_key(&event.window) =>
            {
                let icon = self.icon(event.window);
                self.pending.push(WindowEvent::Icon {
                    window: event.window,
                    icon,
                });
            }
            Event::PropertyNotify(event)
                if event.atom == self.atoms.WM_PROTOCOLS
                    && self.windows.contains_key(&event.window) =>
            {
                let pingable = self.pingable(event.window);
                if let Some(watched) = self.windows.get_mut(&event.window) {
                    watched.pingable = pingable;
                }
            }
            Event::ClientMessage(event)
                if event.window == self.root
                    && event.format == 32
                    && event.type_ == self.atoms.WM_PROTOCOLS =>
            {
                // A late answer to an earlier stamp still says the program is alive.
                let [protocol, _stamp, window, ..] = event.data.as_data32();
                if protocol == self.atoms._NET_WM_PING {
                    self.answered(window);
                }
            }
            _ => {}
        }
    }

    fn mapped(&mut self, window: Window) {
        if self.windows.contains_key(&window) {
            return;
        }
        // Its icon may change later; a window that is already gone fails here and its
        // destruction follows.
        let selected = self.conn.change_window_attributes(
            window,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        );
        if selected.is_err() {
            return;
        }
        let class = self
            .property(window, AtomEnum::WM_CLASS.into(), AtomEnum::STRING.into())
            .and_then(|value| {
                // Instance and class, each ended by a NUL.
                value
                    .split(|byte| *byte == 0)
                    .nth(1)
                    .map(|class| String::from_utf8_lossy(class).into_owned())
            })
            .unwrap_or_default();
        let title = self
            .property(window, self.atoms._NET_WM_NAME, self.atoms.UTF8_STRING)
            .or_else(|| self.property(window, AtomEnum::WM_NAME.into(), AtomEnum::ANY.into()))
            .map(|value| String::from_utf8_lossy(&value).into_owned())
            .unwrap_or_default();
        let icon = self.icon(window);
        let pingable = self.pingable(window);
        self.windows.insert(window, Watched { pingable });
        self.pending.push(WindowEvent::Mapped {
            window,
            class,
            title,
            icon,
        });
    }

    fn gone(&mut self, window: Window) {
        if self.windows.remove(&window).is_none() {
            return;
        }
        self.pending.push(WindowEvent::Unmapped { window });
        self.answered(window);
    }

    fn answered(&mut self, window: Window) {
        let Some((out, waiting)) = &mut self.ping else {
            return;
        };
        waiting.retain(|waiting| *waiting != window);
        if waiting.is_empty() {
            let stamp = *out;
            self.ping = None;
            self.pending.push(WindowEvent::Answered { stamp });
        }
    }

    fn pingable(&self, window: Window) -> bool {
        let Ok(cookie) = self.conn.get_property(
            false,
            window,
            self.atoms.WM_PROTOCOLS,
            AtomEnum::ATOM,
            0,
            64,
        ) else {
            return false;
        };
        cookie
            .reply()
            .ok()
            .and_then(|reply| {
                reply
                    .value32()
                    .map(|mut atoms| atoms.any(|atom| atom == self.atoms._NET_WM_PING))
            })
            .unwrap_or(false)
    }

    /// A property's bytes, up to 64 KiB.
    fn property(&self, window: Window, property: Atom, kind: Atom) -> Option<Vec<u8>> {
        let reply = self
            .conn
            .get_property(false, window, property, kind, 0, 16 * 1024)
            .ok()?
            .reply()
            .ok()?;
        (reply.type_ != x11rb::NONE).then_some(reply.value)
    }

    fn icon(&self, window: Window) -> Vec<IconImage> {
        let reply = self
            .conn
            .get_property(
                false,
                window,
                self.atoms._NET_WM_ICON,
                AtomEnum::CARDINAL,
                0,
                // Enough for the largest image kept, and smaller ones before it.
                2 * MAX_ICON_SIDE * MAX_ICON_SIDE,
            )
            .ok()
            .and_then(|cookie| cookie.reply().ok());
        let Some(words) = reply.as_ref().and_then(|reply| reply.value32()) else {
            return Vec::new();
        };
        icon_images(&words.collect::<Vec<u32>>())
    }
}

/// The images of a `_NET_WM_ICON` value (width, height, then the pixels as non-premultiplied
/// ARGB words, again for each image), squared and premultiplied.
#[must_use]
pub fn icon_images(words: &[u32]) -> Vec<IconImage> {
    let mut images = Vec::new();
    let mut rest = words;
    while let [width, height, tail @ ..] = rest {
        let (width, height) = (*width, *height);
        let Some((pixels, next)) = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|count| usize::try_from(count).ok())
            .and_then(|count| tail.split_at_checked(count))
        else {
            break;
        };
        rest = next;
        let size = width.max(height);
        if width == 0 || height == 0 || size > MAX_ICON_SIDE {
            continue;
        }
        images.push(square(pixels, width, height, size));
    }
    images
}

/// `pixels` (`width` × `height`, at most [`MAX_ICON_SIDE`] each) centred on a transparent `size`
/// × `size` square, premultiplied.
fn square(pixels: &[u32], width: u32, height: u32, size: u32) -> IconImage {
    let bytes = (size as usize)
        .saturating_mul(size as usize)
        .saturating_mul(4);
    let left = size.saturating_sub(width) / 2;
    let top = size.saturating_sub(height) / 2;
    let rows = pixels.chunks(width as usize);
    let mut out = Vec::with_capacity(bytes);
    let blank = |out: &mut Vec<u8>, count: u32| {
        out.extend(std::iter::repeat_n(0, (count as usize).saturating_mul(4)));
    };
    blank(&mut out, top.saturating_mul(size));
    for row in rows {
        blank(&mut out, left);
        for argb in row {
            let [blue, green, red, alpha] = argb.to_le_bytes();
            let premultiply = |colour: u8| {
                u8::try_from(u16::from(colour).saturating_mul(u16::from(alpha)) / 255)
                    .unwrap_or(u8::MAX)
            };
            out.extend([
                premultiply(blue),
                premultiply(green),
                premultiply(red),
                alpha,
            ]);
        }
        blank(&mut out, size.saturating_sub(left).saturating_sub(width));
    }
    out.resize(bytes, 0);
    IconImage { size, pixels: out }
}

#[cfg(test)]
mod tests {
    use super::icon_images;

    #[test]
    fn icon_images_are_squared_and_premultiplied() {
        // A 2×1 image (half-transparent red, opaque blue), then a truncated one.
        let words = [2, 1, 0x80ff_0000, 0xff00_00ff, 4, 4, 0];
        let images = icon_images(&words);
        assert_eq!(images.len(), 1);
        let image = &images[0];
        assert_eq!(image.size, 2);
        assert_eq!(
            image.pixels,
            [
                0, 0, 0x80, 0x80, 0xff, 0, 0, 0xff, // the image, on the square's top row
                0, 0, 0, 0, 0, 0, 0, 0, // the transparent bottom row
            ]
        );
    }
}
