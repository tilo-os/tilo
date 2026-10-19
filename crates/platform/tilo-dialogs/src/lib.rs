#![cfg_attr(
    test,
    allow(
        clippy::disallowed_methods,
        clippy::string_slice,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

//! File choosers, opening files with their app, and opening URIs: one API on
//! Linux, macOS and the web.
//!
//! Apps build a [`portal::PickRequest`] or [`portal::LaunchRequest`] and
//! `send` it, or call [`portal::open_uri`]; the outcome arrives over a channel
//! the UI thread drains. [`FilePicker`] and [`UriOpener`] are the seams a host
//! fills (`ui::host::HostServices` holds one of each per app); [`system_picker`]
//! and [`system_opener`] are this target's backends:
//!
//! | Target | Choosers | Open a file / reveal | Open a URI |
//! |---|---|---|---|
//! | Linux | XDG Desktop Portal `FileChooser` | portal `OpenURI` (by descriptor) | portal `OpenURI`, else the desktop opener |
//! | macOS | `NSOpenPanel` / `NSSavePanel`; under Seatbelt shown by the runtime ([`confined`]) | `NSWorkspace` | `NSWorkspace` |
//! | Web | `<input type=file>` by contents; downloads | ⛔ | `window.open` for `http`, `https`, `mailto`, `tel` |
//! | Unattended native ([`portal::SAVE_DIRECTORY_ENV`]) | Save answers in that directory; Open refused | the target's | the target's |
//! | App launched by a Tilo session ([`shell`]) | the shell's own chooser, over the app's granted files | the shell opens it in its app | the shell opens it in the app that claims its scheme, else the target's |
//!
//! [`availability`] says which [`Feature`]s the target has.

use std::sync::{Arc, OnceLock};

mod caps;
pub mod conformance;
pub mod portal;
#[cfg(unix)]
pub mod shell;
#[cfg(not(target_arch = "wasm32"))]
mod unattended;
pub mod web_policy;

#[cfg(target_os = "macos")]
pub mod confined;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod web;
#[cfg(windows)]
mod windows;
#[cfg(target_os = "linux")]
mod xdg;

pub use caps::{Availability, Feature, Unsupported, availability, available};

use portal::{
    ContentsChannel, LaunchChannel, LaunchOutcome, LaunchRequest, MultiChannel, MultiOutcome,
    OpenFileChannel, OpenFileOutcome, PickRequest, PortalChannel, PortalOutcome, SaveChannel,
};

/// Host-provided file choosing and opening.
///
/// Every method returns a channel the UI thread drains once per frame, so an
/// implementation may answer immediately, from a worker thread, or from a
/// later host callback — it must never block the caller.
pub trait FilePicker: Send + Sync {
    /// A single-file Open, Save or folder chooser.
    fn pick(&self, request: PickRequest) -> PortalChannel;
    /// A multi-select Open chooser.
    fn pick_multiple(&self, request: PickRequest) -> MultiChannel;
    /// A single-file Open chooser that delivers the already-open descriptor.
    fn open_file(&self, request: PickRequest) -> OpenFileChannel;
    /// A Save chooser that delivers the chosen destination already open for
    /// writing, for hosts whose documents have no path the app can write to.
    /// Hosts that answer Save with a path keep this default refusal.
    fn save_file(&self, request: PickRequest) -> OpenFileChannel {
        let _ = request;
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(OpenFileOutcome::Failed(
            "this host answers Save with a path; use `PickRequest::send`".to_string(),
        ));
        rx
    }
    /// Open a local path with its default handler, or reveal its folder.
    fn launch(&self, request: LaunchRequest) -> LaunchChannel;
    /// An Open chooser that delivers the chosen files' contents. The default
    /// reads what [`pick`](Self::pick) or [`pick_multiple`](Self::pick_multiple)
    /// answers; a host without local paths (a browser page) implements it
    /// directly. Implementations must enforce the aggregate
    /// [`PickRequest::contents_byte_limit`] before reading unbounded contents.
    fn pick_contents(&self, request: PickRequest) -> ContentsChannel {
        use crate::portal::PickedPaths;
        let limit = request.contents_byte_limit();
        let paths = if !request.is_open() {
            None
        } else if request.is_multiple() {
            Some(PickedPaths::Many(self.pick_multiple(request)))
        } else {
            Some(PickedPaths::One(self.pick(request)))
        };
        crate::portal::read_picked(paths, limit)
    }
    /// A Save chooser that stores `bytes`. The default writes to the path
    /// [`pick`](Self::pick) answers.
    fn save_contents(&self, request: PickRequest, bytes: Vec<u8>) -> SaveChannel {
        let destination = request
            .suggested_name()
            .is_some()
            .then(|| self.pick(request));
        crate::portal::write_picked(destination, bytes)
    }
}

/// Hands a URI to whatever the host registers as its handler.
pub trait UriOpener: Send + Sync {
    /// Open `uri` without waiting for the handler to finish. May block until
    /// the handler is launched or the request is refused, so the toolkit
    /// calls it off the UI thread.
    ///
    /// # Errors
    /// The host refused or cannot open URIs at all.
    fn open(&self, uri: &str) -> std::io::Result<()>;
}

/// Choosers for a session nobody sits at ([`portal::SAVE_DIRECTORY_ENV`]): a
/// Save answers at once with a free path in `directory`, every Open is
/// refused, and opening a path is `system`'s. `None` unless `directory` is
/// absolute.
#[cfg(not(target_arch = "wasm32"))]
pub fn unattended_picker(
    directory: std::path::PathBuf,
    system: Arc<dyn FilePicker>,
) -> Option<Arc<dyn FilePicker>> {
    unattended::SaveDirectoryPicker::new(directory, system)
        .map(|picker| Arc::new(picker) as Arc<dyn FilePicker>)
}

/// The session shell's own choosers ([`shell`]) when the launcher handed this
/// process a connection to them, with `unattended` answering the ones nobody
/// is at the computer for; `None` otherwise.
#[cfg(not(target_arch = "wasm32"))]
pub fn inherited_shell_picker(
    unattended: Option<Arc<dyn FilePicker>>,
) -> Option<Arc<dyn FilePicker>> {
    #[cfg(unix)]
    return shell::inherited_picker(unattended);
    #[cfg(not(unix))]
    {
        drop(unattended);
        None
    }
}

/// Links opened by the session shell when the launcher handed this process a
/// connection to it ([`shell::ShellLink::opener`]), with `elsewhere` opening
/// the ones no app the shell knows claims; `None` otherwise.
#[cfg(not(target_arch = "wasm32"))]
pub fn inherited_shell_opener(elsewhere: Arc<dyn UriOpener>) -> Option<Arc<dyn UriOpener>> {
    #[cfg(unix)]
    return shell::inherited_opener(elsewhere);
    #[cfg(not(unix))]
    {
        drop(elsewhere);
        None
    }
}

/// This target's file choosers.
pub fn system_picker() -> Arc<dyn FilePicker> {
    #[cfg(target_os = "linux")]
    return Arc::new(xdg::PortalFilePicker);
    #[cfg(target_os = "macos")]
    return match confined::from_env() {
        Some(confined) => confined as Arc<dyn FilePicker>,
        None => Arc::new(macos::PanelFilePicker),
    };
    #[cfg(windows)]
    return Arc::new(windows::WindowsFilePicker);
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    return Arc::new(web::PageFiles);
    #[allow(unreachable_code, reason = "a target without choosers")]
    unavailable_picker()
}

/// This target's URI opener.
pub fn system_opener() -> Arc<dyn UriOpener> {
    Arc::new(SystemOpener)
}

struct SystemOpener;

impl UriOpener for SystemOpener {
    fn open(&self, uri: &str) -> std::io::Result<()> {
        if let Err(error) = url::Url::parse(uri) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("bad URI: {error}"),
            ));
        }
        #[cfg(target_os = "linux")]
        return xdg::open_uri(uri);
        #[cfg(target_os = "macos")]
        return macos::open_uri(uri);
        #[cfg(windows)]
        return open::that_detached(uri);
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        return web::open_uri(uri);
        #[allow(unreachable_code, reason = "a target without a URI handler")]
        UnavailableOpener.open(uri)
    }
}

