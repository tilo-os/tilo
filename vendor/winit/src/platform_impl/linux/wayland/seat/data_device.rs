//! Drag-and-drop *receive* over `wl_data_device`.
//!
//! Upstream winit never implemented this on Wayland: `DroppedFile` /
//! `HoveredFile` / `HoveredFileCancelled` are X11, macOS and Windows only.
//! qOS apps are Wayland clients of a compositor that starts drags from its
//! own chrome (a file dragged out of the built-in file manager), so the
//! events have to come from somewhere.
//!
//! What this does, per drag:
//!
//! * `enter` — pick one mime type from the offer ([`choose_mime`]), accept
//!   it, ask for a copy, and synthesise a `CursorMoved` for the window —
//!   the compositor sends `wl_data_device.motion`, not `wl_pointer.motion`,
//!   during a drag, and apps locate the drop from their tracked pointer.
//! * `motion` — another `CursorMoved`.
//! * `leave` without a drop — `HoveredFileCancelled`.
//! * `drop_performed` — `receive` the accepted mime on a pipe and read it to
//!   the end on a thread; the bytes come back through a calloop channel on
//!   the event-loop thread, where they become paths: a `text/uri-list` is
//!   decoded into its `file://` entries, anything else is spooled to a file
//!   under `$XDG_RUNTIME_DIR` (inside the app's sandbox, so it can read
//!   it). Each path is delivered as `HoveredFile` then `DroppedFile`, and
//!   the offer is finished.
//!
//! Bytes are preferred over a URI list when both are offered: a confined
//! app cannot read a path on the host, but it can always read what the
//! source wrote into the pipe.

use std::io::Read;
use std::path::PathBuf;

use calloop::channel::{self, Channel, Sender};
use sctk::compositor::SurfaceData;
use sctk::data_device_manager::data_device::{DataDevice, DataDeviceData, DataDeviceHandler};
use sctk::data_device_manager::data_offer::{DataOfferHandler, DragOffer};
use sctk::data_device_manager::data_source::DataSourceHandler;
use sctk::data_device_manager::{DataDeviceManagerState, WritePipe};
use sctk::reexports::client::globals::GlobalList;
use sctk::reexports::client::protocol::wl_data_device::WlDataDevice;
use sctk::reexports::client::protocol::wl_data_device_manager::DndAction;
use sctk::reexports::client::protocol::wl_data_source::WlDataSource;
use sctk::reexports::client::protocol::wl_seat::WlSeat;
use sctk::reexports::client::protocol::wl_surface::WlSurface;
use sctk::reexports::client::{Connection, Proxy, QueueHandle};

use crate::dpi::{LogicalPosition, PhysicalPosition};
use crate::event::WindowEvent;
use crate::platform_impl::wayland::state::WinitState;
use crate::platform_impl::wayland::{self, DeviceId, WindowId};

/// The mime type a list of `file://` URIs travels as.
pub const URI_LIST: &str = "text/uri-list";

/// Everything the receive side keeps between `enter` and the delivered
/// drop.
pub struct DndState {
    pub manager: DataDeviceManagerState,
    sender: Sender<DndPayload>,
    /// The drag currently over one of our windows.
    hover: Option<DndHover>,
    /// Counter for spool file names, so two drops in one session never
    /// collide.
    next_spool: u32,
}

struct DndHover {
    window_id: WindowId,
    mime: String,
    offer: DragOffer,
    dropped: bool,
}

/// What the reader thread hands back to the event loop.
pub struct DndPayload {
    window_id: WindowId,
    mime: String,
    bytes: Vec<u8>,
    offer: DragOffer,
}

impl DndState {
    /// Bind the data device manager. `None` when the compositor does not
    /// offer it, in which case drops are simply never delivered — the same
    /// behaviour winit had before this module.
    pub fn new(
        globals: &GlobalList,
        queue_handle: &QueueHandle<WinitState>,
    ) -> Option<(Self, Channel<DndPayload>)> {
        let manager = DataDeviceManagerState::bind(globals, queue_handle).ok()?;
        let (sender, channel) = channel::channel();
        Some((Self { manager, sender, hover: None, next_spool: 0 }, channel))
    }

    pub fn get_data_device(&self, queue_handle: &QueueHandle<WinitState>, seat: &WlSeat) -> DataDevice {
        self.manager.get_data_device(queue_handle, seat)
    }
}

