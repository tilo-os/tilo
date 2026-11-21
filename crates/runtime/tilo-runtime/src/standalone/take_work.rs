//! A member device that takes its workspaces' work: the shell commands of
//! the Terminal sessions of the workspaces a server places on it
//! (`docs/app-model/environments.md`, "Members that take work").
//!
//! A workspace whose server hosts it has no device of the person's running
//! its Terminal sessions. The VM host that holds the workspace's VM does
//! instead, as a member device of the server's own person: it holds the
//! workspace and its files as any member does, and answers the server's runs
//! on its device action service ([`ACTIONS_ALPN`]) with a
//! [`EnvironmentCommands`](ush::environment_commands::EnvironmentCommands) dispatcher over
//! the workspace's shells ([`ush::workspace_shell::WorkspaceShells`]):
//!
//! - ush's own commands run in process on the workspace's files: the folder
//!   of the synced replica the member holds of them
//!   ([`super::files::Replicas`]), or, for a member that holds no replicas, a
//!   directory of the profile per workspace;
//! - a Linux program runs in the workspace's VM, which is the local hosted-VM
//!   service the member's environment names (`TILO_HOSTED_VM_SOCKET`,
//!   `TILO_HOSTED_PLANS`, `TILO_HOSTED_ACCOUNT`, `TILO_HOSTED_VM_SIZE`), with
//!   the account and size each run is paid for coming from the server's
//!   request.
//!
//! The member keeps the lease of each VM its hosted-VM service keeps
//! (`tilo_peer_state::vm_lease`), claiming it in the VM's workspace as its
//! own member's write and renewing it while the VM is there: the server reads
//! the lease to know which machine keeps the VM.
//!
//! The member's endpoint listens on [`PORT`], where the server's members file
//! reaches it. It serves only the workspaces it holds; the first request for
//! one it does not hold yet makes it ask the server to enter it (the
//! workspaces this profile joined, entering on a miss). What it answers is
//! the server's own runs: a device of another person is refused as every
//! device outside a workspace is.
//!
//! A profile served by the standalone runtime takes work when
//! [`SERVER_ENV`] and [`SERVER_DEVICE_ENV`] name its server; [`Member`] is the
//! same member for a host that serves nothing else.

use super::service::ServiceWorkspaces;
use ed25519_dalek::SigningKey;
use iroh::{Endpoint, EndpointAddr, EndpointId};
use std::net::ToSocketAddrs as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tilo_actions::remote::ActionProvider;
use tilo_peer_state::actions::{ACTIONS_ALPN, ActionService};
use tilo_user_identity::DeviceCert;

/// The UDP port a member's endpoint listens on, and the one the relay's
/// members file names for it.
pub const PORT: u16 = 7703;
/// Where the server a member serves is reached, `host:port`.
pub const SERVER_ENV: &str = "TILO_TAKE_WORK_SERVER";
/// The device key of the server a member serves, hex.
pub const SERVER_DEVICE_ENV: &str = "TILO_TAKE_WORK_DEVICE";
/// The server's endpoint id, hex, when its endpoint is keyed apart from its
/// device: a Tilo server's is (`tilo-enroll device-key` prints it as
/// `server_node_id`). Without it the endpoint is the device key.
pub const SERVER_ENDPOINT_ENV: &str = "TILO_TAKE_WORK_ENDPOINT";

/// What a member keeps of its shells, under the profile, and where each
/// workspace's files are below it.
#[cfg(target_os = "linux")]
const STATE: &str = "state/take-work";
/// How long a VM lease this member claims lasts unrenewed, and how often it
/// renews the leases of the VMs it keeps. Linux only, as the hosted-VM service
/// is ush's.
#[cfg(target_os = "linux")]
const VM_LEASE: std::time::Duration = std::time::Duration::from_secs(30);
#[cfg(target_os = "linux")]
const VM_RENEWAL: std::time::Duration = std::time::Duration::from_secs(10);
const FILES: &str = "state/take-work/files";

/// The server a member serves: the host that places work here. It is the
/// member's own person's relay, and the one host this member enters a
/// workspace for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Server {
    device: [u8; 32],
    endpoint: [u8; 32],
    address: String,
}

