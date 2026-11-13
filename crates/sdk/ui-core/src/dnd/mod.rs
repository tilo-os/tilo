#[derive(Clone, Debug)]
pub enum DragPayload {
    Tab { group: u64, tab: u64 },
    File { path: String },
    Text { value: String },
    Custom { kind: String, blob: Vec<u8> },
}

/// Threshold in logical pixels after which a pointer press-and-move is promoted to a drag gesture.
pub const DRAG_START_THRESHOLD_PX: f64 = 4.0;

// Dragging out of the app: a drag that leaves the window, for whatever
// application the pointer is over when the button comes up (everything above
// is a drag *inside* one process). A toolkit capability rather than an app's,
// like the clipboard, because it needs the process Wayland connection.
// `App` publishes its window here at startup so any widget can drag.
//
// A receiving Tilo app is confined to its mount namespace and `HOME`, so
// handing it a path (`/home/you/photo.png`) would not resolve there — the
// same reason the file-chooser portal exists rather than one that returns
// paths. So a drag has **content**: the compositor brokers a pipe, this
// side writes bytes into it, and the receiver spools them into its
// runtime dir. No path crosses, no capability is granted, and the sandbox is
// not widened by a drop. A `text/uri-list` is offered alongside for a
// receiver that wants a name, but never as the only offer.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, Weak};

use tilo_lock::MutexRecover as _;
use winit::window::Window;

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
    for uri in paths
        .iter()
        .filter(|path| path.is_absolute())
        .filter_map(|path| media::source::path_to_file_uri(path).ok())
    {
        out.push_str(&uri);
        out.push_str("\r\n");
    }
    out
}

/// The [`COPIED_FILES_MIME`] body for `paths`: a first line saying whether the copy is a cut or a copy,
/// then the [`uri_list`], as a file manager writes it.
#[must_use]
pub fn copied_files_list(paths: &[PathBuf], cut: bool) -> String {
    let mut body = String::from(if cut { "cut" } else { "copy" });
    body.push_str("\r\n");
    body.push_str(&uri_list(paths));
    body
}

/// The mime type a file manager's copy of files travels as when it also says whether the copy is a cut,
/// first line `copy` or `cut` and the URI list after it.
pub const COPIED_FILES_MIME: &str = "x-special/gnome-copied-files";

/// Whether `mime` is one of the formats a copy of files on the clipboard travels as.
#[must_use]
pub fn is_file_mime(mime: &str) -> bool {
    mime == URI_LIST_MIME || mime == COPIED_FILES_MIME
}

/// The files a copy names: the paths of a [`URI_LIST_MIME`] body, or of a [`COPIED_FILES_MIME`] one
/// with the cut it started with.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CopiedFiles {
    pub paths: Vec<PathBuf>,
    /// The copy was a cut: the sources go away once they are pasted.
    pub cut: bool,
}

/// Read back the files [`uri_list`] wrote, in `mime`.
///
/// Lines that are not file URIs are skipped rather than failing the read: a
/// list may carry comments, and one bad line must not lose the rest of a
/// paste.
#[must_use]
pub fn copied_files(mime: &str, body: &str) -> CopiedFiles {
    let (body, cut) = if mime == COPIED_FILES_MIME {
        match body.split_once('\n') {
            Some((first, rest)) => (rest, first.trim().eq_ignore_ascii_case("cut")),
            None => ("", false),
        }
    } else {
        (body, false)
    };
    CopiedFiles {
        paths: body
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .filter_map(|line| media::source::file_uri_to_path(line).ok())
            .collect(),
        cut,
    }
}

/// The mime type to offer a file's bytes under, from its extension.
pub fn mime_for_path(path: &Path) -> Option<&'static str> {
    let extension = path.extension().and_then(|extension| extension.to_str());
    TILO_TYPES
        .iter()
        .find(|(known, _)| extension.is_some_and(|extension| extension.eq_ignore_ascii_case(known)))
        .map(|(_, media_type)| *media_type)
        .or_else(|| tilo_mime::from_path(path))
}

