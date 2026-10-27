#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! Field edits on another record than the app's own: a settings section and
//! the person's own record, bound by path in a view of their own, published
//! beside the app's resource with their field edits and verbs, changed only
//! by their person whatever their role in the room, checked by the model's
//! invariants and recorded in the record's history. A host without a window
//! (the session) holds an app's settings sections and changes them through
//! the same field edits.

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

fn published<A: ResourceApp>(app: &mut TestApp<A>, resource: &str) -> Vec<String> {
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

// A click on a control bound to a field of a settings section kept in the
// person's records stores the change in the settings document, both ways.
#[test]
fn a_click_on_a_persons_settings_section_is_stored_both_ways() {
    let mut app = TestApp::<BoardApp>::for_person(&ada()).unwrap();
    let control = checkbox(&mut app, "high_contrast");
    app.harness().click_named(&control).unwrap();
    app.settle();
    assert!(
        app.settings::<Display>().get().high_contrast,
        "first click stored"
    );
    app.harness().click_named(&control).unwrap();
    app.settle();
    assert!(
        !app.settings::<Display>().get().high_contrast,
        "second click stored"
    );
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

/// Keeps both records, though its view shows neither.
struct DeclaringApp;

impl ResourceApp for DeclaringApp {
    type Resource = Board;
    const ID: &'static str = "so.tilo.test-board-declaring";
    const TITLE: &'static str = "Board";

    fn view(_cx: &mut ViewCx<Board>) -> Element {
        Box::new(Column::new())
    }

    fn records(records: &mut Records<'_>) {
        records.settings::<Display>().person::<Playback>();
    }
}

#[test]
fn declared_records_are_published_and_committed_though_no_view_shows_them() {
    let mut app = TestApp::<DeclaringApp>::for_person(&ada()).unwrap();
    app.call_on(
        "settings:test.display",
        "field-edit",
        serde_json::json!({ "edit": { "path": [{ "field": "high_contrast" }], "op": "set", "value": true } }),
    )
    .unwrap();
    assert!(app.settings::<Display>().get().high_contrast);
    app.call_on(
        "person:test.playback",
        "mute",
        serde_json::json!({ "channel": "news" }),
    )
    .unwrap();
    let playback = app.person::<Playback>().unwrap();
    assert!(playback.with(|state| state.muted.contains(&"news".to_owned())));

    let report = tilo_app::framework::headless::surface_report::<DeclaringApp>();
    let kinds: Vec<&str> = report["resources"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|surface| surface["kind"].as_str())
        .collect();
    for kind in ["test.board", "test.display", "test.playback"] {
        assert!(kinds.contains(&kind), "{kind} in {kinds:?}");
    }
}

/// Binds its own control to a section's field, as a hand surface does.
struct BindingApp;

impl ResourceApp for BindingApp {
    type Resource = Board;
    const ID: &'static str = "so.tilo.test-board-binding";
    const TITLE: &'static str = "Board";

    fn view(cx: &mut ViewCx<Board>) -> Element {
        let display = cx.settings::<Display>();
        let shown = LocalState::new(display.get().high_contrast);
        cx.record(&display, |cx| {
            let binding = cx
                .actions()
                .edit_binding(
                    Display::paths().high_contrast,
                    tilo_app::tilo_resource::Path::set,
                )
                .unwrap();
            Box::new(
                Column::new().push(Box::new(
                    Checkbox::from_state(&shown)
                        .with_id("contrast")
                        .on_action(binding),
                )),
            )
        })
        .unwrap()
    }
}

#[test]
fn a_control_built_by_hand_binds_a_sections_field_edit() {
    let mut app = TestApp::<BindingApp>::new().unwrap();
    app.harness().click_named("contrast").unwrap();
    assert!(app.settings::<Display>().get().high_contrast);
}

/// Binds a control whose input is not the field's value: a switch for a
/// scale.
struct MappingApp;

impl ResourceApp for MappingApp {
    type Resource = Board;
    const ID: &'static str = "so.tilo.test-board-mapping";
    const TITLE: &'static str = "Board";

    fn view(cx: &mut ViewCx<Board>) -> Element {
        let display = cx.settings::<Display>();
        let large = LocalState::new(false);
        cx.record(&display, |cx| {
            let binding = cx
                .actions()
                .edit_binding_with(Display::paths().scale, |path, large: bool| {
                    Ok(path.set(if large { 2.0 } else { 1.0 }))
                })
                .unwrap();
            Box::new(
                Column::new().push(Box::new(
                    Checkbox::from_state(&large)
                        .with_id("large")
                        .on_action(binding),
                )),
            )
        })
        .unwrap()
    }
}

#[test]
fn a_control_whose_input_is_not_the_fields_value_maps_it_to_the_edit() {
    let mut app = TestApp::<MappingApp>::new().unwrap();
    app.harness().click_named("large").unwrap();
    assert!((app.settings::<Display>().get().scale - 2.0).abs() < f64::EPSILON);
}

/// Lists one entity as a live row offering a section's field edit for it.
#[derive(Default)]
struct Rules {
    rect: tilo_app::prelude::Rect,
    contrast: Option<tilo_app::framework::Actions<Display>>,
}

impl Widget for Rules {
    fn build_layout(
        &mut self,
        taffy: &mut tilo_app::advanced::TaffyTree<()>,
        _: &Theme,
        _: &mut std::collections::HashMap<tilo_app::advanced::NodeId, *mut dyn Widget>,
    ) -> tilo_app::advanced::NodeId {
        taffy.new_leaf(tilo_app::taffy::Style::default()).unwrap()
    }

    fn apply_layout(
        &mut self,
        _: &tilo_app::advanced::TaffyTree<()>,
        _: tilo_app::advanced::NodeId,
        origin: Point,
    ) {
        self.rect = Rect::from_origin_size(origin, Size::ZERO);
    }

    fn name(&self) -> &'static str {
        "Rules"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl ResourceSurface<Board> for Rules {
    fn present(&mut self, _: &std::rc::Rc<Board>) {}

    fn attach(&mut self, cx: tilo_app::framework::SurfaceCx<Board>) {
        let display = cx.settings::<Display>();
        self.contrast = cx.record_actions(&display);
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        let row = LiveRow::new("screen:main", "test.screen", "Main screen");
        let Some(contrast) = &self.contrast else {
            return vec![row];
        };
        let edit = tilo_app::tilo_resource::ActionDecl::<Display>::edit(
            "high-contrast",
            "Use high contrast on this screen",
            Display::paths().high_contrast.set(true),
        );
        vec![match contrast.operation(&edit) {
            Some(operation) => row.operation(operation),
            None => row,
        }]
    }
}

struct RowsApp;

impl ResourceApp for RowsApp {
    type Resource = Board;
    const ID: &'static str = "so.tilo.test-board-rows";
    const TITLE: &'static str = "Board";

    fn view(cx: &mut ViewCx<Board>) -> Element {
        cx.surface(Rules::default())
    }
}

#[test]
fn a_live_row_offers_another_records_field_edit() {
    let mut app = TestApp::<RowsApp>::new().unwrap();
    assert!(!app.settings::<Display>().get().high_contrast);
    app.call_on("screen:main", "high-contrast", serde_json::json!({}))
        .unwrap();
    assert!(app.settings::<Display>().get().high_contrast);
}

#[test]
fn a_settings_section_opens_the_persons_record_only_when_first_written() {
    let mut app = TestApp::<DeclaringApp>::for_person(&ada()).unwrap();
    // The person record of playback is declared and kept open; the display
    // section is published from its stored value without a record of its
    // own.
    assert_eq!(app.open_person_records(), 1);
    assert!(published(&mut app, "settings:test.display").contains(&"field-edit".to_owned()));
    assert_eq!(app.open_person_records(), 1, "publishing creates no record");
    app.call_on(
        "settings:test.display",
        "field-edit",
        serde_json::json!({ "edit": { "path": [{ "field": "high_contrast" }], "op": "set", "value": true } }),
    )
    .unwrap();
    assert_eq!(app.open_person_records(), 2, "the first write keeps it");
    assert!(app.settings::<Display>().get().high_contrast);
    app.call_on(
        "settings:test.display",
        "undo",
        serde_json::json!({ "steps": 1 }),
    )
    .unwrap();
    assert!(
        !app.settings::<Display>().get().high_contrast,
        "and it is the person's step"
    );
}

/// How long before the screen locks: a settings section that is a model.
#[derive(Model, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[model(check = Lock::valid)]
pub struct Lock {
    /// Minutes before the screen locks; 0 never.
    #[model(weaker = "higher", weakest = 0)]
    pub minutes: u32,
}

impl Default for Lock {
    fn default() -> Self {
        Self { minutes: 5 }
    }
}

impl Lock {
    fn valid(&self) -> Result<(), Refusal> {
        if self.minutes <= 60 {
            Ok(())
        } else {
            Err(Refusal::new("at most an hour"))
        }
    }
}

#[resource(kind = "test.lock", label = "Lock")]
impl Lock {}

impl SettingsSchema for Lock {
    const SCOPE: Scope = Scope::Shared("test-lock");
}

struct LockApp;

impl ResourceApp for LockApp {
    type Resource = Board;
    const ID: &'static str = "so.tilo.test-lock";
    const TITLE: &'static str = "Lock";

    fn view(_cx: &mut ViewCx<Board>) -> Element {
        Box::new(Column::new())
    }

    fn records(records: &mut Records<'_>) {
        records.settings::<Lock>();
    }
}

fn edit(minutes: u32) -> serde_json::Map<String, serde_json::Value> {
    serde_json::from_value(
        serde_json::json!({ "edit": { "path": [{ "field": "minutes" }], "op": "set", "value": minutes } }),
    )
    .unwrap()
}

#[test]
fn a_section_is_published_and_changed_without_a_window() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let stored = || {
        tilo_app::storage::Settings::<Lock>::for_app(LockApp::ID)
            .get()
            .minutes
    };
    let address = tilo_app::tilo_actions::ActionAddress::new("settings:test.lock", "field-edit");
    let invoker = tilo_app::tilo_actions::ActionInvoker::actor("ada");

    let records = tilo_app::framework::SettingsRecords::new(LockApp::ID, LockApp::records);
    let resources = records.resources();
    let spec = tilo_app::tilo_actions::door::resolve(&resources, &address)
        .expect("the section's field edit is published");
    assert_eq!(
        spec.confirmation_for_json(&edit(1)),
        tilo_app::tilo_actions::ConfirmationPolicy::None,
        "locking sooner asks nothing"
    );
    assert_eq!(
        spec.confirmation_for_json(&edit(0)),
        tilo_app::tilo_actions::ConfirmationPolicy::Confirm,
        "never locking asks first"
    );

    records
        .invoke(&address, &edit(1), false, &invoker, None)
        .unwrap();
    assert_eq!(stored(), 1);
    assert_eq!(
        records.invoke(&address, &edit(0), false, &invoker, None),
        Err(tilo_app::tilo_actions::DoorError::ConfirmationRequired),
        "never locking runs only once confirmed"
    );
    assert_eq!(stored(), 1);
    assert!(
        records
            .invoke(&address, &edit(90), true, &invoker, None)
            .is_err(),
        "the model's check refuses"
    );
    assert_eq!(stored(), 1, "a refused change stores nothing");

    // What the window changed reaches a holder made before it.
    tilo_app::storage::Settings::<Lock>::for_app(LockApp::ID)
        .set(&Lock { minutes: 10 })
        .unwrap();
    let resources = records.resources();
    let spec = tilo_app::tilo_actions::door::resolve(&resources, &address).unwrap();
    assert_eq!(
        spec.confirmation_for_json(&edit(5)),
        tilo_app::tilo_actions::ConfirmationPolicy::None,
        "judged against the value stored now"
    );
}

/// Settings sections the host keeps: the window shows them and binds its
/// controls to them, but each change is the host's action.
struct KeptApp;

impl ResourceApp for KeptApp {
    type Resource = Board;
    const ID: &'static str = "so.tilo.test-kept";
    const TITLE: &'static str = "Kept";

    fn view(cx: &mut ViewCx<Board>) -> Element {
        let display = cx.settings::<Display>();
        cx.record(&display, |cx| {
            Box::new(Column::new().push(Box::new(
                cx.checkbox("High contrast", Display::paths().high_contrast),
            )))
        })
        .unwrap_or_else(|| Box::new(Column::new()))
    }

    fn records(records: &mut Records<'_>) {
        records.kept_by("test.host", |records| {
            records.settings::<Display>().settings::<Lock>();
        });
    }
}

#[test]
fn sections_the_host_keeps_are_its_actions_bound_here_and_published_only_there() {
    let mut app = TestApp::<KeptApp>::new().unwrap();
    let control = app
        .harness()
        .targets()
        .keys()
        .find(|name| name.as_str() == "high_contrast")
        .cloned()
        .expect("the field's control");
    app.harness().click_named(&control).unwrap();
    app.settle();
    assert!(
        app.settings::<Display>().get().high_contrast,
        "the click ran the host's field edit"
    );
    for kept in ["settings:test.display", "settings:test.lock"] {
        assert_eq!(published(&mut app, kept), Vec::<String>::new(), "{kept}");
    }

    app.call_on("settings:test.lock", "field-edit", edit(1).into())
        .unwrap();
    assert_eq!(app.settings::<Lock>().get().minutes, 1);
    assert_eq!(
        app.call_on("settings:test.lock", "field-edit", edit(0).into()),
        Err(tilo_app::tilo_actions::DoorError::ConfirmationRequired),
        "never locking asks first, here as at the host"
    );
    app.call_confirmed_on("settings:test.lock", "field-edit", edit(0).into())
        .unwrap();
    assert_eq!(app.settings::<Lock>().get().minutes, 0);
    assert!(
        app.call_confirmed_on("settings:test.lock", "field-edit", edit(90).into())
            .is_err(),
        "the host's model refuses"
    );
}
