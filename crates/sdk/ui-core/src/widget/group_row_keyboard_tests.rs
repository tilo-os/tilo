//! Keys reach the window with the keyboard, never a window beside it: each
//! app reads every key it is handed as its own, as a window of its own does.

use super::*;
use crate::headless::FrameHarness;
use std::cell::Cell;
use std::rc::Rc;

/// An app that takes every key it is handed, as Chat does.
struct EveryKey {
    keys: Rc<Cell<u32>>,
    focused: bool,
}

impl Widget for EveryKey {
    fn name(&self) -> &str {
        "EveryKey"
    }
    fn is_focusable(&self) -> bool {
        true
    }
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == ElementState::Pressed
        {
            self.keys.set(self.keys.get() + 1);
            *ctx.handled = true;
        }
    }
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        _: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        tree.widget_leaf(Style::default(), nodes, self)
    }
}

impl crate::widget::AppWidget for EveryKey {
    fn app_kind(&self) -> crate::widget::AppKind {
        crate::widget::AppKind::Editor
    }
    fn app_title(&self) -> &str {
        "Every key"
    }
    fn app_icon(&self) -> crate::widget::IconDescriptor {
        crate::widget::IconDescriptor::Glyph("K")
    }
}

/// Two windows side by side, each an app that takes every key; the second
/// is the focused window. Each window's control holds the keyboard as given.
fn two_windows(holds: [bool; 2]) -> (FrameHarness, [Rc<Cell<u32>>; 2]) {
    let keys = [Rc::new(Cell::new(0)), Rc::new(Cell::new(0))];
    let groups = (0..2u64)
        .map(|i| {
            let tab = i + 1;
            let mut group = WindowGroup::new(
                (i + 1) * 100,
                vec![(
                    TabDescriptor::new(tab, format!("T{tab}")),
                    TileContent::App(Box::new(EveryKey {
                        keys: Rc::clone(&keys[usize::from(i == 1)]),
                        focused: holds[usize::from(i == 1)],
                    })),
                )],
                tab,
                WindowWidth::Normal,
            );
            let _ = group.set_focused(i == 1);
            group
        })
        .collect();
    let mut harness = FrameHarness::for_root(
        crate::State::new(),
        GroupRow::new(groups),
        (1200, 800),
        Theme::dark(),
    );
    harness.relayout();
    (harness, keys)
}

#[test]
fn a_letter_reaches_the_focused_window_not_the_one_before_it() {
    let (mut harness, [first, second]) = two_windows([false, false]);
    harness.key("r");
    assert_eq!(
        first.get(),
        0,
        "the window beside the focused one is not handed the key"
    );
    assert_eq!(second.get(), 1, "the focused window's app takes it");
}

#[test]
fn a_letter_follows_the_focused_control_into_its_window() {
    // The control holding the keyboard is the first window's, before the
    // desktop has moved its own focus there.
    let (mut harness, [first, second]) = two_windows([true, false]);
    harness.key("r");
    assert_eq!(first.get(), 1);
    assert_eq!(second.get(), 0);
}
