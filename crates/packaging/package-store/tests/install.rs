//! Installing into the content store: materialisation, dedup across
//! versions, thin packages, removal and garbage collection.

use std::collections::BTreeMap;
use std::io::Read;

use package_store::{Admission, BlobFetcher, PackageStore, StoreError};
use tilo_app_package::{BlobHash, BlobSink, BlobSource, Packed, SigningIdentity};

use crate::support::{
    bytes, content_files, full_tapp, package, reflinks_supported, scratch, share_extents,
    thin_tapp, tree,
};

#[allow(
    clippy::unnecessary_wraps,
    reason = "the store's admission callback returns a Result; this one never refuses"
)]
fn admit_all(_meta: &tilo_app_package::PackageMeta) -> Result<Admission, String> {
    Ok(Admission::default())
}

fn v1_files() -> BTreeMap<&'static str, Vec<u8>> {
    BTreeMap::from([
        ("bin/app", bytes(1, 300_000)),
        ("share/icon.png", bytes(2, 120_000)),
        ("share/strings.ftl", b"hello = Hello\n".to_vec()),
    ])
}

fn v2_files() -> BTreeMap<&'static str, Vec<u8>> {
    // The icon and strings are unchanged; the binary changed; a file was added.
    BTreeMap::from([
        ("bin/app", bytes(3, 310_000)),
        ("share/icon.png", bytes(2, 120_000)),
        ("share/strings.ftl", b"hello = Hello\n".to_vec()),
        ("share/new.dat", bytes(4, 90_000)),
    ])
}

/// Serves blobs from a package's own files and records what was asked for.
struct CountingFetcher<'a> {
    source: &'a Packed,
    asked: std::sync::Mutex<Vec<BlobHash>>,
}

