use vello::kurbo::Point;

#[derive(Clone, Debug)]
pub enum DragPayload {
    Tab { group: u64, tab: u64 },
    File { path: String },
    Text { value: String },
    Custom { kind: String, blob: Vec<u8> },
}

#[derive(Clone, Debug)]
pub struct DragState {
    pub payload: DragPayload,
    pub origin: Point,
    pub current: Point,
}

#[derive(Default)]
pub struct DragDropManager {
    pub active: Option<DragState>,
}

impl DragDropManager {
    pub fn new() -> Self { Self::default() }
    pub fn start(&mut self, payload: DragPayload, origin: Point) {
        self.active = Some(DragState { payload, origin, current: origin });
    }
    pub fn update_position(&mut self, p: Point) {
        if let Some(s) = &mut self.active { s.current = p; }
    }
    pub fn end(&mut self) -> Option<DragState> { self.active.take() }
    pub fn is_active(&self) -> bool { self.active.is_some() }
}

impl DragDropManager {
    pub fn distance_moved(&self) -> f64 {
        if let Some(s) = &self.active {
            let dx = s.current.x - s.origin.x;
            let dy = s.current.y - s.origin.y;
            (dx * dx + dy * dy).sqrt()
        } else { 0.0 }
    }
}

/// Threshold in logical pixels after which a pointer press-and-move is promoted
/// to a drag gesture. Matches macOS / GTK conventions.
pub const DRAG_START_THRESHOLD_PX: f64 = 4.0;

// ---------------------------------------------------------------------------
// Dragging out of the app
// ---------------------------------------------------------------------------
//
// Everything above this line is a drag *inside* one process — a tab moved
// between groups, a row reordered. This is the other kind: a drag that
// leaves the window and is delivered to whatever application the pointer
// is over when the button comes up.
//
// It is a toolkit capability rather than an app's, for the same reason
// the clipboard is: it needs the process's own Wayland connection, which
// only the toolkit has. `App` publishes its window here at startup and
// any widget can then start a drag without holding one.
//
// ## What crosses, and why it is bytes
//
// A receiving qOS app is confined: its own mount namespace, its own
// `HOME`, and no ambient authority over the sending app's files. Handing
// it `/home/you/photo.png` would hand it a path that does not resolve
// there — which is the same reason the file-chooser portal exists rather
// than a portal that returns paths.
//
// So a drag carries **content**. The compositor brokers a pipe between
// the two clients; this side writes the bytes into it and the receiving
// toolkit spools them into a file inside the receiver's own runtime dir.
// No path from this namespace is ever resolved in that one, no capability
// is granted, and the sandbox is not widened by a drop: the receiver ends
// up with a copy of exactly the bytes a person dragged onto it, and
// nothing else.
//
// A `text/uri-list` is offered alongside so a receiver that wants a
// *name* (a file manager showing what it is about to copy) can have one,
// but it is never the only offer, and a confined receiver is expected to
// prefer the bytes.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, Weak};

use winit::window::Window;

/// The window drags are started from. A `Weak` so the toolkit does not
/// keep the window alive past the event loop that owns it.
static DRAG_WINDOW: Mutex<Option<Weak<Window>>> = Mutex::new(None);

/// Publish the window a drag out of this app is issued on.
///
/// Called by `App` once its window exists. Until then, and on a platform
/// with no drag support, [`start_file_drag`] reports that it started
/// nothing rather than pretending.
pub fn set_drag_window(window: &std::sync::Arc<Window>) {
    *DRAG_WINDOW.lock().unwrap_or_else(|e| e.into_inner()) =
        Some(std::sync::Arc::downgrade(window));
}

/// The mime type a list of `file://` URIs travels as.
pub const URI_LIST_MIME: &str = "text/uri-list";

/// The `text/uri-list` body for `paths`: one `file://` URI per line, CRLF
/// terminated as RFC 2483 wants, with the bytes a URI cannot carry
/// verbatim percent-encoded.
pub fn uri_list(paths: &[PathBuf]) -> String {
    let mut out = String::new();
    for p in paths {
        out.push_str("file://");
        out.push_str(&percent_encode_path(&p.to_string_lossy()));
        out.push_str("\r\n");
    }
    out
}

/// Percent-encode the characters a `file://` URI may not carry literally.
/// Deliberately conservative: everything outside the unreserved set plus
/// `/` is escaped, so a space, a `#` or a newline in a file name survives.
fn percent_encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.as_bytes() {
        let c = *b;
        let unreserved = c.is_ascii_alphanumeric()
            || matches!(c, b'-' | b'_' | b'.' | b'~' | b'/');
        if unreserved {
            out.push(c as char);
        } else {
            out.push_str(&format!("%{c:02X}"));
        }
    }
    out
}

