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
#[cfg(unix)]
use crate::host;
use crate::personal_devices::PersonalDevices;
use crate::{RuntimeServices, apps, profile};
use ed25519_dalek::SigningKey;
use serde_json::{Map, Value, json};
#[cfg(unix)]
use std::process::{Child, Command};
use std::{
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tilo_action_services::workspace_invocations::Origin;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus, RemoteActionMessage,
    RemoteActionTarget, RunningTool, device_agent_id,
};
use tilo_api::daemon::DaemonPlace;
use tilo_api::grant_key::GrantKey;
use tilo_api::server::ServerError;
use tilo_api::session::AccountAnswer;
use tilo_app_host::agents::agent_keys::{AgentSigner, Signed, this_device};
use tilo_app_host::agents::events::WorkspaceEvents;
use tilo_app_host::{
    collab_broker::{AppActionBroker, BrokerHandle, FanoutState},
    collab_core::{BrokerChannels, BrokerSealChecks},
    collab_history::CollabHistoryStore,
    collab_outbound::PeerPublisher,
    services::{
        HostedWorkspaces, LauncherServices, ProcessServices, PublishedResources,
        ShortcutLibraryServices, WorkspaceAuthority,
    },
};
use tilo_desktop_model::WorkspaceId;
use tilo_device_identity::login::Device;
use tilo_identity::broker::{IdentityBrokerHandle, WorkspaceMembership};
use tilo_invocations::Invocations;
use tilo_lock::MutexRecover as _;
use tilo_person_services::PersonServices;
use tilo_server_protocol::page::PageNotice;
use tilo_server_protocol::servers::person_workspace_id;
use tilo_server_protocol::workspaces::{
    BRANCH_OWN_WORKSPACE, CREATE_OWN_WORKSPACE, CREATE_WEBHOOK, CreatedWorkspace, DELETE_WEBHOOK,
    DELETE_WORKSPACE, LIST_WEBHOOKS, REVOKE_KEY, RUN_UNATTENDED, WorkspaceLabels, WorkspaceListing,
};
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::service::ServiceConfig;
use tilo_workspace_service::workspaces::Refusal;
use tilo_workspace_service::workspaces::person_workspace;
use tilo_workspace_service::workspaces::person_workspace::devices::{self, Devices};
use uuid::Uuid;

/// The name the person's devices list a local MCP client under: one agent, whichever client
/// connects.
const MCP_CLIENT: &str = "mcp";

/// The name a workspace VM's tenant connection goes by: no app and no agent was named, so the
/// device gives it one of its own, the person's unnamed agent, which may do only what both an
/// agent and the person may. An asked action still asks.
pub(crate) const UNNAMED_AGENT: &str = "agent";

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
#[cfg(target_os = "linux")]
const WAYLAND_SOCKET_ENV: &str = "WAYLAND_SOCKET";

/// What a daemon says on a computer that holds no Tilo identity and has no login.
pub const SET_UP_FIRST: &str = "This computer has no Tilo identity yet: set it up with \
     `tilo login --new-identity`, or with `tilo login` and the code from your Tilo tab (Set up \
     with your agent).";

/// This computer's daemon, as the device. Drop applications first.
pub struct Daemon {
    // Retired before broker fields: retained original ownership cannot survive
    // replacement of the runtime that supplied its policy time.
    policy_origin: tilo_app_host::policy_clock::PolicyOrigin,
    broker: BrokerHandle,
    /// The links of the shells this daemon starts ([`Self::start_shell`]).
    shell_links: Arc<tilo_app_host::shell::ShellLinks>,
    /// The workspaces' own shells this device runs, where it runs any: an app's shell on the
    /// workspace's own target opens there ([`Self::own_shells`]).
    #[cfg(target_os = "linux")]
    own_shells: Option<Arc<ush::workspace_shell::WorkspaceShells>>,
    workspaces: Arc<ServiceWorkspaces>,
    /// The device's live session: the one place this device is in each workspace's call and
    /// presence, which an app's call lane joins through ([`Self::app_call_setup`]).
    live: std::sync::OnceLock<Arc<super::live_relay::Live>>,
    /// How a workspace VM's daemon asks the host that placed it, where this daemon is one
    /// ([`super::vm_environment`]): its Browser's page logins come from the host's vault.
    #[cfg(target_os = "linux")]
    host_asks: std::sync::OnceLock<super::vm_environment::HostAsks>,
    /// Whether this run walked the old keeper's kept secrets into the person's vault: done once
    /// the workspace is held, so an app launch costs no keyring read after the first.
    pub(crate) old_secrets: std::sync::atomic::AtomicBool,
    _identity: IdentityBrokerHandle,
    workspace: String,
    actor: String,
    #[cfg(unix)]
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
    _media: profile::MediaHost,
    /// The endpoint's router, held so the workspace host, the device actions, the work this
    /// device takes and a ticket's certify call answer for as long as this runtime runs.
    _endpoint_router: iroh::protocol::Router,
    /// The person's other devices heard on this device's local network, published for their shell:
    /// after the router the endpoint that hears them.
    _lan: Arc<super::lan::Lan>,
    /// What this device keeps as a server, when its machine is one. After the router that
    /// answers for it.
    _kept: Option<super::keep::Kept>,
    /// The person's other devices, asked from this runtime's endpoint as this device.
    other_devices: Option<crate::other_devices::OtherDevices>,
    /// The apps installed for this profile: what each publishes, before it runs here.
    apps: Arc<apps::Apps>,
    /// The server this device enters its person's workspaces through, once it knows one.
    home: Arc<super::home::Home>,
    /// The person's workspace here: the one this computer enters through, or the one it founded
    /// with their root.
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
    /// What enters the workspaces the server this member takes work from places here.
    entering: Option<Arc<take_work::Entering>>,
    /// Tilo's API as this device, once a caller needed it: one, so an invocation of the
    /// account's begun on one connection is followed on the next.
    api: std::sync::OnceLock<Arc<tilo_api::direct::Direct>>,
    /// What the person's shell reaches: the one dispatch, and this API for their programs.
    shell_dispatch: Arc<super::shell::Dispatching>,
    /// What hands agents' connections on the native endpoint to the device's session.
    agents: super::serve::Agents,
    /// The connections this part of the daemon serves, closed when it ends.
    connections: Arc<crate::daemon::Connections>,
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
    /// The account store this daemon serves, for as long as it runs.
    _served: tilo_identity::store::ServedIdentityDir,
}

impl Daemon {
    /// The device's live session, once the daemon runs it.
    pub(crate) fn live_session(&self) -> Option<&Arc<super::live_relay::Live>> {
        self.live.get()
    }

    /// How this daemon asks the host that placed it, where it is a workspace VM's.
    #[cfg(target_os = "linux")]
    pub(crate) fn host_asks(&self) -> Option<&super::vm_environment::HostAsks> {
        self.host_asks.get()
    }

    /// The device key of the host that placed this daemon's machine, where a host did: read by a
    /// page fill the host lent, which only a Linux machine is placed for.
    #[cfg(target_os = "linux")]
    pub(crate) fn placing_host(&self) -> Option<[u8; 32]> {
        self.workspaces.confined_host().map(|host| host.device)
    }

    /// This daemon is a workspace VM's, and asks the host that placed it through `asks`.
    #[cfg(target_os = "linux")]
    pub(crate) fn asks_its_host(&self, asks: super::vm_environment::HostAsks) {
        let _ = self.host_asks.set(asks);
    }

    /// [`Self::live_session`], under the name those tests know it by.
    #[cfg(test)]
    pub(crate) fn live(&self) -> Option<&Arc<super::live_relay::Live>> {
        self.live_session()
    }

    /// Whether the app `app` installed here declares `capability` in its signed manifest.
    pub(crate) fn app_declares(&self, app: &str, capability: &str) -> bool {
        self.apps.declares(app, capability)
    }

    /// Whether the app `app` installed here declares the session service `service` in its signed
    /// manifest's sandbox.
    pub(crate) fn app_declares_service(&self, app: &str, service: &str) -> bool {
        self.apps.declares_service(app, service)
    }

    /// What the launch `ticket` this device decided runs as, while it runs.
    pub(crate) fn admitted_launch(
        &self,
        ticket: u64,
    ) -> Option<tilo_app_host::shell::AdmittedLaunch> {
        self.apps.admitted_launch(ticket)
    }

    /// The person's id, hex of their root, as which this device acts.
    pub(crate) fn person_id(&self) -> String {
        hex::encode(self.cert.user_public_key)
    }

    /// Serve Tilo's API to the person's own programs on this computer (the `tilo` command, the
    /// SDKs, scripts), as this device's API ([`Self::api`]): each connection one of them makes at
    /// the API socket in their login's private directory is taken by their shell, which this
    /// daemon started, and handed over on its link to be served here. The key it acts as never
    /// leaves the daemon.
    pub(crate) fn serve_programs_api(self: &Arc<Self>) {
        match self.api() {
            Ok(api) => self
                .shell_dispatch
                .serve_api(self.cert.user_public_key, api),
            Err(error) => tracing::warn!(%error, "no API for this computer's programs"),
        }
    }

    /// Hand each origin connector this device starts the connection to this device's one
    /// dispatch, admitted as the app the launch record says it runs: what a program that signs in
    /// calls the published actions over, decided under that app's grants. A daemon that serves
    /// its person's part calls this once the part is wrapped; an embedder that starts one
    /// ([`crate::RuntimeServices::start_reporting`]) calls it itself.
    pub fn hand_actions_to_connectors(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        self.apps
            .hand_actions(Box::new(move |connector, app, workspace, pid| {
                let daemon = weak
                    .upgrade()
                    .ok_or_else(|| io::Error::other("this computer's daemon is stopping"))?;
                super::local::hand_to_connector(
                    &daemon,
                    connector,
                    &crate::seats::AppAt {
                        app: app.to_owned(),
                        workspace: workspace.to_owned(),
                        pid,
                    },
                )
            }));
    }

    /// The workspace's broker, while this daemon runs one.
    pub(crate) fn app_action_broker(&self) -> Option<AppActionBroker> {
        self.broker.app_action_broker()
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
            let until = tilo_clock::now()
                .checked_add(JOIN_WAIT)
                .ok_or("the join's wait reaches past what this clock holds")?;
            let joining = request.target.address.action == super::live_speaking::JOIN;
            let receipt =
                match tilo_actions::remote::ActionProvider::invoke(&tree, &caller, request.clone())
                    .wait_timeout(JOIN_WAIT)
                    .ok()
                    .flatten()
                    .ok_or("the device did not answer the join in time")?
                {
                    Ok(receipt) => receipt,
                    // The workspace grants the app no join yet: its person is asked once, and the
                    // app joins when it carries the call again after they allowed it.
                    Err(tilo_actions::remote::ActionCallError::NotPermitted) if joining => {
                        let workspaces = daemon.workspaces.copies().clone();
                        let (asking, named) = (
                            PersonServices::of(&workspaces).app_asking(&app, &named),
                            named.clone(),
                        );
                        let facts = super::live_speaking::asked_to_join(&app);
                        block_on_owned(async move {
                            PersonServices::of(&workspaces)
                                .ask_for_app(&named, &asking, &request, facts)
                                .await
                        })?;
                        return Err(super::live_speaking::join_asked_first(&app));
                    }
                    Err(error) => return Err(error.to_string()),
                };
            // An admitted join runs on: it is answered once the device is in the call.
            let receipt = match receipt.status {
                tilo_actions::remote::ReceiptStatus::Accepted => {
                    tilo_action_calls::settle_with(&tree, &caller, receipt.invocation, None, until)
                        .wait_timeout(JOIN_WAIT)
                        .ok()
                        .flatten()
                        .flatten()
                        .map_or(receipt, |state| state.receipt())
                }
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

/// How long a founding waits for the person's own workspace to be kept in this device's
/// workspace service before the call ends as not founded: the signal that it is arrives when the
/// service takes it into the workspaces it holds, and no wait looks for it again.
const BINDING: std::time::Duration = std::time::Duration::from_secs(30);

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
    /// The account store this daemon serves, named for as long as it runs: every reader of the
    /// identity store in this process reads it.
    served: tilo_identity::store::ServedIdentityDir,
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
        let identity_dir = device_identity_dir(profile, this_account_uid(), &keys)?;
        // This daemon serves one person, and its services act as them: every reader of the
        // identity store here — the registry, a join proof, the evidence of an admitted call —
        // reads the store that account keeps, not the one of the account the unit runs as (the
        // system daemon runs as root, whose store names nobody).
        let served = tilo_identity::store::serve_identity_dir(identity_dir.clone());
        let serving = || {
            tilo_identity::trusted_device::serving_in(&identity_dir, &keys).map_err(|error| {
                io::Error::new(io::ErrorKind::NotFound, format!("{SET_UP_FIRST} ({error})"))
            })
        };
        let (mut key, mut cert) = serving()?;
        // A computer that slept past its certificate's renewal renews it before its person's
        // services open with it: the session asks for a renewal only once they run. A device that
        // holds no root of its person's waits for one of theirs that does.
        if tilo_identity::provisioning::renewal_due_at(&cert)
            .is_some_and(|due| due <= tilo_clock::unix_now_ns())
        {
            match renew_certificates(this_account_uid(), cert.device_public_key) {
                Ok(_) => (key, cert) = serving()?,
                Err(error) => {
                    tracing::warn!(%error, "this device's certificate is due and was not renewed");
                }
            }
        }
        Ok(Self {
            key,
            cert,
            place,
            served,
        })
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

/// The person's record of the answers they gave about their devices, followed in their own
/// workspace that `workspaces` keep and changed there once this device holds it. `devices` is the
/// record of their devices, which names the device each answer is on.
fn device_answers(
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
    devices: &tilo_workspace_service::workspaces::FollowedRecord,
) -> tilo_peer_state::AnswersRecord {
    use tilo_workspace_service::workspaces::person_workspace::device_answers::{
        DEVICE_ANSWERS, DeviceAnswers,
    };
    let cert = workspaces.device_cert().clone();
    let (moved, changes) = tokio::sync::watch::channel(0_u64);
    let answers = {
        let _entered = tilo_rt::app().handle().enter();
        workspaces.follow_record(&person_workspace::workspace_id(workspaces), DEVICE_ANSWERS)
    };
    let mut saved = answers.changes();
    tilo_rt::app().spawn(async move {
        while saved.changed().await.is_ok() && !moved.is_closed() {
            moved.send_modify(|count| *count = count.wrapping_add(1));
        }
    });
    let apply = {
        let workspaces = workspaces.clone();
        move |action: tilo_resource::Invoke<DeviceAnswers>| {
            let workspaces = workspaces.clone();
            tilo_rt::app().spawn(async move {
                let applied =
                    person_workspace::apply_when_held(&workspaces, DEVICE_ANSWERS, action).await;
                if let Err(error) = applied {
                    tracing::warn!(%error, "an answer about a device was not kept");
                }
            });
        }
    };
    let (person, devices) = (cert.user_public_key, devices.clone());
    let take_back = apply.clone();
    tilo_peer_state::AnswersRecord {
        cert,
        kept: Arc::new(move || {
            answers
                .state::<DeviceAnswers>()
                .map(|kept| kept.kept())
                .unwrap_or_default()
        }),
        changes,
        devices: Arc::new(move || {
            devices
                .state::<Devices>()
                .map(|kept| kept.certified_at(&person, tilo_clock::unix_now_ns()))
                .unwrap_or_default()
        }),
        answer: Arc::new(move |answer| apply(DeviceAnswers::answering(&answer))),
        take_back: Arc::new(move |device, app, cap| {
            take_back(DeviceAnswers::taking_back(device, app, cap));
        }),
    }
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

/// How long a device waits before it asks again for its person's own workspace, which the device
/// keeping their list did not hand it, the first time: doubled each time after, up to
/// [`PERSONS_WORKSPACE_LONGEST_PAUSE`].
const PERSONS_WORKSPACE_FIRST_PAUSE: std::time::Duration = std::time::Duration::from_secs(1);

/// The longest a device waits between two asks for its person's own workspace.
const PERSONS_WORKSPACE_LONGEST_PAUSE: std::time::Duration = std::time::Duration::from_secs(60);

/// The people whose root this process just made, until the start of their services that follows
/// founds their own workspace: the one step that founds it, on the device that made the root.
/// A deployment handed its owner's root ([`adopt_owner_root`]) is that root's device, and makes it
/// here too. Kept in memory alone, so a root whose founding never finished founds nothing at a
/// later start, and no device that was handed a root otherwise founds a second workspace beside
/// the one it made.
static ROOTS_MADE_HERE: Mutex<Vec<[u8; 32]>> = Mutex::new(Vec::new());

/// Found the person of the account `uid` on this computer, the one way a person is made
/// ([`tilo_identity::provisioning::create_person`]): their root is kept for that account alone,
/// and certifies this device's key in `keys` with its authority. Their own workspace is founded
/// in the same step, by the start of their services that follows in this process, and nowhere
/// else. An account that holds its person already makes nobody new: its root certifies the device
/// again, and one whose identity a login or founding stopped partway left is completed from the
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
    // The key this daemon took, which it never reads again; on a computer that has none yet, the
    // key made here before its daemon first starts, which takes it from its file.
    let refused = |error: tilo_device_identity::PersistError| io::Error::other(error.to_string());
    let device = match tilo_device_identity::persist::taken_device_key(keys) {
        Ok(device) => device.verifying_key().to_bytes(),
        Err(_) if !tilo_device_identity::persist::holds_device_key(keys).map_err(refused)? => {
            let (made, _) =
                tilo_device_identity::persist::load_or_create_device_key(keys).map_err(refused)?;
            made.verifying_key().to_bytes()
        }
        Err(error) => return Err(refused(error)),
    };
    let held = tilo_device_identity::login::served_cert(keys)
        .filter(|cert| cert.device_public_key == device)
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
    let person = tilo_identity::provisioning::create_person(dir, keys, name.trim(), "", storage)?;
    ROOTS_MADE_HERE.lock_recover().push(person);
    Ok(person)
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
    // A computer set up before each account kept its own store keeps its person above it: the
    // account that runs takes that person up before anything reads its store, or this device
    // would find nobody and a new person would be founded in its person's place.
    if tilo_identity::store::take_up_person_kept_above(&own_store)? {
        tracing::info!(
            store = %own_store.display(),
            "this account took up the person its computer kept before each account kept its own"
        );
    }
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

/// Renew this device's certificates of the account `uid`'s person that are due, with the root
/// this computer keeps for that account. `device` is this device's key, which each renewed
/// certificate names. The root is sealed with the device key, so only the daemon opens it: an
/// account's session asks this rather than renew its certificates itself. Each keeps its
/// lifetime from the time it is renewed. Answers how many were renewed.
///
/// # Errors
/// The account holds no person here, this computer keeps no root of theirs while one of their
/// certificates is due, or a certificate cannot be signed or written.
pub(crate) fn renew_certificates(uid: u32, device: [u8; 32]) -> Result<u64, String> {
    use tilo_device_identity::persist;
    use tilo_user_identity::DeviceCert;
    /// One renewal at a time, so two never reissue one certificate.
    static RENEWING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = RENEWING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = account_identity_dir(uid).map_err(|error| error.to_string())?;
    let record = tilo_identity::store::resolve_active_identity(&dir)
        .ok_or("this account holds no person on this computer")?;
    let person = tilo_identity::store::load_metadata_from_path(&record)
        .map_err(|error| error.to_string())?
        .group_public_key;
    let paths = tilo_identity::provisioning::collaboration_identity_paths();
    let now = tilo_clock::unix_now_ns();
    let mut due: Vec<(std::path::PathBuf, DeviceCert)> = Vec::new();
    let path = paths.device_cert_for_user(&person);
    if let Ok(cert) = persist::read_device_cert(&path)
        && cert.user_public_key == person
        && cert.device_public_key == device
        && tilo_identity::provisioning::renewal_due_at(&cert).is_some_and(|at| at <= now)
    {
        due.push((path, cert));
    }
    if due.is_empty() {
        return Ok(0);
    }
    let root = root_for(uid)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| {
            "this computer keeps no root of this person's: a device of theirs that does renews \
             this one's certificate"
                .to_owned()
        })?;
    if root.group_public_key() != person {
        return Err(
            "the root this computer keeps is another person's than the account's".to_owned(),
        );
    }
    for (path, held) in &due {
        let lifetime = held
            .expires_at_ns
            .map(|expires| expires.saturating_sub(held.issued_at_ns));
        let mut cert = DeviceCert {
            issued_at_ns: now,
            expires_at_ns: lifetime.map(|lifetime| now.saturating_add(lifetime)),
            signature: [0; 64],
            issuer: None,
            ..held.clone()
        };
        tilo_user_identity::sign_device_cert(&mut cert, &root)
            .map_err(|error| format!("the certificate is not signed: {error}"))?;
        persist::write_device_cert(path, &cert).map_err(|error| error.to_string())?;
    }
    Ok(u64::try_from(due.len()).unwrap_or(u64::MAX))
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
    let refused = match root_record(uid) {
        Ok(record) => return Ok(record),
        Err(error) => error,
    };
    // A root sealed with the key this daemon took keeps no share the record's public metadata
    // counts as held, so the store names no root there: the daemon that took the key opens it,
    // and the record it opens is the root it keeps.
    opened_root_record(uid).ok_or(refused)
}

/// The record of `uid`'s store whose root this process opens with the device key its daemon took:
/// the one whose secret is sealed to this device, which no metadata reading alone counts as a
/// share held.
fn opened_root_record(uid: u32) -> Option<std::path::PathBuf> {
    opened_root_in(&account_identity_dir(uid).ok()?)
}

/// [`opened_root_record`] of the account store `dir`. A device a ticket made keeps its person's
/// public record there, which opens too but holds no key: counted as a root, the device would
/// take itself for its person's first and found a second copy of their workspace beside the one
/// their devices hand it. Only a record that opens as the person's whole key is their root.
fn opened_root_in(dir: &Path) -> Option<std::path::PathBuf> {
    let record = tilo_identity::store::resolve_active_identity(dir)?;
    tilo_identity::store::load_from_path(record.clone())
        .ok()
        .filter(tilo_identity::store::SigningIdentity::is_single_device)
        .map(|_| record)
}

/// The variable a deployment hands the owner's root with: a file holding the root's 32-byte seed
/// as 64 hexadecimal characters, readable by its owner alone.
pub const OWNER_ROOT_FILE: &str = "TILO_WORKSPACES_ROOT_FILE";

/// Adopt the owner's root a deployment hands this daemon ([`OWNER_ROOT_FILE`]), once: the root
/// kept here as on any of its person's own devices, which certifies this device's key and makes
/// this computer one the person's services start as (so its configured workspaces are founded
/// as the owner, not kept as a server's). A deployment that names none leaves the computer as it
/// was; one whose root is kept here already adopts nothing again.
///
/// # Errors
/// The file cannot be read by its owner alone, it holds no 32-byte seed as hex, the identity
/// store keeps another person's root, or the root cannot be restored and certify this device.
pub(crate) fn adopt_owner_root(profile: &Path) -> io::Result<()> {
    let Some(file) = std::env::var_os(OWNER_ROOT_FILE) else {
        return Ok(());
    };
    if file.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{OWNER_ROOT_FILE} names no file"),
        ));
    }
    let file = PathBuf::from(file);
    let keys = tilo_api::calls::device_keys();
    let dir = device_identity_dir(profile, this_account_uid(), &keys)?;
    adopt_root_file(&file, &dir, &keys)
}