impl BlobFetcher for CountingFetcher<'_> {
    fn fetch(&self, hashes: &[BlobHash], sink: &mut dyn BlobSink) -> std::io::Result<()> {
        self.asked.lock().unwrap().extend_from_slice(hashes);
        for hash in hashes {
            let blob = self
                .source
                .open_blob(hash)?
                .ok_or_else(|| std::io::Error::other("not in this package"))?;
            sink.accept(hash, blob.len, &mut blob.reader.take(blob.len))?;
        }
        Ok(())
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn installs_materialise_the_tree_and_share_unchanged_blobs() {
    // phase: an_install_materialises_exactly_the_package_tree
    {
        let identity = SigningIdentity::generate();
        let files = v1_files();
        let (_payload, packed) = package(&identity, "so.tilo.demo", "1.0.0", &files);
        let root = scratch();
        let store = PackageStore::at(root.path());

        let installed = store
            .install(full_tapp(&packed).as_slice(), admit_all, None)
            .unwrap();
        let row = &installed.package;
        assert_eq!(row.id, "so.tilo.demo");
        assert_eq!(row.version, "1.0.0");
        assert_eq!(row.package_id(), Some(packed.id()));
        assert_eq!(row.payload_root, store.layout().payload(packed.id()));
        assert_eq!(
            std::fs::read(&row.manifest_path).unwrap(),
            packed.meta.manifest_bytes,
            "the launcher reads the signed manifest bytes"
        );

        let expected: BTreeMap<String, Vec<u8>> = files
            .iter()
            .map(|(rel, content)| ((*rel).to_string(), content.clone()))
            .collect();
        assert_eq!(tree(&row.payload_root), expected);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |rel: &str| {
                std::fs::metadata(row.payload_root.join(rel))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777
            };
            assert_eq!(mode("bin/app"), 0o555, "the entrypoint is executable");
            assert_eq!(mode("share/icon.png"), 0o444, "payload files are read-only");
        }

        let listed = store.registry().unwrap();
        assert_eq!(listed.entries(), std::slice::from_ref(row));
        let report = package_store::integrity::verify(row, &row.payload_root).unwrap();
        assert!(
            report.is_intact() && report.unexpected.is_empty(),
            "{report:?}"
        );
        assert!(
            !root.path().join("payloads").exists(),
            "no second unpacked copy is kept"
        );
    }
    // phase: an_upgrade_stores_only_changed_blobs_and_reflinks_shared_ones
    {
        let identity = SigningIdentity::generate();
        let (_p1, v1) = package(&identity, "so.tilo.demo", "1.0.0", &v1_files());
        let (_p2, v2) = package(&identity, "so.tilo.demo", "1.1.0", &v2_files());
        let root = scratch();
        let store = PackageStore::at(root.path());

        let first = store
            .install(full_tapp(&v1).as_slice(), admit_all, None)
            .unwrap();
        assert_eq!(first.blobs, 3);
        assert_eq!(first.blobs_already_stored, 0);

        let second = store
            .install(full_tapp(&v2).as_slice(), admit_all, None)
            .unwrap();
        assert_eq!(second.blobs, 4);
        assert_eq!(
            second.blobs_already_stored, 2,
            "the unchanged icon and strings were already in the store"
        );
        assert_eq!(
            second.replaced.as_ref().map(|row| row.version.as_str()),
            Some("1.0.0")
        );
        assert!(
            !first.package.payload_root.exists(),
            "the replaced version's tree is gone"
        );
        assert_eq!(store.registry().unwrap().entries().len(), 1);

        // Only v2's blobs remain; v1's old binary was collected.
        let mut stored: Vec<BlobHash> = Vec::new();
        for file in content_files(root.path()) {
            let name = file.file_stem().unwrap().to_str().unwrap().to_string();
            stored.push(BlobHash::from_hex(&name).unwrap());
        }
        let old_binary = BlobHash::of(&bytes(1, 300_000));
        assert!(!stored.contains(&old_binary), "v1's binary was evicted");
        assert!(stored.contains(&BlobHash::of(&bytes(3, 310_000))));

        // Where the filesystem can share extents, the materialised file is a
        // clone of the store's file rather than a second copy.
        if reflinks_supported(root.path()) {
            let icon = BlobHash::of(&bytes(2, 120_000));
            let stored_icon = root
                .path()
                .join("content/data")
                .join(format!("{}.data", icon.to_hex()));
            let materialised = second.package.payload_root.join("share/icon.png");
            assert_eq!(
                share_extents(&stored_icon, &materialised),
                Some(true),
                "the materialised icon shares extents with the stored blob"
            );
        }
    }
    // phase: reinstalling_the_same_package_repairs_a_damaged_tree
    {
        let identity = SigningIdentity::generate();
        let (_payload, packed) = package(&identity, "so.tilo.demo", "1.0.0", &v1_files());
        let root = scratch();
        let store = PackageStore::at(root.path());
        let first = store
            .install(full_tapp(&packed).as_slice(), admit_all, None)
            .unwrap();
        let damaged = first.package.payload_root.join("share/strings.ftl");
        std::fs::remove_file(&damaged).unwrap();
        let again = store
            .install(full_tapp(&packed).as_slice(), admit_all, None)
            .unwrap();
        assert_eq!(again.blobs_already_stored, 3);
        assert!(damaged.exists());
        assert!(
            package_store::integrity::verify(&again.package, &again.package.payload_root)
                .unwrap()
                .is_intact()
        );
    }
}

