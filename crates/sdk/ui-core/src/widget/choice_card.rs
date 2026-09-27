//! Shared rich choice card: icon tile, title, description, and affordance.
//!
//! Use this for a small set of prominent paths such as import methods or
//! setup choices. The face owns geometry, interaction feedback, typography,
//! icon treatment, and semantics so those details cannot drift per app.

use std::borrow::Cow;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Vec2};
use vello::peniko::Fill;

use crate::a11y::AccessNode;
use crate::color_compat::ColorCompat;
use crate::icons::{Icon, LUCIDE_DEFAULT_STROKE_PX, draw_icon};
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;

use super::ActionRowFace;
use super::button::ButtonInteract;

#[derive(Clone, Debug)]
pub struct ChoiceCardFace<'a> {
    rect: Rect,
    icon: Icon,
    title: Cow<'a, str>,
    description: Cow<'a, str>,
    emphasized: bool,
    interact: ButtonInteract,
}

impl<'a> ChoiceCardFace<'a> {
    pub fn new(
        rect: Rect,
        icon: Icon,
        title: impl Into<Cow<'a, str>>,
        description: impl Into<Cow<'a, str>>,
    ) -> Self {
        Self {
            rect,
            icon,
            title: title.into(),
            description: description.into(),
            emphasized: false,
            interact: ButtonInteract::default(),
        }
    }

    #[must_use]
    pub fn emphasized(mut self, value: bool) -> Self {
        self.emphasized = value;
        self
    }

    #[must_use]
    pub fn hovered(mut self, value: bool) -> Self {
        self.interact.hovered = value;
        self
    }

    #[must_use]
    pub fn focused(mut self, value: bool) -> Self {
        self.interact.focused = value;
        self
    }

    #[must_use]
    pub fn disabled(mut self, value: bool) -> Self {
        self.interact.disabled = value;
        self
    }

    pub fn hit_test(&self, point: Point) -> bool {
        !self.interact.disabled && self.rect.contains(point)
    }

    pub fn accessibility(&self, target: impl Into<String>) -> AccessNode {
        ActionRowFace::new(self.rect, self.title.clone())
            .value(self.description.clone())
            .focused(self.interact.focused)
            .disabled(self.interact.disabled)
            .accessibility(target)
    }

    pub fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        ActionRowFace::new(self.rect, self.title.clone())
            .accented(self.emphasized)
            .outlined(!self.emphasized)
            .hovered(self.interact.hovered)
            .focused(self.interact.focused)
            .disabled(self.interact.disabled)
            .radius(f64::from(theme.tokens.radii.lg))
            .paint(scene, transform, theme);

        let foreground = if self.emphasized {
            theme.tokens.colors.on_accent(4.5)
        } else {
            theme.tokens.colors.text_primary
        };
        let secondary = if self.emphasized {
            foreground.with_alpha_factor(0.82)
        } else {
            theme.tokens.colors.text_dim
        };
        let icon_tile = Rect::new(
            self.rect.x0 + 14.0,
            self.rect.center().y - 16.0,
            self.rect.x0 + 46.0,
            self.rect.center().y + 16.0,
        );
        let tile_fill = if self.emphasized {
            crate::tokens::Colors::hairline_on(theme.accent_primary, 46)
        } else {
            theme.accent_primary.with_alpha_factor(0.14)
        };
        scene.fill(
            Fill::NonZero,
            transform,
            tile_fill,
            None,
            &RoundedRect::from_rect(icon_tile, f64::from(theme.tokens.radii.md)),
        );
        draw_icon(
            scene,
            transform,
            icon_tile.inflate(-8.0, -8.0),
            self.icon,
            if self.emphasized {
                foreground
            } else {
                theme.accent_primary
            },
            LUCIDE_DEFAULT_STROKE_PX,
        );

        let text_x = icon_tile.x1 + 14.0;
        let text_width = px((self.rect.x1 - text_x - 36.0).max(0.0));
        let title = text_renderer.elide_to_width_weighted(
            self.title.as_ref(),
            15.0,
            text_width,
            TextWeight::Semibold,
        );
        let description = text_renderer.elide_to_width(self.description.as_ref(), 12.0, text_width);
        text_renderer.draw_text_weighted(
            scene,
            &title,
            15.0,
            foreground,
            transform.pre_translate(Vec2::new(text_x, self.rect.center().y - 16.0)),
            None,
            TextWeight::Semibold,
        );
        text_renderer.draw_text(
            scene,
            &description,
            12.0,
            secondary,
            transform.pre_translate(Vec2::new(text_x, self.rect.center().y + 3.0)),
            None,
        );
        draw_icon(
            scene,
            transform,
            Rect::from_center_size(
                Point::new(self.rect.x1 - 22.0, self.rect.center().y),
                (18.0, 18.0),
            ),
            Icon::ChevronRight,
            secondary,
            LUCIDE_DEFAULT_STROKE_PX,
        );
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "bounded logical widget extents are safely representable for text layout"
)]
fn px(value: f64) -> f32 {
    value as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a11y::Role;

    #[test]
    fn choice_card_bounds_drive_semantics_hits_and_elision() {
        fn semantics_and_hit_testing_share_the_card_bounds() {
            let rect = Rect::new(10.0, 20.0, 310.0, 76.0);
            let face = ChoiceCardFace::new(rect, Icon::Image, "Scan image", "Choose a screenshot");
            assert!(face.hit_test(rect.center()));
            assert!(!face.hit_test(Point::ZERO));
            let node = face.accessibility("scan");
            assert_eq!(node.role, Role::Button);
            assert_eq!(node.name.as_deref(), Some("Scan image"));
            assert_eq!(node.value.as_deref(), Some("Choose a screenshot"));
            assert_eq!(node.action_target.as_deref(), Some("scan"));
            assert_eq!(
                node.bounds.map(crate::a11y::AccessBounds::to_rect),
                Some(rect)
            );
        }

        fn narrow_card_elides_copy_instead_of_wrapping_beyond_fixed_height() {
            let rect = Rect::new(0.0, 0.0, 264.0, 56.0);
            let face = ChoiceCardFace::new(
                rect,
                Icon::Image,
                "Scan a QR image with an unusually long localized title",
                "Read the code from a screenshot or photo with a long localized description",
            );
            let mut renderer = TextRenderer::new();
            let mut scene = Scene::new();
            face.paint(&mut scene, Affine::IDENTITY, &mut renderer, &Theme::dark());
            assert!(!scene.encoding().draw_tags.is_empty());

            let available = px(rect.x1 - (rect.x0 + 46.0 + 14.0) - 36.0);
            let title = renderer.elide_to_width_weighted(
                face.title.as_ref(),
                15.0,
                available,
                TextWeight::Semibold,
            );
            let description = renderer.elide_to_width(face.description.as_ref(), 12.0, available);
            assert_ne!(title, face.title.as_ref());
            assert_ne!(description, face.description.as_ref());
            assert!(title.ends_with('…'));
            assert!(description.ends_with('…'));
        }

        semantics_and_hit_testing_share_the_card_bounds();
        narrow_card_elides_copy_instead_of_wrapping_beyond_fixed_height();
    }
}
