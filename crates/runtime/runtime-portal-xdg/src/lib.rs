//! `runtime-portal-xdg` — idiomatic Rust wrapper around the
//! freedesktop XDG portals (via `ashpd`).
//!
//! qOS apps run inside a launcher-managed sandbox. The launcher's
//! default is that the D-Bus session bus is NOT proxied in — the
//! bus is a wide attack surface, so we only plumb it when the
//! manifest's `[sandbox.portals]` declares an intent. For those apps
//! this crate is the client surface.
//!
//! ## Portals covered
//!
//! - [`FileChooser`] — `Open` and `Save` dialogs.
//! - [`ScreenCast`] — `start` a screen capture session and return the
//!   PipeWire node ids the compositor selected.
//! - [`Notification`] — fire-and-forget desktop notification.
//! - [`Secret`] — retrieve the per-application secret bytes the
//!   portal keyring hands to sandboxed apps.
//! - [`OpenUri`] — hand a URL back to the host (browser, mailto, …).
//!
//! ## Graceful degradation
//!
//! Every call returns `Result<T, PortalError>`. If the D-Bus session
//! bus can't be reached (no `$DBUS_SESSION_BUS_ADDRESS`, or the
//! address points at nothing), we surface [`PortalError::NotAvailable`]
//! — matching the uniform SDK contract. Apps are expected to fall
//! back to a local flow (an in-app file picker, in-app toast, ...).
//!
//! Use [`is_portal_available`] to probe the presence of a bus
//! quickly — cheap enough to gate a "Save with system dialog" button.
//!
//! ## Sync vs async
//!
//! Everything is sync-by-default: each function spins up a tiny
//! current-thread tokio runtime just for its own call. Apps that
//! already own a tokio runtime should call the `_async` variants
//! and drive the future themselves; there is no runtime nesting.

use std::path::PathBuf;

use tokio::runtime::Builder;
use url::Url;

/// Reason a portal call failed. Every entry point returns
/// `Result<_, PortalError>` so an app can match on a single import.
#[derive(Debug, thiserror::Error)]
pub enum PortalError {
    /// No D-Bus session bus is reachable. Either the launcher did
    /// not proxy the bus in (the app's manifest did not declare the
    /// portal) or the app is running on stock Linux with no bus.
    #[error("no D-Bus session bus available: portal unreachable")]
    NotAvailable,
    /// The user closed the portal dialog / declined the action.
    #[error("user cancelled the portal request")]
    Cancelled,
    /// The portal responded but the response was empty.
    #[error("portal returned no response")]
    NoResponse,
    /// The portal frontend is not installed on the host system.
    /// Different from `NotAvailable`: we could reach the bus, but
    /// nothing was listening on the portal interface.
    #[error("portal frontend not found on host")]
    PortalNotFound,
    /// Some other portal-side error. The inner string is a
    /// human-readable diagnostic — not stable across ashpd releases.
    #[error("portal error: {0}")]
    Other(String),
}

impl From<ashpd::Error> for PortalError {
    fn from(e: ashpd::Error) -> Self {
        match e {
            ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled) => {
                PortalError::Cancelled
            }
            ashpd::Error::Response(ashpd::desktop::ResponseError::Other) => {
                PortalError::Other("portal response: Other".into())
            }
            ashpd::Error::NoResponse => PortalError::NoResponse,
            ashpd::Error::PortalNotFound(_) => PortalError::PortalNotFound,
            // A zbus-level error typically means the connection to
            // the bus failed. Treat that as `NotAvailable` so the
            // app can fall back the same way it would on stock Linux
            // with no bus at all.
            ashpd::Error::Zbus(_) => PortalError::NotAvailable,
            other => PortalError::Other(other.to_string()),
        }
    }
}

