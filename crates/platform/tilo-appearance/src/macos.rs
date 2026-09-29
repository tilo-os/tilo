//! The accent colour and accessibility display options macOS reports on
//! request. Light or dark comes from the window system.

use objc2::msg_send;
use objc2_app_kit::{NSColor, NSColorSpace, NSWorkspace};
use objc2_foundation::{NSString, NSUserDefaults};

use crate::Appearance;

#[derive(Default)]
pub(crate) struct Reading(Appearance);

pub(crate) fn follow(_changed: impl Fn() + Send + 'static) -> Reading {
    read()
}

pub(crate) fn current(reading: &Reading) -> Appearance {
    reading.0
}

pub(crate) fn read() -> Reading {
    let workspace = NSWorkspace::sharedWorkspace();
    Reading(Appearance {
        scheme: None,
        accent: accent(),
        high_contrast: workspace.accessibilityDisplayShouldIncreaseContrast(),
        reduce_motion: workspace.accessibilityDisplayShouldReduceMotion(),
    })
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
