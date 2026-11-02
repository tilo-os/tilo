//! The session's side of the app wire: the resources its services publish,
//! read, watched and invoked by the apps it runs.
//!
//! An app is a process of its own, and the socket it speaks to its session on
//! is the launcher's: [`tilo_action_broker`] serves it, admits the peer by
//! the handoff the launcher stamped on the connection, and keeps the
//! workspaces the apps share. What this session adds to that broker is one
//! provider: what its services publish ([`Link`], as the broker's
//! [`PublishedResources`]). Nothing of the wire itself is served here.
//!
//! The two sides run on two threads. The broker has a runtime of its own; the
//! services belong to the session's loop, the only thread that may touch
//! them, because a service blocks on its device and keeps its worker. [`Link`]
//! is the seam: an app's call becomes an [`Ask`] the loop answers on its next
//! pass, and a watched resource's reading moves to the apps watching it there.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use runtime_collab::wire::{Event, Receipt};
use serde_json::Map;
use tilo_action_broker::NativePeerBrokerConfig;
use tilo_action_broker::channel_encryption::ChannelKeyProvider;
use tilo_action_broker::collab_broker::{BrokerChannels, BrokerHandle, FanoutState};
use tilo_action_broker::collab_outbound::PeerPublisher;
use tilo_action_broker::services::TrackerRuns;
use tilo_action_broker::services::{
    Caller, ContentStoreServices, DisplayControlServices, Granted, HostServices, HostStepServices,
    PublishedResources, Reading, ResourceWatch, SearchIndexServices, ShortcutLibraryRequest,
    ShortcutLibraryServices, ShortcutStore, SpeakerChange, SpeakerServices, WorkspaceAuthority,
    WorkspaceAuthorityHandle, WorkspaceMemberships,
};
use tilo_desktop_search::content_index::AppContentIndex;
use tilo_lock::MutexRecover as _;
use tilo_service::{Services, Started, Wake};
use tokio::sync::{mpsc, oneshot};

use crate::activity_provider::ActivityWire;
use crate::launches::Launches;
use crate::lists::Lists;
use crate::seams::{Seam, Seams};

/// A watch the loop keeps: the resource an app connection watches, and what
/// that connection was last sent.
struct Watch {
    id: ResourceWatch,
    resource: String,
    /// The services' revision this watch was last served at.
    revision: Option<u64>,
    /// What it was last sent, so a change elsewhere does not re-send it.
    reading: Option<Vec<u8>>,
    sink: mpsc::UnboundedSender<Event>,
}

/// What a connection asked, waiting for the loop's pass.
enum Ask {
    Snapshot {
        resource: String,
        reply: oneshot::Sender<Reading>,
    },
    Invoke {
        caller: Caller,
        resource: String,
        action: String,
        /// The arguments, as the JSON object the wire carried.
        arguments: Vec<u8>,
        reply: oneshot::Sender<Receipt>,
    },
}

/// The readings the services publish to granted callers only, as of the
/// revision the catalogue was last read at.
#[derive(Default)]
struct Declared {
    revision: Option<u64>,
    granted: std::collections::HashMap<String, Granted>,
}

/// The seam between the session's loop and the broker's connections: what an
/// app asks of the services, and the readings the apps watch.
pub struct Link {
    asks: Mutex<Vec<Ask>>,
    watches: Mutex<Vec<Watch>>,
    declared: Mutex<Declared>,
    next_watch: AtomicU64,
    wake: Wake,
}

impl Link {
    /// A link that wakes the session's loop with `wake`.
    #[must_use]
    pub fn new(wake: Wake) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            asks: Mutex::new(Vec::new()),
            watches: Mutex::new(Vec::new()),
            declared: Mutex::new(Declared::default()),
            next_watch: AtomicU64::new(1),
            wake,
        })
    }

    fn ask(&self, ask: Ask) {
        self.asks.lock_recover().push(ask);
        (self.wake)();
    }

    /// Answer what the apps asked since the last pass, and send every
    /// watched resource's reading that moved: what the session's loop calls
    /// once per pass, after the services took in what their devices did.
    pub fn serve(&self, services: &mut Services) {
        self.declare(services);
        let asked = std::mem::take(&mut *self.asks.lock_recover());
        for ask in asked {
            match ask {
                Ask::Snapshot { resource, reply } => {
                    let reading = services.read(&resource).map(|value| encoded(&value));
                    let _ = reply.send((services.revision(), reading));
                }
                Ask::Invoke {
                    caller,
                    resource,
                    action,
                    arguments,
                    reply,
                } => {
                    tracing::debug!(
                        app = caller.app_id,
                        workspace = caller.workspace,
                        %resource,
                        %action,
                        "an app invokes a published action"
                    );
                    let _ = reply.send(receipt(services, &resource, &action, &arguments));
                }
            }
        }
        self.publish(services);
    }

    /// Read which readings the services publish to granted callers only, so
    /// the broker admits every call about one before this loop sees it. The
    /// catalogue is the declaration's only source; it moves with the
    /// services' revision like everything else they publish.
    ///
    /// The session reads it once its services are all mounted, before the
    /// broker answers anyone: an app that connects as the session starts is
    /// decided by the workspace's grants, not by a declaration not yet read.
    pub fn declare(&self, services: &Services) {
        let revision = services.revision();
        let mut declared = self.declared.lock_recover();
        if declared.revision == Some(revision) {
            return;
        }
        declared.granted = services
            .resources()
            .into_iter()
            .filter(|resource| resource.readers == tilo_actions::Readers::Granted)
            .map(|resource| {
                // The session's own resources name the session's app, as its
                // actions do: that is whose grants the workspace says may read
                // them.
                let publisher = resource
                    .app_id
                    .unwrap_or_else(|| tilo_action_policy::roles::SESSION_APP.to_owned());
                (
                    resource.id.as_str().to_owned(),
                    Granted {
                        publisher,
                        kind: resource.kind,
                    },
                )
            })
            .collect();
        declared.revision = Some(revision);
    }

    /// Send the readings of the watched resources that moved.
    fn publish(&self, services: &Services) {
        let revision = services.revision();
        let mut watches = self.watches.lock_recover();
        watches.retain_mut(|watch| {
            if watch.revision == Some(revision) {
                return true;
            }
            let reading = services.read(&watch.resource).map(|value| encoded(&value));
            watch.revision = Some(revision);
            if reading == watch.reading && watch.reading.is_some() {
                return true;
            }
            watch.reading.clone_from(&reading);
            watch
                .sink
                .send(Event::Reading {
                    resource: watch.resource.clone(),
                    revision,
                    reading,
                })
                .is_ok()
        });
    }
}

