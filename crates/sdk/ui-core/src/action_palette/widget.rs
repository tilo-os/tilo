//! The palette as the widget tree sees it.

use super::{
    ActionPalette, Affine, ConfirmState, HashMap, ModelPath, NodeId, Point, PresenceCtx, Rect,
    Scene, Size, Style, TaffyTree, TextRenderer, Theme, Widget, WindowEvent,
};
use crate::i18n::widget_text;
use crate::widget::SearchField;

/// What names the palette's field and what its description adds, whichever
/// rows the palette is over: a step collecting an action's argument is asked
/// in the action's own words, which a description would only repeat; a query
/// is the search field it is, and what the query covers is its description.
pub(crate) fn field_announcement(
    field: &SearchField,
    question: Option<&str>,
) -> (String, Option<String>) {
    match question
        .map(|question| widget_text(question))
        .filter(|question| !question.is_empty())
    {
        Some(question) => (question, None),
        None => field.query_name_and_description(),
    }
}

impl Widget for ActionPalette {
    fn needs_redraw(&self) -> bool {
        crate::widget::redraw_or_children(self.dirty, self)
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
        crate::widget::Widget::mark_children_drawn(self);
    }
    fn name(&self) -> &str {
        "ActionPalette"
    }
    /// The registry key a host resolves the card by: the query field's rect,
    /// as the shell's overlay hosts look the search card up.
    fn test_id(&self) -> Option<&str> {
        if self.hosts_rows() {
            return Some("command_palette_input");
        }
        Some("action-palette")
    }
    fn resets_binding_inheritance(&self) -> bool {
        true
    }
    fn is_focusable(&self) -> bool {
        self.is_presented()
    }
    fn is_focused(&self) -> bool {
        self.is_presented()
            && (self.field.is_focused()
                || self
                    .confirm
                    .as_ref()
                    .and_then(ConfirmState::phrase_field)
                    .is_some_and(|field| field.is_focused())
                || self
                    .controls
                    .children()
                    .iter()
                    .any(|control| control.is_focused()))
    }
    fn set_focus(&mut self, focused: bool) {
        self.dirty = true;
        if self.hosts_rows() {
            self.open = focused;
            self.results.set_focused(focused);
            self.sync_field_focus();
            return;
        }
        if focused {
            if !self.is_focused() {
                self.focus_part(self.confirm.as_ref().map_or(0, ConfirmState::first_part));
            }
        } else {
            self.field.set_focused(false);
            if let Some(field) = self
                .confirm
                .as_mut()
                .and_then(ConfirmState::phrase_field_mut)
            {
                field.set_focus(false);
            }
            for control in self.controls.children_mut() {
                control.set_focus(false);
            }
        }
    }
    fn traps_focus(&self) -> bool {
        self.is_presented()
    }
    fn modal_capture(&self) -> Option<Rect> {
        self.is_presented().then_some(self.rect)
    }
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        use crate::widget::TaffyTreeExt;
        if self.hosts_rows() {
            self.host_prepare(theme);
            // `desired_size` is the single source of truth so a hand-placed
            // card (`set_rect`) and a laid-out one agree.
            let (width, height) = self.desired_size();
            let style = Style {
                size: Size::from_lengths(width, height),
                ..Style::default()
            };
            return tree.widget_leaf(style, nodes, self);
        }
        let node = tree.leaf_node(Style {
            position: taffy::Position::Absolute,
            ..Style::default()
        });
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        if !self.hosts_rows() {
            return;
        }
        self.rect = crate::widget::layout_rect(tree, node, origin);
        self.sync_list();
        self.place_field();
    }
    fn take_structure_dirty(&mut self) -> bool {
        if !self.hosts_rows() {
            return false;
        }
        std::mem::take(&mut self.structure_dirty)
    }
    /// Sized by its style alone: nothing a host row changes needs taffy to
    /// measure it again.
    fn is_layout_dirty(&self) -> bool {
        !self.hosts_rows()
    }
    fn set_platform_conventions(&mut self, conventions: crate::platform::PlatformConventions) {
        if self.hosts_rows() {
            self.set_host_platform(conventions);
        }
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.hosts_rows() {
            self.draw_host(scene, transform, text, theme, presence);
            return;
        }
        ActionPalette::draw(self, scene, transform, text, theme, presence);
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        self.dirty = true;
        *ctx.handled |= self.route_event(
            event,
            ctx.scale_factor,
            *ctx.modifiers,
            &mut ctx.state,
            ctx.path,
        );
    }
    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.hosts_rows()
            && self.is_open()
            && (self.row_at(point).is_some() || self.facet_chip_at(point).is_some())
        {
            return Some(winit::window::CursorIcon::Pointer);
        }
        None
    }
    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.hosts_rows()
            .then(|| self.field.ime_cursor_rect())
            .flatten()
    }
    fn ime_content(&self) -> Option<crate::ime::InputContent> {
        self.hosts_rows()
            .then(|| self.field.ime_content())
            .flatten()
    }
    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        if !self.hosts_rows() {
            return None;
        }
        self.host_ime_fingerprint(key)
    }
    fn accessibility_hidden(&self) -> bool {
        self.hosts_rows() && !self.is_open()
    }
    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        if !self.hosts_rows() {
            return None;
        }
        self.host_synthetic_rect(path)
    }
    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        if !self.hosts_rows() {
            return false;
        }
        self.host_focus_synthetic(path)
    }
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        if self.hosts_rows() {
            return Some(self.host_accessibility());
        }
        if !self.is_presented() {
            return None;
        }
        let mut children = Vec::new();
        if self.confirm.is_none()
            && let Some(field) = self.field.accessibility()
        {
            let target = self.target("field");
            // A step collecting an action's argument is asked in the action's
            // own words; the query is the search field it is, the empty
            // field's words saying what it covers.
            let (name, description) = field_announcement(
                &self.field,
                self.in_argument_mode().then(|| self.field.placeholder()),
            );
            children.push(
                field
                    .with_name(name)
                    .with_optional_description(description)
                    .with_hashed_key(&target)
                    .with_action_target(target)
                    .with_bounds(self.field.get_rect()),
            );
        }
        // A name to type is a field called by its visible label.
        if let Some(field) = self.confirm.as_ref().and_then(ConfirmState::phrase_field)
            && let Some(node) = field.accessibility()
        {
            let target = self.target("field");
            children.push(
                node.with_hashed_key(&target)
                    .with_action_target(target)
                    .with_bounds(field.get_rect()),
            );
        }
        if self.confirm.is_some() || self.history_review_required {
            for (button, part) in self
                .controls
                .children()
                .into_iter()
                .zip(["submit", "cancel"])
            {
                if let Some(node) = button.accessibility() {
                    let target = self.target(part);
                    children.push(
                        node.with_hashed_key(&target)
                            .with_action_target(target)
                            .with_bounds(
                                self.confirmation_sheet
                                    .layout()
                                    .filter(|_| self.confirm.is_some())
                                    .and_then(|layout| {
                                        layout.actions.get(usize::from(part != "submit"))
                                    })
                                    .copied()
                                    .unwrap_or_else(|| button.get_rect()),
                            ),
                    );
                }
            }
        }
        if !self.in_argument_mode()
            && !self.history_review_required
            && let Some(results) = self.results.accessibility()
        {
            children.push(results.with_hashed_key(self.target("results")));
        }
        let confirm = self.confirm.as_ref();
        let description = confirm.map_or("", |confirm| confirm.detail.as_str());
        // The palette names the step it is on: the question it asks, or — while
        // it collects the arguments of an action — that action, and itself
        // otherwise.
        let name = match confirm {
            Some(confirm) => confirm.question.clone(),
            None if self.in_argument_mode() => {
                crate::i18n::widget_format("palette-argument-name", &[])
            }
            None => crate::i18n::widget_format("palette-name", &[]),
        };
        Some(
            crate::a11y::AccessNode::new(crate::a11y::Role::Dialog)
                .with_name(name)
                .with_description(description)
                .with_state(
                    crate::a11y::AccessState::new()
                        .modal(true)
                        .focused(self.is_focused()),
                )
                .with_children(children),
        )
    }
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        doc: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        if self.hosts_rows() {
            return self.host_semantic_action(target, action, doc, path);
        }
        if !self.is_presented() {
            return false;
        }
        if target == self.target("field") {
            if self.confirm.is_none() {
                if matches!(action, crate::a11y::Action::Focus) {
                    self.focus_part(0);
                }
                return self.field.perform_action(action, &[], doc, path);
            }
            if self.confirm.as_ref().is_some_and(ConfirmState::typed) {
                if matches!(action, crate::a11y::Action::Focus) {
                    self.focus_part(0);
                }
                let handled = self
                    .confirm
                    .as_mut()
                    .and_then(ConfirmState::phrase_field_mut)
                    .is_some_and(|field| field.input_mut().perform_action(action, &[], doc, path));
                self.sync_confirmation();
                return handled;
            }
        }
        if !self.in_argument_mode() && !self.history_review_required {
            let handled = self
                .results
                .perform_semantic_action(target, action, doc, path);
            self.poll_view_activation();
            return handled;
        }
        if self.confirm.is_none() && !self.history_review_required {
            return false;
        }
        for (index, part) in ["submit", "cancel"].into_iter().enumerate() {
            if target == self.target(part) {
                if matches!(action, crate::a11y::Action::Focus) {
                    self.focus_part(index + 1);
                }
                let handled =
                    self.controls.children_mut()[index].perform_action(action, &[], doc, path);
                let (doc, path) = crate::doc::document(doc, path);
                self.drain_command(doc, path);
                return handled;
            }
        }
        false
    }
}
