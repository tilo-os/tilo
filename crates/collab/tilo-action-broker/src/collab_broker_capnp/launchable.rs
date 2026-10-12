//! Every application keeps offering its actions while it is closed.
//!
//! No action runs out of sight: invoking one makes the app's view live and
//! runs it there, exactly as a person's click does. So the broker retains
//! the last tree each app published per workspace and record, on disk, and
//! a caller can still find and invoke any of its actions after the app
//! closes or the shell restarts. The retained rows are catalogue state only:
//! invoking one launches the app and admits the invocation against the
//! surface the app publishes fresh, never against the retained copy.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use super::{PublishedAction, PublishedSurface};
use crate::collab_broker::CrdtRoute;
use crate::services::LaunchPlace;

/// Prefix of the provider token naming a retained launchable surface.
const LAUNCHABLE_PROVIDER: &str = "launch";

/// How long an invocation waits for the application it launched to publish
/// the action and its invocation stream.
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(30);

/// Threads the delivery runtime may use to call the host's launcher, which
/// blocks while it starts the app. Further launches queue behind them.
const LAUNCH_CALLS: usize = 4;

/// The runtime every launch-then-deliver waits on: one worker for all of
/// them, since a wait is an event, never a thread, plus a bounded pool for
/// the blocking launcher calls. It runs beside the broker, never on its
/// local set, and starts on first use.
///
/// # Errors
///
/// Why the runtime could not start; remembered, so every launch reports it.
pub(super) fn delivery_runtime() -> Result<&'static tokio::runtime::Runtime, String> {
    static RUNTIME: std::sync::OnceLock<Result<tokio::runtime::Runtime, String>> =
        std::sync::OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tilo_rt::daemon("tilo-launch")
                .workers(1)
                .max_blocking(LAUNCH_CALLS)
                .build()
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Keep overflow launches in the async task, where cancellation and the
/// delivery deadline remain observable, rather than in the blocking queue.
/// The host call owns its permit until it actually returns.
pub(super) fn launch_permits() -> Arc<tokio::sync::Semaphore> {
    static PERMITS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    Arc::clone(PERMITS.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(LAUNCH_CALLS))))
}

/// `(app_id, cluster_id, record_id)`: one retained surface.
type SurfaceKey = (String, String, String);

/// Starts `app_id` in a workspace in place of the host's launcher. Tests
/// stand in for launching the installed app with one.
pub(super) type AppLauncher = Arc<
    dyn Fn(&str, &tilo_desktop_model::Workspace, Option<&str>, &LaunchPlace) -> Result<(), String>
        + Send
        + Sync,
>;

/// How many actors' views [`ActorViews`] remembers.
const REMEMBERED_ACTORS: usize = 1024;

/// The view each actor last worked in, per workspace: the app whose action
/// it last ran there. A view another of its actions needs opens beside it.
#[derive(Default)]
struct ActorViews {
    /// `(room, actor)` → (app, when it was noted).
    views: HashMap<(String, String), (String, u64)>,
    clock: u64,
}

impl ActorViews {
    fn note(&mut self, room: &str, actor: &str, app_id: &str) {
        self.clock = self.clock.wrapping_add(1);
        let key = (room.to_owned(), actor.to_owned());
        if !self.views.contains_key(&key) && self.views.len() >= REMEMBERED_ACTORS {
            let oldest = self
                .views
                .iter()
                .min_by_key(|(_, (_, at))| *at)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                self.views.remove(&oldest);
            }
        }
        self.views.insert(key, (app_id.to_owned(), self.clock));
    }

    fn of(&self, room: &str, actor: &str) -> Option<&str> {
        self.views
            .get(&(room.to_owned(), actor.to_owned()))
            .map(|(app, _)| app.as_str())
    }
}

/// The provider token a catalogue issues for a retained surface's actions.
pub fn launchable_provider(surface: &PublishedSurface) -> String {
    format!(
        "{LAUNCHABLE_PROVIDER}:{}:{}:{}",
        surface.route.cluster_id, surface.app_id, surface.route.record_id
    )
}

/// Whether `provider` names a retained launchable surface.
pub(super) fn is_launchable_provider(provider: &str) -> bool {
    provider
        .strip_prefix(LAUNCHABLE_PROVIDER)
        .is_some_and(|rest| rest.starts_with(':'))
}

/// One retained surface as saved on disk. Connection identity is not
/// saved: it means nothing to the next connection.
#[derive(serde::Serialize, serde::Deserialize)]
struct StoredSurface {
    app_id: String,
    cluster_id: String,
    record_id: String,
    actions: Vec<PublishedAction>,
}

impl StoredSurface {
    fn into_surface(self) -> Option<(SurfaceKey, PublishedSurface)> {
        let actions = launchable_actions(&self.actions);
        if actions.is_empty() || self.app_id.is_empty() || self.cluster_id.is_empty() {
            return None;
        }
        let key = (
            self.app_id.clone(),
            self.cluster_id.clone(),
            self.record_id.clone(),
        );
        Some((
            key,
            PublishedSurface {
                peer_id: 0,
                app_id: self.app_id.clone(),
                route: CrdtRoute {
                    app_id: self.app_id,
                    cluster_id: self.cluster_id,
                    record_id: self.record_id,
                },
                actions,
                live: false,
            },
        ))
    }
}

fn launchable_actions(actions: &[PublishedAction]) -> Vec<PublishedAction> {
    actions
        .iter()
        .filter(|action| action.contract.validate_declaration().is_empty())
        .cloned()
        .collect()
}

/// Saves the catalogue off the broker's thread. Saves coalesce: a burst of
/// changes writes the latest catalogue once.
struct CatalogueStore {
    saves: Option<std::sync::mpsc::Sender<Vec<u8>>>,
    writer: Option<JoinHandle<()>>,
}

impl CatalogueStore {
    fn open(path: PathBuf) -> std::io::Result<Self> {
        let (saves, pending) = std::sync::mpsc::channel::<Vec<u8>>();
        let writer = std::thread::Builder::new()
            .name("tilo-launchable-actions".into())
            .spawn(move || {
                while let Ok(mut bytes) = pending.recv() {
                    while let Ok(newer) = pending.try_recv() {
                        bytes = newer;
                    }
                    if let Err(error) = tilo_files::native::atomic::write_private(&path, &bytes) {
                        tracing::warn!(
                            %error,
                            path = %path.display(),
                            "could not save the launchable action catalogue"
                        );
                    }
                }
            })?;
        Ok(Self {
            saves: Some(saves),
            writer: Some(writer),
        })
    }

    fn save(&self, bytes: Vec<u8>) {
        if let Some(saves) = &self.saves {
            let _ = saves.send(bytes);
        }
    }
}

