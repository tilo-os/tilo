//! An app's route is its own id, whatever its manifest claimed it was published as before: a
//! `previously:` claim renames nothing, so a device that never installed the earlier app routes
//! and keeps the app's own records, and the broker admits it as its own worker.

use super::*;

#[test]
fn a_route_is_the_apps_own_whatever_the_manifest_claims_before() {
    let mut meta = notes().meta;
    // An app id of this test's own: its registration is no other test's.
    meta.app = "so.tilo.lang.route-test".into();
    let workspace = uuid::Uuid::new_v4();
    tilolang_tilo::schema::register_previously(
        &meta.app,
        std::slice::from_ref(&"so.tilo.lang.route-test-before".to_owned()),
    );
    assert_eq!(
        tilolang_process::route_of(&meta, workspace, ADA).app_id,
        meta.app,
        "the route is the app's own, whatever was claimed before"
    );
    tilolang_tilo::schema::register_previously(&meta.app, &[]);
}
