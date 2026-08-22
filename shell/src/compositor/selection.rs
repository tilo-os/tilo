//! Clipboard, primary selection and drag-and-drop for the session compositor.
//!
//! Before this module the session compositor advertised no selection protocol
//! at all, so nothing could be copied between two processes: `arboard` failed
//! to construct and every `ui` app silently fell back to a process-global
//! buffer that looked like a working clipboard from inside one process and was
//! a no-op across two.
//!
//! Four protocols are advertised here:
//!
//! | protocol | who may bind | what it is for |
//! |---|---|---|
//! | `wl_data_device_manager` | every client | Ctrl+C / Ctrl+V and drag-and-drop |
//! | `zwp_primary_selection_device_manager_v1` | every client | select-then-middle-click |
//! | `zwlr_data_control_manager_v1` | privileged clients only | clipboard managers |
//! | `ext_data_control_manager_v1` | privileged clients only | ditto, the successor |
//!
//! ## Why data-control is gated and the other two are not
//!
//! `wl_data_device` and the primary-selection device are **focus-gated by the
//! protocol itself**: smithay only accepts `set_selection` from a client whose
//! surface currently holds keyboard focus, and it only delivers a selection
//! *offer* to the focused client. A background app therefore cannot read what
//! the user copied and cannot overwrite it. The gate is the user's own
//! attention, which is the same gate every desktop has used for thirty years,
//! so exposing these two to sandboxed apps adds no capability an app could not
//! already get by asking the user to click on it.
//!
//! `zwlr_data_control_manager_v1` and `ext_data_control_manager_v1` are a
//! different thing entirely. They exist for clipboard *managers*: a bound
//! client receives every selection on every seat, as it happens, with no focus
//! requirement and no user interaction — and may also overwrite it. In this
//! codebase that is not a theoretical risk. The shipped apps put live TOTP
//! codes, scanned Wi-Fi passwords and identity recovery mnemonics on the
//! clipboard as a normal part of their UI. A sandboxed app holding a
//! data-control device would harvest all of it in the background, and the
//! per-workspace sandbox — the security boundary of this system — would not
//! see a thing, because the exfiltration rides the Wayland socket the app is
//! legitimately given.
//!
//! So data-control is bound to a *privilege* rather than to a client's good
//! behaviour. [`ClientState::privileged`] is set only for connections accepted
//! on the compositor's privileged socket (see
//! [`crate::compositor::native`]), whose path is never bind-mounted into a
//! sandbox — `crates/packaging/launcher/src/sandbox.rs` binds exactly one
//! socket, the public one. A sandboxed app cannot reach the privileged socket
//! even if it knows the path, so the filter is backed by the filesystem
//! namespace and not by an honour system.
//!
//! ## Why the compositor mirrors the selection
//!
//! The chrome (top bar, overlays, the workspace-name editor) and the two
//! in-process apps (`terminal`, `file-explorer`) run *inside* the compositor
//! process. They are not Wayland clients, so they have no data device to read
//! from, and they cannot block waiting for one either: reading a client-owned
//! selection means asking that client to write into a pipe, and the client can
//! only answer once the compositor flushes — which it cannot do while blocked.
//!
//! The compositor therefore keeps a [`SelectionStore`]: whenever a client takes
//! ownership of the clipboard or the primary selection, the compositor pulls
//! the text out of it once, asynchronously, into memory. In-process paste then
//! reads that mirror synchronously. This is exactly what a clipboard manager
//! does, and it also gives the session the behaviour users expect anyway —
//! the clipboard survives the app that filled it.
//!
//! Copy in the other direction is symmetrical: chrome writes into the store,
//! the store raises a publish flag, and the next compositor tick turns it into
//! a compositor-owned selection so focused clients receive a real offer.

use std::fs::File;
use std::io::Read;
use std::os::unix::io::{FromRawFd, OwnedFd};
use std::sync::{Arc, Mutex};

use smithay::input::{Seat, SeatHandler};
use smithay::reexports::calloop::{Interest, LoopHandle, Mode, PostAction, generic::Generic};
use smithay::reexports::wayland_server::protocol::wl_data_source::WlDataSource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, DisplayHandle};
use smithay::wayland::selection::data_device::{
    ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
    clear_data_device_selection, request_data_device_client_selection, set_data_device_focus,
    set_data_device_selection,
};
use smithay::wayland::selection::ext_data_control::DataControlHandler as ExtDataControlHandler;
use smithay::wayland::selection::ext_data_control::DataControlState as ExtDataControlState;
use smithay::wayland::selection::primary_selection::{
    PrimarySelectionHandler, PrimarySelectionState, clear_primary_selection,
    request_primary_client_selection, set_primary_focus, set_primary_selection,
};
use smithay::wayland::selection::wlr_data_control::{DataControlHandler, DataControlState};
use smithay::wayland::selection::{SelectionHandler, SelectionSource, SelectionTarget};

use crate::compositor::state::{ClientState, State};

