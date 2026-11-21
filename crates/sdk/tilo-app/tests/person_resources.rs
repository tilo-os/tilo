//! Records a person keeps for themselves beside the one an app shows: edited
//! through their actions with attribution, history and undo, the same on each
//! of their devices, and apart from the record the app shows.

use tilo_app::prelude::*;

/// A queue in a shared workspace.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Queue {
    pub items: Vec<String>,
}

#[resource(
    kind = "test.queue",
    app = "so.tilo.test-queue",
    catalogue = "tests/locales"
)]
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
    app = "so.tilo.test-queue",
    read_content = Watched::read_document,
    catalogue = "tests/locales"
)]
impl Watched {
    /// Remember that a video was watched.
    #[action]
    pub fn watch(&mut self, cx: &mut ActionCx, video: String) {
        self.videos.push(cx.id(), video);
    }
}

impl Watched {
    /// What was watched, as its fields.
    pub fn read_document(
        &self,
        item: Option<&tilo_app::tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_app::tilo_resource::read::fields(self, item)
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
const WORKSPACE: &str = "00000000-0000-4000-8000-00000000000a";

/// The id of `person`'s own watch history in a workspace.
fn watched_resource_of(person: &str) -> String {
    collab_types::OwnResource::of_person(person, "test.watched")
        .map_or_else(String::new, |own| own.document_id())
}

fn videos(record: &PersonResource<Watched>) -> Vec<String> {
    let mut videos: Vec<String> = record.with(|state| state.videos.values().cloned().collect());
    videos.sort();
    videos
}

tilo_test_env::scenario! {
    fn a_record_kept_beside_the_apps_own_is_one_record_in_each_workspace;

    phase a_record_of_a_kind_is_the_same_record_on_each_device_of_the_workspace;
    {
        crate::own_kinds::declare("so.tilo.test-queue", &["test.watched"]);
        let mut laptop = TestApp::<QueueApp>::for_person_in(&ada(), WORKSPACE).unwrap();
        let mut phone = TestApp::<QueueApp>::for_person_in(&ada(), WORKSPACE).unwrap();
        let on_laptop = laptop.person::<Watched>().unwrap();
        let on_phone = phone.person::<Watched>().unwrap();
        assert!(on_laptop.syncs(), "the workspace has a workspace id");
        let route = on_laptop.record().route().unwrap();
        assert_eq!(route, on_phone.record().route().unwrap());
        assert_eq!(route.app_id, QueueApp::ID);
        assert_eq!(
            route.cluster_id.to_string(),
            WORKSPACE,
            "the record lives in the workspace the app is open in"
        );
        assert_eq!(
            route.document.as_deref(),
            Some(watched_resource_of(&ada()).as_str()),
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
        assert_eq!(version.invoker.unwrap().acting.by, MemberId::person(ada()).into());
        assert!(on_laptop.undo());
        assert_eq!(videos(&on_laptop), ["on-the-phone"]);
        assert!(on_laptop.redo());

        // It is a record of its own: the queue the app shows holds nothing of
        // it.
        let queue = laptop.peer_document();
        assert!(!format!("{queue:?}").contains("on-the-laptop"));
    }

    phase each_workspace_has_its_own_record_of_a_kind;
    {
        let mut here = TestApp::<QueueApp>::for_person_in(&ada(), WORKSPACE).unwrap();
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
            route_here.document, route_elsewhere.document,
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

    phase a_record_of_a_kind_survives_a_relaunch;
    {
        let mut app = TestApp::<QueueApp>::for_person(&ada()).unwrap();
        app.person::<Watched>()
            .unwrap()
            .invoke(&Watched::WATCH.with("kept"))
            .unwrap();
        let mut app = app.reopen().unwrap();
        assert_eq!(videos(&app.person::<Watched>().unwrap()), ["kept"]);
    }

    phase the_palette_and_agents_take_back_the_persons_own_steps;
    {
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

    phase an_owner_shown_by_a_host_keeping_the_persons_records_changes_the_hosts_copy;
    {
        use std::rc::Rc;
        use tilo_app::resource::{ResourceType, ResourceOwners, person::PersonResources};

        let mut owners = ResourceOwners::with_store(Rc::new(durable_store::MemoryStore::new()));
        let held = PersonResources::open(
            &mut owners,
            QueueApp::ID,
            tilo_app::permissions::MemberId::person(ada()),
        )
        .unwrap();
        let mut app = TestApp::<QueueApp>::for_person_shown_by(&ada(), &held).unwrap();
        let owned = app.profile_resource::<Watched>().unwrap();
        owned.invoke(&Watched::WATCH.with("saved-in-the-app")).unwrap();

        let kept = held
            .open_existing_resource(ResourceType::new(Watched::KIND))
            .unwrap()
            .expect("the host keeps the record the app changed");
        assert_eq!(
            kept.heads(),
            owned.record().heads(),
            "the app's edit is in the copy the host stores and syncs"
        );
    }

    phase a_change_to_the_persons_record_from_another_device_wakes_the_window_reading_it;
    {
        let mut app = TestApp::<UndeclaredApp>::for_person(&ada()).unwrap();
        app.settle();
        let read = app.profile_resource::<Watched>().unwrap();
        assert!(shows(&mut app, "0 watched"), "nothing is watched yet: {}", shown(&mut app));

        // Their other device watches one: its copy of the record arrives here,
        // as the person's own store carries it between their devices. Nothing
        // else tells this window, which has been idle since it opened.
        let mut theirs = Replica::<Watched>::load(&read.record().document().unwrap()).unwrap();
        theirs.apply(Watched::WATCH.with("from-their-phone")).unwrap();
        read.record()
            .merge_peer_document(theirs.save())
            .unwrap();

        assert!(
            app.frame(),
            "the window asks for a frame when the record changes under it"
        );
        assert!(
            shows(&mut app, "1 watched"),
            "the frame the arrival asked for shows it: {}",
            shown(&mut app)
        );
    }

    phase the_person_records_an_app_declares_are_its_surface_before_a_view_opens_them;
    {
        let report = tilo_app::framework::headless::surface_report::<DeclaringApp>();
        let resources = report["resources"].as_array().expect("resources");
        let declared = resources
            .iter()
            .find(|resource| resource["kind"] == Watched::KIND)
            .unwrap_or_else(|| {
                panic!(
                    "{} is part of the app's surface: {resources:?}",
                    Watched::KIND
                )
            });
        assert_eq!(declared["person"], true, "{declared:?}");

        // The declared record is open with the window, whether or not a view
        // shows it.
        let mut app = TestApp::<DeclaringApp>::for_person(&ada()).unwrap();
        assert_eq!(app.open_person_resources(), 1);
        assert!(app.profile_resource::<Watched>().is_some());
    }

    phase an_agent_reads_the_persons_own_record_now_and_at_a_version;
    {
        let mut app = TestApp::<QueueApp>::for_person(&ada()).unwrap();
        let watched = app.person::<Watched>().unwrap();
        watched.invoke(&Watched::WATCH.with("first")).unwrap();
        let after_first = watched
            .record()
            .read(|doc| {
                let heads = doc.clone().get_heads();
                heads
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("+")
            })
            .unwrap();
        watched.invoke(&Watched::WATCH.with("second")).unwrap();
        app.harness().relayout();

        let read = app
            .call_on("person:test.watched", "read", serde_json::json!({}))
            .unwrap()
            .expect("a read answers what the record holds");
        let now = read.to_string();
        assert!(
            now.contains("first") && now.contains("second"),
            "the read holds both: {now}"
        );

        let past = app
            .call_on(
                "person:test.watched",
                "read",
                serde_json::json!({ "version": after_first }),
            )
            .unwrap()
            .expect("a read of a version answers what it held");
        let then = past["content"].to_string();
        assert!(
            then.contains("first") && !then.contains("second"),
            "the version holds the first only: {past}"
        );
        assert_eq!(
            videos(&watched),
            ["first", "second"],
            "reads change nothing"
        );
    }
}

/// Whether the window the app shows names `label` to assistive technology.
fn shows(app: &mut TestApp<UndeclaredApp>, label: &str) -> bool {
    app.harness()
        .a11y_nodes()
        .iter()
        .any(|(_, node)| node.label() == Some(label))
}

/// What the window the app shows announces, for a failure to say.
fn shown(app: &mut TestApp<UndeclaredApp>) -> String {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .filter_map(|(_, node)| node.label().map(str::to_owned))
        .collect::<Vec<_>>()
        .join(" | ")
}

/// An app that declares the person's watch history with itself and shows
/// nothing of it.
struct DeclaringApp;

impl ResourceApp for DeclaringApp {
    type Resource = Queue;
    const ID: &'static str = "so.tilo.test-declaring";
    const TITLE: &'static str = "Declaring";
    const PERSON_RESOURCES: &'static [PersonKind] = &[PersonKind::profile::<Watched>()];

    fn view(_cx: &mut ViewCx<Queue>) -> Element {
        Box::new(Column::new())
    }
}

/// An app reading the person's watch history without declaring it.
struct UndeclaredApp;

impl ResourceApp for UndeclaredApp {
    type Resource = Queue;
    const ID: &'static str = "so.tilo.test-undeclared";
    const TITLE: &'static str = "Undeclared";

    fn view(cx: &mut ViewCx<Queue>) -> Element {
        let shown = match cx.profile_resource::<Watched>() {
            Some(watched) => Text::from_state(&watched.state(), |state| {
                format!("{} watched", state.videos.len())
            }),
            None => Text::new("none watched"),
        };
        Box::new(Column::new().push(Box::new(shown)))
    }
}
