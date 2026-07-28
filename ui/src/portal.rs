//! Host-native file open/save via the **XDG Desktop Portal**
//! (`org.freedesktop.portal.FileChooser`, through `ashpd`) wrapped in the
//! spawn-a-worker → drain-once-per-frame pattern every qOS app needs.
//!
//! # Why this lives in `ui`
//!
//! Per the qOS HIG §5 an app must **never draw its own file picker**: choosing a
//! file goes through the portal so the *host desktop* provides the chooser — on
//! GNOME the user sees GNOME's dialog; on qOS the compositor implements the
//! portal backend and renders qOS's picker (which is also where the per-workspace
//! sandbox / file-access prompt lives, so the portal is the security boundary).
//!
//! The portal call is asynchronous and opens a real host window, so it must
//! **never** run on the UI thread (HIG §6). Historically every app re-wrote the
//! same ~160 lines: an outcome enum, a worker thread with its own Tokio runtime,
//! an [`std::sync::mpsc`] channel the app drains each frame, a pure URI→path
//! mapping, and a test injector. That is a recurring pattern, so — per the HIG's
//! "promote a `ui` primitive rather than reimplement (and re-violate) per app" —
//! it is unified here.
//!
//! # Usage
//!
//! ```ignore
//! // Kick off a picker (returns immediately; the dialog runs on a worker):
//! let rx = ui::portal::FileDialog::open("Import OPML")
//!     .accept_label("Import")
//!     .filter(ui::portal::Filter::new("OPML").mimetype("text/x-opml").glob("*.opml"))
//!     .spawn();
//! self.import_rx = Some(rx);
//!
//! // Once per frame, drain it:
//! if let Some(rx) = &self.import_rx {
//!     match rx.try_recv() {
//!         Ok(ui::portal::PortalOutcome::Opened(path)) => { /* read the file */ }
//!         Ok(ui::portal::PortalOutcome::Cancelled) => {}
//!         Ok(ui::portal::PortalOutcome::Failed(reason)) => { /* toast */ }
//!         Ok(ui::portal::PortalOutcome::OpenedMany(paths)) => { /* multi-select */ }
//!         Err(std::sync::mpsc::TryRecvError::Empty) => {}       // still in flight
//!         Err(std::sync::mpsc::TryRecvError::Disconnected) => { self.import_rx = None; }
//!     }
//! }
//! ```
//!
//! The actual portal round-trip (the `ashpd` D-Bus dependency) is compiled only
//! when the crate's **`portal`** feature is enabled — an app opts in with
//! `ui = { path = "…", features = ["portal"] }`. The pure parts ([`PortalOutcome`],
//! the [`FileDialog`] builder, [`map_uris`], [`inject_for_test`]) always compile,
//! so headless tests exercise the exact per-frame drain path without a portal —
//! and a build without the feature degrades to a clean [`PortalOutcome::Failed`]
//! rather than failing to link.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;

/// The result of a portal file interaction, delivered back to the UI thread
/// over a [`PortalChannel`].
///
/// A single-select open **and** a save both report [`PortalOutcome::Opened`]
/// (the one chosen / save-target path); a multi-select open reports
/// [`PortalOutcome::OpenedMany`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortalOutcome {
    /// The user picked a single local file, or chose a save target, at this path.
    Opened(PathBuf),
    /// The user multi-selected several local files (multi-select open only).
    OpenedMany(Vec<PathBuf>),
    /// The user dismissed the picker without choosing anything.
    Cancelled,
    /// The portal was unavailable or errored (no desktop portal running, a
    /// non-local URI, a D-Bus failure, or `ui` built without the `portal`
    /// feature). Carries a human-readable reason.
    Failed(String),
}

impl PortalOutcome {
    /// The first chosen path, if the outcome carries one. Convenient for
    /// single-select call sites that also want to accept the head of a
    /// multi-select result.
    pub fn first_path(&self) -> Option<&Path> {
        match self {
            PortalOutcome::Opened(p) => Some(p.as_path()),
            PortalOutcome::OpenedMany(v) => v.first().map(PathBuf::as_path),
            _ => None,
        }
    }

    /// Whether the user actually chose one or more files.
    pub fn is_success(&self) -> bool {
        matches!(self, PortalOutcome::Opened(_) | PortalOutcome::OpenedMany(_))
    }
}

/// A pending portal request: the receiver the UI thread drains each frame.
/// `Some(rx)` means a picker is in flight; `None` means idle.
pub type PortalChannel = Receiver<PortalOutcome>;