impl Server {
    /// The server whose device key is `device` (hex), reached at `address`
    /// (`host:port`).
    ///
    /// # Errors
    ///
    /// Either is not usable as such.
    pub fn new(device: &str, address: &str) -> Result<Self, String> {
        let key = key(device).ok_or_else(|| format!("{device} is not a device key"))?;
        if address.trim().is_empty() {
            return Err("the server's address is empty".into());
        }
        Ok(Self {
            device: key,
            endpoint: key,
            address: address.trim().to_owned(),
        })
    }

    /// This server with its endpoint keyed `endpoint` (hex), apart from its
    /// device key, as a Tilo server's is.
    ///
    /// # Errors
    ///
    /// `endpoint` is not a key.
    pub fn with_endpoint(mut self, endpoint: &str) -> Result<Self, String> {
        self.endpoint = key(endpoint).ok_or_else(|| format!("{endpoint} is not an endpoint id"))?;
        Ok(self)
    }

    /// The server the environment names ([`SERVER_ENV`], [`SERVER_DEVICE_ENV`]),
    /// when it names one: this profile is a member that takes work.
    ///
    /// # Errors
    ///
    /// A server is named but its address or device key is not usable.
    pub fn from_env() -> Result<Option<Self>, String> {
        let Ok(address) = std::env::var(SERVER_ENV) else {
            return Ok(None);
        };
        let device = std::env::var(SERVER_DEVICE_ENV).map_err(|_| {
            format!("{SERVER_ENV} is set, so {SERVER_DEVICE_ENV} must name the server's device key")
        })?;
        let server = Self::new(&device, &address)?;
        match std::env::var(SERVER_ENDPOINT_ENV) {
            Ok(endpoint) => server.with_endpoint(&endpoint).map(Some),
            Err(_) => Ok(Some(server)),
        }
    }

    /// The server's device key.
    #[must_use]
    pub fn device(&self) -> [u8; 32] {
        self.device
    }

    /// Where the server is reached, `host:port`.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// The server's endpoint, as a session with it is dialed.
    pub(in crate::standalone) fn endpoint_addr(&self) -> Result<EndpointAddr, String> {
        let id = EndpointId::from_bytes(&self.endpoint).map_err(|error| error.to_string())?;
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

/// A 32-byte key written in hex.
fn key(hex_key: &str) -> Option<[u8; 32]> {
    hex::decode(hex_key.trim())
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
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

/// The device action service that serves the work of the workspaces `joined` holds,
/// entering the ones `server` places here that this member does not hold yet.
///
/// # Errors
///
/// This host cannot run a workspace's shell at all.
pub(in crate::standalone) fn action_service(
    profile: &Path,
    workspaces: &Arc<ServiceWorkspaces>,
    server: Server,
    replicas: Option<super::files::ReplicaSlot>,
    browsers: Option<PathBuf>,
) -> std::io::Result<ActionService> {
    #[cfg(target_os = "linux")]
    keep_hosted_vms(workspaces, browsers.clone());
    let dispatcher = shell_runs(profile, workspaces, replicas, browsers).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "a member that takes work runs the workspace's shell on Linux",
        )
    })?;
    let (device, endpoint) = (server.device, server.endpoint);
    let entering = Arc::new(Entering::new(Arc::clone(workspaces), server));
    let reads = Arc::new(super::service::Reads {
        service: Arc::clone(workspaces.service()),
        entering: Some(Arc::clone(&entering)),
    });
    let action_service = {
        let _entered = tilo_rt::app().handle().enter();
        ActionService::over(workspaces.signer(), dispatcher, reads)
    };
    // The server asks from its endpoint, which a Tilo server keys apart from
    // the device it signs its runs as.
    action_service.reach_device_at(device, endpoint);
    Ok(action_service)
}

