//! An inline-editable label: reads as static text until the caller puts it
//! into edit mode, then becomes a real [`TextInput`].
//!
//! An edit ends one of three ways, matching native inline rename: **Enter**
//! commits; **focus loss / a click outside the field** blur-commits — a
//! rename shouldn't be lost just because you clicked away instead of
//! pressing Enter; **Esc** cancels, keeping the previous value. A blank
//! commit also keeps the previous value, so an accidental clear can't empty
//! `value()`.
//!
//! The outcome of the most recent edit is polled once via [`take_outcome`]
//! (rising-edge, like [`TextInput::take_dirty`]).

use crate::widget::ModelPath;
use std::collections::HashMap;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::presence::PresenceCtx;
use crate::text_render::{FontChoice, TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::{TextInput, TextInputKind, Widget};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditOutcome {
    None,
    /// Also returned when a blank entry kept the previous value — the
    /// payload is always the live value.
    Committed(String),
    Cancelled,
}

pub struct EditableLabel {
    id: String,
    accessible_name: Option<String>,
    semantic_target: Option<String>,
    value: String,
    /// Live only while editing; owns the caret/selection and every
    /// editing affordance.
    input: TextInput,
    editing: bool,
    /// An explicit family; otherwise the role's.
    family: Option<FontChoice>,
    /// What the label is; its idle text is set in the theme's style for it.
    role: crate::tokens::TextRole,
    /// The role resolved in the latest theme (stock before the first
    /// layout): size, weight, line height and family, at the text scale.
    style: TextStyle,
    idle_color: Option<vello::peniko::Color>,
    rect: Rect,
    /// Tracked so a press can be classified as inside (caret) vs. outside
    /// (blur-commit) without the press event carrying a position.
    last_pointer: Point,
    outcome: EditOutcome,
    /// Invoked with each committed new value.
    action: Option<crate::widget::ActionEmitter<String>>,
    /// A double click on the idle label begins an edit.
    renames: bool,
    /// When the last press on the idle label was, to count the second.
    last_press: Option<crate::time::Instant>,
    /// Take the width the row leaves.
    grow: bool,
    /// A single press on the idle label begins an edit, as a document's
    /// title does.
    edits_on_click: bool,
    /// Shown, dimmed, while the value is empty and no edit is open.
    placeholder: Option<String>,
    /// Set by something else that offers a rename (a context menu's
    /// "Rename"): the label begins an edit with the next event it sees.
    edit_request: Option<crate::LocalState<bool>>,
}

impl EditableLabel {
    pub fn new(value: &str) -> Self {
        Self {
            id: String::new(),
            accessible_name: None,
            semantic_target: None,
            value: value.to_string(),
            input: TextInput::local().with_kind(TextInputKind::Name),
            editing: false,
            family: None,
            role: crate::tokens::TextRole::Body,
            style: crate::tokens::typography::stock().style(crate::tokens::TextRole::Body),
            idle_color: None,
            rect: Rect::ZERO,
            last_pointer: Point::ZERO,
            outcome: EditOutcome::None,
            action: None,
            renames: false,
            last_press: None,
            grow: false,
            edits_on_click: false,
            placeholder: None,
            edit_request: None,
        }
    }

    /// Begin an edit when `request` is set, as a rename offered elsewhere
    /// (a context menu) asks; the label clears it once the edit is open.
    #[must_use]
    pub fn begins_editing_on(mut self, request: &crate::LocalState<bool>) -> Self {
        self.edit_request = Some(request.clone());
        self
    }

    /// Invoke `binding` with each committed value that differs from the one
    /// before: a rename is the app's own action.
    #[must_use]
    pub fn on_action(mut self, binding: tilo_actions::ActionBinding<String>) -> Self {
        self.action = Some(crate::widget::ActionEmitter::new(binding));
        self
    }

    /// Begin an edit on a double click, the way a list renames its items in
    /// place. A single click is left to what is beneath (selecting the row).
    #[must_use]
    pub fn renames_on_double_click(mut self) -> Self {
        self.renames = true;
        self
    }

    /// Begin an edit on a single press, the way a document's title is
    /// renamed in place. The edit still ends as every edit does: Enter or
    /// a click away commits it once, Escape keeps the value.
    #[must_use]
    pub fn edits_on_click(mut self) -> Self {
        self.edits_on_click = true;
        self
    }

    /// What the idle label reads, dimmed, while its value is empty.
    #[must_use]
    pub fn with_placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.placeholder = Some(placeholder.into());
        self
    }

    /// Take the width the row leaves, eliding a long value.
    #[must_use]
    pub fn filling(mut self) -> Self {
        self.grow = true;
        self
    }

    #[must_use]
    pub fn with_font_family(mut self, family: FontChoice) -> Self {
        self.input.set_font_family(Some(family.clone()));
        self.family = Some(family);
        self
    }

    /// Set the label in `role` (default [`crate::tokens::TextRole::Body`]),
    /// idle and while editing.
    #[must_use]
    pub fn with_text_role(mut self, role: crate::tokens::TextRole) -> Self {
        self.set_text_role(role);
        self
    }

    /// Change the label's role; see [`Self::with_text_role`].
    pub fn set_text_role(&mut self, role: crate::tokens::TextRole) {
        self.role = role;
        self.style = crate::tokens::typography::stock().style(role);
        self.input.set_text_role(role);
    }

    /// The idle style: the theme's style for the role, in an explicit
    /// family when one was given.
    fn adopt_theme(&mut self, theme: &Theme) {
        let mut style = theme.text(self.role);
        if let Some(family) = &self.family {
            style = style.with_family(family.clone());
        }
        self.style = style;
    }

    /// The label's band: the role's line, never under the minimum target.
    fn band(&self) -> f32 {
        self.style.line_height_px().max(28.0)
    }

    /// Hide the insertion caret beneath a modal without committing an edit.
    pub fn suppress_caret_paint(&mut self, suppressed: bool) {
        self.input.suppress_caret_paint(suppressed);
    }

    #[must_use]
    pub fn with_color(mut self, color: vello::peniko::Color) -> Self {
        self.idle_color = Some(color);
        self
    }

    #[must_use]
    pub fn with_id(mut self, id: &str) -> Self {
        self.id = id.to_string();
        self
    }

    #[must_use]
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.accessible_name = Some(name.into());
        self
    }

    /// Bind the editable surface to its current model identity. Changing the
    /// target retires semantic routes from the previously displayed model.
    pub fn set_semantic_target(&mut self, target: Option<String>) {
        self.semantic_target = target;
    }

    pub fn value(&self) -> &str {
        &self.value
    }

    /// The live field contents while editing, else the committed value.
    pub fn current_text(&self) -> &str {
        if self.editing {
            self.input.value()
        } else {
            &self.value
        }
    }

    /// What the idle label shows: its value, or the placeholder while the
    /// value is empty.
    fn shown_text(&self) -> &str {
        match &self.placeholder {
            Some(placeholder) if !self.editing && self.value.is_empty() => placeholder,
            _ => self.current_text(),
        }
    }

    /// Ignored while editing so it doesn't stomp the user's typing.
    pub fn set_value(&mut self, value: &str) {
        if !self.editing {
            self.value = value.to_string();
        }
    }

    pub fn is_editing(&self) -> bool {
        self.editing
    }

    /// Selects the whole value so the first keystroke replaces it (native
    /// rename behaviour).
    pub fn begin_edit(&mut self) {
        self.input.set_text(&self.value);
        self.input.set_focus(true);
        self.input.select_all();
        self.editing = true;
        self.outcome = EditOutcome::None;
    }

    /// A blank entry keeps the previous value. No-op when not editing.
    pub fn commit(&mut self) {
        if !self.editing {
            return;
        }
        let trimmed = self.input.value().trim().to_string();
        let changed = !trimmed.is_empty() && trimmed != self.value;
        if !trimmed.is_empty() {
            self.value = trimmed;
        }
        self.editing = false;
        self.input.set_focus(false);
        self.outcome = EditOutcome::Committed(self.value.clone());
        if changed && let Some(action) = self.action.as_mut() {
            let _ = action.emit(self.value.clone());
        }
    }

    /// No-op when not editing.
    pub fn cancel(&mut self) {
        if !self.editing {
            return;
        }
        self.editing = false;
        self.input.set_focus(false);
        self.outcome = EditOutcome::Cancelled;
    }

    pub fn take_outcome(&mut self) -> EditOutcome {
        std::mem::replace(&mut self.outcome, EditOutcome::None)
    }

    /// For callers embedding the label in their own immediate-mode chrome,
    /// without a taffy pass.
    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.rect = rect;
        self.input.apply_layout_external(rect);
    }

    /// A press on the idle label that begins an edit: any press with
    /// [`Self::edits_on_click`], else the second of a double click.
    fn begin_on_press(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if !(self.renames || self.edits_on_click)
            || !matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                }
            )
            || !self.rect.contains(self.last_pointer)
        {
            return;
        }
        if self.edits_on_click {
            self.begin_edit();
            *ctx.focus_target = Some(ctx.node_id);
            *ctx.handled = true;
            return;
        }
        let now = crate::time::now();
        let second = self.last_press.is_some_and(|at| {
            now.saturating_duration_since(at) < crate::gesture::multi_click_interval()
        });
        self.last_press = if second { None } else { Some(now) };
        if second {
            self.begin_edit();
            *ctx.focus_target = Some(ctx.node_id);
            *ctx.handled = true;
        }
    }

    /// The idle text's line, centred in the rect as the editing field
    /// centres its own.
    fn text_band(&self) -> (f64, f64) {
        let line_height = f64::from(self.style.line_height_px());
        let top = self.rect.y0 + (self.rect.height() - line_height) / 2.0;
        (top, line_height)
    }
}

