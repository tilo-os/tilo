//! The Linux backend: device location via the **XDG Desktop Portal**
//! (`org.freedesktop.portal.Location`, through the [`ashpd`] crate).
//!
//! The *host desktop* (`GeoClue2` behind the portal on GNOME; the compositor's
//! portal backend on Tilo) provides the fix and mediates the permission prompt
//! — the portal is the security boundary. This module never fabricates a
//! position: if the portal is missing, permission is denied, or no fix arrives
//! in time, it returns an honest [`LocationOutcome::Unavailable`].
//!
//! The portal call is asynchronous and mediated by a host permission prompt, so
//! it must **never** run on the UI thread. [`PortalLocation`] runs the D-Bus
//! round-trip on the app runtime ([`tilo_rt::app`]), hands the outcome back
//! over an [`std::sync::mpsc`] channel and wakes the event loop.

use std::time::Duration;

use crate::{LocationChannel, LocationOutcome};

/// The portal location provider.
pub(crate) struct PortalLocation;

impl crate::LocationProvider for PortalLocation {
    fn locate(&self) -> LocationChannel {
        request_location()
    }
}

/// How long to wait for the first location update before giving up. `GeoClue` can
/// take a few seconds to acquire a fix; beyond this we report it unavailable
/// rather than hanging the "Locating…" state forever.
const FIX_TIMEOUT: Duration = Duration::from_secs(20);

/// Start the location request on the app runtime and return the receiver the
/// caller drains. The UI thread is never blocked: the D-Bus round-trip and the
/// host permission prompt run on the runtime, and the outcome wakes the event
/// loop of the wake scope the request was made under.
fn request_location() -> LocationChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    let wake = tilo_rt::wake::WakeHandle::new();
    drop(
        tilo_rt::app().spawn_then(location_future(), move |outcome| {
            // The UI may have moved on (window closed); ignore a dead channel.
            if tx.send(outcome).is_ok() {
                wake.wake();
            }
        }),
    );
    rx
}

/// The async portal interaction: open the Location proxy, create a session,
/// start it (which triggers the host permission prompt), and wait for the first
/// `LocationUpdated` signal — all under a timeout.
async fn location_future() -> LocationOutcome {
    use ashpd::desktop::location::{Accuracy, CreateSessionOptions, LocationProxy, StartOptions};
    use futures_util::StreamExt;

    let proxy = match LocationProxy::new().await {
        Ok(p) => p,
        Err(e) => {
            return LocationOutcome::Unavailable(format!("location portal unavailable: {e}"));
        }
    };
    let session = match proxy
        .create_session(CreateSessionOptions::default().set_accuracy(Accuracy::Exact))
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
    let acquired = tokio::time::timeout(FIX_TIMEOUT, async {
        let start = proxy.start(&session, None, StartOptions::default());
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
