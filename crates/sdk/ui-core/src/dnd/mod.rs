#[derive(Clone, Debug)]
pub enum DragPayload {
    Tab { group: u64, tab: u64 },
    File { path: String },
    Text { value: String },
    Custom { kind: String, blob: Vec<u8> },
}

/// Threshold in logical pixels after which a pointer press-and-move is promoted
/// to a drag gesture. Matches macOS / GTK conventions.
pub const DRAG_START_THRESHOLD_PX: f64 = 4.0;

// Dragging out of the app: a drag that leaves the window, for whatever
// application the pointer is over when the button comes up (everything above
// is a drag *inside* one process). A toolkit capability rather than an app's,
// like the clipboard, because it needs the process's own Wayland connection.
// `App` publishes its window here at startup so any widget can drag.
//
// A receiving Tilo app is confined to its own mount namespace and `HOME`, so
// handing it a path (`/home/you/photo.png`) would not resolve there — the
// same reason the file-chooser portal exists rather than one that returns
// paths. So a drag carries **content**: the compositor brokers a pipe, this
// side writes bytes into it, and the receiver spools them into its own
// runtime dir. No path crosses, no capability is granted, and the sandbox is
// not widened by a drop. A `text/uri-list` is offered alongside for a
// receiver that wants a name, but never as the only offer.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, Weak};

use tilo_lock::MutexRecover as _;
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
    *DRAG_WINDOW.lock_recover() = Some(std::sync::Arc::downgrade(window));
}

/// The mime type a list of `file://` URIs travels as.
pub const URI_LIST_MIME: &str = "text/uri-list";

/// The `text/uri-list` body for `paths`: one `file://` URI per line, CRLF
/// terminated as RFC 2483 wants, with the bytes a URI cannot carry
/// verbatim percent-encoded. A path that is not absolute has no file URI and
/// is left out.
pub fn uri_list(paths: &[PathBuf]) -> String {
    let mut out = String::new();
    for uri in paths.iter().filter_map(|p| file_uri(p)) {
        out.push_str(uri.as_str());
        out.push_str("\r\n");
    }
    out
}

/// A path's `file://` URI. `url` has none on a target without a file system
/// (`wasm32-unknown-unknown`), where no path is a local file.
fn file_uri(path: &Path) -> Option<url::Url> {
    #[cfg(any(unix, windows, target_os = "wasi"))]
    {
        url::Url::from_file_path(path).ok()
    }
    #[cfg(not(any(unix, windows, target_os = "wasi")))]
    {
        let _ = path;
        None
    }
}

/// The mime type to offer a file's bytes under, from its extension.
///
/// `None` when the extension says nothing — the drag then carries only a
/// URI list, rather than claiming
/// `application/octet-stream` content a receiver would have to sniff.
pub fn mime_for_path(path: &Path) -> Option<&'static str> {
    let extension = path.extension().and_then(|extension| extension.to_str());
    TILO_TYPES
        .iter()
        .find(|(known, _)| extension.is_some_and(|extension| extension.eq_ignore_ascii_case(known)))
        .map(|(_, media_type)| *media_type)
        .or_else(|| tilo_mime::from_path(path))
}

/// The file types Tilo's own apps write, which no general table knows:
/// their extension and media type.
const TILO_TYPES: &[(&str, &str)] = &[("artwork", "application/vnd.tilo.artwork+zip")];

/// Start a drag carrying `paths`, out of this app and into whatever
/// application the pointer is released over. Call from a press-and-move
/// gesture once the pointer has travelled [`DRAG_START_THRESHOLD_PX`] while
/// the button is still held, so the compositor can validate the drag against
/// that implicit grab.
///
/// A single file is offered as both its own content type and a URI list;
/// several are offered as a URI list alone, since one pipe can't carry
/// several files' bytes. Returns whether a drag started.
pub fn start_file_drag(paths: &[PathBuf]) -> bool {
    if paths.is_empty() {
        return false;
    }
    let mut offers: Vec<(String, Vec<u8>)> = Vec::new();
    if let [single] = paths
        && let Some(mime) = mime_for_path(single)
    {
        match std::fs::read(single) {
            Ok(bytes) => offers.push((mime.to_string(), bytes)),
            Err(e) => tracing::warn!(
                target: "tilo_ui::dnd",
                path = %single.display(),
                error = %e,
                "could not read the dragged file; offering only its name"
            ),
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
        let slot = DRAG_WINDOW.lock_recover();
        slot.as_ref().and_then(Weak::upgrade)
    };
    let Some(window) = window else {
        // Not an error: the compositor hosts several of these widgets in its
        // own process with no `App` and no window; reporting "no" is how it
        // learns to promote the drag itself.
        tracing::debug!(target: "tilo_ui::dnd", "no window publishes a drag in this process");
        return false;
    };
    #[cfg(target_os = "linux")]
    {
        use winit::platform::wayland::WindowExtWayland;
        let started = window.start_drag(offers);
        if !started {
            tracing::warn!(
                target: "tilo_ui::dnd",
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

    #[test]
    fn outgoing_drags_encode_uri_lists_and_content_types() {
        // Case `a_uri_list_escapes_what_a_uri_cannot_carry`:
        // A dragged name has to survive the trip through a URI, which is the
        // case a naive `format!("file://{}")` gets wrong.
        {
            assert_eq!(
                uri_list(&[PathBuf::from("/home/me/my photo#1.png")]),
                "file:///home/me/my%20photo%231.png\r\n"
            );
        }

        // Case `several_paths_are_one_entry_per_line`.
        {
            assert_eq!(
                uri_list(&[PathBuf::from("/a.png"), PathBuf::from("/b.png")]),
                "file:///a.png\r\nfile:///b.png\r\n"
            );
        }

        // Case `a_known_extension_names_a_content_type_and_an_unknown_one_does_not`:
        // The content type is what a confined receiver actually uses, so an
        // unknown extension must not claim one.
        {
            assert_eq!(mime_for_path(Path::new("/x/photo.PNG")), Some("image/png"));
            assert_eq!(mime_for_path(Path::new("/x/scan.jpeg")), Some("image/jpeg"));
            assert_eq!(mime_for_path(Path::new("/x/archive.tar.zst")), None);
            assert_eq!(mime_for_path(Path::new("/x/noext")), None);
        }

        // Case `an_empty_selection_starts_nothing`:
        // Nothing to drag is not a drag.
        {
            assert!(!start_file_drag(&[]));
        }
    }
}
