//! The X11 program behind a window, and its icon.
//!
//! X11 programs reach Tilo through `xwayland-satellite` (one per workspace,
//! [`crate::compositor::xwayland`]), which maps every X window to a toplevel
//! of its own single connection: to the compositor, every X11 window comes
//! from satellite's process. So a window satellite shows is traced back
//! through its X server, on a worker:
//!
//! - The X window is the mapped top-level window whose `WM_CLASS` class is
//!   the app id satellite gave the toplevel.
//! - Its process is what the X server itself says owns it (the X-Resource
//!   extension's `QueryClientIds` with the local client pid), never the
//!   window's own `_NET_WM_PID`, which any client can set to anything.
//! - Its icon is its `_NET_WM_ICON`, kept as the window's own icon
//!   ([`crate::compositor::toplevel_icon`]) exactly as a Wayland window's
//!   `xdg_toplevel_icon_v1` is, so its tab, search and the launcher show the
//!   program's real artwork.
//!
//! The traced process then goes where a Wayland window's does: to
//! [`crate::shell::downloaded_apps`], which may ask to add it. A window of
//! any other connection passes straight through.

use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, mpsc};

use tilo_lock::MutexRecover as _;
use vello::peniko::ImageData;
use x11rb::connection::{Connection as _, RequestConnection as _};
use x11rb::protocol::res::{ClientIdMask, ClientIdSpec, ConnectionExt as _};
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, MapState, Window};
use x11rb::rust_connection::RustConnection;

use crate::compositor::toplevel_icon::{self, ToplevelIcon};
use crate::shell::downloaded_apps::{self, ObservedWindow};

/// Largest icon edge kept, as for a Wayland window's own icon.
const MAX_EDGE: u32 = 512;
/// Most 32-bit units of `_NET_WM_ICON` read: a 512-pixel icon and smaller
/// ones beside it.
const MAX_ICON_UNITS: u32 = 1 << 19;

enum Job {
    Observe(ObservedWindow),
    /// Hand `reply` to [`downloaded_apps::settle_after`] once every window
    /// queued before it went on, so it answers after their looks ran.
    Settle(mpsc::Sender<()>),
}

/// A window named itself. Called on the compositor's loop: it only queues
/// the window for the worker.
pub fn observe_window(window: ObservedWindow) {
    let _ = worker().lock_recover().send(Job::Observe(window));
}

/// Answer `reply` once every window queued here before this call, and the
/// downloaded-apps look each one went on to, has run.
pub(crate) fn settle(reply: mpsc::Sender<()>) {
    let _ = worker().lock_recover().send(Job::Settle(reply));
}

fn worker() -> &'static Mutex<mpsc::Sender<Job>> {
    static WORKER: OnceLock<Mutex<mpsc::Sender<Job>>> = OnceLock::new();
    WORKER.get_or_init(|| {
        let (send, jobs) = mpsc::channel::<Job>();
        let spawned = std::thread::Builder::new()
            .name("tilo-x11-windows".into())
            .spawn(move || {
                let mut servers = Servers::default();
                while let Ok(job) = jobs.recv() {
                    match job {
                        Job::Observe(window) => run(window, &mut servers),
                        Job::Settle(reply) => downloaded_apps::settle_after(reply),
                    }
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "X11 programs will not be traced: no worker");
        }
        Mutex::new(send)
    })
}

fn run(window: ObservedWindow, servers: &mut Servers) {
    let Some((socket, cookie)) = crate::compositor::xwayland::bridged_display(window.pid) else {
        downloaded_apps::observe_window(window);
        return;
    };
    let Some(traced) = servers.trace(&socket, cookie, &window.app_id) else {
        return;
    };
    tracing::debug!(
        surface = window.surface_id,
        "traced an X11 window to its process {:?} with {} icon sizes",
        traced.pid,
        traced.icon.as_ref().map_or(0, |images| images.len()),
    );
    if let Some(images) = traced.icon {
        // The X window id names the icon: a later trace of the same window
        // finds it unchanged.
        let object = (1_u64 << 63) | u64::from(traced.window);
        if toplevel_icon::set(
            window.surface_id,
            object,
            Some(ToplevelIcon::Pixels(images)),
        ) {
            crate::app_icons::artwork_arrived();
        }
    }
    if let Some(pid) = traced.pid {
        downloaded_apps::observe_window(ObservedWindow {
            surface_id: window.surface_id,
            pid,
            pid_start_time: crate::shell::host_runtime::proc_stat(pid).map(|(_, start)| start),
            app_id: window.app_id,
        });
    }
}

