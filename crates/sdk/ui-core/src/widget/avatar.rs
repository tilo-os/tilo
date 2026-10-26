//! Avatar: a person's (or an account's) picture, or their initials when no
//! picture exists.
//!
//! An avatar is a person, so its fallback wears that person's presence
//! colour: the hue comes from their identity ([`PresenceHue::of_identity`])
//! or from the presence colour the caller already holds for them, never
//! from a position in a list. The initials take one ink per palette, derived
//! from the fill's own hue to at least 4.5:1, so every person's initials
//! read alike (no black on one hue and white on the next). A person's
//! presence ([`PresenceStatus`]) shows as a ringed badge on the corner, in
//! their colour. An app or a thing without a picture is a neutral square
//! ([`AvatarFace::neutral`]). A picture is cover-fitted into the shape and
//! may load lazily from an [`ImageStore`]; the initials stay until it lands.
//!
//! Two forms with one geometry: the retained [`Avatar`] widget and the
//! immediate [`AvatarFace`] painter for rows, gutters and clusters that a
//! caller lays out itself.

use taffy::{NodeId, Size, Style, TaffyTree};
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
    /// A person’s picture while setting up or editing their profile.
    Portrait,
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
            Self::Portrait => target * 4.0,
        }
    }
}

/// Whose colour a fallback avatar wears.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Tint<'a> {
    Identity(&'a str),
    Presence(Color),
    /// No one's: an app, a device, a thing.
    Neutral,
}

/// Whether a person is there: the badge on their avatar's corner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PresenceStatus {
    /// Here now: a filled badge in their colour.
    Online,
    /// Signed in but away: a ring in their colour.
    Away,
    /// Here, but not to be disturbed: a filled badge with a bar.
    Busy,
    /// Not here: no badge.
    Offline,
}

impl PresenceStatus {
    /// What a screen reader says.
    #[must_use]
    pub fn spoken(self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::Away => "away",
            Self::Busy => "busy",
            Self::Offline => "offline",
        }
    }
}

/// Immediate-mode avatar: the same picture, initials, colour and shape as
/// [`Avatar`], painted into a rect the caller laid out.
#[derive(Clone, Copy, Debug)]
pub struct AvatarFace<'a> {
    name: &'a str,
    identity: &'a str,
    tint: Tint<'a>,
    image: Option<&'a peniko::ImageData>,
    square: bool,
    ring_on: Option<Color>,
    status: Option<PresenceStatus>,
    glyph: Option<crate::icons::Icon>,
    presence_ring: bool,
    speaking: f32,
    idle_on: Option<Color>,
}

impl<'a> AvatarFace<'a> {
    /// `name` gives the initials; `identity` (a stable id: a peer id, an
    /// address, an account key) picks the presence hue.
    #[must_use]
    pub fn new(name: &'a str, identity: &'a str) -> Self {
        Self {
            name,
            identity,
            tint: Tint::Identity(identity),
            image: None,
            square: false,
            ring_on: None,
            status: None,
            glyph: None,
            presence_ring: false,
            speaking: 0.0,
            idle_on: None,
        }
    }

    /// An app without artwork of its own: its glyph from the icon set
    /// ([`crate::icons::app_glyph`]) instead of initials, on the neutral
    /// square, the same mark the shell shows for it.
    #[must_use]
    pub fn glyph(mut self, glyph: crate::icons::Icon) -> Self {
        self.glyph = Some(glyph);
        self.tint = Tint::Neutral;
        self.square = true;
        self
    }

    /// A thing rather than a person (an app without an icon, a device): a
    /// neutral square, primary ink on the raised surface.
    #[must_use]
    pub fn neutral(mut self) -> Self {
        self.tint = Tint::Neutral;
        self.square = true;
        self
    }

