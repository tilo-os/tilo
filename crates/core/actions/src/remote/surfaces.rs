//! The surface an app's signed package publishes, in the form it is kept by, as a
//! provider answers for it.

use super::{ActionProvider, Pending, RemoteActionMessage};

/// The surface the workspace's package `app` publishes, from `provider`,
/// answered for `correlation` ([`ActionProvider::installed_surface`]).
pub(super) fn surfaced<H: ActionProvider>(
    provider: &H,
    correlation: u64,
    app: &str,
) -> Pending<RemoteActionMessage> {
    provider
        .installed_surface(app)
        .map(move |surface| RemoteActionMessage::SurfaceResponse {
            correlation,
            result: Ok(surface.map(|bytes| String::from_utf8_lossy(&bytes).into_owned())),
        })
}
