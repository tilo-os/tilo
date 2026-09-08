//! Clipboard, primary selection and drag-and-drop for the session compositor.
//!
//! | protocol | who may bind | what it is for |
//! |---|---|---|
//! | `wl_data_device_manager` | every client | Ctrl+C / Ctrl+V and drag-and-drop |
//! | `zwp_primary_selection_device_manager_v1` | every client | select-then-middle-click |
//! | `zwlr_data_control_manager_v1` | privileged clients only | clipboard managers |
//! | `ext_data_control_manager_v1` | privileged clients only | ditto, the successor |
//!
//! `wl_data_device` is focus-gated by the protocol itself, so every client may
//! bind it safely. The data-control pair sees every selection unconditionally
//! (clipboard managers), which would let a sandboxed app harvest TOTP codes,
//! Wi-Fi passwords etc. off the clipboard — so it's restricted to
//! [`ClientState::privileged`] connections, which the sandbox never gets.
//!
//! Chrome and in-process apps aren't Wayland clients and can't block on one,
//! so a [`SelectionStore`] mirrors selections: pulled from a client
//! asynchronously, read in-process synchronously; writes publish to Wayland
//! on the next tick.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::os::unix::io::{AsFd, FromRawFd, OwnedFd};
use std::sync::{Arc, Mutex};

use smithay::input::Seat;
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

/// Mime type the mirror understands; `text/plain`/`UTF8_STRING`/`STRING`
/// aliases are also offered for X11-heritage clients via Xwayland.
pub const TEXT_MIME: &str = "text/plain;charset=utf-8";