/// The session's services, as the broker's connections read them: a resource
/// is named by its id, and what an app reaches is what the services publish.
impl PublishedResources for Link {
    fn granted(&self, resource: &str) -> Option<Granted> {
        self.declared.lock_recover().granted.get(resource).cloned()
    }

    fn snapshot(&self, _caller: &Caller, resource: &str) -> oneshot::Receiver<Reading> {
        let (reply, answered) = oneshot::channel();
        self.ask(Ask::Snapshot {
            resource: resource.to_owned(),
            reply,
        });
        answered
    }

    fn watch(
        &self,
        _caller: &Caller,
        resource: &str,
        sink: mpsc::UnboundedSender<Event>,
    ) -> ResourceWatch {
        let id = ResourceWatch::new(self.next_watch.fetch_add(1, Ordering::Relaxed));
        self.watches.lock_recover().push(Watch {
            id,
            resource: resource.to_owned(),
            revision: None,
            reading: None,
            sink,
        });
        (self.wake)();
        id
    }

    fn unwatch(&self, id: ResourceWatch) {
        self.watches.lock_recover().retain(|watch| watch.id != id);
    }

    fn invoke(
        &self,
        caller: &Caller,
        resource: &str,
        action: &str,
        arguments: Vec<u8>,
    ) -> oneshot::Receiver<Receipt> {
        let (reply, answered) = oneshot::channel();
        self.ask(Ask::Invoke {
            caller: caller.clone(),
            resource: resource.to_owned(),
            action: action.to_owned(),
            arguments,
            reply,
        });
        answered
    }
}

/// The speaker groups this session's fabric carries, as the broker's
/// Settings route commands them: Settings asks for a snapshot, creates,
/// edits and calibrates a group, and follows their changes, and the speaker
/// service answers from the fabric the loop pumps.
struct FabricSpeakers(tilo_device_fabric::link::SharedLink);

impl SpeakerServices for FabricSpeakers {
    fn command(&self, command: runtime_collab::SpeakerCommand) -> runtime_collab::SpeakerReply {
        tilo_speaker_service::settings::handle(&self.0, command)
    }

    fn changed(&self, after: u64) -> Option<SpeakerChange> {
        Some(Box::pin(tilo_speaker_service::settings::changed(
            &self.0, after,
        )))
    }
}

/// The host service the broker hands apps' search publications to: the
/// session's index of what apps hold, which the desktop's search reads and
/// which keeps what was published after the app closes and the session
/// restarts.
struct ContentIndex(std::sync::Arc<AppContentIndex>);

impl SearchIndexServices for ContentIndex {
    fn publish(
        &self,
        room: tilo_desktop_model::RoomId,
        app: &str,
        record: &str,
        delta: &[u8],
    ) -> Result<(), String> {
        self.0.publish(&room.to_string(), app, record, delta)
    }
}

/// The workspaces' sealed records as this session serves them: the broker
/// the channel service gates, and where an endpoint follows a route's keys.
pub struct Sealed {
    /// The broker's side of the workspaces' sealed records: the channel
    /// service gates a sealed record crossing it.
    pub broker: std::sync::Arc<Mutex<BrokerChannels>>,
    /// Where an endpoint follows when a route's keys change, when this
    /// session's channel service runs.
    pub keys: Option<ChannelKeyProvider>,
}

/// What this session hands the broker besides the link and the launcher: the
/// board every app's live activity shows on, the index its publications land
/// in, the desk it decides a workspace against, the shortcuts it saves and
/// runs, the workspace's activity it serves, and the runs of what people ask
/// its agents.
pub struct Hosted {
    pub live_activities: std::sync::Arc<runtime_collab::LiveActivities>,
    /// The calls this session's apps join: the service an app's
    /// `join-call` reaches.
    pub native_media: Option<tilo_native_media::NativeMediaService>,
    pub content: Option<std::sync::Arc<AppContentIndex>>,
    pub desk: std::sync::Arc<DeskAuthority>,
    pub shortcuts: std::sync::Arc<ShortcutLibrary>,
    pub activity: std::sync::Arc<ActivityWire>,
    pub agent_runs: Option<std::sync::Arc<tilo_agent_runs::AgentRuns>>,
    /// The workspaces' sealed records: what the broker gates, and where an
    /// endpoint follows a route's keys.
    pub sealed: Sealed,
    /// How an app on this device runs one of its actions on another of the
    /// person's devices: what the collaboration plane reaches them through.
    pub own_device: Option<std::sync::Arc<dyn tilo_action_broker::services::OwnDeviceActions>>,
    /// This device's workspaces, whose owner is the canonical copy of what
    /// the apps record in them: an app's write in a workspace the owner
    /// commits is committed there before anyone hears of it, and leaves for
    /// the workspace's peers from there. A change to a workspace's people is
    /// written through it too, and its person is who the broker admits them
    /// as. `None` when the session holds no workspaces.
    pub engine: Option<std::sync::Arc<crate::engine::Engine>>,
    /// The content the workspaces' records reference: where an app keeps
    /// what its record names, and the route an app asks the workspace's other
    /// devices for what it lacks.
    pub record_content: Option<std::sync::Arc<crate::content::Plane>>,
    /// Where an app's offloadable action runs: the round this session asks
    /// the workspace's other devices, when it holds a workspace to ask them
    /// in.
    pub action_offload: Option<std::sync::Arc<dyn tilo_actions::offload::ActionOffload>>,
    /// Where the broker keeps what it admits, under this session's state: a
    /// change is recorded before it runs, and answered from its record when it
    /// is asked again.
    pub journal_root: Option<PathBuf>,
    /// The apps this device has, as search last read them: what the broker
    /// admits an app's declared actions against, by its signed manifest.
    pub lists: Lists,
}

