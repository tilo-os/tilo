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
//! its own account starts a new person on it (`devices#found`, [`found_account_person`]). Here the daemon
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
use crate::{RuntimeServices, apps, host, profile};
use ed25519_dalek::SigningKey;
use serde_json::{Map, Value, json};
#[cfg(unix)]
use std::process::{Child, Command};
use std::{
    io,
    path::{Path, PathBuf},
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
use tilo_server_protocol::page::PageNotice;
use tilo_server_protocol::servers::person_workspace_id;
use tilo_server_protocol::workspaces::{
    BRANCH_OWN_WORKSPACE, CREATE_OWN_WORKSPACE, CREATE_WEBHOOK, CreatedWorkspace, DELETE_WEBHOOK,
    DELETE_WORKSPACE, LIST_WEBHOOKS, REVOKE_KEY, RUN_UNATTENDED, WorkspaceLabels, WorkspaceListing,
};
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::agents::agent_keys::{AgentSigner, Signed, this_device};
use tilo_workspace_service::agents::events::WorkspaceEvents;
use tilo_workspace_service::agents::initiative::Initiative;
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
use tilo_workspace_service::workspaces::person_workspace;
use tilo_workspace_service::workspaces::person_workspace::devices::{self, Devices};
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
pub const SET_UP_FIRST: &str = "This computer has no Tilo identity yet: set it up with \
     `tilo login --new-identity`, or with `tilo login` and the code from your Tilo tab (Set up \
     with your agent).";

/// This computer's daemon, as the device. Drop applications first.
pub struct Daemon {
    // Retired before broker fields: retained original ownership cannot survive
    // replacement of the runtime that supplied its policy time.
    policy_origin: tilo_workspace_service::apps::policy_clock::PolicyOrigin,
    broker: BrokerHandle,
    /// The links of the shells this daemon starts ([`Self::start_shell`]).
    shell_links: Arc<tilo_workspace_service::apps::shell::ShellLinks>,
    workspaces: Arc<ServiceWorkspaces>,
    /// The device's live session: the one place this device is in each workspace's call and
    /// presence, which an app's call lane joins through ([`Self::app_call_setup`]).
    live: std::sync::OnceLock<Arc<super::live_relay::Live>>,
    /// Whether this run walked the old keeper's kept secrets into the person's vault: done once
    /// the workspace is held, so an app launch costs no keyring read after the first.
    pub(crate) old_secrets: std::sync::atomic::AtomicBool,
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
    /// The account sign-ins, while something mounted serves them
    /// ([`super::capabilities::Capabilities::account_sign_ins`]).
    account_sign_ins: Option<Box<dyn std::any::Any + Send + Sync>>,
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
    /// The calls the device took before its person's services ran, which they answer too: a
    /// founding call followed after they start reads its state here.
    kept: super::unclaimed::Calls,
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
    /// The device's key and lock, when this daemon took them itself: one started under the lock
    /// the daemon's device part keeps ([`Self::start_locked`]) has none of its own. The key is
    /// let go of first, so the daemon that takes the device over finds it free.
    _lock: Option<(
        tilo_device_identity::persist::DeviceKeyLock,
        advisory_lock::FileLock,
    )>,
}

impl Daemon {
    /// The device's live session, once the daemon runs it.
    #[cfg(test)]
    pub(crate) fn live(&self) -> Option<&Arc<super::live_relay::Live>> {
        self.live.get()
    }

    /// What an app's call lane needs to carry one of its calls into a workspace's call through
    /// the live session this device holds: the device's word there, and the way to join a call as
    /// the app, through the device's one dispatch under the app's grants. No key material and no
    /// relay are part of it.
    ///
    /// # Errors
    ///
    /// This device runs no live session, or the app's workspace has none: the lane refuses every
    /// carry with the reason.
    pub(crate) fn app_call_setup(
        self: &Arc<Self>,
        at: &crate::seats::AppAt,
    ) -> Result<tilo_live::app_call::CallSetup, String> {
        let live = self
            .live
            .get()
            .ok_or("this computer's live session is not running")?;
        let workspace = tilo_desktop_model::WorkspaceId::parse(&at.workspace)
            .ok_or("that app runs in no workspace")?;
        let call = live
            .call_session(workspace)
            .ok_or("this computer holds no live session for the app's workspace")?;
        let daemon = Arc::downgrade(self);
        let (app, named) = (at.app.clone(), at.workspace.clone());
        let join: tilo_live::app_call::JoinCall = Arc::new(move |call| {
            let daemon = daemon
                .upgrade()
                .ok_or("this computer's daemon is stopping")?;
            let (tree, caller) = daemon
                .app_tree(&app, &named)
                .map_err(|error| error.to_string())?;
            let request = match call {
                Some(call) => super::live_speaking::request(
                    super::live_speaking::JOIN,
                    serde_json::to_value(super::live_speaking::Joining {
                        channel: call.channel,
                        name: call.name,
                        bridge: call.bridge,
                    })
                    .map_err(|error| error.to_string())?,
                ),
                None => super::live_speaking::request(
                    super::live_speaking::LEAVE,
                    serde_json::Value::Object(serde_json::Map::new()),
                ),
            };
            let until = tilo_clock::now() + JOIN_WAIT;
            let receipt = tilo_actions::remote::ActionProvider::invoke(&tree, &caller, request)
                .wait_timeout(JOIN_WAIT)
                .ok()
                .flatten()
                .ok_or("the device did not answer the join in time")?
                .map_err(|error| error.to_string())?;
            // An admitted join runs on: it is answered once the device is in the call.
            let receipt = match receipt.status {
                tilo_actions::remote::ReceiptStatus::Accepted => tilo_actions::remote::settle_with(
                    &tree,
                    &caller,
                    receipt.invocation,
                    None,
                    until,
                )
                .wait_timeout(JOIN_WAIT)
                .ok()
                .flatten()
                .flatten()
                .map_or(receipt, |state| state.receipt()),
                _ => receipt,
            };
            match receipt.status {
                tilo_actions::remote::ReceiptStatus::Completed { output } => match output.value {
                    Some(value) => serde_json::from_value::<super::live_speaking::Joined>(value)
                        .map(|joined| Some((joined.channel, joined.generation)))
                        .map_err(|error| error.to_string()),
                    None => Ok(None),
                },
                tilo_actions::remote::ReceiptStatus::Failed { error } => Err(error.message),
                other => Err(format!("the join did not finish: {other:?}")),
            }
        });
        Ok(tilo_live::app_call::CallSetup {
            service: at.app.clone(),
            speaker: Arc::clone(call.speaker()),
            join,
        })
    }
}

/// How long an app's join of a call may take: its keys may have to arrive first.
const JOIN_WAIT: std::time::Duration = std::time::Duration::from_secs(20);

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
    /// This computer has no identity its person's services can start as ([`SET_UP_FIRST`]).
    fn here(profile: &Path) -> io::Result<Self> {
        let keys = tilo_api::calls::device_keys();
        let place =
            tilo_device_identity::login::Place::here(keys.clone()).map_err(io::Error::other)?;
        // The one check the daemon's device-only state asks too: the account's identity names a
        // person this device holds a certificate for. This daemon lists its own device with the
        // person's once it holds their record, so it starts from its certificate before any host
        // lists it.
        let (key, cert) = tilo_identity::trusted_device::serving_in(
            &device_identity_dir(profile, this_account_uid(), &keys)?,
            &keys,
        )
        .map_err(|error| {
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

tokio::task_local! {
    /// The person whose root-only call runs on this device, while one does: what it signs is
    /// signed with that person's root, never the device's own.
    static ROOT_CALLER: Option<[u8; 32]>;
}

/// Run `future` as the root-only call of `person`.
pub(crate) async fn with_root_caller<F: std::future::Future>(
    person: Option<[u8; 32]>,
    future: F,
) -> F::Output {
    ROOT_CALLER.scope(person, future).await
}

/// The person whose root-only call runs here, while one does.
pub(crate) fn root_caller() -> Option<[u8; 32]> {
    ROOT_CALLER.try_with(|person| *person).ok().flatten()
}

/// What runs when a person is founded while this daemon serves: their root marked in their
/// devices record, and their root-only asks served here, without the daemon starting again.
pub(crate) type PersonFounded = Arc<dyn Fn([u8; 32], u32) + Send + Sync>;

static PERSON_FOUNDED: std::sync::OnceLock<std::sync::Mutex<Option<PersonFounded>>> =
    std::sync::OnceLock::new();

/// Have `hook` run when a person is founded while this daemon serves, in place of one an earlier
/// start of its services left.
pub(crate) fn when_person_founded(hook: PersonFounded) {
    *PERSON_FOUNDED
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock_recover() = Some(hook);
}

/// Run what this daemon does for `person`, founded while it serves.
pub(crate) fn person_founded(person: [u8; 32], uid: u32) {
    let hook = PERSON_FOUNDED
        .get()
        .and_then(|held| held.lock_recover().clone());
    if let Some(hook) = hook {
        hook(person, uid);
    }
}

/// Found the person of the account `uid` on this computer, the one way a person is made
/// ([`tilo_identity::provisioning::create_person`]): their root is kept for that account alone,
/// founds their own workspace, and certifies this device's key in `keys` with its authority. An
/// account that holds its person already makes nobody new: its root certifies the device again,
/// and one whose identity a login or founding stopped partway left is completed from the
/// certificate this device already holds, or refused naming what to do.
///
/// # Errors
/// The identity cannot be made or kept, or this computer holds an identity nothing can start as.
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
    let active = tilo_identity::store::resolve_active_identity(dir);
    let metadata = match &active {
        Some(record) => Some(
            tilo_identity::store::load_metadata_from_path(record)
                .map_err(|error| io::Error::other(error.to_string()))?,
        ),
        None => None,
    };
    if let (Some(record), Some(metadata)) = (&active, &metadata)
        && metadata.single_device
    {
        // A root kept here signs for this device again. One that cannot be opened is what a
        // founding stopped halfway through its record leaves, and starts nothing.
        if tilo_identity::store::load_from_path(record.clone()).is_err() {
            return Err(incomplete(dir, keys, "its key cannot be opened"));
        }
        tilo_identity::provisioning::ensure_collaboration_identity(dir, keys, storage)?;
        return Ok(metadata.group_public_key);
    }
    // No root is kept here: the person this device already holds a certificate for is the one
    // this account is, so a login or founding interrupted before its identity record was written
    // is completed rather than a second person made over it.
    let (device, _) = tilo_device_identity::persist::load_or_create_device_key(keys)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let held = tilo_device_identity::login::served_cert(keys)
        .filter(|cert| cert.device_public_key == device.verifying_key().to_bytes())
        // A certificate that admits devices is the root's own: it is the device's, not a joined
        // account's to take up here.
        .filter(|cert| !cert.capabilities.can_admit_devices)
        .filter(|cert| {
            metadata
                .as_ref()
                .is_none_or(|metadata| metadata.group_public_key == cert.user_public_key)
        });
    if let Some(cert) = held {
        tilo_identity::provisioning::adopt_login_certificate(
            dir,
            keys,
            &cert,
            tilo_clock::unix_now_ns(),
        )
        .map_err(|error| io::Error::other(error.to_string()))?;
        return Ok(cert.user_public_key);
    }
    if metadata.is_some() {
        return Err(incomplete(
            dir,
            keys,
            "this device holds no certificate for the person it names",
        ));
    }
    tilo_identity::provisioning::create_person(dir, keys, name.trim(), "", storage)
}

/// What founding refuses on a computer whose identity says a person but holds nothing it can
/// start as them with: the two ways out, by name.
fn incomplete(
    dir: &std::path::Path,
    keys: &tilo_device_identity::IdentityPaths,
    why: &str,
) -> io::Error {
    let device = keys
        .device_privkey
        .parent()
        .unwrap_or(keys.device_privkey.as_path())
        .display();
    io::Error::other(format!(
        "This computer's Tilo identity is incomplete ({why}). Log in with a ticket from one of \
         the person's devices (`tilo login <ticket>`), or remove this computer's Tilo identity \
         ({}, {device}) and set up a new one (`tilo login --new-identity`).",
        dir.display()
    ))
}

/// Where the person the account `uid` made here is kept: a directory of its own under this
/// computer's identity store, so one account's root is never read for another's.
///
/// # Errors
/// This computer keeps no identity store.
pub fn account_identity_dir(uid: u32) -> io::Result<std::path::PathBuf> {
    Ok(tilo_identity::store::identity_dir_from_env()?
        .join("accounts")
        .join(uid.to_string()))
}

/// What this process admits the person's devices by: one answer for the process, installed
/// once, which each start of the daemon points at the record its own workspaces hold. A daemon
/// stopped and started again in one process (Android's runs beside its windows) answers from its
/// new workspaces, never from the ones the last start dropped.
pub(crate) fn devices_trust() -> &'static Arc<tilo_identity::sources::DevicesTrust> {
    static DEVICES: std::sync::OnceLock<Arc<tilo_identity::sources::DevicesTrust>> =
        std::sync::OnceLock::new();
    DEVICES.get_or_init(|| {
        let devices = Arc::new(tilo_identity::sources::DevicesTrust::new());
        tilo_identity::sources::install_device_trust(Arc::clone(&devices) as _);
        devices
    })
}

/// Where the person of the account this process runs as is kept.
///
/// # Errors
/// This computer keeps no identity store.
pub fn this_accounts_root_dir() -> io::Result<std::path::PathBuf> {
    account_identity_dir(this_account_uid())
}

/// The identity store of the account this device acts as its person, with this device's key and
/// certificates kept in `keys`: the daemon's own account `own` when its identity names the person
/// this device's certificate does; else the account recorded at founding in the daemon's
/// `profile` ([`tilo_api::daemon::admission::person_record`]), when that account's identity names
/// them.
///
/// Another account on the computer whose identity names the same person is never taken for it:
/// only the account founding recorded is the one the device's services start as.
///
/// # Errors
/// This computer keeps no identity store, the profile records no account that made its person,
/// that account keeps no identity record here or one that cannot be read, or its identity names
/// another person than this device's certificate does. Each says what to run.
pub(crate) fn device_identity_dir(
    profile: &Path,
    own: u32,
    keys: &tilo_device_identity::IdentityPaths,
) -> io::Result<std::path::PathBuf> {
    let own_store = account_identity_dir(own)?;
    let Some(person) =
        tilo_device_identity::login::served_cert(keys).map(|cert| cert.user_public_key)
    else {
        return Ok(own_store);
    };
    if store_names(&own_store, person) {
        return Ok(own_store);
    }
    let recorded = tilo_api::daemon::admission::recorded_accounts(profile)?;
    let Some((uid, _)) = recorded.iter().find(|(_, record)| record.device) else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "this computer's Tilo profile ({}) records no account that made its person, so \
                 this device cannot tell whose identity to start: set its person up with `tilo \
                 login --new-identity`, or log in with a ticket from one of the person's devices \
                 (`tilo login <ticket>`).",
                profile.display()
            ),
        ));
    };
    let dir = account_identity_dir(*uid)?;
    let Some(record) = tilo_identity::store::resolve_active_identity(&dir) else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "this computer's Tilo profile records account {uid} as the one that made its \
                 person, but that account keeps no identity record here: log in with a ticket \
                 from one of the person's devices (`tilo login <ticket>`), or set a new identity \
                 up here with `tilo login --new-identity`."
            ),
        ));
    };
    let metadata = tilo_identity::store::load_metadata_from_path(&record).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "this computer's Tilo profile records account {uid} as the one that made its \
                 person, but that account's identity record here cannot be read: log in with a \
                 ticket from one of the person's devices (`tilo login <ticket>`), or set a new \
                 identity up here with `tilo login --new-identity`."
            ),
        )
    })?;
    if metadata.group_public_key != person {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "this computer's Tilo profile records account {uid} as the one that made its \
                 person, but that account's identity names another person than this device's \
                 certificate does: log in with a ticket from the certificate's person's devices \
                 (`tilo login <ticket>`), or set a new identity up here with `tilo login \
                 --new-identity`."
            ),
        ));
    }
    Ok(dir)
}

