//! The window sizes, display scales and text scales every view is laid out at.

use tilo_ui_base::interaction::InteractionEnvironment;

/// A window size people use, in logical pixels, and whether it is held in the hand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub width: u32,
    pub height: u32,
    pub touch: bool,
}

/// Phones, a tablet, a half-width window on a portrait
/// monitor, a laptop, a desktop monitor and an ultrawide
/// one.
pub const SIZES: [Size; 7] = [
    Size {
        width: 320,
        height: 568,
        touch: true,
    },
    Size {
        width: 390,
        height: 844,
        touch: true,
    },
    Size {
        width: 768,
        height: 1024,
        touch: true,
    },
    Size {
        width: 960,
        height: 1080,
        touch: false,
    },
    Size {
        width: 1280,
        height: 800,
        touch: false,
    },
    Size {
        width: 1920,
        height: 1080,
        touch: false,
    },
    Size {
        width: 3440,
        height: 1440,
        touch: false,
    },
];

/// Display scales, a fractional one among them: layout is in logical pixels, so these change only
/// where edges snap to device pixels and how input is scaled.
pub const DISPLAY_SCALES: [f64; 3] = [1.0, 1.5, 2.0];

pub const TEXT_SCALES: [f32; 2] = [1.0, 1.5];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub size: Size,
    pub display_scale: f64,
    pub text_scale: f32,
}

impl std::fmt::Display for Layout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}×{} @{}x text {}",
            self.size.width, self.size.height, self.display_scale, self.text_scale
        )
    }
}

impl Layout {
    #[must_use]
    pub fn theme(&self) -> tilo_ui_base::Theme {
        tilo_ui_base::Theme::light().with_text_scale(self.text_scale)
    }

    #[must_use]
    pub fn input(&self) -> InteractionEnvironment {
        if self.size.touch {
            InteractionEnvironment::TOUCH
        } else {
            InteractionEnvironment::POINTER
        }
    }
}

#[must_use]
pub fn layouts() -> Vec<Layout> {
    let mut out = Vec::new();
    for size in SIZES {
        for display_scale in DISPLAY_SCALES {
            for text_scale in TEXT_SCALES {
                out.push(Layout {
                    size,
                    display_scale,
                    text_scale,
                });
            }
        }
    }
    out
}
