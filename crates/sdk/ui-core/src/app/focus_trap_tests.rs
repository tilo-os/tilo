use super::*;
use crate::widget::modal_dialog::ModalDialog;
use crate::widget::{AppModel, ModelPath};
use crate::widget::{Button, Widget};
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

struct TwoKids {
    kids: Vec<Box<dyn Widget>>,
}
impl TwoKids {
    fn new() -> Self {
        Self {
            kids: vec![
                Box::new(Button::new("ok", "OK", None)),
                Box::new(Button::new("cancel", "Cancel", None)),
            ],
        }
    }
}
impl Widget for TwoKids {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let kids: Vec<_> = self
            .kids
            .iter_mut()
            .map(|c| c.build_layout(taffy, theme, node_map))
            .collect();
        let node = taffy
            .new_with_children(taffy::Style::default(), &kids)
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: Point) {}
    fn draw(
        &mut self,
        _s: &mut Scene,
        _t: Affine,
        _tr: &mut TextRenderer,
        _th: &Theme,
        _p: &PresenceCtx,
    ) {
    }
    fn children(&self) -> Vec<&dyn Widget> {
        self.kids.iter().map(Box::as_ref).collect()
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.kids
            .iter_mut()
            .map(|c| &mut **c as &mut (dyn Widget + '_))
            .collect()
    }
    fn name(&self) -> &'static str {
        "two-kids"
    }
    fn get_rect(&self) -> KurboRect {
        KurboRect::ZERO
    }
}