/// What the X server says about the window behind a toplevel.
struct Traced {
    window: Window,
    /// The process owning it, when every window of that class belongs to
    /// one process.
    pid: Option<u32>,
    icon: Option<Arc<Vec<ImageData>>>,
}

/// One connection per X server, made on first use and made again after an
/// error.
#[derive(Default)]
struct Servers {
    connections: HashMap<PathBuf, RustConnection>,
}

impl Servers {
    fn trace(&mut self, socket: &PathBuf, cookie: [u8; 16], app_id: &str) -> Option<Traced> {
        for _ in 0..2 {
            if !self.connections.contains_key(socket) {
                match connect(socket, cookie) {
                    Ok(conn) => {
                        self.connections.insert(socket.clone(), conn);
                    }
                    Err(error) => {
                        tracing::debug!(%error, socket = %socket.display(), "X server unreachable");
                        return None;
                    }
                }
            }
            let conn = self.connections.get(socket)?;
            match trace(conn, app_id) {
                Ok(traced) => return traced,
                Err(error) => {
                    tracing::debug!(%error, "X server connection lost; connecting again");
                    self.connections.remove(socket);
                }
            }
        }
        None
    }
}

fn connect(socket: &PathBuf, cookie: [u8; 16]) -> Result<RustConnection, String> {
    let stream = UnixStream::connect(socket).map_err(|error| error.to_string())?;
    let (stream, _) = x11rb::rust_connection::DefaultStream::from_unix_stream(stream)
        .map_err(|error| error.to_string())?;
    RustConnection::connect_to_stream_with_auth_info(
        stream,
        0,
        b"MIT-MAGIC-COOKIE-1".to_vec(),
        cookie.to_vec(),
    )
    .map_err(|error| error.to_string())
}

type XResult<T> = Result<T, Box<dyn std::error::Error>>;

/// The mapped top-level windows of class `app_id`, their owner and the
/// first one's icon. `Ok(None)` when no such window is mapped.
fn trace(conn: &RustConnection, app_id: &str) -> XResult<Option<Traced>> {
    let root = conn
        .setup()
        .roots
        .first()
        .ok_or("the X server has no screen")?
        .root;
    let net_wm_icon = conn.intern_atom(false, b"_NET_WM_ICON")?.reply()?.atom;
    let mut matching = Vec::new();
    for child in conn.query_tree(root)?.reply()?.children {
        let Ok(attributes) = conn.get_window_attributes(child)?.reply() else {
            continue;
        };
        if attributes.map_state != MapState::VIEWABLE || attributes.override_redirect {
            continue;
        }
        let class = conn
            .get_property(false, child, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 64)?
            .reply()?
            .value;
        if wm_class(&class).as_deref() == Some(app_id) {
            matching.push(child);
        }
    }
    let Some(&first) = matching.first() else {
        return Ok(None);
    };
    let mut pids = Vec::new();
    for window in &matching {
        if let Some(pid) = owner(conn, *window)? {
            pids.push(pid);
        }
    }
    pids.sort_unstable();
    pids.dedup();
    let pid = match pids.as_slice() {
        [pid] => Some(*pid),
        _ => None,
    };
    let mut icon = None;
    for window in &matching {
        let reply = conn
            .get_property(
                false,
                *window,
                net_wm_icon,
                AtomEnum::CARDINAL,
                0,
                MAX_ICON_UNITS,
            )?
            .reply()?;
        let units: Vec<u32> = reply.value32().map(Iterator::collect).unwrap_or_default();
        let images = net_wm_icon_images(&units);
        if !images.is_empty() {
            icon = Some(Arc::new(images));
            break;
        }
    }
    Ok(Some(Traced {
        window: first,
        pid,
        icon,
    }))
}

