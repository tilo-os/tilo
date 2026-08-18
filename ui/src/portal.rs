//! Host-native file choosing via the **XDG Desktop Portal**
//! (`org.freedesktop.portal.FileChooser`, through [`ashpd`]).
//!
//! # Why this is shared
//!
//! Per the qOS HIG an app must **never draw its own file picker and never key
//! open/save off an environment variable**: it goes through the portal so the
//! *host desktop* provides the chooser — on GNOME the user sees GNOME's file
//! dialog; on qOS the compositor implements the portal backend and renders
//! qOS's picker. That picker is also where the per-workspace sandbox
//! file-access prompt lives, which makes this module **the file-access
//! security boundary**. A per-app copy of it is not merely duplicated code: it
//! is a second, independently-drifting definition of which URIs are allowed to
//! become paths the app then reads or writes.
//!
//! One such drift was live. A copy mapped URIs with
//! `uri.strip_prefix("file://")` instead of [`url::Url::to_file_path`], which
//! turns `file://evil-host/etc/passwd` into the **relative** path
//! `evil-host/etc/passwd` — silently resolved against the process's working
//! directory, from a URI that explicitly named a *remote host*. It also
//! hand-rolled percent-decoding, so escaped filenames round-tripped
//! differently from the other copies. This module converges on
//! `Url::to_file_path`, which rejects a foreign host outright and does the
//! decoding per path segment; see `rejects_a_uri_that_names_a_remote_host`.
//!
//! # Threading contract
//!
//! The portal round-trip is asynchronous and opens a real host window, so it
//! must **never** run on the UI thread. [`PickRequest::send`] spawns a worker
//! thread with its own small Tokio runtime, runs the D-Bus round-trip there,
//! and hands the outcome back over an [`std::sync::mpsc`] channel. The app
//! drains that channel once per frame; the UI thread never blocks.
//!
//! # Feature gating
//!
//! The `ashpd`/`tokio` machinery is behind the `portal` cargo feature so apps
//! that never open a chooser (and hermetic `--shots` builds) do not pay for
//! D-Bus. The pure URI→path mapping ([`map_uris`], [`map_uri`]), the channel
//! types, and [`inject_for_test`] compile unconditionally, so the security
//! rule stays under test in every configuration and `--open PATH` style flags
//! work without a portal.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;

/// The result of a **single-file** portal interaction (open, or a Save
/// destination), delivered back to the UI thread over a channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortalOutcome {
    /// The user picked this local path. A `/var/workspace/...` pick resolves
    /// through the FUSE mount and hydrates on read exactly like a local file.
    Opened(PathBuf),
    /// The user dismissed the picker without choosing anything.
    Cancelled,
    /// The portal was unavailable or errored (no desktop portal running, a
    /// non-local URI, a D-Bus failure). Carries a human-readable reason.
    Failed(String),
}

/// The result of a **multi-select** portal open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MultiOutcome {
    /// The user picked one or more local paths, in the order the portal
    /// returned them. Never empty.
    Selected(Vec<PathBuf>),
    /// The user dismissed the picker without choosing anything.
    Cancelled,
    /// The portal was unavailable or errored. Carries a human-readable reason.
    Failed(String),
}

/// A pending single-file request: the receiver the UI thread drains each
/// frame. `Some(rx)` means a picker is in flight; `None` means idle.
pub type PortalChannel = Receiver<PortalOutcome>;

/// A pending multi-select request: the receiver the UI thread drains each
/// frame.
pub type MultiChannel = Receiver<MultiOutcome>;

/// One entry in the chooser's file-type dropdown.
///
/// Globs and MIME types are both accepted because the portal backends differ
/// in which they honour; supplying both is the portable choice.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileFilter {
    name: String,
    globs: Vec<String>,
    mimetypes: Vec<String>,
}

impl FileFilter {
    /// A filter labelled `name` in the chooser's type dropdown.
    pub fn new(name: &str) -> Self {
        Self { name: name.to_string(), ..Default::default() }
    }

    /// Match files by shell glob, e.g. `*.png`.
    pub fn glob(mut self, g: &str) -> Self {
        self.globs.push(g.to_string());
        self
    }

