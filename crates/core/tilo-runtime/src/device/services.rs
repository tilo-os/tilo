//! The services this computer's one daemon runs for the person: the shared action broker and
//! identity service the apps attach to, over the device's one workspace service.
//!
//! The daemon is the device: it acts as the device key and the certificate the person's root
//! signed for it, for the person that certificate names. A device a ticket made of this computer
//! keeps its person's workspaces as a member, entering them through the server its copy of the
//! person's own workspace names ([`super::home`]). A computer that holds the person's root acts as
//! the identity founded on it (`tilo login --new-identity`), in a workspace founded here whose
//! member list names the person as its owner. A computer with neither is served with the device
//! key alone until a person's certificate names it ([`super::unclaimed`]): a ticket logs it in, or
//! its own account starts a new person on it (`devices#found`, [`found_a_person`]). Here the daemon
//! launches the installed apps through the platform launcher (`crate::apps`), records the grants
//! people give apps for other apps' actions as changes to the workspace's member list, serves
//! route keys for its workspaces (`channels`) and routes offloadable actions through the
//! person's other devices ([`crate::offload`]); every other host service is absent, and the
//! broker answers as it does for any host that does not provide one.

use super::commits::{RecordCreation, ServiceCommits};
use super::ticket::Tickets;
use super::workspaces::{MemberListGrants, Opening, ServiceWorkspaces, block_on_owned};
use super::{host_runs, take_work};
use crate::personal_devices::PersonalDevices;
use crate::{BLOB_SOCKET, COLLAB_SOCKET, IDENTITY_SOCKET, RuntimeServices, apps, host, profile};
use ed25519_dalek::SigningKey;
use serde_json::{Map, Value, json};
use std::{
    io,
    os::fd::OwnedFd,
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::{Arc, Mutex},
};
use tilo_actions::remote::invocation_record::Invocations;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus, RemoteActionMessage,
    RemoteActionTarget, device_agent_id, workspace_invocations::Origin,
};
use tilo_api::daemon::DaemonPlace;
use tilo_api::grant_key::GrantKey;
use tilo_api::server::ServerError;
use tilo_api::session::AccountAnswer;
use tilo_desktop_model::WorkspaceId;
use tilo_device_identity::login::Device;
use tilo_identity::broker::{IdentityBrokerHandle, WorkspaceMembership};
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::workspaces::{
    BRANCH_OWN_WORKSPACE, CREATE_OWN_WORKSPACE, CREATE_WEBHOOK, CreatedWorkspace, DELETE_WEBHOOK,
    DELETE_WORKSPACE, LIST_WEBHOOKS, REVOKE_KEY, RUN_UNATTENDED, WorkspaceLabels, WorkspaceListing,
};
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::agents::agent_keys::{AgentSigner, Signed, this_device};
use tilo_workspace_service::apps::{
    collab_broker::{AppActionBroker, BrokerHandle, FanoutState},
    collab_core::{BrokerChannels, BrokerSealChecks},
    collab_history::CollabHistoryStore,
    collab_outbound::PeerPublisher,
    services::{
        HostedWorkspaces, LauncherServices, ProcessServices, PublishedResources,
        ShortcutLibraryServices, WorkspaceAuthority,
    },
};
use tilo_workspace_service::service::ServiceConfig;
use tilo_workspace_service::workspaces::Refusal;
use uuid::Uuid;

/// The name the person's devices list a local MCP client under: one agent, whichever client
/// connects.
const MCP_CLIENT: &str = "mcp";

/// Durable collaboration history, where the desktop session keeps it under
/// the profile's `XDG_STATE_HOME`.
const COLLABORATION_HISTORY: &str = "state/tilo/collaboration/history";
/// The broker's durable invocation journal: a retried invocation id never
/// starts twice, across restarts too.
pub(super) const INVOCATION_JOURNAL: &str = "invocations";
/// Where the device's workspace service and key service keep their state, in the device's
/// directory.
const OWN: &str = "own";
/// The variable a Wayland client takes its connection from: the window a session started from a
/// session in a window is handed ([`Daemon::start_shell`]).
const WAYLAND_SOCKET_ENV: &str = "WAYLAND_SOCKET";

/// What a daemon says on a computer that holds no Tilo identity and has no login.
pub const SET_UP_FIRST: &str = "This computer has no Tilo identity yet: set up Tilo first, with \
     `tilo login` and the code from your Tilo tab (Set up with your agent).";

/// This computer's daemon, as the device. Drop applications first.
pub struct Daemon {
    // Retired before broker fields: retained original ownership cannot survive
    // replacement of the runtime that supplied its policy time.
    policy_origin: tilo_workspace_service::apps::policy_clock::PolicyOrigin,
    broker: BrokerHandle,
    /// The links of the shells this daemon starts ([`Self::start_shell`]).
    #[cfg(unix)]
    shell_links: Arc<tilo_workspace_service::apps::shell::ShellLinks>,
    workspaces: Arc<ServiceWorkspaces>,
    _identity: IdentityBrokerHandle,
    workspace: String,
    actor: String,
    identity_socket: PathBuf,
    blob_socket: PathBuf,
    /// The files of the workspaces whose work this device takes, each a replica of the
    /// workspace's Files record mirrored at a folder. Before the media host, whose content store
    /// they use.
    replicas: Option<Arc<take_work::files::Replicas>>,
    /// The workspaces' trees, served to the files daemon that mounts them. Before the media
    /// host, whose content store its mounts' bytes are read from.
    _files_tree: Option<super::files_tree::FilesTree>,
    /// The styles the device's installed packages offer, published in the person's own
    /// workspace and read again as the store moves.
    #[cfg(not(windows))]
    _styles: super::styles::StylesShown,
    /// The account sign-ins socket, while this login has a runtime directory to serve it in.
    #[cfg(unix)]
    account_sign_ins: Option<super::account_sign_ins::AccountSignIns>,
    _media: profile::MediaHost,
    /// The endpoint's router, held so the workspace host, the device actions, the work this
    /// device takes and a ticket's certify call answer for as long as this runtime runs.
    _endpoint_router: iroh::protocol::Router,
    /// What this device keeps as a server, when its machine is one. After the router that
    /// answers for it.
    _kept: Option<super::keep::Kept>,
    /// The person's other devices, asked from this runtime's endpoint as this device.
    other_devices: Option<crate::other_devices::OtherDevices>,
    /// The apps installed for this profile: what each publishes, before it runs here.
    apps: Arc<apps::Apps>,
    /// The server this device enters its person's workspaces through, once it knows one.
    home: Arc<super::home::Home>,
    /// The person's workspace here: the one this computer enters through, founded the first time
    /// the person needs one on a computer no ticket made.
    first: Arc<super::first_workspace::FirstWorkspace>,
    /// The device actions this daemon answers: a ticket for a new computer, and a login with one.
    tickets: Arc<Tickets>,
    /// Every invocation this device was asked for, however it was asked.
    invocations: Arc<Invocations>,
    /// The calls of the device's dispatch that wait for their person's yes, each as its asked
    /// record in the one store the workspaces here keep theirs in too.
    asked: Arc<super::asked::DeviceAsked>,
    /// The device this daemon is, as the API it serves locally acts.
    key: SigningKey,
    cert: DeviceCert,
    /// The person's account at their server, once [`Self::home`] names it.
    account: Mutex<Option<Arc<super::account::Account>>>,
    /// Tilo's API as this device, once a caller needed it: one, so an invocation of the
    /// account's begun on one connection is followed on the next.
    api: std::sync::OnceLock<Arc<tilo_api::direct::Direct>>,
    /// What hands agents' connections on the native endpoint to the device's session.
    agents: super::serve::Agents,
    place: DaemonPlace,
    /// The device's directory: its login, its synced folders' settings and its migrations.
    dir: PathBuf,
    /// The device's lock, when this daemon took it itself: one started under the lock the
    /// daemon's device part keeps ([`Self::start_locked`]) holds none of its own.
    _lock: Option<advisory_lock::FileLock>,
}

/// The services a profile is served as: the person's own runtime, or the same
/// in development.
#[derive(Clone, Copy)]
enum Serving {
    Profile,
    Dev,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.policy_origin.invalidate();
    }
}

/// Who this device is: the key and certificate its identity holds.
struct Identity {
    key: SigningKey,
    cert: DeviceCert,
    /// This computer's device directory, with its key and certificates.
    place: tilo_device_identity::login::Place,
}

impl Identity {
    /// The device this computer's identity is.
    ///
    /// # Errors
    /// Nothing certifies this computer's device key ([`SET_UP_FIRST`]).
    fn here() -> io::Result<Self> {
        let place = tilo_device_identity::login::Place::here(tilo_api::calls::device_keys())
            .map_err(io::Error::other)?;
        // This daemon lists its own device with the person's once it holds their record, so it
        // starts from its certificate before any host lists it.
        let (key, cert) = tilo_identity::trusted_device::unlisted("").map_err(|error| {
            io::Error::new(io::ErrorKind::NotFound, format!("{SET_UP_FIRST} ({error})"))
        })?;
        Ok(Self { key, cert, place })
    }

    /// Whether a ticket made this computer a device of its person's, rather than it holding their
    /// root: it keeps their workspaces as a member then, and founds none of its own.
    fn joined(&self) -> bool {
        !device_root_record().is_ok_and(|record| {
            tilo_identity::store::load_metadata_from_path(&record)
                .is_ok_and(|active| active.group_public_key == self.cert.user_public_key)
        })
    }

    /// The person's id, hex of their root, as which they act here.
    fn person(&self) -> String {
        hex::encode(self.cert.user_public_key)
    }
}

/// Found this computer's person, named `name` (unnamed while it is empty): the one way a person is
/// made ([`tilo_identity::provisioning::create_person`]). Their root, kept here, founds their own
/// workspace and certifies this device's key in `keys` with the root's authority, named for the
/// machine. An account that holds its person already makes nobody new: its root
/// certifies the device again.
///
/// # Errors
/// The identity cannot be made or kept.
pub fn found_a_person(
    keys: &tilo_device_identity::IdentityPaths,
    name: &str,
) -> io::Result<[u8; 32]> {
    found_a_person_in(&tilo_identity::store::identity_dir_from_env()?, keys, name)
}

/// Found the person of the account `uid` on this computer ([`found_a_person`]), whose root is
/// kept for that account alone: a computer serving several accounts keeps each one's apart.
///
/// # Errors
/// As [`found_a_person`].
pub fn found_account_person(
    keys: &tilo_device_identity::IdentityPaths,
    name: &str,
    uid: u32,
) -> io::Result<[u8; 32]> {
    found_a_person_in(&account_identity_dir(uid)?, keys, name)
}

fn found_a_person_in(
    dir: &std::path::Path,
    keys: &tilo_device_identity::IdentityPaths,
    name: &str,
) -> io::Result<[u8; 32]> {
    let storage = tilo_identity::provisioning::IdentityStorage::Files;
    if let Some(record) = tilo_identity::store::resolve_active_identity(dir) {
        tilo_identity::provisioning::ensure_collaboration_identity(dir, keys, storage)?;
        let metadata = tilo_identity::store::load_metadata_from_path(&record)
            .map_err(|error| io::Error::other(error.to_string()))?;
        return Ok(metadata.group_public_key);
    }
    tilo_identity::provisioning::create_person(dir, keys, name.trim(), "", storage)
}

/// Where the person the account `uid` made here is kept: a directory of its own under this
/// computer's identity store, so one account's root is never read for another's.
///
/// # Errors
/// This computer keeps no identity store.
pub(crate) fn account_identity_dir(uid: u32) -> io::Result<std::path::PathBuf> {
    Ok(tilo_identity::store::identity_dir_from_env()?
        .join("accounts")
        .join(uid.to_string()))
}

/// The root this computer keeps for the account `uid` alone, when it keeps one.
///
/// # Errors
/// The identity store does not hand it over.
pub(crate) fn root_record(uid: u32) -> io::Result<std::path::PathBuf> {
    let dir = account_identity_dir(uid)?;
    tilo_identity::store::held_root(&dir)
        .or_else(|| {
            // A computer set up before each account's person was kept apart keeps the device's
            // person's root in its one identity store.
            if Some(uid) == device_person_uid() {
                let legacy = tilo_identity::store::identity_dir_from_env().ok()?;
                tilo_identity::store::held_root(&legacy)
            } else {
                None
            }
        })
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "this computer keeps no root of this person's: another of their devices does",
            )
        })
}

/// The account the device itself was set up from, when this process knows it.
fn device_person_uid() -> Option<u32> {
    tilo_api::daemon::admission::installed()
        .and_then(tilo_api::daemon::admission::Admission::device_person_uid)
}

/// The root this computer keeps for the account `uid`, if it keeps one.
///
/// # Errors
/// The identity store does not hand it over.
pub(crate) fn root_for(uid: u32) -> anyhow::Result<Option<tilo_identity::store::SigningIdentity>> {
    match root_record(uid) {
        Ok(record) => Ok(Some(tilo_identity::store::load_from_path(record)?)),
        Err(_) => Ok(None),
    }
}

/// Whether this computer keeps the account `uid`'s person's root.
#[must_use]
pub fn holds_a_root(uid: u32) -> bool {
    root_record(uid).is_ok()
}

/// Restore this computer's person from what `args` hold: their recovery words (`words`), or a
/// backup Tilo wrote (`backup`, its bytes in hex) and the passphrase it was sealed with
/// (`passphrase`). Their root is kept here, as a founded person's is, and certifies this device's
/// key in `keys`.
///
/// # Errors
/// Neither is given, they do not open, or the identity cannot be kept.
pub fn restore_a_person(
    keys: &tilo_device_identity::IdentityPaths,
    args: &Value,
) -> io::Result<[u8; 32]> {
    restore_a_person_in(&tilo_identity::store::identity_dir_from_env()?, keys, args)
}

/// Bring back the person of the account `uid` on this computer ([`restore_a_person`]), whose
/// root is kept for that account alone.
///
/// # Errors
/// As [`restore_a_person`].
pub fn restore_account_person(
    keys: &tilo_device_identity::IdentityPaths,
    args: &Value,
    uid: u32,
) -> io::Result<[u8; 32]> {
    restore_a_person_in(&account_identity_dir(uid)?, keys, args)
}

fn restore_a_person_in(
    dir: &std::path::Path,
    keys: &tilo_device_identity::IdentityPaths,
    args: &Value,
) -> io::Result<[u8; 32]> {
    let storage = tilo_identity::provisioning::IdentityStorage::Files;
    if let Some(words) = args.get("words").and_then(Value::as_str) {
        let profile = tilo_identity::provisioning::restore_from_recovery_words(
            dir, keys, words, None, storage,
        )?;
        return Ok(profile.user_public_key);
    }
    let invalid = |why: &str| io::Error::new(io::ErrorKind::InvalidInput, why.to_owned());
    let backup = args
        .get("backup")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("a restore brings the recovery words or a backup"))?;
    let bytes =
        hex::decode(backup.trim()).map_err(|_| invalid("this is not a backup Tilo wrote"))?;
    let passphrase = args
        .get("passphrase")
        .and_then(Value::as_str)
        .unwrap_or_default();
    tilo_identity::provisioning::prepare_identity_directory(dir)?;
    let file = passing_through(dir, "restoring");
    tilo_files::native::atomic::write_with_mode(&file, &bytes, 0o600)?;
    let restored = tilo_identity::provisioning::restore_from_backup(
        dir,
        keys,
        &file,
        passphrase.as_bytes(),
        storage,
    );
    drop(std::fs::remove_file(&file));
    Ok(restored?.user_public_key)
}

/// The root this device's person keeps here, if it keeps one: what signs a new computer's
/// certificate, a revocation, an agent's grant and the mark of the device that keeps it. Reading
/// it makes this process undumpable, so it is read only when one of those is asked for.
///
/// # Errors
/// The identity store does not hand it over.
pub(crate) fn root() -> anyhow::Result<Option<tilo_identity::store::SigningIdentity>> {
    match device_person_root_record() {
        Ok(record) => Ok(Some(tilo_identity::store::load_from_path(record)?)),
        Err(_) => Ok(None),
    }
}

/// The record of the root this device's person keeps here, when they keep one.
pub(crate) fn device_root_record() -> io::Result<std::path::PathBuf> {
    device_person_root_record()
}

/// Where the device's person's root is kept: in the identity store a computer was set up with,
/// else the account the device was set up from made here.
fn device_person_root_record() -> io::Result<std::path::PathBuf> {
    if let Ok(record) = tilo_identity::store::identity_dir_from_env()
        && let Some(held) = tilo_identity::store::held_root(&record)
    {
        return Ok(held);
    }
    let uid = device_person_uid().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "this computer keeps no root of its person's: another of their devices does",
        )
    })?;
    root_record(uid)
}

/// Take identity step `action` (one of the `devices` identity actions) with `args` in the identity
/// store this daemon keeps: list the identities without secrets, or, with the person's `confirm`,
/// use, remove or protect identity `id`.
///
/// # Errors
/// The step is not confirmed, names no identity, or the store refuses it.
pub fn identity_step(action: &str, args: &Value) -> io::Result<Value> {
    use tilo_device_identity::login::{
        IDENTITIES, PROTECT_IDENTITY, REMOVE_IDENTITY, USE_IDENTITY,
    };
    use tilo_identity::store;
    let dir = store::identity_dir_from_env()?;
    if action == IDENTITIES {
        return serde_json::to_value(store::list_identities(&dir)?).map_err(io::Error::other);
    }
    if args.get("confirm").and_then(Value::as_bool) != Some(true) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "an identity changes only with the person's explicit confirmation",
        ));
    }
    let id = args
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "an identity id is required"))?;
    match action {
        USE_IDENTITY => {
            store::set_active_identity(&dir, id)?;
            Ok(json!({ "id": id, "active": true }))
        }
        REMOVE_IDENTITY => {
            store::remove_identity(&dir, id)?;
            Ok(json!({ "id": id, "removed": true }))
        }
        PROTECT_IDENTITY => {
            Ok(json!({ "id": id, "protection": store::secure_identity(&dir, id)? }))
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("`{action}` is not an identity step"),
        )),
    }
}

/// The 24 recovery words of the person's root this daemon keeps.
///
/// # Errors
/// It keeps none, or the secret store does not hand it over.
pub fn recovery_words() -> io::Result<String> {
    Ok(tilo_identity::store::recovery_words(device_root_record()?)?
        .as_str()
        .to_owned())
}

/// A backup of the person's root this daemon keeps, sealed with `passphrase`: its bytes.
///
/// # Errors
/// It keeps none, or the backup cannot be written.
pub fn backup(passphrase: &str) -> io::Result<Vec<u8>> {
    let record = device_root_record()?;
    let dir = record
        .parent()
        .ok_or_else(|| io::Error::other("the root's record has no directory"))?;
    let file = passing_through(dir, "backup");
    drop(std::fs::remove_file(&file));
    let written =
        tilo_identity::store::export_portable_backup(&record, &file, passphrase.as_bytes())
            .and_then(|_| std::fs::read(&file));
    drop(std::fs::remove_file(&file));
    written
}

/// A file in `dir`, private to this daemon, that a backup passes through on its way in or out.
fn passing_through(dir: &std::path::Path, what: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    dir.join(format!(".{what}-{}-{nanos}", std::process::id()))
}

impl Daemon {
    /// The apps installed for this profile: the packages this device starts, and the processes it
    /// runs each in.
    #[must_use]
    pub fn apps(&self) -> &apps::Apps {
        &self.apps
    }

    /// A new shell link: a connection to this daemon's dispatch that it opens itself and admits as
    /// the shell of the person this device acts for, by a one-use secret no other process ever
    /// sees. Whoever holds it is the shell; hand it only to the shell this daemon starts.
    ///
    /// # Errors
    /// The dispatch's socket does not take the connection.
    #[cfg(unix)]
    pub fn shell_link(&self) -> io::Result<std::os::unix::net::UnixStream> {
        use runtime_collab::wire::shell::SHELL_MAGIC;
        use std::io::Write as _;
        let secret = self
            .shell_links
            .make(tilo_workspace_service::apps::shell::ShellCaller {
                person: self.workspaces.device_cert().user_public_key,
            });
        let mut link = std::os::unix::net::UnixStream::connect(self.broker.app_socket())?;
        link.write_all(SHELL_MAGIC)?;
        link.write_all(&secret)?;
        Ok(link)
    }

    /// The links this daemon made, for a test that shows a secret admits one connection and no
    /// second: [`Self::shell_link`] takes its own secret as it connects.
    #[cfg(test)]
    pub(crate) fn shell_links(&self) -> &Arc<tilo_workspace_service::apps::shell::ShellLinks> {
        &self.shell_links
    }

    /// Start `command` as the person's shell, handed the link this daemon made for it
    /// ([`runtime_collab::wire::shell::SHELL_FD_ENV`]): the daemon starts the shell itself, so the
    /// link passes from it to that one process and to no other of the account. A session in a
    /// window sends the connection it draws on with its start (`window`, which the native endpoint
    /// received); the shell is handed it at [`WINDOW_FD`] as `WAYLAND_SOCKET`, the variable a
    /// Wayland client takes its connection from.
    ///
    /// # Errors
    /// No link could be made, or the shell did not start.
    #[cfg(target_os = "linux")]
    pub fn start_shell(&self, mut command: Command, window: Option<OwnedFd>) -> io::Result<Child> {
        use std::os::fd::AsRawFd as _;
        /// Where the shell finds its link.
        const SHELL_FD: i32 = 3;
        /// Where a session's window connection is handed to the shell it starts: above the
        /// link, so the two never share a descriptor.
        const WINDOW_FD: i32 = 4;
        let link = self.shell_link()?;
        let mut handed = vec![(link.as_raw_fd(), SHELL_FD)];
        if let Some(window) = &window {
            handed.push((window.as_raw_fd(), WINDOW_FD));
        }
        launcher::inherit::hand_down(&mut command, handed, false);
        command.env(
            runtime_collab::wire::shell::SHELL_FD_ENV,
            SHELL_FD.to_string(),
        );
        match &window {
            Some(_) => command.env(WAYLAND_SOCKET_ENV, WINDOW_FD.to_string()),
            // The request's own environment named nothing this shell holds.
            None => command.env_remove(WAYLAND_SOCKET_ENV),
        };
        let child = command.spawn()?;
        drop((link, window));
        Ok(child)
    }