/// Whether the identity store at `dir` names `person` as the account's own.
fn store_names(dir: &Path, person: [u8; 32]) -> bool {
    tilo_identity::store::resolve_active_identity(dir)
        .and_then(|record| tilo_identity::store::load_metadata_from_path(&record).ok())
        .is_some_and(|metadata| metadata.group_public_key == person)
}

/// The uid of the account this process runs as.
#[must_use]
pub fn this_account_uid() -> u32 {
    crate::seats::own_account()
}

/// The root this computer keeps for the account `uid` alone, when it keeps one.
///
/// # Errors
/// The identity store does not hand it over.
pub(crate) fn root_record(uid: u32) -> io::Result<std::path::PathBuf> {
    let dir = account_identity_dir(uid)?;
    tilo_identity::store::held_root(&dir).ok_or_else(|| {
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

/// The record of the root this computer keeps for `person`, when their account is signed in
/// here: what signs a call only their root signs, and never another account's.
///
/// # Errors
/// This computer keeps no root for the person, or their account is not signed in.
pub(crate) fn root_record_for_person(person: [u8; 32]) -> io::Result<std::path::PathBuf> {
    // A daemon serving every account of the computer opens a root only while its account is
    // signed in; a person's own service, which is no account's but its owner's, opens its own.
    if let Some(admission) = tilo_api::daemon::admission::installed() {
        if let Some(uid) = admission.signed_in_account_for(person) {
            return root_record(uid);
        }
        // A person's own service, which is no account's but its owner's, opens the root kept for
        // the account it runs as: no sign-in names that account there.
        if !admission.system() {
            let uid = crate::seats::own_account();
            if let Ok(record) = root_record(uid)
                && tilo_identity::store::load_metadata_from_path(&record)
                    .is_ok_and(|metadata| metadata.group_public_key == person)
            {
                return Ok(record);
            }
        }
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "this person's account is not signed in on this computer",
        ));
    }
    if device_person() == Some(person) {
        return device_root_record();
    }
    // A process that keeps no accounts record (a person's own service, a test's daemon) finds a
    // person's root by what each account's identity names.
    let accounts = tilo_identity::store::identity_dir_from_env()?.join("accounts");
    for entry in std::fs::read_dir(&accounts).into_iter().flatten().flatten() {
        let Ok(uid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(record) = root_record(uid) else {
            continue;
        };
        if tilo_identity::store::load_metadata_from_path(&record)
            .is_ok_and(|metadata| metadata.group_public_key == person)
        {
            return Ok(record);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "this computer keeps no root of this person's",
    ))
}

/// Whether this computer keeps `person`'s root while their account is signed in here.
#[must_use]
pub(crate) fn holds_root_for_person(person: [u8; 32]) -> bool {
    root_record_for_person(person).is_ok()
}

/// The person this device's one identity store is: who a personal computer serves.
pub(crate) fn device_person() -> Option<[u8; 32]> {
    if let Some(admission) = tilo_api::daemon::admission::installed()
        && let Some(uid) = admission.device_person_uid()
        && let Some(held) = admission.recorded_person(uid)
    {
        return Some(held.person);
    }
    // A person's own service keeps their root under the account it runs as.
    let record = root_record(crate::seats::own_account()).ok()?;
    tilo_identity::store::load_metadata_from_path(&record)
        .ok()
        .map(|metadata| metadata.group_public_key)
}

/// Bring back the person of the account `uid` on this computer, whose root is kept for that
/// account alone: a computer's person is its account's, never a root of the device's own.
///
/// # Errors
/// Neither the words nor the backup is given, they do not open, or the identity cannot be kept.
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
pub fn device_root_record() -> io::Result<std::path::PathBuf> {
    device_person_root_record()
}

/// Where the device's person's root is kept: under the account the device was set up from, or,
/// in a person's own service, under the account it runs as.
fn device_person_root_record() -> io::Result<std::path::PathBuf> {
    #[cfg(test)]
    let kept = ROOT_RECORD_FOR_TEST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    #[cfg(test)]
    if let Some(record) = kept {
        return Ok(record);
    }
    let uid = device_person_uid().unwrap_or_else(crate::seats::own_account);
    root_record(uid)
}

/// In a test: the record [`device_root_record`] answers while [`root_record_for_test`]'s guard
/// is held, as if this computer kept that root.
#[cfg(test)]
static ROOT_RECORD_FOR_TEST: std::sync::Mutex<Option<std::path::PathBuf>> =
    std::sync::Mutex::new(None);

/// In a test: have this process's [`device_root_record`] answer `record` until the guard drops.
/// The daemon keeps one root record for the account it runs as, so a test that needs another
/// answers it for the calls it makes, and puts it back after.
#[cfg(test)]
#[must_use]
pub(crate) fn root_record_for_test(record: std::path::PathBuf) -> RootRecordForTest {
    *ROOT_RECORD_FOR_TEST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(record);
    RootRecordForTest(())
}

/// Answers [`device_root_record`] with the test's record until it drops.
#[cfg(test)]
pub(crate) struct RootRecordForTest(());

#[cfg(test)]
impl Drop for RootRecordForTest {
    fn drop(&mut self) {
        *ROOT_RECORD_FOR_TEST
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
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
    // The identities this account's person keeps are the account's own.
    let dir = this_accounts_root_dir()?;
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
    recovery_words_in(&device_root_record()?)
}

/// The 24 recovery words of the root at `record`.
///
/// # Errors
/// It is not a root, or the secret store does not hand it over.
pub fn recovery_words_in(record: &std::path::Path) -> io::Result<String> {
    Ok(tilo_identity::store::recovery_words(record)?
        .as_str()
        .to_owned())
}

/// A backup of the person's root this daemon keeps, sealed with `passphrase`: its bytes.
///
/// # Errors
/// It keeps none, or the backup cannot be written.
pub fn backup(passphrase: &str) -> io::Result<Vec<u8>> {
    backup_in(&device_root_record()?, passphrase)
}

/// A backup of the root at `record`, sealed with `passphrase`: its bytes.
///
/// # Errors
/// The backup cannot be written.
pub fn backup_in(record: &std::path::Path, passphrase: &str) -> io::Result<Vec<u8>> {
    let dir = record
        .parent()
        .ok_or_else(|| io::Error::other("the root's record has no directory"))?;
    let file = passing_through(dir, "backup");
    drop(std::fs::remove_file(&file));
    let written =
        tilo_identity::store::export_portable_backup(record, &file, passphrase.as_bytes())
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
    pub fn shell_link(&self) -> io::Result<tilo_socket::Stream> {
        use runtime_collab::wire::shell::SHELL_MAGIC;
        use std::io::Write as _;
        let secret = self
            .shell_links
            .make(tilo_workspace_service::apps::shell::ShellCaller {
                person: self.workspaces.device_cert().user_public_key,
            });
        let mut link = tilo_socket::connect(self.broker.app_socket())?;
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
    /// received); the shell is handed it at fd 4 as `WAYLAND_SOCKET`, the variable a Wayland client
    /// takes its connection from.
    ///
    /// # Errors
    /// No link could be made, or the shell did not start.
    #[cfg(target_os = "linux")]
    pub fn start_shell(
        &self,
        mut command: Command,
        window: Option<runtime_portal::process_link::Descriptor>,
    ) -> io::Result<Child> {
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

    /// The record content of this device's workspaces: where the bytes of their records' files
    /// are kept and asked for. `None` before the device's content store is open.
    pub(crate) fn record_content(
        &self,
    ) -> Option<Arc<tilo_workspace_service::content::RouteContent>> {
        self.workspaces.record_content_service()
    }

    /// Whether this device runs what waits for a device that keeps a person's root.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn serves_at_root(&self) -> bool {
        self.asked.serves_at_root()
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
    pub fn open_view(
        &self,
        caller: &Caller,
        app: &str,
        workspace: &str,
        record: Option<String>,
        reveal: Option<String>,
    ) -> Result<tilo_socket::Stream, String> {
        self.apps.open_view(caller, app, workspace, record, reveal)
    }

    /// What shows this device's workspaces' apps as web pages: what a page's session asks once
    /// its device proved itself (`/w/<workspace>/<app>`), each app laid out by its own window
    /// ([`Self::open_view`]). `None` on a device that shows none.
    #[must_use]
    pub fn pages(&self) -> Option<&dyn tilo_workspace_service::app_serving::AppPages> {
        self.workspaces.service().sessions().handler.pages()
    }

    /// What the device this computer is is read from, for what enters as it
    /// ([`super::home::Home::get`]).
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

    /// The tree the person reaches in `workspace`, one this device is a member of, and the
    /// caller they are there.
    ///
    /// # Errors
    /// The workspace cannot be admitted to now.
    pub(crate) fn person_tree_in(
        self: &Arc<Self>,
        workspace: &str,
    ) -> io::Result<(tilo_api::session::Provider, Caller)> {
        let caller = self.connection_caller(Some(workspace));
        self.tree_in(workspace, caller)
    }

    /// The tree `workspace` reaches for a program of this device that acts as its person without
    /// being a connection of theirs (a migration walking what their desktop kept), and the
    /// caller it is there. Such a program is no connection's own answer to what a workspace asks
    /// of the person: its calls ask.
    ///
    /// # Errors
    /// The workspace cannot be admitted to now.
    pub(crate) fn program_tree_in(
        self: &Arc<Self>,
        workspace: &str,
    ) -> io::Result<(tilo_api::session::Provider, Caller)> {
        let caller = Caller::local_person(&self.actor).in_workspace(workspace);
        self.tree_in(workspace, caller)
    }

    /// The caller a plain connection to this daemon's own socket is, in `workspace` (none: the
    /// account's own list): the person, where the kernel said the peer is this device's own
    /// account (the seats admit it before a connection reaches here) and this device is one of
    /// theirs that reaches every workspace.
    ///
    /// A device this person's root confined to one record (a workspace's VM, a hosted browser's
    /// environment) is that record's tenant, never the person's own connection: a same-account
    /// peer there is no more their own act than a program is, so its calls ask.
    fn connection_caller(&self, workspace: Option<&str>) -> Caller {
        let person = if self.cert.capabilities.workspace.is_none() {
            Caller::own_person(&self.actor)
        } else {
            Caller::local_person(&self.actor)
        };
        match workspace {
            Some(workspace) => person.in_workspace(workspace),
            None => person,
        }
    }

    /// The tree a plain connection from the person's own account reaches in `workspace` (none:
    /// the one this daemon serves), and the caller it is admitted as there: the person, whom the
    /// daemon admits from who is on the other end of its own socket, with no signing step of its
    /// own added.
    ///
    /// The person's own workspace is opened the first time a connection reaches it, as every
    /// other caller of the daemon opens it ([`crate::RuntimeServices::workspace`]): founded then
    /// on a computer that has none yet, so a command run right after the daemon starts is
    /// answered.
    ///
    /// A connection that names none reaches the account's own tree when this device cannot admit
    /// it anywhere yet: the person's list answers from their login entry and the copies held so
    /// far, so a device logged in by ticket lists its workspaces before it holds one.
    ///
    /// # Errors
    /// The person's workspace cannot be opened, or the workspace cannot be admitted to now.
    pub(crate) fn connection_tree_in(
        self: &Arc<Self>,
        workspace: Option<&str>,
    ) -> io::Result<(tilo_api::session::Provider, Caller)> {
        match workspace {
            Some(workspace) => self.admitted_connection_tree(workspace),
            None => self.person_connection_tree(),
        }
    }

    /// The tree a connection that names no workspace reaches: this daemon's own workspace as the
    /// person, or, while this device holds none of theirs yet (it logged in with a ticket and
    /// entered none), the person's own list, answered from what this device knows: their login
    /// entry and the copies held so far.
    fn person_connection_tree(
        self: &Arc<Self>,
    ) -> io::Result<(tilo_api::session::Provider, Caller)> {
        match self.admitted_connection_tree(&self.workspace) {
            Ok(tree) => Ok(tree),
            Err(why) => {
                tracing::debug!(
                    %why,
                    "your own workspace is not here yet: your account's list answers"
                );
                self.account_tree()
            }
        }
    }

    /// The tree a program of this device reaches in `workspace` (none: the person's own list),
    /// acting as the person but never as a plain connection's own act
    /// ([`Self::program_tree_in`]).
    ///
    /// # Errors
    /// The workspace cannot be admitted to now, or the account's list cannot be answered.
    pub(crate) fn program_tree(
        self: &Arc<Self>,
        workspace: Option<&str>,
    ) -> io::Result<(tilo_api::session::Provider, Caller)> {
        match workspace {
            Some(workspace) => self.program_tree_in(workspace),
            None => self.account_tree_for(Caller::local_person(&self.actor)),
        }
    }

    /// The tree `account` reaches alone: the person's own list, and nothing of a workspace. It is
    /// never admitted into one: a device that holds none yet still lists them.
    fn account_tree(self: &Arc<Self>) -> io::Result<(tilo_api::session::Provider, Caller)> {
        let caller = self.connection_caller(None);
        self.account_tree_for(caller)
    }

    /// [`Self::account_tree`] for `caller`, whose calls the list answers as.
    fn account_tree_for(
        self: &Arc<Self>,
        caller: Caller,
    ) -> io::Result<(tilo_api::session::Provider, Caller)> {
        let api = self.api()?;
        let tree =
            block_on_owned(async move { api.account_tree().await }).map_err(io::Error::other)?;
        Ok((tree, caller))
    }

    /// The tree a connection reaches in the workspace it names: the person, admitted from who is
    /// on the other end of this daemon's own socket.
    fn admitted_connection_tree(
        self: &Arc<Self>,
        workspace: &str,
    ) -> io::Result<(tilo_api::session::Provider, Caller)> {
        if workspace == self.workspace {
            crate::RuntimeServices::workspace(self.as_ref()).map_err(|why| {
                io::Error::other(format!(
                    "this computer could not open your workspace yet ({why}): `tilo status` \
                     says what its daemon is waiting for"
                ))
            })?;
        }
        self.person_tree_in(workspace)
    }

    /// The tree an app the daemon started reaches in `workspace`, and the caller it is there:
    /// the workspace's broker decides its calls with the app's grants, never the person's own
    /// rights.
    ///
    /// # Errors
    /// The action broker is not up, or the workspace cannot be admitted to now.
    pub(crate) fn app_tree(
        self: &Arc<Self>,
        app: &str,
        workspace: &str,
    ) -> io::Result<(tilo_api::session::Provider, Caller)> {
        let broker = self
            .broker
            .app_action_broker()
            .ok_or_else(|| io::Error::other("the action broker is unavailable"))?;
        let api = self.api()?;
        let named = workspace.to_owned();
        let joined = block_on_owned(async move { api.joined(&named, broker.app_calls()).await })
            .map_err(io::Error::other)?;
        Ok((joined, Caller::local_app(app).in_workspace(workspace)))
    }

    /// The tree `workspace` reaches for `caller`.
    fn tree_in(
        self: &Arc<Self>,
        workspace: &str,
        caller: Caller,
    ) -> io::Result<(tilo_api::session::Provider, Caller)> {
        let provider = self
            .person()?
            .provider_at(workspace, caller.clone(), (None, false))?;
        let api = self.api()?;
        let named = workspace.to_owned();
        let joined = block_on_owned(async move { api.joined(&named, Arc::new(provider)).await })
            .map_err(io::Error::other)?;
        Ok((joined, caller))
    }

    /// The app and workspace the daemon started process `pid` as, while it is that process.
    pub(crate) fn app_of(&self, pid: u32) -> Option<(String, String)> {
        self.apps.attested(pid)
    }

    /// The person's account at their server, asked as this device over the device action service
    /// ([`super::account`]): the server [`super::home::Home::get`] names. `None` while it names
    /// none; asked again then, so the account is there once the copy of the person's workspace
    /// arrives.
    pub(crate) async fn account(&self) -> Option<Arc<super::account::Account>> {
        let cached = self.account.lock_recover().as_ref().map(Arc::clone);
        if let Some(account) = cached {
            return Some(account);
        }
        let person = tilo_server_protocol::servers::person_workspace_id(&self.cert.user_public_key);
        let person_key = self.cert.user_public_key;
        let home = self.home.get().await?;
        let server = home.server_address.as_ref()?;
        let account = Arc::new(super::account::Account::at(
            &self.workspaces,
            &server.endpoint_addr,
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
                Ok(listed) => match listed
                    .workspaces
                    .first()
                    .map(tilo_server_protocol::workspaces::ListedWorkspace::entered_at)
                {
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
        let host = self.provider_for(workspace)?;
        let admission = host
            .admission()
            .ok_or_else(|| io::Error::other("this dispatch admitted no run to delegate"))?;
        let (host, caller) = host.agent_session_of(admission, &policy_time, |person| {
            (
                tilo_actions::remote::device_agent_id(person, name),
                name.to_owned(),
            )
        })?;
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
        founding: Option<Arc<super::unclaimed::Founding>>,
    ) -> io::Result<Self> {
        // The calls the device took before this person's services ran: they answer them too, so
        // a founding call followed after they start reads its state where it was kept.
        let inherited = founding
            .as_ref()
            .map(|founding| founding.calls())
            .unwrap_or_default();
        // A server keeps the workspaces placed on it: it takes no other server's work. A node
        // whose unit points at an install token waits for its page to install its certificate
        // before anything else is read: until then, this machine is nobody's device.
        let mut keeping = super::keep::Keeping::configured()?;
        if let Some(keeping) = keeping.as_ref() {
            keeping.enroll()?;
        }
        let identity = Identity::here(root)?;
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
                let lock = super::serve::lock_device(&place)?;
                let key = super::unclaimed::take_key(identity.place.keys(), &identity.key)?;
                Some((key, lock))
            }
        };
        tilo_files::native::private::directory(root)?;
        let sockets = crate::daemon::sockets(root)?;
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
        let apps = Arc::new(apps::Apps::from_env(root, &sockets));
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
        records.bind(&workspaces);
        records.carry_into(&identity.cert.user_public_key);
        // The files people choose for hosted apps are kept in the same records: one table for a
        // window and a hosted instance alike.
        #[cfg(target_os = "macos")]
        apps.keep_chosen_records(Arc::clone(&records));
        workspaces.hold_device_records(Arc::clone(&records));
        workspaces.copies().publish_own(
            tilo_actions::remote::DEVICE_SETTINGS,
            tilo_workspace_service::workspaces::OwnReaders::OwnRecords,
            records,
        );
        let home = Arc::new(super::home::Home::new(
            Arc::clone(&workspaces),
            identity.cert.clone(),
            entered,
        ));
        let take_work = member
            .then(|| take_work::Server::from_login(home.get_blocking().as_ref()))
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
                terminals.push((
                    ush::environment_terminals::ALPN,
                    Box::new(ush::environment_terminals::quic::Route::new(
                        move |endpoint, from, to| {
                            if let Err(error) = streams.serve_terminal(endpoint, from, to) {
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
        // The relay of the workspaces this device serves, on its endpoint: a server's keeping runs
        // its own, which this device's live session joins.
        let live = {
            let _entered = tilo_rt::app().handle().enter();
            super::live_relay::Live::new(
                &workspaces,
                kept.as_ref().and_then(super::keep::Kept::live_relay),
                Arc::clone(&fanout),
            )
        };
        stage("serving the device's endpoint");
        let endpoint_router = serve_device_endpoint(EndpointServing {
            workspaces: &workspaces,
            take_work,
            actions,
            host: (founded.is_some() || kept.is_some()).then(|| workspaces.service().as_ref()),
            kept: kept_protocols,
            terminals: terminals.into_iter().chain(live.protocols()).collect(),
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
        // The Wi-Fi passphrases the person's devices share are kept there too, sealed to each of
        // them: the desktop session shares and joins with them through the device's dispatch.
        super::wifi::Wifi::new(personal.clone(), identity.key.clone())
            .register(&workspaces.copies().providers());
        // Nearby Send between the person's devices: the offers and transfers answered over the
        // action service this daemon already serves, staged and kept on the device's content
        // shelf, and read by their shell as the resource it lists.
        let nearby = super::nearby::Nearby::new(
            &workspaces,
            personal.clone(),
            identity.place.dir().join("nearby"),
        );
        nearby.register(&workspaces.copies().providers());
        // The interfaces this computer wakes through are kept there too, and the wakes of the
        // person's devices asked there are answered from here: the packets for a device on one
        // of this computer's networks, and the word that this computer is back.
        super::wake::Wake::start(personal.clone(), identity.cert.clone());
        // The person's other devices drive this one's keyboard and pointer through its relay,
        // admitted by the record of their devices.
        live.follow_person(personal.clone());
        apps.device_grants().keep_in(std::sync::Arc::new(personal));
        // The person's devices, as the record of their own workspace holds them, each revocation
        // the root's own signature: what `tilo-identity` admits a device by, and the one copy of it.
        // Each start of the daemon reads them from its own workspaces.
        devices_trust().reading({
            let workspaces = std::sync::Arc::downgrade(&workspaces);
            move || {
                workspaces
                    .upgrade()
                    .map(|workspaces| super::signing::devices_seen(&workspaces))
            }
        });
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
            },
        ));
        // What the device's providers publish at fixed addresses is what an app's code calls them
        // by: each instance is handed their packages.
        apps.read_provided_from(workspaces.copies().providers());
        let identity_socket = sockets.identity.clone();
        stage("starting the identity service");
        let identity_broker = identity_service(
            identity_socket.clone(),
            Arc::clone(&workspaces) as Arc<dyn WorkspaceMembership>,
            Arc::new(workspaces.keys()),
            (&apps, &first),
            dev,
        )?;
        stage("opening the content store");
        let media = profile::MediaHost::start(&root.join("content"), &sockets.blobs)?;
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
        let shell_links = Arc::default();
        let shell_dispatch: Arc<super::shell::Dispatching> = Arc::default();
        let invocations: Arc<Invocations> = Arc::default();
        // No record of a call keeps what its action takes secret.
        invocations.keep_no_secrets(workspaces.copies().secret_parameters());
        // The person's shortcuts, saved into the workspaces this device holds and run here: the
        // same store the trigger runner arms them from, so a computer keeps its shortcuts with no
        // desktop session open on it.
        let shortcuts: Arc<dyn ShortcutLibraryServices> =
            Arc::new(super::shortcuts::Shortcuts::new(
                workspaces.copies().clone(),
                workspaces.device_cert().clone(),
                workspaces.signer(),
            )?);
        let initiative = serve_initiative(&workspaces, &shortcuts);
        let (policy_origin, broker) = start_broker(
            BrokerParts {
                shortcuts,
                root,
                sockets: &sockets,
                apps: &apps,
                workspaces: &workspaces,
                keys: &workspaces.keys(),
                creation: Arc::clone(&creation),
                files: Arc::clone(&files_daemons),
                key_sessions: key_sessions.as_ref(),
                nearby: Arc::clone(&nearby),
                shell_links: &shell_links,
                shell_dispatch: &shell_dispatch,
                identity: identity_broker.handover(),
                live: &live,
                fanout: &fanout,
            },
            brokers,
            Arc::clone(&fanout),
        )?;
        // A session reads content through this device's one store, and what it asks for that the
        // store lacks is asked of the workspace's devices by the record content the broker routes:
        // what one of them answers is kept here, where the session's next read finds it.
        if let Some(content) = workspaces.record_content_service() {
            media.fetch_content_through(Arc::new(
                move |scope: &runtime_fs::blobs::BlobScope, hash: &str| {
                    content.want(
                        &tilo_workspace_service::content::CrdtRoute::app(
                            scope.app(),
                            scope.workspace(),
                        ),
                        hash,
                    );
                },
            ));
        }
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
        // The services the daemon's program mounted (the device's image model among them),
        // which apps ask by the addresses they publish rather than through a connection of their
        // own: one copy each on this daemon's dispatch.
        super::capabilities::register(&super::capabilities::Mounting {
            providers: &workspaces.copies().providers(),
            blob_socket: &sockets.blobs,
            search: search
                .search()
                .map(|search| search as Arc<dyn tilo_actions::remote::ResourceSearch>),
            person: &actor,
        });
        workspaces.serve_invocations(Arc::clone(&invocations));
        stage("reading the calls that wait for the person");
        let asked = super::asked::DeviceAsked::new(
            asked_store(&workspaces)?,
            Arc::clone(&invocations),
            admits_again(&broker, &workspaces, &actor),
        );
        asked.withdraw_what_ended();
        // A person whose workspace is founded here after the start, or comes in from their
        // desktop, is held as the start holds one it finds: done by whatever first reaches it.
        first.when_here(super::host_runs::settling(
            &workspaces,
            &asked,
            &tickets,
            &identity.cert,
        ));
        // A read an app's join asks the person about waits here as its asked record.
        if let Some(broker) = broker.app_action_broker() {
            broker.state().serve_asked_reads(
                Arc::clone(&asked) as Arc<dyn tilo_actions::remote::asked::AskedReads>
            );
        }
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
        if let Some(key_sessions) = &key_sessions {
            serve_key_sessions(
                key_sessions,
                (&broker, &invocations, &apps),
                &workspaces,
                (&actor, founded, serves_pages),
                (&policy_origin, &first),
            )?;
        }
        serve_agent_steps(&workspaces, &fanout);
        // The person's shell tells the apps whose view it follows and moves their windows
        // through this daemon, whose fan-out holds the apps' connections.
        Arc::new(super::window_follow::WindowFollow::new(Arc::clone(&fanout)))
            .register(&workspaces.copies().providers());
        start_agents(&broker, &workspaces, &tickets, initiative);
        // The apps installed here that keep a long-lived connection: the trigger runner takes
        // each one's turn where this device has what it needs, and runs it while it holds it.
        #[cfg(unix)]
        workspaces.copies().run_connections(
            Arc::clone(&apps) as Arc<dyn tilo_workspace_service::work::HostConnections>
        );
        start_triggers(&broker, &workspaces, &invocations);
        // A device that serves the workspace's pages runs its apps with no session, so nothing
        // but this daemon would carry their notices to the person: it keeps them in the record
        // of their devices, where every device of theirs and every page raises them.
        #[cfg(not(windows))]
        if serves_pages {
            start_notices(&workspaces, &apps);
        }
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
        match super::packages::PackageCache::this_device(&identity.key) {
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
            &place
                .sockets()
                .join(tilo_device_identity::login::FILES_TREE_SOCKET),
            &workspaces,
            files_daemons,
            identity_socket.clone(),
            (actor.clone(), identity.key.verifying_key().to_bytes()),
        );
        // The workspaces' accounts and their sign-ins, for this login's cloud service and the
        // origin connectors this daemon starts.
        let account_sign_ins = super::capabilities::account_sign_ins(
            workspaces.copies(),
            Caller::authenticated(
                identity.cert.user_public_key,
                identity.cert.device_public_key,
            ),
        );
        // The origin connectors this device starts are handed their workspace's account sign-ins
        // by them.
        let account_sign_ins = account_sign_ins.map(|sign_ins| {
            apps.hand_sign_ins(sign_ins.hands);
            sign_ins.served
        });
        stage("publishing the styles the installed packages offer");
        #[cfg(not(windows))]
        let styles = super::styles::publish(&workspaces);
        // What a founding call that asked for the person's first tickets is answered them with.
        let founding_person = identity.cert.user_public_key;
        let founding_tickets = Arc::clone(&tickets);
        let daemon = Self {
            _files_tree: files_tree,
            #[cfg(not(windows))]
            _styles: styles,
            account_sign_ins,
            policy_origin,
            broker,
            shell_links,
            other_devices: other_devices(&workspaces),
            apps,
            workspaces,
            live: std::sync::OnceLock::new(),
            old_secrets: std::sync::atomic::AtomicBool::new(false),
            _identity: identity_broker,
            workspace: workspace_id,
            actor,
            identity_socket,
            blob_socket: sockets.blobs,
            replicas,
            _media: media,
            _endpoint_router: endpoint_router,
            home,
            first,
            _kept: kept,
            tickets,
            invocations,
            kept: inherited,
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
        let _ = daemon.live.set(Arc::clone(&live));
        // A person founded while this daemon serves has their root marked in their devices record
        // and their root-only asks served here from then on, without the person's services
        // starting again: the first person a fresh computer founds and every account after it.
        {
            let (asked, tickets, serving) = (
                Arc::downgrade(&daemon.asked),
                Arc::downgrade(&daemon.tickets),
                Arc::downgrade(&daemon.workspaces),
            );
            when_person_founded(Arc::new(move |person: [u8; 32], uid: u32| {
                let (Some(asked), Some(tickets), Some(serving)) =
                    (asked.upgrade(), tickets.upgrade(), serving.upgrade())
                else {
                    return;
                };
                tilo_rt::app().handle().spawn(async move {
                    let keys = tilo_api::calls::device_keys();
                    let cert = tilo_device_identity::persist::read_device_cert(
                        &keys.device_cert_for_user(&person),
                    )
                    .ok()
                    .filter(|cert| cert.user_public_key == person)
                    .or_else(|| {
                        tilo_device_identity::persist::read_device_cert(&keys.device_cert)
                            .ok()
                            .filter(|cert| cert.user_public_key == person)
                    });
                    let Some(cert) = cert else {
                        tracing::warn!(
                            person = %hex::encode(person),
                            "a founded person's device certificate is not here"
                        );
                        return;
                    };
                    // The root is the one the account `uid` just made, read by the account it
                    // was made for: nobody looks the person up while they are being founded.
                    let Ok(Some(root)) = root_for(uid) else {
                        tracing::warn!(
                            person = %hex::encode(person),
                            "a founded person's root is not kept here"
                        );
                        return;
                    };
                    if let Err(error) =
                        tilo_workspace_service::workspaces::person_workspace::devices::mark_root_holder(
                            serving.copies(),
                            &root,
                            &cert,
                        )
                        .await
                    {
                        tracing::warn!(
                            %error,
                            person = %hex::encode(person),
                            "a founded person's root is not marked in their devices"
                        );
                    }
                    // Their root-only asks are answered here from now on, as their root is.
                    asked.serve_at_root((*tickets).clone());
                });
            }));
        }
        // The person's shell reaches the one dispatch their own runs do, as them.
        match daemon.person() {
            Ok(person) => shell_dispatch.serve(person),
            Err(error) => tracing::warn!(%error, "this daemon's shell reaches no dispatch"),
        }
        // A founding call that asked for the person's first tickets is answered them here, from
        // now on: they are issued as the person's own act, on the device that founded them.
        if let Some(founding) = founding {
            founding.serving(founding_person, founding_tickets);
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
            | super::asked::Run::Own { workspace, .. }
            | super::asked::Run::Read { workspace, .. } => workspace,
        };
        let (admitting, named, actor) = (Arc::clone(&workspaces), workspace.clone(), actor.clone());
        let admission =
            block_on_owned(async move { admitting.admit_execution(&named, &actor).await })?;
        let super::asked::Run::Agent { agent, name, .. } = run else {
            // The person's own call, through their own dispatch here: only while it serves.
            admission
                .check(workspaces.copies())
                .map_err(|refusal| refusal.to_string())?;
            let provider = broker.certified_local_action_provider(
                admission.caller(),
                admission.presentation(),
                false,
            )?;
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
            .and_then(|keys| AgentSigner::grant(keys, workspace, (agent, name), person))?;
        admission
            .check(workspaces.copies())
            .map_err(|refusal| refusal.to_string())?;
        // Admitted again as a run of the person's admission delegated to this agent, as its
        // session first was ([`RuntimeActionProvider::agent_session`]).
        let delegated = broker.certified_agent_run(
            admission.caller(),
            admission.presentation(),
            delegated_agent(&agent)?,
        )?;
        let provider: Arc<dyn ActionProvider + Send + Sync> = Arc::new(AdmittedAgain {
            provider: Arc::new(Signed::new(delegated, Some(signer))),
            _admission: admission,
        });
        Ok((provider, agent))
    })
}

/// The agent `agent`, the caller a device's agent key acts as, is: the id the workspace's record
/// of its agents lists it by.
fn delegated_agent(agent: &Caller) -> Result<&str, String> {
    agent
        .acting()
        .agent_id()
        .ok_or_else(|| "the agent's key acts as no agent".to_owned())
}

/// A run admitted again for a call that waited across the daemon's restart, kept for as long as
/// its call is read.
struct AdmittedAgain {
    provider: Arc<dyn ActionProvider + Send + Sync>,
    _admission: Arc<tilo_workspace_service::workspaces::SessionAdmission>,
}

impl ActionProvider for AdmittedAgain {
    fn serves(&self, provider: &str) -> bool {
        self.provider.serves(provider)
    }

    fn published(&self) -> Vec<tilo_actions::ResourceSpec> {
        self.provider.published()
    }

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

    fn observed(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
        state: Option<&InvocationState>,
    ) {
        self.provider.observed(caller, id, state);
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

    fn invocation(
        &self,
        address: &tilo_actions::ActionAddress,
        argument: Option<&str>,
    ) -> Option<tilo_actions::api::ActionDescriptor> {
        self.provider.invocation(address, argument)
    }

    fn run(
        &self,
        caller: &Caller,
        request: InvokeRequest,
        admitted: tilo_actions::remote::Admitted,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        self.provider.run(caller, request, admitted)
    }

    fn asks_first(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        admitted: &tilo_actions::remote::Admitted,
    ) -> Option<Pending<Result<(), ActionCallError>>> {
        self.provider.asks_first(caller, request, admitted)
    }

    fn watch(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<tilo_actions::remote::LiveView, String> {
        self.provider.watch(caller, request)
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

/// `person`'s own workspace, at the id their key names.
fn person_workspace(person: &[u8; 32]) -> WorkspaceId {
    WorkspaceId::from(collab_types::person_workspace(person))
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
    /// The daemon's sockets.
    sockets: &'a crate::daemon::Sockets,
    apps: &'a Arc<apps::Apps>,
    workspaces: &'a Arc<ServiceWorkspaces>,
    keys: &'a tilo_workspace_service::apps::workspace_keys::KeyProvider,
    creation: Arc<RecordCreation>,
    /// The files daemons mounting this device's workspaces, whose stores keep their files' bytes.
    files: Arc<tilo_workspace_service::files::socket::Daemons>,
    /// Nearby Send's offers and transfers, which this device's shell reads and its person
    /// answers.
    nearby: Arc<super::nearby::Nearby>,
    /// The links of the shells this daemon starts.
    shell_links: &'a Arc<tilo_workspace_service::apps::shell::ShellLinks>,
    /// The person's key sessions, which serve the workspaces' apps as pages: a page's view's
    /// call is the browser's that shows it.
    key_sessions: Option<&'a Arc<crate::key_sessions::KeySessions>>,
    /// The dispatch those shells' calls reach, once the broker serves.
    shell_dispatch: &'a Arc<super::shell::Dispatching>,
    /// The person's shortcuts, which the workspaces' events are saved as.
    shortcuts: Arc<dyn ShortcutLibraryServices>,
    /// The identity service, serving the connections those shells take for it from the person's
    /// apps.
    identity: Arc<dyn Fn(tilo_socket::Stream) + Send + Sync>,
    /// The device's live session, which carries those shells' live links.
    live: &'a Arc<super::live_relay::Live>,
    /// The broker's fanout, which serves the broker the windows connect to: what the daemon
    /// publishes what those windows hold of.
    fanout: &'a Arc<FanoutState>,
}

/// What carries a shell's live links: `live`, for as long as each link lasts.
fn live_links(
    live: &Arc<super::live_relay::Live>,
) -> tilo_workspace_service::apps::shell::LiveLinks {
    let live = Arc::clone(live);
    Arc::new(move |stream| {
        let live = Arc::clone(&live);
        Box::pin(async move { live.serve_link(stream).await })
    })
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
        parts.sockets.collab.clone(),
        Some(Arc::clone(parts.workspaces) as Arc<dyn WorkspaceAuthority>),
        PeerPublisher::with_commits(ServiceCommits::new(parts.workspaces)
            as Arc<dyn tilo_workspace_service::apps::workspace_commits::WorkspaceCommits>),
        None,
        broker_services(parts, policy_time)?,
        channels,
        fanout,
    )?;
    Ok((policy_origin, broker))
}

/// What this device publishes to the apps attached to it, and to the person's shell: the seat
/// that serves the desktop session's services, and the two bundles of what this device keeps.
struct PublishedHere {
    seat: tilo_workspace_service::apps::seat::Seat,
    forwarded: tilo_workspace_service::apps::seat::Forwarders,
    /// What the person's shell reads and changes over its link: what the daemon keeps, never what
    /// the seat publishes.
    shell: Arc<dyn PublishedResources>,
    /// What every other app attached to this device reads.
    apps: Arc<dyn PublishedResources>,
}

/// The resources this device publishes: each served by the provider that keeps it, through one
/// table, and the seat's own services beside them.
fn published_here(parts: &BrokerParts<'_>) -> PublishedHere {
    // This device's server settings: the one copy its daemon keeps, read and changed by the apps
    // attached to it here and by a desktop session's apps over the shell link.
    let server_settings: Arc<dyn PublishedResources> = Arc::new(
        super::server_settings::Published::new(super::server_settings::Served::this_device()),
    );
    // What only the desktop session signed in here holds (the screen and its windows, the sound,
    // capture, the person's settings, Nearby Send's spool, the speaker groups),
    // reached through the seat its shell serves: while none does, those calls say so.
    let seat = tilo_workspace_service::apps::seat::Seat::new(
        parts.workspaces.copies().providers(),
        tilo_rt::app().handle().clone(),
    );
    let forwarded = seat.forwarders();
    // The session's roles that follow records hear of every record the workspaces took a change
    // of, as the store commits it.
    tilo_workspace_service::apps::seat::tell_record_changes(
        seat.clone(),
        parts.workspaces.copies().document_writes(),
    );
    // Everything this device publishes to the apps attached to it: each resource served by the
    // provider that keeps it, through one table; what the seat's services publish (the sound, the
    // network, the displays Quick Settings changes) is every other one.
    // The person's MCP server: every agent it let in and the sessions open now, read where
    // this device's workspace service keeps them. Its verbs stay the action API's, which the
    // daemon already answers.
    let mcp_server: Arc<dyn PublishedResources> =
        Arc::new(super::mcp_server::published(parts.workspaces));
    // The workspace's model providers: the providers a person set up, their models and what runs
    // where, read where this device's workspace service keeps them. A key never reads back,
    // and its verbs stay the action API's.
    let models: Arc<dyn PublishedResources> = Arc::new(super::models::published(parts.workspaces));
    // The workspace's connected accounts: each mail, calendar and cloud-storage account, read
    // where this device's workspace service keeps the accounts record. No secret reads back,
    // and connecting, signing in again and removing stay the action API's.
    let accounts: Arc<dyn PublishedResources> =
        Arc::new(super::accounts::published(parts.workspaces));
    // Nearby Send's offers and transfers, read by the person's shell over the shell link.
    let nearby: Arc<dyn PublishedResources> = Arc::new(parts.nearby.published());
    // What is live in the workspaces this device keeps, which the desktop shows.
    let live: Arc<dyn PublishedResources> = super::live::Published::of(parts.workspaces.copies());
    // The record each window this device runs holds, which its desktop notes beside the window.
    let window_notes: Arc<dyn PublishedResources> =
        super::window_notes::Published::of(parts.fanout);
    // What a person copies from the vault is put on their session's clipboard from here, so no
    // app ever holds it.
    parts
        .workspaces
        .copies()
        .copy_secrets_to(Arc::new(super::secret_clipboard::SessionClipboard));
    // The code of this moment a vault item's seed makes, which an app that shows one-time codes
    // follows as the item's reading: read here, where the seed is.
    let vault_codes: Arc<dyn PublishedResources> =
        Arc::new(tilo_workspace_service::vault_actions::VaultPublished::new(
            parts.workspaces.copies().clone(),
            tilo_rt::app().handle().clone(),
        ));
    // What this device's daemon keeps itself, each resource served by the provider that keeps it.
    let kept_here = || {
        super::published::Published::new()
            .serving(
                &[super::server_settings::SERVER],
                Arc::clone(&server_settings),
            )
            .serving(&[super::nearby::RESOURCE], Arc::clone(&nearby))
            .serving(&[tilo_workspace_service::live::RESOURCE], Arc::clone(&live))
            .serving(
                &[tilo_workspace_service::window_notes::RESOURCE],
                Arc::clone(&window_notes),
            )
            .serving(&[super::mcp_server::SERVER], Arc::clone(&mcp_server))
            .serving(
                &[tilo_actions::remote::model_providers::RESOURCE],
                Arc::clone(&models),
            )
            .serving_prefix("vault.item:", Arc::clone(&vault_codes))
            .serving(
                &[tilo_actions::remote::person_accounts::RESOURCE],
                Arc::clone(&accounts),
            )
    };
    // What the person's shell reads and changes over its link: what the daemon keeps, never what
    // the seat publishes. The seat is the shell's own, answered on its loop; a read the shell's
    // loop waits for that asked the seat first would wait on that same loop, and the session
    // would never start.
    let shell: Arc<dyn PublishedResources> = Arc::new(kept_here());
    let apps: Arc<dyn PublishedResources> = Arc::new(kept_here().serving_the_rest(Arc::new(
        tilo_workspace_service::apps::seat::SeatPublished::new(seat.clone()),
    )));
    PublishedHere {
        seat,
        forwarded,
        shell,
        apps,
    }
}

/// The checks a project's host approved: this device's work, kept as its jobs at
/// `devices/<device>/jobs` and read through the one record API. What runs comes from this
/// device's own `checks` setting, so a check an app names never brings its own commands. A
/// check's commands are ush's own, which a phone's app carries none of. An app runs one as a
/// step its own `run-approved-check` declares, for an invocation the dispatch delivered to it:
/// that invocation names the project the check is for.
#[cfg(all(unix, not(target_os = "android")))]
fn approved_checks(parts: &BrokerParts<'_>) -> io::Result<()> {
    let jobs = Arc::new(super::approved_checks::CheckJobs::at(
        parts.root,
        parts.workspaces.copies().host_device(),
    ));
    parts.workspaces.copies().publish_own(
        super::approved_checks::JOBS,
        tilo_workspace_service::workspaces::OwnReaders::OwnRecords,
        Arc::clone(&jobs) as Arc<dyn tilo_actions::remote::ResourceReads>,
    );
    parts.workspaces.copies().providers().provide(
        runtime_collab::approved_checks::PROVIDER,
        tilo_app_id::apps::SETTINGS,
        tilo_workspace_service::workspaces::providers::Reach::ThisDevice,
        Arc::new(super::approved_checks::NativeChecks::new(
            parts.workspaces,
            parts.workspaces.device_records().cloned().ok_or_else(|| {
                io::Error::other("the device's settings are not kept for its checks")
            })?,
            parts.root.to_path_buf(),
            jobs,
        )),
    );
    Ok(())
}

/// Elsewhere, a host approves no checks.
#[cfg(not(all(unix, not(target_os = "android"))))]
fn approved_checks(_parts: &BrokerParts<'_>) -> io::Result<()> {
    Ok(())
}

/// The seat's services, as the broker runs them: what the desktop session serves, the app windows
/// it opens, and the agents it admitted.
fn seat_services(
    services: &mut ProcessServices,
    parts: &BrokerParts<'_>,
    seat: tilo_workspace_service::apps::seat::Seat,
    forwarded: tilo_workspace_service::apps::seat::Forwarders,
) {
    services.audio_output = Some(forwarded.audio_output);
    // A page's view's call is the browser's that shows it; the seat carries every other.
    services.native_media = Some(match parts.key_sessions {
        Some(sessions) => Arc::new(super::page_media::PageMedia {
            pages: sessions.pages_served().clone(),
            seat: forwarded.native_media,
        })
            as Arc<dyn tilo_workspace_service::apps::collab_app_broker::NativeMediaServices>,
        None => forwarded.native_media,
    });
    services.continuations = Some(forwarded.continuations);
    services.workspace_nodes = Some(forwarded.workspace_nodes);
    // The device's keyboard shortcuts, the person's account credentials and Files'
    // clipboard are the desktop session's.
    services.keyboard_shortcuts = Some(forwarded.keyboard_shortcuts);
    services.browser_service_sessions = Some(forwarded.browser_service_sessions);
    services.file_clipboard = Some(forwarded.file_clipboard);
    // An app's window opens in the person's desktop session while one serves the seat; the
    // instances that show nothing, and the processes it starts, are this daemon's.
    services.launcher = Some(Arc::new(
        tilo_workspace_service::apps::seat::SeatLauncher::new(
            seat,
            Arc::clone(parts.apps) as Arc<dyn LauncherServices>,
        ),
    ));
    // The agents each workspace's own record names, read here, so an agent is admitted with
    // or without a desktop session; what each spent, and the agent sessions the person
    // admitted, are the desktop session's, which the daemon hands each agent's connection.
    services.agents = Some(Arc::new(super::workspaces::RecordedAgents {
        workspaces: Arc::downgrade(parts.workspaces),
        session: Arc::clone(&forwarded.agents),
    }));
    services.agent_sessions =
        Some(forwarded.agents as Arc<dyn tilo_broker_core::services::AgentSessions>);
}

/// The records the broker reads for the apps it serves: the agents and sessions, the documents
/// they work on, and the workspaces' people.
fn record_services(
    services: &mut ProcessServices,
    parts: &BrokerParts<'_>,
    policy_time: tilo_workspace_service::apps::policy_clock::PolicyTime,
) {
    // The records of the workspaces this device keeps, read for the apps, agents and programs
    // it serves: each capture opens the record's document here, and counts it.
    services.committed_documents = Some(Arc::new(
        tilo_workspace_service::apps::service_reads::ServiceReads::new(
            parts.workspaces.copies().clone(),
            parts.workspaces.service().metrics_counter(),
        ),
    )
        as Arc<dyn tilo_workspace_service::apps::services::NativeDocumentReads>);
    // The device's one store, which the apps it serves write and read their resources in.
    services.resources = Some(Arc::new(
        tilo_workspace_service::apps::service_resources::ServiceResources::of(
            parts.workspaces.service(),
        ),
    ));
    services.native_certificates = Some(Arc::new(Certificates {
        policy_time,
        workspaces: Arc::downgrade(parts.workspaces),
    }));
    services.launchable_catalogue = Some(parts.root.join("state/launchable-actions.json"));
    services.app_action_access = Some(parts.apps.access_resolver());
    services.app_resource_kinds = Some(parts.apps.resource_kinds_resolver());
    services.record_founding = Some({
        let creation = Arc::clone(&parts.creation);
        Arc::new(move |app: &str, kind: &str| creation.allows(app, kind))
    });
    services.workspace_grants = Some(Arc::new(MemberListGrants {
        workspaces: Arc::clone(parts.workspaces),
    }));
    // Who is in each workspace this host holds, for the apps it serves: their names and the
    // picture each person set in the profile kept here.
    services.workspace_memberships = Some(
        tilo_workspace_service::workspaces::ProfileMemberships::new(
            parts.workspaces.copies().clone(),
            Arc::clone(parts.workspaces)
                as tilo_workspace_service::apps::services::WorkspaceAuthorityHandle,
        ) as Arc<dyn tilo_workspace_service::apps::services::WorkspaceMemberships>,
    );
    services.journal_root = Some(parts.root.join(INVOCATION_JOURNAL));
    // What a caller puts into a workspace here is kept on the shelf its records' content is.
    services.content_store = parts
        .workspaces
        .content_shelf()
        .map(tilo_workspace_service::content::shelf_puts);
    // Who is present on which device, as this device's workspaces hear it: their links, and
    // the agent sessions this daemon serves.
    services.present_devices = Some(Arc::new(parts.workspaces.copies().clone())
        as Arc<dyn tilo_workspace_service::workspaces::PresentDevices>);
}

/// The broker's shell, the workspaces placed on another host, and what the daemon publishes of
/// what it keeps: the services the person's desktop and the other devices reach.
fn shell_services(
    services: &mut ProcessServices,
    parts: BrokerParts<'_>,
    published: &Arc<dyn PublishedResources>,
    shell_published: Arc<dyn PublishedResources>,
    hosts: &Arc<super::hosts::Hosts>,
) {
    services.shortcut_library = Some(parts.shortcuts);
    // This device's server settings, as the apps attached to this daemon read and change them:
    // the work its machine takes and the services it offers, saved to the configuration the
    // daemon keeps; and what the seat's services publish.
    services.published = Some(Arc::clone(published));
    // The service the identity service answers key requests from:
    // endpoints ask again when it says a channel changed.
    services.group_keys = Some(parts.keys.clone());
    // An app's action on another of the person's own devices: carried there over the action
    // service this device already serves, and followed until that device answers.
    services.own_device = Some(
        Arc::new(super::own_device::AnotherDevice::over(parts.workspaces))
            as Arc<dyn tilo_broker_core::services::OwnDeviceActions>,
    );
    // The person's desktop session: their workspaces' documents, governance and standing,
    // reached through this device alone.
    services.shell = Some(Arc::new(super::shell::services(
        parts.workspaces,
        parts.shell_links,
        parts.sockets.collab.clone(),
        Arc::clone(parts.shell_dispatch),
        shell_published,
        shell_processes(parts.apps, parts.workspaces),
        super::shell::Serving {
            identity: Arc::clone(&parts.identity),
            live: Some(live_links(parts.live)),
            speaking: Arc::clone(parts.live)
                as Arc<dyn tilo_workspace_service::apps::shell::ShellSpeaking>,
        },
    )));
    // A workspace placed on a host has its records changed there: the person's actions on
    // them are sent to the host, which decides and runs them, and the change reaches this
    // device's copy by sync.
    services.hosted_workspaces = Some(Arc::clone(hosts) as Arc<dyn HostedWorkspaces>);
    // The providers registered on this device ([`Daemon::provide`]): a call made here reaches
    // them through the same registry another device's call does.
    services.providers = Some(parts.workspaces.copies().providers());
    // What an app publishes of what it holds, on its connection here: the device's one
    // search index keeps it after the app closes.
    services.search_index = parts.workspaces.search_index_services();
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
        &parts.sockets.blobs,
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
    // Where an offloadable invocation of the session's broker runs: the session asks this
    // daemon to place it over the shell link, as this daemon's own broker would.
    parts.workspaces.copies().providers().provide(
        tilo_actions::remote::offload_place::PROVIDER,
        tilo_actions::remote::offload_place::PROVIDER,
        tilo_workspace_service::workspaces::providers::Reach::ThisDevice,
        Arc::new(super::offload_place::Placement::new(action_offload.clone())),
    );
    // The turns this device takes for its workspaces' devices are the actions
    // themselves, dispatched through this device's own dispatch; this is what it
    // says of the content it keeps for them. Whether a turn is this device's is
    // decided live, each time it is asked.
    parts
        .workspaces
        .copies()
        .run_jobs(Arc::new(super::dispatch::Kept::new(Arc::clone(
            &document_content,
        ))));
    // The hosts of the workspaces placed on another device: the one dispatch sends a call of a
    // typed action of one of their records there, as the broker sends its apps' invocations.
    let hosts = super::hosts::Hosts::of(parts.workspaces);
    parts.workspaces.hold_hosts(Arc::clone(&hosts));
    let placed = Arc::clone(&hosts);
    parts
        .workspaces
        .copies()
        .providers()
        .place_on_hosts(Arc::new(move |workspace: &str| {
            placed.host_actions(workspace)
        }));
    let PublishedHere {
        seat,
        forwarded,
        shell: shell_published,
        apps: published,
    } = published_here(&parts);
    approved_checks(&parts)?;
    let mut services = ProcessServices {
        action_offload,
        document_content: Some(document_content),
        ..ProcessServices::default()
    };
    seat_services(&mut services, &parts, seat, forwarded);
    record_services(&mut services, &parts, policy_time);
    shell_services(&mut services, parts, &published, shell_published, &hosts);
    Ok(services)
}

/// The device's tilolang processes, as the person's shell hands them its windows' instances on
/// its link: for the workspaces this device has for them alone. `None` where no process can start.
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
        // What each window's process serves as `workspace.content`: the limits this device's
        // person reads and the refusals they read, from the workspace service the daemon holds.
        let content: Arc<dyn super::processes::ContentOf> =
            Arc::new(DeviceContent(workspaces.copies().clone()));
        processes.read_content_from(content);
        follow_content_changes(&processes, workspaces.copies());
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

/// What a workspace serves as its content root to this device's person: its owners' limits on
/// what members post there and the refusals the person reads, read from the workspace service.
#[cfg(target_os = "linux")]
struct DeviceContent(tilo_workspace_service::workspaces::Workspaces);

#[cfg(target_os = "linux")]
impl super::processes::ContentOf for DeviceContent {
    fn content(&self, workspace: &str) -> Option<String> {
        let workspaces = self.0.clone();
        let workspace = workspace.to_owned();
        let reader = workspaces.device_person();
        super::workspaces::block_on_owned(async move {
            let reading = workspaces.content_reading(&workspace, reader).await;
            serde_json::to_string(&reading).ok()
        })
    }
}

/// Tell each process running a workspace's windows when its content moves: a refusal a host
/// recorded, a refusal it withdrew once the limit was raised, or a limit its owners set. The
/// workspace service tells of the records written and of its governance moving, so the windows
/// show the refusal or its going without a poll on a clock.
#[cfg(target_os = "linux")]
fn follow_content_changes(
    processes: &Arc<super::processes::Processes>,
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
) {
    let mut writes = workspaces.document_writes();
    let mut governance = workspaces.governance_changes();
    let processes = Arc::downgrade(processes);
    tilo_rt::app().handle().spawn(async move {
        loop {
            let workspace = tokio::select! {
                write = writes.recv() => match write {
                    Ok(write) => {
                        // A record of content a host refused is written or withdrawn: the root
                        // the windows read carries it from now.
                        if write.app != collab_types::CONTENT_REFUSAL_APP {
                            continue;
                        }
                        write.workspace
                    }
                    // Writes were missed: every workspace is read again by `content_refresh_all`.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        if let Some(processes) = processes.upgrade() {
                            let _ = tokio::task::spawn_blocking(move || {
                                processes.content_refresh_all();
                            })
                            .await;
                        }
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
                change = governance.recv() => match change {
                    Ok(workspace) => workspace,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        if let Some(processes) = processes.upgrade() {
                            let _ = tokio::task::spawn_blocking(move || {
                                processes.content_refresh_all();
                            })
                            .await;
                        }
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
            };
            let Some(host) = processes.upgrade() else {
                return;
            };
            // The reading is read from the workspace's store, which waits: never on this thread.
            let _ = tokio::task::spawn_blocking(move || host.content_changed(&workspace)).await;
        }
    });
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
        founding: Arc<super::unclaimed::Founding>,
    ) -> io::Result<Self> {
        let serving = if dev { Serving::Dev } else { Serving::Profile };
        Self::start(
            root,
            stage,
            serving,
            (Some(device), entered),
            Some(founding),
        )
    }
}

impl RuntimeServices for Daemon {
    type ActionProvider = RuntimeActionProvider;

    fn start_reporting(root: &Path, stage: &mut dyn FnMut(&str)) -> io::Result<Self> {
        Self::start(root, stage, Serving::Profile, (None, None), None)
    }

    fn start_in_dev_mode(root: &Path, stage: &mut dyn FnMut(&str)) -> io::Result<Self> {
        Self::start(root, stage, Serving::Dev, (None, None), None)
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
        let host = self.action_provider()?;
        Ok(host.session_factory(
            workspace,
            self.actor.clone(),
            self.policy_origin.capture().map_err(io::Error::other)?,
        ))
    }

    #[cfg(unix)]
    fn launch(&self, app_id: &str, command: Command) -> io::Result<Child> {
        // The app's connections are opened here, before it runs, and name the person's workspace:
        // the first app launched in it founds it then, so the device is listed in it before the
        // app signs anything. An import that came in first is theirs instead.
        self.first
            .found_if_theirs(&self.workspace)
            .map_err(io::Error::other)?;
        // The workspace is here now: the secrets the apps the move carried over kept in this
        // device's old keeper are the person's vault items from now on.
        self.adopt_kept_secrets();
        // The app's collaboration connection, opened here, names this launch, which the
        // collaboration service attests it as.
        self.apps.launching(app_id, &self.workspace);
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
        // The process runs the app in the person's workspace, as the daemon launched it: what
        // its connections are recognised by, by its exact launch record.
        self.apps
            .record_started(child.id(), app_id, &self.workspace);
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

    /// A call of one of the person's actions in `workspace`, made as the person on this device:
    /// what their own runs reach of a provider the workspace's host answers (`tilo.telling`), or
    /// an action of the workspace itself. It is the one dispatch every call of theirs takes, so
    /// what the host decides for them decides this too.
    ///
    /// # Errors
    /// The workspace is not one this device may act in, or its admission cannot be opened.
    pub(crate) fn person_action(
        &self,
        workspace: &str,
        provider_name: &str,
        resource: &str,
        action: &str,
        arguments: serde_json::Map<String, Value>,
        wait_ms: u32,
    ) -> io::Result<Pending<Result<InvocationReceipt, ActionCallError>>> {
        let dispatch = self.person()?;
        let caller = dispatch.caller(workspace);
        let provider = dispatch.provider(workspace)?;
        let request = InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget {
                provider: provider_name.to_owned(),
                address: tilo_actions::ActionAddress::new(resource, action),
            },
            argument: InvokeArgument::Json(arguments),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: Some(wait_ms),
            appearance: None,
            signed: None,
        };
        Ok(provider.invoke(&caller, request))
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
            kept: Arc::clone(&self.kept),
            asked: Arc::clone(&self.asked),
            first: Arc::clone(&self.first),
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
    /// The calls the device took before this person's services ran, which they answer too.
    kept: super::unclaimed::Calls,
    asked: Arc<super::asked::DeviceAsked>,
    /// The person's own workspace here, founded the first time a call of theirs needs it.
    first: Arc<super::first_workspace::FirstWorkspace>,
}

impl PersonDispatch {
    /// Who the person's calls in `workspace` are made as.
    pub(super) fn caller(&self, workspace: &str) -> Caller {
        Caller::local_person(&self.actor).in_workspace(workspace)
    }

    /// The dispatch the person's calls in `workspace` reach.
    pub(super) fn provider(&self, workspace: &str) -> io::Result<RuntimeActionProvider> {
        self.provider_at(workspace, self.caller(workspace), (None, false))
    }

    /// The dispatch the person's own shell's calls in `workspace` reach, on the link the daemon
    /// made for it alone: where the call was pressed at `pressed` on their shell's screen, the
    /// question it may wait in stands over that window. The shell is the system's view, so its
    /// own answer to what waits for the person is their confirmation.
    pub(super) fn provider_pressed(
        &self,
        workspace: &str,
        pressed: Option<runtime_collab::wire::shell::ShellPress>,
    ) -> io::Result<RuntimeActionProvider> {
        self.provider_at(workspace, self.caller(workspace), (pressed, true))
    }

    /// The dispatch the calls of `caller` in `workspace` reach: every action of one answered
    /// locally, and no other caller let in. `own_shell` where it serves the person's own shell
    /// alone ([`AdmittedRuntimeSession::own_shell`]).
    fn provider_at(
        &self,
        workspace: &str,
        caller: Caller,
        (pressed, own_shell): (Option<runtime_collab::wire::shell::ShellPress>, bool),
    ) -> io::Result<RuntimeActionProvider> {
        // A new person's first call in their own workspace founds it, when nothing came in for
        // them from their desktop.
        self.first
            .found_if_theirs(workspace)
            .map_err(io::Error::other)?;
        let workspaces = Arc::clone(&self.workspaces);
        let actor = self.actor.clone();
        let named = workspace.to_owned();
        let admission =
            block_on_owned(async move { workspaces.admit_execution(&named, &actor).await })
                .map_err(io::Error::other)?;
        let provider = self
            .broker
            .certified_local_action_provider(
                admission.caller(),
                admission.presentation(),
                caller.is_own_connection(),
            )
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
                projection: Some(caller),
                pressed,
                own_shell,
            })),
            tickets: Some(Arc::clone(&self.tickets)),
            invocations: Arc::clone(&self.invocations),
            kept: Arc::clone(&self.kept),
            handed: None,
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
            let (provider, caller) = daemon.key_tree(&key, &cert, None)?;
            tilo_mcp::McpServer::admitted(provider, caller)
        })
    }

    /// A run of `key` (certified by `cert`) in `workspace` (none: the device's), and the caller
    /// its calls go as: the key itself ([`Self::provider_for_key`]).
    ///
    /// # Errors
    /// The certificate does not hold, or the workspace does not admit the key.
    pub(crate) fn key_tree(
        &self,
        key: &SigningKey,
        cert: &DeviceCert,
        workspace: Option<&str>,
    ) -> io::Result<(RuntimeActionProvider, Caller)> {
        let key = tilo_api::grant_key::GrantKey::held(
            key.clone(),
            cert.clone(),
            tilo_clock::unix_now_ns(),
        )
        .map_err(|refused| io::Error::other(refused.to_string()))?;
        let workspace = match workspace {
            Some(workspace) => workspace.to_owned(),
            None => self.workspace()?,
        };
        let provider = self.provider_for_key(&workspace, &key)?;
        let caller = provider
            .admission_caller()
            .ok_or_else(|| io::Error::other("the key's run was not admitted"))?;
        Ok((provider, caller))
    }

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
            .certified_local_action_provider(admission.caller(), admission.presentation(), false)
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
                pressed: None,
                own_shell: false,
            })),
            // A key reads what the device is, as the person's agents do; only the person lets
            // another device in ([`RuntimeActionProvider::is_the_person`]).
            tickets: Some(Arc::clone(&self.tickets)),
            invocations: Arc::clone(&self.invocations),
            kept: Arc::default(),
            handed: None,
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

    fn account(&self) -> tilo_api::session::FoundAccount<'_> {
        Box::pin(async move {
            let daemon = self.daemon().ok()?;
            tilo_api::session::Here::account(&*daemon).await
        })
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

    /// The account this device serves for its person, when it keeps their workspaces itself: the
    /// daemon that founded their workspace answers their list, never a read of the copy a
    /// restarted daemon is still taking in.
    fn account_here(&self) -> Option<Arc<dyn tilo_api::session::AccountHere>> {
        let daemon: Arc<Daemon> = self.daemon().ok()?;
        daemon
            .keeps_its_own_account()
            .then_some(daemon as Arc<dyn tilo_api::session::AccountHere>)
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

    fn account(&self) -> tilo_api::session::FoundAccount<'_> {
        Box::pin(async move {
            Daemon::account(self)
                .await
                .map(|account| account as Arc<dyn tilo_api::session::AccountServer>)
        })
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
            let account = Daemon::account(self).await.ok_or(NO_ACCOUNT_YET)?;
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
}

impl Daemon {
    /// Whether this computer keeps its person's account itself: a computer that logged in
    /// reaches the account through the server that keeps its workspaces, and one that founded
    /// its person here keeps them itself, and answers for them.
    pub(crate) fn keeps_its_own_account(&self) -> bool {
        self.first.founds()
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
                &summary.workspace,
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
                    Err(ServerError::NotFound | ServerError::Refused(_)) => {}
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
                    Err(ServerError::NotFound | ServerError::Refused(_)) => {}
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

/// Enter `workspace` as this device's, as `device`, signing its own row there with this device's
/// key: a workspace the device made for its person is one it keeps, so its views and calls reach
/// it at once.
async fn enter_here(
    workspaces: &ServiceWorkspaces,
    workspace: &str,
    device: &DeviceCert,
) -> Result<(), ServerError> {
    let signer = workspaces.signer();
    workspaces
        .copies()
        .enter(workspace, (device, &*signer))
        .await
        .map(drop)
        .map_err(unavailable)
}

/// A workspace the account created, as its caller reads it back.
fn listed_workspace(
    listing: WorkspaceListing,
    workspaces: &ServiceWorkspaces,
) -> tilo_api::session::Workspace {
    let mut workspace = described_workspace(&listing.workspace, listing.name, workspaces);
    workspace.labels = listing.labels;
    workspace.unattended = listing.unattended;
    workspace
}

/// The workspace `id` called `name`, with what this device keeps of it.
fn described_workspace(
    id: &str,
    name: String,
    workspaces: &ServiceWorkspaces,
) -> tilo_api::session::Workspace {
    let mut workspace = tilo_api::session::held(id.to_owned(), name);
    workspace.labels = workspaces.labels_of(id);
    workspace.unattended = workspaces.runs_unattended(id);
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
    /// The calls the device took before its person's services ran, which they answer too: a
    /// founding call followed after they start reads its state here.
    kept: super::unclaimed::Calls,
    /// Where an agent's call refused for want of its person's yes waits for them, and where
    /// their answer is admitted: `None` for a host that keeps none, whose such calls are refused.
    asked: Option<Arc<super::asked::DeviceAsked>>,
    /// Where a descriptor an answer to the connection this run serves carries waits to be written
    /// with it ([`crate::seats::Handing`]): `None` for a run no native connection serves.
    handed: Option<crate::seats::Hand>,
}

struct AdmittedRuntimeSession {
    policy_time: tilo_workspace_service::apps::policy_clock::PolicyTime,
    provider: Arc<dyn ActionProvider + Send + Sync>,
    admission: Arc<tilo_workspace_service::workspaces::SessionAdmission>,
    /// The agent this run is, under a key of its own: who its calls go as.
    agent: Option<Caller>,
    projection: Option<Caller>,
    /// Where on the shell's screen the person pressed the call, when this dispatch serves their
    /// press: the device and the app are filled in where the call is kept.
    pressed: Option<runtime_collab::wire::shell::ShellPress>,
    /// Whether this run serves the person's own shell on the link the daemon made for it alone:
    /// the system's view, whose answer to a question that waits for the person is their
    /// confirmation. An app or agent the shell names is no caller this run admits.
    own_shell: bool,
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
            // A run with no projection admits the device's own admission, and the agent the run
            // was made for: the connection of an unnamed agent presents the agent itself.
            return caller == self.admission.caller()
                || self.agent.as_ref().is_some_and(|agent| caller == agent);
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
    // What only the calling person's root signs runs here when this computer keeps their root
    // and they are signed in; otherwise it waits in its record for a device of theirs that does.
    let person = caller
        .acting()
        .person_id()
        .and_then(|person| hex::decode(person).ok())
        .and_then(|person| <[u8; 32]>::try_from(person).ok());
    if caller.is_system() || person.is_some_and(holds_root_for_person) {
        return run_device_action(invocations, tickets, caller, request, workspace);
    }
    let (done, answer) = tilo_actions::remote::pending();
    let (asked, caller, workspace) = (asked.cloned(), caller.clone(), workspace.to_owned());
    let invocation = request.invocation;
    tickets.root_only_call(workspace.clone(), invocation, move |_marked, kept| {
        let answered = Pending::ready(hand_on_to_root(
            asked.as_deref(),
            &caller,
            &request,
            (workspace, kept),
        ));
        answered.then(move |answered| {
            done.complete(answered.unwrap_or_else(|| {
                Err(ActionCallError::Unavailable {
                    reason: "the device action ended unanswered".into(),
                })
            }));
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
    // An agent asking for the person's root answers as the agent it is, so the card says who
    // wants it; the person's own call is theirs.
    let run = match (caller.acting().agent_id(), caller.agent_called()) {
        (Some(agent), Some(name)) => super::asked::Run::Agent {
            workspace,
            agent: agent.to_owned(),
            name: name.to_owned(),
        },
        _ => super::asked::Run::Own {
            workspace,
            pressed: None,
        },
    };
    asked.hand_on(&run, caller, request, (facts, kept))
}

impl RuntimeActionProvider {
    /// The run this dispatch was admitted as: what a session it starts here goes on as, rather
    /// than a second admission of the same person in the same workspace.
    pub(super) fn admission(
        &self,
    ) -> Option<Arc<tilo_workspace_service::workspaces::SessionAdmission>> {
        self.session
            .as_ref()
            .map(|session| Arc::clone(&session.admission))
    }

    /// The caller the run was admitted as, where it has a run: the caller the API session keeps
    /// rather than minting another.
    fn admission_caller(&self) -> Option<Caller> {
        self.session
            .as_ref()
            .map(|session| session.admission.caller().clone())
    }

    /// Whether `caller` acts in the run of the person's own plain client on this computer: what
    /// a connection from their account to the daemon's own socket is admitted as
    /// ([`Self::session_factory`]), never a run of an agent elsewhere, of a roster turn, or of
    /// another person's key.
    fn is_own_client(&self, caller: &Caller) -> bool {
        let own = device_agent_id(&self.workspaces.operator(), MCP_CLIENT);
        self.session.as_ref().is_some_and(|session| {
            session.admits(caller)
                && session
                    .agent
                    .as_ref()
                    .is_some_and(|agent| agent.acting().agent_id() == Some(own.as_str()))
        })
    }

    /// Whether `caller` is the person's own run, not an agent's nor another member's: only the
    /// device's person adds a device.
    fn is_the_person(&self, caller: &Caller) -> bool {
        let person = hex::encode(self.workspaces.device_cert().user_public_key);
        self.session.as_ref().is_some_and(|session| {
            session.agent.is_none()
                && session.admission.caller().acting().person_id() == Some(person.as_str())
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
        // Every workspace the agent may work in: a stop is the person's own say about their own
        // agent, which reaches only the agent acting for them, so it is written in each workspace
        // this device holds; going on takes it back where it stands. The answer syncs with the
        // workspace, so a host running the agent elsewhere refuses its next call.
        let operator_person = hex::encode(operator);
        let now = tilo_action_policy::Context::at_time(tilo_clock::wall());
        let targets: Vec<String> = self
            .workspaces
            .workspaces()
            .into_iter()
            .filter(|held| {
                stopping
                    || self.workspaces.policy_of(held.id())()
                        .is_some_and(|policy| policy.stops(&agent, &operator_person, &now))
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

    /// The workspace's own tree (`workspace/apps`, `workspace/vault`, `workspace/members`) as
    /// this device answers it for `caller`, its person: the provider this device registered for
    /// the workspaces it keeps, asked as this device, whose certificate names them. That is the
    /// caller a member's other device is there, and what the tree offers is decided by their
    /// grants. `None` for anyone else, an agent included, or where this device keeps no
    /// workspace.
    fn workspace_tree(
        &self,
        caller: &Caller,
    ) -> Option<(
        tilo_workspace_service::workspaces::providers::Registered,
        Caller,
    )> {
        if !self.is_the_person(caller) {
            return None;
        }
        let tree = self
            .workspaces
            .copies()
            .providers()
            .get(tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER)?;
        let cert = self.workspaces.device_cert();
        let device = Caller::authenticated(cert.user_public_key, cert.device_public_key)
            .in_workspace(&self.workspace);
        Some((tree, device))
    }

    /// `answer`, with the daemon's own `tilo.device` provider listed beside the workspace's apps,
    /// and its page answered whole when the query names it. The workspace's own tree is listed
    /// beside them where `caller` reaches it ([`Self::workspace_tree`]).
    fn with_devices(
        &self,
        caller: &Caller,
        answer: Pending<Result<CataloguePage, ActionCallError>>,
        query: &CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        let query = query.clone();
        let workspace = self.workspace.clone();
        let tree = self.workspace_tree(caller).is_some();
        answer.map(move |answer| {
            answer.map(|page| {
                let mut page = super::ticket::with_device_actions(page, &query, &workspace);
                // The workspace's invocations are published beside its apps, as one more thing
                // a walk of the workspace finds, and so is the workspace's own tree.
                if query.provider.is_none() {
                    page.providers
                        .push(tilo_actions::remote::workspace_invocations::publisher(
                            &workspace,
                        ));
                    let provider = tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER;
                    if tree
                        && !page.providers.iter().any(|listed| {
                            listed.provider == provider && listed.workspace == workspace
                        })
                    {
                        page.providers
                            .push(tilo_actions::remote::CatalogueProvider {
                                provider: provider.to_owned(),
                                app_id: provider.to_owned(),
                                workspace: workspace.clone(),
                                document_id: String::new(),
                                browsable: true,
                            });
                    }
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
            // the person's, and every call the client makes is signed by the agent's key. Each
            // connection is a run of its own, admitted for it: its end retires that run alone,
            // never the one the daemon's other connections go on as.
            let (mut session_host, caller) =
                provider.agent_session(&workspace, &actor, &policy_time, |person| {
                    (device_agent_id(person, MCP_CLIENT), MCP_CLIENT.to_owned())
                })?;
            // A descriptor an answer of this run carries goes with it on the connection it serves.
            session_host.handed = crate::seats::connection_hand();
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
    /// person's public key: admitted here for a dispatch that holds no run of its own, then
    /// [`Self::agent_session_of`].
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
        self.agent_session_of(admission, policy_time, agent)
    }

    /// A run of the person `admission` admits, delegated to the agent `agent` names (its id and
    /// name) from the person's public key: the provider its calls go through and the caller its
    /// admission is. The agent acts under a key of its own this device makes it, its calls
    /// signed by that key and taken by the broker as the agent's. A dispatch that already holds
    /// the person's admission hands it here rather than admitting them a second time.
    pub(super) fn agent_session_of(
        &self,
        admission: Arc<tilo_workspace_service::workspaces::SessionAdmission>,
        policy_time: &tilo_workspace_service::apps::policy_clock::PolicyTime,
        agent: impl FnOnce(&[u8; 32]) -> (String, String),
    ) -> io::Result<(Self, Caller)> {
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
        let person = admission.caller().clone();
        let (named, listed) = (workspace.to_owned(), agent.clone());
        block_on_owned(async move {
            let Some(id) = WorkspaceId::parse(&named) else {
                return;
            };
            tilo_workspace_service::agents::workspace_agent(record.copies(), id, &listed, &person)
                .await;
        });
        // Once listed, the agent's calls are a run of the person's admission, delegated to this
        // agent: what owns a host action they start, rechecked against the workspace's record of
        // its agents. The agent's signature on each call is checked too, but never stands in for
        // that.
        let delegated = self
            .broker
            .certified_agent_run(
                admission.caller(),
                admission.presentation(),
                delegated_agent(&agent).map_err(io::Error::other)?,
            )
            .map_err(io::Error::other)?;
        let run = Self {
            broker: self.broker.clone(),
            workspace: self.workspace.clone(),
            workspaces: self.workspaces.clone(),
            apps: self.apps.clone(),
            session: Some(Arc::new(AdmittedRuntimeSession {
                policy_time: policy_time.clone(),
                provider: Arc::new(Signed::new(delegated, Some(signer))),
                admission,
                agent: Some(agent),
                projection: None,
                pressed: None,
                own_shell: false,
            })),
            tickets: self.tickets.clone(),
            invocations: Arc::clone(&self.invocations),
            kept: Arc::clone(&self.kept),
            asked: self.asked.clone(),
            handed: self.handed.clone(),
        };
        Ok((run, caller))
    }

    /// A run of another member's device `key` in `workspace`, which this device holds: its calls
    /// are that member's own on that device, decided by the workspace's grants for them through
    /// the broker's host for other devices, and a search answers what they may read, never what
    /// this device's person may. Nothing of the device's own is theirs: no ticket, no asked
    /// record of its person's.
    ///
    /// # Errors
    /// The workspace is not kept here, or does not admit the run now.
    pub(super) fn member_session(
        &self,
        workspace: &str,
        key: &DeviceCert,
        policy_time: &tilo_workspace_service::apps::policy_clock::PolicyTime,
    ) -> io::Result<(Self, Caller)> {
        let (copies, named, cert) = (
            self.workspaces.copies().clone(),
            workspace.to_owned(),
            key.clone(),
        );
        // The member's device proved its key on the link that carries its calls, and the
        // workspace's roster admitted it there: its run presents nothing of its own.
        let admission =
            block_on_owned(async move { copies.admit_key(&named, &cert, Vec::new()).await })
                .map_err(io::Error::other)?;
        let caller = admission.caller().clone();
        let run = Self {
            broker: self.broker.clone(),
            workspace: self.workspace.clone(),
            workspaces: self.workspaces.clone(),
            apps: self.apps.clone(),
            session: Some(Arc::new(AdmittedRuntimeSession {
                policy_time: policy_time.clone(),
                provider: self.broker.device_host(),
                admission,
                agent: None,
                projection: None,
                pressed: None,
                own_shell: false,
            })),
            tickets: None,
            invocations: Arc::clone(&self.invocations),
            kept: Arc::default(),
            asked: None,
            handed: None,
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
    /// A call from an admitted session: the person's own call, or one of an agent the device
    /// admitted to, answered as that session's run, and kept as its asked record where it waits
    /// for the person.
    fn admitted_session(
        &self,
        caller: &Caller,
        request: InvokeRequest,
        session: &AdmittedRuntimeSession,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if let Err(refusal) = session.check(caller, &self.workspaces) {
            return Pending::ready(Err(refusal.into()));
        }
        // What the workspace's host keeps for the person — their rule and log, their vault,
        // their accounts and models, their connections and their formats — is the host's to
        // answer, wherever the workspace is kept: a device asks it over the device action
        // service, as the person its transport certified, and never answers it here.
        if tilo_actions::remote::kept_by_workspace_host(&request.target.provider) {
            return self.workspace_kept(session.acting(), request);
        }
        // The person's own act on the workspace's tree (adding to its vault, installing an
        // app), answered as this device ([`Self::workspace_tree`]) and recorded as theirs, so
        // following it reaches what took it.
        if request.target.provider == tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER
            && let Some((tree, device)) = self.workspace_tree(caller)
        {
            self.invocations.record(
                caller,
                &device,
                &request,
                Some(&self.workspace),
                (Arc::clone(&tree.answers), device.clone()),
            );
            return tree.answers.invoke(&device, request);
        }
        // A person's answer to a question that waits for them, as the run admitted them: an
        // agent answers nobody's, its own least. Only the system confirms for them: their own
        // shell's confirm view, or the CLI's own prompt on a connection the kernel says is
        // theirs. A program, an app and an agent are asked, never answer.
        if tilo_actions::remote::asked::is_resolve(&request.target) {
            if session.agent.is_some() || !(session.own_shell || caller.is_own_connection()) {
                return Pending::ready(Err(ActionCallError::Refused {
                    reason: tilo_actions::remote::asked::CONFIRMED_BY_THE_SYSTEM.to_owned(),
                }));
            }
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
                pressed: session.pressed.as_ref().map(|pressed| {
                    let device = self.workspaces.device_cert();
                    tilo_actions::remote::asked::Pressed {
                        device: hex::encode(device.device_public_key),
                        app: tilo_actions::remote::resource_parts(&request.target.provider)
                            .map_or_else(String::new, |(app, _)| app.to_owned()),
                        instance: pressed.instance.clone(),
                        resource: pressed.resource.clone(),
                        at: None,
                    }
                }),
            };
            let route = (Arc::clone(&session.provider), session.acting().clone());
            return asked.send(run, caller, route, request);
        }
        let runs_as = session.acting().clone();
        let invocation = request.invocation;
        self.invocations.record(
            caller,
            session.acting(),
            &request,
            Some(&self.workspace),
            (Arc::clone(&session.provider), runs_as.clone()),
        );
        let records = Arc::clone(&self.invocations);
        session
            .provider
            .invoke(&runs_as, request)
            .map(move |result| {
                // A call the person is asked about goes on and ends once they decide: the record
                // says it waits for them, which the listing shows.
                if result
                    .as_ref()
                    .err()
                    .is_some_and(ActionCallError::awaits_person)
                {
                    records.note_waiting(&invocation);
                }
                result
            })
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
            complete.complete(answered.unwrap_or_else(|| {
                Err(ActionCallError::Unavailable {
                    reason: "a source the watch followed is gone".into(),
                })
            }));
        });
    }
    waited
}

impl RuntimeActionProvider {
    /// `request`, one of what the workspace's host keeps for the person
    /// ([`tilo_actions::remote::kept_by_workspace_host`]), asked of the device that hosts the
    /// workspace as `caller`, a device's person: it is the host's to answer on the device action
    /// service, and nothing of it is decided or kept here.
    fn workspace_kept(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        // What the host keeps for a person is the person's alone: no agent and no app asks it
        // through a device's dispatch.
        if !(caller.acting().is_person() && caller.acting().app().is_none()) {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        }
        let Some(workspace) = caller.workspace() else {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        };
        let hosts = self
            .workspaces
            .hosts()
            .unwrap_or_else(|| super::hosts::Hosts::of(&self.workspaces));
        let Some(host) = hosts.host_actions(workspace) else {
            return Pending::ready(Err(ActionCallError::Unavailable {
                reason: "this device keeps this workspace itself, and what its host keeps for                          the person is asked there"
                    .into(),
            }));
        };
        host.invoke(caller, request)
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
        let Some(agent_id) = agent.acting().agent_id().map(str::to_owned) else {
            return;
        };
        // The agent's own person keeps its name, through their own run.
        if agent.acting().acted_for_person().is_none()
            || agent.acting().acted_for_person() != session.admission.caller().acting().person_id()
        {
            return;
        }
        let person = session.admission.caller().clone();
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
        if query.provider.as_deref() == Some(tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER)
            && let Some((tree, device)) = self.workspace_tree(caller)
        {
            return tree.answers.query(&device, query);
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
                return self.with_devices(caller, answer, &query);
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
                return self.with_devices(caller, answer, &query);
            }
            // Existing general catalogue/builtin APIs retain the exact
            // authenticated run. They make no SDK original-ownership claim.
            let answer = self.broker.state().action_query(session.acting(), &query);
            let answer = session.guarded(Arc::clone(&self.workspaces), caller.clone(), answer);
            return self.with_devices(caller, answer, &query);
        }
        if !self.admitted(caller) {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        }
        let answer = self
            .broker
            .state()
            .action_query(&on_this_device(caller), &query);
        self.with_devices(caller, answer, &query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if let Err(refused) = tilo_actions::appearance::refuse_here(&request) {
            return Pending::ready(Err(refused));
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
            // The device's own person, on the dispatch this device admitted them to, writes as the
            // device their certificate names: what their desktop keeps for its next session.
            let cert = self.workspaces.device_cert();
            let as_device = (self.is_the_person(caller)
                && caller.acting().person_id() == Some(hex::encode(cert.user_public_key).as_str()))
            .then(|| Caller::authenticated(cert.user_public_key, cert.device_public_key));
            return Pending::ready(Ok(
                records.receipt_for(as_device.as_ref().unwrap_or(caller), &request)
            ));
        }
        if let Some(tickets) = &self.tickets
            && Tickets::seals_or_proves(&request)
            && (self.is_the_person(caller) || self.is_own_client(caller))
        {
            return tilo_actions::remote::ActionProvider::invoke(tickets.as_ref(), caller, request);
        }
        // The person's shell runtime on this computer, for the Terminal windows it serves: never
        // an agent that runs elsewhere, nor an app.
        if super::shell_runtime::asks(&request) {
            if !self.is_own_client(caller) {
                return Pending::ready(Err(ActionCallError::Refused {
                    reason: "only the person's own shell runtime on this computer asks for \
                             Terminal's environments and shells"
                        .into(),
                }));
            }
            let Some(person) = self.admission_caller() else {
                return Pending::ready(Err(ActionCallError::NotPermitted));
            };
            return super::shell_runtime::answer(
                super::shell_runtime::Asked {
                    workspaces: &self.workspaces,
                    dispatch: self.clone(),
                    person,
                    handed: self.handed.as_deref(),
                },
                &request,
            );
        }
        if let Some(tickets) = &self.tickets
            && Tickets::answers(&request)
            && (Tickets::asks_for_a_person(&request)
                || self.is_the_person(caller)
                // The person's own programs on this computer ask the daemon to seal, prove and
                // sign with the device key, as their account read the key before the daemon
                // alone kept it: never an agent that runs elsewhere or for a turn.
                || (Tickets::uses_the_device_key(&request) && self.is_own_client(caller))
                || (self
                    .session
                    .as_ref()
                    .is_some_and(|session| session.admits(caller))
                    && (Tickets::only_reads(&request)
                        // A ticket is the person's own to issue. An agent acting for them that
                        // asks for one hands the ask on to a device of theirs that keeps their
                        // root ([`device_action`]), which asks them once: it is never answered
                        // the ticket itself. Waiting for the ticket served last to be taken
                        // issues nothing, and tells which computer took what they let in. A key
                        // naming their device acts as them, and is answered neither.
                        || ((Tickets::adds_a_device(&request) || Tickets::only_waits(&request))
                            && caller.acting().is_agent()))))
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
            return self.admitted_session(caller, request, session);
        }
        Pending::ready(Err(ActionCallError::NotPermitted))
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        // A call the device took before this person's services ran is theirs to answer too: the
        // founding call whose tickets they issue, followed after they start.
        let kept = self.kept.lock_recover().get(id).cloned();
        if let Some(state) = kept {
            return Some(state);
        }
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

    /// Whether `workspace`'s member list, as this device holds it, admits `cert`'s device. A
    /// device its person's root confined to another workspace alone is refused before the list
    /// is asked, by its certificate and by its entry among the person's devices.
    fn admitted(&self, workspace: uuid::Uuid, cert: &DeviceCert) -> Result<(), String> {
        let name = workspace.to_string();
        if !cert.capabilities.reaches_workspace(&name)
            || !super::signing::devices_seen(&*self.held()?).reaches(&cert.device_public_key, &name)
        {
            return Err("this device reaches another workspace alone".to_owned());
        }
        let id = WorkspaceId::parse(&name).ok_or("invalid exact workspace")?;
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
    /// The account sign-ins this daemon serves, as the service mounted to serve them is
    /// (`T`); `None` where nothing serves them, or they are not a `T`.
    #[must_use]
    pub fn account_sign_ins<T: std::any::Any>(&self) -> Option<&T> {
        self.account_sign_ins.as_deref()?.downcast_ref()
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

/// Serve the steps the workspaces' agents and assistant take on this device's dispatch, under the
/// ids apps and agents call them by: an assistant's transform of a selection, a web search, the
/// answers a triggered agent gives, and the tools an agent in a process of its own works through.
/// Each reads the workspaces this daemon holds, so they answer with no desktop session open.
fn serve_agent_steps(workspaces: &ServiceWorkspaces, fanout: &Arc<FanoutState>) {
    use tilo_workspace_service::agents;
    let providers = workspaces.copies().providers();
    let service = tilo_workspace_service::service::ServiceSlot::default();
    service.install(Arc::clone(workspaces.service()));
    let runtime = tilo_rt::app().handle().clone();
    Arc::new(agents::assistant::Transforms::new(
        service.clone(),
        runtime.clone(),
    ))
    .register(&providers);
    Arc::new(agents::WebSearch::new(runtime)).register(&providers);
    Arc::new(agents::triggered::Answers::new(service, Arc::clone(fanout))).register(&providers);
    #[cfg(target_os = "linux")]
    Arc::new(super::agent_tools::AgentToolSteps::default()).register(&providers);
}

/// Run the agents of the workspaces this device's service holds, in this daemon: the one engine
/// a workspace's host runs, each turn's calls decided by `broker` as the person's own calls are.
/// An agent whose person's root is not here is asked for with a ticket `tickets` serves, which
/// its person redeems to approve this device's key for it.
/// What the assistants' look-round proposes is filed through the same broker (`initiative`).
fn start_agents(
    broker: &BrokerHandle,
    workspaces: &ServiceWorkspaces,
    tickets: &Arc<Tickets>,
    initiative: Initiative,
) {
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
            Some(initiative),
        ));
}

/// Answer the workspaces' events and their assistants' look-round on this device: each event is
/// saved as a shortcut in `shortcuts`, and the look-round runs among them. The returned
/// look-round files what it proposes once the device's agents run ([`start_agents`]).
fn serve_initiative(
    workspaces: &ServiceWorkspaces,
    shortcuts: &Arc<dyn ShortcutLibraryServices>,
) -> Initiative {
    let events = Arc::new(WorkspaceEvents::new(
        workspaces.copies().clone(),
        Arc::clone(shortcuts),
    ));
    let initiative = Initiative::new(workspaces.copies().clone(), Arc::clone(&events));
    let providers = workspaces.copies().providers();
    events.register(&providers);
    Arc::new(initiative.clone()).register(&providers);
    initiative
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

/// How often the daemon looks for what its apps raised, and for the answers to what it showed,
/// while nothing is written: what it shows the person is read again as their time there runs
/// out, and their answers are carried out within this.
#[cfg(not(windows))]
const NOTICES_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

/// Carry the notices this device's apps raise into the record of the person's devices, and carry
/// out what the person answers of them there, from now on: what a device serving pages does in
/// place of a desktop session, which the person may not have anywhere.
#[cfg(not(windows))]
fn start_notices(workspaces: &Arc<ServiceWorkspaces>, apps: &Arc<apps::Apps>) {
    let copies = workspaces.copies().clone();
    let cert = workspaces.device_cert().clone();
    let apps = Arc::clone(apps);
    tilo_rt::app().handle().spawn(async move {
        use tilo_workspace_service::tl_records::Package;
        let mut notices = tilo_workspace_service::apps::served_notices::ServedNotices::new(
            copies.clone(),
            cert.device_public_key,
            cert.user_public_key,
        );
        let mut writes = copies.document_writes();
        // The app records whose notices are read: every one the workspaces hold at the start, and
        // each written since.
        let mut pending: std::collections::BTreeSet<(String, String)> =
            std::collections::BTreeSet::new();
        let mut seeded = false;
        // The package each app's record is read through here, by workspace and app: verified once
        // and kept, since an app's install does not change under a running daemon.
        let mut packages: std::collections::BTreeMap<
            (String, String),
            tilo_workspace_service::tl_records::Package,
        > = std::collections::BTreeMap::new();
        // What one pass looks at: every app record written since the last, and the ones the
        // workspaces hold the first time. A pass runs on the tick, so a burst of writes to one
        // record is read once, and an answer the person gave is carried out within the tick.
        let mut tick = tokio::time::interval(NOTICES_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                write = writes.recv() => {
                    match write {
                        Ok(write) => {
                            pending.insert((write.workspace, write.app));
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => seeded = false,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                    }
                    continue;
                }
            }
            if !seeded {
                seeded = true;
                for workspace in copies.list().await {
                    for (app, _) in copies
                        .records(&workspace.workspace)
                        .await
                        .unwrap_or_default()
                    {
                        pending.insert((workspace.workspace.clone(), app));
                    }
                }
            }
            let now = tilo_clock::unix_now_ms_i64();
            for (workspace, app) in std::mem::take(&mut pending) {
                let package = packages
                    .entry((workspace.clone(), app.clone()))
                    .or_insert_with(|| {
                        match super::package_runs::runs_in_process(&app)
                            .then(|| super::package_runs::verified_installed(&workspace, &app))
                        {
                            Some(Ok(verified)) => Package::Installed(verified.module),
                            _ => Package::BuiltIn,
                        }
                    })
                    .clone();
                // An app this device neither installed nor carries runs nowhere here, so its
                // record holds no notice this device could read.
                let readable = !matches!(package, Package::BuiltIn)
                    || copies
                        .tilolang()
                        .is_some_and(|tilolang| tilolang.records.built_in(&app));
                if !readable {
                    continue;
                }
                if let Err(error) = notices
                    .project(
                        &workspace,
                        &app,
                        &apps.display_name(&app),
                        Some(&package),
                        now,
                    )
                    .await
                {
                    tracing::warn!(
                        %workspace,
                        %app,
                        %error,
                        "an app's notices are not shown on the person's devices"
                    );
                }
            }
            if let Err(error) = notices.follow(now).await {
                // This device keeps no copy of the person's own workspace yet (it settles
                // once they are here): nothing of theirs to carry out until it does.
                tracing::debug!(
                    %error,
                    "the person's answers to their notices are not carried out"
                );
            }
        }
    });
}

/// Answer the person's keys from the broker from now on, and serve the workspaces' pages
/// through the daemon's apps: the workspace founded here when this computer founded one
/// (`founded`), else every workspace a keeping server (`serving`) holds.
fn serve_key_sessions(
    key_sessions: &crate::key_sessions::KeySessions,
    (broker, invocations, apps): (&BrokerHandle, &Arc<Invocations>, &Arc<apps::Apps>),
    workspaces: &Arc<ServiceWorkspaces>,
    (actor, founded, serving): (&str, Option<collab_types::WorkspaceId>, bool),
    (policy_origin, first): (
        &tilo_workspace_service::apps::policy_clock::PolicyOrigin,
        &Arc<super::first_workspace::FirstWorkspace>,
    ),
) -> io::Result<()> {
    let broker = broker
        .app_action_broker()
        .ok_or_else(|| io::Error::other("runtime action broker is unavailable"))?;
    let policy_time = policy_origin.capture().map_err(io::Error::other)?;
    let sessions: Arc<dyn crate::key_sessions::Sessions> = match founded {
        // The person's workspace may not be founded yet: their desktop can still hand over the
        // one it kept. Its dispatch is admitted when a key first reaches it, not here.
        Some(workspace) => Arc::new(host_runs::HostRuns {
            broker,
            workspaces: Arc::clone(workspaces),
            invocations: Arc::clone(invocations),
            first: Arc::clone(first),
            provider: std::sync::OnceLock::new(),
            workspace: workspace.to_string(),
            actor: actor.to_owned(),
            policy_time,
        }),
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
    key_sessions
        .pages_served()
        .show_through(Arc::new(AppViews(Arc::clone(apps))));
    key_sessions
        .pages_served()
        .notices_through(Arc::new(AppNotices {
            workspaces: Arc::clone(workspaces),
        }));
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
        kept: Arc::default(),
        handed: None,
        asked: None,
    })
}

/// Apps' views on this device's web pages: each an instance of the app this daemon starts for
/// the page's person, laid out by a window process that draws nothing ([`apps::Apps::open_view`]).
struct AppViews(Arc<apps::Apps>);

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

/// The notifications the person's devices show each other, as the record of their own devices
/// keeps them: what a browser page raises as browser notifications while it is not in front, and
/// where what the person dismisses there is answered, so the record takes it off every device of
/// theirs. The record is the person's own workspace's, whichever workspace the page shows.
struct AppNotices {
    workspaces: Arc<ServiceWorkspaces>,
}

impl crate::key_sessions::PersonNotices for AppNotices {
    fn shown(&self, caller: &Caller, now_ms: i64) -> Vec<PageNotice> {
        let Some(&person) = caller.user_public_key() else {
            return Vec::new();
        };
        let workspaces = Arc::clone(&self.workspaces);
        let record = block_on_owned(async move {
            person_workspace::devices::of(workspaces.copies(), &person).await
        });
        record
            .notices
            .iter()
            .filter(|(_, notice)| notice.shown_at(now_ms))
            .map(|(id, notice)| PageNotice {
                id: id.as_str().to_owned(),
                app_name: notice.app_name.clone(),
                title: notice.title.clone(),
                body: notice.body.clone(),
                urgency: notice.urgency,
            })
            .collect()
    }

    fn answer(&self, caller: &Caller, device: &[u8; 32], id: &str) -> Result<(), String> {
        let person = *caller
            .user_public_key()
            .ok_or("the page's person is not known")?;
        let workspaces = Arc::clone(&self.workspaces);
        let action = Devices::answering_notice(id, device, "");
        let workspace = person_workspace_id(&person);
        block_on_owned(async move {
            person_workspace::apply_on(workspaces.copies(), &workspace, devices::DEVICES, action)
                .await
        })
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
    (apps, first): (
        &Arc<apps::Apps>,
        &Arc<super::first_workspace::FirstWorkspace>,
    ),
    dev: bool,
) -> io::Result<tilo_identity::broker::IdentityBrokerHandle> {
    let person = AttachedSurfaces {
        apps: Arc::clone(apps),
        first: Arc::clone(first),
        dev,
    };
    tilo_identity::broker::spawn(socket, workspaces, keys, Arc::new(person))
}

/// The person-record kinds of the surfaces this runtime holds. Started with
/// `--dev`, an app that is not installed is a build under development (as
/// [`RecordCreation`] treats it): the surface it attaches naming, which
/// `tilolang dev` sets, is taken as its own, so its person records reach
/// the person's workspace. An installed app runs only with its package's signed
/// surface, and no attach replaces a surface the runtime already holds.
pub(super) struct AttachedSurfaces {
    pub(super) apps: Arc<apps::Apps>,
    /// The person's workspace here, which the first app this daemon launched in it founds.
    pub(super) first: Arc<super::first_workspace::FirstWorkspace>,
    pub(super) dev: bool,
}

impl tilo_identity::broker::PersonResourceTypes for AttachedSurfaces {
    fn person_resource_types(&self, app_id: &str) -> Vec<String> {
        ui::app_surface::person_resource_types(app_id)
    }

    fn reads_documents_of(&self, reader: &str, owner: &str) -> bool {
        ui::app_surface::reads_app(reader, owner)
    }

    /// An app this daemon launched in the person's own workspace founds that workspace on a
    /// computer with no login when nothing came in from their desktop yet, before it signs
    /// anything: the device is listed in it then. The app is this daemon's only by its exact
    /// launch record. Any other process, a connection the daemon made for no launch, an app
    /// launched in another workspace, or a workspace already here, founds nothing.
    #[cfg(unix)]
    fn connected(
        &self,
        app_id: &str,
        workspace: &tilo_desktop_model::WorkspaceId,
        stream: &std::os::unix::net::UnixStream,
    ) {
        let Some(pid) = crate::seats::peer_of(stream).ok().and_then(|peer| peer.pid) else {
            return;
        };
        let workspace = workspace.to_string();
        if self.apps.attested(pid) != Some((app_id.to_owned(), workspace.clone())) {
            return;
        }
        if let Err(error) = self.first.found_if_theirs(&workspace) {
            tracing::warn!(%error, app_id, "the person's workspace was not founded for their app");
        }
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
    let (_actions, _endpoint, runtime) = workspaces.turn_action_service()?;
    let cert = workspaces.device_cert();
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
        runtime,
        (cert.user_public_key, cert.device_public_key),
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