impl Widget for EditableLabel {
    crate::provides!(dyn crate::widget::controls::ActionControls);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.adopt_theme(theme);
        let style = Style {
            size: Size {
                width: Dimension::auto(),
                height: Dimension::length(self.band()),
            },
            flex_shrink: 1.0,
            flex_grow: if self.grow { 1.0 } else { 0.0 },
            min_size: Size {
                width: Dimension::length(0.0),
                height: Dimension::auto(),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        _known: taffy::Size<Option<f32>>,
        _available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let (w, _) = text_renderer.measure_style(self.shown_text(), None, &self.style);
        taffy::Size {
            width: w + 4.0,
            height: self.band(),
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        self.input.apply_layout_external(self.rect);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.adopt_theme(theme);
        if self.editing {
            self.input.apply_layout_external(self.rect);
            self.input
                .draw(scene, transform, text_renderer, theme, presence);
            return;
        }
        let (shown, color) = if self.value.is_empty() {
            match &self.placeholder {
                Some(placeholder) => (placeholder.as_str(), theme.text_tertiary),
                None => return,
            }
        } else {
            (
                self.value.as_str(),
                self.idle_color.unwrap_or(theme.text_primary),
            )
        };
        let (top, _lh) = self.text_band();
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "kurbo Affine composition is floating-point matrix math; it cannot overflow or panic"
        )]
        let text_transform = transform * Affine::translate((self.rect.x0, top));
        #[allow(
            clippy::cast_possible_truncation,
            reason = "label width in logical pixels is far below f32::MAX for any realistic UI element"
        )]
        let width = self.rect.width() as f32 + 1.0;
        let shown = text_renderer.elide_to_width_style(shown, width, &self.style);
        text_renderer.draw_style(
            scene,
            &shown,
            color,
            text_transform,
            Some(width),
            &self.style,
        );
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_pointer =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        if !self.editing
            && let Some(request) = &self.edit_request
            && request.get()
        {
            request.set(false);
            self.begin_edit();
            *ctx.focus_target = Some(ctx.node_id);
        }
        if !self.editing {
            self.begin_on_press(event, ctx);
            return;
        }
        match event {
            // The field itself consumes neither Enter nor Esc, so the
            // wrapper intercepts both before forwarding.
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Enter),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                self.commit();
                *ctx.handled = true;
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                self.cancel();
                *ctx.handled = true;
            }
            // Deliberately NOT consumed, so the click still reaches whatever
            // is under it — clicking away both saves and acts, like a
            // native inline rename.
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if !self.rect.contains(self.last_pointer) => {
                self.commit();
                // handled stays false: fall through to the host.
            }
            // A plain hover move is forwarded but not claimed, so the host
            // keeps its own pointer bookkeeping.
            WindowEvent::CursorMoved { .. } => {
                let was = *ctx.handled;
                self.input.handle_event(event, ctx);
                if !self.input.is_dragging() {
                    *ctx.handled = was;
                }
            }
            _ => {
                self.input.handle_event(event, ctx);
            }
        }
    }

    fn name(&self) -> &'static str {
        "EditableLabel"
    }

    fn test_id(&self) -> Option<&str> {
        if self.id.is_empty() {
            Some("editable_label")
        } else {
            Some(self.id.as_str())
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if (self.editing || self.edits_on_click) && self.rect.contains(point) {
            Some(winit::window::CursorIcon::Text)
        } else {
            None
        }
    }

    fn is_focusable(&self) -> bool {
        true
    }

    /// Losing focus while editing blur-commits — the keyboard-navigation
    /// counterpart to a click-outside.
    fn set_focus(&mut self, focused: bool) {
        if focused {
            self.input.set_focus(true);
        } else if self.editing {
            self.commit();
        }
    }

    fn is_focused(&self) -> bool {
        self.editing
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        Widget::ime_cursor_rect(&self.input)
    }

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        self.ime_cursor_rect()?;
        self.input.ime_edit_fingerprint(key)
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        Widget::ime_purpose(&self.input)
    }

    fn wants_pointer_capture(&self) -> bool {
        self.editing && self.input.is_dragging()
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let mut node = if self.editing {
            self.input.accessibility()?
        } else {
            if self.value.is_empty() && self.accessible_name.is_none() {
                return None;
            }
            AccessNode::new(Role::TextInput)
                .with_optional_value(Some(self.value.clone()))
                .with_state(AccessState::new().focusable(true))
        };
        node.name.clone_from(&self.accessible_name);
        if let Some(target) = &self.semantic_target {
            node = node
                .with_semantic_key(target.clone())
                .with_action_target(target.clone());
        }
        Some(node)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        self.semantic_target.as_deref() == Some(target) && self.perform_action(action, state, path)
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        use crate::a11y::Action;
        let (state, path) = crate::doc::document(state, path);

        if matches!(action, Action::Focus | Action::Activate) {
            if !self.editing {
                self.begin_edit();
            }
            return true;
        }
        if !self.editing {
            match action {
                Action::SetValue(_) | Action::SetSelection { .. } => self.begin_edit(),
                _ => return false,
            }
        }
        Widget::perform_action(&mut self.input, action, state, path)
    }
}

