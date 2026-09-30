//! Abstraction for "spawn a file-picker overlay" — used by
//! [`super::file_chooser::FileChooserBackend`] to surface a real dialog
//! without coupling to the shell's chrome.
//!
//! Two production implementors are imagined: the hosted-mode chrome
//! (pushes a [`ui::widget::FilePicker`] into the workspace overlay
//! layer), and the native compositor (a transient xdg-shell surface).
//!
//! Tests use [`MockFilePickerSpawner`], which resolves the
//! `oneshot::Receiver` with a pre-baked path set without rendering
//! anything; [`NullFilePickerSpawner`] always cancels (host fallback for environments without a chrome).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;

use super::PortalCaller;

/// One named file-type filter the calling app asked for.
///
/// Decoded from the portal's `filters` option (`a(sa(us))`): a list of
/// `(label, [(kind, pattern)])` where kind 0 is a glob, kind 1 a MIME
/// type. Both are kept — an app filtering by MIME (`image/png`) and one
/// filtering by glob (`*.png`) are asking the same question.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PickerFilter {
    /// What the user sees in the filter control ("Images").
    pub label: String,
    /// Lower-cased extensions without the dot, derived from globs.
    pub extensions: Vec<String>,
    /// MIME types named directly by the app.
    pub mimetypes: Vec<String>,
}

impl PickerFilter {
    /// Build from a label plus the raw `(kind, pattern)` pairs.
    ///
    /// A glob is reduced to its extension because that's what the dialog
    /// can match cheaply against a directory listing; a pattern with no
    /// usable extension (`*`, `*.*`, `report-??.txt`) widens the filter to
    /// "everything" rather than silently matching nothing — hiding files the app would have accepted is worse than showing extra ones.
    pub fn from_pairs(label: impl Into<String>, pairs: &[(u32, String)]) -> Self {
        let mut extensions = Vec::new();
        let mut mimetypes = Vec::new();
        let mut matches_everything = false;
        for (kind, pattern) in pairs {
            match kind {
                1 => mimetypes.push(pattern.to_ascii_lowercase()),
                _ => match extension_of_glob(pattern) {
                    Some(ext) => extensions.push(ext),
                    None => matches_everything = true,
                },
            }
        }
        if matches_everything {
            extensions.clear();
            mimetypes.clear();
        }
        Self {
            label: label.into(),
            extensions,
            mimetypes,
        }
    }

    /// `true` when this filter accepts every file.
    pub fn is_catch_all(&self) -> bool {
        self.extensions.is_empty() && self.mimetypes.is_empty()
    }
}

/// Reduce a shell glob to the extension it selects, if it selects one.
///
/// `*.png` → `png`. `*`/`*.*` select everything, and a glob whose
/// wildcards fall inside the extension (`*.p?g`) can't be reduced — both answer `None`, read as "matches everything".
fn extension_of_glob(pattern: &str) -> Option<String> {
    let rest = pattern.strip_prefix("*.")?;
    if rest.is_empty() || rest.contains(['*', '?', '[', ']', '.']) {
        return None;
    }
    Some(rest.to_ascii_lowercase())
}

/// One end-user request the file-picker spawner must fulfil.
///
/// The host implementor renders a real dialog and ultimately fulfils
/// `reply` with the user's selection: `Some(paths)` or `None` (cancelled / closed without selection).
///
/// Note what is *not* here: a starting directory or set of roots the app
/// chose. The portal spec has a `current_folder` option, deliberately
/// dropped here — an app that could aim the dialog could use it to probe
/// for files it was never granted, by reading which directories the
/// dialog opens. Where the dialog starts is the compositor's decision (`PickerScope`).
pub struct FilePickerRequest {
    /// Identity of the requesting app — host can show "{`app_id`} wants to open a file" in the dialog header.
    pub caller: PortalCaller,
    /// User-facing dialog title, provided by the calling app.
    pub title: String,
    /// `true` for `SaveFile` / `SaveFiles` flows, `false` for `OpenFile`; affects the action-button label ("Save" vs "Open").
    pub save: bool,
    /// `true` when `OpenFile` asks the user to choose directories.
    pub directory: bool,
    /// `true` when the app asked for `OpenFiles` — the user may return more than one path.
    pub multiple: bool,
    /// The app's preferred label for the confirm button ("Import").
    pub accept_label: Option<String>,
    pub current_name: Option<String>,
    /// File-type filters the app offered. Empty means "any file".
    pub filters: Vec<PickerFilter>,
    /// Where the dialog starts and how far it may roam, when the compositor
    /// decided it ahead of time (the shell's own chooser service keeps it to
    /// the caller's grants). `None` is the user's own directories.
    pub scope: Option<super::PickerScope>,
    /// One-shot reply channel; the spawner fulfils this with the user's selection.
    pub reply: oneshot::Sender<Option<Vec<PathBuf>>>,
}

