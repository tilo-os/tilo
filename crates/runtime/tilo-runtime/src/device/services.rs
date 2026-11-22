//! The services this computer's one daemon runs for the person: the shared action broker and
//! identity service the apps attach to, over the device's one workspace service.
//!
//! The daemon is the device: it acts as the device key and certificate this computer's login
//! keeps ([`tilo_device_identity::login`]), for the person that certificate names, in the
//! workspace the login names. A computer with no login acts as the identity founded on it
//! (`tilo login --new-identity`), in a workspace founded here whose member list names the person
//! as its owner. A computer of one account's that has neither, and was handed no ticket, founds
//! its person at its first start ([`found_a_person`]); Tilo OS's system tree, and a computer handed
//! a ticket, are served with the device key alone until a person's certificate names it
//! ([`super::unclaimed`]). Here the daemon
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
use std::{
    io,
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::{Arc, Mutex},
};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeRequest, Pending,
};
use tilo_api::daemon::DaemonPlace;
use tilo_desktop_model::WorkspaceId;
use tilo_device_identity::login::Device;
use tilo_identity::broker::{IdentityBrokerHandle, WorkspaceMembership};
use tilo_peer_state::owner_commit::OwnerCommitService;
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::apps::{
    collab_broker::{AppActionBroker, BrokerHandle, FanoutState},
    collab_core::{BrokerChannels, BrokerSealChecks},
    collab_history::CollabHistoryStore,
    collab_outbound::PeerPublisher,
    services::{LauncherServices, ProcessServices, ShortcutLibraryServices, WorkspaceAuthority},
};
use tilo_workspace_service::workspaces::Refusal;

/// Durable collaboration history, where the desktop session keeps it under
/// the profile's `XDG_STATE_HOME`.
const COLLABORATION_HISTORY: &str = "state/tilo/collaboration/history";
/// The broker's durable invocation journal: a retried invocation id never
/// starts twice, across restarts too.
pub(super) const INVOCATION_JOURNAL: &str = "invocations";
/// Where a computer with no login keeps the id of the workspace founded on it for a person, in
/// the device's directory: this, then the person's key in hex. A root that comes to this computer
/// later founds a workspace of its own, never enters one another person founded here.
const FOUNDED: &str = "workspace-of-";
/// Where the device's workspace service and key service keep their state, in the device's
/// directory.
const OWN: &str = "own";
/// What the workspace founded on a computer with no login is called.
const FOUNDED_NAME: &str = "Home";
/// What a daemon says on a computer that holds no Tilo identity and has no login.
pub const SET_UP_FIRST: &str = "This computer has no Tilo identity yet: set up Tilo first, with \
     `tilo login` and the code from your Tilo tab (Set up with your agent).";
/// How long a daemon with a login waits for the endpoint its server is reached from before it
/// binds one of its own: a server that is not reachable now is reached later from either.
const BINDING: std::time::Duration = std::time::Duration::from_secs(10);

/// This computer's daemon, as the device. Drop applications first.
pub struct Daemon {
    // Retired before broker fields: retained original ownership cannot survive
    // replacement of the runtime that supplied its policy time.
    policy_origin: tilo_workspace_service::apps::policy_clock::PolicyOrigin,
    broker: BrokerHandle,
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
    _media: profile::MediaHost,
    /// The endpoint's router, held so the workspace host, the device actions, the work this
    /// device takes and a ticket's certify call answer for as long as this runtime runs.
    _endpoint_router: iroh::protocol::Router,
    /// The person's other devices, asked from this runtime's endpoint as this device.
    other_devices: Option<crate::other_devices::OtherDevices>,
    /// The apps installed for this profile: what each publishes, before it runs here.
    apps: Arc<apps::Apps>,
    /// The login this daemon serves, when this computer logged in.
    login: Option<(SigningKey, Device)>,
    /// The device actions this daemon answers: a ticket for a new computer, and a login with one.
    tickets: Arc<Tickets>,
    /// The device this daemon is, as the API it serves locally acts.
    key: SigningKey,
    cert: DeviceCert,
    /// The account's own actions, forwarded over the login's one session.
    account: std::sync::Mutex<Option<Arc<tilo_api::server::Connection>>>,
    place: DaemonPlace,
    _lock: advisory_lock::FileLock,
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

/// Who this device is: its key and certificate, and the login they come from, when there is one.
struct Identity {
    key: SigningKey,
    cert: DeviceCert,
    login: Option<(SigningKey, Device)>,
    /// Where the login keeps this computer's device: its key and certificates, and its directory.
    place: tilo_device_identity::login::Place,
}

impl Identity {
    /// The login's device, or, with no login, the identity founded on this computer.
    ///
    /// # Errors
    /// Neither names this computer's device key ([`SET_UP_FIRST`]), or the login is damaged.
    fn here() -> io::Result<Self> {
        let place = tilo_device_identity::login::Place::here(tilo_api::client::device_keys())
            .map_err(io::Error::other)?;
        // A login that ended, on a computer that holds its person's root, is no login: the
        // computer is that person's own device, serves itself and adds their others.
        let login = tilo_device_identity::login::load(&place)
            .map_err(io::Error::other)?
            .filter(|(_, device)| {
                let ended = !device.reaches_a_server()
                    && super::person::holds_root_of(&device.cert.user_public_key);
                if ended {
                    tracing::info!("this computer's login has ended and it holds the person's root: it serves as their own device");
                }
                !ended
            });
        let (key, cert) = if let Some((key, device)) = &login {
            adopt_login(&place, device);
            (key.clone(), device.cert.clone())
        } else {
            tilo_identity::broker::trusted_current_device("").map_err(|error| {
                io::Error::new(io::ErrorKind::NotFound, format!("{SET_UP_FIRST} ({error})"))
            })?
        };
        Ok(Self {
            key,
            cert,
            login,
            place,
        })
    }