/// The process owning `window`, as the X server knows it.
fn owner(conn: &RustConnection, window: Window) -> XResult<Option<u32>> {
    if conn
        .extension_information(x11rb::protocol::res::X11_EXTENSION_NAME)?
        .is_none()
    {
        return Ok(None);
    }
    let reply = conn
        .res_query_client_ids(&[ClientIdSpec {
            client: window,
            mask: ClientIdMask::LOCAL_CLIENT_PID,
        }])?
        .reply()?;
    Ok(reply
        .ids
        .iter()
        .find(|id| id.spec.mask.contains(ClientIdMask::LOCAL_CLIENT_PID))
        .and_then(|id| id.value.first().copied()))
}

/// The class half of a `WM_CLASS` value (`instance\0class\0`), as satellite
/// names the toplevel's app id; the instance when there is no class.
fn wm_class(value: &[u8]) -> Option<String> {
    let mut fields = value
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty());
    let instance = fields.next()?;
    let class = fields.next().unwrap_or(instance);
    Some(String::from_utf8_lossy(class).into_owned())
}

/// The square images in a `_NET_WM_ICON` value: repeated `width, height`
/// and then `width × height` ARGB pixels, straight alpha.
fn net_wm_icon_images(units: &[u32]) -> Vec<ImageData> {
    let mut images = Vec::new();
    let mut rest = units;
    while let [width, height, tail @ ..] = rest {
        let (width, height) = (*width, *height);
        let Some(count) = width
            .checked_mul(height)
            .and_then(|count| usize::try_from(count).ok())
        else {
            break;
        };
        let Some((pixels, after)) = tail.split_at_checked(count) else {
            break;
        };
        rest = after;
        if width == 0 || width != height || width > MAX_EDGE {
            continue;
        }
        let mut rgba = Vec::with_capacity(count.saturating_mul(4));
        for argb in pixels {
            let [alpha, red, green, blue] = argb.to_be_bytes();
            let premultiply = |channel: u8| {
                u16::from(channel)
                    .saturating_mul(u16::from(alpha))
                    .checked_div(255)
                    .and_then(|value| u8::try_from(value).ok())
                    .unwrap_or(u8::MAX)
            };
            rgba.extend_from_slice(&[
                premultiply(red),
                premultiply(green),
                premultiply(blue),
                alpha,
            ]);
        }
        images.push(ui::raster::rgba8_premultiplied(rgba, width, width));
    }
    images
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn satellite_names_a_window_by_its_class() {
        assert_eq!(wm_class(b"krita\0Krita\0").as_deref(), Some("Krita"));
        assert_eq!(wm_class(b"xterm\0").as_deref(), Some("xterm"));
        assert_eq!(wm_class(b""), None);
    }

    #[test]
    fn every_square_icon_is_kept_premultiplied_and_a_short_one_ends_the_list() {
        let mut units = vec![2, 2];
        units.extend([0xff_ff_00_00, 0x80_00_ff_00, 0x00_00_00_ff, 0xff_ff_ff_ff]);
        // Not square: skipped, and the next one still read.
        units.extend([2, 1, 0, 0]);
        units.extend([1, 1, 0xff_00_00_ff]);
        // Promises more pixels than follow.
        units.extend([4, 4, 0]);
        let images = net_wm_icon_images(&units);
        assert_eq!(
            images.iter().map(|image| image.width).collect::<Vec<_>>(),
            [2, 1]
        );
        let first = images[0].data.data();
        assert_eq!(&first[..4], [0xff, 0, 0, 0xff]);
        assert_eq!(&first[4..8], [0, 0x80, 0, 0x80], "half-transparent green");
        assert_eq!(&first[8..12], [0, 0, 0, 0], "transparent is all zero");
    }
}