impl Drop for CatalogueStore {
    fn drop(&mut self) {
        // Closing the channel lets the writer finish the last save and exit.
        self.saves.take();
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}

/// The retained launchable surfaces, and how long an invocation of one waits
/// for the application it launched. The host's launcher starts the app.
pub(super) struct LaunchableCatalogue {
    surfaces: Mutex<HashMap<SurfaceKey, PublishedSurface>>,
    store: Mutex<Option<CatalogueStore>>,
    launcher: Mutex<Option<AppLauncher>>,
    delivery_timeout: Mutex<Duration>,
    views: Mutex<ActorViews>,
}

impl LaunchableCatalogue {
    pub(super) fn new() -> Self {
        Self {
            surfaces: Mutex::new(HashMap::new()),
            store: Mutex::new(None),
            launcher: Mutex::new(None),
            delivery_timeout: Mutex::new(DELIVERY_TIMEOUT),
            views: Mutex::new(ActorViews::default()),
        }
    }

    /// `actor` just ran an action of `app_id` in `room`: that view is
    /// where it works now.
    pub(super) fn note_view(&self, room: &str, actor: &str, app_id: &str) {
        self.views
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .note(room, actor, app_id);
    }

    /// The app whose view `actor` last worked in, in `room`.
    pub(super) fn view_of(&self, room: &str, actor: &str) -> Option<String> {
        self.views
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .of(room, actor)
            .map(str::to_owned)
    }

    /// Load the catalogue saved at `path` and keep saving changes there. A
    /// missing or unreadable file is an empty catalogue.
    pub(super) fn persist_at(&self, path: &Path) {
        let stored: Vec<StoredSurface> = tilo_files::native::atomic::json::load_or_default(path);
        let store = match CatalogueStore::open(path.to_path_buf()) {
            Ok(store) => store,
            Err(error) => {
                tracing::warn!(%error, "launchable actions will not survive a restart");
                return;
            }
        };
        let Ok(mut surfaces) = self.surfaces.lock() else {
            return;
        };
        for (key, surface) in stored.into_iter().filter_map(StoredSurface::into_surface) {
            // What an application published this session is newer.
            surfaces.entry(key).or_insert(surface);
        }
        if let Ok(mut slot) = self.store.lock() {
            *slot = Some(store);
        }
        self.save(&surfaces);
    }

    /// Every retained surface.
    pub(super) fn surfaces(&self) -> Vec<PublishedSurface> {
        self.surfaces
            .lock()
            .map(|surfaces| surfaces.values().cloned().collect())
            .unwrap_or_default()
    }

    pub(super) fn action_for(
        &self,
        route: &CrdtRoute,
        resource: &str,
        action: &str,
    ) -> Option<(PublishedSurface, PublishedAction)> {
        self.surfaces
            .lock()
            .ok()?
            .get(&(
                route.app_id.clone(),
                route.cluster_id.clone(),
                route.record_id.clone(),
            ))?
            .action_projection(resource, action)
    }

    /// The retained surface `provider` names.
    pub(super) fn surface_for(&self, provider: &str) -> Option<PublishedSurface> {
        self.surfaces.lock().ok().and_then(|surfaces| {
            surfaces
                .values()
                .find(|surface| launchable_provider(surface) == provider)
                .cloned()
        })
    }

    /// Retain the surface a live application just published, replacing what
    /// that app, workspace and record offered before. A surface offering
    /// nothing forgets the old one.
    pub(super) fn retain(&self, published: &PublishedSurface) {
        let key = (
            published.app_id.clone(),
            published.route.cluster_id.clone(),
            published.route.record_id.clone(),
        );
        let actions = launchable_actions(&published.actions);
        let Ok(mut surfaces) = self.surfaces.lock() else {
            return;
        };
        let changed = if actions.is_empty() {
            surfaces.remove(&key).is_some()
        } else if surfaces.get(&key).is_some_and(|retained| {
            retained.actions == actions && retained.route == published.route
        }) {
            false
        } else {
            surfaces.insert(
                key,
                PublishedSurface {
                    peer_id: 0,
                    app_id: published.app_id.clone(),
                    route: published.route.clone(),
                    actions,
                    live: false,
                },
            );
            true
        };
        if changed {
            self.save(&surfaces);
        }
    }

    fn save(&self, surfaces: &HashMap<SurfaceKey, PublishedSurface>) {
        let Ok(store) = self.store.lock() else {
            return;
        };
        let Some(store) = store.as_ref() else {
            return;
        };
        let mut stored = surfaces
            .values()
            .map(|surface| StoredSurface {
                app_id: surface.app_id.clone(),
                cluster_id: surface.route.cluster_id.clone(),
                record_id: surface.route.record_id.clone(),
                actions: surface.actions.clone(),
            })
            .collect::<Vec<_>>();
        stored.sort_by(|a, b| {
            (&a.app_id, &a.cluster_id, &a.record_id).cmp(&(&b.app_id, &b.cluster_id, &b.record_id))
        });
        match serde_json::to_vec(&stored) {
            Ok(bytes) => store.save(bytes),
            Err(error) => {
                tracing::warn!(%error, "could not encode the launchable action catalogue");
            }
        }
    }

