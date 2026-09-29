use super::*;
use crate::widget::{AppModel, ModelPath};
use crate::widget::{Text, TextInput};
use automerge::AutoCommit;
use vello::kurbo::Rect;

/// Recreates its children on every `build_layout`: the focused
/// `TextInput` is a brand-new instance after rebuild, so its caret
/// resets unless the App restores it via `get_state`/`set_state`.
struct RebuildHarness {
    show_status: bool,
    request_reset: bool,
    children: Vec<Box<dyn Widget>>,
    rect: Rect,
}

impl RebuildHarness {
    fn new() -> Self {
        Self {
            show_status: false,
            request_reset: false,
            children: Vec::new(),
            rect: Rect::ZERO,
        }
    }

    fn rebuild_children(&mut self) {
        let mut kids: Vec<Box<dyn Widget>> = Vec::new();
        // `with_initial_value` parks the caret at the END ("hello" -> 5),
        // so a restored caret of 3 is distinguishable from both the
        // reset-to-0 bug and the new instance's own default.
        kids.push(Box::new(TextInput::local().with_initial_value("hello")));
        if self.show_status {
            kids.push(Box::new(Text::new("status")));
        }
        self.children = kids;
    }
}

impl Widget for RebuildHarness {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.rebuild_children();
        let child_nodes: Vec<NodeId> = self
            .children
            .iter_mut()
            .map(|c| c.build_layout(taffy, theme, node_map))
            .collect();
        let node = taffy
            .new_with_children(taffy::Style::default(), &child_nodes)
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        if let Ok(child_nodes) = taffy.children(node) {
            for (c, &cn) in self.children.iter_mut().zip(child_nodes.iter()) {
                c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
            }
        }
    }
    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _tr: &mut TextRenderer,
        _theme: &Theme,
        _p: &PresenceCtx,
    ) {
    }
    fn children(&self) -> Vec<&dyn Widget> {
        self.children.iter().map(Box::as_ref).collect()
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.children
            .iter_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .collect()
    }
    fn on_external_event(&mut self, payload: &str, _s: &mut AppModel, _r: &ModelPath) {
        match payload {
            "show_status" => self.show_status = true,
            "reset" => self.request_reset = true,
            _ => {}
        }
    }
    fn take_focus_reset(&mut self) -> bool {
        std::mem::take(&mut self.request_reset)
    }
    fn name(&self) -> &'static str {
        "RebuildHarness"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}

fn caret(app: &App) -> Option<String> {
    let id = app.focused_node?;
    let w = *app.layout.node_map.get(&id)?;
    unsafe { crate::undo::get_state(&(*w)) }
}

#[test]
fn structure_rebuild_preserves_the_caret_unless_focus_resets() {
    // Case `caret_preserved_across_structure_rebuild`.
    {
        let mut app =
            App::new_with_controlled_inputs(State::new(), Box::new(RebuildHarness::new()));
        app.autofocus_first = true;
        app.autofocus_on_structure_change = true;
        app.init_layout();

        let id = app.focused_node.expect("input focused at startup");
        unsafe {
            crate::undo::set_state(&mut (*(*app.layout.node_map.get(&id).unwrap())), "3");
        }
        assert_eq!(caret(&app).as_deref(), Some("3"));

        // Triggers a structural rebuild that recreates the input as a fresh
        // instance whose own caret default is 5.
        let mut tmp = AutoCommit::new();
        app.root_widget
            .on_external_event("show_status", &mut tmp, &automerge::ROOT);
        app.rebuild_structure();

        assert!(app.focused_node.is_some(), "focus preserved across rebuild");
        assert_eq!(
            caret(&app).as_deref(),
            Some("3"),
            "caret offset must be preserved across the rebuild"
        );
        assert_eq!(
            app.root_widget.children().len(),
            2,
            "status sibling appeared"
        );
        assert_eq!(app.focusable_order.len(), 1, "status is non-focusable");
    }

    // Case `focus_reset_does_not_restore_caret`.
    {
        let mut app =
            App::new_with_controlled_inputs(State::new(), Box::new(RebuildHarness::new()));
        app.autofocus_first = true;
        app.autofocus_on_structure_change = true;
        app.init_layout();

        let id = app.focused_node.unwrap();
        unsafe {
            crate::undo::set_state(&mut (*(*app.layout.node_map.get(&id).unwrap())), "3");
        }

        let mut tmp = AutoCommit::new();
        app.root_widget
            .on_external_event("reset", &mut tmp, &automerge::ROOT);
        app.rebuild_structure();

        assert_eq!(
            caret(&app).as_deref(),
            Some("5"),
            "focus_reset must not paste the old caret onto the new field"
        );
    }
}
