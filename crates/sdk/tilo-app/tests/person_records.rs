//! A record of a kind an app keeps beside the one it shows, in the
//! workspace it is open in: edited through its actions with attribution,
//! history and undo, the same record on each device that holds the
//! workspace, and apart from the record the app shows.

use tilo_app::prelude::*;

/// A queue in a shared room.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Queue {
    pub items: Vec<String>,
}

#[resource(kind = "test.queue", label = "Queue", app = "so.tilo.test-queue")]
impl Queue {
    /// Queue a video.
    #[action]
    pub fn add(&mut self, video: String) {
        self.items.push(video);
    }
}

/// What was watched in the workspace.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Watched {
    pub videos: Collection<String>,
}

#[resource(
    kind = "test.watched",
    label = "Watch history",
    app = "so.tilo.test-queue"
)]
impl Watched {
    /// Remember that a video was watched.
    #[action]
    pub fn watch(&mut self, cx: &mut ActionCx, video: String) {
        self.videos.push(cx.id(), video);
    }
}

struct QueueApp;

impl ResourceApp for QueueApp {
    type Resource = Queue;
    const ID: &'static str = "so.tilo.test-queue";
    const TITLE: &'static str = "Queue";

    fn view(cx: &mut ViewCx<Queue>) -> Element {
        let watched = cx.person::<Watched>();
        let shown = watched.map_or_else(
            || "no history".to_owned(),
            |watched| format!("{} watched", watched.with(|state| state.videos.len())),
        );
        Box::new(Column::new().push(Box::new(Text::new(&shown))))
    }
}

/// Ada, by her root public key.
fn ada() -> String {
    "ad".repeat(32)
}

/// The workspace the tests open the app in.
const ROOM: &str = "00000000-0000-4000-8000-00000000000a";

/// The id of `person`'s own watch history in a workspace.
fn watched_record_of(person: &str) -> String {
    format!("member:{person}:test.watched")
}

fn videos(record: &PersonRecord<Watched>) -> Vec<String> {
    let mut videos: Vec<String> = record.with(|state| state.videos.values().cloned().collect());
    videos.sort();
    videos
}

#[test]
fn a_record_of_a_kind_is_the_same_record_on_each_device_of_the_workspace() {
    let mut laptop = TestApp::<QueueApp>::for_person_in(&ada(), ROOM).unwrap();
    let mut phone = TestApp::<QueueApp>::for_person_in(&ada(), ROOM).unwrap();
    let on_laptop = laptop.person::<Watched>().unwrap();
    let on_phone = phone.person::<Watched>().unwrap();
    assert!(on_laptop.syncs(), "the workspace has a room id");
    let route = on_laptop.record().route().unwrap();
    assert_eq!(route, on_phone.record().route().unwrap());
    assert_eq!(route.app_id, QueueApp::ID);
    assert_eq!(
        route.cluster_id.to_string(),
        ROOM,
        "the record lives in the workspace the app is open in"
    );
    assert_eq!(
        route.record.as_deref(),
        Some(watched_record_of(&ada()).as_str()),
        "it is Ada's own record there: no other member reads or writes it"
    );

    // Each device watches something before they meet; the record's genesis
    // is the same on both, so they merge into one history.
    on_laptop
        .apply(&Watched::WATCH.with("on-the-laptop").build(0).unwrap())
        .unwrap();
    on_phone
        .invoke(&Watched::WATCH.with("on-the-phone"))
        .unwrap();
    let laptops = on_laptop.record().document().unwrap();
    on_phone.record().merge_peer_document(laptops).unwrap();
    let phones = on_phone.record().document().unwrap();
    on_laptop.record().merge_peer_document(phones).unwrap();
    assert_eq!(videos(&on_laptop), ["on-the-laptop", "on-the-phone"]);
    assert_eq!(videos(&on_laptop), videos(&on_phone));

    // The edit is an action of Ada's in the record's history, and her undo
    // takes back her own step on this device.
    let version = on_laptop
        .record()
        .read(|doc| {
            tilo_app::tilo_resource::history::versions(&mut doc.clone())
                .into_iter()
                .filter_map(|version| version.context)
                .find(|context| context.invoker.is_some())
        })
        .flatten()
        .expect("the watch is a version of the record");
    assert_eq!(version.invoker.unwrap().actor, ada());
    assert!(on_laptop.undo());
    assert_eq!(videos(&on_laptop), ["on-the-phone"]);
    assert!(on_laptop.redo());

    // It is a record of its own: the queue the app shows holds nothing of
    // it.
    let queue = laptop.peer_document();
    assert!(!format!("{queue:?}").contains("on-the-laptop"));
}