    /// The launcher standing in for the host's, if one was installed.
    pub(super) fn launcher(&self) -> Option<AppLauncher> {
        self.launcher
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(super) fn delivery_timeout(&self) -> Duration {
        self.delivery_timeout
            .lock()
            .map_or(DELIVERY_TIMEOUT, |timeout| *timeout)
    }

    #[cfg(test)]
    pub(super) fn replace_launcher_for_test(&self, launcher: AppLauncher, timeout: Duration) {
        if let Ok(mut slot) = self.launcher.lock() {
            *slot = Some(launcher);
        }
        if let Ok(mut slot) = self.delivery_timeout.lock() {
            *slot = timeout;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Weak;

    use tilo_actions::remote::{
        ActionHostError, Caller, CatalogueQuery, Confirmation, InvokeArgument, InvokeRequest,
        RemoteActionTarget,
    };

    use super::super::{
        BrokerChannels, CapnpBrokerState, FanoutState, InvocationEnvelope, PeerPublisher,
        PublishedInput,
    };
    use super::*;
    use crate::test_support::{Workspace, WorkspaceId, WorkspaceTree};

    const APP: &str = "so.tilo.files";

    fn action(name: &str, confirmation: tilo_actions::ConfirmationPolicy) -> PublishedAction {
        PublishedAction {
            semantic_identity: None,
            resource: "files.app".into(),
            action: name.into(),
            resource_kind: "files.app".into(),
            resource_label: "Files".into(),
            action_label: name.into(),
            description: None,
            operation_kind: tilo_actions::OperationKind::Effect,
            params: Vec::new(),
            contract: tilo_actions::ActionContract {
                risk: if confirmation == tilo_actions::ConfirmationPolicy::None {
                    tilo_actions::RiskLevel::Low
                } else {
                    tilo_actions::RiskLevel::Destructive
                },
                confirmation,
                ..tilo_actions::ActionContract::default()
            },
            view: None,
            input: PublishedInput::None,
        }
    }

    fn surface(peer_id: u64, room: &str) -> PublishedSurface {
        PublishedSurface {
            peer_id,
            app_id: APP.into(),
            route: CrdtRoute::app(APP, room),
            actions: vec![
                action("new-window", tilo_actions::ConfirmationPolicy::None),
                action("empty-trash", tilo_actions::ConfirmationPolicy::Confirm),
            ],
            live: true,
        }
    }

    fn workspace() -> Workspace {
        let mut workspace = Workspace::new(WorkspaceId(51), "Launch room");
        workspace.set_owner("owner");
        workspace
    }

    /// A broker session hosting `workspace`, and its room.
    fn broker(workspace: &Workspace) -> (Arc<CapnpBrokerState>, String) {
        let (state, room, _) = broker_with_fanout(workspace);
        (state, room)
    }

    /// [`broker`], with the fanout local subscribers hear the room on.
    fn broker_with_fanout(
        workspace: &Workspace,
    ) -> (Arc<CapnpBrokerState>, String, Arc<FanoutState>) {
        let room = workspace.room.to_string();
        let mut tree = WorkspaceTree::default();
        tree.desktop.workspaces.push(workspace.clone());
        let tree = tree.shared();
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge) = CapnpBrokerState::new_configured(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(tree.authority()),
            None,
            None,
            tree.services(),
        );
        (state, room, fanout)
    }

    type Streams =
        std::sync::mpsc::Receiver<tokio::sync::mpsc::UnboundedReceiver<InvocationEnvelope>>;

    /// Stand in for launching the app: it connects as `peer_id`, publishes
    /// its surface and opens its invocation stream, which the returned
    /// receiver hands over. Each launch is recorded.
    fn app_comes_up_as(
        state: &Arc<CapnpBrokerState>,
        peer_id: u64,
    ) -> (Streams, Arc<Mutex<Vec<String>>>) {
        let (streams, launches, _) = app_comes_up_placed(state, peer_id);
        (streams, launches)
    }

    /// The record each launch opened, and where it was placed.
    type Places = Arc<Mutex<Vec<(Option<String>, LaunchPlace)>>>;

    /// [`app_comes_up_as`], also recording which record each launch opened
    /// and where it was placed. The app comes up on the record it was asked
    /// for, else its room record.
    fn app_comes_up_placed(
        state: &Arc<CapnpBrokerState>,
        peer_id: u64,
    ) -> (Streams, Arc<Mutex<Vec<String>>>, Places) {
        let (streams, stream) = std::sync::mpsc::channel();
        let launches = Arc::new(Mutex::new(Vec::new()));
        let places = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&launches);
        let noted = Arc::clone(&places);
        let weak: Weak<CapnpBrokerState> = Arc::downgrade(state);
        state.launchable.replace_launcher_for_test(
            Arc::new(move |app_id, workspace, record, place| {
                recorded.lock().expect("launches").push(app_id.to_string());
                noted
                    .lock()
                    .expect("places")
                    .push((record.map(str::to_owned), place.clone()));
                let state = weak.upgrade().ok_or("the broker is gone")?;
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                state
                    .invocation_subscribers
                    .lock()
                    .expect("subscribers")
                    .insert(peer_id, tx);
                let mut up = surface(peer_id, &workspace.room.to_string());
                if let Some(record) = record {
                    up.route.record_id = record.to_owned();
                }
                state.surfaces.lock().expect("surfaces").insert(peer_id, up);
                streams.send(rx).map_err(|error| error.to_string())
            }),
            Duration::from_secs(5),
        );
        (stream, launches, places)
    }

    /// The provider the catalogue offers `action` under, for `caller`.
    fn offered_provider(
        host: &Arc<CapnpBrokerState>,
        caller: &Caller,
        action: &str,
    ) -> Option<String> {
        let page = host
            .action_query(
                caller,
                &CatalogueQuery {
                    max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
                    ..CatalogueQuery::default()
                },
            )
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten()?
            .ok()?;
        page.rows
            .into_iter()
            .find_map(|row| (row.action.as_ref()?.address.action == action).then_some(row.provider))
    }

    fn invoke(
        host: &Arc<CapnpBrokerState>,
        caller: &Caller,
        provider: &str,
        action: &str,
        confirmation: Option<Confirmation>,
    ) -> Result<tilo_actions::remote::InvocationReceipt, ActionHostError> {
        host.action_invoke(
            caller,
            None,
            None,
            &InvokeRequest {
                on_behalf_of: None,
                invocation: tilo_actions::InvocationId::new(),
                target: RemoteActionTarget {
                    provider: provider.to_string(),
                    address: tilo_actions::ActionAddress::new("files.app", action),
                },
                argument: InvokeArgument::None,
                context: None,
                confirmation,
                revision: None,
                wait_ms: None,
            },
        )
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .unwrap_or_else(|| {
            Err(ActionHostError::Unavailable {
                reason: format!("the admission of {action} never arrived"),
            })
        })
    }

    #[test]
    fn invoking_a_closed_apps_launchable_action_launches_it_and_delivers() {
        let (state, room) = broker(&workspace());
        state.launchable.retain(&surface(7, &room));
        let (streams, launches) = app_comes_up_as(&state, 8);
        let owner = Caller::local_person("owner").in_room(&room);

        let provider = offered_provider(&state, &owner, "new-window")
            .expect("the closed app's launchable action is offered");
        assert!(is_launchable_provider(&provider));
        invoke(&state, &owner, &provider, "new-window", None).expect("admitted once launched");

        assert_eq!(*launches.lock().expect("launches"), [APP]);
        let mut stream = streams.try_recv().expect("the app came up");
        let delivered = stream.try_recv().expect("the invocation reached the app");
        assert_eq!(delivered.action, "new-window");
        assert_eq!(delivered.actor, "owner");
        assert_eq!(
            offered_provider(&state, &owner, "new-window").as_deref(),
            Some("8"),
            "once the app is live its action is offered on its own connection"
        );
    }

    /// A person's own invocation opens the view where they are looking; an
    /// agent's opens beside the view it already works in, on whichever strip
    /// holds that, so neither loses their place.
    #[test]
    fn a_launched_view_opens_where_its_actor_is() {
        use crate::test_support::{Grants as _, KindSet, Subject};
        const NOTES: &str = "so.tilo.notes";
        let mut workspace = workspace();
        workspace.admit_agent("scribe", KindSet::READ);
        for app in [APP, NOTES] {
            workspace.allow_app(Subject::agent("scribe"), app, KindSet::ALL);
        }
        let (state, room) = broker(&workspace);
        state.launchable.retain(&surface(7, &room));
        let mut notes = surface(5, &room);
        notes.app_id = NOTES.into();
        notes.route = CrdtRoute::app(NOTES, &room);
        notes.actions = vec![action("write", tilo_actions::ConfirmationPolicy::None)];
        state.surfaces.lock().expect("surfaces").insert(5, notes);
        let mut notes_stream = state.capture_invocations_for_test(5);
        let (streams, launches, places) = app_comes_up_placed(&state, 8);

        let agent = Caller::local_agent("scribe").in_room(&room);
        invoke(&state, &agent, "5", "write", None).expect("the agent works in Notes");
        assert_eq!(
            notes_stream.try_recv().expect("delivered").actor,
            "agent:scribe"
        );
        let provider = offered_provider(&state, &agent, "new-window")
            .expect("the closed app's action is offered to the agent");
        invoke(&state, &agent, &provider, "new-window", None).expect("admitted once launched");
        assert_eq!(*launches.lock().expect("launches"), [APP]);
        assert_eq!(
            *places.lock().expect("places"),
            [(
                None,
                LaunchPlace::Beside {
                    app_id: NOTES.into()
                }
            )],
            "the agent's view opens beside the one it works in"
        );
        let mut stream = streams.try_recv().expect("the app came up");
        assert_eq!(stream.try_recv().expect("delivered").actor, "agent:scribe");

        state.surfaces.lock().expect("surfaces").remove(&8);
        let owner = Caller::local_person("owner").in_room(&room);
        let provider = offered_provider(&state, &owner, "new-window")
            .expect("offered again once the app closed");
        invoke(&state, &owner, &provider, "new-window", None).expect("admitted once launched");
        assert_eq!(
            places.lock().expect("places").last(),
            Some(&(None, LaunchPlace::Focus)),
            "a person's own invocation opens where they are"
        );
    }

    /// An action of a record the app does not open by default launches the
    /// app on that record, and runs in its view.
    #[test]
    fn a_closed_records_action_launches_the_app_on_that_record() {
        let (state, room) = broker(&workspace());
        let mut draft = surface(7, &room);
        draft.route.record_id = "draft-record".into();
        state.launchable.retain(&draft);
        let (streams, _, places) = app_comes_up_placed(&state, 8);
        let owner = Caller::local_person("owner").in_room(&room);
        let provider = offered_provider(&state, &owner, "new-window")
            .expect("the closed record's action is offered");
        invoke(&state, &owner, &provider, "new-window", None).expect("admitted once launched");
        assert_eq!(
            *places.lock().expect("places"),
            [(Some("draft-record".to_owned()), LaunchPlace::Focus)]
        );
        let mut stream = streams.try_recv().expect("the app came up on the record");
        assert_eq!(stream.try_recv().expect("delivered").action, "new-window");
    }

    /// An agent running a task is held to the task's grants on every call,
    /// whether or not the call names the task.
    #[test]
    fn an_agents_run_is_held_to_its_tasks_grants_without_naming_it() {
        use crate::test_support::{Answer, Grants as _, KindSet, Subject, TreePath};
        let mut workspace = workspace();
        workspace.admit_agent("scribe", KindSet::READ_UNDOABLE);
        workspace.answer(
            Subject::Task("task:tidy".into()),
            &TreePath::root().app(APP).rest().action("new-window"),
            KindSet::READ_UNDOABLE,
            Answer::Deny,
        );
        workspace.tasks.push(tilo_desktop_model::WorkspaceTask {
            id: "task:tidy".into(),
            title: "Tidy up".into(),
            created_by: "owner".into(),
            assigned_agent: Some("scribe".into()),
            route: tilo_desktop_model::WorkspaceTaskRoute::Model,
            status: tilo_desktop_model::WorkspaceTaskStatus::InProgress,
            resource: None,
            created_at: 1,
            updated_at: 1,
            audit: Vec::new(),
        });
        let (state, room) = broker(&workspace);
        state
            .surfaces
            .lock()
            .expect("surfaces")
            .insert(5, surface(5, &room));
        let mut app = state.capture_invocations_for_test(5);
        let agent = Caller::local_agent("scribe").in_room(&room);
        assert!(
            invoke(&state, &agent, "5", "new-window", None).is_err(),
            "the running task's grants leave this action out"
        );
        assert!(app.try_recv().is_err(), "nothing ran");
    }

    /// The broker reads a field edit without the app's code: only removing
    /// an item of a collection not published as quiet asks for confirmation;
    /// sets and an outbox's settle do not.
    #[test]
    fn a_field_edit_asks_only_when_it_removes_from_a_loud_collection() {
        let workspace = workspace();
        let (state, room) = broker(&workspace);
        let mut edits = surface(5, &room);
        let mut field_edit = action("field-edit", tilo_actions::ConfirmationPolicy::None);
        field_edit.contract.tags = vec![
            tilo_actions::ARGUMENT_RISK_TAG.to_owned(),
            tilo_action_policy::edits::FIELD_EDIT_TAG.to_owned(),
            format!("{}drafts", tilo_action_policy::edits::QUIET_REMOVAL_TAG),
        ];
        edits.actions = vec![field_edit];
        state.surfaces.lock().expect("surfaces").insert(5, edits);
        let mut app = state.capture_invocations_for_test(5);
        let owner = Caller::local_person("owner").in_room(&room);
        let edit = |edit: serde_json::Value| {
            state
                .action_invoke(
                    &owner,
                    None,
                    None,
                    &InvokeRequest {
                        on_behalf_of: None,
                        invocation: tilo_actions::InvocationId::new(),
                        target: RemoteActionTarget {
                            provider: "5".into(),
                            address: tilo_actions::ActionAddress::new("files.app", "field-edit"),
                        },
                        argument: InvokeArgument::Json(
                            serde_json::json!({ "edit": edit })
                                .as_object()
                                .cloned()
                                .expect("an object"),
                        ),
                        context: None,
                        confirmation: None,
                        revision: None,
                        wait_ms: None,
                    },
                )
                .wait_timeout(tilo_test_env::hang_bound())
                .ok()
                .flatten()
                .expect("answered")
        };
        edit(serde_json::json!({"path": [{"field": "title"}], "op": "set", "value": "Plan"}))
            .expect("a set asks nothing");
        edit(serde_json::json!({"path": [{"field": "outbox"}], "op": "settle", "id": "e1", "outcome": {}}))
            .expect("settling asks nothing");
        edit(serde_json::json!({"path": [{"field": "drafts"}], "op": "remove-item", "id": "d1"}))
            .expect("a quiet collection's removal asks nothing");
        assert_eq!(app.try_recv().expect("delivered").action, "field-edit");
        assert_eq!(
            edit(
                serde_json::json!({"path": [{"field": "notes"}], "op": "remove-item", "id": "n1"})
            )
            .err(),
            Some(ActionHostError::ConfirmationRequired),
            "removing a note asks, as a delete verb does"
        );
    }

    /// Work done in a task is bounded by the task's grant set: what the
    /// agent may otherwise do, it does in the task only where the task's
    /// grants allow it.
    #[test]
    fn an_agent_working_in_a_task_is_bounded_by_the_tasks_grants() {
        use crate::test_support::{Answer, Grants as _, KindSet, Subject, TreePath};
        let mut workspace = workspace();
        workspace.admit_agent("scribe", KindSet::READ_UNDOABLE);
        workspace.answer(
            Subject::Task("task:tidy".into()),
            &TreePath::root().app(APP).rest().action("empty-trash"),
            KindSet::READ_UNDOABLE,
            Answer::Allow,
        );
        workspace.answer(
            Subject::Task("task:tidy".into()),
            &TreePath::root().app(APP).rest().action("new-window"),
            KindSet::READ_UNDOABLE,
            Answer::Deny,
        );
        let (state, room) = broker(&workspace);
        state
            .surfaces
            .lock()
            .expect("surfaces")
            .insert(5, surface(5, &room));
        let mut app = state.capture_invocations_for_test(5);
        let agent = Caller::local_agent("scribe").in_room(&room);
        let in_task =
            |task: &str| tilo_actions::ActionContext::default().with_group(task, "Tidy up", "task");
        let invoke_in = |context: Option<tilo_actions::ActionContext>| {
            state
                .action_invoke(
                    &agent,
                    None,
                    None,
                    &InvokeRequest {
                        on_behalf_of: None,
                        invocation: tilo_actions::InvocationId::new(),
                        target: RemoteActionTarget {
                            provider: "5".into(),
                            address: tilo_actions::ActionAddress::new("files.app", "new-window"),
                        },
                        argument: InvokeArgument::None,
                        context,
                        confirmation: None,
                        revision: None,
                        wait_ms: None,
                    },
                )
                .wait_timeout(tilo_test_env::hang_bound())
                .ok()
                .flatten()
                .expect("answered")
        };

        assert!(
            invoke_in(Some(in_task("task:tidy"))).is_err(),
            "the task's grants leave this action out"
        );
        assert!(app.try_recv().is_err(), "nothing ran");
        invoke_in(None).expect("outside the task the agent's own grants decide");
        assert_eq!(app.try_recv().expect("delivered").action, "new-window");
        invoke_in(Some(in_task("task:other"))).expect("a task without grants bounds nothing");
        assert_eq!(app.try_recv().expect("delivered").action, "new-window");
    }

    /// An action the room's grants let an agent take only after asking is
    /// offered to it marked as asking for confirmation. The agent's own word
    /// does not confirm it; the person's answer, witnessed on their device,
    /// does, for that one invocation.
    #[test]
    fn an_agent_asked_first_acts_only_on_the_persons_witnessed_confirmation() {
        use crate::test_support::{Answer, Grants as _, KindSet, Subject, TreePath};
        use tilo_actions::remote::CatalogueQuery;
        let mut workspace = workspace();
        workspace.admit_agent("scribe", KindSet::READ);
        workspace.answer(
            Subject::agent("scribe"),
            &TreePath::root().app(APP),
            KindSet::UNDOABLE,
            Answer::Ask,
        );
        let (state, room) = broker(&workspace);
        state
            .surfaces
            .lock()
            .expect("surfaces")
            .insert(5, surface(5, &room));
        let mut app = state.capture_invocations_for_test(5);
        let agent = Caller::local_agent("scribe").in_room(&room);

        let page = state
            .action_query(&agent, &CatalogueQuery::default())
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten()
            .expect("answered")
            .expect("the agent may read the catalogue");
        let offered = page
            .rows
            .iter()
            .filter_map(|row| row.action.as_ref())
            .find(|descriptor| descriptor.address.action == "new-window")
            .expect("an action asking first is offered");
        assert_eq!(
            offered.contract.confirmation,
            tilo_actions::ConfirmationPolicy::Confirm,
            "marked as asking for confirmation"
        );

        assert_eq!(
            invoke(&state, &agent, "5", "new-window", None).err(),
            Some(ActionHostError::ConfirmationRequired)
        );
        assert_eq!(
            invoke(
                &state,
                &agent,
                "5",
                "new-window",
                Some(Confirmation::Confirmed)
            )
            .err(),
            Some(ActionHostError::ConfirmationRequired),
            "an agent never confirms by its own word"
        );
        assert!(app.try_recv().is_err(), "nothing ran");

        let witnessed = state
            .witness_confirmation("agent:scribe", ("files.app", "new-window"), None)
            .expect("the person's answer is recorded");
        invoke(
            &state,
            &agent,
            "5",
            "new-window",
            Some(Confirmation::Witnessed(witnessed.clone())),
        )
        .expect("the person's confirmation admits the call");
        assert_eq!(app.try_recv().expect("delivered").actor, "agent:scribe");
        assert_eq!(
            invoke(
                &state,
                &agent,
                "5",
                "new-window",
                Some(Confirmation::Witnessed(witnessed))
            )
            .err(),
            Some(ActionHostError::ConfirmationRequired),
            "the answer was for that one call"
        );
        let other = state
            .witness_confirmation("agent:scribe", ("files.app", "empty-trash"), None)
            .expect("recorded");
        assert_eq!(
            invoke(
                &state,
                &agent,
                "5",
                "new-window",
                Some(Confirmation::Witnessed(other))
            )
            .err(),
            Some(ActionHostError::ConfirmationRequired),
            "an answer for another action confirms nothing here"
        );
    }

    /// An agent's action shows in the view it runs in, named with the
    /// person it acts for; a person's own action on this device does not
    /// put a second marker on what their view already shows.
    #[test]
    fn an_agents_action_is_present_in_the_view_it_runs_in() {
        use crate::test_support::{Grants as _, KindSet, Subject};
        let mut workspace = workspace();
        workspace.admit_agent("scribe", KindSet::UNDOABLE);
        workspace.allow_app(Subject::agent("scribe"), APP, KindSet::UNDOABLE);
        let (state, room, fanout) = broker_with_fanout(&workspace);
        state
            .surfaces
            .lock()
            .expect("surfaces")
            .insert(5, surface(5, &room));
        let mut app = state.capture_invocations_for_test(5);
        let (seen, mut heard) = tokio::sync::mpsc::unbounded_channel();
        let _subscription = fanout.subscribe_rooms(
            vec![room.clone()],
            Box::new(move |event, _| {
                let _ = seen.send(event.clone());
            }),
        );

        let owner = Caller::local_person("owner").in_room(&room);
        invoke(&state, &owner, "5", "new-window", None).expect("the person acts");
        assert_eq!(app.try_recv().expect("delivered").actor, "owner");
        assert!(
            heard.try_recv().is_err(),
            "a person's own view already shows them"
        );

        let agent = Caller::local_agent("scribe")
            .in_room(&room)
            .acting_for("owner");
        invoke(&state, &agent, "5", "new-window", None).expect("the agent acts");
        assert_eq!(app.try_recv().expect("delivered").actor, "agent:scribe");
        let Ok(ui::collaboration::CollabEvent::RemotePresenceUpdate {
            user_id,
            key,
            payload: ui::collaboration::PresencePayload::Interaction(interaction),
        }) = heard.try_recv()
        else {
            panic!("the agent's action shows in its view");
        };
        assert_eq!(user_id, "agent:scribe");
        assert_eq!(
            key,
            ui::presence::PresenceKey::resource(APP, "files.app", "files.app")
        );
        assert_eq!(
            interaction.actor,
            tilo_actions::InteractionActor::new("agent:scribe", tilo_actions::ActorKind::Agent)
                .acting_for("owner")
        );
        assert_eq!(interaction.phase, tilo_actions::InteractionPhase::Applying);
        assert_eq!(interaction.action.id, "new-window");

        // A Shortcut's step keeps the person's authority, yet the view shows
        // the Shortcut acting for them.
        let shortcut = Caller::local_person("owner")
            .in_room(&room)
            .running_shortcut("morning");
        invoke(&state, &shortcut, "5", "new-window", None).expect("the step runs");
        assert_eq!(app.try_recv().expect("delivered").actor, "owner");
        let Ok(ui::collaboration::CollabEvent::RemotePresenceUpdate {
            payload: ui::collaboration::PresencePayload::Interaction(interaction),
            ..
        }) = heard.try_recv()
        else {
            panic!("the Shortcut's step shows in its view");
        };
        assert_eq!(
            interaction.actor,
            tilo_actions::InteractionActor::new(
                "shortcut:morning",
                tilo_actions::ActorKind::Shortcut
            )
            .acting_for("owner")
        );
    }

    /// An MCP client on the person's device is an agent acting for them,
    /// bounded by the delegation a device gives its agents: it does what the
    /// person may do in the document, an outside action stays the person's to
    /// allow, and the view shows the agent, for the person.
    #[test]
    fn a_client_the_person_delegates_to_acts_as_their_agent() {
        let (state, room, fanout) = broker_with_fanout(&workspace());
        let mut published = surface(5, &room);
        let mut send = action("send", tilo_actions::ConfirmationPolicy::None);
        send.contract.kind = tilo_actions::ActionKind::Outside;
        published.actions.push(send);
        state
            .surfaces
            .lock()
            .expect("surfaces")
            .insert(5, published);
        let mut app = state.capture_invocations_for_test(5);
        let (seen, mut heard) = tokio::sync::mpsc::unbounded_channel();
        let _subscription = fanout.subscribe_rooms(
            vec![room.clone()],
            Box::new(move |event, _| {
                let _ = seen.send(event.clone());
            }),
        );
        let client = Caller::local_person("owner")
            .in_room(&room)
            .in_new_execution()
            .delegated_to("mcp-1");

        invoke(&state, &client, "5", "new-window", None).expect("within the delegation");
        let delivered = app.try_recv().expect("delivered");
        assert_eq!(delivered.actor, "agent:mcp-1");
        let invoker = delivered
            .context()
            .and_then(|context| context.invoker.clone())
            .expect("the host records the invoker");
        assert_eq!(invoker.kind, tilo_actions::ActorKind::Agent);
        assert_eq!(invoker.on_behalf_of.as_deref(), Some("owner"));
        let Ok(ui::collaboration::CollabEvent::RemotePresenceUpdate {
            payload: ui::collaboration::PresencePayload::Interaction(interaction),
            ..
        }) = heard.try_recv()
        else {
            panic!("the agent shows in the view");
        };
        assert_eq!(
            interaction.actor,
            tilo_actions::InteractionActor::new("agent:mcp-1", tilo_actions::ActorKind::Agent)
                .acting_for("owner")
        );

        assert!(
            invoke(&state, &client, "5", "send", None).is_err(),
            "an outside action is the person's to allow"
        );
        assert!(app.try_recv().is_err(), "nothing outside ran");
        let owner = Caller::local_person("owner").in_room(&room);
        invoke(&state, &owner, "5", "send", None).expect("the person may");
        assert_eq!(app.try_recv().expect("delivered").actor, "owner");
    }

    /// An application target reaches an app that never ran in the workspace:
    /// it launches the app and delivers once the app offers the action, and
    /// reaches the running app without launching it again.
    #[test]
    fn an_application_target_launches_an_app_that_never_ran_and_delivers() {
        let (state, room) = broker(&workspace());
        let (streams, launches) = app_comes_up_as(&state, 8);
        let owner = Caller::local_person("owner").in_room(&room);
        let target =
            tilo_actions::remote::application_action_target(&room, APP, "files.app", "new-window");
        assert!(state.launchable.surfaces().is_empty(), "nothing retained");

        invoke(&state, &owner, &target.provider, "new-window", None)
            .expect("admitted once launched");
        assert_eq!(*launches.lock().expect("launches"), [APP]);
        let mut stream = streams.try_recv().expect("the app came up");
        let delivered = stream.try_recv().expect("the invocation reached the app");
        assert_eq!(delivered.action, "new-window");
        assert_eq!(delivered.actor, "owner");

        invoke(&state, &owner, &target.provider, "new-window", None)
            .expect("the running app takes it");
        assert_eq!(
            launches.lock().expect("launches").len(),
            1,
            "a running app is not launched again"
        );
        assert_eq!(
            stream.try_recv().expect("delivered again").action,
            "new-window"
        );
        assert_eq!(
            invoke(&state, &owner, &target.provider, "empty-trash", None),
            Err(ActionHostError::ConfirmationRequired),
            "the app's own contract admits what it offers"
        );
    }

    #[test]
    fn an_application_target_is_refused_before_anything_launches() {
        let (state, room) = broker(&workspace());
        let (_streams, launches) = app_comes_up_as(&state, 8);
        let target =
            tilo_actions::remote::application_action_target(&room, APP, "files.app", "new-window");
        assert_eq!(
            invoke(
                &state,
                &Caller::local_person("stranger").in_room(&room),
                &target.provider,
                "new-window",
                None,
            ),
            Err(ActionHostError::NotPermitted),
            "a caller who may not launch the app does not start it"
        );
        assert_eq!(
            invoke(
                &state,
                &Caller::authenticated([0xA1; 32], [0xA2; 32]),
                &target.provider,
                "new-window",
                None,
            ),
            Err(ActionHostError::UnknownAction),
            "a remote device does not launch apps on this desktop"
        );
        let elsewhere =
            tilo_actions::remote::application_action_target("no-such-room", APP, "files.app", "x");
        assert_eq!(
            invoke(
                &state,
                &Caller::local_person("owner").in_room(&room),
                &elsewhere.provider,
                "new-window",
                None,
            ),
            Err(ActionHostError::UnknownAction),
            "a workspace the caller is not in launches nothing"
        );
        assert!(launches.lock().expect("launches").is_empty());
    }

    #[test]
    fn a_launchable_invocation_is_refused_before_anything_launches() {
        let (state, room) = broker(&workspace());
        state.launchable.retain(&surface(7, &room));
        let (_streams, launches) = app_comes_up_as(&state, 8);
        let owner = Caller::local_person("owner").in_room(&room);
        let provider = offered_provider(&state, &owner, "empty-trash").expect("offered");

        assert_eq!(
            invoke(&state, &owner, &provider, "empty-trash", None),
            Err(ActionHostError::ConfirmationRequired)
        );
        assert_eq!(
            invoke(
                &state,
                &Caller::local_person("stranger").in_room(&room),
                &provider,
                "new-window",
                None,
            ),
            Err(ActionHostError::NotPermitted),
            "a caller who may not launch the app does not start it"
        );
        assert_eq!(
            invoke(
                &state,
                &Caller::authenticated([0xA1; 32], [0xA2; 32]),
                &provider,
                "new-window",
                None,
            ),
            Err(ActionHostError::UnknownAction),
            "a remote device does not launch apps on this desktop"
        );
        assert!(launches.lock().expect("launches").is_empty());

        invoke(
            &state,
            &owner,
            &provider,
            "empty-trash",
            Some(Confirmation::Confirmed),
        )
        .expect("confirmed evidence is bound to the relaunched app");
    }

    #[test]
    fn an_app_that_never_offers_the_action_times_out_with_a_clear_error() {
        let (state, room) = broker(&workspace());
        state.launchable.retain(&surface(7, &room));
        state
            .launchable
            .replace_launcher_for_test(Arc::new(|_, _, _, _| Ok(())), Duration::from_millis(200));
        let owner = Caller::local_person("owner").in_room(&room);
        let provider = offered_provider(&state, &owner, "new-window").expect("offered");

        match invoke(&state, &owner, &provider, "new-window", None) {
            Err(ActionHostError::Unavailable { reason }) => assert!(
                reason.contains(APP) && reason.contains("did not offer"),
                "the timeout names the app and what it failed to do: {reason}"
            ),
            other => panic!("expected a timeout, got {other:?}"),
        }
    }

    /// Stand in for a launcher whose app comes up only later, when the
    /// test publishes it; each launch is recorded.
    fn app_comes_up_later(state: &Arc<CapnpBrokerState>) -> Arc<Mutex<Vec<String>>> {
        let launches = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&launches);
        state.launchable.replace_launcher_for_test(
            Arc::new(move |app_id, _, _, _| {
                recorded.lock().expect("launches").push(app_id.to_string());
                Ok(())
            }),
            tilo_test_env::hang_bound(),
        );
        launches
    }

