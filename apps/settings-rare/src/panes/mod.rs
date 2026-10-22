//! The panes people rarely open: accounts, agent models, devices and sync,
//! speakers, storage and drives, disk unlock, printers, users, region,
//! updates and about. They are a library of their own, loaded from the
//! Settings engine library, so the app's package carries only the panes
//! people open often; each registers through its part ([`PART`]).

pub mod about;
pub mod accounts;
pub mod agent_models;
pub mod agent_sign_in;
pub mod agent_vertex;
pub mod device_mesh;
pub mod disk_unlock;
pub mod drives;
pub mod printers;
pub mod region;
pub mod speakers;
pub mod storage;
pub mod time_date;
pub mod updates;
pub mod users;

pub use crate::pane_model as model;
pub(crate) use crate::pane_model::composed_pane;

mod part;
pub use part::PART;
