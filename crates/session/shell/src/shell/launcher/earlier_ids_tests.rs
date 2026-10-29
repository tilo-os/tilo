//! Earlier app ids (`previously:`) through the launcher's one lookup.

use super::tests::install_fixture;
use super::*;

/// A window, a launch or an open written under an app's earlier id opens the
/// app that took its place, once the store checked that its publisher signed
/// both; the earlier id is not listed beside it.
#[test]
#[serial_test::serial(process_env)]
fn an_earlier_app_id_opens_the_app_that_took_its_place() {
    let tmp = tempfile::tempdir().unwrap();
    let payload = install_fixture(tmp.path(), "so.tilo.lang.tasks", "gui");
    let manifest = payload.join("manifest.toml");
    let renamed = std::fs::read_to_string(&manifest).unwrap().replacen(
        "kind = \"app\"",
        "kind = \"app\"\npreviously = [\"so.tilo.lang.todo\"]",
        1,
    );
    std::fs::write(&manifest, renamed).unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_APPS_DATA_DIR", tmp.path());
    invalidate_app_cache();
    let apps = installed_apps().unwrap();
    // A claim no store checked is not believed.
    assert!(find_installed(&apps, "so.tilo.lang.todo").is_none());

    let registry = tmp.path().join("registry.toml");
    let mut rows = std::fs::read_to_string(&registry).unwrap();
    rows.push_str("\n[[taken-over]]\nid = 'so.tilo.lang.todo'\nby = 'so.tilo.lang.tasks'\n");
    std::fs::write(&registry, rows).unwrap();
    invalidate_app_cache();
    let apps = installed_apps().unwrap();
    let opened = find_installed(&apps, "so.tilo.lang.todo").unwrap();
    assert_eq!(opened.package_id, "so.tilo.lang.tasks");
    assert_eq!(
        installed_earlier_ids().current("so.tilo.lang.todo"),
        "so.tilo.lang.tasks"
    );
    assert_eq!(
        find_installed(&apps, "so.tilo.lang.tasks")
            .unwrap()
            .package_id,
        "so.tilo.lang.tasks"
    );
    env.remove("TILO_APPS_DATA_DIR");
    invalidate_app_cache();
}
