//! Records a person keeps for themselves beside the one an app shows: kept in
//! their own workspace, edited through their actions with attribution, history
//! and undo, and apart from the record the app shows. A window in another
//! workspace holds none of them: it follows them there and its changes are
//! made there, by the app in the person's own workspace.

use tilo_app::prelude::*;
use tilo_app_test_host::TestApp;

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
    read_content = tilo_app::tilo_resource::read::fields,
    catalogue = "tests/locales"
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
    const PERSON_RESOURCES: &'static [PersonKind] = &[PersonKind::profile::<Watched>()];

    fn view(cx: &mut ViewCx<Queue>) -> Element {
        let watched = cx.profile_resource::<Watched>();
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

fn videos(record: &PersonResource<Watched>) -> Vec<String> {
    let mut videos: Vec<String> = record.with(|state| state.videos.values().cloned().collect());
    videos.sort();
    videos
}

tilo_test_env::scenario! {
    fn a_record_kept_beside_the_apps_own_is_the_persons;

    phase a_record_of_a_kind_survives_a_relaunch;
    {
        let mut app = TestApp::<QueueApp>::for_person(&ada()).unwrap();
        app.profile_resource::<Watched>()
            .unwrap()
            .invoke(&Watched::WATCH.with("kept"))
            .unwrap();
        app.settle();
        let mut app = app.reopen().unwrap();
        app.settle();
        assert_eq!(videos(&app.profile_resource::<Watched>().unwrap()), ["kept"]);
    }

    phase a_window_in_another_workspace_reads_the_genesis_until_the_record_answers;
    {
        let mut app = TestApp::<QueueApp>::for_person(&ada()).unwrap();
        let watched = app.profile_resource::<Watched>().unwrap();
        assert!(watched.record().is_none(), "the window holds none of it");
        assert!(videos(&watched).is_empty(), "the genesis, as nothing answered yet");
        app.in_persons_own_workspace(|own| {
            own.profile_resource::<Watched>()
                .unwrap()
                .invoke(&Watched::WATCH.with("from-their-own"))
                .unwrap();
        })
        .expect("the app runs in the person's own workspace");
        app.settle();
        assert_eq!(videos(&watched), ["from-their-own"]);
    }

    phase the_palette_and_agents_take_back_the_persons_own_steps;
    {
        let mut app = TestApp::<QueueApp>::for_person(&ada()).unwrap();
        let watched = app.profile_resource::<Watched>().unwrap();
        watched.invoke(&Watched::WATCH.with("first")).unwrap();
        watched.invoke(&Watched::WATCH.with("second")).unwrap();
        app.settle();
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
        app.settle();
        assert!(videos(&watched).is_empty());
        app.call_on("person:test.watched", "redo", serde_json::json!({}))
            .unwrap();
        app.settle();
        assert_eq!(videos(&watched), ["first"]);

        // The queue's history is another record's: its undo has nothing of
        // the watch history to take back, and that nothing of the queue's.
        app.invoke(Queue::ADD.with("shared")).unwrap();
        app.call_on("person:test.watched", "undo", serde_json::json!({}))
            .unwrap();
        app.settle();
        assert!(videos(&watched).is_empty());
        assert_eq!(app.state().items, ["shared"]);
        assert!(
            app.call_on("person:test.watched", "undo", serde_json::json!({}))
                .is_err(),
            "nothing more of the person's to undo"
        );
    }

    phase a_change_from_a_window_in_another_workspace_lands_in_the_persons_own;
    {
        let mut app = TestApp::<QueueApp>::for_person(&ada()).unwrap();
        let owned = app.profile_resource::<Watched>().unwrap();
        owned.invoke(&Watched::WATCH.with("saved-in-their-own")).unwrap();
        app.settle();

        let kept = app
            .in_persons_own_workspace(|own| {
                let kept = own.profile_resource::<Watched>().unwrap();
                (videos(&kept), kept.record().map(|record| record.heads()))
            })
            .expect("the app runs in the person's own workspace");
        assert_eq!(kept.0, ["saved-in-their-own"], "the edit is made where the record is kept");
        assert_eq!(
            kept.1,
            owned.read(|doc| {
                let mut heads = doc.clone().get_heads();
                heads.sort_unstable();
                heads
            }),
            "and the window reads that copy"
        );

        // An agent of hers changes it only in a window of her own workspace: this one's
        // connection is hers alone.
        let agent = tilo_app::tilo_actions::ActionInvoker::agent("scout", ada()).acting_for(ada());
        assert!(
            owned
                .invoke_as(&agent, &Watched::WATCH.with("by-the-agent"))
                .is_err()
        );
    }

    phase a_change_to_the_persons_record_from_another_device_wakes_the_window_reading_it;
    {
        let mut app = TestApp::<ShowingApp>::for_person(&ada()).unwrap();
        app.settle();
        assert!(shows(&mut app, "0 watched"), "nothing is watched yet: {}", shown(&mut app));

        // Their other device watches one: its copy of the record arrives in their own
        // workspace, which carries it between their devices. Nothing else tells this
        // window, which has been idle since it opened.
        app.in_persons_own_workspace(|own| {
            let kept = own.profile_resource::<Watched>().unwrap();
            let record = kept.record().unwrap();
            let mut theirs = Replica::<Watched>::load(&record.document().unwrap())
                .unwrap()
                .in_app("so.tilo.model", "Model");
            theirs.apply(Watched::WATCH.with("from-their-phone")).unwrap();
            record.merge_peer_document(theirs.save()).unwrap();
        })
        .expect("the app runs in the person's own workspace");

        assert!(
            app.frame_if_owed(),
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

        // The declared record is open and published with the window, whether or not a view
        // shows it.
        let mut app = TestApp::<DeclaringApp>::for_person(&ada()).unwrap();
        app.harness().relayout();
        let published = app
            .agent_surface()
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["resource"] == "person:test.watched" && row["action"] == "watch");
        assert!(published, "the declared record is published");
        assert!(app.profile_resource::<Watched>().is_some());
    }

    phase without_an_identity_the_persons_record_opens_nothing_and_says_why;
    {
        // A device nobody signed in on: the person has no workspace of their own to keep it in.
        let mut app = TestApp::<DeclaringApp>::new().unwrap();
        assert!(app.profile_resource::<Watched>().is_none(), "nothing is opened");
        assert!(app.in_persons_own_workspace(|_| ()).is_none());
        app.harness().relayout();
        let reason = app
            .actions()
            .unwrap()
            .into_iter()
            .find(|entry| {
                entry.address.resource == "person:test.watched" && entry.address.action == "watch"
            })
            .map(|entry| match entry.contract.availability {
                tilo_app::tilo_actions::Availability::Unavailable { reason, .. } => reason,
                tilo_app::tilo_actions::Availability::Available => String::new(),
            })
            .expect("the declared record is published");
        assert!(reason.contains("Sign in"), "{reason}");
    }

    phase an_agent_reads_the_persons_own_record_now_and_at_a_version;
    {
        let mut app = TestApp::<QueueApp>::for_person(&ada()).unwrap();
        let watched = app.profile_resource::<Watched>().unwrap();
        watched.invoke(&Watched::WATCH.with("first")).unwrap();
        app.settle();
        let after_first = watched
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
        app.settle();
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
fn shows(app: &mut TestApp<ShowingApp>, label: &str) -> bool {
    app.harness()
        .a11y_nodes()
        .iter()
        .any(|(_, node)| node.label() == Some(label))
}

/// What the window the app shows announces, for a failure to say.
fn shown(app: &mut TestApp<ShowingApp>) -> String {
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

/// An app showing the person's watch history, which it declares in its records.
struct ShowingApp;

impl ResourceApp for ShowingApp {
    type Resource = Queue;
    const ID: &'static str = "so.tilo.test-showing";
    const TITLE: &'static str = "Showing";

    fn resources(resources: &mut Resources<'_>) {
        resources.profile_resource::<Watched>();
    }

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
