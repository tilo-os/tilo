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
use tilo_action_broker::collab_broker::{BrokerChannels, BrokerHandle, FanoutState};
use tilo_action_broker::collab_outbound::PeerPublisher;
use tilo_action_broker::services::{
    ApplicationServices, BrowserServiceSessionServices, Caller, DisplayControlServices,
    DriveServices, Granted, HostServices, PublishedResources, Reading, ResourceWatch,
    ShortcutLibraryRequest, ShortcutLibraryServices, ShortcutStore, SpeakerChange, SpeakerServices,
    WifiContinuityServices, WorkspaceMemberships,
};
use tilo_actions::DoorError;
use tilo_broker_core::services::TrackerRuns;
use tilo_broker_core::services::{
    ContentStoreServices, SearchIndexServices, WorkspaceAuthority, WorkspaceAuthorityHandle,
};
use tilo_broker_core::steps::HostStepServices;
use tilo_channels::ChannelKeyProvider;
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
                    answer(services, &resource, &action, &arguments, reply);
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
struct FabricSpeakers(crate::platform::fabric::SharedLink);

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
        room: tilo_desktop_model::WorkspaceId,
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
    /// The records' content in that index, read from the records themselves:
    /// what search finds of an app that never opened them here.
    pub records: Option<std::sync::Arc<crate::record_index::RecordIndex>>,
    pub desk: std::sync::Arc<DeskAuthority>,
    pub shortcuts: std::sync::Arc<ShortcutLibrary>,
    pub activity: std::sync::Arc<ActivityWire>,
    /// The turns this session's agents answer, which the broker delivers
    /// as runs.
    pub turns: Option<std::sync::Arc<crate::conversations::Runs>>,
    /// Who keeps each workspace's agents, as its record of them names them:
    /// what the broker admits an agent's session on.
    pub agents: Option<std::sync::Arc<dyn tilo_broker_core::services::AgentRoster>>,
    /// The workspaces' sealed records: what the broker gates, and where an
    /// endpoint follows a route's keys.
    pub sealed: Sealed,
    /// How an app on this device runs one of its actions on another of the
    /// person's devices: what the collaboration plane reaches them through.
    pub own_device: Option<std::sync::Arc<dyn tilo_broker_core::services::OwnDeviceActions>>,
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
    /// The session's tools service, while it serves one: what Files' waits
    /// for its drive and app lists' changes are answered from.
    pub tools: Option<crate::tools::Handle>,
    /// The programs the agents working this session's tasks write, which this
    /// session's door runs as steps of their runs.
    pub task_programs: Option<std::sync::Arc<crate::task_programs::TaskPrograms>>,
    /// The session's own actions — the arrangement, its undo, the windows and the
    /// notices — offered to a turn's agent as the broker's steps, so it arranges the
    /// screen as the REPL does.
    pub session_actions: Option<std::sync::Arc<crate::session_actions::SessionActions>>,
    /// The person's settings, which Settings' window changes through the broker as another
    /// provider's actions: the session keeps them, so its service answers them here.
    pub settings_steps: Option<std::sync::Arc<crate::settings_steps::SettingsSteps>>,
    /// This session's Nearby Send: the person's other devices, and the spool
    /// a file reaches them from. `None` on a session that runs none, where
    /// Files' send has no route at all.
    pub nearby: Option<std::sync::Arc<crate::nearby_transfer::NearbyTransfer>>,
    /// Files' clipboard: what a person copied or cut there, pasted into a
    /// folder of this desktop's, and what it put on the desktop's clipboard.
    pub file_clipboard: std::sync::Arc<crate::file_clipboard::NativeFileClipboard>,
    /// The person's own store, which the broker keeps their own records in as
    /// well as in this desktop: what their other devices' records reach this
    /// session's apps through. `None` on a session that keeps none.
    pub personal: Option<tilo_peer_state::PersonalSync>,
    /// What the broker hands the apps that have a record open: shared with
    /// the person's store, so a write of theirs reaches an app here on the
    /// same route this device's own writes do.
    pub fanout: std::sync::Arc<FanoutState>,
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
    fabric: Option<crate::platform::fabric::SharedLink>,
    hosted: Hosted,
) -> std::io::Result<BrokerHandle> {
    let membership: WorkspaceAuthorityHandle = std::sync::Arc::clone(&hosted.desk) as _;
    let channels = std::sync::Arc::clone(&hosted.sealed.broker);
    let fanout = std::sync::Arc::clone(&hosted.fanout);
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
    // The apps this device has, as its lists last read them: what the broker
    // lists as each app's name and offered actions, and what decides the
    // reach the device's settings app has of the settings document.
    launches.watching(hosted.lists.clone());
    // What search reads of the records themselves, for the changes this
    // session commits and takes in below.
    let records = hosted.records.clone();
    let services = host_services(
        link,
        launches,
        fabric,
        hosted,
        Some(certificates),
        memberships,
    );
    let publisher = owner_publisher(engine, records);
    tilo_action_broker::collab_broker::spawn_with_services(
        socket,
        Some(membership),
        publisher,
        config.as_ref(),
        services,
        channels,
        fanout,
    )
}

/// Where the broker's writes go beyond this device's own apps: into the
/// engine's owner, which publishes what it committed to each workspace's
/// peers. A session that holds no workspaces keeps its apps' writes among
/// them.
pub(crate) fn owner_publisher(
    engine: Option<std::sync::Arc<crate::engine::Engine>>,
    records: Option<std::sync::Arc<crate::record_index::RecordIndex>>,
) -> std::sync::Arc<PeerPublisher> {
    let records = records.map(|records| {
        records as std::sync::Arc<dyn tilo_broker_core::services::RecordIndexChanges>
    });
    engine.map_or_else(PeerPublisher::disabled, |engine| {
        PeerPublisher::with_optional_owner_commit(
            None,
            std::sync::Arc::new(tilo_action_broker::owner_commits::NativeOwnerCommits::new(
                engine, records,
            )),
        )
    })
}

