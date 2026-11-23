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
use collab_types::workspace_grant::WorkspaceGrant;
use ed25519_dalek::SigningKey;
use serde_json::{Map, Value, json};
use std::{
    io,
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::{Arc, Mutex},
};
use tilo_actions::remote::invocation_record::Invocations;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus, RemoteActionMessage,
    RemoteActionTarget, device_agent_id,
    workspace_invocations::InvocationsQuery as WorkspaceInvocationsQuery,
    workspace_invocations::Origin,
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
    BRANCH_OWN_WORKSPACE, CREATE_OWN_WORKSPACE, CreatedWorkspace, DELETE_WORKSPACE, LIST_KEYS,
    REVOKE_KEY, RUN_UNATTENDED, WorkspaceLabels, WorkspaceListing,
};
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::agents::agent_keys::{AgentSigner, Signed, this_device};
use tilo_workspace_service::app_serving::AppActionServer as _;
use tilo_workspace_service::apps::{
    collab_broker::{AppActionBroker, BrokerHandle, FanoutState},
    collab_core::{BrokerChannels, BrokerSealChecks},
    collab_history::CollabHistoryStore,
    collab_outbound::PeerPublisher,
    services::{LauncherServices, ProcessServices, ShortcutLibraryServices, WorkspaceAuthority},
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
/// What the workspace founded on a computer with no login is called.
const FOUNDED_NAME: &str = "Home";
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
    /// The device actions this daemon answers: a ticket for a new computer, and a login with one.
    tickets: Arc<Tickets>,
    /// Every invocation this device was asked for, however it was asked.
    invocations: Arc<Invocations>,
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
        let (key, cert) = tilo_identity::trusted_device::current("").map_err(|error| {
            io::Error::new(io::ErrorKind::NotFound, format!("{SET_UP_FIRST} ({error})"))
        })?;
        Ok(Self { key, cert, place })
    }

    /// Whether a ticket made this computer a device of its person's, rather than it holding their
    /// root: it keeps their workspaces as a member then, and founds none of its own.
    fn joined(&self) -> bool {
        let holds_root = tilo_identity::store::identity_dir_from_env()
            .ok()
            .and_then(|dir| tilo_identity::store::resolve_active_identity(&dir))
            .and_then(|active| tilo_identity::store::load_metadata_from_path(&active).ok())
            .is_some_and(|active| {
                active.single_device && active.group_public_key == self.cert.user_public_key
            });
        !holds_root
    }

    /// The person's id, hex of their root, as which they act here.
    fn person(&self) -> String {
        hex::encode(self.cert.user_public_key)
    }
}

/// Found this computer's person: their root, kept here, certifies this device's key in `keys`.
///
/// # Errors
/// The identity cannot be made or kept.
pub fn found_a_person(keys: &tilo_device_identity::IdentityPaths) -> io::Result<()> {
    tilo_identity::provisioning::ensure_foreign_collaboration_identity(
        &tilo_identity::store::identity_dir_from_env()?,
        keys,
        tilo_identity::provisioning::IdentityStorage::for_session(),
    )?;
    Ok(())
}

impl Daemon {
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

    /// Start `command` as the person's shell, handed the link this daemon made for it
    /// ([`runtime_collab::wire::shell::SHELL_FD_ENV`]): the daemon starts the shell itself, so the
    /// link passes from it to that one process and to no other of the account.
    ///
    /// # Errors
    /// No link could be made, or the shell did not start.
    #[cfg(target_os = "linux")]
    pub fn start_shell(&self, mut command: Command) -> io::Result<Child> {
        use std::os::fd::AsRawFd as _;
        /// Where the shell finds its link.
        const SHELL_FD: i32 = 3;
        let link = self.shell_link()?;
        launcher::inherit::hand_down(&mut command, vec![(link.as_raw_fd(), SHELL_FD)], false);
        command.env(
            runtime_collab::wire::shell::SHELL_FD_ENV,
            SHELL_FD.to_string(),
        );
        let child = command.spawn()?;
        drop(link);
        Ok(child)
    }

