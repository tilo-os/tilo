//! Host-native file open/save via the **XDG Desktop Portal**
//! (`org.freedesktop.portal.FileChooser`, through the [`ashpd`] crate).
//!
//! Per the qOS HIG (§5 system integration) an app must **never draw its own
//! file picker**: open/save goes through the portal so the *host desktop*
//! provides the chooser — on GNOME the user sees GNOME's file dialog; on qOS
//! the compositor implements the portal backend and renders qOS's picker
//! (which is also where the per-workspace sandbox / file-access prompt lives,
//! so the portal is the security boundary).
//!
//! The portal call is asynchronous and opens a real host window, so it must
//! **never** run on the UI thread (HIG §6). Each `request_*` spawns a worker
//! thread with its own small Tokio runtime, runs the D-Bus round-trip there,
//! and hands the outcome back over an [`std::sync::mpsc`] channel. The editor
//! drains that channel once per frame from `sync`; the UI thread never blocks.
//!
//! Three interactions are exposed, matching the editor's real needs:
//!
//! * [`request_open_file`] — open a single existing file into a tab.
//! * [`request_open_folder`] — open a directory as the project tree
//!   (`OpenFileRequest::directory(true)`).
//! * [`request_save`] — a Save-As target for the active buffer
//!   (`SaveFileRequest`, seeded with a suggested name).

use std::path::PathBuf;
use std::sync::mpsc::Receiver;

/// The result of a portal interaction, delivered back to the UI thread over a
/// channel. For open, the path is a file/folder to read; for save, it is the
/// destination to write.
#[derive(Debug)]
pub enum PortalOutcome {
    /// The user picked a local path (a file for open, a folder for open-folder,
    /// or a save destination for save-as).
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

/// Spawn a portal file-open on a worker thread and return the receiver the
/// caller drains each frame. The UI thread is never blocked.
pub fn request_open_file() -> PortalChannel {
    spawn(|| run_blocking(open_file_future()))
}

/// Spawn a portal folder-open (directory selection) on a worker thread.
pub fn request_open_folder() -> PortalChannel {
    spawn(|| run_blocking(open_folder_future()))
}

/// Spawn a portal save-as on a worker thread, seeded with `suggested` as the
/// default file name.
pub fn request_save(suggested: String) -> PortalChannel {
    spawn(move || run_blocking(save_future(suggested)))
}

/// Shared worker-thread wiring: run `f` on a private thread and deliver its
/// outcome over a fresh channel.
fn spawn<F>(f: F) -> PortalChannel
where
    F: FnOnce() -> PortalOutcome + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("code-editor-portal".into())
        .spawn(move || {
            let outcome = f();
            // The UI may have moved on (window closed); ignore a dead channel.
            let _ = tx.send(outcome);
        })
        .expect("spawn portal worker thread");
    rx
}

/// Drive `fut` to completion on a private Tokio runtime. Blocking — only ever
/// called on a worker thread.
fn run_blocking<F: std::future::Future<Output = PortalOutcome>>(fut: F) -> PortalOutcome {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => return PortalOutcome::Failed(format!("portal runtime: {e}")),
    };
    rt.block_on(fut)
}

/// The async portal `OpenFile` interaction for a single source file.
async fn open_file_future() -> PortalOutcome {
    use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};

    // A text/source filter, plus an "all files" escape hatch so the user is
    // never trapped when a file has an unusual MIME type.
    let request = SelectedFiles::open_file()
        .title("Open a file")
        .accept_label("Open")
        .modal(true)
        .multiple(false)
        .filter(
            FileFilter::new("Text & source")
                .mimetype("text/plain")
                .glob("*.txt")
                .glob("*.md")
                .glob("*.rs")
                .glob("*.toml")
                .glob("*.json")
                .glob("*.yaml")
                .glob("*.yml")
                .glob("*.c")
                .glob("*.h")
                .glob("*.cpp")
                .glob("*.go")
                .glob("*.py")
                .glob("*.js")
                .glob("*.ts")
                .glob("*.html")
                .glob("*.css")
                .glob("*.sh"),
        )
        .filter(FileFilter::new("All files").glob("*"));

    match request.send().await.and_then(|r| r.response()) {
        Ok(files) => map_uris(files.uris()),
        // A user dismissal surfaces as a "cancelled" response error, not a bug.
        Err(ashpd::Error::Response(_)) => PortalOutcome::Cancelled,
        Err(e) => PortalOutcome::Failed(e.to_string()),
    }
}

/// The async portal `OpenFile` interaction in *directory* mode — the user
/// picks a folder that becomes the project tree.
async fn open_folder_future() -> PortalOutcome {
    use ashpd::desktop::file_chooser::SelectedFiles;

    let request = SelectedFiles::open_file()
        .title("Open a folder")
        .accept_label("Open")
        .modal(true)
        .multiple(false)
        .directory(true);

    match request.send().await.and_then(|r| r.response()) {
        Ok(files) => map_uris(files.uris()),
        Err(ashpd::Error::Response(_)) => PortalOutcome::Cancelled,
        Err(e) => PortalOutcome::Failed(e.to_string()),
    }
}

/// The async portal `SaveFile` interaction. Seeds the dialog with
/// `suggested_name`; the chosen destination is returned via [`map_uris`] as an
/// `Opened` outcome.
async fn save_future(suggested_name: String) -> PortalOutcome {
    use ashpd::desktop::file_chooser::SelectedFiles;

    let request = SelectedFiles::save_file()
        .title("Save file")
        .accept_label("Save")
        .modal(true)
        .current_name(suggested_name.as_str());

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
        Err(()) => PortalOutcome::Failed(format!("not a local path: {uri}")),
    }
}

/// Test-only: build a channel and immediately deliver `outcome` on it, so a
/// headless test can exercise the exact per-frame drain path the real portal
/// worker feeds — without opening a host dialog. Returns the receiver the app
/// installs, mirroring the `request_*` contract.
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
            url::Url::parse("file:///home/alice/main.rs").unwrap(),
            url::Url::parse("file:///home/alice/other.rs").unwrap(),
        ];
        match map_uris(&uris) {
            PortalOutcome::Opened(p) => assert_eq!(p, std::path::Path::new("/home/alice/main.rs")),
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
        // openable file.
        let uris = vec![url::Url::parse("https://example.com/remote.rs").unwrap()];
        assert!(matches!(map_uris(&uris), PortalOutcome::Failed(_)));
    }

    #[test]
    fn inject_delivers_the_outcome_immediately() {
        let rx = inject_for_test(PortalOutcome::Cancelled);
        assert!(matches!(rx.try_recv(), Ok(PortalOutcome::Cancelled)));
    }
}