/// One file-type filter offered in the host picker. Names a category (e.g.
/// "OPML") plus the globs and/or MIME types that belong to it. The host shows
/// these as the picker's file-type dropdown.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    /// Human-readable category name shown in the picker's type dropdown.
    pub name: String,
    /// Shell globs (e.g. `*.opml`).
    pub globs: Vec<String>,
    /// MIME types (e.g. `text/x-opml`).
    pub mimetypes: Vec<String>,
}

impl Filter {
    /// Start a filter named `name` (the label shown in the type dropdown).
    pub fn new(name: impl Into<String>) -> Self {
        Filter {
            name: name.into(),
            globs: Vec::new(),
            mimetypes: Vec::new(),
        }
    }

    /// Add a shell glob (e.g. `*.png`). Chainable.
    pub fn glob(mut self, glob: impl Into<String>) -> Self {
        self.globs.push(glob.into());
        self
    }

    /// Add a MIME type (e.g. `image/png`). Chainable.
    pub fn mimetype(mut self, mimetype: impl Into<String>) -> Self {
        self.mimetypes.push(mimetype.into());
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Open,
    Save,
}

/// Builder for a portal file dialog. Configure it fluently, then [`spawn`] to
/// run the dialog on a worker thread and get back the [`PortalChannel`] the UI
/// drains once per frame.
///
/// [`spawn`]: FileDialog::spawn
#[derive(Debug, Clone)]
pub struct FileDialog {
    kind: Kind,
    title: String,
    accept_label: Option<String>,
    filters: Vec<Filter>,
    multiple: bool,
    current_name: Option<String>,
    thread_name: String,
    all_files_escape: bool,
}

impl FileDialog {
    fn base(kind: Kind, title: impl Into<String>) -> Self {
        FileDialog {
            kind,
            title: title.into(),
            accept_label: None,
            filters: Vec::new(),
            multiple: false,
            current_name: None,
            thread_name: "ui-portal-worker".to_string(),
            // By default offer an "All files" escape hatch so a file with an
            // unusual MIME type can never trap the user behind the filters.
            all_files_escape: true,
        }
    }

    /// An **Open** dialog with the given window title.
    pub fn open(title: impl Into<String>) -> Self {
        Self::base(Kind::Open, title)
    }

    /// A **Save** dialog with the given window title. Pair with
    /// [`suggested_name`](FileDialog::suggested_name) for a default file name.
    pub fn save(title: impl Into<String>) -> Self {
        Self::base(Kind::Save, title)
    }

    /// Override the confirm-button label (e.g. "Import", "Export"). Defaults to
    /// the host's own ("Open" / "Save").
    pub fn accept_label(mut self, label: impl Into<String>) -> Self {
        self.accept_label = Some(label.into());
        self
    }

    /// Add a file-type [`Filter`]. Chainable; filters appear in the picker's
    /// type dropdown in the order added.
    pub fn filter(mut self, filter: Filter) -> Self {
        self.filters.push(filter);
        self
    }

    /// Allow selecting multiple files (Open only). A multi-select success is
    /// reported as [`PortalOutcome::OpenedMany`].
    pub fn multiple(mut self, multiple: bool) -> Self {
        self.multiple = multiple;
        self
    }

    /// The default file name to pre-fill in a **Save** dialog. Ignored for Open.
    pub fn suggested_name(mut self, name: impl Into<String>) -> Self {
        self.current_name = Some(name.into());
        self
    }

    /// Name the worker thread (aids debugging). Defaults to `ui-portal-worker`.
    pub fn thread_name(mut self, name: impl Into<String>) -> Self {
        self.thread_name = name.into();
        self
    }

    /// Whether to append an automatic "All files" (`*`) escape-hatch filter so
    /// the user is never trapped behind the declared filters. Default `true`.
    pub fn all_files_escape(mut self, yes: bool) -> Self {
        self.all_files_escape = yes;
        self
    }

