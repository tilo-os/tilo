//! Whether a paint put a secret on screen.
//!
//! A secret field that holds a value notes it while it draws, masked or
//! shown. A host brackets its frame in a [`SecretPaint`] and learns whether
//! the frame shows one, so it can withhold the window from screenshots,
//! recordings and screen sharing while it does. Containers that replay a
//! cached subtree instead of drawing it again replay the note with it.

use std::cell::Cell;

thread_local! {
    static PAINTED: Cell<bool> = const { Cell::new(false) };
}

/// Record that the paint running on this thread drew a secret.
pub fn note_secret_painted() {
    PAINTED.with(|painted| painted.set(true));
}

/// Watches one paint (a frame, or a cached subtree) for secrets. Scopes
/// nest: a secret seen inside an inner scope is also seen by every
/// enclosing one.
#[must_use = "the scope reports only through `finish`"]
pub struct SecretPaint {
    outer: bool,
}

impl SecretPaint {
    pub fn begin() -> Self {
        Self {
            outer: PAINTED.with(|painted| painted.replace(false)),
        }
    }

    /// Whether anything painted since [`Self::begin`] was a secret.
    pub fn finish(self) -> bool {
        PAINTED.with(Cell::get)
    }
}

impl Drop for SecretPaint {
    fn drop(&mut self) {
        let outer = self.outer;
        PAINTED.with(|painted| painted.set(outer || painted.get()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_render::TextRenderer;
    use crate::theme::Theme;
    use crate::widget::{Column, Row, Widget};
    use vello::Scene;
    use vello::kurbo::Affine;

    /// A settled leaf that draws a secret: its container replays it from
    /// cache instead of drawing it again.
    struct SettledSecret;

    impl Widget for SettledSecret {
        crate::testing::inert_widget_methods!(leaf_layout, name = "settled-secret");
        fn draw(
            &mut self,
            _: &mut Scene,
            _: Affine,
            _: &mut TextRenderer,
            _: &Theme,
            _: &crate::presence::PresenceCtx,
        ) {
            note_secret_painted();
        }
        fn needs_redraw(&self) -> bool {
            false
        }
    }

    fn frame_shows_secret(widget: &mut dyn Widget) -> bool {
        let presence = crate::presence::PresenceCtx::empty();
        let mut text = TextRenderer::new();
        let secrets = SecretPaint::begin();
        widget.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut text,
            &Theme::dark(),
            &presence,
        );
        secrets.finish()
    }

    #[test]
    fn a_cached_subtree_still_reports_its_secret() {
        let mut column = Column::new().push(Box::new(SettledSecret));
        let mut row = Row::new().push(Box::new(SettledSecret));
        for container in [&mut column as &mut dyn Widget, &mut row] {
            assert!(frame_shows_secret(container), "the first frame draws it");
            assert!(frame_shows_secret(container), "a cached frame replays it");
        }
        let mut plain = Column::new().push(Box::new(Column::new()));
        assert!(!frame_shows_secret(&mut plain));
        assert!(!frame_shows_secret(&mut plain));
    }

    #[test]
    fn a_scope_sees_only_its_own_paint_and_passes_it_outwards() {
        let frame = SecretPaint::begin();
        {
            let plain = SecretPaint::begin();
            assert!(!plain.finish());
        }
        {
            let subtree = SecretPaint::begin();
            note_secret_painted();
            assert!(subtree.finish());
        }
        {
            let later = SecretPaint::begin();
            assert!(!later.finish(), "a sibling starts clean");
        }
        assert!(frame.finish(), "the frame saw the subtree's secret");
        let next = SecretPaint::begin();
        assert!(!next.finish(), "the next frame starts clean");
    }
}
