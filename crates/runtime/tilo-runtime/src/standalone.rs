//! Services for a runtime profile without the Tilo desktop: the shared
//! action broker and identity service over the profile's own workspace.
//!
//! This is what the runtime serves on stock macOS or a foreign Linux
//! desktop. The desktop session hosts the same brokers with its native
//! adapters (launcher, governance, durable owner, peer network). Here the
//! profile launches its installed apps through the platform launcher
//! (`crate::apps`), records the grants people give apps for other apps'
//! actions in its own workspace, places the profile's workspace on the
//! workspace host it runs in process and takes part in it as in every
//! workspace it joins, a Tilo device or cloud host governs (`joined`), serves
//! route keys for them (`channels`) and routes offloadable actions through the
//! person's home relay when one retains its workspace ([`crate::offload`]);
//! every other host service is absent,
//! and the broker answers as it does for any host that does not provide one.
//! What this host keeps durably is in the profile: the workspace host's
//! workspaces, the joined workspaces' record, grants, effect and task records
//! in its own workspace file, the invocation journal and
//! collaboration history.

mod channels;
#[cfg(target_os = "macos")]
mod confirm;
mod execution;
mod files;
mod joined;
mod media_import;
#[cfg(test)]
mod media_import_tests;
pub mod take_work;
mod workspace;
mod workspaces;

use crate::remote_door::PersonalDevices;
use crate::{
    BLOB_SOCKET, COLLAB_SOCKET, IDENTITY_SOCKET, Joiner, RuntimeServices, apps, profile, unix,
};
use joined::{Joined, RecordCreation, Refusal};
use std::{
    io,
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::{Arc, Mutex},
    time::SystemTime,
};
use tilo_action_broker::{
    collab_broker::{AppActionBroker, BrokerHandle, FanoutState},
    collab_core::BrokerChannels,
    collab_history::CollabHistoryStore,
    collab_outbound::PeerPublisher,
    services::{HostServices, LauncherServices, LegacyWorkspaceProjection, WorkspaceAuthority},
};
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeRequest, Pending,
};
use tilo_desktop_model::WorkspaceId;
use tilo_identity_host::{
    broker::{IdentityBrokerHandle, WorkspaceMembership},
    provisioning::IdentityStorage,
};
use tilo_peer_state::actions::ActionPlane;
use tilo_peer_state::owner_commit::OwnerCommitService;
use tilo_peer_state::{PeerPersonal, PersonalSync};
use workspace::{ProfileGrants, ProfileWorkspace};
use workspaces::Workspaces;

/// Durable collaboration history, where the desktop session keeps it under
/// the profile's `XDG_STATE_HOME`.
const COLLABORATION_HISTORY: &str = "state/tilo/collaboration/history";
/// Where the workspace host this runtime runs keeps its workspaces: the
/// directory profiles already keep it in.
const HOST_STORE: &str = "room-host";

/// A standalone service session over one profile. Drop applications first.
pub struct Standalone {
    // Retired before broker fields: retained original custody cannot survive
    // replacement of the runtime that supplied its policy time.
    policy_origin: tilo_action_broker::policy_clock::PolicyOrigin,
    broker: BrokerHandle,
    joined: Arc<Joined>,
    _channels: channels::Channels,
    _identity: IdentityBrokerHandle,
    workspace: String,
    actor: String,
    identity_socket: PathBuf,
    blob_socket: PathBuf,
    /// The files of the workspaces this profile joined, each a replica of
    /// the workspace's Files record mirrored at a folder. Before the media
    /// host, whose content store they use.
    replicas: Option<Arc<files::Replicas>>,
    /// Before the media host, whose content store it uses.
    _files: Option<files::WorkspaceFiles>,
    _media: profile::MediaHost,
    /// The person's own documents, opened on their identity root.
    _personal: PeerPersonal,
    /// The store open to the person's other devices for as long as this
    /// runtime runs, and the workspaces' store answered on the same router.
    _personal_door: tilo_action_broker::personal::Door,
    _personal_router: iroh::protocol::Router,
    /// The agent doors of the person's other devices, dialed from this
    /// runtime's endpoint as the device its store certifies.
    remote_doors: Arc<crate::remote_door::Doors>,
    /// The person's devices, as this runtime's store of them holds them: who a
    /// grant certificate must belong to for the MCP endpoint, and whether it
    /// still holds.
    persons_devices: Arc<dyn crate::remote_door::PersonsDevices>,
    _lock: advisory_lock::FileLock,
}

/// The services a profile is served as: the person's own runtime, the same in
/// development, or an isolated admission profile of the device's existing
/// owner, which nothing is provisioned for.
#[derive(Clone, Copy)]
enum Serving {
    Profile,
    Dev,
    Admission { owner_uid: u32 },
}

impl Drop for Standalone {
    fn drop(&mut self) {
        self.policy_origin.invalidate();
    }
}

impl Standalone {
    #[cfg(test)]
    fn media_host(&self) -> &profile::MediaHost {
        let Self { _media: media, .. } = self;
        media
    }