/// Pick the mime type to receive. Bytes first — any `image/*` or
/// `application/*` — then a URI list. `None` when nothing is usable.
pub fn choose_mime(offered: &[String]) -> Option<String> {
    offered
        .iter()
        .find(|m| m.starts_with("image/") || m.starts_with("application/"))
        .or_else(|| offered.iter().find(|m| m.as_str() == URI_LIST))
        .cloned()
}

/// Decode a `text/uri-list` body into local paths. Lines starting with `#`
/// are comments; only `file://` URIs are kept; percent-encoding is
/// undone so a space in a file name survives the trip.
pub fn parse_uri_list(body: &[u8]) -> Vec<PathBuf> {
    let text = String::from_utf8_lossy(body);
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.strip_prefix("file://"))
        .map(|rest| {
            // `file://host/path` — drop an authority if one is present.
            let path = match rest.find('/') {
                Some(0) => rest,
                Some(i) => &rest[i..],
                None => rest,
            };
            PathBuf::from(percent_decode(path))
        })
        .collect()
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = &s[i + 1..i + 3];
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// File extension for a spooled byte payload, so the app can tell what it
/// was handed without sniffing.
pub fn extension_for_mime(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        "image/svg+xml" => "svg",
        "application/pdf" => "pdf",
        "text/plain" => "txt",
        _ => "bin",
    }
}

fn spool_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

impl WinitState {
    fn dnd_window_id(&self, surface: &WlSurface) -> Option<WindowId> {
        let parent = surface.data::<SurfaceData>().and_then(|d| d.parent_surface());
        let window_id = wayland::make_wid(parent.unwrap_or(surface));
        self.windows.borrow().contains_key(&window_id).then_some(window_id)
    }

    /// Synthesise a pointer position for `window_id` from the drag's
    /// surface-local coordinates.
    fn dnd_cursor_moved(&mut self, window_id: WindowId, x: f64, y: f64) {
        let scale_factor = match self.windows.borrow().get(&window_id) {
            Some(w) => w.lock().unwrap().scale_factor(),
            None => return,
        };
        let position: PhysicalPosition<f64> = LogicalPosition::new(x, y).to_physical(scale_factor);
        let device_id = crate::event::DeviceId(crate::platform_impl::DeviceId::Wayland(DeviceId));
        self.events_sink.push_window_event(WindowEvent::CursorMoved { device_id, position }, window_id);
        self.dispatched_events = true;
    }

    /// The event-loop side of a drop: turn the received bytes into paths
    /// and deliver them as winit events.
    pub fn deliver_dnd(&mut self, payload: DndPayload) {
        let DndPayload { window_id, mime, bytes, offer } = payload;
        let paths = if mime == URI_LIST {
            parse_uri_list(&bytes)
        } else {
            let n = match self.dnd.as_mut() {
                Some(d) => {
                    d.next_spool += 1;
                    d.next_spool
                },
                None => 0,
            };
            let path = spool_dir().join(format!(
                "winit-dnd-{}-{n}.{}",
                std::process::id(),
                extension_for_mime(&mime)
            ));
            match std::fs::write(&path, &bytes) {
                Ok(()) => vec![path],
                Err(err) => {
                    tracing::warn!("could not spool a dropped {mime} payload to {path:?}: {err}");
                    Vec::new()
                },
            }
        };
        for path in &paths {
            self.events_sink.push_window_event(WindowEvent::HoveredFile(path.clone()), window_id);
        }
        for path in paths {
            self.events_sink.push_window_event(WindowEvent::DroppedFile(path), window_id);
        }
        offer.finish();
        offer.destroy();
        self.dispatched_events = true;
    }
}

