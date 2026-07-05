//! Host-native contact import/export via the **XDG Desktop Portal**
//! (`org.freedesktop.portal.FileChooser`, through the [`ashpd`] crate).
//!
//! Per the qOS HIG (§5 system integration) an app must **never draw its own
//! file picker and never key open/save off an environment variable**: it goes
//! through the portal so the *host desktop* provides the chooser — on GNOME the
//! user sees GNOME's file dialog; on qOS the compositor implements the portal
//! backend and renders qOS's picker (which is also where the per-cluster
//! sandbox / file-access prompt lives, so the portal is the security boundary).
//!
//! Two flows:
//!   * **Import** — [`request_import`] opens the chooser in *multi-select* mode
//!     filtered to contact formats (vCard `.vcf`/`.vcard`), so the user can
//!     pick arbitrarily many files at once; every selected local path comes
//!     back and the app parses + merges each into the address book.
//!   * **Export** — [`request_export`] opens the chooser in *Save* mode with a
//!     suggested file name; the single chosen destination comes back.
//!
//! The portal call is asynchronous and opens a real host window, so it must
//! **never** run on the UI thread (HIG §6). Each request spawns a worker thread
//! with its own small Tokio runtime, runs the D-Bus round-trip there, and hands
//! the outcome back over an [`std::sync::mpsc`] channel. The app drains that
//! channel once per frame (see `ContactsApp::drain_portal`); the UI thread
//! never blocks. Only *how files are chosen* changes — the pure-Rust vCard
//! parse/serialise in [`crate::vcard`] is untouched.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;

/// The result of a portal *import* (multi-select open) interaction, delivered
/// back to the UI thread over a channel.
#[derive(Debug)]
pub enum ImportOutcome {
    /// The user picked one or more local files at these paths (in the order
    /// the portal returned them).
    Selected(Vec<PathBuf>),
    /// The user dismissed the picker without choosing anything.
    Cancelled,
    /// The portal was unavailable or errored (no desktop portal running, a
    /// non-local URI, a D-Bus failure). Carries a human-readable reason.
    Failed(String),
}

/// The result of a portal *export* (save) interaction.
#[derive(Debug)]
pub enum ExportOutcome {
    /// The user chose this local destination path to write to.
    Chosen(PathBuf),
    /// The user dismissed the picker without choosing anything.
    Cancelled,
    /// The portal was unavailable or errored. Carries a human-readable reason.
    Failed(String),
}

/// A pending import request: the receiver the UI thread drains each frame.
/// `Some(rx)` means a picker is in flight; `None` means idle.
pub type ImportChannel = Receiver<ImportOutcome>;

/// A pending export request: the receiver the UI thread drains each frame.
pub type ExportChannel = Receiver<ExportOutcome>;

/// Spawn the portal multi-select import on a worker thread and return the
/// receiver the caller drains each frame. The UI thread is never blocked: the
/// D-Bus round-trip and the host dialog live entirely on the worker.
pub fn request_import() -> ImportChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("contacts-portal-import".into())
        .spawn(move || {
            let outcome = run_import_blocking();
            // The UI may have moved on (window closed); ignore a dead channel.
            let _ = tx.send(outcome);
        })
        .expect("spawn portal import worker thread");
    rx
}

/// Spawn the portal Save-dialog export on a worker thread, suggesting
/// `current_name` as the default file name. Returns the receiver the caller
/// drains each frame.
pub fn request_export(current_name: String) -> ExportChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("contacts-portal-export".into())
        .spawn(move || {
            let outcome = run_export_blocking(current_name);
            let _ = tx.send(outcome);
        })
        .expect("spawn portal export worker thread");
    rx
}

/// Run the portal `OpenFile` (multi-select) round-trip to completion on a
/// private Tokio runtime. Blocking — only ever called on the worker thread.
/// Split out so its result mapping is unit-testable via [`map_open_uris`].
fn run_import_blocking() -> ImportOutcome {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => return ImportOutcome::Failed(format!("portal runtime: {e}")),
    };
    rt.block_on(import_future())
}

/// Run the portal `SaveFile` round-trip on a private Tokio runtime. Blocking.
fn run_export_blocking(current_name: String) -> ExportOutcome {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => return ExportOutcome::Failed(format!("portal runtime: {e}")),
    };
    rt.block_on(export_future(current_name))
}

/// The async import interaction. Kept separate so the wiring reads cleanly.
async fn import_future() -> ImportOutcome {
    use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};

    // A vCard filter (the formats a contacts export arrives in), plus an
    // "all files" escape hatch so the user is never trapped if a card has an
    // unusual MIME type. `multiple(true)` lets the user pick arbitrarily many
    // files at once — every one is parsed and merged.
    let request = SelectedFiles::open_file()
        .title("Import contacts")
        .accept_label("Import")
        .modal(true)
        .multiple(true)
        .filter(
            FileFilter::new("vCard")
                .mimetype("text/vcard")
                .mimetype("text/x-vcard")
                .mimetype("text/directory")
                .glob("*.vcf")
                .glob("*.vcard"),
        )
        .filter(FileFilter::new("All files").glob("*"));

    match request.send().await.and_then(|r| r.response()) {
        Ok(files) => map_open_uris(files.uris()),
        // A user dismissal surfaces as a "cancelled" response error, not a bug.
        Err(ashpd::Error::Response(_)) => ImportOutcome::Cancelled,
        Err(e) => ImportOutcome::Failed(e.to_string()),
    }
}

