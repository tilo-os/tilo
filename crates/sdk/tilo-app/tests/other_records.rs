#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test assertions"
)]

//! Field edits on another record than the app's own: a settings section and
//! the person's own record, bound by path in a view of their own, published
//! beside the app's resource with their field edits and verbs, changed only
//! by their person whatever their role in the room, checked by the model's
//! invariants and recorded in the record's history.

use serde::{Deserialize, Serialize};
use tilo_app::Role;
use tilo_app::prelude::*;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Board {
    pub title: String,
}

#[resource(kind = "test.board", label = "Board")]
impl Board {
    /// Retitle the board.
    #[action]
    pub fn retitle(&mut self, title: String) {
        self.title = title;
    }
}

/// How the person's screens look: a settings section that is a model.
#[derive(Model, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[model(check = Display::valid)]
pub struct Display {
    /// Stronger contrast.
    pub high_contrast: bool,
    /// How large everything is drawn.
    pub scale: f64,
}

impl Default for Display {
    fn default() -> Self {
        Self {
            high_contrast: false,
            scale: 1.0,
        }
    }
}

impl Display {
    fn valid(&self) -> Result<(), Refusal> {
        if (0.5..=3.0).contains(&self.scale) {
            Ok(())
        } else {
            Err(Refusal::new("the scale is between 0.5 and 3"))
        }
    }
}

#[resource(kind = "test.display", label = "Display")]
impl Display {
    /// Draw everything at its natural size.
    #[action]
    pub fn reset_scale(&mut self) {
        self.scale = 1.0;
    }
}

impl SettingsSchema for Display {
    const SCOPE: Scope = Scope::Personal;
}

/// What the person plays: their own record.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Playback {
    /// Play the next video by itself.
    pub autoplay: bool,
    pub muted: Set<String>,
}

#[resource(kind = "test.playback", label = "Playback")]
impl Playback {
    /// Mute a channel.
    #[action]
    pub fn mute(&mut self, channel: String) {
        self.muted.insert(channel);
    }
}

struct BoardApp;

impl ResourceApp for BoardApp {
    type Resource = Board;
    const ID: &'static str = "so.tilo.test-board";
    const TITLE: &'static str = "Board";

    fn view(cx: &mut ViewCx<Board>) -> Element {
        let mut column = Column::new();
        let display = cx.settings::<Display>();
        if let Some(part) = cx.record(&display, |cx| {
            Box::new(
                Column::new()
                    .push(Box::new(
                        cx.checkbox("High contrast", Display::paths().high_contrast),
                    ))
                    .push(Box::new(
                        cx.text(|display| format!("scale {}", display.scale))
                            .with_id("scale"),
                    )),
            )
        }) {
            column = column.push(part);
        }
        if let Some(playback) = cx.person::<Playback>()
            && let Some(part) = cx.record(&playback, |cx| {
                Box::new(Column::new().push(Box::new(
                    cx.checkbox("Autoplay", Playback::paths().autoplay),
                )))
            })
        {
            column = column.push(part);
        }
        Box::new(column)
    }
}

/// Ada, by her root public key.
fn ada() -> String {
    "ad".repeat(32)
}

fn checkbox(app: &mut TestApp<BoardApp>, field: &str) -> String {
    app.harness()
        .targets()
        .keys()
        .find(|name| name.as_str() == field)
        .cloned()
        .expect("the field's control")
}

fn published(app: &mut TestApp<BoardApp>, resource: &str) -> Vec<String> {
    let mut actions: Vec<String> = app
        .agent_surface()
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["resource"] == resource)
        .map(|row| row["action"].as_str().unwrap().to_owned())
        .collect();
    actions.sort();
    actions
}

