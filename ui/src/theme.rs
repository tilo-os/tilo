use vello::peniko::Color;

#[derive(Clone, Debug)]
pub struct Theme {
    pub name: String,
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

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            name: "Dark".into(),
            bg_app: Color::rgb8(20, 20, 25),
            bg_primary: Color::rgb8(45, 45, 50),
            bg_hover: Color::rgb8(55, 55, 60),
            bg_active: Color::rgb8(35, 35, 40),
            border_standard: Color::rgb8(80, 80, 85),
            border_focus: Color::rgb8(0, 120, 215),
            text_primary: Color::WHITE,
            text_dim: Color::rgb8(200, 200, 200),
            accent_primary: Color::rgb8(0, 120, 215),
            status_success: Color::rgb8(0, 180, 80),
            radius_standard: 4.0,
            padding_standard: 10.0,
            gap_standard: 10.0,
        }
    }

    pub fn light() -> Self {
        Self {
            name: "Light".into(),
            bg_app: Color::rgb8(240, 240, 245),
            bg_primary: Color::rgb8(255, 255, 255),
            bg_hover: Color::rgb8(230, 230, 235),
            bg_active: Color::rgb8(210, 210, 215),
            border_standard: Color::rgb8(200, 200, 205),
            border_focus: Color::rgb8(0, 120, 215),
            text_primary: Color::BLACK,
            text_dim: Color::rgb8(100, 100, 105),
            accent_primary: Color::rgb8(0, 120, 215),
            status_success: Color::rgb8(0, 150, 70),
            radius_standard: 4.0,
            padding_standard: 10.0,
            gap_standard: 10.0,
        }
    }

    pub fn dracula() -> Self {
        Self {
            name: "Dracula".into(),
            bg_app: Color::rgb8(40, 42, 54),        // #282a36
            bg_primary: Color::rgb8(68, 71, 90),    // #44475a
            bg_hover: Color::rgb8(98, 114, 164),    // #6272a4
            bg_active: Color::rgb8(40, 42, 54),
            border_standard: Color::rgb8(68, 71, 90),
            border_focus: Color::rgb8(189, 147, 249), // Purple
            text_primary: Color::rgb8(248, 248, 242), // Foreground
            text_dim: Color::rgb8(98, 114, 164),      // Comment
            accent_primary: Color::rgb8(255, 121, 198), // Pink
            status_success: Color::rgb8(80, 250, 123),  // Green
            radius_standard: 4.0,
            padding_standard: 10.0,
            gap_standard: 10.0,
        }
    }

    pub fn nord() -> Self {
        Self {
            name: "Nord".into(),
            bg_app: Color::rgb8(46, 52, 64),        // nord0
            bg_primary: Color::rgb8(59, 66, 82),    // nord1
            bg_hover: Color::rgb8(67, 76, 94),      // nord2
            bg_active: Color::rgb8(46, 52, 64),
            border_standard: Color::rgb8(76, 86, 106), // nord3
            border_focus: Color::rgb8(136, 192, 208),  // nord8
            text_primary: Color::rgb8(216, 222, 233), // nord4
            text_dim: Color::rgb8(76, 86, 106),       // nord3
            accent_primary: Color::rgb8(129, 161, 193), // nord9
            status_success: Color::rgb8(163, 190, 140), // nord14 (Green)
            radius_standard: 4.0,
            padding_standard: 10.0,
            gap_standard: 10.0,
        }
    }

    pub fn gruvbox() -> Self {
        Self {
            name: "Gruvbox".into(),
            bg_app: Color::rgb8(40, 40, 40),        // #282828
            bg_primary: Color::rgb8(60, 56, 54),    // bg1
            bg_hover: Color::rgb8(80, 73, 69),      // bg2
            bg_active: Color::rgb8(40, 40, 40),
            border_standard: Color::rgb8(146, 131, 116), // Gray
            border_focus: Color::rgb8(254, 128, 25),     // Bright Orange
            text_primary: Color::rgb8(235, 219, 178),    // fg
            text_dim: Color::rgb8(168, 153, 132),        // Bright Gray
            accent_primary: Color::rgb8(184, 187, 38),   // Bright Green
            status_success: Color::rgb8(152, 151, 26),   // Normal Green
            radius_standard: 2.0,
            padding_standard: 10.0,
            gap_standard: 10.0,
        }
    }

    pub fn solarized() -> Self {
        Self {
            name: "Solarized".into(),
            bg_app: Color::rgb8(0, 43, 54),         // base03
            bg_primary: Color::rgb8(7, 54, 66),     // base02
            bg_hover: Color::rgb8(88, 110, 117),    // base01
            bg_active: Color::rgb8(0, 43, 54),
            border_standard: Color::rgb8(88, 110, 117), // base01
            border_focus: Color::rgb8(38, 139, 210),    // Blue
            text_primary: Color::rgb8(131, 148, 150),   // base0
            text_dim: Color::rgb8(101, 123, 131),       // base00
            accent_primary: Color::rgb8(42, 161, 152),  // Cyan
            status_success: Color::rgb8(133, 153, 0),   // Green
            radius_standard: 0.0,
            padding_standard: 10.0,
            gap_standard: 10.0,
        }
    }
}
