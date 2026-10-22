//! A playing card, drawn on the theme: the face on a floating surface with the
//! rank and suit in the theme's ink, or the back in its owner's presence
//! colour. It is a picture with a name, so a screen reader, an agent and a
//! test read what it shows.

use std::collections::HashMap;

use ui::advanced::{Affine, NodeId, Scene, TaffyTree};
use ui::i18n::I18nManager;
use ui::prelude::*;
use ui::vello::kurbo::{BezPath, Circle, RoundedRect, Shape};
use ui::vello::peniko::{Color, Fill};
use ui::widget::{SurfaceLevel, TaffyTreeExt as _, paint_surface};

use crate::cards;

/// How big a card is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardSize {
    /// In front of a player.
    Small,
    /// On the board.
    Large,
}

impl CardSize {
    /// The card's width and height: the picture of a playing card, in its
    /// proportions.
    const fn extent(self) -> (f32, f32) {
        match self {
            Self::Small => (44.0, 62.0),
            Self::Large => (60.0, 84.0),
        }
    }

    /// How wide the card is drawn.
    #[must_use]
    pub const fn width(self) -> f32 {
        self.extent().0
    }
}

/// What a card shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Showing {
    Face(u8),
    /// Its back, in the presence colour of whoever holds it.
    Back((u8, u8, u8)),
    /// A place on the board no card has reached yet.
    Slot,
}

pub struct CardView {
    rect: Rect,
    showing: Showing,
    size: CardSize,
    id: String,
    name: String,
}

/// What a screen reader says for a card's face ("Queen of hearts").
#[must_use]
pub fn card_name(card: u8) -> String {
    I18nManager::format(
        "poker-card-name",
        &[
            (
                "rank",
                I18nManager::lookup_plain(cards::rank_message(card)).into(),
            ),
            (
                "suit",
                I18nManager::lookup_plain(cards::suit_message(card)).into(),
            ),
        ],
    )
}

impl CardView {
    #[must_use]
    pub fn new(id: impl Into<String>, showing: Showing, size: CardSize) -> Self {
        let name = match showing {
            Showing::Face(card) => card_name(card),
            Showing::Back(_) => I18nManager::lookup_plain("poker-card-hidden"),
            Showing::Slot => I18nManager::lookup_plain("poker-card-slot"),
        };
        Self {
            rect: Rect::ZERO,
            showing,
            size,
            id: id.into(),
            name,
        }
    }

    fn paint_face(
        &self,
        scene: &mut Scene,
        transform: Affine,
        renderer: &mut TextRenderer,
        theme: &Theme,
        card: u8,
    ) {
        let on = paint_surface(scene, transform, self.rect, SurfaceLevel::Floating, theme);
        let colors = &on.tokens.colors;
        let ink = if cards::is_red(card) {
            colors.semantic.danger
        } else {
            colors.text_primary
        };
        let (role, corner_suit) = match self.size {
            CardSize::Small => (TextRole::Label, 6.0),
            CardSize::Large => (TextRole::Heading, 8.0),
        };
        let pad = f64::from(on.tokens.spacing.s1);
        let style = on.text(role);
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "Affine composition is float matrix math and cannot panic"
        )]
        let corner = transform * Affine::translate((self.rect.x0 + pad, self.rect.y0 + pad * 0.5));
        renderer.draw_style(scene, cards::rank_label(card), ink, corner, None, &style);
        let (_, line) = renderer.measure_style(cards::rank_label(card), None, &style);
        let small = Point::new(
            self.rect.x0 + pad + corner_suit,
            self.rect.y0 + pad * 0.5 + f64::from(line) + corner_suit,
        );
        fill_suit(scene, transform, card, small, corner_suit, ink);
        let big = self.rect.width() * 0.26;
        let centre = Point::new(
            self.rect.center().x + self.rect.width() * 0.08,
            self.rect.center().y + self.rect.height() * 0.12,
        );
        fill_suit(scene, transform, card, centre, big, ink);
    }

    fn paint_back(&self, scene: &mut Scene, transform: Affine, theme: &Theme, rgb: (u8, u8, u8)) {
        let on = paint_surface(scene, transform, self.rect, SurfaceLevel::Floating, theme);
        let colors = &on.tokens.colors;
        let tint = colors.presence.of_rgb(rgb);
        let inset = f64::from(on.tokens.spacing.s1);
        let field = self.rect.inset(-inset);
        let radius = SurfaceLevel::Floating.inner_radius(&on, inset);
        let well = RoundedRect::from_rect(field, radius);
        // ratchet-allow(raw_paint): the back of a playing card is the picture of a card, content
        scene.fill(
            Fill::NonZero,
            transform,
            // ratchet-allow(literal_alpha): the back's field is its owner's presence colour, washed out under its lattice
            tint.multiply_alpha(0.35),
            None,
            &well,
        );
        // A lattice of diamonds across the back, four across; the clip
        // trims the last row.
        let step = field.width() / 4.0;
        let r = step * 0.32;
        let mut lattice = BezPath::new();
        for row in 0..6_u8 {
            for column in 0..4_u8 {
                let x = field.x0 + step * (f64::from(column) + 0.5);
                let y = field.y0 + step * (f64::from(row) + 0.5);
                lattice.move_to((x, y - r));
                lattice.line_to((x + r, y));
                lattice.line_to((x, y + r));
                lattice.line_to((x - r, y));
                lattice.close_path();
            }
        }
        scene.push_clip_layer(Fill::NonZero, transform, &well);
        // ratchet-allow(raw_paint): the lattice printed on the card's back, content
        scene.fill(Fill::NonZero, transform, tint, None, &lattice);
        scene.pop_layer();
    }

    fn paint_slot(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let colors = &theme.tokens.colors;
        let slot = RoundedRect::from_rect(self.rect, SurfaceLevel::Raised.radius(theme));
        // ratchet-allow(raw_paint): the outline of a card not dealt yet, drawn on the board as content
        scene.fill(Fill::NonZero, transform, colors.overlay_subtle, None, &slot);
        // ratchet-allow(raw_paint): the outline of a card not dealt yet, drawn on the board as content
        scene.stroke(
            &ui::vello::kurbo::Stroke::new(1.0),
            transform,
            colors.overlay_border_subtle,
            None,
            &RoundedRect::from_rect(self.rect.inset(-0.5), SurfaceLevel::Raised.radius(theme)),
        );
    }
}

