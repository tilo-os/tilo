//! Explicit first-login continuation for authenticated portable applications.
//! The canonical helper owns archive proofs, user-state adoption and signed
//! offline restoration; this controller never substitutes a public receipt for them.
use crate::onboarding_prepared::{Pending as Prepared, PortableApplications};
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
    io::Error::other("Portable applications are pending or unavailable")
}
fn root(home: &Path) -> PathBuf {
    home.join(".local/state/tilo/prepared-applications")
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Choice {
    schema: u32,
    setup_digest: [u8; 32],
    declaration: PortableApplications,
    outcome: String,
}
#[derive(serde::Deserialize)]
#[serde(tag = "event", deny_unknown_fields)]
enum Event {
    #[serde(rename = "ready-to-restore")]
    Ready {
        applications: usize,
        deployments: usize,
        user_state: bool,
    },
    #[serde(rename = "complete")]
    Complete { restored: usize, verified: bool },
}
struct Worker {
    command: bounded_command::AsyncCommand,
    _scratch: tempfile::TempDir,
}
pub(crate) struct Pending {
    home: PathBuf,
    identity: PathBuf,
    declaration: PortableApplications,
    digest: [u8; 32],
    state: &'static str,
    result: serde_json::Value,
    worker: Option<Worker>,
    password: Zeroizing<Vec<u8>>,
    automatic: bool,
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
impl Pending {
    pub(crate) fn open(
        home: PathBuf,
        identity: PathBuf,
        declaration: PortableApplications,
    ) -> io::Result<Self> {
        if !declaration.valid() {
            return Err(failure());
        }
        let prepared = Prepared::open(&home)?;
        if prepared.portable_applications(&identity)?.as_ref() != Some(&declaration) {
            return Err(failure());
        }
        let state = match private_file(&root(&home).join("choice.json"), 8192) {
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
                    "complete"
                } else {
                    "deferred"
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => "pending",
            Err(error) => return Err(error),
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
            || prepared.portable_applications(&self.identity)?.as_ref() != Some(&self.declaration)
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
        &self.declaration.operation
    }
    pub(crate) fn status(&self) -> serde_json::Value {
        serde_json::json!({"state":self.state,"portable_applications":true,"applications_only":true,"operations":[self.operation()],"selected":self.operation(),"result":self.result,"can_review":!self.busy(),"requires_password":self.password.is_empty()&&!self.busy(),"complete":self.state=="complete","deferred":self.state=="deferred"})
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
            "start" if self.state == "reviewed" => "restore",
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
                    "restoring-applications"
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
        let expected: PortableApplications =
            serde_json::from_slice(&private_file(&directory.join("expectation.json"), 8192)?)
                .map_err(|_| failure())?;
        if expected != self.declaration {
            return Err(failure());
        }
        let package = directory.join(&self.declaration.filename);
        // Check private literal file custody; large hashing/authentication belongs to the helper.
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&package)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o077 != 0
            || metadata.len() != self.declaration.sealed_bytes
        {
            return Err(failure());
        }
        let mut pin = crate::onboarding_migration::directory(&self.home)?;
        for segment in [
            ".local",
            "state",
            "tilo",
            "portable-applications",
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
            .arg("--portable-applications")
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
                    .join(".local/state/tilo/portable-applications")
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
        if bytes.len() > 8192 {
            return Err(failure());
        }
        let mut staged = tempfile::NamedTempFile::new_in(&directory)?;
        staged.write_all(&bytes)?;
        staged.as_file().sync_all()?;
        self.validate()?;
        let destination = directory.join("choice.json");
        match private_file(&destination, 8192) {
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
            if last.len() > 8192 {
                return Err(failure());
            }
            serde_json::from_slice::<Event>(last).map_err(|_| failure())
        });
        match result {
            Ok(Event::Ready {
                applications,
                deployments,
                user_state,
            }) => {
                self.state = "reviewed";
                self.result = serde_json::json!({"applications":applications,"deployments":deployments,"user_state":user_state});
                if self.automatic && self.invoke("start", b"").is_err() {
                    self.automatic = false;
                }
            }
            Ok(Event::Complete {
                restored,
                verified: true,
            }) => {
                self.result = serde_json::json!({"restored":restored,"verified":true});
                self.state = if self.acknowledge("complete").is_ok() {
                    "complete"
                } else {
                    "incomplete"
                };
                self.password.zeroize();
            }
            _ => self.state = "failed",
        }
        true
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::{PermissionsExt, symlink};
    pub(crate) fn fixture() -> (tempfile::TempDir, PathBuf, PortableApplications) {
        let (home, identity, _) = crate::onboarding_portable_home::tests::fixture();
        let prepared = Prepared::open(home.path()).unwrap();
        std::fs::remove_file(
            home.path()
                .join(".local/state/tilo/prepared-setup/activated.json"),
        )
        .unwrap();
        let operation = uuid::Uuid::new_v4().to_string();
        // Real selected user-state bytes make this declaration representative
        // without depending on the random identity's JSON decimal length.
        // This remains a controller fixture, not an archive/Flatpak restore.
        let source = home.path().join("selected-application-state");
        let mut selected = Vec::new();
        let mut state_bytes = 0_u64;
        for (id, settings) in [
            (
                "org.mozilla.firefox",
                serde_json::json!({"homepage":"about:blank","restore_previous_session":false}),
            ),
            (
                "org.videolan.VLC",
                serde_json::json!({"volume":75,"resume_playback":true}),
            ),
            (
                "org.gnome.TextEditor",
                serde_json::json!({"show_line_numbers":true,"tab_width":4}),
            ),
        ] {
            let relative = format!(".var/app/{id}/config/settings.json");
            let path = source.join(&relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let bytes = serde_json::to_vec(&settings).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            state_bytes = state_bytes.saturating_add(u64::try_from(bytes.len()).unwrap());
            selected.push(serde_json::json!({
                "reference":format!("app/{id}/x86_64/stable"),
                "state_path":relative,
                "state_sha256":<[u8;32]>::from(Sha256::digest(&bytes)),
            }));
        }
        let entries = selected.len().saturating_add(1);
        let metadata = serde_json::to_vec(&serde_json::json!({
            "schema":1,"selected_user_state":selected,
        }))
        .unwrap();
        let metadata_path = source.join("selected-user-state.json");
        std::fs::write(&metadata_path, &metadata).unwrap();
        std::fs::set_permissions(&metadata_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let ciphertext = b"fixture ciphertext";
        let declaration = PortableApplications {
            schema: 1,
            operation: operation.clone(),
            purpose: "flatpak_applications".into(),
            filename: format!("applications-{operation}.sealed"),
            setup_binding: "a".repeat(256),
            sealed_bytes: u64::try_from(ciphertext.len()).unwrap(),
            plaintext_bytes: u64::try_from(metadata.len())
                .unwrap()
                .saturating_add(state_bytes),
            entries,
            ciphertext_sha256: Sha256::digest(ciphertext).into(),
            manifest_sha256: Sha256::digest(&metadata).into(),
            limits: crate::onboarding_prepared::ApplicationLimits {
                plaintext_bytes: 1024 * 1024,
                entries: 128,
                metadata_bytes: 64 * 1024,
            },
        };
        let signer = runtime_identity::load_from_path(identity.join("primary.json")).unwrap();
        prepared
            .acknowledge(
                signer.group_public_key(),
                None,
                Some(declaration.clone()),
                None,
                None,
                None,
            )
            .unwrap();
        let root = root(home.path());
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        for (name, bytes) in [
            (
                "expectation.json".to_owned(),
                serde_json::to_vec(&declaration).unwrap(),
            ),
            (declaration.filename.clone(), ciphertext.to_vec()),
        ] {
            let file = root.join(name);
            std::fs::write(&file, bytes).unwrap();
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        (home, identity, declaration)
    }
    fn wait(pending: &mut Pending) {
        tilo_test_env::wait_until("the worker to exit", || {
            if !pending.busy() {
                return true;
            }
            pending.poll();
            false
        });
    }
    #[test]
    fn actual_owned_helper_protocol_gates_completion_and_binds_saved_declaration() {
        let (home, identity, declaration) = fixture();
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
        let helper = home.path().join("inert-helper");
        std::fs::write(&helper,b"#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"event\":\"ready-to-restore\",\"applications\":2,\"deployments\":3,\"user_state\":true}'\n").unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        pending.worker = Some(pending.start_with("review", &helper).unwrap());
        wait(&mut pending);
        assert_eq!(pending.status()["state"], "reviewed");
        assert!(pending.pending());
        assert!(!root(home.path()).join("choice.json").exists());
        std::fs::write(&helper,b"#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"event\":\"complete\",\"restored\":2,\"verified\":false}'\n").unwrap();
        pending.worker = Some(pending.start_with("restore", &helper).unwrap());
        wait(&mut pending);
        assert_eq!(pending.status()["state"], "failed");
        assert!(!root(home.path()).join("choice.json").exists());
        std::fs::write(&helper,b"#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"event\":\"complete\",\"restored\":2,\"verified\":true}'\n").unwrap();
        pending.worker = Some(pending.start_with("restore", &helper).unwrap());
        wait(&mut pending);
        assert_eq!(pending.status()["state"], "complete");
        assert!(!pending.pending());
        assert!(pending.password.is_empty());
        let reopened =
            Pending::open(home.path().into(), identity.clone(), declaration.clone()).unwrap();
        assert!(!reopened.pending());
        let mut changed = declaration.clone();
        changed.manifest_sha256 = [9; 32];
        assert!(Pending::open(home.path().into(), identity, changed).is_err());
        let bytes = std::fs::read(root(home.path()).join(&declaration.filename)).unwrap();
        assert_eq!(bytes, b"fixture ciphertext");
        assert_eq!(
            declaration.ciphertext_sha256,
            <[u8; 32]>::from(Sha256::digest(&bytes))
        );
        let selected = std::fs::read(
            home.path()
                .join("selected-application-state/selected-user-state.json"),
        )
        .unwrap();
        assert_eq!(
            declaration.manifest_sha256,
            <[u8; 32]>::from(Sha256::digest(&selected))
        );
        std::fs::remove_file(root(home.path()).join("expectation.json")).unwrap();
        symlink(
            home.path().join("inert-helper"),
            root(home.path()).join("expectation.json"),
        )
        .unwrap();
        assert!(reopened.start_with("review", &helper).is_err());
    }
}
