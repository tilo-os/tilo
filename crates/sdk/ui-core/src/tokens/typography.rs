#[derive(Clone, Debug)]
pub struct TypeScale {
    pub xs: f32,
    pub sm: f32,
    pub base: f32,
    pub lg: f32,
    pub xl: f32,
    pub x2l: f32,
    pub x3l: f32,
    pub x4l: f32,
}

impl Default for TypeScale {
    fn default() -> Self {
        Self {
            xs: 10.0,
            sm: 12.0,
            base: 14.0,
            lg: 16.0,
            xl: 20.0,
            x2l: 24.0,
            x3l: 32.0,
            x4l: 40.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct WeightScale {
    pub regular: u16,
    pub medium: u16,
    pub semibold: u16,
    pub bold: u16,
}

impl Default for WeightScale {
    fn default() -> Self {
        Self {
            regular: 400,
            medium: 500,
            semibold: 600,
            bold: 700,
        }
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

#[derive(Clone, Debug)]
pub struct Typography {
    pub sans_family: String,
    pub serif_family: String,
    pub mono_family: String,
    pub scale: TypeScale,
    pub weights: WeightScale,
    pub line_height_tight: f32,
    pub line_height_normal: f32,
    pub line_height_relaxed: f32,
}

impl Default for Typography {
    fn default() -> Self {
        Self {
            sans_family: crate::text_render::UI_SANS_FAMILY.into(),
            serif_family: crate::text_render::UI_SERIF_FAMILY.into(),
            mono_family: crate::text_render::UI_MONOSPACE_FAMILY.into(),
            scale: TypeScale::default(),
            weights: WeightScale::default(),
            line_height_tight: 1.2,
            line_height_normal: 1.4,
            line_height_relaxed: 1.6,
        }
    }
}

impl Typography {
    /// Resolve a semantic role into the complete style consumed by both text
    /// measurement and painting.
    pub fn style(&self, role: TextRole) -> TextStyle {
        let sans = || FontChoice::Named(self.sans_family.clone());
        let serif = || FontChoice::Named(self.serif_family.clone());
        let mono = || FontChoice::Named(self.mono_family.clone());
        match role {
            TextRole::Caption => TextStyle::new(self.scale.xs)
                .with_line_height(self.line_height_normal)
                .with_family(sans()),
            TextRole::Label => TextStyle::new(self.scale.sm)
                .with_line_height(self.line_height_tight)
                .with_weight(TextWeight::Medium)
                .with_family(sans()),
            TextRole::Body => TextStyle::new(self.scale.base)
                .with_line_height(self.line_height_normal)
                .with_family(sans()),
            TextRole::Reading => TextStyle::new(self.scale.base)
                .with_line_height(self.line_height_relaxed)
                .with_family(serif()),
            TextRole::Title => TextStyle::new(self.scale.lg)
                .with_line_height(self.line_height_tight)
                .with_weight(TextWeight::Semibold)
                .with_family(sans()),
            TextRole::Heading => TextStyle::new(self.scale.xl)
                .with_line_height(self.line_height_tight)
                .with_weight(TextWeight::Bold)
                .with_family(sans()),
            TextRole::Display => TextStyle::new(self.scale.x3l)
                .with_line_height(self.line_height_tight)
                .with_weight(TextWeight::Bold)
                .with_family(sans()),
            TextRole::Code => TextStyle::new(self.scale.sm)
                .with_line_height(self.line_height_normal)
                .with_family(mono()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_roles_resolve_complete_styles() {
        let typography = Typography::default();
        let reading = typography.style(TextRole::Reading);
        assert_eq!(reading.size, typography.scale.base);
        assert_eq!(reading.line_height, typography.line_height_relaxed);
        assert_eq!(
            reading.family,
            FontChoice::Named(crate::text_render::UI_SERIF_FAMILY.into())
        );

        let code = typography.style(TextRole::Code);
        assert_eq!(
            code.family,
            FontChoice::Named(crate::text_render::UI_MONOSPACE_FAMILY.into())
        );
        assert_eq!(code.line_height, typography.line_height_normal);

        let heading = typography.style(TextRole::Heading);
        assert_eq!(heading.weight, TextWeight::Bold);
        assert_eq!(heading.line_height, typography.line_height_tight);
    }
}
use crate::text_render::{FontChoice, TextStyle, TextWeight};

/// Semantic type roles for application UI. Developers select intent rather
/// than manually coordinating size, leading, face, and weight at every call
/// site; [`Typography::style`] resolves the role to the active token set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextRole {
    Caption,
    Label,
    Body,
    Reading,
    Title,
    Heading,
    Display,
    Code,
}
