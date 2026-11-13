//! Driving the toolkit controls the map places by hand, with the event's own document: the From/To
//! fields, the offline sheet's field and stepper, the overflow menu.

use taffy::{NodeId, Style, TaffyTree};
use ui::widget::Widget;
use winit::event::WindowEvent;

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