/// Choosers that fail at once, for hosts that offer none.
pub fn unavailable_picker() -> Arc<dyn FilePicker> {
    Arc::new(UnavailableFilePicker)
}

/// An opener that refuses, for hosts that offer none.
pub fn unavailable_opener() -> Arc<dyn UriOpener> {
    Arc::new(UnavailableOpener)
}

const UNAVAILABLE: &str = "Choosing files isn't available here";

struct UnavailableFilePicker;

fn answered<T>(outcome: T) -> std::sync::mpsc::Receiver<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = tx.send(outcome);
    rx
}

impl FilePicker for UnavailableFilePicker {
    fn pick(&self, _request: PickRequest) -> PortalChannel {
        answered(PortalOutcome::Failed(UNAVAILABLE.to_string()))
    }

    fn pick_multiple(&self, _request: PickRequest) -> MultiChannel {
        answered(MultiOutcome::Failed(UNAVAILABLE.to_string()))
    }

    fn open_file(&self, _request: PickRequest) -> OpenFileChannel {
        answered(OpenFileOutcome::Failed(UNAVAILABLE.to_string()))
    }

    fn launch(&self, _request: LaunchRequest) -> LaunchChannel {
        answered(LaunchOutcome::Failed(UNAVAILABLE.to_string()))
    }
}