/// The mime type every text selection in qOS is offered as, and the only one
/// the compositor's own mirror understands. `text/plain` and the bare
/// `UTF8_STRING` / `STRING` aliases are offered alongside it for clients that
/// only match on those (notably X11-heritage toolkits through Xwayland).
pub const TEXT_MIME: &str = "text/plain;charset=utf-8";

/// Mime types a compositor-owned text selection advertises, most specific
/// first — the order a client sees them in its `wl_data_offer.offer` stream.
pub fn text_mime_types() -> Vec<String> {
    [TEXT_MIME, "text/plain", "UTF8_STRING", "STRING", "TEXT"]
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// Upper bound on a mirrored selection, so a hostile or broken client cannot
/// make the compositor buffer unbounded data by claiming the clipboard and
/// then writing forever. 8 MiB is far past any realistic text selection and
/// still small enough to be irrelevant against the compositor's own working
/// set. A source that exceeds it is dropped rather than truncated — half a
/// paste is worse than none.
const MAX_SELECTION_BYTES: usize = 8 * 1024 * 1024;

/// Bytes a fresh pipe accepts before a write would block. Linux's default is
/// 64 KiB; the conservative figure below keeps the "this cannot block" claim
/// true even if a header or a smaller default is in play.
const PIPE_BUFFER_BYTES: usize = 32 * 1024;

/// Which of the two selections a value refers to. Mirrors smithay's
/// [`SelectionTarget`] but is `serde`-friendly and usable from the test
/// control socket and the `ui` bridge without pulling smithay into them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The Ctrl+C / Ctrl+V clipboard.
    Clipboard,
    /// The select-then-middle-click primary selection.
    Primary,
}

impl From<SelectionTarget> for Target {
    fn from(t: SelectionTarget) -> Self {
        match t {
            SelectionTarget::Clipboard => Target::Clipboard,
            SelectionTarget::Primary => Target::Primary,
        }
    }
}

impl From<Target> for SelectionTarget {
    fn from(t: Target) -> Self {
        match t {
            Target::Clipboard => SelectionTarget::Clipboard,
            Target::Primary => SelectionTarget::Primary,
        }
    }
}

/// One side of the mirror: the text the compositor believes is currently on
/// this selection, plus whether the compositor itself is the owner.
#[derive(Debug, Default, Clone)]
struct Slot {
    /// Last known text. `None` means "nothing has been copied", which is
    /// distinct from `Some("")` — a client is allowed to own an empty
    /// selection.
    text: Option<String>,
    /// `true` while the compositor is the selection owner, i.e. the content
    /// came from in-process chrome and the compositor must answer `receive`
    /// requests from the mirror.
    compositor_owned: bool,
    /// Set when chrome wrote new content that has not yet been turned into a
    /// Wayland selection. Drained by [`State::pump_selection`].
    publish_pending: bool,
    /// Set when chrome asked for the selection to be dropped entirely.
    clear_pending: bool,
}

/// The compositor's clipboard mirror, shared between the Wayland dispatch and
/// the in-process chrome.
///
/// Held behind an `Arc<Mutex<_>>` because `ui`'s clipboard hook is a
/// process-global that any widget may call from the UI thread, while the
/// Wayland side writes it from the compositor's calloop dispatch. Both live on
/// the same thread today; the mutex costs nothing measurable on a
/// once-per-copy path and removes the thread-affinity assumption entirely.
#[derive(Debug, Default)]
pub struct SelectionStore {
    clipboard: Slot,
    primary: Slot,
}

impl SelectionStore {
    fn slot(&self, target: Target) -> &Slot {
        match target {
            Target::Clipboard => &self.clipboard,
            Target::Primary => &self.primary,
        }
    }

    fn slot_mut(&mut self, target: Target) -> &mut Slot {
        match target {
            Target::Clipboard => &mut self.clipboard,
            Target::Primary => &mut self.primary,
        }
    }
}

/// Shared handle to the compositor's selection mirror.
pub type SharedSelection = Arc<Mutex<SelectionStore>>;

/// Read the current text of `target` from the mirror.
///
/// This is the in-process paste path. It never blocks on a client and never
/// touches the Wayland connection, so it is safe to call from a widget's
/// `handle_event` while the compositor's dispatch is on the stack.
pub fn mirror_text(store: &SharedSelection, target: Target) -> Option<String> {
    store.lock().ok()?.slot(target).text.clone()
}

/// Write `text` to `target` from in-process chrome and mark it for publishing.
///
/// Takes effect for other processes on the next compositor tick, when
/// [`State::pump_selection`] converts it into a compositor-owned Wayland
/// selection. In-process readers see it immediately.
pub fn set_mirror_text(store: &SharedSelection, target: Target, text: &str) {
    let Ok(mut store) = store.lock() else {
        return;
    };
    let slot = store.slot_mut(target);
    slot.text = Some(text.to_string());
    slot.compositor_owned = true;
    slot.publish_pending = true;
    slot.clear_pending = false;
}

/// Drop `target` entirely — no owner, no content.
pub fn clear_mirror(store: &SharedSelection, target: Target) {
    let Ok(mut store) = store.lock() else {
        return;
    };
    let slot = store.slot_mut(target);
    slot.text = None;
    slot.compositor_owned = false;
    slot.publish_pending = false;
    slot.clear_pending = true;
}