impl FilePickerRequest {
    /// The label the confirm button should carry.
    pub fn confirm_label(&self) -> String {
        use crate::shell::i18n::t;
        match self.accept_label.as_deref() {
            Some(l) if !l.trim().is_empty() => l.to_string(),
            _ if self.save => t("portal-file-save"),
            _ if self.directory => t("portal-file-choose-folder"),
            _ => t("portal-file-open"),
        }
    }
}

/// Host-side contract: spawn a real file-picker for `req`.
///
/// Implementors typically push `req` into a per-frame queue the
/// compositor drains on the main thread; the reply channel resolves the portal D-Bus call.
pub trait FilePickerSpawner: Send + Sync {
    /// Enqueue a request. Must not block — the caller is the D-Bus
    /// dispatcher and stalling it would stall every portal request on the main thread.
    fn spawn(&self, req: FilePickerRequest);
}

/// Cheap clone wrapper around `Arc<dyn FilePickerSpawner>` — matches the
/// [`super::PromptQueueHandle`] shape so each interface impl can hold its own copy.
#[derive(Clone)]
pub struct FilePickerSpawnerHandle {
    inner: Arc<dyn FilePickerSpawner>,
}

impl FilePickerSpawnerHandle {
    pub fn new(inner: Arc<dyn FilePickerSpawner>) -> Self {
        Self { inner }
    }

    /// Helper — issue a request and return the receiver, same shape `PromptQueueHandle::enqueue` uses.
    pub fn request(
        &self,
        caller: PortalCaller,
        title: impl Into<String>,
        save: bool,
    ) -> oneshot::Receiver<Option<Vec<PathBuf>>> {
        self.request_with(FilePickerOptions::new(caller, title, save))
    }

    /// Issue a request carrying everything the calling app asked for.
    pub fn request_with(&self, opts: FilePickerOptions) -> oneshot::Receiver<Option<Vec<PathBuf>>> {
        let (tx, rx) = oneshot::channel();
        let req = FilePickerRequest {
            caller: opts.caller,
            title: opts.title,
            save: opts.save,
            directory: opts.directory,
            multiple: opts.multiple,
            accept_label: opts.accept_label,
            current_name: opts.current_name,
            filters: opts.filters,
            scope: None,
            reply: tx,
        };
        self.inner.spawn(req);
        rx
    }
}

/// Everything the `FileChooser` backend decoded out of one portal call,
/// minus the reply channel [`FilePickerSpawnerHandle::request_with`] adds.
#[derive(Debug)]
pub struct FilePickerOptions {
    pub caller: PortalCaller,
    pub title: String,
    pub save: bool,
    pub directory: bool,
    pub multiple: bool,
    pub accept_label: Option<String>,
    pub current_name: Option<String>,
    pub filters: Vec<PickerFilter>,
}

impl FilePickerOptions {
    pub fn new(caller: PortalCaller, title: impl Into<String>, save: bool) -> Self {
        Self {
            caller,
            title: title.into(),
            save,
            directory: false,
            multiple: false,
            accept_label: None,
            current_name: None,
            filters: Vec::new(),
        }
    }

    #[must_use]
    pub fn multiple(mut self, multiple: bool) -> Self {
        self.multiple = multiple;
        self
    }

    #[must_use]
    pub fn directory(mut self, directory: bool) -> Self {
        self.directory = directory;
        self
    }

    #[must_use]
    pub fn accept_label(mut self, label: Option<String>) -> Self {
        self.accept_label = label;
        self
    }

    #[must_use]
    pub fn current_name(mut self, name: Option<String>) -> Self {
        self.current_name = name;
        self
    }

