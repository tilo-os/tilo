//! What a device keeps when its machine is a server (`tilo_runtime::device::keep`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

mod common;

#[cfg(feature = "agents")]
mod linux_browser;
#[cfg(feature = "agents")]
mod linux_turn;
mod machines;

#[cfg(feature = "agents")]
mod bridged_avatar;
mod browser_transport;
mod chat_content;
mod drop_frees_address;
mod files_at_disk_speed;
mod holder_content;
mod holder_copies;
#[cfg(feature = "agents")]
mod hosted_browser;
mod hosting_member;
mod kept_limits;
mod listed_workspaces;
mod member_copy;
mod member_list_access;
#[cfg(target_os = "linux")]
mod offloadable_action;
mod region_follows;
mod remote_action_routing;
mod server_daemon;
mod server_member_copy;
mod server_move;
mod servers_document;
mod servers_operations;
mod surface_content;
mod tenant_isolation;
mod workspace_admission;
mod workspace_service;
