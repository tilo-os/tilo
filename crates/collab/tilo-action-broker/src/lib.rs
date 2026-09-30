//! Production collaboration broker services shared by native hosts.
pub mod app_actions;
pub mod automation;
pub mod channel_encryption;
pub mod collab_broker;
pub mod collab_broker_capnp;
pub mod collab_core;
pub mod collab_history;
pub mod collab_inbound;
pub mod collab_outbound;
pub mod collab_presence;
pub mod collaboration_admin;
pub mod host_documents;
pub mod owner_turn;
pub mod peer_actions;
pub mod policy;
pub mod published_authorization;
pub mod route;
pub mod services;
#[cfg(test)]
mod test_owner;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
// Room admission decodes peer bytes in automerge worker processes started
// from this test binary; this serves them.
#[cfg(test)]
tilo_automerge_isolate::test_worker!();