    #[must_use]
    pub fn filters(mut self, filters: Vec<PickerFilter>) -> Self {
        self.filters = filters;
        self
    }
}

/// Cancel-everything spawner — for hosted-mode environments without a
/// chrome (headless tests, the boot-time portal before the first workspace renders). Every request resolves to `None`.
pub struct NullFilePickerSpawner;

impl NullFilePickerSpawner {
    pub fn cancel() -> Self {
        Self
    }
}

impl FilePickerSpawner for NullFilePickerSpawner {
    fn spawn(&self, req: FilePickerRequest) {
        let _ = req.reply.send(None);
    }
}

/// Test-only spawner that returns a pre-baked path list on every
/// request; tracks requests received so tests can assert without rendering anything.
pub struct MockFilePickerSpawner {
    paths: Vec<PathBuf>,
    seen: Mutex<Vec<(PortalCaller, String, bool)>>,
}

impl MockFilePickerSpawner {
    /// Build a mock that resolves every request with `paths`.
    pub fn returning(paths: Vec<PathBuf>) -> Self {
        Self {
            paths,
            seen: Mutex::new(Vec::new()),
        }
    }

    /// Build a mock that always resolves to "user cancelled".
    pub fn always_cancel() -> Self {
        Self {
            paths: Vec::new(),
            seen: Mutex::new(Vec::new()),
        }
    }

