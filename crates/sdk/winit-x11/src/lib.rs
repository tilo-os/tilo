//! Turns on winit's X11 backend.
//!
//! This crate has no items. Depending on it adds `x11` to the workspace's
//! winit, which Tilo apps build without: they run as Wayland clients in their
//! sandbox, where no X server is reachable. The shell's hosted backend and
//! the toolkit's `x11` development feature depend on it instead of naming
//! `winit/x11` themselves, so the workspace hack's dependency analysis can
//! leave this one deliberate variant out (see `.config/hakari.toml`).
