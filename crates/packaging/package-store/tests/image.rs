//! Environment images: fetching, trusting and materialising `env-runtime`
//! and `tool` packages as read-only trees.

use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use package_store::{
    Admission, CertifiedFleet, FleetSource, ImageShelf, ImageTrust, MaterializeError, PackageStore,
    Store, StoreError, check_tool_runtime, materialized_tree, tool_runtime,
};
use tilo_app_package::manifest::Arch;
use tilo_app_package::trust::{RevocationList, RevokedKey};
use tilo_app_package::{
    BlobFetcher, BlobHash, BlobSink, BlobSource, ImageSpec, Kind, PackageFetcher, PackageId,
    Packed, SigningIdentity, seal_image,
};
use tilo_user_identity::DeviceCapabilities;

use crate::support::{bytes, full_tapp, package, scratch, thin_tapp};

/// A registry: thin packages by id, and their payload blobs.
#[derive(Default)]
struct Registry {
    packages: BTreeMap<PackageId, Vec<u8>>,
    blobs: BTreeMap<BlobHash, Vec<u8>>,
    package_fetches: AtomicUsize,
}

impl Registry {
    fn publish(&mut self, packed: &Packed) {
        self.publish_as(packed.meta.id, packed);
    }

    /// Serve `packed` when asked for `id`, as a lying registry would.
    fn publish_as(&mut self, id: PackageId, packed: &Packed) {
        self.packages.insert(id, thin_tapp(packed));
        for hash in packed.meta.payload_hashes() {
            let mut blob = packed.open_blob(&hash).unwrap().unwrap();
            let mut buf = Vec::new();
            blob.reader.read_to_end(&mut buf).unwrap();
            self.blobs.insert(hash, buf);
        }
    }
}

impl BlobFetcher for Registry {
    fn fetch(&self, hashes: &[BlobHash], sink: &mut dyn BlobSink) -> std::io::Result<()> {
        for hash in hashes {
            let blob = self
                .blobs
                .get(hash)
                .ok_or_else(|| std::io::Error::other("no such blob"))?;
            sink.accept(hash, blob.len() as u64, &mut blob.as_slice())?;
        }
        Ok(())
    }
}

impl PackageFetcher for Registry {
    fn fetch_package(&self, id: &PackageId) -> std::io::Result<Option<Vec<u8>>> {
        self.package_fetches.fetch_add(1, Ordering::Relaxed);
        Ok(self.packages.get(id).cloned())
    }
}

/// A shared handle, so a test can keep counting what the store fetched.
#[derive(Clone)]
struct Shared(std::sync::Arc<Registry>);

impl BlobFetcher for Shared {
    fn fetch(&self, hashes: &[BlobHash], sink: &mut dyn BlobSink) -> std::io::Result<()> {
        self.0.fetch(hashes, sink)
    }
}

impl PackageFetcher for Shared {
    fn fetch_package(&self, id: &PackageId) -> std::io::Result<Option<Vec<u8>>> {
        self.0.fetch_package(id)
    }
}

/// A small userland: a shell, a library and the links and directories a
/// real one has.
fn runtime_tree(root: &Path) {
    let write = |rel: &str, content: &[u8], mode: u32| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    write("usr/bin/bash", &bytes(1, 40_000), 0o755);
    write("usr/lib64/libc.so.6", &bytes(2, 60_000), 0o755);
    write("etc/os-release", b"NAME=Fedora\n", 0o644);
    std::os::unix::fs::symlink("bash", root.join("usr/bin/sh")).unwrap();
    std::os::unix::fs::symlink("../usr/lib64", root.join("etc/lib-link")).unwrap();
    std::fs::create_dir_all(root.join("usr/share/empty")).unwrap();
}