#[test]
fn each_workspace_has_its_own_record_of_a_kind() {
    let mut here = TestApp::<QueueApp>::for_person_in(&ada(), ROOM).unwrap();
    let mut elsewhere =
        TestApp::<QueueApp>::for_person_in(&ada(), "00000000-0000-4000-8000-00000000000b").unwrap();
    let watched_here = here.person::<Watched>().unwrap();
    watched_here.invoke(&Watched::WATCH.with("here")).unwrap();

    // The app opened in another workspace starts empty, on another route:
    // nothing routes this workspace's record there.
    let watched_elsewhere = elsewhere.person::<Watched>().unwrap();
    assert!(watched_elsewhere.with(|state| state.videos.is_empty()));
    let (route_here, route_elsewhere) = (
        watched_here.record().route().unwrap(),
        watched_elsewhere.record().route().unwrap(),
    );
    assert_eq!(
        route_here.record, route_elsewhere.record,
        "the same kind of record"
    );
    assert_ne!(
        route_here.cluster_id, route_elsewhere.cluster_id,
        "in each workspace"
    );

    // Even handed this workspace's bytes, the other refuses them: they
    // belong to another workspace.
    let bytes = watched_here.record().document().unwrap();
    assert!(
        watched_elsewhere
            .record()
            .merge_peer_document(bytes)
            .is_err()
    );
    assert!(watched_elsewhere.with(|state| state.videos.is_empty()));
}

#[test]
fn a_record_of_a_kind_survives_a_relaunch() {
    let mut app = TestApp::<QueueApp>::for_person(&ada()).unwrap();
    app.person::<Watched>()
        .unwrap()
        .invoke(&Watched::WATCH.with("kept"))
        .unwrap();
    let mut app = app.reopen().unwrap();
    assert_eq!(videos(&app.person::<Watched>().unwrap()), ["kept"]);
}

#[test]
fn the_palette_and_agents_take_back_the_persons_own_steps() {
    let mut app = TestApp::<QueueApp>::for_person(&ada()).unwrap();
    let watched = app.person::<Watched>().unwrap();
    watched.invoke(&Watched::WATCH.with("first")).unwrap();
    watched.invoke(&Watched::WATCH.with("second")).unwrap();
    app.harness().relayout();

    // The record is published beside the app's resource, with its verbs
    // and field edits and the person's undo and redo.
    let surface = app.agent_surface();
    let actions: Vec<&str> = surface
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["resource"] == "person:test.watched")
        .map(|row| row["action"].as_str().unwrap())
        .collect();
    for expected in ["redo", "undo", "watch", "field-edit"] {
        assert!(actions.contains(&expected), "{expected} in {actions:?}");
    }

    let undone = app
        .call_on(
            "person:test.watched",
            "undo",
            serde_json::json!({ "steps": 2 }),
        )
        .unwrap();
    assert_eq!(undone, Some(serde_json::json!({ "steps": 2 })));
    assert!(videos(&watched).is_empty());
    app.call_on("person:test.watched", "redo", serde_json::json!({}))
        .unwrap();
    assert_eq!(videos(&watched), ["first"]);

    // The queue's history is another record's: its undo has nothing of
    // the watch history to take back, and that nothing of the queue's.
    app.invoke(Queue::ADD.with("shared")).unwrap();
    app.call_on("person:test.watched", "undo", serde_json::json!({}))
        .unwrap();
    assert!(videos(&watched).is_empty());
    assert_eq!(app.state().items, ["shared"]);
    assert!(
        app.call_on("person:test.watched", "undo", serde_json::json!({}))
            .is_err(),
        "nothing more of the person's to undo"
    );
}

/// What the person saved for later, which the queue app keeps for all of
/// their apps; the editor app adds to it too.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Saved {
    pub videos: Collection<String>,
}