    /// The workspaces this device holds, through its opened workspace service.
    #[must_use]
    pub fn workspaces(&self) -> &tilo_workspace_service::workspaces::Workspaces {
        self.workspaces.copies()
    }

    /// Every counter of the workspace service this device runs: the documents its reads opened,
    /// and the traffic its sessions carried.
    #[must_use]
    pub fn metrics(&self) -> tilo_workspace_service::metrics::MetricsSnapshot {
        self.workspaces.service().metrics()
    }

    /// Where this device's endpoint is dialled: the address its workspaces' other devices ask
    /// its actions at (`tilo-actions/1`).
    #[must_use]
    pub fn endpoint_addr(&self) -> iroh::EndpointAddr {
        self.workspaces.endpoint().addr()
    }

    /// The device's workspace service, as the runtime serves it.
    pub(crate) fn serving(&self) -> &Arc<ServiceWorkspaces> {
        &self.workspaces
    }

    /// Answer the requests on this device's action service (`tilo-actions/1`) that name
    /// `provider` with `answers`. The workspace a request asks in admits the asking device to the
    /// action by its ordinary grants, read from this device's own copy of the workspace, before
    /// `answers` sees it; a provider decides no grant itself.
    pub fn provide(
        &self,
        provider: &'static str,
        answers: Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync>,
    ) {
        self.workspaces.copies().provide(provider, answers);
    }

    /// Open a view of the tilolang app `app` in `workspace` for `caller`, whom this device's
    /// dispatch admitted, in an instance this daemon starts with no session
    /// ([`apps::Apps::open_view`]): the device's end of its page window's link
    /// (`tilo_server_protocol::page::window`), which ends the window once closed.
    ///
    /// # Errors
    /// As [`apps::Apps::open_view`].
    #[cfg(unix)]
    pub fn open_view(
        &self,
        caller: &Caller,
        app: &str,
        workspace: &str,
        record: Option<String>,
        reveal: Option<String>,
    ) -> Result<std::os::unix::net::UnixStream, String> {
        self.apps.open_view(caller, app, workspace, record, reveal)
    }

    /// What shows this device's workspaces' apps as web pages: what a page's session asks once
    /// its device proved itself (`/w/<workspace>/<app>`), each app laid out by its own window
    /// ([`Self::open_view`]). `None` on a device that shows none.
    #[must_use]
    pub fn pages(&self) -> Option<&dyn tilo_workspace_service::app_serving::AppPages> {
        self.workspaces.service().sessions().handler.pages()
    }

    /// The device this computer is as it enters its person's workspaces, once it knows their
    /// server ([`super::home::Home::get`]).
    pub(crate) fn home(&self) -> Option<Device> {
        self.home.get()
    }

    /// What [`Self::home`] is read from, for what enters as this device.
    pub(crate) fn home_source(&self) -> Arc<super::home::Home> {
        Arc::clone(&self.home)
    }

    /// The device's key.
    pub(crate) fn key(&self) -> &SigningKey {
        &self.key
    }

    /// The person's own workspace here, once one was founded or moved in: asking founds none.
    #[must_use]
    pub fn first_workspace(&self) -> Option<&str> {
        self.first.get()
    }

    /// The device a ticket made of this computer while this daemon runs, once it logs in with
    /// one: the person's part starts again as it.
    pub(crate) fn logged_in(&self) -> tokio::sync::watch::Receiver<Option<Box<Device>>> {
        self.tickets.logged_in()
    }

    /// Tilo's API as this daemon: its own workspaces answered locally, and the person's account
    /// asked at their server ([`Self::account`]).
    ///
    /// # Errors
    /// The API server cannot be built.
    pub(crate) fn api(self: &Arc<Self>) -> io::Result<Arc<tilo_api::direct::Direct>> {
        if let Some(api) = self.api.get() {
            return Ok(Arc::clone(api));
        }
        // The API reaches the daemon without keeping it: the daemon keeps the API.
        let here = Arc::new(DaemonHere(Arc::downgrade(self))) as Arc<dyn tilo_api::session::Here>;
        let api = Arc::new(
            tilo_api::direct::Direct::here(
                here,
                self.key.clone(),
                self.cert.clone(),
                &self.cert.device_label,
            )
            .map_err(io::Error::other)?,
        );
        Ok(Arc::clone(self.api.get_or_init(|| api)))
    }

    /// The tree the person reaches in action frames at the daemon's native endpoint, and the
    /// caller they are there: the workspace this daemon serves, and the account over the
    /// login's one session.
    ///
    /// # Errors
    /// The workspace cannot be admitted to now.
    pub(crate) fn person_tree(
        self: &Arc<Self>,
    ) -> io::Result<(tilo_api::session::Provider, Caller)> {
        self.person_tree_in(&self.workspace)
    }

    /// The tree the person reaches in `workspace`, one this device is a member of, and the
    /// caller they are there.
    ///
    /// # Errors
    /// The workspace cannot be admitted to now.
    pub(crate) fn person_tree_in(
        self: &Arc<Self>,
        workspace: &str,
    ) -> io::Result<(tilo_api::session::Provider, Caller)> {
        let provider = self.provider_for(workspace)?;
        let api = self.api()?;
        let named = workspace.to_owned();
        let joined = block_on_owned(async move { api.joined(&named, Arc::new(provider)).await })
            .map_err(io::Error::other)?;
        Ok((
            joined,
            Caller::local_person(&self.actor).in_workspace(workspace),
        ))
    }

    /// The person's account at their server, asked as this device over the device action service
    /// ([`super::account`]): the server [`Self::home`] names. `None` while it names none; asked
    /// again then, so the account is there once the copy of the person's workspace arrives.
    pub(crate) fn account(&self) -> Option<Arc<super::account::Account>> {
        if let Some(account) = self.account.lock_recover().as_ref() {
            return Some(Arc::clone(account));
        }
        let person = tilo_server_protocol::servers::person_workspace_id(&self.cert.user_public_key);
        let person_key = self.cert.user_public_key;
        let home = self.home()?;
        let server = home.server_address.as_ref()?;
        let account = Arc::new(super::account::Account::at(
            &self.workspaces,
            &server.endpoint_addr,
            home.server_cert.device_public_key,
            &home.workspace,
        ));
        // The person's own workspace, whose roster lists every device of theirs, is the first
        // this device keeps: entered where the person's list says it is.
        let (own, asked) = (Arc::clone(&self.workspaces), Arc::clone(&account));
        tilo_rt::app().handle().spawn(async move {
            let Some(own_workspace) = WorkspaceId::parse(&person) else {
                return;
            };
            if own.keeps(own_workspace).await {
                return;
            }
            let entered = match asked
                .listed(&person_key, &WorkspaceLabels::new(), Some(&person))
                .await
            {
                Ok(listed) => match listed.workspaces.first().map(|entry| entry.entered_at()) {
                    Some(Ok(hint)) => own
                        .copies()
                        .hold_as_member(own_workspace, &hint)
                        .await
                        .map(drop),
                    Some(Err(why)) => Err(why),
                    None => Err("the person's list does not name it yet".to_owned()),
                },
                Err(why) => Err(why),
            };
            if let Err(why) = entered {
                tracing::warn!(%why, "the person's own workspace is not kept here yet");
            }
        });
        Some(Arc::clone(
            self.account.lock_recover().get_or_insert(account),
        ))
    }

    /// The sessions of the agent `name` names, which this computer's login was made for: each a
    /// run of its own in this daemon's workspace, as the one agent every computer of the person's
    /// knows it by, so what the person granted that agent holds on every connection.
    pub(crate) fn agent_sessions(
        self: &Arc<Self>,
        name: &str,
    ) -> tilo_mcp::SessionFactory<RuntimeActionProvider> {
        let daemon = Arc::downgrade(self);
        let name = name.to_owned();
        Arc::new(move || {
            let daemon = daemon
                .upgrade()
                .ok_or_else(|| io::Error::other("this computer's daemon is stopping"))?;
            daemon.agent_session_in(&name, &daemon.workspace)
        })
    }

    /// A session of the agent `name` names in `workspace`, one this device is a member of: a run
    /// of its own there, as the one agent every computer of the person's knows it by.
    ///
    /// # Errors
    /// The workspace cannot be admitted to now.
    pub(crate) fn agent_session_in(
        self: &Arc<Self>,
        name: &str,
        workspace: &str,
    ) -> io::Result<tilo_mcp::McpServer<RuntimeActionProvider>> {
        let policy_time = self.policy_origin.capture().map_err(io::Error::other)?;
        let (host, caller) = self.provider_for(workspace)?.agent_session(
            workspace,
            &self.actor,
            &policy_time,
            |person| {
                (
                    tilo_actions::remote::device_agent_id(person, name),
                    name.to_owned(),
                )
            },
        )?;
        // The apps of the workspace: what the agent's task programs compile against, run in this
        // daemon's processes.
        let tasks = self.task_hosts(host.clone(), caller.clone(), &self.actor);
        let run = hex::encode(rand::random::<[u8; 8]>());
        tilo_mcp::McpServer::admitted(host, caller)
            .map(|server| server.with_agent_session(run).with_task_hosts(tasks))
    }

    /// The device's directory.
    pub(crate) fn dir(&self) -> PathBuf {
        self.dir.clone()
    }

    /// The key of the device this daemon is.
    pub(crate) const fn device_key(&self) -> [u8; 32] {
        self.cert.device_public_key
    }

    /// Where this daemon's connections reach it.
    pub(crate) fn place(&self) -> &DaemonPlace {
        &self.place
    }

