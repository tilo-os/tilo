use super::*;
use crate::widget::ModelPath;
use crate::widget::TaffyTreeExt;
use crate::widget::{Button, Text, TextInput};
use automerge::AutoCommit;
use vello::kurbo::Rect;

/// Recreates its children on every `build_layout`: the focused
/// `TextInput` is a brand-new instance after rebuild, so its caret
/// resets unless the App restores it via `get_state`/`set_state`.
struct RebuildHarness {
    show_status: bool,
    show_offer: bool,
    request_reset: bool,
    /// Keep the children across layouts instead of recreating them.
    keep_children: bool,
    /// The field the rebuild makes answers to a name, so a rebuilt tree can
    /// hold the answer the person was on.
    named_field: bool,
    children: Vec<Box<dyn Widget>>,
    rect: Rect,
}

impl RebuildHarness {
    fn new() -> Self {
        Self {
            show_status: false,
            show_offer: false,
            request_reset: false,
            keep_children: false,
            named_field: false,
            children: Vec::new(),
            rect: Rect::ZERO,
        }
    }

    /// Two named fields that survive layouts.
    fn two_fields() -> Self {
        let mut this = Self::new();
        this.keep_children = true;
        this.children = vec![
            Box::new(TextInput::local().with_id("first")),
            Box::new(TextInput::local().with_id("second")),
        ];
        this
    }

    fn rebuild_children(&mut self) {
        if self.keep_children {
            return;
        }
        let mut kids: Vec<Box<dyn Widget>> = Vec::new();
        // `with_initial_value` parks the caret at the END ("hello" -> 5),
        // so a restored caret of 3 is distinguishable from both the
        // reset-to-0 bug and the new instance's own default.
        let field = TextInput::local().with_initial_value("hello");
        kids.push(Box::new(if self.named_field {
            field.with_id("only-field")
        } else {
            field
        }));
        if self.show_status {
            kids.push(Box::new(Text::new("status")));
        }
        if self.show_offer {
            kids.push(Box::new(Button::new("offer.skip", "Not now")));
            kids.push(Box::new(
                Button::new("offer.accept", "Turn on").with_default_focus(),
            ));
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
        taffy.widget_node(taffy::Style::default(), &child_nodes, node_map, self)
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        if let Ok(child_nodes) = taffy.children(node) {
            for (c, &cn) in self.children.iter_mut().zip(child_nodes.iter()) {
                c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
            }
        }
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
    fn on_external_event(
        &mut self,
        payload: &str,
        _s: &mut crate::widget::Edits<'_>,
        _r: &ModelPath,
    ) {
        match payload {
            "show_status" => self.show_status = true,
            "reset" => self.request_reset = true,
            // Like a form that re-creates its controls: the fields change
            // places and the one that had the keyboard takes it back by name.
            "swap" => {
                self.children.reverse();
                for child in &mut self.children {
                    child.set_focus(child.test_id() == Some("first"));
                }
            }
            "offer" => {
                self.show_offer = true;
                self.request_reset = true;
            }
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
        app.root_widget.on_external_event(
            "show_status",
            &mut crate::widget::Edits::lend(&mut tmp),
            &automerge::ROOT,
        );
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
        app.root_widget.on_external_event(
            "reset",
            &mut crate::widget::Edits::lend(&mut tmp),
            &automerge::ROOT,
        );
        app.rebuild_structure();

        assert_eq!(
            caret(&app).as_deref(),
            Some("5"),
            "focus_reset must not paste the old caret onto the new field"
        );
    }
}

#[test]
fn a_focus_reset_lands_on_the_default_control() {
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(RebuildHarness::new()));
    app.autofocus_first = true;
    app.autofocus_on_structure_change = true;
    app.init_layout();

    let mut tmp = AutoCommit::new();
    app.root_widget.on_external_event(
        "offer",
        &mut crate::widget::Edits::lend(&mut tmp),
        &automerge::ROOT,
    );
    app.rebuild_structure();

    assert_eq!(app.focusable_order.len(), 3);
    let focused = app.focused_node.expect("a reset focuses a control");
    assert_ne!(
        Some(focused),
        app.focusable_order.first().copied(),
        "the default control wins over the first one"
    );
    assert!(
        app.inspect_node(focused, |widget| widget.is_default_keyboard_focus())
            .unwrap_or(false)
    );
}

#[test]
fn a_rebuild_that_restores_focus_by_identity_leaves_one_focused_field() {
    let mut app =
        App::new_with_controlled_inputs(State::new(), Box::new(RebuildHarness::two_fields()));
    app.autofocus_first = true;
    app.autofocus_on_structure_change = true;
    app.init_layout();
    let focused_id = |app: &App| {
        let node = app.focused_node.expect("a field has the keyboard");
        app.inspect_node(node, |widget| widget.test_id().map(str::to_owned))
            .unwrap()
    };
    assert_eq!(focused_id(&app).as_deref(), Some("first"));

    let mut tmp = AutoCommit::new();
    app.root_widget.on_external_event(
        "swap",
        &mut crate::widget::Edits::lend(&mut tmp),
        &automerge::ROOT,
    );
    app.rebuild_structure();

    assert_eq!(
        focused_id(&app).as_deref(),
        Some("first"),
        "the field that restored its own focus keeps it, not the one in the old slot"
    );
    let flagged: Vec<_> = app
        .root_widget
        .children()
        .into_iter()
        .filter(|child| child.is_focused())
        .filter_map(|child| child.test_id().map(str::to_owned))
        .collect();
    assert_eq!(flagged, ["first"], "exactly one field is drawn as focused");
}

/// A rebuild replaces the focused control with a fresh instance of it. The
/// keyboard stays on the answer the person was on: the field that answers to
/// the same name and stands alone in the new tree holds it, without the app
/// asking to be refocused on every change.
#[test]
fn a_rebuild_that_replaces_the_focused_control_keeps_the_keyboard_on_its_answer() {
    let mut harness = RebuildHarness::new();
    harness.named_field = true;
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(harness));
    app.autofocus_first = true;
    app.init_layout();
    let focused = app.focused_node.expect("the field has the keyboard");
    assert!(
        app.inspect_node(focused, |widget| widget.test_id() == Some("only-field"))
            .unwrap()
    );

    let mut tmp = AutoCommit::new();
    app.root_widget.on_external_event(
        "show_status",
        &mut crate::widget::Edits::lend(&mut tmp),
        &automerge::ROOT,
    );
    app.rebuild_structure();

    let focused = app
        .focused_node
        .expect("the keyboard stays on the field's answer");
    assert!(
        app.inspect_node(focused, |widget| widget.test_id() == Some("only-field"))
            .unwrap(),
        "the rebuilt field is the one that had the keyboard"
    );
    assert!(
        app.inspect_node(focused, |widget| widget.is_focused())
            .unwrap(),
        "the control drawn as focused is the one with the keyboard"
    );
}
