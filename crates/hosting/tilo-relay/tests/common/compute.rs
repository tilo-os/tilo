//! Compute fixtures: a signed package installed the way the package store
//! lays it out, a worker catalogue holding it, and a worker advertisement.

use std::collections::BTreeSet;

use package_store::InstalledPackage;
use tilo_relay::{
    ComputePackageCatalogue, DeviceRole, PowerAvailability, ServerAdvertisement,
    ServiceCapabilities,
};

/// The catalogue of a device with exactly one installed package.
pub struct FixtureCatalogue(pub InstalledPackage);

impl ComputePackageCatalogue for FixtureCatalogue {
    fn resolve(&self, app_id: &str) -> Result<Option<InstalledPackage>, String> {
        Ok((self.0.id == app_id).then(|| self.0.clone()))
    }
}

/// Signed package `app`, offering catalogue command `entrypoint`, installed
/// under `root`, and the digest of its signed root. The same `app` and
/// `entrypoint` always give the same digest, on every device.
pub fn installed_package(
    root: &std::path::Path,
    app: &str,
    entrypoint: &str,
) -> (InstalledPackage, [u8; 32]) {
    use ed25519_dalek::Signer;
    use tilo_app_package::blobs::{BlobHash, Collection};
    use tilo_app_package::signed::SignedRoot;

    let manifest = format!(
        "[package]\nid = {app:?}\nversion = \"1.0.0\"\nkind = \"app\"\n\
         [publisher]\nidentity = \"ed25519:{}\"\n\
         [sandbox]\nnamespaces = []\nseccomp = \"tilo-default\"\nlandlock = \"strict\"\n\
         [sandbox.net]\nmode = \"off\"\n\
         [entrypoint]\nbin = \"bin/worker\"\ntype = \"cli\"\n\
         [cli]\ncommands = [{entrypoint:?}]\n",
        "00".repeat(32),
    );
    let collection =
        Collection::from_entries([("bin/worker".to_string(), BlobHash::of(b"signed worker"))]);
    let collection_bytes = collection.encode();
    let signing_key = ed25519_dalek::SigningKey::from_bytes(&[11; 32]);
    let mut signed = SignedRoot::unsigned(
        BlobHash::of(manifest.as_bytes()),
        BlobHash::of(&collection_bytes),
        signing_key.verifying_key().to_bytes(),
    );
    signed.signature = Some([0; 64]);
    signed.signature = Some(signing_key.sign(&signed.signing_bytes()).to_bytes());
    let signed_bytes = signed.encode();
    let package_digest = *BlobHash::of(&signed_bytes).as_bytes();
    let layout = package_store::Layout::new(root.join("apps"));
    let id = tilo_app_package::PackageId::from_bytes(package_digest);
    let payload = layout.payload(id);
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    std::fs::write(payload.join("bin/worker"), b"signed worker").unwrap();
    let record = layout.meta_dir(id);
    std::fs::create_dir_all(&record).unwrap();
    std::fs::write(record.join("signed-root"), signed_bytes).unwrap();
    std::fs::write(record.join("manifest.toml"), manifest.as_bytes()).unwrap();
    std::fs::write(record.join("collection.qcl"), &collection_bytes).unwrap();
    let package = InstalledPackage {
        id: app.into(),
        version: "1.0.0".into(),
        package_id_hex: hex::encode(package_digest),
        payload_root: payload,
        manifest_path: layout.manifest(id),
        publisher_key_hex: hex::encode(signing_key.verifying_key().to_bytes()),
        pinned: false,
        installed_at: 0,
        last_used: 0,
        attested_by: String::new(),
        cli_commands: vec![entrypoint.into()],
        runtime: String::new(),
        source: String::new(),
        held: false,
        payload_bytes: 0,
        system: false,
    };
    (package, package_digest)
}

/// What worker `worker` offers for `app` in `workspace`, to its executor.
pub fn worker_advertisement(worker: [u8; 32], workspace: u64, app: &str) -> ServerAdvertisement {
    ServerAdvertisement {
        device: worker,
        role: DeviceRole::AlwaysOnServer,
        services: ServiceCapabilities {
            compute: true,
            ..ServiceCapabilities::default()
        },
        workspaces: BTreeSet::from([workspace]),
        apps: BTreeSet::from([app.into()]),
        accelerators: BTreeSet::new(),
        priority: 0,
        latency_ms: 0,
        power: PowerAvailability::External,
        energy_saver: false,
        active_assignments: 0,
        assignment_capacity: 1,
        online: true,
    }
}
