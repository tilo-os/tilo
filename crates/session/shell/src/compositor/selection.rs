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

use std::collections::{BTreeMap, VecDeque};
#[cfg(target_os = "linux")]
use std::fs::File;
#[cfg(target_os = "linux")]
use std::io::Read;
#[cfg(target_os = "linux")]
use std::os::unix::io::{AsFd, FromRawFd, OwnedFd};
use std::sync::{Arc, Mutex};

#[cfg(target_os = "linux")]
use smithay::input::Seat;
#[cfg(target_os = "linux")]
use smithay::reexports::calloop::{Interest, LoopHandle, Mode, PostAction, generic::Generic};
#[cfg(target_os = "linux")]
use smithay::reexports::wayland_server::protocol::wl_data_source::WlDataSource;
#[cfg(target_os = "linux")]
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
#[cfg(target_os = "linux")]
use smithay::reexports::wayland_server::{Client, DisplayHandle};
#[cfg(target_os = "linux")]
use smithay::wayland::selection::data_device::{
    ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
    clear_data_device_selection, request_data_device_client_selection, set_data_device_focus,
    set_data_device_selection,
};
#[cfg(target_os = "linux")]
use smithay::wayland::selection::ext_data_control::DataControlHandler as ExtDataControlHandler;
#[cfg(target_os = "linux")]
use smithay::wayland::selection::ext_data_control::DataControlState as ExtDataControlState;
#[cfg(target_os = "linux")]
use smithay::wayland::selection::primary_selection::{
    PrimarySelectionHandler, PrimarySelectionState, clear_primary_selection,
    request_primary_client_selection, set_primary_focus, set_primary_selection,
};
#[cfg(target_os = "linux")]
use smithay::wayland::selection::wlr_data_control::{DataControlHandler, DataControlState};
#[cfg(target_os = "linux")]
use smithay::wayland::selection::{SelectionHandler, SelectionSource, SelectionTarget};
use tilo_clock::unix_now_ms;

#[cfg(target_os = "linux")]
use crate::compositor::state::{ClientState, State};
use tilo_lock::MutexRecover as _;

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
/// A clipboard manager is useful only while it stays predictable. History is
/// session-private and bounded independently of the larger live selection.
pub const MAX_CLIPBOARD_HISTORY_ITEMS: usize = 50;
pub const MAX_CLIPBOARD_HISTORY_ITEM_BYTES: usize = 256 * 1024;
/// Secret copies (`ui::try_clipboard_copy_secret`) carry this MIME marker to
/// make retention impossible at the compositor boundary.
pub use ui::clipboard::SECRET_MIME;

/// Bytes a fresh pipe accepts before write blocks (Linux default 64 KiB);
/// kept conservative so the "cannot block" assumption holds even under a
/// smaller default.
#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
impl From<SelectionTarget> for Target {
    fn from(t: SelectionTarget) -> Self {
        match t {
            SelectionTarget::Clipboard => Target::Clipboard,
            SelectionTarget::Primary => Target::Primary,
        }
    }
}

#[cfg(target_os = "linux")]
impl From<Target> for SelectionTarget {
    fn from(t: Target) -> Self {
        match t {
            Target::Clipboard => SelectionTarget::Clipboard,
            Target::Primary => SelectionTarget::Primary,
        }
    }
}

/// One side of the mirror.  It retains the actual bytes for the formats Tilo
/// has asynchronously pulled from a client, not just a lossy text rendering.
/// That makes this a suitable bridge for the Clipboard portal as well as the
/// in-process UI.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Slot {
    /// Payloads keyed by advertised MIME type.  The aggregate is bounded by
    /// `MAX_SELECTION_BYTES`; aliases are resolved on read rather than copied.
    values: BTreeMap<String, Vec<u8>>,
    /// MIME types exposed to Wayland clients when Tilo owns the selection.
    mime_types: Vec<String>,
    /// The formats the client source actually offered and Tilo pulls, without
    /// the text aliases added to `mime_types`. A take-over waits for each of
    /// these to finish transferring; an alias resolving to already-arrived
    /// text must not count as a finished transfer of its own.
    pulled: Vec<String>,
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
    /// Payload id and time of the last client selection Tilo took over (see
    /// [`adopt_complete_client_selection`]); stops a clipboard manager that
    /// re-asserts whatever it sees from ping-ponging with the compositor.
    adopted: Option<([u8; 32], u64)>,
}

/// The compositor's clipboard mirror, shared between Wayland dispatch and
/// in-process chrome. `Arc<Mutex<_>>` because `ui`'s clipboard hook runs on
/// the UI thread while Wayland writes it from calloop dispatch.
#[derive(Debug, Default)]
pub struct SelectionStore {
    clipboard: Slot,
    primary: Slot,
    history: VecDeque<ClipboardHistoryItem>,
    revision_watchers: std::sync::Arc<Mutex<Vec<crate::idle_wake::IdleWake>>>,
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

/// Wake `wake` when the clipboard mirror's revision changes.
///
/// The clipboard portal emitter parks on this instead of polling the
/// revision. Primary-selection updates do not poke it.
pub(crate) fn watch_clipboard_revision(store: &SharedSelection, wake: crate::idle_wake::IdleWake) {
    if let Ok(store) = store.lock()
        && let Ok(mut watchers) = store.revision_watchers.lock()
    {
        watchers.push(wake);
    }
}

fn bump_revision(
    slot: &mut Slot,
    watchers: &std::sync::Arc<Mutex<Vec<crate::idle_wake::IdleWake>>>,
    clipboard: bool,
) {
    slot.revision = slot.revision.wrapping_add(1).max(1);
    if !clipboard {
        return;
    }
    if let Ok(watchers) = watchers.lock() {
        for watcher in watchers.iter() {
            watcher.poke();
        }
    }
}

/// Safe metadata shown by the clipboard manager. Payload bytes stay inside the
/// privileged compositor and can only become the live clipboard via
/// [`activate_history_item`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardHistorySnapshot {
    pub id: [u8; 32],
    pub mime_types: Vec<String>,
    pub text_preview: Option<String>,
    pub byte_len: usize,
    pub pinned: bool,
    /// Authenticated fabric author, or `None` when the copy originated in
    /// this compositor. The UI receives identity metadata, never payload
    /// bytes, until the person explicitly activates an item.
    pub source_device: Option<crate::shell::personal_continuity::DeviceId>,
    pub created_unix_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ClipboardHistoryItem {
    id: [u8; 32],
    generation: u64,
    created_unix_ms: u64,
    values: BTreeMap<String, Vec<u8>>,
    mime_types: Vec<String>,
    pinned: bool,
    /// Imported fabric entries remain selectable locally but are not
    /// re-authored and rebroadcast as if they originated on this device.
    fabric_imported: bool,
    source_device: Option<crate::shell::personal_continuity::DeviceId>,
}

/// Payload-safe export used only by the encrypted same-person device lane.
/// The primary selection and secret MIME type never reach this structure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShareableClipboardSnapshot {
    pub id: [u8; 32],
    pub mime: String,
    pub bytes: Vec<u8>,
    pub created_unix_ms: u64,
    pub pinned: bool,
}

impl ClipboardHistoryItem {
    fn snapshot(&self) -> ClipboardHistorySnapshot {
        let text_preview = text_mime_types()
            .iter()
            .find_map(|mime| self.values.get(mime))
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .map(|text| text.chars().take(160).collect());
        ClipboardHistorySnapshot {
            id: self.id,
            mime_types: self.mime_types.clone(),
            text_preview,
            byte_len: total_bytes(&self.values),
            pinned: self.pinned,
            source_device: self.source_device.clone(),
            created_unix_ms: self.created_unix_ms,
        }
    }
}

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

/// Password managers (`1Password`, `KeePassXC`) and Tilo's own secret copies
/// mark a copy with this format. The copy still has to paste, so it is
/// persisted like any other, but it is kept out of history, never logged, and
/// wiped when the next selection replaces it.
pub use ui::clipboard::PASSWORD_MANAGER_HINT_MIME;

/// Whether an offer carries a marker that forbids retention beyond the live selection.
fn is_secret_offer(mimes: &[String]) -> bool {
    mimes
        .iter()
        .any(|mime| mime == SECRET_MIME || mime == PASSWORD_MANAGER_HINT_MIME)
}

/// X11 selection meta targets that Xwayland bridges expose as MIME types. They
/// describe the selection rather than carry content: a source answers them
/// with nothing useful, so they are never pulled and never block taking the
/// clipboard over.
fn is_x11_meta_target(mime: &str) -> bool {
    matches!(
        mime,
        "TARGETS"
            | "TIMESTAMP"
            | "MULTIPLE"
            | "SAVE_TARGETS"
            | "DELETE"
            | "INSERT_SELECTION"
            | "INSERT_PROPERTY"
            | "LENGTH"
            | "HOST_NAME"
            | "USER"
            | "CLIENT_WINDOW"
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

fn history_id(values: &BTreeMap<String, Vec<u8>>) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"tilo-clipboard-history-v1\0");
    for (mime, bytes) in values {
        hash.update(mime.as_bytes());
        hash.update(&[0]);
        hash.update(bytes);
    }
    *hash.finalize().as_bytes()
}

