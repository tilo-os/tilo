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
// Global-search backend: SearchSource trait + five concrete
// implementations (installed / uninstalled qOS apps, installed /
// uninstalled Flatpaks, PATH commands) + fuzzy ranker. See
// `src/search/`.
pub mod search;