    /// Tests assert against this to verify the backend forwarded the right caller / title / save flag.
    pub fn requests(&self) -> Vec<(PortalCaller, String, bool)> {
        self.seen.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

impl FilePickerSpawner for MockFilePickerSpawner {
    fn spawn(&self, req: FilePickerRequest) {
        if let Ok(mut s) = self.seen.lock() {
            s.push((req.caller.clone(), req.title.clone(), req.save));
        }
        // Empty path list ≡ cancelled — mirrors the dialog's "Open with no selection = cancel" treatment.
        let reply = if self.paths.is_empty() {
            None
        } else {
            Some(self.paths.clone())
        };
        let _ = req.reply.send(reply);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::WorkspaceId;
    use std::path::PathBuf;

    #[derive(Default)]
    struct CapturingSpawner {
        seen: Mutex<Option<(bool, bool, Option<String>)>>,
    }

    impl FilePickerSpawner for CapturingSpawner {
        fn spawn(&self, req: FilePickerRequest) {
            *self.seen.lock().unwrap() =
                Some((req.directory, req.multiple, req.accept_label.clone()));
            let _ = req.reply.send(None);
        }
    }

    #[tokio::test]
    async fn picker_spawners_resolve_record_and_cancel_requests() {
        // Phase `null_spawner_resolves_to_none`.
        {
            let h = FilePickerSpawnerHandle::new(Arc::new(NullFilePickerSpawner::cancel()));
            let caller = PortalCaller::new(WorkspaceId(1), "app");
            let rx = h.request(caller, "Open", false);
            let out = rx.await.expect("oneshot");
            assert!(out.is_none(), "null spawner must always cancel");
        }
        // Phase `mock_spawner_returns_baked_paths`.
        {
            let h = FilePickerSpawnerHandle::new(Arc::new(MockFilePickerSpawner::returning(vec![
                PathBuf::from("/tmp/a.txt"),
            ])));
            let caller = PortalCaller::new(WorkspaceId(2), "app");
            let rx = h.request(caller, "Open", false);
            let out = rx.await.expect("oneshot");
            assert_eq!(out, Some(vec![PathBuf::from("/tmp/a.txt")]));
        }
        // Phase `mock_spawner_records_requests`.
        {
            let mock = Arc::new(MockFilePickerSpawner::returning(vec![PathBuf::from("/x")]));
            let h = FilePickerSpawnerHandle::new(mock.clone());
            let caller = PortalCaller::new(WorkspaceId(3), "org.example");
            let _ = h.request(caller.clone(), "Open file", false).await;
            let _ = h.request(caller.clone(), "Save as", true).await;
            let seen = mock.requests();
            assert_eq!(seen.len(), 2);
            assert_eq!(seen[0].1, "Open file");
            assert!(!seen[0].2);
            assert_eq!(seen[1].1, "Save as");
            assert!(seen[1].2);
        }
        // Phase `request_with_propagates_directory_multiple_and_label`.
        {
            let capture = Arc::new(CapturingSpawner::default());
            let h = FilePickerSpawnerHandle::new(capture.clone());
            let caller = PortalCaller::new(WorkspaceId(5), "org.example");
            let out = h
                .request_with(
                    FilePickerOptions::new(caller, "Choose", false)
                        .directory(true)
                        .multiple(true)
                        .accept_label(Some("Attach folder".into())),
                )
                .await
                .expect("oneshot");

            assert!(out.is_none());
            assert_eq!(
                *capture.seen.lock().unwrap(),
                Some((true, true, Some("Attach folder".into())))
            );
        }
        // Phase `mock_always_cancel_returns_none`.
        {
            let h = FilePickerSpawnerHandle::new(Arc::new(MockFilePickerSpawner::always_cancel()));
            let caller = PortalCaller::new(WorkspaceId(4), "app");
            let rx = h.request(caller, "Open", false);
            assert!(rx.await.expect("oneshot").is_none());
        }
    }

    #[test]
    fn picker_labels_prefer_the_app_request_and_fall_back_per_mode() {
        // Phase `directory_defaults_false_and_uses_its_own_default_label`.
        {
            let options = FilePickerOptions::new(
                PortalCaller::new(WorkspaceId(6), "org.example"),
                "Open",
                false,
            );
            assert!(!options.directory);

            let (tx, _rx) = oneshot::channel();
            let req = FilePickerRequest {
                caller: options.caller,
                title: options.title,
                save: false,
                directory: true,
                multiple: false,
                accept_label: None,
                current_name: None,
                filters: Vec::new(),
                scope: None,
                reply: tx,
            };
            assert_eq!(req.confirm_label(), "Choose folder");
        }
        // Phase `the_confirm_label_prefers_what_the_app_asked_for`.
        {
            let (tx, _rx) = oneshot::channel();
            let req = FilePickerRequest {
                caller: PortalCaller::new(WorkspaceId(1), "app"),
                title: "t".into(),
                save: false,
                directory: false,
                multiple: false,
                accept_label: Some("Import".into()),
                current_name: None,
                filters: Vec::new(),
                scope: None,
                reply: tx,
            };
            assert_eq!(req.confirm_label(), "Import");
        }
        // Phase `a_blank_accept_label_falls_back_to_the_mode_default`.
        {
            let (tx, _rx) = oneshot::channel();
            let req = FilePickerRequest {
                caller: PortalCaller::new(WorkspaceId(1), "app"),
                title: "t".into(),
                save: true,
                directory: false,
                multiple: false,
                accept_label: Some("   ".into()),
                current_name: None,
                filters: Vec::new(),
                scope: None,
                reply: tx,
            };
            assert_eq!(req.confirm_label(), "Save");
        }
    }

    #[test]
    fn picker_filters_reduce_globs_and_keep_mime_types() {
        // Phase `a_glob_filter_becomes_an_extension`.
        {
            let f = PickerFilter::from_pairs("Images", &[(0, "*.PNG".into()), (0, "*.jpg".into())]);
            assert_eq!(f.extensions, vec!["png", "jpg"]);
            assert!(f.mimetypes.is_empty());
            assert!(!f.is_catch_all());
        }
        // Phase `a_mime_filter_is_kept_as_a_mime_type`.
        {
            let f = PickerFilter::from_pairs("Images", &[(1, "image/PNG".into())]);
            assert_eq!(f.mimetypes, vec!["image/png"]);
            assert!(f.extensions.is_empty());
        }
        // Phase `an_unreducible_glob_widens_the_filter_instead_of_hiding_everything`.
        {
            // `*.p?g` cannot be reduced to one extension — matching nothing would
            // hide files the app would have accepted, so the filter opens up instead.
            let f = PickerFilter::from_pairs("Odd", &[(0, "*.p?g".into()), (0, "*.png".into())]);
            assert!(
                f.is_catch_all(),
                "a filter with a pattern we cannot honour must not silently \
                 drop the files it was meant to show: {f:?}"
            );
        }
        // Phase `a_bare_star_is_the_all_files_filter`.
        assert!(PickerFilter::from_pairs("All", &[(0, "*".into())]).is_catch_all());
        assert!(PickerFilter::from_pairs("All", &[(0, "*.*".into())]).is_catch_all());
    }
}
