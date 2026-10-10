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

/// Make `path` writable by its owner; the store may write records read-only.
fn make_writable(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
}

#[test]
fn sweep_removes_lru_until_under_cap_in_one_gc() {
    let identity = SigningIdentity::generate();
    let shared = bytes(40, 200_000);
    let own = |seed: u8| bytes(seed, 100_000);
    let (_pp, p) = package(
        &identity,
        "so.tilo.p",
        "1.0.0",
        &BTreeMap::from([("bin/app", own(41))]),
    );
    let (_pa, a) = package(
        &identity,
        "so.tilo.a",
        "1.0.0",
        &BTreeMap::from([("bin/app", own(42)), ("share/lib.so", shared.clone())]),
    );
    let (_pb, b) = package(
        &identity,
        "so.tilo.b",
        "1.0.0",
        &BTreeMap::from([("bin/app", own(43)), ("share/lib.so", shared)]),
    );
    let (_pc, c) = package(
        &identity,
        "so.tilo.c",
        "1.0.0",
        &BTreeMap::from([("bin/app", own(44))]),
    );
    let root = scratch();
    let store = PackageStore::at(root.path());
    store
        .install(
            full_tapp(&p).as_slice(),
            |_| {
                Ok(Admission {
                    pinned: true,
                    ..Admission::default()
                })
            },
            None,
        )
        .unwrap();
    for (packed, last_used) in [(&a, 10), (&b, 20), (&c, 30)] {
        let installed = store
            .install(full_tapp(packed).as_slice(), admit_all, None)
            .unwrap();
        store
            .update_row(&installed.package.id, |row| row.last_used = last_used)
            .unwrap();
    }
    // The pinned package is the least recently used of all.
    store
        .update_row("so.tilo.p", |row| row.last_used = 0)
        .unwrap();
    let before = store.disk_usage().unwrap();
    assert_eq!(
        before.total_bytes,
        4 * 100_000 + 200_000,
        "the library counts once"
    );
    assert_eq!(before.pinned_bytes, 100_000);
    assert_eq!(content_files(root.path()).len(), 5);

    // Evicting a frees only its own binary while b still ships the library,
    // so b must go too; c, newer, stays, and so does the pinned p.
    let removed = store.sweep(before.total_bytes - 250_000).unwrap();
    assert_eq!(
        removed,
        vec!["so.tilo.a".to_string(), "so.tilo.b".to_string()]
    );
    let registry = store.registry().unwrap();
    let left: Vec<&str> = registry
        .entries()
        .iter()
        .map(|row| row.id.as_str())
        .collect();
    assert_eq!(left, vec!["so.tilo.p", "so.tilo.c"]);
    assert_eq!(store.disk_usage().unwrap().total_bytes, 200_000);
    assert!(!store.layout().payload(a.id()).exists());
    assert!(!store.layout().payload(b.id()).exists());
    // The sweep collected garbage before returning: nothing is left over.
    assert_eq!(content_files(root.path()).len(), 2);
    assert_eq!(store.collect_garbage().unwrap(), 0);
    for row in registry.entries() {
        assert!(
            package_store::integrity::verify(row, &row.payload_root)
                .unwrap()
                .is_intact()
        );
    }
    // Under the cap already: nothing more goes, pinned or not.
    assert!(store.sweep(200_000).unwrap().is_empty());
}

#[test]
fn disk_usage_does_not_verify_signatures() {
    let identity = SigningIdentity::generate();
    let (_pa, a) = package(&identity, "so.tilo.a", "1.0.0", &v1_files());
    let root = scratch();
    let store = PackageStore::at(root.path());
    store
        .install(full_tapp(&a).as_slice(), admit_all, None)
        .unwrap();
    let before = store.disk_usage().unwrap();
    assert!(before.total_bytes > 0);

    let signed_root = store
        .layout()
        .meta_dir(a.id())
        .join(package_store::layout::SIGNED_ROOT_FILE);
    let mut corrupted = std::fs::read(&signed_root).unwrap();
    *corrupted.last_mut().unwrap() ^= 0x01;
    make_writable(&signed_root);
    std::fs::write(&signed_root, &corrupted).unwrap();

    // Accounting reads the collection; it never replays the signature.
    assert_eq!(store.disk_usage().unwrap(), before);
    // Verification, the explicit operation, still sees the damage.
    let row = store
        .registry()
        .unwrap()
        .find("so.tilo.a")
        .cloned()
        .unwrap();
    assert!(matches!(
        package_store::integrity::verify(&row, &row.payload_root),
        Err(package_store::integrity::IntegrityError::RootIdMismatch)
    ));
    // And a sweep under pressure still measures and evicts it.
    assert_eq!(store.sweep(1).unwrap(), vec!["so.tilo.a".to_string()]);
    assert!(content_files(root.path()).is_empty());
}

#[test]
fn gc_refuses_when_a_collection_is_unreadable() {
    let identity = SigningIdentity::generate();
    let (_pa, a) = package(&identity, "so.tilo.a", "1.0.0", &v1_files());
    let (_pb, b) = package(&identity, "so.tilo.b", "1.0.0", &v2_files());
    let root = scratch();
    let store = PackageStore::at(root.path());
    store
        .install(full_tapp(&a).as_slice(), admit_all, None)
        .unwrap();
    store
        .install(full_tapp(&b).as_slice(), admit_all, None)
        .unwrap();
    let stored = content_files(root.path()).len();

    std::fs::remove_file(
        store
            .layout()
            .meta_dir(a.id())
            .join(package_store::layout::COLLECTION_FILE),
    )
    .unwrap();

    // b's own blobs are garbage once it goes, but a's can no longer be told
    // apart from them, so nothing is evicted.
    assert!(matches!(
        store.remove("so.tilo.b", false),
        Err(StoreError::CorruptPackage(ref id, _)) if id == "so.tilo.a"
    ));
    assert_eq!(content_files(root.path()).len(), stored);
    assert!(matches!(
        store.collect_garbage(),
        Err(StoreError::CorruptPackage(ref id, _)) if id == "so.tilo.a"
    ));
    assert_eq!(content_files(root.path()).len(), stored);
}
