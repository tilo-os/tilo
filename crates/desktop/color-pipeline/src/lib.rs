#![cfg_attr(
    test,
    allow(
        clippy::float_cmp,
        reason = "in a test an exact comparison is the assertion, not a hazard"
    )
)]
//! The compositor's colour maths: SDR to HDR output encoding, HDR tone
//! mapping, the HDR surface composite and its GPU mirrors, what a display
//! can show, and software dimming. Pure functions and shader sources, below
//! the compositor, so a compositor change does not rebuild them and their
//! tests run on their own.

pub mod color_pipeline;
pub mod hdr_display;
pub mod hdr_surface;
pub mod hdr_tonemap;
pub mod software_dim;
