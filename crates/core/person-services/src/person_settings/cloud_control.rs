//! A device's cloud-storage mounts, as the workspace's accounts tell them what to do.
//!
//! Files mount a cloud account on the device that runs the files service, at
//! `~/Remote/<name>`. The service answers a local socket (tilo_fs_client::cloud): the
//! mounts it holds, each with its health and whether it is paused, and what to do with one —
//! sync it now, pause it, resume it. The workspace's accounts reading says the same pause to
//! every window that shows the account, and its `sync-now`, `pause-sync` and `resume-sync`
//! actions pass the word to the service here. Only the Linux desktop mounts cloud folders, so
//! the wording is this module's and one implementation reaches the service.

#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use tilo_fs_client::cloud::{CloudClient, CloudRequest};

/// One cloud account's folder on this device: the account it belongs to and whether its sync is
/// paused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Mount {
    pub(crate) account: String,
    pub(crate) paused: bool,
}

/// What to do with an account's mounted files: copy them now, or stop and start their sync.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Request {
    SyncNow { account: String },
    Pause { account: String },
    Resume { account: String },
}

/// What tells a device's cloud mounts what to do: the files service that mounts them, or a
/// test's stand-in.
pub(crate) trait CloudControl: Send + Sync {
    /// Every mount this device holds.
    ///
    /// # Errors
    /// The files service does not answer.
    fn mounts(&self) -> Result<Vec<Mount>, String>;

    /// Do `request` on this device's mounts.
    ///
    /// # Errors
    /// The files service does not answer, or refused it.
    fn control(&self, request: Request) -> Result<(), String>;
}

/// The files service of this device, where it listens.
#[cfg(unix)]
pub(crate) struct DeviceCloud {
    socket: Option<PathBuf>,
}

#[cfg(unix)]
impl DeviceCloud {
    /// The files service of this device.
    #[must_use]
    pub(crate) fn here() -> Self {
        Self {
            socket: tilo_fs_client::cloud::default_socket(),
        }
    }
}

#[cfg(unix)]
impl CloudControl for DeviceCloud {
    fn mounts(&self) -> Result<Vec<Mount>, String> {
        let socket = self.socket.clone().ok_or_else(no_service)?;
        let mut client = CloudClient::connect(&socket).map_err(|_| no_service())?;
        client
            .mounts()
            .map(|mounts| {
                mounts
                    .into_iter()
                    .map(|mount| Mount {
                        account: mount.account,
                        paused: mount.paused,
                    })
                    .collect()
            })
            .map_err(|error| error.to_string())
    }

    fn control(&self, request: Request) -> Result<(), String> {
        let request = match request {
            Request::SyncNow { account } => CloudRequest::SyncNow { account },
            Request::Pause { account } => CloudRequest::Pause { account },
            Request::Resume { account } => CloudRequest::Resume { account },
        };
        let socket = self.socket.clone().ok_or_else(no_service)?;
        let mut client = CloudClient::connect(&socket).map_err(|_| no_service())?;
        client.control(&request).map_err(|error| error.to_string())
    }
}

/// A device whose files service mounts no cloud folders: only the Linux desktop's does.
#[cfg(not(unix))]
pub(crate) struct DeviceCloud;

#[cfg(not(unix))]
impl DeviceCloud {
    /// This device, which mounts none.
    #[must_use]
    pub(crate) const fn here() -> Self {
        Self
    }
}

#[cfg(not(unix))]
impl CloudControl for DeviceCloud {
    fn mounts(&self) -> Result<Vec<Mount>, String> {
        Ok(Vec::new())
    }

    fn control(&self, _request: Request) -> Result<(), String> {
        Err(no_service())
    }
}

/// What a person reads when this device mounts no cloud files.
fn no_service() -> String {
    "The files service on this device does not answer, so its cloud files cannot be controlled \
     here"
        .to_owned()
}
