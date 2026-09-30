//! Actions an application keeps offering while it is closed.
//!
//! An application opts an action into [`DiscoveryScope::Launchable`] when
//! launching the app and then delivering the invocation is a sensible way to
//! run it. The broker retains the last such projection per app, workspace and
//! record, on disk, so a caller can still find and invoke it after the app
//! closes or the shell restarts. The retained rows are catalogue state only:
//! invoking one launches the app and admits the invocation against the
//! surface the app publishes fresh, never against the retained copy.
//!
//! [`DiscoveryScope::Launchable`]: tilo_actions::DiscoveryScope::Launchable

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use super::{PublishedAction, PublishedSurface};
use crate::collab_broker::CrdtRoute;

/// Prefix of the provider token naming a retained launchable surface.
const LAUNCHABLE_PROVIDER: &str = "launch";

/// How long an invocation waits for the application it launched to publish
/// the action and its invocation stream.
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(30);

/// `(app_id, cluster_id, record_id)`: one retained surface.
type SurfaceKey = (String, String, String);

/// Starts `app_id` in a workspace in place of the host's launcher. Tests
/// stand in for launching the installed app with one.
pub(super) type AppLauncher =
    Arc<dyn Fn(&str, &tilo_desktop_model::Workspace) -> Result<(), String> + Send + Sync>;

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
        .filter(|action| {
            action.contract.discovery == tilo_actions::DiscoveryScope::Launchable
                && action.contract.validate_declaration().is_empty()
        })
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
}

impl LaunchableCatalogue {
    pub(super) fn new() -> Self {
        Self {
            surfaces: Mutex::new(HashMap::new()),
            store: Mutex::new(None),
            launcher: Mutex::new(None),
            delivery_timeout: Mutex::new(DELIVERY_TIMEOUT),
        }
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

    /// The retained surface `provider` names.
    pub(super) fn surface_for(&self, provider: &str) -> Option<PublishedSurface> {
        self.surfaces.lock().ok().and_then(|surfaces| {
            surfaces
                .values()
                .find(|surface| launchable_provider(surface) == provider)
                .cloned()
        })
    }

    /// Retain the launchable part of a surface a live application just
    /// published, replacing what that app, workspace and record offered
    /// before. A surface with nothing launchable forgets the old one.
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
            resource: "files.app".into(),
            action: name.into(),
            resource_kind: "files.app".into(),
            resource_label: "Files".into(),
            action_label: name.into(),
            description: None,
            operation_kind: tilo_actions::OperationKind::Effect,
            params: Vec::new(),
            contract: tilo_actions::ActionContract {
                discovery: tilo_actions::DiscoveryScope::Launchable,
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
        (state, room)
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
        let (streams, stream) = std::sync::mpsc::channel();
        let launches = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&launches);
        let weak: Weak<CapnpBrokerState> = Arc::downgrade(state);
        state.launchable.replace_launcher_for_test(
            Arc::new(move |app_id, workspace| {
                recorded.lock().expect("launches").push(app_id.to_string());
                let state = weak.upgrade().ok_or("the broker is gone")?;
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                state
                    .invocation_subscribers
                    .lock()
                    .expect("subscribers")
                    .insert(peer_id, tx);
                state
                    .surfaces
                    .lock()
                    .expect("surfaces")
                    .insert(peer_id, surface(peer_id, &workspace.room.to_string()));
                streams.send(rx).map_err(|error| error.to_string())
            }),
            Duration::from_secs(5),
        );
        (stream, launches)
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
                invocation: tilo_actions::InvocationId::new(),
                target: RemoteActionTarget {
                    provider: provider.to_string(),
                    address: tilo_actions::ActionAddress::new("files.app", action),
                },
                argument: InvokeArgument::None,
                context: None,
                confirmation,
                revision: None,
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
            .replace_launcher_for_test(Arc::new(|_, _| Ok(())), Duration::from_millis(200));
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
    fn a_surface_without_launchable_actions_forgets_the_retained_one() {
        let (state, room) = broker(&workspace());
        state.launchable.retain(&surface(7, &room));
        let mut live_only = surface(7, &room);
        for action in &mut live_only.actions {
            action.contract.discovery = tilo_actions::DiscoveryScope::Live;
        }
        state.launchable.retain(&live_only);
        assert!(state.launchable.surfaces().is_empty());
    }
}
