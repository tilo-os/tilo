//! Host-native file picking via the **XDG Desktop Portal**
//! (`org.freedesktop.portal.FileChooser`, through [`ashpd`]).
//!
//! Per the qOS HIG an app never draws its own file picker: the portal is the
//! host desktop's chooser and, on qOS, the compositor's picker — which is also
//! where the per-workspace sandbox file-access prompt lives, so the portal is
//! the security boundary. The picker returns a `file://` URI; a
//! `/var/workspace/...` pick resolves through the FUSE mount and hydrates on
//! read exactly like a local file.
//!
//! The portal round-trip is async and opens a real host window, so it runs on a
//! worker thread with its own Tokio runtime and hands the outcome back over an
//! `std::sync::mpsc` channel the app drains each frame. The ashpd/tokio path is
//! gated behind the `portal` feature; the pure URI→path mapping compiles
//! everywhere so it stays unit-testable and `--open PATH` works without D-Bus.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;

/// The result of a portal file-open interaction, delivered to the UI thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenOutcome {
    /// Play the media at this local (or FUSE-workspace) path.
    Chosen(PathBuf),
    /// The user dismissed the picker.
    Cancelled,
    /// The portal was unavailable or errored. Carries a human-readable reason.
    Failed(String),
}

/// A pending open: the receiver the UI thread drains each frame.
pub type OpenChannel = Receiver<OpenOutcome>;

/// Spawn the portal file-open on a worker thread, returning the receiver the
/// caller drains each frame. The UI thread is never blocked.
#[cfg(feature = "portal")]
pub fn request_open() -> OpenChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("video-player-portal".into())
        .spawn(move || {
            let _ = tx.send(run_open_blocking());
        })
        .expect("spawn portal worker thread");
    rx
}

/// Without the `portal` feature there is no picker: return an immediately
/// "failed" channel so the caller still gets one drained outcome and never
/// hangs waiting for a picker that can't open.
#[cfg(not(feature = "portal"))]
pub fn request_open() -> OpenChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = tx.send(OpenOutcome::Failed(
        "file portal needs the real-desktop build (`--features portal`); use `--open PATH`".into(),
    ));
    rx
}

#[cfg(feature = "portal")]
fn run_open_blocking() -> OpenOutcome {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => return OpenOutcome::Failed(format!("portal runtime: {e}")),
    };
    rt.block_on(open_future())
}

#[cfg(feature = "portal")]
async fn open_future() -> OpenOutcome {
    use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};

    let request = SelectedFiles::open_file()
        .title("Open a video")
        .accept_label("Play")
        .modal(true)
        .multiple(false)
        .filter(
            FileFilter::new("Video")
                .mimetype("video/mp4")
                .mimetype("video/webm")
                .mimetype("video/x-matroska")
                .mimetype("video/quicktime")
                .glob("*.mp4")
                .glob("*.webm")
                .glob("*.mkv")
                .glob("*.mov")
                .glob("*.avi"),
        )
        .filter(FileFilter::new("All files").glob("*"));

    match request.send().await.and_then(|r| r.response()) {
        Ok(files) => {
            let uris: Vec<String> = files.uris().iter().map(|u| u.to_string()).collect();
            map_open_uri(&uris)
        }
        // A user dismissal surfaces as a "cancelled" response error, not a bug.
        Err(ashpd::Error::Response(_)) => OpenOutcome::Cancelled,
        Err(e) => OpenOutcome::Failed(e.to_string()),
    }
}

/// Map the portal's returned URIs to an outcome: the first local `file://`
/// path wins, an empty list is a cancel, a non-local URI a failure. Pure, so it
/// is unit-testable without a portal.
pub fn map_open_uri(uris: &[String]) -> OpenOutcome {
    let Some(uri) = uris.first() else {
        return OpenOutcome::Cancelled;
    };
    match uri.strip_prefix("file://") {
        Some(rest) => OpenOutcome::Chosen(PathBuf::from(percent_decode(rest))),
        None => OpenOutcome::Failed(format!("not a local file: {uri}")),
    }
}

/// Minimal `%XX` percent-decoding for `file://` paths (the inverse of the
/// encoder in `media::source`). Leaves malformed escapes untouched.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Test-only: build a channel and immediately deliver `outcome`, so a headless
/// test can drive the exact per-frame drain path the real worker feeds.
#[cfg(test)]
pub fn inject_for_test(outcome: OpenOutcome) -> OpenChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(outcome).expect("seed injected open outcome");
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_first_local_file_and_decodes_spaces() {
        let uris = vec!["file:///home/alice/My%20Clip.mp4".to_string()];
        assert_eq!(
            map_open_uri(&uris),
            OpenOutcome::Chosen(PathBuf::from("/home/alice/My Clip.mp4"))
        );
    }

    #[test]
    fn empty_is_cancel_and_remote_is_failure() {
        assert_eq!(map_open_uri(&[]), OpenOutcome::Cancelled);
        assert!(matches!(
            map_open_uri(&["https://x/v.mp4".to_string()]),
            OpenOutcome::Failed(_)
        ));
    }

    #[test]
    fn workspace_path_maps_through_the_mount() {
        let uris = vec!["file:///var/workspace/media/a.mkv".to_string()];
        assert_eq!(
            map_open_uri(&uris),
            OpenOutcome::Chosen(PathBuf::from("/var/workspace/media/a.mkv"))
        );
    }
}
