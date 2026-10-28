//! The keyboard reference, presented in the shared modal sheet.
//!
//! The overlay owns its visibility, input policy and scroll. The root app
//! only decides whether opening it is currently allowed (text editors reserve
//! `?` for typing) and hands it the window to lay the sheet out against.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};
use winit::keyboard::{Key, NamedKey};

use ui::TextRole;
use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::modal_dialog::{self, ModalLayout, ModalSpec};
use ui::widget::{PropertyListFace, PropertyRow};

use crate::strings::tr;

#[derive(Default)]
pub(super) struct HelpOverlay {
    visible: bool,
    /// How far the sheet's body is scrolled, in logical pixels.
    scroll: f64,
    /// The furthest the body scrolled at the last paint.
    max_scroll: f64,
    /// Lines a key press scrolls by, measured at the last paint.
    step: f64,
    layout: Option<ModalLayout>,
}

impl HelpOverlay {
    pub(super) fn is_visible(&self) -> bool {
        self.visible
    }

    pub(super) fn show(&mut self) {
        self.visible = true;
        self.scroll = 0.0;
    }

    pub(super) fn hide(&mut self) {
        self.visible = false;
        self.scroll = 0.0;
        self.layout = None;
    }

    /// Route a key through the modal sheet.
    ///
    /// While visible, every key is consumed so presentation navigation and
    /// editor shortcuts cannot leak through it. When hidden, `?` opens it only
    /// when the caller says the current editing context permits that shortcut.
    pub(super) fn on_key(&mut self, key: &Key, allow_open: bool) -> bool {
        if self.visible {
            let step = self.step.max(1.0);
            if matches!(key, Key::Named(NamedKey::Escape))
                || matches!(key, Key::Character(c) if c.as_str() == "?")
            {
                self.hide();
            } else if matches!(key, Key::Named(NamedKey::PageDown | NamedKey::ArrowDown)) {
                self.scroll = (self.scroll + step).min(self.max_scroll);
            } else if matches!(key, Key::Named(NamedKey::PageUp | NamedKey::ArrowUp)) {
                self.scroll = (self.scroll - step).max(0.0);
            }
            return true;
        }

        if allow_open && matches!(key, Key::Character(c) if c.as_str() == "?") {
            self.show();
            return true;
        }
        false
    }

    /// A visible sheet is modal over the whole window: a press on its close
    /// control or on the scrim dismisses it, and every press is swallowed
    /// rather than activating content underneath.
    pub(super) fn on_pointer_press(&mut self, point: Point, window: Rect) -> bool {
        if !self.visible || !window.contains(point) {
            return false;
        }
        let dismisses = self
            .layout
            .as_ref()
            .is_none_or(|layout| layout.dismisses(point));
        if dismisses {
            self.hide();
        }
        true
    }

    /// The reference as plain lines, for assistive technology.
    #[cfg(test)]
    pub(super) fn spoken_rows() -> Vec<String> {
        sections()
            .into_iter()
            .flat_map(|(_, rows)| rows)
            .map(|(key, action)| format!("{key}: {action}"))
            .collect()
    }

    /// Paint the sheet over `window`.
    pub(super) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        window: Rect,
    ) {
        if !self.visible {
            return;
        }
        let title = tr("slides-shortcuts-title");
        let sections = sections();
        let sp = &theme.tokens.spacing;
        let gap = f64::from(sp.s6);
        let label_gap = f64::from(sp.s2);
        let heading = f64::from(theme.text(TextRole::Subheading).line_height_px());
        let lists: Vec<(String, Vec<PropertyRow<'_>>)> = sections
            .iter()
            .map(|(name, rows)| {
                (
                    name.clone(),
                    rows.iter()
                        .map(|(key, action)| PropertyRow::new(key, action))
                        .collect(),
                )
            })
            .collect();
        // The sections stand side by side where the sheet is wide enough
        // for two readable columns, else one under the other.
        let wide = modal_dialog::body_width(window, wide_sheet(), theme);
        let column_min = f64::from(theme.tokens.metrics.panel_width_utility);
        let columns = if wide >= 2.0 * column_min + gap {
            lists.len().max(1)
        } else {
            1
        };
        let max_width = if columns > 1 {
            wide_sheet()
        } else {
            modal_dialog::MODAL_MAX_WIDTH
        };
        let body_w = modal_dialog::body_width(window, max_width, theme);
        let column_w = (body_w - gap * (columns as f64 - 1.0)) / columns as f64;
        let heights: Vec<f64> = lists
            .iter()
            .map(|(_, rows)| {
                heading + label_gap + PropertyListFace::new(rows).height(text, theme, column_w)
            })
            .collect();
        let body_h = if columns > 1 {
            heights.iter().copied().fold(0.0_f64, f64::max)
        } else {
            heights.iter().sum::<f64>() + gap * (heights.len().saturating_sub(1)) as f64
        };

        let spec = ModalSpec::new(&title)
            .body_height(body_h)
            .max_width(max_width);
        let layout = modal_dialog::layout(window, &spec, text, theme);
        modal_dialog::paint_backdrop(scene, transform, text, theme, &layout, &spec);
        let body = layout.body;
        self.max_scroll = (body_h - body.height()).max(0.0);
        self.scroll = self.scroll.clamp(0.0, self.max_scroll);
        self.step = f64::from(theme.text(TextRole::Body).line_height_px()) * 2.0;

        let sheet = theme.on_ground(ui::chrome::panel_ground(theme));
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &body);
        let mut ctx = PaintCtx::new(scene, transform, text, &sheet);
        let mut x = body.x0;
        let mut y = body.y0 - self.scroll;
        for ((name, rows), h) in lists.iter().zip(&heights) {
            ctx.text_role(
                name,
                TextRole::Subheading,
                sheet.tokens.colors.text_primary,
                Point::new(x, y),
                None,
            );
            let top = y + heading + label_gap;
            PropertyListFace::new(rows).paint(&mut ctx, Rect::new(x, top, x + column_w, y + h));
            if columns > 1 {
                x += column_w + gap;
            } else {
                y += h + gap;
            }
        }
        scene.pop_layer();
        self.layout = Some(layout);
    }
}

