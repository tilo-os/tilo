//! Shared rich choice card: icon tile, title, description, and affordance.
//!
//! Use this for a small set of prominent paths such as import methods or
//! setup choices. The face owns geometry, interaction feedback, typography,
//! icon treatment, and semantics so those details cannot drift per app.
//! [`ChoiceCard`] is the retained widget over the same face: the whole card
//! is one pointer target and one keyboard focus stop, activated like a
//! [`Button`](super::Button).

use std::borrow::Cow;
use std::collections::HashMap;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Vec2};
use vello::peniko::Fill;
use winit::event::WindowEvent;

use crate::a11y::AccessNode;
use crate::color_compat::ColorCompat;
use crate::icons::{Icon, LUCIDE_DEFAULT_STROKE_PX, draw_icon};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;

use super::button::ButtonInteract;
use super::{ActionRowFace, Button, ModelPath, TaffyTreeExt, Widget};

/// The card's geometry, all from theme tokens.
#[derive(Clone, Copy)]
struct Geometry {
    /// Inset between the card edge and its icon tile, text and chevron.
    pad: f64,
    icon_tile: f64,
    icon_glyph: f64,
    text_gap: f64,
    chevron: f64,
}

impl Geometry {
    fn of(theme: &Theme) -> Self {
        let spacing = &theme.tokens.spacing;
        let metrics = &theme.tokens.metrics;
        Self {
            pad: f64::from(spacing.s4),
            icon_tile: f64::from(metrics.icon_lg + spacing.s4),
            icon_glyph: f64::from(metrics.icon_md),
            text_gap: f64::from(spacing.s1) / 2.0,
            chevron: f64::from(metrics.icon_md),
        }
    }

    /// The card at 1x: the tile with a full inset above and below reads as
    /// one calm row rather than a cramped list item.
    fn min_height(self) -> f64 {
        self.icon_tile + 2.0 * self.pad
    }

    /// Where the title and description start, and how wide they may run.
    fn text_span(self, rect: Rect) -> (f64, f64) {
        let x = rect.x0 + self.pad + self.icon_tile + self.pad;
        (
            x,
            (rect.x1 - self.pad - self.chevron - self.pad - x).max(0.0),
        )
    }
}

/// The authored minimum, or both text lines plus padding once larger type
/// needs more.
fn card_height(theme: &Theme) -> f64 {
    let geometry = Geometry::of(theme);
    let text = f64::from(theme.text(TextRole::Subheading).line_height_px())
        + geometry.text_gap
        + f64::from(theme.text(TextRole::Body).line_height_px());
    (text + 2.0 * geometry.pad)
        .ceil()
        .max(geometry.min_height())
}

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
    pub fn pressed(mut self, value: bool) -> Self {
        self.interact.pressed = value;
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

    #[must_use]
    fn interact(mut self, interact: ButtonInteract) -> Self {
        self.interact = interact;
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
            .pressed(self.interact.pressed)
            .focused(self.interact.focused)
            .disabled(self.interact.disabled)
            .radius(f64::from(theme.tokens.radii.lg))
            .paint(scene, transform, theme);

        // A pressed card drops to the shared active ground, where accent ink
        // would no longer read.
        let on_accent = self.emphasized && !self.interact.pressed;
        let foreground = if on_accent {
            theme.tokens.colors.on_accent(4.5)
        } else {
            theme.tokens.colors.text_primary
        };
        let secondary = if on_accent {
            foreground.with_alpha_factor(0.82)
        } else {
            theme.tokens.colors.text_dim
        };
        let geometry = Geometry::of(theme);
        let center_y = self.rect.center().y;
        let icon_tile = Rect::new(
            self.rect.x0 + geometry.pad,
            center_y - geometry.icon_tile / 2.0,
            self.rect.x0 + geometry.pad + geometry.icon_tile,
            center_y + geometry.icon_tile / 2.0,
        );
        let tile_fill = if on_accent {
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
            Rect::from_center_size(
                icon_tile.center(),
                (geometry.icon_glyph, geometry.icon_glyph),
            ),
            self.icon,
            if on_accent {
                foreground
            } else {
                theme.accent_primary
            },
            LUCIDE_DEFAULT_STROKE_PX,
        );

        let title_style = theme.text(TextRole::Subheading);
        let description_style = theme.text(TextRole::Body);
        let (text_x, text_width) = geometry.text_span(self.rect);
        let text_width = px(text_width);
        let title =
            text_renderer.elide_to_width_style(self.title.as_ref(), text_width, &title_style);
        let description = text_renderer.elide_to_width_style(
            self.description.as_ref(),
            text_width,
            &description_style,
        );
        let (_, title_height) = text_renderer.measure_style(&title, None, &title_style);
        let (_, description_height) =
            text_renderer.measure_style(&description, None, &description_style);
        let block = f64::from(title_height) + geometry.text_gap + f64::from(description_height);
        let top = center_y - block / 2.0;
        text_renderer.draw_style(
            scene,
            &title,
            foreground,
            transform.pre_translate(Vec2::new(text_x, top)),
            None,
            &title_style,
        );
        text_renderer.draw_style(
            scene,
            &description,
            secondary,
            transform.pre_translate(Vec2::new(
                text_x,
                top + f64::from(title_height) + geometry.text_gap,
            )),
            None,
            &description_style,
        );
        draw_icon(
            scene,
            transform,
            Rect::from_center_size(
                Point::new(
                    self.rect.x1 - geometry.pad - geometry.chevron / 2.0,
                    center_y,
                ),
                (geometry.chevron, geometry.chevron),
            ),
            Icon::ChevronRight,
            secondary,
            LUCIDE_DEFAULT_STROKE_PX,
        );
    }
}