/// A shared `want_open` cell flips the modal's state in `sync`, modelling
/// a host that opens/closes without rebuilding the widget tree.
struct ModalHost {
    outer: Vec<Box<dyn Widget>>,
    modal: ModalDialog<TwoKids>,
    want_open: Rc<Cell<bool>>,
}
impl ModalHost {
    fn new(want_open: Rc<Cell<bool>>) -> Self {
        Self {
            outer: vec![
                Box::new(Button::new("a", "Open", None)),
                Box::new(Button::new("b", "Other", None)),
            ],
            modal: ModalDialog::new("Confirm", TwoKids::new()),
            want_open,
        }
    }
}
impl Widget for ModalHost {
    fn sync(&mut self, s: &mut AppModel, p: &ModelPath) {
        self.modal.set_open(self.want_open.get());
        for c in &mut self.outer {
            c.sync(s, p);
        }
        self.modal.sync(s, p);
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.modal.set_open(self.want_open.get());
        let mut kids: Vec<_> = self
            .outer
            .iter_mut()
            .map(|c| c.build_layout(taffy, theme, node_map))
            .collect();
        kids.push(self.modal.build_layout(taffy, theme, node_map));
        let node = taffy
            .new_with_children(taffy::Style::default(), &kids)
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: Point) {}
    fn draw(
        &mut self,
        _s: &mut Scene,
        _t: Affine,
        _tr: &mut TextRenderer,
        _th: &Theme,
        _p: &PresenceCtx,
    ) {
    }
    fn children(&self) -> Vec<&dyn Widget> {
        let mut v: Vec<&dyn Widget> = self.outer.iter().map(Box::as_ref).collect();
        v.push(&self.modal);
        v
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut v: Vec<&mut (dyn Widget + '_)> = self
            .outer
            .iter_mut()
            .map(|c| &mut **c as &mut (dyn Widget + '_))
            .collect();
        v.push(&mut self.modal as &mut (dyn Widget + '_));
        v
    }
    fn name(&self) -> &'static str {
        "modal-host"
    }
    fn get_rect(&self) -> KurboRect {
        KurboRect::ZERO
    }
}

/// Built closed; flipping the cell + calling `refresh_focus_order`
/// models the per-frame re-evaluation.
fn host_app() -> (App, Rc<Cell<bool>>) {
    let cell = Rc::new(Cell::new(false));
    let app = App::new_with_controlled_inputs(State::new(), Box::new(ModalHost::new(cell.clone())));
    (app, cell)
}

#[test]
fn modal_focus_trap_confines_cycles_restores_and_names_the_dialog() {
    // Case `container_overlay_traps_tab_in_its_controls`.
    {
        use crate::widget::{Column, Container};
        let root = Column::new()
            .push(Box::new(Button::new("background", "Background", None)))
            .push(Box::new(
                Container::new()
                    .with_focus_trap()
                    .with_child(Box::new(TwoKids::new())),
            ));
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
        app.init_layout();
        assert!(app.accessibility.focus_trap_active);
        assert_eq!(app.focusable_order.len(), 2);
        assert!(app.keyboard_nav_allowed(Some(KurboRect::new(0.0, 0.0, 100.0, 100.0))));
        let first = app.focusable_order[0];
        let second = app.focusable_order[1];
        app.set_focused_node(Some(first));
        assert!(app.focus_tab(false));
        assert_eq!(app.focused_node, Some(second));
        assert!(app.focus_tab(false));
        assert_eq!(app.focused_node, Some(first));
        assert!(app.focus_tab(true));
        assert_eq!(app.focused_node, Some(second));
    }

    // Case `closed_modal_does_not_trap_focus`.
    {
        let (mut app, _open) = host_app();
        app.init_layout();
        assert!(
            !app.accessibility.focus_trap_active,
            "no trap while the modal is closed"
        );
        assert_eq!(
            app.focusable_order.len(),
            2,
            "closed modal controls stay out of the background focus order"
        );
    }

    // Case `open_modal_confines_focus_order_to_its_subtree`.
    {
        let (mut app, open) = host_app();
        app.init_layout();
        let full = app.focusable_order.len();

        open.set(true);
        app.refresh_focus_order();

        assert!(
            app.accessibility.focus_trap_active,
            "an open modal traps focus"
        );
        assert_eq!(
            app.focusable_order.len(),
            2,
            "focus is confined to the modal's two buttons (was {full})"
        );
        assert!(
            app.focused_node
                .is_some_and(|id| app.focusable_order.contains(&id)),
            "focus moves into the modal when it opens"
        );
    }

    // Case `tab_cycles_within_the_trapped_modal_via_the_real_handler`.
    {
        let (mut app, open) = host_app();
        app.init_layout();
        open.set(true);
        app.refresh_focus_order();
        assert!(app.accessibility.focus_trap_active);
        assert_eq!(app.focusable_order.len(), 2);
        let modal_set: std::collections::HashSet<_> = app.focusable_order.iter().copied().collect();
        let first = app.focusable_order[0];
        let second = app.focusable_order[1];

        // Must be allowed EVEN while a modal captures the pointer, or the
        // dialog's buttons would be unreachable by keyboard.
        assert!(
            app.keyboard_nav_allowed(Some(KurboRect::new(0.0, 0.0, 10.0, 10.0))),
            "an active focus trap allows Tab to run (scoped to the modal)"
        );

        app.set_focused_node(Some(first));
        assert!(app.focus_tab(false), "Tab moves focus");
        assert_eq!(
            app.focused_node,
            Some(second),
            "Tab advances within the modal"
        );
        assert!(modal_set.contains(&second));
        assert!(app.focus_tab(false));
        assert_eq!(
            app.focused_node,
            Some(first),
            "Tab from the last wraps to the first"
        );
        assert!(app.focus_tab(true));
        assert_eq!(
            app.focused_node,
            Some(second),
            "Shift+Tab from the first wraps to the last"
        );
        assert!(
            modal_set.contains(&app.focused_node.unwrap()),
            "Shift+Tab stays inside the modal subtree"
        );
    }

    // Case `pointer_capture_without_a_trap_suppresses_app_tab`.
    {
        // A pointer-capturing, non-trapping surface (a context menu) must
        // switch off the App's own Tab/arrow navigation.
        let (mut app, _open) = host_app();
        app.init_layout();
        assert!(!app.accessibility.focus_trap_active);
        assert!(
            app.keyboard_nav_allowed(None),
            "with no modal, Tab runs normally"
        );
        assert!(
            !app.keyboard_nav_allowed(Some(KurboRect::new(0.0, 0.0, 10.0, 10.0))),
            "a pointer-capturing surface with no focus trap suppresses App-level Tab"
        );
    }

    // Case `closing_modal_restores_prior_focus`.
    {
        let (mut app, open) = host_app();
        app.init_layout();
        let outer = app.focusable_order[0];
        app.set_focused_node(Some(outer));

        open.set(true);
        app.refresh_focus_order();
        assert!(app.accessibility.focus_trap_active);
        assert_ne!(app.focused_node, Some(outer), "focus moved into the modal");

        open.set(false);
        app.refresh_focus_order();
        assert!(
            !app.accessibility.focus_trap_active,
            "trap released when the modal closes"
        );
        assert_eq!(
            app.focused_node,
            Some(outer),
            "focus is restored to where it was before the modal opened"
        );
    }

    // Case `modal_exposes_dialog_role_with_title_as_name`.
    {
        use crate::a11y::Role;
        let m = ModalDialog::new("Delete file?", TwoKids::new()).open(true);
        let node = Widget::accessibility(&m).expect("an open dialog is in the tree");
        assert_eq!(node.role, Role::Dialog);
        assert_eq!(node.name.as_deref(), Some("Delete file?"));
        assert!(node.state.modal, "an open modal traps focus → modal flag");
        assert!(!node.state.hidden);
    }
}
