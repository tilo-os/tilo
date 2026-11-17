//! The surface an app's signed package publishes, as a host answers for it.

use super::{ActionHost, Pending, RemoteActionMessage};

/// The surface the workspace's package `app` publishes, from `host`,
/// answered for `correlation` ([`ActionHost::installed_surface`]).
pub(super) fn surfaced<H: ActionHost>(
    host: &H,
    correlation: u64,
    app: &str,
) -> Pending<RemoteActionMessage> {
    host.installed_surface(app)
        .map(move |surface| RemoteActionMessage::SurfaceResponse {
            correlation,
            result: Ok(surface.map(|bytes| String::from_utf8_lossy(&bytes).into_owned())),
        })
}