/// The mime type to offer a file's bytes under, from its extension.
///
/// `None` when the extension says nothing — the drag then carries only a
/// URI list, which is the honest answer rather than claiming
/// `application/octet-stream` content a receiver would have to sniff.
pub fn mime_for_path(path: &Path) -> Option<&'static str> {
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

/// Start a drag carrying `paths`, out of this app and into whatever
/// application the pointer is released over.
///
/// Call it from a press-and-move gesture once the pointer has travelled
/// [`DRAG_START_THRESHOLD_PX`], while the button is still held: the
/// compositor validates the drag against that implicit grab.
///
/// A single file is offered as both its own content type and a URI list;
/// several are offered as a URI list alone, because one pipe cannot carry
/// several files' bytes. Reading happens here, once, at the start of the
/// gesture — the app has the authority to read what it is showing, and
/// the receiver never needs any.
///
/// Returns whether a drag was actually started.
pub fn start_file_drag(paths: &[PathBuf]) -> bool {
    if paths.is_empty() {
        return false;
    }
    let mut offers: Vec<(String, Vec<u8>)> = Vec::new();
    if let [single] = paths {
        if let Some(mime) = mime_for_path(single) {
            match std::fs::read(single) {
                Ok(bytes) => offers.push((mime.to_string(), bytes)),
                Err(e) => tracing::warn!(
                    target: "ui::dnd",
                    path = %single.display(),
                    error = %e,
                    "could not read the dragged file; offering only its name"
                ),
            }
        }
    }
    offers.push((URI_LIST_MIME.to_string(), uri_list(paths).into_bytes()));
    start_drag_offering(offers)
}

/// Start a drag carrying arbitrary `offers` (mime type → bytes).
///
/// The general form behind [`start_file_drag`], for a widget dragging out
/// something that is not a file on disk.
pub fn start_drag_offering(offers: Vec<(String, Vec<u8>)>) -> bool {
    let window = {
        let slot = DRAG_WINDOW.lock().unwrap_or_else(|e| e.into_inner());
        slot.as_ref().and_then(Weak::upgrade)
    };
    let Some(window) = window else {
        // Not an error. The compositor hosts several of these widgets in
        // its own process, where there is no `App` and no window to drag
        // from — a drag out of one of those is the host's to promote, and
        // this reporting "no" is how it learns to.
        tracing::debug!(target: "ui::dnd", "no window publishes a drag in this process");
        return false;
    };
    #[cfg(target_os = "linux")]
    {
        use winit::platform::wayland::WindowExtWayland;
        let started = window.start_drag(offers);
        if !started {
            tracing::warn!(
                target: "ui::dnd",
                "the compositor did not take the drag — no pointer grab, or no \
                 wl_data_device_manager"
            );
        }
        started
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (window, offers);
        false
    }
}

#[cfg(test)]
mod outgoing_tests {
    use super::*;

    /// A dragged name has to survive the trip through a URI, which is the
    /// case a naive `format!("file://{}")` gets wrong.
    #[test]
    fn a_uri_list_escapes_what_a_uri_cannot_carry() {
        assert_eq!(
            uri_list(&[PathBuf::from("/home/me/my photo#1.png")]),
            "file:///home/me/my%20photo%231.png\r\n"
        );
    }

    #[test]
    fn several_paths_are_one_entry_per_line() {
        assert_eq!(
            uri_list(&[PathBuf::from("/a.png"), PathBuf::from("/b.png")]),
            "file:///a.png\r\nfile:///b.png\r\n"
        );
    }

    /// The content type is what a confined receiver actually uses, so an
    /// unknown extension must not claim one.
    #[test]
    fn a_known_extension_names_a_content_type_and_an_unknown_one_does_not() {
        assert_eq!(mime_for_path(Path::new("/x/photo.PNG")), Some("image/png"));
        assert_eq!(mime_for_path(Path::new("/x/scan.jpeg")), Some("image/jpeg"));
        assert_eq!(mime_for_path(Path::new("/x/archive.tar.zst")), None);
        assert_eq!(mime_for_path(Path::new("/x/noext")), None);
    }

    /// Nothing to drag is not a drag.
    #[test]
    fn an_empty_selection_starts_nothing() {
        assert!(!start_file_drag(&[]));
    }
}
