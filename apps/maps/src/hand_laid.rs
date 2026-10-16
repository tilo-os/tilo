//! Driving the toolkit controls the map places by hand (the From/To fields,
//! the offline sheet's field and stepper, the overflow menu) with the event's
//! own document.

use taffy::{NodeId, Style, TaffyTree};
use ui::widget::Widget;
use winit::event::WindowEvent;

/// Drive one event through `widget`, a control laid out by hand rather than
/// by the widget tree, against `doc`, the document the event came with.
/// Returns whether the control handled it.
pub(crate) fn drive(
    widget: &mut dyn Widget,
    event: &WindowEvent,
    scale: f64,
    modifiers: winit::event::Modifiers,
    doc: &mut ui::widget::Edits<'_>,
) -> bool {
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let Ok(node) = taffy.new_leaf(Style::default()) else {
        return false;
    };
    let mut handled = false;
    let mut focus_target: Option<NodeId> = None;
    widget.handle_event(
        event,
        &mut ui::widget::EventCtx {
            scale_factor: scale,
            modifiers: &modifiers,
            state: doc.reborrow(),
            path: &automerge::ROOT,
            handled: &mut handled,
            focus_target: &mut focus_target,
            node_id: node,
            taffy: &taffy,
        },
    );
    handled
}
