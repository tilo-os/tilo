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

mod commits;
#[cfg(target_os = "macos")]
mod confirm;
mod content;
mod files;
mod host_runs;
mod media_import;
#[cfg(test)]
mod media_import_tests;
mod service;
pub mod take_work;
mod workspace;
mod workspace_keys;
mod workspaces;

use crate::personal_devices::PersonalDevices;
use crate::{
    BLOB_SOCKET, COLLAB_SOCKET, IDENTITY_SOCKET, Joiner, RuntimeServices, apps, profile, unix,
};
use commits::{RecordCreation, ServiceCommits};
use service::{GovernanceInbox, ServiceWorkspaces};
use std::{
    io,
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::{Arc, Mutex},
    time::SystemTime,
};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeRequest, Pending,
};
use tilo_desktop_model::WorkspaceId;
use tilo_identity::{
    broker::{IdentityBrokerHandle, WorkspaceMembership},
    provisioning::IdentityStorage,
};
use tilo_peer_state::actions::ActionService;
use tilo_peer_state::owner_commit::OwnerCommitService;
use tilo_peer_state::{PeerPersonal, PersonalSync};
use tilo_workspace_service::apps::{
    collab_broker::{AppActionBroker, BrokerHandle, FanoutState},
    collab_core::BrokerChannels,
    collab_history::CollabHistoryStore,
    collab_outbound::PeerPublisher,
    services::{LauncherServices, LegacyWorkspaceProjection, ProcessServices, WorkspaceAuthority},
};
use tilo_workspace_service::workspaces::Refusal;
use workspace::{ProfileGrants, ProfileWorkspace};
use workspaces::Workspaces;

/// Durable collaboration history, where the desktop session keeps it under
/// the profile's `XDG_STATE_HOME`.
const COLLABORATION_HISTORY: &str = "state/tilo/collaboration/history";
/// Where the workspace host this runtime runs keeps its workspaces: the
/// directory profiles already keep it in.
const HOST_STORE: &str = "workspace-host";

