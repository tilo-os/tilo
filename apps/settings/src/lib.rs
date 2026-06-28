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
//! content pane. Each interactive pane (display, audio, network,
//! bluetooth) holds a `Dispatcher` from the shared `os-actions` crate
//! so the same brightnessctl/wpctl/nmcli/bluetoothctl bindings the
//! compositor's quick-settings panel uses drive the Settings sliders
//! and toggles. `workspaces` and `collaboration` show informational
//! stubs — the data they'd surface lives in the compositor, which the
//! Settings app deliberately doesn't depend on.

pub mod categories;
pub mod dispatcher;
pub mod panes;
pub mod settings_app;
pub mod shots;

pub use settings_app::SettingsApp;
pub use shots::render_to_path;