    /// Invoke `action` on `provider` from its own thread, as a caller that
    /// waits for the admission does.
    fn invoke_in_background(
        state: &Arc<CapnpBrokerState>,
        caller: &Caller,
        provider: &str,
        action: &'static str,
    ) -> std::thread::JoinHandle<Result<tilo_actions::remote::InvocationReceipt, ActionHostError>>
    {
        let state = Arc::clone(state);
        let caller = caller.clone();
        let provider = provider.to_string();
        std::thread::spawn(move || invoke(&state, &caller, &provider, action, None))
    }

    /// Wait until `waiters` launches have started and are each parked on
    /// the broker's publisher-change signal.
    fn wait_for_parked_launches(
        state: &Arc<CapnpBrokerState>,
        launches: &Arc<Mutex<Vec<String>>>,
        waiters: usize,
    ) {
        tilo_test_env::wait_until("each launch to wait for its app to publish", || {
            launches.lock().expect("launches").len() == waiters
                && state.surface_changes.receiver_count() == waiters
        });
    }

    /// A launch waits on the broker's publication signal, not on a timer:
    /// while the app is still starting, the waiting invocation holds a
    /// receiver of that signal, and the app publishing its action and
    /// opening its stream admits the invocation. The delivery timeout is the
    /// hang bound, so nothing but the publication can end the wait in time.
    #[test]
    fn launch_then_deliver_wakes_on_publication_without_polling() {
        let (state, room) = broker(&workspace());
        state.launchable.retain(&surface(7, &room));
        let launches = app_comes_up_later(&state);
        let owner = Caller::local_person("owner").in_room(&room);
        let provider = offered_provider(&state, &owner, "new-window").expect("offered");

        let invoking = invoke_in_background(&state, &owner, &provider, "new-window");
        wait_for_parked_launches(&state, &launches, 1);
        let mut stream = state.capture_invocations_for_test(8);
        state.replace_published_surfaces_for_test(vec![surface(8, &room)]);

        invoking
            .join()
            .expect("invoking thread")
            .expect("admitted once the app publishes");
        assert_eq!(
            stream.try_recv().expect("delivered to the app").action,
            "new-window"
        );
        tilo_test_env::wait_until("the finished wait to drop its receiver", || {
            state.surface_changes.receiver_count() == 0
        });
    }