#[test]
fn a_settings_section_is_edited_by_path_and_checked_by_its_model() {
    let mut app = TestApp::<BoardApp>::new().unwrap();
    let control = checkbox(&mut app, "high_contrast");
    app.harness().click_named(&control).unwrap();
    assert!(app.settings::<Display>().get().high_contrast);

    // Its field edits and verbs are published beside the app's resource.
    let actions = published(&mut app, "settings:test.display");
    assert!(
        actions.iter().any(|action| action == "field-edit"),
        "{actions:?}"
    );
    assert!(
        actions.iter().any(|action| action == "reset-scale"),
        "{actions:?}"
    );

    // An agent edits it by path, through the door; the model's invariant
    // holds on every way in.
    app.call_on(
        "settings:test.display",
        "field-edit",
        serde_json::json!({ "edit": { "path": [{ "field": "scale" }], "op": "set", "value": 2.0 } }),
    )
    .unwrap();
    assert!((app.settings::<Display>().get().scale - 2.0).abs() < f64::EPSILON);
    app.harness().relayout();
    assert!(app.harness().inspection_json().unwrap().contains("scale 2"));
    assert!(
        app.call_on(
            "settings:test.display",
            "field-edit",
            serde_json::json!({ "edit": { "path": [{ "field": "scale" }], "op": "set", "value": 9.0 } }),
        )
        .is_err(),
        "the check refuses a scale out of range"
    );
    app.call_on(
        "settings:test.display",
        "reset-scale",
        serde_json::json!({}),
    )
    .unwrap();
    assert!((app.settings::<Display>().get().scale - 1.0).abs() < f64::EPSILON);

    // Each change is a step of the person's: undo sets the value before it
    // again and redo the one after, as new changes of the settings.
    let scale = |app: &TestApp<BoardApp>| app.settings::<Display>().get().scale;
    let step = |app: &mut TestApp<BoardApp>, action: &str, steps: i64| {
        app.call_on(
            "settings:test.display",
            action,
            serde_json::json!({ "steps": steps }),
        )
        .unwrap();
    };
    step(&mut app, "undo", 1);
    assert!((scale(&app) - 2.0).abs() < f64::EPSILON);
    step(&mut app, "undo", 1);
    assert!((scale(&app) - 1.0).abs() < f64::EPSILON);
    assert!(app.settings::<Display>().get().high_contrast);
    step(&mut app, "undo", 1);
    assert!(!app.settings::<Display>().get().high_contrast);
    step(&mut app, "redo", 2);
    assert!(app.settings::<Display>().get().high_contrast);
    assert!((scale(&app) - 2.0).abs() < f64::EPSILON);
    app.harness().relayout();
    assert!(app.harness().inspection_json().unwrap().contains("scale 2"));

    // A write made elsewhere (the Settings app, another device) reaches the
    // bound control.
    app.settings::<Display>()
        .update(|display| display.high_contrast = false)
        .unwrap();
    app.harness().relayout();
    app.harness().click_named(&control).unwrap();
    assert!(app.settings::<Display>().get().high_contrast);
}

#[test]
fn the_persons_record_is_edited_by_path_as_the_person_whatever_their_role() {
    let mut app = TestApp::<BoardApp>::for_person(&ada()).unwrap();
    let control = checkbox(&mut app, "autoplay");
    app.harness().click_named(&control).unwrap();
    let playback = app.person::<Playback>().unwrap();
    assert!(playback.with(|state| state.autoplay));

    let actions = published(&mut app, "person:test.playback");
    for expected in ["field-edit", "mute", "undo", "redo"] {
        assert!(
            actions.iter().any(|action| action == expected),
            "{expected} in {actions:?}"
        );
    }
    app.call_on(
        "person:test.playback",
        "mute",
        serde_json::json!({ "channel": "news" }),
    )
    .unwrap();
    assert!(playback.with(|state| state.muted.contains(&"news".to_owned())));

    // Recorded in the record's history, one step of the person's each.
    app.call_on("person:test.playback", "undo", serde_json::json!({}))
        .unwrap();
    assert!(playback.with(|state| state.muted.is_empty()));
    app.call_on("person:test.playback", "undo", serde_json::json!({}))
        .unwrap();
    assert!(!playback.with(|state| state.autoplay));

    // Someone else in the room does not change it; software acting for
    // the person does.
    let bob = tilo_actions::ActionInvoker::actor("b0".repeat(32));
    assert!(
        app.call_by(
            bob,
            "person:test.playback",
            "mute",
            serde_json::json!({ "channel": "sports" }),
        )
        .is_err()
    );
    let agent = tilo_actions::ActionInvoker::actor("agent:helper")
        .with_kind(tilo_actions::ActorKind::Agent)
        .acting_for(ada());
    app.call_by(
        agent,
        "person:test.playback",
        "mute",
        serde_json::json!({ "channel": "music" }),
    )
    .unwrap();
    assert!(playback.with(|state| state.muted.contains(&"music".to_owned())));
    assert!(!playback.with(|state| state.muted.contains(&"sports".to_owned())));

    // A viewer of the room still changes their own record.
    app.set_role(Role::Viewer);
    app.harness().relayout();
    let control = checkbox(&mut app, "autoplay");
    app.harness().click_named(&control).unwrap();
    assert!(playback.with(|state| state.autoplay));
    assert!(app.invoke(Board::RETITLE.with("not theirs")).is_err());
}
