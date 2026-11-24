//! A footer action's focus node: its place in the window's keyboard focus order and its
//! button in the accessibility tree.

use crate::i18n::widget_text;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::{
    DialogAction, ModelPath, NodeId, Rect, Slot, Style, TaffyTree, TaffyTreeExt, Theme, Widget,
};

/// A footer action's place in the window's keyboard focus order and its button in the
/// accessibility tree.
pub(super) struct FooterKey {
    pub(super) slot: Slot,
    pub(super) label: String,
    pub(super) run: Option<Rc<RefCell<dyn FnMut()>>>,
    pub(super) action_target: String,
    pub(super) rect: Rect,
    pub(super) focused: bool,
    pub(super) enabled: bool,
    pub(super) test_id: String,
}

impl FooterKey {
    pub(super) fn new(
        slot: Slot,
        action: Option<&DialogAction>,
        dialog: Option<&str>,
        target: String,
    ) -> Self {
        let name = match slot {
            Slot::Primary => "primary",
            Slot::Secondary => "secondary",
            Slot::Tertiary => "tertiary",
        };
        Self {
            slot,
            label: action.map(|a| a.label.clone()).unwrap_or_default(),
            run: action.map(|a| Rc::clone(&a.on_click)),
            action_target: target,
            rect: Rect::ZERO,
            focused: false,
            enabled: true,
            test_id: format!("{}.{name}", dialog.unwrap_or("modal")),
        }
    }
}

impl Widget for FooterKey {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            position: taffy::Position::Absolute,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn name(&self) -> &'static str {
        "ModalAction"
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let mut state = AccessState::new().focusable(self.enabled);
        state.focused = self.focused;
        state.disabled = !self.enabled;
        Some(
            AccessNode::new(Role::Button)
                .with_name(widget_text(&self.label))
                .with_semantic_key(&self.test_id)
                .with_state(state)
                .with_action_target(self.action_target.clone()),
        )
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        _key_path: &[u64],
        _: &mut crate::widget::Edits<'_>,
        _: &ModelPath,
    ) -> bool {
        if !self.enabled || !matches!(action, crate::a11y::Action::Activate) {
            return false;
        }
        let Some(run) = &self.run else { return false };
        (run.borrow_mut())();
        true
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        target == self.action_target && self.perform_action(action, &[], state, path)
    }

    fn test_id(&self) -> Option<&str> {
        Some(&self.test_id)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn is_layout_dirty(&self) -> bool {
        false
    }

    fn needs_redraw(&self) -> bool {
        false
    }
}
