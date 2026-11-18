// One test binary; #[path] keeps the files in place.
#![cfg(target_os = "linux")]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::struct_excessive_bools,
    clippy::type_complexity,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

#[path = "support.rs"]
mod support;

#[path = "clipboard.rs"]
mod clipboard;

#[path = "color.rs"]
mod color;

#[path = "dmabuf.rs"]
mod dmabuf;

#[path = "input.rs"]
mod input;

#[path = "keeper.rs"]
mod keeper;

#[path = "process.rs"]
mod process;

#[path = "text_input.rs"]
mod text_input;

#[path = "x11.rs"]
mod x11;

// Drag-and-drop between a kept launch's X11 programs and the desktop.
#[path = "x11_drag.rs"]
mod x11_drag;
