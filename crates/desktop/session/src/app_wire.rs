//! The session's side of the app wire: the resources its services publish,
//! read, watched and invoked by the apps it runs.
//!
//! An app is a process of its own, and the socket it speaks to its session on
//! is the launcher's: [`tilo_workspace_service::apps`] serves it, admits the peer by
//! the handoff the launcher stamped on the connection, and keeps the
//! workspaces the apps share. What this session adds to that broker is one
//! provider: what its services publish ([`Link`], as the broker's
//! [`PublishedResources`]). Nothing of the wire itself is served here.
//!
//! The two sides run on two threads. The broker has a runtime of its own; the
//! services belong to the session's loop, the only thread that may touch
//! them, because a service blocks on its device and keeps its worker. [`Link`]
//! is the service: an app's call becomes an [`Ask`] the loop answers on its next
//! pass, and a watched resource's reading moves to the apps watching it there.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use runtime_collab::wire::{Event, Receipt};
use serde_json::Map;
use tilo_actions::ApiError;
use tilo_broker_core::services::{
    ContentStoreServices, SearchIndexServices, WorkspaceAuthority, WorkspaceAuthorityHandle,
};
use tilo_desktop_search::content_index::AppContentIndex;
use tilo_lock::MutexRecover as _;
use tilo_service::{Services, Started, Wake};
use tilo_workspace_keys::KeyProvider;
use tilo_workspace_service::apps::NativePeerBrokerConfig;
use tilo_workspace_service::apps::collab_broker::{BrokerChannels, BrokerHandle, FanoutState};
use tilo_workspace_service::apps::collab_outbound::PeerPublisher;
use tilo_workspace_service::apps::services::{
    ApplicationServices, BrowserServiceSessionServices, Caller, DisplayControlServices,
    DriveServices, ProcessServices, PublishedResources, Reading, ResourceWatch,
    ShortcutLibraryServices, SpeakerChange, SpeakerServices, WifiContinuityServices,
    WorkspaceMemberships,
};
use tilo_workspace_service::workspaces::providers::Providers;
use tokio::sync::{mpsc, oneshot};

use crate::activity_provider::ActivityWire;
use crate::broker_services::{BrokerService, BrokerServices};
use crate::launches::Launches;
use crate::lists::Lists;

/// A watch the loop keeps: the resource an app connection watches, and what
/// that connection was last sent.
struct Watch {
    id: ResourceWatch,
    resource: String,
    /// The services' revision this watch was last served at.
    revision: Option<u64>,
    /// The catalogue revision, at which it was last told no service publishes
    /// the resource: what it asks again after a service was added, and never
    /// at every reading of one it does publish.
    missing: Option<u64>,
    /// What it was last sent, so a change elsewhere does not re-send it.
    reading: Option<Vec<u8>>,
    sink: mpsc::UnboundedSender<Event>,
}

