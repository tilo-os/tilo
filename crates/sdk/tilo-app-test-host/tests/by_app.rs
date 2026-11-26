//! What an app keeps for itself in a record beside its own
//! (`#[action(by_app)]`): its own code applies it, as whoever it names, and
//! the record's API offers it to nobody and takes it from nobody.

use tilo_app::prelude::*;
use tilo_app_test_host::TestApp;

/// A player: what it plays now.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Player {
    pub playing: String,
}

#[resource(
    kind = "test.jukebox",
    app = "so.tilo.test-jukebox",
    catalogue = "tests/locales"
)]
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
    app = "so.tilo.test-jukebox",
    catalogue = "tests/locales"
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
        let played = cx.profile_resource::<Played>();
        let shown = played.map_or_else(
            || "no history".to_owned(),
            |played| format!("{} played", played.with(|state| state.noted.len())),
        );
        Box::new(Column::new().push(Box::new(Text::new(&shown))))
    }
}

/// Ada, by her root public key.
fn ada() -> String {
    "ad".repeat(32)
}

/// An agent the host admitted for Ada.
fn her_agent() -> tilo_actions::ActionInvoker {
    tilo_actions::ActionInvoker::agent("eve", ada()).acting_for(ada())
}

#[test]
fn the_apps_own_code_applies_a_by_app_verb_as_whoever_it_names() {
    let mut app = TestApp::<PlayerApp>::for_person(&ada()).unwrap();
    let played = app.profile_resource::<Played>().unwrap();
    played.invoke(&Played::NOTE.with("as ada")).unwrap();
    played
        .invoke_as(&her_agent(), &Played::NOTE.with("as her agent"))
        .unwrap();
    assert_eq!(
        played.with(|state| state.noted.clone()),
        ["as ada", "as her agent"]
    );

    // Each is a change in the record's history, by who the app named.
    let authors: Vec<tilo_app::tilo_actions::Actor> = played
        .record()
        .read(|doc| {
            tilo_app::tilo_resource::history::versions(&mut doc.clone())
                .into_iter()
                .filter_map(|version| version.context?.invoker)
                .map(|invoker| invoker.acting.by)
                .collect()
        })
        .unwrap();
    assert_eq!(
        authors,
        [
            MemberId::person(ada()).into(),
            MemberId::agent("eve", ada()).into()
        ]
    );
    // It is still housekeeping: nothing of the person's to take back.
    assert!(!played.undo());
}
