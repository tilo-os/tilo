//! The expand-with-variables modal.
//!
//! When the user copies a snippet whose body contains `${placeholder}`
//! variables, this modal collects a value for each one, shows a live
//! preview of the substituted result, and copies the filled text. It is
//! a self-contained overlay: it draws itself above the rest of the app
//! and consumes all input while open. State is local (it's a transient
//! per-peer flow, not part of the shared library).

use std::collections::HashMap;

use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::app::ExpandOutcome;
use crate::draw;
use crate::highlight;
use crate::model::{self, Snippet, SnippetId};

/// One placeholder field: its name and the user's current value.
struct Field {
    name: String,
    value: String,
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
        let fields = s
            .placeholders()
            .into_iter()
            .map(|name| Field {
                name,
                value: String::new(),
            })
            .collect();
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
            .map(|f| (f.name.clone(), f.value.clone()))
            .collect()
    }

    /// The expanded body with current field values applied.
    fn preview(&self) -> String {
        model::expand(&self.body, &self.values())
    }

    /// True once every field has a non-empty value.
    fn all_filled(&self) -> bool {
        self.fields.iter().all(|f| !f.value.trim().is_empty())
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
        let c = &theme.tokens.colors;
        // Scrim.
        draw::fill_rect(scene, t, area, c.surfaces.overlay);

        // Panel centred horizontally, biased toward the top.
        let pw = 640.0_f64.min(area.width() - 80.0);
        let ph = 520.0_f64.min(area.height() - 80.0);
        let px = area.x0 + (area.width() - pw) / 2.0;
        let py = area.y0 + (area.height() - ph) / 2.0;
        let panel = Rect::new(px, py, px + pw, py + ph);
        draw::fill_round(
            scene,
            t,
            panel,
            f64::from(theme.tokens.radii.lg),
            c.surfaces.raised,
        );
        draw::stroke_round(
            scene,
            t,
            panel,
            f64::from(theme.tokens.radii.lg),
            1.0,
            c.border_standard,
        );

        let pad = 24.0;
        let mut y = panel.y0 + pad;

        draw::text(
            scene,
            tr,
            t,
            "Fill in variables",
            panel.x0 + pad,
            y,
            18.0,
            c.text_primary,
            TextWeight::Bold,
        );
        y += 24.0;
        draw::text(
            scene,
            tr,
            t,
            &self.title,
            panel.x0 + pad,
            y,
            13.0,
            c.text_dim,
            TextWeight::Regular,
        );
        y += 30.0;

        // Field inputs.
        self.field_rects.clear();
        let label_w = 130.0;
        for (i, f) in self.fields.iter().enumerate() {
            let row_y = y;
            // Label as ${name}.
            let lbl = format!("${{{}}}", f.name);
            draw::mono(
                scene,
                tr,
                t,
                &lbl,
                panel.x0 + pad,
                row_y + 7.0,
                13.0,
                c.accent[5],
                TextWeight::Semibold,
            );
            // Input box.
            let input = Rect::new(
                panel.x0 + pad + label_w,
                row_y,
                panel.x1 - pad,
                row_y + 30.0,
            );
            let focused = i == self.focused;
            draw::fill_round(
                scene,
                t,
                input,
                f64::from(theme.tokens.radii.md),
                c.surfaces.base,
            );
            draw::stroke_round(
                scene,
                t,
                input,
                f64::from(theme.tokens.radii.md),
                if focused { 1.5 } else { 1.0 },
                if focused {
                    c.border_focus
                } else {
                    c.border_standard
                },
            );
            let shown = if f.value.is_empty() {
                "type a value…".to_string()
            } else {
                f.value.clone()
            };
            let vcolor = if f.value.is_empty() {
                c.text_dim
            } else {
                c.text_primary
            };
            draw::text(
                scene,
                tr,
                t,
                &shown,
                input.x0 + 10.0,
                input.y0 + 8.0,
                13.0,
                vcolor,
                TextWeight::Regular,
            );
            if focused {
                let tw = if f.value.is_empty() {
                    0.0
                } else {
                    f64::from(tr.measure(&f.value, 13.0, None).0)
                };
                let cx = input.x0 + 10.0 + tw + 1.0;
                draw::fill_rect(
                    scene,
                    t,
                    Rect::new(cx, input.y0 + 7.0, cx + 1.5, input.y0 + 23.0),
                    c.border_focus,
                );
            }
            self.field_rects.push(input);
            y += 40.0;
        }

        if self.fields.is_empty() {
            draw::text(
                scene,
                tr,
                t,
                "This snippet has no variables.",
                panel.x0 + pad,
                y,
                13.0,
                c.text_dim,
                TextWeight::Regular,
            );
            y += 30.0;
        }

        // Live preview header.
        y += 4.0;
        draw::text(
            scene,
            tr,
            t,
            "PREVIEW",
            panel.x0 + pad,
            y,
            11.0,
            c.text_dim,
            TextWeight::Bold,
        );
        y += 18.0;

        // Preview card with highlighted, substituted body.
        let card = Rect::new(panel.x0 + pad, y, panel.x1 - pad, panel.y1 - 72.0);
        draw::fill_round(
            scene,
            t,
            card,
            f64::from(theme.tokens.radii.md),
            c.neutral[0],
        );
        draw::stroke_round(
            scene,
            t,
            card,
            f64::from(theme.tokens.radii.md),
            1.0,
            c.border_standard,
        );
        let preview = self.preview();
        let mut cy = card.y0 + 12.0;
        for line in preview.lines() {
            if cy > card.y1 - 18.0 {
                break;
            }
            let mut sx = card.x0 + 14.0;
            for span in highlight::highlight_line(line, self.language) {
                let color = span.kind.color(theme);
                draw::mono(
                    scene,
                    tr,
                    t,
                    &span.text,
                    sx,
                    cy,
                    13.0,
                    color,
                    TextWeight::Regular,
                );
                sx += draw::mono_width(tr, &span.text, 13.0);
            }
            cy += 19.0;
        }

        // Buttons.
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
        mouse: Point,
        _modifiers: &winit::event::Modifiers,
    ) -> ExpandOutcome {
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.copy_rect.contains(mouse) {
                    return ExpandOutcome::Copy(self.preview(), self.snippet_id);
                }
                if self.cancel_rect.contains(mouse) {
                    return ExpandOutcome::Cancel;
                }
                for (i, r) in self.field_rects.iter().enumerate() {
                    if r.contains(mouse) {
                        self.focused = i;
                        return ExpandOutcome::Stay;
                    }
                }
                ExpandOutcome::Stay
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => self.handle_key(logical_key),
            _ => ExpandOutcome::Stay,
        }
    }

    fn handle_key(&mut self, key: &Key) -> ExpandOutcome {
        match key {
            Key::Named(NamedKey::Escape) => ExpandOutcome::Cancel,
            Key::Named(NamedKey::Enter) => ExpandOutcome::Copy(self.preview(), self.snippet_id),
            Key::Named(NamedKey::Tab) => {
                if !self.fields.is_empty() {
                    self.focused = (self.focused + 1) % self.fields.len();
                }
                ExpandOutcome::Stay
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some(f) = self.fields.get_mut(self.focused) {
                    f.value.pop();
                }
                ExpandOutcome::Stay
            }
            Key::Named(NamedKey::Space) => {
                if let Some(f) = self.fields.get_mut(self.focused) {
                    f.value.push(' ');
                }
                ExpandOutcome::Stay
            }
            Key::Character(s) => {
                if let Some(f) = self.fields.get_mut(self.focused) {
                    f.value.push_str(s.as_str());
                }
                ExpandOutcome::Stay
            }
            _ => ExpandOutcome::Stay,
        }
    }

    // ---- test accessors -------------------------------------------------

    #[doc(hidden)]
    pub fn set_field_for_test(&mut self, name: &str, value: &str) {
        if let Some(f) = self.fields.iter_mut().find(|f| f.name == name) {
            f.value = value.to_string();
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Language, Snippet};

    fn templated() -> Snippet {
        let mut s = Snippet::blank("");
        s.title = "Greet".into();
        s.language = Language::Shell;
        s.body = "echo hi ${name} from ${host}".into();
        s
    }

    #[test]
    fn modal_collects_one_field_per_placeholder() {
        let m = ExpandModal::new(&templated());
        assert_eq!(m.field_count_for_test(), 2);
    }

    #[test]
    fn preview_substitutes_filled_fields() {
        let mut m = ExpandModal::new(&templated());
        m.set_field_for_test("name", "ada");
        m.set_field_for_test("host", "prod");
        assert_eq!(m.preview_for_test(), "echo hi ada from prod");
    }

    #[test]
    fn preview_keeps_unfilled_placeholders_visible() {
        let mut m = ExpandModal::new(&templated());
        m.set_field_for_test("name", "ada");
        assert_eq!(m.preview_for_test(), "echo hi ada from ${host}");
    }
}
