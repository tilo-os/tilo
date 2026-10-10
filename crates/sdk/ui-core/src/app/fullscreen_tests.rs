use super::*;
use crate::widget::TaffyTreeExt;
use vello::kurbo::Rect;

use std::cell::RefCell;
use std::rc::Rc;

/// Lets the App's fullscreen reconcile wiring be exercised without a
/// live window.
struct FsRoot {
    want: Option<bool>,
    rect: Rect,
    changes: Rc<RefCell<Vec<bool>>>,
}

impl FsRoot {
    fn new(want: Option<bool>) -> (Self, Rc<RefCell<Vec<bool>>>) {
        let changes = Rc::new(RefCell::new(Vec::new()));
        (
            Self {
                want,
                rect: Rect::ZERO,
                changes: changes.clone(),
            },
            changes,
        )
    }
}

impl crate::widget::FullscreenProvider for FsRoot {
    fn fullscreen_intent(&self) -> Option<bool> {
        self.want
    }

    fn on_fullscreen_changed(&mut self, active: bool) {
        self.changes.borrow_mut().push(active);
    }
}

impl Widget for FsRoot {
    crate::provides!(dyn crate::widget::FullscreenProvider);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(taffy::Style::default(), node_map, self)
    }
    fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: Point) {}
    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _tr: &mut TextRenderer,
        _theme: &Theme,
        _p: &PresenceCtx,
    ) {
    }
    fn name(&self) -> &'static str {
        "FsRoot"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[test]
fn fullscreen_decisions_and_transitions_notify_and_relayout() {
    // Case `decision_truth_table`.
    {
        assert_eq!(fullscreen_request_decision(false, None), None);
        assert_eq!(fullscreen_request_decision(true, None), None);
        assert_eq!(fullscreen_request_decision(false, Some(false)), None);
        assert_eq!(fullscreen_request_decision(true, Some(true)), None);
        assert_eq!(fullscreen_request_decision(false, Some(true)), Some(true));
        assert_eq!(fullscreen_request_decision(true, Some(false)), Some(false));
    }

    // Case `actual_state_transitions_notify_root_once_per_edge`.
    {
        let (root, changes) = FsRoot::new(None);
        let mut app = App::new(State::new(), Box::new(root));

        assert!(app.note_actual_fullscreen(true));
        assert!(!app.note_actual_fullscreen(true));
        assert!(app.note_actual_fullscreen(false));

        assert_eq!(*changes.borrow(), vec![true, false]);
        assert!(!app.window.fullscreen_active);
    }

    // Case `entering_fullscreen_marks_resync_and_relayout`.
    {
        let (root, _changes) = FsRoot::new(Some(true));
        let mut app = App::new(State::new(), Box::new(root));
        app.needs_sync = false;
        app.layout.layout_dirty = false;
        assert!(app.note_actual_fullscreen(true));
        assert!(app.needs_sync);
        assert!(app.layout.layout_dirty);
    }
}
