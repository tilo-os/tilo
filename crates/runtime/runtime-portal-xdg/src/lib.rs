//! `runtime-portal-xdg` — idiomatic Rust wrapper around the freedesktop XDG
//! portals (via `ashpd`).
//!
//! Tilo apps run inside a launcher-managed sandbox where the D-Bus session
//! bus is proxied in only when the manifest's `[sandbox.portals]` declares
//! an intent — this crate is the client surface for those apps.
//!
//! File choosing and opening URIs or files are not here: they belong to
//! `ui::portal`, which is the file-access security boundary (URI-to-path
//! validation, descriptor-retaining opens) and the one `OpenURI` client.
//!
//! Every call returns `Result<T, PortalError>`; if the bus can't be reached
//! it surfaces [`PortalError::NotAvailable`] and the app is expected to fall
//! back to a local flow. [`is_portal_available`] probes for a bus cheaply
//! enough to gate a "Save with system dialog" button.
//!
//! Everything is sync-by-default, each function spinning up its own
//! current-thread tokio runtime; apps that already own a runtime should
//! call the `_async` variants directly.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

/// Reason a portal call failed; every entry point returns this so an app
/// can match on a single import.
#[derive(Debug, thiserror::Error)]
pub enum PortalError {
    /// No D-Bus session bus reachable: the launcher didn't proxy it in, or the
    /// app is running on stock Linux with no bus.
    #[error("no D-Bus session bus available: portal unreachable")]
    NotAvailable,
    /// The user closed the portal dialog / declined the action.
    #[error("user cancelled the portal request")]
    Cancelled,
    /// The portal responded but the response was empty.
    #[error("portal returned no response")]
    NoResponse,
    /// The portal frontend is not installed on the host. Different from
    /// `NotAvailable`: the bus was reachable, nothing answered the interface.
    #[error("portal frontend not found on host")]
    PortalNotFound,
    /// Some other portal-side error; the string is a diagnostic, not stable
    /// across ashpd releases.
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
            // A zbus-level error usually means the bus connection failed; treat it as
            // `NotAvailable` so the app falls back the same way as on stock Linux.
            ashpd::Error::Zbus(_) => PortalError::NotAvailable,
            other => PortalError::Other(other.to_string()),
        }
    }
}