/// Record a selection the compositor pulled out of a *client*.
///
/// Clears `compositor_owned` — from here on `receive` requests are answered by
/// the owning client, not by the mirror. The mirror keeps the text only so
/// in-process chrome can paste it synchronously.
fn record_client_text(store: &SharedSelection, target: Target, text: Option<String>) {
    let Ok(mut store) = store.lock() else {
        return;
    };
    let slot = store.slot_mut(target);
    slot.text = text;
    slot.compositor_owned = false;
    // A client taking the selection supersedes anything chrome had queued.
    slot.publish_pending = false;
    slot.clear_pending = false;
}

/// Whether the compositor currently owns `target`, i.e. whether a `receive`
/// on it must be answered out of the mirror.
fn compositor_owns(store: &SharedSelection, target: Target) -> bool {
    store
        .lock()
        .map(|s| s.slot(target).compositor_owned)
        .unwrap_or(false)
}

/// A selection ownership change the compositor has seen but not yet mirrored.
///
/// smithay raises [`SelectionHandler::new_selection`] *before* it installs the
/// new source on the seat, so the pull cannot happen inline — asking for the
/// selection at that moment would hand back the previous owner's data. The
/// request is parked here and drained by [`State::pump_selection`] on the next
/// tick, by which point the seat holds the new source.
#[derive(Debug, Clone)]
pub struct PendingMirrorPull {
    /// Which selection changed hands.
    pub target: Target,
    /// The mime types the new owner offered, or `None` when the selection was
    /// cleared (`set_selection(null)`).
    pub mime_types: Option<Vec<String>>,
}

/// Everything the compositor needs to broker selections, hung off [`State`].
pub struct SelectionState {
    /// `wl_data_device_manager` — clipboard + drag-and-drop, every client.
    pub data_device_state: DataDeviceState,
    /// `zwp_primary_selection_device_manager_v1`, every client.
    pub primary_selection_state: PrimarySelectionState,
    /// `zwlr_data_control_manager_v1`, privileged clients only.
    pub data_control_state: DataControlState,
    /// `ext_data_control_manager_v1`, privileged clients only.
    pub ext_data_control_state: ExtDataControlState,
    /// The mirror shared with in-process chrome.
    pub store: SharedSelection,
    /// Ownership changes waiting to be pulled into the mirror.
    pub pending_pulls: Vec<PendingMirrorPull>,
    /// The client that currently holds selection focus, so a repeat focus
    /// sync does not re-offer the selection on every tick.
    pub focused_client: Option<Client>,
    /// Surface a client handed us as its drag icon, held for the duration of
    /// the drag. See [`ClientDndGrabHandler::started`] for why it is kept but
    /// not composited.
    pub dnd_icon: Option<WlSurface>,
    /// A drag the compositor itself started — a file carried out of the
    /// hosted file manager and over a client's window. `Some` from the
    /// moment the server-side grab starts until the client finishes with
    /// the offer or the drag is cancelled. See [`ChromeDrag`].
    pub chrome_drag: Option<ChromeDrag>,
    /// Whether a client-initiated drag-and-drop is in flight.
    ///
    /// Read by the pointer router: while a button is held the compositor
    /// normally pins pointer focus to the surface that owns the implicit
    /// grab, so a client cannot lose its own drag by sliding off its window.
    /// A drag-and-drop is the one case where that is exactly wrong — the
    /// whole point is to travel to a *different* surface, and pinning focus
    /// to the source means the drop target never receives
    /// `wl_data_device.enter` and nothing can ever be dropped anywhere.
    pub dnd_active: bool,
}

impl std::fmt::Debug for SelectionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SelectionState")
            .field("store", &self.store)
            .field("pending_pulls", &self.pending_pulls)
            .finish_non_exhaustive()
    }
}

impl SelectionState {
    /// Advertise all four globals on `display`.
    ///
    /// The data-control pair is created with a filter that admits only
    /// privileged clients; see the module docs for why. `PrimarySelectionState`
    /// is handed to the wlr data-control global so a privileged manager can
    /// also see the primary selection.
    pub fn new(display: &DisplayHandle) -> Self {
        let data_device_state = DataDeviceState::new::<State>(display);
        let primary_selection_state = PrimarySelectionState::new::<State>(display);
        let data_control_state = DataControlState::new::<State, _>(
            display,
            Some(&primary_selection_state),
            is_privileged,
        );
        let ext_data_control_state = ExtDataControlState::new::<State, _>(
            display,
            Some(&primary_selection_state),
            is_privileged,
        );
        Self {
            data_device_state,
            primary_selection_state,
            data_control_state,
            ext_data_control_state,
            store: Arc::new(Mutex::new(SelectionStore::default())),
            pending_pulls: Vec::new(),
            focused_client: None,
            dnd_icon: None,
            dnd_active: false,
            chrome_drag: None,
        }
    }
}

