//! The palette as the widget tree sees it.

use super::{
    ActionPalette, Affine, ConfirmState, HashMap, ModelPath, NodeId, Point, PresenceCtx, Rect,
    Scene, Size, Style, TaffyTree, TextRenderer, Theme, Widget, WindowEvent,
};
use crate::i18n::widget_text;
use crate::widget::SearchField;

/// What names the palette's field and what its description adds, whichever rows the palette is over: a
/// step collecting an action's argument is asked in the action words, which a description would only
/// repeat; a query is the search field it is, and what the query covers is its description.
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
    /// The palette a page shows: a dialog with the field the person types in, and the rows it
    /// offers as options — each with its icon, its words and its shortcut.
    fn html(&self, cx: &mut crate::composition::HtmlCx<'_>) {
        cx.element("dialog");
        if !self.is_presented() {
            return;
        }
        self.compose_field(cx);
        self.compose_rows(cx);
        cx.targets();
    }

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
            if !focused && !self.closes_on_blur {
                // Its host closes it, not a focus that moved elsewhere: it stays as it stands.
                return;
            }
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

impl ActionPalette {
    /// The field the step collects its answer in, as the page's own input.
    fn compose_field(&self, cx: &mut crate::composition::HtmlCx<'_>) {
        let (rect, value, placeholder) =
            if let Some(field) = self.confirm.as_ref().and_then(ConfirmState::phrase_field) {
                (field.get_rect(), field.value().to_owned(), None)
            } else if self.confirm.is_none() {
                (
                    self.field.get_rect(),
                    self.field.value().to_owned(),
                    Some(self.field.placeholder().to_owned()),
                )
            } else {
                return;
            };
        if rect.is_zero_area() {
            return;
        }
        let theme = cx.theme().clone();
        let look = crate::composition::TextLook::new(
            String::new(),
            &theme.text(crate::tokens::TextRole::Body),
            Some(theme.tokens.colors.text_primary),
        );
        let semantics = self.accessibility().and_then(|root| {
            root.children.into_iter().find(|child| {
                matches!(
                    child.role,
                    crate::a11y::Role::TextInput | crate::a11y::Role::MultilineTextInput
                )
            })
        });
        let part = cx.part("field", "PaletteField", rect);
        part.tag = Some("input");
        part.focusable = true;
        part.semantics = semantics;
        part.own = Some(crate::composition::Own::Field {
            value,
            multiline: false,
            placeholder,
            look,
            inset: [0.0, 0.0],
        });
    }

    /// The rows it offers, as the options of its list.
    fn compose_rows(&self, cx: &mut crate::composition::HtmlCx<'_>) {
        let Some(rows) = self.results.accessibility() else {
            return;
        };
        // The list stands where its own accessibility node says when the host lays it out, and
        // where the view stands otherwise.
        let rect = rows
            .bounds
            .map(|bounds| bounds.to_rect())
            .filter(|rect| !rect.is_zero_area())
            .unwrap_or_else(|| self.results.get_rect());
        if rect.is_zero_area() || rows.children.is_empty() {
            return;
        }
        let theme = cx.theme().clone();
        let icon_size = f64::from(theme.tokens.metrics.icon_sm);
        let icon_ink = theme.tokens.colors.text_dim;
        let gap = f64::from(theme.tokens.metrics.icon_label_gap);
        let keycap_ink = theme.tokens.colors.text_dim;
        let mut shown: HashMap<&str, &crate::search::SearchResult> = HashMap::new();
        for group in self.results.groups() {
            for result in &group.results {
                shown.insert(result.action_id.as_str(), result);
            }
        }
        let mut options = Vec::new();
        let mut pending = std::collections::VecDeque::from(rows.children);
        while let Some(mut node) = pending.pop_front() {
            if !node.children.is_empty() {
                pending.extend(node.children.iter().cloned());
                continue;
            }
            let (Some(target), Some(bounds)) = (node.action_target.clone(), node.bounds) else {
                continue;
            };
            let found = shown.get(target.as_str());
            let icon = found
                .and_then(|result| result.icon)
                .and_then(|icon| icon.glyph(icon_size, icon_ink, false));
            let label = node.name.clone().map(|name| {
                crate::composition::TextLook::new(
                    name,
                    &theme.text(crate::tokens::TextRole::Label),
                    Some(theme.tokens.colors.text_primary),
                )
                .with_max_lines(Some(1))
            });
            let detail = found
                .and_then(|result| result.row.as_ref())
                .and_then(|row| match row {
                    crate::search::SearchRow::Command(row) => row.keycaps.as_ref(),
                    _ => None,
                })
                .map(|keycaps| {
                    crate::composition::TextLook::new(
                        keycaps.join("+"),
                        &theme.text(crate::tokens::TextRole::Caption),
                        Some(keycap_ink),
                    )
                    .with_max_lines(Some(1))
                });
            node.role = crate::a11y::Role::ListBoxOption;
            let rect = bounds.to_rect();
            let mut option = crate::composition::Composed::new(
                String::new(),
                "PaletteOption",
                [rect.x0, rect.y0, rect.width(), rect.height()],
            );
            option.tag = Some("li");
            option.focusable = true;
            option.semantics = Some(node);
            option.own = Some(crate::composition::Own::Face(crate::composition::Face {
                icon,
                label,
                detail,
                trailing: None,
                fill: None,
                edge: None,
                radius: 0.0,
                pad: 0.0,
                gap,
                spread: true,
            }));
            options.push(option);
        }
        let list = cx.part("rows", "PaletteRows", rect);
        list.tag = Some("ul");
        list.focusable = true;
        list.semantics = Some(
            crate::a11y::AccessNode::new(crate::a11y::Role::ListBox)
                .with_name(crate::i18n::widget_format("widget-search-results", &[])),
        );
        let base = list.path.clone();
        for (index, mut option) in options.into_iter().enumerate() {
            option.path = format!("{base}#row:{index}");
            list.children.push(option);
        }
    }
}
