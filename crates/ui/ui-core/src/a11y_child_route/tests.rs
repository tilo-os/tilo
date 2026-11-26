use tilo_ui_base::a11y_child_route::*;
use std::cell::Cell;
use std::rc::Rc;

use crate::{widget::Button, widget::Embedded};
use tilo_ui_base::widget::TaffyTreeExt;
use vello::kurbo::Rect;
use tilo_ui_base::widget::Widget;

struct Painter {
    button: Embedded<Button>,
    rect: Rect,
}

impl Widget for Painter {
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        _theme: &tilo_ui_base::theme::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        taffy.fill_leaf(node_map, self)
    }

    fn apply_layout(
        &mut self,
        taffy: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: vello::kurbo::Point,
    ) {
        self.rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        let at = Rect::new(40.0, 60.0, 200.0, 60.0);
        self.button.layout(at, theme, text);
        self.button.draw(&mut tilo_ui_base::draw::DrawCtx::new(
            scene,
            transform,
            text,
            theme,
            env.clone(),
        ));
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn name(&self) -> &str {
        "Painter"
    }

    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        Some(tilo_ui_base::a11y::AccessNode::new(tilo_ui_base::a11y::Role::Group).with_name("Painted"))
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.button.widget()]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.button.widget_mut()]
    }

    fn lays_out_child(&self, _child_index: usize) -> bool {
        true
    }
}

tilo_test_env::scenario! {
    fn a_child_route_is_walked_and_reads_back;

    phase a_painted_surfaces_retained_button_is_operated_through_the_walk;
    {
        let pressed = Rc::new(Cell::new(0));
        let button = Button::new("play", "Play").on_open({
            let pressed = Rc::clone(&pressed);
            move || pressed.set(pressed.get() + 1)
        });
        let painter = Painter {
            button: Embedded::new(button),
            rect: Rect::ZERO,
        };
        let mut harness = crate::headless::Driver::for_root(
            crate::State::new(),
            painter,
            (400, 300),
            tilo_ui_base::theme::Theme::dark(),
        );
        harness.relayout();
        let update = harness.a11y_tree_update();
        let (id, node) = update
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Play"))
            .expect("the button is exported");
        let bounds = node.bounds().expect("where it is drawn");
        assert!(bounds.x0 >= 40.0 && bounds.y0 >= 60.0, "{bounds:?}");
        harness
            .perform_a11y_action(*id, &tilo_ui_base::a11y::Action::Activate)
            .expect("the walk routes to the button");
        assert_eq!(pressed.get(), 1);
    }

    phase a_route_reads_back_as_written;
    {
        let plain = encode(&[2, 0], None);
        assert_eq!(plain, "tilo.child:2/0");
        assert!(matches!(decode(&plain), Some((path, Inner::Widget)) if path == [2, 0]));
        let semantic = encode(&[1], Some("menu:item:3"));
        assert!(
            matches!(decode(&semantic), Some((path, Inner::Semantic("menu:item:3"))) if path == [1])
        );
        let keyed = encode(&[0, 4], Some(&keys(&[7, 9])));
        assert!(
            matches!(decode(&keyed), Some((path, Inner::Keys(keys))) if path == [0, 4] && keys == [7, 9])
        );
        assert!(decode("tilo.child:").is_none());
        assert!(decode("music.page-widget.play").is_none());
    }
}