/// Mime types a compositor-owned selection advertises, most-specific first.
pub fn text_mime_types() -> Vec<String> {
    [TEXT_MIME, "text/plain", "UTF8_STRING", "STRING", "TEXT"]
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// Cap on a mirrored selection: bounds what a hostile or broken client can
/// make the compositor buffer. A source exceeding it is dropped, not
/// truncated — half a paste is worse than none.
pub const MAX_SELECTION_BYTES: usize = 8 * 1024 * 1024;
/// Keep an offer bounded even when a malicious client advertises thousands of
/// names.  This is deliberately the same order as the Clipboard portal cap.
pub const MAX_SELECTION_MIMES: usize = 16;
pub const MAX_SELECTION_MIME_BYTES: usize = 128;

/// Bytes a fresh pipe accepts before write blocks (Linux default 64 KiB);
/// kept conservative so the "cannot block" assumption holds even under a
/// smaller default.
const PIPE_BUFFER_BYTES: usize = 32 * 1024;

/// Which of the two selections a value refers to; serde-friendly mirror of
/// smithay's [`SelectionTarget`], usable from the test-control socket and `ui`.
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

/// One side of the mirror.  It retains the actual bytes for the formats qOS
/// has asynchronously pulled from a client, not just a lossy text rendering.
/// That makes this a suitable bridge for the Clipboard portal as well as the
/// in-process UI.
#[derive(Debug, Default, Clone)]
struct Slot {
    /// Payloads keyed by advertised MIME type.  The aggregate is bounded by
    /// `MAX_SELECTION_BYTES`; aliases are resolved on read rather than copied.
    values: BTreeMap<String, Vec<u8>>,
    /// MIME types exposed to Wayland clients when qOS owns the selection.
    mime_types: Vec<String>,
    /// Bumped whenever the selection changes hands, so an old asynchronous
    /// client transfer cannot overwrite a newer selection.
    generation: u64,
    /// Monotonic mirror revision, including arrival of each asynchronously
    /// pulled client MIME payload. Portal bridges poll this without ever
    /// synchronously reading a Wayland client.
    revision: u64,
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

/// The compositor's clipboard mirror, shared between Wayland dispatch and
/// in-process chrome. `Arc<Mutex<_>>` because `ui`'s clipboard hook runs on
/// the UI thread while Wayland writes it from calloop dispatch.
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SelectionSnapshot {
    pub revision: u64,
    pub compositor_owned: bool,
    pub mime_types: Vec<String>,
    pub has_payload: bool,
}

/// Non-blocking metadata for a portal bridge. The caller receives no bytes
/// here and must still use the normal focus-gated broker to request them.
pub fn selection_snapshot(store: &SharedSelection, target: Target) -> SelectionSnapshot {
    store.lock().map_or_else(
        |_| SelectionSnapshot::default(),
        |store| {
            let slot = store.slot(target);
            SelectionSnapshot {
                revision: slot.revision,
                compositor_owned: slot.compositor_owned,
                mime_types: slot.mime_types.clone(),
                has_payload: !slot.values.is_empty(),
            }
        },
    )
}

fn valid_mime(mime: &str) -> bool {
    !mime.is_empty()
        && mime.len() <= MAX_SELECTION_MIME_BYTES
        && !mime.chars().any(|c| c.is_control() || c.is_whitespace())
}

fn is_text_mime(mime: &str) -> bool {
    matches!(
        mime,
        TEXT_MIME | "text/plain" | "UTF8_STRING" | "STRING" | "TEXT"
    )
}

fn normalized_mimes(mimes: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut mimes: Vec<_> = mimes
        .into_iter()
        .filter(|mime| valid_mime(mime))
        .take(MAX_SELECTION_MIMES)
        .collect();
    if mimes.iter().any(|mime| is_text_mime(mime)) {
        for alias in text_mime_types() {
            if !mimes.contains(&alias) && mimes.len() < MAX_SELECTION_MIMES {
                mimes.push(alias);
            }
        }
    }
    mimes.sort();
    mimes.dedup();
    // UTF-8 first avoids old clients choosing an ambiguous X11 alias.
    mimes.sort_by_key(|mime| (mime != TEXT_MIME, mime.clone()));
    mimes
}

fn text_value(slot: &Slot) -> Option<&[u8]> {
    text_mime_types()
        .iter()
        .find_map(|mime| slot.values.get(mime).map(Vec::as_slice))
}

fn value_for(slot: &Slot, mime: &str) -> Option<Vec<u8>> {
    slot.values
        .get(mime)
        .cloned()
        .or_else(|| is_text_mime(mime).then(|| text_value(slot).map(ToOwned::to_owned))?)
}

fn total_bytes(values: &BTreeMap<String, Vec<u8>>) -> usize {
    values
        .values()
        .fold(0usize, |total, bytes| total.saturating_add(bytes.len()))
}

/// MIME types currently available without waiting on a client.  A portal may
/// read only these; it never gets a synchronous path into an arbitrary focused
/// Wayland client.
pub fn mirror_mime_types(store: &SharedSelection, target: Target) -> Vec<String> {
    store
        .lock()
        .map(|store| store.slot(target).mime_types.clone())
        .unwrap_or_default()
}

/// Read bytes already mirrored from `target`.  This is non-blocking and never
/// bypasses Wayland focus/data-control isolation.
pub fn mirror_bytes(store: &SharedSelection, target: Target, mime: &str) -> Option<Vec<u8>> {
    store
        .lock()
        .ok()
        .and_then(|store| value_for(store.slot(target), mime))
}

/// Read `target`'s current text from the mirror. Never blocks on a client or
/// touches Wayland, so safe to call from a widget's `handle_event`.
pub fn mirror_text(store: &SharedSelection, target: Target) -> Option<String> {
    String::from_utf8(mirror_bytes(store, target, TEXT_MIME)?).ok()
}

/// Write `text` to `target` from chrome and mark it for publishing; visible
/// in-process immediately, becomes a Wayland selection on the next tick via
/// [`State::pump_selection`].
pub fn set_mirror_text(store: &SharedSelection, target: Target, text: &str) -> bool {
    set_mirror_bytes(store, target, TEXT_MIME, text.as_bytes().to_vec()).is_ok()
}

/// Replace qOS's pending portal offer.  There is deliberately no publication
/// until payload arrives: offering stale bytes under a newly remote-owned MIME
/// would leak the prior selection.
///
/// # Errors
///
/// Returns an error for an empty, excessive, or malformed MIME offer, or if
/// the shared selection store is unavailable.
pub fn begin_mirror_offer(
    store: &SharedSelection,
    target: Target,
    mimes: &[String],
) -> Result<(), String> {
    if mimes.is_empty() || mimes.len() > MAX_SELECTION_MIMES || mimes.iter().any(|m| !valid_mime(m))
    {
        return Err("invalid or excessive clipboard MIME offer".into());
    }
    let Ok(mut store) = store.lock() else {
        return Err("clipboard selection store is unavailable".into());
    };
    let slot = store.slot_mut(target);
    slot.values.clear();
    // Do not advertise an offered type until its bounded transfer completed.
    // A Wayland client requesting an as-yet-unavailable remote MIME would
    // otherwise receive an empty file and mistake it for legitimate content.
    slot.mime_types.clear();
    slot.generation = slot.generation.wrapping_add(1);
    slot.revision = slot.revision.wrapping_add(1).max(1);
    slot.compositor_owned = false;
    slot.publish_pending = false;
    // Revoke any old portal selection before the remote producer gives us new
    // bytes.  This is an isolation boundary, not merely visual tidiness.
    slot.clear_pending = true;
    Ok(())
}

/// Store one bounded MIME payload and schedule publication to focused Wayland
/// clients. Text aliases resolve to this single value; binary bytes are kept
/// exactly as supplied (not decoded or re-encoded).
///
/// # Errors
///
/// Returns an error for an invalid MIME type, unavailable store, or a payload
/// that would exceed the bounded selection budget.
pub fn set_mirror_bytes(
    store: &SharedSelection,
    target: Target,
    mime: &str,
    bytes: Vec<u8>,
) -> Result<(), String> {
    if !valid_mime(mime) {
        return Err("invalid clipboard MIME type".into());
    }
    let Ok(mut store) = store.lock() else {
        return Err("clipboard selection store is unavailable".into());
    };
    let slot = store.slot_mut(target);
    if !slot.compositor_owned {
        slot.values.clear();
        slot.mime_types.clear();
        slot.generation = slot.generation.wrapping_add(1).max(1);
    }
    let previous = slot.values.insert(mime.to_owned(), bytes);
    if total_bytes(&slot.values) > MAX_SELECTION_BYTES {
        if let Some(previous) = previous {
            slot.values.insert(mime.to_owned(), previous);
        } else {
            slot.values.remove(mime);
        }
        return Err("clipboard selection exceeds the compositor size limit".into());
    }
    if !slot.mime_types.iter().any(|offered| offered == mime) {
        slot.mime_types.push(mime.to_owned());
    }
    slot.mime_types = normalized_mimes(std::mem::take(&mut slot.mime_types));
    slot.compositor_owned = true;
    slot.revision = slot.revision.wrapping_add(1).max(1);
    slot.publish_pending = true;
    slot.clear_pending = false;
    Ok(())
}

/// Drop `target` entirely — no owner, no content.
pub fn clear_mirror(store: &SharedSelection, target: Target) {
    let Ok(mut store) = store.lock() else {
        return;
    };
    let slot = store.slot_mut(target);
    slot.values.clear();
    slot.mime_types.clear();
    slot.generation = slot.generation.wrapping_add(1);
    slot.revision = slot.revision.wrapping_add(1).max(1);
    slot.compositor_owned = false;
    slot.publish_pending = false;
    slot.clear_pending = true;
}

/// Start mirroring a client-owned selection.  This happens before any pipe
/// reads, and the returned generation binds those reads to this exact owner.
fn begin_client_selection(
    store: &SharedSelection,
    target: Target,
    mimes: Option<Vec<String>>,
) -> u64 {
    let Ok(mut store) = store.lock() else {
        return 0;
    };
    let slot = store.slot_mut(target);
    slot.values.clear();
    slot.mime_types = mimes.map_or_else(Vec::new, normalized_mimes);
    slot.generation = slot.generation.wrapping_add(1).max(1);
    slot.revision = slot.revision.wrapping_add(1).max(1);
    slot.compositor_owned = false;
    // A client taking the selection supersedes anything chrome had queued.
    slot.publish_pending = false;
    slot.clear_pending = false;
    slot.generation
}

/// Record one completed asynchronous client transfer, but only if it belongs
/// to the selection owner that was current when the read began.
fn record_client_bytes(
    store: &SharedSelection,
    target: Target,
    generation: u64,
    mime: &str,
    bytes: Vec<u8>,
) {
    let Ok(mut store) = store.lock() else {
        return;
    };
    let slot = store.slot_mut(target);
    if generation == 0 || slot.generation != generation || bytes.len() > MAX_SELECTION_BYTES {
        return;
    }
    let previous = slot.values.insert(mime.to_owned(), bytes);
    if total_bytes(&slot.values) > MAX_SELECTION_BYTES {
        if let Some(previous) = previous {
            slot.values.insert(mime.to_owned(), previous);
        } else {
            slot.values.remove(mime);
        }
    } else {
        slot.revision = slot.revision.wrapping_add(1).max(1);
    }
}

/// Whether the compositor currently owns `target` (i.e. answers `receive` from the mirror).
fn compositor_owns(store: &SharedSelection, target: Target) -> bool {
    store.lock().is_ok_and(|s| s.slot(target).compositor_owned)
}

/// A selection change seen but not yet mirrored. smithay raises
/// [`SelectionHandler::new_selection`] *before* installing the new source, so
/// pulling inline would read the previous owner's data; parked here and
/// drained by [`State::pump_selection`] once the seat holds the new source.
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
    /// A drag the compositor itself started — a file dragged out of the hosted
    /// file manager, across a client window. `Some` from grab start to
    /// finish/cancel. See [`ChromeDrag`].
    pub chrome_drag: Option<ChromeDrag>,
    /// Whether a client-initiated drag is in flight. The pointer router normally
    /// pins focus to the surface holding the implicit grab while a button is
    /// held, but a drag must travel to a *different* surface — pinning here
    /// would mean drop targets never get `wl_data_device.enter`.
    pub dnd_active: bool,
    /// What the drag in flight is carrying, as something a person can
    /// read. See [`crate::compositor::drag_feedback`].
    pub dnd_label: DragLabel,
}

/// Name painted beside the pointer during a drag, answered in two stages:
/// the first frame only has the offer's mime-type list (kind of thing), the
/// real name arrives later via a threaded read of `text/uri-list` and
/// replaces it — "Image" becomes `photo.png`.
#[derive(Clone, Default)]
pub struct DragLabel(Arc<Mutex<Option<String>>>);

impl DragLabel {
    pub fn set(&self, label: Option<String>) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = label;
    }

    pub fn get(&self) -> Option<String> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
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
    /// Advertise all four globals on `display`. Data-control is filtered to
    /// privileged clients only (see module docs); `PrimarySelectionState` is
    /// handed to the wlr data-control global too.
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
            dnd_label: DragLabel::default(),
            chrome_drag: None,
        }
    }
}

