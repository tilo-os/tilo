#[derive(Clone, Debug)]
pub struct TypeScale { pub xs: f32, pub sm: f32, pub base: f32, pub lg: f32, pub xl: f32, pub x2l: f32, pub x3l: f32, pub x4l: f32 }

impl Default for TypeScale { fn default() -> Self { Self { xs: 10.0, sm: 12.0, base: 14.0, lg: 16.0, xl: 20.0, x2l: 24.0, x3l: 32.0, x4l: 40.0 } } }

#[derive(Clone, Debug)]
pub struct WeightScale { pub regular: u16, pub medium: u16, pub semibold: u16, pub bold: u16 }

impl Default for WeightScale { fn default() -> Self { Self { regular: 400, medium: 500, semibold: 600, bold: 700 } } }

/// A named font family, wrapped so consumers can extend with fallbacks/weights later
/// without breaking token consumers.
#[derive(Clone, Debug)]
pub struct FontFamily {
    pub family: String,
}

impl FontFamily {
    pub fn new(family: impl Into<String>) -> Self { Self { family: family.into() } }
}

#[derive(Clone, Debug)]
pub struct Typography {
    pub sans_family: String,
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
            sans_family: "Inter".into(),
            mono_family: "JetBrains Mono".into(),
            scale: TypeScale::default(),
            weights: WeightScale::default(),
            line_height_tight: 1.2,
            line_height_normal: 1.4,
            line_height_relaxed: 1.6,
        }
    }
}