/// Fill the suit of `card` at `centre`, `size` from its middle to its edge.
fn fill_suit(scene: &mut Scene, transform: Affine, card: u8, centre: Point, size: f64, ink: Color) {
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "Affine composition is float matrix math and cannot panic"
    )]
    let place = transform * Affine::translate(centre.to_vec2()) * Affine::scale(size);
    let mut path = BezPath::new();
    match cards::suit(card) {
        // Spades: a heart upside down on a stem.
        0 => {
            path.move_to((0.0, -0.95));
            path.curve_to((-0.9, -0.3), (-1.0, 0.25), (-0.55, 0.55));
            path.curve_to((-0.25, 0.75), (0.0, 0.55), (0.0, 0.35));
            path.curve_to((0.0, 0.55), (0.25, 0.75), (0.55, 0.55));
            path.curve_to((1.0, 0.25), (0.9, -0.3), (0.0, -0.95));
            path.close_path();
            stem(&mut path, 0.3);
        }
        1 => {
            path.move_to((0.0, 0.9));
            path.curve_to((-0.9, 0.25), (-1.0, -0.35), (-0.55, -0.75));
            path.curve_to((-0.25, -1.0), (0.0, -0.75), (0.0, -0.5));
            path.curve_to((0.0, -0.75), (0.25, -1.0), (0.55, -0.75));
            path.curve_to((1.0, -0.35), (0.9, 0.25), (0.0, 0.9));
            path.close_path();
        }
        2 => {
            path.move_to((0.0, -1.0));
            path.line_to((0.72, 0.0));
            path.line_to((0.0, 1.0));
            path.line_to((-0.72, 0.0));
            path.close_path();
        }
        _ => {
            for (x, y) in [(0.0, -0.45), (-0.47, 0.12), (0.47, 0.12)] {
                path.extend(Circle::new((x, y), 0.4).path_elements(0.01));
            }
            stem(&mut path, 0.1);
        }
    }
    // ratchet-allow(raw_paint): a suit printed on a card's face, content
    scene.fill(Fill::NonZero, place, ink, None, &path);
}

fn stem(path: &mut BezPath, top: f64) {
    path.move_to((0.0, top));
    path.line_to((0.32, 1.0));
    path.line_to((-0.32, 1.0));
    path.close_path();
}

impl Widget for CardView {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        use ui::taffy::{Dimension, Size as Extent, Style};
        let (width, height) = self.size.extent();
        let style = Style {
            size: Extent {
                width: Dimension::length(width),
                height: Dimension::length(height),
            },
            flex_shrink: 0.0,
            ..Style::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    ui::rect_geometry!();

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        match self.showing {
            Showing::Face(card) => self.paint_face(scene, transform, text_renderer, theme, card),
            Showing::Back(rgb) => self.paint_back(scene, transform, theme, rgb),
            Showing::Slot => self.paint_slot(scene, transform, theme),
        }
    }

    fn name(&self) -> &'static str {
        "PokerCard"
    }

    fn test_id(&self) -> Option<&str> {
        Some(&self.id)
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, Role};
        Some(AccessNode::new(Role::Image).with_name(self.name.clone()))
    }
}