/// What a compositor-initiated drag carries. Files (a widget, not a Wayland
/// client) has no `wl_data_source`, so the compositor is the source: paths
/// as `text/uri-list`, plus — for a single known-type file — the bytes
/// themselves, since a sandboxed app can read a pipe but not a host path.
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
        if let [single] = paths.as_slice()
            && let Some(mime) = mime_for_path(single)
        {
            mime_types.push(mime.to_string());
        }
        Self { paths, mime_types }
    }

    /// The `text/uri-list` body: one `file://` URI per line, CRLF-terminated
    /// per RFC 2483, percent-encoded.
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
    use std::fmt::Write as _;
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(b as char);
            }
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

/// Write `bytes` into `fd` without stalling: small payloads fit the pipe
/// buffer and go now; larger ones drain on a thread, whose exit closes `fd` as EOF.
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

/// Data-control filter: only connections on the privileged socket qualify —
/// the sandbox-reachable public socket never sees this global, so sandboxed
/// clipboard tools fall back to focus-gated `wl_data_device` instead of a silent firehose.
fn is_privileged(client: &Client) -> bool {
    client
        .get_data::<ClientState>()
        .is_some_and(|d| d.privileged)
}

/// Flip `fd` non-blocking. Returns `false` if the kernel refused, in which
/// case the caller must not register it — a blocking fd on a level-triggered
/// source would stall the whole compositor.
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