/// What a compositor-initiated drag carries.
///
/// Files is a widget inside the compositor, not a Wayland client, so a row
/// dragged out of it has no `wl_data_source`. The compositor is the source
/// instead: it offers the paths as `text/uri-list`, and — for a single file
/// whose type it knows — the file's own bytes under their mime type. The
/// bytes matter more than the paths: a sandboxed app cannot read a host
/// path, but it can always read what is written into the pipe it asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChromeDrag {
    pub paths: Vec<std::path::PathBuf>,
    pub mime_types: Vec<String>,
}

/// The mime type a list of `file://` URIs travels as.
pub const URI_LIST_MIME: &str = "text/uri-list";

impl ChromeDrag {
    pub fn new(paths: Vec<std::path::PathBuf>) -> Self {
        let mut mime_types = vec![URI_LIST_MIME.to_string()];
        if let [single] = paths.as_slice() {
            if let Some(mime) = mime_for_path(single) {
                mime_types.push(mime.to_string());
            }
        }
        Self { paths, mime_types }
    }

    /// The `text/uri-list` body: one `file://` URI per line, CRLF-terminated
    /// as the RFC 2483 shape requires, with the bytes a path cannot carry
    /// verbatim percent-encoded.
    pub fn uri_list(&self) -> String {
        let mut out = String::new();
        for p in &self.paths {
            out.push_str("file://");
            out.push_str(&percent_encode_path(&p.to_string_lossy()));
            out.push_str("\r\n");
        }
        out
    }

    /// The single file whose bytes `mime` asks for, if that is what the
    /// offer promised.
    pub fn file_for_mime(&self, mime: &str) -> Option<&std::path::Path> {
        if mime == URI_LIST_MIME || !self.mime_types.iter().any(|m| m == mime) {
            return None;
        }
        match self.paths.as_slice() {
            [single] => Some(single.as_path()),
            _ => None,
        }
    }
}

/// The mime type of a file by extension. Only types an app can do
/// something with as raw bytes; anything else travels as a path only.
pub fn mime_for_path(path: &std::path::Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "txt" | "md" => "text/plain",
        _ => return None,
    })
}

fn percent_encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Write `bytes` into `fd` without stalling the compositor: small payloads
/// fit the pipe buffer and go now; anything larger drains on a thread at
/// the receiver's pace, and the thread closing the fd is the receiver's EOF.
fn write_to_pipe(fd: OwnedFd, bytes: Vec<u8>, what: &'static str) {
    use std::io::Write;
    let mut file = File::from(fd);
    if bytes.len() <= PIPE_BUFFER_BYTES {
        if let Err(e) = file.write_all(&bytes) {
            tracing::debug!(target: "shell::selection", error = %e, "{what} receiver went away mid-transfer");
        }
        return;
    }
    std::thread::spawn(move || {
        if let Err(e) = file.write_all(&bytes) {
            tracing::debug!(target: "shell::selection", error = %e, "{what} receiver went away mid-transfer");
        }
    });
}

/// The data-control filter: may this client see the clipboard-manager globals?
///
/// Only connections accepted on the privileged socket qualify. Everything on
/// the public socket — which is the only socket a sandbox can reach — is
/// refused, so the global is simply absent from its registry and `arboard`,
/// `wl-paste` and friends running inside a sandbox fall through to the
/// focus-gated `wl_data_device` path instead of getting a silent firehose.
fn is_privileged(client: &Client) -> bool {
    client
        .get_data::<ClientState>()
        .map(|d| d.privileged)
        .unwrap_or(false)
}

/// Flip `fd` into non-blocking mode. Returns `false` if the kernel refused,
/// in which case the caller must not register it with the event loop — a
/// blocking fd on a level-triggered source would stall the whole compositor.
fn set_nonblocking(fd: &OwnedFd) -> bool {
    use std::os::unix::io::AsRawFd;
    // SAFETY: `fd` is a live descriptor owned by the caller for the duration
    // of the call, and both fcntl commands are total functions on any fd.
    unsafe {
        let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
        if flags < 0 {
            return false;
        }
        libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) >= 0
    }
}

/// Create a close-on-exec pipe for one selection transfer, as
/// `(read end, write end)`.
fn selection_pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `fds` is a live two-element array, which is exactly what
    // `pipe2` writes into on success.
    let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: both descriptors were just created by `pipe2` and are owned by
    // us; nothing else holds them.
    unsafe { Ok((OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1]))) }
}