    /// Match files by MIME type, e.g. `application/pdf`.
    pub fn mimetype(mut self, m: &str) -> Self {
        self.mimetypes.push(m.to_string());
        self
    }

    /// The "All files" escape hatch every picker should offer, so the user is
    /// never trapped by a file with an unusual MIME type.
    pub fn all_files() -> Self {
        Self::new("All files").glob("*")
    }

    /// The filter's dropdown label.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Which chooser to open.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PickKind {
    /// A file (or, when `directory`, a folder) open chooser. `directory` asks
    /// the host picker to select directories rather than files — the portal's
    /// `OpenFile(directory: true)` mode — so "Open a project folder" grays out
    /// files instead of hoping the user picks a directory in a file chooser.
    Open { multiple: bool, directory: bool },
    Save { suggested_name: String },
}

/// A portal file-chooser request, built up and then [`send`](PickRequest::send)
/// on a worker thread.
///
/// The per-app delta between what used to be nine hand-written copies of this
/// module is exactly what this struct captures: a title, an accept label, a
/// filter set, and a worker-thread name.
#[derive(Debug, Clone)]
pub struct PickRequest {
    title: String,
    accept_label: String,
    thread_name: String,
    filters: Vec<FileFilter>,
    kind: PickKind,
}

impl PickRequest {
    /// A single-file open chooser.
    pub fn open(title: &str, accept_label: &str) -> Self {
        Self::new(
            title,
            accept_label,
            PickKind::Open { multiple: false, directory: false },
        )
    }

    /// A multi-select open chooser: the user may pick arbitrarily many files
    /// at once and every local one comes back.
    pub fn open_multiple(title: &str, accept_label: &str) -> Self {
        Self::new(
            title,
            accept_label,
            PickKind::Open { multiple: true, directory: false },
        )
    }

    /// A directory open chooser: the host picker selects a **folder** rather
    /// than a file (files are shown but not selectable). The returned
    /// [`PortalOutcome::Opened`] path is the chosen directory. This is how an
    /// app opens a project/workspace root through the portal — the sandbox's
    /// file-access grant is made for the directory the user actually picked.
    pub fn open_directory(title: &str, accept_label: &str) -> Self {
        Self::new(
            title,
            accept_label,
            PickKind::Open { multiple: false, directory: true },
        )
    }

    /// A Save chooser, seeding the filename field with `suggested_name`.
    pub fn save(title: &str, accept_label: &str, suggested_name: &str) -> Self {
        Self::new(
            title,
            accept_label,
            PickKind::Save { suggested_name: suggested_name.to_string() },
        )
    }

    fn new(title: &str, accept_label: &str, kind: PickKind) -> Self {
        Self {
            title: title.to_string(),
            accept_label: accept_label.to_string(),
            thread_name: "qos-portal".to_string(),
            filters: Vec::new(),
            kind,
        }
    }

    /// Name the worker thread, so a stuck picker is identifiable in a
    /// backtrace or `top -H`.
    pub fn thread_name(mut self, name: &str) -> Self {
        self.thread_name = name.to_string();
        self
    }

    /// Add a file-type filter. Order is the dropdown order; the first is the
    /// default.
    pub fn filter(mut self, f: FileFilter) -> Self {
        self.filters.push(f);
        self
    }

    /// Append the "All files" escape hatch.
    pub fn all_files(self) -> Self {
        self.filter(FileFilter::all_files())
    }

    /// The chooser title, as the host dialog will show it.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Whether this request opens a multi-select chooser.
    pub fn is_multiple(&self) -> bool {
        matches!(self.kind, PickKind::Open { multiple: true, .. })
    }

    /// Whether this request opens a directory (folder) chooser rather than a
    /// file one. Lets an app assert on the chooser it configured without opening
    /// one.
    pub fn is_directory(&self) -> bool {
        matches!(self.kind, PickKind::Open { directory: true, .. })
    }

    /// The filter dropdown labels, in order. Lets an app assert on the chooser
    /// it configured without opening one.
    pub fn filter_names(&self) -> Vec<&str> {
        self.filters.iter().map(|f| f.name()).collect()
    }

