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

/// One device's workspace file versions, as its own jobs name them.
///
/// A file version is addressed by its manifest hash, a job input by the
/// blake3 of its bytes; binding a version records the other address, so a
/// worker reading the content's locator proves its own copy of the version
/// instead of the bytes moving.
#[derive(Clone)]
pub struct WorkspaceVersions {
    store: std::sync::Arc<fs::store::ContentStore>,
    bound: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<[u8; 32], [u8; 32]>>>,
    runtime: tokio::runtime::Handle,
}

impl WorkspaceVersions {
    /// The versions of `store`, whose blocking reads run on `runtime`.
    pub fn new(
        store: std::sync::Arc<fs::store::ContentStore>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            store,
            bound: std::sync::Arc::default(),
            runtime,
        }
    }

    /// Bind the file version `manifest` and return the digest a job names it
    /// by.
    pub async fn bind_file(&self, manifest: [u8; 32]) -> Result<[u8; 32], String> {
        let bytes = read_version(&self.store, manifest).await?;
        let digest = *blake3::hash(&bytes).as_bytes();
        self.bound
            .lock()
            .map_err(|_| "workspace versions poisoned".to_string())?
            .insert(digest, manifest);
        Ok(digest)
    }

    fn manifest_of(&self, digest: [u8; 32]) -> Option<[u8; 32]> {
        let bound = self.bound.lock().ok()?;
        bound.get(&digest).copied()
    }

    async fn read(&self, digest: [u8; 32]) -> Result<Option<Vec<u8>>, String> {
        let Some(manifest) = self.manifest_of(digest) else {
            return Ok(None);
        };
        let bytes = read_version(&self.store, manifest).await?;
        if blake3::hash(&bytes).as_bytes() == &digest {
            Ok(Some(bytes))
        } else {
            Err("bound workspace content changed under its digest".into())
        }
    }

    async fn store_bytes(&self, digest: [u8; 32], bytes: &[u8]) -> Result<(), String> {
        if blake3::hash(bytes).as_bytes() != &digest {
            return Err("content does not match its digest".into());
        }
        let manifest = self
            .store
            .write_content(bytes)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "empty content has no file version".to_string())?;
        self.bound
            .lock()
            .map_err(|_| "workspace versions poisoned".to_string())?
            .insert(digest, *manifest.as_bytes());
        Ok(())
    }

    fn block<T: Send + 'static>(
        &self,
        future: impl std::future::Future<Output = T> + Send + 'static,
    ) -> Result<T, String> {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        self.runtime.spawn(async move {
            let _ = sender.send(future.await);
        });
        receiver
            .recv()
            .map_err(|_| "workspace versions runtime stopped".to_string())
    }
}

/// The whole of file version `manifest`, checked against it.
async fn read_version(
    store: &fs::store::ContentStore,
    manifest: [u8; 32],
) -> Result<Vec<u8>, String> {
    let decoded = store
        .manifest(manifest.into())
        .await
        .ok_or_else(|| "no workspace content under that manifest".to_string())?;
    let bytes = store
        .read_local(manifest.into(), &decoded, 0..decoded.total_len())
        .await
        .ok_or_else(|| "the file's content is not on this device yet".to_string())?;
    if decoded.verify(&bytes) {
        Ok(bytes)
    } else {
        Err("the file's local content does not match its manifest".into())
    }
}

impl tilo_relay::ComputeContentStore for WorkspaceVersions {
    fn get(&self, digest: [u8; 32]) -> Result<Option<Vec<u8>>, String> {
        let this = self.clone();
        self.block(async move { this.read(digest).await })?
    }

    fn put(&self, digest: [u8; 32], bytes: &[u8]) -> Result<(), String> {
        let this = self.clone();
        let bytes = bytes.to_vec();
        self.block(async move { this.store_bytes(digest, &bytes).await })?
    }

    fn locator(&self, digest: [u8; 32]) -> Option<[u8; 32]> {
        self.manifest_of(digest)
    }
}

impl tilo_relay::HeldContent for WorkspaceVersions {
    fn held(
        &self,
        digest: [u8; 32],
        locator: Option<[u8; 32]>,
        max_bytes: u64,
    ) -> Result<Option<Vec<u8>>, String> {
        let Some(manifest) = locator.or_else(|| self.manifest_of(digest)) else {
            return Ok(None);
        };
        let store = std::sync::Arc::clone(&self.store);
        let bytes = self.block(async move {
            let Some(decoded) = store.manifest(manifest.into()).await else {
                return Ok(None);
            };
            if decoded.total_len() > max_bytes {
                return Ok(None);
            }
            let read = store
                .read_local(manifest.into(), &decoded, 0..decoded.total_len())
                .await;
            Ok::<_, String>(read.filter(|bytes| decoded.verify(bytes)))
        })??;
        Ok(bytes.filter(|bytes| blake3::hash(bytes).as_bytes() == &digest))
    }

    fn locates(&self) -> bool {
        true
    }
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