#[test]
fn thin_packages_fetch_only_missing_blobs_and_verify_them() {
    // phase: a_thin_update_fetches_only_the_blobs_the_store_lacks
    {
        let identity = SigningIdentity::generate();
        let (_p1, v1) = package(&identity, "so.tilo.demo", "1.0.0", &v1_files());
        let (_p2, v2) = package(&identity, "so.tilo.demo", "1.1.0", &v2_files());
        let root = scratch();
        let store = PackageStore::at(root.path());
        store
            .install(full_tapp(&v1).as_slice(), admit_all, None)
            .unwrap();

        let thin = thin_tapp(&v2);
        assert!(
            thin.len() < 16 * 1024,
            "thin package is {} bytes",
            thin.len()
        );
        let fetcher = CountingFetcher {
            source: &v2,
            asked: std::sync::Mutex::new(Vec::new()),
        };
        let installed = store
            .install(thin.as_slice(), admit_all, Some(&fetcher))
            .unwrap();
        let mut asked = fetcher.asked.into_inner().unwrap();
        asked.sort();
        let mut changed = vec![
            BlobHash::of(&bytes(3, 310_000)),
            BlobHash::of(&bytes(4, 90_000)),
        ];
        changed.sort();
        assert_eq!(asked, changed, "only the changed and added files travel");
        assert_eq!(installed.blobs_fetched, 2);
        assert_eq!(installed.blobs_already_stored, 2);
        let report =
            package_store::integrity::verify(&installed.package, &installed.package.payload_root)
                .unwrap();
        assert!(report.is_intact(), "{report:?}");
    }
    // phase: a_thin_package_without_a_source_for_its_bytes_is_refused
    {
        let identity = SigningIdentity::generate();
        let (_payload, packed) = package(&identity, "so.tilo.demo", "1.0.0", &v1_files());
        let root = scratch();
        let store = PackageStore::at(root.path());
        let error = store
            .install(thin_tapp(&packed).as_slice(), admit_all, None)
            .unwrap_err();
        assert!(
            matches!(&error, StoreError::MissingBlobs(missing) if missing.len() == 3),
            "{error}"
        );
        assert!(store.registry().unwrap().is_empty());
    }
    // phase: a_fetcher_that_serves_wrong_bytes_is_caught
    {
        struct Liar;
        impl BlobFetcher for Liar {
            fn fetch(&self, hashes: &[BlobHash], sink: &mut dyn BlobSink) -> std::io::Result<()> {
                for hash in hashes {
                    sink.accept(hash, 4, &mut &b"evil"[..])?;
                }
                Ok(())
            }
        }
        let identity = SigningIdentity::generate();
        let (_payload, packed) = package(&identity, "so.tilo.demo", "1.0.0", &v1_files());
        let root = scratch();
        let store = PackageStore::at(root.path());
        assert!(
            store
                .install(thin_tapp(&packed).as_slice(), admit_all, Some(&Liar))
                .is_err()
        );
        assert!(store.registry().unwrap().is_empty());
    }
    // phase: a_refused_package_stores_nothing
    {
        let identity = SigningIdentity::generate();
        let (_payload, packed) = package(&identity, "so.tilo.demo", "1.0.0", &v1_files());
        let root = scratch();
        let store = PackageStore::at(root.path());
        let error = store
            .install(
                full_tapp(&packed).as_slice(),
                |_meta| Err("publisher is not trusted".to_string()),
                None,
            )
            .unwrap_err();
        assert!(matches!(error, StoreError::Refused(_)), "{error}");
        assert!(store.registry().unwrap().is_empty());
        assert!(
            content_files(root.path()).is_empty(),
            "admission runs before any payload byte is stored"
        );
    }
}