    /// Readiness needs both the action and the invocation stream, and the
    /// app may offer them in either order; the wait wakes on each and
    /// delivers once both exist, never before.
    #[test]
    fn launch_then_deliver_handles_publish_before_subscribe_and_reverse() {
        for surface_first in [true, false] {
            let (state, room) = broker(&workspace());
            state.launchable.retain(&surface(7, &room));
            let launches = app_comes_up_later(&state);
            let owner = Caller::local_person("owner").in_room(&room);
            let provider = offered_provider(&state, &owner, "new-window").expect("offered");

            let invoking = invoke_in_background(&state, &owner, &provider, "new-window");
            wait_for_parked_launches(&state, &launches, 1);
            let mut stream = if surface_first {
                state.replace_published_surfaces_for_test(vec![surface(8, &room)]);
                assert!(
                    !invoking.is_finished(),
                    "an action without an invocation stream is not delivered"
                );
                state.capture_invocations_for_test(8)
            } else {
                let stream = state.capture_invocations_for_test(8);
                assert!(
                    !invoking.is_finished(),
                    "a stream without the action is not delivered"
                );
                state.replace_published_surfaces_for_test(vec![surface(8, &room)]);
                stream
            };

            invoking
                .join()
                .expect("invoking thread")
                .unwrap_or_else(|error| {
                    panic!("surface first: {surface_first}: not admitted: {error:?}")
                });
            assert_eq!(
                stream.try_recv().expect("delivered to the app").action,
                "new-window"
            );
            assert_eq!(launches.lock().expect("launches").len(), 1);
        }
    }