    /// What hands agents' connections to the device's session while one holds it.
    pub(crate) fn agents(&self) -> &super::serve::Agents {
        &self.agents
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one start, each service opened in the order the next ones need it"
    )]
    fn start(
        root: &Path,
        stage: &mut dyn FnMut(&str),
        serving: Serving,
        (locked, entered): (Option<[u8; 32]>, Option<Device>),
    ) -> io::Result<Self> {
        // A server keeps the workspaces placed on it: it takes no other server's work. A node
        // whose unit points at an install token waits for its page to install its certificate
        // before anything else is read: until then, this machine is nobody's device.
        let mut keeping = super::keep::Keeping::configured()?;
        if let Some(keeping) = keeping.as_ref() {
            keeping.enroll()?;
        }
        let identity = Identity::here()?;
        tilo_files::native::private::directory(identity.place.dir())?;
        let place = tilo_api::daemon::this_computer().map_err(io::Error::other)?;
        let lock = match locked {
            Some(device) if device == identity.key.verifying_key().to_bytes() => None,
            Some(_) => {
                return Err(io::Error::other(
                    "the device key this daemon locked at its start is not the one its person's \
                     certificate names",
                ));
            }
            None => {
                stage("locking the device");
                Some(super::serve::lock_device(&place)?)
            }
        };
        tilo_files::native::private::directory(root)?;
        let sockets = &crate::daemon::sockets(root)?;
        let dev = matches!(serving, Serving::Dev);
        let actor = identity.person();
        let joined = identity.joined();
        let (workspace_id, founded) = if joined {
            (
                tilo_server_protocol::servers::person_workspace_id(&identity.cert.user_public_key),
                None,
            )
        } else if keeping.is_some() {
            // A server is signed in by its person's ticket; it founds no one of its own.
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                super::keep::SIGN_IN_FIRST,
            ));
        } else {
            // The person's own workspace, at the id their key names: the one a device a ticket
            // of this computer's makes enters, as the `joined` arm above names it.
            let founded = person_workspace(&identity.cert.user_public_key);
            (founded.to_string(), Some(founded))
        };
        let apps = Arc::new(apps::Apps::from_env(root, sockets));
        let brokers = Arc::new(Mutex::new(channels(root)));
        let fanout = Arc::new(FanoutState::new());
        // What apps may found records with here: the kinds their installed
        // manifests declare, or, in dev mode, the kinds a build names.
        let creation = Arc::new(RecordCreation {
            declared_kinds: Box::new({
                let apps = Arc::clone(&apps);
                move |app: &str| apps.declared_resource_kinds(app)
            }),
            dev,
        });
        // The device's workspace service, holding the member copies of the
        // workspaces the login takes part in, or the workspace founded here.
        stage("opening the device's workspace service");
        // What answers the person's keys and serves the workspaces' apps as pages: a computer
        // that founded its person's workspace, and a keeping server, which shows the pages its
        // web listener serves.
        let key_sessions =
            (founded.is_some() || keeping.is_some()).then(crate::key_sessions::KeySessions::new);
        // A device a ticket made takes the work of the server it enters through; a server
        // keeps the workspaces placed on it and takes none.
        let member = joined && keeping.is_none();
        let port = configured_port()?.or_else(|| member.then_some(take_work::PORT));
        let (endpoint, relay) = match keeping.as_mut() {
            Some(keeping) => {
                let (endpoint, relay) = keeping.bind(&identity.key, port)?;
                (endpoint, Some(relay))
            }
            None => (listen_endpoint(&identity.key, port)?, None),
        };
        let itself = keeping
            .as_ref()
            .map(|_| {
                super::keep::Keeping::itself(
                    &identity.key,
                    &identity.cert,
                    &identity.place.dir().join(OWN),
                )
            })
            .transpose()?;
        let workspaces = open_workspaces(
            &identity,
            endpoint,
            Arc::clone(&brokers),
            key_sessions.as_ref(),
            keeping.as_ref().zip(itself.clone()),
        )?;
        // The records the device kept before anyone claimed it: its settings. The person's
        // services carry them into their own workspace, where they read them through the same
        // record API, and the settings actions write the one store from then on.
        let records = Arc::new(super::records::DeviceRecords::at(
            identity.place.dir(),
            identity.key.verifying_key().to_bytes(),
        ));
        records.carry_into(&identity.cert.user_public_key);
        workspaces.hold_device_records(Arc::clone(&records));
        workspaces
            .copies()
            .publish_own(
                tilo_actions::remote::DEVICE_SETTINGS,
                tilo_workspace_service::workspaces::OwnReaders::Workspace,
                records,
            );
        let home = Arc::new(super::home::Home::new(
            Arc::clone(&workspaces),
            identity.cert.clone(),
            entered,
        ));
        let take_work = member
            .then(|| take_work::Server::from_login(home.get().as_ref()))
            .flatten();
        // The person's workspace here: their own on a device a ticket made, else the one this
        // computer founds for them the first time they need one.
        let first = super::first_workspace::FirstWorkspace::open(
            Arc::clone(&workspaces),
            joined.then_some(workspace_id.as_str()),
            identity.place.dir(),
            &identity.cert.user_public_key,
        )?;
        // A server founds and keeps the workspaces its configuration lists of its
        // own: a deployment that serves one names it there, and the server's device
        // makes it on the first start and keeps it from then on.
        if let Some(keeping) = keeping.as_ref() {
            let listed = keeping.kept()?;
            if !listed.is_empty() {
                stage("founding the workspaces this server keeps");
                for (id, name) in listed {
                    tilo_rt::app()
                        .handle()
                        .block_on(workspaces.found(id, &name))
                        .map_err(io::Error::other)?;
                }
            }
        }
        // The workspaces' Linux work: the Terminal environments of the runtime beside this
        // device, and on a server that pays for their machines, the work on their devices that
        // run Linux, whose Browser its host serves.
        #[cfg(target_os = "linux")]
        let environments = Arc::new(super::environments::Environments::here());
        #[cfg(target_os = "linux")]
        let browser_sessions = environments.sessions();
        #[cfg(not(target_os = "linux"))]
        let browser_sessions = None;
        // What a server keeps beyond its copies, on the workspace service it governs.
        let serves_pages = keeping.is_some();
        let kept = match (keeping, relay, itself) {
            (Some(keeping), Some(relay), Some(itself)) => {
                stage("keeping what this server keeps");
                Some(keeping.keep(super::keep::On {
                    dir: &identity.place.dir().join(OWN),
                    joined,
                    identity: itself,
                    service: Arc::clone(workspaces.service()),
                    keys: workspaces.keys(),
                    endpoint: workspaces.endpoint(),
                    relay,
                    app_serving: key_sessions.as_ref().map(|sessions| {
                        Arc::clone(sessions)
                            as Arc<dyn tilo_workspace_service::app_serving::AppActionServer>
                    }),
                    browser_sessions,
                })?)
            }
            _ => None,
        };
        let (kept, kept_protocols) = kept.map_or((None, Vec::new()), |(kept, protocols)| {
            (Some(kept), protocols)
        });
        // A server's update turn, as the update actions take it: the workspaces
        // this computer governs, and where it keeps the image it restarted into.
        let rollout = kept.as_ref().and_then(super::keep::Kept::machine);
        // The machine's controls, as the `server` actions carry them out: the same workspaces,
        // and the state root its root helper reads and answers at.
        let controls = kept.as_ref().and_then(super::keep::Kept::controls);
        // This computer keeps the workspaces its person makes on it as their own, labelled for
        // their programs and listed by the account's keys. A computer that logged in reaches
        // those through the server that keeps them; one that founded its person here keeps them
        // itself.
        let own = !joined;
        if own {
            workspaces.offer_workspaces_here(&identity.cert.device_label);
            tilo_rt::app()
                .handle()
                .block_on(workspaces.claim_workspaces_here(&identity.cert.user_public_key));
        }
        // What the person granted apps in a workspace, as the service that holds its governance
        // answers: what every launch here hands the app it starts.
        apps.keep_launch_grants(Arc::clone(&workspaces) as Arc<dyn apps::LaunchGrantsSource>);
        // The member an app's code that shows nothing runs as, here: the device's own.
        apps.keep_person(&actor);
        workspaces.follow_admitted_writes(Arc::clone(&brokers), Arc::clone(&fanout));
        // The files of the workspaces whose work this device takes, once the broker that carries
        // their records runs: where a member taking a workspace's work runs it.
        let replica_slot = take_work::files::ReplicaSlot::default();
        let taking = take_work.is_some();
        let take_work =
            take_work_action_service(root, &workspaces, take_work, &replica_slot, stage)?;
        // The one action service the daemon's router answers the device actions on: the work
        // this member takes from its server when it takes any, else its workspaces'. A ticket's
        // certify call is admitted on it while a computer asks.
        let actions = take_work.as_ref().map_or_else(
            || workspaces.copies().action_service(),
            take_work::TakesWork::actions,
        );
        // The workspaces this device governs run their Terminal sessions' shells here: the
        // dispatch answers the records' `run` and `open-terminal` with ush's shells, and the
        // endpoint takes the streams it hands out. A member that takes its server's work runs
        // them for its server instead, and answers none itself.
        #[cfg(target_os = "linux")]
        let terminals: TerminalProtocols = {
            let mut terminals: TerminalProtocols = Vec::new();
            if !taking {
                stage("opening the workspaces' shells");
                // This computer is the machine: its person's own workspace runs its programs here,
                // as a desktop install's Terminal does, in ush's Host environment.
                let shells = super::take_work::workspace_shells(
                    root,
                    &workspaces,
                    Some(replica_slot.clone()),
                    None,
                )
                .on_machine(true);
                let host = Arc::new(super::terminal::HostShells::new(
                    shells,
                    workspaces.copies().clone(),
                ));
                // A stream is taken by the device that asked for it, as the endpoint that dials
                // asks on the action service: the device's own endpoint is the device.
                let streams = Arc::clone(&host);
                let dialling = actions.clone();
                terminals.push((
                    ush::environment_terminals::ALPN,
                    Box::new(ush::environment_terminals::quic::Route::new(
                        move |endpoint, from, to| {
                            let device = dialling.device_of(endpoint);
                            if let Err(error) = streams.serve_terminal(device, from, to) {
                                tracing::debug!(%error, "a terminal stream was refused");
                            }
                        },
                    )),
                ));
                // A command's output too long for its record is kept on the shelf its record's
                // content is read from, once the device's content store is open.
                let shelf = Arc::clone(&workspaces);
                let terminal = tilo_workspace_service::workspaces::terminal::TerminalService::new(
                    workspaces.copies().clone(),
                    host,
                    tilo_rt::app().handle().clone(),
                )
                .keeping_output(Arc::new(
                    move |workspace: &str, hash: &str, bytes: &[u8]| {
                        let shelf = shelf
                            .content_shelf()
                            .ok_or("this device's content store is not open")?;
                        shelf.keep(
                            &tilo_workspace_service::content::ContentScope {
                                workspace: workspace.to_owned(),
                                app: tilo_workspace_service::workspaces::terminal::TERMINAL_APP
                                    .to_owned(),
                            },
                            hash,
                            bytes,
                        )
                    },
                ));
                workspaces.copies().provide(
                    tilo_workspace_service::workspaces::terminal::PROVIDER,
                    Arc::new(terminal),
                );
            }
            terminals
        };
        #[cfg(not(target_os = "linux"))]
        let terminals: TerminalProtocols = Vec::new();
        // The device actions the daemon answers itself.
        stage("opening the device's tickets");
        let tickets = Arc::new({
            let _entered = tilo_rt::app().handle().enter();
            Tickets::new(
                (identity.key.clone(), identity.cert.clone()),
                workspaces.endpoint(),
                joined.then(|| Arc::clone(&home)),
                Arc::new(self::root),
                Some(Arc::clone(&first)),
                Some(Arc::clone(&workspaces)),
                identity.place.clone(),
                actions.clone(),
            )
            .with_rollout(rollout)
            .with_controls(controls)
        });
        // A planned machine's ticket this daemon issued before it last stopped is served again.
        let restoring = Arc::clone(&tickets);
        tilo_rt::app()
            .handle()
            .spawn(async move { restoring.restore().await });
        // What the person's other devices ask of this one: the device actions that are its own.
        workspaces.copies().provide(
            tilo_device_identity::login::PROVIDER,
            Arc::new(super::ticket::OnThisDevice(Tickets::clone(&tickets))),
        );
        stage("serving the device's endpoint");
        let endpoint_router = serve_device_endpoint(EndpointServing {
            workspaces: &workspaces,
            take_work,
            actions,
            host: (founded.is_some() || kept.is_some()).then(|| workspaces.service().as_ref()),
            kept: kept_protocols,
            terminals,
        });
        // What is the person's alone is kept in their own workspace, read and written here on
        // this daemon's runtime once its service holds it: their devices, and the answers they
        // gave about this device's camera and microphone, which their other devices see and can
        // take back.
        let personal = tilo_workspace_service::workspaces::PersonResources::default();
        personal.attach(
            std::sync::Arc::new(workspaces.copies().clone()),
            tilo_rt::app().handle(),
        );
        apps.device_grants().keep_in(std::sync::Arc::new(personal));
        // The person's devices, as the record of their own workspace holds them, each revocation
        // the root's own signature: what `tilo-identity` admits a device by, and the one copy of it.
        let devices = std::sync::Arc::new(tilo_identity::sources::DevicesTrust::new());
        devices.reading({
            let workspaces = std::sync::Arc::downgrade(&workspaces);
            move || {
                workspaces
                    .upgrade()
                    .map(|workspaces| super::signing::devices_seen(&workspaces))
            }
        });
        tilo_identity::sources::install_device_trust(devices);
        // This device is one of the person's, listed with their others: what every host admits
        // it by, and where a revocation of it is marked.
        // Kept before anything is served, so the first host that reads the person's devices sees
        // this one enrolled: a device that served before its entry landed would be refused.
        {
            let (copies, own) = (workspaces.copies().clone(), identity.cert.clone());
            if let Err(error) = block_on_owned(async move {
                tilo_workspace_service::workspaces::person_workspace::devices::keep(&copies, &own)
                    .await
            }) {
                tracing::warn!(%error, "this device is not listed with its person's");
            }
        }
        // The keys this computer's agents sign with are certified here, with the root it keeps.
        tilo_workspace_service::agents::agent_keys::install_grants(std::sync::Arc::new(
            super::signing::GrantsOf {
                workspaces: std::sync::Arc::downgrade(&workspaces),
                keys: identity.place.keys().clone(),
            },
        ));
        // What the device's providers publish at fixed addresses is what an app's code calls them
        // by: each instance is handed their packages.
        apps.read_provided_from(workspaces.copies().providers());
        let identity_socket = sockets.join(IDENTITY_SOCKET);
        stage("starting the identity service");
        let identity_broker = identity_service(
            identity_socket.clone(),
            Arc::clone(&workspaces) as Arc<dyn WorkspaceMembership>,
            Arc::new(workspaces.keys()),
            &apps,
            dev,
        )?;
        stage("opening the content store");
        let media = profile::MediaHost::start(&root.join("content"), &sockets.join(BLOB_SOCKET))?;
        // The files daemons that mount this device's workspaces: their stores keep the files'
        // bytes, which the broker's record content reads and keeps through them.
        let files_daemons = Arc::new(tilo_workspace_service::files::socket::Daemons::default());
        // The device's one search index: what its apps publish and its records say, read for
        // every caller as the broker decides it may.
        let search = super::search::DeviceSearch::open(
            root,
            workspaces.copies(),
            Arc::clone(&apps) as Arc<dyn tilo_workspace_service::search::records::RecordReader>,
            &actor,
        );
        workspaces.keep_search(Arc::clone(&search));
        stage("starting the collaboration broker");
        #[cfg(unix)]
        let shell_links = Arc::default();
        #[cfg(unix)]
        let shell_dispatch: Arc<super::shell::Dispatching> = Arc::default();
        let invocations: Arc<Invocations> = Arc::default();
        let (policy_origin, broker) = start_broker(
            BrokerParts {
                root,
                sockets,
                apps: &apps,
                workspaces: &workspaces,
                keys: &workspaces.keys(),
                creation: Arc::clone(&creation),
                person: actor.clone(),
                files: Arc::clone(&files_daemons),
                #[cfg(unix)]
                shell_links: &shell_links,
                #[cfg(unix)]
                shell_dispatch: &shell_dispatch,
            },
            brokers,
            fanout,
        )?;
        // The device's record of every invocation it was asked for is what the workspace's
        // dispatch reads and follows; the workspace service writes what a model call cost into
        // it as it answers.
        // The apps and agents the broker admits read a record outside its app through the
        // device's one reader, decided by the workspace the record is in.
        if let Some(broker) = broker.app_action_broker() {
            broker.state().serve_invocations(Arc::clone(&invocations));
            let reads = tilo_workspace_service::workspaces::RecordReads::with_apps(
                workspaces.copies(),
                broker.state(),
                None,
            );
            broker.state().serve_records(Arc::clone(&reads) as _);
            search.serve(reads);
        }
        // The device's image model, which apps ask by the addresses it publishes rather than
        // through a connection of their own: one copy on this daemon's dispatch.
        #[cfg(unix)]
        if let Ok(model) =
            super::vision::ImageModel::start(&sockets.join(BLOB_SOCKET), &search, &actor)
        {
            workspaces.copies().providers().provide(
                super::vision::PROVIDER,
                super::vision::APP,
                tilo_workspace_service::workspaces::providers::Reach::ThisDevice,
                Arc::new(model),
            );
        } else {
            tracing::warn!("this device runs no image model");
        }
        workspaces.serve_invocations(Arc::clone(&invocations));
        stage("reading the calls that wait for the person");
        let asked = super::asked::DeviceAsked::new(
            asked_store(&workspaces)?,
            Arc::clone(&invocations),
            admits_again(&broker, &workspaces, &actor),
        );
        asked.withdraw_what_ended();
        // A device that keeps the person's root says so in their devices record, signed by that
        // root, and answers what their devices without it hand on; every device follows the
        // calls kept for one, to take them up or to show how its own ended.
        let held_root = self::root().ok().flatten();
        let (copies, cert) = (workspaces.copies().clone(), identity.cert.clone());
        let (serving, following) = (Tickets::clone(&tickets), Arc::downgrade(&asked));
        tilo_rt::app().handle().spawn(async move {
            if let Some(root) = held_root {
                match tilo_workspace_service::workspaces::person_workspace::devices::mark_root_holder(
                    &copies, &root, &cert,
                )
                .await
                {
                    Ok(()) => {
                        if let Some(asked) = following.upgrade() {
                            asked.serve_at_root(serving);
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "this device's root is not marked in its person's devices");
                    }
                }
            }
            copies.follow_asked_at_root(move |asks| {
                if let Some(asked) = following.upgrade() {
                    asked.kept_changed(asks);
                }
            });
        });
        // What posts an account webhook's deliveries: a webhook is a shortcut of the workspace,
        // and its run posts one event through this provider, from the device that keeps it.
        match tilo_workspace_service::apps::webhooks::Poster::new(
            workspaces.copies().clone(),
            Arc::clone(&invocations),
        ) {
            Ok(poster) => workspaces.copies().provide(
                tilo_workspace_service::workspaces::webhooks::PROVIDER,
                Arc::new(poster),
            ),
            Err(error) => tracing::warn!(%error, "this device posts no webhook"),
        }
        // An action an agent makes for the person, which asks for
        // confirmation, is put to them in the runtime's native prompt.
        // Elsewhere nobody here answers, and such an action is refused.
        #[cfg(target_os = "macos")]
        super::confirm::ask_the_person(&broker, &actor, &apps);
        if let Some(key_sessions) = &key_sessions {
            serve_key_sessions(
                key_sessions,
                (&broker, &invocations, &apps),
                &workspaces,
                (&actor, founded, serves_pages),
                &policy_origin,
            )?;
        }
        start_agents(&broker, &workspaces, &tickets);
        start_triggers(&broker, &workspaces, &invocations);
        let replicas = take_work_files(
            &TakenFiles {
                root,
                workspaces: &workspaces,
                media: &media,
                apps: &apps,
                author: take_work::files::Author {
                    person: identity.cert.user_public_key,
                    device: identity.cert.device_public_key,
                },
            },
            taking,
            founded,
            stage,
        );
        if let Some(replicas) = &replicas {
            let _ = replica_slot.set(Arc::downgrade(replicas));
            first.show_files_in(replicas);
        }
        // Device control is this device's own resource: its steps run as the units the image
        // ships, so a person's grant on `devices/<this>/vm` reaches them from boot. A device
        // without the step program offers none.
        #[cfg(target_os = "linux")]
        match super::control::device_control(identity.cert.device_public_key) {
            Ok(control) => workspaces
                .copies()
                .provide(super::control::PROVIDER, control),
            Err(why) => tracing::info!(%why, "this device offers no device control"),
        }
        // The layouts of the workspaces it keeps are read and changed through their actions, by a
        // desktop page's shell, an agent and anyone else alike, each as the caller's.
        {
            let titled = Arc::clone(&apps);
            workspaces.copies().provide(
                super::layout::PROVIDER,
                Arc::new(super::layout::LayoutActions::new(
                    Arc::clone(&workspaces),
                    Arc::new(move |app: &str| titled.display_name(app)),
                )),
            );
        }
        // So is its package cache, the one it answered before the login too.
        #[cfg(unix)]
        match super::packages::PackageCache::this_device(&identity.cert.device_public_key) {
            Ok(cache) => {
                super::packages::follow(&cache, Arc::clone(&workspaces));
                workspaces
                    .copies()
                    .provide(super::packages::PROVIDER, cache);
            }
            Err(why) => tracing::warn!(%why, "this device keeps no packages"),
        }
        // A branch made here takes its source's Terminal environments from the runtime that
        // keeps them, and its machine from the device keeping that.
        #[cfg(target_os = "linux")]
        workspaces.copies().machines(environments);
        stage("serving the workspaces' trees to the files daemon");
        let files_tree = super::files_tree::serve(
            &tilo_device_identity::login::files_tree_socket().map_err(io::Error::other)?,
            &workspaces,
            files_daemons,
            identity_socket.clone(),
            (actor.clone(), identity.key.verifying_key().to_bytes()),
        );
        // The workspaces' accounts and their sign-ins, for this login's cloud service and the
        // origin connectors this daemon starts.
        #[cfg(unix)]
        let account_sign_ins = account_sign_ins(
            &workspaces,
            Caller::authenticated(
                identity.cert.user_public_key,
                identity.cert.device_public_key,
            ),
        );
        // The origin connectors this device starts are handed their workspace's account sign-ins
        // by them.
        #[cfg(unix)]
        if let Some(sign_ins) = &account_sign_ins {
            apps.hand_sign_ins(Box::new(sign_ins.hands()));
        }
        stage("publishing the styles the installed packages offer");
        #[cfg(not(windows))]
        let styles = super::styles::publish(&workspaces);
        let daemon = Self {
            _files_tree: files_tree,
            #[cfg(not(windows))]
            _styles: styles,
            #[cfg(unix)]
            account_sign_ins,
            policy_origin,
            broker,
            #[cfg(unix)]
            shell_links,
            other_devices: other_devices(&workspaces),
            apps,
            workspaces,
            _identity: identity_broker,
            workspace: workspace_id,
            actor,
            identity_socket,
            blob_socket: sockets.join(BLOB_SOCKET),
            replicas,
            _media: media,
            _endpoint_router: endpoint_router,
            home,
            first,
            _kept: kept,
            tickets,
            invocations,
            asked,
            key: identity.key.clone(),
            cert: identity.cert,
            account: Mutex::new(None),
            api: std::sync::OnceLock::new(),
            agents: super::serve::Agents::default(),
            place,
            dir: identity.place.dir().to_path_buf(),
            _lock: lock,
        };
        // The person's shell reaches the one dispatch their own runs do, as them.
        #[cfg(unix)]
        match daemon.person() {
            Ok(person) => shell_dispatch.serve(person),
            Err(error) => tracing::warn!(%error, "this daemon's shell reaches no dispatch"),
        }
        Ok(daemon)
    }
}

/// What admits the run that asked a call which waited across the daemon's restart afresh, on
/// the person's admission here: an agent's for the same agent under the key this device keeps for
/// it, the person's own through their own dispatch. Each call of the run is checked as every
/// call is.
fn admits_again(
    broker: &BrokerHandle,
    workspaces: &Arc<ServiceWorkspaces>,
    actor: &str,
) -> super::asked::Admits {
    let (broker, workspaces, actor) = (
        broker.app_action_broker(),
        Arc::clone(workspaces),
        actor.to_owned(),
    );
    Box::new(move |run| {
        let broker = broker.as_ref().ok_or("the action broker is unavailable")?;
        let workspace = match run {
            super::asked::Run::Agent { workspace, .. }
            | super::asked::Run::Own { workspace, .. } => workspace,
        };
        let (admitting, named, actor) = (Arc::clone(&workspaces), workspace.clone(), actor.clone());
        let admission =
            block_on_owned(async move { admitting.admit_execution(&named, &actor).await })?;
        let super::asked::Run::Agent { agent, name, .. } = run else {
            // The person's own call, through their own dispatch here: only while it serves.
            admission
                .check(workspaces.copies())
                .map_err(|refusal| refusal.to_string())?;
            let provider = broker
                .certified_local_action_provider(admission.caller(), admission.presentation())?;
            let person = admission.caller().clone();
            let provider: Arc<dyn ActionProvider + Send + Sync> = Arc::new(AdmittedAgain {
                provider,
                _admission: admission,
            });
            return Ok((provider, person));
        };
        let caller = admission.caller();
        let (Some(person), Some(workspace)) = (caller.acting().person_id(), caller.workspace())
        else {
            return Err("the admitted run names no person's workspace".to_owned());
        };
        let (signer, agent) = this_device()
            .and_then(|keys| AgentSigner::grant(keys, workspace, (agent, name), person))
            .map_err(|error| error.to_string())?;
        admission
            .check(workspaces.copies())
            .map_err(|refusal| refusal.to_string())?;
        let provider: Arc<dyn ActionProvider + Send + Sync> = Arc::new(AdmittedAgain {
            provider: Arc::new(Signed::new(broker.device_host(), Some(signer))),
            _admission: admission,
        });
        Ok((provider, agent))
    })
}

/// A run admitted again for a call that waited across the daemon's restart, kept for as long as
/// its call is read.
struct AdmittedAgain {
    provider: Arc<dyn ActionProvider + Send + Sync>,
    _admission: Arc<tilo_workspace_service::workspaces::SessionAdmission>,
}

impl ActionProvider for AdmittedAgain {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        self.provider.query(caller, query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        self.provider.invoke(caller, request)
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        self.provider.observe(caller, id)
    }

    fn next_change(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<Pending<()>> {
        self.provider.next_change(caller, id)
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionCallError>> {
        self.provider.cancel(caller, id)
    }
}

/// The one store of the calls here that wait for a person's yes, which the workspaces this
/// device serves opened and answered through: each kept in its workspace's record of its asked
/// calls, so what waited before the daemon started again waits again, and answered through the
/// workspaces here, which write the grant an approve gives.
fn asked_store(
    workspaces: &Arc<ServiceWorkspaces>,
) -> io::Result<Arc<tilo_actions::remote::asked::AskedStore>> {
    workspaces.copies().asked_store().ok_or_else(|| {
        io::Error::other("the workspaces this device serves keep no calls that wait")
    })
}

/// Serve the account sign-ins below this login's runtime directory, as `person`, taking the
/// connections `links` holds a vouch for as that vouch's connector: `None`, said in the log, where
/// there is no runtime directory or the socket cannot be made there, and the cloud service and
/// origin connectors then reach no account's sign-in.
#[cfg(unix)]
fn account_sign_ins(
    workspaces: &ServiceWorkspaces,
    person: Caller,
) -> Option<super::account_sign_ins::AccountSignIns> {
    let Some(path) = accounts::sign_ins::socket() else {
        tracing::warn!(
            "no runtime directory, so the cloud service and origin connectors reach no account's \
             sign-in"
        );
        return None;
    };
    super::account_sign_ins::serve(
        &path,
        workspaces.copies().clone(),
        person,
    )
    .inspect_err(|error| {
        tracing::warn!(%error, path = %path.display(), "the account sign-ins socket is not served");
    })
    .ok()
}

/// `person`'s own workspace, at the id their key names.
fn person_workspace(person: &[u8; 32]) -> WorkspaceId {
    WorkspaceId::parse(&tilo_server_protocol::servers::person_workspace_id(person))
        .expect("a person's workspace id is a workspace id")
}

/// The systemd credential that fixes the UDP port this device's endpoint binds, for a device
/// reached through a forward to that port (a local VM's host forward, a container's published
/// port).
pub const PORT_CREDENTIAL: &str = "tilo.port";
/// [`PORT_CREDENTIAL`] where no credential is handed: a container's or a dev run's environment.
pub const PORT_ENV: &str = "TILO_DEVICE_PORT";

/// The UDP port this device's endpoint binds, when one is fixed ([`PORT_CREDENTIAL`], else
/// [`PORT_ENV`]); `None` leaves the choice to iroh, as on every device that names none.
///
/// # Errors
/// The setting names no port number.
pub(crate) fn configured_port() -> io::Result<Option<u16>> {
    port_from(
        std::env::var_os("CREDENTIALS_DIRECTORY")
            .map(PathBuf::from)
            .as_deref(),
        std::env::var(PORT_ENV).ok(),
    )
}

/// [`configured_port`], from the credentials directory and the environment's [`PORT_ENV`].
pub(crate) fn port_from(
    credentials: Option<&Path>,
    named: Option<String>,
) -> io::Result<Option<u16>> {
    let handed =
        credentials.and_then(|dir| std::fs::read_to_string(dir.join(PORT_CREDENTIAL)).ok());
    let Some(value) = handed.or(named).filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };
    value.trim().parse().map(Some).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "this device's port {value:?} ({PORT_CREDENTIAL} or {PORT_ENV}) is no UDP port number"
            ),
        )
    })
}

/// The name this computer's person gave themself, as its identity keeps it: what a workspace
/// they join shows them by. `None` before they named themself.
fn person_name() -> Option<String> {
    let dir = tilo_identity::store::identity_dir_from_env().ok()?;
    let active = tilo_identity::store::resolve_active_identity(&dir)?;
    tilo_identity::store::load_metadata_from_path(&active)
        .ok()
        .map(|metadata| metadata.display_name)
        .filter(|name| tilo_identity::store::is_chosen_display_name(name))
}

/// Open the device's workspace service in its directory: it governs the workspace founded here
/// on a computer with no login, and holds the member copies of the login's workspaces otherwise.
fn open_workspaces(
    identity: &Identity,
    endpoint: iroh::Endpoint,
    brokers: Arc<Mutex<BrokerChannels>>,
    key_sessions: Option<&Arc<crate::key_sessions::KeySessions>>,
    keeping: Option<(&super::keep::Keeping, tilo_server::daemon::ServerIdentity)>,
) -> io::Result<Arc<ServiceWorkspaces>> {
    let device = tilo_identity::identity_store::DeviceIdentity::held(
        identity.cert.clone(),
        identity.key.clone(),
        Arc::new(tilo_identity::identity_store::files::Files::new(
            identity.place.keys().clone(),
        )),
    );
    let apps = key_sessions.map(|sessions| {
        Arc::clone(sessions) as Arc<dyn tilo_workspace_service::app_serving::AppActionServer>
    });
    let (dir, key) = (identity.place.dir().join(OWN), identity.key.clone());
    let (keeping, itself) = keeping.unzip();
    let configure = keeping.map(|keeping| {
        Box::new(keeping.configuring())
            as Box<dyn FnOnce(ServiceConfig) -> ServiceConfig + Send + 'static>
    });
    block_on_owned(async move {
        ServiceWorkspaces::open(Opening {
            dir: &dir,
            key: &key,
            identity: device,
            endpoint,
            seal_checks: Arc::new(BrokerSealChecks(brokers)),
            apps,
            configure,
            itself,
        })
        .await
    })
    .map_err(io::Error::other)
}

/// The device action service this profile answers its server's work on, when it takes
/// work: served on the endpoint its other devices reach it at.
fn take_work_action_service(
    root: &Path,
    workspaces: &Arc<ServiceWorkspaces>,
    server: Option<take_work::Server>,
    replicas: &take_work::files::ReplicaSlot,
    stage: &mut dyn FnMut(&str),
) -> io::Result<Option<take_work::TakesWork>> {
    let Some(server) = server else {
        return Ok(None);
    };
    stage("taking the work of the workspaces relayed here");
    // Browser environments start on the hosted-VM service this host's
    // environment names, when it runs one.
    take_work::action_service(root, workspaces, server, Some(Arc::clone(replicas)), None).map(Some)
}