/// The workspace's shortcuts as the broker keeps and runs them: a shortcut a
/// person saves is one of its workspace's settings, and a run executes on
/// `runtime`.
pub struct ShortcutLibrary {
    runtime: tokio::runtime::Handle,
    /// The workspaces this device's records are held in: where a saved shortcut
    /// is written. A session that holds none saves none.
    engine: Option<std::sync::Arc<crate::engine::Engine>>,
}

impl ShortcutLibrary {
    /// A library whose runs execute on `runtime`, saving into the workspaces of
    /// `engine`.
    #[must_use]
    pub fn new(
        runtime: tokio::runtime::Handle,
        engine: Option<std::sync::Arc<crate::engine::Engine>>,
    ) -> Self {
        Self { runtime, engine }
    }
}

/// Store `request`'s change in its workspace: signed as the saver's own device
/// ([`tilo_peer_state::workspace::sign_saved_shortcut`]), then written into the
/// workspace's document as one of its settings. Every device of the workspace
/// admits the write by the same rule, and reads the shortcut back from the
/// document.
async fn save_shortcut(
    engine: std::sync::Arc<crate::engine::Engine>,
    request: ShortcutLibraryRequest,
) -> Result<(), String> {
    let (key, device) = tilo_identity_host::broker::trusted_current_device(&request.workspace)
        .map_err(|error| format!("this device cannot sign in the workspace: {error}"))?;
    if request.actor != hex::encode(device.user_public_key) {
        return Err("a shortcut is saved from its saver's own device".into());
    }
    let change = match &request.change {
        tilo_automation::ShortcutChange::Save(shortcut) => {
            let mut shortcut = shortcut.as_ref().clone();
            // A trigger this device cannot watch would be saved into a shortcut
            // that never runs: the person is told now.
            for trigger in &shortcut.triggers {
                crate::automation::triggers::validate_trigger(trigger)?;
            }
            tilo_peer_state::workspace::sign_saved_shortcut(
                &request.workspace,
                &mut shortcut,
                &device,
                &key,
            )?;
            tilo_automation::ShortcutChange::Save(Box::new(shortcut))
        }
        delete @ tilo_automation::ShortcutChange::Delete(_) => delete.clone(),
    };
    let request = tilo_peer_state::owner_commit::OwnerCommitRequest {
        workspace: request.workspace,
        operation: tilo_peer_state::owner_commit::CommitOperation::WorkspaceMembershipCommand {
            invocation: uuid::Uuid::new_v4(),
            caller: device,
            command: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Settings(
                tilo_peer_state::workspace_membership::SettingsChange::Shortcuts(change),
            ),
        },
    };
    engine
        .owner()
        .commit_workspace_membership_command(request, &key, std::time::SystemTime::now())
        .await
        .map(drop)
}

impl ShortcutLibraryServices for ShortcutLibrary {
    fn keymap(&self) -> settings_store::hardware::ShortcutConfig {
        settings_store::hardware::ShortcutConfig::load()
    }

