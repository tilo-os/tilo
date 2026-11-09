//! A member device that takes its workspaces' work: the shell commands of
//! the Terminal sessions of the workspaces a relay places on it
//! (`docs/app-model/environments.md`, "Members that take work").
//!
//! A workspace whose relay hosts it has no device of the person's running
//! its Terminal sessions. The VM host that holds the workspace's VM does
//! instead, as a member device of the relay's own person: it holds the
//! workspace and its files as any member does, and answers the relay's runs
//! on its device plane ([`ACTIONS_ALPN`]) with a
//! [`EnvironmentRuns`](ush::environment_runs::EnvironmentRuns) dispatcher over
//! the workspace's shells ([`ush::workspace_shell::WorkspaceShells`]):
//!
//! - ush's own commands run in process on the workspace's files: the folder
//!   of the synced replica the member holds of them
//!   ([`super::files::Replicas`]), or, for a member that holds no replicas, a
//!   directory of the profile per workspace;
//! - a Linux program runs in the workspace's VM, which is the local hosted-VM
//!   service the member's environment names (`TILO_HOSTED_VM_SOCKET`,
//!   `TILO_HOSTED_PLANS`, `TILO_HOSTED_ACCOUNT`, `TILO_HOSTED_VM_SIZE`), with
//!   the account and size each run is paid for coming from the relay's
//!   request.
//!
//! The member's endpoint listens on [`PORT`], where the relay's members file
//! reaches it. It serves only the workspaces it holds; the first request for
//! one it does not hold yet makes it ask the relay to enter it (the
//! workspaces this profile joined, entering on a miss). What it answers is
//! the relay's own runs: a device of another person is refused as every
//! device outside a workspace is.
//!
//! A profile served by the standalone runtime takes work when
//! [`RELAY_ENV`] and [`RELAY_DEVICE_ENV`] name its relay; [`Member`] is the
//! same member for a host that serves nothing else.

use super::Joined;
use ed25519_dalek::SigningKey;
use iroh::{Endpoint, EndpointAddr, EndpointId};
use std::net::ToSocketAddrs as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tilo_actions::remote::ActionHost;
use tilo_peer_state::actions::{ACTIONS_ALPN, ActionPlane};
use tilo_user_identity::DeviceCert;

/// The UDP port a member's endpoint listens on, and the one the relay's
/// members file names for it.
pub const PORT: u16 = 7703;
/// Where the relay a member serves is reached, `host:port`.
pub const RELAY_ENV: &str = "TILO_TAKE_WORK_RELAY";
/// The device key of the relay a member serves, hex.
pub const RELAY_DEVICE_ENV: &str = "TILO_TAKE_WORK_DEVICE";

/// What a member keeps of its shells, under the profile, and where each
/// workspace's files are below it.
const STATE: &str = "state/take-work";
const FILES: &str = "state/take-work/files";

/// The relay a member serves: the host that places work here. It is the
/// member's own person's relay, and the one host this member enters a
/// workspace for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Relay {
    device: [u8; 32],
    address: String,
}

impl Relay {
    /// The relay whose device key is `device` (hex), reached at `address`
    /// (`host:port`).
    ///
    /// # Errors
    ///
    /// Either is not usable as such.
    pub fn new(device: &str, address: &str) -> Result<Self, String> {
        let key: [u8; 32] = hex::decode(device.trim())
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| format!("{device} is not a device key"))?;
        if address.trim().is_empty() {
            return Err("the relay's address is empty".into());
        }
        Ok(Self {
            device: key,
            address: address.trim().to_owned(),
        })
    }

    /// The relay the environment names ([`RELAY_ENV`], [`RELAY_DEVICE_ENV`]),
    /// when it names one: this profile is a member that takes work.
    ///
    /// # Errors
    ///
    /// A relay is named but its address or device key is not usable.
    pub fn from_env() -> Result<Option<Self>, String> {
        let Ok(address) = std::env::var(RELAY_ENV) else {
            return Ok(None);
        };
        let device = std::env::var(RELAY_DEVICE_ENV).map_err(|_| {
            format!("{RELAY_ENV} is set, so {RELAY_DEVICE_ENV} must name the relay's device key")
        })?;
        Self::new(&device, &address).map(Some)
    }

    /// The relay's device key.
    #[must_use]
    pub fn device(&self) -> [u8; 32] {
        self.device
    }

    /// Where the relay is reached, `host:port`.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// The relay's endpoint, as a session with it is dialed.
    pub(in crate::standalone) fn endpoint_addr(&self) -> Result<EndpointAddr, String> {
        let id = EndpointId::from_bytes(&self.device).map_err(|error| error.to_string())?;
        let sockets = self
            .address
            .to_socket_addrs()
            .map_err(|error| format!("{}: {error}", self.address))?;
        Ok(EndpointAddr::from_parts(
            id,
            sockets.map(iroh::TransportAddr::Ip),
        ))
    }
}