/// Claim the leases of the VMs the hosted-VM service named by `browsers`
/// keeps. Linux only: the service, and the workspaces' shells, are ush's.
#[cfg(target_os = "linux")]
fn keep_hosted_vms(workspaces: &Arc<ServiceWorkspaces>, browsers: Option<PathBuf>) {
    let vms = browsers.or_else(|| std::env::var_os(ush::hosted_vm::SOCKET_ENV).map(PathBuf::from));
    if let Some(vms) = vms {
        keep_vm_leases(workspaces, vms);
    }
}

/// Claim and renew, for as long as `joined` is held, the lease of each VM the
/// hosted-VM service at `vms` keeps, in the VM's workspace where this member
/// holds it: a VM is kept where its disk is, so the member ranks itself as
/// the machine that holds it ([`tilo_peer_state::vm_rank`]). A workspace
/// this member enters is looked at once it holds it, so a VM it was entered
/// for is claimed at once.
#[cfg(target_os = "linux")]
fn keep_vm_leases(holding: &Arc<ServiceWorkspaces>, vms: PathBuf) {
    let entered = holding.entered();
    let held = Arc::downgrade(holding);
    let renewing = move || {
        while let Some(held) = held.upgrade() {
            for (workspace, environment) in ush::hosted_vm::kept(&vms) {
                let Some(id) =
                    tilo_desktop_model::WorkspaceId::parse(&workspace).filter(|id| held.holds(*id))
                else {
                    continue;
                };
                let rank = tilo_peer_state::vm_rank(tilo_peer_state::unranked(), 0, true);
                let lease = tilo_peer_state::vm_lease(&environment);
                if let Err(error) = tilo_rt::app()
                    .handle()
                    .block_on(held.claim_lease(id, lease, VM_LEASE, rank))
                {
                    tracing::warn!(%workspace, %error, "the lease of a VM kept here was not claimed");
                }
            }
            drop(held);
            // The timer is made on the runtime, which drives it.
            tilo_rt::app().handle().block_on(async {
                let _ = tokio::time::timeout(VM_RENEWAL, entered.notified()).await;
            });
        }
    };
    if let Err(error) = std::thread::Builder::new()
        .name("vm-leases".into())
        .spawn(renewing)
    {
        tracing::warn!(%error, "the leases of the VMs kept here are not claimed");
    }
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
    workspaces: &Arc<ServiceWorkspaces>,
    replicas: Option<super::files::ReplicaSlot>,
    browsers: Option<PathBuf>,
) -> Option<Arc<dyn ActionProvider + Send + Sync>> {
    use ush::environment_commands::EnvironmentCommands;
    use ush::workspace_shell::WorkspaceShells;
    let member = profile.to_path_buf();
    let holder = Arc::downgrade(workspaces);
    let shells = WorkspaceShells::from_env(profile.join(STATE))
        .with_files(move |workspace| {
            // The workspace's files are held from its first run on, a workspace
            // entered for the run included.
            replicas
                .as_ref()
                .and_then(|slot| slot.get())
                .zip(holder.upgrade())
                .zip(tilo_desktop_model::WorkspaceId::parse(workspace))
                .and_then(|((replicas, workspaces), id)| {
                    let name = workspaces
                        .workspace(id)
                        .map_or_else(|| "Workspace".to_owned(), |workspace| workspace.name);
                    super::hold_files(replicas, &name, id)
                })
                .unwrap_or_else(|| files(&member, workspace))
        })
        // Browser starts on the hosted-VM service this host names, else the
        // one its environment does: the account each open is paid by comes
        // with the server's request, so the socket alone says this member
        // runs the workspaces' VMs.
        .with_browsers_on(
            browsers.or_else(|| std::env::var_os(ush::hosted_vm::SOCKET_ENV).map(PathBuf::from)),
        );
    Some(Arc::new(EnvironmentCommands::new(
        shells,
        workspaces.operator(),
    )))
}

#[cfg(not(target_os = "linux"))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "the same shape as the variant that serves ush's shell"
)]
fn shell_runs(
    _profile: &Path,
    _workspaces: &Arc<ServiceWorkspaces>,
    _replicas: Option<super::files::ReplicaSlot>,
    _browsers: Option<PathBuf>,
) -> Option<Arc<dyn ActionProvider + Send + Sync>> {
    None
}

