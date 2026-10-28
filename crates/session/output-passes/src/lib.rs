#![cfg_attr(
    test,
    allow(
        clippy::float_cmp,
        reason = "in a test an exact comparison is the assertion, not a hazard"
    )
)]
//! The GPU passes that put a rendered frame on an output: turning it to the
//! output's orientation, applying the output's ICC calibration, encoding it
//! for an HDR output and compositing HDR surfaces over it, keeping the
//! damage history of 8-bit XRGB targets, warming the shader cache, and
//! returning the memory a software GPU freed. Below the compositor, so a
//! compositor change does not rebuild them and their tests run on their own.

mod fullscreen_pass;
pub mod hdr_composite;
pub mod hdr_encode;
pub mod icc_lut;
pub mod orientation;
/// Returning the device memory a software GPU freed.
#[cfg(target_os = "linux")]
pub mod software_gpu_memory;
pub mod warmup;
pub mod xrgb_history;
