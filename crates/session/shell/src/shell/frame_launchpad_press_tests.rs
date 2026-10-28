//! A press on the empty workspace's cards as the compositor delivers one: a
//! tap becomes move, press, release and leave in one batch, with no frame
//! between them, and the press is what moves keyboard focus to the cards.

use super::*;

use automerge::{AutoCommit, ROOT, ReadDoc as _};
use winit::event::{ElementState, MouseButton};

fn button(state: ElementState) -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        state,
        button: MouseButton::Left,
    }
}

fn is_launchpad(widget: *mut dyn ui::widget::Widget) -> bool {
    // SAFETY: the pointer belongs to the frame's current, live layout tree.
    unsafe { (*widget).name() == "CollaborationLaunchpad" }
}

#[test]
fn a_press_that_focuses_the_cards_still_opens_the_card() {
    let mut fb = FrameBuilder::new((1280, 800));
    // An empty workspace, which offers its cards.
    fb.with_chrome(|st| {
        let mut tree = st.tree_mut();
        let workspace = tree.desktop.active_workspace_mut().expect("a workspace");
        for strip in &mut workspace.strips {
            strip.window_groups.clear();
        }
        drop(tree);
        st.bump();
    });
    fb.relayout();
    // The cards are placed as they are drawn.
    let _ = fb.paint_with_damage();
    let registry = fb.widget_registry();
    let card = registry
        .get("launchpad:notes")
        .copied()
        .unwrap_or_else(|| {
            let mut names: Vec<_> = registry.keys().collect();
            names.sort();
            panic!("the empty workspace offers a Notes card: {names:?}")
        })
        .center();
    let launchpad = fb
        .node_map
        .iter()
        .find_map(|(&node, &widget)| is_launchpad(widget).then_some(node))
        .expect("the cards are a node of the live tree");
    // The keyboard is somewhere else on the desktop, as after any other
    // check: the press on the card is what moves it to the cards.
    let elsewhere = fb
        .focusable_order()
        .into_iter()
        .find(|&node| node != launchpad)
        .expect("the desktop has another focusable control");
    let mut focus = None;
    fb.set_native_focus(&mut focus, Some(elsewhere));
    let mut doc = AutoCommit::new();
    for event in [
        ui::synthetic_input::cursor_moved(card.x, card.y),
        button(ElementState::Pressed),
        button(ElementState::Released),
        WindowEvent::CursorLeft {
            device_id: winit::event::DeviceId::dummy(),
        },
    ] {
        fb.dispatch_event(
            &event,
            &winit::event::Modifiers::default(),
            &mut ui::widget::Edits::lend(&mut doc),
            &ROOT,
            &mut focus,
        );
    }
    assert_eq!(focus, Some(launchpad), "the press focused the cards");
    assert!(
        matches!(
            doc.get(&ROOT, tilo_desktop::launchpad::LaunchpadAction::Notes.path()),
            Ok(Some((automerge::Value::Scalar(value), _)))
                if matches!(value.as_ref(), automerge::ScalarValue::Boolean(true))
        ),
        "the press and release on the Notes card asked for Notes"
    );
}
