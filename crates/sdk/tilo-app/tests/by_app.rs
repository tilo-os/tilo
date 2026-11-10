#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]
//! What an app keeps for itself in a record beside its own
//! (`#[action(by_app)]`): its own code applies it, as whoever it names, and
//! the record's door offers it to nobody and takes it from nobody.

use tilo_app::prelude::*;

/// A player: what it plays now.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Player {
    pub playing: String,
}

#[resource(kind = "test.jukebox", label = "Player", app = "so.tilo.test-jukebox")]
impl Player {
    /// Play a video.
    #[action]
    pub fn play(&mut self, video: String) {
        self.playing = video;
    }
}

/// What was played, which the player notes itself, and what the person
/// starred.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Played {
    pub noted: Vec<String>,
    pub starred: Vec<String>,
}

#[resource(
    kind = "test.jukebox-played",
    label = "Played",
    app = "so.tilo.test-jukebox"
)]
impl Played {
    /// Note that a video was played.
    #[action(housekeeping, by_app)]
    pub fn note(&mut self, video: String) {
        self.noted.push(video);
    }

    /// Star a video.
    #[action]
    pub fn star(&mut self, video: String) {
        self.starred.push(video);
    }
}

struct PlayerApp;

impl ResourceApp for PlayerApp {
    type Resource = Player;
    const ID: &'static str = "so.tilo.test-jukebox";
    const TITLE: &'static str = "Player";

    fn view(cx: &mut ViewCx<Player>) -> Element {
        let played = cx.person::<Played>();
        let shown = played.map_or_else(
            || "no history".to_owned(),
            |played| format!("{} played", played.with(|state| state.noted.len())),
        );
        Box::new(Column::new().push(Box::new(Text::new(&shown))))
    }
}

/// The record as the window publishes it.
const RECORD: &str = "person:test.jukebox-played";

/// Ada, by her root public key.
fn ada() -> String {
    "ad".repeat(32)
}

/// An agent the host admitted for Ada.
fn her_agent() -> tilo_actions::ActionInvoker {
    tilo_actions::ActionInvoker::admitted(
        &tilo_actions::InteractionActor::new("agent:eve", tilo_actions::ActorKind::Agent)
            .acting_for(ada()),
    )
}

/// The actions the door offers on the record.
fn offered(app: &mut TestApp<PlayerApp>) -> Vec<String> {
    app.agent_surface()
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["resource"] == RECORD)
        .map(|row| row["action"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn the_door_offers_a_by_app_verb_to_nobody_and_takes_it_from_nobody() {
    let mut app = TestApp::<PlayerApp>::for_person(&ada()).unwrap();
    let played = app.person::<Played>().unwrap();
    app.harness().relayout();

    let offered = offered(&mut app);
    for open in ["star", "apply", "group"] {
        assert!(
            offered.iter().any(|action| action == open),
            "{open} in {offered:?}"
        );
    }
    assert!(
        !offered.iter().any(|action| action == "note"),
        "{offered:?}"
    );

    // By its name, as a value of `apply` and as a member of a group, from
    // the person at the door and from an agent admitted for them.
    let forged = Played::NOTE.with("forged").build(0).unwrap();
    let value = tilo_app::tilo_resource::Invoke::<Played>::apply(&forged).unwrap();
    let value = serde_json::Value::Object(value.arguments);
    let named = serde_json::json!({"video": "forged"});
    let grouped = serde_json::json!({"actions": [
        {"action": "star", "args": {"video": "beside"}},
        {"action": "note", "args": {"video": "forged"}},
    ]});
    for (action, arguments) in [("note", &named), ("apply", &value), ("group", &grouped)] {
        let by_person = app.call_on(RECORD, action, arguments.clone());
        assert!(
            by_person.is_err(),
            "the person's {action} is refused: {by_person:?}"
        );
        let by_agent = app.call_by(her_agent(), RECORD, action, arguments.clone());
        assert!(
            by_agent.is_err(),
            "an agent's {action} is refused: {:?}",
            by_agent.as_ref().ok().map(|output| &output.value)
        );
    }
    assert!(played.with(|state| state.noted.is_empty() && state.starred.is_empty()));

    // The same door takes what the record declares, from both.
    app.call_on(RECORD, "star", serde_json::json!({"video": "by ada"}))
        .unwrap();
    app.call_by(
        her_agent(),
        RECORD,
        "star",
        serde_json::json!({"video": "by her agent"}),
    )
    .unwrap();
    assert_eq!(
        played.with(|state| state.starred.clone()),
        ["by ada", "by her agent"]
    );
    assert!(played.with(|state| state.noted.is_empty()));
}

#[test]
fn the_apps_own_code_applies_a_by_app_verb_as_whoever_it_names() {
    let mut app = TestApp::<PlayerApp>::for_person(&ada()).unwrap();
    let played = app.person::<Played>().unwrap();
    played.invoke(&Played::NOTE.with("as ada")).unwrap();
    played
        .invoke_as(&her_agent(), &Played::NOTE.with("as her agent"))
        .unwrap();
    assert_eq!(
        played.with(|state| state.noted.clone()),
        ["as ada", "as her agent"]
    );

    // Each is a change in the record's history, by who the app named.
    let authors: Vec<String> = played
        .record()
        .read(|doc| {
            tilo_app::tilo_resource::history::versions(&mut doc.clone())
                .into_iter()
                .filter_map(|version| version.context?.invoker)
                .map(|invoker| invoker.actor)
                .collect()
        })
        .unwrap();
    assert_eq!(authors, [ada(), "agent:eve".to_owned()]);
    // It is still housekeeping: nothing of the person's to take back.
    assert!(!played.undo());
}
