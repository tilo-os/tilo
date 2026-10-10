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
pub mod peer_actions;
pub mod policy_schedule;
pub mod published_authorization;
pub mod record_content;
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

/// The person-record kinds an app's surface declares, as this process holds
/// the surfaces of the apps it runs: what the identity service signs for an
/// app in the person's own room, and the broker admits it to there.
pub struct SurfacePersonRecords;

impl tilo_identity_host::broker::PersonRecordKinds for SurfacePersonRecords {
    fn person_record_kinds(&self, app_id: &str) -> Vec<String> {
        ui::app_surface::person_record_kinds(app_id)
    }
}

/// Decide every change this process's rooms receive by its claim against
/// the app surface its op names, read by `read` (the host's content store,
/// in the record's room and app), and name in the changes the process signs
/// for an app the surface that app runs with. A host calls it once, before
/// it opens its rooms.
pub fn decide_receipt_against_surfaces(
    read: impl Fn(&collab_types::Record<'_>, &str) -> Option<Vec<u8>> + Send + Sync + 'static,
) {
    tilo_peer_state::decide_with_surfaces(
        collab_types::Surfaces::from_source(std::sync::Arc::new(
            collab_types::surface::ContentSurfaces::new(read),
        )),
        ui::app_surface::hash_of,
    );
}
