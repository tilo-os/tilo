//! Production collaboration broker services shared by native hosts.
pub mod agent_control;
pub mod agent_reads;
pub mod agent_timing;
pub mod app_actions;
pub mod automation;
pub mod collab_broker;
pub mod collab_broker_capnp;
pub mod collab_core;
pub mod collab_history;
pub mod collab_outbound;
pub mod collab_presence;
pub mod collaboration_admin;
pub mod enrolled_dispatch;
pub mod host_documents;
pub mod peer_actions;
pub mod policy;
pub mod route;
pub mod services;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
/// Bounds a test wait that ends as soon as its condition holds; only a hang reaches it.
#[cfg(test)]
pub(crate) const HANG_BOUND: std::time::Duration = std::time::Duration::from_secs(30);
// Room admission decodes peer bytes in automerge worker processes started
// from this test binary; this serves them.
#[cfg(test)]
tilo_automerge_isolate::test_worker!();
