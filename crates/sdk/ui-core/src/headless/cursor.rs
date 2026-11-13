//! The pointer shape a surface this harness hosts asks for (the browser page's desktop and windows
//! among them), by the rule every host shares ([`crate::widget::cursor`]); the page shows it as the
//! canvas's cursor.

use crate::host_core::HostCore as _;
use vello::kurbo::Point;

use super::FrameHarness;

impl FrameHarness {
    /// The pointer shape the laid-out surface asks for at logical `point`, by the rule every host
    /// shares ([`crate::widget::cursor`]): its laid-out nodes, and the shown tree walked as well, since
    /// a container that places its own children (a desktop's strip of windows) keeps them out of the
    /// node map.
    #[must_use]
    pub fn cursor_at(&self, point: Point) -> Option<winit::window::CursorIcon> {
        let mut pick = self.tree().cursor_pick_at(point);
        for root in self.presentation_roots() {
            let Ok(_entry) = crate::bound_dispatch::enter_widget(root) else {
                continue;
            };
            pick.offer_tree(root, point);
        }
        pick.icon()
    }

    /// The pointer shape to show where the pointer last was: the default arrow off the surface and
    /// where nothing asks for another.
    #[must_use]
    pub fn pointer_cursor(&self) -> winit::window::CursorIcon {
        self.pointer
            .at
            .and_then(|point| self.cursor_at(point))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::State;
    use winit::event::WindowEvent;

    /// The browser page shows the pointer shape a native window would: a text field's I-beam where the
    /// pointer is over it, the arrow where nothing asks for another ([`crate::widget::cursor`]).
    #[test]
    fn a_text_field_shows_the_text_cursor_through_the_shared_rule() {
        use winit::window::CursorIcon;
        let root =
            crate::widget::Column::new().push(Box::new(crate::widget::TextInput::new(vec![
                crate::PathSegment::Field("draft".into()),
            ])));
        let mut harness = FrameHarness::new(State::new(), Box::new(root), (240, 160));
        harness.relayout();
        let over = (1..80)
            .map(|y| Point::new(60.0, f64::from(y)))
            .find(|&p| harness.cursor_at(p) == Some(CursorIcon::Text))
            .expect("the field asks for the text cursor");
        harness.send_host_event(&WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(over.x, over.y),
        });
        assert_eq!(harness.pointer_cursor(), CursorIcon::Text);
        harness.send_host_event(&WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(60.0, 150.0),
        });
        assert_eq!(harness.pointer_cursor(), CursorIcon::Default);
    }

    /// A card that builds its contents again while it draws, as a panel opening the card it was asked
    /// for does, and says so: what it let go of is kept aside, so a read through the old nodes finds
    /// them out of the tree instead of in freed memory.
    struct RebuildsWhileDrawing {
        child: Box<dyn crate::widget::Widget>,
        let_go: Vec<Box<dyn crate::widget::Widget>>,
        rebuild: bool,
        replaced: bool,
        rect: vello::kurbo::Rect,
    }

    fn card() -> Box<dyn crate::widget::Widget> {
        Box::new(
            crate::widget::Row::new().push(Box::new(crate::widget::TextInput::new(vec![
                crate::PathSegment::Field("draft".into()),
            ]))),
        )
    }

    impl crate::widget::Widget for RebuildsWhileDrawing {
        fn name(&self) -> &str {
            "rebuilds-while-drawing"
        }

        fn build_layout(
            &mut self,
            taffy: &mut taffy::TaffyTree<()>,
            theme: &crate::theme::Theme,
            node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn crate::widget::Widget>,
        ) -> taffy::NodeId {
            use crate::widget::TaffyTreeExt as _;
            let child = self.child.build_layout(taffy, theme, node_map);
            let style = taffy::Style {
                size: taffy::Size::from_percent(1.0, 1.0),
                ..Default::default()
            };
            taffy.widget_node(style, &[child], node_map, self)
        }

        fn apply_layout(
            &mut self,
            taffy: &taffy::TaffyTree<()>,
            node: taffy::NodeId,
            origin: Point,
        ) {
            self.rect = crate::widget::layout_rect(taffy, node, origin);
            if let Some(&child) = taffy.children(node).unwrap_or_default().first() {
                self.child.apply_layout(taffy, child, origin);
            }
        }

        fn get_rect(&self) -> vello::kurbo::Rect {
            self.rect
        }

        fn draw(
            &mut self,
            scene: &mut vello::Scene,
            transform: vello::kurbo::Affine,
            text: &mut crate::text_render::TextRenderer,
            theme: &crate::theme::Theme,
            presence: &crate::presence::PresenceCtx,
        ) {
            self.child.draw(scene, transform, text, theme, presence);
            if std::mem::take(&mut self.rebuild) {
                self.let_go.push(std::mem::replace(&mut self.child, card()));
                self.replaced = true;
            }
        }

        fn children(&self) -> Vec<&dyn crate::widget::Widget> {
            vec![self.child.as_ref()]
        }

        fn children_mut(&mut self) -> Vec<&mut (dyn crate::widget::Widget + '_)> {
            vec![self.child.as_mut()]
        }

        fn take_structure_dirty(&mut self) -> bool {
            std::mem::take(&mut self.replaced)
        }
    }

    fn out_of_tree(harness: &FrameHarness) -> usize {
        fn walk(widget: &dyn crate::widget::Widget, live: &mut Vec<usize>) {
            live.push(std::ptr::from_ref(widget).cast::<()>() as usize);
            for child in widget.children() {
                walk(child, live);
            }
        }
        let mut live = Vec::new();
        walk(harness.root.as_ref(), &mut live);
        harness
            .node_map
            .values()
            .filter(|pointer| !live.contains(&(pointer.cast::<()>() as usize)))
            .count()
    }

    /// A widget that built its children again while the frame drew leaves the frame's layout maps
    /// naming the ones it let go of.
    #[test]
    fn input_after_a_widget_rebuilt_while_drawing_reads_no_widget_it_let_go_of() {
        let root = RebuildsWhileDrawing {
            child: card(),
            let_go: Vec::new(),
            rebuild: false,
            replaced: false,
            rect: vello::kurbo::Rect::ZERO,
        };
        let mut harness =
            FrameHarness::for_root(State::new(), root, (240, 160), crate::theme::Theme::dark());
        harness.set_coalesce_input_frames(true);
        harness.relayout();
        assert_eq!(out_of_tree(&harness), 0);
        harness.root_mut::<RebuildsWhileDrawing>().rebuild = true;
        harness.relayout();
        assert!(
            out_of_tree(&harness) > 0,
            "the frame drew a rebuilt card after laying out the old one"
        );
        harness.send_host_event(&WindowEvent::ModifiersChanged(
            winit::event::Modifiers::default(),
        ));
        assert_eq!(
            out_of_tree(&harness),
            0,
            "input read the layout maps through widgets that were let go of"
        );
        let _ = harness.pointer_cursor();
    }
}