/// Where a session keeping its state at `state` keeps what its broker admits.
#[must_use]
pub fn journal_root(state: &std::path::Path) -> PathBuf {
    state.join("tilo").join("invocations")
}

/// Serve the session's published resources to the apps on `socket`: the
/// broker admits each connection by the launch `launches` recorded, asks
/// `link` for what the services publish, hands Settings the speaker groups
/// of `fabric` when this session runs on one, and decides what an app may do
/// in a workspace against `desk`. What apps publish of their own content lands
/// in `content`, the index search reads.
///
/// `sealed` is the broker's side of the workspaces' sealed records: the
/// channel service gates a sealed record crossing it, and its keys are what
/// an endpoint follows to hear when a route's keys change.
///
/// # Errors
///
/// The socket's directory is not this user's private directory, another
/// session answers on the socket, or the broker's thread could not start.
pub fn serve(
    socket: PathBuf,
    link: std::sync::Arc<Link>,
    launches: std::sync::Arc<Launches>,
    fabric: Option<tilo_device_fabric::link::SharedLink>,
    hosted: Hosted,
) -> std::io::Result<BrokerHandle> {
    let membership: WorkspaceAuthorityHandle = std::sync::Arc::clone(&hosted.desk) as _;
    let channels = std::sync::Arc::clone(&hosted.sealed.broker);
    // The identity store's device-certificate checks, on this session's one
    // policy origin: the broker decides a join and a retained action against
    // them, and the services answer an app's question about a device with them.
    let certificates = std::sync::Arc::new(tilo_identity_authority::NativeCertificates::default());
    let memberships: Option<std::sync::Arc<dyn WorkspaceMemberships>> = hosted
        .engine
        .clone()
        .map(|engine| std::sync::Arc::new(SessionMemberships { engine }) as _);
    // A session with an identity and a state to keep its workspaces in is the
    // person's device itself: what its broker admits is recorded before it runs
    // and answered from that record, so a change survives the session stopping
    // mid-run. A session with neither keeps the doors that need no identity.
    let config =
        hosted
            .engine
            .as_ref()
            .zip(hosted.journal_root.clone())
            .map(|(_, journal_root)| NativePeerBrokerConfig {
                journal_root,
                certificate_authority: std::sync::Arc::clone(&certificates) as _,
            });
    let engine = hosted.engine.clone();
    let services = host_services(
        link,
        launches,
        fabric,
        hosted,
        Some(certificates),
        memberships,
    );
    let publisher = owner_publisher(engine);
    tilo_action_broker::collab_broker::spawn_with_services(
        socket,
        Some(membership),
        publisher,
        config.as_ref(),
        services,
        channels,
        std::sync::Arc::new(FanoutState::new()),
    )
}

/// Where the broker's writes go beyond this device's own apps: into the
/// engine's owner, which publishes what it committed to each workspace's
/// peers. A session that holds no workspaces keeps its apps' writes among
/// them.
pub(crate) fn owner_publisher(
    engine: Option<std::sync::Arc<crate::engine::Engine>>,
) -> std::sync::Arc<PeerPublisher> {
    engine.map_or_else(PeerPublisher::disabled, |engine| {
        PeerPublisher::with_optional_owner_commit(
            None,
            std::sync::Arc::new(tilo_action_broker::owner_commits::NativeOwnerCommits::new(
                engine,
            )),
        )
    })
}

/// A saved shortcut change on its way to the desk, answered once the session's
/// loop applied it: the desk belongs to the loop, and the broker asks from its
/// own thread.
pub struct StoreRequest {
    pub workspace: String,
    /// The person the broker admitted the change for.
    pub actor: String,
    pub change: tilo_automation::ShortcutChange,
    pub done: tokio::sync::oneshot::Sender<Result<(), String>>,
}

/// The workspace's shortcuts as the broker keeps and runs them: what a person
/// saves through the action door lands on the desk the loop shows, and a run
/// executes on `runtime`.
pub struct ShortcutLibrary {
    runtime: tokio::runtime::Handle,
    changes: tokio::sync::mpsc::UnboundedSender<StoreRequest>,
}

impl ShortcutLibrary {
    /// A library whose runs execute on `runtime`, handing what a person saves
    /// to the loop through the channel it comes with.
    #[must_use]
    pub fn new(
        runtime: tokio::runtime::Handle,
    ) -> (Self, tokio::sync::mpsc::UnboundedReceiver<StoreRequest>) {
        let (changes, saved) = tokio::sync::mpsc::unbounded_channel();
        (Self { runtime, changes }, saved)
    }
}

impl ShortcutLibraryServices for ShortcutLibrary {
    fn keymap(&self) -> settings_store::hardware::ShortcutConfig {
        settings_store::hardware::ShortcutConfig::load()
    }

    fn store(&self, request: ShortcutLibraryRequest) -> ShortcutStore {
        let changes = self.changes.clone();
        Box::pin(async move {
            let (done, applied) = tokio::sync::oneshot::channel();
            changes
                .send(StoreRequest {
                    workspace: request.workspace,
                    actor: request.actor,
                    change: request.change,
                    done,
                })
                .map_err(|_| "this session keeps no shortcuts".to_owned())?;
            applied
                .await
                .map_err(|_| "this session stopped before it saved the shortcut".to_owned())?
        })
    }

    fn runtime(&self) -> Result<tokio::runtime::Handle, String> {
        Ok(self.runtime.clone())
    }
}

/// The desk the broker's thread decides against: the workspaces it shows and
/// their shortcuts, as the session's loop last published them.
///
/// The desk itself belongs to the loop ([`tilo_desktop_surface::DesktopHandle`]
/// hands it out on this thread only), while the broker asks about a workspace
/// from its own thread. The loop publishes a copy when the desk it shows
/// moved, and the broker reads the copy.
pub struct DeskAuthority {
    tree: Mutex<tilo_desktop_model::DesktopTree>,
    revocations: crate::engine::Revocations,
    journal: Option<std::path::PathBuf>,
}