/// Spawn an async read of `fd` into the mirror slot for `target`.
///
/// Registered as a level-triggered calloop source rather than read inline:
/// the client on the other end of this pipe only writes once the compositor
/// flushes the `send` event to it, so a blocking read here would deadlock the
/// compositor against the very client it is waiting on.
fn read_into_mirror(
    loop_handle: &LoopHandle<'static, State>,
    store: SharedSelection,
    target: Target,
    fd: OwnedFd,
) {
    // Non-blocking, so a level-triggered wakeup that turns out to have no
    // bytes ready cannot stall the loop.
    if !set_nonblocking(&fd) {
        tracing::warn!(target: "shell::selection", "could not set selection pipe non-blocking");
        return;
    }
    // `File` owns the fd for the lifetime of the source; calloop hands the
    // stored value back to the callback on every wakeup, so the read side
    // stays alive until we return `PostAction::Remove`.
    let file = File::from(fd);
    let source = Generic::new(file, Interest::READ, Mode::Level);
    let mut buf: Vec<u8> = Vec::new();
    let mut overflowed = false;
    let registered = loop_handle.insert_source(source, move |_, file, _state| {
        let mut chunk = [0u8; 8192];
        // `&File` is the `Read` impl; calloop hands back a wrapper that only
        // derefs shared, which is all a read needs.
        let mut reader: &File = &*file;
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => {
                    // EOF: the owner closed its end, the transfer is complete.
                    if overflowed {
                        tracing::warn!(
                            target: "shell::selection",
                            limit = MAX_SELECTION_BYTES,
                            "selection source exceeded the mirror size limit; dropping it"
                        );
                        record_client_text(&store, target, None);
                    } else {
                        match String::from_utf8(std::mem::take(&mut buf)) {
                            Ok(text) => record_client_text(&store, target, Some(text)),
                            Err(_) => {
                                // The owner advertised a UTF-8 text mime and
                                // then sent something else. Mirroring bytes we
                                // cannot render would put mojibake in every
                                // in-process paste, so record "nothing" and
                                // leave the client as the authority for
                                // clients that can negotiate another type.
                                tracing::warn!(
                                    target: "shell::selection",
                                    "selection source sent non-UTF-8 bytes for a text mime type"
                                );
                                record_client_text(&store, target, None);
                            }
                        }
                    }
                    return Ok(PostAction::Remove);
                }
                Ok(n) => {
                    if buf.len() + n > MAX_SELECTION_BYTES {
                        overflowed = true;
                        buf.clear();
                    } else {
                        buf.extend_from_slice(&chunk[..n]);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    return Ok(PostAction::Continue);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    tracing::warn!(target: "shell::selection", error = %e, "selection pipe read failed");
                    return Ok(PostAction::Remove);
                }
            }
        }
    });
    if let Err(e) = registered {
        tracing::warn!(target: "shell::selection", error = %e, "could not register selection reader");
    }
}

impl State {
    /// Shared handle to the compositor's selection mirror, for installing the
    /// in-process clipboard bridge.
    pub fn selection_store(&self) -> SharedSelection {
        Arc::clone(&self.selection.store)
    }

    /// Point the selection devices at `client`, so it receives clipboard and
    /// primary offers and is allowed to set them.
    ///
    /// Called from the same place as keyboard focus: smithay gates
    /// `set_selection` on keyboard focus, so a selection focus that lagged
    /// behind the keyboard would let a client be typed into while it was
    /// refused the clipboard.
    pub fn set_selection_focus(&mut self, client: Option<Client>) {
        let same = match (&self.selection.focused_client, &client) {
            (Some(a), Some(b)) => a.id() == b.id(),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        let dh = self.display_handle.clone();
        set_data_device_focus(&dh, &self.seat, client.clone());
        set_primary_focus(&dh, &self.seat, client.clone());
        self.selection.focused_client = client;
    }

    /// Per-tick selection work: publish anything chrome copied, and pull
    /// anything a client copied into the mirror.
    ///
    /// Cheap and allocation-free when nothing changed, which is every tick
    /// except the one right after a copy.
    pub fn pump_selection(&mut self) {
        self.publish_chrome_selection();
        self.drain_pending_pulls();
    }

    /// Turn a pending chrome copy into a compositor-owned Wayland selection.
    fn publish_chrome_selection(&mut self) {
        for target in [Target::Clipboard, Target::Primary] {
            let action = {
                let Ok(mut store) = self.selection.store.lock() else {
                    continue;
                };
                let slot = store.slot_mut(target);
                if slot.clear_pending {
                    slot.clear_pending = false;
                    Some(false)
                } else if slot.publish_pending {
                    slot.publish_pending = false;
                    Some(true)
                } else {
                    None
                }
            };
            let dh = self.display_handle.clone();
            match action {
                Some(true) => match target {
                    Target::Clipboard => {
                        set_data_device_selection(&dh, &self.seat, text_mime_types(), ());
                    }
                    Target::Primary => {
                        set_primary_selection(&dh, &self.seat, text_mime_types(), ());
                    }
                },
                Some(false) => match target {
                    Target::Clipboard => clear_data_device_selection(&dh, &self.seat),
                    Target::Primary => clear_primary_selection(&dh, &self.seat),
                },
                None => {}
            }
        }
    }

    /// Pull every selection that changed owner since the last tick into the
    /// mirror, so in-process chrome can paste it without a round trip.
    fn drain_pending_pulls(&mut self) {
        if self.selection.pending_pulls.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.selection.pending_pulls);
        for pull in pending {
            let Some(mimes) = pull.mime_types else {
                // The owner released the selection without replacing it.
                record_client_text(&self.selection.store, pull.target, None);
                continue;
            };
            // Pick the most specific text type the owner actually offers; a
            // source that offers no text at all (an image drag, say) leaves
            // the mirror empty rather than guessing.
            let Some(mime) = text_mime_types().into_iter().find(|m| mimes.contains(m)) else {
                record_client_text(&self.selection.store, pull.target, None);
                continue;
            };
            let (read_fd, write_fd) = match selection_pipe() {
                Ok(pair) => pair,
                Err(e) => {
                    tracing::warn!(target: "shell::selection", error = %e, "could not create selection pipe");
                    continue;
                }
            };
            // The two `request_*` helpers return distinct per-protocol error
            // enums; only the message matters here, so both collapse to a
            // string at the call site.
            let requested = match pull.target {
                Target::Clipboard => request_data_device_client_selection(&self.seat, mime, write_fd)
                    .map_err(|e| e.to_string()),
                Target::Primary => request_primary_client_selection(&self.seat, mime, write_fd)
                    .map_err(|e| e.to_string()),
            };
            match requested {
                Ok(()) => read_into_mirror(
                    &self.loop_handle,
                    Arc::clone(&self.selection.store),
                    pull.target,
                    read_fd,
                ),
                Err(e) => {
                    // `ServerSideSelection` is the normal outcome when the
                    // compositor itself is the owner — the mirror already holds
                    // the text, so there is nothing to pull.
                    tracing::debug!(target: "shell::selection", error = %e, "no client selection to mirror");
                }
            }
        }
    }