    /// The filename this request seeds a Save dialog with, if it is one.
    pub fn suggested_name(&self) -> Option<&str> {
        match &self.kind {
            PickKind::Save { suggested_name } => Some(suggested_name.as_str()),
            PickKind::Open { .. } => None,
        }
    }

    /// Run the chooser on a worker thread and return the receiver the caller
    /// drains each frame. The UI thread is never blocked: the D-Bus round-trip
    /// and the host dialog live entirely on the worker.
    ///
    /// Use on a request built with [`open`](Self::open) or
    /// [`save`](Self::save); a multi-select request should go through
    /// [`send_multiple`](Self::send_multiple) so every pick is delivered.
    pub fn send(self) -> PortalChannel {
        let (tx, rx) = std::sync::mpsc::channel();
        let name = self.thread_name.clone();
        spawn_worker(&name, move || {
            let _ = tx.send(single(self));
        });
        rx
    }

    /// Multi-select counterpart of [`send`](Self::send).
    pub fn send_multiple(self) -> MultiChannel {
        let (tx, rx) = std::sync::mpsc::channel();
        let name = self.thread_name.clone();
        spawn_worker(&name, move || {
            let _ = tx.send(multiple(self));
        });
        rx
    }
}

/// Spawn the portal worker. The UI may have moved on (window closed) by the
/// time it finishes, so a dead channel is ignored by the closure itself.
fn spawn_worker(name: &str, work: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(work)
        .expect("spawn portal worker thread");
}

// ---------------------------------------------------------------------------
// URI → path mapping. Compiled unconditionally: this is the security rule, and
// it stays under test whether or not the `portal` feature is on.
// ---------------------------------------------------------------------------

/// Map one portal-returned URI to a local path, or `None` if it does not name
/// a file on this machine.
///
/// Builds on [`url::Url::to_file_path`], which is already much stricter than a
/// `strip_prefix("file://")`: it requires the `file` scheme, rejects a URI
/// naming any host other than the empty host or `localhost`, and requires an
/// absolute path.
///
/// It is not strict enough on its own, though. `to_file_path` percent-decodes
/// each segment and then joins, so a segment the portal presented as a single
/// **file name** can decode into several path components: `a%2Fb.txt` becomes
/// `a/b.txt`, and `%2E%2E` becomes a parent-directory hop. A chosen name would
/// then reach outside the directory the user actually browsed. So we also
/// require that the decoded path has exactly as many components as the URI had
/// segments, and that none of them is `..` or `.`.
pub fn to_local_path(uri: &url::Url) -> Option<PathBuf> {
    use std::path::Component;

    let path = uri.to_file_path().ok()?;

    // No parent/current-directory hops: the portal returns resolved picks, so
    // one appearing here came from an escape in a file name.
    if path
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return None;
    }

    // One URI segment must yield exactly one path component. An escaped
    // separator would make the decoded path longer than the URI it came from.
    let segments = uri
        .path_segments()?
        .filter(|s| !s.is_empty())
        .count();
    let components = path
        .components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .count();
    if components != segments {
        return None;
    }

    Some(path)
}

/// Map the portal's returned URIs for a **single-file** interaction: the first
/// local `file://` path wins, an empty list is a cancel, and a URI that is not
/// a local file is a failure.
///
/// Pure, so the channel/threading contract can be tested without a portal.
pub fn map_uri(uris: &[url::Url]) -> PortalOutcome {
    let Some(uri) = uris.first() else {
        return PortalOutcome::Cancelled;
    };
    match to_local_path(uri) {
        Some(path) => PortalOutcome::Opened(path),
        None => PortalOutcome::Failed(format!("not a local file: {uri}")),
    }
}