#[test]
fn removal_evicts_unreferenced_blobs_and_honours_pins() {
    // phase: removal_evicts_only_blobs_no_other_package_references
    {
        let identity = SigningIdentity::generate();
        let shared = bytes(7, 200_000);
        let a_files = BTreeMap::from([
            ("bin/app", bytes(5, 150_000)),
            ("share/lib.so", shared.clone()),
        ]);
        let b_files = BTreeMap::from([
            ("bin/app", bytes(6, 150_000)),
            ("share/lib.so", shared.clone()),
        ]);
        let (_pa, a) = package(&identity, "so.tilo.a", "1.0.0", &a_files);
        let (_pb, b) = package(&identity, "so.tilo.b", "1.0.0", &b_files);
        let root = scratch();
        let store = PackageStore::at(root.path());
        store
            .install(full_tapp(&a).as_slice(), admit_all, None)
            .unwrap();
        let installed_b = store
            .install(full_tapp(&b).as_slice(), admit_all, None)
            .unwrap();
        assert_eq!(
            installed_b.blobs_already_stored, 1,
            "the library both apps ship is stored once"
        );
        assert_eq!(content_files(root.path()).len(), 3);

        let removed = store.remove("so.tilo.a", false).unwrap();
        assert_eq!(removed.evicted_blobs, 1, "only a's own binary goes");
        let shared_hex = BlobHash::of(&shared).to_hex();
        assert!(
            content_files(root.path()).iter().any(|path| path
                .file_stem()
                .unwrap()
                .to_string_lossy()
                == shared_hex)
        );
        assert!(!store.layout().payload(a.id()).exists());
        let row_b = store
            .registry()
            .unwrap()
            .find("so.tilo.b")
            .cloned()
            .unwrap();
        assert!(
            package_store::integrity::verify(&row_b, &row_b.payload_root)
                .unwrap()
                .is_intact()
        );

        store.remove("so.tilo.b", false).unwrap();
        assert!(content_files(root.path()).is_empty());
    }
    // phase: a_pinned_package_is_removed_only_when_forced_and_survives_a_sweep
    {
        let identity = SigningIdentity::generate();
        let (_pa, a) = package(&identity, "so.tilo.a", "1.0.0", &v1_files());
        let (_pb, b) = package(&identity, "so.tilo.b", "1.0.0", &v2_files());
        let root = scratch();
        let store = PackageStore::at(root.path());
        store
            .install(
                full_tapp(&a).as_slice(),
                |_| {
                    Ok(Admission {
                        pinned: true,
                        ..Admission::default()
                    })
                },
                None,
            )
            .unwrap();
        store
            .install(full_tapp(&b).as_slice(), admit_all, None)
            .unwrap();
        assert!(matches!(
            store.remove("so.tilo.a", false),
            Err(StoreError::Pinned(_))
        ));
        let swept = store.sweep(1).unwrap();
        assert_eq!(swept, vec!["so.tilo.b".to_string()]);
        assert!(store.registry().unwrap().find("so.tilo.a").is_some());
        store.remove("so.tilo.a", true).unwrap();
        assert!(store.registry().unwrap().is_empty());
    }
}

#[test]
fn explicit_cohort_install_preserves_held_packages_and_existing_pins() {
    let identity = SigningIdentity::generate();
    let (_first_tree, first) = package(&identity, "so.tilo.demo", "0.1.73018", &v1_files());
    let (_next_tree, next) = package(&identity, "so.tilo.demo", "0.1.0", &v2_files());
    let root = scratch();
    let store = PackageStore::at(root.path());
    store
        .install(
            full_tapp(&first).as_slice(),
            |_| {
                Ok(Admission {
                    held: true,
                    pinned: true,
                    ..Admission::default()
                })
            },
            None,
        )
        .unwrap();
    let original_registry = std::fs::read(store.layout().registry()).unwrap();
    let result = store
        .install_preserving_hold(full_tapp(&next).as_slice(), admit_all, None)
        .unwrap();
    assert!(
        matches!(result, package_store::InstallSelection::Preserved(ref row) if row.package_id() == Some(first.id())),
        "an automatic cohort update must preserve an explicitly held package"
    );
    assert_eq!(
        std::fs::read(store.layout().registry()).unwrap(),
        original_registry
    );
    assert_eq!(
        store
            .registry()
            .unwrap()
            .find("so.tilo.demo")
            .unwrap()
            .package_id(),
        Some(first.id())
    );
    store
        .update_row("so.tilo.demo", |row| row.held = false)
        .unwrap();
    let installed = store
        .install_preserving_hold(full_tapp(&next).as_slice(), admit_all, None)
        .unwrap();
    let package_store::InstallSelection::Installed(installed) = installed else {
        panic!("the nonheld signed cohort package must be installed");
    };
    assert_eq!(installed.package.package_id(), Some(next.id()));
    assert!(installed.package.pinned);
    assert!(!installed.package.held);
    assert_eq!(installed.replaced.unwrap().package_id(), Some(first.id()));
}

