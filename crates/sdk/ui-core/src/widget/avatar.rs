//! Avatar: a person's (or an account's) picture, or their initials when no
//! picture exists.
//!
//! An avatar is a person, so its fallback wears that person's presence
//! colour: the hue comes from their identity ([`PresenceHue::of_identity`])
//! or from the presence colour the caller already holds for them, never
//! from a position in a list. The initials take whichever ink of the theme
//! reads best on that colour. A picture is cover-fitted into the shape and
//! may load lazily from an [`ImageStore`]; the initials stay until it lands.
//!
//! Two forms with one geometry: the retained [`Avatar`] widget and the
//! immediate [`AvatarFace`] painter for rows, gutters and clusters that a
//! caller lays out itself.

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{self, Color, Fill};

use crate::ImageStore;
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::tokens::colors::PresenceHue;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

/// How large an avatar is. Each size reads from the theme's metrics, so it
/// follows the UI scale.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AvatarSize {
    /// Inline with a line of text: a mention, a chip, an attribution.
    Small,
    /// A list row or a roster entry.
    #[default]
    Regular,
    /// A card or a conversation header.
    Large,
    /// The subject of a detail view (a contact, an account).
    Hero,
}

impl AvatarSize {
    /// The side in logical pixels under `theme`.
    #[must_use]
    pub fn side(self, theme: &Theme) -> f64 {
        let metrics = &theme.tokens.metrics;
        let target = f64::from(metrics.min_pointer_target);
        match self {
            Self::Small => f64::from(metrics.icon_md),
            Self::Regular => target,
            Self::Large => target * 1.5,
            Self::Hero => target * 2.5,
        }
    }
}

/// Whose colour a fallback avatar wears.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Tint<'a> {
    Identity(&'a str),
    Presence(Color),
}

/// Immediate-mode avatar: the same picture, initials, colour and shape as
/// [`Avatar`], painted into a rect the caller laid out.
#[derive(Clone, Copy, Debug)]
pub struct AvatarFace<'a> {
    name: &'a str,
    tint: Tint<'a>,
    image: Option<&'a peniko::ImageData>,
    square: bool,
    ring_on: Option<Color>,
}

impl<'a> AvatarFace<'a> {
    /// `name` gives the initials; `identity` (a stable id: a peer id, an
    /// address, an account key) picks the presence hue.
    #[must_use]
    pub fn new(name: &'a str, identity: &'a str) -> Self {
        Self {
            name,
            tint: Tint::Identity(identity),
            image: None,
            square: false,
            ring_on: None,
        }
    }

    /// Use a collaborator's own presence colour (their chosen identity
    /// colour) instead of hashing an id. It is painted at the theme's
    /// presence lightness and chroma, like every other presence mark.
    #[must_use]
    pub fn presence(mut self, colour: Color) -> Self {
        self.tint = Tint::Presence(colour);
        self
    }

    /// The person's picture. Without one the initials show.
    #[must_use]
    pub fn image(mut self, image: Option<&'a peniko::ImageData>) -> Self {
        self.image = image;
        self
    }

    /// A rounded square instead of a circle: an account, a site or an
    /// organisation rather than a person.
    #[must_use]
    pub fn square(mut self) -> Self {
        self.square = true;
        self
    }

    /// Paint a separator ring of `ground` around the avatar, for avatars that
    /// overlap in a cluster. `ground` is the surface the cluster sits on.
    #[must_use]
    pub fn ringed_on(mut self, ground: Color) -> Self {
        self.ring_on = Some(ground);
        self
    }

    /// The initials this face shows: the first letter of the first two
    /// words, upper-cased.
    #[must_use]
    pub fn initials(&self) -> String {
        initials_for_name(self.name)
    }

    /// The fallback fill under `theme`.
    #[must_use]
    pub fn colour(&self, theme: &Theme) -> Color {
        let presence = &theme.tokens.colors.presence;
        match self.tint {
            Tint::Identity(id) => presence.hue(PresenceHue::of_identity(id)),
            Tint::Presence(colour) => presence.of(colour),
        }
    }

    fn shape(&self, rect: Rect, theme: &Theme) -> RoundedRect {
        let side = rect.width().min(rect.height());
        let rect = Rect::from_center_size(rect.center(), (side, side));
        let radius = if self.square {
            let radii = &theme.tokens.radii;
            if side <= f64::from(theme.tokens.metrics.icon_lg) {
                f64::from(radii.detail)
            } else if side <= f64::from(theme.tokens.metrics.min_pointer_target) {
                f64::from(radii.item)
            } else {
                f64::from(radii.control)
            }
        } else {
            side / 2.0
        };
        RoundedRect::from_rect(rect, radius)
    }