    /// Answer a `receive` on a compositor-owned selection out of the mirror.
    fn serve_selection(&mut self, target: Target, mime_type: &str, fd: OwnedFd) {
        if !text_mime_types().iter().any(|m| m == mime_type) {
            return;
        }
        use std::io::Write;
        let text = mirror_text(&self.selection.store, target).unwrap_or_default();
        let mut file = File::from(fd);
        // Anything that fits in the pipe buffer is written here and now: the
        // kernel takes it whether or not the client has started reading, so
        // there is nothing to block on, and a clipboard is almost always a
        // line or two of text.
        if text.len() <= PIPE_BUFFER_BYTES {
            if let Err(e) = file.write_all(text.as_bytes()) {
                tracing::debug!(target: "shell::selection", error = %e, "selection receiver went away mid-transfer");
            }
            return;
        }
        // A larger payload has to go to a thread. Written inline it would
        // block the compositor against the requesting client's read — and
        // that client cannot read while the compositor is stalled, because it
        // is waiting on the compositor to flush. Off-thread the write drains
        // at whatever rate the client reads. The thread ends at `write_all`
        // and the fd closes with it, which is what signals EOF to the reader.
        std::thread::spawn(move || {
            if let Err(e) = file.write_all(text.as_bytes()) {
                tracing::debug!(target: "shell::selection", error = %e, "selection receiver went away mid-transfer");
            }
        });
    }
}

// -----------------------------------------------------------------------------
// The in-process bridge
// -----------------------------------------------------------------------------

/// The clipboard the compositor's own widgets use.
///
/// `terminal`, `file-explorer`, the notes and editor apps and every piece of
/// chrome are linked into the compositor binary and call `ui`'s
/// `clipboard_copy` / `clipboard_paste` like any other widget. They are not
/// Wayland clients, so there is no data device for them to use — and they
/// cannot block on one either, since a client can only answer a selection
/// request once the compositor flushes, which it cannot do while blocked
/// inside a widget's event handler.
///
/// Reads therefore come straight out of the mirror, which is always current
/// because the compositor pulls every client-owned selection into it as the
/// selection changes hands. Writes go into the mirror and raise a publish
/// flag; [`State::pump_selection`] turns that into a compositor-owned Wayland
/// selection on the next tick, so a focused client sees a real offer.
struct HostClipboard {
    store: SharedSelection,
}

impl ui::clipboard::ClipboardBackend for HostClipboard {
    fn set_text(&self, target: ui::clipboard::ClipboardTarget, text: &str) -> bool {
        set_mirror_text(&self.store, ui_target(target), text);
        true
    }

    fn text(&self, target: ui::clipboard::ClipboardTarget) -> Option<String> {
        mirror_text(&self.store, ui_target(target))
    }

    fn name(&self) -> &'static str {
        "compositor-mirror"
    }
}

/// Map `ui`'s target enum onto this module's. They are deliberately separate
/// types: `ui` must not depend on smithay, and the compositor must not depend
/// on `ui`'s enum shape for its protocol handling.
fn ui_target(target: ui::clipboard::ClipboardTarget) -> Target {
    match target {
        ui::clipboard::ClipboardTarget::Clipboard => Target::Clipboard,
        ui::clipboard::ClipboardTarget::Primary => Target::Primary,
    }
}

/// Point `ui`'s clipboard at the compositor's selection mirror, for every
/// widget hosted inside this process.
///
/// Called once during compositor startup. Without it the hosted widgets fall
/// back to `ui`'s in-process buffer, which round-trips within the compositor
/// and is invisible to every client — the exact failure this module exists to
/// remove.
pub fn install_host_clipboard(store: SharedSelection) {
    ui::clipboard::install_clipboard_backend(Arc::new(HostClipboard { store }));
}