#[test]
fn retained_signed_inverse_survives_replacement_and_conditional_restore() {
    let identity = SigningIdentity::generate();
    let trusted_publisher = identity.public_key_bytes();
    let (old_tree, old) = package(&identity, "so.tilo.demo", "73018.0.0", &v1_files());
    let (_new_tree, new) = package(&identity, "so.tilo.demo", "0.1.0", &v2_files());
    let root = scratch();
    let store = PackageStore::at(root.path());
    let original = store
        .install(full_tapp(&old).as_slice(), admit_all, None)
        .unwrap();
    let (captured, archive) = store.export_current("so.tilo.demo", Vec::new()).unwrap();
    assert_eq!(captured, original.package);
    let selected = store
        .install_if_current(full_tapp(&new).as_slice(), Some(&captured), admit_all, None)
        .unwrap();
    let package_store::InstallSelection::Installed(selected) = selected else {
        panic!("the unheld captured row must be replaced");
    };
    assert!(
        !original.package.payload_root.exists(),
        "normal retirement still runs"
    );
    assert_eq!(store.collect_garbage().unwrap(), 0, "normal GC already ran");
    store.set_pinned("so.tilo.demo", true).unwrap();
    let restored = store
        .install_if_current(
            archive.as_slice(),
            Some(&selected.package),
            |meta| {
                assert_eq!(meta.id, old.id());
                assert!(matches!(
                    tilo_app_package::verify(meta, &[trusted_publisher]),
                    tilo_app_package::Verification::Trusted { .. }
                ));
                Ok(Admission::default())
            },
            None,
        )
        .unwrap();
    let package_store::InstallSelection::Installed(restored) = restored else {
        panic!("the owned current selection must restore");
    };
    assert_eq!(restored.package.package_id(), Some(old.id()));
    assert!(
        restored.package.pinned,
        "an intervening explicit pin is preserved"
    );
    assert!(
        package_store::integrity::verify(&restored.package, &restored.package.payload_root)
            .unwrap()
            .is_intact()
    );
    assert_eq!(tree(&restored.package.payload_root), tree(old_tree.path()));
}

#[test]
fn conditional_inverse_refuses_intervening_updates_and_preserves_holds() {
    let identity = SigningIdentity::generate();
    let (_old_tree, old) = package(&identity, "so.tilo.demo", "1.0.0", &v1_files());
    let (_new_tree, new) = package(&identity, "so.tilo.demo", "2.0.0", &v2_files());
    let root = scratch();
    let store = PackageStore::at(root.path());
    let original = store
        .install(full_tapp(&old).as_slice(), admit_all, None)
        .unwrap();
    let selected = store
        .install(full_tapp(&new).as_slice(), admit_all, None)
        .unwrap();
    let before = std::fs::read(store.layout().registry()).unwrap();
    let stale = store.install_if_current(
        full_tapp(&old).as_slice(),
        Some(&original.package),
        admit_all,
        None,
    );
    assert!(matches!(stale, Err(StoreError::Refused(_))));
    assert_eq!(std::fs::read(store.layout().registry()).unwrap(), before);
    let explicit = store
        .install(
            full_tapp(&new).as_slice(),
            |_| {
                Ok(Admission {
                    source: "explicit-user-update".into(),
                    ..Admission::default()
                })
            },
            None,
        )
        .unwrap();
    let same_root_update = store.install_if_current(
        full_tapp(&old).as_slice(),
        Some(&selected.package),
        admit_all,
        None,
    );
    assert!(matches!(same_root_update, Err(StoreError::Refused(_))));
    store
        .update_row("so.tilo.demo", |row| row.held = true)
        .unwrap();
    let held = store
        .registry()
        .unwrap()
        .find("so.tilo.demo")
        .unwrap()
        .clone();
    let preserved = store
        .install_if_current(
            full_tapp(&old).as_slice(),
            Some(&explicit.package),
            admit_all,
            None,
        )
        .unwrap();
    let package_store::InstallSelection::Preserved(preserved) = preserved else {
        panic!("explicit hold must block restoration");
    };
    assert_eq!(*preserved, held);
    let absent = store.install_if_current(full_tapp(&old).as_slice(), None, admit_all, None);
    assert!(matches!(absent, Err(StoreError::Refused(_))));
}

