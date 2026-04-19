use vello::peniko::Color;

#[derive(Clone, Debug)]
pub struct ColorRamp { pub stops: [Color; 11] }

impl ColorRamp {
    pub fn len(&self) -> usize { self.stops.len() }
}

impl std::ops::Index<usize> for ColorRamp {
    type Output = Color;
    fn index(&self, i: usize) -> &Color { &self.stops[i] }
}

#[derive(Clone, Debug)]
pub struct Surfaces {
    pub base: Color,
    pub raised: Color,
    pub overlay: Color,
    pub glass: Color,
    pub glass_blur_px: f32,
    /// White rounded-3xl card that frames each window tile in the concept
    /// (`bg-white` in the React mock). Lives on top of the dark shell bg.
    pub window_card: Color,
    /// Subtle border around the card (`border-white/20` in the concept).
    pub window_card_border: Color,
}

#[derive(Clone, Debug)]
pub struct SemanticColors { pub success: Color, pub warning: Color, pub danger: Color, pub info: Color }

#[derive(Clone, Debug)]
pub struct Colors { pub neutral: ColorRamp, pub accent: ColorRamp, pub semantic: SemanticColors, pub surfaces: Surfaces, pub text_primary: Color, pub text_dim: Color, pub border_standard: Color, pub border_focus: Color }

impl Colors {
    pub fn dark() -> Self {
        // Slate-950 → slate-50, one Tailwind step per ramp slot.
        Self {
            neutral: ColorRamp { stops: [
                Color::rgb8(  2,   6,  23), // slate-950
                Color::rgb8( 15,  23,  42), // slate-900
                Color::rgb8( 30,  41,  59), // slate-800
                Color::rgb8( 51,  65,  85), // slate-700
                Color::rgb8( 71,  85, 105), // slate-600
                Color::rgb8(100, 116, 139), // slate-500
                Color::rgb8(148, 163, 184), // slate-400
                Color::rgb8(203, 213, 225), // slate-300
                Color::rgb8(226, 232, 240), // slate-200
                Color::rgb8(241, 245, 249), // slate-100
                Color::rgb8(248, 250, 252), // slate-50
            ]},
            // Indigo-950 → indigo-50; index 5 is the concept's `indigo-500`
            // accent primary (#6366f1).
            accent: ColorRamp { stops: [
                Color::rgb8( 30,  27,  75), // indigo-950
                Color::rgb8( 49,  46, 129), // indigo-900
                Color::rgb8( 55,  48, 163), // indigo-800
                Color::rgb8( 67,  56, 202), // indigo-700
                Color::rgb8( 79,  70, 229), // indigo-600
                Color::rgb8( 99, 102, 241), // indigo-500 ← accent primary
                Color::rgb8(129, 140, 248), // indigo-400
                Color::rgb8(165, 180, 252), // indigo-300
                Color::rgb8(199, 210, 254), // indigo-200
                Color::rgb8(224, 231, 255), // indigo-100
                Color::rgb8(238, 242, 255), // indigo-50
            ]},
            semantic: SemanticColors {
                success: Color::rgb8( 16, 185, 129), // emerald-500
                warning: Color::rgb8(245, 158,  11), // amber-500
                danger:  Color::rgb8(239,  68,  68), // red-500
                info:    Color::rgb8( 96, 165, 250), // blue-400
            },
            surfaces: Surfaces {
                base:   Color::rgb8(15, 23, 42),       // slate-900
                raised: Color::rgb8(30, 41, 59),       // slate-800
                overlay: Color::rgba8(0, 0, 0, 205),   // black/80 (top bar backdrop)
                glass:   Color::rgba8(15, 23, 42, 180),// slate-900/70 (overlay panels)
                glass_blur_px: 24.0,
                window_card: Color::WHITE,
                window_card_border: Color::rgba8(255, 255, 255, 51), // white/20
            },
            text_primary: Color::WHITE,
            text_dim: Color::rgb8(148, 163, 184), // slate-400
            border_standard: Color::rgb8(51, 65, 85), // slate-700
            border_focus: Color::rgb8(99, 102, 241),  // indigo-500
        }
    }

    pub fn light() -> Self {
        Self {
            neutral: ColorRamp { stops: [
                Color::rgb8(255, 255, 255), Color::rgb8(245, 245, 248), Color::rgb8(235, 235, 240),
                Color::rgb8(220, 220, 225), Color::rgb8(200, 200, 205), Color::rgb8(160, 160, 165),
                Color::rgb8(120, 120, 125), Color::rgb8(90, 90, 95), Color::rgb8(60, 60, 65),
                Color::rgb8(30, 30, 35), Color::rgb8(10, 10, 12),
            ]},
            accent: ColorRamp { stops: [
                Color::rgb8(240, 250, 255), Color::rgb8(220, 240, 255), Color::rgb8(180, 220, 250),
                Color::rgb8(130, 200, 250), Color::rgb8(80, 170, 240), Color::rgb8(40, 150, 230),
                Color::rgb8(0, 120, 215), Color::rgb8(0, 100, 200), Color::rgb8(0, 75, 170),
                Color::rgb8(0, 50, 120), Color::rgb8(0, 25, 60),
            ]},
            semantic: SemanticColors {
                success: Color::rgb8(0, 150, 70),
                warning: Color::rgb8(200, 130, 0),
                danger: Color::rgb8(200, 50, 50),
                info: Color::rgb8(0, 120, 200),
            },
            surfaces: Surfaces {
                base: Color::rgb8(240, 240, 245),
                raised: Color::WHITE,
                overlay: Color::rgba8(255, 255, 255, 230),
                glass: Color::rgba8(245, 245, 250, 180),
                glass_blur_px: 24.0,
                window_card: Color::WHITE,
                window_card_border: Color::rgba8(0, 0, 0, 30),
            },
            text_primary: Color::BLACK,
            text_dim: Color::rgb8(100, 100, 105),
            border_standard: Color::rgb8(200, 200, 205),
            border_focus: Color::rgb8(0, 120, 215),
        }
    }
}
