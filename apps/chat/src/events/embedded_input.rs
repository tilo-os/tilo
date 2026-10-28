//! Driving the widgets Chat lays out by hand (its composers and retained
//! controls) with one raw event, outside the taffy tree they would
//! normally be dispatched through.

use taffy::{NodeId, Style, TaffyTree};
use winit::event::{Modifiers, WindowEvent};

use ui::Widget;

/// Feed one raw winit event into an embedded [`TextInput`]. The composers live
/// outside the taffy tree (the app lays them out by hand), so a throwaway leaf
/// node satisfies the `Widget::handle_event` signature; the input only uses it
/// to report a focus target, which the composer host maps to its retained owner.
pub(crate) fn drive_text_input(
    ti: &mut impl Widget,
    event: &WindowEvent,
    scale: f64,
    modifiers: Modifiers,
    doc: &mut ui::widget::Edits<'_>,
) -> bool {
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let Ok(node) = taffy.new_leaf(Style::default()) else {
        return false;
    };
    let mut handled = false;
    let mut focus_target: Option<NodeId> = None;
    ti.handle_event(
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
    focus_target.is_some()
}

/// Drive a retained shared control from Chat's immediate-mode event host.
/// The temporary layout node only satisfies `Widget::handle_event`; menu
/// geometry is established synchronously by its owner before dispatch.
pub(super) fn drive_retained_control(
    menu: &mut dyn ui::Widget,
    event: &WindowEvent,
    scale: f64,
    modifiers: Modifiers,
    doc: &mut ui::widget::Edits<'_>,
) -> bool {
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let Ok(node) = taffy.new_leaf(Style::default()) else {
        return false;
    };
    let mut handled = false;
    let mut focus_target = None;
    menu.handle_event(
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
