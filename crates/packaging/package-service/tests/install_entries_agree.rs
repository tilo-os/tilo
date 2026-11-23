//! A package installed through the package service's install action and the same package
//! installed by an image build's in-process entry (`tilo-install --offline-store`) end up as the
//! same store: one change behind both.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use package_service::language_pack::LanguagePackInstalls;
use package_service::wire::{Change, Replacement, Trust, Uploaded};
use package_store::{Catalog, InstalledPackage, PackageStore};
use tilo_app_package::trust::TrustFile as _;

use crate::common;

use crate::fixture;

/// What a store holds for a package, without what differs by where the store is: its rows' paths,
/// times and provenance, and every payload file with its bytes.
fn stored(root: &Path) -> (Vec<InstalledPackage>, BTreeMap<String, Vec<u8>>) {
    let rows: Vec<InstalledPackage> = Catalog::at(root, None)
        .list()
        .unwrap()
        .into_iter()
        .map(|mut row| {
            row.payload_root = row.payload_root.strip_prefix(root).unwrap().to_path_buf();
            row.manifest_path = row.manifest_path.strip_prefix(root).unwrap().to_path_buf();
            row.installed_at = 0;
            row.last_used = 0;
            row.source = String::new();
            row
        })
        .collect();
    let mut files = BTreeMap::new();
    for row in &rows {
        let tree = root.join(&row.payload_root);
        let mut stack = vec![tree.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    let name = path.strip_prefix(&tree).unwrap().display().to_string();
                    files.insert(name, std::fs::read(&path).unwrap());
                }
            }
        }
        let report = package_store::integrity::verify(
            &Catalog::at(root, None).find(&row.id).unwrap().unwrap(),
            &tree,
        )
        .unwrap();
        assert!(report.is_intact(), "{report:?}");
    }
    (rows, files)
}

#[test]
fn the_install_action_and_the_image_build_entry_write_the_same_store() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    home.env()
        .remove("TILO_APPS_DATA_DIR")
        .remove("TILO_APPS_SYSTEM_DIR")
        .remove("TILO_RUNTIME_BUNDLE_DIR");
    let (full, _, trust, _) = fixture::fixture();
    trust
        .save(&home.config().join("tilo/trusted-attestors.toml"))
        .unwrap();

    // Through the action, as the Store hands a local file over.
    let device = home.data().join("tilo-apps");
    let store = Arc::new(PackageStore::at(&device));
    let catalog = Arc::new(Catalog::at(&device, None));
    let socket = home.runtime().join("package-service.sock");
    let bound = socket.clone();
    // `serve` accepts forever; the thread ends with the test process.
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        tokio::task::LocalSet::new().block_on(&runtime, async move {
            let listener = package_service::transport::bind(&bound).unwrap();
            package_service::transport::serve(
                listener,
                store,
                catalog,
                0,
                LanguagePackInstalls::new(),
            )
            .await
        })
    });
    let client = common::store_client(&socket);
    let review = client.review_local(&full).unwrap();
    let installed = client.install_local(&full, &review).unwrap();
    assert_eq!(installed.to_hex(), review.root);

    // Through the build entry, in-process into the image's store.
    let image = home.root().join("image/tilo-apps");
    let tapp = home.root().join("local.tapp");
    std::fs::write(&tapp, &full).unwrap();
    let report = package_service::change::apply(
        &package_service::change::Target {
            store: &PackageStore::at(&image),
            catalog: &Catalog::at(&image, None),
            max_store_bytes: package_store::DEFAULT_MAX_STORE_BYTES,
        },
        Change::Install {
            file: Uploaded {
                source: tapp.display().to_string(),
            },
            trust: Trust {
                require_package_root: Some(review.root.clone()),
                require_attestor: Some(review.attestor),
                ..Trust::default()
            },
            pinned: false,
            replacement: Replacement::Refuse,
            ignore_missing_runtime: false,
            dry_run: false,
            payload_from: None,
        },
        Some(&tapp),
    )
    .unwrap();
    assert!(
        report
            .lines
            .iter()
            .any(|line| line.contains("so.tilo.unlisted-local")),
        "{report:?}"
    );

    let (action_rows, action_files) = stored(&device);
    let (build_rows, build_files) = stored(&image);
    assert_eq!(action_rows.len(), 1);
    assert_eq!(action_rows, build_rows);
    assert_eq!(action_files, build_files);
    assert_eq!(
        action_files.get("bin/local").map(Vec::as_slice),
        Some(&b"local signed payload"[..])
    );
}
