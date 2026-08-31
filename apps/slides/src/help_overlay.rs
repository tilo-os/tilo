//! Modal keyboard-shortcuts overlay.
//!
//! The overlay owns its visibility, input policy, and rendering. The root app
//! only decides whether opening it is currently allowed (text editors reserve
//! `?` for typing) and supplies the current viewport and palette when drawing.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;
use winit::keyboard::{Key, NamedKey};

use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

use crate::chrome;
use crate::model::Palette;
use crate::render;

#[derive(Default)]
pub(super) struct HelpOverlay {
    visible: bool,
}

impl HelpOverlay {
    #[cfg(test)]
    pub(super) fn is_visible(&self) -> bool {
        self.visible
    }

    pub(super) fn show(&mut self) {
        self.visible = true;
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
                self.visible = false;
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
        chrome::fill_rect(
            scene,
            transform,
            viewport,
            render::with_alpha(Color::BLACK, 150),
        );

        let width = 580.0_f64.min(viewport.width() - 80.0);
        let height = 588.0_f64.min(viewport.height() - 60.0);
        let x = viewport.x0 + (viewport.width() - width) / 2.0;
        let y = viewport.y0 + (viewport.height() - height) / 2.0;
        let card = Rect::new(x, y, x + width, y + height);
        chrome::fill_rrect(scene, transform, card, 16.0, colors.surfaces.raised);
        chrome::stroke_rrect(
            scene,
            transform,
            card,
            16.0,
            render::with_alpha(colors.text_primary, 30),
            1.0,
        );

        let content_x = card.x0 + 32.0;
        text.draw_text_weighted(
            scene,
            "Keyboard shortcuts",
            20.0,
            colors.text_primary,
            transform * Affine::translate((content_x, card.y0 + 24.0)),
            None,
            TextWeight::Bold,
        );
        text.draw_text_weighted(
            scene,
            "Press ?  or Esc to close",
            12.0,
            colors.text_dim,
            transform * Affine::translate((content_x, card.y0 + 50.0)),
            None,
            TextWeight::Regular,
        );

        Self::draw_rows(scene, transform, text, colors, palette, card, content_x);
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the shared draw context plus the help card geometry"
    )]
    fn draw_rows(
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        colors: &ui::tokens::Colors,
        palette: &Palette,
        card: Rect,
        content_x: f64,
    ) {
        let rows: [(&str, &str); 20] = [
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
        let mut y = card.y0 + 84.0;
        let key_width = 186.0;
        for (key, description) in rows {
            if description.is_empty() {
                y += 6.0;
                text.draw_text_weighted(
                    scene,
                    &key.to_uppercase(),
                    11.0,
                    colors.text_dim,
                    transform * Affine::translate((content_x, y + 2.0)),
                    None,
                    TextWeight::Semibold,
                );
                y += 22.0;
                continue;
            }
            text.draw_text_weighted(
                scene,
                key,
                13.0,
                palette.accent,
                transform * Affine::translate((content_x, y)),
                Some(key_width as f32),
                TextWeight::Semibold,
            );
            text.draw_text_weighted(
                scene,
                description,
                13.0,
                colors.text_primary,
                transform * Affine::translate((content_x + key_width, y)),
                Some((card.width() - key_width - 64.0) as f32),
                TextWeight::Regular,
            );
            y += 25.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_toggles_and_modal_keys_do_not_leak() {
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

    #[test]
    fn visible_overlay_owns_the_next_pointer_press() {
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