fn record_clipboard_history(store: &mut SelectionStore) {
    let slot = &store.clipboard;
    if slot.values.is_empty()
        || is_secret_offer(&slot.mime_types)
        || total_bytes(&slot.values) > MAX_CLIPBOARD_HISTORY_ITEM_BYTES
    {
        return;
    }
    let id = history_id(&slot.values);
    if store
        .history
        .front()
        .is_some_and(|item| item.generation == slot.generation)
    {
        let pinned = store.history.front().is_some_and(|item| item.pinned);
        let created_unix_ms = store
            .history
            .front()
            .map_or_else(unix_now_ms, |item| item.created_unix_ms);
        store.history.pop_front();
        store.history.push_front(ClipboardHistoryItem {
            id,
            generation: slot.generation,
            created_unix_ms,
            values: slot.values.clone(),
            mime_types: slot.mime_types.clone(),
            pinned,
            fabric_imported: false,
            source_device: None,
        });
        return;
    }
    let pinned = store
        .history
        .iter()
        .find(|item| item.id == id)
        .is_some_and(|item| item.pinned);
    store.history.retain(|item| item.id != id);
    while store.history.len() >= MAX_CLIPBOARD_HISTORY_ITEMS {
        let Some(index) = store.history.iter().rposition(|item| !item.pinned) else {
            return;
        };
        store.history.remove(index);
    }
    store.history.push_front(ClipboardHistoryItem {
        id,
        generation: slot.generation,
        created_unix_ms: unix_now_ms(),
        values: slot.values.clone(),
        mime_types: slot.mime_types.clone(),
        pinned,
        fabric_imported: false,
        source_device: None,
    });
}

/// Export safe retained clipboard payloads for the encrypted personal fabric.
/// Text is canonicalized to UTF-8; otherwise one exact binary MIME is used.
pub fn shareable_clipboard_history(store: &SharedSelection) -> Vec<ShareableClipboardSnapshot> {
    let Ok(store) = store.lock() else {
        return Vec::new();
    };
    store
        .history
        .iter()
        .filter(|item| !item.fabric_imported)
        .filter_map(|item| {
            let (mime, bytes) = text_mime_types()
                .iter()
                .find_map(|mime| item.values.get(mime).map(|bytes| (TEXT_MIME, bytes)))
                .or_else(|| {
                    item.values
                        .iter()
                        .find(|(mime, _)| mime.as_str() != SECRET_MIME)
                        .map(|(mime, bytes)| (mime.as_str(), bytes))
                })?;
            Some(ShareableClipboardSnapshot {
                id: item.id,
                mime: mime.to_string(),
                bytes: bytes.clone(),
                created_unix_ms: item.created_unix_ms,
                pinned: item.pinned,
            })
        })
        .collect()
}

/// Merge authenticated personal-fabric items into history without exposing
/// them as the active Wayland clipboard until the person chooses one.
pub fn merge_shared_clipboard_history(
    store: &SharedSelection,
    entries: &[crate::shell::device_fabric::ClipboardEntry],
) {
    let Ok(mut store) = store.lock() else {
        return;
    };
    for entry in entries.iter().rev() {
        let (mime, bytes) = match &entry.payload {
            crate::shell::device_fabric::ClipboardPayload::Text(text) => {
                (TEXT_MIME.to_string(), text.as_bytes().to_vec())
            }
            crate::shell::device_fabric::ClipboardPayload::Binary { mime, bytes } => {
                (mime.clone(), bytes.clone())
            }
        };
        if store.history.iter().any(|item| item.id == entry.id) {
            continue;
        }
        let mut values = BTreeMap::new();
        values.insert(mime.clone(), bytes);
        while store.history.len() >= MAX_CLIPBOARD_HISTORY_ITEMS {
            let Some(index) = store.history.iter().rposition(|item| !item.pinned) else {
                return;
            };
            store.history.remove(index);
        }
        store.history.push_front(ClipboardHistoryItem {
            id: entry.id,
            generation: 0,
            created_unix_ms: entry.created_unix_ms,
            values,
            mime_types: normalized_mimes([mime]),
            pinned: entry.pinned,
            fabric_imported: true,
            source_device: Some(entry.origin.clone()),
        });
    }
}

/// Remove retained entries imported from the personal fabric. The active
/// Wayland selection is intentionally left alone: a person may already have
/// activated one, and policy revocation stops projection rather than erasing
/// an explicit local clipboard choice.
pub fn clear_shared_clipboard_history(store: &SharedSelection) {
    if let Ok(mut store) = store.lock() {
        store.history.retain(|item| !item.fabric_imported);
    }
}

/// Clipboard-manager metadata, newest first. Primary selection is never
/// retained and payload bytes never leave the privileged compositor here.
pub fn clipboard_history(store: &SharedSelection) -> Vec<ClipboardHistorySnapshot> {
    store
        .lock()
        .map(|store| {
            store
                .history
                .iter()
                .map(ClipboardHistoryItem::snapshot)
                .collect()
        })
        .unwrap_or_default()
}

/// Make a retained item the live clipboard and queue normal Wayland
/// publication. This is the only payload-reading clipboard-manager action.
pub fn activate_history_item(store: &SharedSelection, id: [u8; 32]) -> bool {
    let Ok(mut store) = store.lock() else {
        return false;
    };
    let Some(index) = store.history.iter().position(|item| item.id == id) else {
        return false;
    };
    let Some(mut item) = store.history.remove(index) else {
        return false;
    };
    let watchers = std::sync::Arc::clone(&store.revision_watchers);
    let slot = &mut store.clipboard;
    slot.values.clone_from(&item.values);
    slot.mime_types.clone_from(&item.mime_types);
    slot.generation = slot.generation.wrapping_add(1).max(1);
    bump_revision(slot, &watchers, true);
    slot.compositor_owned = true;
    slot.publish_pending = true;
    slot.clear_pending = false;
    item.generation = slot.generation;
    store.history.push_front(item);
    true
}

pub fn pin_history_item(store: &SharedSelection, id: [u8; 32], pinned: bool) -> bool {
    let Ok(mut store) = store.lock() else {
        return false;
    };
    let Some(item) = store.history.iter_mut().find(|item| item.id == id) else {
        return false;
    };
    item.pinned = pinned;
    true
}

pub fn remove_history_item(store: &SharedSelection, id: [u8; 32]) -> bool {
    let Ok(mut store) = store.lock() else {
        return false;
    };
    let before = store.history.len();
    store.history.retain(|item| item.id != id);
    store.history.len() != before
}

/// Clear ordinary history while preserving explicitly pinned entries.
pub fn clear_clipboard_history(store: &SharedSelection) {
    if let Ok(mut store) = store.lock() {
        store.history.retain(|item| item.pinned);
    }
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
    // Ctrl+C starts a new offer even when chrome already owned the clipboard;
    // otherwise unrelated successive copies would be merged into one history
    // generation. Primary selection remains ephemeral and is not retained.
    if let Ok(mut store) = store.lock() {
        let slot = store.slot_mut(target);
        wipe_if_secret(slot);
        slot.values.clear();
        slot.mime_types.clear();
        slot.compositor_owned = false;
    }
    set_mirror_bytes(store, target, TEXT_MIME, text.as_bytes().to_vec()).is_ok()
}

/// Write a secret `text` to the clipboard from chrome: the same as
/// [`set_mirror_text`], but offered with [`SECRET_MIME`] and
/// [`PASSWORD_MANAGER_HINT_MIME`] from the start, so it never enters
/// history or device sync and is wiped when the next selection replaces it.
pub fn set_mirror_secret_text(store: &SharedSelection, text: &str) -> bool {
    let Ok(mut store) = store.lock() else {
        return false;
    };
    let watchers = std::sync::Arc::clone(&store.revision_watchers);
    let slot = store.slot_mut(Target::Clipboard);
    wipe_if_secret(slot);
    slot.values.clear();
    for marker in [SECRET_MIME, PASSWORD_MANAGER_HINT_MIME] {
        slot.values
            .insert(marker.to_owned(), ui::clipboard::SECRET_MARKER.to_vec());
    }
    slot.values
        .insert(TEXT_MIME.to_owned(), text.as_bytes().to_vec());
    slot.mime_types = normalized_mimes(slot.values.keys().cloned());
    slot.generation = slot.generation.wrapping_add(1).max(1);
    slot.compositor_owned = true;
    bump_revision(slot, &watchers, true);
    slot.publish_pending = true;
    slot.clear_pending = false;
    true
}

/// Replace Tilo's pending portal offer.  There is deliberately no publication
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
    let clipboard = target == Target::Clipboard;
    let watchers = std::sync::Arc::clone(&store.revision_watchers);
    let slot = store.slot_mut(target);
    slot.values.clear();
    // Do not advertise an offered type until its bounded transfer completed.
    // A Wayland client requesting an as-yet-unavailable remote MIME would
    // otherwise receive an empty file and mistake it for legitimate content.
    slot.mime_types.clear();
    slot.generation = slot.generation.wrapping_add(1);
    bump_revision(slot, &watchers, clipboard);
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
    let clipboard = target == Target::Clipboard;
    let watchers = std::sync::Arc::clone(&store.revision_watchers);
    {
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
        bump_revision(slot, &watchers, clipboard);
        slot.publish_pending = true;
        slot.clear_pending = false;
    }
    if target == Target::Clipboard {
        record_clipboard_history(&mut store);
    }
    Ok(())
}

/// Drop `target` entirely — no owner, no content.
pub fn clear_mirror(store: &SharedSelection, target: Target) {
    let Ok(mut store) = store.lock() else {
        return;
    };
    let clipboard = target == Target::Clipboard;
    let watchers = std::sync::Arc::clone(&store.revision_watchers);
    let slot = store.slot_mut(target);
    wipe_if_secret(slot);
    slot.values.clear();
    slot.mime_types.clear();
    slot.generation = slot.generation.wrapping_add(1);
    bump_revision(slot, &watchers, clipboard);
    slot.compositor_owned = false;
    slot.publish_pending = false;
    slot.clear_pending = true;
}