fn seal(
    kind: Kind,
    id: &str,
    runtime: Option<PackageId>,
    identity: &SigningIdentity,
    fill: impl FnOnce(&Path),
) -> (tempfile::TempDir, Packed) {
    let dir = scratch();
    fill(dir.path());
    let spec = ImageSpec {
        kind,
        id: id.into(),
        version: "44.1.0".into(),
        name: None,
        arch: Arch::X86_64,
        runtime,
        packages: vec!["bash-0:5.2.37-1.fc44.x86_64".into()],
    };
    let packed = seal_image(dir.path(), &spec, identity).unwrap();
    (dir, packed)
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn a_fetched_runtime_materialises_its_whole_tree_read_only() {
    let ci = SigningIdentity::generate();
    let (_src, runtime) = seal(
        Kind::EnvRuntime,
        "so.tilo.env-runtime",
        None,
        &ci,
        runtime_tree,
    );
    let mut registry = Registry::default();
    registry.publish(&runtime);
    let registry = Shared(std::sync::Arc::new(registry));

    let root = scratch();
    let store = Store::new(PackageStore::at(root.path()))
        .with_trust(ImageTrust::new().with_publisher(ci.public_key_bytes()))
        .with_fetcher(registry.clone());
    let tree = materialized_tree(&store, &runtime.meta.id).unwrap();
    assert_eq!(tree, store.user().layout().payload(runtime.meta.id));

    assert_eq!(
        std::fs::read(tree.join("usr/bin/bash")).unwrap(),
        bytes(1, 40_000)
    );
    assert_eq!(mode(&tree.join("usr/bin/bash")), 0o555);
    assert_eq!(mode(&tree.join("usr/lib64/libc.so.6")), 0o555);
    assert_eq!(mode(&tree.join("etc/os-release")), 0o444);
    assert_eq!(
        std::fs::read_link(tree.join("usr/bin/sh")).unwrap(),
        Path::new("bash")
    );
    assert_eq!(
        std::fs::read_link(tree.join("etc/lib-link")).unwrap(),
        Path::new("../usr/lib64")
    );
    assert!(tree.join("usr/share/empty").is_dir());

    // A second resolution is local: nothing is fetched or rewritten.
    let inode =
        std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(tree.join("usr/bin/bash")).unwrap());
    assert_eq!(materialized_tree(&store, &runtime.meta.id).unwrap(), tree);
    assert_eq!(registry.0.package_fetches.load(Ordering::Relaxed), 1);
    assert_eq!(
        std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(tree.join("usr/bin/bash")).unwrap()),
        inode
    );

    // Images have no registry row, so nothing can launch them.
    assert!(store.user().registry().unwrap().entries().is_empty());
    assert_eq!(store.images().unwrap().len(), 1);
}

#[test]
fn untrusted_revoked_and_substituted_images_are_refused() {
    let ci = SigningIdentity::generate();
    let stranger = SigningIdentity::generate();
    let (_a, trusted) = seal(
        Kind::EnvRuntime,
        "so.tilo.env-runtime",
        None,
        &ci,
        runtime_tree,
    );
    let (_b, foreign) = seal(
        Kind::EnvRuntime,
        "so.example.env",
        None,
        &stranger,
        runtime_tree,
    );
    let missing = PackageId::from_bytes([7; 32]);
    let mut registry = Registry::default();
    registry.publish(&trusted);
    registry.publish(&foreign);
    // Asked for `missing`, this registry answers with another package.
    registry.publish_as(missing, &trusted);

    let root = scratch();
    let trust = ImageTrust::new().with_publisher(ci.public_key_bytes());
    let store = Store::new(PackageStore::at(root.path()))
        .with_trust(trust.clone())
        .with_fetcher(registry);

    let refused = materialized_tree(&store, &foreign.meta.id).unwrap_err();
    assert!(
        matches!(refused, MaterializeError::Untrusted { .. }),
        "{refused}"
    );
    assert!(store.user().image(foreign.meta.id).unwrap().is_none());

    let substituted = materialized_tree(&store, &missing).unwrap_err();
    assert!(
        matches!(substituted, MaterializeError::Fetch { .. }),
        "{substituted}"
    );

    let offline = Store::new(PackageStore::at(root.path()));
    assert!(matches!(
        materialized_tree(&offline, &trusted.meta.id),
        Err(MaterializeError::NotFound(_))
    ));

    let revoked = RevocationList {
        revoked: vec![RevokedKey {
            pubkey_hex: hex::encode(ci.public_key_bytes()),
            reason: "key leaked".into(),
        }],
    };
    let store = store.with_trust(trust.with_revoked(revoked));
    assert!(matches!(
        materialized_tree(&store, &trusted.meta.id),
        Err(MaterializeError::Revoked { .. })
    ));
}