/// What the broker's services are opened from: the device's workspaces and the apps.
struct BrokerParts<'a> {
    root: &'a Path,
    /// The directory the daemon's sockets are in.
    sockets: &'a Path,
    apps: &'a Arc<apps::Apps>,
    workspaces: &'a Arc<ServiceWorkspaces>,
    keys: &'a tilo_workspace_service::apps::workspace_keys::KeyProvider,
    creation: Arc<RecordCreation>,
    /// The person this device acts for.
    person: String,
    /// The files daemons mounting this device's workspaces, whose stores keep their files' bytes.
    files: Arc<tilo_workspace_service::files::socket::Daemons>,
    /// The links of the shells this daemon starts.
    #[cfg(unix)]
    shell_links: &'a Arc<tilo_workspace_service::apps::shell::ShellLinks>,
    /// The dispatch those shells' calls reach, once the broker serves.
    #[cfg(unix)]
    shell_dispatch: &'a Arc<super::shell::Dispatching>,
}

/// Start the collaboration broker over this profile's services.
fn start_broker(
    parts: BrokerParts<'_>,
    channels: Arc<Mutex<BrokerChannels>>,
    fanout: Arc<FanoutState>,
) -> io::Result<(
    tilo_workspace_service::apps::policy_clock::PolicyOrigin,
    BrokerHandle,
)> {
    let (policy_origin, policy_time) = execution_policy_clock()?;
    let broker = tilo_workspace_service::apps::collab_broker::spawn_with_services(
        parts.sockets.join(COLLAB_SOCKET),
        Some(Arc::clone(parts.workspaces) as Arc<dyn WorkspaceAuthority>),
        PeerPublisher::with_optional_commits(
            None,
            ServiceCommits::new(parts.workspaces, Arc::clone(&parts.creation))
                as Arc<dyn tilo_workspace_service::apps::workspace_commits::WorkspaceCommits>,
        ),
        None,
        broker_services(parts, policy_time)?,
        channels,
        fanout,
    )?;
    Ok((policy_origin, broker))
}

/// The services the collaboration broker runs with, including the person's
/// own store: the records of their own scope are kept in it as well as here,
/// so their other devices have them.
fn broker_services(
    parts: BrokerParts<'_>,
    policy_time: tilo_workspace_service::apps::policy_clock::PolicyTime,
) -> io::Result<ProcessServices> {
    let document_content = super::content::document_content(
        parts.workspaces,
        &parts.sockets.join(BLOB_SOCKET),
        Arc::clone(&parts.files),
    );
    // Where this workspace's offloadable actions may run instead of here: the
    // other devices of the invoking workspace are asked to take the turn over
    // the endpoint this profile syncs on, and the content an offloaded run
    // answers with is the content this broker routes.
    let action_offload =
        parts
            .workspaces
            .turn_action_service()
            .and_then(|(action_service, endpoint, runtime)| {
                crate::offload::turn_route(
                    action_service,
                    endpoint,
                    runtime,
                    Arc::clone(&document_content),
                )
            });
    // The turns this device takes for its workspaces' devices run here, over
    // the same content: whether a turn is this device's is decided live, each
    // time it is asked.
    #[cfg(target_os = "linux")]
    {
        use super::compute::{ComputeJobs, InstalledComputeCatalogue, LauncherComputeSandbox};
        let _entered = tilo_rt::app().handle().enter();
        parts
            .workspaces
            .copies()
            .run_jobs(Arc::new(ComputeJobs::new(
                Arc::new(Arc::clone(&document_content)),
                Arc::new(InstalledComputeCatalogue),
                Arc::new(LauncherComputeSandbox::new(None)),
                parts.workspaces.signer().device_public_key(),
                (
                    parts.root.join("compute/apps"),
                    parts.root.join("compute/jobs"),
                ),
            )));
    }
    // The hosts of the workspaces placed on another device: the one dispatch sends a call of a
    // typed action of one of their records there, as the broker sends its apps' invocations.
    let hosts = super::hosts::Hosts::of(parts.workspaces);
    let placed = Arc::clone(&hosts);
    parts
        .workspaces
        .copies()
        .providers()
        .place_on_hosts(Arc::new(move |workspace: &str| {
            placed.host_actions(workspace)
        }));
    // This device's server settings: the one copy its daemon keeps, read and changed by the apps
    // attached to it here and by a desktop session's apps over the shell link.
    let server_settings: Arc<dyn PublishedResources> = Arc::new(
        super::server_settings::Published::new(super::server_settings::Served::this_device()),
    );
    // Everything this device publishes to the apps attached to it: each resource served by the
    // provider that keeps it, through one table.
    let published: Arc<dyn PublishedResources> =
        Arc::new(super::published::Published::new().serving(
            &[super::server_settings::SERVER],
            Arc::clone(&server_settings),
        ));
    Ok(ProcessServices {
        // The records of the workspaces this device keeps, read for the apps, agents and programs
        // it serves: each capture opens the record's document here, and counts it.
        committed_documents: Some(Arc::new(
            tilo_workspace_service::apps::service_reads::ServiceReads::new(
                parts.workspaces.copies().clone(),
                parts.workspaces.service().metrics_counter(),
            ),
        )
            as Arc<dyn tilo_workspace_service::apps::services::NativeDocumentReads>),
        native_certificates: Some(Arc::new(Certificates {
            policy_time,
            workspaces: Arc::downgrade(parts.workspaces),
        })),
        launchable_catalogue: Some(parts.root.join("state/launchable-actions.json")),
        launcher: Some(Arc::clone(parts.apps) as Arc<dyn LauncherServices>),
        app_action_access: Some(parts.apps.access_resolver()),
        app_resource_kinds: Some(parts.apps.resource_kinds_resolver()),
        workspace_grants: Some(Arc::new(MemberListGrants {
            workspaces: Arc::clone(parts.workspaces),
            person: parts.person,
            apps: Arc::clone(parts.apps),
        })),
        // Who is in each workspace this host holds, for the apps it serves: their names and the
        // picture each person set in the profile kept here.
        workspace_memberships: Some(tilo_workspace_service::workspaces::ProfileMemberships::new(
            parts.workspaces.copies().clone(),
            Arc::clone(parts.workspaces)
                as tilo_workspace_service::apps::services::WorkspaceAuthorityHandle,
        )
            as Arc<dyn tilo_workspace_service::apps::services::WorkspaceMemberships>),
        journal_root: Some(parts.root.join(INVOCATION_JOURNAL)),
        // What a caller puts into a workspace here is kept on the shelf its records' content is.
        content_store: parts
            .workspaces
            .content_shelf()
            .map(tilo_workspace_service::content::shelf_puts),
        document_content: Some(document_content),
        // The person's shortcuts, saved into the workspaces this device holds and run here: the
        // same store the trigger runner arms them from, so a computer keeps its shortcuts with
        // no desktop session open on it.
        // Who is present on which device, as this device's workspaces hear it: their links, and
        // the agent sessions this daemon serves.
        present_devices: Some(Arc::new(parts.workspaces.copies().clone())
            as Arc<dyn tilo_workspace_service::workspaces::PresentDevices>),
        shortcut_library: Some(Arc::new(super::shortcuts::Shortcuts::new(
            parts.workspaces.copies().clone(),
            parts.workspaces.device_cert().clone(),
            parts.workspaces.signer(),
        )?) as Arc<dyn ShortcutLibraryServices>),
        // This device's server settings, as the apps attached to this daemon read and change them:
        // the work its machine takes and the services it offers, saved to the configuration the
        // daemon keeps.
        published: Some(Arc::clone(&published)),
        // The service the identity service answers key requests from:
        // endpoints ask again when it says a channel changed.
        group_keys: Some(parts.keys.clone()),
        action_offload,
        // The person's desktop session: their workspaces' documents, governance and standing,
        // reached through this device alone.
        #[cfg(unix)]
        shell: Some(Arc::new(super::shell::services(
            parts.workspaces,
            parts.shell_links,
            parts.sockets.join(COLLAB_SOCKET),
            Arc::clone(parts.shell_dispatch),
            published,
            shell_processes(parts.apps, parts.workspaces),
        ))),
        // A workspace placed on a host has its records changed there: the person's actions on
        // them are sent to the host, which decides and runs them, and the change reaches this
        // device's copy by sync.
        hosted_workspaces: Some(Arc::clone(&hosts) as Arc<dyn HostedWorkspaces>),
        // The providers registered on this device ([`Daemon::provide`]): a call made here reaches
        // them through the same registry another device's call does.
        providers: Some(parts.workspaces.copies().providers()),
        // What an app publishes of what it holds, on its connection here: the device's one
        // search index keeps it after the app closes.
        search_index: parts.workspaces.search_publications(),
        ..ProcessServices::default()
    })
}

/// The device's tilolang processes, as the person's shell hands them its windows' instances on
/// its link: for the workspaces this device has for them alone. `None` where no process can start.
#[cfg(unix)]
fn shell_processes(
    apps: &Arc<apps::Apps>,
    workspaces: &Arc<ServiceWorkspaces>,
) -> Option<tilo_workspace_service::apps::shell::ShellProcesses> {
    #[cfg(target_os = "linux")]
    {
        use tilo_peer_state::workspace_documents::DeviceDocuments as _;
        let processes = apps
            .processes()
            .map_err(|why| tracing::warn!(%why, "no tilolang process can start on this device"))
            .ok()?;
        let documents = tilo_workspace_service::device_documents::ServiceDocuments::of(
            workspaces.copies().clone(),
        );
        Some(processes.for_the_shell(
            Box::new(move |workspace| documents.workspaces().iter().any(|has| has == workspace)),
            Arc::clone(apps) as Arc<dyn crate::device::processes::Origins>,
        ))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (apps, workspaces);
        None
    }
}

fn execution_policy_clock() -> io::Result<(
    tilo_workspace_service::apps::policy_clock::PolicyOrigin,
    tilo_workspace_service::apps::policy_clock::PolicyTime,
)> {
    let origin = tilo_workspace_service::apps::policy_clock::OwnerPolicyClock::new()
        .map_err(io::Error::other)?
        .new_origin();
    let time = origin.capture().map_err(io::Error::other)?;
    Ok((origin, time))
}

impl Daemon {
    /// Start the person's services under the device lock this daemon took at its start
    /// ([`super::unclaimed::ThisDevice`]), in dev mode when `dev`.
    ///
    /// # Errors
    /// As [`RuntimeServices::start_reporting`], and when the locked key is not the person's
    /// device's.
    pub(crate) fn start_locked(
        root: &Path,
        stage: &mut dyn FnMut(&str),
        dev: bool,
        (device, entered): ([u8; 32], Option<Device>),
    ) -> io::Result<Self> {
        let serving = if dev { Serving::Dev } else { Serving::Profile };
        Self::start(root, stage, serving, (Some(device), entered))
    }
}

impl RuntimeServices for Daemon {
    type ActionProvider = RuntimeActionProvider;

    fn start_reporting(root: &Path, stage: &mut dyn FnMut(&str)) -> io::Result<Self> {
        Self::start(root, stage, Serving::Profile, (None, None))
    }

    fn start_in_dev_mode(root: &Path, stage: &mut dyn FnMut(&str)) -> io::Result<Self> {
        Self::start(root, stage, Serving::Dev, (None, None))
    }

    /// The person's other devices, as this device asks them.
    fn other_devices(&self) -> Option<crate::other_devices::OtherDevices> {
        self.other_devices.clone()
    }

    /// The person's workspace here, founded now on a computer with no login that has none yet.
    fn workspace(&self) -> io::Result<String> {
        self.first.found_blocking().map_err(io::Error::other)
    }

    fn mcp_workspace(&self, requested: Option<&str>) -> io::Result<String> {
        let workspace = requested.unwrap_or(&self.workspace);
        if workspace == self.workspace {
            return Ok(self.workspace.clone());
        }
        self.workspaces
            .action_workspace(&self.actor, workspace)
            .map_err(|error| io::Error::new(io::ErrorKind::PermissionDenied, error))
    }

    fn actor(&self) -> &str {
        &self.actor
    }

    fn action_provider(&self) -> io::Result<RuntimeActionProvider> {
        self.provider_for(&self.workspace)
    }

    fn mcp_session_factory(
        &self,
        requested: &str,
    ) -> io::Result<tilo_mcp::SessionFactory<RuntimeActionProvider>> {
        let workspace = self.mcp_workspace(Some(requested))?;
        let actor = self.actor.clone();
        let host = self.action_provider()?;
        Ok(host.session_factory(
            workspace,
            actor,
            self.policy_origin.capture().map_err(io::Error::other)?,
        ))
    }

    fn launch(&self, app_id: &str, command: Command) -> io::Result<Child> {
        let child = host::launch(
            command,
            app_id,
            &self.workspace,
            &host::ServiceSockets {
                collab: self.broker.app_socket(),
                identity: &self.identity_socket,
                blobs: &self.blob_socket,
            },
        )?;
        Ok(child)
    }

    fn media(&self) -> Option<Arc<dyn tilo_mcp::MediaReader>> {
        Some(Arc::new(profile::BlobMedia::new(&self.blob_socket, {
            let workspace = self.workspace.clone();
            Box::new(move || Ok(workspace.clone()))
        })))
    }
}

impl Daemon {
    /// The person's provider in `workspace`: every action of one answered locally, as apps and the
    /// person's input are.
    ///
    /// # Errors
    /// The workspace is not one this device may act in, or its admission cannot be opened.
    fn provider_for(&self, workspace: &str) -> io::Result<RuntimeActionProvider> {
        self.person()?.provider(workspace)
    }

    /// The one dispatch the person's own calls on this device reach, as them.
    fn person(&self) -> io::Result<PersonDispatch> {
        Ok(PersonDispatch {
            workspaces: Arc::clone(&self.workspaces),
            actor: self.actor.clone(),
            broker: self
                .broker
                .app_action_broker()
                .ok_or_else(|| io::Error::other("the action broker is unavailable"))?,
            apps: Arc::clone(&self.apps),
            policy_time: self.policy_origin.capture().map_err(io::Error::other)?,
            tickets: Arc::clone(&self.tickets),
            invocations: Arc::clone(&self.invocations),
            asked: Arc::clone(&self.asked),
        })
    }
}

/// The one dispatch the person's own calls on this device reach (`tilo do`, their shell), in any
/// workspace it holds: admitted as the person, and decided by what they may do there.
#[derive(Clone)]
pub(super) struct PersonDispatch {
    workspaces: Arc<ServiceWorkspaces>,
    actor: String,
    broker: AppActionBroker,
    apps: Arc<apps::Apps>,
    policy_time: tilo_workspace_service::apps::policy_clock::PolicyTime,
    tickets: Arc<Tickets>,
    invocations: Arc<Invocations>,
    asked: Arc<super::asked::DeviceAsked>,
}

impl PersonDispatch {
    /// Who the person's calls in `workspace` are made as.
    pub(super) fn caller(&self, workspace: &str) -> Caller {
        Caller::local_person(&self.actor).in_workspace(workspace)
    }

    /// The dispatch the person's calls in `workspace` reach.
    pub(super) fn provider(&self, workspace: &str) -> io::Result<RuntimeActionProvider> {
        let workspaces = Arc::clone(&self.workspaces);
        let actor = self.actor.clone();
        let named = workspace.to_owned();
        let admission =
            block_on_owned(async move { workspaces.admit_execution(&named, &actor).await })
                .map_err(io::Error::other)?;
        let provider = self
            .broker
            .certified_local_action_provider(admission.caller(), admission.presentation())
            .map_err(io::Error::other)?;
        admission
            .check(self.workspaces.copies())
            .map_err(io::Error::other)?;
        Ok(RuntimeActionProvider {
            broker: self.broker.clone(),
            workspace: workspace.to_owned(),
            workspaces: Arc::clone(&self.workspaces),
            apps: Some(Arc::clone(&self.apps)),
            session: Some(Arc::new(AdmittedRuntimeSession {
                policy_time: self.policy_time.clone(),
                provider,
                admission,
                agent: None,
                projection: Some(self.caller(workspace)),
            })),
            tickets: Some(Arc::clone(&self.tickets)),
            invocations: Arc::clone(&self.invocations),
            asked: Some(Arc::clone(&self.asked)),
        })
    }
}

impl Daemon {
    /// The sessions of an account signed in on this computer as another person than the
    /// device's, each a run of `key` (certified by `cert`) in the device's workspace: its calls
    /// act as that key, and the workspace grants it what it grants its person, never what the
    /// device's own person holds.
    pub(crate) fn key_sessions(
        self: &Arc<Self>,
        key: SigningKey,
        cert: DeviceCert,
    ) -> tilo_mcp::SessionFactory<RuntimeActionProvider> {
        let daemon = Arc::downgrade(self);
        Arc::new(move || {
            let daemon = daemon
                .upgrade()
                .ok_or_else(|| io::Error::other("this computer's daemon is stopping"))?;
            let (provider, caller) = daemon.key_tree(&key, &cert)?;
            tilo_mcp::McpServer::admitted(provider, caller)
        })
    }

    /// A run of `key` (certified by `cert`) in the device's workspace, and the caller its calls
    /// go as: the key itself ([`Self::provider_for_key`]).
    ///
    /// # Errors
    /// The certificate does not hold, or the workspace does not admit the key.
    pub(crate) fn key_tree(
        &self,
        key: &SigningKey,
        cert: &DeviceCert,
    ) -> io::Result<(RuntimeActionProvider, Caller)> {
        let key = tilo_api::grant_key::GrantKey::held(
            key.clone(),
            cert.clone(),
            tilo_clock::unix_now_ns(),
        )
        .map_err(|refused| io::Error::other(refused.to_string()))?;
        let provider = self.provider_for_key(&self.workspace()?, &key)?;
        let caller = provider
            .admission_caller()
            .ok_or_else(|| io::Error::other("the key's run was not admitted"))?;
        Ok((provider, caller))
    }
}

impl Daemon {
    /// A run of the key `key` in `workspace`: its calls are the key's own, admitted by the
    /// certificate its person gave it and decided by what the workspace grants that key
    /// ([`ServiceWorkspaces::admit_key`]), never by what its person may do.
    fn provider_for_key(
        &self,
        workspace: &str,
        key: &tilo_api::grant_key::GrantKey,
    ) -> io::Result<RuntimeActionProvider> {
        let workspaces = Arc::clone(&self.workspaces);
        let (named, signing, cert) = (workspace.to_owned(), key.key().clone(), key.cert().clone());
        let admission =
            block_on_owned(async move { workspaces.admit_key(&named, &signing, &cert).await })
                .map_err(io::Error::other)?;
        let broker = self
            .broker
            .app_action_broker()
            .ok_or_else(|| io::Error::other("the action broker is unavailable"))?;
        let provider = broker
            .certified_local_action_provider(admission.caller(), admission.presentation())
            .map_err(io::Error::other)?;
        admission
            .check(self.workspaces.copies())
            .map_err(io::Error::other)?;
        Ok(RuntimeActionProvider {
            broker,
            workspace: workspace.to_owned(),
            workspaces: Arc::clone(&self.workspaces),
            apps: Some(Arc::clone(&self.apps)),
            session: Some(Arc::new(AdmittedRuntimeSession {
                policy_time: self.policy_origin.capture().map_err(io::Error::other)?,
                provider,
                agent: Some(admission.caller().clone()),
                admission,
                projection: None,
            })),
            // A key reads what the device is, as the person's agents do; only the person lets
            // another device in ([`RuntimeActionProvider::is_the_person`]).
            tickets: Some(Arc::clone(&self.tickets)),
            invocations: Arc::clone(&self.invocations),
            asked: Some(Arc::clone(&self.asked)),
        })
    }
}

impl Daemon {
    /// Where the task programs of an API this daemon serves over `host` run, as `caller` names
    /// the workspace's apps and as `person`: each task's host, which this daemon's processes start
    /// in their sandbox when its first program runs. `None` where this daemon starts none.
    #[cfg(target_os = "linux")]
    pub(crate) fn task_hosts<H>(
        &self,
        host: H,
        caller: tilo_actions::remote::Caller,
        person: &str,
    ) -> Option<Arc<dyn tilo_mcp::TaskHosts>>
    where
        H: tilo_actions::remote::ActionProvider + Send + Sync + 'static,
    {
        let processes = self
            .apps
            .processes()
            .inspect_err(|error| tracing::warn!(%error, "this daemon runs no task's programs"))
            .ok()?;
        let catalogue = super::tasks::WorkspaceCatalogue::new(host, caller);
        Some(Arc::new(super::tasks::TaskHosts::new(
            Arc::new(processes),
            Some(person),
            Arc::new(catalogue),
        )))
    }

    /// What the person's own programs call through in `workspace`: this daemon's API for them,
    /// and the caller it admitted them as.
    #[cfg(target_os = "linux")]
    pub(crate) fn person_programs_api(
        &self,
        workspace: &str,
    ) -> Result<(Arc<dyn ActionProvider + Send + Sync>, Caller), String> {
        let provider = self
            .provider_for(workspace)
            .map_err(|error| error.to_string())?;
        let caller = provider
            .admission_caller()
            .ok_or_else(|| "the person's run was not admitted".to_owned())?;
        Ok((Arc::new(provider), caller))
    }

    /// The person this daemon serves.
    #[cfg(target_os = "linux")]
    pub(crate) fn persons_id(&self) -> &str {
        &self.actor
    }

    /// A task's programs run on Linux alone.
    #[cfg(not(target_os = "linux"))]
    #[expect(
        clippy::unused_self,
        clippy::needless_pass_by_value,
        reason = "the one call the Linux daemon answers"
    )]
    pub(crate) fn task_hosts<H>(
        &self,
        host: H,
        caller: tilo_actions::remote::Caller,
        person: &str,
    ) -> Option<Arc<dyn tilo_mcp::TaskHosts>> {
        let _ = (host, caller, person);
        None
    }
}

/// The daemon as its API reaches it, without keeping it.
struct DaemonHere(std::sync::Weak<Daemon>);

impl DaemonHere {
    fn daemon(&self) -> Result<Arc<Daemon>, String> {
        self.0
            .upgrade()
            .ok_or_else(|| "this device's daemon is stopping".to_owned())
    }
}