/// [`adopt_owner_root`], for the root at `file` and the identity store `dir`, with `keys` this
/// device's own: the same root a person's own device keeps, restored through the one way a
/// person is brought back on a computer ([`tilo_identity::provisioning::restore_from_recovery_words`]).
fn adopt_root_file(
    file: &Path,
    dir: &Path,
    keys: &tilo_device_identity::IdentityPaths,
) -> io::Result<()> {
    use std::io::Read as _;
    let handed = |why: String| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "the owner root this deployment hands the daemon ({}): {why}",
                file.display()
            ),
        )
    };
    let mut text = String::new();
    // The root is its owner's secret: the file is read as a private one, owned by this user,
    // owner-only and singly linked.
    tilo_files::native::private::read(file)
        .and_then(|file| file.take(129).read_to_string(&mut text))
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "the owner root this deployment hands the daemon ({}): {error}",
                    file.display()
                ),
            )
        })?;
    let seed: [u8; 32] = hex::decode(text.trim())
        .ok()
        .and_then(|seed| <[u8; 32]>::try_from(seed).ok())
        .ok_or_else(|| {
            handed("it is the root's 32-byte seed as 64 hexadecimal characters".to_owned())
        })?;
    // The root kept here already is this one: a start after the first adopts nothing again.
    if let Some(record) = tilo_identity::store::held_root(dir) {
        let kept = tilo_identity::store::load_metadata_from_path(&record)
            .map_err(|error| handed(error.to_string()))?;
        let person = SigningKey::from_bytes(&seed).verifying_key().to_bytes();
        return if kept.group_public_key == person {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "the owner root this deployment hands the daemon ({}) is another person's \
                     than the root this computer already keeps ({})",
                    file.display(),
                    hex::encode(kept.group_public_key)
                ),
            ))
        };
    }
    // The identity store's one way of keeping a person's root again, which also certifies this
    // device's key under it: the same path a person's own device takes.
    let words = tilo_user_identity::recovery_words::encode(&seed)
        .ok_or_else(|| handed("its seed did not encode as a recovery key".to_owned()))?;
    tilo_identity::provisioning::restore_from_recovery_words(
        dir,
        keys,
        &words,
        None,
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "the owner root this deployment hands the daemon ({}): {error}",
                file.display()
            ),
        )
    })?;
    // The deployment is its person's root device: their root is made here, as on a first device,
    // and the start of their services that follows founds their workspace.
    ROOTS_MADE_HERE
        .lock_recover()
        .push(SigningKey::from_bytes(&seed).verifying_key().to_bytes());
    Ok(())
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

    /// Show the tilolang app `app`'s window on `workspace`'s layout of this device's person: a
    /// layout showing none gets one placed, and the window showing it comes forward. Whether the
    /// person's layout showed it already.
    ///
    /// # Errors
    /// The layout has no place for the window, or its write was refused.
    #[cfg_attr(
        not(target_os = "linux"),
        expect(
            dead_code,
            reason = "a tilolang app's window is the person's layout's on Linux alone: every \
                      other platform launches a window process for it"
        )
    )]
    pub(crate) fn show_app_window(&self, app: &str, workspace: &str) -> Result<bool, String> {
        let title = self.apps.display_name(app);
        super::layout::DeviceLayouts::new(Arc::clone(&self.workspaces))
            .show(workspace, (app, &title, ""))
    }

    /// The one-use secret a shell redeems for its link: what this daemon gives the shell it starts
    /// and what root's sign-in answers, and what the shell asks `shells#link` with.
    pub fn shell_token(&self) -> [u8; 32] {
        self.shell_links.make(tilo_app_host::shell::ShellCaller {
            person: self.workspaces.device_cert().user_public_key,
        })
    }

    /// The secret a new link for `person`'s shell on `seat` is made with, refused while the seat
    /// has one.
    ///
    /// # Errors
    /// The seat is linked already.
    pub fn shell_token_for(&self, person: [u8; 32], seat: u64) -> io::Result<[u8; 32]> {
        if self.shell_links.live(seat) {
            return Err(io::Error::other("this seat's shell is linked already"));
        }
        Ok(self
            .shell_links
            .make_for(tilo_app_host::shell::ShellCaller { person }, Some(seat)))
    }

    /// The connection `secret` admits on this daemon's dispatch: the shell's end of a link the
    /// daemon made, and the secret let go of when the dispatch cannot be reached.
    ///
    /// # Errors
    /// The dispatch's socket does not take the connection.
    pub(crate) fn shell_link_of(&self, secret: [u8; 32]) -> io::Result<tilo_socket::Stream> {
        match self.link_with(&secret) {
            Ok(link) => Ok(link),
            Err(error) => {
                self.shell_links.forget(&secret);
                Err(error)
            }
        }
    }

    /// Who the one-use secret `token` was made for, without taking it.
    pub(crate) fn shell_token_holder(&self, token: &[u8; 32]) -> Option<[u8; 32]> {
        self.shell_links.peek(token).map(|caller| caller.person)
    }

    /// Redeem `secret`, a one-use token this daemon made, as the link of `seat`: the secret
    /// admits one connection, for that seat, and no other.
    ///
    /// # Errors
    /// No such secret waits, or the dispatch's socket does not take the connection.
    pub(crate) fn shell_redeem(
        &self,
        secret: [u8; 32],
        seat: u64,
    ) -> io::Result<tilo_socket::Stream> {
        if !self.shell_links.assign(&secret, seat) {
            return Err(io::Error::other("this token waits for no shell link"));
        }
        match self.link_with(&secret) {
            Ok(link) => Ok(link),
            Err(error) => {
                self.shell_links.forget(&secret);
                Err(error)
            }
        }
    }

    /// Hand the device's tilolang processes `packets`, which a connection of the account `uid`
    /// carried to the native endpoint with `secret`: admitted when a shell of that account was
    /// answered `secret` on its link moments ago, and never twice
    /// ([`tilo_app_host::shell::ShellLinks::hand_processes`]).
    ///
    /// # Errors
    /// Why the hand-over is refused.
    pub(crate) fn hand_processes(
        &self,
        uid: u32,
        secret: &[u8; 32],
        packets: runtime_portal::process_link::Descriptor,
    ) -> Result<(), String> {
        self.shell_links.hand_processes(uid, secret, packets)
    }

    /// Whether the seat has a shell link standing now.
    pub(crate) fn shell_live(&self, seat: u64) -> bool {
        self.shell_links.live(seat)
    }

    /// Wait until the seat has no shell link standing, up to `within`: whether it is free. A link
    /// the shell ended frees its seat a moment later, when this daemon reads the connection
    /// close, so the shell that takes the seat next waits rather than be refused.
    #[cfg(feature = "test-support")]
    pub(crate) fn wait_shell_free(&self, seat: u64, within: std::time::Duration) -> bool {
        self.shell_links.wait_until_free(seat, within)
    }

    /// Whether this daemon acts for `person`: the person whose certificate names this device,
    /// whose workspaces its services keep, and for whom its shell links are made.
    pub(crate) fn serves(&self, person: [u8; 32]) -> bool {
        self.workspaces.device_cert().user_public_key == person
    }

    /// The connection `secret` admits on this daemon's dispatch: the shell's end of a link the
    /// daemon made.
    ///
    /// # Errors
    /// The dispatch's socket does not take the connection.
    pub fn link_with(&self, secret: &[u8; 32]) -> io::Result<tilo_socket::Stream> {
        use runtime_collab::wire::shell::SHELL_MAGIC;
        use std::io::Write as _;
        let mut link = tilo_socket::connect(self.broker.app_socket())?;
        link.write_all(SHELL_MAGIC)?;
        link.write_all(secret)?;
        Ok(link)
    }

    /// A link for this device's person's shell, made and admitted by a fresh secret: what a test
    /// takes in its own process. It is made for this process's seat, as the daemon's native
    /// endpoint makes the link a shell redeems its token for on the account the kernel names, so
    /// the processes it hands the device are admitted for that account. A link with no seat is
    /// still refused what it hands over.
    ///
    /// # Errors
    /// This process's seat has a shell link already, or the dispatch's socket does not take the
    /// connection.
    pub fn shell_link(&self) -> io::Result<tilo_socket::Stream> {
        let seat = crate::seats::seat_id(crate::seats::own_account(), std::process::id());
        let person = self.workspaces.device_cert().user_public_key;
        let secret = self.shell_token_for(person, seat)?;
        self.link_with(&secret)
    }

    /// Each action this daemon's broker runs, as it is admitted and as it ends: `None` while it
    /// runs no broker.
    #[cfg(target_os = "linux")]
    pub(crate) fn execution_activity(
        &self,
    ) -> Option<
        tokio::sync::broadcast::Receiver<Arc<tilo_app_host::collab_app_broker::ExecutionActivity>>,
    > {
        self.broker
            .app_action_broker()
            .map(|broker| broker.state().subscribe_execution_activity())
    }

    /// The workspaces' own shells this device runs, where it runs any: what an app's shell on
    /// the workspace's own target opens.
    #[cfg(target_os = "linux")]
    pub(crate) fn own_shells(&self) -> Option<Arc<ush::workspace_shell::WorkspaceShells>> {
        self.own_shells.clone()
    }

    /// The links this daemon made, for a test that shows a secret admits one connection and no
    /// second: [`Self::shell_link`] takes its own secret as it connects.
    #[cfg(test)]
    pub(crate) fn shell_links(&self) -> &Arc<tilo_app_host::shell::ShellLinks> {
        &self.shell_links
    }

    /// Start `command` as the person's shell, given the one-use secret it redeems for its link
    /// (`runtime_collab::wire::shell::SHELL_TOKEN_ENV`): the daemon starts the shell itself, so the
    /// secret passes from it to that one process and to no other of the account, and the shell asks
    /// for its link on the daemon's own socket, as it asks for one again when that link ends. A
    /// session in a window sends the connection it draws on with its start (`window`, which the
    /// native endpoint received); the shell is handed it at fd 4 as `WAYLAND_SOCKET`, the variable
    /// a Wayland client takes its connection from.
    ///
    /// # Errors
    /// The shell did not start.
    #[cfg(target_os = "linux")]
    pub fn start_shell(
        &self,
        mut command: Command,
        window: Option<&runtime_portal::process_link::Descriptor>,
    ) -> io::Result<Child> {
        use std::os::fd::AsRawFd as _;
        /// Where a session's window connection is handed to the shell it starts.
        const WINDOW_FD: i32 = 4;
        let token = hex::encode(self.shell_token());
        if let Some(window) = window {
            launcher::inherit::hand_down(
                &mut command,
                vec![(window.as_raw_fd(), WINDOW_FD)],
                false,
            );
            command.env(WAYLAND_SOCKET_ENV, WINDOW_FD.to_string());
        } else {
            // The request's own environment named nothing this shell holds.
            command.env_remove(WAYLAND_SOCKET_ENV);
        }
        command.env(runtime_collab::wire::shell::SHELL_TOKEN_ENV, token);
        command.spawn()
    }

    /// Where an app's search entries are published on its connection here, once the daemon
    /// keeps the device's index.
    #[cfg(feature = "test-support")]
    pub(crate) fn search_index_services(
        &self,
    ) -> Option<Arc<dyn tilo_app_host::services::SearchIndexServices>> {
        self.workspaces.search_index_services()
    }

    /// What a plain connection from the person's own account reaches in `workspace`, and the
    /// caller it is there ([`Self::connection_tree_in`]): what `tilo do`'s own prompt answers a
    /// question that waits for the person on, for a test that has no socket to connect to.
    ///
    /// # Errors
    /// The workspace cannot be admitted to now.
    #[cfg(feature = "test-support")]
    pub fn own_connection_in(
        self: &Arc<Self>,
        workspace: &str,
    ) -> io::Result<(tilo_api::session::Provider, Caller)> {
        self.connection_tree_in(Some(workspace))
    }

    /// The workspaces this device holds, through its opened workspace service.
    #[must_use]
    pub fn workspaces(&self) -> &tilo_workspace_service::workspaces::Workspaces {
        self.workspaces.copies()
    }

    /// The device's workspace service, which a login answered while this part runs enters its
    /// workspaces through, as a start does.
    pub(crate) fn service_workspaces(&self) -> &Arc<ServiceWorkspaces> {
        &self.workspaces
    }

    /// The person this part serves, by their id: whose login it takes in while it runs.
    pub(crate) fn serves_person(&self) -> &str {
        &self.actor
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

    /// [`Self::provide`] for a provider that is the installed app `app`'s own code, which this
    /// device runs for it under its grants: what the app's own calls of it ask is covered by the
    /// yes the app's invocation was given.
    pub fn provide_own(
        &self,
        provider: &'static str,
        app: &'static str,
        answers: Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync>,
    ) {
        self.workspaces.copies().provide_own(provider, app, answers);
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
        self.apps
            .open_view((caller, None), app, workspace, record, reveal)
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

    /// Whether `device` is this computer as it is, logged in somewhere else: the device its own
    /// certificate makes it, which a login with a ticket never answers.
    pub(crate) fn is_this_device(&self, device: &tilo_device_identity::login::Device) -> bool {
        device.cert.device_public_key == self.cert.device_public_key
            && device.cert.user_public_key == self.cert.user_public_key
            && device.cert.signature == self.cert.signature
    }

    /// Enter the person's workspaces through `device`, the server their own workspace names now,
    /// while this part keeps running: its stores, links and calls stay as they are. The work
    /// this member takes comes from that server from now on, and the person's account and this
    /// computer's presence are reached there next time they are asked for.
    pub(crate) fn move_to(&self, device: tilo_device_identity::login::Device) {
        if let Some(entering) = &self.entering
            && let Some(server) = take_work::Server::from_login(Some(&device))
        {
            entering.move_to(server);
        }
        self.home.enter_through(device);
        self.account.lock_recover().take();
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
        let here = self.api_here();
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

    /// Tilo's API as this daemon's device, entering every workspace as `here` does: what a
    /// connection that is not the person's own is answered by ([`super::api_lane`]), named
    /// `label` in History.
    ///
    /// # Errors
    /// The API server cannot be built.
    pub(crate) fn api_as(
        &self,
        here: Arc<dyn tilo_api::session::Here>,
        label: &str,
    ) -> io::Result<Arc<tilo_api::direct::Direct>> {
        tilo_api::direct::Direct::here(here, self.key.clone(), self.cert.clone(), label)
            .map(Arc::new)
            .map_err(io::Error::other)
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
    /// being a connection of theirs (an agent's action frames), and the caller it is there. Such
    /// a program is no connection's own answer to what a workspace asks of the person: its calls
    /// ask.
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
    /// A device confined to one workspace, by a certificate for that workspace or one placed for
    /// records of it (a workspace's VM, a hosted browser's environment), is no device of the
    /// person's own connection: a same-account peer there is no more their own act than a
    /// program is, so its calls ask.
    fn connection_caller(&self, workspace: Option<&str>) -> Caller {
        let person = if self.cert.capabilities.workspace_name().is_none() {
            Caller::own_person(&self.actor)
        } else {
            Caller::local_person(&self.actor)
        };
        match workspace {
            Some(workspace) => person.in_workspace(workspace),
            None => person,
        }
    }

    /// The tree a workspace VM's tenant reaches in `workspace` (none: the one this daemon
    /// serves), and the caller it is admitted as there: the person's unnamed agent, never the
    /// person, so an asked action still asks.
    ///
    /// # Errors
    /// The workspace cannot be admitted to now, or the device cannot make the agent's key.
    pub(crate) fn tenant_tree_in(
        self: &Arc<Self>,
        workspace: Option<&str>,
    ) -> io::Result<(tilo_api::session::Provider, Caller)> {
        // A device placed for records of one workspace admits nothing itself: the tenant's calls
        // go to the workspace's host over the link this device dialled, which decides each as
        // this placed device.
        if let Some(placed) = self.workspaces.confined() {
            let named = placed.to_string();
            let host = super::confined::forwarded(&self.workspaces, workspace)
                .map_err(io::Error::other)?;
            let api = self.api()?;
            let caller = Caller::visitor(self.cert.device_public_key).in_workspace(&named);
            let joined = block_on_owned(async move { api.joined(&named, host).await })
                .map_err(io::Error::other)?;
            return Ok((joined, caller));
        }
        let workspace = workspace.unwrap_or(&self.workspace);
        let time = self.policy_origin.capture().map_err(io::Error::other)?;
        let base = self.provider_for(workspace)?;
        let (provider, _, _, _) = base.agent_session(workspace, &self.actor, &time, |person| {
            (
                device_agent_id(person, UNNAMED_AGENT),
                UNNAMED_AGENT.to_owned(),
            )
        })?;
        // The connection is that agent's: the caller it acts as here is the agent itself, which
        // the workspace decides with the delegation its person gave it and nothing more.
        let caller = provider
            .session
            .as_ref()
            .map(|session| session.acting())
            .cloned()
            .ok_or_else(|| io::Error::other("this device made no agent for the connection"))?;
        let api = self.api()?;
        let named = workspace.to_owned();
        let joined = block_on_owned(async move { api.joined(&named, Arc::new(provider)).await })
            .map_err(io::Error::other)?;
        Ok((joined, caller))
    }

    /// The tree a plain connection from the person's own account reaches in `workspace` (none:
    /// the one this daemon serves), and the caller it is admitted as there: the person, whom the
    /// daemon admits from who is on the other end of its own socket, with no signing step of its
    /// own added.
    ///
    /// The person's own workspace is opened the first time a connection reaches it, as every
    /// other caller of the daemon opens it ([`crate::RuntimeServices::workspace`]).
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
        // A confined device never treats a connection as the person: its own connection acts as
        // the person's unnamed agent at the one workspace its certificate reaches, as the socket
        // its tenant reaches does, so an asked action asks.
        if super::confined::workspace_of(&self.cert).is_some() {
            return self.tenant_tree_in(workspace);
        }
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
        Ok((joined, self.app_caller(app, workspace)))
    }

    /// The workspaces' broker, while it serves apps.
    pub(crate) fn app_broker(&self) -> Option<AppActionBroker> {
        self.broker.app_action_broker()
    }

    /// The caller an app's own connection is decided as in `workspace`: the app, holding the
    /// grant its signed manifest declares where it declares that it signs in to the workspace's
    /// accounts. The grant names the app for the accounts and nothing of a person's rights.
    #[must_use]
    pub(crate) fn app_caller(&self, app: &str, workspace: &str) -> Caller {
        let caller = Caller::launched(app).in_workspace(workspace);
        if !self.app_declares(app, tilo_app_package::manifest::ACCOUNT_SIGN_INS) {
            return caller;
        }
        caller.granting(vec![tilo_action_policy::Grant::new(
            tilo_workspace_records::accounts::sign_in_grant_id(app, workspace),
            tilo_action_policy::Grantee::app(app),
            tilo_workspace_records::accounts::sign_ins_node(workspace),
            tilo_action_types::KindSet::READ,
            tilo_action_policy::Answer::Allow,
            tilo_action_policy::Grantee::person(self.person_id()),
        )])
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

    /// Record `pid`, a shell the device's shell runtime started for `app` in `workspace` at
    /// `started`, as that app's: it and everything started in it act with the app's grants from
    /// now on, never the person's. No workspace named is the runtime's primary one, this daemon's
    /// own; any other must be one this computer holds. Answers the workspace it was recorded in.
    ///
    /// # Errors
    /// The workspace is not held here, or the process is not that shell
    /// ([`crate::apps::Apps::record_shell`]).
    pub(crate) fn record_shell(
        &self,
        pid: u32,
        started: u64,
        app: &str,
        workspace: &str,
    ) -> Result<String, String> {
        let workspace = self.shell_workspace(workspace)?;
        self.apps.record_shell(pid, started, app, &workspace)?;
        Ok(workspace)
    }

    /// The workspace a shell the shell runtime names runs in: this daemon's own for none (the
    /// runtime's primary one), and any other one this computer holds.
    ///
    /// # Errors
    /// The workspace is not held here.
    pub(crate) fn shell_workspace(&self, workspace: &str) -> Result<String, String> {
        let workspace = if workspace.is_empty() {
            self.workspace.clone()
        } else {
            workspace.to_owned()
        };
        if workspace != self.workspace && !self.apps.launches_into(&workspace) {
            return Err(format!("this computer holds no workspace {workspace}"));
        }
        Ok(workspace)
    }

    /// [`Self::app_of`], for a connection process `pid` made to this device: the app it runs has
    /// connected ([`crate::apps::Apps::connected_as`]).
    pub(crate) fn app_connected(&self, pid: u32) -> Option<(String, String)> {
        self.apps.connected_as(pid)
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
        // this device keeps: their home takes it in at the server that login names, by the
        // certificate their root signed, so it is here already on a device that just joined.
        // Where it is kept on another of their devices, their list says where; a device that
        // keeps none of it reads none of the list, so the entry above is what this asks for
        // first. The device that keeps that list may not answer yet (it starts beside this one,
        // or is away), so it is asked again, less often each time, for as long as this account
        // is the one this part asks.
        let (own, asked) = (Arc::downgrade(&self.workspaces), Arc::downgrade(&account));
        tilo_rt::app().handle().spawn(async move {
            let Some(own_workspace) = WorkspaceId::parse(&person) else {
                return;
            };
            let mut pause = PERSONS_WORKSPACE_FIRST_PAUSE;
            loop {
                let (Some(own), Some(asked)) = (own.upgrade(), asked.upgrade()) else {
                    return;
                };
                // A device confined to one workspace keeps no copy of its person's own.
                if own.confined().is_some() || own.keeps(own_workspace).await {
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
                let Err(why) = entered else {
                    return;
                };
                tracing::warn!(
                    %why,
                    retry_in_s = pause.as_secs(),
                    "the person's own workspace is not kept here yet"
                );
                drop((own, asked));
                tokio::time::sleep(pause).await;
                pause = pause.saturating_mul(2).min(PERSONS_WORKSPACE_LONGEST_PAUSE);
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
        Arc::new(move |asked: Option<&str>| {
            let daemon = daemon
                .upgrade()
                .ok_or_else(|| io::Error::other("this computer's daemon is stopping"))?;
            daemon.agent_session_in(&name, asked.unwrap_or(&daemon.workspace))
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
        let (host, caller, cert, agent) =
            host.agent_session_of(admission, &policy_time, |person| {
                (
                    tilo_actions::remote::device_agent_id(person, name),
                    name.to_owned(),
                )
            })?;
        // The apps of the workspace: what the agent's task programs compile against, run in this
        // daemon's processes.
        let tasks = self.task_hosts(host.clone(), caller.clone(), &self.actor);
        let session = agent_session_id(&caller)?;
        tilo_mcp::McpServer::admitted(host, caller).map(|server| {
            server
                .with_agent_session(session)
                .with_invoker(tilo_actions::ActionInvoker::admitted(agent.acting()))
                .with_task_hosts(tasks)
                .with_device_shell()
                .signed_as(cert.user_public_key, cert.device_public_key)
        })
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

    /// The connections this part of the daemon serves: each is closed when it ends
    /// ([`crate::daemon::Connections::end`]), so its clients connect again to the part that
    /// starts next rather than being answered by this one's services after they ended.
    pub(crate) fn connections(&self) -> &Arc<crate::daemon::Connections> {
        &self.connections
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
        founding: Option<&Arc<super::unclaimed::Founding>>,
        reused: Option<iroh::Endpoint>,
    ) -> io::Result<Self> {
        // The calls the device took before this person's services ran: they answer them too, so
        // a founding call followed after they start reads its state where it was kept.
        let inherited = founding
            .map(|founding| founding.calls())
            .unwrap_or_default();
        let place = tilo_api::daemon::this_computer().map_err(io::Error::other)?;
        // The identity signs with the device's key, which only the process that took it reads: a
        // daemon whose start locked nothing takes it first, before anything here opens the
        // identity: a server's enrolment, the owner root a deployment hands it, its person.
        let lock = if locked.is_none() {
            stage("locking the device");
            let lock = super::serve::lock_device(&place)?;
            // As a computer's first start: the key is made here when its system made none.
            let keys = tilo_api::calls::device_keys();
            tilo_device_identity::persist::load_or_create_device_key(&keys).map_err(|error| {
                io::Error::other(format!("this computer's device key: {error}"))
            })?;
            let key = super::unclaimed::take(&keys)?;
            Some((key, lock))
        } else {
            None
        };
        // A server keeps the workspaces placed on it: it takes no other server's work. A node
        // whose unit points at an install token waits for its page to install its certificate
        // before anything else is read: until then, this machine is nobody's device.
        let mut keeping = super::keep::Keeping::configured()?;
        if let Some(keeping) = keeping.as_ref() {
            keeping.enroll()?;
        }
        // The owner's root a deployment hands this daemon is kept here before the identity is
        // read, so this computer is the person's own device and founds its workspaces under it.
        adopt_owner_root(root)?;
        let identity = Identity::here(root)?;
        tilo_files::native::private::directory(identity.place.dir())?;
        if locked.is_some_and(|device| device != identity.key.verifying_key().to_bytes()) {
            return Err(io::Error::other(
                "the device key this daemon locked at its start is not the one its person's \
                 certificate names",
            ));
        }
        tilo_files::native::private::directory(root)?;
        // The native endpoint every account reaches, and the services' own sockets apart from it:
        // a broker's socket refuses a directory another account can read.
        let sockets = crate::daemon::sockets_served(root)?;
        let dev = matches!(serving, Serving::Dev);
        let actor = identity.person();
        let joined = identity.joined();
        // A device its person's root confined to one workspace (a workspace's VM) holds no keys:
        // it lists nothing of its person's, marks no root and keeps no copy of any workspace,
        // and reaches its one workspace through the host its login entered through.
        let confined = super::confined::workspace_of(&identity.cert).is_some();
        let (workspace_id, founded) = if joined {
            (
                tilo_server_protocol::servers::person_workspace_id(&identity.cert.user_public_key),
                None,
            )
        } else {
            // The person's own workspace, at the id their key names: the one a device a ticket
            // of this computer's makes enters, as the `joined` arm above names it.
            let founded = person_workspace(&identity.cert.user_public_key);
            (founded.to_string(), Some(founded))
        };
        let apps = Arc::new(apps::Apps::from_env(root, &sockets));
        // A server's page serves its kept workspaces' links: the apps it starts mint a
        // resource's link under it, and the apps of a device that keeps no server mint none.
        if let Some((web_entry, workspaces)) =
            keeping.as_ref().and_then(super::keep::Keeping::serving)
        {
            apps.keep_serving(&web_entry, &workspaces);
        }
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
        // A device a ticket made takes the work of the server it enters through, when it is one
        // that takes work at all; a server keeps the workspaces placed on it and takes none.
        let member = joined && keeping.is_none() && take_work::TAKES_WORK;
        let port = configured_port()?.or_else(|| member.then_some(take_work::PORT));
        // The person's other devices heard on this device's local network, which this endpoint
        // asks for and answers: what their shell shows as a device nearby, and how two of their
        // devices on one network meet without a relay.
        let lan = super::lan::Lan::new(identity.cert.clone());
        // The endpoint the part that ended bound is handed to the part that starts next, so
        // its socket is the port this device is reached at and is never bound a second time,
        // whatever the part that ended still has to let go of.
        let (endpoint, relay) = match (reused, keeping.as_mut()) {
            // A server binds its own endpoint with the packet relay it runs, so it takes no
            // handover: what the part that ended bound is let go of first, so the address it
            // bound can be bound again with the relay.
            (Some(reused), Some(keeping)) => {
                drop(reused);
                let (endpoint, relay) = keeping.bind(&identity.key, port)?;
                (endpoint, Some(relay))
            }
            (None, Some(keeping)) => {
                let (endpoint, relay) = keeping.bind(&identity.key, port)?;
                (endpoint, Some(relay))
            }
            (Some(endpoint), None) => (endpoint, None),
            (None, None) => (
                listen_endpoint(
                    &identity.key,
                    port,
                    login_relay(&identity.place),
                    Some(&lan),
                )?,
                None,
            ),
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
        let addr_watch = endpoint.watch_addr();
        // The workspaces' Linux work: the Terminal environments of the runtime beside this
        // device, and on a server that pays for their machines, the work on their devices that
        // run Linux, whose Browser its host serves.
        #[cfg(target_os = "linux")]
        let environments = Arc::new(super::environments::Environments::here());
        // The Browser this host places for its workspaces' machines. It is made before the
        // service opens, since what the service answers its devices' app frames is fixed then:
        // an environment asks its host for what it holds no keys for through it, and this
        // device's keys' sessions answer beside it.
        #[cfg(feature = "agents")]
        let browsers = if keeping.is_some() {
            #[cfg(target_os = "linux")]
            let sessions = environments.sessions();
            #[cfg(not(target_os = "linux"))]
            let sessions = None;
            crate::device::keep::hosted_browser::HostedBrowsers::serving(sessions)
                .map(|browsers| browsers.map(Arc::new))
                .map_err(|error| io::Error::other(format!("this host's Browser: {error}")))?
        } else {
            None
        };
        let key_sessions_serving = key_sessions.as_ref().map(|sessions| {
            Arc::clone(sessions) as Arc<dyn tilo_workspace_service::app_serving::AppActionServer>
        });
        #[cfg(feature = "agents")]
        let app_serving = crate::device::keep::hosted_browser::app_serving(
            browsers.clone(),
            key_sessions_serving,
        );
        #[cfg(not(feature = "agents"))]
        let app_serving = key_sessions_serving;
        let workspaces = open_workspaces(
            &identity,
            endpoint,
            Arc::clone(&brokers),
            app_serving.clone(),
            keeping.as_ref().zip(itself.clone()),
        )?;
        // A device of the person's at another peer protocol version is refused by the
        // workspaces here: the person reads which side updates, once for each device and version.
        super::version_notice::watch(&workspaces);
        // The person's record of their devices says where this one answers, so their other
        // devices dial it directly when no relay and no discovery find it: the addresses are
        // the ones the endpoint is bound at, told again whenever a network change moves them.
        {
            let workspaces = Arc::clone(&workspaces);
            tilo_rt::app().handle().spawn(async move {
                use iroh::Watcher as _;
                let mut watch = addr_watch;
                loop {
                    if let Err(error) = workspaces.service().workspaces().say_endpoint().await {
                        tracing::debug!(
                            %error,
                            "the person's record does not say where this device answers"
                        );
                    }
                    if watch.updated().await.is_err() {
                        return;
                    }
                }
            });
        }
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
        // A device placed for records of one workspace reaches it through the host its ticket's
        // login entered through on this start: it keeps no copy to find its host in.
        if let Some(host) = entered
            .as_ref()
            .and_then(|login| super::confined::ConfinedHost::of_login(&identity.cert, login))
        {
            workspaces.confine_to(host);
        }
        workspaces.copies().publish_own(
            tilo_actions::remote::DEVICE_SETTINGS,
            tilo_workspace_service::workspaces::OwnReaders::OwnRecords,
            records,
        );
        // The engine jobs this device runs, each a record of its own device space
        // (`devices/<device>/jobs/<job>`), reported by the process that runs the engine and read
        // through the one record API.
        let jobs = super::jobs::DeviceJobs::new(identity.key.verifying_key().to_bytes());
        jobs.bind(&workspaces);
        if let Ok(processes) = apps.processes() {
            processes.serving_jobs(Arc::clone(&jobs));
        }
        // A server its person's ticket made tells the device its login entered where it answers.
        if let Some(login) = entered.clone().filter(|_| joined && keeping.is_some()) {
            let workspaces = Arc::clone(&workspaces);
            tilo_rt::app().handle().spawn(async move {
                if let Err(error) = workspaces.enter_among_servers(&login).await {
                    tracing::warn!(%error, "this server did not tell where it answers");
                }
            });
        }
        let entering = entered.clone();
        let home = Arc::new(super::home::Home::new(
            Arc::clone(&workspaces),
            identity.cert.clone(),
            entered,
        ));
        // The workspace a ticket login named is entered and kept here now, before this part
        // answers anyone: the person's list and their apps read what the part holds, and a login
        // that answers before its workspace is here leaves both showing none of it.
        if let Some(login) = entering {
            let (entering_with, cert) = (Arc::clone(&workspaces), identity.cert.clone());
            stage("entering the workspace the login named");
            super::workspaces::block_on_owned(async move {
                super::home::entered_while_running(&entering_with, &cert, &login).await
            })
            .unwrap_or_else(
                |why| tracing::warn!(%why, "the workspace this login named is not kept here yet"),
            );
        }
        let take_work = member
            .then(|| take_work::Server::from_login(home.get_blocking().as_ref()))
            .flatten();
        // The person's workspace here: their own on a device a ticket made, else the one this
        // computer founded with their root.
        let first = super::first_workspace::FirstWorkspace::open(
            Arc::clone(&workspaces),
            joined.then_some(workspace_id.as_str()),
        );
        // A server founds and keeps the workspaces its configuration lists of its own, where
        // this computer holds its person's root: founding one is the founder's own device's act,
        // which a server its person's ticket joined is not. Such a server keeps the workspaces
        // placed on it (`servers add`, from a device of its owners), and founds none.
        if let Some(keeping) = keeping.as_ref() {
            let listed = keeping.kept()?;
            if joined && !listed.is_empty() {
                tracing::info!(
                    kept = listed.len(),
                    "this server keeps the workspaces its owners place on it; it founds none"
                );
            } else if !joined && !listed.is_empty() {
                stage("founding the workspaces this server keeps");
                let handle = tilo_rt::app().handle().clone();
                for kept in listed {
                    // The entry the deployment names is the founder's first change: settled as
                    // the workspace is made, never again over what its owners chose since.
                    handle
                        .block_on(workspaces.found_kept(kept.id, &kept.name, kept.entry.as_ref()))
                        .map_err(io::Error::other)?;
                }
            }
        }
        // What a server keeps beyond its copies, on the workspace service it runs.
        let serves_pages = keeping.is_some();
        let kept = if let (Some(keeping), Some(relay), Some(itself)) = (keeping, relay, itself) {
            stage("keeping what this server keeps");
            Some(keeping.keep(super::keep::On {
                dir: &identity.place.dir().join(OWN),
                joined,
                identity: itself,
                service: Arc::clone(workspaces.service()),
                keys: workspaces.keys(),
                endpoint: workspaces.endpoint(),
                relay,
                app_serving,
                #[cfg(feature = "agents")]
                browsers,
            })?)
        } else {
            // A computer that hosts the workspaces its person made on it keeps for their members
            // what a server does.
            let content = crate::device::keep::content::of_device(
                &identity.place.dir().join(OWN),
                app_serving.as_ref(),
            );
            tilo_rt::app().spawn(crate::device::keep::content::keep_for_members(
                workspaces.copies(),
                &content,
                app_serving,
            ));
            None
        };
        let (kept, kept_protocols) = kept.map_or((None, Vec::new()), |(kept, protocols)| {
            (Some(kept), protocols)
        });
        // A server's update turn, as the update actions take it: the workspaces
        // this computer hosts, and where it keeps the image it restarted into.
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
            // The computer that just made the person's root founds their workspace now, in the
            // step that made it, before anyone is told the person is here: the founding call
            // answers once their services run. Any other start founds nothing.
            let person = identity.cert.user_public_key;
            if ROOTS_MADE_HERE.lock_recover().contains(&person) {
                stage("founding the person's workspace");
                let founding = Arc::clone(&first);
                block_on_owned(async move { founding.found().await }).map_err(io::Error::other)?;
                ROOTS_MADE_HERE
                    .lock_recover()
                    .retain(|made| *made != person);
            }
        }
        // What the person granted apps in a workspace, as the service that holds its governance
        // answers: what every launch here hands the app it starts.
        apps.keep_launch_grants(Arc::clone(&workspaces) as Arc<dyn apps::LaunchGrantsSource>);
        // The member an app's code that shows nothing runs as, here: the device's own.
        apps.keep_person(&actor);
        // A workspace's role records decide what this device's person may do in its apps: every
        // process running one is told, as a session's push tells it, though a device that serves
        // pages runs no session.
        follow_app_access(&apps, workspaces.copies());
        workspaces.follow_admitted_writes(Arc::clone(&brokers), Arc::clone(&fanout));
        // The files of the workspaces whose work this device takes, once the broker that carries
        // their records runs: where a member taking a workspace's work runs it.
        let replica_slot = take_work::files::ReplicaSlot::default();
        let taking = take_work.is_some();
        let take_work =
            take_work_action_service(root, &workspaces, take_work, &replica_slot, stage)?;
        let entering = take_work.as_ref().map(take_work::TakesWork::entering);
        // The one action service the daemon's router answers the device actions on: the work
        // this member takes from its server when it takes any, else its workspaces'. A ticket's
        // certify call is admitted on it while a computer asks.
        let actions = take_work.as_ref().map_or_else(
            || workspaces.copies().action_service(),
            take_work::TakesWork::actions,
        );
        // The workspaces this device hosts run their Terminal sessions' shells here: the
        // dispatch answers the records' `run` and `open-terminal` with ush's shells, and the
        // endpoint takes the streams it hands out. A member that takes its server's work runs
        // them for its server instead, and answers none itself.
        // The workspaces' own shells this device runs: what an app's shell on the workspace's own
        // target opens, beside the sessions the dispatch runs.
        #[cfg(target_os = "linux")]
        let mut own_shells = None;
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
                let shells = Arc::new(shells);
                own_shells = Some(Arc::clone(&shells));
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
                let shelves = Arc::clone(&workspaces);
                let terminal = tilo_workspace_service::workspaces::terminal::TerminalService::new(
                    workspaces.copies().clone(),
                    host,
                    tilo_rt::app().handle().clone(),
                )
                .keeping_output_on(Arc::new(move || shelves.content_shelf()));
                workspaces.copies().provide(
                    tilo_workspace_service::workspaces::terminal::PROVIDER,
                    Arc::new(terminal),
                );
                // The workspace's environments are the system's: any app, agent or script
                // reaches them by an ordinary action call, and this answers it by the shell
                // runtime that keeps their records.
                workspaces.copies().provide(
                    super::environment_directory::PROVIDER,
                    Arc::new(super::environment_directory::Environments::here()),
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
                // Where this device enters its person's workspaces, once it knows a server of
                // theirs: a computer a ticket made, and one that founded its person and later
                // gained a server, are both reached through it, and approve a browser's sign-in
                // at the host its ticket names as the device they entered as.
                Some(Arc::clone(&home)),
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
                kept.as_ref().and_then(super::keep::Kept::live_session),
                &fanout,
            )
        };
        // What is the person's alone is kept in their own workspace, read here as it changes and
        // written once this device holds it: their devices, and the answers they gave about this
        // device's camera and microphone, which their other devices see and can take back.
        let person_devices = {
            let _entered = tilo_rt::app().handle().enter();
            workspaces.copies().follow_record(
                &person_workspace::workspace_id(workspaces.copies()),
                devices::DEVICES,
            )
        };
        // The device VPN's peer-to-peer half, when this program mounts one: it dials another of
        // the person's devices and answers their other devices dialling this one. The shell keeps
        // the TUN devices, which only it may open, and hands the packets over its link.
        let vpn = super::capabilities::vpn().map(|vpn| {
            vpn(
                workspaces.endpoint(),
                super::capabilities::person_devices(
                    person_devices.clone(),
                    identity.cert.user_public_key,
                ),
            )
        });
        // What carries a shell's VPN links, kept beside the protocols the endpoint takes.
        let vpn_links = vpn.as_ref().map(|vpn| Arc::clone(&vpn.links));
        // Where this device's member workspaces stand with the devices serving them, published
        // for its person's shell: what an app opening one shows.
        let workspace_sync = {
            let _entered = tilo_rt::app().handle().enter();
            super::workspace_sync::WorkspaceSync::new(workspaces.copies())
        };
        // The person's speakers and remote audio: the daemon holds the endpoint, so it is the one
        // that carries a route's envelopes to the device they are for, and answers the person's
        // other devices' routes on this one. A daemon whose program mounted no fabric carries
        // none of it.
        let speakers = super::capabilities::fabric().map(|fabric| {
            super::speakers::Speakers::new(
                workspaces.endpoint(),
                identity.cert.device_public_key,
                identity.cert.user_public_key,
                workspaces.copies().clone(),
                fabric,
            )
        });
        // The person's devices that hold a session with this one now, published for their shell:
        // what is theirs lasts in the record, and being reachable is this.
        let online = {
            let _entered = tilo_rt::app().handle().enter();
            super::online::Online::new(
                actions.clone(),
                identity.cert.device_public_key,
                identity.cert.user_public_key,
                workspaces.copies().follow_record(
                    &person_workspace::workspace_id(workspaces.copies()),
                    devices::DEVICES,
                ),
            )
        };
        stage("serving the device's endpoint");
        // Another of the person's devices at early boot asks this one for its disk-unlock half:
        // the exchange is the device's own, answered as this device, and the person's yes is an
        // asked record their devices show.
        let unlocking = match asked_store(&workspaces) {
            Ok(store) => Some(super::unlock::serving(&workspaces, store)),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "early-boot unlock is not served: this device keeps no asked calls"
                );
                None
            }
        };
        let endpoint_router = serve_device_endpoint(EndpointServing {
            workspaces: &workspaces,
            take_work,
            actions,
            host: (founded.is_some() || kept.is_some()).then(|| workspaces.service().as_ref()),
            kept: kept_protocols,
            terminals: terminals.into_iter().chain(live.protocols()).collect(),
            unlock: unlocking,
            vpn,
            speakers: speakers.as_ref(),
        });
        // A founding call that asked for the person's first tickets is answered them from here
        // on, once their own workspace is kept in this device's workspace service: the first
        // call of theirs in it reaches that service, and one it does not hold yet is refused
        // rather than waited for. Founding waits on the service saying it kept the workspace;
        // one that cannot happen ends the call with why instead of as done.
        let founding_for_unlock = founding;
        if let Some(founding) = founding {
            let admittance = match founded {
                Some(workspace) => {
                    let copies = Arc::clone(&workspaces);
                    let named = workspace.to_string();
                    block_on_owned(async move {
                        match tokio::time::timeout(BINDING, copies.copies().workspace_kept(&named))
                            .await
                        {
                            Ok(held) => held,
                            Err(_) => Err(format!(
                                "this device's workspace service did not keep your own workspace \
                                 within {} seconds",
                                BINDING.as_secs()
                            )),
                        }
                    })
                }
                None => Ok(()),
            };
            match admittance {
                Ok(()) => founding.serving(identity.cert.user_public_key, Arc::clone(&tickets)),
                Err(why) => founding.failed(format!(
                    "your own workspace is not bound on this computer, so it is not founded: {why}"
                )),
            }
        }
        // Only the devices that record still certifies are heard, and a device the person takes
        // back leaves the local network's answer at once.
        lan.admitting(person_devices.clone());
        // The Wi-Fi passphrases the person's devices share are kept there too, sealed to each of
        // them: the desktop session shares and joins with them through the device's dispatch.
        super::wifi::Wifi::new(
            workspaces.copies().clone(),
            person_devices.clone(),
            identity.key.clone(),
        )
        .register(&workspaces.copies().providers());
        // Nearby Send between the person's devices: the offers and transfers answered over the
        // action service this daemon already serves, staged and kept on the device's content
        // shelf, and read by their shell as the resource it lists.
        let nearby = super::nearby::Nearby::new(
            &workspaces,
            person_devices.clone(),
            identity.place.dir().join("nearby"),
        );
        nearby.register(&workspaces.copies().providers());
        // Clipboard continuity between the person's devices: what their session here copies is
        // shared over the action service this daemon already serves, and what their other devices
        // copied is kept here, where their shell reads it as the copies another device offers.
        let clipboard = super::clipboard::Clipboard::new(&workspaces);
        clipboard.register(&workspaces.copies().providers());
        // A computer that founded its person here makes their workspaces, and makes one for
        // another of their devices that asks it over the same action service.
        if own {
            super::founding_create::FoundingCreate::new(&workspaces)
                .register(&workspaces.copies().providers());
            // It answers for their account too: the keys they approve are certified here.
            super::account_keys::AccountKeys::new().register(&workspaces.copies().providers());
        }
        // The interfaces this computer wakes through are kept there too, and the wakes of the
        // person's devices asked there are answered from here: the packets for a device on one
        // of this computer's networks, and the word that this computer is back.
        super::wake::Wake::start(
            workspaces.copies().clone(),
            person_devices.clone(),
            identity.cert.clone(),
        );
        // The person's other devices drive this one's keyboard and pointer through its relay,
        // admitted by the record of their devices.
        live.follow_person(person_devices.clone());
        apps.device_grants()
            .keep_in(device_answers(workspaces.copies(), &person_devices));
        // The person's devices, as the record of their own workspace holds them, each revocation
        // the root's own signature: what `tilo-identity` admits a device by, and the one copy of it.
        // Each start of the daemon reads them from its own workspaces; a confined device, which
        // keeps none and reads none of its person's records, knows itself alone.
        if confined {
            super::confined::trust_own(devices_trust(), identity.cert.clone());
        } else {
            devices_trust().reading({
                let workspaces = std::sync::Arc::downgrade(&workspaces);
                move || {
                    workspaces
                        .upgrade()
                        .map(|workspaces| super::signing::devices_seen(&workspaces))
                }
            });
        }
        // This device is one of the person's, listed with their others: what every host admits
        // it by, and where a revocation of it is marked. The device whose ticket made this one
        // listed it before it answered the certificate; a device no ticket made, as the one the
        // person was founded on, writes itself once their workspace is here: a workspace this
        // computer founds for them lists it as it founds them
        // ([`super::first_workspace::FirstWorkspace`]), and one that arrives from their desktop
        // lists it as it settles ([`super::host_runs::settling`]). A confined device was listed
        // by the root that certified it, in the same step.
        if !confined {
            let person = identity.cert.user_public_key;
            // A device that founded its person's workspace and one that keeps its member copy
            // alike: a start with neither keeps no record to be listed in, and the founding or
            // arrival lists this device when the workspace is here.
            let own_workspace = WorkspaceId::from(collab_types::person_workspace(&person));
            let (copies, own) = (workspaces.copies().clone(), identity.cert.clone());
            if workspaces.keeps_now(own_workspace)
                && let Err(error) = block_on_owned(async move {
                    use tilo_workspace_service::workspaces::person_workspace;
                    let listed = person_workspace::devices::of(&copies, &person)
                        .await
                        .listed(&person)
                        .iter()
                        .any(|listed| listed.cert.device_public_key == own.device_public_key);
                    if listed {
                        return Ok(());
                    }
                    person_workspace::devices::keep(&copies, &own).await
                })
            {
                tracing::warn!(%error, "this device is not listed with its person's");
            }
        }
        // The keys this computer's agents sign with are certified here, with the root it keeps.
        tilo_app_host::agents::agent_keys::install_grants(std::sync::Arc::new(
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
            Arc::clone(&apps) as Arc<dyn tilo_workspace_search::records::RecordReader>,
            &actor,
        );
        workspaces.keep_search(Arc::clone(&search));
        stage("starting the collaboration broker");
        let shell_links = Arc::default();
        let shell_dispatch: Arc<super::shell::Dispatching> = Arc::default();
        let invocations: Arc<Invocations> = Arc::default();
        // No record of a call keeps what its action takes secret, nor what an action whose
        // declaration says so answers.
        invocations.keep_no_secrets(workspaces.copies().secret_parameters());
        invocations.keep_secret_answers(workspaces.copies().secret_answers());
        invocations.drop_secret_answers();
        // The person's shortcuts, saved into the workspaces this device holds and run here: the
        // same store the trigger runner arms them from, so a computer keeps its shortcuts with no
        // desktop session open on it.
        let shortcuts: Arc<dyn ShortcutLibraryServices> =
            Arc::new(super::shortcuts::Shortcuts::new(
                workspaces.copies().clone(),
                workspaces.device_cert(),
                workspaces.signer(),
            )?);
        serve_events(&workspaces, &shortcuts);
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
                clipboard: Arc::clone(&clipboard),
                online: Arc::clone(&online),
                lan: Arc::clone(&lan),
                workspace_sync: Arc::clone(&workspace_sync),
                shell_links: &shell_links,
                shell_dispatch: &shell_dispatch,
                identity: identity_broker.handover(),
                live: &live,
                vpn: vpn_links,
                speakers: speakers.as_ref(),
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
        // A session holds no workspace's keys: the parts it keeps are sealed here, under the
        // members' secret of their workspace, so the same bytes kept twice are one item.
        let keys = workspaces.keys();
        media.seal_content_with(Arc::new(move |scope: &runtime_fs::blobs::BlobScope| {
            let workspace = scope.workspace().parse().ok()?;
            match keys.get()?.access(workspace).0 {
                tilo_workspace_keys::KeyAccess::Ready(keys) => keys.document_content_secret().ok(),
                _ => None,
            }
        }));
        // The device's record of every invocation it was asked for is what the workspace's
        // dispatch reads and follows; the workspace service writes what a model call cost into
        // it as it answers.
        // The apps and agents the broker admits read a record outside its app through the
        // device's one reader, decided by the workspace the record is in.
        if let Some(broker) = broker.app_action_broker() {
            broker.state().serve_invocations(Arc::clone(&invocations));
            // Another device's jobs are read there, over the same device action service this
            // device already serves: the read goes whole, its wait with it.
            let other_jobs: tilo_workspace_service::workspaces::OtherDeviceReads = {
                let workspaces = Arc::clone(&workspaces);
                let person_workspace = tilo_server_protocol::servers::person_workspace_id(
                    &identity.cert.user_public_key,
                );
                Arc::new(move |space: &str| {
                    let device = tilo_actions::remote::parse_device_record_space(space)?;
                    let addr = iroh::EndpointAddr::new(iroh::EndpointId::from_bytes(&device).ok()?);
                    Some(Arc::new(workspaces.copies().action_service().remote(
                        &workspaces.endpoint(),
                        &addr,
                        &person_workspace,
                    ))
                        as Arc<dyn tilo_actions::remote::ResourceReads>)
                })
            };
            let reads = tilo_workspace_service::workspaces::RecordReads::of_daemon(
                workspaces.copies(),
                broker.state(),
                None,
                Arc::clone(&jobs) as Arc<dyn tilo_workspace_service::workspaces::DeviceJobReads>,
                other_jobs,
            );
            // A job's invocation runs the app's handlers through this same dispatch, is never
            // expired as idle while the job runs, and is cancelled through the daemon.
            jobs.serve_effects(tilo_app_host::collab_app_broker::JobHandlers::new(
                broker.state(),
            ));
            let cancelling = Arc::clone(&jobs);
            broker.state().serve_job_canceller(Arc::new(
                move |invocation: &tilo_actions::InvocationId| {
                    cancelling.cancel_invocation(&invocation.to_string());
                },
            ));
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
                .map(|search| search as Arc<dyn tilo_workspace_records::search::Answers>),
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
        // This device's boot key is enrolled through the same dispatch, asked of the person.
        if let Some(founding) = founding_for_unlock {
            founding.enrolling(Arc::new(super::boot_unlock::BootUnlock {
                device: workspaces.device_cert(),
                tickets: Arc::clone(&tickets),
                invocations: Arc::clone(&invocations),
                asked: Arc::clone(&asked),
                workspaces: Arc::clone(&workspaces),
            }));
        }
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
        // The person's workspace here as the daemon starts is settled now, as one founded or
        // arriving later is: this device listed with their others and, where it keeps their root,
        // marked as the one that does, in one write, and their root-only asks served here
        // ([`super::first_workspace::FirstWorkspace::settle`]). Settling is what marks a root;
        // a settling refused here is tried again by the first call that needs the workspace, and
        // answered to it. Every device follows the calls kept for a root, to take them up or to
        // show how its own ended.
        let copies = workspaces.copies().clone();
        let settling = Arc::clone(&first);
        let following = Arc::downgrade(&asked);
        tilo_rt::app().handle().spawn(async move {
            if let Err(error) = settling.settle().await {
                tracing::warn!(%error, "the person's workspace is not settled as the daemon starts");
            }
            copies.follow_asked_at_root(move |asks| {
                if let Some(asked) = following.upgrade() {
                    asked.kept_changed(asks);
                }
            });
        });
        // What posts an account webhook's deliveries: a webhook is a shortcut of the workspace,
        // and its run posts one event through this provider, from the device that keeps it.
        match tilo_app_host::webhooks::Poster::new(
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
        start_agents(&broker, &workspaces, &tickets, (&asked, &invocations));
        // The apps installed here that keep a long-lived connection: the trigger runner takes
        // each one's turn where this device has what it needs, and runs it while it holds it.
        #[cfg(unix)]
        workspaces.copies().run_connections(
            Arc::clone(&apps) as Arc<dyn tilo_workspace_service::work::HostConnections>
        );
        start_triggers(&broker, &workspaces, &invocations);
        // A device that serves the workspace's pages runs its apps with no session, so nothing
        // but this daemon would carry their notices to the person: it keeps them in the record
        // of their devices, where every device of theirs and every page raises them. A device
        // whose program mounted the platform's wakeups arms the moments its apps land on it.
        #[cfg(not(windows))]
        {
            let alarms = super::capabilities::alarms();
            if alarms.is_none() {
                tracing::info!(
                    "this program mounts no platform wakeups: a moment an app's trigger lands on \
                     this device reaches it as the notice the workspace sends"
                );
            }
            if serves_pages || alarms.is_some() {
                start_notices(&workspaces, &apps, serves_pages, alarms);
            }
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
                // An app whose connection stopped coming up says why in its row.
                cache.watch_app_failures(apps.failures());
                super::packages::follow(&cache, Arc::clone(&workspaces));
                // Builds installed while this start ran are installed in the workspace on a
                // thread of their own.
                drop(
                    cache.install_dev_builds_in(super::dev_builds::DevBuilds::new(
                        Arc::clone(&workspaces),
                        workspace_id.clone(),
                    )),
                );
                workspaces
                    .copies()
                    .provide(super::packages::PROVIDER, cache);
            }
            Err(why) => tracing::warn!(%why, "this device keeps no packages"),
        }
        // Where this device is, read here and never by an app's own process: an app asks this
        // device's action, and is answered only under its declared capability or the person's
        // yes.
        let declaring = Arc::clone(&apps);
        workspaces.copies().provide(
            super::location::PROVIDER,
            Arc::new(super::location::Location::this_device(
                &identity.cert.device_public_key,
                Arc::new(super::location::System),
                Arc::new(move |app: &str| {
                    <crate::apps::Apps as tilo_workspace_service::work::HostConnections>::gives(
                        &declaring,
                        "",
                        app,
                        super::location::CAPABILITY,
                    )
                }),
            )),
        );
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
        stage("publishing the styles the installed packages offer");
        #[cfg(not(windows))]
        let styles = super::styles::publish(&workspaces);
        let daemon = Self {
            _files_tree: files_tree,
            #[cfg(not(windows))]
            _styles: styles,
            policy_origin,
            broker,
            shell_links,
            #[cfg(target_os = "linux")]
            own_shells,
            other_devices: other_devices(&workspaces),
            apps,
            workspaces,
            live: std::sync::OnceLock::new(),
            #[cfg(target_os = "linux")]
            host_asks: std::sync::OnceLock::new(),
            old_secrets: std::sync::atomic::AtomicBool::new(false),
            _identity: identity_broker,
            workspace: workspace_id,
            actor,
            #[cfg(unix)]
            identity_socket,
            blob_socket: sockets.blobs,
            replicas,
            _media: media,
            _endpoint_router: endpoint_router,
            _lan: lan,
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
            entering,
            api: std::sync::OnceLock::new(),
            shell_dispatch: Arc::clone(&shell_dispatch),
            agents: super::serve::Agents::default(),
            connections: Arc::default(),
            place,
            dir: identity.place.dir().to_path_buf(),
            _lock: lock,
            _served: identity.served,
        };
        let _ = daemon.live.set(Arc::clone(&live));
        // A person founded while this daemon serves has their root marked in their devices record
        // and their root-only asks served here from then on, without the person's services
        // starting again: the first person a fresh computer founds and every account after it.
        // A confined device founds nobody.
        if !confined {
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
                    .filter(|cert| cert.user_public_key == person);
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
                    asked.serve_at_root(&serving, (*tickets).clone());
                });
            }));
        }
        // A key its person revokes ends every connection it holds to this daemon at once,
        // told why, rather than being refused by its next call alone.
        let connections = Arc::downgrade(&daemon.connections);
        daemon
            .workspaces
            .copies()
            .on_device_revoked(Arc::new(move |user, device| {
                if let Some(connections) = connections.upgrade() {
                    connections.end_key(user, device);
                }
            }));
        // The person's shell reaches the one dispatch their own runs do, as them.
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
) -> io::Result<Arc<tilo_remote_actions::asked::AskedStore>> {
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

/// Open the device's workspace service in its directory: it hosts the workspace founded here
/// on a computer with no login, and holds the member copies of the login's workspaces otherwise.
/// `apps` is what answers its devices' app frames (`crate::device::keep::hosted_browser::app_serving`).
fn open_workspaces(
    identity: &Identity,
    endpoint: iroh::Endpoint,
    brokers: Arc<Mutex<BrokerChannels>>,
    apps: Option<Arc<dyn tilo_workspace_service::app_serving::AppActionServer>>,
    keeping: Option<(
        &super::keep::Keeping,
        crate::device::keep::server::ServerIdentity,
    )>,
) -> io::Result<Arc<ServiceWorkspaces>> {
    let device = tilo_identity::identity_store::DeviceIdentity::held(
        identity.cert.clone(),
        identity.key.clone(),
        Arc::new(tilo_identity::identity_store::files::Files::new(
            identity.place.keys().clone(),
        )),
    );
    // The evidence of the calls this device's apps have admitted is signed as this device: the
    // certificate its identity holds and the key it took. Named here, so nothing it signs reads
    // the identity store of the account the daemon happens to run as.
    tilo_app_host::serve_receipts_as(
        identity.cert.clone(),
        Arc::new(identity.key.clone()) as Arc<dyn collab_types::signer::DeviceSigner>,
    );
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
    keys: &'a tilo_app_host::workspace_keys::KeyProvider,
    creation: Arc<RecordCreation>,
    /// The files daemons mounting this device's workspaces, whose stores keep their files' bytes.
    files: Arc<tilo_workspace_service::files::socket::Daemons>,
    /// Nearby Send's offers and transfers, which this device's shell reads and its person
    /// answers.
    nearby: Arc<super::nearby::Nearby>,
    /// The copies the person's other devices offer, which this device's shell reads.
    clipboard: Arc<super::clipboard::Clipboard>,
    /// The person's devices that hold a session with this one now, which their shell reads.
    online: Arc<super::online::Online>,
    /// The person's other devices heard on this device's local network, which their shell reads.
    lan: Arc<super::lan::Lan>,
    /// Where this device's member workspaces stand, which their shell reads for the apps in them.
    workspace_sync: Arc<super::workspace_sync::WorkspaceSync>,
    /// The links of the shells this daemon starts.
    shell_links: &'a Arc<tilo_app_host::shell::ShellLinks>,
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
    /// What carries those shells' device-VPN links.
    vpn: Option<tilo_app_host::shell::DeviceVpnLinks>,
    /// The person's speakers, which carries those shells' links to the person's other devices,
    /// where this daemon's program mounted the fabric they travel on.
    speakers: Option<&'a Arc<super::speakers::Speakers>>,
    /// The broker's fanout, which serves the broker the windows connect to: what the daemon
    /// publishes what those windows hold of.
    fanout: &'a Arc<FanoutState>,
}

/// What carries a shell's speakers links: the person's speakers, for as long as each link lasts.
fn speaker_links(speakers: &Arc<super::speakers::Speakers>) -> tilo_app_host::shell::SpeakersLinks {
    let speakers = Arc::clone(speakers);
    Arc::new(move |stream| {
        let speakers = Arc::clone(&speakers);
        Box::pin(async move { speakers.serve_link(stream).await })
    })
}

/// What carries a shell's live links: `live`, for as long as each link lasts.
fn live_links(live: &Arc<super::live_relay::Live>) -> tilo_app_host::shell::LiveLinks {
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
) -> io::Result<(tilo_app_host::policy_clock::PolicyOrigin, BrokerHandle)> {
    let (policy_origin, policy_time) = execution_policy_clock()?;
    // A check's commands are ush's, which runs only on Linux: elsewhere a host approves none.
    #[cfg(target_os = "linux")]
    approved_checks(&parts)?;
    // The workspace's own folder: a file put into it and Files' verbs on it, carried out where
    // this device keeps the tree.
    Arc::new(super::local_files::LocalFiles::new(
        Arc::clone(parts.workspaces) as Arc<dyn super::local_files::Folders>,
    ))
    .register(&parts.workspaces.copies().providers());
    let broker = tilo_app_host::collab_broker::spawn_with_services(
        parts.sockets.collab.clone(),
        Some(Arc::new(parts.workspaces.copies().clone()) as Arc<dyn WorkspaceAuthority>),
        PeerPublisher::with_commits(ServiceCommits::new(parts.workspaces)
            as Arc<dyn tilo_app_host::workspace_commits::WorkspaceCommits>),
        None,
        broker_services(parts, policy_time),
        channels,
        fanout,
    )?;
    Ok((policy_origin, broker))
}

/// What this device publishes to the apps attached to it, and to the person's shell: the seat
/// that serves the desktop session's services, and the two bundles of what this device keeps.
struct PublishedHere {
    seat: tilo_app_host::seat::Seat,
    forwarded: tilo_app_host::seat::Forwarders,
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
    // This computer's journal, which the session's system service serves to the shell and which
    // the daemon serves here when no session does: one read, wherever it is served from.
    let journal: Arc<dyn PublishedResources> = Arc::new(super::journal::Published::new());
    // What only the desktop session signed in here holds (the screen and its windows, the sound,
    // capture, the person's settings, Nearby Send's spool, the speaker groups),
    // reached through the seat its shell serves: while none does, those calls say so.
    let seat = tilo_app_host::seat::Seat::new(
        parts.workspaces.copies().providers(),
        tilo_rt::app().handle().clone(),
    );
    let forwarded = seat.forwarders();
    // The session's roles that follow records hear of every record the workspaces took a change
    // of, as the store commits it.
    tilo_app_host::seat::tell_record_changes(
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
    // The copies the person's other devices offer, read by their shell over the shell link.
    let clipboard: Arc<dyn PublishedResources> = Arc::new(parts.clipboard.published());
    // The person's devices that hold a session with this one now, read by their shell: what says
    // which of their devices it reaches at this moment.
    let online: Arc<dyn PublishedResources> = Arc::new(parts.online.published());
    // The person's other devices heard on this device's local network now, read by their shell.
    let lan: Arc<dyn PublishedResources> = Arc::new(parts.lan.published());
    // Where this device's member workspaces stand, read by their shell for the apps in them.
    let workspace_sync: Arc<dyn PublishedResources> = Arc::new(parts.workspace_sync.published());
    // What is live in the workspaces this device keeps, which the desktop shows.
    let live: Arc<dyn PublishedResources> = super::live::Published::of(parts.workspaces.copies());
    // The record each window this device runs holds, which its desktop notes beside the window.
    let window_notes: Arc<dyn PublishedResources> =
        super::window_notes::Published::of(parts.fanout);
    // What a person copies from the vault is put on their session's clipboard from here, so no
    // app ever holds it.
    parts
        .workspaces
        .people()
        .copy_secrets_to(Arc::new(super::secret_clipboard::SessionClipboard));
    // The code of this moment a vault item's seed makes, which an app that shows one-time codes
    // follows as the item's reading: read here, where the seed is.
    let vault_codes: Arc<dyn PublishedResources> =
        Arc::new(tilo_app_host::vault_actions::VaultPublished::new(
            parts.workspaces.copies().clone(),
            tilo_rt::app().handle().clone(),
        ));
    // The workspace's machine environments, read where the workspace keeps their records and
    // made again whenever one of them changes.
    #[cfg(target_os = "linux")]
    let environments: Arc<dyn PublishedResources> =
        super::environment_readings::published(parts.workspaces);
    // What this device's daemon keeps itself, each resource served by the provider that keeps it.
    let kept_here = || {
        #[cfg(target_os = "linux")]
        let kept = super::published::Published::new().serving(
            &[tilo_action_services::environment_commands::ENVIRONMENTS],
            Arc::clone(&environments),
        );
        #[cfg(not(target_os = "linux"))]
        let kept = super::published::Published::new();
        kept.serving(
            &[super::server_settings::SERVER],
            Arc::clone(&server_settings),
        )
        .serving(
            &[tilo_system_resources::journal::JOURNAL],
            Arc::clone(&journal),
        )
        .serving(&[super::nearby::RESOURCE], Arc::clone(&nearby))
        .serving(&[super::clipboard::RESOURCE], Arc::clone(&clipboard))
        .serving(&[super::online::RESOURCE], Arc::clone(&online))
        .serving(
            &[tilo_workspace_service::workspaces::person_workspace::devices::NEARBY],
            Arc::clone(&lan),
        )
        .serving(
            &[tilo_workspace_service::workspaces::sync::RESOURCE],
            Arc::clone(&workspace_sync),
        )
        .serving(&[tilo_workspace_service::live::RESOURCE], Arc::clone(&live))
        .serving(
            &[tilo_app_host::window_notes::RESOURCE],
            Arc::clone(&window_notes),
        )
        .serving(&[super::mcp_server::SERVER], Arc::clone(&mcp_server))
        .serving(
            &[tilo_workspace_records::models::RESOURCE],
            Arc::clone(&models),
        )
        .serving_prefix("vault.item:", Arc::clone(&vault_codes))
        .serving(
            &[tilo_workspace_records::accounts::RESOURCE],
            Arc::clone(&accounts),
        )
    };
    // What the person's shell reads and changes over its link: what the daemon keeps, never what
    // the seat publishes. The seat is the shell's own, answered on its loop; a read the shell's
    // loop waits for that asked the seat first would wait on that same loop, and the session
    // would never start.
    let shell: Arc<dyn PublishedResources> = Arc::new(kept_here());
    let apps: Arc<dyn PublishedResources> = Arc::new(kept_here().serving_the_rest(Arc::new(
        tilo_app_host::seat::SeatPublished::new(seat.clone()),
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
/// check's commands are ush's own, which runs only on Linux. An app runs one as a
/// step its own `run-approved-check` declares, for an invocation the dispatch delivered to it:
/// that invocation names the project the check is for.
#[cfg(target_os = "linux")]
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
        tilo_action_services::approved_checks::PROVIDER,
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

/// The seat's services, as the broker runs them: what the desktop session serves, the app windows
/// it opens, and the agents it admitted.
fn seat_services(
    services: &mut ProcessServices,
    parts: &BrokerParts<'_>,
    seat: tilo_app_host::seat::Seat,
    forwarded: tilo_app_host::seat::Forwarders,
) {
    services.audio_output = Some(forwarded.audio_output);
    // A page's view's call is the browser's that shows it; the seat carries every other.
    services.call_media = Some(match parts.key_sessions {
        Some(sessions) => Arc::new(super::page_media::PageMedia {
            pages: sessions.pages_served().clone(),
            seat: forwarded.call_media,
        })
            as Arc<dyn tilo_app_host::collab_app_broker::CallMediaServices>,
        None => forwarded.call_media,
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
    services.launcher = Some(Arc::new(tilo_app_host::seat::SeatLauncher::new(
        seat,
        Arc::clone(parts.apps) as Arc<dyn LauncherServices>,
    )));
    // The agents each workspace's own record names, read here, so an agent is admitted with
    // or without a desktop session; what each spent, and the agent sessions the person
    // admitted, are the desktop session's, which the daemon hands each agent's connection.
    services.agents = Some(Arc::new(super::workspaces::RecordedAgents::new(
        parts.workspaces,
        Arc::clone(&forwarded.agents),
    )));
    services.agent_sessions =
        Some(forwarded.agents as Arc<dyn tilo_broker_core::services::AgentSessions>);
}

/// The records the broker reads for the apps it serves: the agents and sessions, the documents
/// they work on, and the workspaces' people.
fn record_services(
    services: &mut ProcessServices,
    parts: &BrokerParts<'_>,
    policy_time: tilo_app_host::policy_clock::PolicyTime,
) {
    // The records of the workspaces this device keeps, read for the apps, agents and programs
    // it serves: each capture opens the record's document here, and counts it.
    services.committed_documents = Some(Arc::new(tilo_app_host::service_reads::ServiceReads::new(
        parts.workspaces.copies().clone(),
        parts.workspaces.service().metrics_counter(),
    ))
        as Arc<dyn tilo_app_host::services::NativeDocumentReads>);
    // The device's one store, which the apps it serves write and read their resources in.
    services.resources = Some(Arc::new(
        tilo_app_host::service_resources::ServiceResources::of(parts.workspaces.service()),
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
            Arc::new(parts.workspaces.copies().clone())
                as tilo_app_host::services::WorkspaceAuthorityHandle,
        ) as Arc<dyn tilo_app_host::services::WorkspaceMemberships>,
    );
    services.journal_root = Some(parts.root.join(INVOCATION_JOURNAL));
    // What a caller puts into a workspace here is kept on the shelf its records' content is.
    services.content_store = parts
        .workspaces
        .content_shelf()
        .map(tilo_workspace_service::content::shelf_puts);
    // A `files#open` given content puts the files into the workspace's own folder, where this
    // device holds its tree.
    services.files_open = parts.workspaces.content_shelf().map(|shelf| {
        Arc::new(tilo_workspace_service::files::OpenedFiles::new(
            parts.workspaces.copies().clone(),
            shelf,
        )) as Arc<dyn tilo_app_host::services::FilesTreePuts>
    });
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
    // Its windows' instances are handed to the device's tilolang processes on the native
    // endpoint, where the kernel names the account handing them over.
    if let Some(processes) = shell_processes(parts.apps, parts.workspaces) {
        parts.shell_links.serve_processes(processes);
    }
    services.shell = Some(Arc::new(super::shell::services(
        parts.workspaces,
        parts.shell_links,
        parts.sockets.native.clone(),
        Arc::clone(parts.shell_dispatch),
        shell_published,
        super::shell::Serving {
            identity: Arc::clone(&parts.identity),
            live: Some(live_links(parts.live)),
            vpn: parts.vpn.clone(),
            speakers: parts.speakers.map(speaker_links),
            speaking: Arc::clone(parts.live) as Arc<dyn tilo_app_host::shell::ShellSpeaking>,
            launcher: Arc::new(crate::apps::DeviceLauncher(Arc::clone(parts.apps))),
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
    policy_time: tilo_app_host::policy_clock::PolicyTime,
) -> ProcessServices {
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
        tilo_action_services::offload_place::PROVIDER,
        tilo_action_services::offload_place::PROVIDER,
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
    let hosts = super::hosts::Hosts::placed(parts.workspaces);
    parts.workspaces.hold_hosts(Arc::clone(&hosts));
    let PublishedHere {
        seat,
        forwarded,
        shell: shell_published,
        apps: published,
    } = published_here(&parts);
    let mut services = ProcessServices {
        action_offload,
        document_content: Some(document_content),
        ..ProcessServices::default()
    };
    seat_services(&mut services, &parts, seat, forwarded);
    record_services(&mut services, &parts, policy_time);
    shell_services(&mut services, parts, &published, shell_published, &hosts);
    services
}

/// The device's tilolang processes, as the person's shell hands them its windows' instances on
/// its link: for the workspaces this device has for them alone. `None` where no process can start.
fn shell_processes(
    apps: &Arc<apps::Apps>,
    workspaces: &Arc<ServiceWorkspaces>,
) -> Option<tilo_app_host::shell::ShellProcesses> {
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
    fn content(&self, workspace: &str) -> futures_util::future::BoxFuture<'static, Option<String>> {
        let workspaces = self.0.clone();
        let workspace = workspace.to_owned();
        Box::pin(async move {
            let reader = workspaces.device_person();
            let reading = workspaces.content_reading(&workspace, reader).await;
            serde_json::to_string(&reading).ok()
        })
    }
}

/// Tell each of this device's tilolang processes what its app's person may now do, as a
/// workspace's role records change: a device that serves the workspace's pages runs no session
/// to tell them, so its own service reads the policy as the session's push reads it.
fn follow_app_access(
    apps: &Arc<apps::Apps>,
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
) {
    let mut governance = workspaces.governance_changes();
    let apps = Arc::downgrade(apps);
    tilo_rt::app().handle().spawn(async move {
        loop {
            let change = governance.recv().await;
            let Some(apps) = apps.upgrade() else {
                return;
            };
            match change {
                Ok(workspace) => apps.access_changed(&workspace),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    apps.access_changed_everywhere();
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    });
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
                            processes.content_refresh_all().await;
                        }
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
                change = governance.recv() => match change {
                    Ok(workspace) => workspace,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        if let Some(processes) = processes.upgrade() {
                            processes.content_refresh_all().await;
                        }
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
            };
            let Some(host) = processes.upgrade() else {
                return;
            };
            host.content_changed(&workspace).await;
        }
    });
}

fn execution_policy_clock() -> io::Result<(
    tilo_app_host::policy_clock::PolicyOrigin,
    tilo_app_host::policy_clock::PolicyTime,
)> {
    let origin = tilo_app_host::policy_clock::OwnerPolicyClock::new()
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
        founding: &Arc<super::unclaimed::Founding>,
        reused: Option<iroh::Endpoint>,
    ) -> io::Result<Self> {
        let serving = if dev { Serving::Dev } else { Serving::Profile };
        Self::start(
            root,
            stage,
            serving,
            (Some(device), entered),
            Some(founding),
            reused,
        )
    }
}

impl RuntimeServices for Daemon {
    type ActionProvider = RuntimeActionProvider;

    fn start_reporting(root: &Path, stage: &mut dyn FnMut(&str)) -> io::Result<Self> {
        Self::start(root, stage, Serving::Profile, (None, None), None, None)
    }

    fn start_in_dev_mode(root: &Path, stage: &mut dyn FnMut(&str)) -> io::Result<Self> {
        Self::start(root, stage, Serving::Dev, (None, None), None, None)
    }

    /// The person's other devices, as this device asks them.
    fn other_devices(&self) -> Option<crate::other_devices::OtherDevices> {
        self.other_devices.clone()
    }

    /// The person's workspace here, or why this computer has none.
    fn workspace(&self) -> io::Result<String> {
        self.first.here_blocking().map_err(io::Error::other)
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
        // the first app launched in it settles it then, so the device is listed in it before the
        // app signs anything.
        self.first
            .settle_if_theirs(&self.workspace)
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

    /// The dispatch a turn this device's agent engine carries out calls through: the broker's
    /// host, which admits the turn by the agent's own key and the roster entry it was bound to,
    /// and decides each call by what the workspace grants that agent. The person's own dispatch
    /// ([`Self::action_provider`]) admits the person's calls, never an agent's.
    ///
    /// # Errors
    /// The device's action broker is not up.
    pub fn turn_action_provider(
        &self,
    ) -> io::Result<Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync>> {
        self.broker
            .app_action_broker()
            .map(|broker| broker.device_host())
            .ok_or_else(|| io::Error::other("the action broker is unavailable"))
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
            asks: false,
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
    policy_time: tilo_app_host::policy_clock::PolicyTime,
    tickets: Arc<Tickets>,
    invocations: Arc<Invocations>,
    /// The calls the device took before this person's services ran, which they answer too.
    kept: super::unclaimed::Calls,
    asked: Arc<super::asked::DeviceAsked>,
    /// The person's own workspace here, settled the first time a call of theirs reaches it.
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

    /// The dispatch the calls of `agent` in `workspace` reach: an agent the person admitted on
    /// their shell, under the key this device made it there, which the shell names as each call's
    /// caller. Its calls are a run of the person's admission delegated to that agent, its key
    /// brought into the workspace as every agent key of the person's is, and decided by what both
    /// the agent and the person may do there.
    ///
    /// # Errors
    /// The workspace does not admit the person, `agent` is no agent this device made a key for,
    /// or the workspace does not let its key in.
    pub(super) fn agent_provider(
        &self,
        workspace: &str,
        agent: &Caller,
    ) -> io::Result<RuntimeActionProvider> {
        let id = agent
            .acting()
            .agent_id()
            .ok_or_else(|| io::Error::other("the shell names no agent"))?
            .to_owned();
        let name = agent.agent_called().unwrap_or(&id).to_owned();
        let person = self.provider(workspace)?;
        let admission = person
            .admission()
            .ok_or_else(|| io::Error::other("this dispatch admitted no run to delegate"))?;
        let (run, _, _, _) =
            person.agent_session_as(admission, &self.policy_time, |_| (id, name), Some(agent))?;
        Ok(run)
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
        // A person's first call in their own workspace settles it here.
        self.first
            .settle_if_theirs(workspace)
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
        Arc::new(move |asked| {
            let daemon = daemon
                .upgrade()
                .ok_or_else(|| io::Error::other("this computer's daemon is stopping"))?;
            let (provider, caller) = daemon.key_tree(&key, &cert, asked)?;
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

    /// A run of the key `key` in `workspace`: its calls are the key's, carried beside its person,
    /// admitted by the certificate its person gave it and decided by what they may do there,
    /// narrowed by the key's scope ([`ServiceWorkspaces::admit_key`]).
    ///
    /// For a caller on a thread of its own: one on the app runtime's thread awaits
    /// [`Self::admit_key`] and builds the run with [`Self::key_run`].
    fn provider_for_key(
        &self,
        workspace: &str,
        key: &tilo_api::grant_key::GrantKey,
    ) -> io::Result<RuntimeActionProvider> {
        let admission = block_on_owned(self.admit_key(workspace, key))?;
        self.key_run(workspace, admission)
    }

    /// Admit `key` in `workspace` ([`ServiceWorkspaces::admit_key`]), which waits on the
    /// workspaces.
    fn admit_key(
        &self,
        workspace: &str,
        key: &tilo_api::grant_key::GrantKey,
    ) -> impl std::future::Future<
        Output = io::Result<Arc<tilo_workspace_service::workspaces::SessionAdmission>>,
    > + Send
    + 'static {
        let workspaces = Arc::clone(&self.workspaces);
        let (named, signing, cert) = (workspace.to_owned(), key.key().clone(), key.cert().clone());
        async move {
            workspaces
                .admit_key(&named, &signing, &cert)
                .await
                .map_err(io::Error::other)
        }
    }

    /// The run of a key the workspace admitted as `admission`.
    fn key_run(
        &self,
        workspace: &str,
        admission: Arc<tilo_workspace_service::workspaces::SessionAdmission>,
    ) -> io::Result<RuntimeActionProvider> {
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
    #[expect(clippy::unused_self, reason = "the one call the Linux daemon answers")]
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

    fn not_here(&self, key: &DeviceCert) -> Vec<tilo_api::session::Workspace> {
        self.daemon()
            .map(|daemon| tilo_api::session::Here::not_here(&*daemon, key))
            .unwrap_or_default()
    }

    fn enter(
        &self,
        workspace: &str,
        key: &tilo_api::grant_key::GrantKey,
    ) -> tilo_api::session::Asked<'_, tilo_api::session::Reached> {
        let (daemon, workspace, key) = (self.daemon(), workspace.to_owned(), key.clone());
        Box::pin(async move {
            let daemon = daemon?;
            tilo_api::session::Here::enter(&*daemon, &workspace, &key).await
        })
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

    fn join_entry(
        &self,
        workspace: &str,
        host: tilo_server_protocol::web::PageHost,
    ) -> tilo_api::session::Asked<'_, String> {
        let (daemon, workspace) = (self.daemon(), workspace.to_owned());
        Box::pin(async move {
            let daemon = daemon?;
            tilo_api::session::Here::join_entry(&*daemon, &workspace, host).await
        })
    }

    fn workspaces_changed(&self) -> Option<Pending<()>> {
        tilo_api::session::Here::workspaces_changed(&*self.daemon().ok()?)
    }

    fn invocations(&self) -> Option<Arc<Invocations>> {
        tilo_api::session::Here::invocations(&*self.daemon().ok()?)
    }

    fn import(&self, by: &Caller, carried: serde_json::Value) -> tilo_api::session::Imported {
        let (daemon, by) = (self.daemon(), by.clone());
        Box::pin(async move {
            let daemon = daemon?;
            tilo_api::session::Here::import(&*daemon, &by, carried).await
        })
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

    fn create_where_founded(
        &self,
        creating: tilo_api::session::Creating,
    ) -> tilo_api::session::FoundingAnswer<'_> {
        let daemon = self.daemon();
        Box::pin(async move {
            let daemon = daemon.map_err(tilo_api::session::NotFounded::Refused)?;
            tilo_api::session::Here::create_where_founded(&*daemon, creating).await
        })
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
                described_workspace(&id, workspace.name, &self.workspaces)
            })
            .collect();
        // The one this computer enters through first: what a command that names none acts in.
        kept.sort_by_key(|workspace| workspace.id != self.workspace);
        kept
    }

    fn not_here(&self, key: &DeviceCert) -> Vec<tilo_api::session::Workspace> {
        if hex::encode(key.user_public_key) != self.actor {
            return Vec::new();
        }
        self.workspaces
            .copies()
            .not_here()
            .into_iter()
            .map(|missing| tilo_api::session::Workspace {
                kept_here: false,
                not_here: Some(missing.why),
                ..tilo_api::session::held(missing.workspace, missing.name, None)
            })
            .collect()
    }

    fn enter(
        &self,
        workspace: &str,
        key: &tilo_api::grant_key::GrantKey,
    ) -> tilo_api::session::Asked<'_, tilo_api::session::Reached> {
        let workspace = workspace.to_owned();
        let mine = hex::encode(key.cert().user_public_key) == self.actor;
        // Admitting the key waits on the workspaces: awaited, as the programs API this answers
        // runs on the app runtime's thread.
        let admitted = self.admit_key(&workspace, key);
        Box::pin(async move {
            if !mine {
                return Err("this key is not of this computer's person".to_owned());
            }
            let admission = admitted.await.map_err(|error| error.to_string())?;
            let provider = self
                .key_run(&workspace, admission)
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
            // One of the person's list is kept here by showing it here again: this device's
            // follower of the list takes it in.
            let copies = self.workspaces.copies();
            copies.show_here(&workspace).await?;
            copies
                .held_within(
                    &workspace,
                    tilo_workspace_service::workspaces::servers::keeping::JOINED_WITHIN,
                )
                .await
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
            // The person leaves it: it goes off their list, and every device of theirs lets it
            // go; this one at once.
            let copies = self.workspaces.copies();
            copies.leave_listed(&workspace).await?;
            copies.release(&workspace).await
        })
    }

    fn join(&self, invite: &str) -> tilo_api::session::Asked<'_, String> {
        let invite = invite.trim().to_owned();
        Box::pin(async move {
            Box::pin(
                self.workspaces
                    .copies()
                    .join_member_as(&invite, person_name().as_deref()),
            )
            .await
            .map(|workspace| workspace.to_string())
        })
    }

    /// This computer's own key signs the admission the entry takes it in by: the join happens
    /// here, in the process that holds that key.
    fn join_entry(
        &self,
        workspace: &str,
        host: tilo_server_protocol::web::PageHost,
    ) -> tilo_api::session::Asked<'_, String> {
        let workspace = workspace.trim().to_owned();
        Box::pin(async move {
            let id = WorkspaceId::parse(&workspace)
                .ok_or_else(|| format!("{workspace} names no workspace"))?;
            Box::pin(
                self.workspaces
                    .copies()
                    .join_entry(id, &host, person_name().as_deref()),
            )
            .await
            .map(|workspace| workspace.to_string())
        })
    }

    fn workspaces_changed(&self) -> Option<Pending<()>> {
        let held = self.workspaces.entered();
        let mut changes = self.workspaces.copies().governance_changes();
        let mut missing = self.workspaces.copies().not_here_changed();
        let (done, answer) = tilo_actions::remote::pending();
        tilo_rt::app().handle().spawn(async move {
            tokio::select! {
                () = held.notified() => {}
                _ = changes.recv() => {}
                _ = missing.changed() => {}
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
            first.moved_in(&moved);
            Ok(imported)
        })
    }

    /// Asked of the person's device that founds their workspaces, in their own workspace, which
    /// both keep: this device signs the ask as itself. A computer with no login that holds no
    /// copy of the person's workspace reaches none of their devices: it says how to add it.
    fn create_where_founded(
        &self,
        creating: tilo_api::session::Creating,
    ) -> tilo_api::session::FoundingAnswer<'_> {
        let workspaces = Arc::clone(&self.workspaces);
        let held = self.first.get().is_some();
        Box::pin(async move {
            if !held {
                return Err(tilo_api::session::NotFounded::Refused(
                    super::first_workspace::not_held(),
                ));
            }
            super::founding_create::ask(&workspaces, creating).await
        })
    }
}

impl Daemon {
    /// Whether this computer keeps its person's account itself: the one that founded their
    /// workspace with their root, and holds it, answers for them. A computer that logged in
    /// reaches the account through the device it entered from.
    pub(crate) fn keeps_its_own_account(&self) -> bool {
        self.first.keeps_account()
    }

    /// The daemon as its API reaches it, without keeping it: what a key's connection answers
    /// through, as the daemon's own listeners do. A `Daemon` cannot be handed as itself: its
    /// account is one only while the daemon is held, so the API reaches it through the weak
    /// handle [`DaemonHere`] keeps.
    #[must_use]
    pub fn api_here(self: &Arc<Self>) -> Arc<dyn tilo_api::session::Here> {
        Arc::new(DaemonHere(Arc::downgrade(self)))
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

    /// Ask `request` of `workspace` as `key`: the workspace's own dispatch answers it for the
    /// device that keeps it, as it answers a page's ask of the same model, and keeps what waits
    /// for the person as their question. `None` for a key this device does not know, or a
    /// workspace it does not admit the key to.
    fn ask(
        &self,
        key: GrantKey,
        workspace: String,
        request: InvokeRequest,
    ) -> AccountAnswer<tilo_api::session::AskedModel> {
        let workspaces = Arc::clone(&self.workspaces);
        let device = self.cert.clone();
        Box::pin(async move {
            known(&device, &key).ok()?;
            // The key's run, entered as its session enters it: the model the workspace keeps is
            // asked by the caller the workspace admits for that key.
            let admission = workspaces
                .admit_key(&workspace, key.key(), key.cert())
                .await
                .ok()?;
            // The workspace's own dispatch answers its models for its members' devices, as it
            // does for this device's own calls.
            let answers: tilo_api::session::Provider = workspaces.copies().device_actions();
            let caller = admission.caller().clone();
            let receipt = answers.invoke(&caller, request).await?.ok()?;
            Some((receipt, answers, caller))
        })
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
            create_here(&workspaces, key.cert(), &device, (name, labels, id)).await
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
            let branch = id.unwrap_or_else(Uuid::new_v4).to_string();
            let made = branch_here(
                &workspaces,
                key.cert(),
                &device,
                source,
                name,
                branch,
                labels,
            )
            .await?;
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
            if workspaces.copies().hosted_copy(id).await.is_none() {
                return Err(ServerError::Refused(
                    "this device does not keep that workspace".into(),
                ));
            }
            enter_the_key(&workspaces, &workspace, &key).await;
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
            delete_here(&workspaces, key.cert(), &device, &workspace).await
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

/// Make a workspace of `device`'s person's own called `name`, labelled `labels`, under `id` when
/// the caller chose one, on this device: `acting` asks the workspace service for it, and `device`
/// signs its way into what was made. What a key of the person's asks here, and what another of
/// their devices asks this one for ([`super::founding_create`]), are made alike.
///
/// # Errors
/// The service did not make it, or the device could not enter it.
pub(crate) async fn create_here(
    workspaces: &ServiceWorkspaces,
    acting: &DeviceCert,
    device: &DeviceCert,
    (name, labels, id): (String, WorkspaceLabels, Option<Uuid>),
) -> Result<tilo_api::session::Workspace, ServerError> {
    let mut arguments = Map::new();
    arguments.insert("name".into(), json!(name));
    // An id the caller chose, so a retried create asks for the same workspace. Without one the
    // host chooses, and a retry with the same labels answers the workspace they labelled rather
    // than another beside it.
    if let Some(id) = id {
        arguments.insert("workspace".into(), json!(id.to_string()));
    }
    if !labels.is_empty() {
        arguments.insert("labels".into(), json!(labels));
    }
    let created: CreatedWorkspace = answer(
        workspaces
            .service()
            .ask_own(acting, CREATE_OWN_WORKSPACE, arguments)
            .await,
    )?;
    enter_here(workspaces, &created.workspace.workspace, device).await?;
    Ok(listed_workspace(created.workspace, workspaces))
}

/// Make `branch`, a branch of `source` called `name`, one of `device`'s person's own, on this
/// device: `acting` asks the workspace service for it, and `device` signs its way into the branch
/// made. Answers the branch the service made.
///
/// # Errors
/// The source is not one the device keeps for its person, the service could not copy it whole, or
/// the device could not enter the branch.
pub(crate) async fn branch_here(
    workspaces: &ServiceWorkspaces,
    acting: &DeviceCert,
    device: &DeviceCert,
    source: String,
    name: String,
    branch: String,
    labels: WorkspaceLabels,
) -> Result<CreatedWorkspace, ServerError> {
    // A workspace this device keeps for its person is theirs, however it was made: a branch is
    // only ever of one of theirs.
    let _ = workspaces
        .copies()
        .claim_own(&device.user_public_key, &source)
        .await;
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
            .ask_own(acting, BRANCH_OWN_WORKSPACE, arguments)
            .await,
    )?;
    enter_here(workspaces, &made.workspace.workspace, device).await?;
    Ok(made)
}

/// Delete `workspace`, one of `device`'s person's own, on this device: `acting` asks the
/// workspace service for it.
///
/// # Errors
/// The device keeps no workspace of that id for its person, or the service did not delete it.
pub(crate) async fn delete_here(
    workspaces: &ServiceWorkspaces,
    acting: &DeviceCert,
    device: &DeviceCert,
    workspace: &str,
) -> Result<(), ServerError> {
    let _ = workspaces
        .copies()
        .claim_own(&device.user_public_key, workspace)
        .await;
    // A workspace the device does not keep for its person is not the key's to delete, and one it
    // deleted before is no such workspace either.
    if !workspaces.copies().owns(&device.user_public_key, workspace) {
        return Err(ServerError::NotFound);
    }
    let mut arguments = Map::new();
    arguments.insert("workspace".into(), json!(workspace));
    workspaces
        .service()
        .ask_own(acting, DELETE_WORKSPACE, arguments)
        .await
        .map(drop)
        .map_err(unavailable)
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

/// The workspace `id` called `name`, with what this device keeps of it and its person's role.
fn described_workspace(
    id: &str,
    name: String,
    workspaces: &ServiceWorkspaces,
) -> tilo_api::session::Workspace {
    let person = hex::encode(workspaces.device_cert().user_public_key);
    let role = WorkspaceId::parse(id)
        .and_then(|held| workspaces.workspace(held)?.policy.role_of(&person))
        .map(tilo_action_policy::Role::key);
    let mut workspace = tilo_api::session::held(id.to_owned(), name, role);
    workspace.labels = workspaces.labels_of(id);
    workspace.unattended = workspaces.runs_unattended(id);
    workspace
}

/// A key of the person's enters the workspace it asks of here, on its first call of any kind: a
/// key issued for a program comes in with no ticket, so this device, which keeps the workspace,
/// signs the key's own row in as every key's enters (`Workspaces::enter`), and the workspace's
/// own certificates admit it from then on. A key the workspace revoked or removed enters
/// nothing, and the call it made is refused as it was; one asked of a workspace this device does
/// not keep is left to the host that does.
async fn enter_the_key(workspaces: &ServiceWorkspaces, workspace: &str, key: &GrantKey) {
    let signing = SigningKey::from_bytes(&key.secret());
    if let Err(error) = workspaces
        .copies()
        .enter(
            workspace,
            (
                key.cert(),
                &signing as &dyn collab_types::signer::DeviceSigner,
            ),
        )
        .await
    {
        tracing::warn!(%error, workspace, "a key does not enter the workspace it asks of");
    }
}

async fn workspace_verb(
    workspaces: &ServiceWorkspaces,
    workspace: &str,
    key: &GrantKey,
    action: &str,
    arguments: Map<String, Value>,
) -> Result<Value, ServerError> {
    let id = WorkspaceId::parse(workspace).ok_or(ServerError::NotFound)?;
    enter_the_key(workspaces, workspace, key).await;
    let caller = tilo_peer_state::key_caller::caller_of_key_here(key.cert());
    let request = InvokeRequest {
        asks: false,
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

/// A person's stop of their agent on this device, or letting a stopped one go on: what this
/// device's runs of the agent are doing ends, and the stop is written in every workspace the
/// agent may work in.
struct AgentStop {
    stopping: bool,
    session: String,
    workspace: String,
    agent: String,
    member: tilo_member_types::MemberId,
    targets: Vec<String>,
    person: Caller,
    receipt: InvocationReceipt,
    workspaces: Arc<ServiceWorkspaces>,
    invocations: Arc<Invocations>,
    broker: AppActionBroker,
}

/// The grant and session a stop or go-on `request` names, as its arguments name them.
fn stop_named(
    request: &InvokeRequest,
) -> Result<
    tilo_action_services::mcp_server::AgentSessionRef,
    Pending<Result<InvocationReceipt, ActionCallError>>,
> {
    match &request.argument {
        InvokeArgument::Json(argument) => {
            match serde_json::from_value(Value::Object(argument.clone())) {
                Ok(named) => Ok(named),
                Err(error) => Err(Pending::ready(Err(ActionCallError::invalid_request(
                    error.to_string(),
                )))),
            }
        }
        _ => Err(Pending::ready(Err(ActionCallError::invalid_request(
            "a stop names its grant and session",
        )))),
    }
}

/// Every workspace `held` keeps that a stop of `agent` for `operator` reaches: all of them for
/// a stop; for going on, those whose policy stops the agent now.
fn stop_targets(
    held: &ServiceWorkspaces,
    stopping: bool,
    agent: &str,
    operator: &str,
    now: &tilo_action_policy::Context<'_>,
) -> Vec<String> {
    held.workspaces()
        .into_iter()
        .filter(|workspace| {
            stopping
                || held.policy_of(workspace.id())()
                    .is_some_and(|policy| policy.stops(agent, operator, now))
        })
        .map(|workspace| workspace.id().to_string())
        .collect()
}

/// Write `stop` as the person's own say. What this device's runs of the agent are doing ends
/// first, then the stop (or going on) is written in each workspace it reaches, and the session's
/// own record follows.
async fn write_agent_stop(stop: AgentStop) -> Result<InvocationReceipt, ActionCallError> {
    let AgentStop {
        stopping,
        session,
        workspace,
        agent,
        member,
        targets,
        person,
        receipt,
        workspaces,
        invocations,
        broker,
    } = stop;
    let copies = workspaces.copies();
    let mut answer = Ok(receipt);
    if stopping {
        // What this device recorded of the runs it serves ends first: those are in the
        // workspace it is bound to, which the listing of held workspaces need not name.
        let own = workspace.clone();
        let origin_writes = |caller: &Caller, origin: &Origin| {
            broker.state().may_write_origin(caller, &own, origin)
        };
        for cancelled in invocations.stop_agent(&person, &workspace, &member, &origin_writes) {
            let _ = cancelled.await;
        }
    }
    for target in &targets {
        if stopping {
            // What it is running ends first: as the dispatch itself cancels a call, the
            // person's own ask, before the answer that refuses the next one. Written the other
            // way, the answer's change of who is here woke a running watch, which answered what
            // it saw before its cancellation reached it.
            let origin_writes = |caller: &Caller, origin: &Origin| {
                broker.state().may_write_origin(caller, target, origin)
            };
            for cancelled in invocations.stop_agent(&person, target, &member, &origin_writes) {
                let _ = cancelled.await;
            }
        }
        let written = if stopping {
            copies.answer_stop(target, &agent).await
        } else {
            copies.answer_go_on(target, &agent).await
        };
        if let Err(reason) = written {
            answer = Err(ActionCallError::Refused { reason });
            break;
        }
    }
    if answer.is_ok() {
        if stopping {
            copies.stop_agent(&workspace, &session);
        } else {
            copies.let_agent_go_on(&workspace, &session);
        }
    }
    answer
}

impl RuntimeActionProvider {
    /// Resolves on the next write landing in a document this device's service holds, from
    /// whichever device or app it came, or once a provider has waited as long as any may.
    fn next_write(
        &self,
    ) -> tilo_actions::remote::Pending<Result<(), tilo_actions::remote::ActionCallError>> {
        let mut writes = self.workspaces.copies().document_writes();
        let (landed, wait) = tilo_actions::remote::pending();
        drop(tilo_rt::app().handle().spawn(async move {
            tokio::select! {
                // A write, writes missed or the service closing: each is something to read again.
                _ = writes.recv() => landed.complete(Ok(())),
                // As long as any provider holds a waiting answer: the watch reads again, and
                // asks again, rather than being told its wait was never answered.
                () = tokio::time::sleep(tilo_actions::remote::MAX_REMOTE_WAIT) => {
                    landed.complete(Ok(()));
                }
                // Nobody waits for it any more.
                () = landed.abandoned() => {}
            }
        }));
        wait
    }
}

/// The device's feed of its workspaces' changes, read by the callers [`RuntimeActionProvider`]
/// admits as their person: the person's own calls. An agent's run reads what its rules let it
/// read, which a change of a whole record does not say, so it is refused.
struct DeviceChanges(RuntimeActionProvider);

impl tilo_actions::remote::ChangeFeed for DeviceChanges {
    fn since(
        &self,
        caller: &Caller,
        after: Option<&str>,
    ) -> Pending<Result<tilo_actions::remote::ChangePage, ActionCallError>> {
        let provider = &self.0;
        let admitted = match &provider.session {
            Some(session) => {
                session.agent.is_none() && session.check(caller, &provider.workspaces).is_ok()
            }
            None => provider.admitted(caller),
        };
        if !admitted {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        }
        // Read on the service's runtime, awaited there: the frame that asked never waits on a
        // thread for the documents it reads.
        let (feed, reads) = provider.workspaces.change_feed();
        let (caller, after) = (caller.clone(), after.map(str::to_owned));
        let (paged, page) = tilo_actions::remote::pending();
        drop(tilo_rt::app().handle().spawn(async move {
            let answer = feed
                .page_of_service(&reads, &caller, after.as_deref())
                .await;
            paged.complete(answer);
        }));
        page
    }
}

struct AdmittedRuntimeSession {
    policy_time: tilo_app_host::policy_clock::PolicyTime,
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

    /// Whether this is a key's own run: the key its person approved, acting as the agent its
    /// certificate names, on the device that key itself is. An agent a device of the person's
    /// runs is another device's, so its acting key is never the admitted one.
    fn is_key_run(&self) -> bool {
        self.agent.as_ref().is_some_and(|agent| {
            agent.acting().is_agent()
                && agent.device_public_key() == self.admission.caller().device_public_key()
        })
    }

    /// Whether `caller` is who this run was admitted for. A program the person runs reaches
    /// their own run as the local person, each in an execution of its own: that narrows what its
    /// calls own, never who makes them. A run projected into one execution admits only that one.
    fn admits(&self, caller: &Caller) -> bool {
        let Some(projection) = &self.projection else {
            // A run with no projection admits the device's own admission, and the agent the run
            // was made for: the connection of an unnamed agent presents the agent itself.
            // Each call the person's shell carries for its agent comes in an execution of its
            // own: that narrows what the call owns, never who makes it.
            return caller == self.admission.caller()
                || self.agent.as_ref().is_some_and(|agent| {
                    caller == agent
                        || match (agent.execution_lease(), caller.execution_lease()) {
                            (None, Some(lease)) => *caller == agent.clone().in_execution(lease),
                            _ => false,
                        }
                });
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
        // A key its person revoked acts no more: the call that finds it is refused, whichever
        // session it was admitted in and whatever the run serves, and the refusal says the key
        // was revoked and what to do. The agent's own key is what its calls go as, beside the
        // admission's caller for a run on the person's own device.
        if let Some(revoked) = workspaces.copies().revoked(caller).or_else(|| {
            self.agent
                .as_ref()
                .and_then(|agent| workspaces.copies().revoked(agent))
        }) {
            tracing::warn!("a revoked key asked to act; this call is refused");
            return Err(revoked);
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
    asked_by: &Caller,
    request: InvokeRequest,
    workspace: &str,
) -> Pending<Result<InvocationReceipt, ActionCallError>> {
    if !Tickets::root_only(&request) {
        return run_device_action(invocations, tickets, caller, asked_by, request, workspace);
    }
    // What only the calling person's root signs runs here when this computer keeps their root
    // and they are signed in; otherwise it waits in its record for a device of theirs that does.
    let person = caller
        .acting()
        .person_id()
        .and_then(|person| hex::decode(person).ok())
        .and_then(|person| <[u8; 32]>::try_from(person).ok());
    if caller.is_system() || person.is_some_and(holds_root_for_person) {
        return run_device_action(invocations, tickets, caller, asked_by, request, workspace);
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
    asked_by: &Caller,
    request: InvokeRequest,
    workspace: &str,
) -> Pending<Result<InvocationReceipt, ActionCallError>> {
    let route: tilo_invocations::Route = (Arc::new(tickets.clone()), caller.clone());
    invocations.record(caller, asked_by, &request, Some(workspace), route);
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
    // A boot key's certificate names the computer that asks and the key, as the person reads it.
    let facts = tilo_actions::remote::asked::AskedFacts {
        verb: listed
            .as_ref()
            .map(|action| action.action_label.clone())
            .unwrap_or_default(),
        on: listed.map(|action| action.resource_label),
        ..super::boot_unlock::asked_facts(request).unwrap_or_default()
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

    /// The store the provider that answers `target` keeps what its actions read, when it keeps
    /// one of its own: the device's own providers do, so their bytes stay here rather than in a
    /// workspace every member's device carries. `None` for an action answered by a provider that
    /// reads a workspace's store, which is where [`Self::content_puts`] puts it.
    fn device_content_puts(
        &self,
        target: &RemoteActionTarget,
    ) -> Option<Arc<dyn tilo_actions::remote::ContentPuts>> {
        let registered = self.workspaces.copies().providers().get(&target.provider)?;
        let content = registered.answers.content_puts()?;
        Some(Arc::new(AdmittedContent {
            session: Arc::clone(self.session.as_ref()?),
            workspaces: Arc::clone(&self.workspaces),
            content,
        }))
    }

    /// Whether `caller` acts in the run of the person's own plain client on this computer: what
    /// a connection from their account to the daemon's own socket is admitted as
    /// ([`Self::session_factory`]), never a run of an agent elsewhere, of a roster turn, or of
    /// another person's key.
    fn is_own_client(&self, caller: &Caller) -> bool {
        let operator = self.workspaces.operator();
        let own = tilo_member_types::MemberId::agent(
            device_agent_id(&operator, MCP_CLIENT),
            hex::encode(operator),
        );
        self.session.as_ref().is_some_and(|session| {
            session.admits(caller)
                && session
                    .agent
                    .as_ref()
                    .is_some_and(|agent| agent.acting().member_id() == Some(&own))
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

    /// What the person's Servers workspace says of their machines, when `request` reads it
    /// ([`machines`](tilo_action_services::machines)): this device's own workspace service answers
    /// the read, as it answers their desktop's, so a computer with no desktop reads it the same
    /// way. The person reads it, and so do their own programs on this computer; their machines'
    /// disk recovery keys only on a connection the kernel says is theirs. `None` when `request`
    /// reads none of it.
    fn machines(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Option<Pending<Result<InvocationReceipt, ActionCallError>>> {
        use tilo_action_services::machines;
        let target = &request.target;
        let step = target.address.action.clone();
        if target.provider != machines::PROVIDER
            || target.address.resource != machines::RESOURCE
            || ![
                machines::LIST,
                machines::SHOW,
                machines::SERVERS,
                machines::RECOVERY_KEYS,
            ]
            .contains(&step.as_str())
        {
            return None;
        }
        let admitted = self.is_the_person(caller)
            || (self.is_own_client(caller)
                && (step != machines::RECOVERY_KEYS || caller.is_own_connection()));
        if !admitted {
            return Some(Pending::ready(Err(ActionCallError::Refused {
                reason: "only you read your machines, on a device of yours".into(),
            })));
        }
        // A read that names nothing (the servers, the recovery keys) comes with no arguments.
        let arguments = match &request.argument {
            InvokeArgument::Json(arguments) => arguments.clone(),
            InvokeArgument::None => Map::new(),
            InvokeArgument::Text(_) => {
                return Some(Pending::ready(Err(ActionCallError::Refused {
                    reason: "a machine step takes typed arguments".into(),
                })));
            }
        };
        let (done, receipt) = tilo_actions::remote::pending();
        let workspaces = Arc::clone(&self.workspaces);
        let invocation = request.invocation;
        tilo_rt::app().spawn(async move {
            let cert = workspaces.device_cert().clone();
            let status = match workspaces.service().ask_own(&cert, &step, arguments).await {
                Ok(value) => ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput {
                        value: Some(value),
                        ..tilo_actions::ActionOutput::default()
                    },
                },
                Err(message) => ReceiptStatus::Failed {
                    error: tilo_actions::ActionFailure {
                        code: "machine_refused".into(),
                        message,
                        details: None,
                        retryable: false,
                    },
                },
            };
            done.complete(Ok(InvocationReceipt { invocation, status }));
        });
        Some(receipt)
    }

    /// Stop the agent session `request` names on this device, or let a stopped one go on, as
    /// the person asks of the MCP server: the person here's own ask, never an agent's and never
    /// a local caller of another person's. `None` when the request is not such a stop, or names
    /// no session this device serves.
    fn stop_or_go_on(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Option<Pending<Result<InvocationReceipt, ActionCallError>>> {
        use tilo_action_services::mcp_server;
        let action = request.target.address.action.as_str();
        if request.target.provider != mcp_server::PROVIDER
            || ![mcp_server::STOP, mcp_server::GO_ON].contains(&action)
            || !self.is_the_person(caller)
        {
            return None;
        }
        let named = match stop_named(request) {
            Ok(named) => named,
            Err(refused) => return Some(refused),
        };
        if !self
            .workspaces
            .copies()
            .knows_agent_session(&self.workspace, &named.session)
        {
            return None;
        }
        let stopping = action == mcp_server::STOP;
        let operator = self.workspaces.operator();
        let agent = device_agent_id(&operator, MCP_CLIENT);
        // The person's own act, as the caller that made the agent's calls works for them.
        let person = Caller::local_person(hex::encode(operator)).in_workspace(&self.workspace);
        // Every workspace the agent may work in: a stop is the person's own say about their own
        // agent, which reaches only the agent acting for them, so it is written in each
        // workspace this device holds; going on takes it back where it stands. The answer syncs
        // with the workspace, so a host running the agent elsewhere refuses its next call.
        let operator_person = hex::encode(operator);
        let member = tilo_member_types::MemberId::agent(agent.clone(), operator_person.clone());
        let now = tilo_action_policy::Context::at_time(tilo_clock::wall());
        let targets = stop_targets(&self.workspaces, stopping, &agent, &operator_person, &now);
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
        let stop = write_agent_stop(AgentStop {
            stopping,
            session: named.session,
            workspace: self.workspace.clone(),
            agent,
            member,
            targets,
            person,
            receipt,
            workspaces: Arc::clone(&self.workspaces),
            invocations: Arc::clone(&self.invocations),
            broker: self.broker.clone(),
        });
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => drop(runtime.spawn(async move { complete.complete(stop.await) })),
            Err(_) => super::workspaces::block_on_owned(async move {
                complete.complete(stop.await);
            }),
        }
        Some(answered)
    }

    /// The workspace's own tree (`workspace/apps`, `workspace/vault`, `workspace/members`) as
    /// this device answers it for `caller`, and the caller it is asked as: the provider this
    /// device registered for the workspaces it keeps. The person's own connection has no device
    /// of its own for the workspace to read, so it is asked as this device, whose certificate
    /// names them, as their other devices are. A key or an agent of theirs is asked as itself,
    /// and what the tree offers it is decided by its own grants, never by the device's. `None`
    /// where this device keeps no workspace, or where `caller` is not a run this runtime serves.
    fn workspace_tree(
        &self,
        caller: &Caller,
    ) -> Option<(
        tilo_workspace_service::workspaces::providers::Registered,
        Caller,
    )> {
        let session = self.session.as_ref()?;
        let asked = if self.is_the_person(caller) {
            let cert = self.workspaces.device_cert();
            Caller::authenticated(cert.user_public_key, cert.device_public_key)
                .in_workspace(&self.workspace)
        } else if session.admits(caller) {
            session.acting().clone()
        } else {
            return None;
        };
        let tree = self
            .workspaces
            .copies()
            .providers()
            .get(tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER)?;
        Some((tree, asked))
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
                        .push(tilo_action_services::workspace_invocations::publisher(
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
        policy_time: tilo_app_host::policy_clock::PolicyTime,
    ) -> tilo_mcp::SessionFactory<Self> {
        let provider = self;
        Arc::new(move |_| {
            // The MCP client is the person's agent, under a key of its own: the admission stays
            // the person's, and every call the client makes is signed by the agent's key. Each
            // connection is a run of its own, admitted for it: its end retires that run alone,
            // never the one the daemon's other connections go on as.
            let (mut session_host, caller, agent_cert, agent) =
                provider.agent_session(&workspace, &actor, &policy_time, |person| {
                    (device_agent_id(person, MCP_CLIENT), MCP_CLIENT.to_owned())
                })?;
            // A descriptor an answer of this run carries goes with it on the connection it serves.
            session_host.handed = crate::seats::connection_hand();
            let session = agent_session_id(&caller)?;
            // The connection is here from the moment it opens, before it asks anything, and
            // says what it is when its client does. The device its admission certified is what
            // the person's MCP server lists the run under.
            if let Some(execution) = caller.execution_id() {
                provider.workspaces.copies().agent_opened(
                    &execution.to_string(),
                    &workspace,
                    tilo_mcp::session_api::UNNAMED_CLIENT,
                    Some(&agent_cert),
                    &Arc::new(tokio::sync::Notify::new()),
                );
            }
            Ok(tilo_mcp::McpServer::admitted(session_host, caller)?
                .with_agent_session(session)
                .with_invoker(tilo_actions::ActionInvoker::admitted(agent.acting()))
                .with_device_shell())
        })
    }

    /// A run of `actor`'s agent in `workspace`, which `agent` names (its id and name) from the
    /// person's public key: admitted here for a dispatch that holds no run of its own, then
    /// [`Self::agent_session_of`].
    pub(super) fn agent_session(
        &self,
        workspace: &str,
        actor: &str,
        policy_time: &tilo_app_host::policy_clock::PolicyTime,
        agent: impl FnOnce(&[u8; 32]) -> (String, String),
    ) -> io::Result<(Self, Caller, DeviceCert, Caller)> {
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
        policy_time: &tilo_app_host::policy_clock::PolicyTime,
        agent: impl FnOnce(&[u8; 32]) -> (String, String),
    ) -> io::Result<(Self, Caller, DeviceCert, Caller)> {
        self.agent_session_as(admission, policy_time, agent, None)
    }

    /// [`Self::agent_session_of`], the run admitting the agent as `carried` names it where it
    /// is given: the agent the person's shell admitted under the key this device made it, whose
    /// connection the shell names in its own words (its session there, the name it goes by).
    /// A caller the shell names under another key than this device made the agent is refused.
    fn agent_session_as(
        &self,
        admission: Arc<tilo_workspace_service::workspaces::SessionAdmission>,
        policy_time: &tilo_app_host::policy_clock::PolicyTime,
        agent: impl FnOnce(&[u8; 32]) -> (String, String),
        carried: Option<&Caller>,
    ) -> io::Result<(Self, Caller, DeviceCert, Caller)> {
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
        let agent = match carried {
            None => agent,
            Some(carried)
                if carried.device_public_key() == agent.device_public_key()
                    && carried.user_public_key() == agent.user_public_key()
                    && carried.acting().agent_id() == agent.acting().agent_id() =>
            {
                carried.kept().restored()
            }
            Some(_) => {
                return Err(io::Error::other(
                    "the shell names an agent under another key than this device made it",
                ));
            }
        };
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
            tilo_app_host::agents::workspace_agent(record.copies(), id, &listed, &person).await;
        });
        // The key enters the workspace as every key of its person's does, by the admission it
        // signs itself, so the workspace reads it as that agent, with its person's grants.
        let (cert, key) = signer.signing().map_err(io::Error::other)?;
        let (record, named, listed) = (
            Arc::clone(&self.workspaces),
            workspace.to_owned(),
            cert.clone(),
        );
        block_on_owned(async move {
            record
                .copies()
                .enter_agent_key((key.as_ref(), &listed), &named, &listed.device_label)
                .await
        })
        .map_err(|why| io::Error::other(format!("the agent can't act in this workspace: {why}")))?;
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
        let run = self.run_as(AdmittedRuntimeSession {
            policy_time: policy_time.clone(),
            provider: Arc::new(Signed::new(delegated, Some(signer))),
            admission,
            agent: Some(agent.clone()),
            projection: None,
            pressed: None,
            own_shell: false,
        });
        // The admission stays the person's; the calls it carries are the agent's, whose key
        // signs each and whose acting the changes they make name.
        Ok((run, caller, cert, agent))
    }

    /// This device's runtime, answering as `session`: the agent's run keeps the device's
    /// tickets, invocations and asked records, since it acts for the device's own person.
    fn run_as(&self, session: AdmittedRuntimeSession) -> Self {
        Self {
            broker: self.broker.clone(),
            workspace: self.workspace.clone(),
            workspaces: self.workspaces.clone(),
            apps: self.apps.clone(),
            session: Some(Arc::new(session)),
            tickets: self.tickets.clone(),
            invocations: Arc::clone(&self.invocations),
            kept: Arc::clone(&self.kept),
            asked: self.asked.clone(),
            handed: self.handed.clone(),
        }
    }

    /// A run of the device `key` in `workspace`, which this device holds: its calls are its
    /// person's own on that device, its certificate chaining to them, decided by the workspace's
    /// grants for them through the broker's host for other devices, and a search answers what
    /// they may read. Nothing of this device's own is theirs: no ticket, no asked record, no
    /// key it makes and no root it keeps.
    ///
    /// # Errors
    /// The workspace is not kept here, or does not admit the run now.
    pub(super) fn member_session(
        &self,
        workspace: &str,
        key: &DeviceCert,
        policy_time: &tilo_app_host::policy_clock::PolicyTime,
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
        // A member is the run's original caller, with the certificate their link verified as
        // its evidence: what a host action the run reaches is owned by and decided as, never
        // this device. An agent's grant has no such evidence of a person, so what it reaches
        // that needs one is refused.
        let provider = if caller.acting().is_person() {
            self.broker
                .certified_key_run(&caller, key)
                .map_err(io::Error::other)?
        } else {
            self.broker.device_host()
        };
        let run = Self {
            broker: self.broker.clone(),
            workspace: self.workspace.clone(),
            workspaces: self.workspaces.clone(),
            apps: self.apps.clone(),
            session: Some(Arc::new(AdmittedRuntimeSession {
                policy_time: policy_time.clone(),
                provider,
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
    /// A person's answer to a question that waits for them, as the run admitted them: an agent
    /// answers nobody's, its own least. Only the system confirms for them: their own shell's
    /// confirm view, or the CLI's own prompt on a connection the kernel says is theirs. A
    /// program, an app and an agent are asked, never answer.
    fn resolve_asked(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        session: &AdmittedRuntimeSession,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if session.agent.is_some() || !(session.own_shell || caller.is_own_connection()) {
            return Pending::ready(Err(ActionCallError::Refused {
                reason: tilo_actions::remote::asked::CONFIRMED_BY_THE_SYSTEM.to_owned(),
            }));
        }
        self.workspaces
            .copies()
            .resolve_asked(session.acting(), request)
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
        if tilo_action_services::kept_by_workspace_host(&request.target.provider) {
            return self.workspace_kept(session.acting(), request);
        }
        // The person's own act on the workspace's tree (adding to its vault, installing an
        // app), answered as this device ([`Self::workspace_tree`]) and recorded as theirs, so
        // following it reaches what took it.
        if request.target.provider == tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER
            && self.is_the_person(caller)
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
        if tilo_actions::remote::asked::is_resolve(&request.target) {
            return self.resolve_asked(caller, &request, session);
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
        // is answered where they pressed it. A key's call on the workspace's own tree is not
        // an agent's: this device answers it as the key, and what the tree offers it is
        // decided by the key's own grant.
        let key_tree = request.target.provider
            == tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER
            && session.is_key_run();
        if !key_tree
            && let (Some(asked), Some(agent)) = (&self.asked, &session.agent)
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
        // outside Tilo asks them through the shell, or a press needs their yes. A key of
        // theirs that is no agent of theirs calls as them, and its call waits for them the
        // same; what it asks of the workspace's own tree is decided by its grant below.
        if let Some(asked) = &self.asked
            && session.agent.as_ref().is_none_or(|key| {
                key.acting().agent_id().is_none()
                    && request.target.provider
                        != tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER
            })
        {
            let run = self.own_run(session, &request);
            let route = (Arc::clone(&session.provider), session.acting().clone());
            return asked.send(run, caller, route, request);
        }
        // A key or an agent of the person's asks the workspace's own tree as itself, where a
        // device of the workspace asks it: the workspace's governance decides it by that run's
        // own grants, and its stores by that run's own certificate ([`Self::workspace_tree`]).
        if request.target.provider == tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER
            && let Some((_, asked)) = self.workspace_tree(caller)
        {
            let answers = self.workspaces.copies().device_actions();
            let answer = answers.invoke(&asked, request.clone());
            self.invocations.record(
                caller,
                &asked,
                &request,
                Some(&self.workspace),
                (Arc::clone(&answers), asked.clone()),
            );
            return answer;
        }
        self.invoke_as_session(caller, request, session)
    }

    /// The call itself, as the session's own run, recorded as its invocation: one kept for the
    /// person's yes where it was sent says so in its state, which the listing reads.
    fn invoke_as_session(
        &self,
        caller: &Caller,
        request: InvokeRequest,
        session: &AdmittedRuntimeSession,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let runs_as = session.acting().clone();
        self.invocations.record(
            caller,
            session.acting(),
            &request,
            Some(&self.workspace),
            (Arc::clone(&session.provider), runs_as.clone()),
        );
        session.provider.invoke(&runs_as, request)
    }

    /// The person's own call of an action a provider the shell serves answers
    /// ([`Self::invoke`]'s own-device branch): recorded as their invocation, and kept as their
    /// asked record where the provider asks for their yes in its own call, which their answer
    /// then runs. A confirmation this call wrote itself is none: only their answer of the asked
    /// record runs what asks them.
    fn answered_at_own_device(
        &self,
        registered: &tilo_workspace_service::workspaces::providers::Registered,
        caller: &Caller,
        mut request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if !matches!(
            request.confirmation.as_ref(),
            Some(tilo_actions::remote::Confirmation::Asked(_))
        ) {
            request.confirmation = None;
        }
        let route = (Arc::clone(&registered.answers), caller.clone());
        if let Some(asked) = &self.asked {
            let run = super::asked::Run::Own {
                workspace: self.workspace.clone(),
                pressed: None,
            };
            return asked.send(run, caller, route, request);
        }
        self.invocations.record(
            caller,
            caller,
            &request,
            Some(&self.workspace),
            route.clone(),
        );
        registered.answers.invoke(caller, request)
    }

    /// The run the person's own call waits on as its asked record: the press it came from,
    /// where a view pressed it.
    fn own_run(
        &self,
        session: &AdmittedRuntimeSession,
        request: &InvokeRequest,
    ) -> super::asked::Run {
        super::asked::Run::Own {
            workspace: self.workspace.clone(),
            pressed: session.pressed.as_ref().map(|pressed| {
                let device = self.workspaces.device_cert();
                tilo_actions::remote::asked::Pressed {
                    device: hex::encode(device.device_public_key),
                    app: tilo_actions::remote::resource_parts(&request.target.provider)
                        .map_or_else(String::new, |(app, _)| app.to_owned()),
                    instance: pressed.instance.clone(),
                    resource: pressed.resource.clone(),
                    at: pressed.at,
                    control: pressed.control,
                }
            }),
        }
    }

    /// Whether the person stopped the session `caller` acts in: it is ended for good, and what
    /// its connection asks next reaches nothing.
    fn session_stopped(&self, caller: &Caller) -> bool {
        caller.execution_id().is_some_and(|execution| {
            self.workspaces
                .copies()
                .agent_stopped(&execution.to_string())
        })
    }

    /// What this device answers itself, before the person's services run. The person stops one
    /// of the agents it serves, or lets a stopped one go on, as their card asks: a Deny in every
    /// workspace the agent works in, and what it is running here cancelled. Then its own
    /// records, its settings written through the same actions before the person's services ran
    /// and after, once the device is theirs, and its machines.
    fn the_device_answers(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Option<Pending<Result<InvocationReceipt, ActionCallError>>> {
        self.stop_or_go_on(caller, request)
            .or_else(|| self.device_records(caller, request))
            .or_else(|| self.machines(caller, request))
    }

    /// What this device answers for a ticket it issues: whether this run may be answered one at
    /// all, and the answer dispatched beside the runtime, or the refusal of a call this device
    /// publishes but may not answer here.
    fn ticket_answer(
        &self,
        caller: &Caller,
        tickets: &Arc<Tickets>,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        // The run's own agent asked it, where a session asked: the record names the agent its
        // person reads, not the connection that carried the call.
        let asked_by = self
            .session
            .as_ref()
            .filter(|session| session.admits(caller))
            .map_or_else(|| caller.clone(), |session| session.acting().clone());
        let answered = Tickets::asks_for_a_person(&request)
            || self.is_the_person(caller)
            // What only the person's root signs, asked by their own programs on this
            // computer: the device signs it with the root it keeps, and hands the ask to a
            // device of theirs that keeps it otherwise. A session reaches its daemon as the
            // person's own agent ([`Self::agent_session`]), so this is the one way their
            // session was ever to certify a key, take a recovery backup or answer a sign-in.
            || (Tickets::root_only(&request) && self.is_own_client(caller))
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
                        && caller.acting().is_agent())));
        if !answered {
            // This device publishes the action whatever this run may do with one: the answer is
            // the device's own refusal, never the broker's that its resource offers no such
            // action.
            return Pending::ready(Err(ActionCallError::NotPermitted));
        }
        // A root-signed answer is blocking work: it reads and writes the person's workspace.
        // Every session of this endpoint shares one current-thread runtime, so that one is
        // answered beside that runtime, as the catalogue the broker assembles is, and the answer
        // it settles with is handed on. Every other device action (what the device is, its
        // identities, a ticket) is answered in its receipt, as a caller reading it at once
        // expects.
        if !Tickets::root_only(&request) {
            return device_action(
                (&self.invocations, self.asked.as_ref()),
                tickets,
                caller,
                &asked_by,
                request,
                &self.workspace,
            );
        }
        let (invocations, asked, tickets, caller, request, workspace) = (
            Arc::clone(&self.invocations),
            self.asked.clone(),
            Arc::clone(tickets),
            caller.clone(),
            request,
            self.workspace.clone(),
        );
        let device_call = move || {
            device_action(
                (&invocations, asked.as_ref()),
                &tickets,
                &caller,
                &asked_by,
                request,
                &workspace,
            )
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return device_call();
        };
        let (done, answer) = tilo_actions::remote::pending();
        drop(runtime.spawn(async move {
            match tokio::task::spawn_blocking(device_call).await {
                Ok(made) => made.then(move |settled| {
                    done.complete(settled.unwrap_or(Err(ActionCallError::Unanswered)));
                }),
                Err(error) => done.complete(Err(ActionCallError::Unavailable {
                    reason: format!("the device's answer did not run: {error}"),
                })),
            }
        }));
        answer
    }
}

// These existing alias forms may eventually dispatch into a published app.
// Keep original certified ownership through the broker's deferred launch path.
fn certified_provider(provider: &str) -> bool {
    provider.parse::<u64>().is_ok()
        || tilo_actions::remote::application_provider(provider).is_some()
        || provider.starts_with("launch:")
}

/// Whether `provider` is one of the person's seat: their settings and model keys, the account this
/// computer is signed in to, and the rest their desktop serves under an app's window
/// ([`runtime_collab::wire::shell::SEAT_PROVIDERS`]).
fn is_seat_provider(provider: &str) -> bool {
    runtime_collab::wire::shell::seat_app(provider).is_some()
}

/// The refusal of a seat provider's call to a connection that is not the person's shell.
fn seat_refused() -> ActionCallError {
    ActionCallError::Refused {
        reason: "what your desktop keeps for you (your settings, model keys and the account this \
                 computer is signed in to) is reached through your desktop, not from another \
                 program"
            .into(),
    }
}

impl RuntimeActionProvider {
    /// Whether `caller` is a connection of the person's own account that is not their shell: a
    /// command or any program they run, which an app may have started. It reaches what their
    /// desktop serves on this device, but never the seat's providers, which hold their secrets,
    /// model keys and server account.
    fn seat_kept_from(&self, caller: &Caller) -> bool {
        caller.is_own_connection()
            && !self
                .session
                .as_ref()
                .is_some_and(|session| session.own_shell)
    }

    /// What [`ActionProvider::query`] lists to `caller`, the seat's providers among it.
    fn listing(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        if query.provider.as_deref() == Some(tilo_action_services::workspace_invocations::PROVIDER)
        {
            if let Some(session) = &self.session
                && let Err(refusal) = session.check(caller, &self.workspaces)
            {
                return Pending::ready(Err(refusal.into()));
            }
            // The calls that wait for their person are listed beside the ones the device kept
            // a record of: their asked records name them, and their person answers them from
            // the invocation that record gives, which a walk of this tree finds.
            let waiting = self
                .workspaces
                .copies()
                .asked_store()
                .map(|store| store.waiting_in(&self.workspace))
                .unwrap_or_default();
            return Pending::ready(self.broker.state().invocation_page(
                &self.workspace,
                caller,
                &query,
                &waiting,
            ));
        }
        if query.provider.as_deref() == Some(tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER)
            && self.is_the_person(caller)
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
                // The person's own shell is this device's own process: it is offered what
                // reaches this device alone, the seat's providers it serves itself among them
                // (Settings' sections, the activity). Any other run here lists as a caller this
                // device does not vouch for.
                let listed = if session.own_shell {
                    caller.clone()
                } else {
                    on_this_device(caller)
                };
                let answer = self.broker.state().action_query(&listed, &query);
                return self.with_devices(caller, answer, &query);
            }
            if let Err(refusal) = session.check(caller, &self.workspaces) {
                return Pending::ready(Err(refusal.into()));
            }
            // A key or an agent of the person's reaches the workspace's own tree as itself
            // ([`Self::workspace_tree`]): what it offers them is decided by their own grants.
            if query.provider.as_deref()
                == Some(tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER)
                && let Some((tree, asked)) = self.workspace_tree(caller)
            {
                return tree.answers.query(&asked, query);
            }
            if query
                .provider
                .as_ref()
                .is_some_and(|provider| certified_provider(provider))
            {
                let answer = session.provider.query(caller, query.clone());
                return self.with_devices(caller, answer, &query);
            }
            // An agent this device runs for its person, under a key it made, is listed what its
            // calls reach: its run, which keeps the person's admission it was delegated, lists as
            // it dispatches, so what the person's desktop serves on this device (its board, its
            // layout) is the agent's to find. What the seat keeps for the person is not listed:
            // their desktop alone reaches it.
            if session.agent.is_some() && !session.is_key_run() && !session.own_shell {
                let answer = session
                    .provider
                    .query(session.acting(), query.clone())
                    .map(|page| {
                        page.map(|mut page| {
                            page.rows.retain(|row| !is_seat_provider(&row.provider));
                            page.providers
                                .retain(|provider| !is_seat_provider(&provider.provider));
                            page
                        })
                    });
                let answer = session.guarded(Arc::clone(&self.workspaces), caller.clone(), answer);
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
}

fn on_this_device(caller: &Caller) -> Caller {
    // The person on a connection the kernel says is their own account, on this device's own
    // daemon, is this device's as their shell is: offered and answered what reaches this device
    // alone, the providers their desktop serves on it among them. Any other run here is a caller
    // this device does not vouch for.
    if caller.is_own_connection() && !caller.is_system() && caller.device_public_key().is_none() {
        return caller.clone();
    }
    caller.clone().from_foreign_device(true)
}

/// Whether `caller` is the plain person and nobody else: no agent acting or named, no app acting
/// as them or launched by them, no turn or agent session, no key of theirs, and no device certificate of their own. What a call on the link
/// this device made for their shell is, as a call on their own daemon's connection is.
fn the_plain_person(caller: &Caller) -> bool {
    let acting = caller.acting();
    caller.device_public_key().is_none()
        && caller.user_public_key().is_none()
        && !caller.is_foreign_device()
        && caller.turn().is_none()
        && acting.agent_id().is_none()
        && acting.app.is_none()
        && acting.agent_session.is_none()
        && !acting.handed
        && acting.acted_for.is_empty()
        && caller.acting_for_person().is_none()
        && caller.agent_called().is_none()
        && caller.launched_app().is_none()
        && caller.key().is_none()
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
    /// ([`tilo_action_services::kept_by_workspace_host`]), asked of the device that hosts the
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
        if let Some(host) = hosts.host_actions(workspace) {
            return host.invoke(caller, request);
        }
        // This device keeps the workspace itself: what its host keeps for the person is
        // answered here, by the provider its workspace service registered for it.
        if let Some(id) = WorkspaceId::parse(workspace)
            && self.workspaces.keeps_now(id)
            && let Some(registered) = self
                .workspaces
                .copies()
                .providers()
                .get(&request.target.provider)
        {
            return registered.answers.invoke(caller, request);
        }
        Pending::ready(Err(ActionCallError::Unavailable {
            reason: "this device keeps this workspace itself, and what its host keeps for                          the person is asked there"
                .into(),
        }))
    }
}

impl ActionProvider for RuntimeActionProvider {
    /// The device's one search index, read as the caller may read it.
    /// What changed in the records of the workspace a caller works in, from the documents this
    /// device holds: what a watch of the whole workspace reads.
    fn change_feed(&self) -> Option<Arc<dyn tilo_actions::remote::ChangeFeed>> {
        Some(Arc::new(DeviceChanges(self.clone())))
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
            if let Err(error) = tilo_app_host::agents::rename_agent(
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
        if !self.seat_kept_from(caller) {
            return self.listing(caller, query);
        }
        if query.provider.as_deref().is_some_and(is_seat_provider) {
            return Pending::ready(Err(seat_refused()));
        }
        self.listing(caller, query).map(|page| {
            page.map(|mut page| {
                page.rows.retain(|row| !is_seat_provider(&row.provider));
                page.providers
                    .retain(|provider| !is_seat_provider(&provider.provider));
                page
            })
        })
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if let Err(refused) = tilo_actions::appearance::refuse_here(&request) {
            return Pending::ready(Err(refused));
        }
        // A session the person stopped is ended for good: what its connection asks next reaches
        // nothing.
        if self.session_stopped(caller) {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        }
        if self.seat_kept_from(caller) && is_seat_provider(&request.target.provider) {
            return Pending::ready(Err(seat_refused()));
        }
        // The device's one search, answered from its index as the caller may read it.
        if tilo_workspace_records::search::is_search(&request.target) {
            let Some(search) = self.workspaces.search() else {
                return Pending::ready(Err(ActionCallError::Unavailable {
                    reason: "this device holds no records to search".into(),
                }));
            };
            return tilo_workspace_records::search::serve(&*search, caller, &request);
        }
        // The device's own actions: what it is, to the person and the agents acting for them;
        // what lets another device in, to the person alone.
        if let Some(answered) = self.the_device_answers(caller, &request) {
            return answered;
        }
        // The person's shell runtime on this computer, for the Terminal windows it serves: never
        // an agent that runs elsewhere, nor an app.
        if let Some(answered) = self.shell_runtime_asks(caller, &request) {
            return answered;
        }
        if let Some(tickets) = &self.tickets
            && Tickets::answers(&request)
        {
            return self.ticket_answer(caller, tickets, request);
        }
        // An action of the person's own device, asked by them on their own daemon connection:
        // this device answers it, as it does before their services run, because what the action
        // changes is this computer's own and no workspace's. An agent, a key or another device is
        // answered on the workspace's admission, as before.
        //
        // The link the daemon made for the person's shell carries their own calls too, as the
        // plain person and never a named agent or an app: their shell's press of one of these
        // actions reaches the provider the same way their own connection does.
        let person = hex::encode(self.workspaces.device_cert().user_public_key);
        let the_persons_own = caller.is_own_connection()
            || (the_plain_person(caller) && caller.acting().person_id() == Some(person.as_str()));
        if the_persons_own
            && !caller.is_system()
            // Their answer to an asked record is the record's own action, which this device's
            // asked dispatch answers below: never one of the providers it serves.
            && !tilo_actions::remote::asked::is_resolve(&request.target)
            && let Some(registered) = self
                .workspaces
                .copies()
                .providers()
                .get(&request.target.provider)
        {
            let providers = self.workspaces.copies().providers();
            let described = providers
                .descriptor(
                    &registered,
                    caller,
                    &request.target.address,
                    request.argument.to_slot().as_deref(),
                )
                .is_some();
            // The person's own rules still decide it: the workspace's verdict, made with their
            // own connection answering what it asks of them, and never an approval this call
            // wrote.
            let verdict = self.broker.state().own_device_verdict(
                caller,
                &self.workspace,
                &registered,
                &request,
            );
            match verdict {
                // It runs, or waits for the person as its asked record where the provider asks
                // for their yes in its own call: a question a program raises, a press that asks.
                Some(Ok(()) | Err(ActionCallError::ConfirmationRequired(_))) => {
                    return self.answered_at_own_device(&registered, caller, request);
                }
                Some(Err(refused)) => return Pending::ready(Err(refused)),
                // A provider that lists no row for the action — a question it raises itself,
                // which nobody is offered — answers the person's own call, and refuses what it
                // does not serve.
                None if !described => {
                    return self.answered_at_own_device(&registered, caller, request);
                }
                None => {}
            }
        }
        let origin_writes = |caller: &Caller, origin: &Origin| {
            self.broker
                .state()
                .may_write_origin(caller, &self.workspace, origin)
        };
        if let Some(answered) = self.invocations.answer(caller, &request, &origin_writes) {
            // A record's own refusal is the call's answer, as a provider's refusal is: the
            // caller reads the words in a rejected receipt, never a dispatch that failed to
            // answer at all.
            let invocation = request.invocation;
            return answered.map(move |answer| match answer {
                Ok(receipt) => Ok(receipt),
                Err(error) => Ok(InvocationReceipt {
                    invocation,
                    status: ReceiptStatus::Rejected { error },
                }),
            });
        }
        // An approve only the device hands back: one a caller writes into its own call is
        // refused whole, nothing of it run.
        if let Err(forged) = tilo_actions::remote::asked::admit_frame(
            self.asked
                .as_ref()
                .map(|asked| asked.store() as &dyn tilo_actions::remote::asked::ApprovalsHanded),
            &request,
        ) {
            return Pending::ready(Err(forged));
        }
        if let Some(session) = &self.session {
            return self.admitted_session(caller, request, session);
        }
        Pending::ready(Err(ActionCallError::NotPermitted))
    }

    fn tool_started(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
        tool: &str,
    ) -> Option<RunningTool> {
        // Only an agent's session shares its calls this way: a person's own tool call is their
        // own act, not an agent's to list and stop.
        let session = self.session.as_ref()?;
        session.agent.as_ref()?;
        session.check(caller, &self.workspaces).ok()?;
        // The call is the agent's, as every call of its session is: its person reads it on
        // their invocations page and stops it from there.
        Some(
            self.invocations
                .tool_started(session.acting(), &self.workspace, *id, tool),
        )
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
        if matches!(scope, tilo_actions::remote::WatchScope::Records) {
            // The records a watch reads are the documents this device's service holds
            // ([`DeviceChanges`]): a write landing in any of them is news to it, whatever else
            // the session follows.
            let landed = self.next_write();
            return Some(match followed {
                Some(followed) => whichever_first(followed, landed),
                None => landed,
            });
        }
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

    /// Where the bytes a put for the action `target` names go: the store the provider that
    /// answers it keeps, which for one of this device's actions is the device's own, so a
    /// development build's files never reach a workspace every member's device carries.
    /// Everything else a put is for stays where [`Self::content_puts`] puts it.
    fn content_puts_for(
        &self,
        target: Option<&RemoteActionTarget>,
    ) -> Option<Arc<dyn tilo_actions::remote::ContentPuts>> {
        if let Some(content) = target.and_then(|target| self.device_content_puts(target)) {
            return Some(content);
        }
        self.content_puts()
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

    /// What the workspace keeps here, read back by the run this runtime admitted: what was put
    /// into it, and its files as the tree names them.
    fn content_gets(&self) -> Option<Arc<dyn tilo_actions::remote::ContentGets>> {
        Some(Arc::new(AdmittedContent {
            session: Arc::clone(self.session.as_ref()?),
            workspaces: Arc::clone(&self.workspaces),
            content: tilo_workspace_service::files::OpenedFiles::new(
                self.workspaces.copies().clone(),
                self.workspaces.content_shelf()?,
            )
            .into_gets(),
        }))
    }
}

impl RuntimeActionProvider {
    /// The device's own records, answer to `request`: its settings, written through the same
    /// actions before the person's services ran and after, once the device is theirs. `None` when
    /// the request is not one of them.
    fn device_records(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Option<Pending<Result<InvocationReceipt, ActionCallError>>> {
        if request.target.provider != super::records::PROVIDER
            || request.target.address.resource != super::records::RESOURCE
        {
            return None;
        }
        let records = self.workspaces.device_records()?;
        // A device placed for records of one workspace keeps no settings of its own: its host
        // keeps them in the workspace's environment record, deciding the write as this placed
        // device.
        if self.workspaces.confined().is_some() {
            return Some(match super::confined::forwarded(&self.workspaces, None) {
                Ok(host) => host.invoke(caller, request.clone()),
                Err(reason) => Pending::ready(Err(ActionCallError::Unavailable { reason })),
            });
        }
        // The device's own person, on the dispatch this device admitted them to, writes as the
        // device their certificate names: what their desktop keeps for its next session.
        let cert = self.workspaces.device_cert();
        let as_device = (self.is_the_person(caller)
            && caller.acting().person_id() == Some(hex::encode(cert.user_public_key).as_str()))
        .then(|| Caller::authenticated(cert.user_public_key, cert.device_public_key));
        Some(Pending::ready(Ok(
            records.receipt_for(as_device.as_ref().unwrap_or(caller), request)
        )))
    }

    /// The person's shell runtime on this computer's answer to `request`, for the Terminal windows
    /// it serves: `None` when the request is no ask of its, and the refusal of a caller that is not
    /// the person's own shell runtime.
    fn shell_runtime_asks(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Option<Pending<Result<InvocationReceipt, ActionCallError>>> {
        if !super::shell_runtime::asks(request) {
            return None;
        }
        if !self.is_own_client(caller) {
            return Some(Pending::ready(Err(ActionCallError::Refused {
                reason: "only the person's own shell runtime on this computer asks for \
                         Terminal's environments and shells"
                    .into(),
            })));
        }
        let Some(person) = self.admission_caller() else {
            return Some(Pending::ready(Err(ActionCallError::NotPermitted)));
        };
        Some(super::shell_runtime::answer(
            &super::shell_runtime::Asked {
                workspaces: &self.workspaces,
                dispatch: self.clone(),
                person,
                handed: self.handed.as_deref(),
            },
            request,
        ))
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
    policy_time: tilo_app_host::policy_clock::PolicyTime,
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
        let held = self.held()?;
        if let Some(confined) = held.confined() {
            return confined_admits(&held, confined, &name, cert);
        }
        if !cert.capabilities.reaches_workspace(&name)
            || !super::signing::devices_seen(&held).reaches(&cert.device_public_key, &name)
        {
            return Err("this device reaches another workspace alone".to_owned());
        }
        let id = WorkspaceId::parse(&name).ok_or("invalid exact workspace")?;
        let peer = held
            .peer(id)
            .ok_or("this device does not have that workspace")?;
        peer.permissions(cert.device_public_key, tilo_clock::wall())
            .map(drop)
            .map_err(|error| error.to_string())
    }
}

/// Whether a device confined to `confined` admits `cert` in the workspace `name` names: its own
/// device alone, in its one workspace, as what it knows of its person's devices lists it
/// ([`super::confined::trust_own`]). It keeps no copy whose member list could say more, and every call it sends there
/// is decided again by the host.
fn confined_admits(
    held: &ServiceWorkspaces,
    confined: WorkspaceId,
    name: &str,
    cert: &DeviceCert,
) -> Result<(), String> {
    use tilo_identity::sources::DeviceTrust as _;
    if WorkspaceId::parse(name) != Some(confined) {
        return Err("this device reaches another workspace alone".to_owned());
    }
    if cert.device_public_key != held.device_cert().device_public_key {
        return Err("a device confined to one workspace admits only its own apps there".to_owned());
    }
    let trust = devices_trust();
    if trust.reaches(cert, name) != Some(true)
        || trust.revoked(&cert.user_public_key, &cert.device_public_key) != Some(false)
    {
        return Err("the person's devices do not list this device in this workspace".to_owned());
    }
    Ok(())
}

impl tilo_app_host::certificate_authority::CertificateAuthority for Certificates {
    fn policy_time(&self) -> Result<Option<tilo_app_host::policy_clock::PolicyTime>, String> {
        self.policy_time
            .now_ns()
            .map_err(|error| error.to_string())?;
        Ok(Some(self.policy_time.clone()))
    }

    fn authorize_with_policy_time(
        &self,
        workspace: uuid::Uuid,
        cert: &DeviceCert,
        time: &tilo_app_host::policy_clock::PolicyTime,
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

    fn device(&self) -> Option<[u8; 32]> {
        Some(self.held().ok()?.device_cert().device_public_key)
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
    /// The workspace host this device runs, when it serves the workspace founded here or the
    /// workspaces placed on a server: the person's keys open their sessions with it here.
    host: Option<&'a tilo_workspace_service::service::WorkspaceService>,
    /// What a server answers for what it keeps.
    kept: crate::device::keep::server::Protocols,
    /// The terminal streams of the shells this device runs, which the devices it hands them to
    /// dial.
    terminals: TerminalProtocols,
    /// Another of the person's devices at early boot asking for its disk-unlock half, when this
    /// device keeps their root and the store the calls that wait for them are kept in.
    unlock: Option<Arc<super::unlock::Unlock>>,
    /// The device VPN, which answers the person's other devices on the endpoint.
    vpn: Option<super::capabilities::VpnServing>,
    /// The person's speakers, which carries their routes to their other devices on the endpoint,
    /// where this daemon's program mounted the fabric they travel on.
    speakers: Option<&'a Arc<super::speakers::Speakers>>,
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
        unlock,
        vpn,
        speakers,
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
    let protocols = terminals
        .into_iter()
        .chain(vpn.into_iter().flat_map(|vpn| vpn.protocols))
        .chain(
            speakers
                .into_iter()
                .flat_map(super::speakers::Speakers::protocols),
        );
    for (alpn, handler) in protocols {
        router = router.accept(alpn, handler);
    }
    if let Some(unlock) = unlock {
        router = router.accept(
            tilo_unlock_protocol::UNLOCK_ALPN,
            super::unlock::Protocol::new(unlock),
        );
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
    use tilo_app_host::agents;
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
fn start_agents(
    broker: &BrokerHandle,
    workspaces: &ServiceWorkspaces,
    tickets: &Arc<Tickets>,
    (asked, invocations): (&Arc<super::asked::DeviceAsked>, &Arc<Invocations>),
) {
    let Some(broker) = broker.app_action_broker() else {
        tracing::warn!("this device's broker is not up, so it runs no agents of its workspaces");
        return;
    };
    tilo_rt::app()
        .handle()
        .spawn(tilo_app_host::agents::run_on_device(
            workspaces.copies().clone(),
            Arc::new(super::turn_dispatch::TurnDispatch::new(
                broker,
                Arc::clone(asked),
                Arc::clone(invocations),
            )),
            Some(Arc::clone(tickets) as _),
        ));
}

/// Answer the workspaces' events on this device: each event is saved as a shortcut in
/// `shortcuts`.
fn serve_events(workspaces: &ServiceWorkspaces, shortcuts: &Arc<dyn ShortcutLibraryServices>) {
    Arc::new(WorkspaceEvents::new(
        workspaces.copies().clone(),
        Arc::clone(shortcuts),
    ))
    .register(&workspaces.copies().providers());
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
    tilo_rt::app()
        .handle()
        .spawn(tilo_app_host::triggers::run_on_device_watching_invocations(
            workspaces.copies().clone(),
            std::sync::Arc::new(broker),
            Arc::clone(invocations),
        ));
}

/// How often the daemon looks for what its apps raised, and for the answers to what it showed,
/// while nothing is written: what it shows the person is read again as their time there runs
/// out, and their answers are carried out within this.
#[cfg(not(windows))]
const NOTICES_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

/// Look after what this device's apps tell from now on, as their records change: where it serves
/// pages (`serves_pages`), carry their notices into the record of the person's devices and carry
/// out what the person answers of them there, as a device serving pages does in place of a
/// desktop session, which the person may not have anywhere; where the program mounted the
/// platform's wakeups (`alarms`), arm one for each moment an app's trigger lands on this device,
/// so it comes offline and asleep, and carry out the buttons the person presses on it.
#[cfg(not(windows))]
fn start_notices(
    workspaces: &Arc<ServiceWorkspaces>,
    apps: &Arc<apps::Apps>,
    serves_pages: bool,
    alarms: Option<Arc<dyn tilo_app_host::wakeups::Notifier>>,
) {
    use tilo_app_host::wakeups::{self, Wakeups};
    let copies = workspaces.copies().clone();
    let cert = workspaces.device_cert();
    let apps = Arc::clone(apps);
    tilo_rt::app().handle().spawn(async move {
        let mut notices = serves_pages.then(|| {
            tilo_app_host::served_notices::ServedNotices::new(
                copies.clone(),
                cert.device_public_key,
                cert.user_public_key,
            )
        });
        let mut wakeups = alarms.map(|alarms| {
            Wakeups::new(
                copies.clone(),
                alarms,
                cert.device_public_key,
                cert.user_public_key,
            )
        });
        // The buttons the person presses on a wakeup that rang, as the platform reports them.
        let (buttons, mut pressed) = tokio::sync::mpsc::unbounded_channel::<String>();
        let mut listening = wakeups.is_some();
        if listening {
            wakeups::set_activation_handler(wakeups::ACTIVATION, move |uri| {
                let _ = buttons.send(uri.to_owned());
            });
        }
        let mut writes = copies.document_writes();
        // The app records whose notices are read: every one the workspaces hold at the start, and
        // each written since.
        let mut pending: std::collections::BTreeSet<(String, String)> =
            std::collections::BTreeSet::new();
        let mut seeded = false;
        let mut packages = Packages::new();
        // What one pass looks at: every app record written since the last, and the ones the
        // workspaces hold the first time. A pass runs on the tick, so a burst of writes to one
        // record is read once, and an answer the person gave is carried out within the tick.
        let mut tick = tokio::time::interval(NOTICES_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                clicked = pressed.recv(), if listening => {
                    // A daemon started after this one hears the buttons from here on.
                    let Some(uri) = clicked else {
                        listening = false;
                        continue;
                    };
                    if let Some(wakeups) = wakeups.as_mut()
                        && let Err(error) = wakeups.chosen(&uri).await
                    {
                        tracing::warn!(%uri, %error, "a wakeup's button was not carried out");
                    }
                    continue;
                }
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
                pending.extend(records_held(&copies).await);
            }
            let now = tilo_clock::unix_now_ms_i64();
            // What rang here on its own since the last pass is not told again when it arrives.
            if let Some(wakeups) = wakeups.as_mut() {
                let rang = wakeups.look();
                if let Some(notices) = notices.as_mut() {
                    notices.rang_here(rang);
                }
            }
            for record in std::mem::take(&mut pending) {
                answer_a_record(
                    record,
                    &mut packages,
                    &copies,
                    &apps,
                    wakeups.as_mut(),
                    notices.as_mut(),
                    now,
                )
                .await;
            }
            if let Some(notices) = notices.as_mut()
                && let Err(error) = notices.follow(now).await
            {
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

/// The package each app's record is read through, by workspace and app: verified once and kept,
/// since an app's install does not change under a running daemon.
#[cfg(not(windows))]
type Packages =
    std::collections::BTreeMap<(String, String), tilo_workspace_service::tl_records::Package>;

/// Every app record the workspaces hold, as the first pass reads them.
#[cfg(not(windows))]
async fn records_held(
    copies: &tilo_workspace_service::workspaces::Workspaces,
) -> std::collections::BTreeSet<(String, String)> {
    let mut held = std::collections::BTreeSet::new();
    for workspace in copies.list().await {
        for (app, _) in copies
            .records(&workspace.workspace)
            .await
            .unwrap_or_default()
        {
            held.insert((workspace.workspace.clone(), app));
        }
    }
    held
}

/// Answer one app record a pass found: arm the moments its triggers land here on, and carry its
/// notices into the record of the person's devices. An app this device neither installed nor
/// carries runs nowhere here, so its record holds no notice this device could read.
#[cfg(not(windows))]
async fn answer_a_record(
    (workspace, app): (String, String),
    packages: &mut Packages,
    copies: &tilo_workspace_service::workspaces::Workspaces,
    apps: &apps::Apps,
    wakeups: Option<&mut tilo_app_host::wakeups::Wakeups>,
    notices: Option<&mut tilo_app_host::served_notices::ServedNotices>,
    now: i64,
) {
    use tilo_workspace_service::tl_records::Package;
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
    let readable = !matches!(package, Package::BuiltIn)
        || copies
            .tilolang()
            .is_some_and(|tilolang| tilolang.records.built_in(&app));
    if !readable {
        return;
    }
    let name = apps.display_name(&app);
    if let Some(wakeups) = wakeups
        && let Err(error) = wakeups
            .arm(
                &workspace,
                (app.as_str(), name.as_str()),
                Some(&package),
                now,
            )
            .await
    {
        tracing::warn!(
            %workspace,
            %app,
            %error,
            "an app's moments on this device are not armed"
        );
    }
    if let Some(notices) = notices
        && let Err(error) = notices
            .project(&workspace, &app, &name, Some(&package), now)
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

/// Answer the person's keys from the broker from now on, and serve the workspaces' pages
/// through the daemon's apps: the workspace founded here when this computer founded one
/// (`founded`), else every workspace a keeping server (`serving`) holds.
fn serve_key_sessions(
    key_sessions: &crate::key_sessions::KeySessions,
    (broker, invocations, apps): (&BrokerHandle, &Arc<Invocations>, &Arc<apps::Apps>),
    workspaces: &Arc<ServiceWorkspaces>,
    (actor, founded, serving): (&str, Option<collab_types::WorkspaceId>, bool),
    (policy_origin, first): (
        &tilo_app_host::policy_clock::PolicyOrigin,
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
            &workspaces.device_cert(),
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
        (caller, certificate): (&Caller, &DeviceCert),
    ) -> Result<
        tilo_server_protocol::page::window::WindowLink,
        tilo_server_protocol::page::PageRefusal,
    > {
        self.0
            .open_view((caller, Some(certificate)), app, workspace, None, None)
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

/// The relay the server this computer logged in to is reached through: the same relay its link to
/// that server goes through. `None` on a computer that logged in to nothing, whose endpoint relays
/// as the person set it.
fn login_relay(place: &tilo_device_identity::login::Place) -> Option<iroh::RelayUrl> {
    let device = tilo_device_identity::login::filed_entry(place)?;
    let url = device.server_address.as_ref()?.relay_url.as_deref()?;
    url.parse().ok()
}

/// Whether `relay` answers on this machine: an address of its loopback, or `localhost` and the
/// names under it, which every resolver here answers itself.
fn relay_on_this_machine(relay: &iroh::RelayUrl) -> bool {
    let Some(host) = relay.host_str() else {
        return false;
    };
    let host = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

/// How this device's endpoint is reached: through `relay`, the server this computer logged in to,
/// with the server's address directory and not number 0's; else through the person's relays and
/// their directory. A relay on this machine is reached from no router, so the endpoint asks none
/// for a port: its `UPnP` probe over `SSDP` would open one that nothing could use.
fn endpoint_reach(
    relay: Option<iroh::RelayUrl>,
    person: &tilo_endpoint_reachability::EndpointRelays,
) -> (iroh::RelayMode, iroh::endpoint::PortmapperConfig, bool) {
    match relay {
        Some(relay) => {
            let portmapper = if relay_on_this_machine(&relay) {
                iroh::endpoint::PortmapperConfig::Disabled
            } else {
                person.portmapper()
            };
            (
                iroh::RelayMode::Custom(iroh::RelayMap::from(relay)),
                portmapper,
                false,
            )
        }
        None => (
            iroh::RelayMode::Custom(person.relays.clone()),
            person.portmapper(),
            person.n0_dns,
        ),
    }
}

/// This device's endpoint, keyed by its device key: bound to `listen` on
/// every interface when a host names the port it is reached at, and to
/// whatever port the machine gives it otherwise. Its relays are the person's
/// ([`tilo_endpoint_reachability::relays`]) on a computer that logged in to nothing, and the
/// server's own (`relay`) on one that did: a runtime relays and resolves as its person's other
/// devices do, and a member as the server that reaches it does.
/// How long the person's part that starts again waits for the one that ended to let go of the
/// workspaces' port.
const PORT_LET_GO: std::time::Duration = std::time::Duration::from_secs(10);

pub(super) fn listen_endpoint(
    key: &SigningKey,
    listen: Option<u16>,
    relay: Option<iroh::RelayUrl>,
    lan: Option<&std::sync::Arc<super::lan::Lan>>,
) -> io::Result<iroh::Endpoint> {
    let person = tilo_endpoint_reachability::relays();
    let (relay_mode, portmapper, n0_dns) = endpoint_reach(relay, &person);
    let mut builder = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .portmapper_config(portmapper)
        .relay_mode(relay_mode)
        .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()));
    if n0_dns {
        builder = builder
            .address_lookup(tilo_endpoint_reachability::collaborating())
            .address_lookup(iroh::address_lookup::DnsAddressLookup::n0_dns());
    }
    // The local network answers for the keys this device dials, so two devices on one network
    // meet without a relay, and it answers only the devices the person's record admits.
    if let Some(lan) = lan {
        builder = builder.address_lookup(lan.lookup());
    }
    if let Some(port) = listen {
        // One program on a computer takes its workspaces' work on this port: a second runtime of a
        // login here is told so, not left with the socket layer's words. The person's part that
        // ended in this process lets go of it as its endpoint's last task ends, which the part
        // starting now waits for, a while.
        let started = std::time::Instant::now();
        let waits = if super::serve::PART_ENDED.load(std::sync::atomic::Ordering::Relaxed) {
            PORT_LET_GO
        } else {
            std::time::Duration::ZERO
        };
        let mut waited = false;
        while let Err(error) = std::net::UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, port))
            && error.kind() == io::ErrorKind::AddrInUse
        {
            if started.elapsed() >= waits {
                let holder = if waits.is_zero() {
                    "most likely another `tilo-runtime` of a Tilo login here. Stop it, then start \
                     this one again."
                        .to_owned()
                } else {
                    format!(
                        "this daemon's person's part that ended still held it after {} seconds",
                        waits.as_secs()
                    )
                };
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!(
                        "another program on this computer holds UDP port {port}, where a \
                         logged-in Tilo runtime takes its workspaces' work: {holder}"
                    ),
                ));
            }
            waited = true;
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        if waited {
            tracing::info!(
                waited_ms = started.elapsed().as_millis(),
                port,
                "the person's part that ended let go of the workspaces' port"
            );
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
        #[cfg(unix)]
        first: Arc::clone(first),
        dev,
    };
    // The person's workspace is founded behind an app's connection, which only a Unix socket
    // carries.
    #[cfg(not(unix))]
    let _ = first;
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
    /// The person's workspace here, which the first app this daemon launched in it settles.
    #[cfg(unix)]
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

    /// An app this daemon launched in the person's own workspace settles that workspace on a
    /// computer with no login, before it signs anything: the device is listed in it then. The
    /// app is this daemon's only by its exact launch record. Any other process, a connection the
    /// daemon made for no launch, or an app launched in another workspace, settles nothing, and
    /// nothing ever founds the workspace here.
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
        if let Err(error) = self.first.settle_if_theirs(&workspace) {
            tracing::warn!(%error, app_id, "the person's workspace is not settled for their app");
        }
    }

    fn attached(&self, app_id: &str, surface: &[u8]) {
        if self.dev
            && self.apps.declared_resource_kinds(app_id).is_none()
            && ui::app_surface::bytes_of(app_id).is_none()
        {
            // Changes are decided against the surface their op names, read
            // from the content store as a package's is: the compiled form, as
            // registering the package keeps it.
            if collab_types::surface::SurfaceFile::compiled(surface).is_err() {
                tracing::warn!(app_id, "the development surface is not the compiled form");
                return;
            }
            if let Err(error) = tilo_content_device::for_app().put(surface) {
                tracing::warn!(%error, app_id, "the development surface could not be stored");
                return;
            }
            tilo_app_host::register_package_surface(app_id, surface);
        }
    }
}

/// The id of the agent session `caller` works in, as its admission minted it: the one id its
/// invocations, its presence and its watch all name it by. An agent's own session where it names
/// one, else its execution's, as [`Caller::agent_session`] names it (a key's run included).
fn agent_session_id(caller: &Caller) -> io::Result<String> {
    if let Some(group) = caller.agent_session() {
        return Ok(group.id);
    }
    let execution = caller
        .execution_id()
        .ok_or_else(|| io::Error::other("this run was admitted with no execution of its own"))?;
    Ok(format!(
        "{}:{execution}",
        tilo_actions::context::AGENT_SESSION_GROUP
    ))
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
    let keys = tilo_app_host::agents::agent_keys::this_device().ok()?;
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