#[test]
fn a_device_signed_tool_is_bound_to_its_runtime() {
    let ci = SigningIdentity::generate();
    let device = SigningIdentity::generate();
    let (_a, runtime) = seal(
        Kind::EnvRuntime,
        "so.tilo.env-runtime",
        None,
        &ci,
        runtime_tree,
    );
    let (_b, tool) = seal(
        Kind::Tool,
        "so.tilo.tool.pandoc",
        Some(runtime.meta.id),
        &device,
        |root| {
            std::fs::create_dir_all(root.join("usr/bin")).unwrap();
            std::fs::write(root.join("usr/bin/pandoc"), bytes(9, 5_000)).unwrap();
            std::fs::set_permissions(
                root.join("usr/bin/pandoc"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        },
    );
    let mut registry = Registry::default();
    registry.publish(&runtime);
    registry.publish(&tool);

    let root = scratch();
    let store = Store::new(PackageStore::at(root.path()))
        .with_trust(
            ImageTrust::new()
                .with_publisher(ci.public_key_bytes())
                .with_publisher(device.public_key_bytes()),
        )
        .with_fetcher(registry);

    check_tool_runtime(&store, &tool.meta.id, &runtime.meta.id).unwrap();
    assert_eq!(
        tool_runtime(&store, &tool.meta.id).unwrap(),
        runtime.meta.id
    );
    let tree = materialized_tree(&store, &tool.meta.id).unwrap();
    assert_eq!(mode(&tree.join("usr/bin/pandoc")), 0o555);

    let other = PackageId::from_bytes([1; 32]);
    assert!(matches!(
        check_tool_runtime(&store, &tool.meta.id, &other),
        Err(MaterializeError::ToolRuntimeMismatch { .. })
    ));
    assert!(matches!(
        tool_runtime(&store, &runtime.meta.id),
        Err(MaterializeError::NotATool(_))
    ));
}

#[test]
fn system_images_open_offline_and_images_are_not_applications() {
    let ci = SigningIdentity::generate();
    let (_a, runtime) = seal(
        Kind::EnvRuntime,
        "so.tilo.env-runtime",
        None,
        &ci,
        runtime_tree,
    );

    // The OS image installs the runtime into its read-only store.
    let system = scratch();
    let shipped = PackageStore::at(system.path())
        .install_image(
            full_tapp(&runtime).as_slice(),
            |_| Ok(Admission::default()),
            None,
        )
        .unwrap();
    assert_eq!(shipped.id, runtime.meta.id);

    // No registry and no trusted publisher: the system copy is used as is.
    let user = scratch();
    let store = Store::new(PackageStore::at(user.path()))
        .with_system(package_store::Layout::new(system.path()));
    let tree = materialized_tree(&store, &runtime.meta.id).unwrap();
    assert!(tree.starts_with(system.path()));
    assert!(tree.join("usr/bin/sh").is_symlink());

    // An image is not an application, and an application not an image.
    let apps = PackageStore::at(user.path());
    let refused = apps
        .install(
            full_tapp(&runtime).as_slice(),
            |_| Ok(Admission::default()),
            None,
        )
        .unwrap_err();
    assert!(matches!(refused, StoreError::Refused(_)), "{refused}");
    let app_identity = SigningIdentity::generate();
    let (_c, app) = package(
        &app_identity,
        "so.tilo.example",
        "1.0.0",
        &BTreeMap::from([("bin/app", bytes(5, 1_000))]),
    );
    let refused = apps
        .install_image(
            full_tapp(&app).as_slice(),
            |_| Ok(Admission::default()),
            None,
        )
        .unwrap_err();
    assert!(matches!(refused, StoreError::Refused(_)), "{refused}");

    // An image in the user's store survives an application's removal and
    // garbage collection, and goes with its own removal.
    apps.install_image(
        full_tapp(&runtime).as_slice(),
        |_| Ok(Admission::default()),
        None,
    )
    .unwrap();
    apps.install(
        full_tapp(&app).as_slice(),
        |_| Ok(Admission::default()),
        None,
    )
    .unwrap();
    apps.remove("so.tilo.example", true).unwrap();
    assert!(apps.image(runtime.meta.id).unwrap().is_some());
    let removed = apps.remove_image(runtime.meta.id).unwrap();
    assert!(removed.evicted_blobs >= runtime.meta.payload_hashes().len());
    assert!(apps.image(runtime.meta.id).unwrap().is_none());
    assert!(!apps.layout().payload(runtime.meta.id).exists());
}

/// A user's fleet as their devices keep it: changed by enrolment and
/// removal while a store is open, and read by the store on every use.
#[derive(Debug, Clone)]
struct LiveFleet(std::sync::Arc<std::sync::Mutex<CertifiedFleet>>);

impl FleetSource for LiveFleet {
    fn fleet(&self) -> Result<CertifiedFleet, String> {
        Ok(self.0.lock().unwrap().clone())
    }
}

/// A fleet that cannot be read.
#[derive(Debug)]
struct Unreadable;

impl FleetSource for Unreadable {
    fn fleet(&self) -> Result<CertifiedFleet, String> {
        Err("the fleet file is corrupt".into())
    }
}

/// A person: their identity root and their fleet.
struct Person {
    root: ed25519_dalek::SigningKey,
    fleet: LiveFleet,
}

impl Person {
    fn new() -> Self {
        let (root, user) = tilo_user_identity::UserIdentity::generate("person", 1);
        Self {
            root,
            fleet: LiveFleet(std::sync::Arc::new(std::sync::Mutex::new(
                CertifiedFleet::new(user.public_key),
            ))),
        }
    }

    /// The identity root's certificate for `device`.
    fn certificate(
        &self,
        device: &SigningIdentity,
        capabilities: DeviceCapabilities,
    ) -> tilo_user_identity::DeviceCert {
        let mut cert = tilo_user_identity::DeviceCert {
            user_public_key: self.root.verifying_key().to_bytes(),
            device_public_key: device.public_key_bytes(),
            device_label: "device".into(),
            capabilities,
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut cert, &self.root).unwrap();
        cert
    }

    fn enrol(&self, device: &SigningIdentity, capabilities: DeviceCapabilities) {
        let cert = self.certificate(device, capabilities);
        self.fleet.0.lock().unwrap().certify(cert).unwrap();
    }

    fn remove(&self, device: &SigningIdentity) {
        let mut entry = tilo_user_identity::RevocationEntry {
            user_public_key: self.root.verifying_key().to_bytes(),
            revoked_device_public_key: device.public_key_bytes(),
            revoked_at_ns: 2,
            reason: tilo_user_identity::RevocationReason::Lost,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut entry, &self.root).unwrap();
        self.fleet.0.lock().unwrap().revoke(entry).unwrap();
    }
}

fn pandoc(runtime: PackageId, signer: &SigningIdentity, seed: u8) -> (tempfile::TempDir, Packed) {
    seal(
        Kind::Tool,
        "so.tilo.tool.pandoc",
        Some(runtime),
        signer,
        |root| {
            std::fs::create_dir_all(root.join("usr/bin")).unwrap();
            std::fs::write(root.join("usr/bin/pandoc"), bytes(seed, 5_000)).unwrap();
        },
    )
}

#[test]
fn images_from_the_users_certified_devices_are_trusted_while_they_are_certified() {
    let ci = SigningIdentity::generate();
    let ada = Person::new();
    let bob = Person::new();
    let laptop = SigningIdentity::generate();
    let phone = SigningIdentity::generate();
    let home_relay = SigningIdentity::generate();
    let open_relay = SigningIdentity::generate();
    let bobs_laptop = SigningIdentity::generate();
    ada.enrol(&laptop, DeviceCapabilities::primary());
    ada.enrol(&phone, DeviceCapabilities::member());
    ada.enrol(&home_relay, DeviceCapabilities::host());
    bob.enrol(&bobs_laptop, DeviceCapabilities::primary());

    let runtime = PackageId::from_bytes([3; 32]);
    let signers = [&laptop, &phone, &home_relay, &open_relay, &bobs_laptop];
    let built: Vec<(tempfile::TempDir, Packed)> = (20u8..)
        .zip(signers)
        .map(|(seed, signer)| pandoc(runtime, signer, seed))
        .collect();
    let id = |index: usize| built[index].1.meta.id;
    let mut registry = Registry::default();
    for (_, packed) in &built {
        registry.publish(packed);
    }
    let (_c, catalogued) = pandoc(runtime, &ci, 40);
    registry.publish(&catalogued);

    let root = scratch();
    let store = Store::new(PackageStore::at(root.path()))
        .with_trust(
            ImageTrust::new()
                .with_publisher(ci.public_key_bytes())
                .with_fleet(ada.fleet.clone()),
        )
        .with_fetcher(registry);

    // Ada's devices, her home relay included: it holds her certificate.
    for own in [0, 1, 2] {
        materialized_tree(&store, &id(own)).unwrap();
    }
    // A relay without a certificate is anyone: only CI-signed images open.
    let refused = materialized_tree(&store, &id(3)).unwrap_err();
    assert!(
        matches!(refused, MaterializeError::Untrusted { .. }),
        "{refused}"
    );
    materialized_tree(&store, &catalogued.meta.id).unwrap();
    // Bob's device is certified, by Bob: never trusted for Ada.
    let refused = materialized_tree(&store, &id(4)).unwrap_err();
    assert!(
        matches!(refused, MaterializeError::Untrusted { .. }),
        "{refused}"
    );
    assert!(store.user().image(id(4)).unwrap().is_none());
    // Ada's certificate does not make her laptop one of Bob's.
    let theirs = ada.certificate(&laptop, DeviceCapabilities::primary());
    assert!(bob.fleet.0.lock().unwrap().certify(theirs).is_err());

    // Ada removes her phone: its image, already here, no longer opens, and
    // neither does a later one from it, wherever it is fetched.
    ada.remove(&phone);
    let refused = materialized_tree(&store, &id(1)).unwrap_err();
    assert!(
        matches!(refused, MaterializeError::Revoked { .. }),
        "{refused}"
    );
    materialized_tree(&store, &id(0)).unwrap();
    let (_d, later) = pandoc(runtime, &phone, 50);
    let mut registry = Registry::default();
    registry.publish(&later);
    let fresh = scratch();
    let elsewhere = Store::new(PackageStore::at(fresh.path()))
        .with_trust(store.trust().clone())
        .with_fetcher(registry);
    let refused = materialized_tree(&elsewhere, &later.meta.id).unwrap_err();
    assert!(
        matches!(refused, MaterializeError::Revoked { .. }),
        "{refused}"
    );
    assert!(elsewhere.user().image(later.meta.id).unwrap().is_none());

    // A fleet that cannot be read vouches for nothing.
    let unreadable = store.with_trust(ImageTrust::new().with_fleet(Unreadable));
    let refused = materialized_tree(&unreadable, &id(0)).unwrap_err();
    assert!(
        matches!(refused, MaterializeError::FleetUnavailable { .. }),
        "{refused}"
    );
}

#[test]
fn an_image_one_device_shelved_is_fetched_by_another_instead_of_rebuilt() {
    let ada = Person::new();
    let laptop = SigningIdentity::generate();
    ada.enrol(&laptop, DeviceCapabilities::primary());
    let runtime = PackageId::from_bytes([3; 32]);
    let (_a, tool) = pandoc(runtime, &laptop, 60);

    // The laptop built the tool into its store, and shelves it.
    let laptop_root = scratch();
    let laptop_store = PackageStore::at(laptop_root.path());
    laptop_store
        .install_image(
            full_tapp(&tool).as_slice(),
            |_| Ok(Admission::default()),
            None,
        )
        .unwrap();
    let synced = scratch();
    let shelf = ImageShelf::at(synced.path().join("images"));
    let shelved = shelf.put(&laptop_store, tool.meta.id).unwrap();
    assert_eq!(shelved, shelf.path(&tool.meta.id));
    shelf.put(&laptop_store, tool.meta.id).unwrap();

    // The relay reads the same shelf; its registry, asked only after the
    // shelf, is offline and does not matter.
    let relay_root = scratch();
    let relay = Store::new(PackageStore::at(relay_root.path()))
        .with_trust(ImageTrust::new().with_fleet(ada.fleet))
        .with_shelf(shelf.clone())
        .with_fetcher(Offline);
    let tree = materialized_tree(&relay, &tool.meta.id).unwrap();
    assert_eq!(
        std::fs::read(tree.join("usr/bin/pandoc")).unwrap(),
        bytes(60, 5_000)
    );

    // A shelf is not trusted: a stranger's image on it is refused, and one
    // filed under another image's name is not that image.
    let stranger = SigningIdentity::generate();
    let (_b, planted) = pandoc(runtime, &stranger, 61);
    std::fs::write(shelf.path(&planted.meta.id), full_tapp(&planted)).unwrap();
    let refused = materialized_tree(&relay, &planted.meta.id).unwrap_err();
    assert!(
        matches!(refused, MaterializeError::Untrusted { .. }),
        "{refused}"
    );
    let misfiled = PackageId::from_bytes([8; 32]);
    std::fs::write(shelf.path(&misfiled), full_tapp(&tool)).unwrap();
    let refused = materialized_tree(&relay, &misfiled).unwrap_err();
    assert!(
        matches!(refused, MaterializeError::Fetch { .. }),
        "{refused}"
    );
    // Missing everywhere: an unreachable source says so, rather than
    // passing for "nobody has it".
    let nowhere = PackageId::from_bytes([9; 32]);
    let refused = materialized_tree(&relay, &nowhere).unwrap_err();
    assert!(
        matches!(&refused, MaterializeError::Fetch { detail, .. } if detail.starts_with("registry")),
        "{refused}"
    );
    let shelf_only = Store::new(PackageStore::at(relay_root.path())).with_shelf(shelf);
    assert!(matches!(
        materialized_tree(&shelf_only, &nowhere),
        Err(MaterializeError::NotFound(_))
    ));
}

/// A registry that cannot be reached.
struct Offline;

impl BlobFetcher for Offline {
    fn fetch(&self, _hashes: &[BlobHash], _sink: &mut dyn BlobSink) -> std::io::Result<()> {
        Err(std::io::Error::other("offline"))
    }
}

impl PackageFetcher for Offline {
    fn fetch_package(&self, _id: &PackageId) -> std::io::Result<Option<Vec<u8>>> {
        Err(std::io::Error::other("offline"))
    }
}

#[test]
fn system_image_overlay_lowers_are_materialized_locally_with_current_trust_and_integrity() {
    let ci = SigningIdentity::generate();
    let (_source, runtime) = seal(
        Kind::EnvRuntime,
        "so.tilo.env-runtime",
        None,
        &ci,
        runtime_tree,
    );
    let system = scratch();
    PackageStore::at(system.path())
        .install_image(
            full_tapp(&runtime).as_slice(),
            |_| Ok(Admission::default()),
            None,
        )
        .unwrap();
    let user = scratch();
    let store = Store::new(PackageStore::at(user.path()))
        .with_system(package_store::Layout::new(system.path()));
    let source = materialized_tree(&store, &runtime.meta.id).unwrap();
    let local = package_store::local_materialized_tree(&store, &runtime.meta.id).unwrap();
    assert!(local.starts_with(user.path()));
    assert_ne!(local, source);
    assert_eq!(
        std::fs::read(local.join("usr/bin/bash")).unwrap(),
        std::fs::read(source.join("usr/bin/bash")).unwrap()
    );
    assert_eq!(mode(&local.join("usr/bin/bash")), 0o555);
    assert_eq!(mode(&local.join("etc/os-release")), 0o444);
    assert_eq!(
        std::fs::read_link(local.join("usr/bin/sh")).unwrap(),
        Path::new("bash")
    );
    assert!(local.join("usr/share/empty").is_dir());
    assert_eq!(
        package_store::local_materialized_tree(&store, &runtime.meta.id).unwrap(),
        local
    );

    let revoked = RevocationList {
        revoked: vec![RevokedKey {
            pubkey_hex: hex::encode(ci.public_key_bytes()),
            reason: "withdrawn".into(),
        }],
    };
    let untrusted = store
        .clone()
        .with_trust(ImageTrust::new().with_revoked(revoked));
    assert!(matches!(
        package_store::local_materialized_tree(&untrusted, &runtime.meta.id),
        Err(MaterializeError::Revoked { .. })
    ));

    std::fs::write(local.join("etc/unsigned"), b"injected config").unwrap();
    assert!(package_store::local_materialized_tree(&store, &runtime.meta.id).is_err());
    std::fs::remove_file(local.join("etc/unsigned")).unwrap();
    std::fs::set_permissions(
        local.join("etc/os-release"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    std::fs::write(local.join("etc/os-release"), b"corrupted cached bytes").unwrap();
    assert!(package_store::local_materialized_tree(&store, &runtime.meta.id).is_err());
    assert_eq!(
        std::fs::read(source.join("etc/os-release")).unwrap(),
        b"NAME=Fedora\n"
    );
}

#[test]
fn modified_system_image_is_not_imported_as_a_signed_overlay_lower() {
    let ci = SigningIdentity::generate();
    let (_source, runtime) = seal(
        Kind::EnvRuntime,
        "so.tilo.env-runtime",
        None,
        &ci,
        runtime_tree,
    );
    let system = scratch();
    PackageStore::at(system.path())
        .install_image(
            full_tapp(&runtime).as_slice(),
            |_| Ok(Admission::default()),
            None,
        )
        .unwrap();
    let user = scratch();
    let store = Store::new(PackageStore::at(user.path()))
        .with_system(package_store::Layout::new(system.path()));
    let source = materialized_tree(&store, &runtime.meta.id).unwrap();
    std::fs::set_permissions(
        source.join("usr/bin/bash"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::fs::write(source.join("usr/bin/bash"), b"unsigned shell").unwrap();
    assert!(package_store::local_materialized_tree(&store, &runtime.meta.id).is_err());
    assert!(!store.user().layout().payload(runtime.meta.id).exists());
}

#[cfg(target_os = "linux")]
fn watch_staging(root: &Path) -> std::fs::File {
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::CString::new(root.as_os_str().as_bytes()).unwrap();
    // SAFETY: no pointers are passed; the returned owned descriptor is checked.
    let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
    assert!(fd >= 0, "inotify must observe the production staging path");
    // SAFETY: fd is freshly owned and name is a live NUL-terminated path.
    let watch = unsafe { libc::inotify_add_watch(fd, name.as_ptr(), libc::IN_CREATE) };
    // SAFETY: fd is valid and its ownership is transferred exactly once.
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    assert!(
        watch >= 0,
        "staging watch must be installed before resolution"
    );
    file
}

#[cfg(target_os = "linux")]
fn created_staging_files(mut watch: std::fs::File) -> Vec<String> {
    let mut names = Vec::new();
    let mut buffer = [0u8; 4096];
    let mut total = 0usize;
    loop {
        let size = match watch.read(&mut buffer) {
            Ok(size) => size,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            result => panic!("staging observation failed: {result:?}"),
        };
        assert!(size > 0);
        total = total.checked_add(size).unwrap();
        assert!(total <= 64 * 1024, "staging observation must be bounded");
        let mut events = &buffer[..size];
        while !events.is_empty() {
            assert!(events.len() >= 16, "inotify event header is complete");
            let (header, tail) = events.split_at(16);
            let mask = u32::from_ne_bytes(header[4..8].try_into().unwrap());
            let len = u32::from_ne_bytes(header[12..16].try_into().unwrap()) as usize;
            assert_eq!(mask & libc::IN_Q_OVERFLOW, 0, "no lost staging events");
            assert!(len <= tail.len(), "inotify event name is complete");
            let (name, remaining) = tail.split_at(len);
            if mask & libc::IN_CREATE != 0 && mask & libc::IN_ISDIR == 0 {
                let name = std::ffi::CStr::from_bytes_until_nul(name).unwrap();
                names.push(name.to_str().unwrap().to_owned());
            }
            events = remaining;
        }
    }
    assert!(total > 0, "the watch must observe actual materialization");
    names
}

fn runtime_with_large_blob(root: &Path) {
    runtime_tree(root);
    std::fs::write(root.join("usr/lib64/cached.bin"), bytes(9, 300_000)).unwrap();
}

fn forget_materialized_image(store: &PackageStore, id: PackageId) {
    std::fs::remove_dir_all(store.layout().payload(id)).unwrap();
    std::fs::remove_dir_all(store.layout().meta_dir(id)).unwrap();
}

#[test]
#[cfg(target_os = "linux")]
fn local_overlay_reuses_complete_content_without_restaging_verified_source() {
    let identity = SigningIdentity::generate();
    let (_source, image) = seal(
        Kind::EnvRuntime,
        "so.tilo.env-runtime",
        None,
        &identity,
        runtime_with_large_blob,
    );
    let system = scratch();
    PackageStore::at(system.path())
        .install_image(
            full_tapp(&image).as_slice(),
            |_| Ok(Admission::default()),
            None,
        )
        .unwrap();
    let user = scratch();
    let packages = PackageStore::at(user.path());
    packages
        .install_image(
            full_tapp(&image).as_slice(),
            |_| Ok(Admission::default()),
            None,
        )
        .unwrap();
    forget_materialized_image(&packages, image.meta.id);
    let watch = watch_staging(&packages.layout().tmp());
    let store = Store::new(packages).with_system(package_store::Layout::new(system.path()));
    let local = package_store::local_materialized_tree(&store, &image.meta.id).unwrap();
    for (relative, expected) in &image.meta.collection.entries {
        assert_eq!(
            BlobHash::of_reader(&mut std::fs::File::open(local.join(relative)).unwrap())
                .unwrap()
                .0,
            *expected
        );
    }
    assert_eq!(mode(&local.join("usr/bin/bash")), 0o555);
    assert_eq!(
        std::fs::read_link(local.join("usr/bin/sh")).unwrap(),
        Path::new("bash")
    );
    assert!(local.join("usr/share/empty").is_dir());
    assert_eq!(
        created_staging_files(watch),
        Vec::<String>::new(),
        "complete signed blobs must not be copied into ingestion staging again"
    );
}

fn runtime_with_two_large_blobs(root: &Path) {
    runtime_with_large_blob(root);
    std::fs::write(root.join("usr/lib64/missing.bin"), bytes(10, 310_000)).unwrap();
}

#[test]
#[cfg(target_os = "linux")]
fn local_overlay_imports_only_missing_content_from_partial_cache() {
    let identity = SigningIdentity::generate();
    let (_source, image) = seal(
        Kind::EnvRuntime,
        "so.tilo.env-runtime",
        None,
        &identity,
        runtime_with_two_large_blobs,
    );
    let system = scratch();
    PackageStore::at(system.path())
        .install_image(
            full_tapp(&image).as_slice(),
            |_| Ok(Admission::default()),
            None,
        )
        .unwrap();
    let user = scratch();
    let packages = PackageStore::at(user.path());
    packages
        .install_image(
            full_tapp(&image).as_slice(),
            |_| Ok(Admission::default()),
            None,
        )
        .unwrap();
    forget_materialized_image(&packages, image.meta.id);
    let missing = image.meta.collection.entries["usr/lib64/missing.bin"];
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let content = fs::store::ContentStore::open(&packages.layout().content())
            .await
            .unwrap();
        content
            .evict_blobs(&std::collections::HashSet::from([
                iroh_blobs::Hash::from_bytes(*missing.as_bytes()),
            ]))
            .await
            .unwrap();
        assert_eq!(
            content
                .complete_len(iroh_blobs::Hash::from_bytes(*missing.as_bytes()))
                .await
                .unwrap(),
            None
        );
        content.close().await.unwrap();
    });
    let watch = watch_staging(&packages.layout().tmp());
    let store = Store::new(packages).with_system(package_store::Layout::new(system.path()));
    let local = package_store::local_materialized_tree(&store, &image.meta.id).unwrap();
    assert_eq!(
        std::fs::read(local.join("usr/lib64/missing.bin")).unwrap(),
        bytes(10, 310_000)
    );
    assert_eq!(
        std::fs::read(local.join("usr/lib64/cached.bin")).unwrap(),
        bytes(9, 300_000)
    );
    assert_eq!(
        created_staging_files(watch).len(),
        1,
        "only the missing large blob needs ingestion staging"
    );
}

#[test]
fn local_overlay_still_checks_modified_and_missing_source_with_cached_content() {
    for missing in [false, true] {
        let identity = SigningIdentity::generate();
        let (_source, image) = seal(
            Kind::EnvRuntime,
            "so.tilo.env-runtime",
            None,
            &identity,
            runtime_with_large_blob,
        );
        let system = scratch();
        let source_store = PackageStore::at(system.path());
        source_store
            .install_image(
                full_tapp(&image).as_slice(),
                |_| Ok(Admission::default()),
                None,
            )
            .unwrap();
        let user = scratch();
        let packages = PackageStore::at(user.path());
        packages
            .install_image(
                full_tapp(&image).as_slice(),
                |_| Ok(Admission::default()),
                None,
            )
            .unwrap();
        forget_materialized_image(&packages, image.meta.id);
        let source = source_store
            .layout()
            .payload(image.meta.id)
            .join("usr/lib64/cached.bin");
        if missing {
            std::fs::remove_file(&source).unwrap();
        } else {
            std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o644)).unwrap();
            std::fs::write(&source, bytes(11, 300_000)).unwrap();
        }
        let store = Store::new(packages).with_system(package_store::Layout::new(system.path()));
        assert!(package_store::local_materialized_tree(&store, &image.meta.id).is_err());
        assert!(!store.user().layout().payload(image.meta.id).exists());
    }
}

#[test]
fn local_overlay_refuses_corrupt_or_truncated_cached_bytes() {
    for size in [300_000, 29] {
        let identity = SigningIdentity::generate();
        let (_source, image) = seal(
            Kind::EnvRuntime,
            "so.tilo.env-runtime",
            None,
            &identity,
            runtime_with_large_blob,
        );
        let system = scratch();
        PackageStore::at(system.path())
            .install_image(
                full_tapp(&image).as_slice(),
                |_| Ok(Admission::default()),
                None,
            )
            .unwrap();
        let user = scratch();
        let packages = PackageStore::at(user.path());
        packages
            .install_image(
                full_tapp(&image).as_slice(),
                |_| Ok(Admission::default()),
                None,
            )
            .unwrap();
        forget_materialized_image(&packages, image.meta.id);
        let expected = image.meta.collection.entries["usr/lib64/cached.bin"];
        let cached = packages
            .layout()
            .content()
            .join("data")
            .join(format!("{}.data", expected.to_hex()));
        assert!(cached.is_file(), "large retained blob must be observable");
        std::fs::write(&cached, bytes(12, size)).unwrap();
        let store = Store::new(packages).with_system(package_store::Layout::new(system.path()));
        assert!(package_store::local_materialized_tree(&store, &image.meta.id).is_err());
        assert_eq!(
            std::fs::read(
                system
                    .path()
                    .join("pkgs")
                    .join(image.meta.id.to_hex())
                    .join("usr/lib64/cached.bin")
            )
            .unwrap(),
            bytes(9, 300_000)
        );
    }
}
