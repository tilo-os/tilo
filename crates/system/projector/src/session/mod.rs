//! A projection session: one receiver, run on a thread of the service until
//! it is stopped or fails.

pub(crate) mod cast;
pub(crate) mod context;
pub(crate) mod external;
pub(crate) mod hls;
pub(crate) mod paths;

use std::time::Duration;

use tilo_projector::control::Credential;
use tilo_projector::{ProjectionDevice, ProjectionOptions, ProjectionProtocol};

use crate::error::ProjectorError;
use context::SessionContext;

/// How long a session may take to show on the receiver before the service
/// stops it.
pub(crate) const SESSION_READY_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a stopping session may take to release its receiver and helpers.
pub(crate) const SESSION_STOP_TIMEOUT: Duration = Duration::from_secs(3);

/// The external programs sessions run, resolved once when the service starts.
#[derive(Clone, Debug)]
pub(crate) struct Helpers {
    /// The `AirPlay` sender.
    pub(crate) doubletake: String,
    /// The Miracast sender.
    pub(crate) fluxcast: String,
}

impl Helpers {
    pub(crate) fn from_env() -> Self {
        Self {
            doubletake: std::env::var("TILO_DOUBLETAKE")
                .unwrap_or_else(|_| "/usr/libexec/tilo-doubletake".to_owned()),
            fluxcast: std::env::var("TILO_FLUXCAST")
                .unwrap_or_else(|_| "/usr/libexec/tilo-fluxcast".to_owned()),
        }
    }
}

/// Runs one session to `device` until it is stopped (`Ok`) or fails.
///
/// # Errors
///
/// Returns why the session could not start or ended on its own.
pub(crate) fn run(
    device: &ProjectionDevice,
    options: &ProjectionOptions,
    credential: Option<Credential>,
    context: &SessionContext,
) -> Result<(), ProjectorError> {
    if !(24..=120).contains(&options.stream_policy.max_fps) {
        return Err(ProjectorError::Protocol(
            "the frame rate limit must be between 24 and 120".to_owned(),
        ));
    }
    context.paths.clear();
    match device.protocol {
        ProjectionProtocol::AirPlay => {
            external::run_airplay_session(device, options, credential, context)
        }
        ProjectionProtocol::Miracast => external::run_miracast_session(device, options, context),
        ProjectionProtocol::FCast | ProjectionProtocol::Chromecast => {
            cast::run_cast_session(device, options, context)
        }
    }
}

/// The address the session reaches `device` at: an IP for the network
/// protocols, the peer's MAC for Miracast.
fn receiver_address(device: &ProjectionDevice) -> Result<&str, ProjectorError> {
    device
        .addresses
        .first()
        .map(String::as_str)
        .ok_or_else(|| ProjectorError::Protocol(format!("{} has no address", device.name)))
}
