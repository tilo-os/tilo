use vello::peniko::Color;
use crate::tokens::Tokens;

#[derive(Clone, Debug)]
pub struct Theme {
    pub name: String,
    pub tokens: Tokens,
    // Legacy shortcuts — derived from tokens on construction, kept for compat during migration.
    pub bg_app: Color,
    pub bg_primary: Color,
    pub bg_hover: Color,
    pub bg_active: Color,
    pub border_standard: Color,
    pub border_focus: Color,
    pub text_primary: Color,
    pub text_dim: Color,
    pub accent_primary: Color,
    pub status_success: Color,
    pub radius_standard: f64,
    pub padding_standard: f32,
    pub gap_standard: f32,
}

impl Default for Theme { fn default() -> Self { Self::dark() } }

impl Theme {
    fn from_tokens(name: &str, tokens: Tokens) -> Self {
        let c = &tokens.colors;
        Self {
            name: name.into(),
            bg_app: c.surfaces.base,
            bg_primary: c.surfaces.raised,
            bg_hover: c.neutral[4],
            bg_active: c.neutral[2],
            border_standard: c.border_standard,
            border_focus: c.border_focus,
            text_primary: c.text_primary,
            text_dim: c.text_dim,
            accent_primary: c.accent[4],
            status_success: c.semantic.success,
            radius_standard: tokens.radii.md as f64,
            padding_standard: tokens.spacing.s3,
            gap_standard: tokens.spacing.s3,
            tokens,
        }
    }
    pub fn dark() -> Self { Self::from_tokens("Dark", Tokens::dark()) }
    pub fn light() -> Self { Self::from_tokens("Light", Tokens::light()) }
    pub fn dracula() -> Self {
        let mut t = Tokens::dark();
        t.colors.surfaces.base = Color::rgb8(40, 42, 54);
        t.colors.surfaces.raised = Color::rgb8(68, 71, 90);
        t.colors.text_primary = Color::rgb8(248, 248, 242);
        t.colors.accent.stops[4] = Color::rgb8(255, 121, 198);
        t.colors.border_focus = Color::rgb8(189, 147, 249);
        Self::from_tokens("Dracula", t)
    }
    pub fn nord() -> Self {
        let mut t = Tokens::dark();
        t.colors.surfaces.base = Color::rgb8(46, 52, 64);
        t.colors.surfaces.raised = Color::rgb8(59, 66, 82);
        t.colors.text_primary = Color::rgb8(216, 222, 233);
        t.colors.accent.stops[4] = Color::rgb8(129, 161, 193);
        t.colors.border_focus = Color::rgb8(136, 192, 208);
        Self::from_tokens("Nord", t)
    }
    pub fn gruvbox() -> Self {
        let mut t = Tokens::dark();
        t.colors.surfaces.base = Color::rgb8(40, 40, 40);
        t.colors.surfaces.raised = Color::rgb8(60, 56, 54);
        t.colors.text_primary = Color::rgb8(235, 219, 178);
        t.colors.accent.stops[4] = Color::rgb8(184, 187, 38);
        t.colors.border_focus = Color::rgb8(254, 128, 25);
        t.radii.md = 2.0;
        Self::from_tokens("Gruvbox", t)
    }
    pub fn solarized() -> Self {
        let mut t = Tokens::dark();
        t.colors.surfaces.base = Color::rgb8(0, 43, 54);
        t.colors.surfaces.raised = Color::rgb8(7, 54, 66);
        t.colors.text_primary = Color::rgb8(131, 148, 150);
        t.colors.accent.stops[4] = Color::rgb8(42, 161, 152);
        t.colors.border_focus = Color::rgb8(38, 139, 210);
        t.radii.md = 0.0;
        Self::from_tokens("Solarized", t)
    }
}
