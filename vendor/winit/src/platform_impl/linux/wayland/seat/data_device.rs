//! Drag-and-drop *receive* over `wl_data_device`.
//!
//! Upstream winit never implemented this on Wayland: `DroppedFile` /
//! `HoveredFile` / `HoveredFileCancelled` are X11, macOS and Windows only.
//! Tilo apps are Wayland clients of a compositor that starts drags from its
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
//!
//! The same module carries the *send* half — [`start_drag`] — so a `ui`
//! app can be the source of a drag rather than only its target. It offers
//! bytes for the same reason the receive side prefers them: the app on
//! the other end is confined, and a path from this one's namespace means
//! nothing in that one's. A file's bytes are read when a receiver asks
//! for them, on the thread that writes the pipe ([`DragData::File`]), so
//! starting a drag never reads a file, however large, on the app's thread.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use calloop::channel::{self, Channel, Sender};
use sctk::compositor::SurfaceData;
use sctk::data_device_manager::data_device::{DataDevice, DataDeviceData, DataDeviceHandler};
use sctk::data_device_manager::data_offer::{DataOfferHandler, DragOffer};
use sctk::data_device_manager::data_source::{DataSourceData, DataSourceHandler};
use sctk::data_device_manager::{DataDeviceManagerState, WritePipe};
use sctk::reexports::client::backend::ObjectId;
use sctk::reexports::client::globals::GlobalList;
use sctk::reexports::client::protocol::wl_data_device::WlDataDevice;
use sctk::reexports::client::protocol::wl_data_device_manager::{DndAction, WlDataDeviceManager};
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
        Some((
            Self {
                manager,
                sender,
                hover: None,
                next_spool: 0,
            },
            channel,
        ))
    }

    pub fn get_data_device(
        &self,
        queue_handle: &QueueHandle<WinitState>,
        seat: &WlSeat,
    ) -> DataDevice {
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

/// Where a dropped payload is written so the app can open it.
///
/// `$XDG_RUNTIME_DIR` first, which is the conventional answer and the
/// one the receiving app is most likely to recognise. It is not a
/// reliable answer inside a sandbox, though: the variable is set, but
/// the directory it names may be one the sandbox synthesised in order to
/// mount a socket into, with nothing writable about it. A drop that
/// lands there silently produces no file and the app is handed nothing,
/// which looks exactly like a drag that was never accepted.
///
/// So each candidate is *probed* rather than assumed, ending at the app's
/// own directory, which a Tilo app always has and always owns.
fn spool_dir() -> PathBuf {
    let candidates = [
        std::env::var_os("XDG_RUNTIME_DIR"),
        std::env::var_os("XDG_CACHE_HOME"),
        std::env::var_os("HOME"),
    ];
    for candidate in candidates.into_iter().flatten() {
        if candidate.is_empty() {
            continue;
        }
        let dir = PathBuf::from(candidate);
        if is_writable(&dir) {
            return dir;
        }
    }
    std::env::temp_dir()
}

/// Whether a file can actually be created in `dir`, asked by creating one.
///
/// The permission bits are not the whole story — a read-only mount says
/// nothing about them — so the question is put to the filesystem.
fn is_writable(dir: &std::path::Path) -> bool {
    let probe = dir.join(format!(".winit-dnd-probe-{}", std::process::id()));
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

impl WinitState {
    fn dnd_window_id(&self, surface: &WlSurface) -> Option<WindowId> {
        let parent = surface
            .data::<SurfaceData>()
            .and_then(|d| d.parent_surface());
        let window_id = wayland::make_wid(parent.unwrap_or(surface));
        self.windows
            .borrow()
            .contains_key(&window_id)
            .then_some(window_id)
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
        self.events_sink.push_window_event(
            WindowEvent::CursorMoved {
                device_id,
                position,
            },
            window_id,
        );
        self.dispatched_events = true;
    }

    /// The event-loop side of a drop: turn the received bytes into paths
    /// and deliver them as winit events.
    pub fn deliver_dnd(&mut self, payload: DndPayload) {
        let DndPayload {
            window_id,
            mime,
            bytes,
            offer,
        } = payload;
        let paths = if mime == URI_LIST {
            parse_uri_list(&bytes)
        } else {
            let n = match self.dnd.as_mut() {
                Some(d) => {
                    d.next_spool += 1;
                    d.next_spool
                }
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
                }
            }
        };
        for path in &paths {
            self.events_sink
                .push_window_event(WindowEvent::HoveredFile(path.clone()), window_id);
        }
        for path in paths {
            self.events_sink
                .push_window_event(WindowEvent::DroppedFile(path), window_id);
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
        let Some(window_id) = self.dnd_window_id(wl_surface) else {
            return;
        };
        let Some(offer) = data_device
            .data::<DataDeviceData>()
            .and_then(|d| d.drag_offer())
        else {
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
            dnd.hover = Some(DndHover {
                window_id,
                mime,
                offer,
                dropped: false,
            });
        }
        self.dnd_cursor_moved(window_id, x, y);
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {
        let Some(hover) = self.dnd.as_mut().and_then(|d| d.hover.take()) else {
            return;
        };
        if hover.dropped {
            // The drop is in flight on the reader thread; `deliver_dnd`
            // finishes the offer once the bytes are in.
            return;
        }
        self.events_sink
            .push_window_event(WindowEvent::HoveredFileCancelled, hover.window_id);
        self.dispatched_events = true;
    }

    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, x: f64, y: f64) {
        let Some(window_id) = self
            .dnd
            .as_ref()
            .and_then(|d| d.hover.as_ref())
            .map(|h| h.window_id)
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
        let Some(hover) = dnd.hover.as_mut() else {
            return;
        };
        hover.dropped = true;
        let pipe = match hover.offer.receive(hover.mime.clone()) {
            Ok(pipe) => pipe,
            Err(err) => {
                tracing::warn!("could not receive the dropped {}: {err}", hover.mime);
                dnd.hover = None;
                return;
            }
        };
        let sender = dnd.sender.clone();
        let window_id = hover.window_id;
        let mime = hover.mime.clone();
        let offer = hover.offer.clone();
        // Read on a thread: the source writes as the compositor relays, and
        // blocking the event loop on it would also block the reply the
        // source is waiting for.
        let _ = std::thread::Builder::new()
            .name("winit-dnd-read".into())
            .spawn(move || {
                let mut pipe = pipe;
                let mut bytes = Vec::new();
                if let Err(err) = pipe.read_to_end(&mut bytes) {
                    tracing::warn!("reading the dropped {mime} failed: {err}");
                    return;
                }
                let _ = sender.send(DndPayload {
                    window_id,
                    mime,
                    bytes,
                    offer,
                });
            });
    }
}

impl DataOfferHandler for WinitState {
    fn source_actions(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }

    fn selected_action(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }
}

// ---------------------------------------------------------------------------
// The send half: starting a drag
// ---------------------------------------------------------------------------

/// The seat objects a drag needs, published once the seat has a data
/// device.
///
/// A drag is started from the application thread (inside an event
/// handler), not from the event loop's own dispatch, so the objects it
/// needs cannot be reached through `&mut WinitState`. They are protocol
/// proxies, which are `Send + Sync` and queue their requests
/// thread-safely, so a process-global is both sound and enough: one
/// connection, one seat, and at most one drag at a time because a drag
/// is a held pointer button.
struct DragSeat {
    manager: WlDataDeviceManager,
    /// The seat's *existing* data device. Deliberately not a second one:
    /// a new `wl_data_device` would also receive `enter` / `drop`, and
    /// the receive half above would deliver every drop twice.
    device: WlDataDevice,
    queue_handle: QueueHandle<WinitState>,
}

static DRAG_SEAT: Mutex<Option<DragSeat>> = Mutex::new(None);

/// What an in-flight outgoing drag is offering, keyed by the source that
/// carries it.
///
/// The bytes live here rather than in the app because the receiver asks
/// for them through `wl_data_source.send`, which arrives on the event
/// loop long after the call that started the drag returned.
struct Outgoing {
    source: ObjectId,
    offers: Vec<(String, Arc<DragData>)>,
}

/// What one mime type of an outgoing drag carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DragData {
    /// These bytes.
    Bytes(Vec<u8>),
    /// The content of this file. It is opened and streamed into the pipe
    /// when a receiver asks for it, on the thread that writes the pipe,
    /// never on the thread that started the drag.
    File(PathBuf),
}

impl DragData {
    /// Write the content into `pipe`.
    fn write_to(&self, pipe: &mut impl Write) -> std::io::Result<()> {
        match self {
            Self::Bytes(bytes) => pipe.write_all(bytes),
            Self::File(path) => {
                std::io::copy(&mut std::fs::File::open(path)?, pipe).map(|_| ())
            },
        }
    }
}

static OUTGOING: Mutex<Vec<Outgoing>> = Mutex::new(Vec::new());

/// Publish the seat's data device so [`start_drag`] can reach it.
pub fn register_drag_seat(
    manager: WlDataDeviceManager,
    device: WlDataDevice,
    queue_handle: QueueHandle<WinitState>,
) {
    *DRAG_SEAT.lock().unwrap_or_else(|e| e.into_inner()) = Some(DragSeat {
        manager,
        device,
        queue_handle,
    });
}

/// Withdraw the published data device when its seat goes away.
pub fn clear_drag_seat() {
    *DRAG_SEAT.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Start a drag from `surface`, offering `offers` as mime → bytes.
///
/// `serial` must be the serial of the pointer button press that is still
/// held: the compositor validates it against the implicit grab and
/// ignores a drag whose serial does not match one.
///
/// Bytes, not paths, are what a drag carries here — deliberately. The
/// receiver is a sandboxed app with its own mount namespace, so a host
/// path it was handed would not resolve; what always works is the source
/// writing the content into the pipe the compositor brokers. A
/// `text/uri-list` may be offered alongside for receivers that want a
/// name, but it is never the only offer.
///
/// Returns whether the request was sent.
pub fn start_drag(surface: &WlSurface, serial: u32, offers: Vec<(String, DragData)>) -> bool {
    if offers.is_empty() {
        return false;
    }
    let seat = DRAG_SEAT.lock().unwrap_or_else(|e| e.into_inner());
    let Some(seat) = seat.as_ref() else {
        tracing::warn!("a drag was started before the seat had a data device");
        return false;
    };
    let source = seat
        .manager
        .create_data_source(&seat.queue_handle, DataSourceData::default());
    for (mime, _) in &offers {
        source.offer(mime.clone());
    }
    // `set_actions` is version 3; on an older manager the drag is still
    // valid, it just cannot negotiate copy-vs-move.
    if source.version() >= 3 {
        source.set_actions(DndAction::Copy);
    }
    OUTGOING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(Outgoing {
            source: source.id(),
            offers: offers.into_iter().map(|(mime, data)| (mime, Arc::new(data))).collect(),
        });
    seat.device.start_drag(Some(&source), surface, None, serial);
    true
}

/// Drop an outgoing drag's payload once the source is done with it.
fn forget_outgoing(source: &WlDataSource) {
    let id = source.id();
    OUTGOING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|o| o.source != id);
}

impl DataSourceHandler for WinitState {
    fn accept_mime(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        _: Option<String>,
    ) {
    }

    /// The receiver asked for one of the mime types we offered. Write the
    /// bytes registered for it.
    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        source: &WlDataSource,
        mime: String,
        pipe: WritePipe,
    ) {
        let id = source.id();
        let data = {
            let out = OUTGOING.lock().unwrap_or_else(|e| e.into_inner());
            out.iter()
                .find(|o| o.source == id)
                .and_then(|o| o.offers.iter().find(|(m, _)| *m == mime))
                .map(|(_, data)| Arc::clone(data))
        };
        let Some(data) = data else {
            tracing::warn!("a receiver asked for {mime}, which this drag never offered");
            return;
        };
        // On a thread, for the same reason the read side is: a payload
        // bigger than the pipe buffer blocks until the receiver drains
        // it, and the receiver may well be waiting on an event this loop
        // has not dispatched yet. A file is read there too, as it streams.
        let _ = std::thread::Builder::new()
            .name("winit-dnd-write".into())
            .spawn(move || {
                let mut pipe = pipe;
                if let Err(err) = data.write_to(&mut pipe) {
                    tracing::warn!("writing a dragged {mime} to the receiver failed: {err}");
                }
            });
    }

    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource) {
        forget_outgoing(source);
        source.destroy();
    }

    /// The pointer was released over a receiver that accepted. The source
    /// is *not* destroyed here — the receiver may still ask for the bytes
    /// after the drop, and `dnd_finished` is where it says it is done.
    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource) {
        forget_outgoing(source);
        source.destroy();
    }

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
    fn a_dragged_file_streams_its_content_when_it_is_written() {
        let path = std::env::temp_dir()
            .join(format!("winit-drag-data-{}.bin", std::process::id()));
        let content: Vec<u8> = (0..200_000u32).map(|n| (n % 251) as u8).collect();
        std::fs::write(&path, &content).unwrap();
        let data = DragData::File(path.clone());
        let mut pipe = Vec::new();
        data.write_to(&mut pipe).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(pipe, content);
        let mut pipe = Vec::new();
        assert!(DragData::File(path).write_to(&mut pipe).is_err(), "a file gone by then fails");
        let mut pipe = Vec::new();
        DragData::Bytes(b"abc".to_vec()).write_to(&mut pipe).unwrap();
        assert_eq!(pipe, b"abc");
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
        assert_eq!(
            choose_mime(&mimes(&["text/uri-list"])).as_deref(),
            Some("text/uri-list")
        );
    }

    #[test]
    fn nothing_usable_is_refused() {
        assert_eq!(
            choose_mime(&mimes(&["text/plain", "x-special/thing"])),
            None
        );
    }

    #[test]
    fn uri_list_decodes_file_uris_and_skips_the_rest() {
        let body = b"# a comment\r\nfile:///home/me/photo%20one.png\r\nhttps://example.com/x\r\nfile://localhost/tmp/b.jpg\r\n";
        assert_eq!(
            parse_uri_list(body),
            vec![
                PathBuf::from("/home/me/photo one.png"),
                PathBuf::from("/tmp/b.jpg")
            ]
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
