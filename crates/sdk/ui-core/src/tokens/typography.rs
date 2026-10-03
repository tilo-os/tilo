//! The semantic type system.
//!
//! Apps say what a piece of text *is* — a [`TextRole`] — and the theme
//! decides its face, size, weight and leading. Every role size is a ratio of
//! one body size, so a theme author retunes the whole scale with
//! [`Typography::with_base_size`] and the accessibility text scale grows every
//! role at once. [`Typography::style`] (and its shorthand
//! [`crate::theme::Theme::text`]) returns a style whose size already includes
//! the theme's UI and text scale ([`TextScaling::Resolved`]), so the renderer
//! paints it exactly once at that size.

use crate::text_render::{FontChoice, NumericStyle, TextScaling, TextStyle, TextWeight};

/// The body size, in logical pixels at 1×, that every role is designed
/// against. Role sizes are `body × role_px / REFERENCE_BODY_PX`, so they land
/// on whole pixels at the stock size.
pub const REFERENCE_BODY_PX: f32 = 14.0;

/// What a piece of text is. Pick the role; never a pixel size.
///
/// | Role | Size / weight / leading at 1× | Use |
/// |---|---|---|
/// | `Display` | 34 / 600 / 1.15 | One hero number or name per screen at most |
/// | `LargeTitle` | 26 / 600 / 1.2 | Top of a content page |
/// | `Title` | 20 / 600 / 1.25 | Detail title (a message subject, a contact name) |
/// | `Heading` | 16 / 600 / 1.3 | Section heading |
/// | `Subheading` | 14 / 600 / 1.35 | Group label inside a section, list section header |
/// | `Body` | 14 / 400 / 1.45 | Default text |
/// | `Label` | 13 / 500 / 1.3 | Control labels, row titles in dense lists |
/// | `Caption` | 12 / 400 / 1.35 | Secondary line, timestamps, metadata |
/// | `Reading` | 16 / 400 / 1.6, serif | Long-form documents and books |
/// | `Code` | 13 / 400 / 1.5, mono | Code and terminal |
///
/// Hierarchy comes from size and weight plus ink tier. There is no all-caps
/// or letter-spaced role: a section header is sentence case in
/// [`TextRole::Subheading`] or [`TextRole::Heading`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum TextRole {
    Display,
    LargeTitle,
    Title,
    Heading,
    Subheading,
    #[default]
    Body,
    Label,
    Caption,
    Reading,
    Code,
}

/// Which of the theme's three families a role is set in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Face {
    Sans,
    Serif,
    Mono,
}

/// One row of the role table.
#[derive(Clone, Copy, Debug)]
struct RoleSpec {
    px: f32,
    weight: TextWeight,
    leading: f32,
    face: Face,
}

impl TextRole {
    /// Every role, largest first.
    pub const ALL: [Self; 10] = [
        Self::Display,
        Self::LargeTitle,
        Self::Title,
        Self::Heading,
        Self::Subheading,
        Self::Body,
        Self::Label,
        Self::Caption,
        Self::Reading,
        Self::Code,
    ];

    const fn spec(self) -> RoleSpec {
        const fn sans(px: f32, weight: TextWeight, leading: f32) -> RoleSpec {
            RoleSpec {
                px,
                weight,
                leading,
                face: Face::Sans,
            }
        }
        match self {
            Self::Display => sans(34.0, TextWeight::Semibold, 1.15),
            Self::LargeTitle => sans(26.0, TextWeight::Semibold, 1.2),
            Self::Title => sans(20.0, TextWeight::Semibold, 1.25),
            Self::Heading => sans(16.0, TextWeight::Semibold, 1.3),
            Self::Subheading => sans(14.0, TextWeight::Semibold, 1.35),
            Self::Body => sans(14.0, TextWeight::Regular, 1.45),
            Self::Label => sans(13.0, TextWeight::Medium, 1.3),
            Self::Caption => sans(12.0, TextWeight::Regular, 1.35),
            Self::Reading => RoleSpec {
                px: 16.0,
                weight: TextWeight::Regular,
                leading: 1.6,
                face: Face::Serif,
            },
            Self::Code => RoleSpec {
                px: 13.0,
                weight: TextWeight::Regular,
                leading: 1.5,
                face: Face::Mono,
            },
        }
    }

    /// The role's own weight, before any `.strong()` or override.
    #[must_use]
    pub const fn weight(self) -> TextWeight {
        self.spec().weight
    }

    /// The screen-reader heading level a role implies: `LargeTitle` and
    /// `Title` are level 1, `Heading` level 2 and `Subheading` level 3.
    /// `Display` is a hero value, not a heading.
    #[must_use]
    pub const fn heading_level(self) -> Option<u8> {
        match self {
            Self::LargeTitle | Self::Title => Some(1),
            Self::Heading => Some(2),
            Self::Subheading => Some(3),
            _ => None,
        }
    }
}

