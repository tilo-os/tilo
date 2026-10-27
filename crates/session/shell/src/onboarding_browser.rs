//! First-login continuation for separately selected browser domains.
//! The canonical helper authenticates the carrier and independently verifies the
//! projection. A saved public choice never substitutes for those proofs.
use crate::onboarding_prepared::{BrowserDeclaration, Pending as Prepared};
use sha2::{Digest, Sha256};
use std::{
    fs::OpenOptions,
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
use zeroize::{Zeroize, Zeroizing};

fn failure() -> io::Error {
    io::Error::other("Browser data are pending or unavailable")
}
fn root(home: &Path) -> PathBuf {
    home.join(".local/state/tilo/prepared-browser")
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Choice {
    schema: u32,
    setup_digest: [u8; 32],
    declaration: BrowserDeclaration,
    outcome: String,
}
#[derive(serde::Deserialize)]
#[serde(tag = "event", deny_unknown_fields)]
enum Event {
    #[serde(rename = "ready-to-activate")]
    Ready { profiles: usize },
    #[serde(rename = "complete")]
    Complete {
        verified: bool,
        projection: PathBuf,
        identity_public_key: [u8; 32],
        operation: String,
        profiles: usize,
    },
}
struct Worker {
    command: bounded_command::AsyncCommand,
    _scratch: tempfile::TempDir,
}
pub(crate) struct Pending {
    home: PathBuf,
    identity: PathBuf,
    declaration: BrowserDeclaration,
    digest: [u8; 32],
    state: &'static str,
    result: serde_json::Value,
    worker: Option<Worker>,
    password: Zeroizing<Vec<u8>>,
    automatic: bool,
    projection: Option<PathBuf>,
}
fn private_file(path: &Path, bound: u64) -> io::Result<Vec<u8>> {
    let parent = crate::onboarding_migration::directory(path.parent().ok_or_else(failure)?)?;
    crate::onboarding_migration::private(&parent)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
        || metadata.len() > bound
    {
        return Err(failure());
    }
    let mut bytes = Vec::new();
    file.take(bound.checked_add(1).ok_or_else(failure)?)
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).map_err(|_| failure())? > bound {
        return Err(failure());
    }
    Ok(bytes)
}
/// Authorize the certificate the proof carries against the current person's
/// enrolled device. Returns the person and device public keys; never a key read
/// from the proof itself.
fn authorized_device(identity: &Path, certificate: &[u8]) -> io::Result<([u8; 32], [u8; 32])> {
    let paths = crate::firstboot::collaboration_identity_paths();
    if !tilo_identity_host::provisioning::active_identity_owns_device(identity, &paths)? {
        return Err(failure());
    }
    let active = runtime_identity::resolve_active_identity(identity).ok_or_else(failure)?;
    let metadata = runtime_identity::load_metadata_from_path(&active).map_err(|_| failure())?;
    let cert =
        tilo_user_identity::persist::read_device_cert(&paths.device_cert).map_err(|_| failure())?;
    let proof_cert: tilo_user_identity::DeviceCert =
        serde_json::from_slice(certificate).map_err(|_| failure())?;
    let now = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| failure())?
            .as_nanos(),
    )
    .map_err(|_| failure())?;
    tilo_user_identity::verify_device_cert_at(&proof_cert, now).map_err(|_| failure())?;
    // The proof is the group's own certificate for this very device.
    let group_signed = proof_cert.user_public_key == metadata.group_public_key;
    let this_device = proof_cert.device_public_key == cert.device_public_key;
    if !group_signed || !this_device {
        return Err(failure());
    }
    let fleet = crate::device_fleet::DeviceFleetStore::at(identity, paths)
        .load(&hex::encode(metadata.group_public_key))?;
    if fleet.is_revoked(&cert.device_public_key) {
        return Err(failure());
    }
    Ok((metadata.group_public_key, cert.device_public_key))
}