#[test]
fn inverse_export_refuses_corrupt_evidence_and_restore_keeps_admission() {
    let identity = SigningIdentity::generate();
    let (_tree, packed) = package(&identity, "so.tilo.demo", "1.0.0", &v1_files());
    let root = scratch();
    let store = PackageStore::at(root.path());
    let original = store
        .install(full_tapp(&packed).as_slice(), admit_all, None)
        .unwrap();
    let before = std::fs::read(store.layout().registry()).unwrap();
    let denied = store.install_if_current(
        full_tapp(&packed).as_slice(),
        Some(&original.package),
        |_| Err("publisher no longer admitted".into()),
        None,
    );
    assert!(matches!(denied, Err(StoreError::Refused(_))));
    assert_eq!(std::fs::read(store.layout().registry()).unwrap(), before);
    store
        .update_row("so.tilo.demo", |row| row.version = "forged-version".into())
        .unwrap();
    assert!(matches!(
        store.export_current("so.tilo.demo", Vec::new()),
        Err(StoreError::CorruptPackage(_, _))
    ));
    store
        .update_row("so.tilo.demo", |row| row.version = "1.0.0".into())
        .unwrap();
    let record = package_store::integrity::record_dir(&original.package);
    std::fs::write(
        record.join(package_store::layout::SIGNED_ROOT_FILE),
        b"not signed evidence",
    )
    .unwrap();
    assert!(matches!(
        store.export_current("so.tilo.demo", Vec::new()),
        Err(StoreError::CorruptPackage(_, _))
    ));
}

#[test]
fn conditional_inverse_removes_only_unpinned_unheld_owned_new_selection() {
    let identity = SigningIdentity::generate();
    let (_tree, packed) = package(&identity, "so.tilo.demo", "1.0.0", &v1_files());
    let root = scratch();
    let store = PackageStore::at(root.path());
    let selected = store
        .install(full_tapp(&packed).as_slice(), admit_all, None)
        .unwrap();
    store.set_pinned("so.tilo.demo", true).unwrap();
    assert!(
        store
            .remove_if_current(&selected.package)
            .unwrap()
            .is_none()
    );
    store.set_pinned("so.tilo.demo", false).unwrap();
    store
        .update_row("so.tilo.demo", |row| row.held = true)
        .unwrap();
    assert!(
        store
            .remove_if_current(&selected.package)
            .unwrap()
            .is_none()
    );
    store
        .update_row("so.tilo.demo", |row| row.held = false)
        .unwrap();
    let explicit = store
        .install(
            full_tapp(&packed).as_slice(),
            |_| {
                Ok(Admission {
                    source: "explicit-user-update".into(),
                    ..Admission::default()
                })
            },
            None,
        )
        .unwrap();
    assert!(
        store
            .remove_if_current(&selected.package)
            .unwrap()
            .is_none()
    );
    let removed = store.remove_if_current(&explicit.package).unwrap().unwrap();
    assert_eq!(removed.package.unwrap(), explicit.package);
    assert!(store.registry().unwrap().find("so.tilo.demo").is_none());
    assert!(!selected.package.payload_root.exists());
    assert!(
        store
            .remove_if_current(&explicit.package)
            .unwrap()
            .is_none()
    );
}
