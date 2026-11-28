//! The records a window holds beside the one it shows, in the person's own workspace, and the
//! routes they ride: a person's records are their workspace's, opened as any workspace's are.

use super::*;
use crate::resource::ResourceOwners;
use crate::resource::StoredResource;
use crate::resource::workspace::WorkspaceResources;
use automerge::ReadDoc as _;
use automerge::transaction::Transactable as _;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

const APP: &str = "so.tilo.test-person";
const ROOT_KEY: [u8; 32] = [42; 32];

fn person() -> tilo_ui_base::permissions::MemberId {
    tilo_ui_base::permissions::MemberId::person(hex::encode(ROOT_KEY))
}

/// The person's own workspace, which their root names alone.
fn own_workspace() -> uuid::Uuid {
    collab_types::person_workspace_of(&hex::encode(ROOT_KEY)).expect("a person workspace")
}

/// The type of the watched list: one map of the videos watched, which every device founds the
/// record with.
fn watched_type() -> tilo_actions::surface::Surface {
    serde_json::from_value(serde_json::json!({
        "kind": "test.watched",
        "label": "Watched",
        "version": 1,
        "fields": [{ "path": "watched", "type": { "map": "bool" } }],
    }))
    .expect("a surface")
}

/// The person's watched list `APP` keeps in their own workspace, opened from `owners`.
fn watched_in(owners: ResourceOwners) -> StoredResource {
    let workspace = own_workspace().to_string();
    owners
        .clone()
        .open_beside(APP, &workspace)
        .expect("the app's store in the person's workspace")
        .declare_type("test.watched", watched_type);
    WorkspaceResources::in_workspace(owners, workspace, person())
        .open_resource_at(
            APP,
            &collab_types::person_document_id("test.watched"),
            "test.watched",
            &|doc| {
                doc.put_object(automerge::ROOT, "watched", automerge::ObjType::Map)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            },
        )
        .unwrap()
}

/// One device's copy of the person's watched list in their own workspace.
fn personal_device() -> StoredResource {
    watched_in(ResourceOwners::in_memory())
}

fn watch(record: &StoredResource, video: &str) {
    record
        .edit(true, |doc| {
            let watched = doc.get(automerge::ROOT, "watched").unwrap().unwrap().1;
            doc.put(&watched, video, true)
                .map_err(|error| error.to_string())
        })
        .unwrap();
}

fn watched(record: &StoredResource) -> Vec<String> {
    record
        .read(|doc| {
            let watched = doc.get(automerge::ROOT, "watched").unwrap().unwrap().1;
            doc.keys(&watched).collect()
        })
        .unwrap()
}

/// A view that keeps the person's record open beside what it shows.
struct Holds {
    text: crate::widget::Text,
    owner: crate::history_mode::HistoryOwner,
}

impl tilo_ui_base::widget::Decorator for Holds {
    type Inner = crate::widget::Text;
    fn inner(&self) -> &crate::widget::Text {
        &self.text
    }
    fn inner_mut(&mut self) -> &mut crate::widget::Text {
        &mut self.text
    }
    fn provide<'a>(&'a self, request: &mut tilo_ui_base::widget::capability::Request<'a, '_>) {
        if request.wants::<dyn crate::bound_dispatch::BoundWidget>() {
            request.provide::<dyn crate::bound_dispatch::BoundWidget>(self);
        } else if request.wants::<dyn tilo_ui_base::widget::Decorated>() {
            request.provide::<dyn tilo_ui_base::widget::Decorated>(self);
        } else {
            self.text.provide(request);
        }
    }
}

impl tilo_ui_base::widget::component_walk::ComponentBoundary for Holds {
    fn component_scope(&self) -> Option<tilo_ui_base::widget::component_walk::Binding> {
        None
    }
}

impl crate::bound_dispatch::BoundWidget for Holds {
    fn resource_owners(&self) -> Vec<crate::history_mode::HistoryOwner> {
        vec![self.owner.clone()]
    }
}

fn app_holding(record: &StoredResource) -> App {
    let mut app = App::new_with_controlled_inputs(
        State::new(),
        Box::new(Holds {
            text: crate::widget::Text::new("shown"),
            owner: record.history_owner(),
        }),
    );
    app.collaboration.manager = CollaborationManager::connected_for_lifecycle_test();
    // The app shows a record of a shared workspace; the record kept beside it
    // is not in it.
    app.actions.bound_broker_route_for_test = Some(::collaboration::AcceptedRoute {
        route: ::collaboration::RouteSpec::document(APP, uuid::Uuid::new_v4(), "shown"),
        epoch: uuid::Uuid::new_v4(),
    });
    app.refresh_document_routes();
    // The broker admits each route join asynchronously, and a route refuses
    // documents until its document mode is resolved.
    let deadline = tilo_test_env::hang_deadline();
    while !app.document_routes.owners.iter().all(|entry| {
        app.collaboration
            .manager
            .document_route_publishable(&entry.route)
    }) {
        assert!(
            crate::time::now() < deadline,
            "the record's route never opened"
        );
        std::thread::sleep(crate::time::Duration::from_millis(5));
    }
    app
}

