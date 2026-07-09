//! Host-native file open via the **XDG Desktop Portal**
//! (`org.freedesktop.portal.FileChooser`, through the [`ashpd`] crate).
//!
//! Per the qOS HIG (§5 system integration) an app must **never draw its own
//! file picker**: open/save goes through the portal so the *host desktop*
//! provides the chooser — on GNOME the user sees GNOME's file dialog; on qOS
//! the compositor implements the portal backend and renders qOS's picker
//! (which is also where the per-cluster sandbox / file-access prompt lives, so
//! the portal is the security boundary).
//!
//! The portal call is asynchronous and opens a real host window, so it must
//! **never** run on the UI thread (HIG §6). [`request_open`] spawns a worker
//! thread with its own small Tokio runtime, runs the D-Bus round-trip there,
//! and hands the outcome back over an [`std::sync::mpsc`] channel. The reader
//! drains that channel once per frame and loads whatever path the user chose;
//! the UI thread never blocks.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;

/// The result of a portal file-open interaction, delivered back to the UI
/// thread over a channel.
#[derive(Debug)]
pub enum PortalOutcome {
    /// The user picked a local file at this path.
    Opened(PathBuf),
    /// The user dismissed the picker without choosing anything.
    Cancelled,
    /// The portal was unavailable or errored (no desktop portal running, a
    /// non-local URI, a D-Bus failure). Carries a human-readable reason.
    Failed(String),
}

/// A pending portal request: the receiver the UI thread drains each frame.
/// `Some(rx)` means a picker is in flight; `None` means idle.
pub type PortalChannel = Receiver<PortalOutcome>;

/// Spawn the portal file-open on a worker thread and return the receiver the
/// caller drains each frame. The UI thread is never blocked: the D-Bus
/// round-trip and the host dialog live entirely on the worker.
pub fn request_open() -> PortalChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("pdf-portal-open".into())
        .spawn(move || {
            let outcome = run_open_blocking();
            // The UI may have moved on (window closed); ignore a dead channel.
            let _ = tx.send(outcome);
        })
        .expect("spawn portal worker thread");
    rx
}

/// Spawn a portal file-open for *importing* pages — a PDF or an image — and
/// return the receiver the caller drains each frame. Same non-blocking contract
/// as [`request_open`]; the chosen path arrives as [`PortalOutcome::Opened`].
pub fn request_import() -> PortalChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("pdf-portal-import".into())
        .spawn(move || {
            let outcome = run_import_blocking();
            let _ = tx.send(outcome);
        })
        .expect("spawn portal import worker thread");
    rx
}

/// Spawn the portal file-*save* on a worker thread and return the receiver the
/// caller drains each frame. `suggested_name` seeds the dialog's filename. The
/// chosen path comes back as [`PortalOutcome::Opened`] (the save target), so the
/// UI thread is never blocked — same contract as [`request_open`].
pub fn request_save(suggested_name: String) -> PortalChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("pdf-portal-save".into())
        .spawn(move || {
            let outcome = run_save_blocking(suggested_name);
            let _ = tx.send(outcome);
        })
        .expect("spawn portal save worker thread");
    rx
}

/// Run the portal `OpenFile` round-trip to completion on a private Tokio
/// runtime. Blocking — only ever called on the worker thread from
/// [`request_open`]. Split out and `pub` so its result mapping is unit-testable
/// via [`map_uris`].
fn run_open_blocking() -> PortalOutcome {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => return PortalOutcome::Failed(format!("portal runtime: {e}")),
    };
    rt.block_on(open_future())
}

/// Run the portal `SaveFile` round-trip to completion on a private Tokio
/// runtime. Blocking — only ever called on the worker thread from
/// [`request_save`].
fn run_save_blocking(suggested_name: String) -> PortalOutcome {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => return PortalOutcome::Failed(format!("portal runtime: {e}")),
    };
    rt.block_on(save_future(suggested_name))
}

/// Run the portal import `OpenFile` round-trip (PDF or image) to completion on
/// a private Tokio runtime. Blocking — only ever called on the worker thread
/// from [`request_import`].
fn run_import_blocking() -> PortalOutcome {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => return PortalOutcome::Failed(format!("portal runtime: {e}")),
    };
    rt.block_on(import_future())
}

