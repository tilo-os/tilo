//! The session's side of the app wire: the resources its services publish,
//! read, watched and invoked by the apps it runs.
//!
//! An app is a process of its own, and the socket it speaks to its session on
//! is the launcher's: [`tilo_action_broker`] serves it, admits the peer by
//! the handoff the launcher stamped on the connection, and keeps the rooms
//! the apps share. What this session adds to that broker is one provider:
//! what its services publish ([`Link`], as the broker's
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
use tilo_action_broker::collab_broker::{BrokerChannels, BrokerHandle, FanoutState};
use tilo_action_broker::collab_outbound::PeerPublisher;
use tilo_action_broker::services::TrackerRuns;
use tilo_action_broker::services::{
    Caller, HostServices, HostStepServices, PublishedResources, Reading, ResourceWatch,
    SearchIndexServices, ShortcutLibraryRequest, ShortcutLibraryServices, ShortcutStore,
    SpeakerChange, SpeakerServices, WorkspaceAuthority, WorkspaceAuthorityHandle,
};
use tilo_desktop_search::content_index::AppContentIndex;
use tilo_lock::MutexRecover as _;
use tilo_service::{Services, Started, Wake};
use tokio::sync::{mpsc, oneshot};

use crate::activity_provider::ActivityWire;
use crate::launches::Launches;

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

/// The seam between the session's loop and the broker's connections: what an
/// app asks of the services, and the readings the apps watch.
pub struct Link {
    asks: Mutex<Vec<Ask>>,
    watches: Mutex<Vec<Watch>>,
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

/// What this session hands the broker besides the link and the launcher: the
/// board every app's live activity shows on, the index its publications land
/// in, the desk it decides a room against, the shortcuts it saves and runs,
/// the workspace's activity it serves, and the runs of what people ask its
/// agents.
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
}

/// Serve the session's published resources to the apps on `socket`: the
/// broker admits each connection by the launch `launches` recorded, asks
/// `link` for what the services publish, hands Settings the speaker groups
/// of `fabric` when this session runs on one, and decides what an app may do
/// in a room against `desk`. What apps publish of their own content lands in
/// `content`, the index search reads.
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
    let services = host_services(link, launches, fabric, hosted);
    tilo_action_broker::collab_broker::spawn_with_services(
        socket,
        Some(membership),
        PeerPublisher::disabled(),
        None,
        services,
        std::sync::Arc::new(Mutex::new(BrokerChannels::default())),
        std::sync::Arc::new(FanoutState::new()),
    )
}

/// A saved shortcut change on its way to the desk, answered once the session's
/// loop applied it: the desk belongs to the loop, and the broker asks from its
/// own thread.
pub struct StoreRequest {
    pub room: String,
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
                    room: request.room,
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
/// hands it out on this thread only), while the broker asks about a room from
/// its own thread. The loop publishes a copy when the desk it shows moved, and
/// the broker reads the copy.
pub struct DeskAuthority {
    tree: Mutex<tilo_desktop_model::DesktopTree>,
    revocations: crate::engine::Revocations,
}

impl DeskAuthority {
    /// The authority over `tree`, as the loop sees it now, deciding what each
    /// of its rooms revokes from `revocations`.
    #[must_use]
    pub fn new(
        tree: tilo_desktop_model::DesktopTree,
        revocations: crate::engine::Revocations,
    ) -> Self {
        Self {
            tree: Mutex::new(tree),
            revocations,
        }
    }

