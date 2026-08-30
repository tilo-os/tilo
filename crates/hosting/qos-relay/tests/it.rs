// One test binary instead of one per file; #[path] keeps the files in place.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
)]

#[path = "common/mod.rs"]
mod common;

#[path = "anti_entropy_recovers_lost_delta.rs"]
mod anti_entropy_recovers_lost_delta;
#[path = "auth_reject.rs"]
mod auth_reject;
#[path = "client_owns_its_endpoint.rs"]
mod client_owns_its_endpoint;
#[path = "coedit_through_host.rs"]
mod coedit_through_host;
#[path = "cross_user_workspace_host.rs"]
mod cross_user_workspace_host;
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
#[path = "multi_host.rs"]
mod multi_host;
#[path = "relay_daemon.rs"]
mod relay_daemon;
#[path = "store_and_forward.rs"]
mod store_and_forward;