fn snapshots(app: &mut App) -> Vec<(::collaboration::RouteSpec, Vec<u8>)> {
    std::mem::take(&mut app.document_routes.sent)
        .into_iter()
        .filter_map(|(route, request)| match request {
            CollabRequest::Document {
                mutation: collab_types::DocumentMutation::Snapshot(bytes),
                ..
            } => Some((route, bytes)),
            _ => None,
        })
        .collect()
}

/// `document` whole on `route`, as the device's store sends what it holds of
/// the record.
fn from_device(route: &::collaboration::RouteSpec, document: Vec<u8>) -> CollabEvent {
    CollabEvent::Document {
        route: route.clone(),
        mutation: collab_types::DocumentMutation::Snapshot(document),
    }
}

tilo_test_env::scenario! {
    fn a_persons_records_ride_their_own_routes_and_converge;

    phase the_persons_records_ride_their_persons_workspace;
    // What is the person's alone is a record of their own workspace: its route
    // names that workspace, which every device of theirs derives from their
    // identity root alone, so the same record opened by the same app on another
    // device is the same document and the workspace carries it there.
    {
        let laptop = personal_device();
        watch(&laptop, "watched-on-the-laptop");
        let route = laptop
            .route()
            .expect("the person's own cluster carries their record");
        assert_eq!(route.app_id, APP);
        assert_eq!(
            route.cluster_id,
            own_workspace()
        );
        assert_eq!(route.document.as_deref(), Some(laptop.checkout().id.as_str()));

        // Another device of the person derives the very same route for the
        // record of that kind, without either device naming the other.
        let phone = personal_device();
        assert_eq!(phone.route(), Some(route.clone()));

        // The app publishes the record on that route, as any other.
        let mut app = app_holding(&laptop);
        app.publish_owned_documents();
        assert!(!app.document_routes.sent.is_empty());
        assert!(
            app.document_routes
                .sent
                .iter()
                .all(|(sent, _)| sent == &route)
        );
    }

    phase a_device_that_was_away_catches_up_when_its_route_returns;
    {
        let laptop = personal_device();
        let phone = personal_device();
        let mut laptop_app = app_holding(&laptop);
        let mut phone_app = app_holding(&phone);
        laptop_app.publish_owned_documents();
        let (route, first) = snapshots(&mut laptop_app).into_iter().next().unwrap();
        phone_app.apply_document_event(&from_device(&route, first));
        phone_app.publish_owned_documents();
        phone_app.document_routes.sent.clear();

        // The phone is away while the laptop watches something: the change the
        // laptop publishes never reaches it.
        watch(&laptop, "while-away");
        laptop_app.publish_owned_documents();
        let lost = std::mem::take(&mut laptop_app.document_routes.sent);
        assert!(
            lost.iter().all(|(sent, request)| sent == &route
                && matches!(
                    request,
                    CollabRequest::Document {
                        mutation: collab_types::DocumentMutation::Incremental(_),
                        ..
                    }
                )),
            "an ordinary change goes out as a delta"
        );
        assert!(!watched(&phone).contains(&"while-away".to_owned()));

        // The phone's route returns: it reads what the store holds of the
        // record, which took the laptop's change, and, merging it, has
        // everything.
        phone_app.apply_document_event(&from_device(&route, laptop.document().unwrap()));
        assert_eq!(watched(&phone), watched(&laptop));
        assert!(watched(&phone).contains(&"while-away".to_owned()));
    }

    phase a_second_window_holding_an_in_memory_copy_hears_what_reaches_the_route;
    // The same app open in a second window of the device, while the first
    // holds the stored record: the second keeps an in-memory copy on the very
    // same route, so what the person's host replays there — and what the first
    // window publishes — reaches it too. A route is what lets a copy follow a
    // record, which the person's record has.
    {
        let port = Rc::new(crate::resource::MemoryWorkspace::default());
        let open = |port: &Rc<crate::resource::MemoryWorkspace>| {
            watched_in(ResourceOwners::writing(
                Rc::clone(port) as Rc<dyn crate::resource::DocumentPort>,
            ))
        };
        let first = open(&port);
        let second = open(&port);
        assert_eq!(
            first.route(),
            second.route(),
            "the in-memory copy names the stored record's route"
        );
        let route = second.route().unwrap();
        let mut second_app = app_holding(&second);

        // What the person's host kept from another device arrives on the route.
        let laptop = personal_device();
        watch(&laptop, "kept-by-the-host");
        second_app
            .apply_document_event(&from_device(&route, laptop.document().unwrap()));
        assert_eq!(watched(&second), ["kept-by-the-host"]);

        // What the first window publishes on the route reaches it as well.
        watch(&first, "in-the-first-window");
        second_app
            .apply_document_event(&from_device(&route, first.document().unwrap()));
        assert_eq!(
            watched(&second),
            ["in-the-first-window", "kept-by-the-host"]
        );
    }
}