    #[cfg(test)]
    fn personal(&self) -> &PeerPersonal {
        let Self {
            _personal: personal,
            ..
        } = self;
        personal
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one start, each service opened in the order the next ones need it"
    )]
    fn start(root: &Path, stage: &mut dyn FnMut(&str), serving: Serving) -> io::Result<Self> {
        let take_work = taking_work(serving)?;
        stage("locking the profile");
        let lock = profile::lock(root)?;
        let storage = IdentityStorage::for_session();
        let device = match serving {
            Serving::Profile | Serving::Dev => {
                stage("provisioning the collaboration identity");
                provision_device(storage)?
            }
            Serving::Admission { owner_uid } => admit_device(owner_uid, stage)?,
        };
        let dev = matches!(serving, Serving::Dev);
        let (workspace, identity, actor) = open_profile_workspace(root, stage)?;
        let workspace_id = workspace.workspace();
        let workspace = Arc::new(workspace);
        let apps = Arc::new(apps::Apps::from_env(root, &workspace_id));
        serve_device_grants(root, &apps);
        let channels = Arc::new(Mutex::new(channels(root)));
        let fanout = Arc::new(FanoutState::new());
        stage("opening the joined workspaces and channel keys");
        let (joined, channel_keys) = joined_workspaces_and_keys(
            root,
            (&workspace_id, workspace.governs()),
            storage,
            (Arc::clone(&channels), Arc::clone(&fanout)),
            take_work.as_ref().map(|_| take_work::PORT),
        )?;
        if workspace.governs() {
            stage("placing the workspace on this device's workspace host");
            place_own_workspace(root, &workspace, &joined, &channel_keys.keys, (&apps, dev))?;
        }
        // The joined workspaces' files, once the broker that carries their
        // records runs: where a member taking a workspace's work runs it.
        let replica_slot = files::ReplicaSlot::default();
        let take_work = take_work_plane(root, &joined, take_work, &replica_slot, stage)?;
        stage("opening the person's store to their other devices");
        let personal = serve_personally(PersonalServing {
            root,
            identity: &identity,
            workspace_id: &workspace_id,
            joined: &joined,
            channels: Arc::clone(&channels),
            fanout: Arc::clone(&fanout),
            take_work,
        })?;
        let workspaces = Arc::new(Workspaces::new(&workspace, &joined));
        let identity_socket = root.join(IDENTITY_SOCKET);
        stage("starting the identity service");
        let identity_broker = identity_service(
            identity_socket.clone(),
            Arc::clone(&workspaces) as Arc<dyn WorkspaceMembership>,
            Arc::new(channel_keys.keys.clone()),
            &apps,
            dev,
        )?;
        stage("opening the content store");
        let media = profile::MediaHost::start(&root.join("content"), &root.join(BLOB_SOCKET))?;
        stage("starting the collaboration broker");
        let (policy_origin, broker) = start_broker(
            BrokerParts {
                root,
                workspace_id: &workspace_id,
                workspace: &workspace,
                apps: &apps,
                media: &media,
                joined: &joined,
                keys: &channel_keys.keys,
                device,
                personal: personal.sync.clone(),
            },
            workspaces,
            channels,
            fanout,
        )?;
        // An action an agent makes for the person, which asks for
        // confirmation, is put to them in the runtime's native prompt.
        // Elsewhere nobody here answers, and such an action is refused.
        #[cfg(target_os = "macos")]
        confirm::ask_the_person(&broker, &actor, &apps);
        let parts = JoinedFiles {
            root,
            own: &workspace,
            joined: &joined,
            media: &media,
            apps: &apps,
            author: files::Author {
                person: identity.user_public_key,
                device,
            },
        };
        let files = workspace_files(&parts, stage);
        let replicas = joined_files(&parts, stage);
        if let Some(replicas) = &replicas {
            let _ = replica_slot.set(Arc::clone(replicas));
        }
        Ok(Self {
            policy_origin,
            broker,
            remote_doors: remote_doors(&joined, &personal.sync),
            persons_devices: Arc::new(PersonalDevices::new(personal.sync.clone())),
            joined,
            _channels: channel_keys,
            _identity: identity_broker,
            workspace: workspace_id,
            actor,
            identity_socket,
            blob_socket: root.join(BLOB_SOCKET),
            replicas,
            _files: files,
            _media: media,
            _personal: personal.sync.store(),
            _personal_door: personal.door,
            _personal_router: personal.router,
            _lock: lock,
        })
    }
}

/// The profile's workspace and this device's identity for it, saved as the
/// workspace its apps act in, and the actor they act as there.
fn open_profile_workspace(
    root: &Path,
    stage: &mut dyn FnMut(&str),
) -> io::Result<(
    ProfileWorkspace,
    tilo_identity_host::registry::IdentityProfile,
    String,
)> {
    stage("loading the workspace");
    let workspace = ProfileWorkspace::open(root)?;
    let identity = tilo_identity_host::registry::identity_for_workspace(&workspace.workspace())?
        .ok_or_else(|| io::Error::other("runtime workspace has no selected identity"))?;
    let actor = identity.principal_id();
    stage("saving the workspace");
    workspace.adopt(&actor)?;
    Ok((workspace, identity, actor))
}

/// The relay whose work this profile takes, when it is a member: what the
/// environment names, read before anything of the profile is opened, so a
/// broken configuration is no profile's. An admission profile is a session
/// of an existing owner, never a member.
fn taking_work(serving: Serving) -> io::Result<Option<take_work::Relay>> {
    match serving {
        Serving::Profile | Serving::Dev => take_work::Relay::from_env(),
        Serving::Admission { .. } => Ok(None),
    }
    .map_err(io::Error::other)
}

/// The device plane this profile answers its relay's work on, when it takes
/// work: served on the endpoint its other devices reach it at.
fn take_work_plane(
    root: &Path,
    joined: &Arc<Joined>,
    relay: Option<take_work::Relay>,
    replicas: &files::ReplicaSlot,
    stage: &mut dyn FnMut(&str),
) -> io::Result<Option<ActionPlane>> {
    let Some(relay) = relay else {
        return Ok(None);
    };
    stage("taking the work of the workspaces relayed here");
    // Browser environments start on the hosted-VM service this host's
    // environment names, when it runs one.
    take_work::plane(root, joined, relay, Some(Arc::clone(replicas)), None).map(Some)
}

