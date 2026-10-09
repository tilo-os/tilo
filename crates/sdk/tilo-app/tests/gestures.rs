#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! Consecutive edits of one field by one person within one gesture are one
//! undo step: typing a word, dragging a slider.

use tilo_app::prelude::*;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Mixer {
    pub name: String,
    pub level: f64,
    pub pan: Option<f64>,
}

#[resource(kind = "test.mixer", label = "Mixer")]
impl Mixer {}

struct MixerApp;

impl ResourceApp for MixerApp {
    type Resource = Mixer;
    const ID: &'static str = "so.tilo.test-mixer";
    const TITLE: &'static str = "Mixer";

    fn view(cx: &mut ViewCx<Mixer>) -> Element {
        let p = cx.paths();
        Box::new(tilo_app::column![
            cx.input(p.name).with_id("name"),
            cx.control(|state| Slider::from_state(state).with_id("level"), p.level),
            cx.control_map(
                |state| Slider::from_state(state).with_id("pan"),
                p.pan,
                |pan| pan.unwrap_or(0.5),
                Some,
            ),
        ])
    }
}

#[test]
fn typing_a_word_into_a_bound_field_is_one_undo_step() {
    let mut app = TestApp::<MixerApp>::new().unwrap();
    app.harness().click_named("name").unwrap();
    app.harness().type_text("Drums");
    assert_eq!(app.state().name, "Drums");
    assert!(app.undo());
    assert_eq!(app.state().name, "", "the whole word is one step");
}

#[test]
fn dragging_a_bound_slider_is_one_undo_step_and_the_next_drag_another() {
    let mut app = TestApp::<MixerApp>::new().unwrap();
    let slider = app.harness().targets().get("level").copied().unwrap();
    let y = f64::midpoint(slider.y0, slider.y1);
    let at = |fraction: f64| slider.x0 + (slider.x1 - slider.x0) * fraction;
    let versions = app.versions().len();
    app.harness().drag_at((at(0.1), y), (at(0.5), y));
    let first = app.state().level;
    assert!(first > 0.3, "the drag moved the level: {first}");
    assert!(
        app.versions().len() > versions + 1,
        "collaborators follow the drag live"
    );
    app.harness().drag_at((at(0.5), y), (at(0.9), y));
    assert!(app.state().level > first);

    assert!(app.undo());
    assert!(
        (app.state().level - first).abs() < 1e-9,
        "undo takes back the second drag whole: {}",
        app.state().level
    );
    assert!(app.undo());
    assert!(
        app.state().level < 0.15,
        "and then the first: {}",
        app.state().level
    );
}

/// A hand-built editor over the same field, splicing it in place.
struct EditorApp;

impl ResourceApp for EditorApp {
    type Resource = Mixer;
    const ID: &'static str = "so.tilo.test-mixer-editor";
    const TITLE: &'static str = "Mixer";

    fn view(cx: &mut ViewCx<Mixer>) -> Element {
        Box::new(tilo_app::framework::TextControl::at(
            TextInput::local().with_id("name"),
            cx.dispatcher(),
            cx.paths().name,
        ))
    }
}

#[test]
fn a_text_control_at_a_path_splices_the_field_and_undoes_a_word() {
    let mut app = TestApp::<EditorApp>::new().unwrap();
    app.harness().click_named("name").unwrap();
    app.harness().type_text("Bass");
    assert_eq!(app.state().name, "Bass");
    assert!(app.undo());
    assert_eq!(app.state().name, "");
}

#[test]
fn a_control_of_another_type_maps_to_the_field() {
    let mut app = TestApp::<MixerApp>::new().unwrap();
    assert_eq!(app.state().pan, None);
    let targets = app.harness().targets();
    let slider = targets
        .get("pan")
        .copied()
        .unwrap_or_else(|| panic!("{:?}", targets.keys().collect::<Vec<_>>()));
    let y = f64::midpoint(slider.y0, slider.y1);
    let at = |fraction: f64| slider.x0 + (slider.x1 - slider.x0) * fraction;
    app.harness().drag_at((at(0.5), y), (at(0.9), y));
    let pan = app.state().pan.expect("the drag set the pan");
    assert!(pan > 0.6, "{pan}");
}
