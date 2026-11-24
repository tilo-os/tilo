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

#[cfg(feature = "agents")]
#[path = "agents_list.rs"]
mod agents_list;
#[path = "auth_reject.rs"]
mod auth_reject;
#[cfg(feature = "agents")]
#[path = "bridged_avatar.rs"]
mod bridged_avatar;
#[path = "browser_transport.rs"]
mod browser_transport;
#[path = "governed_workspace_access.rs"]
mod governed_workspace_access;
#[path = "governed_workspaces.rs"]
mod governed_workspaces;
#[path = "holder_content.rs"]
mod holder_content;
#[path = "holder_copies.rs"]
mod holder_copies;
#[cfg(feature = "agents")]
#[path = "hosted_browser.rs"]
mod hosted_browser;
#[path = "hosting_member.rs"]
mod hosting_member;
#[path = "member_copy.rs"]
mod member_copy;
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