/// Overwrite a password-manager copy's bytes before they are dropped.
fn wipe_if_secret(slot: &mut Slot) {
    use zeroize::Zeroize as _;
    if is_secret_offer(&slot.mime_types) {
        for bytes in slot.values.values_mut() {
            bytes.zeroize();
        }
    }
}

/// Start mirroring a client-owned selection.  This happens before any pipe
/// reads, and the returned generation binds those reads to this exact owner.
pub fn begin_client_selection(
    store: &SharedSelection,
    target: Target,
    mimes: Option<Vec<String>>,
) -> u64 {
    let Ok(mut store) = store.lock() else {
        return 0;
    };
    let clipboard = target == Target::Clipboard;
    let watchers = std::sync::Arc::clone(&store.revision_watchers);
    let slot = store.slot_mut(target);
    wipe_if_secret(slot);
    slot.values.clear();
    slot.pulled = mimes
        .iter()
        .flatten()
        .filter(|mime| valid_mime(mime) && !is_x11_meta_target(mime))
        .take(MAX_SELECTION_MIMES)
        .cloned()
        .collect();
    slot.mime_types = mimes.map_or_else(Vec::new, normalized_mimes);
    slot.generation = slot.generation.wrapping_add(1).max(1);
    bump_revision(slot, &watchers, clipboard);
    slot.compositor_owned = false;
    // A client taking the selection supersedes anything chrome had queued.
    slot.publish_pending = false;
    slot.clear_pending = false;
    slot.generation
}

/// Record one completed asynchronous client transfer, but only if it belongs
/// to the selection owner that was current when the read began.
pub fn record_client_bytes(
    store: &SharedSelection,
    target: Target,
    generation: u64,
    mime: &str,
    bytes: Vec<u8>,
) {
    let Ok(mut store) = store.lock() else {
        return;
    };
    let clipboard = target == Target::Clipboard;
    let watchers = std::sync::Arc::clone(&store.revision_watchers);
    let accepted = {
        let slot = store.slot_mut(target);
        if generation == 0 || slot.generation != generation {
            return;
        }
        if bytes.len() > MAX_SELECTION_BYTES {
            tracing::info!(
                target: "shell::selection",
                ?target, mime, bytes = bytes.len(), limit = MAX_SELECTION_BYTES,
                "client selection dropped: payload exceeds the mirror limit"
            );
            return;
        }
        tracing::debug!(
            target: "shell::selection",
            ?target, mime, bytes = bytes.len(), "client selection format mirrored"
        );
        let previous = slot.values.insert(mime.to_owned(), bytes);
        if total_bytes(&slot.values) > MAX_SELECTION_BYTES {
            if let Some(previous) = previous {
                slot.values.insert(mime.to_owned(), previous);
            } else {
                slot.values.remove(mime);
            }
            tracing::info!(
                target: "shell::selection",
                ?target, mime, limit = MAX_SELECTION_BYTES,
                "client selection dropped: formats together exceed the mirror limit"
            );
            false
        } else {
            bump_revision(slot, &watchers, clipboard);
            true
        }
    };
    if accepted && target == Target::Clipboard {
        record_clipboard_history(&mut store);
    }
}

/// Keep a client's clipboard alive after the client is gone.
///
/// A Wayland selection is only as long-lived as the `wl_data_source` behind
/// it: when the copying client exits (`wl-copy`, a closed window) the seat
/// drops the selection and every later paste finds "nothing copied" — even
/// though Tilo already holds every byte in the mirror. GNOME/KDE avoid that
/// with a clipboard manager. Tilo does it here: once a client-owned
/// clipboard has been mirrored completely (every advertised format
/// transferred), the compositor takes the selection over from the mirror, so
/// it outlives its source. Only the clipboard is taken over; primary
/// selection stays ephemeral, as on every other desktop.
///
/// Returns whether ownership changed. The same payload is not taken over
/// again within [`ADOPT_COOLDOWN_MS`], so a clipboard manager that re-copies
/// the selection it sees cannot start a ping-pong with the compositor.
pub fn adopt_complete_client_selection(store: &SharedSelection, now_ms: u64) -> bool {
    let Ok(mut store) = store.lock() else {
        return false;
    };
    let slot = &mut store.clipboard;
    if slot.compositor_owned || slot.generation == 0 {
        return false;
    }
    // Every pulled format must have finished transferring (an entry exists,
    // even an empty one), or the source is still the only holder of it. A
    // take-over while a pull is in flight would make Xwayland's bridge own the
    // selection it is still reading from, which then answers itself with
    // nothing.
    let formats: Vec<String> = slot.pulled.clone();
    if formats.is_empty()
        || !formats
            .iter()
            .all(|mime| slot.values.contains_key(mime.as_str()))
    {
        return false;
    }
    let formats = slot
        .mime_types
        .iter()
        .filter(|mime| !is_x11_meta_target(mime))
        .cloned();
    // Keep the formats that returned bytes. Chromium and Electron also offer
    // internal formats (a source-frame token, an empty `text/html`) that read
    // back empty, and X11 sources offer meta targets; none of those may stop
    // the copy from outliving its source. A copy with no real content at all
    // is an aborted transfer: taking over would replace a working source with
    // nothing.
    let held: Vec<String> = formats
        .filter(|mime| value_for(slot, mime).is_some_and(|bytes| !bytes.is_empty()))
        .collect();
    let has_content = held
        .iter()
        .any(|mime| is_text_mime(mime) || mime.starts_with("text/") || mime.starts_with("image/"));
    if !has_content {
        return false;
    }
    slot.mime_types = held;
    slot.values.retain(|_, bytes| !bytes.is_empty());
    let id = history_id(&slot.values);
    if slot
        .adopted
        .is_some_and(|(last, at)| last == id && now_ms.saturating_sub(at) < ADOPT_COOLDOWN_MS)
    {
        return false;
    }
    slot.adopted = Some((id, now_ms));
    slot.compositor_owned = true;
    slot.publish_pending = true;
    slot.clear_pending = false;
    true
}

/// See [`adopt_complete_client_selection`].
const ADOPT_COOLDOWN_MS: u64 = 2_000;

/// Whether the compositor currently owns `target` (i.e. answers `receive` from the mirror).
pub fn compositor_owns(store: &SharedSelection, target: Target) -> bool {
    store.lock().is_ok_and(|s| s.slot(target).compositor_owned)
}

/// A selection change already recorded in the mirror but not yet pulled.
/// smithay raises [`SelectionHandler::new_selection`] *before* installing the
/// new source, so pulling inline would read the previous owner's data; the
/// reads are parked here and drained by [`State::pump_selection`] once the
/// seat holds the new source.
#[derive(Debug, Clone)]
#[cfg(target_os = "linux")]
pub struct PendingMirrorPull {
    /// Which selection changed hands.
    pub target: Target,
    /// The mirror generation [`begin_client_selection`] gave the new owner.
    pub generation: u64,
    /// The mime types the new owner offered, or `None` when the selection was
    /// cleared (`set_selection(null)`).
    pub mime_types: Option<Vec<String>>,
}

/// Everything the compositor needs to broker selections, hung off [`State`].
#[cfg(target_os = "linux")]
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
    /// Payload requested while a client drag is still alive. Wayland cancels
    /// a source released over compositor chrome (there is no target
    /// `wl_surface`) before `ClientDndGrabHandler::dropped` returns, so the
    /// shell must begin this transfer before release.
    client_drag_capture: Option<Arc<Mutex<ClientDragCapture>>>,
    /// A release over shell-owned space waiting for the asynchronous pipe
    /// reads above. The native frame pump resolves its point through the same
    /// placement grammar as an app/tab drag.
    pending_shell_drop: Option<PendingShellDrop>,
    /// Exact shared-row target painted for a client drag. Captured on motion
    /// and committed after the asynchronous payload transfer completes.
    pub(crate) shell_drop_placement: Option<crate::shell::tab_binding::Placement>,
}

struct ClientDragCapture {
    /// 0 = drag live, 1 = shell owns the drop, 2 = another client/cancel owns it.
    disposition: u8,
    paths: Option<Vec<std::path::PathBuf>>,
    raw_path: Option<std::path::PathBuf>,
    wait_timeout: std::time::Duration,
    uri_transfer_complete: bool,
    raw_transfer_complete: bool,
}

impl Default for ClientDragCapture {
    fn default() -> Self {
        Self {
            disposition: 0,
            paths: None,
            raw_path: None,
            wait_timeout: std::time::Duration::from_secs(10),
            uri_transfer_complete: true,
            raw_transfer_complete: true,
        }
    }
}

struct DragTransferCompletion {
    capture: Arc<Mutex<ClientDragCapture>>,
    uri: bool,
}

impl Drop for DragTransferCompletion {
    fn drop(&mut self) {
        let mut capture = self.capture.lock_recover();
        if self.uri {
            capture.uri_transfer_complete = true;
        } else {
            capture.raw_transfer_complete = true;
        }
    }
}

struct PendingShellDrop {
    placement: crate::shell::tab_binding::Placement,
    capture: Arc<Mutex<ClientDragCapture>>,
    queued_at: std::time::Instant,
    wait_timeout: std::time::Duration,
}

fn drag_import_directory() -> Option<std::path::PathBuf> {
    tilo_paths::xdg::runtime_dir()
        .map(|runtime| runtime.join(format!("tilo-drag-imports-{}", std::process::id())))
}

#[cfg(target_os = "linux")]
impl Drop for SelectionState {
    fn drop(&mut self) {
        let Some(directory) = drag_import_directory() else {
            return;
        };
        // This directory is owned by this compositor process. Refuse to
        // recurse through a replacement symlink even during teardown.
        if std::fs::symlink_metadata(&directory).is_ok_and(|metadata| {
            metadata.file_type().is_dir() && !metadata.file_type().is_symlink()
        }) {
            let _ = std::fs::remove_dir_all(directory);
        }
    }
}

