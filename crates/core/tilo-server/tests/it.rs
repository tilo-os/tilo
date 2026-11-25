// One test binary instead of one per file; #[path] keeps the files in place.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

// Integration tests re-exec this binary as the automerge worker.
tilo_automerge_isolate::test_worker!();

#[path = "common/mod.rs"]
mod common;

#[cfg(feature = "agents")]
#[path = "linux_browser.rs"]
mod linux_browser;
#[cfg(feature = "agents")]
#[cfg(feature = "agents")]
#[path = "linux_turn.rs"]
mod linux_turn;
#[path = "machines.rs"]
mod machines;

#[path = "auth_reject.rs"]
mod auth_reject;
#[cfg(feature = "agents")]
#[path = "bridged_avatar.rs"]
mod bridged_avatar;
#[path = "browser_transport.rs"]
mod browser_transport;
#[path = "chat_content.rs"]
mod chat_content;
#[path = "drop_frees_address.rs"]
mod drop_frees_address;
#[path = "files_at_disk_speed.rs"]
mod files_at_disk_speed;
#[path = "holder_content.rs"]
mod holder_content;
#[path = "holder_copies.rs"]
mod holder_copies;
#[cfg(feature = "agents")]
#[path = "hosted_browser.rs"]
mod hosted_browser;
#[path = "hosting_member.rs"]
mod hosting_member;
#[path = "kept_limits.rs"]
mod kept_limits;
#[path = "listed_workspaces.rs"]
mod listed_workspaces;
#[path = "member_copy.rs"]
mod member_copy;
#[path = "member_list_access.rs"]
mod member_list_access;
#[cfg(target_os = "linux")]
#[path = "offloadable_action.rs"]
mod offloadable_action;
#[path = "region_follows.rs"]
mod region_follows;
#[path = "remote_action_routing.rs"]
mod remote_action_routing;
#[path = "server_daemon.rs"]
mod server_daemon;
#[path = "server_member_copy.rs"]
mod server_member_copy;
#[path = "server_move.rs"]
mod server_move;
#[path = "servers_document.rs"]
mod servers_document;
#[path = "servers_operations.rs"]
mod servers_operations;
#[path = "surface_content.rs"]
mod surface_content;
#[path = "tenant_isolation.rs"]
mod tenant_isolation;
#[path = "workspace_admission.rs"]
mod workspace_admission;
#[path = "workspace_service.rs"]
mod workspace_service;

#[path = "additional_protocol.rs"]
mod additional_protocol;
