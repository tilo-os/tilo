//! Device location via the **XDG Desktop Portal**
//! (`org.freedesktop.portal.Location`, through the [`ashpd`] crate).
//!
//! Per the qOS HIG (§5 system integration) an app prefers host/portal services
//! over reinventing them: the *host desktop* (`GeoClue2` behind the portal on
//! GNOME; the compositor's portal backend on qOS) provides the fix and mediates
//! the permission prompt — the portal is the security boundary. This module
//! never fabricates a position: if the portal is missing, permission is denied,
//! or no fix arrives in time, it returns an honest [`LocationOutcome::Unavailable`].
//!
//! The portal call is asynchronous and mediated by a host permission prompt, so
//! it must **never** run on the UI thread (HIG §6). [`request_location`] spawns
//! a worker thread with its own small Tokio runtime, runs the D-Bus round-trip
//! there, and hands the outcome back over an [`std::sync::mpsc`] channel. The
//! app drains that channel once per frame; the UI thread never blocks.
//!
//! Because a real GPS/GeoClue fix depends on the host's location service and the
//! user's permission, the coordinates cannot be verified headlessly. The result
//! *handling* (setting the origin, re-routing, the honest failure states) is
//! unit-tested via [`inject_for_test`], which feeds the exact per-frame drain
//! path without touching D-Bus.

use std::sync::mpsc::Receiver;
use std::time::Duration;

/// The result of a location-portal interaction, delivered back to the UI thread
/// over a channel.
#[derive(Clone, Debug)]
pub enum LocationOutcome {
    /// A real fix from the host location service.
    Located {
        lat: f64,
        lon: f64,
        /// Reported horizontal accuracy in metres (0 when unknown).
        accuracy_m: f64,
    },
    /// No position could be obtained: the portal was missing, the user denied
    /// permission, or no fix arrived. Carries a human-readable reason so the UI
    /// can say *why* rather than inventing a location.
    Unavailable(String),
}

/// A pending location request: the receiver the UI thread drains each frame.
pub type LocationChannel = Receiver<LocationOutcome>;

/// How long to wait for the first location update before giving up. `GeoClue` can
/// take a few seconds to acquire a fix; beyond this we report it unavailable
/// rather than hanging the "Locating…" state forever.
const FIX_TIMEOUT: Duration = Duration::from_secs(20);

/// Spawn the location request on a worker thread and return the receiver the
/// caller drains each frame. The UI thread is never blocked: the D-Bus
/// round-trip and the host permission prompt live entirely on the worker.
pub fn request_location() -> LocationChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("maps-location".into())
        .spawn(move || {
            let outcome = run_location_blocking();
            // The UI may have moved on (window closed); ignore a dead channel.
            let _ = tx.send(outcome);
        })
        .expect("spawn location worker thread");
    rx
}

/// Run the location session to completion on a private Tokio runtime. Blocking —
/// only ever called on the worker thread from [`request_location`].
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
fn portal_reason(e: ashpd::Error) -> String {
    match e {
        ashpd::Error::Response(_) => "permission denied".to_string(),
        other => other.to_string(),
    }
}

/// Test-only: build a channel and immediately deliver `outcome` on it, so a
/// headless test can exercise the exact per-frame drain path the real portal
/// worker feeds — without opening a D-Bus session. Mirrors [`request_location`].
#[cfg(test)]
pub fn inject_for_test(outcome: LocationOutcome) -> LocationChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(outcome).expect("seed injected location outcome");
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inject_delivers_located_immediately() {
        let rx = inject_for_test(LocationOutcome::Located {
            lat: 48.137,
            lon: 11.575,
            accuracy_m: 12.0,
        });
        match rx.try_recv() {
            Ok(LocationOutcome::Located { lat, lon, .. }) => {
                assert!((lat - 48.137).abs() < 1e-9);
                assert!((lon - 11.575).abs() < 1e-9);
            }
            other => panic!("expected Located, got {other:?}"),
        }
    }

    #[test]
    fn inject_delivers_unavailable() {
        let rx = inject_for_test(LocationOutcome::Unavailable("permission denied".into()));
        assert!(matches!(rx.try_recv(), Ok(LocationOutcome::Unavailable(_))));
    }
}
