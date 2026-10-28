//! The pointer shape a surface this harness hosts asks for (the browser
//! page's desktop and windows among them), by the rule every host shares
//! ([`crate::widget::cursor`]); the page shows it as the canvas's cursor.

use vello::kurbo::Point;

use super::FrameHarness;

impl FrameHarness {
    /// The pointer shape the laid-out surface asks for at logical `point`,
    /// by the rule every host shares ([`crate::widget::cursor`]): its laid-out
    /// nodes, and the shown tree walked as well, since a container that
    /// places its own children (a desktop's strip of windows) keeps them out
    /// of the node map.
    #[must_use]
    pub fn cursor_at(&self, point: Point) -> Option<winit::window::CursorIcon> {
        let mut pick = crate::widget::cursor::CursorPick::default();
        for &node in self.node_map.keys() {
            let _ = self.inspect_node(node, |widget| pick.offer(widget, point));
        }
        for root in self.presentation_roots() {
            let Ok(_entry) = crate::bound_dispatch::enter_widget(root) else {
                continue;
            };
            pick.offer_tree(root, point);
        }
        pick.icon()
    }

    /// The pointer shape to show where the pointer last was: the default
    /// arrow off the surface and where nothing asks for another.
    #[must_use]
    pub fn pointer_cursor(&self) -> winit::window::CursorIcon {
        self.pointer_at
            .and_then(|point| self.cursor_at(point))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::State;
    use winit::event::WindowEvent;

    /// The browser page shows the pointer shape a native window would: a
    /// text field's I-beam where the pointer is over it, the arrow where
    /// nothing asks for another ([`crate::widget::cursor`]).
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
}