/// A standalone service session over one profile. Drop applications first.
pub struct Standalone {
    // Retired before broker fields: retained original ownership cannot survive
    // replacement of the runtime that supplied its policy time.
    policy_origin: tilo_workspace_service::apps::policy_clock::PolicyOrigin,
    broker: BrokerHandle,
    workspaces: Arc<ServiceWorkspaces>,
    _workspace_keys: workspace_keys::KeyService,
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
    _personal_links: tilo_workspace_service::apps::personal::Links,
    _personal_router: iroh::protocol::Router,
    /// The MCP servers of the person's other devices, dialed from this
    /// runtime's endpoint as the device its store certifies.
    other_devices: Arc<tilo_device_mcp::OtherDevices>,
    /// The apps installed for this profile: what each publishes, before it
    /// runs here.
    apps: Arc<apps::Apps>,
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
        let channels = Arc::new(Mutex::new(channels(root)));
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
        // The device's workspace service, holding the profile's own workspace
        // and the member copies of the workspaces it joined, and this device's
        // channel keys for them.
        stage("opening the device's workspace service");
        let opened = open_workspaces(
            root,
            OpenWorkspaces {
                workspace: &workspace,
                storage,
                broker: (Arc::clone(&channels), Arc::clone(&fanout)),
                listen: take_work.as_ref().map(|_| take_work::PORT),
            },
        )?;
        let workspaces = opened.workspaces;
        let group_keys = opened.keys;
        let key_sessions = opened.key_sessions;
        workspaces.follow_member_writes(Arc::clone(&channels), Arc::clone(&fanout));
        // The member copies' files, once the broker that carries their records
        // runs: where a member taking a workspace's work runs it.
        let replica_slot = files::ReplicaSlot::default();
        let take_work =
            take_work_action_service(root, &workspaces, take_work, &replica_slot, stage)?;
        stage("opening the person's store to their other devices");
        let personal = serve_personally(PersonalServing {
            root,
            identity: &identity,
            workspace_id: &workspace_id,
            workspaces: &workspaces,
            channels: Arc::clone(&channels),
            fanout: Arc::clone(&fanout),
            take_work,
            host: Some(workspaces.service().as_ref()),
        })?;
        // What the person answers about this device's camera and microphone is kept in their own
        // store, where their other devices see it and can take it back.
        apps.device_grants().keep_in(personal.sync.clone());
        let authority = Arc::new(Workspaces::new(&workspace, &workspaces));
        let identity_socket = root.join(IDENTITY_SOCKET);
        stage("starting the identity service");
        let identity_broker = identity_service(
            identity_socket.clone(),
            Arc::clone(&authority) as Arc<dyn WorkspaceMembership>,
            Arc::new(group_keys.keys.clone()),
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
                workspaces: &workspaces,
                keys: &group_keys.keys,
                creation: Arc::clone(&creation),
                device,
                personal: personal.sync.clone(),
            },
            authority,
            channels,
            fanout,
        )?;
        // An action an agent makes for the person, which asks for
        // confirmation, is put to them in the runtime's native prompt.
        // Elsewhere nobody here answers, and such an action is refused.
        #[cfg(target_os = "macos")]
        confirm::ask_the_person(&broker, &actor, &apps);
        if let Some(key_sessions) = &key_sessions {
            serve_key_sessions(
                key_sessions,
                &broker,
                &workspaces,
                &personal.sync,
                (&workspace_id, &actor),
                &policy_origin,
            )?;
        }
        let parts = JoinedFiles {
            root,
            own: &workspace,
            workspaces: &workspaces,
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
            apps,
            other_devices: other_devices(&workspaces, &personal.sync),
            workspaces,
            _workspace_keys: group_keys,
            _identity: identity_broker,
            workspace: workspace_id,
            actor,
            identity_socket,
            blob_socket: root.join(BLOB_SOCKET),
            replicas,
            _files: files,
            _media: media,
            _personal: personal.sync.store(),
            _personal_links: personal.links,
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
    tilo_identity::registry::IdentityProfile,
    String,
)> {
    stage("loading the workspace");
    let workspace = ProfileWorkspace::open(root)?;
    let identity = tilo_identity::registry::identity_for_workspace(&workspace.workspace())?
        .ok_or_else(|| io::Error::other("runtime workspace has no selected identity"))?;
    let actor = identity.person_id();
    stage("saving the workspace");
    workspace.adopt(&actor)?;
    Ok((workspace, identity, actor))
}

/// The server whose work this profile takes, when it is a member: what the
/// environment names, read before anything of the profile is opened, so a
/// broken configuration is no profile's. An admission profile is a session
/// of an existing owner, never a member.
fn taking_work(serving: Serving) -> io::Result<Option<take_work::Server>> {
    match serving {
        Serving::Profile | Serving::Dev => take_work::Server::from_env(),
        Serving::Admission { .. } => Ok(None),
    }
    .map_err(io::Error::other)
}

/// The device action service this profile answers its server's work on, when it takes
/// work: served on the endpoint its other devices reach it at.
fn take_work_action_service(
    root: &Path,
    workspaces: &Arc<ServiceWorkspaces>,
    server: Option<take_work::Server>,
    replicas: &files::ReplicaSlot,
    stage: &mut dyn FnMut(&str),
) -> io::Result<Option<ActionService>> {
    let Some(server) = server else {
        return Ok(None);
    };
    stage("taking the work of the workspaces relayed here");
    // Browser environments start on the hosted-VM service this host's
    // environment names, when it runs one.
    take_work::action_service(root, workspaces, server, Some(Arc::clone(replicas)), None).map(Some)
}