impl DeskAuthority {
    /// The authority over `tree`, as the loop sees it now, deciding what each
    /// of its workspaces revokes from `revocations`, and journalling what the
    /// broker admits on the person's behalf under `journal` (`None` for a
    /// session that keeps no state).
    #[must_use]
    pub fn new(
        tree: tilo_desktop_model::DesktopTree,
        revocations: crate::engine::Revocations,
        journal: Option<std::path::PathBuf>,
    ) -> Self {
        Self {
            tree: Mutex::new(tree),
            revocations,
            journal,
        }
    }

    /// The desk as the loop shows it now, for the broker's next decision.
    pub fn publish(&self, tree: tilo_desktop_model::DesktopTree) {
        *self.tree.lock_recover() = tree;
    }
}

/// The desk as the identity service reads it: an app signs in the person's
/// workspace through the desk this session shows, so the workspace the
/// request names is the one the person sees it in.
impl tilo_identity_host::broker::WorkspaceMembership for DeskAuthority {
    fn with_workspace(
        &self,
        workspace: &tilo_desktop_model::RoomId,
        read: &mut dyn FnMut(&tilo_desktop_model::Workspace),
    ) {
        let tree = self.tree.lock_recover();
        if let Some(held) = tree
            .workspaces
            .iter()
            .find(|held| &held.room() == workspace)
        {
            read(held);
        }
    }
}

impl WorkspaceAuthority for DeskAuthority {
    fn with_tree(
        &self,
        read: &mut dyn FnMut(&tilo_desktop_model::DesktopTree),
    ) -> Result<(), String> {
        read(&self.tree.lock_recover());
        Ok(())
    }

    /// A workspace this desk shows is one this device was let into, so its
    /// shortcuts are proven against the revocations its workspace records. A
    /// workspace whose document this device does not hold — a desk kept from
    /// before it joined, or one it left — revokes nothing here: the shortcuts
    /// it shows are proven by what it does hold, and a call it can no longer
    /// make is refused elsewhere.
    fn workspace_revocations(&self, workspace: &str) -> Option<tilo_user_identity::RevocationList> {
        let tree = self.tree.lock_recover();
        tilo_action_broker::services::workspace_in(&tree, workspace)?;
        Some((self.revocations)(workspace).unwrap_or_default())
    }

    /// Where the broker keeps what it admitted on this device's behalf: a
    /// native action — a shortcut a trigger starts, a schedule firing — is
    /// recorded before it runs and settled after, so a session that stops
    /// mid-run neither loses it silently nor runs it twice. The broker
    /// refuses such actions without it, so a session that keeps state keeps
    /// this beside the rest of it.
    fn legacy_journal_root(&self) -> Option<std::path::PathBuf> {
        self.journal.clone()
    }
}

/// A workspace's people as this session's engine writes them: the change goes
/// into the workspace's own record, signed with this device's key, and every
/// device of the workspace admits it as it admits every member's change.
struct SessionMemberships {
    engine: std::sync::Arc<crate::engine::Engine>,
}

impl WorkspaceMemberships for SessionMemberships {
    /// A workspace this session's engine holds is one whose people change
    /// here.
    fn requires_workspace(&self, workspace: &str) -> bool {
        tilo_desktop_model::RoomId::parse(workspace).is_some_and(|room| self.engine.holds(room))
    }

    /// The engine keeps no checkpoints of a workspace's records: its owner
    /// commits them, and a member's changes travel as the changes they are.
    fn keeps_checkpoints(&self, _workspace: &str) -> bool {
        false
    }

    fn health(&self) -> Result<(), String> {
        self.engine.owner().health()
    }

    fn execute(
        &self,
        request: tilo_peer_state::owner_commit::OwnerCommitRequest,
    ) -> tilo_peer_state::owner_commit::CommitFuture {
        let engine = std::sync::Arc::clone(&self.engine);
        Box::pin(async move {
            let identity = engine.identity();
            engine.owner().commit_workspace_membership_command(
                request,
                &identity.device_signing_key,
                std::time::SystemTime::now(),
            )
        })
    }
}

/// What the apps this session runs reach of it: the resources its services
/// publish, the launcher that started them, and — when this session runs on a
/// device fabric — the speaker groups Settings asks that fabric for.
fn host_services(
    link: std::sync::Arc<Link>,
    launches: std::sync::Arc<Launches>,
    fabric: Option<tilo_device_fabric::link::SharedLink>,
    hosted: Hosted,
    certificates: Option<std::sync::Arc<tilo_identity_authority::NativeCertificates>>,
    memberships: Option<std::sync::Arc<dyn WorkspaceMemberships>>,
) -> HostServices {
    seams(link, launches, fabric, hosted, certificates, memberships).into_host_services()
}

