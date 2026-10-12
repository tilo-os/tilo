#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! A resource of the person's own, kept beside the record an app shows:
//! edited through its actions with attribution, history and undo, the same
//! record on each of the person's devices, and never part of the room a
//! collaborator shares.

use tilo_app::prelude::*;

/// A queue in a shared room.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Queue {
    pub items: Vec<String>,
}

#[resource(kind = "test.queue", label = "Queue")]
impl Queue {
    /// Queue a video.
    #[action]
    pub fn add(&mut self, video: String) {
        self.items.push(video);
    }
}

/// What the person watched: theirs alone.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Watched {
    pub videos: Collection<String>,
}

#[resource(kind = "test.watched", label = "Watch history")]
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

fn bob() -> String {
    "b0".repeat(32)
}

/// The person room of `person`.
fn room_of(person: &str) -> String {
    tilo_app::record::person::scope_of(&tilo_app::permissions::UserId(person.to_owned()))
}

fn videos(record: &PersonRecord<Watched>) -> Vec<String> {
    let mut videos: Vec<String> = record.with(|state| state.videos.values().cloned().collect());
    videos.sort();
    videos
}

#[test]
fn a_persons_record_is_the_same_record_on_each_of_their_devices() {
    let mut laptop = TestApp::<QueueApp>::for_person(&ada()).unwrap();
    let mut phone = TestApp::<QueueApp>::for_person(&ada()).unwrap();
    let on_laptop = laptop.person::<Watched>().unwrap();
    let on_phone = phone.person::<Watched>().unwrap();
    assert!(on_laptop.syncs(), "Ada has a root identity, so a room");
    let route = on_laptop.record().route().unwrap();
    assert_eq!(route, on_phone.record().route().unwrap());
    assert_eq!(route.app_id, QueueApp::ID);
    assert_eq!(
        route.cluster_id.to_string(),
        room_of(&ada()),
        "the record lives in Ada's own room"
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

    // It is not the room's record: the queue the app shows holds nothing
    // of it.
    let queue = laptop.peer_document();
    assert!(!format!("{queue:?}").contains("on-the-laptop"));
}

#[test]
fn a_collaborator_in_the_same_room_never_holds_the_persons_record() {
    let mut ada_app = TestApp::<QueueApp>::for_person(&ada()).unwrap();
    let mut bob_app = TestApp::<QueueApp>::for_person(&bob()).unwrap();
    let adas = ada_app.person::<Watched>().unwrap();
    adas.invoke(&Watched::WATCH.with("private")).unwrap();

    // They share the queue: Bob merges what Ada queued, and nothing of what
    // she watched comes with it.
    ada_app.invoke(Queue::ADD.with("shared")).unwrap();
    let mut shared = ada_app.peer_document();
    bob_app.merge_peer(&mut shared).unwrap();
    assert_eq!(bob_app.state().items, ["shared"]);
    let saved = shared.save();
    assert!(
        !saved
            .windows(b"private".len())
            .any(|window| window == b"private"),
        "the shared record carries none of Ada's history"
    );

    // Bob's own history is a record in his own room, on another route:
    // nothing routes Ada's to him.
    let bobs = bob_app.person::<Watched>().unwrap();
    assert!(bobs.with(|state| state.videos.is_empty()));
    let (ada_route, bob_route) = (
        adas.record().route().unwrap(),
        bobs.record().route().unwrap(),
    );
    assert_eq!(
        ada_route.record, bob_route.record,
        "the same kind of record"
    );
    assert_ne!(
        ada_route.cluster_id, bob_route.cluster_id,
        "in each person's own room"
    );

    // Even handed Ada's bytes, Bob's record refuses them: they belong to
    // another room.
    let adas_bytes = adas.record().document().unwrap();
    assert!(bobs.record().merge_peer_document(adas_bytes).is_err());
    assert!(bobs.with(|state| state.videos.is_empty()));
}

#[test]
fn a_persons_record_survives_a_relaunch() {
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

    // The shared queue's history is not the person's: its undo has nothing
    // of theirs to take back, and theirs nothing of the queue's.
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

/// The queue app keeps the person's watch history for all of their apps;
/// the editor app adds to it too.
impl tilo_app::tilo_resource::SharedRecord for Watched {
    const OWNER: &'static str = QueueApp::ID;
    const EDITORS: &'static [&'static str] = &[EditorApp::ID];
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
    let owned = owner_app.shared_person::<Watched>().unwrap();
    let edited = editor_app.shared_person::<Watched>().unwrap();
    let read = reader_app.shared_person::<Watched>().unwrap();
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
        .invoke(&Watched::WATCH.with("kept-by-the-owner"))
        .unwrap();
    edited
        .invoke(&Watched::WATCH.with("added-by-the-editor"))
        .unwrap();
    owned
        .record()
        .merge_peer_document(edited.record().document().unwrap())
        .unwrap();
    read.record()
        .merge_peer_document(owned.record().document().unwrap())
        .unwrap();
    assert_eq!(videos(&read), ["added-by-the-editor", "kept-by-the-owner"]);

    assert!(
        read.invoke(&Watched::WATCH.with("from-the-reader"))
            .is_err(),
        "a reader does not change it"
    );
    assert_eq!(videos(&read), ["added-by-the-editor", "kept-by-the-owner"]);

    // The editor's copy is kept in its own store across launches.
    drop(edited);
    let mut editor_app = editor_app.reopen().unwrap();
    let kept = editor_app.shared_person::<Watched>().unwrap();
    assert_eq!(videos(&kept), ["added-by-the-editor"]);
}
