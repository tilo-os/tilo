//! What a session's own surfaces — its panels, sheets and cards — are, on every host: where one
//! stands, what it shows a host that draws it, and what reaches it as a person types and points.
//!
//! A host draws the surfaces and hands them their input; the roles above the seam only speak the
//! vocabulary here, so the same role runs on the Linux desktop's compositor and on the page.

mod host;

pub use cursor_icon::CursorIcon;
pub use host::{
    Changed, FrameWatch, HostDrag, HostFrame, HostIme, HostLayer, HostTextInput, SurfaceHost,
};

/// The rectangle a layer is shown on, in the host's logical layout, and the
/// scale it is drawn at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// Top-left corner, logical pixels.
    pub origin: (i32, i32),
    /// Size, logical pixels.
    pub size: (u32, u32),
    /// Physical pixels per logical pixel.
    pub scale: f64,
}

impl Placement {
    /// The rectangle in physical pixels: each side scaled, and at least one
    /// pixel wide.
    #[must_use]
    pub fn physical_size(self) -> (u32, u32) {
        let scale = |side: u32| {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a surface side on a screen, scaled by a small positive factor"
            )]
            let physical = (f64::from(side) * self.scale).round().max(1.0) as u32;
            physical
        };
        (scale(self.size.0), scale(self.size.1))
    }
}

/// A whole-pixel rectangle of a surface: the part one draw changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    /// Its left edge, in pixels.
    pub x: u32,
    /// Its top edge, in pixels.
    pub y: u32,
    /// How wide it is, in pixels.
    pub width: u32,
    /// How tall it is, in pixels.
    pub height: u32,
}

impl Region {
    /// The rectangle at `(x, y)` of `width` by `height` pixels.
    #[must_use]
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

/// A window a surface pictures: its live buffer, scaled to fit `rect` (centred, its aspect kept),
/// clipped to `clip`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowPicture {
    /// The window, by its id in the desk.
    pub window: u64,
    /// Where the picture rests, logical `(x, y, width, height)`.
    pub rect: (f64, f64, f64, f64),
    /// Its opacity, 0 to 1.
    pub opacity: f32,
    /// What it may cover, logical `(x, y, width, height)`; all of the output when `None`.
    pub clip: Option<(f64, f64, f64, f64)>,
}

/// A region a surface draws its material in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BackdropRequest {
    /// The region, logical `(x, y, width, height)`.
    pub rect: (f64, f64, f64, f64),
    /// The size of the image it takes, in texels.
    pub texels: (u32, u32),
    /// The blur's standard deviation, in texels.
    pub blur: f64,
}

/// What lies below a [`BackdropRequest`]'s region, blurred: rows of premultiplied R, G, B, A at its
/// texel size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackdropImage {
    /// The blurred pixels, `texels.0 * texels.1 * 4` bytes.
    pub rgba: Vec<u8>,
    /// How many texels wide and tall.
    pub texels: (u32, u32),
}

/// An output as a surface is told of it: its logical rectangle and the scale it draws at. The two
/// travel together, so a surface placing itself on an output cannot take the rectangle without the
/// scale it is to draw at — which would leave it stretched.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HostedOutput {
    /// Its logical rectangle `(x, y, w, h)`.
    pub rect: (i32, i32, i32, i32),
    /// Physical pixels per logical pixel it draws at.
    pub scale: f64,
}

impl HostedOutput {
    /// The scale of the output holding `point` in the logical layout, else of the first output
    /// there is (and 1 when there is none): the scale a surface covering that point draws at.
    #[must_use]
    pub fn scale_at(outputs: &[Self], point: (f64, f64)) -> f64 {
        let holds = |output: &&Self| {
            let (x, y, w, h) = output.rect;
            point.0 >= f64::from(x)
                && point.1 >= f64::from(y)
                && point.0 < f64::from(x.saturating_add(w))
                && point.1 < f64::from(y.saturating_add(h))
        };
        outputs
            .iter()
            .find(holds)
            .or_else(|| outputs.first())
            .map_or(1.0, |output| output.scale)
    }
}

