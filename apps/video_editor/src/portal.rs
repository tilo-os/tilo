//! Host-native media import via the **XDG Desktop Portal**
//! (`org.freedesktop.portal.FileChooser`, through [`ashpd`]).
//!
//! Per the qOS HIG an app never draws its own file picker: the portal is the
//! host desktop's chooser and, on qOS, the compositor's picker — which is also
//! where the per-workspace sandbox file-access prompt lives, so the portal is
//! the security boundary. The picker returns `file://` URIs; a
//! `/var/workspace/...` pick resolves through the FUSE mount and hydrates on
//! read exactly like a local file (see [`crate::media`]).
//!
//! The round-trip is async and opens a real host window, so it runs on a worker
//! thread with its own Tokio runtime and hands the outcome back over an
//! `std::sync::mpsc` channel the editor drains each frame. The ashpd/tokio path
//! is gated behind the `portal` feature; the pure URI→path mapping compiles
//! everywhere so it stays unit-testable without D-Bus.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;

/// The result of a portal import interaction, delivered to the UI thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportOutcome {
    /// Import these local (or FUSE-workspace) files into the media bin.
    Chosen(Vec<PathBuf>),
    /// The user dismissed the picker.
    Cancelled,
    /// The portal was unavailable or errored. Carries a human-readable reason.
    Failed(String),
}

/// A pending import: the receiver the UI thread drains each frame.
pub type ImportChannel = Receiver<ImportOutcome>;

/// Spawn the portal import on a worker thread, returning the receiver the caller
/// drains each frame. The UI thread is never blocked.
#[cfg(feature = "portal")]
pub fn request_import() -> ImportChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("video-editor-import".into())
        .spawn(move || {
            let _ = tx.send(run_import_blocking());
        })
        .expect("spawn portal worker thread");
    rx
}

/// Without the `portal` feature there is no picker: return an immediately
/// "failed" channel so the caller still gets one drained outcome and never
/// hangs waiting for a picker that can't open. Honest — no fake success.
#[cfg(not(feature = "portal"))]
pub fn request_import() -> ImportChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = tx.send(ImportOutcome::Failed(
        "Media import needs the real-desktop build (`--features portal`)".into(),
    ));
    rx
}

#[cfg(feature = "portal")]
fn run_import_blocking() -> ImportOutcome {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => return ImportOutcome::Failed(format!("portal runtime: {e}")),
    };
    rt.block_on(import_future())
}

#[cfg(feature = "portal")]
async fn import_future() -> ImportOutcome {
    use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};

    let request = SelectedFiles::open_file()
        .title("Import media")
        .accept_label("Import")
        .modal(true)
        .multiple(true)
        .filter(
            FileFilter::new("Media")
                .mimetype("video/mp4")
                .mimetype("video/webm")
                .mimetype("video/x-matroska")
                .mimetype("video/quicktime")
                .mimetype("audio/wav")
                .mimetype("audio/mpeg")
                .mimetype("image/png")
                .mimetype("image/jpeg")
                .glob("*.mp4")
                .glob("*.mov")
                .glob("*.webm")
                .glob("*.mkv")
                .glob("*.wav")
                .glob("*.mp3")
                .glob("*.png")
                .glob("*.jpg")
                .glob("*.jpeg"),
        )
        .filter(FileFilter::new("All files").glob("*"));

    match request.send().await.and_then(|r| r.response()) {
        Ok(files) => {
            let uris: Vec<String> = files.uris().iter().map(|u| u.to_string()).collect();
            map_import_uris(&uris)
        }
        // A user dismissal surfaces as a "cancelled" response error, not a bug.
        Err(ashpd::Error::Response(_)) => ImportOutcome::Cancelled,
        Err(e) => ImportOutcome::Failed(e.to_string()),
    }
}

/// Map the portal's returned URIs to an outcome: every local `file://` path is
/// imported; an empty list is a cancel. Non-local URIs are skipped. Pure, so it
/// is unit-testable without a portal.
pub fn map_import_uris(uris: &[String]) -> ImportOutcome {
    if uris.is_empty() {
        return ImportOutcome::Cancelled;
    }
    let paths: Vec<PathBuf> = uris
        .iter()
        .filter_map(|u| u.strip_prefix("file://"))
        .map(|rest| PathBuf::from(percent_decode(rest)))
        .collect();
    if paths.is_empty() {
        ImportOutcome::Failed("no local files were selected".into())
    } else {
        ImportOutcome::Chosen(paths)
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
pub fn inject_for_test(outcome: ImportOutcome) -> ImportChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(outcome).expect("seed injected import outcome");
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_every_local_file_and_decodes_spaces() {
        let uris = vec![
            "file:///home/alice/My%20Clip.mp4".to_string(),
            "file:///var/workspace/media/b.mov".to_string(),
        ];
        assert_eq!(
            map_import_uris(&uris),
            ImportOutcome::Chosen(vec![
                PathBuf::from("/home/alice/My Clip.mp4"),
                PathBuf::from("/var/workspace/media/b.mov"),
            ])
        );
    }

    #[test]
    fn empty_is_cancel_and_all_remote_is_failure() {
        assert_eq!(map_import_uris(&[]), ImportOutcome::Cancelled);
        assert!(matches!(
            map_import_uris(&["https://x/v.mp4".to_string()]),
            ImportOutcome::Failed(_)
        ));
    }
}
