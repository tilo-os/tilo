//! What a session's own surfaces — its panels, sheets and cards — are, on every shell: where one
//! stands, what it shows the shell that draws it, and what reaches it as a person types and points.
//!
//! The shell draws the surfaces and hands them their input; the roles above the boundary only speak
//! the vocabulary here, so the same role runs on the Linux desktop's compositor and on the page.

pub mod drawing;

/// Run `deliver` as what the person's press of `serial` on the system's own surfaces does: a
/// call the person's shell makes for them within it names the press, which the compositor's
/// record of it proves to the device's daemon ([`tilo_actions::remote::press_points::handling`]).
pub fn handling_press<R>(serial: Option<u32>, deliver: impl FnOnce() -> R) -> R {
    tilo_actions::remote::press_points::handling(serial, deliver)
}

/// The person's press the system's own surfaces act on now ([`handling_press`]), if any.
#[must_use]
pub fn handled_press() -> Option<u32> {
    tilo_actions::remote::press_points::handled()
}
mod shell;

pub use cursor_icon::CursorIcon;
pub use shell::{
    CELL, Changed, FrameWatch, ShellSurface, SurfaceDrag, SurfaceFrame, SurfaceIme, SurfaceLayer,
    SurfaceTextInput,
};
#[cfg(unix)]
pub use shell::{SurfaceBuffer, SurfaceBuffers};

/// The rectangle a layer is shown on, in the shell's logical layout, and the
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
    /// The window, by its id in the layout.
    pub window: u64,
    /// Where the picture rests, logical `(x, y, width, height)`.
    pub rect: (f64, f64, f64, f64),
    /// Its opacity, 0 to 1.
    pub opacity: f32,
    /// What it may cover, logical `(x, y, width, height)`; all of the output when `None`.
    pub clip: Option<(f64, f64, f64, f64)>,
}

/// What a surface below the windows draws over them for one window: a floating window's card, bar
/// and body, opaque inside its rounded corners. The windows behind are drawn around the card and
/// under the shadows it casts.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowCover {
    /// The window, by its id in the layout.
    pub window: u64,
    /// The card, logical `(x, y, width, height)` in the layout.
    pub rect: (i32, i32, i32, i32),
    /// The shadows it casts on what lies under it, each a frame round the card, bottom first.
    pub shadows: Vec<WindowFrame>,
}

/// A drop shadow: the outline of the window, blurred, offset down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shadow {
    /// How far below the window it sits, logical pixels.
    pub offset_y: f64,
    /// Its blur radius, logical pixels (a Gaussian of half that as its standard deviation, as CSS
    /// and the design tokens mean it).
    pub blur: f64,
    /// Its colour, straight alpha.
    pub rgba: [u8; 4],
}

/// The hairline that marks where a window ends, drawn just outside it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Edge {
    /// Its width, logical pixels.
    pub width: f64,
    /// Its colour, straight alpha.
    pub rgba: [u8; 4],
}

/// How every window is drawn on the desktop.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowFrame {
    /// Corner radius, logical pixels; zero for square corners.
    pub radius: f64,
    /// The shadow under the window, if it casts one.
    pub shadow: Option<Shadow>,
    /// The hairline around the window, if it has one.
    pub edge: Option<Edge>,
}

impl WindowFrame {
    /// No corners, no shadow, no edge: a window drawn as its buffer is (a window that fills the
    /// display, which the display can scan out).
    #[must_use]
    pub const fn square() -> Self {
        Self::corners(0.0)
    }

    /// A window whose shell paints it whole — its card, the lift under it and the edge around it, in
    /// the shell's own colours: the compositor only shapes its corners to the shell's card and draws
    /// nothing around it. The shell's tiles are drawn this way.
    #[must_use]
    pub const fn corners(radius: f64) -> Self {
        Self {
            radius,
            shadow: None,
            edge: None,
        }
    }

    /// Whether this draws nothing around a window.
    #[must_use]
    pub fn is_square(&self) -> bool {
        self.radius <= 0.0
            && self.shadow.is_none_or(|shadow| shadow.rgba[3] == 0)
            && self
                .edge
                .is_none_or(|edge| edge.rgba[3] == 0 || edge.width <= 0.0)
    }
}

impl Default for WindowFrame {
    /// The window tile the design language draws: what every app had around it before it was a
    /// foreign window.
    fn default() -> Self {
        Self {
            radius: f64::from(tilo_ui_base::tokens::Radii::WINDOW),
            shadow: Some(Shadow {
                offset_y: 2.0,
                blur: 16.0,
                rgba: [0, 0, 0, 71],
            }),
            edge: Some(Edge {
                width: 1.0,
                rgba: [255, 255, 255, 36],
            }),
        }
    }
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
pub struct SurfaceOutput {
    /// Its logical rectangle `(x, y, w, h)`.
    pub rect: (i32, i32, i32, i32),
    /// Physical pixels per logical pixel it draws at.
    pub scale: f64,
}

impl SurfaceOutput {
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

/// A scroll, `(horizontal, vertical)`, counted down and right as positive as the Wayland axis
/// counts it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Scroll {
    /// Fingers on a touchpad, or another continuous device, moved this many logical pixels.
    Pixels(f64, f64),
    /// A wheel turned this many notches, fractions of one for a high-resolution wheel. Clients
    /// scroll a line of their own per notch.
    Notches(f64, f64),
    /// The fingers scrolling a touchpad lifted: what they scrolled may glide on from here, at the
    /// speed they let go at.
    Lifted,
}

impl Scroll {
    /// Logical pixels a notch stands for where a reader takes only pixels.
    pub const NOTCH_PIXELS: f64 = 15.0;

    /// The scroll in logical pixels.
    #[must_use]
    pub fn pixels(self) -> (f64, f64) {
        match self {
            Self::Pixels(x, y) => (x, y),
            Self::Notches(x, y) => (x * Self::NOTCH_PIXELS, y * Self::NOTCH_PIXELS),
            Self::Lifted => (0.0, 0.0),
        }
    }

    /// The scroll `factor` times as far.
    #[must_use]
    pub fn scaled(self, factor: f64) -> Self {
        match self {
            Self::Pixels(x, y) => Self::Pixels(x * factor, y * factor),
            Self::Notches(x, y) => Self::Notches(x * factor, y * factor),
            Self::Lifted => Self::Lifted,
        }
    }
}

/// A key as a surface receives it: the shell's keymap already applied, so surfaces need no keyboard
/// state of their own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceKey {
    /// The evdev code.
    pub keycode: u32,
    /// When the key changed at the device. What a surface shows is not the person's answer to a
    /// press that began before it appeared: the loop may route that press after the surface is up.
    pub at: std::time::Instant,
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

/// The modifiers held alongside a [`SurfaceKey`].
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

    /// Where the rectangle `(x, y, width, height)` is drawn in this pose: scaled about its anchor
    /// and moved by its offset.
    #[must_use]
    pub fn rect(&self, (x, y, width, height): (f64, f64, f64, f64)) -> (f64, f64, f64, f64) {
        let grown = 1.0 - self.scale;
        (
            x + width * self.anchor.0 * grown + self.offset.0,
            y + height * self.anchor.1 * grown + self.offset.1,
            width * self.scale,
            height * self.scale,
        )
    }
}

impl Default for LayerPose {
    fn default() -> Self {
        Self::SETTLED
    }
}
