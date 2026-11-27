//! The immediate-mode avatar: a person's picture, initials, colour and shape, painted into a rect
//! the caller laid out, as every surface draws a member.

use vello::kurbo::{Point, Rect, RoundedRect};
use vello::peniko::{self, Color, Fill};

use crate::paint::PaintCtx;
use crate::text_render::TextStyle;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::tokens::colors::PresenceHue;

/// How large an avatar is. Each size reads from the theme's metrics, so it
/// follows the UI scale.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AvatarSize {
    Small,
    #[default]
    Regular,
    Large,
    Hero,
    Portrait,
}

impl AvatarSize {
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

#[derive(Clone, Copy, Debug, PartialEq)]
enum Tint<'a> {
    Identity(&'a str),
    Presence(Color),
    /// One of the presence palette's hues, named by the caller rather than
    /// derived from an identity (an agent's client colour).
    Hue(PresenceHue),
    Neutral,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PresenceStatus {
    Online,
    Away,
    Busy,
    Offline,
}

impl PresenceStatus {
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
    agent: bool,
    presence_ring: bool,
    speaking: f32,
    /// Active now — in a call: the arc this sweeps round the face's ring.
    active: Option<crate::motion::Sweep>,
    idle_on: Option<Color>,
}

impl<'a> AvatarFace<'a> {
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
            agent: false,
            presence_ring: false,
            speaking: 0.0,
            active: None,
            idle_on: None,
        }
    }

    /// The face `person` wears on every surface, in the hue they wear: a person's picture
    /// when they set one, else their initials; an agent's mark ([`Self::agent`]); a bot's
    /// picture or initials on a rounded square. Every place that shows a member draws this one,
    /// so they look the same in chat, in history, on the bar and in a chip.
    #[must_use]
    pub fn of(person: &'a crate::component::Person) -> Self {
        match person.identity.member_id() {
            Some(member) => Self::member(&person.name, member),
            None => Self::new(&person.name, person.identity.id()),
        }
        .hue(person.hue)
    }

    /// [`Self::of`] for `member`, called `name`, in the hue the member wears
    /// ([`PresenceHue::of_member`]).
    #[must_use]
    pub fn member(name: &'a str, member: &'a tilo_actions::MemberId) -> Self {
        use tilo_actions::MemberId;
        let face = Self::new(name, member.id()).hue(PresenceHue::of_member(member));
        match member {
            MemberId::Agent { .. } => face.agent(),
            MemberId::Bot(_) => face.square(),
            MemberId::Person(_) => face,
        }
    }