impl tilo_api::session::Here for DaemonHere {
    fn workspaces(&self, key: &DeviceCert) -> Vec<tilo_api::session::Workspace> {
        self.daemon()
            .map(|daemon| tilo_api::session::Here::workspaces(&*daemon, key))
            .unwrap_or_default()
    }

    fn enter(
        &self,
        workspace: &str,
        key: &tilo_api::grant_key::GrantKey,
    ) -> Result<tilo_api::session::Reached, String> {
        tilo_api::session::Here::enter(&*self.daemon()?, workspace, key)
    }

    fn account(&self) -> Option<Arc<dyn tilo_api::session::AccountServer>> {
        tilo_api::session::Here::account(&*self.daemon().ok()?)
    }

    fn keep(&self, workspace: &str) -> tilo_api::session::Asked<'_, ()> {
        let (daemon, workspace) = (self.daemon(), workspace.to_owned());
        Box::pin(async move {
            let daemon = daemon?;
            tilo_api::session::Here::keep(&*daemon, &workspace).await
        })
    }

    fn leave(&self, workspace: &str) -> tilo_api::session::Asked<'_, ()> {
        let (daemon, workspace) = (self.daemon(), workspace.to_owned());
        Box::pin(async move {
            let daemon = daemon?;
            tilo_api::session::Here::leave(&*daemon, &workspace).await
        })
    }

    fn join(&self, invite: &str) -> tilo_api::session::Asked<'_, String> {
        let (daemon, invite) = (self.daemon(), invite.to_owned());
        Box::pin(async move {
            let daemon = daemon?;
            tilo_api::session::Here::join(&*daemon, &invite).await
        })
    }

    fn workspaces_changed(&self) -> Option<Pending<()>> {
        tilo_api::session::Here::workspaces_changed(&*self.daemon().ok()?)
    }

    fn invocations(&self) -> Option<Arc<Invocations>> {
        tilo_api::session::Here::invocations(&*self.daemon().ok()?)
    }
}

impl tilo_api::session::Here for Daemon {
    /// The workspaces a key of this computer's person reaches; a key of anyone else's reaches
    /// none here (their own devices hold what they may reach).
    fn workspaces(&self, key: &DeviceCert) -> Vec<tilo_api::session::Workspace> {
        if hex::encode(key.user_public_key) != self.actor {
            return Vec::new();
        }
        let mut kept: Vec<_> = self
            .workspaces
            .workspaces()
            .into_iter()
            .map(|workspace| {
                let id = workspace.id().to_string();
                let mut workspace = tilo_api::session::held(id.clone(), workspace.name);
                workspace.labels = self.workspaces.labels_of(&id);
                workspace.unattended = self.workspaces.runs_unattended(&id);
                workspace
            })
            .collect();
        // The one this computer enters through first: what a command that names none acts in.
        kept.sort_by_key(|workspace| workspace.id != self.workspace);
        kept
    }

    fn enter(
        &self,
        workspace: &str,
        key: &tilo_api::grant_key::GrantKey,
    ) -> Result<tilo_api::session::Reached, String> {
        if hex::encode(key.cert().user_public_key) != self.actor {
            return Err("this key is not of this computer's person".to_owned());
        }
        let provider = self
            .provider_for_key(workspace, key)
            .map_err(|error| error.to_string())?;
        let caller = provider
            .admission_caller()
            .ok_or_else(|| "the key's run was not admitted".to_owned())?;
        let tasks = self.task_hosts(provider.clone(), caller.clone(), &self.actor);
        Ok(tilo_api::session::Reached {
            provider: Arc::new(provider),
            person: self.actor.clone(),
            caller: Some(caller),
            tasks,
        })
    }

    fn account(&self) -> Option<Arc<dyn tilo_api::session::AccountServer>> {
        Daemon::account(self).map(|account| account as Arc<dyn tilo_api::session::AccountServer>)
    }

    fn keep(&self, workspace: &str) -> tilo_api::session::Asked<'_, ()> {
        let workspace = workspace.to_owned();
        Box::pin(async move {
            let id = WorkspaceId::parse(&workspace)
                .ok_or_else(|| format!("{workspace} names no workspace"))?;
            if self.workspaces.keeps(id).await {
                return Ok(());
            }
            let person = self.cert.user_public_key;
            let account = Daemon::account(self).ok_or(NO_ACCOUNT_YET)?;
            let listed = account
                .listed(
                    &person,
                    &tilo_server_protocol::workspaces::WorkspaceLabels::new(),
                    Some(&workspace),
                )
                .await?;
            let entry = listed
                .workspaces
                .first()
                .ok_or("the person's list of workspaces names no such workspace")?;
            let hint = entry.entered_at()?;
            self.workspaces
                .copies()
                .hold_as_member(id, &hint)
                .await
                .map(drop)
        })
    }

    fn leave(&self, workspace: &str) -> tilo_api::session::Asked<'_, ()> {
        let workspace = workspace.to_owned();
        Box::pin(async move {
            if workspace == self.workspace {
                return Err(
                    "this computer enters through this workspace: it keeps it for as long as it \
                     is the person's device"
                        .to_owned(),
                );
            }
            self.workspaces.copies().release(&workspace).await
        })
    }

    fn join(&self, invite: &str) -> tilo_api::session::Asked<'_, String> {
        let invite = invite.trim().to_owned();
        Box::pin(async move {
            self.workspaces
                .copies()
                .join_member_as(&invite, person_name().as_deref())
                .await
                .map(|workspace| workspace.to_string())
        })
    }

    fn workspaces_changed(&self) -> Option<Pending<()>> {
        let held = self.workspaces.entered();
        let mut changes = self.workspaces.copies().governance_changes();
        let (done, answer) = tilo_actions::remote::pending();
        tilo_rt::app().handle().spawn(async move {
            tokio::select! {
                () = held.notified() => {}
                _ = changes.recv() => {}
            }
            done.complete(());
        });
        Some(answer)
    }

    fn invocations(&self) -> Option<Arc<Invocations>> {
        Some(Arc::clone(&self.invocations))
    }

    /// Only this device's own person, on this device, hands it what its desktop kept: never an
    /// agent of theirs, another device, or anyone else.
    fn import(&self, by: &Caller, carried: serde_json::Value) -> tilo_api::session::Imported {
        let own = by.user_public_key() == Some(&self.cert.user_public_key)
            && by.device_public_key() == Some(&self.cert.device_public_key)
            && by.agent_name().is_none()
            && !by.is_session_delegated();
        let workspaces = Arc::clone(&self.workspaces);
        let first = Arc::clone(&self.first);
        Box::pin(async move {
            if !own {
                return Err(
                    "only this device's own person takes its desktop's workspaces in".into(),
                );
            }
            let carried = serde_json::from_value::<super::workspaces::Carried>(carried)
                .map_err(|error| format!("what the desktop handed over does not read: {error}"))?;
            let (imported, moved) = workspaces.import(carried).await?;
            first.moved_in(&moved)?;
            Ok(imported)
        })
    }

    fn account_here(&self) -> Option<&dyn tilo_api::session::AccountHere> {
        // A computer that logged in reaches the account through the server that keeps its
        // workspaces; one that founded its person here keeps them itself, and answers for them.
        if !self.first.founds() {
            return None;
        }
        Some(self as &dyn tilo_api::session::AccountHere)
    }
}

/// What a device's own daemon answers of the person's account: the workspaces it keeps for them
/// and the keys they make there, without a server.
///
/// Every call is the key's own: the key's certificate names its root and its reach, the device
/// answers only for its own person's root, and a workspace it holds for another host is not the
/// key's to change.
impl tilo_api::session::AccountHere for Daemon {
    fn workspaces(&self, key: &GrantKey) -> Vec<tilo_api::session::Workspace> {
        if !self.knows(key) {
            return Vec::new();
        }
        let scope = key.scope();
        tilo_api::session::Here::workspaces(self, key.cert())
            .into_iter()
            .filter(|workspace| scope.reaches(&workspace.id))
            .collect()
    }

    fn create(
        &self,
        key: GrantKey,
        name: String,
        labels: WorkspaceLabels,
        id: Option<Uuid>,
    ) -> AccountAnswer<Result<tilo_api::session::Workspace, ServerError>> {
        let workspaces = Arc::clone(&self.workspaces);
        let device = self.cert.clone();
        Box::pin(async move {
            known(&device, &key)?;
            let id = id.unwrap_or_else(Uuid::new_v4);
            let mut arguments = Map::new();
            arguments.insert("name".into(), json!(name));
            arguments.insert("workspace".into(), json!(id.to_string()));
            if !labels.is_empty() {
                arguments.insert("labels".into(), json!(labels));
            }
            let created: CreatedWorkspace = answer(
                workspaces
                    .service()
                    .ask_own(key.cert(), CREATE_OWN_WORKSPACE, arguments)
                    .await,
            )?;
            enter_here(&workspaces, &created.workspace.workspace, &device).await?;
            Ok(listed_workspace(created.workspace, &workspaces))
        })
    }

    fn branch(
        &self,
        key: GrantKey,
        source: String,
        name: String,
        labels: WorkspaceLabels,
        id: Option<Uuid>,
    ) -> AccountAnswer<Result<tilo_api::session::Workspace, ServerError>> {
        let workspaces = Arc::clone(&self.workspaces);
        let device = self.cert.clone();
        Box::pin(async move {
            known(&device, &key)?;
            // A workspace this device keeps for its person is theirs, however it was made: a branch
            // is only ever of one of theirs.
            let _ = workspaces
                .copies()
                .claim_own(&device.user_public_key, &source)
                .await;
            let branch = id.unwrap_or_else(Uuid::new_v4).to_string();
            let mut arguments = Map::new();
            arguments.insert("workspace".into(), json!(source));
            arguments.insert("name".into(), json!(name));
            arguments.insert("branch".into(), json!(branch));
            if !labels.is_empty() {
                arguments.insert("labels".into(), json!(labels));
            }
            let made: CreatedWorkspace = answer(
                workspaces
                    .service()
                    .ask_own(key.cert(), BRANCH_OWN_WORKSPACE, arguments)
                    .await,
            )?;
            enter_here(&workspaces, &made.workspace.workspace, &device).await?;
            Ok(listed_workspace(made.workspace, &workspaces))
        })
    }

    fn rename(
        &self,
        key: GrantKey,
        workspace: String,
        name: String,
    ) -> AccountAnswer<Result<tilo_api::session::Workspace, ServerError>> {
        let workspaces = Arc::clone(&self.workspaces);
        let device = self.cert.clone();
        Box::pin(async move {
            known(&device, &key)?;
            let id = WorkspaceId::parse(&workspace).ok_or(ServerError::NotFound)?;
            // Only a workspace this device keeps takes a name here; one another host keeps is named
            // there.
            if workspaces.copies().governed_copy(id).await.is_none() {
                return Err(ServerError::Refused(
                    "this device does not keep that workspace".into(),
                ));
            }
            let summary = workspaces
                .copies()
                .rename(&workspace, &name)
                .await
                .map_err(unavailable)?;
            Ok(described_workspace(
                summary.workspace,
                summary.name,
                &workspaces,
            ))
        })
    }

    fn delete(&self, key: GrantKey, workspace: String) -> AccountAnswer<Result<(), ServerError>> {
        let workspaces = Arc::clone(&self.workspaces);
        let device = self.cert.clone();
        Box::pin(async move {
            known(&device, &key)?;
            let _ = workspaces
                .copies()
                .claim_own(&device.user_public_key, &workspace)
                .await;
            // A workspace the device does not keep for its person is not the key's to delete, and one
            // it deleted before is no such workspace either.
            if !workspaces
                .copies()
                .owns(&device.user_public_key, &workspace)
            {
                return Err(ServerError::NotFound);
            }
            let mut arguments = Map::new();
            arguments.insert("workspace".into(), json!(workspace));
            workspaces
                .service()
                .ask_own(key.cert(), DELETE_WORKSPACE, arguments)
                .await
                .map(drop)
                .map_err(unavailable)
        })
    }

    fn unattended(
        &self,
        key: GrantKey,
        workspace: String,
        on: bool,
    ) -> AccountAnswer<Result<bool, ServerError>> {
        let workspaces = Arc::clone(&self.workspaces);
        let device = self.cert.clone();
        Box::pin(async move {
            known(&device, &key)?;
            let _ = workspaces
                .copies()
                .claim_own(&device.user_public_key, &workspace)
                .await;
            let mut arguments = Map::new();
            arguments.insert("on".into(), json!(on));
            let answered =
                workspace_verb(&workspaces, &workspace, &key, RUN_UNATTENDED, arguments).await?;
            Ok(answered
                .get("unattended")
                .and_then(Value::as_bool)
                .unwrap_or(on))
        })
    }

    fn revoke_key(
        &self,
        key: GrantKey,
        workspace: String,
        grant: String,
    ) -> AccountAnswer<Result<bool, ServerError>> {
        let workspaces = Arc::clone(&self.workspaces);
        let device = self.cert.clone();
        Box::pin(async move {
            known(&device, &key)?;
            let mut arguments = Map::new();
            arguments.insert("grant".into(), json!(grant));
            match workspace_verb(&workspaces, &workspace, &key, REVOKE_KEY, arguments).await {
                Ok(_) => Ok(true),
                // A workspace that holds no such grant, or already refuses the key as revoked,
                // has nothing left to record.
                Err(ServerError::NotFound) => Ok(false),
                Err(error) => Err(error),
            }
        })
    }

    fn webhooks(
        &self,
        key: GrantKey,
        workspace: Option<String>,
    ) -> AccountAnswer<Result<Value, ServerError>> {
        let targets = match workspace {
            Some(workspace) => vec![workspace],
            None => <Self as tilo_api::session::AccountHere>::workspaces(self, &key)
                .into_iter()
                .map(|workspace| workspace.id)
                .collect(),
        };
        let workspaces = Arc::clone(&self.workspaces);
        let device = self.cert.clone();
        Box::pin(async move {
            known(&device, &key)?;
            let mut rows = Vec::new();
            for workspace in targets {
                let answered =
                    workspace_verb(&workspaces, &workspace, &key, LIST_WEBHOOKS, Map::new()).await;
                match answered {
                    Ok(answered) => rows.extend(
                        answered
                            .get("webhooks")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default(),
                    ),
                    // A workspace this device does not serve keeps no webhook it could list.
                    Err(ServerError::NotFound | ServerError::Refused(_)) => continue,
                    Err(error) => return Err(error),
                }
            }
            Ok(json!({"webhooks": rows}))
        })
    }

    fn create_webhook(
        &self,
        key: GrantKey,
        workspace: String,
        url: String,
        events: Vec<String>,
        path: Option<String>,
    ) -> AccountAnswer<Result<Value, ServerError>> {
        let workspaces = Arc::clone(&self.workspaces);
        let device = self.cert.clone();
        Box::pin(async move {
            known(&device, &key)?;
            let mut arguments = Map::new();
            arguments.insert("url".into(), json!(url));
            if !events.is_empty() {
                arguments.insert("events".into(), json!(events));
            }
            if let Some(path) = path {
                arguments.insert("path".into(), json!(path));
            }
            workspace_verb(&workspaces, &workspace, &key, CREATE_WEBHOOK, arguments).await
        })
    }

    fn delete_webhook(
        &self,
        key: GrantKey,
        webhook: String,
    ) -> AccountAnswer<Result<Value, ServerError>> {
        let targets: Vec<String> = <Self as tilo_api::session::AccountHere>::workspaces(self, &key)
            .into_iter()
            .map(|workspace| workspace.id)
            .collect();
        let workspaces = Arc::clone(&self.workspaces);
        let device = self.cert.clone();
        Box::pin(async move {
            known(&device, &key)?;
            for workspace in targets {
                let mut arguments = Map::new();
                arguments.insert("webhook".into(), json!(webhook));
                match workspace_verb(&workspaces, &workspace, &key, DELETE_WEBHOOK, arguments).await
                {
                    Ok(deleted) => return Ok(deleted),
                    Err(ServerError::NotFound) => continue,
                    Err(ServerError::Refused(_)) => continue,
                    Err(error) => return Err(error),
                }
            }
            Err(ServerError::NotFound)
        })
    }
}

/// Whether the device answers for `key`: only its own person's root certifies a key it serves.
fn known(device: &DeviceCert, key: &GrantKey) -> Result<(), ServerError> {
    if key.cert().user_public_key == device.user_public_key {
        Ok(())
    } else {
        Err(ServerError::Refused(
            "this device serves its own person's keys alone".into(),
        ))
    }
}

impl Daemon {
    /// Whether `key` is one of the person's own: the same root as this device's.
    fn knows(&self, key: &GrantKey) -> bool {
        known(&self.cert, key).is_ok()
    }
}

/// Enter `workspace` as this device's, as `device`: a workspace the device made for its person
/// is one it keeps, so its views and calls reach it at once.
async fn enter_here(
    workspaces: &ServiceWorkspaces,
    workspace: &str,
    device: &DeviceCert,
) -> Result<(), ServerError> {
    workspaces
        .copies()
        .enter(workspace, device)
        .await
        .map(drop)
        .map_err(unavailable)
}

/// A workspace the account created, as its caller reads it back.
fn listed_workspace(
    listing: WorkspaceListing,
    workspaces: &ServiceWorkspaces,
) -> tilo_api::session::Workspace {
    let mut workspace = described_workspace(listing.workspace, listing.name, workspaces);
    workspace.labels = listing.labels;
    workspace.unattended = listing.unattended;
    workspace
}

/// The workspace `id` called `name`, with what this device keeps of it.
fn described_workspace(
    id: String,
    name: String,
    workspaces: &ServiceWorkspaces,
) -> tilo_api::session::Workspace {
    let mut workspace = tilo_api::session::held(id.clone(), name);
    workspace.labels = workspaces.labels_of(&id);
    workspace.unattended = workspaces.runs_unattended(&id);
    workspace
}

/// Run the workspace's own verb `action` with `arguments` as `key`, in process: its output.
async fn workspace_verb(
    workspaces: &ServiceWorkspaces,
    workspace: &str,
    key: &GrantKey,
    action: &str,
    arguments: Map<String, Value>,
) -> Result<Value, ServerError> {
    let id = WorkspaceId::parse(workspace).ok_or(ServerError::NotFound)?;
    let caller = Caller::authenticated(key.cert().user_public_key, key.cert().device_public_key);
    let request = InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget::workspace_governance(workspace, action),
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let pending = workspaces
        .service()
        .actions()
        .serve_certified(
            id,
            &caller,
            key.cert(),
            RemoteActionMessage::Invoke(request),
        )
        .ok_or_else(|| ServerError::Refused("this device does not serve that workspace".into()))?;
    match pending.await {
        Some(RemoteActionMessage::Receipt(receipt)) => match receipt.status {
            ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
            ReceiptStatus::Rejected { error } => Err(refused(&error)),
            ReceiptStatus::Failed { error } => Err(ServerError::Unavailable(error.message)),
            ReceiptStatus::Accepted | ReceiptStatus::Cancelled { .. } => Err(
                ServerError::Unavailable("the workspace did not finish its answer".into()),
            ),
        },
        _ => Err(ServerError::Unavailable(
            "the workspace service dropped the request".into(),
        )),
    }
}

/// What a workspace's own rules refused `key`, as the account answers it.
fn refused(error: &ActionCallError) -> ServerError {
    match error {
        ActionCallError::UnknownAction => ServerError::NotFound,
        ActionCallError::Refused { reason } => {
            ServerError::Refused(refusal_reason(reason).unwrap_or_else(|| reason.clone()))
        }
        ActionCallError::NotPermitted => {
            ServerError::Refused("the workspace does not let this key do that".into())
        }
        other => ServerError::Unavailable(other.to_string()),
    }
}

/// The refusal `reason` names, in the words a person reads, when it carries one.
fn refusal_reason(reason: &str) -> Option<String> {
    tilo_server_protocol::workspaces::AdminRefusal::from_reason(reason)
        .map(|refusal| refusal.to_string())
}

/// Why the device answered nothing.
fn unavailable(why: impl std::fmt::Display) -> ServerError {
    ServerError::Unavailable(why.to_string())
}

/// The workspace service's answer, read as `T`.
fn answer<T: serde::de::DeserializeOwned>(
    answered: Result<Value, String>,
) -> Result<T, ServerError> {
    let value = answered.map_err(unavailable)?;
    serde_json::from_value(value).map_err(|error| {
        ServerError::Unavailable(format!("the host answered unexpectedly: {error}"))
    })
}

/// Why a workspace this daemon does not keep cannot be kept: no server says where it is.
const NO_ACCOUNT_YET: &str = "this computer isn't logged in to a server that says where the \
     workspace is kept: run `tilo login <ticket>` first";

/// The broker's action links as one more local caller, for no app
/// connection. Its callers act from this foreign device.
#[derive(Clone)]
pub struct RuntimeActionProvider {
    broker: AppActionBroker,
    workspace: String,
    workspaces: Arc<ServiceWorkspaces>,
    /// The apps installed for this runtime's profile: what one publishes
    /// before it opens. `None` for a host that serves only the person's
    /// other devices.
    apps: Option<Arc<apps::Apps>>,
    // Retains original ownership for both owner/search and MCP callers. An
    // owner projection is not the local person and never acquires the local person's authority.
    session: Option<Arc<AdmittedRuntimeSession>>,
    /// The daemon's device actions, for the person's own session: serving a ticket for a new
    /// computer, and a login with one. `None` for an agent's run, which cannot add a device.
    tickets: Option<Arc<Tickets>>,
    /// The device's record of every invocation it was asked for: what following, waiting on and
    /// stopping one asks, wherever the call came in.
    invocations: Arc<Invocations>,
    /// Where an agent's call refused for want of its person's yes waits for them, and where
    /// their answer is admitted: `None` for a host that keeps none, whose such calls are refused.
    asked: Option<Arc<super::asked::DeviceAsked>>,
}

