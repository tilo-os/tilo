//! Environment images: fetching, trusting and materialising `env-runtime`
//! and `tool` packages as read-only trees.

use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use package_store::{
    Admission, ImageTrust, MaterializeError, PackageStore, Store, StoreError, check_tool_runtime,
    materialized_tree, tool_runtime,
};
use tilo_app_package::manifest::Arch;
use tilo_app_package::trust::{RevocationList, RevokedKey};
use tilo_app_package::{
    BlobFetcher, BlobHash, BlobSink, BlobSource, ImageSpec, Kind, PackageFetcher, PackageId,
    Packed, SigningIdentity, seal_image,
};

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
