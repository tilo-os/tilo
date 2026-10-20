//! A declared portable home stays pending independently of identity activation.
//! The canonical headless helper authenticates and adopts; this controller only
//! retains explicit intent, bounded public progress and completion/defer receipts.
use crate::onboarding_prepared::{Pending as Prepared, PortableHome};
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
    io::Error::other("Portable home migration is pending or unavailable")
}
#[derive(PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Choice {
    schema: u32,
    setup_digest: [u8; 32],
    declaration: PortableHome,
    outcome: String,
}
#[derive(serde::Deserialize)]
#[serde(tag = "event", deny_unknown_fields)]
enum HelperEvent {
    #[serde(rename = "ready-to-adopt")]
    Ready { preview: HelperPreview },
    #[serde(rename = "complete")]
    Complete { result: HelperResult },
    #[serde(rename = "incomplete")]
    Incomplete { result: HelperResult },
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HelperPreview {
    install_items: usize,
    collisions: usize,
    inactive_custody: usize,
    runtime_sockets: u64,
    recovered: bool,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HelperResult {
    complete: bool,
    cancelled: bool,
    installed: usize,
    collisions: usize,
    inactive_custody: usize,
    failures: usize,
    verified: bool,
}
pub(crate) struct Pending {
    home: PathBuf,
    identity: PathBuf,
    declaration: PortableHome,
    digest: [u8; 32],
    state: &'static str,
    result: serde_json::Value,
    worker: Option<Worker>,
    password: Zeroizing<Vec<u8>>,
    automatic: bool,
}
struct Worker {
    command: bounded_command::AsyncCommand,
    _scratch: tempfile::TempDir,
}
fn root(home: &Path) -> PathBuf {
    home.join(".local/state/tilo/prepared-home")
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
pub(crate) fn helper_executable() -> io::Result<&'static Path> {
    let executable = match std::fs::symlink_metadata("/etc/tilo/prepared-setup") {
        Ok(_) => Path::new("/etc/tilo/prepared-setup"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Path::new("/usr/libexec/tilo-prepared-setup")
        }
        Err(_) => return Err(failure()),
    };
    let helper = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(executable)?;
    let metadata = helper.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || metadata.mode() & 0o111 == 0
    {
        return Err(failure());
    }
    Ok(executable)
}

impl Pending {
    pub(crate) fn open(
        home: PathBuf,
        identity: PathBuf,
        declaration: PortableHome,
    ) -> io::Result<Self> {
        if !declaration.valid() {
            return Err(failure());
        }
        let prepared = Prepared::open(&home)?;
        if prepared.portable_home(&identity)?.as_ref() != Some(&declaration) {
            return Err(failure());
        }
        let digest = prepared.digest();
        let state = match private_file(&root(&home).join("choice.json"), 2048) {
            Ok(bytes) => {
                let receipt: Choice = serde_json::from_slice(&bytes).map_err(|_| failure())?;
                if receipt.schema != 1
                    || receipt.setup_digest != digest
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
            digest,
            state,
            result: serde_json::Value::Null,
            worker: None,
            password: Zeroizing::new(Vec::new()),
            automatic: false,
        })
    }
    /// Retain only the credential just authenticated for this exact prepared
    /// package. The onboarding reducer may continue the already-consented migration
    /// with it after activation; retaining the password alone starts no process.
    pub(crate) fn receive_prepared_password(
        &mut self,
        digest: [u8; 32],
        password: Zeroizing<Vec<u8>>,
    ) -> io::Result<()> {
        let prepared = Prepared::open(&self.home)?;
        if self.busy()
            || !self.pending()
            || digest != self.digest
            || prepared.digest() != digest
            || prepared.portable_home(&self.identity)?.as_ref() != Some(&self.declaration)
            || password.is_empty()
            || password.len() > 4096
            || std::str::from_utf8(&password).is_err()
        {
            return Err(failure());
        }
        self.password = password;
        self.automatic = true;
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
    pub(crate) fn operation(&self) -> &str {
        &self.declaration.setup_binding
    }
    pub(crate) fn pending(&self) -> bool {
        !["complete", "deferred"].contains(&self.state)
    }
    pub(crate) fn busy(&self) -> bool {
        self.worker.is_some()
    }
    pub(crate) fn needs_password(&self) -> bool {
        self.password.is_empty() && !self.busy() && self.pending()
    }
    pub(crate) fn status(&self) -> serde_json::Value {
        serde_json::json!({"state":self.state,"portable_home":true,"setup_binding":self.declaration.setup_binding,
            "operations":[self.declaration.setup_binding],"selected":self.declaration.setup_binding,
            "result":self.result,"can_review":!self.busy(),"requires_password":self.needs_password(),
            "complete":self.state=="complete","deferred":self.state=="deferred"})
    }
    pub(crate) fn next_action(&self) -> &'static str {
        if self.state == "ready-to-adopt" {
            "adopt"
        } else {
            "review"
        }
    }
    pub(crate) fn invoke(&mut self, op: &str, password: &[u8]) -> io::Result<()> {
        if op == "resume" && self.state == "deferred" && !self.busy() {
            let prepared = Prepared::open(&self.home)?;
            if prepared.digest() != self.digest
                || prepared.portable_home(&self.identity)?.as_ref() != Some(&self.declaration)
            {
                return Err(failure());
            }
            self.state = "pending";
            return Ok(());
        }
        if op == "later" && !self.busy() {
            self.acknowledge("deferred")?;
            self.state = "deferred";
            self.password.zeroize();
            return Ok(());
        }
        if op == "cancel" && self.busy() {
            self.worker = None;
            self.state = "cancelled";
            self.password.zeroize();
            return Ok(());
        }
        if op == "continue" {
            return Err(failure());
        }
        let action = match op {
            "review" | "retry" if !self.busy() => "review",
            "adopt" if self.state == "ready-to-adopt" && !self.busy() => "adopt",
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
                self.state = if action == "adopt" {
                    "adopting"
                } else {
                    "reviewing"
                };
            }
            Err(error) => {
                self.state = "unavailable";
                self.password.zeroize();
                return Err(error);
            }
        }
        Ok(())
    }
    fn ensure_root(&self) -> io::Result<()> {
        let mut pin = crate::onboarding_migration::directory(&self.home)?;
        for segment in [".local", "state", "tilo", "prepared-home"] {
            pin = crate::onboarding_migration::create(&pin, segment)?;
            crate::onboarding_migration::private(&pin)?;
        }
        Ok(())
    }
    fn start(&self, action: &str) -> io::Result<Worker> {
        let prepared = Prepared::open(&self.home)?;
        if prepared.digest() != self.digest
            || prepared.portable_home(&self.identity)?.as_ref() != Some(&self.declaration)
        {
            return Err(failure());
        }
        let directory = root(&self.home);
        let receipt = private_file(&directory.join("expectation.json"), 16 * 1024)?;
        let receipt: serde_json::Value = serde_json::from_slice(&receipt).map_err(|_| failure())?;
        let operation = receipt
            .get("operation")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(failure)?;
        let filename = receipt
            .get("filename")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(failure)?;
        if operation.is_empty()
            || operation.len() > 96
            || !operation
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            || filename != format!("home-{operation}.sealed")
            || receipt
                .get("setup_binding")
                .and_then(serde_json::Value::as_str)
                != Some(self.declaration.setup_binding.as_str())
        {
            return Err(failure());
        }
        // Refuse missing/redirected files before launching. The helper validates
        // exact ciphertext, limits and authenticated footer, without a large UI-thread hash.
        let pin = crate::onboarding_migration::directory(&directory)?;
        crate::onboarding_migration::private(&pin)?;
        let package = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(directory.join(filename))?;
        let metadata = package.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o077 != 0
        {
            return Err(failure());
        }
        let mut workspace_pin = crate::onboarding_migration::directory(&self.home)?;
        for segment in [".local", "state", "tilo", "portable-home", operation] {
            workspace_pin = crate::onboarding_migration::create(&workspace_pin, segment)?;
            crate::onboarding_migration::private(&workspace_pin)?;
        }
        let scratch = tempfile::Builder::new()
            .prefix(".setup-review-")
            .tempdir_in(&directory)?;
        let executable = helper_executable()?;
        let mut command = Command::new(executable);
        command
            .arg("--portable-home")
            .arg("--package")
            .arg(directory.join(filename))
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
                    .join(".local/state/tilo/portable-home")
                    .join(operation),
            )
            .arg("--action")
            .arg(action);
        let command = bounded_command::spawn_prepared(
            command,
            Some(&self.password),
            16 * 1024,
            Duration::from_secs(3600),
        )
        .map_err(|_| failure())?;
        Ok(Worker {
            command,
            _scratch: scratch,
        })
    }
    fn acknowledge(&self, outcome: &str) -> io::Result<()> {
        self.ensure_root()?;
        let directory = root(&self.home);
        let pin = crate::onboarding_migration::directory(&directory)?;
        crate::onboarding_migration::private(&pin)?;
        let receipt = Choice {
            schema: 1,
            setup_digest: self.digest,
            declaration: self.declaration.clone(),
            outcome: outcome.into(),
        };
        let bytes = serde_json::to_vec(&receipt).map_err(|_| failure())?;
        let destination = directory.join("choice.json");
        let replace = match private_file(&destination, 2048) {
            Ok(existing) => {
                let previous: Choice = serde_json::from_slice(&existing).map_err(|_| failure())?;
                if previous == receipt {
                    return pin.sync_all();
                }
                if previous.schema != 1
                    || previous.setup_digest != self.digest
                    || previous.declaration != self.declaration
                    || previous.outcome != "deferred"
                    || outcome != "complete"
                {
                    return Err(failure());
                }
                true
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return Err(error),
        };
        let mut staged = tempfile::NamedTempFile::new_in(&directory)?;
        staged.write_all(&bytes)?;
        staged.as_file().sync_all()?;
        let prepared = Prepared::open(&self.home)?;
        if prepared.digest() != self.digest
            || prepared.portable_home(&self.identity)?.as_ref() != Some(&self.declaration)
        {
            return Err(failure());
        }
        if replace {
            staged.persist(&destination).map_err(|_| failure())?;
        } else {
            staged
                .persist_noclobber(&destination)
                .map_err(|_| failure())?;
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
            serde_json::from_slice::<HelperEvent>(last).map_err(|_| failure())
        });
        match result {
            Ok(HelperEvent::Ready { preview }) => {
                self.state = "ready-to-adopt";
                let safe_to_continue = preview.collisions == 0;
                self.result = serde_json::to_value(preview).unwrap_or_default();
                if self.automatic && safe_to_continue && self.invoke("adopt", b"").is_err() {
                    self.automatic = false;
                }
            }
            Ok(HelperEvent::Complete { result })
                if result.complete
                    && result.verified
                    && !result.cancelled
                    && result.failures == 0
                    && result.collisions == 0 =>
            {
                self.result = serde_json::to_value(result).unwrap_or_default();
                self.state = if self.acknowledge("complete").is_ok() {
                    "complete"
                } else {
                    "incomplete"
                };
                self.password.zeroize();
            }
            Ok(HelperEvent::Incomplete { result }) => {
                self.state = "incomplete";
                self.result = serde_json::to_value(result).unwrap_or_default();
                self.password.zeroize();
            }
            _ => {
                self.state = "failed";
                self.password.zeroize();
            }
        }
        true
    }
}
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;
    pub(crate) fn fixture() -> (tempfile::TempDir, PathBuf, PortableHome) {
        let home = tempfile::tempdir().unwrap();
        let state = home.path().join(".local/state/tilo/prepared-setup");
        std::fs::create_dir_all(&state).unwrap();
        for part in [
            ".local",
            ".local/state",
            ".local/state/tilo",
            ".local/state/tilo/prepared-setup",
        ] {
            std::fs::set_permissions(
                home.path().join(part),
                std::fs::Permissions::from_mode(0o700),
            )
            .unwrap();
        }
        let bytes = b"private transport boundary fixture ciphertext";
        for (name, value) in [("prepared-setup.sealed", bytes.to_vec()),("receipt.json", serde_json::to_vec(&serde_json::json!({"schema":1,"sealed_bytes":bytes.len(),"ciphertext_sha256":hex::encode(Sha256::digest(bytes))})).unwrap())] {
            let path=state.join(name);std::fs::write(&path,value).unwrap();std::fs::set_permissions(path,std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let identity = home.path().join(".config/tilo/identity");
        std::fs::create_dir_all(&identity).unwrap();
        for part in [".config", ".config/tilo", ".config/tilo/identity"] {
            std::fs::set_permissions(
                home.path().join(part),
                std::fs::Permissions::from_mode(0o700),
            )
            .unwrap();
        }
        tilo_identity_host::provisioning::create_primary_identity(
            &identity,
            "Already imported",
            [10, 40, 60],
            tilo_identity_host::provisioning::IdentityStorage::for_session(),
        )
        .unwrap();
        runtime_identity::set_active_identity(&identity, "primary").unwrap();
        let signer = runtime_identity::load_from_path(identity.join("primary.json")).unwrap();
        let declaration = PortableHome {
            schema: 1,
            setup_binding: "installed-operation".into(),
            selection: "full_home".into(),
            applications: false,
        };
        Prepared::open(home.path())
            .unwrap()
            .acknowledge(
                signer.group_public_key(),
                Some(declaration.clone()),
                None,
                None,
                None,
                None,
            )
            .unwrap();
        (home, identity, declaration)
    }
    #[test]
    fn authenticated_password_handoff_is_bound_to_exact_setup_and_defer_clears_it() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let (home, identity, declaration) = fixture();
        let digest = Prepared::open(home.path()).unwrap().digest();
        let mut pending = Pending::open(home.path().to_path_buf(), identity, declaration).unwrap();
        let secret = b"previously authenticated setup password";
        assert!(
            pending
                .receive_prepared_password([0; 32], Zeroizing::new(secret.to_vec()))
                .is_err()
        );
        assert!(pending.needs_password());
        pending
            .receive_prepared_password(digest, Zeroizing::new(secret.to_vec()))
            .unwrap();
        assert!(!pending.needs_password());
        assert_eq!(pending.state, "pending");
        assert!(!pending.busy());
        assert!(
            !pending
                .status()
                .to_string()
                .contains(std::str::from_utf8(secret).unwrap())
        );
        pending.invoke("later", &[]).unwrap();
        assert!(pending.password.is_empty());
        let mut reopened = Pending::open(
            pending.home.clone(),
            pending.identity.clone(),
            pending.declaration.clone(),
        )
        .unwrap();
        reopened.invoke("resume", &[]).unwrap();
        assert!(reopened.needs_password());
    }

    #[test]
    fn declared_missing_home_stays_pending_and_explicit_defer_is_not_completion() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let (home, identity, declaration) = fixture();
        let before = std::fs::read(identity.join("primary.json")).unwrap();
        let setup = std::fs::read(
            home.path()
                .join(".local/state/tilo/prepared-setup/prepared-setup.sealed"),
        )
        .unwrap();
        let mut pending =
            Pending::open(home.path().into(), identity.clone(), declaration.clone()).unwrap();
        assert!(pending.pending());
        assert!(pending.invoke("continue", b"").is_err());
        assert!(pending.invoke("review", b"private setup password").is_err());
        assert_eq!(pending.status()["state"], "unavailable");
        assert!(pending.pending());
        assert!(!pending.busy());
        assert!(
            !pending
                .status()
                .to_string()
                .contains("private setup password")
        );
        assert!(pending.invoke("review", b"private setup password").is_err());
        pending.invoke("later", b"").unwrap();
        assert_eq!(pending.status()["state"], "deferred");
        assert!(!pending.pending());
        let receipt: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root(home.path()).join("choice.json")).unwrap())
                .unwrap();
        assert_eq!(receipt["outcome"], "deferred");
        assert_eq!(
            receipt["declaration"]["setup_binding"],
            declaration.setup_binding
        );
        assert_eq!(
            std::fs::read(identity.join("primary.json")).unwrap(),
            before
        );
        assert_eq!(
            std::fs::read(
                home.path()
                    .join(".local/state/tilo/prepared-setup/prepared-setup.sealed")
            )
            .unwrap(),
            setup
        );
        let mut resumed = Pending::open(home.path().into(), identity, declaration).unwrap();
        assert!(!resumed.pending());
        resumed.invoke("resume", b"").unwrap();
        assert!(resumed.pending());
        assert!(resumed.invoke("review", b"private setup password").is_err());
        assert!(resumed.pending());
        resumed.invoke("later", b"").unwrap();
        assert_eq!(resumed.status()["state"], "deferred");
    }
    #[test]
    fn declaration_receipt_refuses_wrong_binding_and_unsafe_defer_destination() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let (home, identity, declaration) = fixture();
        let mut wrong = declaration.clone();
        wrong.setup_binding = "unrelated-operation".into();
        assert!(Pending::open(home.path().into(), identity.clone(), wrong).is_err());
        let elsewhere = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), root(home.path())).unwrap();
        assert!(Pending::open(home.path().into(), identity, declaration).is_err());
        assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
    }
}
