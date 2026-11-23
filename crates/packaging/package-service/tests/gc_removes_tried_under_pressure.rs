use crate::common;
use package_service::install::install_tapp;
use package_service::{PackageStore, Pinning};

/// The collection after an install keeps the app that install wrote, though it would otherwise be
/// the last thing the sweep evicts.
///
/// Eviction itself — LRU order, pinned rows, shared blobs, a zero cap — is
/// exercised against a materialised store in `package-store/tests/install.rs`
/// (`sweep_removes_lru_until_under_cap_in_one_gc`,
/// `removal_evicts_unreferenced_blobs_and_honours_pins`) and its
/// `plan_eviction` tests.
#[test]
fn the_collection_after_an_install_keeps_the_app_it_installed() {
    let dir = tempfile::tempdir().unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_bundle::RUNTIME_DIR_ENV, dir.path().join("runtimes"));
    let store = PackageStore::at(dir.path().join("apps"));
    let catalog = package_store::Catalog::at(store.layout().root(), None);
    let (used, _) = common::build_tapp_for("so.tilo.used-before", 24);
    install_tapp(&store, &used, Pinning::Tried, None).unwrap();
    store
        .update_row("so.tilo.used-before", |row| row.last_used = 1_000)
        .unwrap();
    let (large, _) = common::build_tapp_for("so.tilo.just-installed", 96);
    let installed = install_tapp(&store, &large, Pinning::Tried, None).unwrap();

    package_service::service::collect_after_change(
        &store,
        &catalog,
        60 * 1024,
        &[installed.id.as_str()],
    );

    let registry = store.registry().unwrap();
    assert!(
        registry.find("so.tilo.used-before").is_none(),
        "the app used before goes for size"
    );
    let kept = registry
        .find("so.tilo.just-installed")
        .expect("the app just installed stays");
    assert!(
        package_store::integrity::verify(kept, &kept.payload_root)
            .unwrap()
            .is_intact(),
        "and opens whole"
    );
}