/// Where `workspace`'s files are on a member that holds no synced replica of
/// them: its own copy.
#[must_use]
pub fn files(profile: &Path, workspace: &str) -> PathBuf {
    profile.join(FILES).join(named(workspace))
}

/// A workspace id as one directory name.
fn named(workspace: &str) -> String {
    workspace
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// The device plane that serves the work of the workspaces `joined` holds,
/// entering the ones `relay` places here that this member does not hold yet.
///
/// # Errors
///
/// This host cannot run a workspace's shell at all.
pub(in crate::standalone) fn plane(
    profile: &Path,
    joined: &Arc<Joined>,
    relay: Relay,
    replicas: Option<super::files::ReplicaSlot>,
) -> std::io::Result<ActionPlane> {
    let dispatcher = shell_runs(profile, joined, replicas).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "a member that takes work runs the workspace's shell on Linux",
        )
    })?;
    let entering = Arc::new(super::joined::enter::Entering::new(
        Arc::clone(joined),
        relay,
    ));
    let _entered = tilo_rt::app().handle().enter();
    Ok(ActionPlane::over(joined.device_key(), dispatcher, entering))
}

/// The runs this member answers with: the workspace's shell, asked by the
/// devices of the person this member belongs to, in the folder of the synced
/// replica of the workspace's files once `replicas` holds them. `None` on a
/// host whose shell, ush, is not built for it.
#[cfg(target_os = "linux")]
#[allow(
    clippy::unnecessary_wraps,
    reason = "the variant for a host ush is not built for answers no dispatcher"
)]
fn shell_runs(
    profile: &Path,
    joined: &Arc<Joined>,
    replicas: Option<super::files::ReplicaSlot>,
) -> Option<Arc<dyn ActionHost + Send + Sync>> {
    use ush::environment_runs::EnvironmentRuns;
    use ush::workspace_shell::WorkspaceShells;
    let member = profile.to_path_buf();
    let holder = Arc::downgrade(joined);
    let shells = WorkspaceShells::from_env(profile.join(STATE)).with_files(move |workspace| {
        // The workspace's files are held from its first run on, a workspace
        // entered for the run included.
        replicas
            .as_ref()
            .and_then(|slot| slot.get())
            .zip(holder.upgrade())
            .zip(tilo_desktop_model::WorkspaceId::parse(workspace))
            .and_then(|((replicas, joined), id)| super::hold_files(replicas, &joined, id))
            .unwrap_or_else(|| files(&member, workspace))
    });
    Some(Arc::new(EnvironmentRuns::new(shells, joined.operator())))
}

#[cfg(not(target_os = "linux"))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "the same shape as the variant that serves ush's shell"
)]
fn shell_runs(
    _profile: &Path,
    _joined: &Arc<Joined>,
    _replicas: Option<super::files::ReplicaSlot>,
) -> Option<Arc<dyn ActionHost + Send + Sync>> {
    None
}

/// A member device that takes work over a profile of its own: the workspaces
/// it holds, and the device plane their runs are answered on. Drop it to stop
/// serving; the workspaces it entered stay held in the profile.
pub struct Member {
    /// Answers the member's plane on its endpoint, holding the workspaces it
    /// serves.
    _router: iroh::protocol::Router,
    profile: PathBuf,
}

impl Member {
    /// Serve, as the device `key` and `cert` certify, the work of the
    /// workspaces kept in `profile` over `endpoint`, entering the ones
    /// `relay` places here. `endpoint` is keyed by the same device key.
    ///
    /// # Errors
    ///
    /// The profile cannot be opened, or this host cannot run a workspace's
    /// shell.
    pub fn open(
        profile: &Path,
        (key, cert): (SigningKey, DeviceCert),
        endpoint: Endpoint,
        relay: Relay,
    ) -> std::io::Result<Self> {
        let custody = tilo_identity_host::custody::HostIdentity {
            device_cert: cert,
            device_signing_key: key,
            workspace_device_certs: std::collections::HashMap::new(),
            workspace_revocations: std::collections::HashMap::new(),
            // Nothing here renews sealed writes: the member holds its
            // workspaces as it enrolled in them.
            identity_paths: None,
        };
        let joined = Joined::from_profile(profile, custody, endpoint.clone())?;
        let plane = plane(profile, &joined, relay, None)?;
        // The plane holds the workspaces it serves; this member answers them
        // through the router.
        drop(joined);
        let router = {
            let _entered = tilo_rt::app().handle().enter();
            iroh::protocol::Router::builder(endpoint)
                .accept(ACTIONS_ALPN, plane)
                .spawn()
        };
        Ok(Self {
            _router: router,
            profile: profile.to_path_buf(),
        })
    }

    /// Where `workspace`'s files are on this member.
    #[must_use]
    pub fn files(&self, workspace: &str) -> PathBuf {
        files(&self.profile, workspace)
    }
}

#[cfg(test)]
#[path = "take_work_tests.rs"]
mod tests;
