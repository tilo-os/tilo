//! The fill-in-variables dialog.
//!
//! Copying a snippet whose body holds `${placeholder}` variables opens this
//! dialog: one field per variable, a live preview of the filled body, and
//! Copy. It is the toolkit's modal sheet ([`modal_dialog`]) drawn in the
//! window's top layer; it consumes all input while open. Its values are
//! local to this peer (a transient flow, not part of the shared library).

#[cfg(test)]
use automerge::AutoCommit;
use std::collections::HashMap;

use automerge::ObjId;
use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::modal_dialog::{self, ModalAction, ModalHit, ModalSpec};
use ui::widget::{ControlSize, EventCtx, TextInput, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::app::ExpandOutcome;
use crate::draw::{self, CodeRow};
use crate::model::{self, Snippet, SnippetId};

/// The most preview rows (wrapped lines) the dialog shows; a longer body
/// ends in an ellipsis row.
const PREVIEW_ROWS: usize = 12;

/// One placeholder field: its name and the person's current value.
struct Field {
    name: String,
    input: TextInput,
}

/// Where the dialog's parts landed in the last paint.
#[derive(Default)]
struct Geometry {
    card: Rect,
    /// Cancel, then Copy.
    actions: Vec<Rect>,
    scrim: Rect,
    fields: Vec<Rect>,
}

pub struct ExpandModal {
    /// The sheet's title and its two actions, in the person's language.
    title: String,
    cancel: String,
    copy: String,
    snippet_id: SnippetId,
    body: String,
    language: crate::model::Language,
    fields: Vec<Field>,
    /// Index of the field with keyboard focus.
    focused: usize,
    pointer: Point,
    geometry: Geometry,
}

impl ExpandModal {
    pub fn new(id: &SnippetId, s: &Snippet) -> Self {
        let mut fields: Vec<Field> = s
            .placeholders()
            .into_iter()
            .map(|name| Field {
                input: TextInput::local()
                    .with_id(&format!("snippets.expand.field.{name}"))
                    .with_placeholder(crate::tr("snippets-expand-value"))
                    .with_text_role(TextRole::Code),
                name,
            })
            .collect();
        if let Some(first) = fields.first_mut() {
            first.input.set_focus(true);
        }
        Self {
            title: crate::tr("snippets-expand-title"),
            cancel: crate::tr("snippets-cancel"),
            copy: crate::tr("snippets-copy"),
            snippet_id: id.clone(),
            body: s.body.clone(),
            language: s.language,
            fields,
            focused: 0,
            pointer: Point::ZERO,
            geometry: Geometry::default(),
        }
    }

    fn values(&self) -> HashMap<String, String> {
        self.fields
            .iter()
            .map(|f| (f.name.clone(), f.input.value().to_string()))
            .collect()
    }

    /// The expanded body with current field values applied.
    fn preview(&self) -> String {
        model::expand(&self.body, &self.values())
    }

    /// The sheet's panel, for the host's modal capture.
    pub fn card_rect(&self) -> Rect {
        self.geometry.card
    }

    fn actions<'a>(&self, cancel: &'a str, copy: &'a str) -> [ModalAction<'a>; 2] {
        let hovered = |index: usize| {
            self.geometry
                .actions
                .get(index)
                .is_some_and(|rect| rect.contains(self.pointer))
        };
        [
            ModalAction::secondary(cancel).hovered(hovered(0)),
            ModalAction::primary(copy).hovered(hovered(1)),
        ]
    }

    /// The label column: wide enough for the longest variable name, never
    /// more than a third of the body.
    fn label_width(&self, text: &mut TextRenderer, theme: &Theme, body_w: f64) -> f64 {
        let style = theme.text(TextRole::Code);
        self.fields
            .iter()
            .map(|f| f64::from(text.measure_style(&f.name, None, &style).0))
            .fold(0.0, f64::max)
            .min(body_w / 3.0)
            .ceil()
    }

    fn body_height(&self, text: &mut TextRenderer, theme: &Theme, body_w: f64) -> f64 {
        let rhythm = &theme.tokens.spacing.rhythm;
        let field_h = ControlSize::Regular.height(theme);
        let rows = self.fields.len() as f64;
        let fields = rows * field_h + (rows - 1.0).max(0.0) * f64::from(rhythm.sibling);
        let heading = f64::from(
            text.measure_style("Ag", None, &theme.text(TextRole::Subheading))
                .1,
        );
        fields
            + f64::from(rhythm.group)
            + heading
            + f64::from(rhythm.related)
            + self.preview_height(text, theme, body_w)
    }

    /// The filled body wrapped to the preview well `width` wide, cut to
    /// [`PREVIEW_ROWS`], and whether it was cut.
    fn preview_rows(
        &self,
        text: &mut TextRenderer,
        theme: &Theme,
        width: f64,
    ) -> (Vec<CodeRow>, bool) {
        let pad = f64::from(theme.tokens.spacing.s3);
        let mut rows = draw::wrap_code(
            text,
            &theme.text(TextRole::Code),
            &self.preview(),
            self.language,
            width - pad * 2.0,
        );
        let cut = rows.len() > PREVIEW_ROWS;
        // The last kept row makes room for the ellipsis that says so.
        rows.truncate(if cut { PREVIEW_ROWS - 1 } else { PREVIEW_ROWS });
        (rows, cut)
    }

    fn preview_height(&self, text: &mut TextRenderer, theme: &Theme, width: f64) -> f64 {
        let line = f64::from(theme.text(TextRole::Code).line_height_px());
        let (rows, cut) = self.preview_rows(text, theme, width);
        let shown = rows.len() + usize::from(cut);
        shown.max(1) as f64 * line + f64::from(theme.tokens.spacing.s3) * 2.0
    }

    /// Paint the dialog over `window` and record where its parts landed.
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        window: Rect,
    ) {
        let (title, cancel, copy) = (self.title.clone(), self.cancel.clone(), self.copy.clone());
        let actions = self.actions(&cancel, &copy);
        let spec = ModalSpec::new(&title).closable(false).actions(&actions);
        let body_w = modal_dialog::body_width(window, spec.max_width, theme);
        let spec = spec.body_height(self.body_height(text, theme, body_w));
        let layout = modal_dialog::layout(window, &spec, text, theme);
        modal_dialog::paint_backdrop(scene, t, text, theme, &layout, &spec);

        let ink = theme.on_ground(ui::chrome::panel_ground(theme));
        let rhythm = &theme.tokens.spacing.rhythm;
        let body = layout.body;
        let field_h = ControlSize::Regular.height(theme);
        let label_w = self.label_width(text, theme, body.width());
        let field_x = body.x0 + label_w + f64::from(rhythm.section);
        let mut y = body.y0;
        self.geometry.fields.clear();
        for field in &mut self.fields {
            let row = Rect::new(body.x0, y, body.x1, y + field_h);
            PaintCtx::new(scene, t, text, &ink).text_role_in_band(
                &field.name,
                TextRole::Code,
                ink.text_primary,
                row.x0,
                row,
                Some(label_w as f32),
            );
            let rect = Rect::new(field_x, row.y0, body.x1, row.y1);
            field.input.apply_layout_external(rect);
            field
                .input
                .draw(scene, t, text, &ink, &ui::presence::PresenceCtx::empty());
            self.geometry.fields.push(rect);
            y = row.y1 + f64::from(rhythm.sibling);
        }
        if !self.fields.is_empty() {
            y -= f64::from(rhythm.sibling);
        }

        // The filled body, as it will be copied.
        y += f64::from(rhythm.group);
        let mut ctx = PaintCtx::new(scene, t, text, &ink);
        ctx.text_role(
            &crate::tr("snippets-expand-preview"),
            TextRole::Subheading,
            ink.text_primary,
            Point::new(body.x0, y),
            None,
        );
        y += ctx.line_box(TextRole::Subheading) + f64::from(rhythm.related);
        let well = Rect::new(
            body.x0,
            y,
            body.x1,
            (y + self.preview_height(text, theme, body.width())).min(body.y1),
        );
        self.paint_preview(scene, t, text, theme, well);

        modal_dialog::paint_actions(scene, t, text, theme, &layout, &spec);
        self.geometry.card = layout.card;
        self.geometry.scrim = layout.scrim;
        self.geometry.actions = layout.actions;
    }

    /// The filled body as it will be copied: long lines wrap inside the
    /// well, and a body longer than the well ends in an ellipsis row.
    fn paint_preview(
        &self,
        scene: &mut Scene,
        t: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        well: Rect,
    ) {
        let ground = theme.tokens.colors.surfaces.raised;
        ui::surface::card(
            scene,
            t,
            well,
            f64::from(theme.tokens.radii.control),
            ground,
        );
        let ink = theme.on_ground(ground);
        let pad = f64::from(theme.tokens.spacing.s3);
        let (rows, cut) = self.preview_rows(text, theme, well.width());
        let line = f64::from(theme.text(TextRole::Code).line_height_px());
        let mut ctx = PaintCtx::new(scene, t, text, &ink);
        let origin = Point::new(well.x0 + pad, well.y0 + pad);
        draw::paint_code_rows(&mut ctx, &rows, origin, None);
        if cut {
            ctx.text_role(
                "…",
                TextRole::Code,
                ink.text_tertiary,
                Point::new(origin.x, origin.y + rows.len() as f64 * line),
                None,
            );
        }
    }

    /// Route a window event. Returns the dialog's lifecycle outcome.
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut EventCtx<'_>,
        pointer: Point,
    ) -> ExpandOutcome {
        self.pointer = pointer;
        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == ElementState::Pressed
        {
            match &key.logical_key {
                Key::Named(NamedKey::Escape) => return ExpandOutcome::Cancel,
                Key::Named(NamedKey::Enter) => {
                    return ExpandOutcome::Copy(self.preview(), self.snippet_id.clone());
                }
                Key::Named(NamedKey::Tab) => {
                    if !self.fields.is_empty() {
                        let count = self.fields.len();
                        self.focused = if ctx.modifiers.state().shift_key() {
                            (self.focused + count - 1) % count
                        } else {
                            (self.focused + 1) % count
                        };
                        self.focus_field(self.focused);
                    }
                    return ExpandOutcome::Stay;
                }
                _ => {}
            }
        }

        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        ) {
            let layout_hit = self.hit(pointer);
            match layout_hit {
                Some(ModalHit::Action(0) | ModalHit::Close | ModalHit::Scrim) => {
                    return ExpandOutcome::Cancel;
                }
                Some(ModalHit::Action(_)) => {
                    return ExpandOutcome::Copy(self.preview(), self.snippet_id.clone());
                }
                _ => {}
            }
        }

        let under = self
            .geometry
            .fields
            .iter()
            .position(|r| r.contains(pointer));
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left | MouseButton::Right,
                ..
            }
        ) && let Some(index) = under
        {
            self.focused = index;
            self.focus_field(index);
        }

        // Pointer motion reaches every field so each keeps its hover and
        // drag state. Keys and IME go to the focused field; pointer events
        // to the field under the pointer.
        if matches!(event, WindowEvent::CursorMoved { .. }) {
            for field in &mut self.fields {
                let mut child_handled = false;
                field.input.handle_event(
                    event,
                    &mut EventCtx {
                        handled: &mut child_handled,
                        ..ctx.reborrow(ctx.path, ctx.node_id)
                    },
                );
            }
        } else if let Some(index) = under.or_else(|| {
            self.fields
                .get(self.focused)
                .is_some_and(|f| f.input.is_focused())
                .then_some(self.focused)
        }) && let Some(field) = self.fields.get_mut(index)
        {
            let mut child_handled = false;
            field.input.handle_event(
                event,
                &mut EventCtx {
                    handled: &mut child_handled,
                    ..ctx.reborrow(ctx.path, ctx.node_id)
                },
            );
            let _ = field.input.take_dirty();
        }

        ExpandOutcome::Stay
    }

    /// What `p` lands on, from the last paint.
    fn hit(&self, p: Point) -> Option<ModalHit> {
        if let Some(index) = self.geometry.actions.iter().position(|r| r.contains(p)) {
            return Some(ModalHit::Action(index));
        }
        if self.geometry.card.contains(p) {
            return Some(ModalHit::Card);
        }
        self.geometry.scrim.contains(p).then_some(ModalHit::Scrim)
    }

    fn focus_field(&mut self, index: usize) {
        for (i, field) in self.fields.iter_mut().enumerate() {
            field.input.set_focus(i == index);
        }
    }

    // ---- test accessors -------------------------------------------------

    #[doc(hidden)]
    pub fn set_field_for_test(&mut self, name: &str, value: &str) {
        if let Some(f) = self.fields.iter_mut().find(|f| f.name == name) {
            f.input.set_text(value);
        }
    }

    #[doc(hidden)]
    pub fn preview_for_test(&self) -> String {
        self.preview()
    }

    #[doc(hidden)]
    pub fn field_count_for_test(&self) -> usize {
        self.fields.len()
    }

    pub fn ime_cursor_rect(&self) -> Option<Rect> {
        self.fields
            .get(self.focused)
            .and_then(|f| f.input.ime_cursor_rect())
    }

    pub fn ime_content(&self) -> Option<ui::ime::InputContent> {
        self.fields
            .get(self.focused)
            .and_then(|f| f.input.ime_content())
    }

    pub fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.fields.iter().find_map(|f| f.input.cursor_shape(point))
    }

    pub fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if let Some(field) = self.fields.get_mut(self.focused) {
            field
                .input
                .draw_overlay(scene, transform, tr, theme, presence);
        }
    }

    pub fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let mut children: Vec<_> = self
            .fields
            .iter()
            .filter_map(|field| {
                field.input.accessibility().map(|node| {
                    node.with_name(&field.name)
                        .with_action_target(format!("snippets.expand.field.{}", field.name))
                })
            })
            .collect();
        let rect = |index: usize| {
            self.geometry
                .actions
                .get(index)
                .copied()
                .unwrap_or(Rect::ZERO)
        };
        for (target, label, bounds) in [
            ("snippets.expand.cancel", self.cancel.as_str(), rect(0)),
            ("snippets.expand.copy", self.copy.as_str(), rect(1)),
        ] {
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(label)
                    .with_action_target(target)
                    .with_bounds(bounds)
                    .with_state(AccessState::new().focusable(true)),
            );
        }
        Some(
            AccessNode::new(Role::Dialog)
                .with_name(&self.title)
                .with_children(children),
        )
    }

    pub fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ObjId,
    ) -> Option<ExpandOutcome> {
        if matches!(action, ui::a11y::Action::Activate) {
            match target {
                "snippets.expand.copy" => {
                    return Some(ExpandOutcome::Copy(self.preview(), self.snippet_id.clone()));
                }
                "snippets.expand.cancel" => return Some(ExpandOutcome::Cancel),
                _ => {}
            }
        }
        let index = self
            .fields
            .iter()
            .position(|field| target == format!("snippets.expand.field.{}", field.name))?;
        if matches!(action, ui::a11y::Action::Focus) {
            self.focused = index;
            self.focus_field(index);
            return Some(ExpandOutcome::Stay);
        }
        let field = self.fields.get_mut(index)?;
        let accepted = field.input.perform_action(action, &[], state, path);
        field.input.take_dirty();
        accepted.then_some(ExpandOutcome::Stay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Language, Snippet};
    use taffy::TaffyTree;

    fn key(named: NamedKey) -> WindowEvent {
        // `KeyEvent`'s platform field is private; initialize the same minimal
        // synthetic event used by the shared TextInput's own tests.
        let event = ui::synthetic_input::key_event(Key::Named(named), None, ElementState::Pressed);
        ui::synthetic_input::keyboard_input(event)
    }

    fn drive(modal: &mut ExpandModal, event: &WindowEvent, doc: &mut AutoCommit) -> ExpandOutcome {
        let mut taffy = TaffyTree::new();
        let node = taffy.new_leaf(taffy::Style::default()).unwrap();
        let mut focus = None;
        let mut handled = false;
        modal.handle_event(
            event,
            &mut EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: ui::widget::Edits::lend(doc),
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
            Point::ZERO,
        )
    }

    fn templated() -> Snippet {
        Snippet {
            language: Language::Shell,
            body: "echo hi ${name} from ${host}".into(),
            ..Snippet::titled("Greet", "")
        }
    }

    fn modal() -> ExpandModal {
        ExpandModal::new(&SnippetId::from_raw("greet"), &templated())
    }

    /// The expand modal: one field per placeholder, preview substitution and IME/backspace editing.
    #[test]
    fn expand_modal_collects_fields_and_previews_live() {
        // Case `modal_collects_one_field_per_placeholder`.
        {
            let m = modal();
            assert_eq!(m.field_count_for_test(), 2);
        }

        // Case `preview_substitutes_filled_fields`.
        {
            let mut m = modal();
            m.set_field_for_test("name", "ada");
            m.set_field_for_test("host", "prod");
            assert_eq!(m.preview_for_test(), "echo hi ada from prod");
        }

        // Case `preview_keeps_unfilled_placeholders_visible`.
        {
            let mut m = modal();
            m.set_field_for_test("name", "ada");
            assert_eq!(m.preview_for_test(), "echo hi ada from ${host}");
        }

        // Case `shared_editor_ime_and_unicode_backspace_drive_live_preview_without_crdt_writes`.
        {
            let mut modal = modal();
            let mut doc = AutoCommit::new();
            let before = doc.get_heads();

            assert!(matches!(
                drive(
                    &mut modal,
                    &WindowEvent::Ime(winit::event::Ime::Commit("漢".into())),
                    &mut doc,
                ),
                ExpandOutcome::Stay
            ));
            assert_eq!(modal.preview_for_test(), "echo hi 漢 from ${host}");

            let _ = drive(&mut modal, &key(NamedKey::Backspace), &mut doc);
            assert_eq!(modal.preview_for_test(), "echo hi ${name} from ${host}");
            assert_eq!(doc.get_heads(), before, "modal values must stay peer-local");
        }
    }
}
