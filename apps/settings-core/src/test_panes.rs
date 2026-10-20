//! Every pane of Settings, compiled into this crate's own tests: the window
//! is tested with every pane, as it runs. Outside the tests the panes are
//! the `settings` app's and `settings-rare`'s.

/// The panes people open often, as the `settings` app has them.
#[path = "../../settings/src/panes"]
mod common {
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
    pub mod part;
    pub mod permissions;
    pub mod personal_apps;
    pub mod power;
    pub mod time_date;

    pub(crate) use crate::pane_model::installed_programs_unreadable;
}

/// The panes people rarely open, as `settings-rare` has them.
#[path = "../../settings-rare/src/panes"]
mod rare {
    pub mod about;
    pub mod accounts;
    pub mod agent_models;
    pub mod agent_vertex;
    pub mod device_mesh;
    pub mod disk_unlock;
    pub mod drives;
    pub mod part;
    pub mod printers;
    pub mod region;
    pub mod speakers;
    pub mod storage;
    pub mod updates;
    pub mod users;
}

pub use common::{
    accessibility, appearance, audio, bluetooth, display, display_arrangement, keyboard_shortcuts,
    mouse_keyboard, network, notifications, permissions, personal_apps, power, time_date,
};
pub use rare::{
    about, accounts, agent_models, agent_vertex, device_mesh, disk_unlock, drives, printers,
    region, speakers, storage, updates, users,
};

pub use crate::pane_model as model;
pub(crate) use crate::pane_model::{composed_pane, pane_node};

/// Every part of Settings.
pub const PARTS: &[&crate::area::Part] = &[
    &crate::effects::PART,
    &common::part::PART,
    &rare::part::PART,
];
