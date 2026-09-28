//! Modal keyboard-shortcuts overlay.
//!
//! The overlay owns its visibility, input policy, and rendering. The root app
//! only decides whether opening it is currently allowed (text editors reserve
//! `?` for typing) and supplies the current viewport and palette when drawing.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::keyboard::{Key, NamedKey};

use ui::paint::PaintCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

use crate::chrome;
use crate::model::Palette;

/// How far the shortcuts dialog dims the editor behind it
/// (`ui::chrome::fill_scrim` strength).
const SCRIM_STRENGTH: f32 = 0.6;

#[derive(Default)]
pub(super) struct HelpOverlay {
    visible: bool,
    /// First shortcut row exposed in the large-text single-column viewport.
    /// This stays local like every other modal viewport concern.
    scroll_row: usize,
}

impl HelpOverlay {
    pub(super) fn is_visible(&self) -> bool {
        self.visible
    }

    pub(super) fn show(&mut self) {
        self.visible = true;
        self.scroll_row = 0;
    }

    pub(super) fn hide(&mut self) {
        self.visible = false;
        self.scroll_row = 0;
    }

    /// Route a key through the modal overlay.
    ///
    /// While visible, every key is consumed so presentation navigation and
    /// editor shortcuts cannot leak through it. When hidden, `?` opens it only
    /// when the caller says the current editing context permits that shortcut.
    pub(super) fn on_key(&mut self, key: &Key, allow_open: bool) -> bool {
        if self.visible {
            if matches!(key, Key::Named(NamedKey::Escape))
                || matches!(key, Key::Character(c) if c.as_str() == "?")
            {
                self.hide();
            } else if matches!(key, Key::Named(NamedKey::PageDown | NamedKey::ArrowDown)) {
                self.scroll_row = (self.scroll_row + 1).min(HELP_ROWS.len().saturating_sub(1));
            } else if matches!(key, Key::Named(NamedKey::PageUp | NamedKey::ArrowUp)) {
                self.scroll_row = self.scroll_row.saturating_sub(1);
            }
            return true;
        }

        if allow_open && matches!(key, Key::Character(c) if c.as_str() == "?") {
            self.visible = true;
            return true;
        }
        false
    }

    /// A visible overlay is modal over the whole viewport: the first pointer
    /// press dismisses it and is swallowed rather than activating content
    /// underneath.
    pub(super) fn on_pointer_press(&mut self, point: Point, viewport: Rect) -> bool {
        if !self.visible || !viewport.contains(point) {
            return false;
        }
        self.visible = false;
        true
    }

    pub(super) fn draw(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        viewport: Rect,
        palette: &Palette,
    ) {
        if !self.visible {
            return;
        }

        let colors = &theme.tokens.colors;
        // Dim the editor in the theme's own scrim, then the dialog panel
        // (HIG §9.1 Panel role) — not a literal black wash under a raised
        // card with a declared-alpha ink stroke.
        ui::chrome::fill_scrim(scene, transform, viewport, SCRIM_STRENGTH, theme);

        let scale = f64::from(theme.text_scale);
        // At 200% a single 20-row column cannot fit in an ordinary desktop
        // height. Give the dialog almost the available frame and arrange the
        // reference in two columns; this keeps its modal content readable
        // rather than silently clipping half the keyboard contract.
        let large_text = scale >= 1.5;
        let width = if large_text {
            1_080.0_f64.min(viewport.width() - 40.0)
        } else {
            580.0_f64.min(viewport.width() - 80.0)
        };
        let height = if large_text {
            (viewport.height() - 40.0).max(200.0)
        } else {
            588.0_f64.min(viewport.height() - 60.0)
        };
        let x = viewport.x0 + (viewport.width() - width) / 2.0;
        let y = viewport.y0 + (viewport.height() - height) / 2.0;
        let card = Rect::new(x, y, x + width, y + height);
        chrome::panel(scene, transform, theme, card, 16.0);

        let content_x = card.x0 + 32.0 * scale;
        text.draw_text_weighted(
            scene,
            "Keyboard shortcuts",
            20.0,
            colors.text_primary,
            transform * Affine::translate((content_x, card.y0 + 24.0 * scale)),
            None,
            TextWeight::Bold,
        );
        text.draw_text_weighted(
            scene,
            "Press ?  or Esc to close",
            12.0,
            colors.text_dim,
            transform * Affine::translate((content_x, card.y0 + 50.0 * scale)),
            None,
            TextWeight::Regular,
        );

        self.draw_rows(
            &mut PaintCtx::new(scene, transform, text, theme),
            palette,
            card,
            content_x,
            large_text,
        );
    }

