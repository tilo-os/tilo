//! The theme's values a painter draws with.

use std::borrow::Cow;
use std::ops::RangeInclusive;
use std::sync::Arc;

use peniko::Color;

/// A parameter a painter set takes from the theme document: its name, the
/// value it has when a document leaves it out (`None`: a document must
/// state it), the band a document may state it in, and the band the theme
/// is built at.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamSpec {
    pub name: Cow<'static, str>,
    pub default: Option<f32>,
    pub admitted: RangeInclusive<f32>,
    pub usable: RangeInclusive<f32>,
}

impl ParamSpec {
    /// `name`, admitted and usable across `band`; `default` when left out.
    #[must_use]
    pub fn new(name: &'static str, default: Option<f32>, band: RangeInclusive<f32>) -> Self {
        Self {
            name: Cow::Borrowed(name),
            default,
            usable: band.clone(),
            admitted: band,
        }
    }

    /// This parameter, built at no more than `usable` of its admitted band.
    #[must_use]
    pub fn usable_within(mut self, usable: RangeInclusive<f32>) -> Self {
        self.usable = usable;
        self
    }
}

/// A named number from the theme document, for the painter set that reads
/// it (a glass set's `refraction`, a dither set's `levels`).
#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub name: String,
    pub value: f32,
}

/// The sides of a part that stand on the display's own edge.
///
/// A surface flush with the screen has no neighbour on that side and no
/// display behind it: it must not draw an edge, a rim or a hairline there,
/// because a pixel of another colour would show between it and the screen's
/// edge. A painter reads this to skip its outline on those sides only;
/// every other side keeps whatever the part's material gives it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Sides(u8);

impl Sides {
    /// No side stands on the display's edge.
    pub const NONE: Self = Self(0);
    /// The top side is the display's own edge.
    pub const TOP: Self = Self(1);
    /// The bottom side is the display's own edge.
    pub const BOTTOM: Self = Self(2);
    /// The leading side is the display's own edge.
    pub const LEADING: Self = Self(4);
    /// The trailing side is the display's own edge.
    pub const TRAILING: Self = Self(8);

    /// These sides with `other`'s added.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether `side` stands on the display's edge.
    #[must_use]
    pub const fn has(self, side: Self) -> bool {
        self.0 & side.0 == side.0
    }

    /// Whether any side stands on the display's edge.
    #[must_use]
    pub const fn any(self) -> bool {
        self.0 != 0
    }

    /// The sides these bits name; unknown bits are `NONE`, so a newer peer's
    /// bits read as sides this version does not know rather than as a
    /// different set.
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits & 0b1111)
    }

    /// These sides as bits, for the wire and for a document.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }
}

impl std::ops::BitOr for Sides {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        self.union(other)
    }
}

/// The theme's values for one part, resolved for its role and state: the
/// colours already carry the theme's contrast guarantees, so a painter that
/// lays `ink` on `fill` is legible.
#[derive(Clone, Debug, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "four independent facts about the frame a painter reads, each its own wire field"
)]
pub struct PartTokens {
    /// The part's own fill: a control's face, a panel's ground, as its role
    /// and state resolved it.
    pub fill: Color,
    /// The ink drawn on `fill`.
    pub ink: Color,
    /// The part's edge, where the design language gives it one.
    pub edge: Color,
    /// The ground the part stands on.
    pub ground: Color,
    /// One surface step above `ground`: a hover's or a rest control's lift.
    pub raised: Color,
    /// The person's accent.
    pub accent: Color,
    /// The light a surface catches: the theme's daylight, at no chroma, for
    /// a sheen across a body or a rim lit along its edge. How strongly a
    /// part catches it is the part's own (`specular`, `edge_highlight`).
    pub light: Color,
    /// Corner radius, logical pixels.
    pub radius: f32,
    /// The theme's border, logical pixels wide; `0` for none.
    pub border_width: f32,
    /// The border's colour.
    pub border: Color,
    /// Device pixels per logical pixel, so a painter can draw to the pixel
    /// grid (pixel edges, a dither aligned to the screen).
    pub scale: f32,
    pub dark: bool,
    pub high_contrast: bool,
    /// Nothing may be translucent: a [`crate::Brush::Backdrop`] shows its
    /// fallback.
    pub reduce_transparency: bool,
    /// A [`crate::Brush::Backdrop`] shows what lies below the part this
    /// frame: its role takes a backdrop (floating chrome, a bar), nothing
    /// forbids translucency, and the toolkit runs a backdrop pass. Where
    /// not, a backdrop brush is its fallback, and a painter may draw the
    /// part opaque its own way instead.
    pub backdrop: bool,
    /// The painter set's own parameters from the theme document, as its
    /// [`ParamSpec`]s declare them.
    pub params: Arc<[Param]>,
    /// The theme's own surface shader (`crate::Brush::Shader`), empty for a
    /// set that draws without one: the toolkit replaces the marker fill
    /// with the evaluated shader. A theme's document is validated when it is
    /// read, so a shader here has already been checked.
    pub shader: crate::ShaderSource,
    /// The sides standing on the display's own edge, where a body draws no
    /// edge of its own and a rim does not light ([`Sides`]).
    pub flush: Sides,
}

impl PartTokens {
    /// The parameter `name`, or `default` when the document does not state
    /// it.
    #[must_use]
    pub fn param(&self, name: &str, default: f32) -> f32 {
        self.params
            .iter()
            .find(|param| param.name == name)
            .map_or(default, |param| param.value)
    }
}

impl Default for PartTokens {
    fn default() -> Self {
        Self {
            fill: Color::WHITE,
            ink: Color::BLACK,
            edge: Color::BLACK,
            ground: Color::WHITE,
            raised: Color::WHITE,
            accent: Color::from_rgb8(0x2a, 0x3f, 0xd1),
            light: Color::WHITE,
            radius: 0.0,
            border_width: 0.0,
            border: Color::BLACK,
            scale: 1.0,
            dark: false,
            high_contrast: false,
            reduce_transparency: false,
            backdrop: false,
            params: Arc::new([]),
            shader: crate::ShaderSource::default(),
            flush: Sides::NONE,
        }
    }
}