    /// Many invocations of a closed app wait at once without a thread
    /// each: every one parks on the same signal, and one publication
    /// admits them all.
    #[test]
    fn concurrent_launch_waiters_all_wake_on_one_publication() {
        const WAITERS: usize = 16;
        let (state, room) = broker(&workspace());
        state.launchable.retain(&surface(7, &room));
        let launches = app_comes_up_later(&state);
        let owner = Caller::local_person("owner").in_room(&room);
        let provider = offered_provider(&state, &owner, "new-window").expect("offered");

        let invoking = (0..WAITERS)
            .map(|_| invoke_in_background(&state, &owner, &provider, "new-window"))
            .collect::<Vec<_>>();
        wait_for_parked_launches(&state, &launches, WAITERS);
        let mut stream = state.capture_invocations_for_test(8);
        state.replace_published_surfaces_for_test(vec![surface(8, &room)]);

        for invocation in invoking {
            invocation
                .join()
                .expect("invoking thread")
                .expect("every waiter is admitted");
        }
        let mut delivered = 0_usize;
        while let Ok(envelope) = stream.try_recv() {
            assert_eq!(envelope.action, "new-window");
            delivered = delivered.saturating_add(1);
        }
        assert_eq!(delivered, WAITERS);
    }

    /// A caller that drops its answer while the app is still starting ends
    /// the wait then — long before the delivery timeout — and the app that
    /// comes up afterwards receives nothing.
    #[test]
    fn launch_then_deliver_stops_when_caller_abandons() {
        let (state, room) = broker(&workspace());
        state.launchable.retain(&surface(7, &room));
        let launches = app_comes_up_later(&state);
        let owner = Caller::local_person("owner").in_room(&room);
        let provider = offered_provider(&state, &owner, "new-window").expect("offered");

        let answer = state.action_invoke(
            &owner,
            None,
            None,
            &InvokeRequest {
                on_behalf_of: None,
                invocation: tilo_actions::InvocationId::new(),
                target: RemoteActionTarget {
                    provider,
                    address: tilo_actions::ActionAddress::new("files.app", "new-window"),
                },
                argument: InvokeArgument::None,
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: None,
            },
        );
        wait_for_parked_launches(&state, &launches, 1);
        drop(answer);
        tilo_test_env::wait_until("the abandoned wait to end", || {
            state.surface_changes.receiver_count() == 0
        });

        let mut stream = state.capture_invocations_for_test(8);
        state.replace_published_surfaces_for_test(vec![surface(8, &room)]);
        assert!(
            stream.try_recv().is_err(),
            "an abandoned invocation is not delivered"
        );
    }