    /// Run the dialog on a worker thread and return the [`PortalChannel`] the
    /// caller drains each frame. The UI thread is never blocked: the D-Bus
    /// round-trip and the host dialog live entirely on the worker.
    ///
    /// If the worker thread cannot even be spawned (effectively never), the
    /// returned channel immediately yields a [`PortalOutcome::Failed`] so the
    /// app degrades gracefully instead of waiting forever.
    pub fn spawn(self) -> PortalChannel {
        let (tx, rx) = std::sync::mpsc::channel();
        let thread_name = self.thread_name.clone();
        let spawned = std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                // The UI may have moved on (window closed); ignore a dead channel.
                let _ = tx.send(self.run_blocking());
            });
        if let Err(e) = spawned {
            // `tx` was moved into the (failed) closure and dropped; make a fresh
            // channel that reports the failure so the app doesn't hang.
            let (tx2, rx2) = std::sync::mpsc::channel();
            let _ = tx2.send(PortalOutcome::Failed(format!("spawn portal worker: {e}")));
            return rx2;
        }
        rx
    }

    /// Run the portal round-trip to completion on a private current-thread Tokio
    /// runtime. Blocking — only ever called on the worker thread from [`spawn`].
    ///
    /// [`spawn`]: FileDialog::spawn
    #[cfg(feature = "portal")]
    fn run_blocking(self) -> PortalOutcome {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => return PortalOutcome::Failed(format!("portal runtime: {e}")),
        };
        rt.block_on(self.run_future())
    }

    /// Without the `portal` feature there is no D-Bus dependency compiled in, so
    /// a request degrades to a clean failure the app can toast — the channel /
    /// drain plumbing (and its tests) still work.
    #[cfg(not(feature = "portal"))]
    fn run_blocking(self) -> PortalOutcome {
        PortalOutcome::Failed(
            "portal support not compiled into ui (enable the `portal` feature)".to_string(),
        )
    }

    #[cfg(feature = "portal")]
    async fn run_future(self) -> PortalOutcome {
        use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};

        let ashpd_filters: Vec<FileFilter> = self
            .filters
            .iter()
            .map(|f| {
                let mut ff = FileFilter::new(&f.name);
                for m in &f.mimetypes {
                    ff = ff.mimetype(m);
                }
                for g in &f.globs {
                    ff = ff.glob(g);
                }
                ff
            })
            .chain(
                self.all_files_escape
                    .then(|| FileFilter::new("All files").glob("*")),
            )
            .collect();

        // `open_file()` and `save_file()` are distinct request builders in
        // ashpd (only open supports `.multiple`), so the two arms diverge.
        let result = match self.kind {
            Kind::Open => {
                let mut request = SelectedFiles::open_file()
                    .title(self.title.as_str())
                    .modal(true)
                    .multiple(self.multiple);
                if let Some(label) = &self.accept_label {
                    request = request.accept_label(label.as_str());
                }
                for ff in ashpd_filters {
                    request = request.filter(ff);
                }
                request.send().await.and_then(|r| r.response())
            }
            Kind::Save => {
                let mut request = SelectedFiles::save_file()
                    .title(self.title.as_str())
                    .modal(true);
                if let Some(label) = &self.accept_label {
                    request = request.accept_label(label.as_str());
                }
                if let Some(name) = &self.current_name {
                    request = request.current_name(name.as_str());
                }
                for ff in ashpd_filters {
                    request = request.filter(ff);
                }
                request.send().await.and_then(|r| r.response())
            }
        };

        match result {
            Ok(files) => map_uris(files.uris(), self.multiple),
            // A user dismissal surfaces as a "cancelled" response error, not a bug.
            Err(ashpd::Error::Response(_)) => PortalOutcome::Cancelled,
            Err(e) => PortalOutcome::Failed(e.to_string()),
        }
    }
}

/// Map the portal's returned URIs to a [`PortalOutcome`]. Pure, so the
/// channel/threading contract can be tested without a live portal.
///
/// - `multiple == false` (single-select open / save): the first local `file://`
///   path wins → [`PortalOutcome::Opened`]; an empty list is a
///   [`PortalOutcome::Cancelled`]; a non-local first URI is a
///   [`PortalOutcome::Failed`].
/// - `multiple == true` (multi-select open): every local `file://` path is
///   collected → [`PortalOutcome::OpenedMany`]; an empty list is a
///   [`PortalOutcome::Cancelled`]; a selection with *only* non-local URIs is a
///   [`PortalOutcome::Failed`] (a mix keeps the local ones).
pub fn map_uris(uris: &[url::Url], multiple: bool) -> PortalOutcome {
    if multiple {
        if uris.is_empty() {
            return PortalOutcome::Cancelled;
        }
        let paths: Vec<PathBuf> = uris.iter().filter_map(|u| u.to_file_path().ok()).collect();
        if paths.is_empty() {
            return PortalOutcome::Failed("no local files in selection".to_string());
        }
        PortalOutcome::OpenedMany(paths)
    } else {
        let Some(uri) = uris.first() else {
            return PortalOutcome::Cancelled;
        };
        match uri.to_file_path() {
            Ok(path) => PortalOutcome::Opened(path),
            Err(()) => PortalOutcome::Failed(format!("not a local file: {uri}")),
        }
    }
}