/// What the broker's services are opened from: this profile's workspace, apps
/// and media, the workspaces this device joined, and the person's own store.
struct BrokerParts<'a> {
    root: &'a Path,
    workspace_id: &'a str,
    workspace: &'a Arc<ProfileWorkspace>,
    apps: &'a Arc<apps::Apps>,
    media: &'a profile::MediaHost,
    workspaces: &'a Arc<ServiceWorkspaces>,
    keys: &'a tilo_workspace_service::apps::workspace_keys::KeyProvider,
    creation: Arc<RecordCreation>,
    device: [u8; 32],
    personal: PersonalSync,
}

/// Start the collaboration broker over this profile's services.
fn start_broker(
    parts: BrokerParts<'_>,
    workspaces: Arc<Workspaces>,
    channels: Arc<Mutex<BrokerChannels>>,
    fanout: Arc<FanoutState>,
) -> io::Result<(
    tilo_workspace_service::apps::policy_clock::PolicyOrigin,
    BrokerHandle,
)> {
    let (policy_origin, policy_time) = execution_policy_clock()?;
    let broker = tilo_workspace_service::apps::collab_broker::spawn_with_services(
        parts.root.join(COLLAB_SOCKET),
        Some(workspaces as Arc<dyn WorkspaceAuthority>),
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
        content::document_content(parts.workspaces, &parts.root.join(BLOB_SOCKET));
    // Where this workspace's offloadable actions may run instead of here: the
    // other devices of the invoking workspace answer an offer round over the
    // endpoint this profile syncs on, and the content an offloaded run reads
    // and answers with is the content this broker routes.
    let action_offload =
        parts
            .workspaces
            .offer_action_service()
            .and_then(|(action_service, endpoint, runtime)| {
                crate::offload::offer_route(
                    action_service,
                    endpoint,
                    runtime,
                    Arc::clone(&document_content),
                )
            });
    Ok(ProcessServices {
        native_certificates: Some(Arc::new(Certificates {
            workspace: parts.workspace_id.to_owned(),
            device: parts.device,
            policy_time,
            workspaces: Arc::downgrade(parts.workspaces),
        })),
        launchable_catalogue: Some(parts.root.join("state/launchable-actions.json")),
        launcher: Some(Arc::clone(parts.apps) as Arc<dyn LauncherServices>),
        app_action_access: Some(parts.apps.access_resolver()),
        app_resource_kinds: Some(parts.apps.resource_kinds_resolver()),
        workspace_grants: Some(Arc::new(ProfileGrants {
            workspace: Arc::clone(parts.workspace),
            apps: Arc::clone(parts.apps),
        })),
        legacy_workspace: Some(Arc::clone(parts.workspace) as Arc<dyn LegacyWorkspaceProjection>),
        workspace_governance: Some(media_import::service(parts.workspace, parts.media)?),
        document_content: Some(document_content),
        // The service the identity service answers key requests from:
        // endpoints ask again when it says a channel changed.
        group_keys: Some(parts.keys.clone()),
        action_offload,
        personal: Some(parts.personal),
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

impl RuntimeServices for Standalone {
    type ActionProvider = RuntimeActionProvider;

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
        self.workspaces
            .action_workspace(&self.actor, workspace)
            .map_err(|error| io::Error::new(io::ErrorKind::PermissionDenied, error))
    }

    fn actor(&self) -> &str {
        &self.actor
    }

    fn action_provider(&self) -> io::Result<RuntimeActionProvider> {
        let broker = self
            .broker
            .app_action_broker()
            .ok_or_else(|| io::Error::other("runtime action broker is unavailable"))?;
        let workspaces = Arc::clone(&self.workspaces);
        let (workspace, actor) = (self.workspace.clone(), self.actor.clone());
        let admission =
            service::block_on_owned(
                async move { workspaces.admit_execution(&workspace, &actor).await },
            )
            .map_err(io::Error::other)?;
        let provider = broker
            .certified_local_action_provider(admission.caller(), admission.presentation())
            .map_err(io::Error::other)?;
        admission
            .check(self.workspaces.copies())
            .map_err(io::Error::other)?;
        Ok(RuntimeActionProvider {
            broker,
            workspace: self.workspace.clone(),
            workspaces: Arc::clone(&self.workspaces),
            apps: Some(self.apps.clone()),
            session: Some(Arc::new(AdmittedRuntimeSession {
                policy_time: self.policy_origin.capture().map_err(io::Error::other)?,
                provider,
                admission,
                agent: None,
                projection: Some(Caller::local_person(&self.actor).in_workspace(&self.workspace)),
            })),
        })
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

    fn joiner(&self) -> Option<Joiner> {
        let workspaces = Arc::clone(&self.workspaces);
        let replicas = self.replicas.clone();
        Some(Arc::new(move |invitation: &str| {
            let invitation = invitation.to_owned();
            let (workspace, name) = tilo_rt::app()
                .handle()
                .block_on({
                    let workspaces = Arc::clone(&workspaces);
                    async move {
                        let workspace = workspaces.copies().join_member(&invitation).await?;
                        let name = workspaces
                            .copies()
                            .member_copy(workspace)
                            .await
                            .map_or_else(|| "Workspace".to_owned(), |(name, _)| name);
                        Ok::<_, String>((workspace, name))
                    }
                })
                .map_err(io::Error::other)?;
            // The workspace's files are held from the join on: the folder
            // fills as the workspace's tree arrives.
            if let Some(replicas) = &replicas {
                let _ = hold_files(replicas, &name, workspace);
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

    fn media(&self) -> Option<Arc<dyn tilo_mcp::MediaReader>> {
        Some(Arc::new(profile::BlobMedia::new(
            &self.blob_socket,
            &self.workspace,
        )))
    }

    fn other_devices(&self) -> Option<Arc<tilo_device_mcp::OtherDevices>> {
        Some(Arc::clone(&self.other_devices))
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
        if caller
            != self
                .projection
                .as_ref()
                .unwrap_or_else(|| self.admission.caller())
        {
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
    fn agent_session(
        &self,
        workspace: &str,
        actor: &str,
        policy_time: &tilo_workspace_service::apps::policy_clock::PolicyTime,
        agent: impl FnOnce(&Caller) -> String,
    ) -> io::Result<(Self, Caller, String)> {
        let workspaces = Arc::clone(&self.workspaces);
        let (workspace, actor) = (workspace.to_owned(), actor.to_owned());
        let admission =
            service::block_on_owned(
                async move { workspaces.admit_execution(&workspace, &actor).await },
            )
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
            if session.projection.as_ref() == Some(caller) {
                if !self.admitted(caller) {
                    return Pending::ready(Err(ActionCallError::NotPermitted));
                }
                return self
                    .broker
                    .state()
                    .action_query(&on_this_device(caller), &query);
            }
            if let Err(refusal) = session.check(caller, &self.workspaces) {
                return Pending::ready(Err(refusal.into()));
            }
            if query
                .provider
                .as_ref()
                .is_some_and(|provider| certified_provider(provider))
            {
                return session.provider.query(caller, query);
            }
            // Existing general catalogue/builtin APIs retain the exact
            // authenticated run. They make no SDK original-ownership claim.
            let answer = self.broker.state().action_query(&session.acting(), &query);
            return session.guarded(Arc::clone(&self.workspaces), caller.clone(), answer);
        }
        if !self.admitted(caller) {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        }
        self.broker
            .state()
            .action_query(&on_this_device(caller), &query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if let Err(refused) = tilo_actions::appearance::refuse_here(&request) {
            return Pending::ready(Err(refused));
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
    ) -> Option<Pending<()>> {
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

/// The identity store's device-certificate checks, as the broker asks them.
/// This device governs the profile's own workspace, so a foreign runtime is not
/// held to the cap there.
struct Certificates {
    workspace: String,
    device: [u8; 32],
    policy_time: tilo_workspace_service::apps::policy_clock::PolicyTime,
    workspaces: std::sync::Weak<ServiceWorkspaces>,
}

impl Certificates {
    fn check_registered_ownership(&self, workspace: uuid::Uuid) -> Result<(), String> {
        let workspaces = self
            .workspaces
            .upgrade()
            .ok_or("runtime ownership owner unavailable")?;
        let workspace =
            WorkspaceId::parse(&workspace.to_string()).ok_or("invalid exact workspace")?;
        workspaces.recheck_executions(workspace)
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
        cert: &tilo_user_identity::DeviceCert,
        time: &tilo_workspace_service::apps::policy_clock::PolicyTime,
    ) -> Result<(), String> {
        if !time.same_origin(&self.policy_time) {
            return Err("original runtime policy origin changed".into());
        }
        let now = time.now_ns().map_err(|error| error.to_string())?;
        self.check_registered_ownership(workspace)?;
        tilo_user_identity::verify_device_cert_at(cert, now).map_err(|error| error.to_string())?;
        // This store check is conservative: known revocations are refused even
        // before their effective instant. It never extends a credential lease.
        tilo_identity::certificates::authorize(&workspace.to_string(), cert)
    }

    fn authorize(
        &self,
        workspace: uuid::Uuid,
        cert: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        tilo_identity::certificates::authorize(&workspace.to_string(), cert)
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
        self.check_registered_ownership(workspace)?;
        tilo_identity::certificates::authorize_join(&workspace.to_string(), cert)
    }
    fn governs(&self, workspace: uuid::Uuid, device: Option<&[u8; 32]>) -> bool {
        uuid::Uuid::parse_str(&self.workspace) == Ok(workspace)
            && device.is_none_or(|d| *d == self.device)
    }
}

/// What the person's own store is served with: the profile's state it is kept
/// in, the identity this device acts as, and the workspace this runtime runs.
struct PersonalServing<'a> {
    root: &'a Path,
    identity: &'a tilo_identity::registry::IdentityProfile,
    /// The workspace this runtime runs, by its id.
    workspace_id: &'a str,
    workspaces: &'a ServiceWorkspaces,
    channels: Arc<Mutex<BrokerChannels>>,
    fanout: Arc<FanoutState>,
    /// The device action service this profile answers its server's work on, when it
    /// takes work at all.
    take_work: Option<ActionService>,
    /// The workspace host this device runs, when it governs the profile's
    /// workspace: the person's keys open their sessions with it here.
    host: Option<&'a tilo_workspace_service::service::WorkspaceService>,
}

/// The person's own store, and what serves it to their other devices.
struct PersonalParts {
    sync: PersonalSync,
    links: tilo_workspace_service::apps::personal::Links,
    /// The endpoint's accept loop, answering the person's store to their
    /// other devices.
    router: iroh::protocol::Router,
}

/// Open the person's own store on the identity this device acts as, and serve
/// it to their other devices from the endpoint this runtime runs: the store,
/// the workspaces this device keeps and, for a member that takes work, its
/// device action service answer on one router, so neither protocol is answered by a
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
        workspaces,
        channels,
        fanout,
        take_work,
        host,
    } = serving;
    let (key, device) = tilo_identity::broker::trusted_current_device(workspace_id)?;
    let state = root.join("state");
    let sync = PersonalSync::new(
        tilo_workspace_service::apps::personal::open(
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
        let mut router = iroh::protocol::Router::builder(workspaces.endpoint())
            .accept(tilo_peer_state::PERSONAL_ALPN, sync.clone());
        // The workspace host answers the person's keys as every host does.
        // A member that takes its server's work answers the device action service with
        // that work's action service instead, registered after it.
        for (alpn, handler) in host
            .map(tilo_workspace_service::service::WorkspaceService::protocols)
            .unwrap_or_default()
        {
            router = router.accept(alpn, handler);
        }
        if let Some(action_service) = take_work {
            router = router.accept(tilo_peer_state::actions::ACTIONS_ALPN, action_service);
        }
        router.spawn()
    };
    let links = tilo_workspace_service::apps::personal::Links::serve(
        runtime,
        workspaces.endpoint(),
        sync.clone(),
        &tilo_workspace_service::apps::personal::Person::of(
            identity.id.clone(),
            identity.user_public_key,
        ),
        &state,
        channels,
        fanout,
    );
    Ok(PersonalParts {
        sync,
        links,
        router,
    })
}

/// The MCP servers of the person's other devices, dialed from `joined`'s
/// endpoint as the device the person's store `personal` certifies.
fn other_devices(
    workspaces: &ServiceWorkspaces,
    personal: &PersonalSync,
) -> Arc<tilo_device_mcp::OtherDevices> {
    Arc::new(tilo_device_mcp::OtherDevices::new(
        workspaces.endpoint(),
        Arc::new(PersonalDevices::new(personal.clone())),
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
    let identity_dir = tilo_identity::store::identity_dir_from_env()?;
    let paths = tilo_identity::provisioning::collaboration_identity_paths();
    tilo_identity::provisioning::ensure_foreign_collaboration_identity(
        &identity_dir,
        &paths,
        storage,
    )?;
    Ok(tilo_device_identity::persist::load_device_key(&paths)
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
    let identity_dir = tilo_identity::store::identity_dir_from_env()?;
    let paths = tilo_identity::provisioning::collaboration_identity_paths();
    tilo_identity::provisioning::verify_collaboration_identity(
        &identity_dir,
        &paths,
        tilo_clock::unix_now_ns(),
    )?;
    if !tilo_identity::provisioning::active_identity_owns_device(&identity_dir, &paths)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "native admission identity does not own this device",
        ));
    }
    Ok(tilo_device_identity::persist::load_device_key(&paths)
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
            parts.workspaces.endpoint(),
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
    workspaces: &'a Arc<ServiceWorkspaces>,
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
    let content = parts.workspaces.record_content_service()?;
    let own = parts.own.workspace();
    let own_folder = parts
        .own
        .governs()
        .then(|| files::folder_in(&home, &parts.own.name()));
    let held = Arc::downgrade(parts.workspaces);
    let replicas = Arc::new(files::Replicas::new(files::ReplicaHost {
        profile: parts.root.to_path_buf(),
        home,
        own_folder,
        store: parts.media.store(),
        endpoint: parts.workspaces.endpoint(),
        author: parts.author,
        credentials: Box::new(move |workspace| {
            held.upgrade()
                .ok_or_else(|| io::Error::other("the workspace service closed"))?
                .files_credentials(workspace)
        }),
        collab: parts.root.join(COLLAB_SOCKET),
        identity: parts.root.join(IDENTITY_SOCKET),
        content,
        apps: Arc::clone(parts.apps),
    }));
    let workspaces: Vec<WorkspaceId> = parts
        .workspaces
        .workspaces()
        .iter()
        .map(tilo_desktop_model::Workspace::id)
        .filter(|workspace| workspace.to_string() != own)
        .collect();
    if !workspaces.is_empty() {
        stage("syncing the joined workspaces' files");
    }
    for workspace in workspaces {
        let name = parts
            .workspaces
            .workspace(workspace)
            .map_or_else(|| "Workspace".to_owned(), |workspace| workspace.name);
        let _ = hold_files(&replicas, &name, workspace);
    }
    Some(replicas)
}

/// Hold `workspace`'s files in `replicas`, under the name its governance gives
/// it: the folder that mirrors them, unless they cannot be held.
fn hold_files(replicas: &files::Replicas, name: &str, workspace: WorkspaceId) -> Option<PathBuf> {
    let name = name.to_owned();
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

/// The workspace service this device keeps for the profile, and this device's
/// channel keys for the workspaces it holds.
struct OpenedWorkspaces {
    workspaces: Arc<ServiceWorkspaces>,
    keys: workspace_keys::KeyService,
    key_sessions: Option<Arc<crate::key_sessions::KeySessions>>,
}

/// What opening the device's workspace service is told.
struct OpenWorkspaces<'a> {
    workspace: &'a ProfileWorkspace,
    storage: IdentityStorage,
    broker: (Arc<Mutex<BrokerChannels>>, Arc<FanoutState>),
    listen: Option<u16>,
}

/// Open the workspace service this device keeps for the profile, and this
/// device's channel keys for the workspaces it holds.
///
/// When this device governs the profile's own workspace, the service hosts it:
/// apps create resources there in the kinds their installed manifests declare,
/// or in dev mode (`apps`, `dev`) the kinds a build that is not installed
/// names, and the runtime writes into it through the service. Otherwise the
/// profile's workspace is the desktop session's, and the service holds only
/// the workspaces this device joined as a member.
fn open_workspaces(profile: &Path, opened: OpenWorkspaces<'_>) -> io::Result<OpenedWorkspaces> {
    let OpenWorkspaces {
        workspace,
        storage,
        broker,
        listen,
    } = opened;
    let workspace_id_string = workspace.workspace();
    let governs = workspace.governs();
    let (key, cert) = tilo_identity::broker::trusted_current_device(&workspace_id_string)?;
    let workspace_id = WorkspaceId::parse(&workspace_id_string)
        .ok_or_else(|| io::Error::other("invalid runtime workspace"))?;
    let endpoint = listen_endpoint(&key, listen)?;
    // The device and its service never number changes from one count: the
    // service writes under a key of its own, which this device certifies.
    let store = profile.join(HOST_STORE);
    let service_identity = tilo_rt::app()
        .handle()
        .block_on(
            tilo_workspace_service::service::ServiceIdentity::beside_device(&store, &cert, &key),
        )
        .map_err(io::Error::other)?;
    let key_sessions = governs.then(crate::key_sessions::KeySessions::new);
    let identity = tilo_identity::identity_store::DeviceIdentity::held(
        cert.clone(),
        key.clone(),
        std::sync::Arc::new(tilo_identity::identity_store::files::Files::new(
            tilo_identity::provisioning::collaboration_identity_paths(),
        )),
    );
    let keys = tilo_workspace_service::apps::workspace_keys::KeyProvider::default();
    let inbox = Arc::new(GovernanceInbox::default());
    let config = tilo_workspace_service::service::ServiceConfig {
        apps: key_sessions.as_ref().map(|sessions| {
            Arc::clone(sessions) as Arc<dyn tilo_workspace_service::app_serving::AppActionServer>
        }),
        member: Some(Arc::new(tilo_workspace_service::workspaces::MemberDevice {
            cert: cert.clone(),
            signer: Arc::clone(&identity.signer),
        })),
        ..tilo_workspace_service::service::ServiceConfig::new(
            store,
            tilo_server_protocol::WorkSetting::none(),
        )
    };
    let device_groups = Some(workspace_keys::DeviceGroupKeys::device(
        keys.clone(),
        Arc::clone(&inbox),
    ));
    let handle = tilo_rt::app().handle().clone();
    let opened = if governs {
        handle.block_on(tilo_workspace_service::service::WorkspaceService::open(
            config,
            service_identity,
            endpoint.clone(),
            device_groups,
        ))
    } else {
        handle.block_on(
            tilo_workspace_service::service::WorkspaceService::open_joining(
                config,
                service_identity,
                endpoint.clone(),
                device_groups,
            ),
        )
    };
    let service = Arc::new(opened.map_err(io::Error::other)?);
    let refused = if governs {
        std::collections::BTreeSet::new()
    } else {
        std::collections::BTreeSet::from([workspace_id])
    };
    let workspaces = ServiceWorkspaces::new(
        Arc::clone(&service),
        identity,
        endpoint,
        refused,
        keys,
        inbox,
    );
    if governs {
        tilo_rt::app()
            .handle()
            .block_on(service.hold_own_workspace(&workspace_id_string, &workspace.name(), &cert))
            .map_err(io::Error::other)?;
    }
    let keys = workspace_keys::open(
        profile,
        workspace_id,
        storage,
        key,
        Arc::clone(&workspaces),
        Arc::clone(&broker.0),
    )?;
    Ok(OpenedWorkspaces {
        workspaces,
        keys,
        key_sessions,
    })
}

/// Open a workspace service of `profile` alone, holding the member copies it
/// keeps and entering the ones the server it serves places on it: the service
/// a member that takes work answers on, whether or not it serves a workspace
/// of its own.
pub(in crate::standalone) fn open_member_service(
    profile: &Path,
    key: &ed25519_dalek::SigningKey,
    cert: &tilo_user_identity::DeviceCert,
    identity: tilo_identity::identity_store::DeviceIdentity,
    endpoint: iroh::Endpoint,
) -> io::Result<Arc<ServiceWorkspaces>> {
    let store = profile.join(HOST_STORE);
    let service_identity = tilo_rt::app()
        .handle()
        .block_on(
            tilo_workspace_service::service::ServiceIdentity::beside_device(&store, cert, key),
        )
        .map_err(io::Error::other)?;
    let keys = tilo_workspace_service::apps::workspace_keys::KeyProvider::default();
    let inbox = Arc::new(GovernanceInbox::default());
    let config = tilo_workspace_service::service::ServiceConfig {
        member: Some(Arc::new(tilo_workspace_service::workspaces::MemberDevice {
            cert: cert.clone(),
            signer: Arc::clone(&identity.signer),
        })),
        ..tilo_workspace_service::service::ServiceConfig::new(
            store,
            tilo_server_protocol::WorkSetting::none(),
        )
    };
    let device_groups = Some(workspace_keys::DeviceGroupKeys::device(
        keys.clone(),
        Arc::clone(&inbox),
    ));
    let service = tilo_rt::app()
        .handle()
        .block_on(
            tilo_workspace_service::service::WorkspaceService::open_joining(
                config,
                service_identity,
                endpoint.clone(),
                device_groups,
            ),
        )
        .map_err(io::Error::other)?;
    let service = Arc::new(service);
    let workspaces = ServiceWorkspaces::new(
        Arc::clone(&service),
        identity,
        endpoint,
        std::collections::BTreeSet::new(),
        keys,
        inbox,
    );
    Ok(workspaces)
}

/// Answer the record actions of the profile's workspace (`workspace`, as
/// `actor`) for the person's keys from `broker` from now on. The host opened
/// before the broker and refused them as not running yet until now.
fn serve_key_sessions(
    key_sessions: &crate::key_sessions::KeySessions,
    broker: &BrokerHandle,
    workspaces: &Arc<ServiceWorkspaces>,
    personal: &PersonalSync,
    (workspace, actor): (&str, &str),
    policy_origin: &tilo_workspace_service::apps::policy_clock::PolicyOrigin,
) -> io::Result<()> {
    let broker = broker
        .app_action_broker()
        .ok_or_else(|| io::Error::other("runtime action broker is unavailable"))?;
    key_sessions.serve(
        Arc::new(PersonalDevices::new(personal.clone())),
        Arc::new(host_runs::HostRuns {
            provider: RuntimeActionProvider {
                broker,
                workspace: workspace.to_owned(),
                workspaces: Arc::clone(workspaces),
                apps: None,
                session: None,
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
fn listen_endpoint(
    key: &ed25519_dalek::SigningKey,
    listen: Option<u16>,
) -> io::Result<iroh::Endpoint> {
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
        // Every interface, on the port a server reaches this member at: IPv4,
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
            // from the content store as a package's is.
            if let Err(error) = tilo_content_device::for_app().put(surface) {
                tracing::warn!(%error, app_id, "the development surface could not be stored");
                return;
            }
            tilo_workspace_service::apps::register_package_surface(app_id, surface);
        }
    }
}

#[cfg(test)]
#[path = "standalone_tests.rs"]
mod tests;
