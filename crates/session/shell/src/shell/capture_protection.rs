//! Capture protection: what leaves the local display as a placeholder.
//!
//! The local display always shows everything. Every path that copies the
//! desktop's pixels somewhere else — screenshots, recordings, the recent
//! capture buffer, live mirrors, the Screenshot and `ScreenCast` portals (which
//! `RemoteDesktop` streams through), projection to another display, the
//! per-window collaboration stream and the control-socket snapshot — draws
//! protected content as the placeholder painted here instead.
//!
//! Protected content is:
//!
//! - every surface of an app known to hold secrets ([`holds_secrets`]),
//!   and of an app that asked for protection over its
//!   authenticated shell-ui connection (password managers, authenticator
//!   apps), recorded per surface in [`ForeignFrameStore`], plus the popups
//!   those windows open;
//! - the on-screen keyboard and the input method's candidate popups while
//!   the focused field declares a password, a PIN or hidden text, marked per
//!   surface by the compositor from the field's content type;
//! - the shell's own secret prompts — administrator authentication, network
//!   secrets, a shortcut step's secret answer, the Wi-Fi passphrase, an
//!   `AirPlay` PIN or password, a Bluetooth pairing dialog, and the setup
//!   steps that restore or back up the identity (passphrases and a nearby
//!   device's pairing code) — regardless of any request;
//! - every window of a Tilo app while it shows a password field holding a
//!   value, masked or shown, which the app requests on its own;
//! - the whole desktop while the session is locked.
//!
//! The placeholder carries no text: a themed surface with the shield mark the
//! shell already uses for "protected", so a viewer sees that something is
//! withheld without learning what.
//!
//! [`ForeignFrameStore`]: crate::shell::foreign::ForeignFrameStore

use std::cell::Cell;

use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect};
use vello::peniko::{Color, Fill};

/// Apps known to show secrets — password managers and authenticator codes.
/// Their windows are protected whether or not they ask, since most cannot:
/// only apps built on the Tilo toolkit speak the shell-ui request. Matched
/// against the Wayland app id, ignoring case.
const SECRET_HOLDING_APP_IDS: &[&str] = &[
    "so.tilo.passwords",
    "so.tilo.totp",
    "org.keepassxc.keepassxc",
    "com.bitwarden.desktop",
    "bitwarden",
    "1password",
    "org.gnome.world.secrets",
    "org.gnome.seahorse.application",
    "com.belmoussaoui.authenticator",
];

/// Whether windows of `app_id` are protected for what the app is, before
/// any request.
pub fn holds_secrets(app_id: &str) -> bool {
    SECRET_HOLDING_APP_IDS
        .iter()
        .any(|known| known.eq_ignore_ascii_case(app_id))
}

thread_local! {
    static CAPTURE_PAINT: Cell<bool> = const { Cell::new(false) };
}

/// Marks the paint running on this thread as one whose pixels leave the
/// local display. While it lives, protected surfaces draw the placeholder.
///
/// Only a paint made *for* a capture takes it — a pass whose scene is also
/// presented would blank the protected window locally too.
#[must_use = "protection applies only while the guard is alive"]
pub struct CapturePaint {
    previous: bool,
}

impl CapturePaint {
    pub fn begin() -> Self {
        Self {
            previous: CAPTURE_PAINT.with(|flag| flag.replace(true)),
        }
    }
}

impl Drop for CapturePaint {
    fn drop(&mut self) {
        let previous = self.previous;
        CAPTURE_PAINT.with(|flag| flag.set(previous));
    }
}

/// Whether the paint on this thread is being made for a capture.
pub fn painting_for_capture() -> bool {
    CAPTURE_PAINT.with(Cell::get)
}

/// The placeholder's ground. Always opaque: a translucent theme surface
/// would let the withheld pixels show through.
pub fn placeholder_fill(theme: &Theme) -> Color {
    theme.tokens.colors.surfaces.base.with_alpha(1.0)
}

pub fn placeholder_mark(theme: &Theme) -> Color {
    theme.tokens.colors.text_dim
}

/// Paint the placeholder over `rect`, clipped to `radius` like the window it
/// replaces.
pub fn paint_placeholder(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    radius: f64,
    theme: &Theme,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    scene.fill(
        Fill::NonZero,
        transform,
        placeholder_fill(theme),
        None,
        &RoundedRect::from_rect(rect, radius),
    );
    // The mark never grows past the largest spacing step, so a full-screen
    // placeholder stays calm; below a small step it is noise, and the fill
    // alone reads as withheld.
    let spacing = &theme.tokens.spacing;
    let side = (rect.width().min(rect.height()) / 4.0).min(f64::from(spacing.s12));
    if side < f64::from(spacing.s3) {
        return;
    }
    ui::icons::draw_icon(
        scene,
        transform,
        Rect::from_center_size(rect.center(), (side, side)),
        ui::icons::Icon::Shield,
        placeholder_mark(theme),
        ui::icons::LUCIDE_DEFAULT_STROKE_PX,
    );
}

/// A whole frame of placeholder, for paths that hand raw pixels to an
/// encoder rather than drawing a scene: straight RGBA, `width × height`.
pub fn placeholder_rgba(width: u32, height: u32, fill: [u8; 4]) -> Vec<u8> {
    let pixels =
        usize::try_from(u64::from(width).saturating_mul(u64::from(height))).unwrap_or(usize::MAX);
    fill.repeat(pixels)
}

/// [`placeholder_fill`] as the bytes [`placeholder_rgba`] repeats.
pub fn placeholder_fill_rgba8(theme: &Theme) -> [u8; 4] {
    placeholder_fill(theme).to_rgba8().to_u8_array()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_capture_paint_flag_is_scoped_and_nests() {
        assert!(!painting_for_capture());
        {
            let _outer = CapturePaint::begin();
            assert!(painting_for_capture());
            {
                let _inner = CapturePaint::begin();
                assert!(painting_for_capture());
            }
            assert!(
                painting_for_capture(),
                "an inner capture paint must not end the outer one"
            );
        }
        assert!(!painting_for_capture());
    }

    #[test]
    fn a_raw_placeholder_frame_is_the_fill_at_every_pixel() {
        let fill = placeholder_fill_rgba8(&Theme::dark());
        let frame = placeholder_rgba(3, 2, fill);
        assert_eq!(frame.len(), 3 * 2 * 4);
        assert!(frame.chunks_exact(4).all(|pixel| pixel == fill));
    }

    #[test]
    fn the_placeholder_takes_its_colours_from_the_theme() {
        for theme in [Theme::dark(), Theme::light()] {
            assert_eq!(
                placeholder_fill(&theme),
                theme.tokens.colors.surfaces.base.with_alpha(1.0)
            );
            assert_eq!(placeholder_mark(&theme), theme.tokens.colors.text_dim);
        }
    }
}