struct AdmittedRuntimeSession {
    policy_time: tilo_workspace_service::apps::policy_clock::PolicyTime,
    provider: Arc<dyn ActionProvider + Send + Sync>,
    admission: Arc<tilo_workspace_service::workspaces::SessionAdmission>,
    /// The agent this run is, under a key of its own: who its calls go as.
    agent: Option<Caller>,
    projection: Option<Caller>,
}

impl AdmittedRuntimeSession {
    /// Who the run's calls go as: its agent, or the admitted person.
    fn acting(&self) -> &Caller {
        self.agent
            .as_ref()
            .unwrap_or_else(|| self.admission.caller())
    }

    /// Whether `caller` is who this run was admitted for. A program the person runs reaches
    /// their own run as the local person, each in an execution of its own: that narrows what its
    /// calls own, never who makes them. A run projected into one execution admits only that one.
    fn admits(&self, caller: &Caller) -> bool {
        let Some(projection) = &self.projection else {
            return caller == self.admission.caller();
        };
        match (projection.execution_lease(), caller.execution_lease()) {
            (None, Some(lease)) => *caller == projection.clone().in_execution(lease),
            _ => caller == projection,
        }
    }

    /// Whether this call may run as the run's caller, now.
    ///
    /// A refusal says why it can be acted on: a run that ended is ended for
    /// good, a caller this run was not admitted for is a stranger, and a
    /// check that could not be made (a certificate read or its validity
    /// check that failed this once) refuses only this call, whose error
    /// tells the agent to retry. Every refusal is logged with its cause;
    /// none logs identity material.
    fn check(&self, caller: &Caller, workspaces: &ServiceWorkspaces) -> Result<(), Refusal> {
        let now = match self.policy_time.now_ns() {
            Ok(now) => now,
            Err(error) => {
                tracing::warn!(%error, "the action links cannot read the policy clock");
                return Err(Refusal::Unavailable {
                    reason: "the policy clock is unavailable".into(),
                });
            }
        };
        let verified = tilo_user_identity::verify_device_cert_at(self.admission.certificate(), now);
        if let Err(error) = verified {
            tracing::warn!(%error, "the admitted run's device certificate does not verify now");
            return Err(Refusal::Unavailable {
                reason: format!("the device certificate does not verify ({error})"),
            });
        }
        if !self.admits(caller) {
            tracing::warn!(
                caller = ?caller.acting(),
                "a caller this run was not admitted for tried to act as it"
            );
            return Err(Refusal::Stranger);
        }
        self.admission.check(workspaces.copies())
    }

    fn guarded<T: Send + 'static>(
        self: &Arc<Self>,
        workspaces: Arc<ServiceWorkspaces>,
        caller: Caller,
        answer: Pending<Result<T, ActionCallError>>,
    ) -> Pending<Result<T, ActionCallError>> {
        let session = Arc::clone(self);
        let (complete, pending) = tilo_actions::remote::pending();
        answer.then(move |result| {
            let result = session
                .check(&caller, &workspaces)
                .map_err(ActionCallError::from)
                .and_then(|()| {
                    result.unwrap_or_else(|| {
                        Err(ActionCallError::Unavailable {
                            reason: "action links closed".into(),
                        })
                    })
                });
            complete.complete(result);
        });
        pending
    }
}

/// Run the device action `request` that `caller` was admitted to ask, on the one dispatch: the
/// device's record of the invocation is written first, routed to `tickets`, so it is followed,
/// listed and stopped as every other call is. A call whose answer is a secret is recorded as
/// one: the record keeps neither its arguments nor its answer, only that it was given.
///
/// What only the person's root signs is decided by the cap of a device their devices record does
/// not mark as keeping that root: it waits in its record for one of their devices that does.
pub(crate) fn device_action(
    (invocations, asked): (&Arc<Invocations>, Option<&Arc<super::asked::DeviceAsked>>),
    tickets: &Tickets,
    caller: &Caller,
    request: InvokeRequest,
    workspace: &str,
) -> Pending<Result<InvocationReceipt, ActionCallError>> {
    if !Tickets::root_only(&request) {
        return run_device_action(invocations, tickets, caller, request, workspace);
    }
    let (done, answer) = tilo_actions::remote::pending();
    let (invocations, asked, ran) = (Arc::clone(invocations), asked.cloned(), tickets.clone());
    let (caller, workspace) = (caller.clone(), workspace.to_owned());
    let invocation = request.invocation;
    tickets.root_only_call(workspace.clone(), invocation, move |marked, kept| {
        let answered = if marked {
            run_device_action(&invocations, &ran, &caller, request, &workspace)
        } else {
            Pending::ready(hand_on_to_root(
                asked.as_deref(),
                &caller,
                &request,
                (workspace, kept),
            ))
        };
        answered.then(move |answered| {
            done.complete(answered.unwrap_or(Err(ActionCallError::Unavailable {
                reason: "the device action ended unanswered".into(),
            })));
        });
    });
    answer
}

/// [`device_action`] on this device.
fn run_device_action(
    invocations: &Invocations,
    tickets: &Tickets,
    caller: &Caller,
    request: InvokeRequest,
    workspace: &str,
) -> Pending<Result<InvocationReceipt, ActionCallError>> {
    let route: tilo_actions::remote::invocation_record::Route =
        (Arc::new(tickets.clone()), caller.clone());
    invocations.record(caller, caller, &request, Some(workspace), route);
    if Tickets::answers_a_secret(&request) {
        invocations.keep_secret(&request.invocation);
    }
    tilo_actions::remote::ActionProvider::invoke(tickets, caller, request)
}

/// Keep `request`, which only the person's root signs, for a device of theirs that keeps it, as
/// the cap of a device that keeps none answers: or follow `kept`, its record kept there already.
fn hand_on_to_root(
    asked: Option<&super::asked::DeviceAsked>,
    caller: &Caller,
    request: &InvokeRequest,
    (workspace, kept): (String, Option<tilo_actions::remote::asked::KeptAsk>),
) -> Result<InvocationReceipt, ActionCallError> {
    let cap = tilo_action_policy::Cap::ROOTLESS_DEVICE;
    let (Some(asked), true) = (asked, cap.root_only().answered_at_root()) else {
        return Err(ActionCallError::Refused {
            reason: cap.why.to_owned(),
        });
    };
    let listed = Tickets::listed(request);
    let facts = tilo_actions::remote::asked::AskedFacts {
        verb: listed
            .as_ref()
            .map(|action| action.action_label.clone())
            .unwrap_or_default(),
        on: listed.map(|action| action.resource_label),
        ..Default::default()
    };
    let run = super::asked::Run::Own {
        workspace,
        pressed: None,
    };
    asked.hand_on(&run, caller, request, (facts, kept))
}

impl RuntimeActionProvider {
    /// The caller the run was admitted as, where it has a run: the caller the API session keeps
    /// rather than minting another.
    fn admission_caller(&self) -> Option<Caller> {
        self.session
            .as_ref()
            .map(|session| session.admission.caller().clone())
    }

    /// Whether `caller` is the person's own run, not an agent's: only the person adds a device.
    fn is_the_person(&self, caller: &Caller) -> bool {
        self.session.as_ref().is_some_and(|session| {
            session.agent.is_none()
                && (session.admits(caller) || caller == session.admission.caller())
        })
    }

    /// Stop the agent session `request` names on this device, or let a stopped one go on, as
    /// the person asks of the MCP server: their own ask, never an agent's. `None` when the
    /// request is not such a stop, or names no session this device serves.
    fn stop_or_go_on(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Option<Pending<Result<InvocationReceipt, ActionCallError>>> {
        use tilo_actions::remote::mcp_server;
        let target = &request.target;
        let action = target.address.action.as_str();
        if target.provider != mcp_server::PROVIDER
            || ![mcp_server::STOP, mcp_server::GO_ON].contains(&action)
            || !caller.acting().is_person()
        {
            return None;
        }
        let named: mcp_server::AgentSessionRef = match &request.argument {
            InvokeArgument::Json(argument) => {
                match serde_json::from_value(Value::Object(argument.clone())) {
                    Ok(named) => named,
                    Err(error) => {
                        return Some(Pending::ready(Err(ActionCallError::invalid_request(
                            error.to_string(),
                        ))));
                    }
                }
            }
            _ => {
                return Some(Pending::ready(Err(ActionCallError::invalid_request(
                    "a stop names its grant and session",
                ))));
            }
        };
        let copies = self.workspaces.copies();
        if !copies.knows_agent_session(&self.workspace, &named.session) {
            return None;
        }
        let stopping = action == mcp_server::STOP;
        let (session, workspace) = (named.session, self.workspace.clone());
        let operator = self.workspaces.operator();
        let agent = device_agent_id(&operator, MCP_CLIENT);
        // The person's own act, as the caller that made the agent's calls works for them.
        let person = Caller::local_person(hex::encode(operator)).in_workspace(&workspace);
        // Every workspace the agent works in: the one its session is in, and each whose member
        // list names it. The answer syncs with the workspace, so a host running the agent
        // elsewhere refuses its next call.
        let targets: Vec<String> = self
            .workspaces
            .workspaces()
            .into_iter()
            .filter(|held| {
                held.id().to_string() == workspace
                    || self.workspaces.policy_of(held.id())()
                        .is_some_and(|policy| policy.agents.contains_key(&agent))
            })
            .map(|held| held.id().to_string())
            .collect();
        let workspaces = Arc::clone(&self.workspaces);
        let invocations = Arc::clone(&self.invocations);
        let broker = self.broker.clone();
        let receipt = InvocationReceipt {
            invocation: request.invocation,
            status: ReceiptStatus::Completed {
                output: tilo_actions::ActionOutput {
                    value: Some(Value::Null),
                    resources: Vec::new(),
                },
            },
        };
        let (complete, answered) = tilo_actions::remote::pending();
        let stop = async move {
            let copies = workspaces.copies();
            let mut answer = Ok(receipt);
            for target in &targets {
                let written = if stopping {
                    copies.answer_stop(target, &agent).await
                } else {
                    copies.answer_go_on(target, &agent).await
                };
                if let Err(reason) = written {
                    answer = Err(ActionCallError::Refused { reason });
                    break;
                }
                if stopping {
                    // As the dispatch itself cancels a call, the person's own ask.
                    let origin_writes = |caller: &Caller, origin: &Origin| {
                        broker.state().may_write_origin(caller, target, origin)
                    };
                    for cancelled in invocations.stop_agent(&person, target, &agent, &origin_writes)
                    {
                        let _ = cancelled.await;
                    }
                }
            }
            if answer.is_ok() {
                if stopping {
                    copies.stop_agent(&workspace, &session);
                } else {
                    copies.let_agent_go_on(&workspace, &session);
                }
            }
            complete.complete(answer);
        };
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => drop(runtime.spawn(stop)),
            Err(_) => super::workspaces::block_on_owned(stop),
        }
        Some(answered)
    }

    /// `answer`, with the daemon's own `tilo.device` provider listed beside the workspace's apps,
    /// and its page answered whole when the query names it.
    fn with_devices(
        &self,
        answer: Pending<Result<CataloguePage, ActionCallError>>,
        query: &CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        let query = query.clone();
        let workspace = self.workspace.clone();
        answer.map(move |answer| {
            answer.map(|page| {
                let mut page = super::ticket::with_device_actions(page, &query, &workspace);
                // The workspace's invocations are published beside its apps, as one more thing
                // a walk of the workspace finds.
                if query.provider.is_none() {
                    page.providers
                        .push(tilo_actions::remote::workspace_invocations::publisher(
                            &workspace,
                        ));
                }
                page
            })
        })
    }

    fn session_factory(
        self,
        workspace: String,
        actor: String,
        policy_time: tilo_workspace_service::apps::policy_clock::PolicyTime,
    ) -> tilo_mcp::SessionFactory<Self> {
        let provider = self;
        Arc::new(move || {
            // The MCP client is the person's agent, under a key of its own: the admission stays
            // the person's, and every call the client makes is signed by the agent's key.
            let (session_host, caller) =
                provider.agent_session(&workspace, &actor, &policy_time, |person| {
                    (device_agent_id(person, MCP_CLIENT), MCP_CLIENT.to_owned())
                })?;
            let run = tilo_mcp::agent_id(&caller);
            // The connection is here from the moment it opens, before it asks anything, and
            // says what it is when its client does. The device its admission certified is what
            // the person's MCP server lists the run under.
            if let Some(execution) = caller.execution_id() {
                let cert = provider
                    .session
                    .as_ref()
                    .map(|session| session.admission.certificate().clone());
                provider.workspaces.copies().agent_opened(
                    &execution.to_string(),
                    &workspace,
                    tilo_mcp::session_api::UNNAMED_CLIENT,
                    cert.as_ref(),
                    &Arc::new(tokio::sync::Notify::new()),
                );
            }
            Ok(tilo_mcp::McpServer::admitted(session_host, caller)?.with_agent_session(run))
        })
    }

    /// A run of `actor`'s agent in `workspace`, which `agent` names (its id and name) from the
    /// person's public key: the provider its calls go through and the caller its admission is.
    /// The agent acts under a key of its own this device makes it, its calls signed by that key
    /// and taken by the broker as the agent's.
    pub(super) fn agent_session(
        &self,
        workspace: &str,
        actor: &str,
        policy_time: &tilo_workspace_service::apps::policy_clock::PolicyTime,
        agent: impl FnOnce(&[u8; 32]) -> (String, String),
    ) -> io::Result<(Self, Caller)> {
        let workspaces = Arc::clone(&self.workspaces);
        let (workspace, actor) = (workspace.to_owned(), actor.to_owned());
        let admission =
            block_on_owned(async move { workspaces.admit_execution(&workspace, &actor).await })
                .map_err(io::Error::other)?;
        let caller = admission.caller().clone();
        let (Some(person), Some(user), Some(workspace)) = (
            caller.acting().person_id(),
            caller.user_public_key(),
            caller.workspace(),
        ) else {
            return Err(io::Error::other(
                "the admitted run names no person's workspace",
            ));
        };
        let (agent, name) = agent(user);
        let keys = this_device().map_err(io::Error::other)?;
        // The keys the device makes are kept among its person's keys in the record this device
        // holds, and a revocation of one is announced from here to its workspaces' hosts.
        {
            let (keys, record) = (Arc::clone(&keys), self.workspaces.copies().clone());
            block_on_owned(async move { keys.keep_in(&record) });
        }
        let (signer, agent) = AgentSigner::grant(keys, workspace, (&agent, &name), person)
            .map_err(io::Error::other)?;
        admission
            .check(self.workspaces.copies())
            .map_err(io::Error::other)?;
        // The workspace's record of its agents names the computer's agent as it first arrives:
        // the same write that certifies the key it acts under keeps it there, active and acting
        // for its person, so the Agents tab and the workspace's roster read it by the id every
        // run of it shares.
        let record = Arc::clone(&self.workspaces);
        let (named, listed) = (workspace.to_owned(), agent.clone());
        block_on_owned(async move {
            let Some(id) = WorkspaceId::parse(&named) else {
                return;
            };
            tilo_workspace_service::agents::workspace_agent(record.copies(), id, &listed).await;
        });
        let run = Self {
            broker: self.broker.clone(),
            workspace: self.workspace.clone(),
            workspaces: self.workspaces.clone(),
            apps: self.apps.clone(),
            session: Some(Arc::new(AdmittedRuntimeSession {
                policy_time: policy_time.clone(),
                provider: Arc::new(Signed::new(self.broker.device_host(), Some(signer))),
                admission,
                agent: Some(agent),
                projection: None,
            })),
            tickets: self.tickets.clone(),
            invocations: Arc::clone(&self.invocations),
            asked: self.asked.clone(),
        };
        Ok((run, caller))
    }

    fn admitted(&self, caller: &Caller) -> bool {
        let Some(workspace) = caller
            .workspace()
            .filter(|workspace| *workspace != self.workspace)
        else {
            return true;
        };
        let Some(actor) = caller.acting().person_id() else {
            return false;
        };
        self.workspaces.action_workspace(actor, workspace).is_ok()
    }
}

// These existing alias forms may eventually dispatch into a published app.
// Keep original certified ownership through the broker's deferred launch path.
fn certified_provider(provider: &str) -> bool {
    provider.parse::<u64>().is_ok()
        || tilo_actions::remote::application_provider(provider).is_some()
        || provider.starts_with("launch:")
}

fn on_this_device(caller: &Caller) -> Caller {
    caller.clone().from_foreign_device(true)
}

/// One answer for either of two waits: resolves when the first of them does, with what it
/// answered, so a watch woken by either reads again.
fn whichever_first(
    first: Pending<Result<(), ActionCallError>>,
    second: Pending<Result<(), ActionCallError>>,
) -> Pending<Result<(), ActionCallError>> {
    let (complete, waited) = tilo_actions::remote::pending();
    let answer = Arc::new(Mutex::new(Some(complete)));
    for one in [first, second] {
        let answer = Arc::clone(&answer);
        one.then(move |answered| {
            let Some(complete) = answer.lock().ok().and_then(|mut answer| answer.take()) else {
                return;
            };
            complete.complete(answered.unwrap_or(Err(ActionCallError::Unavailable {
                reason: "a source the watch followed is gone".into(),
            })));
        });
    }
    waited
}

/// The answer to `tilo.asking/waiting#list`: every question that waits for `caller`'s person,
/// oldest first, from the records the calls are kept as.
fn asking(
    request: &InvokeRequest,
    caller: &Caller,
    asked: &super::asked::DeviceAsked,
) -> Result<InvocationReceipt, ActionCallError> {
    use tilo_actions::remote::asked as one_ask;
    let address = &request.target.address;
    if (address.resource.as_str(), address.action.as_str()) != (one_ask::WAITING, one_ask::LIST) {
        return Err(ActionCallError::UnknownAction);
    }
    let person = one_ask::asked_person(caller).ok_or(ActionCallError::NotPermitted)?;
    let person: [u8; 32] = hex::decode(&person)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(ActionCallError::NotPermitted)?;
    let questions = asked.asked_of(&person);
    let value = serde_json::to_value(one_ask::AskedList { questions }).map_err(|error| {
        ActionCallError::Unavailable {
            reason: error.to_string(),
        }
    })?;
    Ok(InvocationReceipt {
        invocation: request.invocation,
        status: ReceiptStatus::Completed {
            output: tilo_actions::ActionOutput {
                value: Some(value),
                resources: Vec::new(),
            },
        },
    })
}

/// The record reader of a device's dispatch. The workspace's invocations
/// ([`tilo_actions::remote::workspace_invocations::PROVIDER`]) are a resource the dispatch
/// publishes itself: read whole (an empty record) or one by its id, as the caller's grants let
/// them read each call, and followed by watching for the next one recorded or the next change
/// of one still going. Every other record is the broker's to read.
struct DispatchReads {
    broker: AppActionBroker,
    workspace: String,
    workspaces: Arc<ServiceWorkspaces>,
    session: Option<Arc<AdmittedRuntimeSession>>,
    invocations: Arc<Invocations>,
    records: Option<Arc<dyn tilo_actions::remote::ResourceReads>>,
}

impl DispatchReads {
    fn publishes(address: &tilo_actions::remote::ResourceAddress) -> bool {
        address.app == tilo_actions::remote::workspace_invocations::PROVIDER
    }

    fn records(&self) -> Result<&Arc<dyn tilo_actions::remote::ResourceReads>, ActionCallError> {
        self.records
            .as_ref()
            .ok_or_else(|| ActionCallError::Unavailable {
                reason: tilo_actions::remote::RESOURCE_READS_UNAVAILABLE.into(),
            })
    }

    /// Whether `caller` reads `workspace`'s invocations here: this dispatch's workspace, as the
    /// session it admitted.
    fn admit(&self, caller: &Caller, workspace: &str) -> Result<(), ActionCallError> {
        if workspace != self.workspace {
            return Err(ActionCallError::Refused {
                reason: tilo_actions::remote::RESOURCE_READ_REFUSED.into(),
            });
        }
        if let Some(session) = &self.session {
            session.check(caller, &self.workspaces)?;
        }
        Ok(())
    }

