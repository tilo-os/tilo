//! Library root for the qOS native Settings app.
//!
//! Hosts the `SettingsApp` widget. The binary in `main.rs` is a thin
//! shim that constructs `SettingsApp` and either hands it to `ui::App`
//! for the live event loop or renders one offscreen frame for the
//! `--shots PATH` snapshot mode (mirroring the sibling terminal /
//! file-explorer apps).
//!
//! The structure follows the same shape as `apps/file-explorer`:
//! `categories.rs` enumerates the left sidebar entries, `panes/`
//! contains one module per category that renders the right-hand
//! content pane. Most panes are placeholders today; only `about`
//! pulls real data (from `/etc/os-release`, `/proc/sys/kernel/osrelease`
//! and `/proc/uptime`). The interactive panes (display brightness,
//! audio volume, Wi-Fi, BT, workspaces, collaboration) are stubbed
//! out behind a `Coming soon` placeholder until the shell-side
//! `qs_actions` binding lands.

pub mod categories;
pub mod panes;
pub mod settings_app;
pub mod shots;

pub use settings_app::SettingsApp;
pub use shots::render_to_path;
