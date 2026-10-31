//! One event sent to a widget under test.
//!
//! [`dispatch`] covers the common case: a scratch document, a default-styled
//! leaf for layout, no modifiers held, scale factor 1. [`Dispatch`] changes
//! any of those and reports the focus target the widget asked for.

use crate::widget::{Edits, EventCtx, TaffyTreeExt, Widget};
use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, Style, TaffyTree};
use winit::event::{Modifiers, WindowEvent};

/// Send `event` to `widget` in a scratch context; whether it was handled.
pub fn dispatch<W: Widget + ?Sized>(widget: &mut W, event: &WindowEvent) -> bool {
    Dispatch::new().send(widget, event).handled
}

/// Answer the controls `widget`'s subtree queued, the way a host answers the
/// action a press published: a person's click on a
/// [`Button::on_view`](crate::widget::Button::on_view) reaches its handler
/// here, not at the press. Answers how many ran.
///
/// For a tree that keeps no resource host of its own: the control that
/// published the action is the one that answers it, exactly as
/// [`crate::widget::controls::start_pending_control_invocations`] does for
/// the shell. A test that presses a published control and then asserts what
/// it did calls this between the two; one that only asserts the press was
/// handled need not.
pub fn answer_controls(widget: &mut dyn Widget) -> usize {
    let mut doc = AutoCommit::new();
    let mut state = Edits::lend(&mut doc);
    crate::widget::controls::start_pending_control_invocations(widget, &mut state, &automerge::ROOT)
}

/// The context [`Dispatch::send`] lends a widget, scratch unless set.
pub struct Dispatch<'a> {
    scale_factor: f64,
    modifiers: Modifiers,
    doc: Option<&'a mut AutoCommit>,
    path: Option<&'a ObjId>,
    layout: Option<(&'a TaffyTree<()>, NodeId)>,
    handled: bool,
    focus_target: Option<NodeId>,
}

/// What one sent event left behind.
#[derive(Debug)]
pub struct Dispatched {
    pub handled: bool,
    pub focus_target: Option<NodeId>,
}

impl Default for Dispatch<'_> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> Dispatch<'a> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            scale_factor: 1.0,
            modifiers: Modifiers::default(),
            doc: None,
            path: None,
            layout: None,
            handled: false,
            focus_target: None,
        }
    }

    /// Hold `modifiers` while the event is handled.
    #[must_use]
    pub fn modifiers(mut self, modifiers: impl Into<Modifiers>) -> Self {
        self.modifiers = modifiers.into();
        self
    }

    #[must_use]
    pub fn scale_factor(mut self, scale_factor: f64) -> Self {
        self.scale_factor = scale_factor;
        self
    }

    /// Lend `doc` instead of a scratch document.
    #[must_use]
    pub fn doc(mut self, doc: &'a mut AutoCommit) -> Self {
        self.doc = Some(doc);
        self
    }

    /// The document path the widget sees; the root by default.
    #[must_use]
    pub fn path(mut self, path: &'a ObjId) -> Self {
        self.path = Some(path);
        self
    }

    /// Handle the event at `node` of a laid-out tree instead of a scratch leaf.
    #[must_use]
    pub fn layout(mut self, taffy: &'a TaffyTree<()>, node: NodeId) -> Self {
        self.layout = Some((taffy, node));
        self
    }

    /// Start each send as already handled, as if an earlier handler took it.
    #[must_use]
    pub fn handled(mut self, handled: bool) -> Self {
        self.handled = handled;
        self
    }

    /// Start each send with this focus target instead of none.
    #[must_use]
    pub fn focus_target(mut self, focus_target: Option<NodeId>) -> Self {
        self.focus_target = focus_target;
        self
    }

    /// Send `event` to `widget`; reusable for a sequence of events.
    pub fn send<W: Widget + ?Sized>(&mut self, widget: &mut W, event: &WindowEvent) -> Dispatched {
        let mut scratch_taffy = TaffyTree::<()>::new();
        let (taffy, node_id) = if let Some(layout) = self.layout {
            layout
        } else {
            let node = scratch_taffy.leaf_node(Style::default());
            (&scratch_taffy, node)
        };
        let mut scratch_doc = AutoCommit::new();
        let doc = match self.doc.as_deref_mut() {
            Some(doc) => doc,
            None => &mut scratch_doc,
        };
        let mut handled = self.handled;
        let mut focus_target = self.focus_target;
        widget.handle_event(
            event,
            &mut EventCtx {
                scale_factor: self.scale_factor,
                modifiers: &self.modifiers,
                state: Edits::lend(doc),
                path: self.path.unwrap_or(&automerge::ROOT),
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id,
                taffy,
            },
        );
        Dispatched {
            handled,
            focus_target,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::Button;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn answer_controls_runs_the_action_a_press_published() {
        let fired = Rc::new(Cell::new(0_u32));
        let count = Rc::clone(&fired);
        let mut button = Button::new("answer", "Answer").on_view(move || {
            count.set(count.get() + 1);
        });
        let mut doc = AutoCommit::new();
        let activated = Widget::perform_action(
            &mut button,
            &crate::a11y::Action::Activate,
            &[],
            &mut Edits::lend(&mut doc),
            &automerge::ROOT,
        );
        assert!(activated, "the button takes the activation");
        assert_eq!(fired.get(), 0, "a published press runs when it is answered");
        assert_eq!(answer_controls(&mut button), 1);
        assert_eq!(fired.get(), 1);
        assert_eq!(answer_controls(&mut button), 0, "answered once");
    }
}