impl DataDeviceHandler for WinitState {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        data_device: &WlDataDevice,
        x: f64,
        y: f64,
        wl_surface: &WlSurface,
    ) {
        let Some(window_id) = self.dnd_window_id(wl_surface) else { return };
        let Some(offer) = data_device.data::<DataDeviceData>().and_then(|d| d.drag_offer()) else {
            return;
        };
        let mime = offer.with_mime_types(choose_mime);
        offer.accept_mime_type(offer.serial, mime.clone());
        let Some(mime) = mime else {
            // Nothing we can take; the compositor will cancel the drop.
            return;
        };
        offer.set_actions(DndAction::Copy, DndAction::Copy);
        if let Some(dnd) = self.dnd.as_mut() {
            dnd.hover = Some(DndHover { window_id, mime, offer, dropped: false });
        }
        self.dnd_cursor_moved(window_id, x, y);
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {
        let Some(hover) = self.dnd.as_mut().and_then(|d| d.hover.take()) else { return };
        if hover.dropped {
            // The drop is in flight on the reader thread; `deliver_dnd`
            // finishes the offer once the bytes are in.
            return;
        }
        self.events_sink.push_window_event(WindowEvent::HoveredFileCancelled, hover.window_id);
        self.dispatched_events = true;
    }

    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, x: f64, y: f64) {
        let Some(window_id) = self.dnd.as_ref().and_then(|d| d.hover.as_ref()).map(|h| h.window_id)
        else {
            return;
        };
        self.dnd_cursor_moved(window_id, x, y);
    }

    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {
        // The clipboard is handled elsewhere (`smithay-clipboard`); a
        // selection offer is not ours to consume here.
    }

    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {
        let Some(dnd) = self.dnd.as_mut() else { return };
        let Some(hover) = dnd.hover.as_mut() else { return };
        hover.dropped = true;
        let pipe = match hover.offer.receive(hover.mime.clone()) {
            Ok(pipe) => pipe,
            Err(err) => {
                tracing::warn!("could not receive the dropped {}: {err}", hover.mime);
                dnd.hover = None;
                return;
            },
        };
        let sender = dnd.sender.clone();
        let window_id = hover.window_id;
        let mime = hover.mime.clone();
        let offer = hover.offer.clone();
        // Read on a thread: the source writes as the compositor relays, and
        // blocking the event loop on it would also block the reply the
        // source is waiting for.
        let _ = std::thread::Builder::new().name("winit-dnd-read".into()).spawn(move || {
            let mut pipe = pipe;
            let mut bytes = Vec::new();
            if let Err(err) = pipe.read_to_end(&mut bytes) {
                tracing::warn!("reading the dropped {mime} failed: {err}");
                return;
            }
            let _ = sender.send(DndPayload { window_id, mime, bytes, offer });
        });
    }
}

impl DataOfferHandler for WinitState {
    fn source_actions(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}

    fn selected_action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}
}

/// We never create a data source — winit does not *start* drags — but the
/// delegate macro routes `wl_data_source` events here, so the trait has to
/// exist.
impl DataSourceHandler for WinitState {
    fn accept_mime(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: Option<String>) {}

    fn send_request(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: String, _: WritePipe) {}

    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}

sctk::delegate_data_device!(WinitState);

#[cfg(test)]
mod tests {
    use super::*;

    fn mimes(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn bytes_win_over_a_uri_list_when_both_are_offered() {
        assert_eq!(
            choose_mime(&mimes(&["text/uri-list", "image/png"])).as_deref(),
            Some("image/png")
        );
    }

    #[test]
    fn a_uri_list_alone_is_accepted() {
        assert_eq!(choose_mime(&mimes(&["text/uri-list"])).as_deref(), Some("text/uri-list"));
    }

    #[test]
    fn nothing_usable_is_refused() {
        assert_eq!(choose_mime(&mimes(&["text/plain", "x-special/thing"])), None);
    }

    #[test]
    fn uri_list_decodes_file_uris_and_skips_the_rest() {
        let body = b"# a comment\r\nfile:///home/me/photo%20one.png\r\nhttps://example.com/x\r\nfile://localhost/tmp/b.jpg\r\n";
        assert_eq!(
            parse_uri_list(body),
            vec![PathBuf::from("/home/me/photo one.png"), PathBuf::from("/tmp/b.jpg")]
        );
    }

    #[test]
    fn percent_decoding_leaves_bad_escapes_alone() {
        assert_eq!(percent_decode("a%zzb%4"), "a%zzb%4");
        assert_eq!(percent_decode("%41%42"), "AB");
    }

    #[test]
    fn spooled_payloads_get_an_extension_from_their_mime() {
        assert_eq!(extension_for_mime("image/png"), "png");
        assert_eq!(extension_for_mime("image/jpeg"), "jpg");
        assert_eq!(extension_for_mime("application/x-unknown"), "bin");
    }
}