/// Cheap probe: is the D-Bus session bus reachable?
///
/// Checks two things:
/// 1. `$DBUS_SESSION_BUS_ADDRESS` is set.
/// 2. If the address is a `unix:path=/...` (the common case), the
///    socket file exists.
///
/// Returns `false` on any failure. Never blocks on a real socket
/// connect — that would defeat the purpose of a cheap probe.
///
/// This is a heuristic, not an authorisation gate. Apps MUST still
/// call the relevant portal function and match on
/// [`PortalError::NotAvailable`] to handle "bus vanished between
/// probe and use" races.
pub fn is_portal_available() -> bool {
    let Some(addr) = std::env::var_os("DBUS_SESSION_BUS_ADDRESS") else {
        return false;
    };
    if addr.is_empty() {
        return false;
    }
    // The address is a semicolon-separated list of transports. The
    // one real-world use case for us is `unix:path=/run/user/<uid>/bus`
    // or `unix:abstract=...`. For `unix:path=` we can stat the file;
    // for `unix:abstract=` we optimistically return true (the abstract
    // namespace can't be probed without connect()ing).
    let addr = addr.to_string_lossy();
    for entry in addr.split(';') {
        if entry.is_empty() {
            continue;
        }
        let Some(rest) = entry.strip_prefix("unix:") else {
            // tcp: or launchd: — assume reachable, defer to real connect.
            return true;
        };
        for kv in rest.split(',') {
            if let Some(path) = kv.strip_prefix("path=") {
                if std::path::Path::new(path).exists() {
                    return true;
                }
            } else if kv.starts_with("abstract=") || kv.starts_with("runtime=") {
                // Abstract-namespace socket — no cheap probe. Assume ok.
                return true;
            }
        }
    }
    false
}

/// Run a future to completion on a fresh single-threaded tokio
/// runtime. Used by all the sync wrappers. Panics only if the
/// runtime itself cannot be created (OOM).
fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    let rt = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio current-thread runtime");
    rt.block_on(fut)
}

// -----------------------------------------------------------------------------
// FileChooser
// -----------------------------------------------------------------------------

/// The Open / Save file dialog portal.
pub struct FileChooser;

/// One selected file. `path` is the local filesystem path when the
/// portal returned a `file://` URI (the common case for Flatpak +
/// qOS launchers). `uri` is always populated with the raw string.
#[derive(Debug, Clone)]
pub struct SelectedFile {
    pub uri: String,
    pub path: Option<PathBuf>,
}

fn uris_to_selected(uris: &[Url]) -> Vec<SelectedFile> {
    uris.iter()
        .map(|u| SelectedFile {
            uri: u.as_str().to_string(),
            path: u.to_file_path().ok(),
        })
        .collect()
}

impl FileChooser {
    /// Show an Open dialog. Blocks the current thread until the user
    /// picks a file (or cancels).
    pub fn open(title: &str) -> Result<Vec<SelectedFile>, PortalError> {
        block_on(Self::open_async(title))
    }

    /// Async form of [`FileChooser::open`].
    pub async fn open_async(title: &str) -> Result<Vec<SelectedFile>, PortalError> {
        use ashpd::desktop::file_chooser::SelectedFiles;

        let request = SelectedFiles::open_file().title(title).modal(true);
        let response = request.send().await.map_err(PortalError::from)?;
        let files = response.response().map_err(PortalError::from)?;
        Ok(uris_to_selected(files.uris()))
    }

    /// Show a Save dialog with an optional suggested filename.
    /// Returns the target file (or `None` if the user cancelled).
    pub fn save(
        title: &str,
        current_name: Option<&str>,
    ) -> Result<Option<SelectedFile>, PortalError> {
        block_on(Self::save_async(title, current_name))
    }

    /// Async form of [`FileChooser::save`].
    pub async fn save_async(
        title: &str,
        current_name: Option<&str>,
    ) -> Result<Option<SelectedFile>, PortalError> {
        use ashpd::desktop::file_chooser::SelectedFiles;

        let mut request = SelectedFiles::save_file().title(title).modal(true);
        if let Some(name) = current_name {
            request = request.current_name(name);
        }
        let response = request.send().await.map_err(PortalError::from)?;
        match response.response() {
            Ok(files) => Ok(uris_to_selected(files.uris()).into_iter().next()),
            Err(ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled)) => Ok(None),
            Err(other) => Err(PortalError::from(other)),
        }
    }
}

