//! Linux device location through the XDG Desktop Portal
//! (`org.freedesktop.portal.Location`, via [`ashpd`]): the portal is the
//! security boundary and mediates the permission prompt.

use std::time::Duration;

use crate::{LocationChannel, LocationOutcome};

pub(crate) struct PortalLocation;

impl crate::LocationProvider for PortalLocation {
    fn locate(&self) -> LocationChannel {
        request_location()
    }
}

/// How long to wait for the first fix: GeoClue can take seconds, and past this
/// the answer is unavailable.
const FIX_TIMEOUT: Duration = Duration::from_secs(20);

/// Start the request on the app runtime: the D-Bus round trip and the host's
/// prompt never block the caller.
fn request_location() -> LocationChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    let wake = tilo_rt::wake::WakeHandle::new();
    drop(
        tilo_rt::app().spawn_then(location_future(), move |outcome| {
            if tx.send(outcome).is_ok() {
                wake.wake();
            }
        }),
    );
    rx
}

/// The portal round trip and the first `LocationUpdated`, under a timeout.
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

    // `start` opens the permission prompt and the fix arrives later on
    // `LocationUpdated`, so the two are driven together.
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

fn portal_reason(e: ashpd::Error) -> String {
    match e {
        ashpd::Error::Response(_) => "permission denied".to_string(),
        other => other.to_string(),
    }
}
