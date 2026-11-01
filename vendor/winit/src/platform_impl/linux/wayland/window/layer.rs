//! Where a layer surface sits and what it asks the compositor for.

use bitflags::bitflags;
use sctk::shell::wlr_layer;

use crate::dpi::LogicalSize;

/// The layer a surface is placed on. Higher layers are drawn over the
/// windows, and over the lower layers.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Layer {
    /// Below everything, including the windows.
    Background,
    /// Above the background, below the windows.
    Bottom,
    /// Above the windows.
    #[default]
    Top,
    /// Above every other layer.
    Overlay,
}

// The edges (and so corners) a surface is anchored to on its output. An
// axis with no anchor is centered, and an anchored axis the surface does not
// stretch along keeps its own size.
bitflags! {
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct Anchor: u32 {
        const TOP = 1;
        const BOTTOM = 2;
        const LEFT = 4;
        const RIGHT = 8;
    }
}

/// How a surface takes the keyboard: not at all, when the pointer is over
/// it, or all of it.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum KeyboardInteractivity {
    /// The surface never takes keyboard focus (a bar, a desk).
    #[default]
    None,
    /// The compositor gives it focus when the pointer is over it.
    OnDemand,
    /// The surface takes the keyboard until it is unmapped (a lock screen,
    /// a modal dialog).
    Exclusive,
}

/// A window that places itself on a layer of an output instead of being a
/// toplevel the desktop places.
#[derive(Clone, Debug)]
pub struct LayerSurfaceAttributes {
    /// The layer to sit on.
    pub layer: Layer,
    /// Names the surface to the compositor, for its own logs and rules
    /// (there is no app id on a layer surface).
    pub namespace: String,
    /// The edges the surface is anchored to.
    pub anchor: Anchor,
    /// The size the surface asks for. On an anchored axis, zero stretches
    /// it to that edge; with no anchor on an axis the size is the
    /// surface's own.
    pub size: LogicalSize<u32>,
    /// How much of the anchored edge the surface reserves so the desktop
    /// keeps windows out of it. Zero means none.
    pub exclusive_zone: i32,
    /// Space between the surface and what it is anchored to: top, right,
    /// bottom, left.
    pub margin: (i32, i32, i32, i32),
    /// Whether it takes the keyboard.
    pub keyboard_interactivity: KeyboardInteractivity,
}

impl Default for LayerSurfaceAttributes {
    fn default() -> Self {
        Self {
            layer: Layer::default(),
            namespace: String::from("winit"),
            anchor: Anchor::empty(),
            size: LogicalSize::new(0, 0),
            exclusive_zone: 0,
            margin: (0, 0, 0, 0),
            keyboard_interactivity: KeyboardInteractivity::default(),
        }
    }
}

impl From<Layer> for wlr_layer::Layer {
    fn from(layer: Layer) -> Self {
        match layer {
            Layer::Background => Self::Background,
            Layer::Bottom => Self::Bottom,
            Layer::Top => Self::Top,
            Layer::Overlay => Self::Overlay,
        }
    }
}

impl From<Anchor> for wlr_layer::Anchor {
    fn from(anchor: Anchor) -> Self {
        Self::from_bits_truncate(anchor.bits())
    }
}

impl From<KeyboardInteractivity> for wlr_layer::KeyboardInteractivity {
    fn from(keyboard: KeyboardInteractivity) -> Self {
        match keyboard {
            KeyboardInteractivity::None => Self::None,
            KeyboardInteractivity::OnDemand => Self::OnDemand,
            KeyboardInteractivity::Exclusive => Self::Exclusive,
        }
    }
}