#[resource(kind = "test.saved", label = "Saved", app = "so.tilo.test-queue")]
impl Saved {
    /// Save a video for later.
    #[action]
    pub fn save(&mut self, cx: &mut ActionCx, video: String) {
        self.videos.push(cx.id(), video);
    }
}

impl tilo_app::tilo_resource::SharedRecord for Saved {
    const OWNER: &'static str = QueueApp::ID;
    const EDITORS: &'static [&'static str] = &[EditorApp::ID];
}

fn saved(record: &PersonRecord<Saved>) -> Vec<String> {
    let mut videos: Vec<String> = record.with(|state| state.videos.values().cloned().collect());
    videos.sort();
    videos
}

#[test]
fn an_owner_shown_by_a_host_keeping_the_persons_records_changes_the_hosts_copy() {
    use std::rc::Rc;
    use tilo_app::record::{RecordKind, RecordOwners, person::PersonRecords};

    let mut owners = RecordOwners::with_store(Rc::new(durable_store::MemoryStore::new()));
    let held = PersonRecords::open(
        &mut owners,
        QueueApp::ID,
        tilo_app::permissions::UserId(ada()),
    )
    .unwrap();
    let mut app = TestApp::<QueueApp>::for_person_shown_by(&ada(), &held).unwrap();
    let owned = app.shared_person::<Saved>().unwrap();
    owned.invoke(&Saved::SAVE.with("saved-in-the-app")).unwrap();

    let kept = held
        .open_existing_record(RecordKind::new(Saved::KIND))
        .unwrap()
        .expect("the host keeps the record the app changed");
    assert_eq!(
        kept.heads(),
        owned.record().heads(),
        "the app's edit is in the copy the host stores and syncs"
    );
}

/// Another app, adding to the history the queue app keeps.
struct EditorApp;

impl ResourceApp for EditorApp {
    type Resource = Queue;
    const ID: &'static str = "so.tilo.test-editor";
    const TITLE: &'static str = "Editor";

    fn view(_cx: &mut ViewCx<Queue>) -> Element {
        Box::new(Column::new())
    }
}

/// Another app, reading the history the queue app keeps.
struct ReaderApp;

impl ResourceApp for ReaderApp {
    type Resource = Queue;
    const ID: &'static str = "so.tilo.test-reader";
    const TITLE: &'static str = "Reader";

    fn view(_cx: &mut ViewCx<Queue>) -> Element {
        Box::new(Column::new())
    }
}

#[test]
fn a_shared_record_is_one_record_kept_by_each_app_and_changed_by_its_owner_and_editors() {
    let mut owner_app = TestApp::<QueueApp>::for_person(&ada()).unwrap();
    let mut editor_app = TestApp::<EditorApp>::for_person(&ada()).unwrap();
    let mut reader_app = TestApp::<ReaderApp>::for_person(&ada()).unwrap();
    let owned = owner_app.shared_person::<Saved>().unwrap();
    let edited = editor_app.shared_person::<Saved>().unwrap();
    let read = reader_app.shared_person::<Saved>().unwrap();
    assert!(owned.editable() && edited.editable());
    assert!(!read.editable());
    for copy in [&edited, &read] {
        assert_eq!(
            copy.record().route().unwrap(),
            owned.record().route().unwrap(),
            "every app's copy is the owner's record"
        );
    }

    owned
        .invoke(&Saved::SAVE.with("kept-by-the-owner"))
        .unwrap();
    edited
        .invoke(&Saved::SAVE.with("added-by-the-editor"))
        .unwrap();
    owned
        .record()
        .merge_peer_document(edited.record().document().unwrap())
        .unwrap();
    read.record()
        .merge_peer_document(owned.record().document().unwrap())
        .unwrap();
    assert_eq!(saved(&read), ["added-by-the-editor", "kept-by-the-owner"]);

    assert!(
        read.invoke(&Saved::SAVE.with("from-the-reader")).is_err(),
        "a reader does not change it"
    );
    assert_eq!(saved(&read), ["added-by-the-editor", "kept-by-the-owner"]);

    // The editor's copy is kept in its own store across launches.
    drop(edited);
    let mut editor_app = editor_app.reopen().unwrap();
    let kept = editor_app.shared_person::<Saved>().unwrap();
    assert_eq!(saved(&kept), ["added-by-the-editor"]);
}