/// Name painted beside the pointer during a drag, answered in two stages:
/// the first frame only has the offer's mime-type list (kind of thing), the
/// real name arrives later via a threaded read of `text/uri-list` and
/// replaces it — "Image" becomes `photo.png`.
#[derive(Clone, Default)]
pub struct DragLabel(Arc<Mutex<crate::compositor::drag_feedback::DragVisual>>);

impl DragLabel {
    pub fn set(&self, label: Option<String>) {
        let mut visual = self.0.lock_recover();
        if label.is_none() {
            *visual = crate::compositor::drag_feedback::DragVisual::default();
        } else {
            visual.label = label;
        }
    }

    pub fn get(&self) -> Option<String> {
        self.0.lock_recover().label.clone()
    }

    pub fn set_preview(&self, preview: crate::compositor::drag_feedback::DragPreview) {
        self.0.lock_recover().preview = Some(preview);
    }

    pub fn snapshot(&self) -> crate::compositor::drag_feedback::DragVisual {
        self.0.lock_recover().clone()
    }
}

#[cfg(target_os = "linux")]
impl std::fmt::Debug for SelectionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SelectionState")
            .field("store", &self.store)
            .field("pending_pulls", &self.pending_pulls)
            .finish_non_exhaustive()
    }
}

#[cfg(target_os = "linux")]
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
            client_drag_capture: None,
            pending_shell_drop: None,
            shell_drop_placement: None,
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

pub use ui::dnd::{URI_LIST_MIME, mime_for_path};

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
        ui::dnd::uri_list(&self.paths)
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

/// Write `bytes` into `fd` without stalling: small payloads fit the pipe
/// buffer and go now; larger ones drain on a thread, whose exit closes `fd` as EOF.
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
fn is_privileged(client: &Client) -> bool {
    client
        .get_data::<ClientState>()
        .is_some_and(|d| d.privileged)
}

/// Flip `fd` non-blocking. Returns `false` if the kernel refused, in which
/// case the caller must not register it — a blocking fd on a level-triggered
/// source would stall the whole compositor.
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
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
                            ?target,
                            mime,
                            limit = MAX_SELECTION_BYTES,
                            "selection source exceeded the mirror size limit; dropping it"
                        );
                        // Finished, with nothing kept: the format is dropped
                        // rather than left pending forever.
                        record_client_bytes(&store, target, generation, &mime, Vec::new());
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
                    record_client_bytes(&store, target, generation, &mime, Vec::new());
                    return Ok(PostAction::Remove);
                }
            }
        }
    });
    if let Err(e) = registered {
        tracing::warn!(target: "shell::selection", error = %e, "could not register selection reader");
    }
}

#[cfg(target_os = "linux")]
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
        // Dispatch may already have installed a newer client source while
        // the previous mirror's last transfer completed. Incorporate that
        // ownership edge first: adopting the old mirror would overwrite the
        // new source before its formats can even be pulled.
        self.drain_pending_pulls();
        adopt_complete_client_selection(&self.selection.store, unix_now_ms());
        self.publish_chrome_selection();
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
        let mut pending = std::mem::take(&mut self.selection.pending_pulls);
        // `request_*_client_selection` always reads the seat's *current*
        // source, so pulling for an owner already replaced within this tick
        // would fetch the newer owner's bytes under the older owner's offer.
        // Only the newest change per target is real.
        let mut seen = Vec::new();
        pending.reverse();
        pending.retain(|pull| {
            let fresh = !seen.contains(&pull.target);
            seen.push(pull.target);
            fresh
        });
        pending.reverse();
        for pull in pending {
            // A release (`None`) has nothing to read; the mirror already
            // dropped the old owner when it was announced.
            let Some(mimes) = pull.mime_types else {
                continue;
            };
            let generation = pull.generation;
            // Each request is a non-blocking calloop transfer. Pull all
            // bounded offered formats, except GTK's clipboard-manager
            // operation SAVE_TARGETS. It has no data payload; requesting it
            // leaves GTK 3's selection queue waiting and stalls later copies.
            for mime in mimes
                .into_iter()
                .filter(|mime| valid_mime(mime) && !is_x11_meta_target(mime))
                .take(MAX_SELECTION_MIMES)
            {
                let (read_fd, write_fd) = match selection_pipe() {
                    Ok(pair) => pair,
                    Err(e) => {
                        tracing::warn!(target: "shell::selection", error = %e, "could not create selection pipe");
                        record_client_bytes(
                            &self.selection.store,
                            pull.target,
                            generation,
                            &mime,
                            Vec::new(),
                        );
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
                    Ok(()) => {
                        tracing::debug!(
                            target: "shell::selection",
                            target_selection = ?pull.target, mime = %mime, generation,
                            "asked the client selection owner for a format"
                        );
                        read_into_mirror(
                            &self.loop_handle,
                            Arc::clone(&self.selection.store),
                            pull.target,
                            generation,
                            mime,
                            read_fd,
                        );
                    }
                    Err(e) => {
                        // `ServerSideSelection` is normal when Tilo owns the
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
            tracing::info!(
                target: "shell::selection",
                ?target, mime = mime_type,
                "selection request answered with nothing: the mirror holds no bytes for that format"
            );
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
/// real Wayland selection next tick. A copy therefore settles before it
/// returns, and chrome reads its outcome at once.
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

    fn set_secret_text(&self, text: &str) -> bool {
        if text.len() > ui::clipboard::MAX_CLIPBOARD_IMAGE_BYTES {
            return false;
        }
        set_mirror_secret_text(&self.store, text)
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
        let Ok(png) = ui::png::encode_rgba8(width, height, rgba) else {
            return false;
        };
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

/// Install the host services hosted widgets use: the native desktop's, with
/// the clipboard pointed at the compositor's mirror and Tilo links to the
/// session's workspaces opened in place. Called once at startup; without it
/// hosted widgets fall back to `ui`'s in-process buffer, invisible to every
/// client.
pub fn install_host_services(
    store: SharedSelection,
    tree: crate::shell::state::WorkspaceTreeHandle,
) {
    let mut services = host_services(store);
    services.opener = crate::shell::link_open::session_opener(tree, services.opener);
    ui::host::install_default(services);
}

/// The services in-process widgets see: native, except that the clipboard is
/// the compositor's own mirror.
pub(crate) fn host_services(store: SharedSelection) -> ui::host::HostServices {
    ui::host::native::services(None).with_clipboard(Arc::new(HostClipboard { store }))
}

#[cfg(target_os = "linux")]
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
        // The mirror follows the new owner now: until its bytes arrive it
        // must not be taken over as the previous owner's complete copy (that
        // would install the older copy over this one), nor serve the older
        // copy to chrome.
        let mime_types = source.map(|s| s.mime_types());
        let generation = begin_client_selection(&self.selection.store, target, mime_types.clone());
        // Park the reads — inline would read the *previous* owner, since smithay
        // calls this before installing the new source.
        self.selection.pending_pulls.push(PendingMirrorPull {
            target,
            generation,
            mime_types,
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
        tracing::debug!(
            target: "shell::selection",
            ?target, mime = %mime_type,
            compositor_owned = compositor_owns(&self.selection.store, target),
            "a client asked for the selection's bytes"
        );
        if !compositor_owns(&self.selection.store, target) {
            // Only routes here for compositor-owned selections; a mismatch means the
            // mirror and seat disagree — answer with nothing rather than stale content.
            return;
        }
        self.serve_selection(target, &mime_type, fd);
    }
}

#[cfg(target_os = "linux")]
impl DataDeviceHandler for State {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.selection.data_device_state
    }
}

#[cfg(target_os = "linux")]
impl PrimarySelectionHandler for State {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.selection.primary_selection_state
    }
}

#[cfg(target_os = "linux")]
impl DataControlHandler for State {
    fn data_control_state(&self) -> &DataControlState {
        &self.selection.data_control_state
    }
}

#[cfg(target_os = "linux")]
impl ExtDataControlHandler for State {
    fn data_control_state(&self) -> &ExtDataControlState {
        &self.selection.ext_data_control_state
    }
}

/// Ask a client's drag source for the name of what it's carrying, via
/// `text/uri-list` (the only mime that carries a name without risking a
/// large payload). May be asked any number of times without disturbing the
/// drop; reads happen on a thread since the client can't answer while this loop blocks.
#[cfg(target_os = "linux")]
fn request_dragged_name(
    source: &WlDataSource,
    label: DragLabel,
    capture: Arc<Mutex<ClientDragCapture>>,
) {
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
    capture.lock_recover().uri_transfer_complete = false;
    let completion = DragTransferCompletion {
        capture: capture.clone(),
        uri: true,
    };
    let _ = std::thread::Builder::new()
        .name("tilo-drag-name".into())
        .spawn(move || {
            use std::io::Read as _;

            let _completion = completion;
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
            let paths = paths_from_uri_list(&body);
            if !paths.is_empty() {
                let previewable: Vec<_> = paths
                    .iter()
                    .filter(|path| path.starts_with("/var/workspace"))
                    .cloned()
                    .collect();
                if !previewable.is_empty() {
                    request_path_drag_preview(&previewable, label.clone());
                }
                capture.lock_recover().paths = Some(paths);
            }
        });
}

fn paths_from_uri_list(body: &[u8]) -> Vec<std::path::PathBuf> {
    String::from_utf8_lossy(body)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            let uri = url::Url::parse(line).ok()?;
            (uri.scheme() == "file").then(|| uri.to_file_path().ok())?
        })
        .collect()
}

fn preferred_drag_content_mime(mimes: &[String]) -> Option<&str> {
    mimes.iter().map(String::as_str).find(|mime| {
        *mime != URI_LIST_MIME
            && *mime != SECRET_MIME
            && (mime.starts_with("image/")
                || mime.starts_with("video/")
                || mime.starts_with("audio/")
                || mime.starts_with("text/")
                || matches!(
                    *mime,
                    "application/pdf"
                        | "application/json"
                        | "application/javascript"
                        | "application/xml"
                ))
    })
}

fn drag_content_suffix(mime: &str) -> &'static str {
    match mime {
        "image/png" => ".png",
        "image/jpeg" => ".jpg",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        "image/bmp" => ".bmp",
        "image/avif" => ".avif",
        "image/svg+xml" => ".svg",
        "video/mp4" => ".mp4",
        "video/webm" => ".webm",
        "video/x-matroska" => ".mkv",
        "video/quicktime" => ".mov",
        "audio/mpeg" => ".mp3",
        "audio/ogg" => ".ogg",
        "application/pdf" => ".pdf",
        "application/json" => ".json",
        "application/javascript" | "text/javascript" => ".js",
        "application/xml" => ".xml",
        "text/html" => ".html",
        "text/markdown" | "text/x-markdown" => ".md",
        "text/css" => ".css",
        mime if mime.starts_with("text/") => ".txt",
        _ => ".bin",
    }
}

fn drag_content_limit(mime: &str) -> (u64, std::time::Duration) {
    if mime.starts_with("video/") {
        (512 * 1024 * 1024, std::time::Duration::from_secs(60))
    } else if mime.starts_with("audio/") {
        (256 * 1024 * 1024, std::time::Duration::from_secs(30))
    } else {
        (32 * 1024 * 1024, std::time::Duration::from_secs(10))
    }
}

#[cfg(target_os = "linux")]
fn request_dragged_content(
    source: &WlDataSource,
    mime: &str,
    capture: Arc<Mutex<ClientDragCapture>>,
    visual: DragLabel,
) {
    let (reader, writer) = match std::io::pipe() {
        Ok(pair) => pair,
        Err(error) => {
            tracing::debug!(target: "shell::selection", %error, "no pipe for dragged content");
            return;
        }
    };
    source.send(mime.to_string(), writer.as_fd());
    drop(writer);
    let suffix = drag_content_suffix(mime);
    let (max_content_bytes, wait_timeout) = drag_content_limit(mime);
    capture.lock_recover().wait_timeout = wait_timeout;
    capture.lock_recover().raw_transfer_complete = false;
    let completion = DragTransferCompletion {
        capture: capture.clone(),
        uri: false,
    };
    let mime = mime.to_string();
    let _ = std::thread::Builder::new()
        .name("tilo-drag-content".into())
        .spawn(move || {
            use std::io::{Read as _, Write as _};
            use std::os::unix::fs::PermissionsExt as _;

            let _completion = completion;
            // This transfer begins speculatively so a shell drop can survive
            // Wayland cancelling its source. Keep it modest: ordinary file
            // drags also offer a URI, and merely crossing the desktop must not
            // hydrate or copy a multi-gigabyte video.
            // The spool is session-owned capability material. Never fall back
            // to a predictable directory in world-writable /tmp: an attacker
            // could pre-create that path as a symlink before the compositor.
            let Some(base) = drag_import_directory() else {
                return;
            };
            match std::fs::symlink_metadata(&base) {
                Ok(metadata)
                    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() =>
                {
                    return;
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    if std::fs::create_dir(&base).is_err() {
                        return;
                    }
                }
                Err(_) => return,
            }
            if std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).is_err() {
                return;
            }
            let Ok(mut output) = tempfile::Builder::new()
                .prefix("dragged-content-")
                .suffix(suffix)
                .tempfile_in(&base)
            else {
                return;
            };
            let mut bounded = reader.take(max_content_bytes.saturating_add(1));
            let Ok(written) = std::io::copy(&mut bounded, output.as_file_mut()) else {
                return;
            };
            if written > max_content_bytes || output.as_file_mut().flush().is_err() {
                return;
            }
            let Ok((_file, path)) = output.keep() else {
                return;
            };
            {
                // The transfer already ran off-thread; enrich the visual from
                // the same bounded local copy rather than opening a second
                // Wayland pipe or doing decode work on the frame thread.
                if written <= 16 * 1024 * 1024
                    && let Ok(bytes) = std::fs::read(&path)
                {
                    let preview = if mime.starts_with("image/") {
                        crate::compositor::drag_feedback::image_preview(&bytes)
                    } else if mime.starts_with("text/") {
                        crate::compositor::drag_feedback::text_preview(&bytes)
                    } else {
                        None
                    };
                    if let Some(preview) = preview {
                        visual.set_preview(preview);
                    }
                }
                let mut state = capture.lock_recover();
                if state.disposition == 2 {
                    drop(state);
                    let _ = std::fs::remove_file(path);
                } else {
                    state.raw_path = Some(path);
                }
            }
        });
}