    /// Paint into `rect` (the shorter side is the avatar's side, centred).
    pub fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect) {
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let shape = self.shape(rect, ctx.theme);
        if let Some(ground) = self.ring_on {
            let ring = f64::from(ctx.theme.tokens.metrics.chrome_rule_px) * 2.0;
            let outer = shape.rect().inflate(ring, ring);
            let radius = if self.square {
                shape.radii().top_left + ring
            } else {
                outer.width() / 2.0
            };
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                ground,
                None,
                &RoundedRect::from_rect(outer, radius),
            );
        }
        if let Some(image) = self.image {
            crate::image_store::paint_image_cover(
                ctx.scene,
                ctx.transform,
                shape.rect(),
                image,
                shape.radii().top_left,
            );
            return;
        }
        let fill = self.colour(ctx.theme);
        ctx.scene
            .fill(Fill::NonZero, ctx.transform, fill, None, &shape);
        let initials = self.initials();
        if initials.is_empty() {
            return;
        }
        let side = shape.rect().width();
        let style = initials_style(ctx.theme, side);
        let (width, height) = ctx.text.measure_style(&initials, None, &style);
        let centre = shape.rect().center();
        ctx.text(
            &initials,
            &style,
            initial_ink(fill, ctx.theme),
            Point::new(
                centre.x - f64::from(width) / 2.0,
                centre.y - f64::from(height) / 2.0,
            ),
            None,
        );
    }

    /// The "+N more people" disc that ends an overlapping cluster: the
    /// raised surface with secondary ink. `count == 0` paints nothing.
    /// `ring_on` is the ground of an overlapping cluster, as in
    /// [`Self::ringed_on`].
    pub fn paint_overflow(
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        count: usize,
        ring_on: Option<Color>,
    ) {
        if count == 0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let side = rect.width().min(rect.height());
        let disc = Rect::from_center_size(rect.center(), (side, side));
        if let Some(ground) = ring_on {
            let ring = f64::from(ctx.theme.tokens.metrics.chrome_rule_px) * 2.0;
            let outer = disc.inflate(ring, ring);
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                ground,
                None,
                &RoundedRect::from_rect(outer, outer.width() / 2.0),
            );
        }
        let raised = ctx.theme.tokens.colors.surfaces.raised;
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            raised,
            None,
            &RoundedRect::from_rect(disc, side / 2.0),
        );
        let label = format!("+{count}");
        let style = overflow_style(ctx.theme, side);
        let (width, height) = ctx.text.measure_style(&label, None, &style);
        let centre = disc.center();
        let ink = ctx.theme.text_dim;
        ctx.text(
            &label,
            &style,
            ink,
            Point::new(
                centre.x - f64::from(width) / 2.0,
                centre.y - f64::from(height) / 2.0,
            ),
            None,
        );
    }
}

/// The first letters of the first and the last word, upper-cased:
/// `"Ada Lovelace"` gives `"AL"`, `"Grace Brewster Hopper"` gives `"GH"`,
/// `"ada"` gives `"A"`.
#[must_use]
pub fn initials_for_name(name: &str) -> String {
    let letters: Vec<char> = name
        .split_whitespace()
        .filter_map(|word| word.chars().find(|c| c.is_alphanumeric()))
        .collect();
    let picked: Vec<char> = match letters.as_slice() {
        [] => Vec::new(),
        [only] => vec![*only],
        [first, .., last] => vec![*first, *last],
    };
    picked.into_iter().flat_map(char::to_uppercase).collect()
}

/// The initials ink on `fill`: of the theme's own inks (its primary text and
/// its content ground), whichever contrasts more with the fill.
#[must_use]
pub fn initial_ink(fill: Color, theme: &Theme) -> Color {
    let candidates = [theme.text_primary, theme.tokens.colors.surfaces.content];
    candidates
        .into_iter()
        .max_by(|a, b| {
            crate::color::contrast_ratio(*a, fill)
                .total_cmp(&crate::color::contrast_ratio(*b, fill))
        })
        .unwrap_or(theme.text_primary)
}

/// Initials are sized by the disc they sit in, not by the reader's text
/// scale: the theme's emphasised label face at 40 % of the side.
fn initials_style(theme: &Theme, side: f64) -> TextStyle {
    theme
        .text(TextRole::Label)
        .strong()
        .with_content_size(crate::num::f32_of_f64(side * 0.4))
}

/// "+12" is up to three glyphs, so it sits a little smaller than initials.
fn overflow_style(theme: &Theme, side: f64) -> TextStyle {
    theme
        .text(TextRole::Label)
        .strong()
        .with_content_size(crate::num::f32_of_f64(side * 0.36))
}

/// Where a retained avatar's picture comes from.
enum Picture {
    None,
    /// Resolved from the store on `sync`, so a picture that finishes loading
    /// repaints the avatar without rebuilding the tree.
    Lazy {
        store: ImageStore,
        url: String,
    },
}