    /// The workspaces this device holds, through its opened workspace service.
    #[must_use]
    pub fn workspaces(&self) -> &tilo_workspace_service::workspaces::Workspaces {
        self.workspaces.copies()
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
        let provider = self.provider_for(&self.workspace)?;
        let api = self.api()?;
        let workspace = self.workspace.clone();
        let joined =
            block_on_owned(async move { api.joined(&workspace, Arc::new(provider)).await })
                .map_err(io::Error::other)?;
        Ok((
            joined,
            Caller::local_person(&self.actor).in_workspace(&self.workspace),
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
        let home = self.home()?;
        let server = home.server_address.as_ref()?;
        let account = Arc::new(super::account::Account::at(
            &self.workspaces,
            &server.endpoint_addr,
            home.server_cert.device_public_key,
            &home.workspace,
        ));
        // The person's own workspace, whose roster lists every device of theirs, is the first
        // this device keeps: entered where the server says it is.
        let (own, asked) = (Arc::clone(&self.workspaces), Arc::clone(&account));
        tilo_rt::app().handle().spawn(async move {
            let Some(person) = WorkspaceId::parse(&person) else {
                return;
            };
            if own.keeps(person).await {
                return;
            }
            let entered = match asked.address(person).await {
                Ok(hint) => own.copies().hold_as_member(person, &hint).await.map(drop),
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
            let policy_time = daemon.policy_origin.capture().map_err(io::Error::other)?;
            let (host, caller) = daemon.action_provider()?.agent_session(
                &daemon.workspace,
                &daemon.actor,
                &policy_time,
                |person| {
                    (
                        tilo_actions::remote::device_agent_id(person, &name),
                        name.clone(),
                    )
                },
            )?;
            // The apps of the workspace: what the agent's task programs compile against, run in
            // this daemon's processes.
            let tasks = daemon.task_hosts(host.clone(), caller.clone(), &daemon.actor);
            let run = hex::encode(rand::random::<[u8; 8]>());
            Ok(tilo_mcp::McpServer::admitted(host, caller)?
                .with_agent_session(run)
                .with_task_hosts(tasks))
        })
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
        let identity = Identity::here()?;
        // A server keeps the workspaces placed on it: it takes no other server's work.
        let mut keeping = super::keep::Keeping::configured()?;
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
        let apps = Arc::new(apps::Apps::from_env(root, sockets, &workspace_id));
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
        let key_sessions = founded.map(|_| crate::key_sessions::KeySessions::new());
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
        let home = Arc::new(super::home::Home::new(
            Arc::clone(&workspaces),
            identity.cert.clone(),
            entered,
        ));
        let take_work = member
            .then(|| take_work::Server::from_login(home.get().as_ref()))
            .flatten();
        // What a server keeps beyond its copies, on the workspace service it governs.
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
                })?)
            }
            _ => None,
        };
        let (kept, kept_protocols) = kept.map_or((None, Vec::new()), |(kept, protocols)| {
            (Some(kept), protocols)
        });
        // This computer keeps the workspaces its person makes on it as their own, labelled for
        // their programs and listed by the account's keys. A computer that logged in reaches
        // those through the server that keeps them; one that founded its person here keeps them
        // itself.
        let own = identity.login.is_none();
        if own {
            workspaces.offer_workspaces_here(&identity.cert.device_label);
        }
        if let Some(founded) = founded {
            tilo_rt::app()
                .handle()
                .block_on(workspaces.found(founded, FOUNDED_NAME))
                .map_err(io::Error::other)?;
        }
        if own {
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
                let terminal = tilo_workspace_service::workspaces::terminal::TerminalService::new(
                    workspaces.copies().clone(),
                    host,
                    tilo_rt::app().handle().clone(),
                );
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
                Arc::new(super::person::root),
                founded.map(|workspace| super::ticket::Founded {
                    workspace: workspace.to_string(),
                    name: FOUNDED_NAME.to_owned(),
                }),
                Some(Arc::clone(&workspaces)),
                identity.place.clone(),
                actions.clone(),
            )
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
        personal.attach(workspaces.copies().clone(), tilo_rt::app().handle(), None);
        apps.device_grants().keep_in(std::sync::Arc::new(personal));
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
        if let Some(broker) = broker.app_action_broker() {
            broker.state().serve_invocations(Arc::clone(&invocations));
        }
        workspaces.serve_invocations(Arc::clone(&invocations));
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
                (&workspace_id, &actor),
                &policy_origin,
            )?;
        }
        start_agents(&broker, &workspaces);
        start_triggers(&broker, &workspaces);
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
        stage("serving the workspaces' trees to the files daemon");
        let files_tree = super::files_tree::serve(
            &tilo_device_identity::login::files_tree_socket().map_err(io::Error::other)?,
            &workspaces,
            files_daemons,
            identity_socket.clone(),
            (actor.clone(), identity.key.verifying_key().to_bytes()),
        );
        let daemon = Self {
            _files_tree: files_tree,
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
            _kept: kept,
            tickets,
            invocations,
            key: identity.key.clone(),
            cert: identity.cert,
            account: Mutex::new(None),
            api: std::sync::OnceLock::new(),
            agents: super::serve::Agents::default(),
            place,
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
            governs,
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
    Ok(ProcessServices {
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
        published: Some(Arc::new(super::server_settings::Published::new(
            super::server_settings::Served::this_device(),
        ))),
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
        ))),
        // The providers registered on this device ([`Daemon::provide`]): a call made here reaches
        // them through the same registry another device's call does.
        providers: Some(parts.workspaces.copies().providers()),
        ..ProcessServices::default()
    })
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

    fn workspace(&self) -> &str {
        &self.workspace
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
        self.apps.handed_connections(child.id(), app_id);
        Ok(child)
    }

    fn media(&self) -> Option<Arc<dyn tilo_mcp::MediaReader>> {
        Some(Arc::new(profile::BlobMedia::new(
            &self.blob_socket,
            &self.workspace,
        )))
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
            who_is_here: Arc::clone(&self.who_is_here),
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
    who_is_here: Arc<who_is_here::WhoIsHere>,
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
        })
    }

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
            let key = tilo_api::grant_key::GrantKey::held(
                key.clone(),
                cert.clone(),
                tilo_clock::unix_now_ns(),
            )
            .map_err(|refused| io::Error::other(refused.to_string()))?;
            let provider = daemon.provider_for_key(&daemon.workspace()?, &key)?;
            let caller = provider
                .admission_caller()
                .ok_or_else(|| io::Error::other("the key's run was not admitted"))?;
            tilo_mcp::McpServer::admitted(provider, caller)
        })
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
            processes,
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
            let hint = Daemon::account(self)
                .ok_or(NO_ACCOUNT_YET)?
                .address(id)
                .await?;
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

    fn account_here(&self) -> Option<&dyn tilo_api::session::AccountHere> {
        // A computer that logged in reaches the account through the server that keeps its
        // workspaces; one that founded its person here keeps them itself, and answers for them.
        if self.login.is_some() {
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
        tilo_api::session::Here::workspaces(self)
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

    fn keys(&self, key: GrantKey) -> AccountAnswer<Result<Vec<WorkspaceGrant>, ServerError>> {
        let workspaces = Arc::clone(&self.workspaces);
        let device = self.cert.clone();
        Box::pin(async move {
            known(&device, &key)?;
            let mut grants = Vec::new();
            for (workspace, _, _) in workspaces.held() {
                let answered = workspace_verb(
                    &workspaces,
                    &workspace.to_string(),
                    &key,
                    LIST_KEYS,
                    Map::new(),
                )
                .await;
                match answered {
                    Ok(answered) => {
                        if let Ok(listed) = serde_json::from_value::<ListedGrants>(answered) {
                            grants.extend(listed.grants);
                        }
                    }
                    Err(error) => {
                        tracing::info!(%error, workspace = %workspace, "a workspace's keys were not read");
                    }
                }
            }
            Ok(grants)
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
    tilo_server_protocol::workspaces::Refusal::from_reason(reason)
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

/// The keys a workspace records.
#[derive(serde::Deserialize)]
struct ListedGrants {
    grants: Vec<WorkspaceGrant>,
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
            // says what it is when its client does.
            if let Some(execution) = caller.execution_id() {
                provider.workspaces.copies().agent_opened(
                    &execution.to_string(),
                    &workspace,
                    tilo_mcp::session_api::UNNAMED_CLIENT,
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
        let (signer, agent) = this_device()
            .and_then(|keys| AgentSigner::grant(keys, workspace, (&agent, &name), person))
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

/// The answer to the person stopping the agent session `request` names in `workspace`, when
/// this device serves it: `None` for a session it does not serve, whose stop is somebody
/// else's to answer.
fn stop_session(
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
    workspace: &str,
    request: &InvokeRequest,
) -> Option<InvocationReceipt> {
    let tilo_actions::remote::InvokeArgument::Json(argument) = &request.argument else {
        return None;
    };
    let stop: tilo_actions::remote::mcp_server::AgentSessionRef =
        serde_json::from_value(serde_json::Value::Object(argument.clone())).ok()?;
    if !workspaces.stop_agent(workspace, &stop.session) {
        return None;
    }
    Some(InvocationReceipt {
        invocation: request.invocation,
        status: tilo_actions::remote::ReceiptStatus::Completed {
            output: tilo_actions::ActionOutput {
                value: Some(serde_json::Value::Null),
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
                &WorkspaceInvocationsQuery {
                    limit: tilo_actions::remote::workspace_invocations::MOST,
                    cursor: cursor.take(),
                    invocation: invocation.clone(),
                    ..WorkspaceInvocationsQuery::default()
                },
            )?;
            items.extend(page.items);
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => return Ok(items),
            }
        }
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
        // The person stops one of the agent sessions this device serves, as their card asks:
        // the session leaves who is here and answers no more.
        if request.target
            == tilo_actions::remote::mcp_server::target(tilo_actions::remote::mcp_server::STOP)
            && self.is_the_person(caller)
            && let Some(receipt) = stop_session(self.workspaces.copies(), &self.workspace, &request)
        {
            return Pending::ready(Ok(receipt));
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
            return tickets.invoke(&request);
        }
        let origin_writes = |caller: &Caller, origin: &Origin| {
            self.broker
                .state()
                .may_write_origin(caller, &self.workspace, origin)
        };
        if let Some(answered) = self.invocations.answer(caller, &request, &origin_writes) {
            return answered;
        }
        if let Some(session) = &self.session {
            if let Err(refusal) = session.check(caller, &self.workspaces) {
                return Pending::ready(Err(refusal.into()));
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
        if let Some(tickets) = &self.tickets
            && self.is_the_person(caller)
            && let Some(state) = tickets.observe(id)
        {
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

    /// The person this run's agent acts for confirms `request` on this
    /// device, as the broker asks them; the agent never confirms by its own
    /// word. A person acting themselves confirms on their own screen, so
    /// nothing answers for them here.
    fn confirm(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<Result<tilo_actions::remote::ConfirmedInvocation, ActionCallError>> {
        let Some(session) = &self.session else {
            return Pending::ready(Err(ActionCallError::confirmation_required()));
        };
        if session.agent.is_none() {
            return Pending::ready(Err(ActionCallError::confirmation_required()));
        }
        if let Err(refusal) = session.check(caller, &self.workspaces) {
            return Pending::ready(Err(refusal.into()));
        }
        session.provider.confirm(session.acting(), request)
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionCallError>> {
        if let Some(tickets) = &self.tickets
            && self.is_the_person(caller)
            && tickets.observe(id).is_some()
        {
            return Pending::ready(Err(ActionCallError::NotCancellable {
                reason: "a device action ends by itself".into(),
            }));
        }
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

    /// Records read through the one record API: the workspace's invocations, which this
    /// dispatch publishes itself ([`DispatchReads`]), and every app's record by the broker's
    /// reader.
    fn resource_reads(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceReads>> {
        Some(Arc::new(DispatchReads {
            broker: self.broker.clone(),
            workspace: self.workspace.clone(),
            workspaces: Arc::clone(&self.workspaces),
            session: self.session.clone(),
            invocations: Arc::clone(&self.invocations),
            records: self.broker.state().resource_reads(),
        }))
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

    /// This device governs the workspace founded on it, and no other.
    fn governs(&self, workspace: uuid::Uuid, device: Option<&[u8; 32]>) -> bool {
        let Ok(held) = self.held() else {
            return false;
        };
        device.is_none_or(|device| *device == held.device_cert().device_public_key)
            && held
                .founded()
                .is_some_and(|founded| founded.to_string() == workspace.to_string())
    }

    /// The device's own certificate, which it reads the workspace founded on it under.
    fn own_device(&self) -> Option<DeviceCert> {
        let held = self.held().ok()?;
        held.founded()?;
        Some(held.device_cert().clone())
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
fn start_agents(broker: &BrokerHandle, workspaces: &ServiceWorkspaces) {
    let Some(broker) = broker.app_action_broker() else {
        tracing::warn!("this device's broker is not up, so it runs no agents of its workspaces");
        return;
    };
    tilo_rt::app()
        .handle()
        .spawn(tilo_workspace_service::agents::run_on_device(
            workspaces.copies().clone(),
            broker.device_host(),
        ));
}

/// Run the triggers of the shortcuts the person saved in this device's workspaces: each firing
/// that is this device's turn starts on `broker`.
fn start_triggers(broker: &BrokerHandle, workspaces: &ServiceWorkspaces) {
    let Some(broker) = broker.app_action_broker() else {
        tracing::warn!("this device's broker is not up, so it starts no shortcut's trigger");
        return;
    };
    tilo_rt::app()
        .handle()
        .spawn(tilo_workspace_service::apps::triggers::run_on_device(
            workspaces.copies().clone(),
            std::sync::Arc::new(broker),
        ));
}

/// Answer the record actions of the workspace founded here (`workspace`, as
/// `actor`) for the person's keys from `broker` from now on. The host opened
/// before the broker and refused them as not running yet until now.
fn serve_key_sessions(
    key_sessions: &crate::key_sessions::KeySessions,
    (broker, invocations, apps): (&BrokerHandle, &Arc<Invocations>, &Arc<apps::Apps>),
    workspaces: &Arc<ServiceWorkspaces>,
    (workspace, actor): (&str, &str),
    policy_origin: &tilo_workspace_service::apps::policy_clock::PolicyOrigin,
) -> io::Result<()> {
    let broker = broker
        .app_action_broker()
        .ok_or_else(|| io::Error::other("runtime action broker is unavailable"))?;
    key_sessions.serve(
        Arc::new(PersonalDevices::new(
            workspaces.copies().clone(),
            workspaces.device_cert(),
        )),
        Arc::new(host_runs::HostRuns {
            provider: RuntimeActionProvider {
                broker,
                workspace: workspace.to_owned(),
                workspaces: Arc::clone(workspaces),
                apps: None,
                session: None,
                tickets: None,
                invocations: Arc::clone(invocations),
            },
            workspace: workspace.to_owned(),
            actor: actor.to_owned(),
            policy_time: policy_origin.capture().map_err(io::Error::other)?,
        }),
    );
    #[cfg(unix)]
    key_sessions
        .pages_served()
        .show_through(Arc::new(AppViews(Arc::clone(apps))));
    #[cfg(not(unix))]
    let _ = apps;
    Ok(())
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