// -----------------------------------------------------------------------------
// ScreenCast
// -----------------------------------------------------------------------------

/// The screen-capture portal.
pub struct ScreenCast;

/// One captured screen source, as returned by the portal.
#[derive(Debug, Clone)]
pub struct ScreenCastStream {
    /// PipeWire node id the app can dial to receive frames.
    pub pipewire_node_id: u32,
    /// Optional stable id assigned by the portal so the app can
    /// correlate streams across restarts.
    pub id: Option<String>,
    /// Reported source size in pixels, if known.
    pub size: Option<(i32, i32)>,
}

impl ScreenCast {
    /// Request a screencast session. The portal drives its own
    /// user-consent UI (source selection). Returns the selected
    /// streams on success.
    pub fn start() -> Result<Vec<ScreenCastStream>, PortalError> {
        block_on(Self::start_async())
    }

    /// Async form of [`ScreenCast::start`].
    pub async fn start_async() -> Result<Vec<ScreenCastStream>, PortalError> {
        use ashpd::WindowIdentifier;
        use ashpd::desktop::PersistMode;
        use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
        use ashpd::enumflags2::BitFlags;

        let proxy = Screencast::new().await.map_err(PortalError::from)?;
        let session = proxy.create_session().await.map_err(PortalError::from)?;
        proxy
            .select_sources(
                &session,
                CursorMode::Hidden,
                BitFlags::from(SourceType::Monitor) | SourceType::Window,
                false,
                None,
                PersistMode::DoNot,
            )
            .await
            .map_err(PortalError::from)?;
        let streams = proxy
            .start(&session, &WindowIdentifier::default())
            .await
            .map_err(PortalError::from)?
            .response()
            .map_err(PortalError::from)?;
        let out = streams
            .streams()
            .iter()
            .map(|s| ScreenCastStream {
                pipewire_node_id: s.pipe_wire_node_id(),
                id: s.id().map(str::to_string),
                size: s.size(),
            })
            .collect();
        Ok(out)
    }
}

// -----------------------------------------------------------------------------
// Notification
// -----------------------------------------------------------------------------

/// The desktop-notification portal.
pub struct Notification;

impl Notification {
    /// Send a fire-and-forget notification with the given title.
    /// A random id is generated per call.
    pub fn send(title: &str) -> Result<(), PortalError> {
        Self::send_full("", title, None)
    }

    /// Full form: choose the id + body. When `id` is empty a
    /// per-process sequence id is generated.
    pub fn send_full(
        id: &str,
        title: &str,
        body: Option<&str>,
    ) -> Result<(), PortalError> {
        block_on(Self::send_full_async(id, title, body))
    }

    /// Async form of [`Notification::send_full`].
    pub async fn send_full_async(
        id: &str,
        title: &str,
        body: Option<&str>,
    ) -> Result<(), PortalError> {
        use ashpd::desktop::notification::{Notification as AshNotification, NotificationProxy};

        let proxy = NotificationProxy::new().await.map_err(PortalError::from)?;
        let mut n = AshNotification::new(title);
        if let Some(b) = body {
            n = n.body(b);
        }
        let effective_id = if id.is_empty() {
            format!("qos-{}", monotonic_id())
        } else {
            id.to_string()
        };
        proxy
            .add_notification(&effective_id, n)
            .await
            .map_err(PortalError::from)?;
        Ok(())
    }
}

/// Monotonic-per-process id — good enough for notification IDs
/// (they're per-app + short-lived, don't need to be globally unique).
fn monotonic_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{epoch:x}-{n:x}")
}

// -----------------------------------------------------------------------------
// Secret
// -----------------------------------------------------------------------------

/// The per-application secret portal — the portal keyring hands the
/// app a stable random secret it can use to derive per-user keys.
pub struct Secret;

impl Secret {
    /// Retrieve the current secret bytes. Blocks the calling thread.
    pub fn retrieve() -> Result<Vec<u8>, PortalError> {
        block_on(Self::retrieve_async())
    }