// -----------------------------------------------------------------------------
// smithay handler impls
// -----------------------------------------------------------------------------

impl SelectionHandler for State {
    /// The compositor's own selections carry no per-selection data — the text
    /// lives in [`SelectionStore`], keyed by target, because in-process chrome
    /// has to read it without a seat in hand.
    type SelectionUserData = ();

    fn new_selection(
        &mut self,
        ty: SelectionTarget,
        source: Option<SelectionSource>,
        _seat: Seat<Self>,
    ) {
        let target = Target::from(ty);
        // Park the pull. Doing it inline would read the *previous* owner:
        // smithay calls this hook before it installs the new source.
        self.selection.pending_pulls.push(PendingMirrorPull {
            target,
            mime_types: source.map(|s| s.mime_types()),
        });
    }

    fn send_selection(
        &mut self,
        ty: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
        _seat: Seat<Self>,
        _user_data: &(),
    ) {
        let target = Target::from(ty);
        if !compositor_owns(&self.selection.store, target) {
            // smithay only routes here for compositor-owned selections, so a
            // mismatch means the mirror and the seat disagree. Answering with
            // stale chrome content would be worse than answering with nothing.
            return;
        }
        self.serve_selection(target, &mime_type, fd);
    }
}

impl DataDeviceHandler for State {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.selection.data_device_state
    }
}

impl PrimarySelectionHandler for State {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.selection.primary_selection_state
    }
}

impl DataControlHandler for State {
    fn data_control_state(&self) -> &DataControlState {
        &self.selection.data_control_state
    }
}

impl ExtDataControlHandler for State {
    fn data_control_state(&self) -> &ExtDataControlState {
        &self.selection.ext_data_control_state
    }
}

impl ClientDndGrabHandler for State {
    fn started(&mut self, _source: Option<WlDataSource>, icon: Option<WlSurface>, _seat: Seat<Self>) {
        // The drag icon is a client surface with the `dnd_icon` role. qOS does
        // not composite it: the compositor draws its own cursor and its own
        // drag feedback, and a client-supplied icon surface would be painted
        // outside the strip layout the whole spatial model depends on.
        // Recording it keeps the role assignment honest for the client, which
        // is what the protocol requires.
        tracing::debug!(
            target: "shell::selection",
            has_icon = icon.is_some(),
            "client started a drag"
        );
        self.selection.dnd_icon = icon;
        self.selection.dnd_active = true;
    }

    fn dropped(&mut self, target: Option<WlSurface>, validated: bool, _seat: Seat<Self>) {
        tracing::debug!(
            target: "shell::selection",
            dropped_on_a_surface = target.is_some(),
            validated,
            "drag finished"
        );
        self.selection.dnd_icon = None;
        self.selection.dnd_active = false;
    }
}

/// Server-initiated drag-and-drop — a drag whose *source* is compositor chrome.
///
/// The one such drag is a file carried out of the hosted file manager and
/// over a client's window (`client_input::pump_chrome_drag` starts the grab,
/// [`ChromeDrag`] is what it carries). A tab dragged between groups never
/// comes here: that is moved inside the compositor's own widget tree.
impl ServerDndGrabHandler for State {
    fn accept(&mut self, mime_type: Option<String>, _seat: Seat<Self>) {
        tracing::debug!(target: "shell::selection", ?mime_type, "client accepted a chrome drag mime");
    }

    fn send(&mut self, mime_type: String, fd: OwnedFd, _seat: Seat<Self>) {
        let Some(drag) = self.selection.chrome_drag.clone() else {
            tracing::warn!(target: "shell::selection", %mime_type, "send for a chrome drag that is not in flight");
            return;
        };
        if mime_type == URI_LIST_MIME {
            write_to_pipe(fd, drag.uri_list().into_bytes(), "uri-list");
            return;
        }
        let Some(path) = drag.file_for_mime(&mime_type) else {
            tracing::debug!(target: "shell::selection", %mime_type, "client asked for a mime the chrome drag never offered");
            return;
        };
        match std::fs::read(path) {
            Ok(bytes) => write_to_pipe(fd, bytes, "dropped file"),
            Err(e) => tracing::warn!(
                target: "shell::selection",
                path = %path.display(),
                error = %e,
                "could not read the dropped file"
            ),
        }
    }

    fn dropped(&mut self, _seat: Seat<Self>) {
        // The client may still request data; `finished` ends the drag.
        tracing::debug!(target: "shell::selection", "chrome drag dropped");
    }

    fn cancelled(&mut self, _seat: Seat<Self>) {
        tracing::debug!(target: "shell::selection", "chrome drag cancelled");
        self.selection.chrome_drag = None;
    }

