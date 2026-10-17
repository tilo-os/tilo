#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test assertions"
)]

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

/// Several field edits a surface must see at once (a paste) are one action
/// when submitted now as a group: one change, one entry in history.
#[test]
fn a_group_submitted_now_is_one_change() {
    use tilo_app::framework::testing::detached_dispatcher;
    let mut mixer = Replica::<Mixer>::new();
    let dispatcher = detached_dispatcher::<Mixer>("ada");
    let p = Mixer::paths();
    let doc = mixer.doc_mut();
    let before = doc.get_changes(&[]).len();
    let group = Invoke::group([
        Invoke::edit(&p.name.replace("Drums")).unwrap(),
        Invoke::edit(&p.level.set(0.5)).unwrap(),
    ]);
    dispatcher
        .submit_now(&mut tilo_app::widget::Edits::lend(doc), group)
        .unwrap();
    assert_eq!(doc.get_changes(&[]).len(), before + 1, "one change");
    let state = mixer.state().unwrap();
    assert_eq!(state.name, "Drums");
    assert!((state.level - 0.5).abs() < f64::EPSILON);
}

/// The same field with spelling help.
struct AssistedApp;

impl ResourceApp for AssistedApp {
    type Resource = Mixer;
    const ID: &'static str = "so.tilo.test-mixer-assisted";
    const TITLE: &'static str = "Mixer";

    fn view(cx: &mut ViewCx<Mixer>) -> Element {
        Box::new(
            cx.input(cx.paths().name)
                .with_id("name")
                .with_spellcheck(true)
                .with_autocorrect(true)
                .with_spell_language("en_US"),
        )
    }
}

fn ctrl_z(harness: &mut tilo_app::headless::FrameHarness) {
    harness.set_modifiers(&["ctrl".into()]);
    harness.key("z");
    harness.set_modifiers(&[]);
}

#[test]
fn undoing_an_autocorrection_gives_back_the_typed_word_and_keeps_the_typing_before_it() {
    let mut app = TestApp::<AssistedApp>::new().unwrap();
    app.harness().click_named("name").unwrap();
    app.harness().type_text("we saw teh ");
    assert_eq!(app.state().name, "we saw the ");
    ctrl_z(app.harness());
    assert_eq!(
        app.state().name,
        "we saw teh ",
        "only the correction is undone"
    );
    ctrl_z(app.harness());
    assert_ne!(
        app.state().name,
        "we saw teh ",
        "the typing before the correction is a step of its own"
    );
}

/// A field whose surface runs a text action (a rewrite) for the selection,
/// as an app hosting model work does.
struct Rewriter {
    input: TextInput,
}

impl tilo_app::widget::Decorator for Rewriter {
    type Inner = TextInput;
    fn inner(&self) -> &TextInput {
        &self.input
    }
    fn inner_mut(&mut self) -> &mut TextInput {
        &mut self.input
    }
}

impl ResourceSurface<Mixer> for Rewriter {
    fn present(&mut self, _: &std::rc::Rc<Mixer>) {}
}

struct RewriterApp;

impl ResourceApp for RewriterApp {
    type Resource = Mixer;
    const ID: &'static str = "so.tilo.test-mixer-rewriter";
    const TITLE: &'static str = "Mixer";

    fn view(cx: &mut ViewCx<Mixer>) -> Element {
        let input = cx
            .input(cx.paths().name)
            .with_id("name")
            .with_text_action("shout", "Shout");
        cx.surface(Rewriter { input })
    }
}

/// Select the whole field and run its text action from the context menu,
/// as the person does; the request the surface takes.
fn request_shout(app: &mut TestApp<RewriterApp>) -> tilo_app::widget::TextActionRequest {
    use tilo_app::synthetic_input::{cursor_moved, mouse_input};
    use tilo_app::winit::event::{ElementState, MouseButton};
    let harness = app.harness();
    harness.set_modifiers(&["ctrl".into()]);
    harness.key("a");
    harness.set_modifiers(&[]);
    let field = harness.targets()["name"];
    let (x, y) = (field.x0 + 12.0, field.center().y);
    harness.send(&cursor_moved(x, y));
    harness.send(&mouse_input(ElementState::Pressed, MouseButton::Right));
    harness.send(&mouse_input(ElementState::Released, MouseButton::Right));
    // The open menu owns the keyboard; the registered action is its first row.
    harness.key("Down");
    harness.key("Enter");
    app.with_surface(|surface: &mut Rewriter| surface.input.take_text_action_request())
        .flatten()
        .expect("the selection's action was requested")
}

fn apply(app: &mut TestApp<RewriterApp>, result: &tilo_app::widget::TextActionResult) -> bool {
    app.with_surface(|surface: &mut Rewriter| surface.input.apply_text_action_result(result))
        .unwrap()
}

#[test]
fn a_text_action_result_is_one_undo_step_and_refused_once_its_range_changed() {
    let mut app = TestApp::<RewriterApp>::new().unwrap();
    app.harness().click_named("name").unwrap();
    app.harness().type_text("drums");
    let request = request_shout(&mut app);
    assert_eq!(request.text, "drums");
    assert!(apply(
        &mut app,
        &request.result(request.text.to_uppercase())
    ));
    assert_eq!(app.state().name, "DRUMS");
    ctrl_z(app.harness());
    assert_eq!(app.state().name, "drums", "one undo takes the result back");

    // The person types before the range while the action runs, moving the
    // text it was asked about: the late result is refused rather than
    // written over the wrong letters.
    let request = request_shout(&mut app);
    app.harness().key("Home");
    app.harness().type_text("big ");
    assert!(!apply(&mut app, &request.result("DRUMS")));
    assert_eq!(app.state().name, "big drums");
}
