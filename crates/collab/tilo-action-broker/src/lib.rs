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

/// Run the signed package `app_id` with `surface`, the `surface.toml` it
/// ships: the person-record kinds it declares work as a built-in app's, and
/// the changes it signs name that surface. A host calls it as it launches
/// the package.
pub fn register_package_surface(app_id: &str, surface: &[u8]) {
    ui::app_surface::register_package(app_id, surface);
}

/// Decide every change this process's rooms receive by its claim against
/// the app surface its op names, and name in the changes the process signs
/// for an app the surface that app runs with. A host calls it once, before
/// it opens its rooms.
///
/// The surface is the one this process runs the app with when the op names
/// that one, and otherwise what `read` finds (the host's content store, in
/// the record's room and app). The process's own writes name the surface it
/// runs, and nothing puts that file in the content store, so without the
/// first source every change a host signs for a package that ships a surface
/// would wait for it forever, its first record included.
pub fn decide_receipt_against_surfaces(
    read: impl Fn(&collab_types::Record<'_>, &str) -> Option<Vec<u8>> + Send + Sync + 'static,
) {
    tilo_peer_state::decide_with_surfaces(
        collab_types::Surfaces::from_source(std::sync::Arc::new(
            collab_types::surface::ContentSurfaces::new(move |record, hash| {
                running_surface(record.app, hash).or_else(|| read(record, hash))
            }),
        )),
        ui::app_surface::hash_of,
    );
}

/// The surface `hash` of `app`, when it is the one this process runs `app`
/// with. The bytes are addressed by their hash, which the reader checks
/// again, so where they come from does not change what they mean.
fn running_surface(app: &str, hash: &str) -> Option<Vec<u8>> {
    let running = ui::app_surface::hash_of(app)?;
    (hex::encode(running) == hash)
        .then(|| ui::app_surface::bytes_of(app).map(<[u8]>::to_vec))
        .flatten()
}

#[cfg(test)]
mod running_surface_tests {
    use super::running_surface;

    #[test]
    fn a_package_surface_this_process_runs_is_served_by_its_hash_only() {
        let app = "so.tilo.test.running-surface";
        let surface = b"[kinds]\nnote = \"so.tilo.test.running-surface.note\"\n";
        assert_eq!(
            running_surface(app, &hex::encode(blake3::hash(surface).as_bytes())),
            None
        );
        super::register_package_surface(app, surface);
        assert_eq!(
            running_surface(app, &hex::encode(blake3::hash(surface).as_bytes())).as_deref(),
            Some(surface.as_slice()),
            "the change this process signs names this surface, so it must decide it"
        );
        assert_eq!(
            running_surface(
                app,
                &hex::encode(blake3::hash(b"another surface").as_bytes())
            ),
            None,
            "a surface the process does not run is the content store's to serve"
        );
        assert_eq!(
            running_surface(
                "so.tilo.test.other-app",
                &hex::encode(blake3::hash(surface).as_bytes())
            ),
            None,
            "a surface is served only for the app that runs it"
        );
    }
}