impl crate::widget::controls::ActionControls for EditableLabel {
    crate::widget::controls::optional_emitter_controls!();
    fn control_action_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
        let Some(action) = &self.action else {
            return Vec::new();
        };
        let control_id = self
            .semantic_target
            .as_deref()
            .or_else(|| self.test_id())
            .unwrap_or("editable_label");
        vec![action.metadata(control_id)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use winit::event::Modifiers;

    fn drive(el: &mut EditableLabel, doc: &mut AutoCommit, ev: &WindowEvent, m: Modifiers) -> bool {
        let mut handled = false;
        let mut focus_target = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.leaf_node(Style::default());
        el.handle_event(
            ev,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &m,
                state: crate::widget::Edits::lend(doc),
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &taffy,
            },
        );
        handled
    }

    use crate::synthetic_input::char_key;

    use crate::synthetic_input::named_key;

    fn mouse_move(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(x, y),
        }
    }

    fn left_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

    fn no_mods() -> Modifiers {
        Modifiers::default()
    }

    #[test]
    fn editable_label_commits_on_enter_or_blur_and_cancels_on_escape() {
        fn blur_commit_saves_typed_value_on_click_outside() {
            let mut el = EditableLabel::new("Old Name");
            el.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 30.0));
            let mut doc = AutoCommit::new();

            el.begin_edit();
            assert!(el.is_editing());

            // begin_edit selects-all, so the first keystroke replaces the value.
            drive(&mut el, &mut doc, &char_key("N"), no_mods());
            drive(&mut el, &mut doc, &char_key("e"), no_mods());
            drive(&mut el, &mut doc, &char_key("w"), no_mods());
            assert_eq!(
                el.current_text(),
                "New",
                "typing replaced the selected value"
            );
            assert!(el.is_editing(), "still editing until a commit trigger");

            // Move the pointer well outside the field, then press — a click away.
            drive(&mut el, &mut doc, &mouse_move(500.0, 500.0), no_mods());
            let consumed = drive(&mut el, &mut doc, &left_press(), no_mods());

            assert!(!el.is_editing(), "click outside ends the edit");
            assert_eq!(el.value(), "New", "blur committed the typed value");
            assert!(
                !consumed,
                "the outside click is not swallowed — it still reaches the host",
            );
            assert_eq!(el.take_outcome(), EditOutcome::Committed("New".into()));
            assert_eq!(
                el.take_outcome(),
                EditOutcome::None,
                "outcome is rising-edge"
            );
        }

        fn enter_commits_and_consumes() {
            let mut el = EditableLabel::new("first");
            el.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 30.0));
            let mut doc = AutoCommit::new();
            el.begin_edit();
            drive(&mut el, &mut doc, &char_key("z"), no_mods()); // replaces selection → "z"
            let consumed = drive(&mut el, &mut doc, &named_key(NamedKey::Enter), no_mods());
            assert!(consumed, "Enter is consumed");
            assert!(!el.is_editing());
            assert_eq!(el.value(), "z");
            assert_eq!(el.take_outcome(), EditOutcome::Committed("z".into()));
        }

        fn escape_cancels_and_keeps_previous_value() {
            let mut el = EditableLabel::new("keep me");
            el.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 30.0));
            let mut doc = AutoCommit::new();
            el.begin_edit();
            drive(&mut el, &mut doc, &char_key("X"), no_mods()); // would-be new value
            let consumed = drive(&mut el, &mut doc, &named_key(NamedKey::Escape), no_mods());
            assert!(consumed, "Esc is consumed");
            assert!(!el.is_editing());
            assert_eq!(el.value(), "keep me", "cancel restores the previous value");
            assert_eq!(el.take_outcome(), EditOutcome::Cancelled);
        }

        fn blank_commit_keeps_previous_value() {
            let mut el = EditableLabel::new("Original");
            el.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 30.0));
            let mut doc = AutoCommit::new();
            el.begin_edit(); // selects all
            // select-all is already active; type a space then backspace to empty it.
            drive(&mut el, &mut doc, &char_key(" "), no_mods()); // → " "
            drive(
                &mut el,
                &mut doc,
                &named_key(NamedKey::Backspace),
                no_mods(),
            ); // → ""
            assert_eq!(el.current_text(), "", "field is empty before commit");
            el.commit();
            assert_eq!(el.value(), "Original", "blank commit keeps the old value");
        }

        fn focus_loss_blur_commits() {
            let mut el = EditableLabel::new("before");
            el.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 30.0));
            let mut doc = AutoCommit::new();
            el.begin_edit();
            drive(&mut el, &mut doc, &char_key("A"), no_mods()); // → "A"
            el.set_focus(false);
            assert!(!el.is_editing());
            assert_eq!(el.value(), "A");
            assert_eq!(el.take_outcome(), EditOutcome::Committed("A".into()));
        }

        blur_commit_saves_typed_value_on_click_outside();
        enter_commits_and_consumes();
        escape_cancels_and_keeps_previous_value();
        blank_commit_keeps_previous_value();
        focus_loss_blur_commits();
    }

    #[test]
    fn editable_label_draft_survives_modal_paint_and_selects_all() {
        fn modal_paint_suppression_retains_title_draft_and_selection() {
            let mut label = EditableLabel::new("Draft title");
            label.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 50.0));
            label.begin_edit();
            let before = crate::undo::get_state(&label.input);
            let mut renderer = crate::TextRenderer::new();
            let mut draw = |label: &mut EditableLabel| {
                let mut scene = Scene::new();
                label.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut renderer,
                    &Theme::dark(),
                    &PresenceCtx::empty(),
                );
                scene.encoding().draw_tags.len()
            };
            let visible = draw(&mut label);
            label.suppress_caret_paint(true);
            assert_eq!(draw(&mut label).saturating_add(1), visible);
            assert!(label.is_editing());
            assert_eq!(crate::undo::get_state(&label.input), before);
            assert_eq!(label.current_text(), "Draft title");
            label.suppress_caret_paint(false);
            assert_eq!(draw(&mut label), visible);
        }

        fn ctrl_a_selects_all_through_embedded_field() {
            let mut el = EditableLabel::new("hello world");
            el.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 30.0));
            let mut doc = AutoCommit::new();
            el.begin_edit();
            // Collapse the initial select-all by pressing Right (caret to end).
            drive(
                &mut el,
                &mut doc,
                &named_key(NamedKey::ArrowRight),
                no_mods(),
            );
            // Now Ctrl+A must reselect everything, then typing replaces it all.
            let mut ctrl = winit::keyboard::ModifiersState::empty();
            ctrl |= winit::keyboard::ModifiersState::CONTROL;
            drive(&mut el, &mut doc, &char_key("a"), Modifiers::from(ctrl));
            drive(&mut el, &mut doc, &char_key("Z"), no_mods());
            assert_eq!(
                el.current_text(),
                "Z",
                "Ctrl+A selected the whole value so the next keystroke replaced it",
            );
        }

        modal_paint_suppression_retains_title_draft_and_selection();
        ctrl_a_selects_all_through_embedded_field();
    }
    #[test]
    fn the_idle_label_sets_in_its_roles_style() {
        let theme = Theme::dark().with_text_scale(2.0);
        let mut label =
            EditableLabel::new("Groceries").with_text_role(crate::tokens::TextRole::Title);
        let mut tree = TaffyTree::new();
        let node = label.build_layout(&mut tree, &theme, &mut HashMap::new());
        let style = theme.text(crate::tokens::TextRole::Title);
        assert_eq!(label.style, style, "weight, size and leading of the role");
        let height = tree.style(node).unwrap().size.height;
        assert_eq!(
            height,
            Dimension::length(style.line_height_px().max(28.0)),
            "the band is the role's scaled line"
        );
    }
}