struct UnavailableOpener;

impl UriOpener for UnavailableOpener {
    fn open(&self, _uri: &str) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Opening links isn't available here",
        ))
    }
}

/// How `send` finds the current host's picker and opener.
///
/// `ui::host` installs one that resolves through its scoped
/// `HostServices`; without one, requests reach [`unavailable_picker`] and
/// [`unavailable_opener`], as with a host that offers none.
#[derive(Clone, Copy)]
pub struct Resolver {
    pub picker: fn() -> Arc<dyn FilePicker>,
    pub opener: fn() -> Arc<dyn UriOpener>,
}

static RESOLVER: OnceLock<Resolver> = OnceLock::new();

/// Install how requests find the current host. The first call wins; the host
/// registry calls it before it answers any request.
pub fn install_resolver(resolver: Resolver) {
    let _ = RESOLVER.set(resolver);
}

pub(crate) fn current_picker() -> Arc<dyn FilePicker> {
    RESOLVER
        .get()
        .map_or_else(unavailable_picker, |resolver| (resolver.picker)())
}

pub(crate) fn current_opener() -> Arc<dyn UriOpener> {
    RESOLVER
        .get()
        .map_or_else(unavailable_opener, |resolver| (resolver.opener)())
}

/// A chooser round-trip that did not return URIs.
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
pub(crate) enum PortalFailure {
    /// The user dismissed the picker. Surfaces from ashpd as a "cancelled"
    /// response error, not a bug.
    Cancelled,
    Failed(String),
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
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
    fn into_open_file(self) -> OpenFileOutcome {
        match self {
            PortalFailure::Cancelled => OpenFileOutcome::Cancelled,
            PortalFailure::Failed(e) => OpenFileOutcome::Failed(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_services_conform() {
        conformance::check_picker_contract(&*unavailable_picker());
        conformance::check_opener_contract(&*unavailable_opener());
    }

    #[test]
    fn the_system_backends_answer_bad_requests_without_a_desktop() {
        conformance::check_picker_contract(&*system_picker());
        conformance::check_opener_contract(&*system_opener());
    }

    #[test]
    fn without_a_resolver_requests_fail_rather_than_hang() {
        // No host installed a resolver in this test binary.
        let outcome = PickRequest::open("Open", "Open").send().recv().unwrap();
        assert!(matches!(outcome, PortalOutcome::Failed(_)));
        assert!(portal::open_uri("https://example.invalid").is_err());
    }
}

#[cfg(test)]
#[cfg(any(unix, windows))]
mod bounded_contents_tests {
    use super::*;
    use crate::portal::ContentsOutcome;
    use std::path::PathBuf;

    struct Selected(Vec<PathBuf>);

    impl FilePicker for Selected {
        fn pick(&self, _: PickRequest) -> PortalChannel {
            let (tx, rx) = std::sync::mpsc::channel();
            tx.send(PortalOutcome::Opened(self.0.first().unwrap().clone()))
                .unwrap();
            rx
        }
        fn pick_multiple(&self, _: PickRequest) -> MultiChannel {
            let (tx, rx) = std::sync::mpsc::channel();
            tx.send(MultiOutcome::Selected(self.0.clone())).unwrap();
            rx
        }
        fn open_file(&self, request: PickRequest) -> OpenFileChannel {
            unavailable_picker().open_file(request)
        }
        fn launch(&self, request: LaunchRequest) -> LaunchChannel {
            unavailable_picker().launch(request)
        }
    }

    fn picked(picker: &Selected, request: PickRequest) -> ContentsOutcome {
        picker
            .pick_contents(request)
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap()
    }

    #[test]
    fn bounded_contents_preserve_exact_limit_and_default_reads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.json");
        std::fs::write(&path, b"hello").unwrap();
        let picker = Selected(vec![path.clone()]);
        for request in [
            PickRequest::open("Open", "Open"),
            PickRequest::open("Open", "Open").max_contents_bytes(5),
        ] {
            let ContentsOutcome::Picked(files) = picked(&picker, request) else {
                panic!("valid selected contents must be delivered");
            };
            assert_eq!(files.len(), 1);
            assert_eq!(files[0].bytes, b"hello");
            assert_eq!(files[0].location.as_ref(), Some(&path));
        }
        assert!(
            matches!(picked(&picker, PickRequest::open("Open", "Open").max_contents_bytes(4)),
            ContentsOutcome::Failed(reason) if reason.contains("byte limit"))
        );
    }

    #[test]
    fn bounded_contents_budget_is_aggregate_and_allows_empty_files() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::write(&a, b"ab").unwrap();
        std::fs::write(&b, b"cd").unwrap();
        let picker = Selected(vec![a.clone(), b]);
        assert!(
            matches!(picked(&picker, PickRequest::open_multiple("Open", "Open").max_contents_bytes(3)),
            ContentsOutcome::Failed(reason) if reason.contains("byte limit"))
        );
        let ContentsOutcome::Picked(files) = picked(
            &picker,
            PickRequest::open_multiple("Open", "Open").max_contents_bytes(4),
        ) else {
            panic!("exact aggregate budget must be accepted");
        };
        assert_eq!(files.iter().map(|f| f.bytes.len()).sum::<usize>(), 4);
        std::fs::write(&a, b"").unwrap();
        let picker = Selected(vec![a]);
        let ContentsOutcome::Picked(files) = picked(
            &picker,
            PickRequest::open("Open", "Open").max_contents_bytes(0),
        ) else {
            panic!("empty file fits a zero byte budget");
        };
        assert!(files[0].bytes.is_empty());
    }

    #[test]
    fn bounded_contents_reject_a_large_sparse_file_and_report_read_failures() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("weights");
        std::fs::File::create(&path)
            .unwrap()
            .set_len(1 << 34)
            .unwrap();
        let picker = Selected(vec![path.clone()]);
        assert!(
            matches!(picked(&picker, PickRequest::open("Open", "Open").max_contents_bytes(1024)),
            ContentsOutcome::Failed(reason) if reason.contains("byte limit"))
        );
        std::fs::remove_file(path).unwrap();
        assert!(
            matches!(picked(&picker, PickRequest::open("Open", "Open").max_contents_bytes(1024)),
            ContentsOutcome::Failed(reason) if !reason.is_empty())
        );
    }
}