/// A key as a surface receives it: the host's keymap already applied, so surfaces need no keyboard
/// state of their own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKey {
    /// The evdev code.
    pub keycode: u32,
    /// Pressed or released.
    pub pressed: bool,
    /// A press repeated while the key is held, not a new one.
    pub repeat: bool,
    /// The keysym with the active modifiers and layout applied.
    pub keysym: u32,
    /// The keysym of the key's first level (no Shift), for shortcuts.
    pub unmodified_keysym: u32,
    /// The keysym another configured layout puts on this key when the active one has none in Latin:
    /// a shortcut is named by it, so Ctrl+C copies while a Cyrillic, Greek or Hebrew layout types.
    pub latin_keysym: u32,
    /// The text the key types, if it types any: the character the layout gives it, or what a dead
    /// key's or the Compose key's sequence finishes with, which is a character of its own or
    /// several.
    pub text: Option<String>,
    /// The modifiers held as the key changed.
    pub modifiers: KeyModifiers,
}

/// The modifiers held alongside a [`HostKey`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools, reason = "one flag per modifier key")]
pub struct KeyModifiers {
    /// Control.
    pub ctrl: bool,
    /// Alt.
    pub alt: bool,
    /// Shift.
    pub shift: bool,
    /// Logo (the desktop key).
    pub logo: bool,
    /// Caps lock.
    pub caps_lock: bool,
}

impl KeyModifiers {
    /// These modifiers as the toolkit reports them to a surface it hosts.
    #[must_use]
    pub fn state(self) -> ui::winit::keyboard::ModifiersState {
        let mut state = ui::winit::keyboard::ModifiersState::empty();
        state.set(ui::winit::keyboard::ModifiersState::SHIFT, self.shift);
        state.set(ui::winit::keyboard::ModifiersState::CONTROL, self.ctrl);
        state.set(ui::winit::keyboard::ModifiersState::ALT, self.alt);
        state.set(ui::winit::keyboard::ModifiersState::SUPER, self.logo);
        state
    }
}

/// What a surface answers to a press it is offered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeyClaim {
    /// Not this surface's: the press goes on to whoever has the keyboard.
    #[default]
    Pass,
    /// The surface takes the press and its release.
    Take,
    /// The surface takes the press, repeats while it is held, and its release (volume up held down
    /// ramps the volume).
    TakeRepeating,
}

/// How a layer surface is drawn this frame instead of as it stands: the panel of another process
/// coming and going, which only the session, who places it, can pose.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayerPose {
    /// Scaled by this much about its [`Self::anchor`] (1: as it is).
    pub scale: f64,
    /// Its opacity, 0 (invisible) to 1.
    pub opacity: f32,
    /// The point it grows from and shrinks back into, as a fraction of its own rectangle:
    /// `(0.0, 0.0)` its top-left corner, `(1.0, 0.0)` its top-right, `(0.5, 0.5)` its middle. The
    /// corner its slot anchors it by.
    pub anchor: (f64, f64),
    /// Where it is drawn instead of where it stands, in logical pixels: how far it is still toward
    /// the anchor it comes from or goes back to.
    pub offset: (f64, f64),
}

impl LayerPose {
    /// Drawn as it stands.
    pub const SETTLED: Self = Self {
        scale: 1.0,
        opacity: 1.0,
        anchor: (0.0, 0.0),
        offset: (0.0, 0.0),
    };

    /// Whether it is drawn as it stands: nothing of a pose is applied.
    #[must_use]
    pub fn settled(&self) -> bool {
        self.scale >= 1.0 && self.opacity >= 1.0 && self.offset == (0.0, 0.0)
    }
}

impl Default for LayerPose {
    fn default() -> Self {
        Self::SETTLED
    }
}
