use crate::common;
use package_service::install::{InstallError, InstallExpectation, install_verified_tapp};
use package_service::{PackageStore, Pinning};

#[test]
fn a_verified_install_binds_every_registry_identity_before_writing() {
    // The install → resolve → list flow itself is exercised over the wire
    // (`tests/wire_round_trip.rs`) and against a materialised tree
    // (`package-store/tests/install.rs`); what only this install path does is
    // check the registry's expectations.
    {
        let dir = tempfile::tempdir().unwrap();
        let store = PackageStore::at(dir.path());
        let (bytes, id) = common::build_tapp(1);
        let refused = |expected: &InstallExpectation| {
            let result = install_verified_tapp(&store, &bytes, Pinning::Tried, expected, None);
            assert!(
                matches!(
                    result,
                    Err(InstallError::Verify(_)
                        | InstallError::Store(package_store::StoreError::Refused(_)))
                ),
                "{result:?}"
            );
            assert!(
                store.registry().unwrap().is_empty(),
                "a rejected identity must not touch the registry"
            );
            assert!(
                !dir.path().join("pkgs").exists(),
                "nor materialise anything"
            );
        };

        let mut expected = expectation(&bytes, id);
        expected.app_id = "so.tilo.impostor".into();
        refused(&expected);

        let mut expected = expectation(&bytes, id);
        expected.publisher[0] ^= 0x80;
        refused(&expected);

        let mut expected = expectation(&bytes, id);
        expected.size_bytes += 1;
        refused(&expected);

        let mut expected = expectation(&bytes, id);
        expected.package_root = package_service::PackageId::from_bytes([1; 32]);
        refused(&expected);

        let installed = install_verified_tapp(
            &store,
            &bytes,
            Pinning::Tried,
            &expectation(&bytes, id),
            None,
        )
        .unwrap();
        assert_eq!(installed.package_id(), Some(id));
    }
}

fn expectation(bytes: &[u8], id: package_service::PackageId) -> InstallExpectation {
    let meta = app_registry::tapp::load(bytes).unwrap();
    InstallExpectation {
        package_root: id,
        publisher: meta.manifest.publisher.identity_bytes().unwrap(),
        app_id: meta.manifest.package.id,
        version: meta.manifest.package.version,
        size_bytes: bytes.len() as u64,
    }
}
