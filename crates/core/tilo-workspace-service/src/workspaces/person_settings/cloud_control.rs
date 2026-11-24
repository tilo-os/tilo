//! A device's cloud-storage mounts, as the workspace's accounts tell them what to do.
//!
//! Files mount a cloud account on the device that runs the files service, at
//! `~/Remote/<name>`. The service answers a local socket ([`tilo_fs_client::cloud`]): the
//! mounts it holds, each with its health and whether it is paused, and what to do with one —
//! sync it now, pause it, resume it. The workspace's accounts reading says the same pause to
//! every window that shows the account, and its `sync-now`, `pause-sync` and `resume-sync`
//! actions pass the word to the service here.

use std::path::PathBuf;

use tilo_fs_client::cloud::{CloudClient, CloudMount, CloudRequest};

/// What tells a device's cloud mounts what to do: the files service that mounts them, or a
/// test's stand-in.
pub(crate) trait CloudControl: Send + Sync {
    /// Every mount this device holds.
    ///
    /// # Errors
    /// The files service does not answer.
    fn mounts(&self) -> Result<Vec<CloudMount>, String>;

    /// Do `request` on this device's mounts.
    ///
    /// # Errors
    /// The files service does not answer, or refused it.
    fn control(&self, request: CloudRequest) -> Result<(), String>;
}

/// The files service of this device, where it listens.
pub(crate) struct DeviceCloud {
    socket: Option<PathBuf>,
}

impl DeviceCloud {
    /// The files service of this device.
    #[must_use]
    pub(crate) fn here() -> Self {
        Self {
            socket: tilo_fs_client::cloud::default_socket(),
        }
    }
}

impl CloudControl for DeviceCloud {
    fn mounts(&self) -> Result<Vec<CloudMount>, String> {
        let socket = self.socket.clone().ok_or_else(no_service)?;
        let mut client = CloudClient::connect(&socket).map_err(|_| no_service())?;
        client.mounts().map_err(|error| error.to_string())
    }

    fn control(&self, request: CloudRequest) -> Result<(), String> {
        let socket = self.socket.clone().ok_or_else(no_service)?;
        let mut client = CloudClient::connect(&socket).map_err(|_| no_service())?;
        client.control(&request).map_err(|error| error.to_string())
    }
}

/// What a person reads when this device mounts no cloud files.
fn no_service() -> String {
    "The files service on this device does not answer, so its cloud files cannot be controlled \
     here"
        .to_owned()
}