/// A full-width choice that opens its own flow: icon tile, title, a one-line
/// description and a trailing chevron, all one target.
///
/// Activation follows [`Button`]: a click, Enter, Space or an assistive
/// `Activate` runs [`ChoiceCard::on_click`].
pub struct ChoiceCard {
    icon: Icon,
    title: String,
    description: String,
    emphasized: bool,
    /// Owns hover, press, focus, enablement, activation and semantics; the
    /// card paints its own face over that contract.
    control: Button,
    rect: Rect,
}

impl ChoiceCard {
    pub fn new(
        id: &str,
        icon: Icon,
        title: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        let title = title.into();
        Self {
            icon,
            control: Button::new(id, &title),
            title,
            description: description.into(),
            emphasized: false,
            rect: Rect::ZERO,
        }
    }

    /// Run `f` on each activation: a click, Enter, Space or an assistive
    /// `Activate`.
    #[must_use]
    pub fn on_click<F: FnMut() + 'static>(mut self, f: F) -> Self {
        self.control = self.control.on_click(f);
        self
    }

    /// Give the recommended path the accent surface.
    #[must_use]
    pub fn emphasized(mut self, value: bool) -> Self {
        self.emphasized = value;
        self
    }

    /// The height a card takes in `theme`, for hosts that reserve room for
    /// a pinned stack of cards before laying it out.
    pub fn height_for(theme: &Theme) -> f32 {
        px(card_height(theme))
    }

    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.control = self.control.with_enabled(enabled);
        self
    }

    fn face(&self) -> ChoiceCardFace<'_> {
        ChoiceCardFace::new(
            self.rect,
            self.icon,
            self.title.as_str(),
            self.description.as_str(),
        )
        .emphasized(self.emphasized)
        .interact(self.control.interact())
    }
}