/// This session's decision for every seam of the broker's [`HostServices`]:
/// what it serves, and for what it serves nothing, the reason an app's call
/// is refused with (see [`crate::seams`]).
fn seams(
    link: std::sync::Arc<Link>,
    launches: std::sync::Arc<Launches>,
    fabric: Option<tilo_device_fabric::link::SharedLink>,
    hosted: Hosted,
    certificates: Option<std::sync::Arc<tilo_identity_authority::NativeCertificates>>,
    memberships: Option<std::sync::Arc<dyn WorkspaceMemberships>>,
) -> Seams {
    let Hosted {
        live_activities,
        native_media,
        content,
        desk,
        shortcuts,
        activity,
        agent_runs,
        sealed,
        own_device,
        engine: _,
        record_content,
        action_offload,
        journal_root,
        lists,
    } = hosted;
    let shortcut_library: std::sync::Arc<dyn ShortcutLibraryServices> = shortcuts;
    // The certificate checks this session's broker decides a join and a
    // retained action against: the session's own, or one of the tests'.
    let certificates: std::sync::Arc<dyn tilo_action_broker::CertificateAuthority> = certificates
        .unwrap_or_else(|| {
            std::sync::Arc::new(tilo_identity_authority::NativeCertificates::default())
        });
    // This person's settings on this device: each app reaches its own entries
    // and the shared sections, and a display change is written here too.
    let settings = settings_store::source();
    let display_control: Option<std::sync::Arc<dyn DisplayControlServices>> =
        settings.as_ref().map(|source| {
            std::sync::Arc::new(crate::display_control::DisplayControl::native(
                std::sync::Arc::clone(source),
                std::sync::Arc::clone(&desk),
            )) as std::sync::Arc<dyn DisplayControlServices>
        });
    // What each installed app's signed manifest declares it may do on the
    // action door: a ceiling the broker decides an app's calls against.
    let app_action_access = crate::seams::declared_access(lists);
    // Where the broker keeps the actions of apps that are closed, beside the
    // journal of what it admitted: a closed app's actions survive a restart.
    let launchable_catalogue = journal_root.as_ref().and_then(|journal| {
        journal
            .parent()
            .map(|state| state.join("actions").join("launchable.json"))
    });
    Seams {
        approved_checks: Seam::unserved(
            "this session hosts no project's checks: no project enforces any here",
        ),
        resource_views: Seam::unserved(
            "this session reads no other app's existing resources: search lists them instead",
        ),
        committed_documents: Seam::unserved(
            "this session reads no committed document on an app's behalf",
        ),
        // A call an app joins is this session's: the channel it is in, who
        // else is there, and what the app hears of it until it leaves.
        native_media: Seam::when(native_media.map(|media| {
            std::sync::Arc::new(media)
                as std::sync::Arc<dyn tilo_action_broker::services::NativeMediaServices>
        })),
        audio_output: Seam::unserved(
            "the sound service publishes its outputs as a resource of its own",
        ),
        workspace_memberships: Seam::when(memberships),
        // What people ask this session's agents to do: a run is the broker's
        // to deliver, and its reports come back to the run.
        tracker_runs: Seam::when(agent_runs.map(|runs| runs as std::sync::Arc<dyn TrackerRuns>)),
        person_chat: Seam::unserved("this session keeps no chat service for apps"),
        // Which devices this person's identity certifies now: what the broker
        // admits a join with, and what it decides a retained action against.
        native_certificates: Seam::served(certificates),
        // The certificate this device acts under when the person starts
        // something here themselves — a shortcut's press, a schedule's fire
        // time — rather than an app or a peer asking for it.
        local_person: Seam::served(
            std::sync::Arc::new(tilo_identity_authority::NativeLocalPerson)
                as std::sync::Arc<dyn tilo_action_broker::services::LocalPersonServices>,
        ),
        record_changes: Seam::unserved(
            "no workspace origin here applies an agent's task change to a record",
        ),
        legacy_workspace: Seam::unserved(
            "the desk's workspaces are read where they are shown, not through a projection",
        ),
        launcher: Seam::served(launches),
        // The resources this session's services publish, which the apps it
        // admits read, watch and invoke.
        published: Seam::served(link),
        workspace_governance: Seam::unserved(
            "workspace grants are given in the desk's own sheets, not by an app",
        ),
        // A shortcut a person saves lands on the desk, and a shortcut's run
        // executes on the session's runtime.
        shortcut_library: Seam::served(shortcut_library),
        keyboard_shortcuts: Seam::unserved(
            "the shortcut catalogue is not published on this session's action door",
        ),
        continuations: Seam::unserved(
            "this session offers no app's unfinished work to another app",
        ),
        // What an app publishes of its own content is found whether or not
        // it runs, once it is closed and after a restart.
        search_index: Seam::when(content.map(|content| {
            std::sync::Arc::new(ContentIndex(content)) as std::sync::Arc<dyn SearchIndexServices>
        })),
        // Content a record references is kept on this device beside the
        // workspace's records, and asked of the workspace's other devices.
        content_store: Seam::when(
            record_content
                .as_ref()
                .map(|content| content.puts() as std::sync::Arc<dyn ContentStoreServices>),
        ),
        workspace_control: Seam::unserved(
            "the person creates and joins workspaces from the desk, not on an app's behalf",
        ),
        realtime: Seam::unserved("this session serves no realtime continuity"),
        // The speaker groups of the fabric this session runs on.
        speakers: Seam::when(fabric.map(|fabric| {
            std::sync::Arc::new(FabricSpeakers(fabric)) as std::sync::Arc<dyn SpeakerServices>
        })),
        drives: Seam::unserved(
            "no drive list is told to Files from here: the session's own sheet shows them",
        ),
        applications: Seam::unserved("no app list is told to Files from here"),
        settings: Seam::when(settings),
        display_control: Seam::when(display_control),
        // Every app shows its live activity (a running timer, a stopwatch)
        // outside its windows on this one board.
        live_activities: Seam::served(live_activities),
        wifi_continuity: Seam::unserved("this session serves no wifi continuity"),
        browser_service_sessions: Seam::unserved(
            "this session serves no browser's service sessions",
        ),
        nearby_transfer: Seam::unserved("nearby transfer is not served by this session's app door"),
        file_clipboard: Seam::unserved(
            "the clipboard belongs to the compositor, which serves it to windows itself",
        ),
        app_action_access: Seam::served(app_action_access),
        app_action_grants: Seam::unserved(
            "an app's grant for another app's actions is recorded in the workspace's own \
             governance, which no app reaches here",
        ),
        launchable_catalogue: Seam::when(launchable_catalogue),
        record_content: Seam::when(record_content.map(|content| content.route())),
        // This session's channel keys, when it holds workspaces: a route's keys
        // reach its apps through them, and an endpoint follows their changes
        // here instead of asking again every second.
        channel_keys: Seam::when(sealed.keys),
        // An offloadable action runs where the workspace's devices would run
        // it sooner.
        action_offload: Seam::when(action_offload),
        // An app's action on another of the person's own devices is carried
        // there by the collaboration plane.
        own_device: Seam::when(own_device),
        workspace_nodes: Seam::unserved(
            "a node's workspace is reached through the desk's own sharing, not by an app",
        ),
        personal: Seam::unserved(
            "this session keeps no personal-scope store: the person's records travel with \
             their workspaces",
        ),
        // The workspace's activity: what the Activity app shows, served by
        // the desk's own model.
        host_steps: Seam::served(vec![activity as std::sync::Arc<dyn HostStepServices>]),
    }
}

