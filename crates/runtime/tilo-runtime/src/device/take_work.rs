//! A member device that takes its workspaces' Linux work: the shell commands,
//! terminals, machines and Browser of the workspaces its server serves
//! (`docs/app-model/environments.md`, "Members that take work").
//!
//! A workspace whose server hosts it has no device of the person's running
//! its Terminal sessions. A VM host does instead, as a member device of the
//! server's own person: it holds the workspace and its files as any member
//! does, and takes the workspace's Linux turns
//! ([`tilo_workspace_service::work::Turn::Linux`]) on its device action service
//! ([`ACTIONS_ALPN`]) when [`takes_turn`](tilo_workspace_service::work::takes_turn)
//! says they are its, running each with an
//! [`EnvironmentCommands`](ush::environment_commands::EnvironmentCommands) dispatcher over
//! the workspace's shells ([`ush::workspace_shell::WorkspaceShells`]):
//!
//! - ush's own commands run in process on the workspace's files: the folder
//!   of the synced replica the member holds of them
//!   ([`files::Replicas`]), or, for a member that holds no replicas, a
//!   directory of the profile per workspace;
//! - a Linux program runs in the workspace's VM, which is the local hosted-VM
//!   service the member's environment names (`TILO_HOSTED_VM_SOCKET`,
//!   `TILO_HOSTED_PLANS`, `TILO_HOSTED_ACCOUNT`, `TILO_HOSTED_VM_SIZE`), with
//!   the account and size each run is paid for coming from the server's
//!   request.
//!
//! The member keeps the lease of each VM its hosted-VM service keeps
//! (`tilo_peer_state::vm_lease`), claiming it in the VM's workspace as its
//! own member's write and renewing it while the VM is there: the workspace's
//! Linux turns go to the device that keeps its VM, where its disk is.
//!
//! A Terminal session's shell opened on a terminal here is carried to the server on a stream
//! of its own ([`ush::environment_terminals::ALPN`]), which only the server that asked for it
//! takes ([`TakesWork`]).
//!
//! The member's endpoint listens on [`PORT`], the address its entry among its
//! person's servers names. It serves only the workspaces it holds; the first request for
//! one it does not hold yet makes it ask the server to enter it (the
//! workspaces this profile joined, entering on a miss). What it answers is
//! the server's own runs: a device of another person is refused as every
//! device outside a workspace is.
//!
//! This computer's daemon takes work when its login names its server, as a
//! VM host's ticket leaves it; [`Member`] is the same member for a host that
//! serves nothing else.

pub(crate) mod files;

use super::workspaces::{ServiceWorkspaces, block_on_owned};
use ed25519_dalek::SigningKey;
use iroh::Endpoint;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tilo_actions::remote::ActionProvider;
use tilo_peer_state::actions::{ACTIONS_ALPN, ActionService};
use tilo_user_identity::DeviceCert;

/// The UDP port a member's endpoint listens on where its device names none
/// ([`configured_port`](super::services::configured_port)).
pub const PORT: u16 = 7703;
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
}