/// The one size every role derives from: the body size, already multiplied
/// by the theme's UI and text scale. Private so nothing outside the toolkit
/// can pick a pixel size; read a role through [`Typography::style`].
#[derive(Clone, Debug, PartialEq)]
pub struct TypeScale {
    body: f32,
}

impl TypeScale {
    pub(crate) const fn new(body: f32) -> Self {
        Self { body }
    }

    /// This scale grown by `factor` (text scale, UI scale, or both).
    #[must_use]
    pub(crate) fn scaled(&self, factor: f32) -> Self {
        Self::new(self.body * factor)
    }

    /// The resolved size of `role`.
    pub(crate) fn size(&self, role: TextRole) -> f32 {
        self.body * role.spec().px / REFERENCE_BODY_PX
    }
}

impl Default for TypeScale {
    fn default() -> Self {
        Self::new(REFERENCE_BODY_PX)
    }
}

/// A named font family, wrapped so consumers can extend with fallbacks/weights later
/// without breaking token consumers.
#[derive(Clone, Debug)]
pub struct FontFamily {
    pub family: String,
}

impl FontFamily {
    pub fn new(family: impl Into<String>) -> Self {
        Self {
            family: family.into(),
        }
    }
}

/// A theme's type: three families and the scale every role derives from.
#[derive(Clone, Debug)]
pub struct Typography {
    pub sans_family: String,
    pub serif_family: String,
    pub mono_family: String,
    pub(crate) scale: TypeScale,
}

impl Default for Typography {
    fn default() -> Self {
        Self {
            sans_family: crate::text_render::UI_SANS_FAMILY.into(),
            serif_family: crate::text_render::UI_SERIF_FAMILY.into(),
            mono_family: crate::text_render::UI_MONOSPACE_FAMILY.into(),
            scale: TypeScale::default(),
        }
    }
}

/// The stock typography, built once: what a retained widget resolves its
/// role against before it has seen the active theme.
pub(crate) fn stock() -> &'static Typography {
    static STOCK: std::sync::LazyLock<Typography> = std::sync::LazyLock::new(Typography::default);
    &STOCK
}

impl Typography {
    /// Set the 1× body size every role derives from. Theme authors call this
    /// on a fresh theme, before the text or UI scale is applied.
    #[must_use]
    pub fn with_base_size(mut self, body_px: f32) -> Self {
        self.scale = TypeScale::new(body_px.max(1.0));
        self
    }

    /// Resolve a role into the complete style consumed by both text
    /// measurement and painting. The size already carries the theme's UI and
    /// text scale, so the style is [`TextScaling::Resolved`].
    #[must_use]
    pub fn style(&self, role: TextRole) -> TextStyle {
        let spec = role.spec();
        let family = match spec.face {
            Face::Sans => &self.sans_family,
            Face::Serif => &self.serif_family,
            Face::Mono => &self.mono_family,
        };
        let mut style = TextStyle::new(self.scale.size(role))
            .with_line_height(spec.leading)
            .with_weight(spec.weight)
            .with_family(FontChoice::Named(family.clone()));
        style.scaling = TextScaling::Resolved;
        style
    }

    /// The resolved size of `role`, for a custom painter that has to size
    /// geometry (an icon, a swatch) against a line of that role. Paint the
    /// text itself with [`Self::style`].
    #[must_use]
    pub fn size(&self, role: TextRole) -> f32 {
        self.scale.size(role)
    }
}

impl crate::theme::Theme {
    /// The resolved style of `role` in this theme: face, size, weight and
    /// leading, with the UI and text scale already applied. Measure and draw
    /// with the same value through [`crate::text_render::TextRenderer::measure_style`]
    /// and [`crate::text_render::TextRenderer::draw_style`] (or the
    /// [`crate::paint::PaintCtx`] role helpers).
    #[must_use]
    pub fn text(&self, role: TextRole) -> TextStyle {
        self.tokens.typography.style(role)
    }
}

impl TextStyle {
    /// The next emphasis weight: regular and medium text become semibold,
    /// semibold becomes bold. Use it for emphasis inside a role
    /// (`Body.strong()` for a name in a row) instead of picking a weight.
    #[must_use]
    pub fn strong(mut self) -> Self {
        self.weight = match self.weight {
            TextWeight::Regular | TextWeight::Medium => TextWeight::Semibold,
            TextWeight::Semibold | TextWeight::Bold => TextWeight::Bold,
        };
        self
    }

    /// Tabular lining figures, so digits in a column or a changing readout
    /// line up.
    #[must_use]
    pub fn numeric(self) -> Self {
        self.with_numeric_style(NumericStyle::TabularLining)
    }