/// Give a compositor-owned Files drag the same visual treatment as a client
/// drag. Metadata and bytes may hydrate over the collaborative filesystem, so
/// both are resolved on a worker and the frame thread only sees the result.
#[cfg(target_os = "linux")]
pub fn request_path_drag_preview(paths: &[std::path::PathBuf], visual: DragLabel) {
    let fallback = if let [path] = paths {
        let mimes: Vec<String> = mime_guess::from_path(path)
            .first_raw()
            .map(str::to_string)
            .into_iter()
            .collect();
        crate::compositor::drag_feedback::preview_for_mimes(&mimes)
    } else {
        crate::compositor::drag_feedback::DragPreview::Kind(
            crate::compositor::drag_feedback::DragKind::File,
        )
    };
    visual.set_preview(fallback);
    let [path] = paths else { return };
    let path = path.clone();
    let _ = std::thread::Builder::new()
        .name("tilo-path-drag-preview".into())
        .spawn(move || {
            const MAX_PREVIEW_BYTES: u64 = 16 * 1024 * 1024;
            let mime = mime_guess::from_path(&path).first_raw().unwrap_or("");
            if !(mime.starts_with("image/") || mime.starts_with("text/")) {
                return;
            }
            let Ok(file) = File::open(path) else { return };
            let mut bytes = Vec::new();
            if file
                .take(MAX_PREVIEW_BYTES + 1)
                .read_to_end(&mut bytes)
                .is_err()
                || bytes.len() as u64 > MAX_PREVIEW_BYTES
            {
                return;
            }
            let preview = if mime.starts_with("image/") {
                crate::compositor::drag_feedback::image_preview(&bytes)
            } else {
                crate::compositor::drag_feedback::text_preview(&bytes)
            };
            if let Some(preview) = preview {
                visual.set_preview(preview);
            }
        });
}

