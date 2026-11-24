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
//! Apps build a [`portal::PickRequest`] or [`portal::LaunchRequest`] and `send`
//! it, or call [`portal::open_uri`]; the outcome arrives over a channel the UI
//! thread drains. [`FilePicker`] and [`UriOpener`] are the boundaries a host fills
//! (`ui::process::ProcessServices` holds one of each per app): an app process inside Tilo
//! reaches the session's chooser and opener over the sockets it was handed ([`shell`]), and a
//! runner hosting an app on a foreign desktop fills them from the desktop's portals
//! (`portal-client`). [`system_picker`] and [`system_opener`] are this target's backends.
//! [`availability`] says which [`Feature`]s the target has.

use std::sync::{Arc, OnceLock};

mod caps;
#[cfg(unix)]
pub mod chooser_service;
#[cfg(unix)]
pub mod chosen;
#[cfg(any(test, feature = "test-support"))]
pub mod conformance;
pub mod portal;
#[cfg(any(unix, windows))]
pub mod shell;
#[cfg(not(any(unix, windows)))]
#[path = "shell_absent.rs"]
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
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub use web::download;
#[cfg(windows)]
mod windows;
#[cfg(all(target_os = "linux", feature = "portal-client"))]
mod xdg;

pub use caps::{Availability, Feature, Unsupported, availability, available};

use portal::{
    LaunchChannel, LaunchOutcome, LaunchRequest, MultiChannel, MultiOutcome, OpenFileChannel,
    OpenFileOutcome, PickRequest, PortalChannel, PortalOutcome, SaveChannel,
};

pub trait FilePicker: Send + Sync {
    /// Ask the person for a file and answer it by reference: what names it again, and what is
    /// handed on when it is opened.
    fn pick(&self, request: PickRequest) -> PortalChannel;
    fn pick_multiple(&self, request: PickRequest) -> MultiChannel;

    /// Ask the person for a file or folder and answer it already open, with what names it again
    /// ([`portal::ChosenHandle`]): for a file an app reads in place, or a folder it walks. This
    /// is the one answer a chooser gives an app: a native host hands a descriptor it opened with
    /// the person's rights, and a page hands the bytes it read. A chooser that only picks paths
    /// answers none.
    fn open_handle(&self, request: PickRequest) -> OpenFileChannel {
        let _ = request;
        answered(OpenFileOutcome::Failed(UNAVAILABLE.to_string()))
    }

    /// Open a file or folder again by the reference a choice answered
    /// ([`portal::ChosenHandle::reference`]): a window restored on what it showed, or a file
    /// inside a folder the person chose. Only a reference this app was given is reached; a host
    /// that cannot name a choice again answers none.
    fn reopen(&self, reference: &std::path::Path, write: bool) -> OpenFileChannel {
        let _ = (reference, write);
        answered(OpenFileOutcome::Failed(
            "This host cannot open the file again once it is chosen.".to_string(),
        ))
    }

    fn save_file(&self, request: PickRequest) -> OpenFileChannel {
        let _ = request;
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(OpenFileOutcome::Failed(
            "this host answers Save with a path; use `PickRequest::send`".to_string(),
        ));
        rx
    }
    fn launch(&self, request: LaunchRequest) -> LaunchChannel;
    fn save_destination(&self, request: PickRequest) -> portal::DestinationChannel {
        if request.suggested_name().is_none() {
            return portal::DestinationChannel::answered(portal::DestinationOutcome::Failed(
                "only a Save chooser chooses where to save".to_owned(),
            ));
        }
        portal::DestinationChannel::from_path(self.pick(request))
    }
    fn save_contents(&self, request: PickRequest, bytes: Vec<u8>) -> SaveChannel {
        let destination = request
            .suggested_name()
            .is_some()
            .then(|| self.pick(request));
        crate::portal::write_picked(destination, bytes)
    }
}

pub trait UriOpener: Send + Sync {
    fn open(&self, uri: &str) -> std::io::Result<()>;
}

#[cfg(not(target_arch = "wasm32"))]
pub fn unattended_picker(directory: std::path::PathBuf) -> Option<Arc<dyn FilePicker>> {
    unattended::SaveDirectoryPicker::new(directory)
        .map(|picker| Arc::new(picker) as Arc<dyn FilePicker>)
}