/// Map the portal's returned URIs for a **multi-select** open: every local
/// `file://` path is collected, an empty list is a cancel, and a selection
/// with no local file at all is a failure.
///
/// A mixed selection keeps the local entries and drops the rest — the user
/// asked for those files, and refusing all of them because one was remote
/// would be surprising.
pub fn map_uris(uris: &[url::Url]) -> MultiOutcome {
    if uris.is_empty() {
        return MultiOutcome::Cancelled;
    }
    let paths: Vec<PathBuf> = uris.iter().filter_map(to_local_path).collect();
    if paths.is_empty() {
        return MultiOutcome::Failed(match uris {
            [only] => format!("not a local file: {only}"),
            _ => "no local files in selection".to_string(),
        });
    }
    MultiOutcome::Selected(paths)
}

/// Build a channel and immediately deliver `outcome` on it, so a headless test
/// (and the `--shots` scenarios) can exercise the exact per-frame drain path
/// the real portal worker feeds — without opening a host dialog. Returns the
/// receiver the app installs, mirroring [`PickRequest::send`]'s contract.
pub fn inject_for_test(outcome: PortalOutcome) -> PortalChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(outcome).expect("seed injected portal outcome");
    rx
}

/// Multi-select counterpart of [`inject_for_test`].
pub fn inject_multi_for_test(outcome: MultiOutcome) -> MultiChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(outcome).expect("seed injected portal outcome");
    rx
}

impl PortalOutcome {
    /// The chosen path, if the interaction produced one.
    pub fn path(&self) -> Option<&Path> {
        match self {
            PortalOutcome::Opened(p) => Some(p.as_path()),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// The ashpd round-trip.
// ---------------------------------------------------------------------------

#[cfg(feature = "portal")]
fn single(req: PickRequest) -> PortalOutcome {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => return PortalOutcome::Failed(format!("portal runtime: {e}")),
    };
    rt.block_on(async move {
        match run(req).await {
            Ok(uris) => map_uri(&uris),
            Err(e) => e.into_single(),
        }
    })
}

#[cfg(feature = "portal")]
fn multiple(req: PickRequest) -> MultiOutcome {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => return MultiOutcome::Failed(format!("portal runtime: {e}")),
    };
    rt.block_on(async move {
        match run(req).await {
            Ok(uris) => map_uris(&uris),
            Err(e) => e.into_multi(),
        }
    })
}

/// A portal round-trip that did not return URIs.
#[cfg(feature = "portal")]
enum PortalFailure {
    /// The user dismissed the picker. Surfaces from ashpd as a "cancelled"
    /// response error, not a bug.
    Cancelled,
    Failed(String),
}

#[cfg(feature = "portal")]
impl PortalFailure {
    fn into_single(self) -> PortalOutcome {
        match self {
            PortalFailure::Cancelled => PortalOutcome::Cancelled,
            PortalFailure::Failed(e) => PortalOutcome::Failed(e),
        }
    }
    fn into_multi(self) -> MultiOutcome {
        match self {
            PortalFailure::Cancelled => MultiOutcome::Cancelled,
            PortalFailure::Failed(e) => MultiOutcome::Failed(e),
        }
    }
}

#[cfg(feature = "portal")]
async fn run(req: PickRequest) -> Result<Vec<url::Url>, PortalFailure> {
    use ashpd::desktop::file_chooser::SelectedFiles;

    let to_ashpd = |f: &FileFilter| {
        let mut out = ashpd::desktop::file_chooser::FileFilter::new(&f.name);
        for g in &f.globs {
            out = out.glob(g);
        }
        for m in &f.mimetypes {
            out = out.mimetype(m);
        }
        out
    };

    let response = match &req.kind {
        PickKind::Open { multiple, directory } => {
            let mut r = SelectedFiles::open_file()
                .title(req.title.as_str())
                .accept_label(req.accept_label.as_str())
                .modal(true)
                .multiple(*multiple)
                // Select a folder rather than a file when this is a directory
                // chooser; the portal grays out files in that mode.
                .directory(*directory);
            for f in &req.filters {
                r = r.filter(to_ashpd(f));
            }
            r.send().await.and_then(|r| r.response())
        }
        PickKind::Save { suggested_name } => {
            let mut r = SelectedFiles::save_file()
                .title(req.title.as_str())
                .accept_label(req.accept_label.as_str())
                .modal(true)
                .current_name(suggested_name.as_str());
            for f in &req.filters {
                r = r.filter(to_ashpd(f));
            }
            r.send().await.and_then(|r| r.response())
        }
    };

    match response {
        Ok(files) => Ok(files.uris().to_vec()),
        Err(ashpd::Error::Response(_)) => Err(PortalFailure::Cancelled),
        Err(e) => Err(PortalFailure::Failed(e.to_string())),
    }
}

/// Without the `portal` feature there is no picker: report a failure so the
/// caller still gets exactly one drained outcome and never hangs waiting for a
/// picker that cannot open.
#[cfg(not(feature = "portal"))]
fn single(_req: PickRequest) -> PortalOutcome {
    PortalOutcome::Failed(NO_PORTAL.to_string())
}

#[cfg(not(feature = "portal"))]
fn multiple(_req: PickRequest) -> MultiOutcome {
    MultiOutcome::Failed(NO_PORTAL.to_string())
}

#[cfg(not(feature = "portal"))]
const NO_PORTAL: &str =
    "the file picker needs the real-desktop build (`--features portal`)";

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> url::Url {
        url::Url::parse(s).unwrap()
    }

