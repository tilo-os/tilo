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

#[path = "anti_entropy_recovers_lost_delta.rs"]
mod anti_entropy_recovers_lost_delta;
#[path = "auth_reject.rs"]
mod auth_reject;
#[path = "browser_transport.rs"]
mod browser_transport;
#[path = "client_owns_its_endpoint.rs"]
mod client_owns_its_endpoint;
#[path = "coedit_through_host.rs"]
mod coedit_through_host;
#[path = "commit_dedup.rs"]
mod commit_dedup;
#[path = "compute_session.rs"]
mod compute_session;
#[path = "cross_user_workspace_host.rs"]
mod cross_user_workspace_host;
#[path = "device_certification.rs"]
mod device_certification;
#[path = "durable_host_restart.rs"]
mod durable_host_restart;
#[path = "end_to_end_sync.rs"]
mod end_to_end_sync;
#[path = "federation_cross_user_reject.rs"]
mod federation_cross_user_reject;
#[path = "federation_echo_suppression.rs"]
mod federation_echo_suppression;
#[path = "federation_offline_reconnect.rs"]
mod federation_offline_reconnect;
#[path = "federation_three_node_chain.rs"]
mod federation_three_node_chain;
#[path = "federation_two_relays.rs"]
mod federation_two_relays;
#[path = "federation_workspace_shard.rs"]
mod federation_workspace_shard;
#[path = "governed_rooms.rs"]
mod governed_rooms;
#[path = "governed_workspace_access.rs"]
mod governed_workspace_access;
#[path = "interactive_workspace_owner.rs"]
mod interactive_workspace_owner;
#[path = "live_plane.rs"]
mod live_plane;
#[path = "multi_host.rs"]
mod multi_host;
#[path = "personal_device_process.rs"]
mod personal_device_process;
#[path = "relay_daemon.rs"]
mod relay_daemon;
#[path = "remote_action_routing.rs"]
mod remote_action_routing;
#[path = "store_and_forward.rs"]
mod store_and_forward;

#[path = "additional_protocol.rs"]
mod additional_protocol;
#[path = "addressed_actions.rs"]
mod addressed_actions;