/// Spawn an async read of `fd` into the mirror slot for `target`, as a
/// level-triggered calloop source rather than inline — the client only
/// writes once the compositor flushes `send`, so a blocking read here would
/// deadlock against it.
fn read_into_mirror(
    loop_handle: &LoopHandle<'static, State>,
    store: SharedSelection,
    target: Target,
    generation: u64,
    mime: String,
    fd: OwnedFd,
) {
    // Non-blocking: a level-triggered wakeup with no bytes ready won't stall the loop.
    if !set_nonblocking(&fd) {
        tracing::warn!(target: "shell::selection", "could not set selection pipe non-blocking");
        return;
    }
    // `File` owns the fd for the source's lifetime; calloop hands it back each
    // wakeup until we return `PostAction::Remove`.
    let file = File::from(fd);
    let source = Generic::new(file, Interest::READ, Mode::Level);
    let mut buf: Vec<u8> = Vec::new();
    let mut overflowed = false;
    let registered = loop_handle.insert_source(source, move |_, file, _state| {
        let mut chunk = [0u8; 8192];
        // `&File` impls `Read`; calloop's wrapper only derefs shared, which is all a read needs.
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
                    } else {
                        // Keep raw bytes. Text validation happens only where
                        // a UI wants text; binary MIME values must remain exact.
                        record_client_bytes(
                            &store,
                            target,
                            generation,
                            &mime,
                            std::mem::take(&mut buf),
                        );
                    }
                    return Ok(PostAction::Remove);
                }
                Ok(n) => {
                    // `Read::read` guarantees `n <= chunk.len()`; treat a violation as a hard
                    // read failure rather than slicing blind on a client-influenced count.
                    let Some(read) = chunk.get(..n) else {
                        tracing::warn!(
                            target: "shell::selection",
                            "selection pipe read reported more bytes than the buffer holds"
                        );
                        return Ok(PostAction::Remove);
                    };
                    if buf.len().saturating_add(n) > MAX_SELECTION_BYTES {
                        overflowed = true;
                        buf.clear();
                    } else {
                        buf.extend_from_slice(read);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    return Ok(PostAction::Continue);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
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

    /// Point selection devices at `client`. Called alongside keyboard-focus
    /// changes — smithay gates `set_selection` on keyboard focus, so lagging
    /// selection focus behind it would let a client type while denied the clipboard.
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

    /// Per-tick selection work: publish chrome's pending copy, pull any
    /// client-owned change into the mirror. Allocation-free when nothing changed.
    pub fn pump_selection(&mut self) {
        self.publish_chrome_selection();
        self.drain_pending_pulls();
    }

    /// Turn a pending chrome copy into a compositor-owned Wayland selection.
    fn publish_chrome_selection(&self) {
        for target in [Target::Clipboard, Target::Primary] {
            let action = {
                let Ok(mut store) = self.selection.store.lock() else {
                    continue;
                };
                let slot = store.slot_mut(target);
                if slot.clear_pending {
                    slot.clear_pending = false;
                    Some(None)
                } else if slot.publish_pending {
                    slot.publish_pending = false;
                    Some(Some(slot.mime_types.clone()))
                } else {
                    None
                }
            };
            let dh = self.display_handle.clone();
            match action {
                Some(Some(mime_types)) => match target {
                    Target::Clipboard => {
                        set_data_device_selection(&dh, &self.seat, mime_types, ());
                    }
                    Target::Primary => {
                        set_primary_selection(&dh, &self.seat, mime_types, ());
                    }
                },
                Some(None) => match target {
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
                begin_client_selection(&self.selection.store, pull.target, None);
                continue;
            };
            let generation =
                begin_client_selection(&self.selection.store, pull.target, Some(mimes.clone()));
            // Each request is a non-blocking calloop transfer.  Pull all
            // bounded offered formats (text, PNG/JPEG, and future formats),
            // not only a hand-picked text alias.
            for mime in mimes
                .into_iter()
                .filter(|mime| valid_mime(mime))
                .take(MAX_SELECTION_MIMES)
            {
                let (read_fd, write_fd) = match selection_pipe() {
                    Ok(pair) => pair,
                    Err(e) => {
                        tracing::warn!(target: "shell::selection", error = %e, "could not create selection pipe");
                        continue;
                    }
                };
                // The two helpers return distinct error enums; only the
                // diagnostic matters here.
                let requested = match pull.target {
                    Target::Clipboard => {
                        request_data_device_client_selection(&self.seat, mime.clone(), write_fd)
                            .map_err(|e| e.to_string())
                    }
                    Target::Primary => {
                        request_primary_client_selection(&self.seat, mime.clone(), write_fd)
                            .map_err(|e| e.to_string())
                    }
                };
                match requested {
                    Ok(()) => read_into_mirror(
                        &self.loop_handle,
                        Arc::clone(&self.selection.store),
                        pull.target,
                        generation,
                        mime,
                        read_fd,
                    ),
                    Err(e) => {
                        // `ServerSideSelection` is normal when qOS owns the
                        // source; it already has the bytes in the mirror.
                        tracing::debug!(target: "shell::selection", error = %e, "no client selection to mirror");
                    }
                }
            }
        }
    }

    /// Answer a `receive` on a compositor-owned selection out of the mirror.
    fn serve_selection(&self, target: Target, mime_type: &str, fd: OwnedFd) {
        let Some(bytes) = mirror_bytes(&self.selection.store, target, mime_type) else {
            return;
        };
        write_to_pipe(fd, bytes, "selection");
    }
}

/// The clipboard `ui`'s in-process widgets use — they're linked into the
/// compositor binary, not Wayland clients, and can't block on one (a client
/// only answers once the compositor flushes, which can't happen mid-handler).
/// Reads come from the mirror (always current); writes go into the mirror
/// and raise a publish flag that [`State::pump_selection`] turns into a
/// real Wayland selection next tick.
struct HostClipboard {
    store: SharedSelection,
}

impl ui::clipboard::ClipboardBackend for HostClipboard {
    fn set_text(&self, target: ui::clipboard::ClipboardTarget, text: &str) -> bool {
        if text.len() > ui::clipboard::MAX_CLIPBOARD_IMAGE_BYTES {
            return false;
        }
        set_mirror_text(&self.store, ui_target(target), text)
    }

    fn text(&self, target: ui::clipboard::ClipboardTarget) -> Option<String> {
        mirror_text(&self.store, ui_target(target))
    }

    fn set_image(&self, width: usize, height: usize, rgba: &[u8]) -> bool {
        let Some(expected_len) = width
            .checked_mul(height)
            .and_then(|area| area.checked_mul(4))
        else {
            return false;
        };
        if width == 0 || height == 0 || rgba.len() != expected_len {
            return false;
        }
        let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) else {
            return false;
        };
        if rgba.len() > ui::clipboard::MAX_CLIPBOARD_IMAGE_INPUT_BYTES {
            return false;
        }
        let mut png = Vec::new();
        let mut encoder = png::Encoder::new(&mut png, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let Ok(mut writer) = encoder.write_header() else {
            return false;
        };
        if writer.write_image_data(rgba).is_err() {
            return false;
        }
        drop(writer);
        if png.len() > ui::clipboard::MAX_CLIPBOARD_IMAGE_BYTES {
            return false;
        }
        set_mirror_bytes(&self.store, Target::Clipboard, "image/png", png).is_ok()
    }

    fn name(&self) -> &'static str {
        "compositor-mirror"
    }
}

/// Maps `ui`'s target enum onto this module's — kept separate so `ui`
/// doesn't depend on smithay.
fn ui_target(target: ui::clipboard::ClipboardTarget) -> Target {
    match target {
        ui::clipboard::ClipboardTarget::Clipboard => Target::Clipboard,
        ui::clipboard::ClipboardTarget::Primary => Target::Primary,
    }
}

/// Point `ui`'s clipboard at the compositor's mirror, for hosted widgets.
/// Called once at startup; without it hosted widgets fall back to `ui`'s
/// in-process buffer, invisible to every client.
pub fn install_host_clipboard(store: SharedSelection) {
    ui::clipboard::install_clipboard_backend(Arc::new(HostClipboard { store }));
}

impl SelectionHandler for State {
    /// Compositor selections carry no per-selection data — text lives in
    /// [`SelectionStore`], keyed by target, so in-process chrome can read it without a seat.
    type SelectionUserData = ();

    fn new_selection(
        &mut self,
        ty: SelectionTarget,
        source: Option<SelectionSource>,
        _seat: Seat<Self>,
    ) {
        let target = Target::from(ty);
        // Park the pull — inline would read the *previous* owner, since smithay
        // calls this before installing the new source.
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
            // Only routes here for compositor-owned selections; a mismatch means the
            // mirror and seat disagree — answer with nothing rather than stale content.
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

/// Ask a client's drag source for the name of what it's carrying, via
/// `text/uri-list` (the only mime that carries a name without risking a
/// large payload). May be asked any number of times without disturbing the
/// drop; reads happen on a thread since the client can't answer while this loop blocks.
fn request_dragged_name(source: &WlDataSource, label: DragLabel) {
    let (reader, writer) = match std::io::pipe() {
        Ok(pair) => pair,
        Err(e) => {
            tracing::debug!(target: "shell::selection", error = %e, "no pipe for the drag's name");
            return;
        }
    };
    source.send(URI_LIST_MIME.to_string(), writer.as_fd());
    // Our copy of the write end must close too, or the read below never sees
    // EOF — the client's copy closing alone isn't enough.
    drop(writer);
    let _ = std::thread::Builder::new()
        .name("qos-drag-name".into())
        .spawn(move || {
            use std::io::Read;
            let mut reader = reader;
            let mut body = Vec::new();
            // Bounded: a URI list is small, and a source that streams forever must not
            // pin a thread for the session's life.
            if reader
                .by_ref()
                .take(64 * 1024)
                .read_to_end(&mut body)
                .is_err()
            {
                return;
            }
            if let Some(name) = crate::compositor::drag_feedback::name_from_uri_list(&body) {
                label.set(Some(name));
            }
        });
}

impl ClientDndGrabHandler for State {
    fn started(
        &mut self,
        source: Option<WlDataSource>,
        icon: Option<WlSurface>,
        _seat: Seat<Self>,
    ) {
        // Only the offer (mime list) is available here, not the payload — so this
        // reports the kind of thing being carried, not its name yet.
        let mimes = source
            .as_ref()
            .and_then(|s| {
                smithay::wayland::selection::data_device::with_source_metadata(s, |meta| {
                    meta.mime_types.clone()
                })
                .ok()
            })
            .unwrap_or_default();
        self.selection
            .dnd_label
            .set(Some(crate::compositor::drag_feedback::label_for_mimes(
                &mimes,
            )));
        // Then ask for the name from the URI list the source is already offering —
        // turns "Image" into "photo.png" on screen.
        if let Some(source) = source.as_ref()
            && mimes.iter().any(|m| m == URI_LIST_MIME)
        {
            request_dragged_name(source, self.selection.dnd_label.clone());
        }
        // The drag icon is a client surface with the `dnd_icon` role, but qOS
        // doesn't composite it — the compositor draws its own drag feedback, and a
        // client icon would sit outside the strip layout. Recording it just keeps
        // the role assignment honest per protocol.
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
        self.selection.dnd_label.set(None);
    }
}

/// Server-initiated drag-and-drop — source is compositor chrome, e.g. a file
/// dragged out of the hosted file manager onto a client window (started via
/// `client_input::pump_chrome_drag`, carried as [`ChromeDrag`]). A tab drag
/// between groups never comes here.
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
    use ui::clipboard::ClipboardBackend;

    fn store() -> SharedSelection {
        Arc::new(Mutex::new(SelectionStore::default()))
    }

    /// Chrome's clipboard write must be readable in-process immediately (paste
    /// can't wait for a tick) and leave a publish flag for the real Wayland selection.
    #[test]
    fn chrome_copy_is_readable_immediately_and_queues_a_publish() {
        let s = store();
        set_mirror_text(&s, Target::Clipboard, "from chrome");
        assert_eq!(
            mirror_text(&s, Target::Clipboard).as_deref(),
            Some("from chrome")
        );
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

        let generation =
            begin_client_selection(&s, Target::Clipboard, Some(vec![TEXT_MIME.to_string()]));
        record_client_bytes(
            &s,
            Target::Clipboard,
            generation,
            TEXT_MIME,
            b"from a client".to_vec(),
        );
        assert!(!compositor_owns(&s, Target::Clipboard));
        assert_eq!(
            mirror_text(&s, Target::Clipboard).as_deref(),
            Some("from a client"),
        );
        // The queued chrome publish must not fire after the client won the
        // selection — that would silently take the clipboard back.
        assert!(!s.lock().unwrap().clipboard.publish_pending);
    }

    /// Primary and clipboard are independent; middle-click must not return what Ctrl+C put down.
    #[test]
    fn primary_and_clipboard_do_not_alias() {
        let s = store();
        set_mirror_text(&s, Target::Clipboard, "ctrl-c text");
        set_mirror_text(&s, Target::Primary, "drag-selected text");
        assert_eq!(
            mirror_text(&s, Target::Clipboard).as_deref(),
            Some("ctrl-c text")
        );
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

    /// Offered mimes must lead with the specific UTF-8 type — the first match
    /// wins, and a client stuck with `STRING` has to guess the encoding.
    #[test]
    fn text_mimes_lead_with_utf8() {
        let mimes = text_mime_types();
        assert_eq!(mimes.first().map(String::as_str), Some(TEXT_MIME));
        assert!(mimes.iter().any(|m| m == "text/plain"));
        assert!(mimes.iter().any(|m| m == "UTF8_STRING"));
    }

    #[test]
    fn binary_offer_round_trips_without_text_decoding_and_keeps_text_aliases() {
        let s = store();
        begin_mirror_offer(
            &s,
            Target::Clipboard,
            &["image/png".into(), "text/plain".into()],
        )
        .unwrap();
        let png = vec![0x89, b'P', b'N', b'G', 0, 0xff];
        set_mirror_bytes(&s, Target::Clipboard, "image/png", png.clone()).unwrap();
        set_mirror_bytes(&s, Target::Clipboard, "text/plain", b"hello".to_vec()).unwrap();
        assert_eq!(mirror_bytes(&s, Target::Clipboard, "image/png"), Some(png));
        assert_eq!(
            mirror_bytes(&s, Target::Clipboard, "UTF8_STRING"),
            Some(b"hello".to_vec())
        );
        assert_eq!(mirror_text(&s, Target::Clipboard).as_deref(), Some("hello"));
        let mimes = mirror_mime_types(&s, Target::Clipboard);
        assert!(mimes.iter().any(|mime| mime == "image/png"));
        assert!(mimes.iter().any(|mime| mime == "UTF8_STRING"));
    }

    #[test]
    fn chrome_image_copy_offers_png_and_preserves_text() {
        let s = store();
        set_mirror_text(&s, Target::Clipboard, "caption");
        let host = HostClipboard {
            store: Arc::clone(&s),
        };
        assert!(host.set_image(1, 1, &[0xff, 0x00, 0x80, 0xff]));
        let bytes = mirror_bytes(&s, Target::Clipboard, "image/png").unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(
            mirror_text(&s, Target::Clipboard).as_deref(),
            Some("caption")
        );
        let mimes = mirror_mime_types(&s, Target::Clipboard);
        assert!(mimes.iter().any(|mime| mime == "image/png"));
        assert!(mimes.iter().any(|mime| mime == TEXT_MIME));
    }

    #[test]
    fn chrome_image_copy_rejects_invalid_dimensions_and_payload_size() {
        let s = store();
        let host = HostClipboard { store: s };
        assert!(!host.set_image(0, 1, &[]));
        assert!(!host.set_image(1, 1, &[]));
        assert!(!host.set_image(usize::MAX, 1, &[0; 4]));
        assert!(!host.set_image(
            ui::clipboard::MAX_CLIPBOARD_IMAGE_INPUT_BYTES / 4 + 1,
            1,
            &[0; 4]
        ));
    }

    #[test]
    fn chrome_write_supersedes_stale_client_payloads() {
        let s = store();
        let generation = begin_client_selection(
            &s,
            Target::Clipboard,
            Some(vec!["application/octet-stream".into()]),
        );
        record_client_bytes(
            &s,
            Target::Clipboard,
            generation,
            "application/octet-stream",
            vec![1, 2, 3],
        );
        assert!(set_mirror_text(&s, Target::Clipboard, "fresh"));
        assert_eq!(
            mirror_bytes(&s, Target::Clipboard, "application/octet-stream"),
            None
        );
        assert_eq!(mirror_text(&s, Target::Clipboard).as_deref(), Some("fresh"));
    }

    #[test]
    fn overlarge_or_stale_client_payload_cannot_replace_current_selection() {
        let s = store();
        let old = begin_client_selection(&s, Target::Clipboard, Some(vec!["image/jpeg".into()]));
        let new = begin_client_selection(&s, Target::Clipboard, Some(vec!["image/png".into()]));
        record_client_bytes(&s, Target::Clipboard, old, "image/jpeg", vec![7]);
        assert_eq!(mirror_bytes(&s, Target::Clipboard, "image/jpeg"), None);
        record_client_bytes(
            &s,
            Target::Clipboard,
            new,
            "image/png",
            vec![0; MAX_SELECTION_BYTES + 1],
        );
        assert_eq!(mirror_bytes(&s, Target::Clipboard, "image/png"), None);
    }

    #[test]
    fn a_chrome_drag_offers_a_uri_list_and_the_single_files_bytes() {
        let drag = ChromeDrag::new(vec![std::path::PathBuf::from(
            "/var/workspace/Demo/my photo.png",
        )]);
        assert_eq!(
            drag.mime_types,
            vec![URI_LIST_MIME.to_string(), "image/png".to_string()]
        );
        assert_eq!(
            drag.uri_list(),
            "file:///var/workspace/Demo/my%20photo.png\r\n"
        );
        assert_eq!(
            drag.file_for_mime("image/png"),
            Some(std::path::Path::new("/var/workspace/Demo/my photo.png"))
        );
        assert_eq!(drag.file_for_mime("image/jpeg"), None, "not offered");
        assert_eq!(
            drag.file_for_mime(URI_LIST_MIME),
            None,
            "the list is not a file"
        );
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
        assert_eq!(
            mime_for_path(std::path::Path::new("x.JPG")),
            Some("image/jpeg")
        );
        assert_eq!(mime_for_path(std::path::Path::new("x")), None);
    }
}