impl Server {
    /// The server whose device key is `device` (hex). Where it serves a workspace is what its
    /// requests say, never configured here.
    ///
    /// # Errors
    ///
    /// `device` is not a device key.
    pub fn new(device: &str) -> Result<Self, String> {
        let key = key(device).ok_or_else(|| format!("{device} is not a device key"))?;
        Ok(Self {
            device: key,
            endpoint: key,
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

    /// The server this computer's login names, for a member whose profile was joined with a
    /// ticket: the device whose certificate the login carries, reached at the endpoint the
    /// ticket's entry named. `None` without a login or an address.
    #[must_use]
    pub fn from_login(login: Option<&tilo_device_identity::login::Device>) -> Option<Self> {
        let address = login?.server_address.as_ref()?;
        let server = Self::new(&hex::encode(login?.server_cert.device_public_key)).ok()?;
        server
            .with_endpoint(&hex::encode(address.endpoint_addr.id.as_bytes()))
            .ok()
    }

    /// The server's device key.
    #[must_use]
    pub fn device(&self) -> [u8; 32] {
        self.device
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

/// What a member that takes work answers its server on: the device action service, and the
/// terminal streams of the shells it opened for the server there.
pub struct TakesWork {
    actions: ActionService,
    terminals: Box<dyn iroh::protocol::DynProtocolHandler>,
}

impl TakesWork {
    /// Each protocol this member answers its server on, and what answers it.
    #[must_use]
    pub fn protocols(self) -> [(&'static [u8], Box<dyn iroh::protocol::DynProtocolHandler>); 2] {
        [
            (ACTIONS_ALPN, self.actions.into()),
            (ush_alpn(), self.terminals),
        ]
    }

    /// The device action service this member answers its server's work on: the one its
    /// daemon answers the device actions on.
    pub(crate) fn actions(&self) -> ActionService {
        self.actions.clone()
    }

    /// The terminal streams of the shells it opened for its server, and their protocol.
    pub(crate) fn terminals(self) -> (&'static [u8], Box<dyn iroh::protocol::DynProtocolHandler>) {
        (ush_alpn(), self.terminals)
    }
}

/// The protocol the terminal streams are dialled on.
#[cfg(target_os = "linux")]
const fn ush_alpn() -> &'static [u8] {
    ush::environment_terminals::ALPN
}

#[cfg(not(target_os = "linux"))]
const fn ush_alpn() -> &'static [u8] {
    b"tilo-environment-terminal/1"
}

/// What a member's shells answer with: the Linux machines its turns run on, and what
/// serves the terminal streams they hand out, once that service maps the endpoints that
/// dial it to their devices.
struct Shells {
    linux: Arc<dyn tilo_workspace_service::work::HostLinux>,
    terminals: TerminalsOf,
}

type TerminalsOf = Box<dyn FnOnce(ActionService) -> Box<dyn iroh::protocol::DynProtocolHandler>>;

/// The device action service that serves the work of the workspaces `joined` holds,
/// entering the ones `server` places here that this member does not hold yet, and the
/// terminal streams of its shells.
///
/// # Errors
///
/// This host cannot run a workspace's shell at all.
pub(crate) fn action_service(
    profile: &Path,
    workspaces: &Arc<ServiceWorkspaces>,
    server: Server,
    replicas: Option<files::ReplicaSlot>,
    browsers: Option<PathBuf>,
) -> std::io::Result<TakesWork> {
    #[cfg(target_os = "linux")]
    keep_hosted_vms(workspaces, browsers.clone());
    let shells = shell_runs(profile, workspaces, replicas, browsers).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "a member that takes work runs the workspace's shell on Linux",
        )
    })?;
    workspaces.take_work();
    let (device, endpoint) = (server.device, server.endpoint);
    let entering = Arc::new(Entering::new(Arc::clone(workspaces), server));
    let reads = Arc::new(super::workspaces::Reads {
        service: Arc::clone(workspaces.service()),
        entering: Some(Arc::clone(&entering)),
    });
    workspaces.copies().run_linux(shells.linux);
    let action_service = {
        let _entered = tilo_rt::app().handle().enter();
        ActionService::over(
            workspaces.signer(),
            workspaces.copies().device_actions(),
            reads,
        )
    };
    // The server asks from its endpoint, which a Tilo server keys apart from
    // the device it signs its runs as.
    action_service.reach_device_at(device, endpoint);
    Ok(TakesWork {
        terminals: (shells.terminals)(action_service.clone()),
        actions: action_service,
    })
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
    replicas: Option<files::ReplicaSlot>,
    browsers: Option<PathBuf>,
) -> Option<Shells> {
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
                .and_then(std::sync::Weak::upgrade)
                .zip(holder.upgrade())
                .zip(tilo_desktop_model::WorkspaceId::parse(workspace))
                .and_then(|((replicas, workspaces), id)| {
                    let name = workspaces
                        .workspace(id)
                        .map_or_else(|| "Workspace".to_owned(), |workspace| workspace.name);
                    super::services::hold_files(&replicas, &name, id)
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
    let commands = Arc::new(EnvironmentCommands::new(shells, workspaces.operator()));
    let streams = Arc::clone(&commands);
    let terminals: TerminalsOf = Box::new(move |actions: ActionService| {
        // A stream is taken by the device that asked for it, as the endpoint that dials asks
        // on the action service: the server's endpoint is the server's device.
        Box::new(ush::environment_terminals::quic::Route::new(
            move |endpoint, from, to| {
                if let Err(error) = streams.serve_terminal(actions.device_of(endpoint), from, to) {
                    tracing::debug!(%error, "a terminal stream was refused");
                }
            },
        )) as Box<dyn iroh::protocol::DynProtocolHandler>
    });
    Some(Shells {
        linux: Arc::new(Machines(commands)),
        terminals,
    })
}

/// The workspaces' Linux machines on this member: its hosted-VM service, through the
/// dispatcher that runs their work.
#[cfg(target_os = "linux")]
struct Machines(Arc<ush::environment_commands::EnvironmentCommands>);

#[cfg(target_os = "linux")]
impl tilo_workspace_service::work::HostLinux for Machines {
    fn machine_lease(&self) -> String {
        tilo_peer_state::vm_lease(ush::environment_commands::ENVIRONMENT)
    }

    fn has_room(&self) -> bool {
        self.0.has_room()
    }

    fn runs(&self) -> Arc<dyn ActionProvider + Send + Sync> {
        Arc::clone(&self.0) as Arc<dyn ActionProvider + Send + Sync>
    }
}

#[cfg(not(target_os = "linux"))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "the same shape as the variant that serves ush's shell"
)]
fn shell_runs(
    _profile: &Path,
    _workspaces: &Arc<ServiceWorkspaces>,
    _replicas: Option<files::ReplicaSlot>,
    _browsers: Option<PathBuf>,
) -> Option<Shells> {
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
    workspaces: Arc<ServiceWorkspaces>,
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
        let workspaces = open_member_service(profile, &cert, identity, endpoint.clone())?;
        let takes_work = action_service(profile, &workspaces, server, None, browsers)?;
        let router = {
            let _entered = tilo_rt::app().handle().enter();
            let mut router = iroh::protocol::Router::builder(endpoint);
            for (alpn, handler) in takes_work.protocols() {
                router = router.accept(alpn, handler);
            }
            router.spawn()
        };
        Ok(Self {
            _router: router,
            workspaces,
            profile: profile.to_path_buf(),
        })
    }