/// What the broker's services are opened from: this profile's workspace, apps
/// and media, the workspaces this device joined, and the person's own store.
struct BrokerParts<'a> {
    root: &'a Path,
    workspace_id: &'a str,
    workspace: &'a Arc<ProfileWorkspace>,
    apps: &'a Arc<apps::Apps>,
    media: &'a profile::MediaHost,
    joined: &'a Arc<Joined>,
    keys: &'a tilo_action_broker::channel_encryption::ChannelKeyProvider,
    device: [u8; 32],
    personal: PersonalSync,
}

/// Start the collaboration broker over this profile's services.
fn start_broker(
    parts: BrokerParts<'_>,
    workspaces: Arc<Workspaces>,
    channels: Arc<Mutex<BrokerChannels>>,
    fanout: Arc<FanoutState>,
) -> io::Result<(tilo_action_broker::policy_clock::PolicyOrigin, BrokerHandle)> {
    let (policy_origin, policy_time) = execution_policy_clock()?;
    let broker = tilo_action_broker::collab_broker::spawn_with_services(
        parts.root.join(COLLAB_SOCKET),
        Some(workspaces as Arc<dyn WorkspaceAuthority>),
        PeerPublisher::with_optional_owner_commit(
            None,
            Arc::clone(parts.joined) as Arc<dyn OwnerCommitService>,
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
    policy_time: tilo_action_broker::policy_clock::PolicyTime,
) -> io::Result<HostServices> {
    let record_content = joined::record_content(parts.joined, &parts.root.join(BLOB_SOCKET));
    // Where this workspace's offloadable actions may run instead of here: the
    // other devices of the invoking workspace answer an offer round over the
    // endpoint this profile syncs on, and the content an offloaded run reads
    // and answers with is the content this broker routes.
    let action_offload = parts
        .joined
        .offer_plane()
        .and_then(|(plane, endpoint, runtime)| {
            crate::offload::offer_door(plane, endpoint, runtime, Arc::clone(&record_content))
        });
    Ok(HostServices {
        native_certificates: Some(Arc::new(Certificates {
            workspace: parts.workspace_id.to_owned(),
            device: parts.device,
            policy_time,
            joined: Arc::downgrade(parts.joined),
        })),
        launchable_catalogue: Some(parts.root.join("state/launchable-actions.json")),
        launcher: Some(Arc::clone(parts.apps) as Arc<dyn LauncherServices>),
        app_action_access: Some(parts.apps.access_resolver()),
        app_action_grants: Some(Arc::new(ProfileGrants {
            workspace: Arc::clone(parts.workspace),
            apps: Arc::clone(parts.apps),
        })),
        legacy_workspace: Some(Arc::clone(parts.workspace) as Arc<dyn LegacyWorkspaceProjection>),
        workspace_governance: Some(media_import::service(parts.workspace, parts.media)?),
        record_content: Some(record_content),
        // The service the identity service answers key requests from:
        // endpoints ask again when it says a channel changed.
        channel_keys: Some(parts.keys.clone()),
        action_offload,
        personal: Some(parts.personal),
        ..HostServices::default()
    })
}

fn execution_policy_clock() -> io::Result<(
    tilo_action_broker::policy_clock::PolicyOrigin,
    tilo_action_broker::policy_clock::PolicyTime,
)> {
    let origin = tilo_action_broker::policy_clock::OwnerPolicyClock::new()
        .map_err(io::Error::other)?
        .new_origin();
    let time = origin.capture().map_err(io::Error::other)?;
    Ok((origin, time))
}

impl RuntimeServices for Standalone {
    type ActionHost = RuntimeActionHost;

    fn start_reporting(root: &Path, stage: &mut dyn FnMut(&str)) -> io::Result<Self> {
        Self::start(root, stage, Serving::Profile)
    }

    fn start_admission_reporting(
        root: &Path,
        owner_uid: u32,
        stage: &mut dyn FnMut(&str),
    ) -> io::Result<Self> {
        Self::start(root, stage, Serving::Admission { owner_uid })
    }

    fn start_in_dev_mode(root: &Path, stage: &mut dyn FnMut(&str)) -> io::Result<Self> {
        Self::start(root, stage, Serving::Dev)
    }

    fn workspace(&self) -> &str {
        &self.workspace
    }

    fn mcp_workspace(&self, requested: Option<&str>) -> io::Result<String> {
        let workspace = requested.unwrap_or(&self.workspace);
        if workspace == self.workspace {
            return Ok(self.workspace.clone());
        }
        self.joined
            .action_workspace(&self.actor, workspace)
            .map_err(|error| io::Error::new(io::ErrorKind::PermissionDenied, error))
    }

    fn actor(&self) -> &str {
        &self.actor
    }

    fn action_host(&self) -> io::Result<RuntimeActionHost> {
        let broker = self
            .broker
            .app_action_broker()
            .ok_or_else(|| io::Error::other("runtime action broker is unavailable"))?;
        let admission = self
            .joined
            .admit_mcp_execution(&self.workspace, &self.actor)
            .map_err(io::Error::other)?;
        let host = broker
            .certified_local_action_host(admission.caller(), admission.presentation())
            .map_err(io::Error::other)?;
        admission.check(&self.joined).map_err(io::Error::other)?;
        Ok(RuntimeActionHost {
            broker,
            workspace: self.workspace.clone(),
            joined: Arc::clone(&self.joined),
            session: Some(Arc::new(AdmittedRuntimeSession {
                policy_time: self.policy_origin.capture().map_err(io::Error::other)?,
                host,
                admission,
                agent: None,
                projection: Some(Caller::local_person(&self.actor).in_workspace(&self.workspace)),
            })),
        })
    }

    fn mcp_session_factory(
        &self,
        requested: &str,
    ) -> io::Result<crate::mcp::SessionFactory<RuntimeActionHost>> {
        let workspace = self.mcp_workspace(Some(requested))?;
        let actor = self.actor.clone();
        let host = self.action_host()?;
        Ok(host.session_factory(
            workspace,
            actor,
            self.policy_origin.capture().map_err(io::Error::other)?,
        ))
    }

    fn joiner(&self) -> Option<Joiner> {
        let joined = Arc::clone(&self.joined);
        let replicas = self.replicas.clone();
        Some(Arc::new(move |invitation: &str| {
            let invitation = invitation.to_owned();
            let workspace = tilo_rt::app()
                .handle()
                .block_on({
                    let joined = Arc::clone(&joined);
                    async move { joined.join(&invitation).await }
                })
                .map_err(io::Error::other)?;
            // The workspace's files are held from the join on: the folder
            // fills as the workspace's tree arrives.
            if let Some(replicas) = &replicas {
                let _ = hold_files(replicas, &joined, workspace);
            }
            Ok(workspace.to_string())
        }))
    }

    fn launch(&self, app_id: &str, command: Command) -> io::Result<Child> {
        unix::launch(
            command,
            app_id,
            &self.workspace,
            &unix::ServiceSockets {
                collab: self.broker.app_socket(),
                identity: &self.identity_socket,
                blobs: &self.blob_socket,
            },
        )
    }

    fn media(&self) -> Option<Arc<dyn crate::MediaReader>> {
        Some(Arc::new(profile::BlobMedia::new(
            &self.blob_socket,
            &self.workspace,
        )))
    }

    fn remote_doors(&self) -> Option<Arc<crate::remote_door::Doors>> {
        Some(Arc::clone(&self.remote_doors))
    }

    fn persons_devices(&self) -> Option<Arc<dyn crate::remote_door::PersonsDevices>> {
        Some(Arc::clone(&self.persons_devices))
    }
}

/// The broker's action door as one more local caller, standing for no app
/// connection. Its callers act from this foreign device.
#[derive(Clone)]
pub struct RuntimeActionHost {
    broker: AppActionBroker,
    workspace: String,
    joined: Arc<Joined>,
    // Retains original custody for both owner/search and MCP callers. An
    // owner projection is not a native seat and never acquires seat authority.
    session: Option<Arc<AdmittedRuntimeSession>>,
}

struct AdmittedRuntimeSession {
    policy_time: tilo_action_broker::policy_clock::PolicyTime,
    host: Arc<dyn ActionHost + Send + Sync>,
    admission: Arc<joined::SessionAdmission>,
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

    /// Whether this call may run as the run's caller, now.
    ///
    /// A refusal says why it can be acted on: a run that ended is ended for
    /// good, a caller this run was not admitted for is a stranger, and a
    /// check that could not be made (a certificate read or its validity
    /// check that failed this once) refuses only this call, whose error
    /// tells the agent to retry. Every refusal is logged with its cause;
    /// none logs identity material.
    fn check(&self, caller: &Caller, joined: &Joined) -> Result<(), Refusal> {
        let now = match self.policy_time.now_ns() {
            Ok(now) => now,
            Err(error) => {
                tracing::warn!(%error, "the action door cannot read the policy clock");
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
        if caller
            != self
                .projection
                .as_ref()
                .unwrap_or_else(|| self.admission.caller())
        {
            tracing::warn!(
                caller = ?caller.principal(),
                "a caller this run was not admitted for tried to act as it"
            );
            return Err(Refusal::Stranger);
        }
        self.admission.check(joined)
    }

    fn guarded<T: Send + 'static>(
        self: &Arc<Self>,
        joined: Arc<Joined>,
        caller: Caller,
        answer: Pending<Result<T, ActionHostError>>,
    ) -> Pending<Result<T, ActionHostError>> {
        let session = Arc::clone(self);
        let (complete, pending) = tilo_actions::remote::pending();
        answer.then(move |result| {
            let result = session
                .check(&caller, &joined)
                .map_err(ActionHostError::from)
                .and_then(|()| {
                    result.unwrap_or_else(|| {
                        Err(ActionHostError::Unavailable {
                            reason: "action door closed".into(),
                        })
                    })
                });
            complete.complete(result);
        });
        pending
    }
}

impl RuntimeActionHost {
    fn session_factory(
        self,
        workspace: String,
        actor: String,
        policy_time: tilo_action_broker::policy_clock::PolicyTime,
    ) -> crate::mcp::SessionFactory<Self> {
        let host = self;
        Arc::new(move || {
            let admission = host
                .joined
                .admit_mcp_execution(&workspace, &actor)
                .map_err(io::Error::other)?;
            let caller = admission.caller().clone();
            // The MCP client is an agent the person's device hands this
            // session to; the admission stays the person's. Its name is part
            // of the durable owner, so people are shown a short label for it
            // instead of the whole run id.
            let agent = crate::mcp::agent_id(&caller);
            let qualified = host
                .broker
                .certified_local_agent_host(&caller, admission.presentation(), &agent)
                .map_err(io::Error::other)?;
            let label = crate::mcp::agent_label(&agent);
            admission.check(&host.joined).map_err(io::Error::other)?;
            let session_host = RuntimeActionHost {
                broker: host.broker.clone(),
                workspace: host.workspace.clone(),
                joined: host.joined.clone(),
                session: Some(Arc::new(AdmittedRuntimeSession {
                    policy_time: policy_time.clone(),
                    host: qualified,
                    admission,
                    agent: Some(agent.clone()),
                    projection: None,
                })),
            };
            Ok(crate::mcp::McpServer::admitted(session_host, caller)?.with_agent_run(agent, label))
        })
    }

    fn admitted(&self, caller: &Caller) -> bool {
        let Some(workspace) = caller
            .workspace()
            .filter(|workspace| *workspace != self.workspace)
        else {
            return true;
        };
        let tilo_actions::remote::CallerPrincipal::Person(actor) = caller.principal() else {
            return false;
        };
        self.joined.action_workspace(actor, workspace).is_ok()
    }
}

// These existing alias forms may eventually dispatch into a published app.
// Keep original certified custody through the broker's deferred launch path.
fn certified_provider(provider: &str) -> bool {
    provider.parse::<u64>().is_ok()
        || tilo_actions::remote::application_provider(provider).is_some()
        || provider.starts_with("launch:")
}

fn on_this_device(caller: &Caller) -> Caller {
    caller.clone().from_foreign_device(true)
}

impl ActionHost for RuntimeActionHost {
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
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        if let Some(session) = &self.session {
            // Owner discovery remains the ordinary read path: no per-query
            // identity-store resolution. Opening a result uses retained custody.
            if session.projection.as_ref() == Some(caller) {
                if !self.admitted(caller) {
                    return Pending::ready(Err(ActionHostError::NotPermitted));
                }
                return self
                    .broker
                    .state()
                    .action_query(&on_this_device(caller), &query);
            }
            if let Err(refusal) = session.check(caller, &self.joined) {
                return Pending::ready(Err(refusal.into()));
            }
            if query
                .provider
                .as_ref()
                .is_some_and(|provider| certified_provider(provider))
            {
                return session.host.query(caller, query);
            }
            // Existing general catalogue/builtin doors retain the exact
            // authenticated run. They make no SDK original-custody claim.
            let answer = self.broker.state().action_query(&session.acting(), &query);
            return session.guarded(Arc::clone(&self.joined), caller.clone(), answer);
        }
        if !self.admitted(caller) {
            return Pending::ready(Err(ActionHostError::NotPermitted));
        }
        self.broker
            .state()
            .action_query(&on_this_device(caller), &query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        if let Err(refused) = tilo_actions::appearance::refuse_here(&request) {
            return Pending::ready(Err(refused));
        }
        if let Some(session) = &self.session {
            if let Err(refusal) = session.check(caller, &self.joined) {
                return Pending::ready(Err(refusal.into()));
            }
            return session.host.invoke(session.admission.caller(), request);
        }
        Pending::ready(Err(ActionHostError::NotPermitted))
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        if let Some(session) = &self.session {
            session.check(caller, &self.joined).ok()?;
            return session.host.observe(session.admission.caller(), id);
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
            session.check(caller, &self.joined).ok()?;
            return session.host.here(session.admission.caller());
        }
        if !self.admitted(caller) {
            return None;
        }
        self.broker.state().here(&on_this_device(caller))
    }

    /// Routed and authorized exactly as [`Self::observe`]: a caller that may
    /// not observe an invocation may not wait on it either.
    fn next_change(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<Pending<()>> {
        if let Some(session) = &self.session {
            session.check(caller, &self.joined).ok()?;
            return session.host.next_change(session.admission.caller(), id);
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
    ) -> Pending<Result<tilo_actions::remote::ConfirmedInvocation, ActionHostError>> {
        let Some(session) = &self.session else {
            return Pending::ready(Err(ActionHostError::ConfirmationRequired));
        };
        if session.agent.is_none() {
            return Pending::ready(Err(ActionHostError::ConfirmationRequired));
        }
        if let Err(refusal) = session.check(caller, &self.joined) {
            return Pending::ready(Err(refusal.into()));
        }
        session.host.confirm(session.admission.caller(), request)
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionHostError>> {
        if let Some(session) = &self.session {
            if let Err(refusal) = session.check(caller, &self.joined) {
                return Pending::ready(Err(refusal.into()));
            }
            return session.host.cancel(session.admission.caller(), id);
        }
        if !self.admitted(caller) {
            return Pending::ready(Err(ActionHostError::UnknownInvocation));
        }
        self.broker
            .state()
            .action_cancel(&on_this_device(caller), id)
    }
}

/// The identity store's device-certificate checks, as the broker asks them.
/// This device governs the profile's own workspace, so a foreign runtime is not
/// held to the cap there.
struct Certificates {
    workspace: String,
    device: [u8; 32],
    policy_time: tilo_action_broker::policy_clock::PolicyTime,
    joined: std::sync::Weak<Joined>,
}

impl Certificates {
    fn check_registered_custody(&self, workspace: uuid::Uuid) -> Result<(), String> {
        let joined = self
            .joined
            .upgrade()
            .ok_or("runtime custody owner unavailable")?;
        let workspace =
            WorkspaceId::parse(&workspace.to_string()).ok_or("invalid exact workspace")?;
        joined.recheck_mcp_executions(workspace)
    }
}

impl tilo_action_broker::certificate_authority::CertificateAuthority for Certificates {
    fn policy_time(&self) -> Result<Option<tilo_action_broker::policy_clock::PolicyTime>, String> {
        self.policy_time
            .now_ns()
            .map_err(|error| error.to_string())?;
        Ok(Some(self.policy_time.clone()))
    }

    fn authorize_with_policy_time(
        &self,
        workspace: uuid::Uuid,
        cert: &tilo_user_identity::DeviceCert,
        time: &tilo_action_broker::policy_clock::PolicyTime,
    ) -> Result<(), String> {
        if !time.same_origin(&self.policy_time) {
            return Err("original runtime policy origin changed".into());
        }
        let now = time.now_ns().map_err(|error| error.to_string())?;
        self.check_registered_custody(workspace)?;
        tilo_user_identity::verify_device_cert_at(cert, now).map_err(|error| error.to_string())?;
        // This store check is conservative: known revocations are refused even
        // before their effective instant. It never extends a credential lease.
        tilo_identity_host::certificates::authorize(&workspace.to_string(), cert)
    }

    fn authorize(
        &self,
        workspace: uuid::Uuid,
        cert: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        tilo_identity_host::certificates::authorize(&workspace.to_string(), cert)
    }
    fn authorize_join(
        &self,
        workspace: uuid::Uuid,
        cert: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        let now = self
            .policy_time
            .now_ns()
            .map_err(|error| error.to_string())?;
        tilo_user_identity::verify_device_cert_at(cert, now).map_err(|error| error.to_string())?;
        self.check_registered_custody(workspace)?;
        tilo_identity_host::certificates::authorize_join(&workspace.to_string(), cert)
    }
    fn governs(&self, workspace: uuid::Uuid, device: Option<&[u8; 32]>) -> bool {
        uuid::Uuid::parse_str(&self.workspace) == Ok(workspace)
            && device.is_none_or(|d| *d == self.device)
    }
}

/// The person's camera and microphone decisions, as actions on the
/// profile's console socket (macOS, where the runtime brokers capture). A
/// socket that cannot be served leaves the runtime running without it.
fn serve_device_grants(root: &Path, apps: &apps::Apps) {
    #[cfg(target_os = "macos")]
    {
        let served = apps.device_grants().and_then(|grants| {
            crate::device_grants::serve(grants, &root.join(crate::CONSOLE_SOCKET))
        });
        if let Err(error) = served {
            tracing::warn!(%error, "the device grants are not served on the console socket");
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (root, apps);
}

/// What the person's own store is served with: the profile's state it is kept
/// in, the identity this device acts as, and the workspace this runtime runs.
struct PersonalServing<'a> {
    root: &'a Path,
    identity: &'a tilo_identity_host::registry::IdentityProfile,
    /// The workspace this runtime runs, by its id.
    workspace_id: &'a str,
    joined: &'a Joined,
    channels: Arc<Mutex<BrokerChannels>>,
    fanout: Arc<FanoutState>,
    /// The device plane this profile answers its relay's work on, when it
    /// takes work at all.
    take_work: Option<ActionPlane>,
}

/// The person's own store, and what serves it to their other devices.
struct PersonalParts {
    sync: PersonalSync,
    door: tilo_action_broker::personal::Door,
    /// The endpoint's accept loop, answering the person's store to their
    /// other devices.
    router: iroh::protocol::Router,
}

/// Open the person's own store on the identity this device acts as, and serve
/// it to their other devices from the endpoint this runtime runs: the store,
/// the workspaces this device keeps and, for a member that takes work, its
/// device plane answer on one router, so neither protocol is answered by a
/// router the other one dropped.
///
/// # Errors
///
/// The store kept beside the profile's state is not this device's to open, or
/// the workspaces this device keeps cannot be served.
fn serve_personally(serving: PersonalServing<'_>) -> io::Result<PersonalParts> {
    let PersonalServing {
        root,
        identity,
        workspace_id,
        joined,
        channels,
        fanout,
        take_work,
    } = serving;
    let (key, device) = tilo_identity_host::broker::trusted_current_device(workspace_id)?;
    let state = root.join("state");
    let sync = PersonalSync::new(
        tilo_action_broker::personal::open(
            &state,
            identity.user_public_key,
            device,
            SystemTime::now(),
        )
        .map_err(io::Error::other)?,
        key,
    );
    let runtime = tilo_rt::app().handle();
    let router = {
        let _entered = runtime.enter();
        let mut router = iroh::protocol::Router::builder(joined.endpoint())
            .accept(tilo_peer_state::PERSONAL_ALPN, sync.clone());
        if let Some(plane) = take_work {
            router = router.accept(tilo_peer_state::actions::ACTIONS_ALPN, plane);
        }
        router.spawn()
    };
    let door = tilo_action_broker::personal::Door::serve(
        runtime,
        joined.endpoint(),
        sync.clone(),
        &tilo_action_broker::personal::Person::of(identity.id.clone(), identity.user_public_key),
        &state,
        channels,
        fanout,
    );
    Ok(PersonalParts { sync, door, router })
}

/// The agent doors of the person's other devices, dialed from `joined`'s
/// endpoint as the device the person's store `personal` certifies.
fn remote_doors(joined: &Joined, personal: &PersonalSync) -> Arc<crate::remote_door::Doors> {
    Arc::new(crate::remote_door::Doors::new(
        joined.endpoint(),
        Arc::new(crate::remote_door::PersonalDevices::new(personal.clone())),
        tilo_rt::app().handle().clone(),
    ))
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

/// This device's collaboration identity, provisioned in `storage` on first
/// start; answers its device key.
fn provision_device(storage: IdentityStorage) -> io::Result<[u8; 32]> {
    let identity_dir = runtime_identity::identity_dir_from_env()?;
    let paths = tilo_identity_host::provisioning::collaboration_identity_paths();
    tilo_identity_host::provisioning::ensure_foreign_collaboration_identity(
        &identity_dir,
        &paths,
        storage,
    )?;
    Ok(tilo_user_identity::persist::load_device_key(&paths)
        .map_err(io::Error::other)?
        .verifying_key()
        .to_bytes())
}

/// The identity an isolated admission profile is served under: the existing
/// owner's, verified and never created, renewed or written. Answers its device
/// key.
fn admit_device(owner_uid: u32, stage: &mut dyn FnMut(&str)) -> io::Result<[u8; 32]> {
    if owner_uid == 0 || rustix::process::geteuid().as_raw() != owner_uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "native admission must run as the existing non-root device owner",
        ));
    }
    stage("verifying the existing collaboration identity");
    let identity_dir = runtime_identity::identity_dir_from_env()?;
    let paths = tilo_identity_host::provisioning::collaboration_identity_paths();
    tilo_identity_host::provisioning::verify_collaboration_identity(
        &identity_dir,
        &paths,
        tilo_clock::unix_now_ns(),
    )?;
    if !tilo_identity_host::provisioning::active_identity_owns_device(&identity_dir, &paths)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "native admission identity does not own this device",
        ));
    }
    Ok(tilo_user_identity::persist::load_device_key(&paths)
        .map_err(io::Error::other)?
        .verifying_key()
        .to_bytes())
}

/// The profile's workspace files, shown at their folder in this user's
/// home. Without the folder the host still serves: apps keep their
/// documents, only the folder view of the files is missing. A profile the
/// desktop session serves has none: that host projects its files itself.
fn workspace_files(
    parts: &JoinedFiles<'_>,
    stage: &mut dyn FnMut(&str),
) -> Option<files::WorkspaceFiles> {
    if !parts.own.governs() {
        return None;
    }
    stage("showing the workspace files");
    let started = files::folder(&parts.own.name()).and_then(|folder| {
        let projection = files::Projection::for_host();
        files::WorkspaceFiles::start(
            parts.root,
            &folder,
            parts.media.store(),
            parts.joined.endpoint(),
            &parts.author,
            projection,
        )
    });
    started
        .inspect_err(|error| tracing::warn!(%error, "the workspace folder is unavailable"))
        .ok()
}

/// What the profile's own workspace files and the joined workspaces' files
/// are held with.
struct JoinedFiles<'a> {
    root: &'a Path,
    /// The profile's own workspace, whose files [`workspace_files`] shows.
    own: &'a ProfileWorkspace,
    joined: &'a Arc<Joined>,
    media: &'a profile::MediaHost,
    apps: &'a Arc<apps::Apps>,
    author: files::Author,
}

/// The files of every workspace this profile joined, each mirrored at a
/// folder in this user's home beside the profile's own. Without a home, or
/// before the broker serves record content, none are held: the workspaces
/// still sync, only their files are missing here.
fn joined_files(
    parts: &JoinedFiles<'_>,
    stage: &mut dyn FnMut(&str),
) -> Option<Arc<files::Replicas>> {
    let home = tilo_paths::home()?;
    let content = parts.joined.record_content_service()?;
    let own = parts.own.workspace();
    let own_folder = parts
        .own
        .governs()
        .then(|| files::folder_in(&home, &parts.own.name()));
    let joined = Arc::downgrade(parts.joined);
    let replicas = Arc::new(files::Replicas::new(files::ReplicaHost {
        profile: parts.root.to_path_buf(),
        home,
        own_folder,
        store: parts.media.store(),
        endpoint: parts.joined.endpoint(),
        author: parts.author,
        credentials: Box::new(move |workspace| {
            joined
                .upgrade()
                .ok_or_else(|| io::Error::other("the joined workspaces closed"))?
                .files_credentials(workspace)
        }),
        collab: parts.root.join(COLLAB_SOCKET),
        identity: parts.root.join(IDENTITY_SOCKET),
        content,
        apps: Arc::clone(parts.apps),
    }));
    let workspaces: Vec<WorkspaceId> = parts
        .joined
        .workspaces()
        .iter()
        .map(tilo_desktop_model::Workspace::id)
        .filter(|workspace| workspace.to_string() != own)
        .collect();
    if !workspaces.is_empty() {
        stage("syncing the joined workspaces' files");
    }
    for workspace in workspaces {
        let _ = hold_files(&replicas, parts.joined, workspace);
    }
    Some(replicas)
}

/// Hold `workspace`'s files in `replicas`, under the name its governance gives
/// it: the folder that mirrors them, unless they cannot be held.
fn hold_files(
    replicas: &files::Replicas,
    joined: &Joined,
    workspace: WorkspaceId,
) -> Option<PathBuf> {
    let name = joined
        .workspace(workspace)
        .map_or_else(|| "Workspace".to_owned(), |workspace| workspace.name);
    replicas
        .hold(&workspace.to_string(), &name)
        .inspect_err(|error| {
            tracing::warn!(%error, %workspace, "a joined workspace's files are not held here");
        })
        .ok()
}

impl Standalone {
    /// The folder mirroring the files of `workspace`, a workspace this profile
    /// joined, while they are held: what a member taking the workspace's work
    /// maps the workspace's files to.
    #[must_use]
    pub fn joined_files_folder(&self, workspace: &str) -> Option<PathBuf> {
        self.replicas.as_ref()?.folder(workspace)
    }
}

/// Place the profile's own workspace, called `name`, on the workspace host
/// this runtime runs in process, and hold it as a joined workspace from then on.
/// Apps create records there in the kinds their installed manifests declare,
/// or in dev mode (`apps`, `dev`) the kinds a build that is not installed
/// names.
fn place_own_workspace(
    profile: &Path,
    workspace: &ProfileWorkspace,
    joined: &Arc<Joined>,
    keys: &tilo_action_broker::channel_encryption::ChannelKeyProvider,
    (apps, dev): (&Arc<apps::Apps>, bool),
) -> io::Result<()> {
    let workspace_id = workspace.workspace();
    let (key, cert) = tilo_identity_host::broker::trusted_current_device(&workspace_id)?;
    let workspace_id = WorkspaceId::parse(&workspace_id)
        .ok_or_else(|| io::Error::other("invalid runtime workspace"))?;
    let host = tilo_rt::app()
        .handle()
        .block_on(tilo_room_host::host::WorkspaceHost::open(
            tilo_room_host::host::HostConfig::new(
                profile.join(HOST_STORE),
                tilo_room_host::host::HostMode::Device,
            ),
            tilo_room_host::host::HostIdentity {
                user_public_key: cert.user_public_key,
                device_signing_key: key,
                device_cert: cert,
                revocations: tilo_user_identity::RevocationList::new(),
            },
            joined.endpoint(),
            Some(channels::DeviceChannelKeys::device(keys.clone(), joined)),
        ))
        .map_err(io::Error::other)?;
    let apps = Arc::clone(apps);
    joined
        .install_host(
            Arc::new(host),
            RecordCreation {
                declared_kinds: Box::new(move |app| apps.declared_resource_kinds(app)),
                dev,
            },
        )
        .map_err(io::Error::other)?;
    tilo_rt::app()
        .handle()
        .block_on(joined.place(workspace_id, &workspace.name()))
        .map_err(io::Error::other)?;
    joined.hold_session(workspace_id);
    Ok(())
}

/// The joined workspaces and this device's channel keys for them, under the
/// device's trusted custody. `workspace` is the profile's own, which it governs
/// unless the desktop session does. `listen` is the UDP port the endpoint
/// binds when a relay must reach it, and the machine's own choice otherwise.
fn joined_workspaces_and_keys(
    profile: &Path,
    (workspace, governs): (&str, bool),
    storage: IdentityStorage,
    broker: (Arc<Mutex<BrokerChannels>>, Arc<FanoutState>),
    listen: Option<u16>,
) -> io::Result<(Arc<Joined>, channels::Channels)> {
    let custody = tilo_identity_host::broker::trusted_current_device(workspace)?;
    let channels = Arc::clone(&broker.0);
    let joined = joined_workspaces(
        profile,
        (workspace, governs),
        custody.clone(),
        broker,
        listen,
    )?;
    let keys = channels::open(
        profile,
        WorkspaceId::parse(workspace)
            .ok_or_else(|| io::Error::other("invalid runtime workspace"))?,
        storage,
        custody.0,
        Arc::clone(&joined),
        channels,
    )?;
    Ok((joined, keys))
}

/// The workspaces this profile joined, synced as this device over an endpoint
/// keyed by its device key. It refuses to enroll in the workspaces the desktop
/// session already enrolled this device in, and in the profile's own workspace
/// when the desktop session governs it.
fn joined_workspaces(
    profile: &Path,
    (workspace, governs): (&str, bool),
    (key, cert): (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert),
    broker: (Arc<Mutex<BrokerChannels>>, Arc<FanoutState>),
    listen: Option<u16>,
) -> io::Result<Arc<Joined>> {
    let custody = tilo_identity_host::custody::HostIdentity::held(
        cert,
        key.clone(),
        std::sync::Arc::new(tilo_identity_host::custody::files::Files::new(
            tilo_identity_host::provisioning::collaboration_identity_paths(),
        )),
    );
    let endpoint = listen_endpoint(&key, listen)?;
    let mut refused = joined::desktop_enrolled(profile)?;
    if !governs {
        refused.extend(WorkspaceId::parse(workspace));
    }
    Joined::open(profile, custody, endpoint, refused, broker)
}

/// This device's endpoint, keyed by its device key: bound to `listen` on
/// every interface when a host names the port it is reached at, and to
/// whatever port the machine gives it otherwise.
fn listen_endpoint(
    key: &ed25519_dalek::SigningKey,
    listen: Option<u16>,
) -> io::Result<iroh::Endpoint> {
    let mut builder = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()));
    if let Some(port) = listen {
        // Every interface, on the port a relay reaches this member at: IPv4,
        // and IPv6 where the machine has it.
        builder = builder
            .clear_ip_transports()
            .bind_addr_with_opts(
                std::net::SocketAddr::from((std::net::Ipv4Addr::UNSPECIFIED, port)),
                iroh::endpoint::BindOpts::default(),
            )
            .map_err(io::Error::other)?
            .bind_addr_with_opts(
                std::net::SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, port)),
                iroh::endpoint::BindOpts::default().set_is_required(false),
            )
            .map_err(io::Error::other)?;
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
    keys: Arc<dyn tilo_identity_host::broker::ChannelKeySource>,
    apps: &Arc<apps::Apps>,
    dev: bool,
) -> io::Result<tilo_identity_host::broker::IdentityBrokerHandle> {
    let person = AttachedSurfaces {
        apps: Arc::clone(apps),
        dev,
    };
    tilo_identity_host::broker::spawn(socket, workspaces, keys, Arc::new(person))
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

impl tilo_identity_host::broker::PersonRecordKinds for AttachedSurfaces {
    fn person_record_kinds(&self, app_id: &str) -> Vec<String> {
        ui::app_surface::person_record_kinds(app_id)
    }

    fn attached(&self, app_id: &str, surface: &[u8]) {
        if self.dev
            && self.apps.declared_resource_kinds(app_id).is_none()
            && ui::app_surface::bytes_of(app_id).is_none()
        {
            // Changes are decided against the surface their op names, read
            // from the content store as a package's is.
            if let Err(error) = tilo_content_device::for_app().put(surface) {
                tracing::warn!(%error, app_id, "the development surface could not be stored");
                return;
            }
            tilo_action_broker::register_package_surface(app_id, surface);
        }
    }
}

#[cfg(test)]
#[path = "standalone_tests.rs"]
mod tests;