/// A person's avatar as a retained widget. See the module docs.
pub struct Avatar {
    name: String,
    identity: String,
    presence: Option<Color>,
    picture: Picture,
    image: Option<peniko::ImageData>,
    size: AvatarSize,
    square: bool,
    side: f64,
    rect: Rect,
    dirty: bool,
}

impl Avatar {
    /// An avatar for `name`, coloured by `identity`.
    pub fn person(name: impl Into<String>, identity: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            identity: identity.into(),
            presence: None,
            picture: Picture::None,
            image: None,
            size: AvatarSize::Regular,
            square: false,
            side: 0.0,
            rect: Rect::ZERO,
            dirty: true,
        }
    }

    /// See [`AvatarFace::presence`].
    #[must_use]
    pub fn with_presence(mut self, colour: Color) -> Self {
        self.presence = Some(colour);
        self
    }

    /// The person's picture, already decoded.
    #[must_use]
    pub fn with_image(mut self, image: peniko::ImageData) -> Self {
        self.picture = Picture::None;
        self.image = Some(image);
        self
    }

    /// The person's picture, loaded from `store` in the background. The
    /// initials show until it arrives; an empty `url` keeps them.
    #[must_use]
    pub fn with_image_url(mut self, store: ImageStore, url: impl Into<String>) -> Self {
        self.picture = Picture::Lazy {
            store,
            url: url.into(),
        };
        self
    }

    #[must_use]
    pub fn size(mut self, size: AvatarSize) -> Self {
        self.size = size;
        self
    }

    /// See [`AvatarFace::square`].
    #[must_use]
    pub fn square(mut self) -> Self {
        self.square = true;
        self
    }

    /// The initials the fallback shows.
    #[must_use]
    pub fn initials(&self) -> String {
        initials_for_name(&self.name)
    }

    fn face(&self) -> AvatarFace<'_> {
        let mut face = AvatarFace::new(&self.name, &self.identity).image(self.image.as_ref());
        if let Some(colour) = self.presence {
            face = face.presence(colour);
        }
        if self.square {
            face = face.square();
        }
        face
    }
}