    /// This style's face, weight and leading at exactly `px`, as content
    /// ([`TextScaling::Content`]): type sized by the thing it sits in — the
    /// initials in an avatar disc, a label on a zoomed canvas — rather than
    /// by the reader's text scale.
    #[must_use]
    pub fn with_content_size(mut self, px: f32) -> Self {
        self.size = px;
        self.scaling = TextScaling::Content;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    #[test]
    fn roles_resolve_to_the_design_table_from_one_base() {
        let typography = Typography::default();
        let expect = [
            (TextRole::Display, 34.0, TextWeight::Semibold, 1.15),
            (TextRole::LargeTitle, 26.0, TextWeight::Semibold, 1.2),
            (TextRole::Title, 20.0, TextWeight::Semibold, 1.25),
            (TextRole::Heading, 16.0, TextWeight::Semibold, 1.3),
            (TextRole::Subheading, 14.0, TextWeight::Semibold, 1.35),
            (TextRole::Body, 14.0, TextWeight::Regular, 1.45),
            (TextRole::Label, 13.0, TextWeight::Medium, 1.3),
            (TextRole::Caption, 12.0, TextWeight::Regular, 1.35),
            (TextRole::Reading, 16.0, TextWeight::Regular, 1.6),
            (TextRole::Code, 13.0, TextWeight::Regular, 1.5),
        ];
        for (role, size, weight, leading) in expect {
            let style = typography.style(role);
            assert_eq!(style.size, size, "{role:?} size");
            assert_eq!(style.weight, weight, "{role:?} weight");
            assert_eq!(style.line_height, leading, "{role:?} leading");
            assert_eq!(style.scaling, TextScaling::Resolved, "{role:?} scaling");
        }
        assert_eq!(
            typography.style(TextRole::Reading).family,
            FontChoice::Named(crate::text_render::UI_SERIF_FAMILY.into())
        );
        assert_eq!(
            typography.style(TextRole::Code).family,
            FontChoice::Named(crate::text_render::UI_MONOSPACE_FAMILY.into())
        );
        assert_eq!(
            typography.style(TextRole::Body).family,
            FontChoice::Named(crate::text_render::UI_SANS_FAMILY.into())
        );

        // One value retunes every role.
        let larger = Typography::default().with_base_size(16.0);
        for role in TextRole::ALL {
            let ratio = larger.style(role).size / typography.style(role).size;
            assert!(
                (ratio - 16.0 / 14.0).abs() < 1e-5,
                "{role:?} follows the base"
            );
        }
    }

    #[test]
    fn ui_roles_descend_monotonically() {
        let typography = Typography::default();
        let order = [
            TextRole::Display,
            TextRole::LargeTitle,
            TextRole::Title,
            TextRole::Heading,
            TextRole::Subheading,
            TextRole::Label,
            TextRole::Caption,
        ];
        for pair in order.windows(2) {
            let (a, b) = (typography.style(pair[0]), typography.style(pair[1]));
            assert!(
                a.size > b.size || (a.size == b.size && a.weight.to_u16() > b.weight.to_u16()),
                "{:?} must outrank {:?}",
                pair[0],
                pair[1]
            );
        }
        // Subheading and Body share a size; weight carries the step.
        let (sub, body) = (
            typography.style(TextRole::Subheading),
            typography.style(TextRole::Body),
        );
        assert_eq!(sub.size, body.size);
        assert!(sub.weight.to_u16() > body.weight.to_u16());
        assert!(body.size > typography.style(TextRole::Label).size);
    }

    #[test]
    fn theme_text_is_resolved_once_through_both_scales() {
        let theme = Theme::dark().with_ui_scale(1.25).with_text_scale(1.5);
        for role in TextRole::ALL {
            let style = theme.text(role);
            let one_x = Theme::dark().text(role);
            assert!(
                (style.size - one_x.size * 1.25 * 1.5).abs() < 1e-4,
                "{role:?}: {} must be {} × 1.25 × 1.5",
                style.size,
                one_x.size
            );
        }
        // A weight override keeps the resolved size.
        let strong = theme.text(TextRole::Body).strong();
        assert_eq!(strong.size, theme.text(TextRole::Body).size);
        assert_eq!(strong.weight, TextWeight::Semibold);
        assert_eq!(strong.scaling, TextScaling::Resolved);
    }

    #[test]
    fn custom_theme_families_apply_to_every_role() {
        let mut theme = Theme::dark();
        theme.tokens.typography.sans_family = "Inter".into();
        theme.tokens.typography.serif_family = "Literata".into();
        theme.tokens.typography.mono_family = "Iosevka".into();
        for role in TextRole::ALL {
            let expected = match role {
                TextRole::Reading => "Literata",
                TextRole::Code => "Iosevka",
                _ => "Inter",
            };
            assert_eq!(
                theme.text(role).family,
                FontChoice::Named(expected.into()),
                "{role:?}"
            );
        }
    }

    #[test]
    fn heading_levels_follow_the_role() {
        assert_eq!(TextRole::LargeTitle.heading_level(), Some(1));
        assert_eq!(TextRole::Title.heading_level(), Some(1));
        assert_eq!(TextRole::Heading.heading_level(), Some(2));
        assert_eq!(TextRole::Subheading.heading_level(), Some(3));
        assert_eq!(TextRole::Display.heading_level(), None);
        assert_eq!(TextRole::Body.heading_level(), None);
    }
}
