#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! Where a participant's action leaves them is the framework's: the surface
//! is told, and reads it back, instead of keeping its own copy.

use std::cell::RefCell;

use tilo_app::advanced::{Affine, NodeId, Scene, TaffyTree};
use tilo_app::prelude::*;
use tilo_resource::Place;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Line {
    #[model(label)]
    pub text: String,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Script {
    pub lines: Collection<Line>,
}

#[resource(kind = "test.script", label = "Script", lands = Script::lands)]
impl Script {
    /// Append a line.
    #[action]
    pub fn append(&mut self, cx: &mut ActionCx, text: String) {
        self.lines.push(cx.id(), Line { text });
    }
}

impl Script {
    /// An appended line leaves its writer on it.
    fn lands(state: &Self, _: &<Self as Resource>::Action) -> Option<Place> {
        state
            .lines
            .iter()
            .last()
            .map(|(id, _)| Place::Item(id.to_string()))
    }
}

thread_local! {
    static LANDED: RefCell<Vec<(String, Place)>> = const { RefCell::new(Vec::new()) };
}

#[derive(Default)]
struct Page {
    rect: Rect,
    live: Option<Live<Script>>,
}

impl Widget for Page {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _: &Theme,
        _: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.new_leaf(tilo_app::taffy::Style::default()).unwrap()
    }

    fn apply_layout(&mut self, _: &TaffyTree<()>, _: NodeId, origin: Point) {
        self.rect = Rect::from_origin_size(origin, Size::ZERO);
    }

    fn draw(&mut self, _: &mut Scene, _: Affine, _: &mut TextRenderer, _: &Theme, _: &PresenceCtx) {
    }

    fn name(&self) -> &'static str {
        "Page"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl ResourceSurface<Script> for Page {
    fn present(&mut self, _: &std::rc::Rc<Script>) {}

    fn attach(&mut self, cx: SurfaceCx<Script>) {
        self.live = Some(cx.live());
    }

    fn landed(&mut self, participant: &tilo_actions::ParticipantId, place: &Place) {
        // What it is told is what the framework keeps.
        assert_eq!(
            self.live.as_ref().unwrap().place_of(participant).as_ref(),
            Some(place)
        );
        LANDED.with(|landed| {
            landed
                .borrow_mut()
                .push((participant.as_str().to_owned(), place.clone()));
        });
    }
}

struct ScriptApp;

impl ResourceApp for ScriptApp {
    type Resource = Script;
    const ID: &'static str = "so.tilo.test-script";
    const TITLE: &'static str = "Script";

    fn view(cx: &mut ViewCx<Script>) -> Element {
        cx.surface(Page::default())
    }
}

#[test]
fn a_participants_verb_leaves_them_where_it_lands_and_the_surface_is_told() {
    let mut app = TestApp::<ScriptApp>::new().unwrap();
    let agent = tilo_actions::ParticipantId::new("agent-1").unwrap();
    app.call_by(
        tilo_actions::ActionInvoker::actor("agent").with_participant(agent),
        "test.script",
        "append",
        serde_json::json!({"text": "Hello"}),
    )
    .unwrap();
    app.settle();
    let state = app.state();
    let (line, _) = state.lines.iter().next().unwrap();
    assert_eq!(
        LANDED.with(|landed| landed.borrow().clone()),
        [("agent-1".to_owned(), Place::Item(line.to_string()))]
    );

    // The person at the device is nobody's participant: nothing lands.
    app.call("append", serde_json::json!({"text": "Mine"}))
        .unwrap();
    app.settle();
    assert_eq!(LANDED.with(|landed| landed.borrow().len()), 1);
}