    type AdmissionAnswer = tilo_actions::remote::Pending<
        Result<tilo_actions::remote::InvocationReceipt, ActionHostError>,
    >;

    fn launch_answer(state: &Arc<CapnpBrokerState>, room: &str) -> AdmissionAnswer {
        let caller = Caller::local_person("owner").in_room(room);
        let provider = offered_provider(state, &caller, "new-window").expect("offered");
        state.action_invoke(
            &caller,
            None,
            None,
            &InvokeRequest {
                on_behalf_of: None,
                invocation: tilo_actions::InvocationId::new(),
                target: RemoteActionTarget {
                    provider,
                    address: tilo_actions::ActionAddress::new("files.app", "new-window"),
                },
                argument: InvokeArgument::None,
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: None,
            },
        )
    }

    struct OccupiedLaunchPool {
        _serial: std::sync::MutexGuard<'static, ()>,
        release: Arc<(Mutex<bool>, std::sync::Condvar)>,
        _answers: Vec<AdmissionAnswer>,
    }

    impl Drop for OccupiedLaunchPool {
        fn drop(&mut self) {
            let (released, wake) = &*self.release;
            *released.lock().expect("release") = true;
            wake.notify_all();
        }
    }

    /// Occupy the production delivery pool with four authorized host calls.
    /// The guard releases them even when the regression assertion panics.
    fn occupy_launch_pool() -> OccupiedLaunchPool {
        static SATURATION: Mutex<()> = Mutex::new(());
        let serial = SATURATION.lock().expect("saturation fixture");
        let (state, room) = broker(&workspace());
        state.launchable.retain(&surface(7, &room));
        let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let held = release.clone();
        let (started, launches) = std::sync::mpsc::channel();
        state.launchable.replace_launcher_for_test(
            Arc::new(move |_, _, _, _| {
                started.send(()).expect("launch started");
                let (released, wake) = &*held;
                let guard = released.lock().expect("release");
                drop(
                    wake.wait_while(guard, |released| !*released)
                        .expect("release wait"),
                );
                Ok(())
            }),
            tilo_test_env::hang_bound(),
        );
        let pool = OccupiedLaunchPool {
            _serial: serial,
            release,
            _answers: (0..LAUNCH_CALLS)
                .map(|_| launch_answer(&state, &room))
                .collect(),
        };
        for _ in 0..LAUNCH_CALLS {
            tilo_test_env::recv("every launcher worker to enter the host call", &launches);
        }
        pool
    }

