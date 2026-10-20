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

pub mod accessibility;
pub mod appearance;
pub mod audio;
pub mod bluetooth;
pub mod display;
pub mod display_arrangement;
pub mod keyboard_shortcuts;
pub mod mouse_keyboard;
pub mod network;
pub mod notifications;
pub mod permissions;
pub mod personal_apps;
pub mod power;

// The panes people rarely open are `settings-rare`'s, loaded from the
// Settings engine library; they are named here as panes of Settings too.
pub use settings_rare::panes::{
    about, accounts, agent_models, agent_vertex, device_mesh, disk_unlock, drives, printers,
    region, speakers, storage, time_date, updates, users,
};

pub use crate::pane_model as model;
pub(crate) use crate::pane_model::{composed_pane, installed_programs_unreadable, pane_node};

mod part;
pub use part::PART;

/// Every part of Settings: its own, the panes people open often, and the
/// panes people rarely open.
pub const PARTS: &[&crate::area::Part] =
    &[&crate::effects::PART, &PART, &settings_rare::panes::PART];
