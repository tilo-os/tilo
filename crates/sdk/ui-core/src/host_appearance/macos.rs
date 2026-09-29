//! The accent colour and accessibility display options macOS reports on
//! request. They are read at start and again whenever the window regains
//! focus or the system appearance changes — the moments right after a
//! visit to System Settings.

use objc2::msg_send;
use objc2_app_kit::{NSColor, NSColorSpace, NSWorkspace};
use objc2_foundation::{NSString, NSUserDefaults};

use super::HostAppearance;

#[derive(Default)]
pub(super) struct Platform {
    reading: HostAppearance,
}

impl Platform {
    pub(super) fn start() -> Self {
        Self { reading: read() }
    }

    pub(super) fn refresh(&mut self) {
        self.reading = read();
    }

    pub(super) fn current(&self) -> HostAppearance {
        self.reading
    }
}

fn read() -> HostAppearance {
    let workspace = NSWorkspace::sharedWorkspace();
    HostAppearance {
        scheme: None,
        accent: accent(),
        high_contrast: workspace.accessibilityDisplayShouldIncreaseContrast(),
        reduce_motion: workspace.accessibilityDisplayShouldReduceMotion(),
    }
}

/// The user's chosen accent, or `None` for "Multicolor" — the default,
/// which leaves each app its own accent and stores no `AppleAccentColor`.
fn accent() -> Option<[u8; 3]> {
    let key = NSString::from_str("AppleAccentColor");
    NSUserDefaults::standardUserDefaults().objectForKey(&key)?;
    let color =
        NSColor::controlAccentColor().colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())?;
    // SAFETY: `color` is in an RGB colour space, where these accessors are
    // defined; `CGFloat` is `f64` on every macOS target.
    let (red, green, blue): (f64, f64, f64) = unsafe {
        (
            msg_send![&color, redComponent],
            msg_send![&color, greenComponent],
            msg_send![&color, blueComponent],
        )
    };
    Some([red, green, blue].map(channel))
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 0.0..=255.0 before the cast"
)]
fn channel(component: f64) -> u8 {
    (component * 255.0).round().clamp(0.0, 255.0) as u8
}