    /// The person's presence, as a badge on the avatar's corner in their
    /// colour, ringed in the ground ([`Self::ringed_on`], else the content
    /// surface).
    #[must_use]
    pub fn status(mut self, status: PresenceStatus) -> Self {
        self.status = Some(status);
        self
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

    /// Whether it is the rounded square rather than the circle.
    #[must_use]
    pub fn is_square(&self) -> bool {
        self.square
    }

    /// Paint a separator ring of `ground` around the avatar, for avatars that
    /// overlap in a cluster. `ground` is the surface the cluster sits on.
    #[must_use]
    pub fn ringed_on(mut self, ground: Color) -> Self {
        self.ring_on = Some(ground);
        self
    }

    /// A person's face as presence shows it: their colour as a ring around
    /// the disc, inside `rect`, with the ground showing between the two.
    #[must_use]
    pub fn presence_ring(mut self) -> Self {
        self.presence_ring = true;
        self
    }

    /// The person is idle or away: their colour quietened for a face on
    /// `ground` ([`crate::tokens::colors::PresencePalette::idle_on`]), so
    /// it reads as dimmed and still stands out from the ground.
    #[must_use]
    pub fn idle_on(mut self, ground: Color) -> Self {
        self.idle_on = Some(ground);
        self
    }

    /// Their voice while they talk in a call: a soft ring in their colour
    /// just outside `rect`, as strong as `level` (`ui::motion::Speaking`).
    #[must_use]
    pub fn speaking(mut self, level: f32) -> Self {
        self.speaking = level.clamp(0.0, 1.0);
        self
    }

    /// The width of the presence ring and of the gap beside it for a face
    /// `side` across: 2 px apart on a face of 32 px or more, finer on the
    /// small faces of rows and chips so their initials stay legible.
    fn presence_ring_px(theme: &Theme, side: f64) -> (f64, f64) {
        let rule = f64::from(theme.tokens.metrics.chrome_rule_px);
        if side >= 32.0 {
            (2.0 * rule, 2.0 * rule)
        } else {
            (1.5 * rule, rule)
        }
    }

    /// The presence ring and the speaking ring around `shape`; returns the
    /// shape the disc is painted in, inside the ring.
    fn paint_presence_rings(&self, ctx: &mut PaintCtx<'_>, shape: RoundedRect) -> RoundedRect {
        let theme = ctx.theme;
        let colour = self.presence_colour(theme);
        let (ring, gap) = Self::presence_ring_px(theme, shape.rect().width());
        let rounded = |rect: Rect, inset: f64| {
            let radius = if self.square {
                (shape.radii().top_left - inset).max(0.0)
            } else {
                rect.width().min(rect.height()) / 2.0
            };
            RoundedRect::from_rect(rect, radius)
        };
        if self.speaking > 0.0 {
            // Just outside the face, the ground between.
            let reach = gap + ring / 2.0;
            ctx.scene.stroke(
                &vello::kurbo::Stroke::new(ring),
                ctx.transform,
                colour.multiply_alpha(self.speaking),
                None,
                &rounded(shape.rect().inflate(reach, reach), -reach),
            );
        }
        if !self.presence_ring {
            return shape;
        }
        let band = shape.rect().inset(-ring / 2.0);
        ctx.scene.stroke(
            &vello::kurbo::Stroke::new(ring),
            ctx.transform,
            colour,
            None,
            &rounded(band, ring / 2.0),
        );
        let inset = ring + gap;
        rounded(shape.rect().inset(-inset), inset)
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
        let person = match self.tint {
            Tint::Identity(id) => presence.hue(PresenceHue::of_identity(id)),
            Tint::Presence(colour) => presence.of(colour),
            Tint::Neutral => return theme.tokens.colors.surfaces.raised,
        };
        let person = match self.idle_on {
            Some(ground) => presence.idle_on(person, ground),
            None => person,
        };
        initials_fill(person, theme)
    }

    /// The person's presence colour (the badge's), whatever the fallback.
    fn presence_colour(&self, theme: &Theme) -> Color {
        match self.tint {
            Tint::Neutral => theme.text_dim,
            _ => self.colour(theme),
        }
    }

    /// Where the presence badge sits for an avatar in `rect`: on the
    /// bottom-trailing corner, a third of the side.
    #[must_use]
    pub fn status_rect(&self, rect: Rect, theme: &Theme) -> Rect {
        let shape = self.shape(rect, theme).rect();
        let side = (shape.width() / 3.0).max(f64::from(theme.tokens.spacing.s2));
        // On a disc the badge sits on the rim at the bottom-trailing 45°; on
        // a square in its corner. Either way it stays inside the avatar.
        let reach = if self.square {
            shape.width() / 2.0
        } else {
            shape.width() / 2.0 * std::f64::consts::FRAC_1_SQRT_2
        };
        let limit = shape.width() / 2.0 - side / 2.0;
        let offset = reach.min(limit);
        let centre = shape.center() + vello::kurbo::Vec2::new(offset, offset);
        Rect::from_center_size(centre, (side, side))
    }

    fn paint_status(&self, ctx: &mut PaintCtx<'_>, rect: Rect) {
        let Some(status) = self.status else {
            return;
        };
        if status == PresenceStatus::Offline {
            return;
        }
        let theme = ctx.theme;
        let badge = self.status_rect(rect, theme);
        let ring = f64::from(theme.tokens.metrics.chrome_rule_px) * 2.0;
        let ground = self.ring_on.unwrap_or(theme.tokens.colors.surfaces.content);
        let colour = self.presence_colour(theme);
        let outer = badge.inflate(ring, ring);
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            ground,
            None,
            &RoundedRect::from_rect(outer, outer.width() / 2.0),
        );
        let disc = RoundedRect::from_rect(badge, badge.width() / 2.0);
        match status {
            PresenceStatus::Online => {
                ctx.scene
                    .fill(Fill::NonZero, ctx.transform, colour, None, &disc);
            }
            PresenceStatus::Away => {
                let stroke = (badge.width() / 4.0).max(ring);
                let inner = badge.inset(-stroke / 2.0);
                ctx.scene.stroke(
                    &vello::kurbo::Stroke::new(stroke),
                    ctx.transform,
                    colour,
                    None,
                    &RoundedRect::from_rect(inner, inner.width() / 2.0),
                );
            }
            PresenceStatus::Busy => {
                ctx.scene
                    .fill(Fill::NonZero, ctx.transform, colour, None, &disc);
                let bar_h = (badge.height() / 5.0).max(ring / 2.0);
                let bar = Rect::from_center_size(badge.center(), (badge.width() * 0.55, bar_h));
                ctx.scene.fill(
                    Fill::NonZero,
                    ctx.transform,
                    ground,
                    None,
                    &RoundedRect::from_rect(bar, bar_h / 2.0),
                );
            }
            PresenceStatus::Offline => {}
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
        // A preceding nested text scene can leave a resolved glyph transform
        // active even when its last encoded transform equals ours. Establish
        // the avatar's geometry state explicitly before its first path.
        ctx.scene.encoding_mut().force_next_transform_and_style();
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
        let shape = self.paint_presence_rings(ctx, shape);
        #[cfg(feature = "local-daemons")]
        let public_image = if self.image.is_none() {
            if self.tint == Tint::Neutral {
                None
            } else {
                crate::local_identity::photo_for(self.identity).map(|photo| profile_image(&photo))
            }
        } else {
            None
        };
        #[cfg(feature = "local-daemons")]
        let image = self.image.or(public_image.as_ref());
        #[cfg(not(feature = "local-daemons"))]
        let image = self.image;
        // The picture the person chose for their profile, wherever they
        // show up, unless the caller drew them neutral.
        let chosen = if image.is_none() && self.tint != Tint::Neutral {
            crate::people_photos::photo(self.identity)
        } else {
            None
        };
        let image = image.or(chosen.as_ref());
        if let Some(image) = image {
            crate::image_store::paint_image_cover(
                ctx.scene,
                ctx.transform,
                shape.rect(),
                image,
                shape.radii().top_left,
            );
            self.paint_status(ctx, rect);
            return;
        }
        let fill = self.colour(ctx.theme);
        ctx.scene
            .fill(Fill::NonZero, ctx.transform, fill, None, &shape);
        if self.tint == Tint::Neutral {
            ctx.scene.stroke(
                &vello::kurbo::Stroke::new(f64::from(ctx.theme.tokens.metrics.chrome_rule_px)),
                ctx.transform,
                crate::surface::edge_on(fill),
                None,
                &shape,
            );
        }
        if let Some(glyph) = self.glyph {
            // The glyph takes the share of the tile an icon takes in the
            // icon set's own 24 px grid, centred.
            let side = shape.rect().width() * 0.55;
            glyph.draw(
                ctx.scene,
                ctx.transform,
                Rect::from_center_size(shape.rect().center(), (side, side)),
                ctx.theme.text_primary,
            );
            self.paint_status(ctx, rect);
            return;
        }
        let initials = self.initials();
        if initials.is_empty() {
            self.paint_status(ctx, rect);
            return;
        }
        let side = shape.rect().width();
        let style = initials_style(ctx.theme, side);
        let (width, height) = ctx.text.measure_style(&initials, None, &style);
        let centre = shape.rect().center();
        let ink = if self.tint == Tint::Neutral {
            ctx.theme.text_primary
        } else {
            initial_ink(fill, ctx.theme)
        };
        ctx.text(
            &initials,
            &style,
            ink,
            Point::new(
                centre.x - f64::from(width) / 2.0,
                centre.y - f64::from(height) / 2.0,
            ),
            None,
        );
        self.paint_status(ctx, rect);
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
        ctx.scene.encoding_mut().force_next_transform_and_style();
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
        // As readable as the initials beside it.
        let ink = ctx.theme.text_primary;
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
/// `"ada"` gives `"A"`. Words without a letter are skipped, so `"Tilo 4"`
/// gives `"T"`, never `"T4"`.
#[must_use]
pub fn initials_for_name(name: &str) -> String {
    let letters: Vec<char> = name
        .split_whitespace()
        .filter_map(|word| word.chars().find(|c| c.is_alphabetic()))
        .collect();
    let picked: Vec<char> = match letters.as_slice() {
        [] => Vec::new(),
        [only] => vec![*only],
        [first, .., last] => vec![*first, *last],
    };
    picked.into_iter().flat_map(char::to_uppercase).collect()
}

/// Whether initials are dark in this theme: one direction per presence
/// palette, whichever reads better on the palette's weakest hue, so initials
/// never flip between black and white from one person to the next.
fn dark_initials(theme: &Theme) -> bool {
    use crate::color::{contrast_ratio, oklch};
    let palette = &theme.tokens.colors.presence;
    let weakest = |ink: Color| {
        palette
            .colors
            .iter()
            .map(|&c| contrast_ratio(ink, c))
            .fold(f64::INFINITY, f64::min)
    };
    weakest(oklch(0.0, 0.0, 0.0)) >= weakest(oklch(1.0, 0.0, 0.0))
}

/// The fill a tinted avatar wears: the person's presence colour, moved in
/// lightness (never in hue) away from the initials' ink until initials can
/// reach small-text contrast on it. Presence hues sit at one mid lightness,
/// where neither ink clears much more than 4.5:1; small bold letters on a
/// saturated mid tone need the headroom.
#[must_use]
pub fn initials_fill(fill: Color, theme: &Theme) -> Color {
    use crate::color::{Oklch, contrast_ratio, oklch};
    use crate::tokens::colors::SMALL_TEXT_MIN_CONTRAST;
    let dark = dark_initials(theme);
    let extreme = if dark {
        oklch(0.0, 0.0, 0.0)
    } else {
        oklch(1.0, 0.0, 0.0)
    };
    let base = Oklch::from_color(fill);
    let target = if dark { 1.0 } else { 0.0 };
    for step in 0..=20_u8 {
        let t = f32::from(step) / 40.0;
        let candidate = oklch(base.l + (target - base.l) * t, base.c * (1.0 - t), base.h);
        if contrast_ratio(extreme, candidate) >= SMALL_TEXT_MIN_CONTRAST {
            return candidate;
        }
    }
    oklch(base.l + (target - base.l) * 0.5, base.c * 0.5, base.h)
}

/// The initials ink on `fill`.
///
/// One direction per presence palette (`dark_initials`). The ink takes
/// the fill's own hue, as deep (or as pale) as it must be to reach
/// small-text contrast (7:1) on that fill, or the extreme where it cannot.
#[must_use]
pub fn initial_ink(fill: Color, theme: &Theme) -> Color {
    use crate::color::{Oklch, contrast_ratio, oklch};
    use crate::tokens::colors::SMALL_TEXT_MIN_CONTRAST;
    let dark = dark_initials(theme);
    let hue = Oklch::from_color(fill).h;
    let chroma = 0.04_f32;
    // Walk from a tinted ink toward black (or white) until it reads.
    let mut step = 0_u8;
    loop {
        let t = f32::from(step) / 20.0;
        let (l, c) = if dark {
            (0.34 * (1.0 - t), chroma * (1.0 - t))
        } else {
            (0.96 + 0.04 * t, chroma * 0.5 * (1.0 - t))
        };
        let ink = oklch(l, c, hue);
        if contrast_ratio(ink, fill) >= SMALL_TEXT_MIN_CONTRAST || step >= 20 {
            return ink;
        }
        step += 1;
    }
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
        .with_content_size(crate::num::f32_of_f64(side * 0.4))
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
    name_state: Option<crate::local_state::LocalBinding<String>>,
    identity: String,
    presence: Option<Color>,
    picture: Picture,
    image: Option<peniko::ImageData>,
    size: AvatarSize,
    square: bool,
    neutral: bool,
    status: Option<PresenceStatus>,
    glyph: Option<crate::icons::Icon>,
    side: f64,
    rect: Rect,
    dirty: bool,
}

// The photo type comes from runtime-identity, which only `local-daemons`
// builds link; portable builds (the browser's) show initials or an image.
#[cfg(feature = "local-daemons")]
pub(crate) fn profile_image(photo: &runtime_identity::ProfilePhoto) -> peniko::ImageData {
    peniko::ImageData {
        data: peniko::Blob::new(std::sync::Arc::new(photo.rgba().to_vec())),
        format: peniko::ImageFormat::Rgba8,
        alpha_type: peniko::ImageAlphaType::Alpha,
        width: u32::from(photo.width()),
        height: u32::from(photo.height()),
    }
}

impl Avatar {
    pub fn person(name: impl Into<String>, identity: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            name_state: None,
            identity: identity.into(),
            presence: None,
            picture: Picture::None,
            image: None,
            size: AvatarSize::Regular,
            square: false,
            neutral: false,
            status: None,
            glyph: None,
            side: 0.0,
            rect: Rect::ZERO,
            dirty: true,
        }
    }

    /// An app without artwork of its own: see [`AvatarFace::glyph`]. A
    /// picture ([`Self::with_image_url`]) still replaces it once it loads.
    #[must_use]
    pub fn with_glyph(mut self, glyph: crate::icons::Icon) -> Self {
        self.glyph = Some(glyph);
        self.neutral = true;
        self.square = true;
        self
    }

    /// Bind the displayed name and accessible description to process-local
    /// presentation state, retaining the avatar and adjacent input widgets.
    /// Identity, picture and geometry remain independently configured.
    #[must_use]
    pub fn with_name_state(mut self, state: &crate::LocalState<String>) -> Self {
        let (name, binding) = crate::local_state::LocalBinding::new(state);
        self.name = name;
        self.name_state = Some(binding);
        self.dirty = true;
        self
    }

    fn refresh_name(&mut self) -> bool {
        if let Some(binding) = &mut self.name_state
            && let Some(name) = binding.poll()
            && self.name != name
        {
            self.name = name;
            self.dirty = true;
            return true;
        }
        false
    }

    /// See [`AvatarFace::presence`].
    #[must_use]
    pub fn with_presence(mut self, colour: Color) -> Self {
        self.presence = Some(colour);
        self
    }

    /// Render the validated public photo carried by canonical identity metadata.
    /// Pixels are already normalized; this opens no files and decodes no image.
    #[cfg(feature = "local-daemons")]
    #[must_use]
    pub fn with_profile_photo(self, photo: &runtime_identity::ProfilePhoto) -> Self {
        self.with_image(profile_image(photo))
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

    /// An app or a thing: see [`AvatarFace::neutral`].
    #[must_use]
    pub fn neutral(mut self) -> Self {
        self.neutral = true;
        self.square = true;
        self
    }

    /// See [`AvatarFace::status`].
    #[must_use]
    pub fn status(mut self, status: PresenceStatus) -> Self {
        self.status = Some(status);
        self
    }

    /// Change the presence badge.
    pub fn set_status(&mut self, status: Option<PresenceStatus>) {
        if status != self.status {
            self.status = status;
            self.dirty = true;
        }
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
        if self.neutral {
            face = face.neutral();
        }
        if let Some(glyph) = self.glyph {
            face = face.glyph(glyph);
        }
        if let Some(status) = self.status {
            face = face.status(status);
        }
        face
    }
}

impl Widget for Avatar {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn sync(&mut self, _state: &crate::widget::AppModel, _path: &crate::widget::ModelPath) {
        self.refresh_name();
        if self.image.is_none()
            && let Picture::Lazy { store, url } = &self.picture
            && let Some(image) = store.get(url)
        {
            self.image = Some(image);
            self.dirty = true;
        }
    }

    fn update(
        &mut self,
        _state: &crate::widget::AppModel,
        _path: &crate::widget::ModelPath,
    ) -> bool {
        self.refresh_name()
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.side = self.size.side(theme);
        let side = crate::num::f32_of_f64(self.side);
        let style = Style {
            size: Size::from_lengths(side, side),
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = crate::widget::layout_rect(taffy, node, origin);
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

    crate::redraw_flag!();

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
        let name = (!self.name.trim().is_empty()).then(|| match self.status {
            Some(status) => format!("{}, {}", self.name, status.spoken()),
            None => self.name.clone(),
        });
        Some(AccessNode::new(Role::Image).with_optional_name(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color_compat::ColorCompat;
    use vello::peniko::ImageData as PenikoImage;
    use vello_encoding::DrawTag;

    fn image(width: u32, height: u32) -> PenikoImage {
        let pixels = usize::try_from(width * height * 4).unwrap();
        crate::raster::rgba8(vec![200; pixels], width, height)
    }

    #[test]
    fn an_app_glyph_replaces_the_initials_on_the_neutral_square() {
        let theme = Theme::dark();
        // A nameless neutral tile paints only the tile.
        let blank = paint_face(AvatarFace::new("", "so.tilo.notes").neutral(), &theme);
        let glyph = paint_face(
            AvatarFace::new("", "so.tilo.notes").glyph(crate::icons::Icon::FileText),
            &theme,
        );
        assert!(
            tags(&glyph, DrawTag::COLOR) > tags(&blank, DrawTag::COLOR),
            "the glyph's strokes are painted on the tile"
        );
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

    fn nested_message_scene(text: &mut TextRenderer, theme: &Theme, transform: Affine) -> Scene {
        use crate::widget::{BlockContent, BlockEditor, Embedded};
        let mut editor = BlockEditor::new()
            .with_wrap_lines(true)
            .with_compact_paragraph_spacing(true)
            .with_block_handles(false)
            .with_read_only(true);
        editor.present_blocks(&[BlockContent::paragraph("message", "A preceding message")]);
        let mut body = Embedded::new(editor);
        body.layout(Rect::new(68.0, 28.0, 300.0, 28.0), theme, text);
        let mut scene = Scene::new();
        body.draw(&mut scene, transform, text, theme, &PresenceCtx::empty());
        assert!(
            !scene.encoding().resources.glyphs.is_empty(),
            "text must render"
        );
        scene
    }

    fn assert_avatar_pixels(theme: &Theme, renderer: &mut crate::offscreen::OffscreenRenderer) {
        let mut text = TextRenderer::new();
        let transform = Affine::translate((13.0, 17.0));
        let mut scene = nested_message_scene(&mut text, theme, transform);
        let ground = theme.tokens.colors.surfaces.content;
        let before = renderer.render(&scene, ground);
        let mut avatar = crate::widget::Embedded::new(Avatar::person("Scribe Agent", "scribe"));
        avatar.layout(Rect::new(24.0, 104.0, 64.0, 144.0), theme, &mut text);
        let bounds = transform.transform_rect_bbox(avatar.rect());
        avatar.draw(
            &mut scene,
            transform,
            &mut text,
            theme,
            &PresenceCtx::empty(),
        );
        let after = renderer.render(&scene, ground);
        let fill = AvatarFace::new("Scribe Agent", "scribe").colour(theme);
        let mut changed = 0;
        let mut ink = 0;
        for y in 0..240_u32 {
            for x in 0..360_u32 {
                let pixel = crate::offscreen::OffscreenRenderer::sample(&after, 360, x, y);
                if pixel == crate::offscreen::OffscreenRenderer::sample(&before, 360, x, y) {
                    continue;
                }
                assert!(
                    bounds.contains(Point::new(f64::from(x), f64::from(y))),
                    "avatar escaped its laid-out bounds at ({x}, {y})"
                );
                changed += 1;
                if bounds
                    .inset(10.0)
                    .contains(Point::new(f64::from(x), f64::from(y)))
                    && crate::color::contrast_ratio(
                        Color::from_rgb8(pixel.0, pixel.1, pixel.2),
                        fill,
                    ) >= crate::tokens::colors::SMALL_TEXT_MIN_CONTRAST - 0.05
                {
                    ink += 1;
                }
            }
        }
        assert!(
            changed > 700,
            "the avatar background must paint in its bounds"
        );
        assert!(
            ink > 10,
            "legible initials must paint inside that background"
        );
        let pixel = crate::offscreen::OffscreenRenderer::sample(&after, 360, 57, 126);
        assert_eq!((pixel.0, pixel.1, pixel.2), (fill.r(), fill.g(), fill.b()));
        assert!(
            crate::color::contrast_ratio(initial_ink(fill, theme), fill)
                >= crate::tokens::colors::SMALL_TEXT_MIN_CONTRAST - 0.05
        );
    }

    #[test]
    fn nested_avatar_background_and_initials_share_the_laid_out_bounds() {
        let mut renderer = crate::offscreen::OffscreenRenderer::new(360, 240);
        for theme in [Theme::dark(), Theme::light()] {
            assert_avatar_pixels(&theme, &mut renderer);
        }
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

    #[cfg(feature = "local-daemons")]
    #[test]
    fn bound_name_refresh_keeps_photo_and_adjacent_input_selection() {
        use crate::widget::{Row, TextInput};
        let name = crate::LocalState::new("Ada".to_owned());
        let photo = runtime_identity::ProfilePhoto::new(1, 1, vec![200, 100, 50, 255]).unwrap();
        let avatar = Avatar::person("Ada", "ada")
            .with_name_state(&name)
            .with_profile_photo(&photo)
            .size(AvatarSize::Portrait);
        let input = TextInput::local().with_initial_value("Ada");
        let root = Row::new().push(Box::new(avatar)).push(Box::new(input));
        let mut harness = crate::headless::FrameHarness::new_with_theme(
            crate::State::new(),
            Box::new(root),
            (640, 320),
            Theme::dark(),
        );
        harness
            .with_widget_mut::<TextInput, _>(|input| {
                input.set_text("Ada Lovelace");
                input.set_focus(true);
                input.set_selection(4, 8);
            })
            .unwrap();
        let original = harness
            .with_widget_mut::<Avatar, _>(|avatar| (std::ptr::from_ref(avatar), avatar.get_rect()))
            .unwrap();
        name.set("Ada Lovelace".to_owned());
        // Run the root's production update/sync/layout passes before inspecting
        // the widgets: with_widget_mut invokes its visitor before pumping a frame.
        // A name edit must not rebuild the neighbouring editor or replace its
        // presentation state.
        harness.relayout();
        harness
            .with_widget_mut::<Avatar, _>(|avatar| {
                assert_eq!(std::ptr::from_ref(avatar), original.0);
                assert_eq!(avatar.get_rect(), original.1);
                assert_eq!(avatar.name, "Ada Lovelace");
                assert!(avatar.image.is_some());
                assert_eq!(
                    Widget::accessibility(avatar).unwrap().name.as_deref(),
                    Some("Ada Lovelace")
                );
            })
            .unwrap();
        harness
            .with_widget_mut::<TextInput, _>(|input| {
                assert!(input.is_focused());
                assert_eq!(input.cursor_pos(), 8);
                assert_eq!(input.selection_range_public(), Some((4, 8)));
            })
            .unwrap();
    }

    #[cfg(feature = "local-daemons")]
    #[test]
    fn explicit_canonical_photo_is_drawn_by_shared_avatar() {
        let photo = runtime_identity::ProfilePhoto::new(1, 1, vec![200, 100, 50, 255]).unwrap();
        let mut avatar = Avatar::person("Ada", "ada").with_profile_photo(&photo);
        let mut tree = TaffyTree::new();
        let mut map = std::collections::HashMap::new();
        let theme = Theme::dark();
        let node = avatar.build_layout(&mut tree, &theme, &mut map);
        tree.compute_layout(
            node,
            <taffy::Size<taffy::AvailableSpace> as taffy::prelude::TaffyMaxContent>::MAX_CONTENT,
        )
        .unwrap();
        avatar.apply_layout(&tree, node, Point::ZERO);
        let mut scene = Scene::new();
        avatar.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &theme,
            &PresenceCtx::empty(),
        );
        assert_eq!(tags(&scene, DrawTag::IMAGE), 1);
        assert_eq!(
            Widget::accessibility(&avatar).unwrap().name.as_deref(),
            Some("Ada")
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

    #[test]
    fn initials_reach_small_text_contrast_on_every_avatar_fill() {
        for theme in [Theme::dark(), Theme::light()] {
            for (index, &hue) in theme.tokens.colors.presence.colors.iter().enumerate() {
                let fill = initials_fill(hue, &theme);
                let ink = initial_ink(fill, &theme);
                let ratio = crate::color::contrast_ratio(ink, fill);
                assert!(
                    ratio >= crate::tokens::colors::SMALL_TEXT_MIN_CONTRAST - 0.05,
                    "{}: initials on presence hue {index} reach only {ratio:.2}:1",
                    theme.name
                );
                let (before, after) = (
                    crate::color::Oklch::from_color(hue).h,
                    crate::color::Oklch::from_color(fill).h,
                );
                let turn = (before - after).abs().min(360.0 - (before - after).abs());
                assert!(
                    turn < 8.0,
                    "{}: the fill keeps the person's hue",
                    theme.name
                );
            }
        }
    }

    #[test]
    fn initials_ink_is_one_legible_direction_per_palette() {
        for theme in [Theme::dark(), Theme::light()] {
            let palette = theme.tokens.colors.presence;
            let inks: Vec<Color> = palette
                .colors
                .iter()
                .map(|&fill| initial_ink(fill, &theme))
                .collect();
            for (ink, fill) in inks.iter().zip(palette.colors) {
                let ratio = crate::color::contrast_ratio(*ink, fill);
                assert!(ratio >= 4.5, "{}: initials at {ratio:.2}:1", theme.name);
            }
            let dark = |c: &Color| crate::color::relative_luminance(*c) < 0.18;
            assert!(
                inks.iter().all(dark) || !inks.iter().any(dark),
                "{}: initials flip between dark and light inks",
                theme.name
            );
        }
    }

    #[test]
    fn initials_skip_words_without_letters() {
        assert_eq!(initials_for_name("Tilo 4"), "T");
        assert_eq!(initials_for_name("Ada 2"), "A");
        assert_eq!(initials_for_name("Pixel 7 Pro"), "PP");
        assert_eq!(initials_for_name("2024"), "");
    }

    #[test]
    fn a_status_badge_sits_on_the_corner_and_is_spoken() {
        let theme = Theme::dark();
        let face = AvatarFace::new("Ada Lovelace", "ada").status(PresenceStatus::Online);
        let rect = Rect::new(0.0, 0.0, 40.0, 40.0);
        let badge = face.status_rect(rect, &theme);
        assert!(badge.center().x > rect.center().x && badge.center().y > rect.center().y);
        assert!(badge.x1 <= rect.x1 + 0.5 && badge.y1 <= rect.y1 + 0.5);
        let mut text = TextRenderer::new();
        let mut with = Scene::new();
        face.paint(
            &mut PaintCtx::new(&mut with, Affine::IDENTITY, &mut text, &theme),
            rect,
        );
        let mut without = Scene::new();
        AvatarFace::new("Ada Lovelace", "ada").paint(
            &mut PaintCtx::new(&mut without, Affine::IDENTITY, &mut text, &theme),
            rect,
        );
        assert!(with.encoding().draw_tags.len() > without.encoding().draw_tags.len());
        let avatar = Avatar::person("Ada Lovelace", "ada").status(PresenceStatus::Away);
        assert_eq!(
            Widget::accessibility(&avatar).unwrap().name.as_deref(),
            Some("Ada Lovelace, away")
        );
        let neutral = AvatarFace::new("Notes", "notes").neutral();
        assert_eq!(neutral.colour(&theme), theme.tokens.colors.surfaces.raised);
    }
}
