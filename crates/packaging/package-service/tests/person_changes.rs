//! What a person changes from a terminal (`tilo-install`, `app`) is the package service's action:
//! only the person's own connection may make it, only to the store the service writes, and the
//! file it names travels over the connection, as the service cannot read the caller's files.

use std::path::Path;
use std::sync::Arc;

use package_service::language_pack::LanguagePackInstalls;
use package_service::wire::{Change, Replacement, SavedData, Trust, Uploaded};
use package_store::{Catalog, PackageStore, Pinning};
use tilo_app_package::trust::TrustFile as _;

use crate::{common, fixture};

const APP: &str = "so.tilo.unlisted-local";

/// The package service writing `root`, on a thread that ends with the test process.
fn serve(root: &Path, socket: &Path) {
    let store = Arc::new(PackageStore::at(root));
    let catalog = Arc::new(Catalog::at(root, None));
    let bound = socket.to_path_buf();
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
}

fn pinning(root: &Path) -> Option<Pinning> {
    PackageStore::at(root)
        .registry()
        .unwrap()
        .find(APP)
        .map(package_store::InstalledPackage::pinning)
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one person's session of changes, in the order they make them"
)]
fn a_persons_changes_are_the_services_actions_on_its_own_store() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    home.env()
        .remove("TILO_APPS_DATA_DIR")
        .remove("TILO_APPS_SYSTEM_DIR")
        .remove("TILO_RUNTIME_BUNDLE_DIR");
    let (full, _, trust, _) = fixture::fixture();
    let attestors = home.config().join("tilo/trusted-attestors.toml");
    trust.save(&attestors).unwrap();
    let device = home.data().join("tilo-apps");
    let socket = home.runtime().join("package-service.sock");
    serve(&device, &socket);
    let tapp = home.root().join("local.tapp");
    std::fs::write(&tapp, &full).unwrap();
    let review = common::store_client(&socket).review_local(&full).unwrap();
    let install = Change::Install {
        file: Uploaded {
            source: tapp.display().to_string(),
        },
        trust: Trust {
            require_package_root: Some(review.root.clone()),
            require_publisher: None,
            require_attestor: Some(review.attestor),
            allow_self_signed: false,
        },
        pinned: false,
        replacement: Replacement::Refuse,
        ignore_missing_runtime: false,
        dry_run: false,
        payload_from: None,
    };

    // An app the launcher connected may not change the store this way, the Store included.
    let refused = common::store_client(&socket)
        .change(&device, install.clone(), Some(&tapp))
        .unwrap_err();
    assert!(
        refused.contains("may not change the package store"),
        "{refused}"
    );
    // A shell that names another store is told how to write that one, not silently redirected.
    let elsewhere = home.root().join("elsewhere/tilo-apps");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let person = common::person_client(&socket);
    let refused = person
        .change(&elsewhere, install.clone(), Some(&tapp))
        .unwrap_err();
    assert!(refused.contains("--offline-store"), "{refused}");
    assert_eq!(pinning(&device), None);
    assert_eq!(pinning(&elsewhere), None);

    // Installed from the person's file, which the service received over the connection.
    let report = person.change(&device, install, Some(&tapp)).unwrap();
    assert!(
        report.lines.iter().any(|line| line.contains(APP)),
        "{report:?}"
    );
    assert_eq!(pinning(&device), Some(Pinning::Tried));
    let installed = Catalog::at(&device, None).find(APP).unwrap().unwrap();
    assert_eq!(
        std::fs::read(installed.payload_root.join("bin/local")).unwrap(),
        b"local signed payload"
    );

    person
        .change(
            &device,
            Change::SetPinned {
                app: APP.into(),
                pinned: true,
            },
            None,
        )
        .unwrap();
    assert_eq!(pinning(&device), Some(Pinning::Pinned));
    person
        .change(
            &device,
            Change::SetPinned {
                app: APP.into(),
                pinned: false,
            },
            None,
        )
        .unwrap();
    assert_eq!(pinning(&device), Some(Pinning::Tried));

    // The device's trust lists are the service's to change too.
    let key = "11".repeat(32);
    person
        .change(
            &device,
            Change::AddAttestor {
                id: "person-added".into(),
                pubkey: key.clone(),
                scope: "all".into(),
            },
            None,
        )
        .unwrap();
    assert!(
        std::fs::read_to_string(&attestors)
            .unwrap()
            .contains("person-added")
    );
    person
        .change(
            &device,
            Change::RemoveAttestor {
                id: "person-added".into(),
            },
            None,
        )
        .unwrap();
    assert!(
        !std::fs::read_to_string(&attestors)
            .unwrap()
            .contains("person-added")
    );
    person
        .change(
            &device,
            Change::Revoke {
                pubkey: key.clone(),
                reason: "lost".into(),
            },
            None,
        )
        .unwrap();
    let revoked = std::fs::read_to_string(home.config().join("tilo/revoked-keys.toml")).unwrap();
    assert!(revoked.contains(&key), "{revoked}");

    // A dry run says what it would do and leaves the package where it is.
    let plan = person
        .change(
            &device,
            Change::Uninstall {
                app: APP.into(),
                saved_data: SavedData::Keep,
                dry_run: true,
            },
            None,
        )
        .unwrap();
    assert!(
        plan.lines.iter().any(|line| line.starts_with("plan:")),
        "{plan:?}"
    );
    assert_eq!(pinning(&device), Some(Pinning::Tried));
    person
        .change(
            &device,
            Change::Uninstall {
                app: APP.into(),
                saved_data: SavedData::Keep,
                dry_run: false,
            },
            None,
        )
        .unwrap();
    assert_eq!(pinning(&device), None);
    assert!(Catalog::at(&device, None).find(APP).unwrap().is_none());
}