#[cfg(target_os = "linux")]
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
        self.selection
            .dnd_label
            .set_preview(crate::compositor::drag_feedback::preview_for_mimes(&mimes));
        let capture = Arc::new(Mutex::new(ClientDragCapture::default()));
        self.selection.client_drag_capture = Some(capture.clone());
        self.selection.pending_shell_drop = None;
        // Then ask for the name from the URI list the source is already offering —
        // turns "Image" into "photo.png" on screen.
        if let Some(source) = source.as_ref()
            && mimes.iter().any(|m| m == URI_LIST_MIME)
        {
            request_dragged_name(source, self.selection.dnd_label.clone(), capture.clone());
        }
        // Also begin one bounded content transfer while the source is alive.
        // This is what lets text, an image, or video that has no host-visible
        // path materialise as a normal file-backed app after a shell drop.
        if let Some(source) = source.as_ref()
            && let Some(mime) = preferred_drag_content_mime(&mimes)
        {
            request_dragged_content(source, mime, capture, self.selection.dnd_label.clone());
        }
        // The drag icon is a client surface with the `dnd_icon` role, but Tilo
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
        if let Some(capture) = self.selection.client_drag_capture.take() {
            if target.is_none()
                && let Some(placement) = self.selection.shell_drop_placement.take()
            {
                capture.lock_recover().disposition = 1;
                let wait_timeout = capture.lock_recover().wait_timeout;
                self.selection.pending_shell_drop = Some(PendingShellDrop {
                    placement,
                    wait_timeout,
                    capture,
                    queued_at: std::time::Instant::now(),
                });
            } else {
                self.selection.shell_drop_placement = None;
                let raw = {
                    let mut state = capture.lock_recover();
                    state.disposition = 2;
                    state.raw_path.take()
                };
                if let Some(path) = raw {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
    }
}

/// Complete a client-to-shell drag once the asynchronous data pipe has
/// yielded a host path or a spooled content file. Returns whether it changed
/// desktop state. Called from the native frame loop, where the retained row
/// geometry is available; protocol callbacks intentionally do not own that UI.
#[cfg(target_os = "linux")]
pub fn pump_pending_shell_drop(
    state: &mut State,
    frame_builder: &mut crate::shell::frame::FrameBuilder,
) -> bool {
    let Some(pending) = state.selection.pending_shell_drop.as_ref() else {
        return false;
    };
    let (paths, unused_raw) = {
        let mut capture = pending.capture.lock_recover();
        let workspace_root = std::path::Path::new("/var/workspace").canonicalize().ok();
        let from_uri: Vec<_> = capture
            .paths
            .as_ref()
            .into_iter()
            .flatten()
            // Client URI lists are untrusted. A sandbox may nominate a path
            // directly only inside the collaborative filesystem; every other
            // payload has to arrive through the bounded data pipe above.
            .filter_map(|path| {
                let root = workspace_root.as_ref()?;
                let canonical = path.canonicalize().ok()?;
                canonical.starts_with(root).then_some(canonical)
            })
            .collect();
        if from_uri.is_empty() {
            (capture.raw_path.iter().cloned().collect(), None)
        } else {
            (from_uri, capture.raw_path.take())
        }
    };
    if let Some(path) = unused_raw {
        let _ = std::fs::remove_file(path);
    }
    if !paths.is_empty() {
        let Some(pending) = state.selection.pending_shell_drop.take() else {
            return false;
        };
        let mut changed = false;
        for (offset, path) in paths.iter().enumerate() {
            let placement = advance_drop_placement(pending.placement, offset);
            changed |= frame_builder
                .host()
                .open_dropped_path_with_placement(path, placement);
        }
        if !changed {
            let raw = pending.capture.lock_recover().raw_path.take();
            if let Some(path) = raw {
                let _ = std::fs::remove_file(path);
            }
        }
        return changed;
    }
    let transfers_complete = {
        let capture = pending.capture.lock_recover();
        capture.uri_transfer_complete && capture.raw_transfer_complete
    };
    if transfers_complete || pending.queued_at.elapsed() > pending.wait_timeout {
        let Some(pending) = state.selection.pending_shell_drop.take() else {
            return false;
        };
        let raw = {
            let mut capture = pending.capture.lock_recover();
            capture.disposition = 2;
            capture.raw_path.take()
        };
        if let Some(path) = raw {
            let _ = std::fs::remove_file(path);
        }
    }
    false
}

fn advance_drop_placement(
    placement: crate::shell::tab_binding::Placement,
    offset: usize,
) -> crate::shell::tab_binding::Placement {
    use crate::shell::tab_binding::Placement;
    match placement {
        Placement::TabAt(group, index) => Placement::TabAt(group, index.saturating_add(offset)),
        Placement::GroupAt(strip, index) => Placement::GroupAt(strip, index.saturating_add(offset)),
        other => other,
    }
}

/// Server-initiated drag-and-drop — source is compositor chrome, e.g. a file
/// dragged out of the hosted file manager onto a client window (started via
/// `client_input::pump_chrome_drag`, carried as [`ChromeDrag`]). A tab drag
/// between groups never comes here.
#[cfg(target_os = "linux")]
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

    #[test]
    fn drags_carry_decoded_local_paths_and_safe_payloads() {
        // Phase `client_drag_uri_paths_are_local_absolute_and_percent_decoded`.
        assert_eq!(
            paths_from_uri_list(
                b"# source names\r\nfile:///var/workspace/My%20Photo.png\r\n\
                  file://localhost/tmp/report%23final.pdf\r\nhttps://example.invalid/nope\r\n"
            ),
            vec![
                std::path::PathBuf::from("/var/workspace/My Photo.png"),
                std::path::PathBuf::from("/tmp/report#final.pdf"),
            ]
        );
        // Phase `client_drag_content_prefers_visual_or_text_payload_not_uri_or_secret`.
        {
            let mimes = vec![
                URI_LIST_MIME.to_string(),
                SECRET_MIME.to_string(),
                "image/png".to_string(),
                "text/plain".to_string(),
            ];
            assert_eq!(preferred_drag_content_mime(&mimes), Some("image/png"));
            assert_eq!(drag_content_suffix("video/webm"), ".webm");
            assert_eq!(drag_content_suffix("image/bmp"), ".bmp");
            assert_eq!(drag_content_suffix("image/avif"), ".avif");
            assert_eq!(drag_content_suffix("video/x-matroska"), ".mkv");
            assert_eq!(drag_content_suffix("video/quicktime"), ".mov");
            assert_eq!(drag_content_suffix("text/plain;charset=utf-8"), ".txt");
            assert_eq!(drag_content_limit("text/plain").0, 32 * 1024 * 1024);
            assert_eq!(drag_content_limit("audio/ogg").0, 256 * 1024 * 1024);
            assert_eq!(drag_content_limit("video/mp4").0, 512 * 1024 * 1024);
        }
        // Phase `multi_item_drop_advances_exact_insertion_side`.
        {
            use crate::shell::tab_binding::Placement;
            assert_eq!(
                advance_drop_placement(Placement::TabAt(7, 2), 3),
                Placement::TabAt(7, 5)
            );
            assert_eq!(
                advance_drop_placement(
                    Placement::GroupAt(crate::shell::state::StripId::from(8), 1),
                    2
                ),
                Placement::GroupAt(crate::shell::state::StripId::from(8), 3)
            );
        }
        // Phase `a_chrome_drag_offers_a_uri_list_and_the_single_files_bytes`.
        {
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
        // Phase `a_multi_file_drag_is_paths_only`.
        {
            let drag = ChromeDrag::new(vec![
                std::path::PathBuf::from("/a.png"),
                std::path::PathBuf::from("/b.png"),
            ]);
            assert_eq!(drag.mime_types, vec![URI_LIST_MIME.to_string()]);
            assert_eq!(drag.uri_list(), "file:///a.png\r\nfile:///b.png\r\n");
            assert_eq!(drag.file_for_mime("image/png"), None);
        }
        // Phase `unknown_extensions_travel_as_paths_only`.
        {
            let drag = ChromeDrag::new(vec![std::path::PathBuf::from("/thing.qcow2")]);
            assert_eq!(drag.mime_types, vec![URI_LIST_MIME.to_string()]);
            assert_eq!(
                mime_for_path(std::path::Path::new("x.JPG")),
                Some("image/jpeg")
            );
            assert_eq!(mime_for_path(std::path::Path::new("x")), None);
        }
    }

    #[test]
    fn selection_ownership_follows_the_newest_valid_writer() {
        // Phase `chrome_copy_is_readable_immediately_and_queues_a_publish`.
        // Chrome's clipboard write must be readable in-process immediately (paste
        // can't wait for a tick) and leave a publish flag for the real Wayland selection.
        {
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
        // Phase `client_copy_drops_compositor_ownership`.
        // A client taking the clipboard drops compositor ownership, so a later
        // `receive` is answered by that client and not out of a stale mirror.
        {
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
        // Phase `primary_and_clipboard_do_not_alias`.
        // Primary and clipboard are independent; middle-click must not return what Ctrl+C put down.
        {
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
        // Phase `clear_is_distinct_from_an_empty_selection`.
        // Clearing must distinguish "nothing was ever copied" from "an empty
        // string was copied" — a client may legitimately own an empty selection.
        {
            let s = store();
            set_mirror_text(&s, Target::Clipboard, "");
            assert_eq!(mirror_text(&s, Target::Clipboard).as_deref(), Some(""));
            clear_mirror(&s, Target::Clipboard);
            assert_eq!(mirror_text(&s, Target::Clipboard), None);
            assert!(!compositor_owns(&s, Target::Clipboard));
            assert!(s.lock().unwrap().clipboard.clear_pending);
        }
        // Phase `chrome_write_supersedes_stale_client_payloads`.
        {
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
        // Phase `overlarge_or_stale_client_payload_cannot_replace_current_selection`.
        {
            let s = store();
            let old =
                begin_client_selection(&s, Target::Clipboard, Some(vec!["image/jpeg".into()]));
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
    }

    #[test]
    fn selection_mime_offers_preserve_text_and_binary_payloads() {
        // Phase `text_mimes_lead_with_utf8`.
        // Offered mimes must lead with the specific UTF-8 type — the first match
        // wins, and a client stuck with `STRING` has to guess the encoding.
        {
            let mimes = text_mime_types();
            assert_eq!(mimes.first().map(String::as_str), Some(TEXT_MIME));
            assert!(mimes.iter().any(|m| m == "text/plain"));
            assert!(mimes.iter().any(|m| m == "UTF8_STRING"));
        }
        // Phase `binary_offer_round_trips_without_text_decoding_and_keeps_text_aliases`.
        {
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
        // Phase `chrome_image_copy_offers_png_and_preserves_text`.
        {
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
        // Phase `chrome_image_copy_rejects_invalid_dimensions_and_payload_size`.
        {
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
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn clipboard_history_keeps_distinct_safe_copies() {
        // Phase `clipboard_manager_tracks_distinct_copies_newest_first`.
        {
            let s = store();
            assert!(set_mirror_text(&s, Target::Clipboard, "first"));
            assert!(set_mirror_text(&s, Target::Clipboard, "second"));
            assert!(set_mirror_text(&s, Target::Clipboard, "first"));
            let history = clipboard_history(&s);
            assert_eq!(
                history.len(),
                2,
                "copying identical content de-duplicates it"
            );
            assert_eq!(history[0].text_preview.as_deref(), Some("first"));
            assert_eq!(history[1].text_preview.as_deref(), Some("second"));
        }
        // Phase `primary_and_secret_or_oversized_selections_never_enter_history`.
        {
            let s = store();
            assert!(set_mirror_text(&s, Target::Primary, "selection churn"));
            assert!(clipboard_history(&s).is_empty());

            begin_mirror_offer(&s, Target::Clipboard, &[SECRET_MIME.into()]).unwrap();
            set_mirror_bytes(&s, Target::Clipboard, SECRET_MIME, b"totp".to_vec()).unwrap();
            assert!(clipboard_history(&s).is_empty());

            set_mirror_bytes(
                &s,
                Target::Clipboard,
                "application/octet-stream",
                vec![0; MAX_CLIPBOARD_HISTORY_ITEM_BYTES + 1],
            )
            .unwrap();
            assert!(clipboard_history(&s).is_empty());
        }
        // Phase `clipboard_manager_can_pin_clear_restore_and_remove`.
        {
            let s = store();
            set_mirror_text(&s, Target::Clipboard, "keep");
            let keep = clipboard_history(&s)[0].id;
            assert!(pin_history_item(&s, keep, true));
            set_mirror_text(&s, Target::Clipboard, "discard");
            let discard = clipboard_history(&s)[0].id;

            clear_clipboard_history(&s);
            let history = clipboard_history(&s);
            assert_eq!(history.len(), 1);
            assert_eq!(history[0].id, keep);

            clear_mirror(&s, Target::Clipboard);
            assert!(activate_history_item(&s, keep));
            assert_eq!(mirror_text(&s, Target::Clipboard).as_deref(), Some("keep"));
            assert!(compositor_owns(&s, Target::Clipboard));
            assert!(remove_history_item(&s, keep));
            assert!(!remove_history_item(&s, discard));
        }
        // Phase `asynchronous_multi_mime_copy_becomes_one_history_item`.
        {
            let s = store();
            let generation = begin_client_selection(
                &s,
                Target::Clipboard,
                Some(vec![TEXT_MIME.into(), "image/png".into()]),
            );
            record_client_bytes(
                &s,
                Target::Clipboard,
                generation,
                TEXT_MIME,
                b"caption".to_vec(),
            );
            record_client_bytes(
                &s,
                Target::Clipboard,
                generation,
                "image/png",
                vec![1, 2, 3],
            );
            let history = clipboard_history(&s);
            assert_eq!(history.len(), 1);
            assert_eq!(history[0].text_preview.as_deref(), Some("caption"));
            assert!(history[0].mime_types.iter().any(|mime| mime == "image/png"));
        }
        // Phase `fabric_clipboard_is_history_only_and_is_not_reauthored`.
        {
            use crate::shell::device_fabric::{ClipboardEntry, ClipboardPayload, FabricStamp};
            use crate::shell::personal_continuity::DeviceId;

            let s = store();
            let origin = DeviceId::new("remote-device").unwrap();
            let entry = ClipboardEntry::shareable(
                ClipboardPayload::Text("from the other screen".into()),
                origin.clone(),
                42,
                FabricStamp {
                    counter: 7,
                    device: origin.clone(),
                },
            )
            .unwrap();
            merge_shared_clipboard_history(&s, std::slice::from_ref(&entry));

            assert_eq!(mirror_text(&s, Target::Clipboard), None);
            assert_eq!(
                clipboard_history(&s)[0].text_preview.as_deref(),
                Some("from the other screen")
            );
            assert_eq!(
                clipboard_history(&s)[0].source_device.as_ref(),
                Some(&origin)
            );
            assert_eq!(clipboard_history(&s)[0].created_unix_ms, 42);
            assert!(shareable_clipboard_history(&s).is_empty());

            assert!(activate_history_item(&s, entry.id));
            assert_eq!(
                mirror_text(&s, Target::Clipboard).as_deref(),
                Some("from the other screen")
            );
            assert!(shareable_clipboard_history(&s).is_empty());

            clear_shared_clipboard_history(&s);
            assert!(clipboard_history(&s).is_empty());
            assert_eq!(
                mirror_text(&s, Target::Clipboard).as_deref(),
                Some("from the other screen")
            );
        }
    }

    /// Mirror a complete client copy of `text` the way the pull does.
    fn client_copy(s: &SharedSelection, target: Target, mimes: &[&str], text: &str) -> u64 {
        let generation = begin_client_selection(
            s,
            target,
            Some(mimes.iter().map(|m| (*m).to_owned()).collect()),
        );
        for mime in mimes {
            record_client_bytes(s, target, generation, mime, text.as_bytes().to_vec());
        }
        generation
    }

    fn offered(s: &SharedSelection) -> Vec<String> {
        mirror_mime_types(s, Target::Clipboard)
    }

    #[test]
    fn a_client_copy_outlives_its_source_once_fully_mirrored() {
        let s = store();
        // Half-mirrored: the source is still the only holder of one format,
        // so Tilo must not take over and lose it.
        let generation = begin_client_selection(
            &s,
            Target::Clipboard,
            Some(vec!["image/png".into(), TEXT_MIME.into()]),
        );
        record_client_bytes(&s, Target::Clipboard, generation, TEXT_MIME, b"hi".to_vec());
        assert!(!adopt_complete_client_selection(&s, 1_000));
        assert!(!compositor_owns(&s, Target::Clipboard));
        record_client_bytes(&s, Target::Clipboard, generation, "image/png", vec![1, 2]);
        assert!(adopt_complete_client_selection(&s, 1_000));
        assert!(compositor_owns(&s, Target::Clipboard));
        assert!(s.lock().unwrap().clipboard.publish_pending);
        assert_eq!(mirror_text(&s, Target::Clipboard).as_deref(), Some("hi"));
        // Adopting is idempotent while the compositor holds it.
        assert!(!adopt_complete_client_selection(&s, 1_001));
        // Primary is never adopted.
        client_copy(&s, Target::Primary, &[TEXT_MIME], "sel");
        assert!(!adopt_complete_client_selection(&s, 9_000));
        assert!(!compositor_owns(&s, Target::Primary));
    }

    #[test]
    fn x11_meta_targets_neither_block_nor_ride_along_a_takeover() {
        let s = store();
        // What Xwayland's bridge offers for an X11 copy: content formats plus
        // meta targets that answer with nothing.
        let generation = begin_client_selection(
            &s,
            Target::Clipboard,
            Some(vec![
                "TARGETS".into(),
                "TIMESTAMP".into(),
                "UTF8_STRING".into(),
                TEXT_MIME.into(),
            ]),
        );
        record_client_bytes(&s, Target::Clipboard, generation, "TIMESTAMP", Vec::new());
        record_client_bytes(
            &s,
            Target::Clipboard,
            generation,
            TEXT_MIME,
            b"x11".to_vec(),
        );
        assert!(
            !adopt_complete_client_selection(&s, 999),
            "the offered text alias is still being pulled"
        );
        record_client_bytes(
            &s,
            Target::Clipboard,
            generation,
            "UTF8_STRING",
            b"x11".to_vec(),
        );
        assert!(adopt_complete_client_selection(&s, 1_000));
        assert!(
            !offered(&s)
                .iter()
                .any(|m| m == "TIMESTAMP" || m == "TARGETS")
        );
        assert_eq!(mirror_text(&s, Target::Clipboard).as_deref(), Some("x11"));
    }

    #[test]
    fn a_password_manager_copy_persists_but_leaves_no_history_and_is_wiped() {
        let s = store();
        let generation = begin_client_selection(
            &s,
            Target::Clipboard,
            Some(vec![TEXT_MIME.into(), PASSWORD_MANAGER_HINT_MIME.into()]),
        );
        record_client_bytes(
            &s,
            Target::Clipboard,
            generation,
            TEXT_MIME,
            b"hunter2".to_vec(),
        );
        record_client_bytes(
            &s,
            Target::Clipboard,
            generation,
            PASSWORD_MANAGER_HINT_MIME,
            b"secret".to_vec(),
        );
        assert!(
            adopt_complete_client_selection(&s, 1_000),
            "the paste must outlive the manager"
        );
        assert_eq!(
            mirror_text(&s, Target::Clipboard).as_deref(),
            Some("hunter2")
        );
        assert!(
            clipboard_history(&s).is_empty(),
            "a password-manager copy must not enter clipboard history"
        );
        // The next selection replaces and clears it.
        client_copy(&s, Target::Clipboard, &[TEXT_MIME], "next");
        assert_eq!(mirror_text(&s, Target::Clipboard).as_deref(), Some("next"));
    }

    #[test]
    fn a_secret_copy_from_chrome_pastes_as_text_but_never_enters_history_or_sync() {
        let s = store();
        let _host = ui::host::enter(&host_services(s.clone()));
        assert!(set_mirror_text(&s, Target::Clipboard, "ordinary copy"));
        assert_eq!(
            ui::try_clipboard_copy_secret("hunter2", std::time::Duration::from_secs(3600))
                .outcome(),
            Some(true)
        );
        assert_eq!(
            mirror_text(&s, Target::Clipboard).as_deref(),
            Some("hunter2"),
            "a plain-text paste still receives the secret"
        );
        for mime in [
            TEXT_MIME,
            "text/plain",
            SECRET_MIME,
            PASSWORD_MANAGER_HINT_MIME,
        ] {
            assert!(offered(&s).iter().any(|offered| offered == mime), "{mime}");
        }
        let history: Vec<_> = clipboard_history(&s)
            .into_iter()
            .map(|item| item.text_preview)
            .collect();
        assert_eq!(history, [Some("ordinary copy".to_owned())]);
        assert!(
            shareable_clipboard_history(&s)
                .iter()
                .all(|shared| shared.bytes != b"hunter2"),
            "the device fabric is never offered the secret"
        );
        assert!(set_mirror_text(&s, Target::Clipboard, "next"));
        assert!(
            !offered(&s).iter().any(|mime| mime == SECRET_MIME),
            "the next copy is not a secret"
        );
    }

    /// The owner announces its next selection on the tick its previous copy
    /// finished mirroring. The take-over must follow the newer owner, not
    /// install the older, complete copy over it (a text copy replacing the
    /// image copied right after it, or a cleared clipboard coming back).
    /// Linux only: it drives the Wayland compositor state.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_complete_copy_is_not_taken_over_once_its_owner_announced_another() {
        use smithay::reexports::calloop::EventLoop;
        use smithay::reexports::wayland_server::Display;
        use smithay::wayland::selection::SelectionTarget;

        let event_loop: EventLoop<'static, State> = EventLoop::try_new().expect("calloop");
        let display: Display<State> = Display::new().expect("display");
        let mut state = State::new(event_loop.handle(), display.handle());
        let s = state.selection_store();
        client_copy(&s, Target::Clipboard, &[TEXT_MIME], "older copy");

        let seat = state.seat.clone();
        SelectionHandler::new_selection(&mut state, SelectionTarget::Clipboard, None, seat);
        state.pump_selection();

        assert!(
            smithay::wayland::selection::data_device::current_data_device_selection_userdata(
                &state.seat
            )
            .is_none(),
            "the seat serves the older copy after its owner replaced it"
        );
        assert!(!compositor_owns(&s, Target::Clipboard));
        assert_eq!(mirror_text(&s, Target::Clipboard), None);
    }

    #[test]
    fn an_aborted_transfer_is_never_adopted() {
        let s = store();
        client_copy(&s, Target::Clipboard, &[TEXT_MIME], "");
        assert!(!adopt_complete_client_selection(&s, 1_000));
        assert!(!compositor_owns(&s, Target::Clipboard));
    }

    #[test]
    fn ownership_changes_never_resurrect_stale_content_or_mimes() {
        let s = store();
        // Wayland client A, then an X11-bridge owner B (the bridge is just
        // another client with its own offer), then chrome, then A again.
        let a = client_copy(&s, Target::Clipboard, &[TEXT_MIME], "from A");
        assert!(adopt_complete_client_selection(&s, 10_000));

        let b = begin_client_selection(
            &s,
            Target::Clipboard,
            Some(vec!["UTF8_STRING".into(), "image/png".into()]),
        );
        // The new owner starts empty: nothing of A leaks, and the mime list
        // is B's own (UTF8_STRING pulls in the text aliases, nothing of A).
        assert!(!compositor_owns(&s, Target::Clipboard));
        assert_eq!(mirror_text(&s, Target::Clipboard), None);
        assert!(offered(&s).contains(&"image/png".to_owned()));
        assert!(!adopt_complete_client_selection(&s, 10_001));
        // A late transfer for A's generation is rejected.
        record_client_bytes(&s, Target::Clipboard, a, TEXT_MIME, b"from A".to_vec());
        assert_eq!(mirror_text(&s, Target::Clipboard), None);
        record_client_bytes(&s, Target::Clipboard, b, "UTF8_STRING", b"from B".to_vec());
        record_client_bytes(&s, Target::Clipboard, b, "image/png", vec![9]);
        assert!(adopt_complete_client_selection(&s, 10_002));
        assert_eq!(
            mirror_text(&s, Target::Clipboard).as_deref(),
            Some("from B")
        );
        let mut mimes = offered(&s);
        mimes.sort();
        assert_eq!(mimes.iter().filter(|m| *m == "text/plain").count(), 1);

        // Chrome copies; B's image must not stay advertised.
        set_mirror_text(&s, Target::Clipboard, "from chrome");
        assert_eq!(offered(&s), normalized_mimes([TEXT_MIME.to_owned()]));
        assert_eq!(
            mirror_text(&s, Target::Clipboard).as_deref(),
            Some("from chrome")
        );

        // A late byte from B cannot overwrite chrome's copy.
        record_client_bytes(&s, Target::Clipboard, b, "UTF8_STRING", b"from B".to_vec());
        assert_eq!(
            mirror_text(&s, Target::Clipboard).as_deref(),
            Some("from chrome")
        );

        // Clearing leaves nothing adoptable to resurrect.
        clear_mirror(&s, Target::Clipboard);
        assert!(!adopt_complete_client_selection(&s, 20_000));
        assert!(offered(&s).is_empty());
        assert_eq!(mirror_text(&s, Target::Clipboard), None);
    }

    #[test]
    fn a_manager_re_copying_the_selection_does_not_ping_pong() {
        let s = store();
        client_copy(&s, Target::Clipboard, &[TEXT_MIME], "same");
        assert!(adopt_complete_client_selection(&s, 5_000));
        // The manager re-asserts identical content as a new client owner.
        client_copy(&s, Target::Clipboard, &[TEXT_MIME], "same");
        assert!(!adopt_complete_client_selection(&s, 5_100));
        // Later, or different content, is a real copy again.
        assert!(adopt_complete_client_selection(
            &s,
            5_000 + ADOPT_COOLDOWN_MS
        ));
        client_copy(&s, Target::Clipboard, &[TEXT_MIME], "different");
        assert!(adopt_complete_client_selection(&s, 5_200));
    }
}

// Refused selections.
//
// smithay answers a `set_selection` from a client without keyboard focus by
// silently dropping it (a `debug!` inside smithay), which made "copy does
// nothing" invisible in the session log. The device dispatch is wrapped here:
// the request is checked against the same rule, logged with its reason, and
// then handed to smithay unchanged.

/// Why the compositor refused or dropped a client's selection.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionRefusal {
    /// The client's device belongs to a client that does not hold the seat's keyboard focus.
    ClientLacksKeyboardFocus,
    /// The seat has no keyboard at all.
    NoKeyboard,
}

#[cfg(target_os = "linux")]
impl SelectionRefusal {
    fn reason(self) -> &'static str {
        match self {
            Self::ClientLacksKeyboardFocus => "client does not hold keyboard focus",
            Self::NoKeyboard => "seat has no keyboard",
        }
    }
}

#[cfg(target_os = "linux")]
impl State {
    /// The refusal smithay will apply to a `set_selection` made on `device`, if any.
    fn selection_refusal(
        &self,
        device: &smithay::reexports::wayland_server::backend::ObjectId,
    ) -> Option<SelectionRefusal> {
        match self.seat.get_keyboard() {
            None => Some(SelectionRefusal::NoKeyboard),
            Some(keyboard) if keyboard.client_of_object_has_focus(device) => None,
            Some(_) => Some(SelectionRefusal::ClientLacksKeyboardFocus),
        }
    }

    fn log_selection_refusal(
        &self,
        protocol: &'static str,
        client: &Client,
        refusal: SelectionRefusal,
        clears: bool,
    ) {
        let pid = client.get_data::<ClientState>().and_then(|state| state.pid);
        tracing::info!(
            target: "shell::selection",
            protocol,
            reason = refusal.reason(),
            clears_selection = clears,
            client_pid = ?pid,
            client_app_ids = ?self.client_app_ids(client),
            keyboard_focus = %self.keyboard_focus_holder(),
            "selection refused"
        );
    }
}

#[cfg(target_os = "linux")]
mod logged_dispatch {
    use smithay::reexports::wayland_protocols::wp::primary_selection::zv1::server::{
        zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1,
        zwp_primary_selection_device_v1::{self, ZwpPrimarySelectionDeviceV1},
        zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1,
    };
    use smithay::reexports::wayland_server::protocol::wl_data_device::{self, WlDataDevice};
    use smithay::reexports::wayland_server::protocol::wl_data_device_manager::WlDataDeviceManager;
    use smithay::reexports::wayland_server::protocol::wl_data_source::WlDataSource;
    use smithay::reexports::wayland_server::{
        Client, DataInit, Dispatch, DisplayHandle, Resource, delegate_dispatch,
        delegate_global_dispatch,
    };
    use smithay::wayland::selection::data_device::{
        DataDeviceState, DataDeviceUserData, DataSourceUserData,
    };
    use smithay::wayland::selection::primary_selection::{
        PrimaryDeviceManagerGlobalData, PrimaryDeviceUserData, PrimarySelectionState,
        PrimarySourceUserData,
    };

    use crate::compositor::state::State;

    delegate_global_dispatch!(State: [WlDataDeviceManager: ()] => DataDeviceState);
    delegate_dispatch!(State: [WlDataDeviceManager: ()] => DataDeviceState);
    delegate_dispatch!(State: [WlDataSource: DataSourceUserData] => DataDeviceState);
    delegate_global_dispatch!(State: [ZwpPrimarySelectionDeviceManagerV1: PrimaryDeviceManagerGlobalData] => PrimarySelectionState);
    delegate_dispatch!(State: [ZwpPrimarySelectionDeviceManagerV1: ()] => PrimarySelectionState);
    delegate_dispatch!(State: [ZwpPrimarySelectionSourceV1: PrimarySourceUserData] => PrimarySelectionState);

    impl Dispatch<WlDataDevice, DataDeviceUserData> for State {
        fn request(
            state: &mut Self,
            client: &Client,
            resource: &WlDataDevice,
            request: wl_data_device::Request,
            data: &DataDeviceUserData,
            dh: &DisplayHandle,
            data_init: &mut DataInit<'_, Self>,
        ) {
            if let wl_data_device::Request::SetSelection { source, .. } = &request
                && let Some(refusal) = state.selection_refusal(&resource.id())
            {
                state.log_selection_refusal("wl_data_device", client, refusal, source.is_none());
            }
            <DataDeviceState as Dispatch<WlDataDevice, DataDeviceUserData, Self>>::request(
                state, client, resource, request, data, dh, data_init,
            );
        }
    }

    impl Dispatch<ZwpPrimarySelectionDeviceV1, PrimaryDeviceUserData> for State {
        fn request(
            state: &mut Self,
            client: &Client,
            resource: &ZwpPrimarySelectionDeviceV1,
            request: zwp_primary_selection_device_v1::Request,
            data: &PrimaryDeviceUserData,
            dh: &DisplayHandle,
            data_init: &mut DataInit<'_, Self>,
        ) {
            if let zwp_primary_selection_device_v1::Request::SetSelection { source, .. } = &request
                && let Some(refusal) = state.selection_refusal(&resource.id())
            {
                state.log_selection_refusal(
                    "zwp_primary_selection_device_v1",
                    client,
                    refusal,
                    source.is_none(),
                );
            }
            <PrimarySelectionState as Dispatch<
                ZwpPrimarySelectionDeviceV1,
                PrimaryDeviceUserData,
                Self,
            >>::request(state, client, resource, request, data, dh, data_init);
        }
    }
}