    /// The person's id, hex of their root, as which they act here.
    fn person(&self) -> String {
        hex::encode(self.cert.user_public_key)
    }
}

/// Whether this daemon founds its person at its start, while no person's certificate names its
/// key: on a computer of one account's (not Tilo OS's system tree) that was handed no ticket, as a
/// laptop's first `tilo` command is.
#[must_use]
pub(crate) fn founds_a_person() -> bool {
    let handed = std::env::var_os("CREDENTIALS_DIRECTORY").is_some_and(|dir| {
        Path::new(&dir)
            .join(super::unclaimed::TICKET_CREDENTIAL)
            .exists()
    });
    !handed
        && tilo_identity::provisioning::collaboration_identity_root()
            != Path::new(tilo_device_identity::persist::SYSTEM_IDENTITY_ROOT)
}

/// Found this computer's person: their root, kept here, certifies this device's key.
///
/// # Errors
/// The identity cannot be made or kept.
pub(crate) fn found_a_person() -> io::Result<()> {
    tilo_identity::provisioning::ensure_foreign_collaboration_identity(
        &tilo_identity::store::identity_dir_from_env()?,
        &tilo_identity::provisioning::collaboration_identity_paths(),
        tilo_identity::provisioning::IdentityStorage::for_session(),
    )?;
    Ok(())
}

/// The login's device as this computer's enrolment: its certificate filed as this device's, and
/// its person as the account's identity, so the identity service signs for apps here as the
/// login's person, as on a device enrolled any other way. A computer that holds the person's root
/// is theirs already. One enrolled for another person (the root it made for itself before it
/// logged in, or an earlier login's) becomes the login's person's: they just chose it.
fn adopt_login(place: &tilo_device_identity::login::Place, device: &Device) {
    if device.cert.capabilities.can_admit_devices {
        return;
    }
    let enrolled = tilo_identity::store::identity_dir_from_env().and_then(|dir| {
        tilo_identity::provisioning::adopt_login_certificate(
            &dir,
            place.keys(),
            &device.cert,
            tilo_clock::unix_now_ns(),
        )
    });
    if let Err(error) = enrolled {
        tracing::warn!(%error, "the login's device is not this computer's enrolment: apps here cannot sign as its person");
    }
}

impl Daemon {
    /// The workspaces this device holds, through its opened workspace service.
    #[must_use]
    pub fn workspaces(&self) -> &tilo_workspace_service::workspaces::Workspaces {
        self.workspaces.copies()
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

    /// The login this daemon serves, when this computer logged in.
    pub(crate) fn login(&self) -> Option<&(SigningKey, Device)> {
        self.login.as_ref()
    }

    /// Tilo's API as this daemon: its own workspaces answered locally, and the account forwarded
    /// over the login's one session when `keep_account` gave it one.
    ///
    /// # Errors
    /// The API server cannot be built.
    pub(crate) fn api(self: &Arc<Self>) -> io::Result<Arc<tilo_api::direct::Direct>> {
        let here = Arc::clone(self) as Arc<dyn tilo_api::session::Here>;
        Ok(Arc::new(
            tilo_api::direct::Direct::here(
                here,
                self.key.clone(),
                self.cert.clone(),
                &self.cert.device_label,
            )
            .map_err(io::Error::other)?,
        ))
    }

    /// Answer the account's own actions over `connection`, the login's one session with its
    /// server: the daemon's workspaces are answered locally, the account is not its to keep.
    pub(crate) fn keep_account(&self, connection: Arc<tilo_api::server::Connection>) {
        *self
            .account
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(connection);
    }

    /// Where this daemon's connections reach it.
    pub(crate) fn place(&self) -> &DaemonPlace {
        &self.place
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one start, each service opened in the order the next ones need it"
    )]
    fn start(
        root: &Path,
        stage: &mut dyn FnMut(&str),
        serving: Serving,
        locked: Option<([u8; 32], advisory_lock::FileLock)>,
    ) -> io::Result<Self> {
        let identity = Identity::here()?;
        let take_work =
            take_work::Server::from_login(identity.login.as_ref().map(|(_, device)| device));
        tilo_files::native::private::directory(identity.place.dir())?;
        let place = DaemonPlace::of(
            identity.place.dir(),
            &identity.key.verifying_key().to_bytes(),
        )
        .map_err(io::Error::other)?;
        let lock = match locked {
            Some((device, lock)) if device == identity.key.verifying_key().to_bytes() => lock,
            Some(_) => {
                return Err(io::Error::other(
                    "the device key this daemon locked at its start is not the one its person's \
                     certificate names",
                ));
            }
            None => {
                stage("locking the device");
                super::serve::lock_device(&place)?
            }
        };
        tilo_files::native::private::directory(root)?;
        let dev = matches!(serving, Serving::Dev);
        let actor = identity.person();
        let (workspace_id, founded) = if let Some((_, device)) = &identity.login {
            (device.workspace.clone(), None)
        } else {
            let founded = founded_workspace(identity.place.dir(), &identity.cert.user_public_key)?;
            (founded.to_string(), Some(founded))
        };
        let apps = Arc::new(apps::Apps::from_env(root, &workspace_id));
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
        let member = take_work.is_some();
        let port = configured_port()?.or_else(|| member.then_some(take_work::PORT));
        let endpoint = endpoint(&identity, port, member)?;
        let workspaces = open_workspaces(
            &identity,
            endpoint,
            Arc::clone(&brokers),
            key_sessions.as_ref(),
        )?;
        if let Some(founded) = founded {
            tilo_rt::app()
                .handle()
                .block_on(workspaces.found(founded, FOUNDED_NAME))
                .map_err(io::Error::other)?;
        }
        // What the person granted apps in a workspace, as the service that holds its governance
        // answers: what every launch here hands the app it starts.
        apps.keep_launch_grants(Arc::clone(&workspaces) as Arc<dyn apps::LaunchGrantsSource>);
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
        // The device actions the daemon answers itself.
        stage("opening the device's tickets");
        let tickets = Arc::new({
            let _entered = tilo_rt::app().handle().enter();
            Tickets::new(
                (identity.key.clone(), identity.cert.clone()),
                workspaces.endpoint(),
                identity.login.clone(),
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
        stage("serving the device's endpoint");
        let endpoint_router = serve_device_endpoint(EndpointServing {
            workspaces: &workspaces,
            take_work,
            actions,
            host: founded.map(|_| workspaces.service().as_ref()),
        });
        // What is the person's alone is kept in their own workspace, read and written here on
        // this daemon's runtime once its service holds it: their devices, and the answers they
        // gave about this device's camera and microphone, which their other devices see and can
        // take back.
        let personal = tilo_workspace_service::workspaces::PersonResources::default();
        personal.attach(workspaces.copies().clone(), tilo_rt::app().handle(), None);
        apps.device_grants().keep_in(std::sync::Arc::new(personal));
        let identity_socket = root.join(IDENTITY_SOCKET);
        stage("starting the identity service");
        let identity_broker = identity_service(
            identity_socket.clone(),
            Arc::clone(&workspaces) as Arc<dyn WorkspaceMembership>,
            Arc::new(workspaces.keys()),
            &apps,
            dev,
        )?;
        stage("opening the content store");
        let media = profile::MediaHost::start(&root.join("content"), &root.join(BLOB_SOCKET))?;
        stage("starting the collaboration broker");
        let (policy_origin, broker) = start_broker(
            BrokerParts {
                root,
                apps: &apps,
                workspaces: &workspaces,
                keys: &workspaces.keys(),
                creation: Arc::clone(&creation),
                person: actor.clone(),
            },
            brokers,
            fanout,
        )?;
        // An action an agent makes for the person, which asks for
        // confirmation, is put to them in the runtime's native prompt.
        // Elsewhere nobody here answers, and such an action is refused.
        #[cfg(target_os = "macos")]
        super::confirm::ask_the_person(&broker, &actor, &apps);
        if let Some(key_sessions) = &key_sessions {
            serve_key_sessions(
                key_sessions,
                &broker,
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
        Ok(Self {
            policy_origin,
            broker,
            apps,
            other_devices: other_devices(&workspaces),
            workspaces,
            _identity: identity_broker,
            workspace: workspace_id,
            actor,
            identity_socket,
            blob_socket: root.join(BLOB_SOCKET),
            replicas,
            _media: media,
            _endpoint_router: endpoint_router,
            login: identity.login,
            tickets,
            key: identity.key.clone(),
            cert: identity.cert,
            account: std::sync::Mutex::new(None),
            place,
            _lock: lock,
        })
    }
}

/// The workspace founded on this computer for `person`, as it has no login: the one an earlier
/// start founded for them, kept in the device's directory `dir`, or a new one.
fn founded_workspace(dir: &Path, person: &[u8; 32]) -> io::Result<WorkspaceId> {
    let path = dir.join(format!("{FOUNDED}{}", hex::encode(person)));
    match std::fs::read_to_string(&path) {
        Ok(kept) => WorkspaceId::parse(kept.trim()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} names no workspace", path.display()),
            )
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let founded = WorkspaceId::new();
            tilo_files::native::atomic::write_with_mode(
                &path,
                founded.to_string().as_bytes(),
                0o600,
            )?;
            Ok(founded)
        }
        Err(error) => Err(error),
    }
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

/// This device's endpoint, keyed by its device key and bound on UDP `port` when one is fixed: the
/// one its login's server is reached from, or, with no login or for a member that takes work, one
/// bound as [`listen_endpoint`] binds it.
fn endpoint(identity: &Identity, port: Option<u16>, member: bool) -> io::Result<iroh::Endpoint> {
    let Some((key, device)) = identity.login.as_ref().filter(|_| !member) else {
        return listen_endpoint(&identity.key, port);
    };
    let bound = tilo_rt::app().handle().block_on(async {
        let server = tilo_api::daemon::enter::server_of(device).await?;
        tokio::time::timeout(BINDING, server.bind_at(Some(key.to_bytes()), port))
            .await
            .map_err(|_| anyhow::anyhow!("the endpoint did not bind in time"))?
            .map_err(|error| anyhow::anyhow!("{error}"))
    });
    match bound {
        Ok(endpoint) => Ok(endpoint),
        Err(error) => {
            tracing::warn!(%error, "the login's server is not reached now; this device relays as the person's other devices do");
            listen_endpoint(&identity.key, port)
        }
    }
}

/// Open the device's workspace service in its directory: it governs the workspace founded here
/// on a computer with no login, and holds the member copies of the login's workspaces otherwise.
fn open_workspaces(
    identity: &Identity,
    endpoint: iroh::Endpoint,
    brokers: Arc<Mutex<BrokerChannels>>,
    key_sessions: Option<&Arc<crate::key_sessions::KeySessions>>,
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
    let governs = identity.login.is_none();
    block_on_owned(async move {
        ServiceWorkspaces::open(Opening {
            dir: &dir,
            key: &key,
            identity: device,
            endpoint,
            governs,
            seal_checks: Arc::new(BrokerSealChecks(brokers)),
            apps,
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
    apps: &'a Arc<apps::Apps>,
    workspaces: &'a Arc<ServiceWorkspaces>,
    keys: &'a tilo_workspace_service::apps::workspace_keys::KeyProvider,
    creation: Arc<RecordCreation>,
    /// The person this device acts for.
    person: String,
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
        parts.root.join(COLLAB_SOCKET),
        Some(Arc::clone(parts.workspaces) as Arc<dyn WorkspaceAuthority>),
        PeerPublisher::with_optional_owner_commit(
            None,
            ServiceCommits::new(parts.workspaces, Arc::clone(&parts.creation))
                as Arc<dyn OwnerCommitService>,
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
    let document_content =
        super::content::document_content(parts.workspaces, &parts.root.join(BLOB_SOCKET));
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
        document_content: Some(document_content),
        // The person's shortcuts, saved into the workspaces this device holds and run here: the
        // same store the trigger runner arms them from, so a computer keeps its shortcuts with
        // no desktop session open on it.
        shortcut_library: Some(Arc::new(super::shortcuts::Shortcuts::new(
            parts.workspaces.copies().clone(),
            parts.workspaces.device_cert().clone(),
            parts.workspaces.signer(),
        )?) as Arc<dyn ShortcutLibraryServices>),
        // The service the identity service answers key requests from:
        // endpoints ask again when it says a channel changed.
        group_keys: Some(parts.keys.clone()),
        action_offload,
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
        locked: ([u8; 32], advisory_lock::FileLock),
    ) -> io::Result<Self> {
        let serving = if dev { Serving::Dev } else { Serving::Profile };
        Self::start(root, stage, serving, Some(locked))
    }
}

impl RuntimeServices for Daemon {
    type ActionProvider = RuntimeActionProvider;

    fn start_reporting(root: &Path, stage: &mut dyn FnMut(&str)) -> io::Result<Self> {
        Self::start(root, stage, Serving::Profile, None)
    }

    fn start_in_dev_mode(root: &Path, stage: &mut dyn FnMut(&str)) -> io::Result<Self> {
        Self::start(root, stage, Serving::Dev, None)
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
        host::launch(
            command,
            app_id,
            &self.workspace,
            &host::ServiceSockets {
                collab: self.broker.app_socket(),
                identity: &self.identity_socket,
                blobs: &self.blob_socket,
            },
        )
    }

    fn media(&self) -> Option<Arc<dyn tilo_mcp::MediaReader>> {
        Some(Arc::new(profile::BlobMedia::new(
            &self.blob_socket,
            &self.workspace,
        )))
    }

    fn other_devices(&self) -> Option<crate::other_devices::OtherDevices> {
        self.other_devices.clone()
    }
}

impl Daemon {
    /// The person's provider in `workspace`: every action of one answered locally, as apps and the
    /// person's input are.
    ///
    /// # Errors
    /// The workspace is not one this device may act in, or its admission cannot be opened.
    fn provider_for(&self, workspace: &str) -> io::Result<RuntimeActionProvider> {
        let workspaces = Arc::clone(&self.workspaces);
        let actor = self.actor.clone();
        let named = workspace.to_owned();
        let admission =
            block_on_owned(async move { workspaces.admit_execution(&named, &actor).await })
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
                admission,
                agent: None,
                projection: Some(Caller::local_person(&self.actor).in_workspace(workspace)),
            })),
            tickets: Some(Arc::clone(&self.tickets)),
        })
    }
}

impl tilo_api::session::Here for Daemon {
    fn workspaces(&self) -> Vec<tilo_api::session::Workspace> {
        self.workspaces
            .workspaces()
            .into_iter()
            .map(|workspace| tilo_api::session::held(workspace.id().to_string(), workspace.name))
            .collect()
    }

    fn enter(&self, workspace: &str) -> Result<tilo_api::session::Reached, String> {
        let provider = self
            .provider_for(workspace)
            .map_err(|error| error.to_string())?;
        // The run is the person's own, admitted beside the workspace's broker: the session keeps
        // that admission rather than minting another.
        let caller = provider
            .admission_caller()
            .ok_or_else(|| "this daemon has no person's admission".to_owned())?;
        let catalogue = tilo_mcp::task_hosts::WorkspaceCatalogue::new(provider.clone(), caller);
        Ok(tilo_api::session::Reached {
            provider: Arc::new(provider),
            person: self.actor.clone(),
            tasks: Arc::new(tilo_mcp::task_hosts::TaskHosts::new(
                Some(&self.actor),
                Arc::new(catalogue),
            )),
        })
    }

    fn connection(&self) -> Option<Arc<tilo_api::server::Connection>> {
        self.account
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
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
}

struct AdmittedRuntimeSession {
    policy_time: tilo_workspace_service::apps::policy_clock::PolicyTime,
    provider: Arc<dyn ActionProvider + Send + Sync>,
    admission: Arc<tilo_workspace_service::workspaces::SessionAdmission>,
    /// The agent the person's device handed this run to.
    agent: Option<String>,
    projection: Option<Caller>,
}
impl AdmittedRuntimeSession {
    /// The run's caller as the broker admits it: the agent, for the person.
    fn acting(&self) -> Caller {
        let caller = self.admission.caller().clone();
        match &self.agent {
            Some(agent) => caller.delegated_to(agent.clone()),
            None => caller,
        }
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
            answer.map(|page| super::ticket::with_device_actions(page, &query, &workspace))
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
            // The MCP client is an agent the person's device hands this
            // session to; the admission stays the person's.
            let (session_host, caller, agent) =
                provider.agent_session(&workspace, &actor, &policy_time, tilo_mcp::agent_id)?;
            Ok(tilo_mcp::McpServer::admitted(session_host, caller)?.with_agent_session(agent))
        })
    }

    /// A run of an agent the person's device hands it, in `workspace`, as
    /// `actor`: the provider its calls go through, the caller its admission is,
    /// and the agent `agent` names for that caller.
    pub(super) fn agent_session(
        &self,
        workspace: &str,
        actor: &str,
        policy_time: &tilo_workspace_service::apps::policy_clock::PolicyTime,
        agent: impl FnOnce(&Caller) -> String,
    ) -> io::Result<(Self, Caller, String)> {
        let workspaces = Arc::clone(&self.workspaces);
        let (workspace, actor) = (workspace.to_owned(), actor.to_owned());
        let admission =
            block_on_owned(async move { workspaces.admit_execution(&workspace, &actor).await })
                .map_err(io::Error::other)?;
        let caller = admission.caller().clone();
        let agent = agent(&caller);
        let qualified = self
            .broker
            .certified_local_agent_provider(&caller, admission.presentation(), &agent)
            .map_err(io::Error::other)?;
        admission
            .check(self.workspaces.copies())
            .map_err(io::Error::other)?;
        let run = Self {
            broker: self.broker.clone(),
            workspace: self.workspace.clone(),
            workspaces: self.workspaces.clone(),
            apps: self.apps.clone(),
            session: Some(Arc::new(AdmittedRuntimeSession {
                policy_time: policy_time.clone(),
                provider: qualified,
                admission,
                agent: Some(agent.clone()),
                projection: None,
            })),
            tickets: None,
        };
        Ok((run, caller, agent))
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

impl ActionProvider for RuntimeActionProvider {
    fn installed_surface(&self, app_id: &str) -> tilo_actions::remote::Pending<Option<Vec<u8>>> {
        tilo_actions::remote::Pending::ready(
            self.apps
                .as_ref()
                .and_then(|apps| apps.installed_surface(app_id)),
        )
    }

    fn retire_execution(&self, caller: &Caller) {
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

    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
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
            let answer = self.broker.state().action_query(&session.acting(), &query);
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
        if let Some(tickets) = &self.tickets
            && self.is_the_person(caller)
            && Tickets::answers(&request)
        {
            return tickets.invoke(&request);
        }
        if let Some(session) = &self.session {
            if let Err(refusal) = session.check(caller, &self.workspaces) {
                return Pending::ready(Err(refusal.into()));
            }
            return session.provider.invoke(session.admission.caller(), request);
        }
        Pending::ready(Err(ActionCallError::NotPermitted))
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        if let Some(tickets) = &self.tickets
            && self.is_the_person(caller)
            && let Some(state) = tickets.observe(id)
        {
            return Some(state);
        }
        if let Some(session) = &self.session {
            session.check(caller, &self.workspaces).ok()?;
            return session.provider.observe(session.admission.caller(), id);
        }
        if !self.admitted(caller) {
            return None;
        }
        self.broker
            .state()
            .action_observe(&on_this_device(caller), id)
    }

    /// Who is in the workspace, as the desktop's roster names them: routed
    /// and authorized exactly as [`Self::observe`], and empty for a host
    /// that keeps no roster.
    fn here(&self, caller: &Caller) -> Option<Vec<tilo_actions::remote::Here>> {
        if let Some(session) = &self.session {
            session.check(caller, &self.workspaces).ok()?;
            return session.provider.here(session.admission.caller());
        }
        if !self.admitted(caller) {
            return None;
        }
        self.broker.state().here(&on_this_device(caller))
    }

    /// Where the session's workspace stands, for a caller of it: authorized
    /// exactly as [`Self::here`]. A runtime that serves from no session says
    /// nothing.
    fn session_state(&self, caller: &Caller) -> Option<tilo_actions::remote::SessionState> {
        let session = self.session.as_ref()?;
        session.check(caller, &self.workspaces).ok()?;
        session.provider.session_state(session.admission.caller())
    }

    fn session_state_changed(&self, caller: &Caller) -> Option<Pending<()>> {
        let session = self.session.as_ref()?;
        session.check(caller, &self.workspaces).ok()?;
        session
            .provider
            .session_state_changed(session.admission.caller())
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
        if let Some(session) = &self.session {
            session.check(caller, &self.workspaces).ok()?;
            return session.provider.changed(session.admission.caller(), scope);
        }
        if !self.admitted(caller) {
            return None;
        }
        self.broker.state().changed(&on_this_device(caller), scope)
    }

    /// Routed and authorized exactly as [`Self::observe`]: a caller that may
    /// not observe an invocation may not wait on it either.
    fn next_change(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<Pending<()>> {
        if let Some(session) = &self.session {
            session.check(caller, &self.workspaces).ok()?;
            return session.provider.next_change(session.admission.caller(), id);
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
        session
            .provider
            .confirm(session.admission.caller(), request)
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
        if let Some(session) = &self.session {
            if let Err(refusal) = session.check(caller, &self.workspaces) {
                return Pending::ready(Err(refusal.into()));
            }
            return session.provider.cancel(session.admission.caller(), id);
        }
        if !self.admitted(caller) {
            return Pending::ready(Err(ActionCallError::UnknownInvocation));
        }
        self.broker
            .state()
            .action_cancel(&on_this_device(caller), id)
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
    /// The workspace host this device runs, when it governs the workspace founded here: the
    /// person's keys open their sessions with it here.
    host: Option<&'a tilo_workspace_service::service::WorkspaceService>,
}

/// Serve what this runtime answers on the endpoint its workspaces run, from one router: the
/// workspaces this device keeps, its host, the work it takes and a ticket's certify call. An
/// endpoint's protocols are fixed once bound, so everything it answers is accepted here.
///
/// # Errors
///
/// The workspaces this device keeps cannot be served.
fn serve_device_endpoint(serving: EndpointServing<'_>) -> iroh::protocol::Router {
    let EndpointServing {
        workspaces,
        take_work,
        actions,
        host,
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
    router = router.accept(tilo_peer_state::actions::ACTIONS_ALPN, actions);
    router.spawn()
}

/// The person's other devices, asked over the action frame from `workspaces`' endpoint as this
/// device, in the workspaces it holds.
fn other_devices(
    workspaces: &Arc<ServiceWorkspaces>,
) -> Option<crate::other_devices::OtherDevices> {
    let (actions, endpoint, runtime) = workspaces.turn_action_service()?;
    let cert = workspaces.device_cert();
    Some(crate::other_devices::OtherDevices::new(
        actions,
        endpoint,
        runtime,
        (cert.user_public_key, cert.device_public_key),
        Arc::new(Held(Arc::downgrade(workspaces))),
    ))
}

/// The workspaces this device holds, as its other devices are looked up in them. Whether one of
/// them is connected here now is not known: each is asked, and says so when it is away.
struct Held(std::sync::Weak<ServiceWorkspaces>);

impl crate::other_devices::Whereabouts for Held {
    fn workspaces(&self) -> Vec<String> {
        self.0.upgrade().map_or_else(Vec::new, |workspaces| {
            workspaces
                .held()
                .into_iter()
                .map(|(workspace, ..)| workspace.to_string())
                .collect()
        })
    }

    fn reachable(&self, _workspace: &str, _device: [u8; 32]) -> bool {
        true
    }
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

/// Where the files of the workspaces this device holds are shown, each mirrored at a folder in
/// this user's home. When it `takes` work, the files of every workspace it holds are held from
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
            credentials: Box::new(move |workspace| {
                held.upgrade()
                    .ok_or_else(|| io::Error::other("the workspace service closed"))?
                    .files_credentials(workspace)
            }),
            identity: parts.root.join(IDENTITY_SOCKET),
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
/// it: the folder that mirrors them, unless they cannot be held.
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
    broker: &BrokerHandle,
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
            },
            workspace: workspace.to_owned(),
            actor: actor.to_owned(),
            policy_time: policy_origin.capture().map_err(io::Error::other)?,
        }),
    );
    Ok(())
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
