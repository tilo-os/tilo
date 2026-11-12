// One test binary instead of one per file; #[path] keeps the files in place.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

#[path = "headless/harness.rs"]
mod harness;

#[path = "capture.rs"]
mod capture;
#[path = "clipboard.rs"]
mod clipboard;
#[path = "color_management_protocol.rs"]
mod color_management_protocol;
#[path = "dnd.rs"]
mod dnd;
#[path = "gamma.rs"]
mod gamma;
#[path = "hdr_surface_colour.rs"]
mod hdr_surface_colour;
#[path = "headless.rs"]
mod headless;
#[path = "host_frames.rs"]
mod host_frames;
#[path = "hotplug.rs"]
mod hotplug;
#[path = "layers.rs"]
mod layers;
#[path = "output_management.rs"]
mod output_management;
#[path = "server_side_decorations.rs"]
mod server_side_decorations;
#[path = "shared_window_color.rs"]
mod shared_window_color;
#[path = "xwayland.rs"]
mod xwayland;
#[path = "xwayland_dnd.rs"]
mod xwayland_dnd;