    // ---- request builders --------------------------------------------

    #[test]
    fn open_directory_configures_a_folder_chooser() {
        let req = PickRequest::open_directory("Open a project folder", "Open");
        assert!(req.is_directory(), "a directory request selects a folder");
        assert!(!req.is_multiple(), "a directory open is single-select");
        assert_eq!(req.suggested_name(), None, "an open has no save name");
        // A plain file open is NOT a directory chooser.
        assert!(!PickRequest::open("Open a file", "Open").is_directory());
        assert!(!PickRequest::open_multiple("Attach", "Attach").is_directory());
    }

    // ---- the security rule -------------------------------------------

    #[test]
    fn rejects_a_uri_that_names_a_remote_host() {
        // The drift this module was built to end. `strip_prefix("file://")`
        // maps this to the *relative* path `evil-host.example.com/etc/passwd`,
        // which the process then resolves against its own working directory —
        // from a URI that explicitly named a remote host. It must be refused.
        let uri = u("file://evil-host.example.com/etc/passwd");

        assert_eq!(to_local_path(&uri), None, "a foreign host is not a local path");
        assert!(
            matches!(map_uri(&[uri.clone()]), PortalOutcome::Failed(_)),
            "a foreign-host URI must not become an openable path"
        );
        assert!(matches!(map_uris(&[uri]), MultiOutcome::Failed(_)));
    }

    #[test]
    fn a_rejected_host_never_yields_a_relative_path() {
        // Independent of the enum: whatever we hand back must never be a path
        // that would resolve against the working directory.
        for s in [
            "file://evil-host.example.com/etc/passwd",
            "file://192.0.2.1/srv/secret",
            "https://example.com/remote.png",
            "smb://fileserver/share/doc.odt",
        ] {
            let out = map_uri(&[u(s)]);
            if let PortalOutcome::Opened(p) = &out {
                panic!("{s} was accepted as {p:?}");
            }
            assert!(matches!(out, PortalOutcome::Failed(_)), "{s} must fail");
        }
    }

    #[test]
    fn localhost_is_the_local_machine() {
        // `file://localhost/...` is the spec's spelling of "this machine" and
        // is equivalent to an empty host.
        assert_eq!(
            map_uri(&[u("file://localhost/home/alice/a.mkv")]),
            PortalOutcome::Opened(PathBuf::from("/home/alice/a.mkv"))
        );
    }

    // ---- escaped filenames, which the nine copies disagreed about -----

    #[test]
    fn percent_escapes_in_filenames_are_decoded() {
        // The case the diverged copy hand-rolled: `%20` is a space.
        assert_eq!(
            map_uri(&[u("file:///home/alice/My%20Clip.mp4")]),
            PortalOutcome::Opened(PathBuf::from("/home/alice/My Clip.mp4"))
        );
        // Multi-byte UTF-8 escapes must survive too.
        assert_eq!(
            map_uri(&[u("file:///home/alice/%C3%A9t%C3%A9%20caf%C3%A9.mp3")]),
            PortalOutcome::Opened(PathBuf::from("/home/alice/été café.mp3"))
        );
        // Other reserved characters that appear in real filenames.
        assert_eq!(
            map_uri(&[u("file:///home/alice/a%23b%3Fc%25d.txt")]),
            PortalOutcome::Opened(PathBuf::from("/home/alice/a#b?c%d.txt"))
        );
    }