/// A member device that takes work over a profile of its own: the workspaces
/// it holds, and the device action service their runs are answered on. Drop it to stop
/// serving; the workspaces it entered stay held in the profile.
pub struct Member {
    /// Answers the member's action service on its endpoint, holding the workspaces it
    /// serves.
    _router: iroh::protocol::Router,
    /// The workspace service holding the workspaces it serves.
    _workspaces: Arc<ServiceWorkspaces>,
    profile: PathBuf,
}

impl Member {
    /// Serve, as the device `key` and `cert` certify, the work of the
    /// workspaces kept in `profile` over `endpoint`, entering the ones
    /// `server` places here. `endpoint` is keyed by the same device key.
    /// `browsers` names the hosted-VM service a workspace's browser
    /// environment starts on, when this member runs them.
    ///
    /// # Errors
    ///
    /// The profile cannot be opened, or this host cannot run a workspace's
    /// shell.
    pub fn open(
        profile: &Path,
        (key, cert): (SigningKey, DeviceCert),
        endpoint: Endpoint,
        server: Server,
        browsers: Option<PathBuf>,
    ) -> std::io::Result<Self> {
        let identity = tilo_identity::identity_store::DeviceIdentity::kept(cert.clone(), key);
        let workspaces =
            super::open_member_service(profile, &cert, identity, endpoint.clone())?;
        let action_service = action_service(profile, &workspaces, server, None, browsers)?;
        let router = {
            let _entered = tilo_rt::app().handle().enter();
            iroh::protocol::Router::builder(endpoint)
                .accept(ACTIONS_ALPN, action_service)
                .spawn()
        };
        Ok(Self {
            _router: router,
            _workspaces: workspaces,
            profile: profile.to_path_buf(),
        })
    }

    /// Where `workspace`'s files are on this member.
    #[must_use]
    pub fn files(&self, workspace: &str) -> PathBuf {
        files(&self.profile, workspace)
    }
}

/// How long the server is given to admit this member to a workspace it placed
/// here: the session, the ask and the invitation redeemed.
const ENTER_WITHIN: std::time::Duration = std::time::Duration::from_secs(30);
/// How long the invitation this member redeems stays redeemable.
const ENTER_INVITATION_SECONDS: u64 = 600;
/// How long a refusal keeps this member from asking the server about the same
/// workspace again.
const REFUSED_FOR: std::time::Duration = std::time::Duration::from_secs(30);
/// How many refusals are remembered before the oldest is forgotten, so nothing
/// that names many workspaces can grow what this member keeps.
const KEPT_REFUSALS: usize = 64;

/// The workspaces a member answers for, as its device action service reads
/// them: those it holds, and on a miss the one the server it serves enters it
/// into. What admits the member is the server's own decision, never the ask:
/// the invitation it answers names a device of its own person, and a request
/// another person's device makes is refused here.
pub(in crate::standalone) struct Entering {
    workspaces: Arc<ServiceWorkspaces>,
    /// The server that places this member's work: the one host this member
    /// enters a workspace for.
    server: Server,
    /// One enter at a time, so two requests for a workspace the member does
    /// not hold yet make one enter.
    turn: tokio::sync::Mutex<()>,
    /// The workspaces the server did not admit this member to, and why, while
    /// it is not worth asking again.
    refused: Refusals,
}

impl Entering {
    pub(in crate::standalone) fn new(workspaces: Arc<ServiceWorkspaces>, server: Server) -> Self {
        Self {
            workspaces,
            server,
            turn: tokio::sync::Mutex::new(()),
            refused: Refusals::default(),
        }
    }

    /// The device of the server this member serves: the one asker whose
    /// request may make it enter a workspace.
    pub(in crate::standalone) fn server(&self) -> [u8; 32] {
        self.server.device()
    }