/// A value as the wire carries it: its JSON.
fn encoded(value: &serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_default()
}

/// What invoking one of the services' actions answered.
fn receipt(services: &mut Services, resource: &str, action: &str, arguments: &[u8]) -> Receipt {
    let arguments = match serde_json::from_slice::<Map<String, serde_json::Value>>(arguments) {
        Ok(arguments) => arguments,
        Err(error) => {
            return Receipt::Refused {
                code: "invalid_arguments".into(),
                message: format!("the arguments are not a JSON object: {error}"),
            };
        }
    };
    match services.invoke(resource, action, &arguments) {
        Some(Ok(Started::Done(output))) => match serde_json::to_vec(&output) {
            Ok(output) => Receipt::Done(output),
            Err(error) => Receipt::Refused {
                code: "failed".into(),
                message: format!("the action's output does not encode: {error}"),
            },
        },
        Some(Ok(Started::Accepted)) => Receipt::Accepted,
        Some(Err(error)) => Receipt::Refused {
            code: error.code().to_owned(),
            message: error.to_string(),
        },
        None => Receipt::Refused {
            code: "unknown_action".into(),
            message: format!("this session publishes no `{resource}` resource"),
        },
    }
}

/// The plane while the session serves it: what the apps ask of the services,
/// and the broker that answers them on the socket.
pub(crate) struct Plane {
    pub(crate) link: std::sync::Arc<Link>,
    /// The socket the broker answers on: what the launcher hands an app, and
    /// what a surface app connects for itself.
    pub(crate) path: std::path::PathBuf,
    /// The desk the apps' broker decides against, which the loop keeps level
    /// with the desk it shows.
    pub(crate) desk: std::sync::Arc<DeskAuthority>,
    /// Held so the broker's thread ends and its socket is unlinked with the
    /// session.
    pub(crate) broker: tilo_action_broker::collab_broker::BrokerHandle,
    /// The calls of the workspaces this session takes part in, as the loop
    /// heard them: what a voice channel shows.
    pub(crate) calls: std::sync::Arc<crate::calls::Calls>,
    /// The service apps join a call through, when this session carries
    /// calls at all.
    pub(crate) calls_service: Option<tilo_native_media::NativeMediaService>,
}

impl Plane {
    /// Say this device's call where everyone in the workspace reads it, and take
    /// in what they say of theirs: the channel a person is in and who is in
    /// each channel's call, which is what this session's apps show of calls.
    pub(crate) fn follow_calls(&self, sync: &crate::sync::Client) {
        let own = self
            .calls_service
            .as_ref()
            .and_then(tilo_native_media::NativeMediaService::own_call);
        let name = own.as_ref().map_or_else(String::new, |call| {
            // What the app calls the channel, so the workspace reads a name and
            // not an id; what the people in it said is the fallback.
            self.action_broker()
                .and_then(|broker| {
                    crate::calls::channel_name(
                        &broker.surfaces(),
                        (&call.workspace, &call.app, &call.record),
                        &call.channel,
                    )
                })
                .unwrap_or_else(|| self.calls.name(&call.channel))
        });
        let presence = own.as_ref().map(|call| crate::sync::InCall {
            call: ui::CallPresence {
                channel: call.channel.clone(),
                name,
            },
            camera: call.camera,
            screen: call.screen,
        });
        sync.in_call(presence.clone());
        self.calls.follow(sync, presence.as_ref());
    }

    /// The broker the apps' runs are started on, while it serves one.
    pub(crate) fn action_broker(
        &self,
    ) -> Option<std::sync::Arc<tilo_action_broker::collab_broker::AppActionBroker>> {
        self.broker.app_action_broker().map(std::sync::Arc::new)
    }