    /// The desk as the loop shows it now, for the broker's next decision.
    pub fn publish(&self, tree: tilo_desktop_model::DesktopTree) {
        *self.tree.lock_recover() = tree;
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
    /// shortcuts are proven against the revocations its room records. A room
    /// whose document this device does not hold — a desk kept from before it
    /// joined, or one it left — revokes nothing here: the shortcuts it shows
    /// are proven by what it does hold, and a call it can no longer make is
    /// refused elsewhere.
    fn room_revocations(&self, room: &str) -> Option<tilo_user_identity::RevocationList> {
        let tree = self.tree.lock_recover();
        tilo_action_broker::services::room_in(&tree, room)?;
        Some((self.revocations)(room).unwrap_or_default())
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
) -> HostServices {
    let Hosted {
        live_activities,
        native_media,
        content,
        desk: _,
        shortcuts,
        activity,
        agent_runs,
    } = hosted;
    let speakers: Option<std::sync::Arc<dyn SpeakerServices>> = fabric.map(|fabric| {
        std::sync::Arc::new(FabricSpeakers(fabric)) as std::sync::Arc<dyn SpeakerServices>
    });
    let search_index: Option<std::sync::Arc<dyn SearchIndexServices>> = content.map(|content| {
        std::sync::Arc::new(ContentIndex(content)) as std::sync::Arc<dyn SearchIndexServices>
    });
    let shortcut_library: std::sync::Arc<dyn ShortcutLibraryServices> = shortcuts;
    HostServices {
        published: Some(link),
        launcher: Some(launches),
        speakers,
        // Every app shows its live activity (a running timer, a stopwatch)
        // outside its windows on this one board.
        live_activities: Some(live_activities),
        // What an app publishes of its own content is found whether or not
        // it runs, once it is closed and after a restart.
        search_index,
        // A shortcut a person saves lands on the desk, and a shortcut's run
        // executes on the session's runtime.
        shortcut_library: Some(shortcut_library),
        // The workspace's activity: what the Activity app shows, served by
        // the desk's own model.
        host_steps: vec![activity as std::sync::Arc<dyn HostStepServices>],
        // What people ask this session's agents to do: a run is the broker's
        // to deliver, and its reports come back to the run.
        tracker_runs: agent_runs.map(|runs| runs as std::sync::Arc<dyn TrackerRuns>),
        // A call an app joins is this session's: the channel it is in, who
        // else is there, and what the app hears of it until it leaves.
        native_media: native_media.map(|media| {
            std::sync::Arc::new(media)
                as std::sync::Arc<dyn tilo_action_broker::services::NativeMediaServices>
        }),
        ..HostServices::default()
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
    /// The calls of the rooms this session takes part in, as the loop
    /// heard them: what a voice channel shows.
    pub(crate) calls: std::sync::Arc<crate::calls::Calls>,
    /// The service apps join a call through, when this session carries
    /// calls at all.
    pub(crate) calls_service: Option<tilo_native_media::NativeMediaService>,
}

impl Plane {
    /// Say this device's call where everyone in the room reads it, and take
    /// in what they say of theirs: the channel a person is in and who is in
    /// each channel's call, which is what this session's apps show of calls.
    pub(crate) fn follow_calls(&self, sync: &crate::sync::Client) {
        let own = self
            .calls_service
            .as_ref()
            .and_then(tilo_native_media::NativeMediaService::own_call);
        let name = own.as_ref().map_or_else(String::new, |call| {
            // What the app calls the channel, so the room reads a name and
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
        let presence = own.as_ref().map(|call| ui::CallPresence {
            channel: call.channel.clone(),
            name,
        });
        sync.in_call(presence);
        self.calls
            .follow(sync, own.as_ref().map(|call| call.channel.as_str()));
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
        )
    }

    /// What a session with no index and no desk of its own hands the broker:
    /// the services under test, and the empty desk it decides against.
    fn hosted(content: Option<std::sync::Arc<AppContentIndex>>) -> Hosted {
        Hosted {
            live_activities: std::sync::Arc::new(runtime_collab::LiveActivities::default()),
            content,
            desk: std::sync::Arc::new(DeskAuthority::new(
                tilo_desktop_model::DesktopTree::default(),
                crate::engine::revocations_of(None),
            )),
            shortcuts: library(),
            activity: crate::activity_provider::wire(requests()),
            agent_runs: None,
            native_media: None,
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
        );
        let search_index = services
            .search_index
            .expect("a session with an index offers apps its search index");
        let room = tilo_desktop_model::RoomId::new();
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
                room,
                "so.tilo.notes",
                "tab:0001",
                &delta.encode().expect("a delta"),
            )
            .expect("the index takes the publication");
        assert_eq!(
            content.title(&room.to_string(), "n1").as_deref(),
            Some("Groceries"),
            "what the app published is in the index search reads"
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
}