    #[test]
    fn a_plus_in_a_path_stays_a_plus() {
        // `+` means space only in `application/x-www-form-urlencoded` query
        // strings. In a path component it is a literal plus, so `my+song.mp3`
        // must open as `my+song.mp3` and not be mangled into `my song.mp3`.
        assert_eq!(
            map_uri(&[u("file:///home/alice/my+song.mp3")]),
            PortalOutcome::Opened(PathBuf::from("/home/alice/my+song.mp3"))
        );
    }

    #[test]
    fn an_escaped_separator_does_not_become_a_new_path_component() {
        // `url::Url::to_file_path` decodes each segment and then joins, so a
        // bare `to_file_path` maps this to `/home/alice/a/b.txt` — one URI
        // segment silently becoming two path components. What the portal
        // presented as a file name must stay a file name.
        assert_eq!(
            to_local_path(&u("file:///home/alice/a%2Fb.txt")),
            None,
            "an escaped separator must not add a path component"
        );
        assert!(matches!(
            map_uri(&[u("file:///home/alice/a%2Fb.txt")]),
            PortalOutcome::Failed(_)
        ));
    }

    #[test]
    fn a_parent_hop_hidden_inside_one_segment_is_rejected() {
        // A single segment whose *escaped separators* decode into a traversal:
        // the URI has three segments, the decoded path would have five, and
        // what the portal presented as one file name would climb two levels.
        for s in [
            "file:///home/alice/%2E%2E%2F%2E%2E%2Fetc/shadow",
            "file:///var/workspace/%2E%2E%2Fetc/shadow",
        ] {
            assert_eq!(to_local_path(&u(s)), None, "{s} must not resolve");
            assert!(matches!(map_uri(&[u(s)]), PortalOutcome::Failed(_)), "{s}");
        }
    }

    #[test]
    fn a_dot_dot_segment_is_resolved_by_uri_normalisation_not_smuggled() {
        // `%2E%2E` as a whole segment is not an escape hatch: RFC 3986 path
        // normalisation happens when the URI is *parsed*, so the URI's own
        // path is already `/etc/shadow` and every conforming consumer agrees.
        // We must therefore return exactly that — no hidden extra component —
        // and leave whether the app may read it to the sandbox, which is the
        // layer that owns that question.
        let uri = u("file:///home/alice/%2E%2E/%2E%2E/etc/shadow");
        assert_eq!(uri.path(), "/etc/shadow", "the URI normalises on parse");
        assert_eq!(to_local_path(&uri), Some(PathBuf::from("/etc/shadow")));
    }

    #[test]
    fn ordinary_nested_paths_still_resolve() {
        // The hardening must not reject the everyday case it sits next to.
        for (uri, want) in [
            ("file:///a.txt", "/a.txt"),
            ("file:///home/alice/Documents/report.pdf", "/home/alice/Documents/report.pdf"),
            ("file:///var/workspace/deep/er/still/x.odp", "/var/workspace/deep/er/still/x.odp"),
            ("file:///home/alice/dot.name.with.dots.tar.gz", "/home/alice/dot.name.with.dots.tar.gz"),
        ] {
            assert_eq!(
                to_local_path(&u(uri)),
                Some(PathBuf::from(want)),
                "{uri} must resolve normally"
            );
        }
    }

    // ---- the shape every copy implemented -----------------------------

    #[test]
    fn empty_is_cancel() {
        assert_eq!(map_uri(&[]), PortalOutcome::Cancelled);
        assert_eq!(map_uris(&[]), MultiOutcome::Cancelled);
    }