/// What a window's transport was asked to send on a route: the document
/// writes (per route), and whether it was asked to join.
#[derive(Default)]
struct Recording {
    route: Option<::collaboration::RouteSpec>,
    /// The announced primary route, until it is reported once: a broker
    /// acknowledges the route it was joined with.
    acknowledged: bool,
    writes: Vec<::collaboration::RouteSpec>,
    joins: Vec<::collaboration::RouteSpec>,
}

struct RecordingTransport(Arc<Mutex<Recording>>);

impl ::collaboration::CollabTransport for RecordingTransport {
    fn status(&self) -> ::collaboration::TransportStatus {
        ::collaboration::TransportStatus::Ready
    }
    fn submit(
        &mut self,
        command: ::collaboration::TransportCommand,
    ) -> Result<(), ::collaboration::TransportError> {
        use ::collaboration::TransportCommand as Command;
        match command {
            Command::Primary(CollabRequest::Document { .. }) => {
                let route = self
                    .0
                    .lock()
                    .unwrap()
                    .route
                    .clone()
                    .expect("the primary route");
                self.0.lock().unwrap().writes.push(route);
            }
            Command::ToRoute(route, CollabRequest::Document { .. }) => {
                self.0.lock().unwrap().writes.push(route);
            }
            Command::Join(route, _) => self.0.lock().unwrap().joins.push(route),
            _ => {}
        }
        Ok(())
    }
    fn drain(&mut self, _: &mut Vec<::collaboration::CollabEvent>) {}
    fn take_primary_route_result(&mut self) -> Option<Result<::collaboration::RouteSpec, String>> {
        let mut recording = self.0.lock().unwrap();
        if recording.acknowledged {
            return None;
        }
        recording.acknowledged = true;
        recording.route.clone().map(Ok)
    }
    fn take_action_directory_requests(
        &mut self,
        _: &mut Vec<::collaboration::ActionDirectoryRequest>,
    ) {
    }
    // A transport whose routes all take documents, as the broker's do once
    // a route is joined: what is tested is what the window offers it.
    fn document_publishable(&self, _: Option<&str>) -> bool {
        true
    }
}

/// A window of `app` holding `record` beside the document it shows, on a
/// `Recording` transport that stands in for its broker.
#[cfg(unix)]
fn recording_window(app: &str, record: &StoredResource) -> (App, Arc<Mutex<Recording>>) {
    let primary = ::collaboration::RouteSpec::document(app, uuid::Uuid::new_v4(), "shown");
    let recording = Arc::new(Mutex::new(Recording {
        route: Some(primary.clone()),
        ..Recording::default()
    }));
    let transport: Box<dyn ::collaboration::CollabTransport> =
        Box::new(RecordingTransport(Arc::clone(&recording)));
    let mut manager = CollaborationManager::with_transport(transport);
    assert!(manager.send(CollabRequest::Hello {
        app_id: primary.app_id,
        cluster_id: primary.cluster_id,
        record: primary.document,
        resource_type: None,
        handle: None,
    }));
    manager.poll();
    let mut app = App::new_with_controlled_inputs(
        State::new(),
        Box::new(Holds {
            text: crate::widget::Text::new("shown"),
            owner: record.history_owner(),
        }),
    );
    app.collaboration.manager = manager;
    app.refresh_document_routes();
    app.publish_owned_documents();
    (app, recording)
}

tilo_test_env::scenario! {
    #[cfg(unix)]
    fn an_app_publishes_the_record_it_keeps;

    phase an_app_publishes_the_record_it_keeps_on_its_route;
    // The app that keeps a record of a kind publishes it on the route it syncs
    // on: the rule is about who may sign a change on a route, not about the
    // route.
    {
        let owned = personal_device();
        let route = owned.route().expect("a route in the workspace");
        let (_app, recording) = recording_window(APP, &owned);
        assert_eq!(
            recording.lock().unwrap().writes.clone(),
            vec![route],
            "the keeper's whole record goes out once on its route"
        );
    }
}
