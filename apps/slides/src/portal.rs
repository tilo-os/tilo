//! Host-native image open via the **XDG Desktop Portal**
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
//! drains that channel once per frame and inserts whatever image the user
//! chose; the UI thread never blocks.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;

/// The result of a portal file-open interaction, delivered back to the UI
/// thread over a channel.
#[derive(Debug)]
pub enum PortalOutcome {
    /// The user picked a local image file at this path.
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

/// The result of a `.odp` document open/save interaction. Open carries the
/// path the user chose to load; Save carries the path to write the deck to.
#[derive(Debug)]
pub enum DocOutcome {
    Open(PathBuf),
    Save(PathBuf),
    Cancelled,
    Failed(String),
}

/// Receiver the UI thread drains for an in-flight `.odp` open/save.
pub type DocPortalChannel = Receiver<DocOutcome>;

/// Open the host file chooser for an OpenDocument Presentation to load, on a
/// worker thread. The chosen path arrives over the returned channel; the UI
/// thread reads + decodes the file when it drains it.
pub fn request_open_document() -> DocPortalChannel {
    spawn_doc("slides-portal-open-doc", || run_doc_blocking(false))
}

/// Open the host Save dialog for an OpenDocument Presentation. `suggested_name`
/// seeds the filename field. The chosen destination arrives over the channel;
/// the UI thread serialises + writes the deck there.
pub fn request_save_document(suggested_name: &str) -> DocPortalChannel {
    let name = suggested_name.to_string();
    spawn_doc("slides-portal-save-doc", move || run_doc_blocking_save(&name))
}

fn spawn_doc(
    thread_name: &str,
    work: impl FnOnce() -> DocOutcome + Send + 'static,
) -> DocPortalChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name(thread_name.into())
        .spawn(move || {
            let _ = tx.send(work());
        })
        .expect("spawn portal worker thread");
    rx
}

fn run_doc_blocking(_save: bool) -> DocOutcome {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => return DocOutcome::Failed(format!("portal runtime: {e}")),
    };
    rt.block_on(open_document_future())
}

fn run_doc_blocking_save(name: &str) -> DocOutcome {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => return DocOutcome::Failed(format!("portal runtime: {e}")),
    };
    rt.block_on(save_document_future(name))
}

async fn open_document_future() -> DocOutcome {
    use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};
    let request = SelectedFiles::open_file()
        .title("Open presentation")
        .accept_label("Open")
        .modal(true)
        .multiple(false)
        .filter(FileFilter::new("Presentations").glob("*.odp"))
        .filter(FileFilter::new("All files").glob("*"));
    match request.send().await.and_then(|r| r.response()) {
        Ok(files) => match map_uris(files.uris()) {
            PortalOutcome::Opened(p) => DocOutcome::Open(p),
            PortalOutcome::Cancelled => DocOutcome::Cancelled,
            PortalOutcome::Failed(e) => DocOutcome::Failed(e),
        },
        Err(ashpd::Error::Response(_)) => DocOutcome::Cancelled,
        Err(e) => DocOutcome::Failed(e.to_string()),
    }
}

async fn save_document_future(name: &str) -> DocOutcome {
    use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};
    let request = SelectedFiles::save_file()
        .title("Save presentation")
        .accept_label("Save")
        .modal(true)
        .current_name(name)
        .filter(FileFilter::new("Presentations").glob("*.odp"));
    match request.send().await.and_then(|r| r.response()) {
        Ok(files) => match map_uris(files.uris()) {
            PortalOutcome::Opened(p) => DocOutcome::Save(p),
            PortalOutcome::Cancelled => DocOutcome::Cancelled,
            PortalOutcome::Failed(e) => DocOutcome::Failed(e),
        },
        Err(ashpd::Error::Response(_)) => DocOutcome::Cancelled,
        Err(e) => DocOutcome::Failed(e.to_string()),
    }
}

/// Test-only: build a channel and immediately deliver a `.odp` `outcome`.
#[cfg(test)]
pub fn inject_doc_for_test(outcome: DocOutcome) -> DocPortalChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(outcome).expect("seed injected doc outcome");
    rx
}

/// Spawn the portal image-open on a worker thread and return the receiver the
/// caller drains each frame. The UI thread is never blocked: the D-Bus
/// round-trip and the host dialog live entirely on the worker.
pub fn request_open() -> PortalChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("slides-portal-open".into())
        .spawn(move || {
            let outcome = run_open_blocking();
            // The UI may have moved on (window closed); ignore a dead channel.
            let _ = tx.send(outcome);
        })
        .expect("spawn portal worker thread");
    rx
}

/// Run the portal `OpenFile` round-trip to completion on a private Tokio
/// runtime. Blocking — only ever called on the worker thread from
/// [`request_open`]. Split out so its result mapping is unit-testable via
/// [`map_uris`].
fn run_open_blocking() -> PortalOutcome {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => return PortalOutcome::Failed(format!("portal runtime: {e}")),
    };
    rt.block_on(open_future())
}

/// The async portal interaction. Kept separate so the wiring reads cleanly.
async fn open_future() -> PortalOutcome {
    use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};

    // An images-only filter, plus an "all files" escape hatch so the user is
    // never trapped if a picture has an unusual MIME type.
    let request = SelectedFiles::open_file()
        .title("Insert image")
        .accept_label("Insert")
        .modal(true)
        .multiple(false)
        .filter(
            FileFilter::new("Images")
                .glob("*.png")
                .glob("*.jpg")
                .glob("*.jpeg")
                .glob("*.gif")
                .glob("*.webp")
                .glob("*.bmp")
                .glob("*.svg"),
        )
        .filter(FileFilter::new("All files").glob("*"));

    match request.send().await.and_then(|r| r.response()) {
        Ok(files) => map_uris(files.uris()),
        // A user dismissal surfaces as a "cancelled" response error, not a bug.
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
            url::Url::parse("file:///home/alice/photo.png").unwrap(),
            url::Url::parse("file:///home/alice/other.png").unwrap(),
        ];
        match map_uris(&uris) {
            PortalOutcome::Opened(p) => assert_eq!(p, std::path::Path::new("/home/alice/photo.png")),
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
        // openable image.
        let uris = vec![url::Url::parse("https://example.com/remote.png").unwrap()];
        assert!(matches!(map_uris(&uris), PortalOutcome::Failed(_)));
    }

    #[test]
    fn inject_delivers_the_outcome_immediately() {
        let rx = inject_for_test(PortalOutcome::Cancelled);
        assert!(matches!(rx.try_recv(), Ok(PortalOutcome::Cancelled)));
    }
}