/// Build a channel and immediately deliver `outcome` on it, so a headless test
/// can exercise the exact per-frame drain path the real portal worker feeds —
/// without opening a host dialog. Returns the receiver an app would install,
/// mirroring [`FileDialog::spawn`]'s contract.
///
/// Shipped in the library (not `#[cfg(test)]`) so downstream apps' own test
/// suites can drive their drain logic against it.
pub fn inject_for_test(outcome: PortalOutcome) -> PortalChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(outcome).expect("seed injected portal outcome");
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> url::Url {
        url::Url::parse(s).expect("parse test url")
    }

    #[test]
    fn single_takes_first_local_file() {
        let uris = [
            url("file:///home/alice/subscriptions.opml"),
            url("file:///home/alice/other.opml"),
        ];
        assert_eq!(
            map_uris(&uris, false),
            PortalOutcome::Opened(PathBuf::from("/home/alice/subscriptions.opml"))
        );
    }

    #[test]
    fn single_empty_is_cancel() {
        assert_eq!(map_uris(&[], false), PortalOutcome::Cancelled);
    }

    #[test]
    fn single_non_local_is_failure() {
        // A remote URI has no local path, so it must not be treated as openable.
        let uris = [url("https://example.com/feeds.opml")];
        assert!(matches!(map_uris(&uris, false), PortalOutcome::Failed(_)));
    }

    #[test]
    fn multiple_collects_local_files() {
        let uris = [
            url("file:///home/alice/a.vcf"),
            url("file:///home/alice/b.vcf"),
        ];
        assert_eq!(
            map_uris(&uris, true),
            PortalOutcome::OpenedMany(vec![
                PathBuf::from("/home/alice/a.vcf"),
                PathBuf::from("/home/alice/b.vcf"),
            ])
        );
    }

    #[test]
    fn multiple_keeps_local_and_skips_remote() {
        let uris = [
            url("https://example.com/remote.vcf"),
            url("file:///home/alice/local.vcf"),
        ];
        assert_eq!(
            map_uris(&uris, true),
            PortalOutcome::OpenedMany(vec![PathBuf::from("/home/alice/local.vcf")])
        );
    }

    #[test]
    fn multiple_empty_is_cancel() {
        assert_eq!(map_uris(&[], true), PortalOutcome::Cancelled);
    }

    #[test]
    fn multiple_only_remote_is_failure() {
        let uris = [url("https://example.com/remote.vcf")];
        assert!(matches!(map_uris(&uris, true), PortalOutcome::Failed(_)));
    }

    #[test]
    fn builder_carries_configuration() {
        let dialog = FileDialog::open("Import OPML")
            .accept_label("Import")
            .filter(Filter::new("OPML").mimetype("text/x-opml").glob("*.opml"))
            .thread_name("feeds-portal");
        assert_eq!(dialog.kind, Kind::Open);
        assert_eq!(dialog.title, "Import OPML");
        assert_eq!(dialog.accept_label.as_deref(), Some("Import"));
        assert_eq!(dialog.filters.len(), 1);
        assert_eq!(dialog.filters[0].name, "OPML");
        assert_eq!(dialog.filters[0].globs, ["*.opml"]);
        assert_eq!(dialog.filters[0].mimetypes, ["text/x-opml"]);
        assert!(dialog.all_files_escape);
        assert!(!dialog.multiple);
    }

    #[test]
    fn save_builder_carries_suggested_name() {
        let dialog = FileDialog::save("Export contacts")
            .accept_label("Export")
            .suggested_name("contacts.vcf");
        assert_eq!(dialog.kind, Kind::Save);
        assert_eq!(dialog.current_name.as_deref(), Some("contacts.vcf"));
    }

    #[test]
    fn inject_delivers_the_outcome_immediately() {
        let rx = inject_for_test(PortalOutcome::Cancelled);
        assert_eq!(rx.try_recv(), Ok(PortalOutcome::Cancelled));
    }

    #[test]
    fn spawn_without_portal_feature_reports_failure() {
        // Without the `portal` feature the worker still runs and delivers a
        // clean Failed the app can drain — the plumbing is exercised either way.
        #[cfg(not(feature = "portal"))]
        {
            let rx = FileDialog::open("x").spawn();
            let outcome = rx.recv().expect("worker delivers an outcome");
            assert!(matches!(outcome, PortalOutcome::Failed(_)));
        }
    }

    #[test]
    fn first_path_and_success_helpers() {
        assert_eq!(
            PortalOutcome::Opened(PathBuf::from("/a")).first_path(),
            Some(Path::new("/a"))
        );
        assert_eq!(
            PortalOutcome::OpenedMany(vec![PathBuf::from("/a"), PathBuf::from("/b")]).first_path(),
            Some(Path::new("/a"))
        );
        assert_eq!(PortalOutcome::Cancelled.first_path(), None);
        assert!(PortalOutcome::Opened(PathBuf::from("/a")).is_success());
        assert!(!PortalOutcome::Cancelled.is_success());
        assert!(!PortalOutcome::Failed("x".into()).is_success());
    }
}