/// What a connection asked, waiting for the loop's pass.
enum Ask {
    Snapshot {
        caller: Caller,
        resource: String,
        reply: oneshot::Sender<Result<Reading, String>>,
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

/// The service between the session's loop and the broker's connections: what an
/// app asks of the services, and the readings the apps watch.
pub struct Link {
    asks: Mutex<Vec<Ask>>,
    watches: Mutex<Vec<Watch>>,
    next_watch: AtomicU64,
    /// The readings that are the person's own, by resource id, as the services published them:
    /// only the session's own programs read those.
    personal: Mutex<Personal>,
    /// The apps this session started, as the broker's connections are admitted by them: an app
    /// the session runs itself, without a package's sandbox, is its own code. Set once the broker
    /// serves the link.
    launches: std::sync::OnceLock<std::sync::Arc<Launches>>,
    wake: Wake,
}

/// [`Link::personal`], with the services' catalogue it was read at: what a service publishes moves
/// only when a service is added.
#[derive(Default)]
struct Personal {
    catalogue: Option<u64>,
    readings: std::collections::BTreeSet<String>,
}

impl Personal {
    /// Read the personal readings out of `resources`, every resource under them included.
    fn read(catalogue: u64, resources: &[tilo_actions::ResourceSpec]) -> Self {
        fn collect(
            into: &mut std::collections::BTreeSet<String>,
            resource: &tilo_actions::ResourceSpec,
        ) {
            if resource.personal {
                into.insert(resource.id.as_str().to_owned());
            }
            for child in &resource.children {
                collect(into, child);
            }
        }
        let mut readings = std::collections::BTreeSet::new();
        for resource in resources {
            collect(&mut readings, resource);
        }
        Self {
            catalogue: Some(catalogue),
            readings,
        }
    }
}

impl Link {
    /// A link that wakes the session's loop with `wake`.
    #[must_use]
    pub fn new(wake: Wake) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            asks: Mutex::new(Vec::new()),
            watches: Mutex::new(Vec::new()),
            next_watch: AtomicU64::new(1),
            personal: Mutex::new(Personal::default()),
            launches: std::sync::OnceLock::new(),
            wake,
        })
    }

    /// A link that serves a test's services: nothing wakes its session.
    #[cfg(test)]
    fn for_test() -> std::sync::Arc<Self> {
        Self::new(std::sync::Arc::new(|| ()))
    }

    /// Refuse `resource`'s reading to `caller` when it is the person's own and the caller is not
    /// a program this session runs itself: a package's app reads what it declares, not the
    /// person's clipboard or their notifications.
    fn reading_granted(&self, caller: &Caller, resource: &str) -> Result<(), String> {
        let personal = self.personal.lock_recover().readings.contains(resource);
        let own_program = self
            .launches
            .get()
            .is_none_or(|launches| launches.own_program(&caller.app_id));
        if personal && !own_program {
            return Err("caller is not granted this reading".to_owned());
        }
        Ok(())
    }

    fn ask(&self, ask: Ask) {
        self.asks.lock_recover().push(ask);
        (self.wake)();
    }

    /// Answer what the apps asked since the last pass, and send every
    /// watched resource's reading that moved: what the session's loop calls
    /// once per pass, after the services took in what their devices did.
    pub fn serve(&self, services: &mut Services) {
        self.read_personal(services);
        let asked = std::mem::take(&mut *self.asks.lock_recover());
        for ask in asked {
            match ask {
                Ask::Snapshot {
                    caller,
                    resource,
                    reply,
                } => {
                    let reading = self.reading_granted(&caller, &resource).map(|()| {
                        (
                            services.revision(),
                            services.read(&resource).map(|value| encoded(&value)),
                        )
                    });
                    let _ = reply.send(reading);
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

    /// Read the personal readings out of `services` again when a service was added.
    fn read_personal(&self, services: &Services) {
        let catalogue = services.catalogue_revision();
        let mut personal = self.personal.lock_recover();
        if personal.catalogue == Some(catalogue) {
            return;
        }
        *personal = Personal::read(catalogue, &services.resources());
    }

    /// Send the readings of the watched resources that moved.
    fn publish(&self, services: &Services) {
        let revision = services.revision();
        let catalogue = services.catalogue_revision();
        let mut watches = self.watches.lock_recover();
        watches.retain_mut(|watch| {
            if watch.revision == Some(revision) {
                return true;
            }
            let reading = services.read(&watch.resource).map(|value| encoded(&value));
            watch.revision = Some(revision);
            if reading.is_none() {
                // No service publishes it yet: the app is told once, and
                // again only once a service was added, which is the only
                // thing that can make one publish it.
                if watch.missing == Some(catalogue) {
                    return true;
                }
                watch.missing = Some(catalogue);
            } else {
                watch.missing = None;
                if reading == watch.reading {
                    return true;
                }
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
    fn snapshot(
        &self,
        caller: &Caller,
        resource: &str,
    ) -> oneshot::Receiver<Result<Reading, String>> {
        let (reply, answered) = oneshot::channel();
        self.ask(Ask::Snapshot {
            caller: caller.clone(),
            resource: resource.to_owned(),
            reply,
        });
        answered
    }

    fn watch(
        &self,
        caller: &Caller,
        resource: &str,
        sink: mpsc::UnboundedSender<Event>,
    ) -> Result<ResourceWatch, String> {
        self.reading_granted(caller, resource)?;
        let id = ResourceWatch::new(self.next_watch.fetch_add(1, Ordering::Relaxed));
        self.watches.lock_recover().push(Watch {
            id,
            resource: resource.to_owned(),
            revision: None,
            missing: None,
            reading: None,
            sink,
        });
        (self.wake)();
        Ok(id)
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

/// Who is present on which device, as this session hears it: the links this device's workspaces
/// hold into each, and the person's own devices on a live session with it, as its fabric hears
/// them.
struct SessionPresence {
    service: tilo_workspace_service::service::ServiceSlot,
    fabric: Option<crate::platform::fabric::SharedLink>,
}

impl tilo_workspace_service::workspaces::PresentDevices for SessionPresence {
    fn attach(&self, workspace: &str, here: &mut Vec<tilo_actions::remote::Here>) {
        if let Some(service) = self.service.service() {
            tilo_workspace_service::workspaces::PresentDevices::attach(
                service.workspaces(),
                workspace,
                here,
            );
        }
        let (Some(fabric), Some(identity)) = (&self.fabric, crate::sync::device_identity()) else {
            return;
        };
        let person = hex::encode(identity.device_cert.user_public_key);
        for device in crate::platform::fabric::FabricLink::online_device_ids(fabric) {
            tilo_workspace_service::workspaces::present_on(
                here,
                &person,
                device.as_str().to_owned(),
            );
        }
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
/// the channel service checks, and where an endpoint follows a route's keys.
pub struct Sealed {
    /// The broker's side of the workspaces' sealed records: the channel
    /// service checks a sealed record crossing it.
    pub broker: std::sync::Arc<Mutex<BrokerChannels>>,
    /// Where an endpoint follows when a route's keys change, when this
    /// session's channel service runs.
    pub keys: Option<KeyProvider>,
}

/// What this session hands the broker besides the link and the launcher: the
/// board every app's live activity shows on, the index its publications land
/// in, the session it decides a workspace against, the shortcuts it saves and
/// runs, the workspace's activity it serves, and the runs of what people ask
/// its agents.
pub struct Hosted {
    pub live_activities: std::sync::Arc<runtime_collab::LiveActivities>,
    /// The calls this session's apps join: the service a view's call steps
    /// reach.
    pub native_media: Option<tilo_native_media::NativeMediaService>,
    pub content: Option<std::sync::Arc<AppContentIndex>>,
    /// The records' content in that index, read from the records themselves:
    /// what search finds of an app that never opened them here.
    pub records: Option<std::sync::Arc<crate::resource_index::RecordIndex>>,
    pub layout: std::sync::Arc<LayoutAuthority>,
    /// The workspace's shortcuts as the broker saves and runs them, from this device's own
    /// workspace service; no session without a device identity keeps any.
    pub shortcuts: Option<std::sync::Arc<dyn ShortcutLibraryServices>>,
    /// The workspace's events and its assistant's look-round, saved through those shortcuts:
    /// what a person and their assistant set up, change and pause.
    pub initiative: Option<tilo_workspace_service::agents::initiative::Initiative>,
    pub activity: std::sync::Arc<ActivityWire>,
    /// Who keeps each workspace's agents, as its record of them names them:
    /// what the broker admits an agent's session on.
    pub agents: Option<std::sync::Arc<dyn tilo_broker_core::services::AgentRoster>>,
    /// The agent sessions the person admitted at the MCP server: what the broker admits each
    /// session's certificate on.
    pub agent_sessions: Option<std::sync::Arc<dyn tilo_broker_core::services::AgentSessions>>,
    /// The workspaces' sealed records: what the broker checks, and where an
    /// endpoint follows a route's keys.
    pub sealed: Sealed,
    /// How an app on this device runs one of its actions on another of the
    /// person's devices: what the peer network reaches them through.
    pub own_device: Option<std::sync::Arc<dyn tilo_broker_core::services::OwnDeviceActions>>,
    /// This device's workspaces, whose owner is the canonical copy of what
    /// the apps record in them: an app's write in a workspace the owner
    /// commits is committed there before anyone hears of it, and leaves for
    /// the workspace's peers from there. A change to a workspace's people is
    /// written through it too, and its person is who the broker admits them
    /// as. `None` when the session holds no workspaces.
    pub engine: Option<std::sync::Arc<crate::engine::Engine>>,
    /// How the person's workspaces stand, as the device's daemon publishes them, and the actions
    /// that change them.
    pub view: Option<std::sync::Arc<crate::workspace_view::WorkspaceView>>,
    /// Where the endpoint this device runs under its key lands once bound:
    /// what it dials the hosts of its workspaces from. `None` when the
    /// session runs none.
    pub endpoint: Option<std::sync::Arc<std::sync::OnceLock<iroh::Endpoint>>>,
    /// This device's action service, once the network runs it: what a ticket for a new computer
    /// is served through ([`crate::device_tickets`]).
    pub device_actions:
        Option<std::sync::Arc<std::sync::OnceLock<crate::device_actions::DeviceActions>>>,
    /// This device's own workspace service, where it holds the workspaces an app's window writes:
    /// the write is committed by the service, into the copy the session's own writes reach and its
    /// reads answer from, and the owner keeps only a workspace the service holds none of.
    pub service: tilo_workspace_service::service::ServiceSlot,
    /// The shell link the device's daemon handed this session, or why it has none: each app this
    /// session launches is joined at the daemon on a connection vouched for on it, and its
    /// documents are answered there.
    pub daemon: tilo_workspace_service::apps::services::DeviceDaemon,
    /// Each write the daemon takes, as the session follows them: what a query an agent holds open
    /// here is asked again on.
    pub daemon_writes: tilo_session_client::daemon_documents::DaemonWritesFeed,
    /// The content the workspaces' records reference: where an app keeps
    /// what its record names, and the route an app asks the workspace's other
    /// devices for what it lacks.
    pub document_content: Option<std::sync::Arc<crate::content::ContentService>>,
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
    /// session's links runs as steps of their runs.
    pub task_programs:
        Option<std::sync::Arc<tilo_workspace_service::workspaces::task_programs::TaskPrograms>>,
    /// The session's own actions — the layout, its undo, the windows and the
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
    /// What Browser's asks for vault items are answered with: nothing in a person's own
    /// session, and the workspace's server's word where this session holds a workspace's
    /// Browser.
    pub item_lending: std::sync::Arc<crate::item_lending::ItemLending>,
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
/// in a workspace against `session`. What apps publish of their own content lands
/// in `content`, the index search reads.
///
/// `sealed` is the broker's side of the workspaces' sealed records: the
/// channel service checks a sealed record crossing it, and its keys are what
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
) -> std::io::Result<(BrokerHandle, Option<crate::workspace_terminals::Terminals>)> {
    let _ = link.launches.set(std::sync::Arc::clone(&launches));
    let membership: WorkspaceAuthorityHandle = std::sync::Arc::clone(&hosted.layout) as _;
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
    // mid-run. A session with neither keeps the services that need no identity.
    let config =
        hosted
            .engine
            .as_ref()
            .zip(hosted.journal_root.clone())
            .map(|(_, journal_root)| NativePeerBrokerConfig {
                journal_root,
                certificate_authority: std::sync::Arc::clone(&certificates) as _,
            });
    // The apps this device has, as its lists last read them: what the broker
    // lists as each app's name and offered actions, and what decides the
    // reach the device's settings app has of the settings document.
    launches.watching(hosted.lists.clone());
    // An app's writes go to the device's daemon on its own connection there; the receipts of the
    // effects the actions this session runs had are recorded by the daemon too.
    let publisher = match hosted.daemon.clone() {
        Ok(link) => PeerPublisher::with_optional_commits(
            None,
            std::sync::Arc::new(tilo_session_client::daemon_documents::DaemonReceipts(
                tilo_session_client::daemon_documents::DaemonShell::new(
                    Ok(link),
                    tilo_rt::app().handle().clone(),
                ),
            )),
        ),
        Err(reason) => {
            tracing::warn!(%reason, "no daemon records the effects of this session's actions");
            PeerPublisher::disabled()
        }
    };
    let (services, terminals) = host_services_with_hosts(
        link,
        launches,
        fabric,
        hosted,
        Some(certificates),
        memberships,
    );
    let broker = tilo_workspace_service::apps::collab_broker::spawn_with_services(
        socket,
        Some(membership),
        publisher,
        config.as_ref(),
        services,
        channels,
        fanout,
    )?;
    Ok((broker, terminals))
}

/// The session the broker's thread decides against: the workspaces it shows and
/// their shortcuts, as the session's loop last published them.
///
/// The session itself belongs to the loop ([`tilo_session_core::desktop::state::DesktopHandle`]
/// hands it out on this thread only), while the broker asks about a workspace
/// from its own thread. The loop publishes a copy when the session it shows
/// moved, and the broker reads the copy.
pub struct LayoutAuthority {
    tree: Mutex<tilo_desktop_model::Layout>,
    /// Counts the session's publications, so a reader that only cares whether
    /// the session changed since it last looked asks one number, and one that
    /// waits for a change hears it ([`Self::until`]).
    generation: tokio::sync::watch::Sender<u64>,
    revocations: crate::engine::Revocations,
    /// The person at this device, and the ids they went by before it had an identity: a workspace
    /// whose governance names them by one of those is theirs, and what the broker decides in it is
    /// as theirs.
    names: Names,
    /// Who is here in each workspace, as presence names and colours them: what the broker tells the
    /// apps there of each member ([`Self::set_people`]).
    people: Mutex<std::collections::HashMap<String, Vec<collab_types::Member>>>,
    /// This device's engine, whose workspaces the tree does not show are still the session's: the
    /// person's own workspace above all.
    held: Option<std::sync::Arc<crate::engine::Engine>>,
}

/// Who the person at this device is, as the broker's decisions name them.
#[derive(Clone, Default)]
pub struct Names {
    pub person: String,
    pub earlier: Vec<String>,
}

impl LayoutAuthority {
    /// The authority over `tree`, as the loop sees it now, deciding what each
    /// of its workspaces revokes from `revocations`.
    #[must_use]
    pub fn new(
        tree: tilo_desktop_model::Layout,
        revocations: crate::engine::Revocations,
        names: Names,
    ) -> Self {
        let mut tree = tree;
        claim_earlier_owners(&mut tree, &names);
        Self {
            tree: Mutex::new(tree),
            generation: tokio::sync::watch::Sender::new(0),
            revocations,
            names,
            people: Mutex::new(std::collections::HashMap::new()),
            held: None,
        }
    }

    /// The authority over `tree`, also answering for the workspaces `held` — this device's
    /// engine — keeps but the tree does not show. The person's own workspace is one: it is
    /// theirs, held beside the shell, and what its governance says decides a read or a change in
    /// it as an on-screen workspace's does.
    #[must_use]
    pub fn holding(mut self, held: Option<std::sync::Arc<crate::engine::Engine>>) -> Self {
        self.held = held;
        self
    }

    /// `workspace` as this device's engine describes it: its name, policy, sandbox and the names
    /// of its people, as their governance has them. `None` for a workspace this device does not
    /// hold.
    fn held_workspace(&self, workspace: &str) -> Option<tilo_desktop_model::Workspace> {
        let engine = self.held.as_deref()?;
        let id = tilo_desktop_model::WorkspaceId::parse(workspace)?;
        engine
            .owner()
            .with_current(id, |view| {
                let peer = view.peer();
                let Some(document) = peer.admitted_workspace() else {
                    return Ok(None);
                };
                let mut described = tilo_desktop_model::Workspace::new(id, String::new());
                tilo_peer_state::workspace::adopt_workspace(&mut described, document, &[]);
                let Ok(sandbox) = peer.governance().governed_sandbox() else {
                    return Ok(None);
                };
                described.sandbox = sandbox;
                if let Ok(people) = peer.governance().governed_members(tilo_clock::wall()) {
                    described.named_people(
                        people
                            .members
                            .iter()
                            .filter(|member| !member.removed)
                            .filter_map(|member| {
                                Some((member.id.person_id()?, member.name.as_str()))
                            }),
                    );
                }
                Ok(Some(described))
            })
            .ok()
            .flatten()
    }

    /// Who is here in `workspace` now: each person by their user root, with the name and colour
    /// every surface here draws them in. The broker tells the apps there, so a person's caret and
    /// marks in a window wear the name and colour of their face on the bar. Nothing is published
    /// while it holds still.
    pub fn set_people(&self, workspace: &str, people: Vec<collab_types::Member>) {
        {
            let mut held = self.people.lock_recover();
            if held.get(workspace) == Some(&people) {
                return;
            }
            held.insert(workspace.to_owned(), people);
        }
        let mut tree = self.tree.lock_recover();
        self.write_people(&mut tree);
        drop(tree);
        self.announce();
    }

    /// Write who is here into each workspace of `tree` presence names people in.
    fn write_people(&self, tree: &mut tilo_desktop_model::Layout) {
        let people = self.people.lock_recover();
        for workspace in &mut tree.workspaces {
            if let Some(here) = people.get(&workspace.id().to_string()) {
                workspace.members.clone_from(here);
            }
        }
    }

    /// Tell the broker the session it decides against moved.
    fn announce(&self) {
        self.generation
            .send_modify(|generation| *generation = generation.wrapping_add(1));
        tilo_identity::policy_change::announce();
    }

    /// The session as the loop shows it now, for the broker's next decision, which re-checks what
    /// it holds on the announcement rather than on a clock.
    pub fn publish(&self, mut tree: tilo_desktop_model::Layout) {
        claim_earlier_owners(&mut tree, &self.names);
        self.write_people(&mut tree);
        *self.tree.lock_recover() = tree;
        self.announce();
    }

    /// Wait until the session, as the loop publishes it, satisfies `held`: a
    /// change written where the loop takes it from, such as a workspace's
    /// governance, is in force for the broker from then on.
    pub async fn until(&self, held: impl Fn(&tilo_desktop_model::Layout) -> bool) {
        let mut published = self.generation.subscribe();
        while !self.holds(&held) {
            if published.changed().await.is_err() {
                return;
            }
        }
    }

    fn holds(&self, held: &impl Fn(&tilo_desktop_model::Layout) -> bool) -> bool {
        held(&self.tree.lock_recover())
    }

    /// How many times the session was published; equal values mean the session
    /// did not change between the two reads.
    pub fn generation(&self) -> u64 {
        *self.generation.borrow()
    }
}

/// A workspace whose governance names the person by an id they went by before this device had an
/// identity is theirs as the person they are now: the session claims it ([`crate::Shared`]'s
/// `own_workspaces`) and so does the copy the broker decides against, in the same moment, so their
/// own action in it is never refused for the id their own workspace was founded under.
fn claim_earlier_owners(tree: &mut tilo_desktop_model::Layout, names: &Names) {
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

/// The session as the identity service reads it: an app signs in the person's
/// workspace through the session this session shows, so the workspace the
/// request names is the one the person sees it in — or one this device holds
/// beside the shell, the person's own above all.
impl tilo_identity::broker::WorkspaceMembership for LayoutAuthority {
    fn with_workspace(
        &self,
        workspace: &tilo_desktop_model::WorkspaceId,
        read: &mut dyn FnMut(&tilo_desktop_model::Workspace),
    ) {
        {
            let tree = self.tree.lock_recover();
            if let Some(held) = tree.workspaces.iter().find(|held| &held.id() == workspace) {
                read(held);
                return;
            }
        }
        if let Some(held) = self.held_workspace(&workspace.to_string()) {
            read(&held);
        }
    }
}

impl WorkspaceAuthority for LayoutAuthority {
    fn with_tree(&self, read: &mut dyn FnMut(&tilo_desktop_model::Layout)) -> Result<(), String> {
        read(&self.tree.lock_recover());
        Ok(())
    }

    /// A workspace this session acts in is one the shell shows, or one this device holds beside
    /// it: the person's own workspace is never on the shell, and a read or a change in it is
    /// decided by its governance all the same.
    fn with_workspace(
        &self,
        workspace: &str,
        read: &mut dyn FnMut(Option<&tilo_desktop_model::Workspace>),
    ) -> Result<(), String> {
        {
            let tree = self.tree.lock_recover();
            if let Some(shown) = tilo_broker_core::services::workspace_in(&tree, workspace) {
                read(Some(shown));
                return Ok(());
            }
        }
        if let Some(held) = self.held_workspace(workspace) {
            read(Some(&held));
            return Ok(());
        }
        read(None);
        Ok(())
    }

    /// A workspace this session shows is one this device was let into, so its
    /// shortcuts are proven against the revocations its workspace records. A
    /// held workspace the shell does not show is let into all the same. A
    /// workspace whose document this device does not hold — a session kept from
    /// before it joined, or one it left — revokes nothing here: the shortcuts
    /// it shows are proven by what it does hold, and a call it can no longer
    /// make is refused elsewhere.
    fn workspace_revocations(&self, workspace: &str) -> Option<tilo_user_identity::RevocationList> {
        let shown = {
            let tree = self.tree.lock_recover();
            tilo_broker_core::services::workspace_in(&tree, workspace).is_some()
        };
        let held = shown
            || self.held.as_ref().is_some_and(|engine| {
                tilo_desktop_model::WorkspaceId::parse(workspace).is_some_and(|id| engine.holds(id))
            });
        held.then(|| self.revocations.of(workspace).unwrap_or_default())
    }

    /// Every write the broker's checks read here is announced: the session on [`Self::publish`],
    /// the workspaces' governance by their owner, the device fleet and identity registry by
    /// their stores, and an agent's run ending by its links.
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

    fn health(&self) -> Result<(), String> {
        self.engine.owner().health()
    }

    fn execute(
        &self,
        workspace: String,
        caller: tilo_user_identity::DeviceCert,
        command: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
    ) -> tilo_workspace_service::apps::services::WorkspaceMembershipFuture {
        let engine = std::sync::Arc::clone(&self.engine);
        Box::pin(async move {
            let identity = engine.identity();
            let workspace = tilo_desktop_model::WorkspaceId::parse(&workspace)
                .ok_or("a membership command names an invalid workspace")?;
            engine
                .owner()
                .commit_membership(
                    workspace,
                    &caller,
                    &command,
                    identity.signer.as_ref(),
                    std::time::SystemTime::now(),
                )
                .await
                .map(drop)
                .map_err(|error| error.to_string())
        })
    }
}

/// The grants a person gives an app for another app's action, written into
/// the workspace's governance by this session's engine, as every other change
/// of its people is: signed with this device's key and admitted by every
/// device of the workspace. A grant is in force once the session the broker
/// decides against carries it.
pub(crate) struct SessionGrants {
    engine: std::sync::Arc<crate::engine::Engine>,
    layout: std::sync::Arc<LayoutAuthority>,
}

impl SessionGrants {
    pub(crate) const fn new(
        engine: std::sync::Arc<crate::engine::Engine>,
        layout: std::sync::Arc<LayoutAuthority>,
    ) -> Self {
        Self { engine, layout }
    }

    /// Write `change` into `workspace`'s governance as this device.
    fn commit(
        &self,
        workspace: &str,
        change: tilo_action_policy::Change,
    ) -> tilo_workspace_service::apps::services::WorkspaceMembershipFuture {
        let engine = std::sync::Arc::clone(&self.engine);
        let command =
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Member { change };
        let workspace = workspace.to_owned();
        Box::pin(async move {
            let identity = engine.identity();
            let workspace = tilo_desktop_model::WorkspaceId::parse(&workspace)
                .ok_or("a membership change names an invalid workspace")?;
            engine
                .owner()
                .commit_membership(
                    workspace,
                    &identity.device_cert,
                    &command,
                    identity.signer.as_ref(),
                    std::time::SystemTime::now(),
                )
                .await
                .map(drop)
                .map_err(|error| error.to_string())
        })
    }

    /// Whether the layout the broker decides against shows `grant` in
    /// `workspace`.
    fn shown(
        tree: &tilo_desktop_model::Layout,
        workspace: &str,
        grant: &tilo_action_policy::GrantId,
    ) -> bool {
        tilo_broker_core::services::workspace_in(tree, workspace)
            .is_some_and(|held| held.policy.grants.iter().any(|kept| &kept.id == grant))
    }
}

impl tilo_workspace_service::apps::services::WorkspaceGrants for SessionGrants {
    /// The person at this device records where its engine holds the
    /// workspace's governance and the workspace lets this device administer it.
    fn may_record(&self, workspace: &str, person: &str) -> bool {
        let names = &self.layout.names;
        (names.person == person || names.earlier.iter().any(|earlier| earlier == person))
            && tilo_desktop_model::WorkspaceId::parse(workspace)
                .is_some_and(|id| self.engine.holds(id))
            && self.engine.administers(workspace)
    }

    fn record(
        &self,
        request: &tilo_workspace_service::apps::app_actions::AppActionConsentRequest,
        grant: tilo_action_policy::Grant,
    ) -> tilo_workspace_service::apps::services::GrantRecording {
        let workspace = request.workspace.clone();
        let id = grant.id.clone();
        let committed = self.commit(&workspace, tilo_action_policy::Change::Grant(grant));
        let layout = std::sync::Arc::clone(&self.layout);
        Box::pin(async move {
            committed.await?;
            layout
                .until(|tree| Self::shown(tree, &workspace, &id))
                .await;
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

/// The steps an agent session takes here, registered: the programs its task writes, and the
/// workspace's tools an agent in a process of its own calls.
fn agent_session_steps(
    programs: std::sync::Arc<tilo_workspace_service::workspaces::task_programs::TaskPrograms>,
    providers: &Providers,
) {
    programs.register(providers);
    std::sync::Arc::new(crate::agent_tools::AgentToolSteps::default()).register(providers);
}

/// What the apps this session runs reach of it: the resources its services
/// publish, the launcher that started them, and — when this session runs on a
/// device fabric — the speaker groups Settings asks that fabric for.
#[cfg(test)]
fn host_services(
    link: std::sync::Arc<Link>,
    launches: std::sync::Arc<Launches>,
    fabric: Option<crate::platform::fabric::SharedLink>,
    hosted: Hosted,
    certificates: Option<std::sync::Arc<tilo_identity_authority::NativeCertificates>>,
    memberships: Option<std::sync::Arc<dyn WorkspaceMemberships>>,
) -> ProcessServices {
    host_services_with_hosts(link, launches, fabric, hosted, certificates, memberships).0
}

/// [`host_services`], handing out what a Terminal window's shell in a workspace placed on a
/// host is opened through as well.
fn host_services_with_hosts(
    link: std::sync::Arc<Link>,
    launches: std::sync::Arc<Launches>,
    fabric: Option<crate::platform::fabric::SharedLink>,
    hosted: Hosted,
    certificates: Option<std::sync::Arc<tilo_identity_authority::NativeCertificates>>,
    memberships: Option<std::sync::Arc<dyn WorkspaceMemberships>>,
) -> (
    ProcessServices,
    Option<crate::workspace_terminals::Terminals>,
) {
    // The hosts of the workspaces placed away from this device: where the person's actions on
    // those workspaces go. The device's daemon publishes each workspace's host on the person's
    // list and carries the actions there on its one dispatch; a session the host placed in a
    // workspace's VM runs no daemon, and reaches its host itself.
    let runtime = tilo_rt::app().handle().clone();
    let on_daemon = hosted
        .view
        .clone()
        .zip(hosted.daemon.clone().ok())
        .map(|(view, link)| {
            let _inside = runtime.enter();
            (view, link.dispatch())
        });
    let hosting: Option<
        std::sync::Arc<dyn tilo_workspace_service::apps::services::HostedWorkspaces>,
    > = match &on_daemon {
        Some((view, dispatch)) => Some(std::sync::Arc::new(crate::broker_services::DaemonHosts {
            view: std::sync::Arc::clone(view),
            dispatch: dispatch.clone(),
        })),
        None => hosted.engine.as_ref().and_then(|engine| {
            crate::hosted_workspaces::HostedWorkspaces::service(
                Some(engine),
                hosted.endpoint.clone(),
                runtime.clone(),
            )
            .map(|hosting| {
                std::sync::Arc::new(hosting)
                    as std::sync::Arc<dyn tilo_workspace_service::apps::services::HostedWorkspaces>
            })
        }),
    };
    let terminals = on_daemon
        .zip(hosted.engine.as_ref())
        .map(
            |((view, dispatch), engine)| crate::workspace_terminals::Terminals {
                view,
                dispatch,
                endpoint: hosted.endpoint.clone(),
                device: engine.identity().device_cert.device_public_key,
                runtime,
            },
        );
    let decided = broker_services(
        link,
        launches,
        fabric,
        hosted,
        certificates,
        memberships,
        hosting,
    );
    // What this session serves nothing of, and why: a session start leaves
    // every decision in its log, so a service an app found missing can be found
    // here with the reason it was left out.
    for (service, why) in decided.decisions() {
        if let Some(why) = why {
            tracing::debug!(service, why, "this session serves nothing of this service");
        }
    }
    (decided.into_host_services(), terminals)
}

/// This session's decision for every service of the broker's [`ProcessServices`]:
/// what it serves, and for what it serves nothing, the reason it was left
/// out (see [`crate::services`]).
#[allow(
    clippy::too_many_lines,
    reason = "one service per line, in the broker's order: a reviewer reads every decision at once"
)]
fn broker_services(
    link: std::sync::Arc<Link>,
    launches: std::sync::Arc<Launches>,
    fabric: Option<crate::platform::fabric::SharedLink>,
    hosted: Hosted,
    certificates: Option<std::sync::Arc<tilo_identity_authority::NativeCertificates>>,
    memberships: Option<std::sync::Arc<dyn WorkspaceMemberships>>,
    hosting: Option<std::sync::Arc<dyn tilo_workspace_service::apps::services::HostedWorkspaces>>,
) -> BrokerServices {
    let Hosted {
        live_activities,
        native_media,
        content,
        records,
        layout,
        shortcuts,
        initiative,
        activity,
        agents,
        agent_sessions,
        sealed,
        own_device,
        engine,
        view,
        endpoint: hosted_endpoint,
        service,
        device_actions: hosted_device_actions,
        daemon,
        daemon_writes,
        document_content,
        action_offload,
        journal_root,
        lists,
        tools,
        task_programs,
        session_actions,
        settings_steps,
        nearby,
        file_clipboard,
        item_lending,
        fanout,
    } = hosted;
    // What each window this session runs shows, kept with its launches: read
    // by the service a window's app speaks on, and by the session's own passes.
    let window_notes = launches.notes();
    // A ticket for a new computer: a planned machine's is served again once this device's
    // workspace service opens, after a restart.
    let device_tickets = crate::device_tickets::DeviceTickets::new(
        hosted_endpoint,
        hosted_device_actions,
        service.clone(),
    );
    let present_devices: std::sync::Arc<dyn tilo_workspace_service::workspaces::PresentDevices> =
        std::sync::Arc::new(SessionPresence {
            service: service.clone(),
            fabric: fabric.clone(),
        });
    device_tickets.restore_once_open();
    // The session decides who may administer a workspace's identity.
    let layout_authority = std::sync::Arc::clone(&layout);
    let grants_layout = std::sync::Arc::clone(&layout);
    // The certificate checks this session's broker decides a join and a
    // retained action against: the session's own, or one of the tests'.
    let certificates: std::sync::Arc<dyn tilo_workspace_service::apps::CertificateAuthority> =
        certificates.unwrap_or_else(|| {
            std::sync::Arc::new(tilo_identity_authority::NativeCertificates::default())
        });
    // This person's settings on this device: each app reaches its own entries
    // and the shared sections, and a display change is written here too.
    let settings = settings_store::source();
    let display_control: Option<std::sync::Arc<dyn DisplayControlServices>> =
        settings.as_ref().map(|source| {
            std::sync::Arc::new(crate::display_control::DisplayControl::native(
                std::sync::Arc::clone(source),
                &layout,
            )) as std::sync::Arc<dyn DisplayControlServices>
        });
    // What each installed app's signed manifest declares it may do on the
    // action links: a ceiling the broker decides an app's calls against.
    let app_action_access = crate::broker_services::declared_access(lists.clone());
    let app_resource_kinds = crate::broker_services::declared_resource_kinds(lists);
    // Where the broker keeps the actions of apps that are closed, beside the
    // journal of what it admitted: a closed app's actions survive a restart.
    let launchable_catalogue = journal_root.as_ref().and_then(|journal| {
        journal
            .parent()
            .map(|state| state.join("actions").join("launchable.json"))
    });
    // The person's account credentials: the vault stays on this device, under
    // their own config root, and a session with no workspace of its own has
    // no route to decide a call on.
    let service_sessions: Option<std::sync::Arc<dyn BrowserServiceSessionServices>> =
        engine.as_ref().map(|engine| {
            std::sync::Arc::new(crate::service_sessions::ServiceSessions::new(
                std::sync::Arc::clone(engine),
                std::sync::Arc::clone(&item_lending),
                service.clone(),
            )) as std::sync::Arc<dyn BrowserServiceSessionServices>
        });
    // The providers this session answers, each under the id a call names: the steps it takes
    // itself, the services it reaches in the workspaces it keeps, and the ones a turn's agent or
    // an agent session takes. The dispatch admits each call under the application the
    // registration names.
    let providers = Providers::default();
    activity.register(&providers);
    std::sync::Arc::new(device_tickets).register(&providers);
    crate::machine_steps::MachineSteps::session(view.clone()).register(&providers);
    crate::model_steps::ModelSteps::session(engine.clone()).register(&providers);
    std::sync::Arc::new(crate::identity_admin::BrokerApi {
        layout: layout_authority,
    })
    .register(&providers);
    crate::server_account::ServerAccountApi::session().register(&providers);
    std::sync::Arc::new(tilo_workspace_service::agents::assistant::Transforms::new(
        service.clone(),
        tilo_rt::app().handle().clone(),
    ))
    .register(&providers);
    std::sync::Arc::new(tilo_workspace_service::agents::WebSearch::new(
        tilo_rt::app().handle().clone(),
    ))
    .register(&providers);
    std::sync::Arc::new(tilo_workspace_service::agents::triggered::Answers::new(
        service.clone(),
        std::sync::Arc::clone(&fanout),
    ))
    .register(&providers);
    if let Some(programs) = task_programs {
        agent_session_steps(programs, &providers);
    }
    if let Some(initiative) = &initiative {
        initiative.events_served().register(&providers);
        std::sync::Arc::new(initiative.clone()).register(&providers);
    }
    if let Some(actions) = session_actions {
        // The device's dispatch reaches them over the shell's link too: what the daemon's own
        // endpoint and its agents ask of the layout, the windows and the notices.
        if let Ok(link) = &daemon {
            actions.serve_on(link.clone());
        }
        actions.register(&providers);
    }
    if let Some(settings) = settings_steps {
        settings.register(&providers);
    }
    BrokerServices {
        // No approved-check runner is installed with this session: a check a
        // project's app asks for has nothing to run it.
        approved_checks: BrokerService::unserved(
            "no project's approved check runs here: this session installs no check service",
        ),
        // The device's daemon keeps the workspaces: an app's Open of a record is answered there,
        // on the connection the router joins for it, and an agent's read of one is read from it
        // over the shell's link, never from a copy here.
        committed_documents: BrokerService::when(daemon.clone().ok().map(|link| {
            std::sync::Arc::new(crate::broker_services::DaemonRecordReads(
                tilo_session_client::daemon_documents::DaemonDocuments(
                    tilo_session_client::daemon_documents::DaemonShell::new(
                        Ok(link),
                        tilo_rt::app().handle().clone(),
                    ),
                ),
            ))
                as std::sync::Arc<dyn tilo_workspace_service::apps::services::NativeDocumentReads>
        })),
        // A call an app joins is this session's: the channel it is in, who
        // else is there, and what the app hears of it until it leaves.
        native_media: BrokerService::when(native_media.map(|media| {
            std::sync::Arc::new(media)
                as std::sync::Arc<dyn tilo_workspace_service::apps::services::NativeMediaServices>
        })),
        // An app's sound plays on this device's output, rendered here: a hosted app has no socket
        // and no mixer of its own.
        #[cfg(target_os = "linux")]
        audio_output: BrokerService::served(std::sync::Arc::new(
            crate::audio_output::AppAudioOutput::new(),
        )),
        #[cfg(not(target_os = "linux"))]
        audio_output: BrokerService::unserved("this host plays no app audio itself"),
        workspace_memberships: BrokerService::when(memberships),
        // Which agents each workspace keeps, and whose session runs each: the
        // record the broker admits an agent's join on.
        agents: BrokerService::when(agents),
        // Which devices each person is present on: the links this device's workspaces hold,
        // and the person's own devices its fabric hears.
        present_devices: BrokerService::served(present_devices),
        agent_sessions: BrokerService::when(agent_sessions),
        // Which devices this person's identity certifies now: what the broker
        // admits a join with, and what it decides a retained action against.
        native_certificates: BrokerService::served(certificates),
        // The certificate this device acts under when the person starts
        // something here themselves — a shortcut's press, a schedule's fire
        // time — rather than an app or a peer asking for it.
        local_person: BrokerService::served(std::sync::Arc::new(
            tilo_identity_authority::NativeLocalPerson,
        )
            as std::sync::Arc<dyn tilo_broker_core::services::LocalPersonServices>),
        // A record's own origin is its app's peer network route, and a
        // task's change reaches it as the app's action; the session applies
        // no record change of its own for the links to ask.
        resource_changes: BrokerService::unserved(
            "an agent's change to a record goes through the record's own app, not through \
             a record origin this session keeps",
        ),
        // Where this session's broker keeps what it admits, when it keeps
        // anything at all: no host above it supplies a durable owner.
        journal_root: BrokerService::served(journal_root),
        launcher: BrokerService::served(launches),
        // The resources this session's services publish, which the apps it
        // admits read, watch and invoke.
        published: BrokerService::served(link),
        // A workspace's grants are the session's: the person gives them on its
        // sheets, and the broker decides against the session it was started with.
        workspace_governance: BrokerService::unserved(
            "workspace grants are given in the session's own sheets, not by an app",
        ),
        // A shortcut a person saves lands in its workspace, written through this device's own
        // workspace service, and a shortcut's run executes on the library's runtime. A session
        // with no device identity keeps no shortcut and says so.
        shortcut_library: match shortcuts {
            Some(library) => BrokerService::served(library),
            None => BrokerService::unserved(
                "this session holds no device identity to save a shortcut with",
            ),
        },
        // The device's shortcut catalogue, as the shortcut guide and
        // Settings read it: the broker hands over the keymap, and only this
        // machine's callers are offered it.
        keyboard_shortcuts: BrokerService::served(crate::keyboard_shortcuts::servable()),
        // The continuation channel a window's app speaks on: its checkpoint
        // and handoff reports are taken by the session that runs it rather
        // than refused. What a window shows is read from the broker's record
        // subscriptions instead ([`crate::continuations`]).
        continuations: BrokerService::served(window_notes),
        // What an app publishes of its own content is found whether or not
        // it runs, once it is closed and after a restart.
        search_index: BrokerService::when(content.map(|content| {
            std::sync::Arc::new(ContentIndex(content)) as std::sync::Arc<dyn SearchIndexServices>
        })),
        // The records' content in that index, read from the records
        // themselves: a change to a workspace's record is indexed here.
        document_index_changes: BrokerService::when(records.map(|records| {
            records as std::sync::Arc<dyn tilo_broker_core::services::DocumentIndexChanges>
        })),
        // Content a record references is kept on this device beside the
        // workspace's records, and asked of the workspace's other devices.
        content_store: BrokerService::when(
            document_content
                .as_ref()
                .map(|content| content.puts() as std::sync::Arc<dyn ContentStoreServices>),
        ),
        workspace_control: BrokerService::unserved(
            "the person creates and joins workspaces from the session, not on an app's behalf",
        ),
        realtime: BrokerService::unserved("this session serves no realtime continuity"),
        // The speaker groups of the fabric this session runs on.
        speakers: BrokerService::when(fabric.clone().map(|fabric| {
            std::sync::Arc::new(FabricSpeakers(fabric)) as std::sync::Arc<dyn SpeakerServices>
        })),
        // Files waits on the tools service for both lists to change, and asks
        // it for the lists themselves: the drive list and the app list are
        // that service's, in this session as in the launcher's.
        drives: BrokerService::when(
            tools
                .clone()
                .map(|tools| std::sync::Arc::new(tools) as std::sync::Arc<dyn DriveServices>),
        ),
        applications: BrokerService::when(
            tools
                .map(|tools| std::sync::Arc::new(tools) as std::sync::Arc<dyn ApplicationServices>),
        ),
        settings: BrokerService::when(settings),
        display_control: BrokerService::when(display_control),
        // Every app shows its live activity (a running timer, a stopwatch)
        // outside its windows on this one board.
        live_activities: BrokerService::served(live_activities),
        // The network the person is on is the session's own service: Settings
        // reads where this device can be woken, and shares a passphrase it
        // learned with the person's other devices — or joins with one they
        // shared — through the fabric this session runs on.
        wifi_continuity: BrokerService::when(fabric.map(|fabric| {
            std::sync::Arc::new(crate::wifi_continuity::WifiContinuity::new(fabric))
                as std::sync::Arc<dyn WifiContinuityServices>
        })),
        // The session is the owner of the person's account secrets: Browser
        // resolves, lists and forgets its website sessions here, Settings seals
        // an account's secret, and Mail is released the one it keeps. Every
        // call is decided on the certificate the broker attested for the app's
        // connection in its workspace, and the vault stays on this device.
        browser_service_sessions: BrokerService::when(service_sessions),
        // Files offers a file to the person's other devices over the same
        // device fabric this session already runs: one Nearby Send stages a
        // screenshot and a file alike, and the receiving device's person
        // chooses where it lands there.
        nearby_transfer: BrokerService::when(nearby.map(|nearby| {
            nearby
                as std::sync::Arc<
                    dyn tilo_workspace_service::apps::services::NearbyTransferServices,
                >
        })),
        // A person copies and cuts in Files and pastes the copy in another
        // folder or another app: what they copied is offered here, and every
        // file of it is pasted by the same engine the desktop's own paste of
        // files runs.
        file_clipboard: BrokerService::served(file_clipboard),
        app_action_access: BrokerService::served(app_action_access),
        app_resource_kinds: BrokerService::served(app_resource_kinds),
        // An answer the person gives "allow for this issue" is recorded in the
        // workspace's own governance, which this session's engine writes.
        workspace_grants: BrokerService::when(engine.clone().map(|engine| {
            std::sync::Arc::new(SessionGrants::new(
                engine,
                std::sync::Arc::clone(&grants_layout),
            ))
                as std::sync::Arc<dyn tilo_workspace_service::apps::services::WorkspaceGrants>
        })),
        launchable_catalogue: BrokerService::when(launchable_catalogue),
        document_content: BrokerService::when(document_content.map(|content| content.route())),
        // This session's channel keys, when it holds workspaces: a route's keys
        // reach its apps through them, and an endpoint follows their changes
        // here instead of asking again every second.
        group_keys: BrokerService::when(sealed.keys),
        // An offloadable action runs where the workspace's devices would run
        // it sooner.
        action_offload: BrokerService::when(action_offload),
        // An app's action on another of the person's own devices is carried
        // there by the peer network.
        own_device: BrokerService::when(own_device),
        // A node of a workspace's tree, as an app asks about it: a share, its end, or what the
        // person may do there, decided by the workspace's own governance.
        workspace_nodes: BrokerService::when(crate::workspace_nodes::WorkspaceNodes::service(
            view.as_ref(),
        )),
        // A workspace placed on a host has its records changed there: the
        // person's actions on them are sent to the host, which decides and
        // runs them, and the change reaches the windows here by sync.
        hosted_workspaces: BrokerService::when(hosting),
        // Apps write and read their resources in the device's one store, which the daemon keeps:
        // the router sends an app's commands there, and an agent this session admits reads them
        // over the shell's link.
        resources: BrokerService::when(daemon.clone().ok().map(|link| {
            std::sync::Arc::new(crate::broker_services::DaemonResources {
                shell: tilo_session_client::daemon_documents::DaemonShell::new(
                    Ok(link),
                    tilo_rt::app().handle().clone(),
                ),
                writes: daemon_writes,
            })
                as std::sync::Arc<dyn tilo_workspace_service::apps::services::ResourceServices>
        })),
        shell: BrokerService::unserved(
            "the person's shell is answered by this computer's Tilo service, never by a session",
        ),
        // An app's documents are the person's workspaces', which the device's daemon keeps: each
        // app this session launches is joined there too, on a connection vouched for on the shell
        // link, and this session answers only the services it keeps itself.
        device_daemon: BrokerService::served(daemon),
        // The providers this session answers: the steps only it may take for the person — the
        // workspace's activity the Activity app reads, a ticket for a new computer, administering
        // their identities and the devices trusted under them, the machines they planned, their
        // workspace's models, their hosting account, a shortcut's transforms and the web, and the
        // answers their triggers invoke — beside the ones a turn's agent and an agent session
        // take, and the person's settings. Each was registered above under the id a call names.
        providers: BrokerService::served(providers),
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
                    Err(ApiError::Failed("the action's work was given up".into()))
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
fn receipt(done: Result<tilo_actions::ActionOutput, ApiError>) -> Receipt {
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

/// The app wire while the session serves it: what the apps ask of the services,
/// and the broker that answers them on the socket.
pub(crate) struct AppWire {
    pub(crate) link: std::sync::Arc<Link>,
    /// The socket the broker answers on: what the launcher hands an app, and
    /// what a surface app connects for itself.
    pub(crate) path: std::path::PathBuf,
    /// The session the apps' broker decides against, which the loop keeps level
    /// with the session it shows.
    pub(crate) layout: std::sync::Arc<LayoutAuthority>,
    /// Held so the broker's thread ends and its socket is unlinked with the
    /// session.
    pub(crate) broker: tilo_workspace_service::apps::collab_broker::BrokerHandle,
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
    /// The workspaces' trees served to the files daemon that mounts them, until the session ends.
    pub(crate) _files_tree: Option<tilo_workspace_service::files::socket::Serving>,
    /// What a Terminal window's shell in a workspace placed on a host is opened through.
    pub(crate) terminals: Option<crate::workspace_terminals::Terminals>,
    /// What Browser's asks for vault items are answered with, shared with the service that
    /// answers them.
    pub(crate) item_lending: std::sync::Arc<crate::item_lending::ItemLending>,
}

impl AppWire {
    /// What Browser's asks for vault items are answered with: a session that holds a
    /// workspace's Browser says so here, and names who asks its server.
    pub(crate) fn item_lending(&self) -> std::sync::Arc<crate::item_lending::ItemLending> {
        std::sync::Arc::clone(&self.item_lending)
    }

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
                        (&call.workspace, &call.app),
                        &call.channel,
                    )
                })
                .unwrap_or_else(|| self.calls.name(&call.channel))
        });
        let presence = own.as_ref().map(|call| crate::sync::InCall {
            call: ui::CallPresence {
                channel: call.channel.clone(),
                name,
                bridge: None,
            },
            camera: call.camera,
            screen: call.screen,
            muted: call.muted,
            deafened: call.deafened,
        });
        sync.in_call(presence.clone());
        self.calls.follow(sync, presence.as_ref());
    }

    /// The broker the apps' runs are started on, while it serves one.
    pub(crate) fn action_broker(
        &self,
    ) -> Option<std::sync::Arc<tilo_workspace_service::apps::collab_broker::AppActionBroker>> {
        self.broker.app_action_broker().map(std::sync::Arc::new)
    }

    /// What the session tells the workspace's apps of its own session: whose view this person
    /// follows, and the step of a move one window's app is asked for.
    pub(crate) fn follow_dispatcher(
        &self,
    ) -> tilo_workspace_service::apps::collab_broker::SessionFollowDispatcher {
        self.broker.follow_dispatcher()
    }

    /// The broker a program the person admitted as an agent is served
    /// through, while it takes work.
    pub(crate) fn agent_broker(
        &self,
    ) -> Option<std::sync::Arc<tilo_workspace_service::apps::collab_app_broker::AppBrokerState>>
    {
        self.broker.fanout.app_broker()
    }

    /// The hosts of the workspaces this device takes part in but does not
    /// host, while the broker reaches them: where a page of a hosted window
    /// asks the host holding it about a departure it holds for the person.
    pub(crate) fn hosted_workspaces(
        &self,
    ) -> Option<std::sync::Arc<dyn tilo_workspace_service::apps::services::HostedWorkspaces>> {
        self.action_broker()
            .and_then(|broker| broker.hosted_workspaces())
    }
}

/// Whether the shortcut catalogue the services hand an app on this machine names every
/// shortcut of the desktop.
#[cfg(test)]
fn shortcut_catalogue(services: &tilo_workspace_service::apps::services::ProcessServices) -> bool {
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

/// The session's own actions as the broker's steps, for a fixture: a links that is never asked
/// anything.
#[cfg(test)]
fn test_session_actions() -> std::sync::Arc<crate::session_actions::SessionActions> {
    let services =
        crate::mcp_server::SessionServices::new(tilo_service::LoopWake::default().handle(), "Pat");
    std::sync::Arc::new(crate::session_actions::SessionActions::new(
        std::sync::Arc::new(services),
        None,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::fabric::{
        AudioEndpoint, AudioEndpointKind, EndpointSelector, SpeakerFabricState,
    };
    use crate::platform::fabric::{
        DeviceAbout, DeviceId, DeviceKind, FabricCapabilities, PersonDevice, PersonDevices,
        PersonalFabricAction,
    };
    use crate::platform::fabric::{FabricLink, PersonalFabricSendError};

    /// This laptop's devices, with the one output it publishes.
    struct Fabric {
        local: DeviceId,
        converged: PersonDevices,
    }

    impl Fabric {
        fn laptop() -> Self {
            let local = DeviceId::new("this-laptop").expect("a device id");
            let mut device = PersonDevice::new(
                local.clone(),
                DeviceAbout {
                    label: "This laptop".into(),
                    kind: DeviceKind::Laptop,
                    capabilities: FabricCapabilities {
                        audio_output: true,
                        audio_group: true,
                        ..FabricCapabilities::default()
                    },
                },
            );
            device.online = true;
            device.last_seen_unix_ms = 1;
            device.speakers = SpeakerFabricState {
                endpoints: vec![AudioEndpoint {
                    selector: EndpointSelector::mint(&local, "alsa_output.analog-stereo"),
                    kind: AudioEndpointKind::Output,
                    label: "Speakers".into(),
                    is_default: true,
                }],
                ..SpeakerFabricState::default()
            };
            let converged = PersonDevices::new([device], Vec::new());
            Self { local, converged }
        }
    }

    impl FabricLink for Fabric {
        fn devices(&self) -> PersonDevices {
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

    /// The activity's links, which these tests serve nothing on.
    fn requests() -> tilo_session_core::desktop::session::ActivityRequests {
        tilo_session_core::desktop::session::activity_requests(std::sync::Arc::new(
            std::sync::atomic::AtomicBool::new(false),
        ))
    }

    fn services(fabric: Option<crate::platform::fabric::SharedLink>) -> ProcessServices {
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

    /// What a session with no index and no session of its own hands the broker:
    /// the services under test, and the empty session it decides against.
    fn hosted(content: Option<std::sync::Arc<AppContentIndex>>) -> Hosted {
        hosted_with(content, Lists::default())
    }

    /// The same, over the app lists the test reads: what each app declared it
    /// may do on the action links is served out of them.
    fn hosted_with(content: Option<std::sync::Arc<AppContentIndex>>, lists: Lists) -> Hosted {
        Hosted {
            live_activities: std::sync::Arc::new(runtime_collab::LiveActivities::default()),
            content,
            records: None,
            layout: std::sync::Arc::new(LayoutAuthority::new(
                tilo_desktop_model::Layout::default(),
                crate::engine::revocations_of(None),
                Names::default(),
            )),
            shortcuts: None,
            initiative: None,
            activity: crate::activity_provider::wire(requests()),
            agents: None,
            agent_sessions: None,
            native_media: None,
            sealed: Sealed {
                broker: std::sync::Arc::new(Mutex::new(BrokerChannels::default())),
                keys: None,
            },
            own_device: None,
            nearby: None,
            engine: None,
            view: None,
            document_content: None,
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
            item_lending: std::sync::Arc::default(),
            fanout: std::sync::Arc::new(FanoutState::new()),
            endpoint: None,
            device_actions: None,
            service: tilo_workspace_service::service::ServiceSlot::default(),
            daemon: Err("a test's session has no daemon".to_owned()),
            daemon_writes: tilo_session_client::daemon_documents::DaemonWritesFeed::start(
                &tilo_session_client::daemon_documents::DaemonShell::new(
                    Err("a test's session has no daemon".to_owned()),
                    tilo_rt::app().handle().clone(),
                ),
            ),
        }
    }

    tilo_test_env::scenario! {
        fn the_app_wire_serves_an_app_what_this_session_keeps_for_it;

        phase an_apps_search_publication_lands_in_the_index_search_reads;
        // An app publishes what it holds through the broker, and the index the
        // session handed it keeps it: it is the same index search reads.
        {
            use ui::search::index_publication::{IndexDelta, IndexEntry};

            let content = std::sync::Arc::new(AppContentIndex::in_memory());
            let services = host_services(
                Link::for_test(),
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

        phase an_app_keeps_and_asks_for_content_through_the_services_of_this_session;
        // An app's `offerBlob`, `wantBlob` and `subscribeBlobs`, and the content
        // put links, are served by the app wire this session keeps: the same store
        // and the same asks of the workspace's other devices. A session that
        // keeps no content hands the broker none of it.
        {
            let kept = crate::content::ContentService::open(None);
            let wake: Wake = std::sync::Arc::new(|| ());
            let with = host_services(
                Link::new(wake),
                std::sync::Arc::new(Launches::default()),
                None,
                Hosted {
                    document_content: Some(kept),
                    ..hosted(None)
                },
                None,
                None,
            );
            assert!(
                with.document_content.is_some(),
                "the broker asks the workspace through this session's app wire"
            );
            assert!(
                with.content_store.is_some(),
                "and uploads into the store this device keeps"
            );

            let without = services(None);
            assert!(
                without.document_content.is_none() && without.content_store.is_none(),
                "a session that keeps content hands the broker no content app wire"
            );
        }

        phase the_session_answers_settings_speaker_route_from_its_fabric;
        // Settings reaches the speaker groups of the fabric this session runs
        // on, and of nothing else: a session without one has no speaker route.
        {
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

        phase an_apps_settings_are_the_document_this_session_keeps;
        // An app's settings are this session's own document: the call the
        // broker answers through its settings server reads and writes what this
        // device keeps, scoped to that app, and serves the person's dictionary
        // from the same document.
        {
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
            let written: serde_json::Value = serde_json::from_slice(&written).expect("a settings view");
            assert_eq!(
                written["app_personal"],
                serde_json::json!({"so.tilo.notes": {"font": "serif"}})
            );
            assert_eq!(
                serde_json::Value::Object(records.load()),
                serde_json::json!({"app_personal": {"so.tilo.notes": {"font": "serif"}}}),
                "the write lands in this device's settings document, and the view's dictionary is \
             not kept there"
            );
            // The view carries the person's dictionary as the same document holds it.
            let projected = |view: &serde_json::Value| {
                view["app_device_local"]["so.tilo.notes"]
                    [settings_store::personal_dictionary::delivery::VIEW_KEY]
                    .clone()
            };
            assert_eq!(
                projected(&written),
                serde_json::to_value(settings_store::personal_dictionary::snapshot_of(
                    &records.load()
                ))
                .expect("the dictionary's snapshot")
            );
            let word =
                settings_store::personal_dictionary::Word::new("Tilo", None).expect("a word to add");
            settings_store::personal_dictionary::edit_in(
                records.as_ref(),
                0,
                settings_store::personal_dictionary::Edit::Add(word),
            )
            .expect("the person adds a word");
            let (_, read) = local
                .block_on(&runtime, notes.read(0))
                .expect("the app reads its setting back");
            let read: serde_json::Value = serde_json::from_slice(&read).expect("a settings view");
            assert_eq!(read["app_personal"], written["app_personal"]);
            assert_eq!(
                projected(&read)["words"],
                serde_json::json!([{"spelling": "Tilo", "language": null}]),
                "a word the person adds reaches the app's view"
            );
        }

        phase an_apps_declared_access_is_what_this_device_read;
        // What an app declared it may do on the action links is served out of the
        // lists this device read: its own declaration, and nothing for an app
        // that declared nothing.
        {
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
                Link::for_test(),
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

        phase a_resource_no_service_publishes_is_told_once;
        // An app watching a resource no service of this device publishes hears it
        // once, and again only when a service is added: a machine without a
        // battery does not tell it on every reading of another service.
        {
            let link = Link::new(std::sync::Arc::new(|| {}));
            let (sink, mut heard) = mpsc::unbounded_channel();
            let caller = Caller {
                app_id: "so.tilo.quick-settings".to_owned(),
                workspace: String::new(),
            };
            let _watch = link.watch(&caller, "power:battery", sink);
            let mut services = Services::default();
            link.publish(&services);
            assert!(
                matches!(heard.try_recv(), Ok(Event::Reading { reading: None, .. })),
                "the app is told no service publishes it"
            );

            // Another service's reading moves: nothing is said of the battery
            // again, however many revisions go by.
            let counted = std::sync::Arc::new(std::sync::Mutex::new(1_u8));
            services.add(Box::new(Counted(std::sync::Arc::clone(&counted))));
            link.publish(&services);
            // The catalogue moved with the service added, so it is asked again
            // once, and that is all.
            assert!(
                matches!(heard.try_recv(), Ok(Event::Reading { reading: None, .. })),
                "a service was added, so it is asked again once"
            );
            *counted.lock().unwrap() = 2;
            services
                .invoke("test:counted", "bump", &Map::new())
                .expect("the service is published")
                .expect("the bump is taken");
            link.publish(&services);
            assert!(
                heard.try_recv().is_err(),
                "a reading of another service says nothing of the battery"
            );
        }
    }

    /// What each shipped app's sources call of the action links: the service each
    /// call reaches, and the apps whose sources make it.
    fn boundaries_shipped_apps_call()
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
            for &(marker, service) in crate::broker_services::CALLED_SERVICES {
                if text.contains(marker) {
                    called.entry(service).or_default().insert(app.clone());
                }
            }
        }
        called
    }

    /// Every service a shipped app's sources call is served, or named in
    /// [`crate::broker_services::UNSERVED_BUT_CALLED`] as one this session still owes
    /// the app; that list shrinks as each call is served or moved off the
    /// service, and a call it does not name fails here.
    fn shipped_apps_calls_are_served(decisions: &[(&'static str, Option<&'static str>)]) {
        let served = |name: &str| {
            decisions
                .iter()
                .find(|(service, _)| *service == name)
                .map_or_else(|| panic!("`{name}` is decided"), |(_, why)| why.is_none())
        };
        let called = boundaries_shipped_apps_call();
        assert!(
            called.len() >= 4,
            "the shipped apps' sources are read and their calls found: {called:?}"
        );
        for (service, apps) in &called {
            let reason = decisions
                .iter()
                .find(|(name, _)| name == service)
                .map_or_else(
                    || panic!("`{service}` is not a service of the broker"),
                    |(_, why)| *why,
                );
            if reason.is_none() {
                assert!(
                    !crate::broker_services::UNSERVED_BUT_CALLED.contains(service),
                    "`{service}` is served now: take it off the list of services still to serve"
                );
                continue;
            }
            assert!(
                crate::broker_services::UNSERVED_BUT_CALLED.contains(service),
                "{apps:?} call `{service}`, and this session serves nothing of it: {reason:?}"
            );
        }
        for service in crate::broker_services::UNSERVED_BUT_CALLED {
            assert!(
                called.contains_key(service),
                "no shipped app calls `{service}` any more: take it off the list of services still to serve"
            );
            assert!(
                !served(service),
                "`{service}` is served now: take it off the list of services still to serve"
            );
        }
    }

    /// Every service the broker offers is decided here: served, served where
    /// this run has what it carries, or not served with the reason it was
    /// left out. Every service a shipped app's sources call is served, or named
    /// as one still to serve — so an app that starts calling a service this
    /// session serves nothing of fails this test.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one walk through every boundary this session decides"
    )]
    fn every_boundary_is_decided_and_no_shipped_app_asks_for_what_is_not_served() {
        let decided = broker_services(
            Link::for_test(),
            std::sync::Arc::new(Launches::default()),
            None,
            hosted(None),
            None,
            None,
            None,
        );
        let decisions = decided.decisions();
        // The broker names its services, so a service is decided here and never
        // dropped from the list: the field's conversion above would not
        // notice a name that went missing.
        assert_eq!(
            decisions.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            tilo_workspace_service::apps::services::ProcessServices::NAMES,
            "the broker's services, each decided in its order"
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
                .find(|(service, _)| *service == name)
                .map_or_else(|| panic!("`{name}` is decided"), |(_, why)| why.is_none())
        };
        for name in crate::broker_services::EVERY_APP {
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
        let registered = services
            .providers
            .as_ref()
            .expect("this session registers its own providers")
            .all();
        let providers: Vec<&str> = registered.iter().map(|entry| entry.provider).collect();
        assert_eq!(
            providers.len(),
            10,
            "the providers this session answers are the workspace's activity, a ticket for a new \
             computer, identity administration, the person's hosting account, the machines they \
             planned, its own actions, its agents' transforms, the web its shortcuts reach, the \
             answers their triggers invoke and the workspace's models"
        );
        for (provider, why) in [
            (
                tilo_actions::remote::device_tickets::PROVIDER,
                "Settings adds a device with a ticket",
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
                tilo_actions::remote::server_account::PROVIDER,
                "Settings' Billing reads the person's account with their host",
            ),
            (
                tilo_actions::remote::machines::PROVIDER,
                "the installer's window and the command plan a machine",
            ),
            (
                tilo_workspace_service::agents::assistant::transforms::PROVIDER,
                "a shortcut's AI step and an app's ask are transformed by this device's agents",
            ),
            (
                tilo_workspace_service::agents::WEB_SEARCH_PROVIDER,
                "a shortcut's step and an app search and read the web",
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

    use tilo_actions::ResourceSpec;

    /// A service publishing one reading the test moves.
    struct Counted(std::sync::Arc<std::sync::Mutex<u8>>);

    impl tilo_service::Service for Counted {
        fn resources(&self) -> Vec<ResourceSpec> {
            vec![
                ResourceSpec::new("test:counted", "test", "Counted").with_action(
                    tilo_actions::ActionSpec::typed_operation(
                        "bump",
                        "",
                        "",
                        tilo_actions::OperationKind::Effect,
                        "test.bump",
                        Vec::new(),
                        |_| {
                            tilo_actions::EncodedAction::encode_operation("test.bump", &())
                                .unwrap_or_else(|error| tilo_actions::EncodedAction {
                                    type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE
                                        .to_owned(),
                                    payload: error.to_string().into_bytes(),
                                })
                        },
                    ),
                ),
            ]
        }

        fn read(&self, resource: &str) -> Option<serde_json::Value> {
            (resource == "test:counted")
                .then(|| serde_json::json!({ "value": *self.0.lock().unwrap() }))
        }

        fn invoke(
            &mut self,
            _resource: &str,
            _action: &str,
            _: &Map<String, serde_json::Value>,
        ) -> Result<Started, ApiError> {
            Ok(Started::Done(tilo_actions::ActionOutput::default()))
        }
    }

    /// The broker this session starts answers the shortcut library a launched app asks for: the
    /// library in its services writes a shortcut through this device's own workspace service, and
    /// the workspace lists it.
    #[tokio::test]
    async fn the_services_an_app_is_given_answer_the_shortcut_library() {
        use tilo_workspace_service::apps::services::ShortcutLibraryRequest;
        use tilo_workspace_service::workspaces::fixtures;

        let directory = tempfile::tempdir().expect("a directory for the service");
        let (key, cert) = fixtures::governor();
        let host = fixtures::test_host(directory.path(), &key, &cert).await;
        let workspaces = host.workspaces().clone();
        let workspace = workspaces
            .create("Home")
            .await
            .expect("a workspace")
            .workspace;
        let library = tilo_runtime::device::shortcuts::Shortcuts::new(
            workspaces.clone(),
            cert.clone(),
            std::sync::Arc::new(key.clone()),
        )
        .expect("a runtime for shortcut runs");
        let decided = broker_services(
            Link::for_test(),
            std::sync::Arc::new(Launches::default()),
            None,
            Hosted {
                shortcuts: Some(std::sync::Arc::new(library)),
                initiative: None,
                ..hosted(None)
            },
            None,
            None,
            None,
        );
        assert!(
            decided
                .decisions()
                .iter()
                .any(|(service, why)| *service == "shortcut_library" && why.is_none()),
            "the broker this session starts serves the shortcut library"
        );
        let library = decided
            .into_host_services()
            .shortcut_library
            .expect("the broker holds the library it serves");
        let saved = library
            .store(ShortcutLibraryRequest {
                workspace: workspace.clone(),
                actor: tilo_action_policy::MemberId::person(hex::encode(cert.user_public_key)),
                acting_for: None,
                change: serde_json::from_value(serde_json::json!({
                    "save": { "id": "tidy", "name": "Tidy", "steps": [], "triggers": [] },
                }))
                .expect("a shortcut change"),
                context: tilo_actions::ActionContext::default(),
                invocation: tilo_actions::InvocationId::new(),
            })
            .await;
        assert_eq!(saved, Ok(()));
        let described = workspaces.described().await;
        let listed = described
            .iter()
            .find(|described| described.id().to_string() == workspace)
            .expect("the workspace the shortcut was saved in");
        assert_eq!(
            listed.shortcuts.len(),
            1,
            "the workspace lists the shortcut the app saved"
        );
        host.shutdown().await;
    }
}