/// The session shell's chooser, when the launcher handed this process its connection: the
/// person picks in the session's own sheet. `elsewhere` answers what the session says nobody is
/// at the computer for — a session with no desktop portal behind it has nobody to choose a path
/// for an app, and the unattended picker or the desktop's chooser still answers a Save.
#[cfg(not(target_arch = "wasm32"))]
pub fn inherited_shell_picker(elsewhere: Arc<dyn FilePicker>) -> Option<Arc<dyn FilePicker>> {
    shell::inherited_picker(elsewhere)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn inherited_shell_opener(elsewhere: Arc<dyn UriOpener>) -> Option<Arc<dyn UriOpener>> {
    shell::inherited_opener(elsewhere)
}

pub fn system_picker() -> Arc<dyn FilePicker> {
    #[cfg(all(target_os = "linux", feature = "portal-client"))]
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
    #[allow(
        unreachable_code,
        reason = "a target without choosers, or an app process whose host holds one"
    )]
    unavailable_picker()
}

pub fn system_opener() -> Arc<dyn UriOpener> {
    Arc::new(SystemOpener)
}

struct SystemOpener;

impl UriOpener for SystemOpener {
    fn open(&self, uri: &str) -> std::io::Result<()> {
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        if let Err(error) = url::Url::parse(uri) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("bad URI: {error}"),
            ));
        }
        #[cfg(all(target_os = "linux", feature = "portal-client"))]
        return xdg::open_uri(uri);
        // A host without the portal client (an app process) opens through the desktop's own
        // handler, which needs no D-Bus.
        #[cfg(all(target_os = "linux", not(feature = "portal-client")))]
        return open::that_detached(uri);
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

pub fn unavailable_picker() -> Arc<dyn FilePicker> {
    Arc::new(UnavailableFilePicker)
}

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

    fn launch(&self, _request: LaunchRequest) -> LaunchChannel {
        answered(LaunchOutcome::Failed(UNAVAILABLE.to_string()))
    }

    fn save_destination(&self, _request: PickRequest) -> portal::DestinationChannel {
        portal::DestinationChannel::answered(portal::DestinationOutcome::Failed(
            UNAVAILABLE.to_string(),
        ))
    }

    fn save_contents(&self, _request: PickRequest, _bytes: Vec<u8>) -> SaveChannel {
        answered(portal::SaveOutcome::Failed(UNAVAILABLE.to_string()))
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

#[derive(Clone, Copy)]
pub struct Resolver {
    pub picker: fn() -> Arc<dyn FilePicker>,
    pub opener: fn() -> Arc<dyn UriOpener>,
}

static RESOLVER: OnceLock<Resolver> = OnceLock::new();

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

#[cfg(any(
    all(target_os = "linux", feature = "portal-client"),
    target_os = "macos",
    windows
))]
pub(crate) enum PortalFailure {
    Cancelled,
    Failed(String),
}

#[cfg(any(
    all(target_os = "linux", feature = "portal-client"),
    target_os = "macos",
    windows
))]
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

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn without_a_desktop_every_service_answers_instead_of_hanging;

        phase unavailable_services_conform;
        {
            conformance::check_picker_contract(&*unavailable_picker());
            conformance::check_opener_contract(&*unavailable_opener());
        }

        phase the_system_backends_answer_bad_requests_without_a_desktop;
        {
            conformance::check_picker_contract(&*system_picker());
            conformance::check_opener_contract(&*system_opener());
        }

        phase without_a_resolver_requests_fail_rather_than_hang;
        {
            let outcome = PickRequest::open("Open", "Open").send().recv().unwrap();
            assert!(matches!(outcome, PortalOutcome::Failed(_)));
            assert!(portal::open_uri("https://example.invalid").is_err());
        }
    }

    /// The app process's build: the session's chooser is the only chooser, reached over the
    /// connection its launcher handed it. It carries no portal client, so a pick answers no
    /// choice rather than reaching the desktop over D-Bus.
    #[cfg(all(target_os = "linux", not(feature = "portal-client")))]
    #[test]
    fn an_app_process_chooses_only_through_its_session() {
        let outcome = system_picker()
            .pick(PickRequest::open("Open", "Open"))
            .recv()
            .unwrap();
        assert!(
            matches!(outcome, PortalOutcome::Failed(_)),
            "an app process holds no portal"
        );
    }
}