    /// Every invocation of the workspace `caller` may read, newest first.
    fn listed(
        &self,
        caller: &Caller,
        invocation: Option<String>,
    ) -> Result<Vec<tilo_actions::remote::workspace_invocations::Invocation>, ActionCallError> {
        let mut items = Vec::new();
        let mut cursor = None;
        loop {
            let page = self.broker.state().invocation_listing(
                &self.workspace,
                caller,
                &tilo_actions::remote::workspace_invocations::InvocationsQuery {
                    limit: tilo_actions::remote::workspace_invocations::MOST,
                    cursor: cursor.take(),
                    invocation: invocation.clone(),
                    ..tilo_actions::remote::workspace_invocations::InvocationsQuery::default()
                },
            )?;
            items.extend(page.items);
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        if let Some(store) = self.workspaces.copies().asked_store() {
            with_questions(
                &mut items,
                store.waiting_in(&self.workspace),
                invocation.as_deref(),
            );
        }
        Ok(items)
    }
}

/// `items`, a reading of a workspace's invocations, with what each call that waits in
/// `waiting` asks its person, and each such call the device keeps no record of (it waited across
/// a restart, or another dispatch here keeps it) listed beside them: only `only`, when the
/// reading names one.
pub(super) fn with_questions(
    items: &mut Vec<tilo_actions::remote::workspace_invocations::Invocation>,
    waiting: Vec<tilo_actions::remote::asked::Waiting>,
    only: Option<&str>,
) {
    use tilo_actions::remote::asked::AskedBy;
    use tilo_actions::remote::workspace_invocations::{Invocation, InvocationBy, InvocationStatus};
    for question in waiting {
        let id = question.invocation.0.to_string();
        if only.is_some_and(|only| only != id) {
            continue;
        }
        if let Some(item) = items
            .iter_mut()
            .find(|item| item.id.as_deref() == Some(id.as_str()))
        {
            item.status = InvocationStatus::Waiting;
            item.person = Some(question.person);
            item.asked = Some(Box::new(question.asked));
            continue;
        }
        let by = match &question.asked.facts.by {
            AskedBy::Agent(agent) => InvocationBy {
                name: agent.name.clone(),
                kind: "agent".to_owned(),
                id: Some(agent.id.clone()),
            },
            AskedBy::App { app, title, .. } => InvocationBy {
                name: title.clone(),
                kind: "app".to_owned(),
                id: Some(app.clone()),
            },
            AskedBy::Device(device) => InvocationBy {
                name: device.name.clone(),
                kind: "device".to_owned(),
                id: Some(device.id.clone()),
            },
            AskedBy::Program { name, app_id } => InvocationBy {
                name: name.clone(),
                kind: "program".to_owned(),
                id: app_id.clone(),
            },
        };
        let row = Invocation {
            id: Some(id.clone()),
            calls: vec![id],
            by,
            status: InvocationStatus::Waiting,
            started_ms: i64::try_from(question.asked.asked_at_ms).unwrap_or(i64::MAX),
            action: Some(question.asked.action.clone()),
            remembered: true,
            person: Some(question.person),
            asked: Some(Box::new(question.asked)),
            ..Invocation::default()
        };
        // Newest first, as the reading is.
        let at = items
            .iter()
            .position(|item| item.started_ms <= row.started_ms)
            .unwrap_or(items.len());
        items.insert(at, row);
    }
}

impl tilo_actions::remote::ResourceReads for DispatchReads {
    fn read(
        &self,
        caller: &Caller,
        address: &tilo_actions::remote::ResourceAddress,
    ) -> Result<tilo_actions::remote::ResourceCopy, ActionCallError> {
        use tilo_actions::remote::ResourceCopy;
        if !Self::publishes(address) {
            return self.records()?.read(caller, address);
        }
        self.admit(caller, &address.workspace)?;
        if address.record.is_empty() {
            let items = self.listed(caller, None)?;
            let total = items.len();
            let waiting = items
                .iter()
                .filter(|invocation| {
                    invocation.status
                        == tilo_actions::remote::workspace_invocations::InvocationStatus::Waiting
                })
                .count();
            return Ok(ResourceCopy::reading(
                serde_json::json!({ "items": items, "total": total, "waiting": waiting }),
                None,
            ));
        }
        let mut one = self.listed(caller, Some(address.record.clone()))?;
        Ok(match one.pop() {
            Some(invocation) => ResourceCopy::reading(
                serde_json::to_value(invocation).map_err(|error| ActionCallError::Unavailable {
                    reason: error.to_string(),
                })?,
                None,
            ),
            None => ResourceCopy::Absent,
        })
    }

    fn answer_read(
        self: Arc<Self>,
        caller: &Caller,
        correlation: u64,
        read: tilo_actions::remote::ResourceReadRequest,
    ) -> Pending<RemoteActionMessage> {
        if read.app != tilo_actions::remote::workspace_invocations::PROVIDER
            && let Some(records) = &self.records
        {
            return Arc::clone(records).answer_read(caller, correlation, read);
        }
        tilo_actions::remote::answer_read(self, caller, correlation, read)
    }

    fn watch(
        &self,
        caller: &Caller,
        address: &tilo_actions::remote::ResourceAddress,
        wake: &tilo_actions::remote::ResourceWake,
    ) -> Result<(), ActionCallError> {
        if !Self::publishes(address) {
            return self.records()?.watch(caller, address, wake);
        }
        self.admit(caller, &address.workspace)?;
        self.invocations.watch(&self.workspace, wake);
        Ok(())
    }

    fn store_read(
        &self,
        caller: &Caller,
        address: &tilo_actions::remote::ResourceAddress,
        read: serde_json::Value,
    ) -> Result<serde_json::Value, ActionCallError> {
        if Self::publishes(address) {
            return Err(ActionCallError::Unavailable {
                reason: "a workspace's invocations are read whole or one by one, not from a store"
                    .into(),
            });
        }
        self.records()?.store_read(caller, address, read)
    }
}

impl ActionProvider for RuntimeActionProvider {
    /// The device's one search index, read as the caller may read it.
    fn resource_search(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceSearch>> {
        self.workspaces
            .search()
            .map(|search| search as Arc<dyn tilo_actions::remote::ResourceSearch>)
    }

    fn installed_surface(&self, app_id: &str) -> tilo_actions::remote::Pending<Option<Vec<u8>>> {
        tilo_actions::remote::Pending::ready(
            self.apps
                .as_ref()
                .and_then(|apps| apps.installed_surface(app_id)),
        )
    }

    fn retire_execution(&self, caller: &Caller) {
        // The connection ended, so its session leaves who is here.
        if let Some(execution) = caller.execution_id() {
            self.workspaces
                .copies()
                .agent_closed(&execution.to_string());
        }
        if let Some(session) = &self.session {
            if caller
                == session
                    .projection
                    .as_ref()
                    .unwrap_or_else(|| session.admission.caller())
            {
                self.broker
                    .state()
                    .retire_execution(session.admission.caller());
            }
            return;
        }
        self.broker.state().retire_execution(caller);
    }

    /// The connection's client named itself (`Claude Code`): who is here calls its session so
    /// from now on.
    fn introduced(&self, caller: &Caller, client: &str) {
        let Some(session) = &self.session else {
            return;
        };
        // Only the run's own connection may say what it is, and the person's own run is no agent.
        if !session.admits(caller) || session.agent.is_none() {
            return;
        }
        if let Some(execution) = caller.execution_id() {
            self.workspaces
                .copies()
                .agent_named(&execution.to_string(), client);
        }
        // The workspace's record of its agents calls the agent what its client names itself,
        // keeping the id every run of it shares: a person reads "Claude Code" in the roster, not
        // the name the key its run acts under was made by.
        let Some(agent) = session.agent.as_ref() else {
            return;
        };
        let (Some(agent_id), Some(person)) = (
            agent.acting().agent_id().map(str::to_owned),
            agent.acting().acted_for_person().map(str::to_owned),
        ) else {
            return;
        };
        let (workspaces, workspace, client) = (
            Arc::clone(&self.workspaces),
            self.workspace.clone(),
            client.to_owned(),
        );
        block_on_owned(async move {
            let Some(workspace) = WorkspaceId::parse(&workspace) else {
                return;
            };
            if let Err(error) = tilo_workspace_service::agents::rename_agent(
                workspaces.copies(),
                workspace,
                (&agent_id, &client),
                &person,
            )
            .await
            {
                tracing::warn!(
                    %error,
                    %agent_id,
                    "an agent's name is not kept in the workspace's agents"
                );
            }
        });
    }

    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        if query.provider.as_deref() == Some(tilo_actions::remote::workspace_invocations::PROVIDER)
        {
            if let Some(session) = &self.session
                && let Err(refusal) = session.check(caller, &self.workspaces)
            {
                return Pending::ready(Err(refusal.into()));
            }
            return Pending::ready(self.broker.state().invocation_page(
                &self.workspace,
                caller,
                &query,
            ));
        }
        if let Some(session) = &self.session {
            // Owner discovery remains the ordinary read path: no per-query
            // identity-store resolution. Opening a result uses retained ownership.
            if session.projection.is_some() && session.admits(caller) {
                if !self.admitted(caller) {
                    return Pending::ready(Err(ActionCallError::NotPermitted));
                }
                let answer = self
                    .broker
                    .state()
                    .action_query(&on_this_device(caller), &query);
                return self.with_devices(answer, &query);
            }
            if let Err(refusal) = session.check(caller, &self.workspaces) {
                return Pending::ready(Err(refusal.into()));
            }
            if query
                .provider
                .as_ref()
                .is_some_and(|provider| certified_provider(provider))
            {
                let answer = session.provider.query(caller, query.clone());
                return self.with_devices(answer, &query);
            }
            // Existing general catalogue/builtin APIs retain the exact
            // authenticated run. They make no SDK original-ownership claim.
            let answer = self.broker.state().action_query(session.acting(), &query);
            let answer = session.guarded(Arc::clone(&self.workspaces), caller.clone(), answer);
            return self.with_devices(answer, &query);
        }
        if !self.admitted(caller) {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        }
        let answer = self
            .broker
            .state()
            .action_query(&on_this_device(caller), &query);
        self.with_devices(answer, &query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if let Err(refused) = tilo_actions::appearance::refuse_here(&request) {
            return Pending::ready(Err(refused));
        }
        // What waits for the person's yes, listed from the records the calls are kept as: a
        // terminal answers them with their own `resolve`, as their shell does.
        if request.target.provider == tilo_actions::remote::asked::PROVIDER
            && let Some(asked) = &self.asked
        {
            return Pending::ready(asking(&request, caller, asked));
        }
        // The device's own actions: what it is, to the person and the agents acting for them;
        // what lets another device in, to the person alone.
        //
        // A session the person stopped is ended for good: what its connection asks next reaches
        // nothing.
        if let Some(execution) = caller.execution_id()
            && self
                .workspaces
                .copies()
                .agent_stopped(&execution.to_string())
        {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        }
        // The person stops one of the agents this device serves, or lets a stopped one go on,
        // as their card asks: a Deny in every workspace the agent works in, and what it is
        // running here cancelled.
        if let Some(answered) = self.stop_or_go_on(caller, &request) {
            return answered;
        }
        // The device's own records: its settings, written through the same actions before the
        // person's services ran and after, once the device is theirs.
        if request.target.provider == super::records::PROVIDER
            && request.target.address.resource == super::records::RESOURCE
            && let Some(records) = self.workspaces.device_records()
        {
            return Pending::ready(Ok(records.receipt_for(caller, &request)));
        }
        if let Some(tickets) = &self.tickets
            && Tickets::answers(&request)
            && (self.is_the_person(caller)
                || (Tickets::only_reads(&request)
                    && self
                        .session
                        .as_ref()
                        .is_some_and(|session| session.admits(caller))))
        {
            return device_action(
                (&self.invocations, self.asked.as_ref()),
                tickets,
                caller,
                request,
                &self.workspace,
            );
        }
        let origin_writes = |caller: &Caller, origin: &Origin| {
            self.broker
                .state()
                .may_write_origin(caller, &self.workspace, origin)
        };
        if let Some(answered) = self.invocations.answer(caller, &request, &origin_writes) {
            return answered;
        }
        // An approve only the device hands back: one a caller writes into its own call is
        // refused whole, nothing of it run.
        if let Err(forged) = tilo_actions::remote::asked::admit_frame(
            self.asked.as_ref().map(|asked| asked.store()),
            &request,
        ) {
            return Pending::ready(Err(forged));
        }
        if let Some(session) = &self.session {
            if let Err(refusal) = session.check(caller, &self.workspaces) {
                return Pending::ready(Err(refusal.into()));
            }
            // A person's answer to a question that waits for them, as the run admitted them: an
            // agent answers nobody's, its own least.
            if tilo_actions::remote::asked::is_resolve(&request.target) {
                return self
                    .workspaces
                    .copies()
                    .resolve_asked(session.acting(), &request);
            }
            // What an agent asks shown goes on its person's layout, where their devices show it.
            if session.agent.is_some()
                && let Some(shown) = super::layout::show_on_layout(
                    &self.workspaces,
                    &self.workspace,
                    (session.acting(), &request),
                    |app| {
                        self.apps
                            .as_ref()
                            .map_or_else(|| app.to_owned(), |apps| apps.display_name(app))
                    },
                )
            {
                return shown;
            }
            // An agent's call waits for its person's yes as its asked record; the person's own
            // is answered where they pressed it.
            if let (Some(asked), Some(agent)) = (&self.asked, &session.agent)
                && let (Some(id), Some(name)) = (agent.acting().agent_id(), agent.agent_called())
            {
                let run = super::asked::Run::Agent {
                    workspace: self.workspace.clone(),
                    agent: id.to_owned(),
                    name: name.to_owned(),
                };
                let route = (Arc::clone(&session.provider), agent.clone());
                return asked.send(run, caller, route, request);
            }
            // The person's own call waits for them as its asked record too: where a program
            // outside Tilo asks them through the shell, or a press needs their yes.
            if let Some(asked) = &self.asked
                && session.agent.is_none()
            {
                let run = super::asked::Run::Own {
                    workspace: self.workspace.clone(),
                    pressed: None,
                };
                let route = (Arc::clone(&session.provider), session.acting().clone());
                return asked.send(run, caller, route, request);
            }
            let runs_as = session.acting().clone();
            let invocation = request.invocation;
            self.invocations.record(
                caller,
                &session.acting(),
                &request,
                Some(&self.workspace),
                (Arc::clone(&session.provider), runs_as.clone()),
            );
            let records = Arc::clone(&self.invocations);
            return session
                .provider
                .invoke(&runs_as, request)
                .map(move |result| {
                    // A call the person is asked about goes on and ends once they decide: the
                    // record says it waits for them, which the listing shows.
                    if result
                        .as_ref()
                        .err()
                        .is_some_and(ActionCallError::awaits_person)
                    {
                        records.note_waiting(&invocation);
                    }
                    result
                });
        }
        Pending::ready(Err(ActionCallError::NotPermitted))
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        if let Some(state) = self.invocations.observe(caller, id) {
            return Some(state);
        }
        if let Some(session) = &self.session {
            session.check(caller, &self.workspaces).ok()?;
            return session.provider.observe(session.acting(), id);
        }
        if !self.admitted(caller) {
            return None;
        }
        self.broker
            .state()
            .action_observe(&on_this_device(caller), id)
    }

    /// Who is in the workspace, as the desktop's roster names them with the
    /// presence of this device's workspaces beside it: routed and authorized
    /// exactly as [`Self::observe`], and empty for a session whose host keeps
    /// no roster.
    fn here(&self, caller: &Caller) -> Option<Vec<tilo_actions::remote::Here>> {
        let listed = if let Some(session) = &self.session {
            session.check(caller, &self.workspaces).ok()?;
            // The broker's signed host keeps no roster: an agent's run reads it as the agent.
            match &session.agent {
                Some(agent) => self.broker.state().here(agent),
                None => session.provider.here(session.acting()),
            }
        } else {
            if !self.admitted(caller) {
                return None;
            }
            self.broker.state().here(&on_this_device(caller))
        };
        if self.session.is_some() {
            return Some(listed.unwrap_or_default());
        }
        listed
    }

    /// Where the session's workspace stands, for a caller of it: authorized
    /// exactly as [`Self::here`]. A runtime that serves from no session says
    /// nothing.
    fn session_state(&self, caller: &Caller) -> Option<tilo_actions::remote::SessionState> {
        let session = self.session.as_ref()?;
        session.check(caller, &self.workspaces).ok()?;
        session.provider.session_state(session.acting())
    }

    fn session_state_changed(&self, caller: &Caller) -> Option<Pending<()>> {
        let session = self.session.as_ref()?;
        session.check(caller, &self.workspaces).ok()?;
        session.provider.session_state_changed(session.acting())
    }

    /// Why the agent this runtime's session runs acts no further: the session's own provider
    /// says, read for whichever caller asks, as a revoked key ends every caller of it.
    fn ended(&self, _caller: &Caller) -> Option<String> {
        let session = self.session.as_ref()?;
        session.provider.ended(session.acting())
    }

    /// What a watch of the workspace follows, routed and authorized as [`Self::here`] and
    /// [`Self::session_state`] are: the session host's own signal for a session this runtime
    /// serves, and the broker's for one it runs itself.
    fn changed(
        &self,
        caller: &Caller,
        scope: tilo_actions::remote::WatchScope,
    ) -> Option<tilo_actions::remote::Pending<Result<(), tilo_actions::remote::ActionCallError>>>
    {
        let followed = if let Some(session) = &self.session {
            session.check(caller, &self.workspaces).ok()?;
            match &session.agent {
                Some(agent) => self.broker.state().changed(agent, scope),
                None => session.provider.changed(session.acting(), scope),
            }
        } else {
            if !self.admitted(caller) {
                return None;
            }
            self.broker.state().changed(&on_this_device(caller), scope)
        };
        if !matches!(scope, tilo_actions::remote::WatchScope::Here) {
            return followed;
        }
        // Presence moving (an agent session opening, naming itself or leaving, a device coming
        // or going) is news to who is here, as the roster's own changes are.
        let (wake, moved) = tilo_actions::remote::ResourceWake::new();
        tilo_workspace_service::workspaces::on_presence_change(&wake);
        let moved = moved.map(|()| Ok(()));
        Some(match followed {
            Some(followed) => whichever_first(followed, moved),
            None => moved,
        })
    }

    /// Routed and authorized exactly as [`Self::observe`]: a caller that may
    /// not observe an invocation may not wait on it either.
    fn next_change(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<Pending<()>> {
        if self.invocations.get(caller, id).is_some() {
            return self.invocations.next_change(caller, id);
        }
        if let Some(session) = &self.session {
            session.check(caller, &self.workspaces).ok()?;
            return session.provider.next_change(session.acting(), id);
        }
        if !self.admitted(caller) {
            return None;
        }
        self.broker
            .state()
            .action_next_change_after_now(&on_this_device(caller), id)
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionCallError>> {
        if let Some(stopping) =
            self.invocations
                .cancel(caller, id, &|caller: &Caller, origin: &Origin| {
                    self.broker
                        .state()
                        .may_write_origin(caller, &self.workspace, origin)
                })
        {
            return stopping;
        }
        if let Some(session) = &self.session {
            if let Err(refusal) = session.check(caller, &self.workspaces) {
                return Pending::ready(Err(refusal.into()));
            }
            return session.provider.cancel(session.acting(), id);
        }
        if !self.admitted(caller) {
            return Pending::ready(Err(ActionCallError::UnknownInvocation));
        }
        self.broker
            .state()
            .action_cancel(&on_this_device(caller), id)
    }

    /// The workspace's content, put into the shelf its records' content is kept on once the
    /// workspace's grants allow the put: for the run this runtime admitted, as that run.
    fn content_puts(&self) -> Option<Arc<dyn tilo_actions::remote::ContentPuts>> {
        Some(Arc::new(AdmittedContent {
            session: Arc::clone(self.session.as_ref()?),
            workspaces: Arc::clone(&self.workspaces),
            content: self.broker.state().content_puts()?,
        }))
    }

    /// Records read through the device's one record reader, the workspace's invocations with
    /// the rest: a key's run as that key, whose certificate the workspace keeps, and every caller
    /// by the grants the apps this device serves decide for it.
    fn resource_reads(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceReads>> {
        let (own, session, workspaces) = (
            self.workspace.clone(),
            self.session.clone(),
            Arc::clone(&self.workspaces),
        );
        // A dispatch's invocations are its own workspace's, read as the run it admitted.
        let admitted = move |caller: &Caller, workspace: &str| {
            if workspace != own {
                return Err(ActionCallError::Refused {
                    reason: tilo_actions::remote::RESOURCE_READ_REFUSED.into(),
                });
            }
            if let Some(session) = &session {
                session.check(caller, &workspaces)?;
            }
            Ok(())
        };
        Some(tilo_workspace_service::workspaces::RecordReads::with_apps(
            self.workspaces.copies(),
            self.broker.state(),
            Some(Arc::new(admitted)),
        ))
    }

    /// What was put into the workspace here, read back by the run this runtime admitted.
    fn content_gets(&self) -> Option<Arc<dyn tilo_actions::remote::ContentGets>> {
        Some(Arc::new(AdmittedContent {
            session: Arc::clone(self.session.as_ref()?),
            workspaces: Arc::clone(&self.workspaces),
            content: tilo_workspace_service::content::shelf_gets(self.workspaces.content_shelf()?),
        }))
    }
}

/// A workspace's content as the run a runtime admitted reaches it: each call is checked as the
/// run's actions are, and made as the run.
struct AdmittedContent<T: ?Sized> {
    session: Arc<AdmittedRuntimeSession>,
    workspaces: Arc<ServiceWorkspaces>,
    content: Arc<T>,
}

impl<T: ?Sized> AdmittedContent<T> {
    fn run(&self, caller: &Caller) -> Result<&Caller, ActionCallError> {
        self.session.check(caller, &self.workspaces)?;
        Ok(self.session.admission.caller())
    }
}

impl tilo_actions::remote::ContentPuts for AdmittedContent<dyn tilo_actions::remote::ContentPuts> {
    fn begin(
        &self,
        caller: &Caller,
        size: u64,
    ) -> Result<Box<dyn tilo_actions::remote::ContentPut>, ActionCallError> {
        self.content.begin(self.run(caller)?, size)
    }

    fn put_many(&self, caller: &Caller, files: &[Vec<u8>]) -> Result<Vec<String>, ActionCallError> {
        self.content.put_many(self.run(caller)?, files)
    }

    fn have(&self, caller: &Caller, hashes: &[String]) -> Result<Vec<bool>, ActionCallError> {
        self.content.have(self.run(caller)?, hashes)
    }

    fn finish(
        &self,
        caller: &Caller,
        put: Box<dyn tilo_actions::remote::ContentPut>,
    ) -> Result<String, ActionCallError> {
        self.content.finish(self.run(caller)?, put)
    }
}

impl tilo_actions::remote::ContentGets for AdmittedContent<dyn tilo_actions::remote::ContentGets> {
    fn get(
        &self,
        caller: &Caller,
        content: &str,
        offset: u64,
        length: u64,
    ) -> Result<tilo_actions::remote::ContentPiece, ActionCallError> {
        self.content.get(self.run(caller)?, content, offset, length)
    }
}

/// The device-certificate checks the broker asks: a certificate is in force in a workspace
/// while the workspace's member list, as this device holds it, admits its device.
struct Certificates {
    policy_time: tilo_workspace_service::apps::policy_clock::PolicyTime,
    workspaces: std::sync::Weak<ServiceWorkspaces>,
}

impl Certificates {
    fn held(&self) -> Result<Arc<ServiceWorkspaces>, String> {
        self.workspaces
            .upgrade()
            .ok_or_else(|| "the device's workspace service closed".to_owned())
    }

    fn check_registered_ownership(&self, workspace: uuid::Uuid) -> Result<(), String> {
        let workspace =
            WorkspaceId::parse(&workspace.to_string()).ok_or("invalid exact workspace")?;
        self.held()?.recheck_executions(workspace)
    }

    /// Whether `workspace`'s member list, as this device holds it, admits `cert`'s device.
    fn admitted(&self, workspace: uuid::Uuid, cert: &DeviceCert) -> Result<(), String> {
        let id = WorkspaceId::parse(&workspace.to_string()).ok_or("invalid exact workspace")?;
        let peer = self
            .held()?
            .peer(id)
            .ok_or("this device does not hold that workspace")?;
        peer.permissions(cert.device_public_key, tilo_clock::wall())
            .map(drop)
            .map_err(|error| error.to_string())
    }
}

impl tilo_workspace_service::apps::certificate_authority::CertificateAuthority for Certificates {
    fn policy_time(
        &self,
    ) -> Result<Option<tilo_workspace_service::apps::policy_clock::PolicyTime>, String> {
        self.policy_time
            .now_ns()
            .map_err(|error| error.to_string())?;
        Ok(Some(self.policy_time.clone()))
    }

    fn authorize_with_policy_time(
        &self,
        workspace: uuid::Uuid,
        cert: &DeviceCert,
        time: &tilo_workspace_service::apps::policy_clock::PolicyTime,
    ) -> Result<(), String> {
        if !time.same_origin(&self.policy_time) {
            return Err("original runtime policy origin changed".into());
        }
        let now = time.now_ns().map_err(|error| error.to_string())?;
        self.check_registered_ownership(workspace)?;
        tilo_user_identity::verify_device_cert_at(cert, now).map_err(|error| error.to_string())?;
        self.admitted(workspace, cert)
    }

    fn authorize(&self, workspace: uuid::Uuid, cert: &DeviceCert) -> Result<(), String> {
        self.admitted(workspace, cert)
    }

    fn authorize_join(&self, workspace: uuid::Uuid, cert: &DeviceCert) -> Result<(), String> {
        let now = self
            .policy_time
            .now_ns()
            .map_err(|error| error.to_string())?;
        tilo_user_identity::verify_device_cert_at(cert, now).map_err(|error| error.to_string())?;
        self.check_registered_ownership(workspace)?;
        self.admitted(workspace, cert)
    }
}

/// One protocol the endpoint answers beside the device actions, and what answers it.
type TerminalProtocols = Vec<(&'static [u8], Box<dyn iroh::protocol::DynProtocolHandler>)>;

/// What the runtime's endpoint is served with: the workspaces it keeps, the host its person's
/// keys reach, the work it takes and a ticket's certify call.
struct EndpointServing<'a> {
    workspaces: &'a ServiceWorkspaces,
    /// The device action service this profile answers its server's work on, when it
    /// takes work at all.
    take_work: Option<take_work::TakesWork>,
    /// The one action service the device actions are answered on: its workspaces', or the work
    /// it takes from its server, admitting a ticket's certify call while a computer asks.
    actions: tilo_peer_state::actions::ActionService,
    /// The workspace host this device runs, when it governs the workspace founded here or the
    /// workspaces placed on a server: the person's keys open their sessions with it here.
    host: Option<&'a tilo_workspace_service::service::WorkspaceService>,
    /// What a server answers for what it keeps.
    kept: tilo_server::daemon::Protocols,
    /// The terminal streams of the shells this device runs, which the devices it hands them to
    /// dial.
    terminals: TerminalProtocols,
}

/// Serve what this runtime answers on the endpoint its workspaces run, from one router: the
/// workspaces this device keeps, its host, the work it takes and a ticket's certify call. An
/// endpoint's protocols are fixed once bound, so everything it answers is accepted here.
fn serve_device_endpoint(serving: EndpointServing<'_>) -> iroh::protocol::Router {
    let EndpointServing {
        workspaces,
        take_work,
        actions,
        host,
        kept,
        terminals,
    } = serving;
    let runtime = tilo_rt::app().handle();
    let _entered = runtime.enter();
    let mut router = iroh::protocol::Router::builder(workspaces.endpoint());
    // The workspace host answers the person's keys as every host does. Every device answers
    // the device actions once, on `actions`; a member that takes its server's work answers
    // that work's terminals beside it.
    for (alpn, handler) in host
        .map(tilo_workspace_service::service::WorkspaceService::protocols)
        .into_iter()
        .flatten()
        .filter(|(alpn, _)| alpn != tilo_peer_state::actions::ACTIONS_ALPN)
    {
        router = router.accept(alpn, handler);
    }
    if let Some((alpn, handler)) = take_work.map(take_work::TakesWork::terminals) {
        router = router.accept(alpn, handler);
    }
    for (alpn, handler) in kept {
        router = router.accept(alpn, handler);
    }
    for (alpn, handler) in terminals {
        router = router.accept(alpn, handler);
    }
    router = router.accept(tilo_peer_state::actions::ACTIONS_ALPN, actions);
    router.spawn()
}

/// The broker's channel state, with the profile's durable collaboration
/// history. Without it scrollback is limited to memory, as in the desktop
/// session, and the host still serves.
fn channels(root: &Path) -> BrokerChannels {
    let mut channels = BrokerChannels::default();
    match CollabHistoryStore::open(root.join(COLLABORATION_HISTORY)) {
        Ok(history) => channels.history = Some(history),
        Err(error) => tracing::warn!(
            %error,
            "durable collaboration history unavailable; scrollback is limited to memory"
        ),
    }
    channels
}

/// What the files of the workspaces whose work this device takes are held with.
struct TakenFiles<'a> {
    root: &'a Path,
    workspaces: &'a Arc<ServiceWorkspaces>,
    media: &'a profile::MediaHost,
    apps: &'a Arc<apps::Apps>,
    author: take_work::files::Author,
}

/// Where the files of the workspaces this device holds are shown, each at a folder in this
/// user's home. When it `takes` work, the files of every workspace it holds are held from
/// the start, where its shells run. So are they on a computer with no login, which `founded` a
/// workspace here: its folders are where the person keeps their files, the founded workspace's
/// and every one they joined, with no `tilo folders` to choose them. Without a home, or
/// before the broker serves record content, none are held: the workspaces still sync, only their
/// files are missing here.
fn take_work_files(
    parts: &TakenFiles<'_>,
    takes: bool,
    founded: Option<WorkspaceId>,
    stage: &mut dyn FnMut(&str),
) -> Option<Arc<take_work::files::Replicas>> {
    let home = tilo_paths::home()?;
    let content = parts.workspaces.record_content_service()?;
    let held = Arc::downgrade(parts.workspaces);
    let replicas = Arc::new(take_work::files::Replicas::new(
        take_work::files::ReplicaHost {
            profile: parts.root.to_path_buf(),
            home,
            own_folder: None,
            store: parts.media.store(),
            author: parts.author,
            writers: Arc::new(tilo_fs::writer::WriterResolver::default()),
            credentials: Box::new(move |workspace| {
                held.upgrade()
                    .ok_or_else(|| io::Error::other("the workspace service closed"))?
                    .files_credentials(workspace)
            }),
            content,
            apps: Arc::clone(parts.apps),
        },
        Arc::downgrade(parts.workspaces),
    ));
    let workspaces: Vec<_> = parts
        .workspaces
        .held()
        .into_iter()
        .filter(|_| takes || founded.is_some())
        .collect();
    if !workspaces.is_empty() {
        stage("showing the workspaces' files");
    }
    for (workspace, name, _) in workspaces {
        let _ = hold_files(&replicas, &name, workspace);
    }
    Some(replicas)
}

/// Hold `workspace`'s files in `replicas`, under the name its governance gives
/// it: the folder that shows them, unless they cannot be held.
pub(super) fn hold_files(
    replicas: &take_work::files::Replicas,
    name: &str,
    workspace: WorkspaceId,
) -> Option<PathBuf> {
    let name = name.to_owned();
    replicas
        .hold(&workspace.to_string(), &name)
        .inspect_err(|error| {
            tracing::warn!(%error, %workspace, "a workspace's files are not held here");
        })
        .ok()
}

impl Daemon {
    /// The account sign-ins this daemon serves, which hand an origin connector its own
    /// ([`super::account_sign_ins::AccountSignIns::hand`]); `None` where this login has no runtime
    /// directory to serve them in.
    #[cfg(unix)]
    #[must_use]
    pub fn account_sign_ins(&self) -> Option<&super::account_sign_ins::AccountSignIns> {
        self.account_sign_ins.as_ref()
    }

    /// Told each time this device starts holding a workspace as a member: what a computer that
    /// just logged in waits on before anything is admitted to its workspace.
    pub(crate) fn entered(&self) -> Arc<tokio::sync::Notify> {
        self.workspaces.entered()
    }

    /// Where the files of the workspaces whose work this device takes are held.
    #[cfg(test)]
    pub(crate) fn replicas(&self) -> Option<&Arc<take_work::files::Replicas>> {
        self.replicas.as_ref()
    }

    /// The folder mirroring the files of `workspace`, a workspace whose work this device takes,
    /// while they are held: what a member taking the workspace's work maps the workspace's files
    /// to.
    #[must_use]
    pub fn joined_files_folder(&self, workspace: &str) -> Option<PathBuf> {
        self.replicas.as_ref()?.folder(workspace)
    }

    /// Where the sync of the folder of `workspace`, a workspace this profile joined, stands,
    /// while its files are held: what `tilo folders` tells the person of it.
    #[must_use]
    pub fn joined_files_status(&self, workspace: &str) -> Option<tilo_fs::tree_sync::Status> {
        self.replicas.as_ref()?.status(workspace)
    }
}

/// Run the agents of the workspaces this device's service holds, in this daemon: the one engine
/// a workspace's host runs, each turn's calls decided by `broker` as the person's own calls are.
/// An agent whose person's root is not here is asked for with a ticket `tickets` serves, which
/// its person redeems to approve this device's key for it.
fn start_agents(broker: &BrokerHandle, workspaces: &ServiceWorkspaces, tickets: &Arc<Tickets>) {
    let Some(broker) = broker.app_action_broker() else {
        tracing::warn!("this device's broker is not up, so it runs no agents of its workspaces");
        return;
    };
    tilo_rt::app()
        .handle()
        .spawn(tilo_workspace_service::agents::run_on_device(
            workspaces.copies().clone(),
            broker.device_host(),
            Some(Arc::clone(tickets) as _),
        ));
}

/// Run the triggers of the shortcuts the person saved in this device's workspaces: each firing
/// that is this device's turn starts on `broker`.
fn start_triggers(
    broker: &BrokerHandle,
    workspaces: &ServiceWorkspaces,
    invocations: &Arc<Invocations>,
) {
    let Some(broker) = broker.app_action_broker() else {
        tracing::warn!("this device's broker is not up, so it starts no shortcut's trigger");
        return;
    };
    tilo_rt::app().handle().spawn(
        tilo_workspace_service::apps::triggers::run_on_device_watching_invocations(
            workspaces.copies().clone(),
            std::sync::Arc::new(broker),
            Arc::clone(invocations),
        ),
    );
}

/// Answer the person's keys from the broker from now on, and serve the workspaces' pages
/// through the daemon's apps: the workspace founded here when this computer founded one
/// (`founded`), else every workspace a keeping server (`serving`) holds.
fn serve_key_sessions(
    key_sessions: &crate::key_sessions::KeySessions,
    (broker, invocations, apps): (&BrokerHandle, &Arc<Invocations>, &Arc<apps::Apps>),
    workspaces: &Arc<ServiceWorkspaces>,
    (actor, founded, serving): (&str, Option<collab_types::WorkspaceId>, bool),
    policy_origin: &tilo_workspace_service::apps::policy_clock::PolicyOrigin,
) -> io::Result<()> {
    let broker = broker
        .app_action_broker()
        .ok_or_else(|| io::Error::other("runtime action broker is unavailable"))?;
    let policy_time = policy_origin.capture().map_err(io::Error::other)?;
    let sessions: Arc<dyn crate::key_sessions::Sessions> = match founded {
        Some(workspace) => {
            let workspace = workspace.to_string();
            let provider =
                action_provider(&broker, workspaces, None, invocations, actor, &workspace)?;
            Arc::new(host_runs::HostRuns {
                provider,
                workspace,
                actor: actor.to_owned(),
                policy_time,
            })
        }
        None if serving => Arc::new(host_runs::KeptRuns {
            broker,
            workspaces: Arc::clone(workspaces),
            apps: Arc::clone(apps),
            invocations: Arc::clone(invocations),
            actor: actor.to_owned(),
            policy_time,
        }),
        None => {
            return Err(io::Error::other(
                "this device serves no workspace for its person's keys",
            ));
        }
    };
    key_sessions.serve(
        Arc::new(PersonalDevices::new(
            workspaces.copies().clone(),
            workspaces.device_cert(),
        )),
        sessions,
    );
    #[cfg(unix)]
    key_sessions
        .pages_served()
        .show_through(Arc::new(AppViews(Arc::clone(apps))));
    #[cfg(not(unix))]
    let _ = apps;
    Ok(())
}

/// `workspace`'s dispatch as a key's run or a page of it reaches it: the broker's certified
/// local action provider, admitted for `actor` and kept for one workspace.
///
/// # Errors
/// The workspace is not one this device may act in, or its admission cannot be opened.
pub(super) fn action_provider(
    broker: &AppActionBroker,
    workspaces: &Arc<ServiceWorkspaces>,
    apps: Option<Arc<apps::Apps>>,
    invocations: &Arc<Invocations>,
    actor: &str,
    workspace: &str,
) -> io::Result<RuntimeActionProvider> {
    let admitting = Arc::clone(workspaces);
    let (named, actor) = (workspace.to_owned(), actor.to_owned());
    let admission = block_on_owned(async move { admitting.admit_execution(&named, &actor).await })
        .map_err(io::Error::other)?;
    let provider = broker
        .certified_local_action_provider(admission.caller(), admission.presentation())
        .map_err(io::Error::other)?;
    admission
        .check(workspaces.copies())
        .map_err(io::Error::other)?;
    Ok(RuntimeActionProvider {
        broker: broker.clone(),
        workspace: workspace.to_owned(),
        workspaces: Arc::clone(workspaces),
        apps,
        session: None,
        tickets: None,
        invocations: Arc::clone(invocations),
        asked: None,
    })
}

/// Apps' views on this device's web pages: each an instance of the app this daemon starts for
/// the page's person, laid out by a window process that draws nothing ([`apps::Apps::open_view`]).
#[cfg(unix)]
struct AppViews(Arc<apps::Apps>);

#[cfg(unix)]
impl crate::key_sessions::PageViews for AppViews {
    fn open(
        &self,
        workspace: &str,
        app: &str,
        caller: &Caller,
    ) -> Result<
        tilo_server_protocol::page::window::WindowLink,
        tilo_server_protocol::page::PageRefusal,
    > {
        self.0
            .open_view(caller, app, workspace, None, None)
            .map(tilo_server_protocol::page::window::WindowLink::over)
            .map_err(tilo_server_protocol::page::PageRefusal::Unavailable)
    }
}

/// This device's endpoint, keyed by its device key: bound to `listen` on
/// every interface when a host names the port it is reached at, and to
/// whatever port the machine gives it otherwise. Its relays are the person's
/// ([`tilo_endpoint_reachability::relays`]): a runtime on a machine of theirs
/// relays and resolves as their other devices do.
pub(super) fn listen_endpoint(key: &SigningKey, listen: Option<u16>) -> io::Result<iroh::Endpoint> {
    let relays = tilo_endpoint_reachability::relays();
    let mut builder = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Custom(relays.relays))
        .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()));
    if relays.n0_dns {
        builder = builder
            .address_lookup(tilo_endpoint_reachability::collaborating())
            .address_lookup(iroh::address_lookup::DnsAddressLookup::n0_dns());
    }
    if let Some(port) = listen {
        // One program on a computer takes its workspaces' work on this port: a second runtime of a
        // login here is told so, not left with the socket layer's words.
        if let Err(error) = std::net::UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, port))
            && error.kind() == io::ErrorKind::AddrInUse
        {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!(
                    "another program on this computer holds UDP port {port}, where a logged-in \
                     Tilo runtime takes its workspaces' work: most likely another `tilo-runtime` \
                     of a Tilo login here. Stop it, then start this one again."
                ),
            ));
        }
        builder =
            tilo_server_client::host::listening_on(builder, port).map_err(io::Error::other)?;
    }
    tilo_rt::app()
        .handle()
        .block_on(builder.bind())
        .map_err(io::Error::other)
}