impl Widget for Avatar {
    fn sync(&mut self, _state: &mut crate::widget::AppModel, _path: &crate::widget::ModelPath) {
        if self.image.is_none()
            && let Picture::Lazy { store, url } = &self.picture
            && let Some(image) = store.get(url)
        {
            self.image = Some(image);
            self.dirty = true;
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.side = self.size.side(theme);
        let side = crate::num::f32_of_f64(self.side);
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::length(side),
                height: Dimension::length(side),
            },
            flex_shrink: 0.0,
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        let rect = Rect::from_origin_size(
            (
                origin.x + f64::from(layout.location.x),
                origin.y + f64::from(layout.location.y),
            ),
            (f64::from(layout.size.width), f64::from(layout.size.height)),
        );
        if rect != self.rect {
            self.dirty = true;
        }
        self.rect = rect;
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if presence.should_cull(self.rect) {
            return;
        }
        let face = self.face();
        face.paint(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            self.rect,
        );
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    /// Keyed off the laid-out rect, never a draw-time cache: an avatar
    /// culled off-screen is never drawn, and must not report a pending
    /// measure on every scroll frame because of it.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn name(&self) -> &'static str {
        "Avatar"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        let name = (!self.name.trim().is_empty()).then(|| self.name.clone());
        Some(AccessNode::new(Role::Image).with_optional_name(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use vello::peniko::{Blob, ImageAlphaType, ImageData as PenikoImage, ImageFormat as Format};
    use vello_encoding::DrawTag;

    fn image(width: u32, height: u32) -> PenikoImage {
        let pixels = usize::try_from(width * height * 4).unwrap();
        PenikoImage {
            data: Blob::new(Arc::new(vec![200; pixels])),
            format: Format::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width,
            height,
        }
    }

    fn paint_face(face: AvatarFace<'_>, theme: &Theme) -> Scene {
        let mut text = TextRenderer::new();
        let mut scene = Scene::new();
        face.paint(
            &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, theme),
            Rect::new(0.0, 0.0, 40.0, 40.0),
        );
        scene
    }

    fn tags(scene: &Scene, tag: DrawTag) -> usize {
        scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == tag)
            .count()
    }

    #[test]
    fn initials_come_from_the_name_and_colour_from_the_identity() {
        assert_eq!(initials_for_name("Ada Lovelace"), "AL");
        assert_eq!(initials_for_name("  grace   brewster hopper "), "GH");
        assert_eq!(initials_for_name("(Ops) team"), "OT");
        assert_eq!(initials_for_name(""), "");

        let theme = Theme::dark();
        // The same person is the same colour wherever they appear, and two
        // people with the same initials are not forced to share one.
        let a = AvatarFace::new("Ada Lovelace", "ada@example.org").colour(&theme);
        let again = AvatarFace::new("A. Lovelace", "ada@example.org").colour(&theme);
        assert_eq!(a, again);
        let expected = theme
            .tokens
            .colors
            .presence
            .hue(PresenceHue::of_identity("ada@example.org"));
        assert_eq!(a, expected);
        // A collaborator's own colour lands on the theme's presence L/C.
        let own = AvatarFace::new("Bob", "bob")
            .presence(Color::from_rgb8(0xe0, 0x60, 0x20))
            .colour(&theme);
        assert_eq!(
            own,
            theme
                .tokens
                .colors
                .presence
                .of(Color::from_rgb8(0xe0, 0x60, 0x20))
        );
    }

    #[test]
    fn initials_ink_reads_on_every_presence_hue_in_every_preset() {
        for theme in [Theme::dark(), Theme::light()] {
            for hue in PresenceHue::ALL {
                let fill = theme.tokens.colors.presence.hue(hue);
                let ink = initial_ink(fill, &theme);
                let ratio = crate::color::contrast_ratio(ink, fill);
                assert!(
                    ratio >= 3.0,
                    "{}: initials on {hue:?} reach only {ratio:.2}:1",
                    theme.name
                );
            }
        }
    }

    #[test]
    fn a_picture_replaces_the_initials_and_is_cover_fitted() {
        let theme = Theme::dark();
        let picture = image(64, 32);
        let with_image = paint_face(
            AvatarFace::new("Ada Lovelace", "ada").image(Some(&picture)),
            &theme,
        );
        assert_eq!(tags(&with_image, DrawTag::IMAGE), 1);
        assert_eq!(tags(&with_image, DrawTag::COLOR), 0, "no initials disc");
        let fallback = paint_face(AvatarFace::new("Ada Lovelace", "ada"), &theme);
        assert_eq!(tags(&fallback, DrawTag::IMAGE), 0);
        assert!(tags(&fallback, DrawTag::COLOR) >= 2, "disc and glyphs");
    }

    #[test]
    fn sizes_follow_the_metrics_and_square_avatars_use_concentric_radii() {
        let theme = Theme::dark();
        let small = AvatarSize::Small.side(&theme);
        let regular = AvatarSize::Regular.side(&theme);
        let large = AvatarSize::Large.side(&theme);
        let hero = AvatarSize::Hero.side(&theme);
        assert!(small < regular && regular < large && large < hero);
        let scaled = Theme::dark().with_ui_scale(2.0);
        assert!((AvatarSize::Regular.side(&scaled) - regular * 2.0).abs() < 0.5);

        let face = AvatarFace::new("Site", "site").square();
        let shape = face.shape(Rect::new(0.0, 0.0, 48.0, 48.0), &theme);
        assert!((shape.radii().top_left - f64::from(theme.tokens.radii.control)).abs() < 0.01);
        let circle = AvatarFace::new("Ada", "ada").shape(Rect::new(0.0, 0.0, 48.0, 30.0), &theme);
        assert!((circle.radii().top_left - 15.0).abs() < 0.01);
        assert!(
            (circle.rect().width() - 30.0).abs() < 0.01,
            "centred square"
        );
    }

    #[test]
    fn a_culled_avatar_does_not_stay_layout_dirty() {
        let mut avatar = Avatar::person("Ada Lovelace", "ada");
        let mut taffy = TaffyTree::new();
        let mut map = std::collections::HashMap::new();
        let node = avatar.build_layout(&mut taffy, &Theme::dark(), &mut map);
        taffy
            .compute_layout(node, <taffy::Size<taffy::AvailableSpace> as taffy::prelude::TaffyMaxContent>::MAX_CONTENT)
            .unwrap();
        avatar.apply_layout(&taffy, node, Point::new(0.0, 4000.0));
        // Never drawn (off-screen), yet the layout is settled.
        assert!(!avatar.is_layout_dirty());
        let node = Widget::accessibility(&avatar).unwrap();
        assert_eq!(node.name.as_deref(), Some("Ada Lovelace"));
    }

    #[test]
    fn overflow_disc_paints_on_its_ground() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let mut scene = Scene::new();
        AvatarFace::paint_overflow(
            &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
            Rect::new(0.0, 0.0, 32.0, 32.0),
            4,
            Some(theme.tokens.colors.surfaces.content),
        );
        assert!(tags(&scene, DrawTag::COLOR) >= 3, "ring, disc, label");
        let mut empty = Scene::new();
        AvatarFace::paint_overflow(
            &mut PaintCtx::new(&mut empty, Affine::IDENTITY, &mut text, &theme),
            Rect::new(0.0, 0.0, 32.0, 32.0),
            0,
            Some(theme.tokens.colors.surfaces.content),
        );
        assert_eq!(tags(&empty, DrawTag::COLOR), 0);
    }
}