fn completion_projection(
    home: &Path,
    identity: &Path,
    declaration: &BrowserDeclaration,
    digest: [u8; 32],
) -> io::Result<Option<PathBuf>> {
    let receipt = root(home).join("completion.json");
    let proof = match browser_continuity::completion::Completion::read(&receipt) {
        Ok(proof) => proof,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let (person, device) = authorized_device(identity, proof.certificate())?;
    if person != declaration.plan.identity_public_key {
        return Err(failure());
    }
    proof.verify_binding(
        &declaration.plan,
        digest,
        declaration.payload.ciphertext_sha256,
        &declaration.payload.operation,
    )?;
    let projection = proof.verify_projection(device)?;
    if !valid_projection(home, &declaration.payload.operation, &projection) {
        return Err(failure());
    }
    Ok(Some(projection))
}

/// The deferred same-computer route has no installation ciphertext. Its proof
/// binds the reviewed migration operation and the exact projection the Browser
/// app will import, and the certificate still comes from the active person's
/// enrolled device.
pub(crate) fn deferred_projection(home: &Path, identity: &Path) -> io::Result<Option<PathBuf>> {
    let base = home.join(".local/state/tilo/browser-continuity");
    let pin = match crate::onboarding_migration::directory(&base) {
        Ok(pin) => pin,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    crate::onboarding_migration::private(&pin)?;
    let mut found = None;
    for entry in std::fs::read_dir(&base)?.take(64) {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if uuid::Uuid::parse_str(&name).is_err() {
            continue;
        }
        let operation_root = base.join(&name);
        let receipt = operation_root.join(browser_continuity::completion::FILE_NAME);
        let proof = match browser_continuity::completion::Completion::read(&receipt) {
            Ok(proof) => proof,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let operation = proof.operation();
        // The receipt belongs to the canonical directory derived from its
        // reviewed migration operation; a receipt copied elsewhere is refused.
        if browser_continuity::completion::deferred_root(home, operation) != operation_root {
            // A completion in this root belongs to the installation-media
            // route, not the deferred one; leave it to that consumer.
            continue;
        }
        let plan_file = home
            .join(".local/state/tilo/migration-handoff")
            .join(format!("{operation}.migration.json"));
        match private_file(&plan_file, 9 * 1024 * 1024) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        }
        let (person, device) = authorized_device(identity, proof.certificate())?;
        if person != proof.plan().identity_public_key {
            continue;
        }
        let plan_bytes = proof.plan().encode().map_err(|_| failure())?;
        let browser_digest: [u8; 32] = Sha256::digest(&plan_bytes).into();
        proof.verify_binding(
            proof.plan(),
            browser_continuity::completion::deferred_setup_digest(operation),
            browser_digest,
            operation,
        )?;
        let projection = proof.verify_projection(device)?;
        if projection
            != browser_continuity::completion::deferred_projection(
                &browser_continuity::completion::deferred_root(home, operation),
            )
            || !valid_deferred_projection(home, &projection)
        {
            return Err(failure());
        }
        if found.replace(projection).is_some() {
            return Err(failure());
        }
    }
    Ok(found)
}

/// Recheck the signed deferred proof and attach it to the Browser launch.
pub(crate) fn attach_deferred_continuity() {
    let Some(home) = crate::onboarding_prepared::session_home() else {
        return;
    };
    let Ok(store) = crate::identity_registry::IdentityRegistryStore::from_env() else {
        return;
    };
    if let Ok(Some(path)) = deferred_projection(&home, store.identity_dir()) {
        crate::shell::launcher::set_browser_continuity(Some(path));
    }
}

fn valid_deferred_projection(home: &Path, path: &Path) -> bool {
    let base = home.join(".local/state/tilo/browser-continuity");
    let Ok(relative) = path.strip_prefix(&base) else {
        return false;
    };
    let parts = relative.components().collect::<Vec<_>>();
    let [
        std::path::Component::Normal(operation),
        std::path::Component::Normal(container),
        std::path::Component::Normal(import),
        std::path::Component::Normal(data),
    ] = parts.as_slice()
    else {
        return false;
    };
    operation
        .to_str()
        .is_some_and(|operation| uuid::Uuid::parse_str(operation).is_ok())
        && container.to_str() == Some(browser_continuity::completion::DEFERRED_CONTAINER)
        && import.to_str() == Some("import")
        && data.to_str() == Some("home")
        && crate::onboarding_migration::directory(path)
            .is_ok_and(|pin| crate::onboarding_migration::private(&pin).is_ok())
}
fn valid_projection(home: &Path, operation: &str, path: &Path) -> bool {
    let workspace = home
        .join(".local/state/tilo/browser-continuity")
        .join(operation);
    path.file_name().is_some_and(|name| name == "home")
        && path.parent().is_some_and(|parent| {
            parent.file_name().is_some_and(|name| name == "import")
                && parent.parent().is_some_and(|container| {
                    container.parent() == Some(workspace.as_path())
                        && container
                            .file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| name.starts_with("browser-import-"))
                })
        })
        && crate::onboarding_migration::directory(path)
            .is_ok_and(|pin| crate::onboarding_migration::private(&pin).is_ok())
}
/// Recheck current-person/device authority and the signed exact projection on every launch.
pub(crate) fn validate_published_projection(path: &Path) -> bool {
    let Some(home) = crate::onboarding_prepared::session_home() else {
        return false;
    };
    let Ok(store) = crate::identity_registry::IdentityRegistryStore::from_env() else {
        return false;
    };
    let identity = store.identity_dir();
    if deferred_projection(&home, identity).is_ok_and(|found| found.as_deref() == Some(path)) {
        return true;
    }
    let Ok(prepared) = Prepared::open(&home) else {
        return false;
    };
    let Ok(Some(declaration)) = prepared.browser(identity) else {
        return false;
    };
    completion_projection(&home, identity, &declaration, prepared.digest())
        .is_ok_and(|verified| verified.as_deref() == Some(path))
}
impl Pending {
    pub(crate) fn open(
        home: PathBuf,
        identity: PathBuf,
        declaration: BrowserDeclaration,
    ) -> io::Result<Self> {
        if !declaration.valid() {
            return Err(failure());
        }
        let prepared = Prepared::open(&home)?;
        if prepared.browser(&identity)?.as_ref() != Some(&declaration) {
            return Err(failure());
        }
        let state = match private_file(&root(&home).join("choice.json"), 24576) {
            Ok(bytes) => {
                let receipt: Choice = serde_json::from_slice(&bytes).map_err(|_| failure())?;
                if receipt.schema != 1
                    || receipt.setup_digest != prepared.digest()
                    || receipt.declaration != declaration
                    || !["complete", "deferred"].contains(&receipt.outcome.as_str())
                {
                    return Err(failure());
                }
                if receipt.outcome == "complete" {
                    "pending"
                } else {
                    "deferred"
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => "pending",
            Err(error) => return Err(error),
        };
        let projection = if state == "deferred" {
            None
        } else {
            completion_projection(&home, &identity, &declaration, prepared.digest())?
        };
        let state = if projection.is_some() {
            "complete"
        } else {
            state
        };
        Ok(Self {
            home,
            identity,
            declaration,
            digest: prepared.digest(),
            state,
            result: serde_json::Value::Null,
            worker: None,
            password: Zeroizing::new(Vec::new()),
            automatic: false,
            projection,
        })
    }
    pub(crate) fn receive_prepared_password(
        &mut self,
        digest: [u8; 32],
        password: Zeroizing<Vec<u8>>,
    ) -> io::Result<()> {
        self.validate()?;
        if digest != self.digest
            || password.is_empty()
            || password.len() > 4096
            || std::str::from_utf8(&password).is_err()
            || self.busy()
        {
            return Err(failure());
        }
        self.password = password;
        self.automatic = true;
        Ok(())
    }
    fn validate(&self) -> io::Result<()> {
        let prepared = Prepared::open(&self.home)?;
        if prepared.digest() != self.digest
            || prepared.browser(&self.identity)?.as_ref() != Some(&self.declaration)
        {
            return Err(failure());
        }
        Ok(())
    }
    pub(crate) fn continue_automatic(&mut self) -> bool {
        if self.automatic && self.state == "pending" && !self.password.is_empty() && !self.busy() {
            if self.invoke("review", b"").is_err() {
                self.automatic = false;
            }
            return true;
        }
        false
    }
    pub(crate) fn deferred(&self) -> bool {
        self.state == "deferred"
    }
    pub(crate) fn pending(&self) -> bool {
        !["complete", "deferred"].contains(&self.state)
    }
    pub(crate) fn needs_password(&self) -> bool {
        self.password.is_empty() && !self.busy() && self.pending()
    }
    pub(crate) fn busy(&self) -> bool {
        self.worker.is_some()
    }
    pub(crate) fn operation(&self) -> &str {
        &self.declaration.payload.operation
    }
    pub(crate) fn status(&self) -> serde_json::Value {
        serde_json::json!({"state":self.state,"browser":true,"operations":[self.operation()],"selected":self.operation(),"result":self.result,"can_review":!self.busy(),"requires_password":self.password.is_empty()&&!self.busy(),"complete":self.state=="complete","deferred":self.state=="deferred"})
    }
    pub(crate) fn next_action(&self) -> &'static str {
        if self.state == "reviewed" {
            "start"
        } else {
            "review"
        }
    }
    pub(crate) fn invoke(&mut self, op: &str, password: &[u8]) -> io::Result<()> {
        if op == "cancel" && self.busy() {
            self.worker = None;
            self.password.zeroize();
            self.state = "cancelled";
            return Ok(());
        }
        if self.busy() {
            return Err(failure());
        }
        if op == "resume" && self.deferred() {
            self.validate()?;
            self.state = "pending";
            return Ok(());
        }
        if op == "later" {
            self.acknowledge("deferred")?;
            self.state = "deferred";
            self.password.zeroize();
            return Ok(());
        }
        if op == "continue" && self.state == "complete" {
            self.validate()?;
            return Ok(());
        }
        let action = match op {
            "review" | "retry" => "review",
            "start" if self.state == "reviewed" => "activate",
            _ => return Err(failure()),
        };
        if !password.is_empty() {
            if password.len() > 4096 || std::str::from_utf8(password).is_err() {
                return Err(failure());
            }
            self.password = Zeroizing::new(password.to_vec());
        }
        if self.password.is_empty() {
            return Err(failure());
        }
        match self.start(action) {
            Ok(worker) => {
                self.worker = Some(worker);
                self.state = if action == "review" {
                    "reviewing"
                } else {
                    "activating-browser"
                };
                Ok(())
            }
            Err(error) => {
                self.state = "unavailable";
                Err(error)
            }
        }
    }
    fn start(&self, action: &str) -> io::Result<Worker> {
        self.start_with(
            action,
            crate::onboarding_portable_home::helper_executable()?,
        )
    }
    fn start_with(&self, action: &str, executable: &Path) -> io::Result<Worker> {
        self.validate()?;
        let directory = root(&self.home);
        let expected: crate::onboarding_prepared::BrowserPayload =
            serde_json::from_slice(&private_file(&directory.join("expectation.json"), 24576)?)
                .map_err(|_| failure())?;
        if expected != self.declaration.payload {
            return Err(failure());
        }
        let package = directory.join(&self.declaration.payload.filename);
        // Check private literal file custody; large hashing/authentication belongs to the helper.
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&package)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o077 != 0
            || metadata.len() != self.declaration.payload.sealed_bytes
        {
            return Err(failure());
        }
        let mut pin = crate::onboarding_migration::directory(&self.home)?;
        for segment in [
            ".local",
            "state",
            "tilo",
            "browser-continuity",
            self.operation(),
        ] {
            pin = crate::onboarding_migration::create(&pin, segment)?;
            if [".local", "state", "tilo"].contains(&segment) {
                tilo_files::native::atomic::StorageDirectory::from_file(pin.try_clone()?)?
                    .validate_namespace_control()?;
            } else {
                crate::onboarding_migration::private(&pin)?;
            }
        }
        let scratch = tempfile::Builder::new()
            .prefix(".setup-review-")
            .tempdir_in(&directory)?;
        let mut command = Command::new(executable);
        command
            .arg("--browser-transfer")
            .arg("--package")
            .arg(package)
            .arg("--receipt")
            .arg(directory.join("expectation.json"))
            .arg("--setup-package")
            .arg(
                self.home
                    .join(".local/state/tilo/prepared-setup/prepared-setup.sealed"),
            )
            .arg("--setup-quarantine")
            .arg(scratch.path().join("contents"))
            .arg("--home")
            .arg(&self.home)
            .arg("--workspace")
            .arg(
                self.home
                    .join(".local/state/tilo/browser-continuity")
                    .join(self.operation()),
            )
            .arg("--action")
            .arg(action);
        let command = bounded_command::spawn_prepared(
            command,
            Some(&self.password),
            64 * 1024,
            Duration::from_secs(1800),
        )
        .map_err(|_| failure())?;
        Ok(Worker {
            command,
            _scratch: scratch,
        })
    }
    fn acknowledge(&self, outcome: &str) -> io::Result<()> {
        self.validate()?;
        let directory = root(&self.home);
        let pin = crate::onboarding_migration::directory(&directory)?;
        crate::onboarding_migration::private(&pin)?;
        let choice = Choice {
            schema: 1,
            setup_digest: self.digest,
            declaration: self.declaration.clone(),
            outcome: outcome.into(),
        };
        let bytes = serde_json::to_vec(&choice).map_err(|_| failure())?;
        if bytes.len() > 24576 {
            return Err(failure());
        }
        let mut staged = tempfile::NamedTempFile::new_in(&directory)?;
        staged.write_all(&bytes)?;
        staged.as_file().sync_all()?;
        self.validate()?;
        let destination = directory.join("choice.json");
        match private_file(&destination, 24576) {
            Ok(previous) => {
                let previous: Choice = serde_json::from_slice(&previous).map_err(|_| failure())?;
                if previous.schema != 1
                    || previous.setup_digest != self.digest
                    || previous.declaration != self.declaration
                {
                    return Err(failure());
                }
                staged.persist(&destination).map_err(|_| failure())?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                staged
                    .persist_noclobber(&destination)
                    .map_err(|_| failure())?;
            }
            Err(error) => return Err(error),
        }
        pin.sync_all()
    }
    pub(crate) fn poll(&mut self) -> bool {
        let Some(worker) = &mut self.worker else {
            return false;
        };
        let Some(result) = worker.command.try_wait() else {
            return false;
        };
        self.worker = None;
        let result = result.map_err(|_| failure()).and_then(|output| {
            if !output.status.success() {
                return Err(failure());
            }
            let last = output
                .stdout
                .split(|byte| *byte == b'\n')
                .rfind(|line| !line.is_empty())
                .ok_or_else(failure)?;
            if last.len() > 24576 {
                return Err(failure());
            }
            serde_json::from_slice::<Event>(last).map_err(|_| failure())
        });
        match result {
            Ok(Event::Ready { profiles }) if profiles == self.declaration.plan.profiles.len() => {
                self.state = "reviewed";
                self.result = serde_json::json!({"profiles": profiles});
                if self.automatic && self.invoke("start", b"").is_err() {
                    self.automatic = false;
                }
            }
            Ok(Event::Complete {
                verified: true,
                projection,
                identity_public_key,
                operation,
                profiles,
            }) if identity_public_key == self.declaration.plan.identity_public_key
                && operation == self.operation()
                && profiles == self.declaration.plan.profiles.len()
                && self.valid_projection(&projection) =>
            {
                if !completion_projection(
                    &self.home,
                    &self.identity,
                    &self.declaration,
                    self.digest,
                )
                .is_ok_and(|verified| verified.as_deref() == Some(projection.as_path()))
                {
                    self.state = "failed";
                    self.result = serde_json::Value::Null;
                    return true;
                }
                self.result = serde_json::json!({"profiles":profiles,"verified":true});
                self.state = if self.acknowledge("complete").is_ok() {
                    "complete"
                } else {
                    "incomplete"
                };
                if self.state == "complete" {
                    self.projection = Some(projection);
                }
                self.password.zeroize();
            }
            _ => {
                self.state = "failed";
                self.result = serde_json::Value::Null;
            }
        }
        true
    }
    fn valid_projection(&self, path: &Path) -> bool {
        valid_projection(&self.home, self.operation(), path)
    }
    pub(crate) fn verified_projection(&self) -> Option<&Path> {
        self.projection
            .as_deref()
            .filter(|path| self.state == "complete" && self.valid_projection(path))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;

    pub(crate) fn fixture() -> (tempfile::TempDir, PathBuf, BrowserDeclaration) {
        let (home, identity, _) = crate::onboarding_portable_home::tests::fixture();
        let prepared = Prepared::open(home.path()).unwrap();
        std::fs::remove_file(
            home.path()
                .join(".local/state/tilo/prepared-setup/activated.json"),
        )
        .unwrap();
        let signer = runtime_identity::load_from_path(identity.join("primary.json")).unwrap();
        let plan = browser_continuity::Plan::new(
            "browser-binding".into(),
            signer.group_public_key(),
            vec![
                browser_continuity::Profile::new(
                    "a".repeat(64),
                    browser_continuity::Provider::Chromium,
                    vec!["browser".into(), "Default".into()],
                    true,
                    false,
                )
                .unwrap(),
            ],
        )
        .unwrap();
        let operation = uuid::Uuid::new_v4().to_string();
        let declaration = BrowserDeclaration {
            payload: crate::onboarding_prepared::BrowserPayload {
                schema: 1,
                filename: format!("browser-{operation}.sealed"),
                operation,
                purpose: "browser_profiles".into(),
                setup_binding: plan.setup_binding.clone(),
                sealed_bytes: 18,
                plaintext_bytes: 16,
                entries: 2,
                ciphertext_sha256: [2; 32],
                plan_sha256: Sha256::digest(plan.encode().unwrap()).into(),
                limits: crate::onboarding_prepared::ApplicationLimits {
                    plaintext_bytes: 1024,
                    entries: 64,
                    metadata_bytes: 1024,
                },
            },
            plan,
        };
        prepared
            .acknowledge(
                signer.group_public_key(),
                None,
                None,
                Some(declaration.clone()),
                None,
                None,
            )
            .unwrap();
        let directory = root(home.path());
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        for (name, bytes) in [
            (
                "expectation.json".to_owned(),
                serde_json::to_vec(&declaration.payload).unwrap(),
            ),
            (
                declaration.payload.filename.clone(),
                b"fixture ciphertext".to_vec(),
            ),
        ] {
            let path = directory.join(name);
            std::fs::write(&path, bytes).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        (home, identity, declaration)
    }
    pub(crate) fn certified_projection(
        home: &Path,
        identity: &Path,
        declaration: &BrowserDeclaration,
    ) -> PathBuf {
        let paths = crate::firstboot::collaboration_identity_paths();
        crate::firstboot::ensure_collaboration_identity(identity, &paths).unwrap();
        let projection = home
            .join(".local/state/tilo/browser-continuity")
            .join(&declaration.payload.operation)
            .join("browser-import-fixture/import/home");
        let profile = projection
            .join("profiles")
            .join(&declaration.plan.profiles.first().unwrap().id);
        std::fs::create_dir_all(&profile).unwrap();
        let mut current = profile.as_path();
        while current != home {
            std::fs::set_permissions(current, std::fs::Permissions::from_mode(0o700)).unwrap();
            current = current.parent().unwrap();
        }
        for (path, bytes) in [
            (
                projection.join("plan.json"),
                declaration.plan.encode().unwrap(),
            ),
            (
                profile.join("Bookmarks"),
                br#"{"roots":{"bookmark_bar":{"type":"folder","name":"Bookmarks","children":[]}}}"#
                    .to_vec(),
            ),
        ] {
            std::fs::write(&path, bytes).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let digest = Prepared::open(home).unwrap().digest();
        let unsigned = browser_continuity::completion::capture(
            declaration.plan.clone(),
            digest,
            declaration.payload.ciphertext_sha256,
            declaration.payload.operation.clone(),
            projection.clone(),
        )
        .unwrap();
        unsigned.verify_unchanged().unwrap();
        let key = tilo_user_identity::persist::load_device_key(&paths).unwrap();
        let cert = tilo_user_identity::persist::read_device_cert(&paths.device_cert).unwrap();
        let proof = unsigned
            .sign(&key, serde_json::to_vec(&cert).unwrap())
            .unwrap();
        let receipt = root(home).join("completion.json");
        std::fs::write(&receipt, proof.encode().unwrap()).unwrap();
        std::fs::set_permissions(receipt, std::fs::Permissions::from_mode(0o600)).unwrap();
        projection
    }
    pub(crate) fn deferred_projection_fixture(home: &Path, identity: &Path) -> (PathBuf, PathBuf) {
        let paths = crate::firstboot::collaboration_identity_paths();
        crate::firstboot::ensure_collaboration_identity(identity, &paths).unwrap();
        let signer = runtime_identity::load_from_path(identity.join("primary.json")).unwrap();
        let operation = uuid::Uuid::new_v4().to_string();
        let plans = home.join(".local/state/tilo/migration-handoff");
        std::fs::create_dir_all(&plans).unwrap();
        std::fs::set_permissions(&plans, std::fs::Permissions::from_mode(0o700)).unwrap();
        let plan_file = plans.join(format!("{operation}.migration.json"));
        std::fs::write(
            &plan_file,
            serde_json::to_vec(&serde_json::json!({
                "schema": 1,
                "operation_id": operation,
                "categories": {"home": true, "flatpak": false, "accounts": false},
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::set_permissions(&plan_file, std::fs::Permissions::from_mode(0o600)).unwrap();
        let plan = browser_continuity::Plan::new(
            format!("deferred:{operation}"),
            signer.group_public_key(),
            vec![
                browser_continuity::Profile::new(
                    "b".repeat(64),
                    browser_continuity::Provider::Chromium,
                    vec!["browser".into(), "Default".into()],
                    true,
                    false,
                )
                .unwrap(),
            ],
        )
        .unwrap();
        let root = browser_continuity::completion::deferred_root(home, &operation);
        let projection = browser_continuity::completion::deferred_projection(&root);
        let profile = projection
            .join("profiles")
            .join(&plan.profiles.first().unwrap().id);
        std::fs::create_dir_all(&profile).unwrap();
        let mut current = profile.as_path();
        while current != home {
            std::fs::set_permissions(current, std::fs::Permissions::from_mode(0o700)).unwrap();
            current = current.parent().unwrap();
        }
        for (path, bytes) in [
            (projection.join("plan.json"), plan.encode().unwrap()),
            (
                profile.join("Bookmarks"),
                br#"{"roots":{"bookmark_bar":{"type":"folder","name":"Bookmarks","children":[]}}}"#
                    .to_vec(),
            ),
        ] {
            std::fs::write(&path, bytes).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let setup_digest = browser_continuity::completion::deferred_setup_digest(&operation);
        let browser_digest: [u8; 32] = Sha256::digest(plan.encode().unwrap()).into();
        let unsigned = browser_continuity::completion::capture(
            plan,
            setup_digest,
            browser_digest,
            operation,
            projection.clone(),
        )
        .unwrap();
        let key = tilo_user_identity::persist::load_device_key(&paths).unwrap();
        let cert = tilo_user_identity::persist::read_device_cert(&paths.device_cert).unwrap();
        let proof = unsigned
            .sign(&key, serde_json::to_vec(&cert).unwrap())
            .unwrap();
        let receipt = root.join(browser_continuity::completion::FILE_NAME);
        std::fs::write(&receipt, proof.encode().unwrap()).unwrap();
        std::fs::set_permissions(receipt, std::fs::Permissions::from_mode(0o600)).unwrap();
        (root, projection)
    }

    #[test]
    fn deferred_first_launch_projection_is_attached_only_with_its_signed_proof() {
        let (home, identity, _declaration) = fixture();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("HOME", home.path());
        env.set("XDG_CONFIG_HOME", home.path().join(".config"));
        env.set("TILO_IDENTITY_ROOT", home.path().join("system"));
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let (root, projection) = deferred_projection_fixture(home.path(), &identity);
        assert_eq!(
            deferred_projection(home.path(), &identity).unwrap(),
            Some(projection.clone())
        );
        assert!(validate_published_projection(&projection));
        assert!(
            !std::fs::read(root.join(browser_continuity::completion::FILE_NAME))
                .unwrap()
                .is_empty()
        );
        let bookmarks = projection
            .join("profiles")
            .join(
                &browser_continuity::Plan::decode(
                    &std::fs::read(projection.join("plan.json")).unwrap(),
                )
                .unwrap()
                .profiles
                .first()
                .unwrap()
                .id,
            )
            .join("Bookmarks");
        std::fs::write(&bookmarks, b"changed after completion").unwrap();
        assert!(
            deferred_projection(home.path(), &identity).is_err(),
            "a changed projection must not be attached"
        );
        assert!(!validate_published_projection(&projection));
    }

    fn wait(pending: &mut Pending) {
        tilo_test_env::wait_until("the worker to finish", || {
            if !pending.busy() {
                return true;
            }
            pending.poll();
            false
        });
    }
    #[test]
    fn authenticated_browser_protocol_requires_exact_verified_projection_and_rechecks_reopen() {
        let (home, identity, declaration) = fixture();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("HOME", home.path());
        env.set("XDG_CONFIG_HOME", home.path().join(".config"));
        env.set("TILO_IDENTITY_ROOT", home.path().join("system"));
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let mut pending =
            Pending::open(home.path().into(), identity.clone(), declaration.clone()).unwrap();
        let prepared = Prepared::open(home.path()).unwrap();
        pending
            .receive_prepared_password(
                prepared.digest(),
                Zeroizing::new(b"private password".to_vec()),
            )
            .unwrap();
        pending.automatic = false;
        let helper = home.path().join("inert-browser-helper");
        std::fs::write(&helper,b"#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"event\":\"ready-to-activate\",\"profiles\":1}'\n").unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        pending.worker = Some(pending.start_with("review", &helper).unwrap());
        wait(&mut pending);
        assert_eq!(pending.status()["state"], "reviewed");
        assert!(pending.verified_projection().is_none());
        let projection = home
            .path()
            .join(".local/state/tilo/browser-continuity")
            .join(&declaration.payload.operation)
            .join("browser-import-fixture/import/home");
        std::fs::create_dir_all(&projection).unwrap();
        for path in [
            projection.clone(),
            projection.parent().unwrap().to_path_buf(),
            projection.parent().unwrap().parent().unwrap().to_path_buf(),
        ] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut result = serde_json::json!({"event":"complete","verified":true,"projection":projection,"identity_public_key":vec![0u8;32],"operation":declaration.payload.operation,"profiles":1});
        let emit = |result: &serde_json::Value| {
            std::fs::write(
                &helper,
                format!(
                    "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{}'\n",
                    serde_json::to_string(result).unwrap()
                ),
            )
            .unwrap();
        };
        emit(&result);
        pending.worker = Some(pending.start_with("activate", &helper).unwrap());
        wait(&mut pending);
        assert_eq!(pending.status()["state"], "failed");
        assert!(pending.verified_projection().is_none());
        assert!(!root(home.path()).join("choice.json").exists());
        result["identity_public_key"] = serde_json::json!(declaration.plan.identity_public_key);
        emit(&result);
        pending.worker = Some(pending.start_with("activate", &helper).unwrap());
        wait(&mut pending);
        assert_eq!(
            pending.status()["state"],
            "failed",
            "verified stdout is not a signed completion proof"
        );
        assert!(pending.verified_projection().is_none());
        assert_eq!(
            certified_projection(home.path(), &identity, &declaration),
            projection
        );
        emit(&result);
        pending.worker = Some(pending.start_with("activate", &helper).unwrap());
        wait(&mut pending);
        assert_eq!(pending.verified_projection(), Some(projection.as_path()));
        assert!(pending.password.is_empty());
        let reopened = Pending::open(home.path().into(), identity, declaration).unwrap();
        assert!(!reopened.pending());
        assert_eq!(reopened.verified_projection(), Some(projection.as_path()));
        assert_eq!(
            std::fs::read(root(home.path()).join(&pending.declaration.payload.filename)).unwrap(),
            b"fixture ciphertext"
        );
    }
}