    /// The broker a program the person admitted as an agent is served
    /// through, while it takes work.
    pub(crate) fn agent_broker(
        &self,
    ) -> Option<std::sync::Arc<tilo_action_broker::collab_app_broker::AppBrokerState>> {
        self.broker.fanout.app_broker()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_device_fabric::link::{FabricLink, PersonalFabricSendError};
    use tilo_device_fabric::speaker_group::{
        AudioEndpoint, AudioEndpointKind, EndpointSelector, SpeakerFabricState,
    };
    use tilo_device_fabric::{
        DeviceFabric, DeviceFabricSnapshot, DeviceId, DeviceKind, DevicePresence,
        FABRIC_SCHEMA_VERSION, FabricCapabilities, FabricStamp, OwnerSession, PersonalFabricAction,
    };

    /// This laptop's device fabric, with the one output it publishes.
    struct Fabric {
        local: DeviceId,
        converged: DeviceFabric,
    }

    impl Fabric {
        fn laptop() -> Self {
            let local = DeviceId::new("this-laptop").expect("a device id");
            let mut converged = DeviceFabric::default();
            converged
                .upsert(DeviceFabricSnapshot {
                    schema_version: FABRIC_SCHEMA_VERSION,
                    owner: local.clone(),
                    presence: DevicePresence {
                        device: local.clone(),
                        label: "This laptop".into(),
                        kind: DeviceKind::Laptop,
                        capabilities: FabricCapabilities {
                            audio_output: true,
                            audio_group: true,
                            ..FabricCapabilities::default()
                        },
                        online: true,
                        last_seen_unix_ms: 1,
                        wake_routes: Vec::new(),
                        session: OwnerSession::default(),
                        stamp: FabricStamp {
                            counter: 1,
                            device: local.clone(),
                        },
                    },
                    media: Vec::new(),
                    clipboard: Vec::new(),
                    settings: Vec::new(),
                    wifi: Vec::new(),
                    display: None,
                    speakers: SpeakerFabricState {
                        endpoints: vec![AudioEndpoint {
                            selector: EndpointSelector::mint(&local, "alsa_output.analog-stereo"),
                            kind: AudioEndpointKind::Output,
                            label: "Speakers".into(),
                            is_default: true,
                        }],
                        ..SpeakerFabricState::default()
                    },
                    unlock: tilo_device_fabric::unlock::UnlockFabricState::default(),
                })
                .expect("the snapshot is valid");
            Self { local, converged }
        }
    }

    impl FabricLink for Fabric {
        fn device_fabric(&self) -> DeviceFabric {
            self.converged.clone()
        }

        fn online_device_ids(&self) -> std::collections::BTreeSet<DeviceId> {
            std::collections::BTreeSet::from([self.local.clone()])
        }

        fn local_device_fabric_identity(&self) -> Option<(DeviceId, String)> {
            Some((self.local.clone(), "This laptop".into()))
        }

        fn send_personal_fabric_action(
            &self,
            _: &DeviceId,
            _: PersonalFabricAction,
        ) -> Result<[u8; 16], String> {
            Err("no transport in a test".into())
        }

        fn try_send_personal_fabric_action(
            &self,
            _: &DeviceId,
            _: PersonalFabricAction,
        ) -> Result<[u8; 16], PersonalFabricSendError> {
            Err(PersonalFabricSendError::Rejected("no transport".into()))
        }

        fn require_media(&self) -> Result<(), String> {
            Ok(())
        }
    }

    /// The activity's door, which these tests serve nothing on.
    fn requests() -> tilo_desktop_surface::session::ActivityRequests {
        tilo_desktop_surface::session::activity_requests(std::sync::Arc::new(
            std::sync::atomic::AtomicBool::new(false),
        ))
    }

    fn services(fabric: Option<tilo_device_fabric::link::SharedLink>) -> HostServices {
        let wake: Wake = std::sync::Arc::new(|| ());
        let link = Link::new(wake);
        host_services(
            link,
            std::sync::Arc::new(Launches::default()),
            fabric,
            hosted(None),
            None,
            None,
        )
    }

    /// What a session with no index and no desk of its own hands the broker:
    /// the services under test, and the empty desk it decides against.
    fn hosted(content: Option<std::sync::Arc<AppContentIndex>>) -> Hosted {
        hosted_with(content, Lists::default())
    }

    /// The same, over the app lists the test reads: what each app declared it
    /// may do on the action door is served out of them.
    fn hosted_with(content: Option<std::sync::Arc<AppContentIndex>>, lists: Lists) -> Hosted {
        Hosted {
            live_activities: std::sync::Arc::new(runtime_collab::LiveActivities::default()),
            content,
            desk: std::sync::Arc::new(DeskAuthority::new(
                tilo_desktop_model::DesktopTree::default(),
                crate::engine::revocations_of(None),
                None,
            )),
            shortcuts: library(),
            activity: crate::activity_provider::wire(requests()),
            agent_runs: None,
            native_media: None,
            sealed: Sealed {
                broker: std::sync::Arc::new(Mutex::new(BrokerChannels::default())),
                keys: None,
            },
            own_device: None,
            engine: None,
            record_content: None,
            action_offload: None,
            journal_root: None,
            lists,
        }
    }

    /// A shortcut library whose runs would execute on a runtime of the
    /// tests' own. Nothing starts a run here: these tests publish and read,
    /// they save no shortcut.
    fn library() -> std::sync::Arc<ShortcutLibrary> {
        static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
        let runtime = RUNTIME.get_or_init(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a runtime for shortcut runs")
        });
        let (library, saved) = ShortcutLibrary::new(runtime.handle().clone());
        drop(saved);
        std::sync::Arc::new(library)
    }

    /// An app publishes what it holds through the broker, and the index the
    /// session handed it keeps it: it is the same index search reads.
    #[test]
    fn an_apps_search_publication_lands_in_the_index_search_reads() {
        use ui::search::index_publication::{IndexDelta, IndexEntry};

        let content = std::sync::Arc::new(AppContentIndex::in_memory());
        let services = host_services(
            Link::new(std::sync::Arc::new(|| ())),
            std::sync::Arc::new(Launches::default()),
            None,
            hosted(Some(std::sync::Arc::clone(&content))),
            None,
            None,
        );
        let search_index = services
            .search_index
            .expect("a session with an index offers apps its search index");
        let workspace_id = tilo_desktop_model::RoomId::new();
        let delta = IndexDelta {
            reset: true,
            upsert: vec![IndexEntry {
                key: "n1".into(),
                resource: "n1".into(),
                action: None,
                title: "Groceries".into(),
                subtitle: None,
                group: None,
                icon: None,
                boost: 0,
                fields: Vec::new(),
                secret: false,
                references: Vec::new(),
            }],
            ..IndexDelta::default()
        };
        search_index
            .publish(
                workspace_id,
                "so.tilo.notes",
                "tab:0001",
                &delta.encode().expect("a delta"),
            )
            .expect("the index takes the publication");
        assert_eq!(
            content.title(&workspace_id.to_string(), "n1").as_deref(),
            Some("Groceries"),
            "what the app published is in the index search reads"
        );
    }

    /// An app's `offerBlob`, `wantBlob` and `subscribeBlobs`, and the content
    /// put door, are served by the plane this session keeps: the same store
    /// and the same asks of the workspace's other devices. A session that
    /// keeps no content hands the broker none of it.
    #[test]
    fn an_app_keeps_and_asks_for_content_through_the_services_of_this_session() {
        let kept = crate::content::Plane::open(None);
        let wake: Wake = std::sync::Arc::new(|| ());
        let with = host_services(
            Link::new(wake),
            std::sync::Arc::new(Launches::default()),
            None,
            Hosted {
                record_content: Some(kept),
                ..hosted(None)
            },
            None,
            None,
        );
        assert!(
            with.record_content.is_some(),
            "the broker asks the workspace through this session's plane"
        );
        assert!(
            with.content_store.is_some(),
            "and uploads into the store this device keeps"
        );

        let without = services(None);
        assert!(
            without.record_content.is_none() && without.content_store.is_none(),
            "a session that keeps content hands the broker no content plane"
        );
    }