    fn draw_rows(
        &self,
        ctx: &mut PaintCtx<'_>,
        palette: &Palette,
        card: Rect,
        content_x: f64,
        large_text: bool,
    ) {
        let PaintCtx {
            scene,
            transform,
            text,
            theme,
        } = ctx;
        let (transform, colors) = (*transform, &theme.tokens.colors);
        let scroll_row = self.scroll_row;
        let scale = f64::from(text.text_scale());
        let top = card.y0 + 84.0 * scale;
        let column_width = card.width() - 64.0 * scale;
        let key_width = if large_text { 280.0 } else { 186.0 };
        let row_height = if large_text { 32.0 * scale } else { 25.0 };
        let section_gap = if large_text { 6.0 * scale } else { 6.0 };
        let max_rows = if large_text { 10 } else { HELP_ROWS.len() };
        let start = if large_text {
            scroll_row.min(HELP_ROWS.len().saturating_sub(max_rows))
        } else {
            0
        };
        for (row, (key, description)) in HELP_ROWS.iter().skip(start).take(max_rows).enumerate() {
            let x = content_x;
            let mut y = top + row as f64 * row_height;
            if description.is_empty() {
                y += section_gap;
                text.draw_text_weighted(
                    scene,
                    &key.to_uppercase(),
                    11.0,
                    colors.text_dim,
                    transform * Affine::translate((x, y + 2.0 * scale)),
                    None,
                    TextWeight::Semibold,
                );
                continue;
            }
            text.draw_text_weighted(
                scene,
                key,
                13.0,
                palette.accent,
                transform * Affine::translate((x, y)),
                Some(key_width as f32),
                TextWeight::Semibold,
            );
            text.draw_text_weighted(
                scene,
                description,
                13.0,
                colors.text_primary,
                transform * Affine::translate((x + key_width, y)),
                Some((column_width - key_width - 10.0 * scale) as f32),
                TextWeight::Regular,
            );
        }
        if large_text {
            text.draw_text_weighted(
                scene,
                "Use Page Up / Page Down or ↑ / ↓ to browse all shortcuts",
                11.0,
                colors.text_dim,
                transform * Affine::translate((content_x, card.y1 - 30.0 * scale)),
                None,
                TextWeight::Regular,
            );
        }
    }
}

const HELP_ROWS: [(&str, &str); 20] = [
    ("N", "New slide"),
    ("Click title", "Rename the deck"),
    ("P", "Present from current slide"),
    ("G", "Slide-sorter overview"),
    ("T / R / O / L", "Insert text · rect · ellipse · line"),
    ("Tab", "Cycle selection on the slide"),
    ("Enter", "Edit selected text box"),
    ("Drag", "Move · resize (snaps to guides)"),
    ("Arrows", "Nudge selection (Shift = 1px)"),
    ("[  /  ]", "Send back · bring to front"),
    ("Ctrl-D", "Duplicate element"),
    ("Del / Backspace", "Delete element"),
    ("Ctrl-Z / Ctrl-Y", "Undo · redo"),
    ("Present mode", ""),
    ("← → / Space / click", "Previous · next slide"),
    ("Home / End / 1-9", "First · last · jump to slide N"),
    ("P", "Toggle presenter dashboard"),
    ("S  ·  B / W", "Speaker notes · black / white out"),
    ("K  ·  0", "Pause timer · reset timer"),
    ("?", "Toggle this help"),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The help overlay toggles without leaking keys and owns the next pointer press.
    #[test]
    fn help_overlay_owns_its_keys_and_the_next_press() {
        // Case `shortcut_toggles_and_modal_keys_do_not_leak`.
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

        // Case `visible_overlay_owns_the_next_pointer_press`.
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
    }
}
