//! The cards a frame's root stands over it. The frame has one window, so it
//! stands them through [`crate::shell_cards::ShellCards`], as a page's or a
//! nested desktop's shell does: here it only gives the column its area and
//! hands it the frame's pointer and event context.

use super::*;

impl FrameHarness {
    /// Stand the cards the root presents and tell their apps where they went,
    /// once a frame, before it draws and routes input.
    pub(super) fn follow_cards(&mut self) {
        let work = vello::kurbo::Rect::new(
            0.0,
            0.0,
            f64::from(self.viewport.0),
            f64::from(self.viewport.1),
        );
        let bar = vello::kurbo::Rect::new(0.0, 0.0, work.width(), 0.0);
        self.cards.follow(self.root.as_mut(), work, bar);
    }

    /// Deliver `event` to the card it is for, ahead of the windows under it.
    /// Answers whether a card took it.
    pub(super) fn dispatch_to_card(
        &mut self,
        event: &WindowEvent,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
    ) -> bool {
        if let WindowEvent::CursorMoved { position, .. } = event {
            // A press that follows without a move of its own is where the
            // pointer last was.
            self.pointer.at = Some(Point::new(
                position.x / self.input_scale,
                position.y / self.input_scale,
            ));
        }
        if self.cards.is_empty() {
            return false;
        }
        self.cards.route(
            self.root.as_mut(),
            event,
            self.pointer.at,
            &mut EventCtx {
                scale_factor: self.input_scale,
                modifiers: &self.modifiers,
                state: crate::widget::Edits::lend(&mut self.state.doc),
                path: &self.state.root,
                handled,
                focus_target,
                node_id: self.child_node,
                taffy: &self.taffy,
            },
        )
    }

    /// The cards as a reader finds them: the reading and controls each app
    /// draws, at the rectangle the frame stands it. The app paints a card, so
    /// its controls own no retained widget and the tree names them here.
    pub(super) fn floating_surface_inspection(&self) -> Vec<InspectionNode> {
        self.cards
            .accessibility(self.root.as_ref())
            .into_iter()
            .map(|(id, window, semantics)| InspectionNode {
                path: format!("floating:{id}"),
                widget: "FloatingSurface".into(),
                test_id: None,
                focusable: false,
                rect: [window.x0, window.y0, window.width(), window.height()],
                child_clip: None,
                page_title: None,
                layout: None,
                paint_key: None,
                semantics: Some(semantics),
                children: Vec::new(),
            })
            .collect()
    }
}