    /// Where `workspace`'s files are on this member.
    #[must_use]
    pub fn files(&self, workspace: &str) -> PathBuf {
        files(&self.profile, workspace)
    }

    /// Enter its person's workspaces as `login`, the login a ticket left, says, as the device
    /// `key`: the member is then one of its person's servers, found there for their Linux turns.
    ///
    /// # Errors
    ///
    /// The server the login names did not let it in.
    pub async fn log_in(
        &self,
        key: &SigningKey,
        login: &tilo_device_identity::login::Device,
    ) -> Result<(), String> {
        let lent = Some(self.workspaces.lent());
        let entered = tilo_api::daemon::enter::connect(key, login, lent).await;
        self.workspaces
            .hold(&entered.map_err(|error| format!("{error:#}"))?)
            .await;
        Ok(())
    }
}

/// Open a workspace service of `profile` alone, holding the member copies it
/// keeps and entering the ones the server it serves places on it: the service
/// a member that takes work answers on, when it runs no daemon.
fn open_member_service(
    profile: &Path,
    cert: &DeviceCert,
    identity: tilo_identity::identity_store::DeviceIdentity,
    endpoint: Endpoint,
) -> std::io::Result<Arc<ServiceWorkspaces>> {
    // A member opens on whichever thread its embedder calls it from,
    // a runtime's own included.
    let (store, cert, signer, bound) = (
        profile.join("workspace-host"),
        cert.clone(),
        Arc::clone(&identity.signer),
        endpoint.clone(),
    );
    let groups = tilo_workspace_service::own_device::key_groups(
        tilo_workspace_service::apps::workspace_keys::KeyProvider::default(),
        None,
        None,
        None,
    );
    let service = block_on_owned(async move {
        tilo_workspace_service::own_device::open(
            store,
            tilo_workspace_service::own_device::Device {
                cert: &cert,
                signer: &signer,
            },
            bound,
            groups,
            false,
            |config| config,
        )
        .await
    })
    .map_err(std::io::Error::other)?;
    Ok(ServiceWorkspaces::new(
        Arc::new(service),
        identity,
        endpoint,
    ))
}

/// How long the server is given to admit this member to a workspace it placed
/// here: the membership proven and the workspace taken in.
const ENTER_WITHIN: std::time::Duration = std::time::Duration::from_secs(30);
/// How long a refusal keeps this member from asking the server about the same
/// workspace again.
const REFUSED_FOR: std::time::Duration = std::time::Duration::from_secs(30);
/// How many refusals are remembered before the oldest is forgotten, so nothing
/// that names many workspaces can grow what this member keeps.
const KEPT_REFUSALS: usize = 64;

/// The workspaces a member answers for, as its device action service reads
/// them: those it holds, and on a miss the one the server it serves places
/// here, which the member holds as a member copy from the server's signed
/// address. What admits the member is the workspace's own decision, never the
/// ask: the server admits a device its governance names, and a request another
/// device makes is refused here.
pub(crate) struct Entering {
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
    pub(crate) fn new(workspaces: Arc<ServiceWorkspaces>, server: Server) -> Self {
        Self {
            workspaces,
            server,
            turn: tokio::sync::Mutex::new(()),
            refused: Refusals::default(),
        }
    }

    /// The device of the server this member serves: the one asker whose
    /// request may make it enter a workspace.
    pub(crate) fn server(&self) -> [u8; 32] {
        self.server.device()
    }

    /// Enter `workspace`, which the server placed here from `placement`, its signed address for
    /// the workspace, unless the member holds it already or the server refused it a moment ago.
    pub(crate) async fn enter(
        &self,
        workspace: &str,
        placement: Option<&tilo_peer_state::join::TransportHint>,
    ) -> Result<(), String> {
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
        let entered =
            match tokio::time::timeout(ENTER_WITHIN, self.hold_from(workspace, placement)).await {
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

    /// Hold `workspace` as a member copy synced with the server, proving this member's
    /// membership at `placement`, the address the server signed for it.
    async fn hold_from(
        &self,
        workspace: tilo_desktop_model::WorkspaceId,
        placement: Option<&tilo_peer_state::join::TransportHint>,
    ) -> Result<(), String> {
        let placement = placement.ok_or("the server named no address for the workspace")?;
        if placement.host.device_public_key != self.server.device() {
            return Err("the workspace was placed here from another device's address".into());
        }
        if self
            .workspaces
            .copies()
            .hold_as_member(workspace, placement)
            .await?
            != workspace
        {
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
