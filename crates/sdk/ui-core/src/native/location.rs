//! Device location via the **XDG Desktop Portal**
//! (`org.freedesktop.portal.Location`, through the [`ashpd`] crate) — the
//! native [`crate::host::LocationProvider`], with the `portal` feature.
//!
//! The *host desktop* (`GeoClue2` behind the portal on GNOME; the compositor's
//! portal backend on Tilo) provides the fix and mediates the permission prompt
//! — the portal is the security boundary. This module never fabricates a
//! position: if the portal is missing, permission is denied, or no fix arrives
//! in time, it returns an honest [`LocationOutcome::Unavailable`].
//!
//! The portal call is asynchronous and mediated by a host permission prompt, so
//! it must **never** run on the UI thread. [`DesktopLocation`] spawns a worker
//! thread with its own small Tokio runtime, runs the D-Bus round-trip there,
//! and hands the outcome back over an [`std::sync::mpsc`] channel.
#![allow(
    clippy::disallowed_methods,
    reason = "native host runner: the portal location request runs on its own thread"
)]

#[cfg(all(feature = "portal", unix))]
use std::time::Duration;

use crate::location::{LocationChannel, LocationOutcome};

/// The native location provider.
pub struct DesktopLocation;

impl crate::host::LocationProvider for DesktopLocation {
    fn locate(&self) -> LocationChannel {
        request_location()
    }
}

/// How long to wait for the first location update before giving up. `GeoClue` can
/// take a few seconds to acquire a fix; beyond this we report it unavailable
/// rather than hanging the "Locating…" state forever.
#[cfg(all(feature = "portal", unix))]
const FIX_TIMEOUT: Duration = Duration::from_secs(20);

/// Spawn the location request on a worker thread and return the receiver the
/// caller drains each frame. The UI thread is never blocked: the D-Bus
/// round-trip and the host permission prompt live entirely on the worker.
fn request_location() -> LocationChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    // A clone survives a failed spawn (the closure below, and the original
    // `tx` it owns, never runs in that case) so the honest Unavailable below
    // still has a sender to use.
    let fallback_tx = tx.clone();
    let spawned = std::thread::Builder::new()
        .name("maps-location".into())
        .spawn(move || {
            let outcome = run_location_blocking();
            // The UI may have moved on (window closed); ignore a dead channel.
            let _ = tx.send(outcome);
        })
        .is_ok();
    if !spawned {
        // Consistent with this module's contract: never hang the UI's
        // "Locating…" state — report the failure honestly instead.
        let _ = fallback_tx.send(LocationOutcome::Unavailable(
            "failed to spawn location worker thread".to_string(),
        ));
    }
    rx
}

/// Run the location session to completion on a private Tokio runtime. Blocking —
/// only ever called on the worker thread from [`request_location`].
#[cfg(all(feature = "portal", unix))]
fn run_location_blocking() -> LocationOutcome {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => return LocationOutcome::Unavailable(format!("location runtime: {e}")),
    };
    rt.block_on(location_future())
}

/// The async portal interaction: open the Location proxy, create a session,
/// start it (which triggers the host permission prompt), and wait for the first
/// `LocationUpdated` signal — all under a timeout.
#[cfg(all(feature = "portal", unix))]
async fn location_future() -> LocationOutcome {
    use ashpd::WindowIdentifier;
    use ashpd::desktop::location::{Accuracy, LocationProxy};
    use futures_util::StreamExt;

    let proxy = match LocationProxy::new().await {
        Ok(p) => p,
        Err(e) => {
            return LocationOutcome::Unavailable(format!("location portal unavailable: {e}"));
        }
    };
    let session = match proxy
        .create_session(None, None, Some(Accuracy::Exact))
        .await
    {
        Ok(s) => s,
        Err(e) => return LocationOutcome::Unavailable(portal_reason(e)),
    };
    let mut stream = match proxy.receive_location_updated().await {
        Ok(s) => s,
        Err(e) => {
            let _ = session.close().await;
            return LocationOutcome::Unavailable(portal_reason(e));
        }
    };

    // `start` opens the permission prompt; the fix arrives asynchronously via
    // the `LocationUpdated` signal, so the two must be driven together.
    let identifier = WindowIdentifier::default();
    let acquired = tokio::time::timeout(FIX_TIMEOUT, async {
        let start = proxy.start(&session, &identifier);
        let updated = stream.next();
        futures_util::join!(start, updated)
    })
    .await;

    let outcome = match acquired {
        Ok((Ok(_), Some(location))) => LocationOutcome::Located {
            lat: location.latitude(),
            lon: location.longitude(),
            accuracy_m: location.accuracy(),
        },
        Ok((Ok(_), None)) => LocationOutcome::Unavailable("no location fix available".to_string()),
        Ok((Err(e), _)) => LocationOutcome::Unavailable(portal_reason(e)),
        Err(_) => LocationOutcome::Unavailable("timed out waiting for a location fix".to_string()),
    };
    let _ = session.close().await;
    outcome
}

/// Map a portal error to an honest, user-facing reason. A `Response` error is
/// how the portal surfaces a denied/cancelled permission prompt.
#[cfg(all(feature = "portal", unix))]
fn portal_reason(e: ashpd::Error) -> String {
    match e {
        ashpd::Error::Response(_) => "permission denied".to_string(),
        other => other.to_string(),
    }
}

/// Without the portal (the `portal` feature, on Unix) there is no location
/// service to ask.
#[cfg(not(all(feature = "portal", unix)))]
fn run_location_blocking() -> LocationOutcome {
    LocationOutcome::Unavailable(
        "device location needs the real-desktop build (`--features portal`)".to_string(),
    )
}