    fn drain_launch_queue() {
        // A following blocking job is a queue drain fence: a cancelled host
        // call must not execute later when the occupied workers are released.
        let (done, drained) = std::sync::mpsc::channel();
        delivery_runtime()
            .expect("runtime")
            .spawn_blocking(move || {
                done.send(()).expect("drained");
            });
        tilo_test_env::recv("the launch queue to drain", &drained);
    }

    #[test]
    fn queued_launch_stops_before_submission_when_caller_abandons() {
        let pool = occupy_launch_pool();
        let (state, room) = broker(&workspace());
        state.launchable.retain(&surface(7, &room));
        let (_streams, launches) = app_comes_up_as(&state, 8);
        let answer = launch_answer(&state, &room);
        tilo_test_env::wait_until("the queued launch to subscribe", || {
            state.surface_changes.receiver_count() == 1
        });
        drop(answer);
        tilo_test_env::wait_until(
            "queued abandonment before a launcher worker is free",
            || state.surface_changes.receiver_count() == 0,
        );
        drop(pool);
        drain_launch_queue();
        assert!(
            launches.lock().expect("launches").is_empty(),
            "an abandoned queued invocation must never launch the app"
        );
    }

    #[test]
    fn queued_launch_reports_deadline_before_a_launcher_worker_is_free() {
        let pool = occupy_launch_pool();
        let (state, room) = broker(&workspace());
        state.launchable.retain(&surface(7, &room));
        let (_streams, launches) = app_comes_up_as(&state, 8);
        *state.launchable.delivery_timeout.lock().expect("timeout") = Duration::from_millis(200);
        let answer = launch_answer(&state, &room);
        let result = answer
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten();
        assert!(
            matches!(result, Some(Err(ActionHostError::Unavailable { ref reason }))
            if reason.contains("within 0.2 s")),
            "the queued invocation must report its delivery deadline before pool release: {result:?}"
        );
        drop(pool);
        drain_launch_queue();
        assert!(
            launches.lock().expect("launches").is_empty(),
            "an expired queued invocation cannot launch the app"
        );
    }

    #[test]
    fn launchable_actions_survive_a_broker_restart() {
        let dir = tempfile::tempdir().expect("state dir");
        let path = dir.path().join("actions/launchable.json");
        let workspace = workspace();
        let (state, room) = broker(&workspace);
        state.launchable.persist_at(&path);
        state.launchable.retain(&surface(7, &room));
        // Dropping the session finishes its last save.
        drop(state);

        let (restarted, _) = broker(&workspace);
        restarted.launchable.persist_at(&path);
        let retained = restarted.launchable.surfaces();
        assert_eq!(retained.len(), 1);
        assert!(!retained[0].live);

        let (streams, _) = app_comes_up_as(&restarted, 3);
        let owner = Caller::local_person("owner").in_room(&room);
        let provider = offered_provider(&restarted, &owner, "new-window")
            .expect("the action is still offered after a restart");
        invoke(&restarted, &owner, &provider, "new-window", None)
            .expect("admitted after a restart");
        let mut stream = streams.try_recv().expect("the app came up");
        assert_eq!(stream.try_recv().expect("delivered").action, "new-window");
    }

    #[test]
    fn a_surface_offering_nothing_forgets_the_retained_one() {
        let (state, room) = broker(&workspace());
        state.launchable.retain(&surface(7, &room));
        let mut empty = surface(7, &room);
        empty.actions.clear();
        state.launchable.retain(&empty);
        assert!(state.launchable.surfaces().is_empty());
    }

    #[test]
    fn every_published_action_stays_offered_once_the_app_closes() {
        let (state, room) = broker(&workspace());
        let mut published = surface(7, &room);
        published.actions.push(PublishedAction {
            operation_kind: tilo_actions::OperationKind::Document,
            contract: tilo_actions::ActionContract::default(),
            ..action("rename", tilo_actions::ConfirmationPolicy::None)
        });
        state.launchable.retain(&published);
        let retained = state.launchable.surfaces();
        assert_eq!(retained.len(), 1);
        assert_eq!(
            retained[0]
                .actions
                .iter()
                .map(|action| action.action.as_str())
                .collect::<Vec<_>>(),
            ["new-window", "empty-trash", "rename"],
            "no action needs to opt in to being reachable while its app is closed"
        );
    }
}
