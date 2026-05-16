pub mod shell;
// Legacy non-UI modules retained for compositor/collab work (deferred to Plan 4+).
pub mod config;
pub mod sandbox;
pub mod security;
pub mod network;
pub mod workspace;
// Phase 1 of the real-Wayland-compositor plan: --mode hosted | native dispatch.
pub mod compositor;
// Sandbox Phase 4.3: xdg-desktop-portal backend (FileChooser, ScreenCast,
// Access/Camera) over zbus. Opt-in via `--enable-portals` on the CLI.
pub mod portal;
