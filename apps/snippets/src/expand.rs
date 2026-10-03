//! The expand-with-variables modal.
//!
//! When the user copies a snippet whose body contains `${placeholder}`
//! variables, this modal collects a value for each one, shows a live
//! preview of the substituted result, and copies the filled text. It is
//! a self-contained overlay: it draws itself above the rest of the app
//! and consumes all input while open. State is local (it's a transient
//! per-peer flow, not part of the shared library).

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use ui::paint::PaintCtx;
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace, EventCtx, TextInput, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::app::ExpandOutcome;
use crate::draw;
use crate::highlight;
use crate::model::{self, Snippet, SnippetId};

/// One placeholder field: its name and the user's current value.
struct Field {
    name: String,
    input: TextInput,
}

pub struct ExpandModal {
    snippet_id: SnippetId,
    title: String,
    body: String,
    language: crate::model::Language,
    fields: Vec<Field>,
    /// Index of the field with keyboard focus.
    focused: usize,
    /// Hit regions recorded each draw: (rect, which field index, or
    /// sentinel for the buttons). Resolved on click.
    field_rects: Vec<Rect>,
    copy_rect: Rect,
    cancel_rect: Rect,
}

impl ExpandModal {
    pub fn new(s: &Snippet) -> Self {
        let mut fields: Vec<Field> = s
            .placeholders()
            .into_iter()
            .map(|name| Field {
                input: TextInput::local()
                    .with_id(&format!("snippets.expand.field.{name}"))
                    .with_placeholder("type a value…")
                    .with_font_size(13.0)
                    .with_plain_chrome(),
                name,
            })
            .collect();
        if let Some(first) = fields.first_mut() {
            first.input.set_focus(true);
        }
        Self {
            snippet_id: s.id,
            title: s.title.clone(),
            body: s.body.clone(),
            language: s.language,
            fields,
            focused: 0,
            field_rects: Vec::new(),
            copy_rect: Rect::ZERO,
            cancel_rect: Rect::ZERO,
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

    /// True once every field has a non-empty value.
    fn all_filled(&self) -> bool {
        self.fields
            .iter()
            .all(|f| !f.input.value().trim().is_empty())
    }

    /// Render the modal. The modal records its own button/field hit
    /// regions internally (the panes beneath are inert while it's open).
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) {
        let panel = Self::draw_frame(scene, t, theme, area);
        let pad = 24.0;
        let y = self.draw_header(scene, t, tr, theme, panel, pad);
        let y = self.draw_fields(scene, t, tr, theme, panel, pad, y);
        self.draw_preview(scene, t, tr, theme, panel, pad, y);
        self.draw_buttons(scene, t, tr, theme, panel, pad);
    }

    /// Scrim + the panel's background and border. Returns the panel rect
    /// every other section draws relative to.
    fn draw_frame(scene: &mut Scene, t: Affine, theme: &Theme, area: Rect) -> Rect {
        let c = &theme.tokens.colors;
        // Scrim.
        draw::fill_rect(scene, t, area, c.surfaces.overlay);

        // Panel centred horizontally, biased toward the top.
        let pw = 640.0_f64.min(area.width() - 80.0);
        let ph = 520.0_f64.min(area.height() - 80.0);
        let px = area.x0 + (area.width() - pw) / 2.0;
        let py = area.y0 + (area.height() - ph) / 2.0;
        let panel = Rect::new(px, py, px + pw, py + ph);
        // Panel role (HIG §9.5): the modal's ground and its derived border,
        // shared with every other floating panel on the desktop.
        draw::fill_round(
            scene,
            t,
            panel,
            f64::from(theme.tokens.radii.lg),
            ui::chrome::panel_ground(theme),
        );
        draw::stroke_edge(
            scene,
            t,
            panel,
            f64::from(theme.tokens.radii.lg),
            ui::chrome::panel_border(theme),
        );
        panel
    }

    /// The "Fill in variables" title and the snippet's own title beneath
    /// it. Returns the y cursor for the next section.
    fn draw_header(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
        pad: f64,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let mut y = panel.y0 + pad;

        PaintCtx::new(scene, t, tr, theme).text(
            "Fill in variables",
            &TextStyle::new(18.0).with_weight(TextWeight::Bold),
            c.text_primary,
            Point::new(panel.x0 + pad, y),
            None,
        );
        y += 24.0;
        PaintCtx::new(scene, t, tr, theme).text(
            &self.title,
            &TextStyle::new(13.0),
            c.text_dim,
            Point::new(panel.x0 + pad, y),
            None,
        );
        y + 30.0
    }

    /// One labelled input row per placeholder, or a "no variables" note
    /// when the snippet has none. Records `field_rects` for hit-testing
    /// and returns the y cursor for the next section.
    fn draw_fields(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
        pad: f64,
        mut y: f64,
    ) -> f64 {
        let c = &theme.tokens.colors;
        self.field_rects.clear();
        let label_w = 130.0;
        for (i, f) in self.fields.iter_mut().enumerate() {
            let row_y = y;
            // Label as ${name}.
            let lbl = format!("${{{}}}", f.name);
            PaintCtx::new(scene, t, tr, theme).text(
                &lbl,
                &draw::mono_style(13.0, TextWeight::Semibold),
                c.accent[5],
                Point::new(panel.x0 + pad, row_y + 7.0),
                None,
            );
            // Input box.
            let input = Rect::new(
                panel.x0 + pad + label_w,
                row_y,
                panel.x1 - pad,
                row_y + 30.0,
            );
            let focused = i == self.focused;
            ui::surface::card(
                scene,
                t,
                input,
                f64::from(theme.tokens.radii.md),
                c.surfaces.base,
            );
            if focused {
                draw::stroke_round(
                    scene,
                    t,
                    input,
                    f64::from(theme.tokens.radii.md),
                    c.border_focus,
                    1.5,
                );
            }
            // The modal keeps its established label/box chrome; the shared
            // editor owns only the inset content and interaction behaviour.
            f.input.apply_layout_external(Rect::new(
                input.x0 + 10.0,
                input.y0,
                input.x1 - 10.0,
                input.y1,
            ));
            f.input
                .draw(scene, t, tr, theme, &ui::presence::PresenceCtx::empty());
            self.field_rects.push(input);
            y += 40.0;
        }

        if self.fields.is_empty() {
            PaintCtx::new(scene, t, tr, theme).text(
                "This snippet has no variables.",
                &TextStyle::new(13.0),
                c.text_dim,
                Point::new(panel.x0 + pad, y),
                None,
            );
            y += 30.0;
        }
        y
    }

    /// The live "PREVIEW" panel: the substituted body, syntax-highlighted.
    fn draw_preview(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
        pad: f64,
        mut y: f64,
    ) {
        let c = &theme.tokens.colors;
        // Live preview header.
        y += 4.0;
        PaintCtx::new(scene, t, tr, theme).text(
            "PREVIEW",
            &TextStyle::new(11.0).with_weight(TextWeight::Bold),
            c.text_dim,
            Point::new(panel.x0 + pad, y),
            None,
        );
        y += 18.0;

        // Preview card with highlighted, substituted body.
        let card = Rect::new(panel.x0 + pad, y, panel.x1 - pad, panel.y1 - 72.0);
        ui::surface::card(
            scene,
            t,
            card,
            f64::from(theme.tokens.radii.md),
            c.neutral[0],
        );
        let preview = self.preview();
        let mut cy = card.y0 + 12.0;
        for spans in highlight::highlight_lines(&preview, self.language) {
            if cy > card.y1 - 18.0 {
                break;
            }
            let mut sx = card.x0 + 14.0;
            for span in spans {
                let color = theme.tokens.syntax.color(span.kind);
                PaintCtx::new(scene, t, tr, theme).text(
                    &span.text,
                    &draw::mono_style(13.0, TextWeight::Regular),
                    color,
                    Point::new(sx, cy),
                    None,
                );
                sx += draw::mono_width(tr, &span.text, 13.0);
            }
            cy += 19.0;
        }
    }

    /// The Copy / Cancel buttons, right-aligned along the panel's bottom
    /// edge. Records `copy_rect` / `cancel_rect` for hit-testing.
    fn draw_buttons(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
        pad: f64,
    ) {
        let by = panel.y1 - 52.0;
        let copy_label = if self.all_filled() {
            "Copy"
        } else {
            "Copy (partial)"
        };
        let cw = f64::from(tr.measure(copy_label, 14.0, None).0) + 36.0;
        self.copy_rect = Rect::new(panel.x1 - pad - cw, by, panel.x1 - pad, by + 36.0);
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(self.copy_rect, copy_label)
                .radius(f64::from(theme.tokens.radii.md))
                .text_size(14.0)
                .weight(TextWeight::Semibold)
                .primary(),
        );

        let cancel_w = f64::from(tr.measure("Cancel", 14.0, None).0) + 36.0;
        self.cancel_rect = Rect::new(
            self.copy_rect.x0 - 12.0 - cancel_w,
            by,
            self.copy_rect.x0 - 12.0,
            by + 36.0,
        );
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(self.cancel_rect, "Cancel")
                .radius(f64::from(theme.tokens.radii.md))
                .text_size(14.0)
                .weight(TextWeight::Semibold),
        );
    }

    /// Route a window event. Returns the modal's lifecycle outcome.
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut EventCtx<'_>,
        mouse: Point,
    ) -> ExpandOutcome {
        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == ElementState::Pressed
        {
            match &key.logical_key {
                Key::Named(NamedKey::Escape) => return ExpandOutcome::Cancel,
                Key::Named(NamedKey::Enter) => {
                    return ExpandOutcome::Copy(self.preview(), self.snippet_id);
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

        let left_press = matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        );
        if left_press {
            if self.copy_rect.contains(mouse) {
                return ExpandOutcome::Copy(self.preview(), self.snippet_id);
            }
            if self.cancel_rect.contains(mouse) {
                return ExpandOutcome::Cancel;
            }
        }

        let pointed = self.field_rects.iter().position(|r| r.contains(mouse));
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left | MouseButton::Right,
                ..
            }
        ) && let Some(index) = pointed
        {
            self.focused = index;
            self.focus_field(index);
        }

        // Cursor motion reaches every editor so each can maintain accurate
        // hover/drag state. Keyboard and IME events go only to the focused
        // field; pointer events go to the field under the pointer.
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
        } else if let Some(index) = pointed.or_else(|| {
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

    pub fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        self.fields
            .get(self.focused)
            .and_then(|f| f.input.ime_purpose())
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
        for (target, label, bounds) in [
            ("snippets.expand.copy", "Copy", self.copy_rect),
            ("snippets.expand.cancel", "Cancel", self.cancel_rect),
        ] {
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(label)
                    .with_action_target(target)
                    .with_bounds(bounds)
                    .with_state(AccessState {
                        focusable: true,
                        ..Default::default()
                    }),
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
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> Option<ExpandOutcome> {
        if matches!(action, ui::a11y::Action::Activate) {
            match target {
                "snippets.expand.copy" => {
                    return Some(ExpandOutcome::Copy(self.preview(), self.snippet_id));
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
        let accepted = field.input.perform_action(action, state, path);
        field.input.take_dirty();
        accepted.then_some(ExpandOutcome::Stay)
    }

    pub fn needs_commit(&self) -> bool {
        self.fields.iter().any(|f| ui::undo::needs_commit(&f.input))
    }

    pub fn on_commit(&mut self) {
        for field in &mut self.fields {
            ui::undo::on_commit(&mut field.input);
        }
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
        let mut event: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        event.logical_key = Key::Named(named);
        event.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        event.location = winit::keyboard::KeyLocation::Standard;
        event.state = ElementState::Pressed;
        event.repeat = false;
        event.text = None;
        WindowEvent::KeyboardInput {
            device_id: winit::event::DeviceId::dummy(),
            event,
            is_synthetic: false,
        }
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
                state: doc,
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
        let mut s = Snippet::blank("");
        s.title = "Greet".into();
        s.language = Language::Shell;
        s.body = "echo hi ${name} from ${host}".into();
        s
    }

    /// The expand modal: one field per placeholder, preview substitution and IME/backspace editing.
    #[test]
    fn expand_modal_collects_fields_and_previews_live() {
        // Case `modal_collects_one_field_per_placeholder`.
        {
            let m = ExpandModal::new(&templated());
            assert_eq!(m.field_count_for_test(), 2);
        }

        // Case `preview_substitutes_filled_fields`.
        {
            let mut m = ExpandModal::new(&templated());
            m.set_field_for_test("name", "ada");
            m.set_field_for_test("host", "prod");
            assert_eq!(m.preview_for_test(), "echo hi ada from prod");
        }

        // Case `preview_keeps_unfilled_placeholders_visible`.
        {
            let mut m = ExpandModal::new(&templated());
            m.set_field_for_test("name", "ada");
            assert_eq!(m.preview_for_test(), "echo hi ada from ${host}");
        }

        // Case `shared_editor_ime_and_unicode_backspace_drive_live_preview_without_crdt_writes`.
        {
            let mut modal = ExpandModal::new(&templated());
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