    fn finished(&mut self, _seat: Seat<Self>) {
        tracing::debug!(target: "shell::selection", "chrome drag finished");
        self.selection.chrome_drag = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> SharedSelection {
        Arc::new(Mutex::new(SelectionStore::default()))
    }

    /// Chrome writing the clipboard must be readable in-process immediately —
    /// the paste path cannot wait for a compositor tick — and must leave a
    /// publish flag behind so the tick turns it into a real Wayland selection.
    #[test]
    fn chrome_copy_is_readable_immediately_and_queues_a_publish() {
        let s = store();
        set_mirror_text(&s, Target::Clipboard, "from chrome");
        assert_eq!(mirror_text(&s, Target::Clipboard).as_deref(), Some("from chrome"));
        assert!(compositor_owns(&s, Target::Clipboard));
        assert!(s.lock().unwrap().clipboard.publish_pending);
        // Primary is a separate buffer and must not have been touched.
        assert_eq!(mirror_text(&s, Target::Primary), None);
    }

    /// A client taking the clipboard drops compositor ownership, so a later
    /// `receive` is answered by that client and not out of a stale mirror.
    #[test]
    fn client_copy_drops_compositor_ownership() {
        let s = store();
        set_mirror_text(&s, Target::Clipboard, "from chrome");
        assert!(compositor_owns(&s, Target::Clipboard));

        record_client_text(&s, Target::Clipboard, Some("from a client".into()));
        assert!(!compositor_owns(&s, Target::Clipboard));
        assert_eq!(
            mirror_text(&s, Target::Clipboard).as_deref(),
            Some("from a client"),
        );
        // The queued chrome publish must not fire after the client won the
        // selection — that would silently take the clipboard back.
        assert!(!s.lock().unwrap().clipboard.publish_pending);
    }

    /// Primary and clipboard are independent buffers. Middle-click paste must
    /// not return what Ctrl+C put down, which is the bug users notice first.
    #[test]
    fn primary_and_clipboard_do_not_alias() {
        let s = store();
        set_mirror_text(&s, Target::Clipboard, "ctrl-c text");
        set_mirror_text(&s, Target::Primary, "drag-selected text");
        assert_eq!(mirror_text(&s, Target::Clipboard).as_deref(), Some("ctrl-c text"));
        assert_eq!(
            mirror_text(&s, Target::Primary).as_deref(),
            Some("drag-selected text"),
        );
    }

    /// Clearing must distinguish "nothing was ever copied" from "an empty
    /// string was copied" — a client may legitimately own an empty selection.
    #[test]
    fn clear_is_distinct_from_an_empty_selection() {
        let s = store();
        set_mirror_text(&s, Target::Clipboard, "");
        assert_eq!(mirror_text(&s, Target::Clipboard).as_deref(), Some(""));
        clear_mirror(&s, Target::Clipboard);
        assert_eq!(mirror_text(&s, Target::Clipboard), None);
        assert!(!compositor_owns(&s, Target::Clipboard));
        assert!(s.lock().unwrap().clipboard.clear_pending);
    }

    /// The offered mime list must lead with the specific UTF-8 type, because
    /// the first type a client can match is the one it asks for, and a client
    /// that gets `STRING` back has to guess the encoding.
    #[test]
    fn text_mimes_lead_with_utf8() {
        let mimes = text_mime_types();
        assert_eq!(mimes.first().map(String::as_str), Some(TEXT_MIME));
        assert!(mimes.iter().any(|m| m == "text/plain"));
        assert!(mimes.iter().any(|m| m == "UTF8_STRING"));
    }

    #[test]
    fn a_chrome_drag_offers_a_uri_list_and_the_single_files_bytes() {
        let drag = ChromeDrag::new(vec![std::path::PathBuf::from("/var/workspace/Demo/my photo.png")]);
        assert_eq!(drag.mime_types, vec![URI_LIST_MIME.to_string(), "image/png".to_string()]);
        assert_eq!(drag.uri_list(), "file:///var/workspace/Demo/my%20photo.png\r\n");
        assert_eq!(
            drag.file_for_mime("image/png"),
            Some(std::path::Path::new("/var/workspace/Demo/my photo.png"))
        );
        assert_eq!(drag.file_for_mime("image/jpeg"), None, "not offered");
        assert_eq!(drag.file_for_mime(URI_LIST_MIME), None, "the list is not a file");
    }

    #[test]
    fn a_multi_file_drag_is_paths_only() {
        let drag = ChromeDrag::new(vec![
            std::path::PathBuf::from("/a.png"),
            std::path::PathBuf::from("/b.png"),
        ]);
        assert_eq!(drag.mime_types, vec![URI_LIST_MIME.to_string()]);
        assert_eq!(drag.uri_list(), "file:///a.png\r\nfile:///b.png\r\n");
        assert_eq!(drag.file_for_mime("image/png"), None);
    }

    #[test]
    fn unknown_extensions_travel_as_paths_only() {
        let drag = ChromeDrag::new(vec![std::path::PathBuf::from("/thing.qcow2")]);
        assert_eq!(drag.mime_types, vec![URI_LIST_MIME.to_string()]);
        assert_eq!(mime_for_path(std::path::Path::new("x.JPG")), Some("image/jpeg"));
        assert_eq!(mime_for_path(std::path::Path::new("x")), None);
    }
}