const TILO_TYPES: &[(&str, &str)] = &[
    ("artwork", "application/vnd.tilo.artwork+zip"),
    (
        tilo_content::RECORDING.extension,
        tilo_content::RECORDING.mime,
    ),
];

/// Start a drag carrying `paths`, out of this app and into whatever
/// application the pointer is released over. Call from a press-and-move
/// gesture once the pointer has travelled [`DRAG_START_THRESHOLD_PX`] while
/// the button is still held, so the compositor can validate the drag against
/// that implicit grab.
///
/// A single file is offered as both its content type and a URI list;
/// several are offered as a URI list alone, since one pipe can't carry
/// several files' bytes. Returns whether a drag started.
///
/// The file is not read here: its bytes are streamed into the pipe when a
/// receiver asks for them, on the toolkit's pipe-writing thread. Starting a
/// drag runs on the app's event thread, inside a pointer motion, and a file
/// can be gigabytes on a slow or remote disk.
pub fn start_file_drag(paths: &[PathBuf]) -> bool {
    let offers = file_offers(paths);
    if offers.is_empty() {
        return false;
    }
    start_drag_with(offers)
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Offer {
    Bytes(Vec<u8>),
    File(PathBuf),
}

fn file_offers(paths: &[PathBuf]) -> Vec<(String, Offer)> {
    if paths.is_empty() {
        return Vec::new();
    }
    let mut offers = Vec::new();
    if let [single] = paths
        && let Some(mime) = mime_for_path(single)
    {
        offers.push((mime.to_string(), Offer::File(single.clone())));
    }
    offers.push((
        URI_LIST_MIME.to_string(),
        Offer::Bytes(uri_list(paths).into_bytes()),
    ));
    offers
}

/// Start a drag carrying arbitrary `offers` (mime type → bytes).
pub fn start_drag_offering(offers: Vec<(String, Vec<u8>)>) -> bool {
    start_drag_with(
        offers
            .into_iter()
            .map(|(mime, bytes)| (mime, Offer::Bytes(bytes)))
            .collect(),
    )
}

static HOSTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

static HOSTED_DRAG: Mutex<Option<Vec<(String, Vec<u8>)>>> = Mutex::new(None);

/// The compositor hosts this process's apps itself (no window): a drag an app starts is kept for the
/// host to carry ([`take_hosted_drag`]).
pub fn host_drags_in_process() {
    HOSTED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// The drag a hosted app asked for since the last call: bytes per format.
pub fn take_hosted_drag() -> Option<Vec<(String, Vec<u8>)>> {
    HOSTED_DRAG.lock_recover().take()
}

fn start_drag_with(offers: Vec<(String, Offer)>) -> bool {
    let window = {
        let slot = DRAG_WINDOW.lock_recover();
        slot.as_ref().and_then(Weak::upgrade)
    };
    if window.is_none() && HOSTED.load(std::sync::atomic::Ordering::Relaxed) {
        let bytes: Vec<(String, Vec<u8>)> = offers
            .into_iter()
            .filter_map(|(mime, offer)| match offer {
                Offer::Bytes(bytes) => Some((mime, bytes)),
                Offer::File(_) => None,
            })
            .collect();
        if bytes.is_empty() {
            return false;
        }
        *HOSTED_DRAG.lock_recover() = Some(bytes);
        return true;
    }
    let Some(window) = window else {
        tracing::debug!(target: "tilo_ui::dnd", "no window publishes a drag in this process");
        return false;
    };
    #[cfg(target_os = "linux")]
    {
        use winit::platform::wayland::{DragData, WindowExtWayland};
        let offers = offers
            .into_iter()
            .map(|(mime, offer)| {
                let data = match offer {
                    Offer::Bytes(bytes) => DragData::Bytes(bytes),
                    Offer::File(path) => DragData::File(path),
                };
                (mime, data)
            })
            .collect();
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
    fn a_copied_file_list_reads_back_as_the_files_it_names() {
        // Case `a_uri_list_reads_back_as_the_paths_it_was_written_from`:
        // what `uri_list` escapes has to come back as the same path, or a
        // file with a space in its name is pasted under a broken name.
        {
            let paths = [PathBuf::from("/home/me/my photo#1.png")];
            let body = uri_list(&paths);
            assert_eq!(
                copied_files(URI_LIST_MIME, &body),
                CopiedFiles {
                    paths: paths.to_vec(),
                    cut: false
                }
            );
            assert!(!copied_files(URI_LIST_MIME, &body).cut, "a uri list copies");
            let copied = copied_files_list(&paths, false);
            assert!(copied.starts_with("copy\r\n"), "a copy says so: {copied}");
            assert_eq!(
                copied_files(COPIED_FILES_MIME, &copied),
                CopiedFiles {
                    paths: paths.to_vec(),
                    cut: false
                },
                "a copy reads back with every file it names"
            );
            assert!(copied_files(COPIED_FILES_MIME, &copied_files_list(&paths, true)).cut);
        }

        {
            let body = "cut\r\nfile:///home/me/a.txt\r\nfile:///home/me/b.txt\r\n";
            assert_eq!(
                copied_files(COPIED_FILES_MIME, body),
                CopiedFiles {
                    paths: vec![
                        PathBuf::from("/home/me/a.txt"),
                        PathBuf::from("/home/me/b.txt")
                    ],
                    cut: true
                }
            );
        }

        // Case `a_list_with_a_comment_or_a_stray_line_keeps_its_files`:
        // One line that is not a file URI must not lose the paste.
        {
            let body =
                "# copied from Files\r\nfile:///a\r\nhttps://example.com/b\r\n\r\nfile:///c\r\n";
            assert_eq!(
                copied_files(URI_LIST_MIME, body).paths,
                vec![PathBuf::from("/a"), PathBuf::from("/c")]
            );
        }

        assert!(is_file_mime(URI_LIST_MIME));
        assert!(is_file_mime(COPIED_FILES_MIME));
        assert!(
            !is_file_mime("text/plain;charset=utf-8"),
            "text is not files"
        );
    }

    #[test]
    fn a_drag_in_a_hosted_process_waits_for_its_host() {
        host_drags_in_process();
        let files = file_offers(&[PathBuf::from("/a.png")]);
        assert!(start_drag_with(files), "the host takes it");
        assert_eq!(
            take_hosted_drag(),
            Some(vec![(
                URI_LIST_MIME.to_owned(),
                b"file:///a.png\r\n".to_vec()
            )]),
            "files travel as their URI list"
        );
        assert_eq!(take_hosted_drag(), None, "taken once");
    }

    #[test]
    fn outgoing_drags_encode_uri_lists_and_content_types() {
        {
            assert_eq!(
                uri_list(&[PathBuf::from("/home/me/my photo#1.png")]),
                "file:///home/me/my%20photo%231.png\r\n"
            );
        }

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

        // Case `a_dragged_file_is_offered_unread`:
        // Starting a drag runs inside a pointer motion on the app's thread;
        // reading the file there froze Files for 11 s on a 4.3 GB video. The
        // drag names the file and its bytes stream when a receiver asks.
        {
            let video = PathBuf::from("/home/me/Downloads/film.png");
            assert_eq!(
                file_offers(std::slice::from_ref(&video)),
                vec![
                    ("image/png".to_string(), Offer::File(video.clone())),
                    (
                        URI_LIST_MIME.to_string(),
                        Offer::Bytes(uri_list(std::slice::from_ref(&video)).into_bytes())
                    ),
                ]
            );
            let folder = PathBuf::from("/home/me/Downloads");
            assert_eq!(
                file_offers(std::slice::from_ref(&folder)),
                vec![(
                    URI_LIST_MIME.to_string(),
                    Offer::Bytes(uri_list(std::slice::from_ref(&folder)).into_bytes())
                )]
            );
        }

        {
            assert!(!start_file_drag(&[]));
        }
    }
}
