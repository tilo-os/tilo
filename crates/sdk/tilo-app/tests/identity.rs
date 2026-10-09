#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::float_cmp,
    reason = "test assertions; levels are whole numbers, exact in f64"
)]

//! Who is editing, what a new record starts with, and which changes are
//! someone's undo step: views and actions know their participant, a new
//! record is seeded by its creator without an undo step, content from
//! outside syncs without being anyone's step, two devices creating the same
//! record converge on one, and a drag is one step.

use std::cell::Cell;
use std::rc::Rc;

use tilo_app::advanced::{Affine, NodeId, Scene, TaffyTree, WindowEvent};
use tilo_app::prelude::*;
use tilo_app::tilo_resource::Stamped;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Board {
    pub owner: String,
    pub headline: String,
    pub level: f64,
    pub notes: Collection<Note>,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Note {
    pub text: String,
    pub author: String,
}

#[resource(kind = "test.board", label = "Board")]
impl Board {
    /// Make whoever invokes this the board's owner.
    #[action]
    pub fn claim(&mut self, cx: &mut ActionCx) {
        cx.author().unwrap_or_default().clone_into(&mut self.owner);
    }

    /// Post a note, signed by whoever posts it.
    #[action]
    pub fn post(&mut self, cx: &mut ActionCx, text: String) {
        let author = cx.author().unwrap_or_default().to_owned();
        self.notes.push(cx.id(), Note { text, author });
    }

    /// Show the headline a refresh fetched.
    #[action(external)]
    pub fn show_headline(&mut self, headline: String) {
        self.headline = headline;
    }

    /// Set the level.
    #[action]
    pub fn set_level(&mut self, level: f64) {
        self.level = level;
    }
}

struct BoardApp;

impl ResourceApp for BoardApp {
    type Resource = Board;
    const ID: &'static str = "so.tilo.test-board";
    const TITLE: &'static str = "Board";

    fn label(_: &Board) -> String {
        "Studio mixer".into()
    }

    fn view(cx: &mut ViewCx<Board>) -> Element {
        let me = cx.me().map(str::to_owned);
        let signed_in = format!("Signed in as {}", me.as_deref().unwrap_or("nobody"));
        Box::new(
            Column::new()
                .push(Box::new(Text::new(&signed_in)))
                .push(Box::new(cx.text(move |board| {
                    let mine = board
                        .notes
                        .values()
                        .filter(|note| Some(note.author.as_str()) == me.as_deref())
                        .count();
                    format!("{mine} of yours")
                })))
                .push(Box::new(
                    cx.button("Post", Board::POST.with("from the button".into())),
                ))
                .push(cx.surface(LevelSlider::<Board>::default())),
        )
    }

    fn seed(cx: &SeedCx<'_>) -> Vec<Invoke<Board>> {
        assert_eq!(cx.me(), "test-author", "the creator seeds the record");
        vec![Board::CLAIM.invoke()]
    }
}

/// A board whose level changes coalesce: one drag of the slider is one undo
/// step.
struct Mixer;

impl Resource for Mixer {
    const KIND: &'static str = <Board as Resource>::KIND;
    const LABEL: &'static str = <Board as Resource>::LABEL;
    type State = Board;
    type Action = <Board as Resource>::Action;

    fn read(doc: &tilo_resource::Doc) -> Result<Board, tilo_resource::ModelError> {
        <Board as Resource>::read(doc)
    }

    fn initialize(doc: &mut tilo_resource::Doc) -> Result<(), tilo_resource::ModelError> {
        <Board as Resource>::initialize(doc)
    }

    fn coalesces(previous: &Self::Action, next: &Self::Action) -> bool {
        matches!(
            (&previous.operation, &next.operation),
            (BoardAction::SetLevel { .. }, BoardAction::SetLevel { .. })
        )
    }
}

struct MixerApp;

impl ResourceApp for MixerApp {
    type Resource = Mixer;
    const ID: &'static str = "so.tilo.test-mixer";
    const TITLE: &'static str = "Mixer";

    fn view(cx: &mut ViewCx<Mixer>) -> Element {
        cx.surface(LevelSlider::<Mixer>::default())
    }
}

thread_local! {
    static SLIDER: Cell<Rect> = const { Cell::new(Rect::ZERO) };
    static SURFACE_ME: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// The action that sets the level, which both resources apply.
type LevelAction = <Board as Resource>::Action;

/// A slider the surface draws itself: every value a drag passes through is
/// applied at once, so the document follows the drag live, and letting go
/// settles the gesture.
struct LevelSlider<R: Resource> {
    rect: Rect,
    cursor: Point,
    dragging: bool,
    dispatch: Option<Dispatcher<R>>,
}

impl<R: Resource> Default for LevelSlider<R> {
    fn default() -> Self {
        Self {
            rect: Rect::ZERO,
            cursor: Point::ZERO,
            dragging: false,
            dispatch: None,
        }
    }
}

impl<R: Resource<Action = LevelAction>> Widget for LevelSlider<R> {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _: &Theme,
        _: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        use tilo_app::taffy::{Dimension, Size as Extent, Style};
        taffy
            .new_leaf(Style {
                size: Extent {
                    width: Dimension::length(200.0),
                    height: Dimension::length(20.0),
                },
                ..Style::default()
            })
            .unwrap()
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::from_origin_size(
            origin,
            Size::new(f64::from(layout.size.width), f64::from(layout.size.height)),
        );
        SLIDER.with(|slider| slider.set(self.rect));
    }

    fn draw(&mut self, _: &mut Scene, _: Affine, _: &mut TextRenderer, _: &Theme, _: &PresenceCtx) {
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        use tilo_app::winit::event::{ElementState, MouseButton};
        let Some(dispatch) = &self.dispatch else {
            return;
        };
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                if self.dragging {
                    let level = ((self.cursor.x - self.rect.x0) / self.rect.width() * 100.0)
                        .clamp(0.0, 100.0)
                        .round();
                    let action = Stamped {
                        operation: BoardAction::SetLevel { level },
                        now_ms: 0,
                    };
                    dispatch.apply_now(&mut ctx.edits(), &action).unwrap();
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => match state {
                ElementState::Pressed if self.rect.contains(self.cursor) => {
                    self.dragging = true;
                    *ctx.handled = true;
                }
                ElementState::Released if self.dragging => {
                    self.dragging = false;
                    dispatch.settle();
                    *ctx.handled = true;
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        self.dragging
    }

    fn name(&self) -> &'static str {
        "LevelSlider"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl<R: Resource<Action = LevelAction>> ResourceSurface<R> for LevelSlider<R> {
    fn present(&mut self, _: &Rc<R::State>) {}

    fn attach(&mut self, cx: SurfaceCx<R>) {
        SURFACE_ME.with(|me| *me.borrow_mut() = cx.me().map(str::to_owned));
        self.dispatch = Some(cx.dispatcher());
    }
}

/// Drag the slider from `from` to `to`, in percent of its width.
fn drag<A: ResourceApp>(app: &mut TestApp<A>, from: f64, to: f64) {
    let rect = SLIDER.with(Cell::get);
    let at = |percent: f64| (rect.x0 + rect.width() * percent / 100.0, rect.center().y);
    app.harness().drag_at(at(from), at(to));
}

fn shown(app: &mut TestApp<BoardApp>) -> String {
    app.harness().inspection_json().unwrap()
}

#[test]
fn views_surfaces_and_actions_know_who_is_editing() {
    let mut app = TestApp::<BoardApp>::new().unwrap();
    assert!(shown(&mut app).contains("Signed in as test-author"));
    assert_eq!(
        SURFACE_ME.with(|me| me.borrow().clone()).as_deref(),
        Some("test-author")
    );

    // A control's action reads its author when it applies; the view compares
    // the authors the resource records with its reader.
    app.harness().click_named("post").unwrap();
    let notes: Vec<Note> = app.state().notes.values().cloned().collect();
    assert_eq!(
        notes,
        [Note {
            text: "from the button".into(),
            author: "test-author".into(),
        }]
    );
    assert!(shown(&mut app).contains("1 of yours"));

    // The same through the door an agent or a script uses.
    app.invoke(Board::POST.with("through the door".into()))
        .unwrap();
    assert!(
        app.state()
            .notes
            .values()
            .all(|note| note.author == "test-author")
    );
    assert!(shown(&mut app).contains("2 of yours"));

    // A surface's immediate edit is attributed to its participant too.
    drag(&mut app, 10.0, 40.0);
    assert_eq!(app.state().level, 40.0);
    let last = app.versions().pop().unwrap();
    let context = last.context.unwrap();
    let resource = context.resource.unwrap();
    assert_eq!(resource.kind, "test.board");
    assert_eq!(resource.label, "Studio mixer");
    assert_ne!(resource.label, <Board as Resource>::LABEL);
    let invoker = context.invoker.unwrap();
    assert_eq!(invoker.actor, "test-author");

    // A past version is read by nobody in particular.
    let first = app.versions().first().unwrap().hash;
    let past = app.past_version(&[first]).unwrap();
    assert!(
        past.texts()
            .iter()
            .any(|text| text == "Signed in as nobody")
    );
}

#[test]
fn a_new_record_is_seeded_by_its_creator_without_an_undo_step() {
    let mut app = TestApp::<BoardApp>::new().unwrap();
    assert_eq!(app.state().owner, "test-author");
    let seed = app
        .versions()
        .into_iter()
        .find_map(|version| version.context)
        .unwrap();
    assert_eq!(seed.change, tilo_actions::ChangeKind::External);
    assert_eq!(seed.invoker.unwrap().actor, "test-author");

    // Neither Ctrl+Z nor the `undo` action takes the seed back.
    assert!(!app.undo());
    assert!(app.call("undo", serde_json::json!({})).is_err());
    assert_eq!(app.state().owner, "test-author");

    // The seed is what a new record starts with, not what every open does.
    let versions = app.versions().len();
    let app = app.reopen().unwrap();
    assert_eq!(app.versions().len(), versions);
    assert_eq!(app.state().owner, "test-author");
}

#[test]
fn content_from_outside_syncs_but_is_nobodys_undo_step() {
    let mut app = TestApp::<BoardApp>::new().unwrap();
    app.invoke(Board::POST.with("mine".into())).unwrap();
    app.invoke(Board::SHOW_HEADLINE.with("Fetched".into()))
        .unwrap();
    let peer = app.peer_document();
    assert_eq!(
        <Board as Resource>::read(&peer).unwrap().headline,
        "Fetched"
    );

    // Undo takes back the person's post, past the fetched headline, and
    // leaves the headline: it was nobody's step.
    assert!(app.undo());
    assert!(app.state().notes.is_empty());
    assert_eq!(app.state().headline, "Fetched");
    assert!(!app.undo());

    // The history verb skips it the same way.
    let mut app = TestApp::<BoardApp>::new().unwrap();
    app.invoke(Board::POST.with("mine".into())).unwrap();
    app.invoke(Board::SHOW_HEADLINE.with("Fetched".into()))
        .unwrap();
    app.call("undo", serde_json::json!({})).unwrap();
    assert!(app.state().notes.is_empty());
    assert_eq!(app.state().headline, "Fetched");
}

#[test]
fn two_devices_creating_the_same_record_converge_on_one() {
    // Two devices open the same record before they have synced: each
    // creates it, and each posts into the collection its genesis made.
    let mut here = TestApp::<BoardApp>::new().unwrap();
    let mut there = TestApp::<BoardApp>::new().unwrap();
    here.invoke(Board::POST.with("written here".into()))
        .unwrap();
    there
        .invoke(Board::POST.with("written there".into()))
        .unwrap();
    let mut theirs = there.peer_document();
    here.merge_peer(&mut theirs).unwrap();
    let mut texts: Vec<String> = here
        .state()
        .notes
        .values()
        .map(|note| note.text.clone())
        .collect();
    texts.sort();
    assert_eq!(texts, ["written here", "written there"]);
}

#[test]
fn a_drag_is_one_undo_step_and_the_next_drag_another() {
    let mut app = TestApp::<MixerApp>::new().unwrap();
    let before = app.versions().len();
    drag(&mut app, 10.0, 55.0);
    let first = app.state().level;
    assert_eq!(first, 55.0);
    // Every value the drag passed through reached the document as it moved.
    assert!(app.versions().len() >= before + 4);
    drag(&mut app, 55.0, 95.0);
    assert_eq!(app.state().level, 95.0);

    assert!(app.undo());
    assert_eq!(
        app.state().level,
        first,
        "one undo takes back the last drag"
    );
    assert!(app.undo());
    assert_eq!(app.state().level, 0.0, "and one more the first");
    assert!(!app.undo());
    assert!(app.redo());
    assert_eq!(app.state().level, first);
}
