//! Root projection of actual authenticated enrollment and monotonic revocations.
//! Only public signed material is stored; transport passwords/device keys are not.
use super::{installed_retained, invalid};
use crate::setup::installed_retained_actor::RootInstalledPersonAnchor;
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsStr,
    fs::File,
    io,
    os::unix::fs::{FileExt, MetadataExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tilo_files::native::atomic::{Privacy, PublishMode, StorageDirectory};
use tilo_user_identity::{DeviceCert, RevocationEntry, RevocationList};
const LOCATION: &str = "var/lib/tilo/retained-persons";
const LIMIT: u64 = 512 * 1024;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u32,
    purpose: String,
    certificate: DeviceCert,
    revocations: Vec<RevocationEntry>,
}
fn root() -> io::Result<()> {
    if rustix::process::getuid().as_raw() != 0 || rustix::process::geteuid().as_raw() != 0 {
        return Err(invalid());
    }
    Ok(())
}
fn hex(key: &[u8; 32]) -> String {
    hex::encode(key)
}
fn validate(r: &Record) -> io::Result<RevocationList> {
    if r.schema != 1
        || r.purpose != "installed-retained-person"
        || r.revocations.len() > 256
        || r.certificate.capabilities.foreign
    {
        return Err(invalid());
    }
    tilo_user_identity::verify_device_cert_at(&r.certificate, super::now_ns()?)
        .map_err(|_| invalid())?;
    let mut list = RevocationList::new();
    for e in &r.revocations {
        if e.user_public_key != r.certificate.user_public_key {
            return Err(invalid());
        }
        list.insert(e.clone()).map_err(|_| invalid())?;
    }
    if list.is_revoked(&r.certificate.device_public_key) {
        return Err(invalid());
    }
    Ok(list)
}
fn read(file: &File) -> io::Result<Vec<u8>> {
    let m = file.metadata()?;
    if !m.is_file()
        || m.uid() != 0
        || m.mode() & 0o7777 != 0o600
        || m.nlink() != 1
        || m.len() > LIMIT
    {
        return Err(invalid());
    }
    let mut bytes = vec![0; usize::try_from(m.len()).map_err(io::Error::other)?];
    file.read_exact_at(&mut bytes, 0)?;
    let after = file.metadata()?;
    if (m.dev(), m.ino(), m.len(), m.ctime(), m.ctime_nsec())
        != (
            after.dev(),
            after.ino(),
            after.len(),
            after.ctime(),
            after.ctime_nsec(),
        )
    {
        return Err(invalid());
    }
    Ok(bytes)
}
/// Seed is minted only by the actual authenticated encrypted enrolled transport.
/// Exact retries compare the initial seed; later revocations use a separate file.
pub(super) fn publish_namespace(
    enrolled: &tilo_identity_host::enrolled_transport::AuthenticatedEnrolledDevice,
    namespace: &Path,
) -> io::Result<()> {
    root()?;
    let (certificate, revocations) = enrolled.public_native_material()?;
    publish_material(certificate, revocations, namespace)
}
fn publish_material(
    certificate: DeviceCert,
    revocations: Vec<RevocationEntry>,
    namespace: &Path,
) -> io::Result<()> {
    root()?;
    let r = Record {
        schema: 1,
        purpose: "installed-retained-person".into(),
        certificate,
        revocations,
    };
    validate(&r)?;
    let directory = installed_retained::chain(&namespace.join("retained-persons"), true)?;
    let storage = StorageDirectory::from_file(directory.last().ok_or_else(invalid)?.try_clone()?)?;
    let name = format!("{}.json", hex(&r.certificate.user_public_key));
    let bytes = serde_json::to_vec(&r)?;
    if bytes.len() as u64 > LIMIT {
        return Err(invalid());
    }
    match storage.open_read(OsStr::new(&name), Privacy::OwnerOnly) {
        Ok(existing) => {
            if read(&existing)? != bytes {
                return Err(invalid());
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            storage
                .publish(
                    OsStr::new(&name),
                    &bytes,
                    PublishMode::IfAbsent,
                    Privacy::OwnerOnly,
                )
                .map_err(|e| e.into_io_error())?;
        }
        Err(e) => return Err(e),
    }
    Ok(())
}
/// Publish first-device public authority only after actual canonical onboarding.
/// The controller supplies the same live target peer and its authenticated
/// identity-directory observer; the installed declaration supplies the person.
pub fn publish_original_copy_owner(
    installed: &super::AuthenticatedInstalledOriginalCopy,
    current: &tilo_identity_host::enrolled_transport::RootObservedCurrentDevice,
    peer: &runtime_portal::process_binding::ExecutingPeer,
) -> io::Result<()> {
    root()?;
    installed.revalidate()?;
    current.validate()?;
    let manifest = installed.manifest();
    let declaration = manifest
        .installed_original_copy_declaration()?
        .ok_or_else(invalid)?;
    if !matches!(
        manifest.identity_kind,
        super::IdentityKind::SingleDeviceBackup
    ) || current.owner_uid() != peer.uid()
        || !peer.current()
        || current.public_key() != manifest.identity_public_key
        || declaration.identity_public_key() != current.public_key()
        || declaration.setup_binding() != manifest.setup_binding
    {
        return Err(invalid());
    }
    let (certificate, revocations) = current.public_native_material()?;
    if certificate.capabilities != tilo_user_identity::DeviceCapabilities::primary()
        || certificate.user_public_key != manifest.identity_public_key
        || certificate.device_public_key != current.device_public_key()
    {
        return Err(invalid());
    }
    installed.revalidate()?;
    current.validate()?;
    if !peer.current() {
        return Err(invalid());
    }
    publish_observed_material(certificate, revocations)?;
    installed.revalidate()?;
    current.validate()?;
    if !peer.current() {
        return Err(invalid());
    }
    Ok(())
}
/// First-login primary Home authority comes from authenticated installed consent
/// and the actual target-owned current-device observer on the same QPE lane.
/// The primary backup remains its original identity kind; no member enrollment
/// or source credential is inferred from a public certificate or request label.
pub fn publish_retained_primary_owner(
    installed: &super::AuthenticatedInstalledRetained,
    current: &tilo_identity_host::enrolled_transport::RootObservedCurrentDevice,
    peer: &runtime_portal::process_binding::ExecutingPeer,
) -> io::Result<()> {
    root()?;
    installed.revalidate()?;
    current.validate()?;
    let manifest = installed.manifest();
    let plan = manifest
        .retained_home
        .as_ref()
        .ok_or_else(invalid)?
        .as_plan();
    let declaration = manifest.retained_caller.as_ref().ok_or_else(invalid)?;
    declaration.validate_binding(plan, &manifest.setup_binding, &manifest.identity_public_key)?;
    if !matches!(
        manifest.identity_kind,
        super::IdentityKind::SingleDeviceBackup
    ) || current.owner_uid() != peer.uid()
        || !peer.current()
        || current.public_key() != manifest.identity_public_key
    {
        return Err(invalid());
    }
    let (certificate, revocations) = current.public_native_material()?;
    if certificate.capabilities != tilo_user_identity::DeviceCapabilities::primary()
        || certificate.user_public_key != manifest.identity_public_key
        || certificate.device_public_key != current.device_public_key()
    {
        return Err(invalid());
    }
    installed.revalidate()?;
    current.validate()?;
    if !peer.current() {
        return Err(invalid());
    }
    publish_observed_material(certificate, revocations)?;
    installed.revalidate()?;
    current.validate()?;
    if !peer.current() {
        return Err(invalid());
    }
    Ok(())
}
/// Mutable current-owner fleets retain the immutable initial seed and refresh
/// only its separately persisted monotonic revocations. Exact enrolled transport
/// retry semantics remain unchanged.
fn publish_observed_material(
    certificate: DeviceCert,
    revocations: Vec<RevocationEntry>,
) -> io::Result<()> {
    root()?;
    let observed = Record {
        schema: 1,
        purpose: "installed-retained-person".into(),
        certificate: certificate.clone(),
        revocations: revocations.clone(),
    };
    validate(&observed)?;
    let namespace = Path::new("/var/lib/tilo");
    let directory = installed_retained::chain(&namespace.join("retained-persons"), true)?;
    let storage = StorageDirectory::from_file(directory.last().ok_or_else(invalid)?.try_clone()?)?;
    let name = format!("{}.json", hex(&certificate.user_public_key));
    match storage.open_read(OsStr::new(&name), Privacy::OwnerOnly) {
        Ok(existing) => {
            let seed: Record = serde_json::from_slice(&read(&existing)?)?;
            validate(&seed)?;
            if tilo_user_identity::canonical::canonical_bytes(&seed.certificate)
                .map_err(|_| invalid())?
                != tilo_user_identity::canonical::canonical_bytes(&certificate)
                    .map_err(|_| invalid())?
            {
                return Err(invalid());
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            publish_material(certificate.clone(), revocations.clone(), namespace)?;
        }
        Err(error) => return Err(error),
    }
    let anchor =
        InstalledPersonAnchor::open(certificate.user_public_key, certificate.device_public_key)?;
    let (held_certificate, permanent) = anchor.current()?;
    if held_certificate != certificate || permanent.iter().any(|entry| !revocations.contains(entry))
    {
        return Err(invalid());
    }
    anchor.refresh_verified(&revocations)?;
    let (_, refreshed) = anchor.current()?;
    if revocations
        .iter()
        .any(|entry| !refreshed.iter().any(|known| known == entry))
    {
        return Err(invalid());
    }
    Ok(())
}
/// No CLI-authored path/person/certificate can construct this adapter. The root
/// actor opens the fixed root record selected by authenticated installed Setup.
pub struct InstalledPersonAnchor {
    path: PathBuf,
    chain: Vec<File>,
    name: String,
    seed: File,
    seed_bytes: Vec<u8>,
    certificate: DeviceCert,
    installed_certificate_path: PathBuf,
    installed_certificate_chain: Vec<File>,
    installed_certificate: File,
    installed_certificate_bytes: Vec<u8>,
    mutex: Mutex<()>,
}
impl InstalledPersonAnchor {
    pub(in crate::setup) fn open(person: [u8; 32], device: [u8; 32]) -> io::Result<Arc<Self>> {
        Self::open_at(Path::new("/"), person, device)
    }
    fn open_at(system: &Path, person: [u8; 32], device: [u8; 32]) -> io::Result<Arc<Self>> {
        root()?;
        let path = system.join(LOCATION);
        let chain = installed_retained::chain(&path, false)?;
        let storage = StorageDirectory::from_file(chain.last().ok_or_else(invalid)?.try_clone()?)?;
        let name = format!("{}.json", hex(&person));
        let seed = storage.open_read(OsStr::new(&name), Privacy::OwnerOnly)?;
        let seed_bytes = read(&seed)?;
        let r: Record = serde_json::from_slice(&seed_bytes)?;
        validate(&r)?;
        if r.certificate.user_public_key != person || r.certificate.device_public_key != device {
            return Err(invalid());
        }
        let installed_certificate_path =
            tilo_user_identity::persist::IdentityPaths::under(system.join("var/lib/tilo"))
                .device_cert_for_user(&person);
        let installed_certificate_chain = installed_retained::public_certificate_chain(
            installed_certificate_path.parent().ok_or_else(invalid)?,
        )?;
        let certificate_storage = StorageDirectory::from_file(
            installed_certificate_chain
                .last()
                .ok_or_else(invalid)?
                .try_clone()?,
        )?;
        let installed_certificate = certificate_storage.open_read(
            installed_certificate_path.file_name().ok_or_else(invalid)?,
            Privacy::Inherit,
        )?;
        let installed_certificate_bytes = read_public_certificate(&installed_certificate)?;
        let installed: DeviceCert =
            tilo_user_identity::canonical::from_canonical_bytes(&installed_certificate_bytes)
                .map_err(|_| invalid())?;
        if serde_json::to_vec(&installed)? != serde_json::to_vec(&r.certificate)? {
            return Err(invalid());
        }
        let this = Arc::new(Self {
            path,
            chain,
            name,
            seed,
            seed_bytes,
            certificate: r.certificate,
            installed_certificate_path,
            installed_certificate_chain,
            installed_certificate,
            installed_certificate_bytes,
            mutex: Mutex::new(()),
        });
        this.current()?;
        Ok(this)
    }
    fn attest_installed_certificate(&self) -> io::Result<()> {
        let current = installed_retained::public_certificate_chain(
            self.installed_certificate_path
                .parent()
                .ok_or_else(invalid)?,
        )?;
        if current.len() != self.installed_certificate_chain.len() {
            return Err(invalid());
        }
        for (held, named) in self.installed_certificate_chain.iter().zip(&current) {
            let (a, b) = (held.metadata()?, named.metadata()?);
            if (a.dev(), a.ino()) != (b.dev(), b.ino()) {
                return Err(invalid());
            }
        }
        let storage =
            StorageDirectory::from_file(current.last().ok_or_else(invalid)?.try_clone()?)?;
        let named = storage.open_read(
            self.installed_certificate_path
                .file_name()
                .ok_or_else(invalid)?,
            Privacy::Inherit,
        )?;
        let (a, b) = (named.metadata()?, self.installed_certificate.metadata()?);
        if (a.dev(), a.ino()) != (b.dev(), b.ino())
            || read_public_certificate(&named)? != self.installed_certificate_bytes
            || read_public_certificate(&self.installed_certificate)?
                != self.installed_certificate_bytes
        {
            return Err(invalid());
        }
        tilo_user_identity::verify_device_cert_at(&self.certificate, super::now_ns()?)
            .map_err(|_| invalid())?;
        Ok(())
    }
    fn storage(&self) -> io::Result<StorageDirectory> {
        root()?;
        self.attest_installed_certificate()?;
        let current = installed_retained::chain(&self.path, false)?;
        if current.len() != self.chain.len() {
            return Err(invalid());
        }
        for (held, named) in self.chain.iter().zip(&current) {
            let (a, b) = (held.metadata()?, named.metadata()?);
            if (a.dev(), a.ino()) != (b.dev(), b.ino()) {
                return Err(invalid());
            }
        }
        let storage =
            StorageDirectory::from_file(current.last().ok_or_else(invalid)?.try_clone()?)?;
        let named = storage.open_read(OsStr::new(&self.name), Privacy::OwnerOnly)?;
        let (a, b) = (named.metadata()?, self.seed.metadata()?);
        if (a.dev(), a.ino()) != (b.dev(), b.ino())
            || read(&named)? != self.seed_bytes
            || read(&self.seed)? != self.seed_bytes
        {
            return Err(invalid());
        }
        Ok(storage)
    }
    fn record(&self, storage: &StorageDirectory) -> io::Result<Record> {
        let seed: Record = serde_json::from_slice(&self.seed_bytes)?;
        let current = match storage.open_read(
            OsStr::new(&format!(
                "{}.revocations.json",
                hex(&self.certificate.user_public_key)
            )),
            Privacy::OwnerOnly,
        ) {
            Ok(f) => serde_json::from_slice::<Record>(&read(&f)?)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(seed),
            Err(e) => return Err(e),
        };
        if serde_json::to_vec(&current.certificate)? != serde_json::to_vec(&self.certificate)? {
            return Err(invalid());
        }
        let list = validate(&current)?;
        // Replacement of the refresh file cannot remove the initial authenticated
        // baseline, even before a live actor has observed further revocations.
        if seed
            .revocations
            .iter()
            .any(|e| !list.is_revoked(&e.revoked_device_public_key))
        {
            return Err(invalid());
        }
        Ok(current)
    }
}
impl RootInstalledPersonAnchor for InstalledPersonAnchor {
    fn current(&self) -> io::Result<(DeviceCert, RevocationList)> {
        let _guard = self.mutex.lock().map_err(|_| invalid())?;
        let storage = self.storage()?;
        let r = self.record(&storage)?;
        Ok((r.certificate.clone(), validate(&r)?))
    }
    fn refresh_verified(&self, entries: &[RevocationEntry]) -> io::Result<()> {
        let _guard = self.mutex.lock().map_err(|_| invalid())?;
        let storage = self.storage()?;
        let _lock = advisory_lock::FileLock::exclusive(
            self.chain.last().ok_or_else(invalid)?.try_clone()?,
        )?;
        let mut r = self.record(&storage)?;
        let mut list = validate(&r)?;
        if entries.len() > 256 {
            return Err(invalid());
        }
        for e in entries {
            if e.user_public_key != self.certificate.user_public_key {
                return Err(invalid());
            }
            list.insert(e.clone()).map_err(|_| invalid())?;
        }
        r.revocations = list.iter().cloned().collect();
        r.revocations.sort_by_key(|e| e.revoked_device_public_key);
        // Persist permanent revocations even when they revoke this actor. The
        // subsequent current() refuses it; never discard a valid root revocation.
        if r.revocations.len() > 256 {
            return Err(invalid());
        }
        let bytes = serde_json::to_vec(&r)?;
        if bytes.len() as u64 > LIMIT {
            return Err(invalid());
        }
        let name = format!(
            "{}.revocations.json",
            hex(&self.certificate.user_public_key)
        );
        storage
            .publish(
                OsStr::new(&name),
                &bytes,
                PublishMode::Replace,
                Privacy::OwnerOnly,
            )
            .map_err(|e| e.into_io_error())?;
        self.storage()?;
        Ok(())
    }
}

fn read_public_certificate(file: &File) -> io::Result<Vec<u8>> {
    let before = file.metadata()?;
    if !before.is_file()
        || before.uid() != 0
        || before.nlink() != 1
        || before.mode() & 0o7022 != 0
        || before.len() == 0
        || before.len() > 65536
    {
        return Err(invalid());
    }
    let mut bytes = vec![0; usize::try_from(before.len()).map_err(io::Error::other)?];
    file.read_exact_at(&mut bytes, 0)?;
    let after = file.metadata()?;
    if (
        before.dev(),
        before.ino(),
        before.len(),
        before.ctime(),
        before.ctime_nsec(),
        before.mode(),
    ) != (
        after.dev(),
        after.ino(),
        after.len(),
        after.ctime(),
        after.ctime_nsec(),
        after.mode(),
    ) {
        return Err(invalid());
    }
    Ok(bytes)
}

#[cfg(all(test, target_os = "linux", feature = "native-capture-qualification"))]
mod native_tests;