    /// Settings reaches the speaker groups of the fabric this session runs
    /// on, and of nothing else: a session without one has no speaker route.
    #[test]
    fn the_session_answers_settings_speaker_route_from_its_fabric() {
        assert!(
            services(None).speakers.is_none(),
            "a session on no fabric carries no speaker groups"
        );

        let speakers = services(Some(std::sync::Arc::new(Fabric::laptop())))
            .speakers
            .expect("the speaker route is registered");
        let runtime_collab::SpeakerReply::Snapshot { snapshot } =
            speakers.command(runtime_collab::SpeakerCommand::Snapshot)
        else {
            panic!("Settings asks for a snapshot");
        };
        assert!(snapshot.media_enabled);
        let labels = snapshot
            .devices
            .iter()
            .map(|device| device.label.as_str())
            .collect::<Vec<_>>();
        assert_eq!(labels, ["This laptop"]);
        assert_eq!(
            snapshot.devices[0].outputs.len(),
            1,
            "the output this laptop publishes is what Settings can put in a set"
        );
        assert!(
            speakers.changed(0).is_some(),
            "and it tells Settings when that changes"
        );
    }

    /// Every seam the broker offers is decided here: served, served where
    /// this run has what it carries, or not served with the reason an app's
    /// call is refused with. No shipped app calls a seam this session serves
    /// nothing of.
    #[test]
    fn every_seam_is_decided_and_no_shipped_app_asks_for_what_is_not_served() {
        let decided = seams(
            Link::new(std::sync::Arc::new(|| ())),
            std::sync::Arc::new(Launches::default()),
            None,
            hosted(None),
            None,
            None,
        );
        let decisions = decided.decisions();
        assert_eq!(decisions.len(), 42, "the broker's seams, each decided");
        let mut names = std::collections::BTreeSet::new();
        for (name, why) in &decisions {
            assert!(names.insert(*name), "`{name}` is decided once");
            if let Some(why) = why {
                assert!(
                    why.len() > 20,
                    "`{name}` says why it is not served: {why:?}"
                );
            }
        }
        let served = |name: &str| {
            decisions
                .iter()
                .find(|(seam, _)| *seam == name)
                .map(|(_, why)| why.is_none())
                .unwrap_or_else(|| panic!("`{name}` is decided"))
        };
        for name in crate::seams::EVERY_APP {
            assert!(served(name), "every app calls `{name}` on its connection");
        }
        for (app, needed) in crate::seams::SHIPPED {
            for name in *needed {
                assert!(served(name), "{app} calls `{name}`");
            }
        }

        let services = decided.into_host_services();
        assert!(
            services.settings.is_some(),
            "this person's settings are served to their apps"
        );
        assert!(
            services.app_action_access.is_some(),
            "what each installed app declared it may do is served"
        );
        assert!(
            services.published.is_some() && services.launcher.is_some(),
            "the resources an app reads and the launch it is admitted by"
        );
        assert_eq!(
            services.host_steps.len(),
            1,
            "the workspace's activity is served as the step it is"
        );
        assert_eq!(
            services.display_control.is_some(),
            services.settings.is_some(),
            "a display change is written where this person's settings are kept"
        );
        assert!(
            services.drives.is_none() && services.applications.is_none(),
            "nothing is told to Files from a session that serves it nothing"
        );
        assert!(
            services.workspace_memberships.is_none(),
            "a session on no workspace engine has no memberships to serve"
        );
    }

    /// An app's settings are this session's own document: the call the
    /// broker answers through its settings server reads and writes what this
    /// device keeps, scoped to that app.
    #[test]
    fn an_apps_settings_are_the_document_this_session_keeps() {
        let dir = tempfile::tempdir().expect("a profile of the test's own");
        let records: std::sync::Arc<dyn settings_store::SettingsSource> =
            std::sync::Arc::new(settings_store::Store::open(dir.path()).expect("a settings store"));
        let _scoped = settings_store::scoped_source(Some(std::sync::Arc::clone(&records)));

        let settings = services(None)
            .settings
            .expect("this person's settings are served to their apps");
        assert!(
            std::sync::Arc::ptr_eq(&settings, &records),
            "the app writes where the session keeps settings, not a copy"
        );

        let notes = runtime_collab::settings::host::AppSettingsServer::new(
            "so.tilo.notes",
            settings,
            std::sync::Arc::new(|| Ok(())),
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime for the app's calls");
        let local = tokio::task::LocalSet::new();
        let (_, written) = notes
            .update(
                "app_personal",
                br#"{"so.tilo.notes": {"font": "serif"}}"#,
                Some("set-font"),
            )
            .expect("the app writes its own setting");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&written).expect("a settings view"),
            serde_json::json!({"app_personal": {"so.tilo.notes": {"font": "serif"}}})
        );
        assert_eq!(
            serde_json::Value::Object(records.load()),
            serde_json::json!({"app_personal": {"so.tilo.notes": {"font": "serif"}}}),
            "and the write lands in this device's settings document"
        );
        let (_, read) = local
            .block_on(&runtime, notes.read(0))
            .expect("the app reads its setting back");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&read).expect("a settings view"),
            serde_json::json!({"app_personal": {"so.tilo.notes": {"font": "serif"}}})
        );
    }

    /// What an app declared it may do on the action door is served out of the
    /// lists this device read: its own declaration, and nothing for an app
    /// that declared nothing.
    #[test]
    fn an_apps_declared_access_is_what_this_device_read() {
        use tilo_action_broker::app_actions::AppActionAccess;

        let mut lists = crate::lists::AppLists::default();
        lists.declared.insert(
            "so.tilo.notes".into(),
            AppActionAccess {
                query: true,
                invoke: false,
            },
        );
        let services = host_services(
            Link::new(std::sync::Arc::new(|| ())),
            std::sync::Arc::new(Launches::default()),
            None,
            hosted_with(None, Lists::fixed(lists)),
            None,
            None,
        );
        let access = services
            .app_action_access
            .expect("what an installed app declared is served");
        assert_eq!(
            access("so.tilo.notes"),
            AppActionAccess {
                query: true,
                invoke: false
            }
        );
        assert_eq!(
            access("so.tilo.not-installed"),
            AppActionAccess::default(),
            "an app this device did not read declared nothing"
        );
    }
}