    fn store(&self, request: ShortcutLibraryRequest) -> ShortcutStore {
        let Some(engine) = self.engine.clone() else {
            return Box::pin(std::future::ready(Err(
                "this session keeps no shortcuts".to_owned()
            )));
        };
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime
                .spawn(save_shortcut(engine, request))
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
    /// Counts the desk's publications, so a reader that only cares whether
    /// the desk changed since it last looked asks one number, and one that
    /// waits for a change hears it ([`Self::until`]).
    generation: tokio::sync::watch::Sender<u64>,
    revocations: crate::engine::Revocations,
    journal: Option<std::path::PathBuf>,
    /// The person at this device, and the ids they went by before it had an identity: a workspace
    /// whose governance names them by one of those is theirs, and what the broker decides in it is
    /// as theirs.
    names: Names,
    /// Who is here in each workspace, by the person they are, as presence names and colours them:
    /// what the broker tells the apps there of each member ([`Self::set_people`]).
    people: Mutex<std::collections::HashMap<String, Vec<tilo_desktop_model::Collaborator>>>,
}

/// Who the person at this device is, as the broker's decisions name them.
#[derive(Clone, Default)]
pub struct Names {
    pub person: String,
    pub earlier: Vec<String>,
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
        names: Names,
    ) -> Self {
        let mut tree = tree;
        claim_earlier_owners(&mut tree, &names);
        Self {
            tree: Mutex::new(tree),
            generation: tokio::sync::watch::Sender::new(0),
            revocations,
            journal,
            names,
            people: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Who is here in `workspace` now: each person by their user root, with the name and colour
    /// every surface here draws them in. The broker tells the apps there, so a person's caret and
    /// marks in a window wear the name and colour of their face on the bar. Nothing is published
    /// while it holds still.
    pub fn set_people(&self, workspace: &str, people: Vec<tilo_desktop_model::Collaborator>) {
        {
            let mut held = self.people.lock_recover();
            if held.get(workspace) == Some(&people) {
                return;
            }
            held.insert(workspace.to_owned(), people);
        }
        let mut tree = self.tree.lock_recover();
        self.seat_people(&mut tree);
        drop(tree);
        self.announce();
    }

    /// Write who is here into each workspace of `tree` presence names people in.
    fn seat_people(&self, tree: &mut tilo_desktop_model::DesktopTree) {
        let people = self.people.lock_recover();
        for workspace in &mut tree.workspaces {
            if let Some(here) = people.get(&workspace.id().to_string()) {
                workspace.collaborators.clone_from(here);
            }
        }
    }

    /// Tell the broker the desk it decides against moved.
    fn announce(&self) {
        self.generation
            .send_modify(|generation| *generation = generation.wrapping_add(1));
        tilo_identity_host::policy_change::announce();
    }

    /// The desk as the loop shows it now, for the broker's next decision, which re-checks what
    /// it holds on the announcement rather than on a clock.
    pub fn publish(&self, mut tree: tilo_desktop_model::DesktopTree) {
        claim_earlier_owners(&mut tree, &self.names);
        self.seat_people(&mut tree);
        *self.tree.lock_recover() = tree;
        self.announce();
    }

    /// Wait until the desk, as the loop publishes it, satisfies `held`: a
    /// change written where the loop takes it from, such as a workspace's
    /// governance, is in force for the broker from then on.
    pub async fn until(&self, held: impl Fn(&tilo_desktop_model::DesktopTree) -> bool) {
        let mut published = self.generation.subscribe();
        while !self.holds(&held) {
            if published.changed().await.is_err() {
                return;
            }
        }
    }

    fn holds(&self, held: &impl Fn(&tilo_desktop_model::DesktopTree) -> bool) -> bool {
        held(&self.tree.lock_recover())
    }

    /// How many times the desk was published; equal values mean the desk
    /// did not change between the two reads.
    pub fn generation(&self) -> u64 {
        *self.generation.borrow()
    }
}

/// A workspace whose governance names the person by an id they went by before this device had an
/// identity is theirs as the person they are now: the desk claims it ([`crate::Shared`]'s
/// `own_workspaces`) and so does the copy the broker decides against, in the same moment, so their
/// own action in it is never refused for the id their own workspace was founded under.
fn claim_earlier_owners(tree: &mut tilo_desktop_model::DesktopTree, names: &Names) {
    if names.earlier.is_empty() {
        return;
    }
    for workspace in &mut tree.workspaces {
        if workspace
            .owner()
            .is_some_and(|owner| names.earlier.iter().any(|earlier| earlier == owner))
        {
            workspace.set_owner(names.person.clone());
        }
    }
}

/// The desk as the identity service reads it: an app signs in the person's
/// workspace through the desk this session shows, so the workspace the
/// request names is the one the person sees it in.
impl tilo_identity_host::broker::WorkspaceMembership for DeskAuthority {
    fn with_workspace(
        &self,
        workspace: &tilo_desktop_model::WorkspaceId,
        read: &mut dyn FnMut(&tilo_desktop_model::Workspace),
    ) {
        let tree = self.tree.lock_recover();
        if let Some(held) = tree.workspaces.iter().find(|held| &held.id() == workspace) {
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
        tilo_broker_core::services::workspace_in(&tree, workspace)?;
        Some(self.revocations.of(workspace).unwrap_or_default())
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

    /// Every write the broker's checks read here is announced: the desk on [`Self::publish`],
    /// the workspaces' governance by their owner, the device fleet and identity registry by
    /// their stores, and an agent's run ending by its door.
    fn announces_policy_changes(&self) -> bool {
        true
    }

    /// What each app installed here is called, as this session's app lists last read it: what
    /// the windows it serves name another app's changes by.
    fn installed_apps(&self) -> Vec<collab_types::history::NamedApp> {
        crate::app_icons::app_lists()
            .map(|lists| lists.named.clone())
            .unwrap_or_default()
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
        tilo_desktop_model::WorkspaceId::parse(workspace)
            .is_some_and(|room| self.engine.holds(room))
    }

    /// An app opening in `workspace` is let author there when this device's
    /// person administers the workspace and it does not let that app author
    /// yet: the same decision the launch before it makes
    /// ([`crate::membership::authorship`]). A member whose role only lets them
    /// write asks nothing of the workspace's people.
    fn app_opens_by_granting(
        &self,
        workspace: &str,
        app: &str,
        _certificate: &tilo_user_identity::DeviceCert,
    ) -> bool {
        if !self.engine.administers(workspace) {
            return false;
        }
        self.engine.policy(workspace).is_ok_and(|policy| {
            !policy.app_authors(
                app,
                &tilo_action_policy::Context::at_time(std::time::SystemTime::now()),
            )
        })
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
            engine
                .owner()
                .commit_workspace_membership_command(
                    request,
                    identity.signer.as_ref(),
                    std::time::SystemTime::now(),
                )
                .await
        })
    }
}

/// The grants a person gives an app for another app's action, written into
/// the workspace's governance by this session's engine, as every other change
/// of its people is: signed with this device's key and admitted by every
/// device of the workspace. A grant is in force once the desk the broker
/// decides against carries it.
pub(crate) struct SessionGrants {
    engine: std::sync::Arc<crate::engine::Engine>,
    desk: std::sync::Arc<DeskAuthority>,
}

impl SessionGrants {
    pub(crate) const fn new(
        engine: std::sync::Arc<crate::engine::Engine>,
        desk: std::sync::Arc<DeskAuthority>,
    ) -> Self {
        Self { engine, desk }
    }

    /// Write `change` into `workspace`'s governance as this device.
    fn commit(
        &self,
        workspace: &str,
        change: tilo_action_policy::Change,
    ) -> tilo_peer_state::owner_commit::CommitFuture {
        use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
        let engine = std::sync::Arc::clone(&self.engine);
        let request = OwnerCommitRequest {
            workspace: workspace.to_owned(),
            operation: CommitOperation::WorkspaceMembershipCommand {
                invocation: uuid::Uuid::new_v4(),
                caller: engine.identity().device_cert.clone(),
                command:
                    tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Member {
                        change,
                    },
            },
        };
        Box::pin(async move {
            let identity = engine.identity();
            engine
                .owner()
                .commit_workspace_membership_command(
                    request,
                    identity.signer.as_ref(),
                    std::time::SystemTime::now(),
                )
                .await
        })
    }

    /// Whether the desk the broker decides against shows `grant` in
    /// `workspace`.
    fn shown(
        tree: &tilo_desktop_model::DesktopTree,
        workspace: &str,
        grant: &tilo_action_policy::GrantId,
    ) -> bool {
        tilo_broker_core::services::workspace_in(tree, workspace)
            .is_some_and(|held| held.policy.grants.iter().any(|kept| &kept.id == grant))
    }
}

impl tilo_action_broker::services::AppActionGrants for SessionGrants {
    /// The person at this device grants where its engine holds the
    /// workspace's governance and the workspace lets this device administer it.
    fn may_grant(&self, workspace: &str, person: &str) -> bool {
        let names = &self.desk.names;
        (names.person == person || names.earlier.iter().any(|earlier| earlier == person))
            && tilo_desktop_model::WorkspaceId::parse(workspace)
                .is_some_and(|id| self.engine.holds(id))
            && self.engine.administers(workspace)
    }

    fn record(
        &self,
        request: &tilo_action_broker::app_actions::AppActionConsentRequest,
        grant: tilo_action_policy::Grant,
        _answer: tilo_action_broker::app_actions::AppActionConsent,
    ) -> tilo_action_broker::services::GrantRecording {
        let workspace = request.workspace.clone();
        let id = grant.id.clone();
        let committed = self.commit(&workspace, tilo_action_policy::Change::Grant(grant));
        let desk = std::sync::Arc::clone(&self.desk);
        Box::pin(async move {
            committed.await?;
            desk.until(|tree| Self::shown(tree, &workspace, &id)).await;
            Ok(id.as_str().to_owned())
        })
    }

    fn revoke(&self, workspace: &str, grant: &str) {
        let committed = self.commit(
            workspace,
            tilo_action_policy::Change::Revoke(tilo_action_policy::GrantId::new(grant)),
        );
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(grant, "no runtime to take back a grant given once on");
            return;
        };
        let grant = grant.to_owned();
        runtime.spawn(async move {
            if let Err(error) = committed.await {
                tracing::warn!(grant, %error, "a grant given once could not be taken back");
            }
        });
    }
}

/// What the apps this session runs reach of it: the resources its services
/// publish, the launcher that started them, and — when this session runs on a
/// device fabric — the speaker groups Settings asks that fabric for.
fn host_services(
    link: std::sync::Arc<Link>,
    launches: std::sync::Arc<Launches>,
    fabric: Option<crate::platform::fabric::SharedLink>,
    hosted: Hosted,
    certificates: Option<std::sync::Arc<tilo_identity_authority::NativeCertificates>>,
    memberships: Option<std::sync::Arc<dyn WorkspaceMemberships>>,
) -> HostServices {
    let decided = seams(link, launches, fabric, hosted, certificates, memberships);
    // What this session serves nothing of, and why: a session start leaves
    // every decision in its log, so a seam an app found missing can be found
    // here with the reason it was left out.
    for (seam, why) in decided.decisions() {
        if let Some(why) = why {
            tracing::debug!(seam, why, "this session serves nothing of this seam");
        }
    }
    decided.into_host_services()
}

/// This session's decision for every seam of the broker's [`HostServices`]:
/// what it serves, and for what it serves nothing, the reason it was left
/// out (see [`crate::seams`]).
#[allow(
    clippy::too_many_lines,
    reason = "one seam per line, in the broker's order: a reviewer reads every decision at once"
)]
fn seams(
    link: std::sync::Arc<Link>,
    launches: std::sync::Arc<Launches>,
    fabric: Option<crate::platform::fabric::SharedLink>,
    hosted: Hosted,
    certificates: Option<std::sync::Arc<tilo_identity_authority::NativeCertificates>>,
    memberships: Option<std::sync::Arc<dyn WorkspaceMemberships>>,
) -> Seams {
    let Hosted {
        live_activities,
        native_media,
        content,
        records,
        desk,
        shortcuts,
        activity,
        turns,
        agents,
        sealed,
        own_device,
        engine,
        record_content,
        action_offload,
        journal_root,
        lists,
        tools,
        task_programs,
        session_actions,
        settings_steps,
        nearby,
        file_clipboard,
        personal,
        fanout: _,
    } = hosted;
    let shortcut_library: std::sync::Arc<dyn ShortcutLibraryServices> = shortcuts;
    // What each window this session runs shows, kept with its launches: read
    // by the seam a window's app speaks on, and by the session's own passes.
    let window_notes = launches.notes();
    // The desk decides who may administer a workspace's identity, and the
    // fabric is where pairing reaches the person's other devices.
    let desk_authority = std::sync::Arc::clone(&desk);
    let grants_desk = std::sync::Arc::clone(&desk);
    let steps_fabric = fabric.clone();
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
                &desk,
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
    // The person's account custody: the vault stays on this device, under
    // their own config root, and a session with no workspace of its own has
    // no route to decide a call on.
    let service_sessions: Option<std::sync::Arc<dyn BrowserServiceSessionServices>> =
        engine.as_ref().map(|engine| {
            std::sync::Arc::new(crate::service_sessions::ServiceSessions::new(
                std::sync::Arc::clone(engine),
            )) as std::sync::Arc<dyn BrowserServiceSessionServices>
        });
    // The hosts of the workspaces placed away from this device: where the person's actions on
    // those workspaces go, where their workspace list is kept, and where the devices of that
    // record are read back from.
    let hosting = engine.as_ref().and_then(|engine| {
        crate::hosted_workspaces::HostedWorkspaces::service(
            Some(engine),
            tilo_rt::app().handle().clone(),
        )
    });
    Seams {
        // No approved-check runner is installed with this session: a check a
        // project's app asks for has nothing to run it.
        approved_checks: Seam::unserved(
            "no project's approved check runs here: this session installs no check service",
        ),
        // This device's own copy of each workspace's records: an app's Open of a record the
        // workspace holds, and an agent's task read, are answered from it, sealed writes opened
        // with the record channel's keys.
        committed_documents: crate::seams::document_reads(engine.as_ref()),
        // A call an app joins is this session's: the channel it is in, who
        // else is there, and what the app hears of it until it leaves.
        native_media: Seam::when(native_media.map(|media| {
            std::sync::Arc::new(media)
                as std::sync::Arc<dyn tilo_action_broker::services::NativeMediaServices>
        })),
        // An app's sound plays on this device's output, rendered here: a hosted app has no socket
        // and no mixer of its own.
        #[cfg(target_os = "linux")]
        audio_output: Seam::served(std::sync::Arc::new(
            crate::audio_output::AppAudioOutput::new(),
        )),
        #[cfg(not(target_os = "linux"))]
        audio_output: Seam::unserved("this host plays no app audio itself"),
        workspace_memberships: Seam::when(memberships),
        // What people ask this session's agents to do: a turn a person's
        // message in a conversation started. A run is the broker's to
        // deliver, and its reports come back to the turn.
        tracker_runs: Seam::when(turns.map(|turns| turns as std::sync::Arc<dyn TrackerRuns>)),
        // Which agents each workspace keeps, and whose session runs each: the
        // record the broker admits an agent's join on.
        agents: Seam::when(agents),
        // Which devices this person's identity certifies now: what the broker
        // admits a join with, and what it decides a retained action against.
        native_certificates: Seam::served(certificates),
        // The certificate this device acts under when the person starts
        // something here themselves — a shortcut's press, a schedule's fire
        // time — rather than an app or a peer asking for it.
        local_person: Seam::served(
            std::sync::Arc::new(tilo_identity_authority::NativeLocalPerson)
                as std::sync::Arc<dyn tilo_broker_core::services::LocalPersonServices>,
        ),
        // A record's own origin is its app's collaboration plane route, and a
        // task's change reaches it as the app's action; the session applies
        // no record change of its own for the door to ask.
        record_changes: Seam::unserved(
            "an agent's change to a record goes through the record's own app, not through \
             a record origin this session keeps",
        ),
        // The desk's own migration is the one reader of what an older
        // session journaled, and it imports it once into this session's
        // workspaces.
        legacy_workspace: Seam::unserved(
            "older workspaces are imported once by the desk's migration, and no app feeds \
             their journal through this session",
        ),
        launcher: Seam::served(launches),
        // The resources this session's services publish, which the apps it
        // admits read, watch and invoke.
        published: Seam::served(link),
        // A workspace's grants are the desk's: the person gives them on its
        // sheets, and the broker decides against the desk it was started with.
        workspace_governance: Seam::unserved(
            "workspace grants are given in the desk's own sheets, not by an app",
        ),
        // A shortcut a person saves lands on the desk, and a shortcut's run
        // executes on the session's runtime.
        shortcut_library: Seam::served(shortcut_library),
        // The device's shortcut catalogue, as the shortcut guide and
        // Settings read it: the broker hands over the keymap, and only this
        // machine's callers are offered it.
        keyboard_shortcuts: Seam::served(crate::keyboard_shortcuts::servable()),
        // The continuation channel a window's app speaks on: its checkpoint
        // and handoff reports are taken by the session that runs it rather
        // than refused. What a window shows is read from the broker's record
        // subscriptions instead ([`crate::continuations`]).
        continuations: Seam::served(window_notes),
        // What an app publishes of its own content is found whether or not
        // it runs, once it is closed and after a restart.
        search_index: Seam::when(content.map(|content| {
            std::sync::Arc::new(ContentIndex(content)) as std::sync::Arc<dyn SearchIndexServices>
        })),
        // The records' content in that index, read from the records
        // themselves: a change to a workspace's record is indexed here.
        record_index_changes: Seam::when(records.map(|records| {
            records as std::sync::Arc<dyn tilo_broker_core::services::RecordIndexChanges>
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
        speakers: Seam::when(fabric.clone().map(|fabric| {
            std::sync::Arc::new(FabricSpeakers(fabric)) as std::sync::Arc<dyn SpeakerServices>
        })),
        // Files waits on the tools service for both lists to change, and asks
        // it for the lists themselves: the drive list and the app list are
        // that service's, in this session as in the launcher's.
        drives: Seam::when(
            tools
                .clone()
                .map(|tools| std::sync::Arc::new(tools) as std::sync::Arc<dyn DriveServices>),
        ),
        applications: Seam::when(
            tools
                .map(|tools| std::sync::Arc::new(tools) as std::sync::Arc<dyn ApplicationServices>),
        ),
        settings: Seam::when(settings),
        display_control: Seam::when(display_control),
        // Every app shows its live activity (a running timer, a stopwatch)
        // outside its windows on this one board.
        live_activities: Seam::served(live_activities),
        // The network the person is on is the session's own service: Settings
        // reads where this device can be woken, and shares a passphrase it
        // learned with the person's other devices — or joins with one they
        // shared — through the fabric this session runs on.
        wifi_continuity: Seam::when(fabric.map(|fabric| {
            std::sync::Arc::new(crate::wifi_continuity::WifiContinuity::new(fabric))
                as std::sync::Arc<dyn WifiContinuityServices>
        })),
        // The session is the custodian of the person's account secrets: Browser
        // resolves, lists and forgets its website sessions here, Settings seals
        // an account's secret, and Mail is released the one it keeps. Every
        // call is decided on the certificate the broker attested for the app's
        // connection in its workspace, and the vault stays on this device.
        browser_service_sessions: Seam::when(service_sessions),
        // Files offers a file to the person's other devices over the same
        // device fabric this session already runs: one Nearby Send stages a
        // screenshot and a file alike, and the receiving device's person
        // chooses where it lands there.
        nearby_transfer: Seam::when(nearby.map(|nearby| {
            nearby as std::sync::Arc<dyn tilo_action_broker::services::NearbyTransferServices>
        })),
        // A person copies and cuts in Files and pastes the copy in another
        // folder or another app: what they copied is offered here, and every
        // file of it is pasted by the same engine the desktop's own paste of
        // files runs.
        file_clipboard: Seam::served(file_clipboard),
        app_action_access: Seam::served(app_action_access),
        // A grant for an app to take another app's action is kept in a
        // workspace's own record, which this session's engine commits; the
        // host records none of them for the door to ask.
        // A grant the person gives an app for another app's action, on the
        // sheet that asked them, is recorded in the workspace's own
        // governance, which this session's engine writes.
        app_action_grants: Seam::when(engine.clone().map(|engine| {
            std::sync::Arc::new(SessionGrants::new(
                engine,
                std::sync::Arc::clone(&grants_desk),
            )) as std::sync::Arc<dyn tilo_action_broker::services::AppActionGrants>
        })),
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
        // A node of a workspace's tree, as an app asks about it: a share, its end, or what the
        // person may do there, decided by the workspace's own governance.
        workspace_nodes: Seam::when(crate::workspace_nodes::WorkspaceNodes::service(
            engine.as_ref(),
        )),
        // The person's own records, kept in their store as well as in this
        // desktop: what an app writes of its own scope reaches their other
        // devices, and what those wrote reaches the app here.
        personal: Seam::when(personal),
        // A workspace placed on a host has its records changed there: the
        // person's actions on them are sent to the host, which decides and
        // runs them, and the change reaches the windows here by sync. The
        // person's list is kept there too, and the devices of it read back
        // from the host that keeps it, so Settings lists the same devices on
        // every device of theirs.
        hosted_workspaces: Seam::when(hosting.clone().map(|hosting| {
            std::sync::Arc::new(hosting)
                as std::sync::Arc<dyn tilo_action_broker::services::HostedWorkspaces>
        })),
        // The steps only this session may take for the person: the
        // workspace's activity the Activity app reads, pairing a nearby
        // device, administering their identities and the devices trusted
        // under them, whose secret stays here, and their hosting account.
        host_steps: Seam::served(
            [
                vec![
                    activity as std::sync::Arc<dyn HostStepServices>,
                    std::sync::Arc::new(crate::pairing::BrokerDoor {
                        desk: std::sync::Arc::clone(&desk_authority),
                        fabric: steps_fabric,
                        pairing: crate::pairing::Pairing::session(),
                    }),
                    std::sync::Arc::new(crate::identity_admin::BrokerDoor {
                        desk: desk_authority,
                        admin: crate::identity_admin::IdentityAdmin::session(),
                        hosting,
                        engine: engine.clone(),
                    }),
                    // The person's account with the host the session runs
                    // on: what Settings and the overview of their Tilo show,
                    // followed as the host's plans change.
                    crate::host_account::HostAccountDoor::session(),
                ],
                // An agent's task writes its programs; this session's door runs them as the
                // steps of the run the task is in.
                task_programs
                    .map(|programs| programs as std::sync::Arc<dyn HostStepServices>)
                    .into_iter()
                    .collect(),
                // The session's own actions, published where every other caller of the
                // desktop reaches them: the arrangement, its undo, the windows and the
                // notices, which a turn's agent arranges the screen with (shell.md §8.1).
                session_actions
                    .map(|actions| actions as std::sync::Arc<dyn HostStepServices>)
                    .into_iter()
                    .collect(),
                // The person's settings, under the provider Settings' window names for every
                // control it shows: the session keeps them, so its settings service runs each
                // change, with the broker admitting it as any caller's first.
                settings_steps
                    .map(|settings| settings as std::sync::Arc<dyn HostStepServices>)
                    .into_iter()
                    .collect(),
            ]
            .concat(),
        ),
    }
}

/// A value as the wire carries it: its JSON.
fn encoded(value: &serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_default()
}

/// Answer `reply` with what invoking one of the services' actions answered: at once, or when the
/// async work it started lands.
fn answer(
    services: &mut Services,
    resource: &str,
    action: &str,
    arguments: &[u8],
    reply: oneshot::Sender<Receipt>,
) {
    let arguments = match serde_json::from_slice::<Map<String, serde_json::Value>>(arguments) {
        Ok(arguments) => arguments,
        Err(error) => {
            let _ = reply.send(Receipt::Refused {
                code: "invalid_arguments".into(),
                message: format!("the arguments are not a JSON object: {error}"),
            });
            return;
        }
    };
    let done = match services.invoke(resource, action, &arguments) {
        Some(Ok(Started::Done(output))) => Ok(output),
        Some(Ok(Started::Accepted)) => {
            let _ = reply.send(Receipt::Accepted);
            return;
        }
        Some(Ok(Started::Later(later))) => {
            later.then(move |landed| {
                let _ = reply.send(receipt(landed.unwrap_or_else(|| {
                    Err(DoorError::Failed("the action's work was given up".into()))
                })));
            });
            return;
        }
        Some(Err(error)) => Err(error),
        None => {
            let _ = reply.send(Receipt::Refused {
                code: "unknown_action".into(),
                message: format!("this session publishes no `{resource}` resource"),
            });
            return;
        }
    };
    let _ = reply.send(receipt(done));
}

/// What an action that finished answered, as the wire carries it.
fn receipt(done: Result<tilo_actions::ActionOutput, DoorError>) -> Receipt {
    match done {
        Ok(output) => match serde_json::to_vec(&output) {
            Ok(output) => Receipt::Done(output),
            Err(error) => Receipt::Refused {
                code: "failed".into(),
                message: format!("the action's output does not encode: {error}"),
            },
        },
        Err(error) => Receipt::Refused {
            code: error.code().to_owned(),
            message: error.to_string(),
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
    /// The people's carets in the workspaces this session takes part in: the person here's as
    /// their apps publish it, and everyone else's as this session's apps show them.
    pub(crate) carets: crate::carets::Carets,
    /// Files' clipboard, whose copies the loop puts on the desktop's
    /// clipboard: what the person pastes into any other app.
    pub(crate) file_clipboard: std::sync::Arc<crate::file_clipboard::NativeFileClipboard>,
    /// The person's own store open to their other devices, held so its links
    /// and the writes it keeps end with the session.
    pub(crate) _personal: Option<tilo_action_broker::personal::Door>,
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

    /// The route to one window's app, for the step of a move it is asked for:
    /// what answers on the window's continuation channel.
    pub(crate) fn handoff(&self) -> tilo_action_broker::collab_broker::SessionFollowDispatcher {
        self.broker.follow_dispatcher()
    }

    /// The broker a program the person admitted as an agent is served
    /// through, while it takes work.
    pub(crate) fn agent_broker(
        &self,
    ) -> Option<std::sync::Arc<tilo_action_broker::collab_app_broker::AppBrokerState>> {
        self.broker.fanout.app_broker()
    }

    /// The hosts of the workspaces this device takes part in but does not
    /// host, while the broker reaches them: where a page of a hosted window
    /// asks the host holding it about a departure it holds for the person.
    pub(crate) fn hosted_workspaces(
        &self,
    ) -> Option<std::sync::Arc<dyn tilo_action_broker::services::HostedWorkspaces>> {
        self.action_broker()
            .and_then(|broker| broker.hosted_workspaces())
    }
}

/// Whether the shortcut catalogue the services hand an app on this machine names every
/// shortcut of the desktop.
#[cfg(test)]
fn shortcut_catalogue(services: &tilo_action_broker::services::HostServices) -> bool {
    let Some(keys) = services.keyboard_shortcuts.as_ref() else {
        return false;
    };
    let listed = keys
        .apply(
            &settings_store::hardware::ShortcutConfig::default(),
            &tilo_actions::ActionAddress::new(
                tilo_sheets::keyboard_shortcuts::RESOURCE,
                tilo_sheets::keyboard_shortcuts::LIST_ACTION,
            ),
            None,
        )
        .expect("the catalogue is answered")
        .value
        .expect("the catalogue is the answer");
    listed["shortcuts"]
        .as_array()
        .is_some_and(|rows| !rows.is_empty())
}

/// The session's own actions as the broker's steps, for a fixture: a door that is never asked
/// anything.
#[cfg(test)]
fn test_session_actions() -> std::sync::Arc<crate::session_actions::SessionActions> {
    let doors =
        crate::agent_door::SessionDoors::new(tilo_service::LoopWake::default().handle(), "Pat");
    std::sync::Arc::new(crate::session_actions::SessionActions::new(
        std::sync::Arc::new(doors),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::fabric::{
        AudioEndpoint, AudioEndpointKind, EndpointSelector, SpeakerFabricState,
    };
    use crate::platform::fabric::{
        DeviceFabric, DeviceFabricSnapshot, DeviceId, DeviceKind, DevicePresence,
        FABRIC_SCHEMA_VERSION, FabricCapabilities, FabricStamp, OwnerSession, PersonalFabricAction,
    };
    use crate::platform::fabric::{FabricLink, PersonalFabricSendError};

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
                    unlock: crate::platform::fabric::UnlockFabricState::default(),
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

    fn services(fabric: Option<crate::platform::fabric::SharedLink>) -> HostServices {
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
            records: None,
            desk: std::sync::Arc::new(DeskAuthority::new(
                tilo_desktop_model::DesktopTree::default(),
                crate::engine::revocations_of(None),
                None,
                Names::default(),
            )),
            shortcuts: library(),
            activity: crate::activity_provider::wire(requests()),
            turns: None,
            agents: None,
            native_media: None,
            sealed: Sealed {
                broker: std::sync::Arc::new(Mutex::new(BrokerChannels::default())),
                keys: None,
            },
            own_device: None,
            nearby: None,
            engine: None,
            record_content: None,
            action_offload: None,
            journal_root: None,
            lists,
            tools: None,
            task_programs: None,
            session_actions: Some(test_session_actions()),
            settings_steps: None,
            file_clipboard: std::sync::Arc::new(crate::file_clipboard::NativeFileClipboard::new(
                std::sync::Arc::new(|| ()),
            )),
            personal: None,
            fanout: std::sync::Arc::new(FanoutState::new()),
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
        std::sync::Arc::new(ShortcutLibrary::new(runtime.handle().clone(), None))
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
        let workspace_id = tilo_desktop_model::WorkspaceId::new();
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
                ..IndexEntry::default()
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

    /// What each shipped app's sources call of the action door: the seam each
    /// call reaches, and the apps whose sources make it.
    fn seams_shipped_apps_call()
    -> std::collections::BTreeMap<&'static str, std::collections::BTreeSet<String>> {
        fn sources(directory: &std::path::Path, into: &mut Vec<std::path::PathBuf>) {
            let Ok(entries) = std::fs::read_dir(directory) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    sources(&path, into);
                } else if path.extension().is_some_and(|extension| extension == "rs") {
                    into.push(path);
                }
            }
        }
        let apps = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps");
        let mut files = Vec::new();
        sources(&apps, &mut files);
        let mut called: std::collections::BTreeMap<
            &'static str,
            std::collections::BTreeSet<String>,
        > = std::collections::BTreeMap::new();
        for file in files {
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            let app = file
                .strip_prefix(&apps)
                .ok()
                .and_then(|path| path.components().next())
                .map_or_else(
                    || file.display().to_string(),
                    |app| app.as_os_str().to_string_lossy().into_owned(),
                );
            for &(marker, seam) in crate::seams::CALLED_SEAMS {
                if text.contains(marker) {
                    called.entry(seam).or_default().insert(app.clone());
                }
            }
        }
        called
    }

    /// Every seam a shipped app's sources call is served, or named in
    /// [`crate::seams::UNSERVED_BUT_CALLED`] as one this session still owes
    /// the app; that list shrinks as each call is served or moved off the
    /// seam, and a call it does not name fails here.
    fn shipped_apps_calls_are_served(decisions: &[(&'static str, Option<&'static str>)]) {
        let served = |name: &str| {
            decisions
                .iter()
                .find(|(seam, _)| *seam == name)
                .map_or_else(|| panic!("`{name}` is decided"), |(_, why)| why.is_none())
        };
        let called = seams_shipped_apps_call();
        assert!(
            called.len() >= 4,
            "the shipped apps' sources are read and their calls found: {called:?}"
        );
        for (seam, apps) in &called {
            let reason = decisions.iter().find(|(name, _)| name == seam).map_or_else(
                || panic!("`{seam}` is not a seam of the broker"),
                |(_, why)| *why,
            );
            if reason.is_none() {
                assert!(
                    !crate::seams::UNSERVED_BUT_CALLED.contains(seam),
                    "`{seam}` is served now: take it off the list of seams still to serve"
                );
                continue;
            }
            assert!(
                crate::seams::UNSERVED_BUT_CALLED.contains(seam),
                "{apps:?} call `{seam}`, and this session serves nothing of it: {reason:?}"
            );
        }
        for seam in crate::seams::UNSERVED_BUT_CALLED {
            assert!(
                called.contains_key(seam),
                "no shipped app calls `{seam}` any more: take it off the list of seams still to serve"
            );
            assert!(
                !served(seam),
                "`{seam}` is served now: take it off the list of seams still to serve"
            );
        }
    }

    /// A session that holds workspaces reads an app's record outside the app, from this device's
    /// own copy; one that holds none reads only on the app's own channel.
    #[test]
    fn a_session_that_holds_workspaces_reads_a_record_outside_its_app() {
        let wake: Wake = std::sync::Arc::new(|| ());
        let services = |engine| {
            let mut hosted = hosted(None);
            hosted.engine = engine;
            host_services(
                Link::new(std::sync::Arc::clone(&wake)),
                std::sync::Arc::new(Launches::default()),
                None,
                hosted,
                None,
                None,
            )
            .committed_documents
            .is_some()
        };
        assert!(
            !services(None),
            "a session that holds no workspace reads a record only on the app's own channel"
        );
        let state = tempfile::tempdir().expect("a state directory");
        let engine = std::sync::Arc::new(
            crate::engine::Engine::open(
                held_identity(),
                Some(state.path()),
                std::iter::empty(),
                crate::platform::custody::of_this_desk(),
            )
            .expect("this device's engine opens"),
        );
        assert!(
            services(Some(engine)),
            "this device's copy of the workspace's records is what an app's Open reads"
        );
    }

    /// A device of a person, certified for their root, that holds workspaces.
    fn held_identity() -> tilo_identity_host::custody::HostIdentity {
        let key = ed25519_dalek::SigningKey::from_bytes(&[0x34; 32]);
        let cert = tilo_peer_state::owner::testing::member_certificate(
            &ed25519_dalek::SigningKey::from_bytes(&[0x33; 32]),
            &key,
            "Laptop",
        );
        tilo_identity_host::custody::HostIdentity::kept(cert, key)
    }

    /// Every seam the broker offers is decided here: served, served where
    /// this run has what it carries, or not served with the reason it was
    /// left out. Every seam a shipped app's sources call is served, or named
    /// as one still to serve — so an app that starts calling a seam this
    /// session serves nothing of fails this test.
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
        // The broker names its seams, so a seam is decided here and never
        // dropped from the list: the field's conversion above would not
        // notice a name that went missing.
        assert_eq!(
            decisions.iter().map(|(seam, _)| *seam).collect::<Vec<_>>(),
            tilo_action_broker::services::HostServices::SEAMS,
            "the broker's seams, each decided in its order"
        );
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
                .map_or_else(|| panic!("`{name}` is decided"), |(_, why)| why.is_none())
        };
        for name in crate::seams::EVERY_APP {
            assert!(served(name), "every app calls `{name}` on its connection");
        }
        shipped_apps_calls_are_served(&decisions);

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
            5,
            "the steps this session serves are the workspace's activity, device \
             pairing, identity administration, the person's hosting account and its own actions"
        );
        let providers: Vec<&str> = services
            .host_steps
            .iter()
            .map(|steps| steps.provider())
            .collect();
        for (provider, why) in [
            (
                tilo_actions::remote::device_pairing::PROVIDER,
                "Settings pairs a device",
            ),
            (
                tilo_actions::remote::identity_admin::PROVIDER,
                "Settings administers the person's devices",
            ),
            (
                tilo_actions::remote::SESSION_ACTIONS_PROVIDER,
                "a turn's agent arranges the screen",
            ),
            (
                tilo_actions::remote::host_account::PROVIDER,
                "Settings' Billing reads the person's account with their host",
            ),
        ] {
            assert!(
                providers.contains(&provider),
                "{why} through this session: {providers:?}"
            );
        }
        assert_eq!(
            services.display_control.is_some(),
            services.settings.is_some(),
            "a display change is written where this person's settings are kept"
        );
        assert!(
            shortcut_catalogue(&services),
            "every shortcut of this desktop is listed"
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
        use tilo_broker_core::access::AppActionAccess;

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
