//! Host-native tunnel import via the **XDG Desktop Portal**
//! (`org.freedesktop.portal.FileChooser`, through the [`ashpd`] crate).
//!
//! Per the qOS HIG an app must **never draw its own file picker and never key
//! open/save off an environment variable**: choosing the `.conf` / `.ovpn` to
//! import goes through the portal so the *host desktop* provides the chooser —
//! on GNOME the user sees GNOME's file dialog; on qOS the compositor implements
//! the portal backend and renders qOS's picker (which is also where the
//! per-cluster sandbox / file-access prompt lives, so the portal is the
//! security boundary).
//!
//! The portal call is asynchronous and opens a real host window, so it must
//! **never** run on the UI thread. [`request_import`] spawns a worker thread
//! with its own small Tokio runtime, runs the D-Bus round-trip there, and
//! hands the outcome back over an [`std::sync::mpsc`] channel. The app drains
//! that channel once per frame; the UI thread never blocks. Only *how the file
//! is chosen* changes — the pure-Rust `.conf` / `.ovpn` parse in
//! [`crate::config`] is untouched.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use crate::model::ConnKind;

/// The result of a portal import (file-open) interaction, delivered back to
/// the UI thread over a channel.
#[derive(Debug)]
pub enum ImportOutcome {
    /// The user picked a local config file at this path.
    Opened(PathBuf),
    /// The user dismissed the picker without choosing anything.
    Cancelled,
    /// The portal was unavailable or errored (no desktop portal running, a
    /// non-local URI, a D-Bus failure). Carries a human-readable reason.
    Failed(String),
}

/// A pending import request: the receiver the UI thread drains each frame.
/// `Some(rx)` means a picker is in flight; `None` means idle.
pub type ImportChannel = Receiver<ImportOutcome>;

/// Spawn the portal `OpenFile` on a worker thread and return the receiver the
/// caller drains each frame. `kind` seeds the filter shown to the user
/// (`WireGuard` `.conf` vs `OpenVPN` `.ovpn`). The UI thread is never blocked: the
/// D-Bus round-trip and the host dialog live entirely on the worker.
pub fn request_import(kind: ConnKind) -> ImportChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("vpn-portal-import".into())
        .spawn(move || {
            let outcome = run_import_blocking(kind);
            // The UI may have moved on (window closed); ignore a dead channel.
            let _ = tx.send(outcome);
        })
        .expect("spawn portal import worker thread");
    rx
}

/// Run the portal `OpenFile` round-trip to completion on a private Tokio
/// runtime. Blocking — only ever called on the worker thread.
fn run_import_blocking(kind: ConnKind) -> ImportOutcome {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => return ImportOutcome::Failed(format!("portal runtime: {e}")),
    };
    rt.block_on(import_future(kind))
}

/// The async portal interaction. Kept separate so the wiring reads cleanly.
async fn import_future(kind: ConnKind) -> ImportOutcome {
    use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};

    // A protocol-specific filter, plus an "all files" escape hatch so the
    // user is never trapped if a config has an unusual MIME type.
    let (title, filter) = match kind {
        ConnKind::WireGuard => (
            "Import WireGuard tunnel",
            FileFilter::new("WireGuard (.conf)").glob("*.conf"),
        ),
        ConnKind::OpenVpn => (
            "Import OpenVPN tunnel",
            FileFilter::new("OpenVPN (.ovpn)").glob("*.ovpn"),
        ),
    };
    let request = SelectedFiles::open_file()
        .title(title)
        .accept_label("Import")
        .modal(true)
        .multiple(false)
        .filter(filter)
        .filter(FileFilter::new("All files").glob("*"));

    match request.send().await.and_then(|r| r.response()) {
        Ok(files) => map_uris(files.uris()),
        // A user dismissal surfaces as a "cancelled" response error, not a bug.
        Err(ashpd::Error::Response(_)) => ImportOutcome::Cancelled,
        Err(e) => ImportOutcome::Failed(e.to_string()),
    }
}

/// Map the portal's returned URIs to an outcome: the first local `file://`
/// path wins, an empty list is a cancel, and a non-local URI is a failure.
/// Pure, so the channel/threading contract can be tested without a portal.
pub fn map_uris(uris: &[url::Url]) -> ImportOutcome {
    let Some(uri) = uris.first() else {
        return ImportOutcome::Cancelled;
    };
    match uri.to_file_path() {
        Ok(path) => ImportOutcome::Opened(path),
        Err(()) => ImportOutcome::Failed(format!("not a local file: {uri}")),
    }
}

/// Test-only: build a channel and immediately deliver `outcome` on it, so a
/// headless test can exercise the exact per-frame drain path the real portal
/// worker feeds — without opening a host dialog.
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
    fn map_uris_takes_first_local_file() {
        let uris = vec![
            url::Url::parse("file:///home/alice/home.conf").unwrap(),
            url::Url::parse("file:///home/alice/work.conf").unwrap(),
        ];
        match map_uris(&uris) {
            ImportOutcome::Opened(p) => {
                assert_eq!(p, std::path::Path::new("/home/alice/home.conf"));
            }
            other => panic!("expected Opened, got {other:?}"),
        }
    }

    #[test]
    fn map_uris_empty_is_cancel() {
        assert!(matches!(map_uris(&[]), ImportOutcome::Cancelled));
    }

    #[test]
    fn map_uris_non_local_is_failure() {
        let uris = vec![url::Url::parse("https://example.com/home.conf").unwrap()];
        assert!(matches!(map_uris(&uris), ImportOutcome::Failed(_)));
    }

    #[test]
    fn inject_delivers_the_outcome_immediately() {
        let rx = inject_for_test(ImportOutcome::Cancelled);
        assert!(matches!(rx.try_recv(), Ok(ImportOutcome::Cancelled)));
    }
}