/// The async export (Save) interaction.
async fn export_future(current_name: String) -> ExportOutcome {
    use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};

    let request = SelectedFiles::save_file()
        .title("Export contacts")
        .accept_label("Export")
        .modal(true)
        .current_name(current_name.as_str())
        .filter(
            FileFilter::new("vCard")
                .mimetype("text/vcard")
                .glob("*.vcf")
                .glob("*.vcard"),
        );

    match request.send().await.and_then(|r| r.response()) {
        Ok(files) => map_save_uri(files.uris()),
        Err(ashpd::Error::Response(_)) => ExportOutcome::Cancelled,
        Err(e) => ExportOutcome::Failed(e.to_string()),
    }
}

/// Map the portal's returned URIs (multi-select open) to an import outcome:
/// every local `file://` path is collected, an empty list is a cancel, and a
/// selection with *only* non-local URIs is a failure. Pure, so the
/// channel/threading contract can be tested without a portal.
pub fn map_open_uris(uris: &[url::Url]) -> ImportOutcome {
    if uris.is_empty() {
        return ImportOutcome::Cancelled;
    }
    let mut paths = Vec::new();
    for uri in uris {
        match uri.to_file_path() {
            Ok(p) => paths.push(p),
            // Skip a remote entry but keep the local ones — the user may have
            // multi-selected a mix.
            Err(()) => continue,
        }
    }
    if paths.is_empty() {
        return ImportOutcome::Failed("no local files in selection".to_string());
    }
    ImportOutcome::Selected(paths)
}

/// Map the portal's returned URIs (Save) to an export outcome: the first local
/// `file://` path wins, an empty list is a cancel, a non-local URI a failure.
pub fn map_save_uri(uris: &[url::Url]) -> ExportOutcome {
    let Some(uri) = uris.first() else {
        return ExportOutcome::Cancelled;
    };
    match uri.to_file_path() {
        Ok(path) => ExportOutcome::Chosen(path),
        Err(()) => ExportOutcome::Failed(format!("not a local file: {uri}")),
    }
}

/// Test-only: build a channel and immediately deliver `outcome` on it, so a
/// headless test can exercise the exact per-frame drain path the real portal
/// worker feeds — without opening a host dialog. Returns the receiver the app
/// installs, mirroring [`request_import`]'s contract.
#[cfg(test)]
pub fn inject_import_for_test(outcome: ImportOutcome) -> ImportChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(outcome).expect("seed injected import outcome");
    rx
}

/// Test-only counterpart of [`inject_import_for_test`] for the Save flow.
#[cfg(test)]
pub fn inject_export_for_test(outcome: ExportOutcome) -> ExportChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(outcome).expect("seed injected export outcome");
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_uris_collects_every_local_file() {
        let uris = vec![
            url::Url::parse("file:///home/alice/work.vcf").unwrap(),
            url::Url::parse("file:///home/alice/family.vcard").unwrap(),
        ];
        match map_open_uris(&uris) {
            ImportOutcome::Selected(p) => {
                assert_eq!(p.len(), 2, "both selected files are imported");
                assert_eq!(p[0], std::path::Path::new("/home/alice/work.vcf"));
                assert_eq!(p[1], std::path::Path::new("/home/alice/family.vcard"));
            }
            other => panic!("expected Selected, got {other:?}"),
        }
    }

    #[test]
    fn open_uris_empty_is_cancel() {
        assert!(matches!(map_open_uris(&[]), ImportOutcome::Cancelled));
    }

    #[test]
    fn open_uris_skips_remote_but_keeps_local() {
        let uris = vec![
            url::Url::parse("https://example.com/remote.vcf").unwrap(),
            url::Url::parse("file:///home/alice/local.vcf").unwrap(),
        ];
        match map_open_uris(&uris) {
            ImportOutcome::Selected(p) => {
                assert_eq!(p, vec![std::path::PathBuf::from("/home/alice/local.vcf")]);
            }
            other => panic!("expected Selected with the local file, got {other:?}"),
        }
    }

    #[test]
    fn open_uris_all_remote_is_failure() {
        let uris = vec![url::Url::parse("https://example.com/remote.vcf").unwrap()];
        assert!(matches!(map_open_uris(&uris), ImportOutcome::Failed(_)));
    }

    #[test]
    fn save_uri_takes_first_local_file() {
        let uris = vec![url::Url::parse("file:///home/alice/export.vcf").unwrap()];
        match map_save_uri(&uris) {
            ExportOutcome::Chosen(p) => {
                assert_eq!(p, std::path::Path::new("/home/alice/export.vcf"))
            }
            other => panic!("expected Chosen, got {other:?}"),
        }
    }

    #[test]
    fn save_uri_empty_is_cancel() {
        assert!(matches!(map_save_uri(&[]), ExportOutcome::Cancelled));
    }

    #[test]
    fn save_uri_non_local_is_failure() {
        let uris = vec![url::Url::parse("https://example.com/remote.vcf").unwrap()];
        assert!(matches!(map_save_uri(&uris), ExportOutcome::Failed(_)));
    }

    #[test]
    fn inject_delivers_the_outcome_immediately() {
        let rx = inject_import_for_test(ImportOutcome::Cancelled);
        assert!(matches!(rx.try_recv(), Ok(ImportOutcome::Cancelled)));
        let rx = inject_export_for_test(ExportOutcome::Cancelled);
        assert!(matches!(rx.try_recv(), Ok(ExportOutcome::Cancelled)));
    }
}
