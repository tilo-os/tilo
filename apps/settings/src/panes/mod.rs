//! One module per settings category content pane.
//!
//! Each pane exposes `build() -> Box<dyn Widget>` returning that category's
//! body. The pane header (icon + title + subtitle) is owned by
//! `settings_app`, so the modules here focus on the category-specific
//! content.
//!
//! Panes fall into two groups:
//! - **Hardware panes** (network, bluetooth, sound, displays) own an
//!   `os_actions::Dispatcher` and drive real `nmcli`/`wpctl`/`brightnessctl`/
//!   `bluetoothctl` bindings — the same ones the compositor's quick-settings
//!   panel uses.
//! - **Information / editor panes** (accounts, region, users, printers,
//!   time & date, mouse & keyboard, permissions, updates, about) render
//!   real system state where it's cheaply available, and otherwise a
//!   populated, realistic sample so the surface reads as designed. Each
//!   pane's docstring states which parts are live vs. sample.

pub mod about;
pub mod accessibility;
pub mod accounts;
pub mod appearance;
pub mod audio;
pub mod bluetooth;
pub mod display;
pub mod keyboard_shortcuts;
pub mod mouse_keyboard;
pub mod network;
pub mod notifications;
pub mod permissions;
pub mod power;
pub mod printers;
pub mod region;
pub mod time_date;
pub mod updates;
pub mod users;

/// The "Sound" category renders the audio pane.
pub mod sound {
    pub use super::audio::build;
}