    #[test]
    fn single_takes_the_first_local_file() {
        let uris = [u("file:///home/alice/photo.png"), u("file:///home/alice/other.png")];
        assert_eq!(
            map_uri(&uris),
            PortalOutcome::Opened(PathBuf::from("/home/alice/photo.png"))
        );
    }

    #[test]
    fn multi_collects_every_local_file() {
        let uris = [u("file:///home/alice/work.vcf"), u("file:///home/alice/family.vcard")];
        assert_eq!(
            map_uris(&uris),
            MultiOutcome::Selected(vec![
                PathBuf::from("/home/alice/work.vcf"),
                PathBuf::from("/home/alice/family.vcard"),
            ])
        );
    }

    #[test]
    fn multi_skips_remote_but_keeps_local() {
        let uris = [u("https://example.com/remote.vcf"), u("file:///home/alice/local.vcf")];
        assert_eq!(
            map_uris(&uris),
            MultiOutcome::Selected(vec![PathBuf::from("/home/alice/local.vcf")]),
            "a mixed multi-selection keeps the files the user can actually read"
        );
    }

    #[test]
    fn multi_all_remote_is_failure() {
        assert!(matches!(
            map_uris(&[u("https://example.com/a.vcf"), u("https://example.com/b.vcf")]),
            MultiOutcome::Failed(_)
        ));
    }

    #[test]
    fn workspace_paths_resolve_through_the_fuse_mount() {
        // A `/var/workspace/...` pick is an ordinary local path; the FUSE
        // driver hydrates it on read.
        assert_eq!(
            map_uri(&[u("file:///var/workspace/media/a.mkv")]),
            PortalOutcome::Opened(PathBuf::from("/var/workspace/media/a.mkv"))
        );
    }

    #[test]
    fn inject_delivers_the_outcome_immediately() {
        let rx = inject_for_test(PortalOutcome::Cancelled);
        assert_eq!(rx.try_recv(), Ok(PortalOutcome::Cancelled));

        let rx = inject_multi_for_test(MultiOutcome::Cancelled);
        assert_eq!(rx.try_recv(), Ok(MultiOutcome::Cancelled));
    }

    #[test]
    fn outcome_path_accessor_reports_only_a_real_pick() {
        assert_eq!(
            PortalOutcome::Opened(PathBuf::from("/tmp/a")).path(),
            Some(Path::new("/tmp/a"))
        );
        assert_eq!(PortalOutcome::Cancelled.path(), None);
        assert_eq!(PortalOutcome::Failed("x".into()).path(), None);
    }

    // ---- the request builder ------------------------------------------

    #[test]
    fn builder_records_the_per_app_delta() {
        let r = PickRequest::open("Insert image", "Insert")
            .thread_name("wp-portal-open")
            .filter(FileFilter::new("Images").glob("*.png").mimetype("image/png"))
            .all_files();
        assert_eq!(r.title(), "Insert image");
        assert!(!r.is_multiple());
        assert_eq!(r.filters.len(), 2);
        assert_eq!(r.filters[0].name(), "Images");
        assert_eq!(r.filters[0].globs, ["*.png"]);
        assert_eq!(r.filters[0].mimetypes, ["image/png"]);
        assert_eq!(r.filters[1], FileFilter::all_files(), "escape hatch is last");
        assert_eq!(r.thread_name, "wp-portal-open");

        assert!(PickRequest::open_multiple("Import contacts", "Import").is_multiple());
        let s = PickRequest::save("Export contacts", "Export", "contacts.vcf");
        assert!(!s.is_multiple());
        assert_eq!(s.kind, PickKind::Save { suggested_name: "contacts.vcf".into() });
    }

    /// Without the `portal` feature the picker cannot open, and the caller
    /// must still receive exactly one outcome rather than hang.
    #[cfg(not(feature = "portal"))]
    #[test]
    fn without_the_feature_a_request_fails_rather_than_hanging() {
        let rx = PickRequest::open("Open", "Open").send();
        assert!(matches!(rx.recv(), Ok(PortalOutcome::Failed(_))));
        let rx = PickRequest::open_multiple("Open", "Open").send_multiple();
        assert!(matches!(rx.recv(), Ok(MultiOutcome::Failed(_))));
    }
}