/// The identity service apps sign with: over `workspaces`, handing out route
/// keys from `keys`, and in the person's own workspace only for the records of
/// theirs an app's surface declares (see [`AttachedSurfaces`] for `apps`
/// and `dev`).
fn identity_service(
    socket: PathBuf,
    workspaces: Arc<dyn WorkspaceMembership>,
    keys: Arc<dyn tilo_identity::broker::GroupKeySource>,
    apps: &Arc<apps::Apps>,
    dev: bool,
) -> io::Result<tilo_identity::broker::IdentityBrokerHandle> {
    let person = AttachedSurfaces {
        apps: Arc::clone(apps),
        dev,
    };
    tilo_identity::broker::spawn(socket, workspaces, keys, Arc::new(person))
}

/// The person-record kinds of the surfaces this runtime holds. Started with
/// `--dev`, an app that is not installed is a build under development (as
/// [`RecordCreation`] treats it): the surface it attaches naming, which
/// `tilo-app dev` sets, is taken as its own, so its person records reach
/// the person's workspace. An installed app runs only with its package's signed
/// surface, and no attach replaces a surface the runtime already holds.
struct AttachedSurfaces {
    apps: Arc<apps::Apps>,
    dev: bool,
}

impl tilo_identity::broker::PersonResourceTypes for AttachedSurfaces {
    fn person_resource_types(&self, app_id: &str) -> Vec<String> {
        ui::app_surface::person_resource_types(app_id)
    }

    fn reads_documents_of(&self, reader: &str, owner: &str) -> bool {
        ui::app_surface::reads_app(reader, owner)
    }

    fn attached(&self, app_id: &str, surface: &[u8]) {
        if self.dev
            && self.apps.declared_resource_kinds(app_id).is_none()
            && ui::app_surface::bytes_of(app_id).is_none()
        {
            // Changes are decided against the surface their op names, read
            // from the content store as a package's is: the form it is kept
            // by, as registering the package keeps it.
            let Ok((_, kept)) = collab_types::surface::SurfaceFile::kept(surface) else {
                tracing::warn!(app_id, "the development surface does not parse");
                return;
            };
            if let Err(error) = tilo_content_device::for_app().put(kept.as_bytes()) {
                tracing::warn!(%error, app_id, "the development surface could not be stored");
                return;
            }
            tilo_workspace_service::apps::register_package_surface(app_id, surface);
        }
    }
}

#[cfg(test)]
#[path = "services_tests.rs"]
pub(crate) mod tests;

/// The person's other devices, asked from this runtime's endpoint as this device: what the
/// forwarding tool lists, and what a `--connect --device` client is carried through.
fn other_devices(
    workspaces: &Arc<ServiceWorkspaces>,
) -> Option<crate::other_devices::OtherDevices> {
    let (_actions, endpoint, runtime) = workspaces.turn_action_service()?;
    let cert = workspaces.device_cert().clone();
    let signer = workspaces.signer();
    let revocations = {
        let mut revocations = tilo_user_identity::RevocationList::new();
        for (_, _, peer) in workspaces.held() {
            for entry in peer.revocations() {
                let _ = revocations.insert(entry);
            }
        }
        revocations
    };
    let keys = tilo_workspace_service::agents::agent_keys::this_device().ok()?;
    Some(crate::other_devices::OtherDevices::new(
        endpoint,
        runtime,
        (cert.user_public_key, cert.device_public_key),
        (cert, signer),
        revocations,
        keys,
        Arc::new(Known(Arc::downgrade(workspaces))),
    ))
}

/// The workspaces this device holds, as its other devices are looked up in them. Whether one of
/// them is connected here now is not known: each is asked, and says so when it is away.
struct Known(std::sync::Weak<ServiceWorkspaces>);

impl crate::other_devices::Whereabouts for Known {
    fn devices(&self, workspace: &str) -> Vec<tilo_user_identity::DeviceCert> {
        self.0
            .upgrade()
            .and_then(|workspaces| {
                workspaces
                    .peer(tilo_desktop_model::WorkspaceId::parse(workspace)?)
                    .map(|peer| peer.certificates())
            })
            .unwrap_or_default()
    }

    fn workspaces(&self) -> Vec<String> {
        self.0.upgrade().map_or_else(Vec::new, |workspaces| {
            workspaces
                .held()
                .into_iter()
                .map(|(workspace, ..)| workspace.to_string())
                .collect()
        })
    }
}

#[cfg(test)]
#[path = "second_person_tests.rs"]
mod second_person_tests;
