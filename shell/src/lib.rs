pub mod shell;
pub mod config;
pub mod network;
pub mod sandbox;
pub mod security;
pub mod workspace;
pub mod compositor;
// xdg-desktop-portal backend (FileChooser, ScreenCast, Access/Camera) over
// zbus. Opt-in via `--enable-portals` on the CLI.
pub mod portal;
// The `org.freedesktop.Notifications` D-Bus service. The compositor owns
// the notification daemon so sandboxed apps' toasts render as OS-drawn
// chrome. See `src/notifications.rs`.
pub mod notifications;
// First-boot setup invoked by `quick-desktop-firstboot.service` via
// `quick-desktop --firstboot`. See `src/firstboot.rs`.
pub mod firstboot;
pub mod bluetooth;
pub mod bluetooth_prompt;
pub mod onboarding;
pub mod onboarding_view;
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