/// Cheap probe: is the D-Bus session bus reachable? Checks that
/// `$DBUS_SESSION_BUS_ADDRESS` is set, and for a `unix:path=` address that
/// the socket file exists. Never blocks on a real connect.
///
/// A heuristic, not an authorisation gate — apps must still call the portal
/// and match [`PortalError::NotAvailable`] for the "bus vanished between
/// probe and use" race.
pub fn is_portal_available() -> bool {
    let Some(addr) = std::env::var_os("DBUS_SESSION_BUS_ADDRESS") else {
        return false;
    };
    if addr.is_empty() {
        return false;
    }
    // A semicolon-separated list of transports. For `unix:path=` we can stat
    // the file; for `unix:abstract=` we optimistically return true.
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

/// Drive a portal call to completion for a synchronous caller, on the
/// process's app runtime ([`tilo_rt::app`]) rather than a runtime per call.
/// The sync wrappers are for worker threads: on a UI thread this returns
/// [`PortalError::Other`] instead of freezing the window until the user
/// answers the dialog — use the `*_async` form there.
fn block_on<T, F>(fut: F) -> Result<T, PortalError>
where
    F: std::future::Future<Output = Result<T, PortalError>>,
{
    tilo_rt::try_app()
        .and_then(|runtime| runtime.block_on(fut))
        .map_err(|e| PortalError::Other(e.to_string()))?
}

/// The screen-capture portal.
pub struct ScreenCast;

/// One captured screen source, as returned by the portal.
#[derive(Debug, Clone)]
pub struct ScreenCastStream {
    /// `PipeWire` node id the app can dial to receive frames.
    pub pipewire_node_id: u32,
    /// Optional stable id the portal assigns so the app can correlate streams
    /// across restarts.
    pub id: Option<String>,
    /// Reported source size in pixels, if known.
    pub size: Option<(i32, i32)>,
}

impl ScreenCast {
    /// Request a screencast session; the portal drives its own consent UI
    /// (source selection) and this returns the selected streams on success.
    ///
    /// # Errors
    ///
    /// Returns [`PortalError::NotAvailable`] if no D-Bus session bus is
    /// reachable, [`PortalError::Cancelled`] if the user declines the
    /// consent dialog, or another [`PortalError`] variant for a
    /// portal-side failure.
    pub fn start() -> Result<Vec<ScreenCastStream>, PortalError> {
        block_on(Self::start_async())
    }

    /// Async form of [`Self::start`].
    ///
    /// # Errors
    ///
    /// Same conditions as [`Self::start`].
    pub async fn start_async() -> Result<Vec<ScreenCastStream>, PortalError> {
        use ashpd::desktop::screencast::{
            CursorMode, Screencast, SelectSourcesOptions, SourceType, StartCastOptions,
        };
        use ashpd::desktop::{CreateSessionOptions, PersistMode};
        use ashpd::enumflags2::BitFlags;

        let proxy = Screencast::new().await.map_err(PortalError::from)?;
        let session = proxy
            .create_session(CreateSessionOptions::default())
            .await
            .map_err(PortalError::from)?;
        proxy
            .select_sources(
                &session,
                SelectSourcesOptions::default()
                    .set_cursor_mode(CursorMode::Hidden)
                    .set_sources(BitFlags::from(SourceType::Monitor) | SourceType::Window)
                    .set_multiple(false)
                    .set_persist_mode(PersistMode::DoNot),
            )
            .await
            .map_err(PortalError::from)?;
        let streams = proxy
            .start(&session, None, StartCastOptions::default())
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

/// The desktop-notification portal.
pub struct Notification;

impl Notification {
    /// Send a fire-and-forget notification with a random per-call id.
    ///
    /// # Errors
    ///
    /// Returns [`PortalError::NotAvailable`] if no D-Bus session bus is
    /// reachable, or another [`PortalError`] variant for a portal-side
    /// failure.
    pub fn send(title: &str) -> Result<(), PortalError> {
        Self::send_full("", title, None)
    }

    /// Full form: choose id + body; an empty `id` gets a per-process sequence id.
    ///
    /// # Errors
    ///
    /// Same conditions as [`Self::send`].
    pub fn send_full(id: &str, title: &str, body: Option<&str>) -> Result<(), PortalError> {
        block_on(Self::send_full_async(id, title, body))
    }

    /// Async form of [`Self::send_full`].
    ///
    /// # Errors
    ///
    /// Same conditions as [`Self::send`].
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
            format!("tilo-{}", monotonic_id())
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

/// Monotonic-per-process id — good enough since notification ids are
/// per-app and short-lived.
fn monotonic_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let epoch = tilo_clock::unix_now_ms();
    format!("{epoch:x}-{n:x}")
}

/// The per-application secret portal — the portal keyring hands the
/// app a stable random secret it can use to derive per-user keys.
pub struct Secret;

impl Secret {
    /// Retrieve the current secret bytes. Blocks the calling thread.
    ///
    /// # Errors
    ///
    /// Returns [`PortalError::NotAvailable`] if no D-Bus session bus is
    /// reachable, or another [`PortalError`] variant for a portal-side
    /// failure.
    pub fn retrieve() -> Result<Vec<u8>, PortalError> {
        block_on(Self::retrieve_async())
    }

    /// Async form of [`Self::retrieve`].
    ///
    /// # Errors
    ///
    /// Same conditions as [`Self::retrieve`].
    pub async fn retrieve_async() -> Result<Vec<u8>, PortalError> {
        ashpd::desktop::secret::retrieve()
            .await
            .map_err(PortalError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Start from an unset session bus address; the returned scope restores
    /// the caller's value when the test ends so no state leaks across tests.
    fn without_bus() -> tilo_test_env::ScopedEnv {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.remove("DBUS_SESSION_BUS_ADDRESS");
        env
    }

    #[test]
    fn portal_availability_follows_the_session_bus_address() {
        {
            let _env = without_bus();
            assert!(!is_portal_available(), "case env missing");
        }
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("fake.sock");
        std::fs::write(&sock, b"").unwrap();
        // (case, DBUS_SESSION_BUS_ADDRESS, available)
        let cases = [
            ("env empty", String::new(), false),
            (
                "unix path missing",
                "unix:path=/definitely/does/not/exist/tilo-portal-test".to_owned(),
                false,
            ),
            (
                "existing unix path",
                format!("unix:path={}", sock.display()),
                true,
            ),
            (
                "abstract namespace",
                "unix:abstract=/tmp/dbus-abstract-XXXXXX,guid=deadbeef".to_owned(),
                true,
            ),
        ];
        for (case, address, want) in cases {
            let mut env = without_bus();
            env.set("DBUS_SESSION_BUS_ADDRESS", address);
            assert_eq!(is_portal_available(), want, "case {case}");
        }

        // With a bogus bus the connection attempt fails: every portal
        // reports NotAvailable.
        let mut env = without_bus();
        env.set(
            "DBUS_SESSION_BUS_ADDRESS",
            "unix:path=/definitely/does/not/exist/tilo-portal-test-bogus",
        );
        assert!(matches!(
            Notification::send("bogus-bus-test"),
            Err(PortalError::NotAvailable)
        ));
        assert!(matches!(Secret::retrieve(), Err(PortalError::NotAvailable)));
    }
}
