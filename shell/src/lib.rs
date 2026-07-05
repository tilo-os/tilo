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
// Track D1: the `org.freedesktop.Notifications` D-Bus service. The
// compositor owns the notification daemon so sandboxed apps' toasts
// render as OS-drawn chrome. See `src/notifications.rs`.
pub mod notifications;
// First-boot setup invoked by `quick-desktop-firstboot.service` via
// `quick-desktop --firstboot`. See `src/firstboot.rs`.
pub mod firstboot;
// Observability counters aggregated per peer surface — read by the OSD
// overlay and by the flatpak-share e2e test. See `src/diagnostics.rs`.
pub mod diagnostics;
// End-to-end orchestration for the "share a Flatpak app window to peers"
// story. Composes the surface capture / H.264 encoder / share registry /
// `NeighbourMsg` pipeline that already exists in `network::streaming` +
// `compositor::surface_readback` behind a single public entry point.
// See `src/collab/flatpak_share.rs`.
pub mod collab;
// Hand-rolled Wayland dispatch for extension protocols smithay 0.7
// doesn't wrap — currently `color-management-v1` + a small EDID
// parser used by the DRM output-side reporting. See
// `src/wayland/color_management.rs`.
pub mod wayland;
