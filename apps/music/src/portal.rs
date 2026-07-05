//! Host-native image picking via the **XDG Desktop Portal**
//! (`org.freedesktop.portal.FileChooser`, through the [`ashpd`] crate).
//!
//! Used to set a **playlist cover** from a local image. Per the qOS HIG an app
//! must never draw its own file picker: it goes through the portal so the *host
//! desktop* provides the chooser — on GNOME the user sees GNOME's file dialog;
//! on qOS the compositor implements the portal backend and renders qOS's picker
//! (which is also where the per-workspace sandbox file-access prompt lives, so
//! the portal is the security boundary). Mirrors `apps/contacts`/`apps/slides`.
//!
//! The portal call is async and opens a real host window, so it must never run
//! on the UI thread: [`request_cover`] spawns a worker thread with its own small
//! Tokio runtime, runs the D-Bus round-trip there, and hands the outcome back
//! over an [`std::sync::mpsc`] channel the app drains once per frame. The
//! ashpd/tokio path is gated behind the `audio` (real-desktop) feature; the pure
//! URI→path mapping compiles everywhere so it stays unit-testable.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;

/// The result of a portal image-open interaction, delivered to the UI thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoverOutcome {
    /// The playlist `playlist_id` should adopt the image at this local path.
    Chosen { playlist_id: String, path: PathBuf },
    /// The user dismissed the picker without choosing anything.
    Cancelled,
    /// The portal was unavailable or errored (no desktop portal, a non-local
    /// URI, a D-Bus failure). Carries a human-readable reason.
    Failed(String),
}

/// A pending cover-pick: the receiver the UI thread drains each frame. `Some`
/// means a picker is in flight; `None` means idle.
pub type CoverChannel = Receiver<CoverOutcome>;

/// Spawn the portal image-open on a worker thread for `playlist_id`, returning
/// the receiver the caller drains each frame. The UI thread is never blocked.
#[cfg(feature = "audio")]
pub fn request_cover(playlist_id: String) -> CoverChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("music-portal-cover".into())
        .spawn(move || {
            let outcome = run_cover_blocking(playlist_id);
            let _ = tx.send(outcome);
        })
        .expect("spawn portal cover worker thread");
    rx
}

/// Without the real-desktop feature there is no portal: return an immediately
/// "failed" channel so callers still get a single drained outcome and never
/// hang waiting for a picker that can't open.
#[cfg(not(feature = "audio"))]
pub fn request_cover(_playlist_id: String) -> CoverChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = tx.send(CoverOutcome::Failed(
        "file portal needs the real-desktop build (`--features audio`)".into(),
    ));
    rx
}

/// Run the portal `OpenFile` (single image) round-trip on a private Tokio
/// runtime. Blocking — only ever called on the worker thread.
#[cfg(feature = "audio")]
fn run_cover_blocking(playlist_id: String) -> CoverOutcome {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => return CoverOutcome::Failed(format!("portal runtime: {e}")),
    };
    rt.block_on(cover_future(playlist_id))
}

/// The async cover-pick interaction.
#[cfg(feature = "audio")]
async fn cover_future(playlist_id: String) -> CoverOutcome {
    use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};

    let request = SelectedFiles::open_file()
        .title("Choose a playlist cover")
        .accept_label("Set cover")
        .modal(true)
        .multiple(false)
        .filter(
            FileFilter::new("Images")
                .mimetype("image/png")
                .mimetype("image/jpeg")
                .mimetype("image/webp")
                .glob("*.png")
                .glob("*.jpg")
                .glob("*.jpeg")
                .glob("*.webp"),
        )
        .filter(FileFilter::new("All files").glob("*"));

    match request.send().await.and_then(|r| r.response()) {
        Ok(files) => map_cover_uri(&playlist_id, files.uris()),
        // A user dismissal surfaces as a "cancelled" response error, not a bug.
        Err(ashpd::Error::Response(_)) => CoverOutcome::Cancelled,
        Err(e) => CoverOutcome::Failed(e.to_string()),
    }
}

/// Map the portal's returned URIs to a cover outcome: the first local `file://`
/// path wins, an empty list is a cancel, a non-local URI a failure. Pure, so the
/// mapping is unit-testable without a portal.
pub fn map_cover_uri(playlist_id: &str, uris: &[url::Url]) -> CoverOutcome {
    let Some(uri) = uris.first() else {
        return CoverOutcome::Cancelled;
    };
    match uri.to_file_path() {
        Ok(path) => CoverOutcome::Chosen { playlist_id: playlist_id.to_string(), path },
        Err(()) => CoverOutcome::Failed(format!("not a local file: {uri}")),
    }
}

/// Test-only: build a channel and immediately deliver `outcome`, so a headless
/// test can drive the exact per-frame drain path the real worker feeds.
#[cfg(test)]
pub fn inject_cover_for_test(outcome: CoverOutcome) -> CoverChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(outcome).expect("seed injected cover outcome");
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cover_uri_takes_first_local_file() {
        let uris = vec![url::Url::parse("file:///home/alice/cover.png").unwrap()];
        match map_cover_uri("pl_morning", &uris) {
            CoverOutcome::Chosen { playlist_id, path } => {
                assert_eq!(playlist_id, "pl_morning");
                assert_eq!(path, std::path::Path::new("/home/alice/cover.png"));
            }
            other => panic!("expected Chosen, got {other:?}"),
        }
    }

    #[test]
    fn cover_uri_empty_is_cancel() {
        assert_eq!(map_cover_uri("pl_x", &[]), CoverOutcome::Cancelled);
    }

    #[test]
    fn cover_uri_non_local_is_failure() {
        let uris = vec![url::Url::parse("https://example.com/cover.png").unwrap()];
        assert!(matches!(map_cover_uri("pl_x", &uris), CoverOutcome::Failed(_)));
    }

    #[test]
    fn inject_delivers_immediately() {
        let rx = inject_cover_for_test(CoverOutcome::Cancelled);
        assert_eq!(rx.try_recv(), Ok(CoverOutcome::Cancelled));
    }
}