/// Widest the sheet is drawn when the sections stand side by side: two
/// list panels.
fn wide_sheet() -> f64 {
    2.0 * f64::from(ui::tokens::Metrics::BASE.panel_width_list)
}

/// The reference, by section: `(key, what it does)`.
fn sections() -> Vec<(String, Vec<(String, String)>)> {
    let rows = |keys: &[(&str, &str)]| -> Vec<(String, String)> {
        keys.iter()
            .map(|(key, action)| ((*key).to_owned(), tr(action)))
            .collect()
    };
    vec![
        (
            tr("slides-shortcuts-editing"),
            rows(&[
                ("N", "slides-key-new-slide"),
                ("P", "slides-key-present"),
                ("G", "slides-key-overview"),
                ("T, R, O, L", "slides-key-insert"),
                ("Tab", "slides-key-cycle"),
                ("Enter", "slides-key-edit-text"),
                ("Arrows", "slides-key-nudge"),
                ("[ and ]", "slides-key-restack"),
                ("Ctrl+D", "slides-key-duplicate"),
                ("Delete", "slides-key-delete"),
                ("Ctrl+Z, Ctrl+Y", "slides-key-undo"),
                ("?", "slides-key-help"),
            ]),
        ),
        (
            tr("slides-shortcuts-presenting"),
            rows(&[
                ("Left, Right, Space", "slides-key-step"),
                ("Home, End", "slides-key-first-last"),
                ("1 to 9", "slides-key-jump"),
                ("P", "slides-key-presenter"),
                ("S", "slides-key-notes"),
                ("B, W", "slides-key-blank"),
                ("K, 0", "slides-key-timer"),
                ("Esc", "slides-key-exit"),
            ]),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sheet toggles without leaking keys and owns the next pointer press.
    #[test]
    fn help_overlay_owns_its_keys_and_the_next_press() {
        {
            let mut overlay = HelpOverlay::default();
            let question = Key::Character("?".into());

            assert!(!overlay.on_key(&question, false));
            assert!(!overlay.is_visible());
            assert!(overlay.on_key(&question, true));
            assert!(overlay.is_visible());

            assert!(overlay.on_key(&Key::Named(NamedKey::ArrowRight), true));
            assert!(overlay.is_visible(), "unrelated modal keys are swallowed");
            assert!(overlay.on_key(&Key::Named(NamedKey::Escape), true));
            assert!(!overlay.is_visible());
        }

        {
            let mut overlay = HelpOverlay::default();
            let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
            let inside = Point::new(400.0, 300.0);
            assert!(!overlay.on_pointer_press(inside, viewport));
            overlay.show();
            assert!(!overlay.on_pointer_press(Point::new(900.0, 300.0), viewport));
            assert!(overlay.is_visible(), "an out-of-viewport press is ignored");
            assert!(overlay.on_pointer_press(inside, viewport));
            assert!(!overlay.is_visible());
            assert!(!overlay.on_pointer_press(inside, viewport));
        }

        {
            let spoken = HelpOverlay::spoken_rows();
            assert!(spoken.iter().any(|row| row.starts_with("N: ")));
        }
    }
}