impl Widget for ChoiceCard {
    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        self.control.set_interaction_environment(environment);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            flex_shrink: 0.0,
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(px(card_height(theme))),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::from_origin_size(
            Point::new(
                origin.x + f64::from(layout.location.x),
                origin.y + f64::from(layout.location.y),
            ),
            (f64::from(layout.size.width), f64::from(layout.size.height)),
        );
        self.control.apply_layout_external(self.rect);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &crate::presence::PresenceCtx,
    ) {
        self.face().paint(scene, transform, text_renderer, theme);
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut super::EventCtx<'_>) {
        self.control.handle_event(event, ctx);
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        root: &ModelPath,
    ) -> bool {
        self.control.perform_action(action, state, root)
    }

    fn name(&self) -> &'static str {
        "ChoiceCard"
    }

    fn test_id(&self) -> Option<&str> {
        self.control.test_id()
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.control.cursor_shape(point)
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.control.set_focus(focused);
    }

    fn is_focused(&self) -> bool {
        self.control.is_focused()
    }

    fn is_enabled(&self) -> bool {
        self.control.is_enabled()
    }

    fn accessibility(&self) -> Option<AccessNode> {
        self.control
            .accessibility()
            .map(|node| node.with_value(self.description.as_str()))
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
            let rect = Rect::new(0.0, 0.0, 264.0, 72.0);
            let face = ChoiceCardFace::new(
                rect,
                Icon::Image,
                "Scan a QR image with an unusually long localized title",
                "Read the code from a screenshot or photo with a long localized description",
            );
            let mut renderer = TextRenderer::new();
            let mut scene = Scene::new();
            let theme = Theme::dark();
            face.paint(&mut scene, Affine::IDENTITY, &mut renderer, &theme);
            assert!(!scene.encoding().draw_tags.is_empty());

            let available = px(Geometry::of(&theme).text_span(rect).1);
            let title = renderer.elide_to_width_style(
                face.title.as_ref(),
                available,
                &theme.text(TextRole::Subheading),
            );
            let description = renderer.elide_to_width_style(
                face.description.as_ref(),
                available,
                &theme.text(TextRole::Body),
            );
            assert_ne!(title, face.title.as_ref());
            assert_ne!(description, face.description.as_ref());
            assert!(title.ends_with('…'));
            assert!(description.ends_with('…'));
        }

        semantics_and_hit_testing_share_the_card_bounds();
        narrow_card_elides_copy_instead_of_wrapping_beyond_fixed_height();
    }

    #[test]
    fn choice_card_is_one_focusable_target_that_reports_its_choice() {
        use automerge::{AutoCommit, ROOT};

        let mut card = ChoiceCard::new(
            "setup.restore",
            Icon::Archive,
            "Restore from a backup",
            "Use an encrypted backup file",
        )
        .on_click(crate::testing::record_click("setup.restore"));
        assert!(card.is_focusable());
        assert_eq!(card.test_id(), Some("setup.restore"));
        let node = card.accessibility().expect("the card is announced");
        assert_eq!(node.role, Role::Button);
        assert_eq!(node.name.as_deref(), Some("Restore from a backup"));
        assert_eq!(node.value.as_deref(), Some("Use an encrypted backup file"));

        let mut doc = AutoCommit::new();
        assert!(card.perform_action(
            &crate::a11y::Action::Activate,
            &mut crate::widget::Edits::lend(&mut doc),
            &ROOT
        ));
        assert_eq!(crate::testing::clicks("setup.restore"), 1);
        assert!(doc.get_heads().is_empty(), "the choice writes nothing");

        let mut disabled = ChoiceCard::new(
            "setup.disabled",
            Icon::Archive,
            "Unavailable",
            "Not offered here",
        )
        .with_enabled(false);
        assert!(!disabled.is_enabled());
        let mut doc = AutoCommit::new();
        assert!(!disabled.perform_action(
            &crate::a11y::Action::Activate,
            &mut crate::widget::Edits::lend(&mut doc),
            &ROOT
        ));
    }

    #[test]
    fn choice_card_grows_with_larger_type() {
        let theme = Theme::dark();
        let minimum = Geometry::of(&theme).min_height();
        assert!((card_height(&theme) - minimum).abs() < f64::EPSILON);
        let mut large = Theme::dark();
        large.tokens.typography.scale = crate::tokens::TypeScale {
            lg: 32.0,
            base: 28.0,
            ..large.tokens.typography.scale
        };
        assert!(card_height(&large) > minimum);
    }
}