/// The async portal interaction. Kept separate so the wiring reads cleanly.
async fn open_future() -> PortalOutcome {
    use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};

    // A PDF-only filter, plus an "all files" escape hatch so the user is never
    // trapped if a PDF has an odd MIME type.
    let request = SelectedFiles::open_file()
        .title("Open a PDF")
        .accept_label("Open")
        .modal(true)
        .multiple(false)
        .filter(FileFilter::new("PDF documents").mimetype("application/pdf"))
        .filter(FileFilter::new("All files").glob("*"));

    match request.send().await.and_then(|r| r.response()) {
        Ok(files) => map_uris(files.uris()),
        // A user dismissal surfaces as a "cancelled" response error, not a bug.
        Err(ashpd::Error::Response(_)) => PortalOutcome::Cancelled,
        Err(e) => PortalOutcome::Failed(e.to_string()),
    }
}

/// The async portal *import* interaction — a PDF or an image to add as pages.
async fn import_future() -> PortalOutcome {
    use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};

    let request = SelectedFiles::open_file()
        .title("Import pages from a PDF or image")
        .accept_label("Import")
        .modal(true)
        .multiple(false)
        .filter(
            FileFilter::new("PDF or image")
                .mimetype("application/pdf")
                .mimetype("image/png")
                .mimetype("image/jpeg"),
        )
        .filter(FileFilter::new("All files").glob("*"));

    match request.send().await.and_then(|r| r.response()) {
        Ok(files) => map_uris(files.uris()),
        Err(ashpd::Error::Response(_)) => PortalOutcome::Cancelled,
        Err(e) => PortalOutcome::Failed(e.to_string()),
    }
}

/// The async portal save interaction. Seeds the dialog with `suggested_name`;
/// the chosen path is returned via [`map_uris`] as an `Opened` outcome.
async fn save_future(suggested_name: String) -> PortalOutcome {
    use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};

    let request = SelectedFiles::save_file()
        .title("Save annotated PDF")
        .accept_label("Save")
        .modal(true)
        .current_name(suggested_name.as_str())
        .filter(FileFilter::new("PDF documents").mimetype("application/pdf"));

    match request.send().await.and_then(|r| r.response()) {
        Ok(files) => map_uris(files.uris()),
        Err(ashpd::Error::Response(_)) => PortalOutcome::Cancelled,
        Err(e) => PortalOutcome::Failed(e.to_string()),
    }
}

/// Map the portal's returned URIs to an outcome: the first local `file://`
/// path wins, an empty list is a cancel, and a non-local URI is a failure.
/// Pure, so the channel/threading contract can be tested without a portal.
pub fn map_uris(uris: &[url::Url]) -> PortalOutcome {
    let Some(uri) = uris.first() else {
        return PortalOutcome::Cancelled;
    };
    match uri.to_file_path() {
        Ok(path) => PortalOutcome::Opened(path),
        Err(()) => PortalOutcome::Failed(format!("not a local file: {uri}")),
    }
}

/// Test-only: build a channel and immediately deliver `outcome` on it, so a
/// headless test can exercise the exact per-frame drain path the real portal
/// worker feeds — without opening a host dialog. Returns the receiver the app
/// installs, mirroring [`request_open`]'s contract.
#[cfg(test)]
pub fn inject_for_test(outcome: PortalOutcome) -> PortalChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(outcome).expect("seed injected portal outcome");
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_uris_takes_first_local_file() {
        let uris = vec![
            url::Url::parse("file:///home/alice/report.pdf").unwrap(),
            url::Url::parse("file:///home/alice/other.pdf").unwrap(),
        ];
        match map_uris(&uris) {
            PortalOutcome::Opened(p) => assert_eq!(p, std::path::Path::new("/home/alice/report.pdf")),
            other => panic!("expected Opened, got {other:?}"),
        }
    }

    #[test]
    fn map_uris_empty_is_cancel() {
        assert!(matches!(map_uris(&[]), PortalOutcome::Cancelled));
    }

    #[test]
    fn map_uris_non_local_is_failure() {
        // A remote URI has no local file path, so it must not be treated as an
        // openable document.
        let uris = vec![url::Url::parse("https://example.com/remote.pdf").unwrap()];
        assert!(matches!(map_uris(&uris), PortalOutcome::Failed(_)));
    }

    #[test]
    fn inject_delivers_the_outcome_immediately() {
        let rx = inject_for_test(PortalOutcome::Cancelled);
        assert!(matches!(rx.try_recv(), Ok(PortalOutcome::Cancelled)));
    }
}