    /// Software, not a person: the agent's own face, the agent mark on a rounded square in its
    /// colour ([`Self::hue`] for its client's), drawn as the faces of who is here draw it
    /// ([`crate::agent_identity::draw_agent_identity_mark`]), so an agent looks the same on every
    /// row, card and bar it shows on.
    #[must_use]
    pub fn agent(mut self) -> Self {
        self.agent = true;
        self.square = true;
        self.glyph = None;
        if self.tint == Tint::Neutral {
            self.tint = Tint::Identity(self.identity);
        }
        self
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

    #[must_use]
    pub fn status(mut self, status: PresenceStatus) -> Self {
        self.status = Some(status);
        self
    }

    /// Use a member's own presence colour (their chosen identity
    /// colour) instead of hashing an id. It is painted at the theme's
    /// presence lightness and chroma, like every other presence mark.
    #[must_use]
    pub fn presence(mut self, colour: Color) -> Self {
        self.tint = Tint::Presence(colour);
        self
    }

    /// Wear one of the presence palette's hues, named directly instead of
    /// derived from the identity: how an agent's client reads (warm for one,
    /// cool for another) at the theme's presence lightness and chroma.
    #[must_use]
    pub fn hue(mut self, hue: PresenceHue) -> Self {
        self.tint = Tint::Hue(hue);
        self
    }

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

    #[must_use]
    pub fn ringed_on(mut self, ground: Color) -> Self {
        self.ring_on = Some(ground);
        self
    }

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

    #[must_use]
    pub fn speaking(mut self, level: f32) -> Self {
        self.speaking = level.clamp(0.0, 1.0);
        self
    }

    /// The face is active now — they are in a call: a bright arc sweeps round the ring the face
    /// wears, or, where motion is reduced, the ring is lit and still ([`crate::motion::Sweep`]),
    /// as every face active in a call wears it.
    #[must_use]
    pub fn active(mut self, sweep: crate::motion::Sweep) -> Self {
        self.active = Some(sweep);
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
            let reach = gap + ring / 2.0;
            ctx.scene.stroke(
                &vello::kurbo::Stroke::new(ring),
                ctx.transform,
                colour.multiply_alpha(self.speaking),
                None,
                &rounded(shape.rect().inflate(reach, reach), -reach),
            );
        }
        if let Some(sweep) = self.active {
            let reach = gap + ring / 2.0;
            let band = rounded(shape.rect().inflate(reach, reach), -reach);
            crate::presence::paint_active_ring(ctx, colour, &band, ring, &band, ring, sweep);
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

    /// The initials on the face: none for a person nobody named yet, whose face is the person
    /// glyph ([`Self::unnamed_person`]), never letters of the words said in place of a name.
    #[must_use]
    pub fn initials(&self) -> String {
        if self.unnamed_person() {
            return String::new();
        }
        initials_for_name(self.name)
    }

    /// A person who gave no name: the neutral disc with the person glyph, the same on every
    /// surface, their own top bar included. A picture they chose still shows instead.
    fn unnamed_person(&self) -> bool {
        !self.agent
            && !self.square
            && self.glyph.is_none()
            && crate::presence::is_unnamed_person(self.name)
    }

    #[must_use]
    pub fn colour(&self, theme: &Theme) -> Color {
        let presence = &theme.tokens.colors.presence;
        let person = match self.tint {
            Tint::Identity(id) => presence.hue(PresenceHue::of_identity(id)),
            Tint::Presence(colour) => presence.of(colour),
            Tint::Hue(hue) => presence.hue(hue),
            Tint::Neutral => return theme.tokens.colors.surfaces.raised,
        };
        let person = match self.idle_on {
            Some(ground) => presence.idle_on(person, ground),
            None => person,
        };
        initials_fill(person, theme)
    }

    fn presence_colour(&self, theme: &Theme) -> Color {
        match self.tint {
            Tint::Neutral => theme.text_dim,
            _ => self.colour(theme),
        }
    }

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

    pub(super) fn shape(&self, rect: Rect, theme: &Theme) -> RoundedRect {
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

    pub fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect) {
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
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
        if self.agent {
            crate::agent_identity::draw_agent_identity_mark(
                ctx.scene,
                ctx.transform,
                shape.rect(),
                self.colour(ctx.theme),
                ctx.theme,
                crate::agent_identity::AgentIdentityStyle::Solid,
            );
            self.paint_status(ctx, rect);
            return;
        }
        // The picture the host reads for itself (its own person's), unless the caller drew
        // them neutral.
        let public_image = if self.image.is_none() && self.tint != Tint::Neutral {
            crate::people_photos::looked_up(self.identity)
        } else {
            None
        };
        let image = self.image.or(public_image.as_ref());
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
        let unnamed = self.unnamed_person();
        let neutral = self.tint == Tint::Neutral || unnamed;
        let fill = if unnamed {
            ctx.theme.tokens.colors.surfaces.raised
        } else {
            self.colour(ctx.theme)
        };
        ctx.scene
            .fill(Fill::NonZero, ctx.transform, fill, None, &shape);
        if neutral {
            ctx.scene.stroke(
                &vello::kurbo::Stroke::new(f64::from(ctx.theme.tokens.metrics.chrome_rule_px)),
                ctx.transform,
                crate::surface::edge_on(fill),
                None,
                &shape,
            );
        }
        if let Some(glyph) = self.glyph.or(unnamed.then_some(crate::icons::Icon::User)) {
            // The glyph takes the share of the tile an icon takes in the
            // icon set's own 24 px grid, centred.
            let side = shape.rect().width() * 0.55;
            let ink = if neutral {
                ctx.theme.text_primary
            } else {
                initial_ink(fill, ctx.theme)
            };
            glyph.draw(
                ctx.scene,
                ctx.transform,
                Rect::from_center_size(shape.rect().center(), (side, side)),
                ink,
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
        let style = initials_style(ctx.theme, side);
        let (width, height) = ctx.text.measure_style(&label, None, &style);
        let centre = disc.center();
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

pub use tilo_face::initials as initials_for_name;

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
/// saturated mid tone need the margin.
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
/// The direction is chosen against this fill, not the palette as a whole: whichever of black and
/// white reads better on it, so an initial clears [`ON_FILL_MIN_CONTRAST`] on any hue and
/// lightness the palette, a theme's presence tone or a member's own colour puts under it. The ink
/// takes the fill's own hue, as deep (or as pale) as it must be to reach small-text contrast (7:1)
/// on that fill; where no tinted shade can, it is that extreme, which always clears the on-fill
/// floor.
#[must_use]
pub fn initial_ink(fill: Color, theme: &Theme) -> Color {
    use crate::color::{Oklch, contrast_ratio, oklch};
    use crate::tokens::colors::{ON_FILL_MIN_CONTRAST, SMALL_TEXT_MIN_CONTRAST};
    let _ = theme;
    let black = oklch(0.0, 0.0, 0.0);
    let white = oklch(1.0, 0.0, 0.0);
    let dark = contrast_ratio(black, fill) >= contrast_ratio(white, fill);
    let hue = Oklch::from_color(fill).h;
    // Walk from a shade of the fill's own hue toward the extreme until it reads.
    for step in 0..=20_u8 {
        let t = f32::from(step) / 20.0;
        let (l, c) = if dark {
            (0.34 * (1.0 - t), 0.04 * (1.0 - t))
        } else {
            (0.96 + 0.04 * t, 0.02 * (1.0 - t))
        };
        let ink = oklch(l, c, hue);
        if contrast_ratio(ink, fill) >= SMALL_TEXT_MIN_CONTRAST {
            return ink;
        }
    }
    // No shade of the hue reaches small-text contrast: the extreme chosen for this fill reads at
    // least as well as any other ink, and never below the on-fill floor.
    let extreme = if dark { black } else { white };
    debug_assert!(contrast_ratio(extreme, fill) >= ON_FILL_MIN_CONTRAST);
    extreme
}

/// Initials are sized by the disc they sit in, not by the reader's text
/// scale: the theme's emphasised label face at 40 % of the side.
pub(crate) fn initials_style(theme: &Theme, side: f64) -> TextStyle {
    // A small face (a chip's, a row's) gives its initials a larger share of it, so a 20 px face
    // still says who it is in letters a person reads.
    let share = if side < 24.0 { 0.45 } else { 0.4 };
    theme
        .text(TextRole::Label)
        .strong()
        .with_content_size(crate::num::f32_of_f64(side * share))
}

/// A person's profile photo, as the identity keeps it (`width` by `height` RGBA pixels), as an
/// image a face draws.
#[must_use]
pub fn profile_image(rgba: &[u8], width: u16, height: u16) -> peniko::ImageData {
    peniko::ImageData {
        data: peniko::Blob::new(std::sync::Arc::new(rgba.to_vec())),
        format: peniko::ImageFormat::Rgba8,
        alpha_type: peniko::ImageAlphaType::Alpha,
        width: u32::from(width),
        height: u32::from(height),
    }
}
