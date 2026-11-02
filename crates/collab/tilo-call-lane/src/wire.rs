//! The codes a call's pictures and their layers travel as: what a lane's
//! control messages name a source and a layer with, and the stream a source's
//! frames belong to.
//!
//! A page speaks the same codes as a device does, so they live here rather
//! than with the lane's pictures, and a page that carries neither iroh nor an
//! encoder still reads them.

use crate::picture::{Layer, Source};

use crate::frame::Stream;

/// The byte a control message names a source with.
#[must_use]
pub const fn source_wire(source: Source) -> u8 {
    match source {
        Source::Camera => 0,
        Source::Screen => 1,
    }
}

/// The source a control message names, or `None` for any other byte.
#[must_use]
pub const fn source_of_wire(byte: u8) -> Option<Source> {
    match byte {
        0 => Some(Source::Camera),
        1 => Some(Source::Screen),
        _ => None,
    }
}

/// The byte a control message names a layer with.
#[must_use]
pub const fn layer_wire(layer: Layer) -> u8 {
    match layer {
        Layer::Low => 0,
        Layer::Mid => 1,
        Layer::High => 2,
    }
}

/// The layer a control message names, or `None` for any other byte.
#[must_use]
pub const fn layer_of_wire(byte: u8) -> Option<Layer> {
    match byte {
        0 => Some(Layer::Low),
        1 => Some(Layer::Mid),
        2 => Some(Layer::High),
        _ => None,
    }
}

/// The stream of a call's frames a picture source travels on.
#[must_use]
pub const fn stream_of(source: Source) -> Stream {
    match source {
        Source::Camera => Stream::Camera,
        Source::Screen => Stream::Screen,
    }
}