    /// Enter `workspace`, unless the member holds it already or the server
    /// refused it a moment ago.
    pub(in crate::standalone) async fn enter(&self, workspace: &str) -> Result<(), String> {
        let Some(workspace) = tilo_desktop_model::WorkspaceId::parse(workspace) else {
            return Err("the action service named no workspace".into());
        };
        if self.workspaces.holds(workspace) {
            return Ok(());
        }
        if let Some(reason) = self.refused.reason(workspace) {
            return Err(reason);
        }
        let _turn = self.turn.lock().await;
        if self.workspaces.holds(workspace) {
            // What another request waited for this turn entered.
            return Ok(());
        }
        let entered = match tokio::time::timeout(ENTER_WITHIN, self.ask_the_server(workspace)).await
        {
            Ok(entered) => entered,
            Err(_) => Err("the server did not admit this member in time".into()),
        };
        if let Err(reason) = &entered {
            // A member that cannot enter the workspaces its server places here
            // can do none of their work: every run for one is refused until
            // it can, so the reason belongs in the host's log and not behind
            // a debug flag.
            tracing::warn!(%workspace, %reason, "a workspace is not entered on this member");
            self.refused.remember(workspace, reason.clone());
        }
        entered
    }

    /// Ask the server to enter this member into `workspace`, and hold what it
    /// answers.
    async fn ask_the_server(
        &self,
        workspace: tilo_desktop_model::WorkspaceId,
    ) -> Result<(), String> {
        let client = tilo_server_session::ServerClient::connect_v2_to_own_device(
            self.workspaces.endpoint(),
            self.server.endpoint_addr()?,
            self.workspaces.operator(),
            self.server.device(),
            self.workspaces.device_cert().clone(),
            self.workspaces.signer(),
            tilo_user_identity::RevocationList::new(),
        )
        .await
        .map_err(|error| format!("this member could not reach its server: {error}"))?;
        let invitation = tilo_server_session::workspaces::enter_invitation(
            &client,
            &workspace.to_string(),
            ENTER_INVITATION_SECONDS,
        )
        .await?;
        if self.workspaces.copies().join_member(&invitation).await? != workspace {
            return Err("the server admitted this member to another workspace".into());
        }
        // The workspace's files are held from the join on: the folder fills
        // as the workspace's tree arrives.
        self.workspaces.entered().notify_waiters();
        Ok(())
    }
}

/// The workspaces the server refused this member, and why, while asking again
/// about them is not worth it: one answer from the server per workspace is
/// kept, so a request that names a workspace this member may not have costs
/// no ask at all.
#[derive(Default)]
struct Refusals(
    Mutex<
        std::collections::BTreeMap<tilo_desktop_model::WorkspaceId, (std::time::Instant, String)>,
    >,
);

impl Refusals {
    /// Why the server refused this member `workspace` a moment ago, if it did.
    fn reason(&self, workspace: tilo_desktop_model::WorkspaceId) -> Option<String> {
        self.reason_at(workspace, std::time::Instant::now())
    }

    fn reason_at(
        &self,
        workspace: tilo_desktop_model::WorkspaceId,
        now: std::time::Instant,
    ) -> Option<String> {
        let refused = self.0.lock().ok()?;
        let fresh = refused
            .get(&workspace)
            .filter(|(at, _)| now.saturating_duration_since(*at) < REFUSED_FOR)
            .map(|(_, reason)| reason.clone());
        drop(refused);
        fresh
    }

    fn remember(&self, workspace: tilo_desktop_model::WorkspaceId, reason: String) {
        self.remember_at(workspace, std::time::Instant::now(), reason);
    }

    fn remember_at(
        &self,
        workspace: tilo_desktop_model::WorkspaceId,
        at: std::time::Instant,
        reason: String,
    ) {
        let Ok(mut refused) = self.0.lock() else {
            return;
        };
        refused
            .retain(|_, (remembered, _)| at.saturating_duration_since(*remembered) < REFUSED_FOR);
        while refused.len() >= KEPT_REFUSALS {
            let Some(oldest) = refused
                .iter()
                .min_by_key(|(_, (remembered, _))| *remembered)
                .map(|(workspace, _)| *workspace)
            else {
                break;
            };
            refused.remove(&oldest);
        }
        refused.insert(workspace, (at, reason));
    }
}

#[cfg(test)]
#[path = "take_work_tests.rs"]
mod tests;
