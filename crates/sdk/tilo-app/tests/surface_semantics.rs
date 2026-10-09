#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! A custom surface's own controls are operated by name, the way assistive
//! technology and agents reach them: its stable id and the semantic targets
//! it publishes resolve through the framework's host, in the same exported
//! accessibility tree a screen reader reads.

use std::collections::HashMap;
use tilo_app::widget::TaffyTreeExt;

use tilo_app::a11y::{AccessNode, Action as A11yAction, Role};
use tilo_app::framework::{Dispatcher, SurfaceCx};
use tilo_app::prelude::*;

use tilo_app::advanced::{Affine, NodeId, Scene, TaffyTree};
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Ballot {
    pub yes: i64,
    pub no: i64,
}

#[resource(kind = "test.ballot", label = "Ballot")]
impl Ballot {
    /// Count a vote for.
    #[action]
    pub fn approve(&mut self) {
        self.yes = self.yes.saturating_add(1);
    }

    /// Count a vote against.
    #[action]
    pub fn reject(&mut self) {
        self.no = self.no.saturating_add(1);
    }
}

/// A painted ballot: two buttons it draws itself and publishes as
/// semantic children.
#[derive(Default)]
struct Booth {
    rect: Rect,
    dispatch: Option<Dispatcher<Ballot>>,
}

impl Booth {
    fn half(&self, right: bool) -> Rect {
        let mid = f64::midpoint(self.rect.x0, self.rect.x1);
        if right {
            Rect::new(mid, self.rect.y0, self.rect.x1, self.rect.y1)
        } else {
            Rect::new(self.rect.x0, self.rect.y0, mid, self.rect.y1)
        }
    }

    fn vote(&self, target: &str) -> bool {
        let Some(dispatch) = &self.dispatch else {
            return false;
        };
        match target {
            "approve" => dispatch.invoke(Ballot::APPROVE.invoke()),
            "reject" => dispatch.invoke(Ballot::REJECT.invoke()),
            _ => return false,
        }
        true
    }
}

impl Widget for Booth {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = tilo_app::taffy::Style {
            size: tilo_app::taffy::Size {
                width: tilo_app::taffy::Dimension::length(200.0),
                height: tilo_app::taffy::Dimension::length(40.0),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        let origin = Point::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
        );
        self.rect = Rect::from_origin_size(
            origin,
            Size::new(f64::from(layout.size.width), f64::from(layout.size.height)),
        );
    }

    fn draw(&mut self, _: &mut Scene, _: Affine, _: &mut TextRenderer, _: &Theme, _: &PresenceCtx) {
    }

    fn name(&self) -> &'static str {
        "Booth"
    }

    fn test_id(&self) -> Option<&str> {
        Some("booth")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let button = |target: &str, name: &str, bounds: Rect| {
            AccessNode::new(Role::Button)
                .with_name(name)
                .with_action_target(target)
                .with_bounds(bounds)
        };
        Some(
            AccessNode::new(Role::Group)
                .with_name("Ballot")
                .with_bounds(self.rect)
                .with_children(vec![
                    button("approve", "Approve", self.half(false)),
                    button("reject", "Reject", self.half(true)),
                ]),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &A11yAction,
        _: &mut tilo_app::widget::Edits<'_>,
        _: &tilo_app::widget::ModelPath,
    ) -> bool {
        matches!(action, A11yAction::Activate) && self.vote(target)
    }
}

impl ResourceSurface<Ballot> for Booth {
    fn present(&mut self, _: &std::rc::Rc<Ballot>) {}

    fn attach(&mut self, cx: SurfaceCx<Ballot>) {
        self.dispatch = Some(cx.dispatcher());
    }
}

/// The surface is the whole view: the root and its host wrap the same node.
struct WholeApp;

impl ResourceApp for WholeApp {
    type Resource = Ballot;
    const ID: &'static str = "so.tilo.test-ballot";
    const TITLE: &'static str = "Ballot";

    fn view(cx: &mut ViewCx<Ballot>) -> Element {
        cx.surface(Booth::default())
    }
}

/// The surface beside framework controls.
struct NestedApp;

impl ResourceApp for NestedApp {
    type Resource = Ballot;
    const ID: &'static str = "so.tilo.test-ballot-nested";
    const TITLE: &'static str = "Ballot";

    fn view(cx: &mut ViewCx<Ballot>) -> Element {
        Box::new(
            Column::new()
                .push(Box::new(Text::new("Cast your vote")))
                .push(cx.surface(Booth::default())),
        )
    }
}

fn operated_by_name<A: ResourceApp<Resource = Ballot>>() {
    let mut app = TestApp::<A>::new().unwrap();
    assert!(
        app.harness().targets().contains_key("booth"),
        "the surface is addressable by its id"
    );

    app.harness()
        .perform_semantic_named("booth", Some("approve"), &A11yAction::Activate)
        .unwrap();
    app.settle();
    assert_eq!(app.state().yes, 1);

    // A control is named by its own target too, as search and agents name
    // it, without the widget that paints it.
    app.harness()
        .perform_named("approve", &A11yAction::Activate)
        .unwrap();
    app.settle();
    assert_eq!(app.state().yes, 2);
    assert!(
        app.harness()
            .perform_named("abstain", &A11yAction::Activate)
            .is_err(),
        "a target nothing publishes names nothing"
    );

    // The exported tree a screen reader reads carries the surface's
    // controls, and operating one there is the same edit.
    let reject = app
        .harness()
        .a11y_node_named("Reject")
        .expect("the reject button is in the accessibility tree");
    app.harness()
        .perform_a11y_action(reject, &A11yAction::Activate)
        .unwrap();
    app.settle();
    assert_eq!(app.state().no, 1);
    assert_eq!(app.state().yes, 2);

    // Each is one undo step, as any edit.
    assert!(app.undo());
    assert_eq!(app.state().no, 0);
}

#[test]
fn a_surface_that_is_the_whole_view_is_operated_by_name() {
    operated_by_name::<WholeApp>();
}

#[test]
fn a_surface_inside_a_view_is_operated_by_name() {
    operated_by_name::<NestedApp>();
}

/// The surface stacked in a view.
struct StackedApp;

impl ResourceApp for StackedApp {
    type Resource = Ballot;
    const ID: &'static str = "so.tilo.test-ballot-stacked";
    const TITLE: &'static str = "Ballot";

    fn view(cx: &mut ViewCx<Ballot>) -> Element {
        Box::new(tilo_app::widget::Stack::new().push(cx.surface(Booth::default())))
    }
}

#[test]
fn a_stacked_surface_is_operated_by_name() {
    operated_by_name::<StackedApp>();
}
