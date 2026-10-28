//! Asking the host to install the engine runtime.
//!
//! The Browser lists the engine in its manifest's `optional-runtimes`, and
//! everything that installs the Browser brings it. When it is missing anyway
//! (evicted, or the Browser arrived beside an older image) the app does not
//! stop at a message: it asks the host, over the `app-host` service the
//! launcher gave it, to fetch exactly the runtime it declares, and follows
//! the install's progress. The host decides where from and under which
//! signatures; see `runtime_portal::runtime_request`.

/// Where an install stands, in the terms the page area shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallStatus {
    /// The engine is installed.
    Installed,
    /// The host is fetching it.
    Installing {
        phase: InstallPhase,
        received: u64,
        /// Bytes expected, once the host knows.
        total: Option<u64>,
    },
    /// The host stopped; asking again retries.
    Failed(String),
}

/// What the host is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallPhase {
    /// Finding the build.
    Preparing,
    /// Fetching and checking it.
    Downloading,
    /// Putting it in place.
    Installing,
}

/// A way to ask for the engine and follow the request. One connection, one
/// `status` call per look; the first starts the install.
pub trait EngineInstaller: Send {
    /// Start the install if none runs, and report where it stands.
    fn status(&mut self) -> Result<InstallStatus, String>;

    /// Start a fresh instance of the app, on `uri` if given: a runtime is
    /// mounted into an app's sandbox when it starts, so the app reopens to
    /// have the engine it just installed.
    ///
    /// # Errors
    ///
    /// Returns why the host would not.
    fn relaunch(&mut self, uri: Option<&str>) -> Result<(), String>;
}

/// The host's installer, when the launcher granted this app the service.
#[cfg(feature = "local-daemons")]
#[must_use]
pub fn host_installer() -> Option<Box<dyn EngineInstaller>> {
    runtime_portal::runtime_request::Client::from_service()
        .map(|client| Box::new(HostInstaller(client)) as Box<dyn EngineInstaller>)
}

/// Without the host service there is nothing to ask.
#[cfg(not(feature = "local-daemons"))]
#[must_use]
pub fn host_installer() -> Option<Box<dyn EngineInstaller>> {
    None
}

#[cfg(feature = "local-daemons")]
struct HostInstaller(runtime_portal::runtime_request::Client);

#[cfg(feature = "local-daemons")]
impl EngineInstaller for HostInstaller {
    fn status(&mut self) -> Result<InstallStatus, String> {
        use runtime_portal::runtime_request::{Phase, Response};
        match self
            .0
            .ensure(super::runtime::RUNTIME_REF)
            .map_err(|error| error.to_string())?
        {
            Response::Installed => Ok(InstallStatus::Installed),
            Response::Installing {
                phase,
                received,
                total,
            } => Ok(InstallStatus::Installing {
                phase: match phase {
                    Phase::Preparing => InstallPhase::Preparing,
                    Phase::Downloading => InstallPhase::Downloading,
                    Phase::Installing => InstallPhase::Installing,
                },
                received,
                total,
            }),
            Response::Failed { message } | Response::Refused { message } => {
                Ok(InstallStatus::Failed(message))
            }
            Response::Relaunched | Response::LanguagePacks { .. } => {
                Err("the host answered an install with something else".into())
            }
        }
    }

    fn relaunch(&mut self, uri: Option<&str>) -> Result<(), String> {
        use runtime_portal::runtime_request::Response;
        match self.0.relaunch(uri).map_err(|error| error.to_string())? {
            Response::Relaunched => Ok(()),
            Response::Failed { message } | Response::Refused { message } => Err(message),
            other => Err(format!("unexpected answer to a launch: {other:?}")),
        }
    }
}