    /// Async form of [`Secret::retrieve`].
    pub async fn retrieve_async() -> Result<Vec<u8>, PortalError> {
        ashpd::desktop::secret::retrieve()
            .await
            .map_err(PortalError::from)
    }
}

// -----------------------------------------------------------------------------
// OpenUri
// -----------------------------------------------------------------------------

/// The `org.freedesktop.portal.OpenURI` portal — hands a URL back
/// to the host to open in the appropriate default application.
pub struct OpenUri;

impl OpenUri {
    /// Ask the host to open the given URL. Blocks until the portal
    /// acknowledges (usually a few ms — the host app launches
    /// asynchronously).
    pub fn open(uri: &str) -> Result<(), PortalError> {
        block_on(Self::open_async(uri))
    }

    /// Async form of [`OpenUri::open`].
    pub async fn open_async(uri: &str) -> Result<(), PortalError> {
        use ashpd::desktop::open_uri::OpenFileRequest;

        let url = Url::parse(uri).map_err(|e| PortalError::Other(format!("bad url: {e}")))?;
        OpenFileRequest::default()
            .send_uri(&url)
            .await
            .map_err(PortalError::from)?;
        Ok(())
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Save and restore the DBUS env var — several tests toggle it
    /// and we must not leak state across tests.
    struct BusGuard(Option<std::ffi::OsString>);
    impl BusGuard {
        fn take() -> Self {
            let prev = std::env::var_os("DBUS_SESSION_BUS_ADDRESS");
            // SAFETY: setter on env vars; tests within this module
            // run sequentially per default cargo test scheduling, and
            // we always drop-restore the previous value.
            unsafe { std::env::remove_var("DBUS_SESSION_BUS_ADDRESS") };
            Self(prev)
        }
    }
    impl Drop for BusGuard {
        fn drop(&mut self) {
            match self.0.take() {
                Some(v) => unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", v) },
                None => unsafe { std::env::remove_var("DBUS_SESSION_BUS_ADDRESS") },
            }
        }
    }

    #[test]
    fn is_portal_available_false_when_env_missing() {
        let _g = BusGuard::take();
        assert!(!is_portal_available());
    }

    #[test]
    fn is_portal_available_false_when_env_empty() {
        let _g = BusGuard::take();
        // SAFETY: single-thread env mutation for test setup.
        unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", "") };
        assert!(!is_portal_available());
    }

    #[test]
    fn is_portal_available_false_when_unix_path_missing() {
        let _g = BusGuard::take();
        unsafe {
            std::env::set_var(
                "DBUS_SESSION_BUS_ADDRESS",
                "unix:path=/definitely/does/not/exist/qos-portal-test",
            );
        }
        assert!(!is_portal_available());
    }

    #[test]
    fn is_portal_available_true_for_existing_unix_path() {
        let _g = BusGuard::take();
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("fake.sock");
        std::fs::write(&sock, b"").unwrap();
        unsafe {
            std::env::set_var(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}", sock.display()),
            );
        }
        assert!(is_portal_available());
    }

    #[test]
    fn is_portal_available_true_for_abstract_namespace() {
        let _g = BusGuard::take();
        unsafe {
            std::env::set_var(
                "DBUS_SESSION_BUS_ADDRESS",
                "unix:abstract=/tmp/dbus-abstract-XXXXXX,guid=deadbeef",
            );
        }
        assert!(is_portal_available());
    }

    #[test]
    fn portal_calls_report_notavailable_with_bogus_bus() {
        let _g = BusGuard::take();
        unsafe {
            std::env::set_var(
                "DBUS_SESSION_BUS_ADDRESS",
                "unix:path=/definitely/does/not/exist/qos-portal-test-bogus",
            );
        }
        // Connection attempt will fail. Every portal reports NotAvailable.
        assert!(matches!(
            Notification::send("bogus-bus-test"),
            Err(PortalError::NotAvailable)
        ));
        assert!(matches!(
            OpenUri::open("https://example.org/"),
            Err(PortalError::NotAvailable)
        ));
        assert!(matches!(
            Secret::retrieve(),
            Err(PortalError::NotAvailable)
        ));
    }
}
