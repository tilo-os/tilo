//! Explicit existing-device enrollment into isolated future-device custody.
//! The canonical helper owns the protocol; public status is never custody proof.
use crate::{
    destination_security, migration::PinnedDirectory, prepared_identity::PreparedIdentity,
};
use std::{
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{Duration, SystemTime},
};

const PROCESS_BOUND: Duration = Duration::from_secs(840);
const STATUS_LIMIT: u64 = 8192;

struct Custody {
    root: PathBuf,
    identities: PathBuf,
    status: PathBuf,
    pin: Arc<PinnedDirectory>,
    root_pin: Arc<PinnedDirectory>,
    identity_pin: Arc<PinnedDirectory>,
}

#[derive(Default)]
pub struct Pairing {
    process: Option<bounded_command::AsyncCommand>,
    custody: Option<Custody>,
    code: Option<String>,
    prepared: Option<Arc<PreparedIdentity>>,
    state: &'static str,
    approved_label: Option<String>,
}

impl Pairing {
    pub fn resume(prepared: Arc<PreparedIdentity>) -> std::io::Result<Self> {
        let label = prepared
            .enrolled_device_label()
            .ok_or_else(|| std::io::Error::other("Prepared device is not enrolled custody"))?
            .to_owned();
        Ok(Self {
            prepared: Some(prepared),
            approved_label: Some(label),
            state: "ready",
            ..Self::default()
        })
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn fixture_operation_directory(&self) -> PathBuf {
        self.custody
            .as_ref()
            .unwrap()
            .root
            .parent()
            .unwrap()
            .to_owned()
    }
    pub fn busy(&self) -> bool {
        self.process.is_some()
    }
    pub fn ready(&self) -> bool {
        self.prepared.is_some()
    }
    pub fn prepared(&self) -> Option<Arc<PreparedIdentity>> {
        self.prepared.clone()
    }
    pub fn start(
        &mut self,
        label: &str,
        requester_pid: u32,
        requester_start_time: u64,
    ) -> Result<(), &'static str> {
        self.start_resolving(label, requester_pid, requester_start_time, trusted_helper)
    }
    fn start_resolving(
        &mut self,
        label: &str,
        requester_pid: u32,
        requester_start_time: u64,
        helper: impl FnOnce() -> Option<PathBuf>,
    ) -> Result<(), &'static str> {
        if self.busy() || self.ready() {
            return Err("installer-pairing-busy");
        }
        if label.is_empty() || label.len() > 64 || label.chars().any(char::is_control) {
            return Err("installer-preview-device-label-invalid");
        }
        let helper = helper().ok_or("installer-pairing-component-missing")?;
        let directory = preparation_directory().map_err(|_| "installer-pairing-failed")?;
        let operation = directory.as_path();
        let pin = ensure_private_tree(operation).map_err(|_| "installer-pairing-failed")?;
        destination_security::check_private_file(&pin.file)
            .map_err(|_| "installer-pairing-failed")?;
        let root = operation.join("future-device");
        let root_pin = pin
            .create_child(std::ffi::OsStr::new("future-device"))
            .map_err(|_| "installer-pairing-failed")?;
        let identity_pin = pin
            .create_child(std::ffi::OsStr::new("identities"))
            .map_err(|_| "installer-pairing-failed")?;
        let custody = Custody {
            root,
            identities: operation.join("identities"),
            status: operation.join("pairing-status.json"),
            pin,
            root_pin,
            identity_pin,
        };
        let mut command = Command::new(helper);
        command
            .arg("pair-device")
            .arg("--identity-root")
            .arg(&custody.root)
            .arg("--identity-dir")
            .arg(&custody.identities)
            .args(["--label", label, "--timeout", "270"])
            .arg("--status-file")
            .arg(&custody.status)
            .arg("--requester-pid")
            .arg(requester_pid.to_string())
            .arg("--requester-start-time")
            .arg(requester_start_time.to_string());
        self.begin(command, custody, PROCESS_BOUND)?;
        self.approved_label = Some(label.to_owned());
        Ok(())
    }
    fn begin(
        &mut self,
        command: Command,
        custody: Custody,
        timeout: Duration,
    ) -> Result<(), &'static str> {
        let process = bounded_command::spawn_prepared(command, None, 16384, timeout)
            .map_err(|_| "installer-pairing-failed")?;
        self.process = Some(process);
        self.custody = Some(custody);
        self.prepared = None;
        self.code = None;
        self.state = "starting";
        Ok(())
    }
    pub fn cancel(&mut self) -> Result<(), &'static str> {
        if !self.busy() {
            return Err("installer-pairing-not-running");
        }
        self.process = None;
        self.code = None;
        self.state = "cancelled";
        // Custody is deliberately retained: local cancellation does not revoke
        // any certificate the other device may already have approved.
        Ok(())
    }
    pub fn poll(&mut self) -> bool {
        let Some(custody) = &self.custody else {
            return false;
        };
        if !self.busy() {
            return false;
        }
        let status = read_status(custody);
        let mut changed = false;
        if let Ok(value) = &status
            && let Some(code) = public_code(value)
            && self.code.as_ref() != Some(&code)
        {
            self.code = Some(code);
            self.state = "waiting";
            changed = true;
        }
        let Some(process) = &mut self.process else {
            return changed;
        };
        let Some(result) = process.try_wait() else {
            return changed;
        };
        self.process = None;
        let result = result.map_err(|_| ()).and_then(|output| {
            if output.status.success() {
                read_status(custody).map_err(|_| ())
            } else {
                Err(())
            }
        });
        match result
            .and_then(|status| authenticate(custody, &status, self.approved_label.as_deref()))
        {
            Ok(prepared) => {
                self.prepared = Some(Arc::new(prepared));
                self.state = "ready";
            }
            Err(()) => self.state = "failed",
        }
        true
    }
    pub fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }
    pub fn state(&self) -> &'static str {
        self.state
    }
    pub fn projection(&self) -> serde_json::Value {
        serde_json::json!({"state": if self.state.is_empty() { "unconnected" } else { self.state }, "verification_code": self.code, "device_label": self.approved_label, "public_key": self.prepared.as_ref().map(|value| hex::encode(value.public_key()))})
    }
}

fn preparation_directory() -> std::io::Result<PathBuf> {
    let state = if let Some(selected) =
        std::env::var_os("XDG_STATE_HOME").filter(|path| !path.is_empty())
    {
        crate::home_transfer::granted_root(Path::new(&selected))?
    } else {
        crate::home_transfer::current_home()
            .map_err(std::io::Error::other)?
            .join(".local/state")
    };
    Ok(state
        .join("tilo/installer/preparations")
        .join(uuid::Uuid::new_v4().to_string()))
}

fn trusted_helper() -> Option<PathBuf> {
    let current = std::env::current_exe().ok()?.canonicalize().ok()?;
    let sibling = current.parent()?.join("tilo-enroll");
    trusted_helper_from(
        &current,
        [sibling, PathBuf::from("/usr/libexec/tilo/tilo-enroll")],
    )
}

fn trusted_helper_from(
    current: &Path,
    candidates: impl IntoIterator<Item = PathBuf>,
) -> Option<PathBuf> {
    candidates.into_iter().find(|path| {
        std::fs::symlink_metadata(path).is_ok_and(|metadata| {
            metadata.is_file()
                && !metadata.is_symlink()
                && metadata.mode() & 0o022 == 0
                && metadata.mode() & 0o111 != 0
                && (metadata.uid() == 0
                    || std::fs::metadata(current).is_ok_and(|owner| owner.uid() == metadata.uid()))
        })
    })
}

fn read_status(custody: &Custody) -> std::io::Result<serde_json::Value> {
    custody.pin.validate()?;
    custody.root_pin.validate()?;
    custody.identity_pin.validate()?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&custody.status)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > STATUS_LIMIT {
        return Err(std::io::Error::other("Invalid pairing status"));
    }
    if metadata.uid() != custody.pin.file.metadata()?.uid()
        || metadata.mode() & 0o022 != 0
        || metadata.nlink() != 1
    {
        return Err(std::io::Error::other("Untrusted pairing status"));
    }
    let mut bytes = Vec::new();
    file.take(STATUS_LIMIT + 1).read_to_end(&mut bytes)?;
    custody.pin.validate()?;
    if bytes.len() > 8192 {
        return Err(std::io::Error::other("Oversized pairing status"));
    }
    serde_json::from_slice(&bytes).map_err(std::io::Error::other)
}
fn public_code(status: &serde_json::Value) -> Option<String> {
    let code = status.get("verification_code")?.as_str()?;
    (!code.is_empty()
        && code.len() <= 32
        && code
            .chars()
            .all(|char| char.is_ascii_alphanumeric() || char == '-'))
    .then(|| code.to_owned())
}
fn authenticate(
    custody: &Custody,
    status: &serde_json::Value,
    label: Option<&str>,
) -> Result<PreparedIdentity, ()> {
    if status.get("state").and_then(serde_json::Value::as_str) != Some("complete") {
        return Err(());
    }
    let root = status
        .get("identity")
        .and_then(serde_json::Value::as_str)
        .ok_or(())?;
    let expected_root: [u8; 32] = hex::decode(root)
        .map_err(|_| ())?
        .try_into()
        .map_err(|_| ())?;
    let record = PathBuf::from(
        status
            .get("record_path")
            .and_then(serde_json::Value::as_str)
            .ok_or(())?,
    );
    if record.parent() != Some(custody.identities.as_path()) {
        return Err(());
    }
    custody.pin.validate().map_err(|_| ())?;
    custody.root_pin.validate().map_err(|_| ())?;
    custody.identity_pin.validate().map_err(|_| ())?;
    let paths = tilo_user_identity::persist::IdentityPaths::under(&custody.root);
    let device = tilo_user_identity::persist::load_device_key(&paths).map_err(|_| ())?;
    let now = u64::try_from(
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|_| ())?
            .as_nanos(),
    )
    .map_err(|_| ())?;
    let enrolled = tilo_identity_host::enrolled_transport::EnrolledDevice::review(
        &custody.root,
        &record,
        expected_root,
        device.verifying_key().to_bytes(),
        now,
    )
    .map_err(|_| ())?;
    if Some(enrolled.device_label()) != label {
        return Err(());
    }
    PreparedIdentity::from_enrolled(Arc::new(enrolled)).map_err(|_| ())
}

/// Actual locally approved canonical custody, served by an inert helper.
/// No pairing network or live account paths are touched by this fixture.
#[cfg(any(test, feature = "test-support"))]
pub fn certified_fixture() -> (tempfile::TempDir, Pairing) {
    use std::os::unix::fs::PermissionsExt as _;
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temporary = tempfile::tempdir().unwrap();
    let state = temporary.path().canonicalize().unwrap();
    let directory = state
        .join("tilo/installer/preparations")
        .join(uuid::Uuid::new_v4().to_string());
    ensure_private_tree(&directory).unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let root_identity = PreparedIdentity::create(
        &directory.join("approver"),
        "Approving person",
        [10, 20, 30],
        tilo_identity_host::provisioning::IdentityStorage::for_session(),
    )
    .unwrap();
    let signer = runtime_identity::load_from_path(directory.join("approver/primary.json")).unwrap();
    let system = directory.join("future-device");
    let identities = directory.join("identities");
    std::fs::create_dir(&system).unwrap();
    std::fs::create_dir(&identities).unwrap();
    std::fs::set_permissions(&system, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::set_permissions(&identities, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::create_dir(identities.join("fleets")).unwrap();
    let paths = tilo_user_identity::persist::IdentityPaths::under(&system);
    let (device, _) = tilo_user_identity::persist::load_or_create_device_key(&paths).unwrap();
    let mut certificate = tilo_user_identity::DeviceCert {
        user_public_key: root_identity.public_key(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "Approved future computer".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    certificate.signature = signer
        .sign(&tilo_user_identity::device_cert_signing_digest(&certificate).unwrap())
        .unwrap();
    tilo_user_identity::persist::write_public_key(&paths.user_pubkey, &certificate.user_public_key)
        .unwrap();
    tilo_user_identity::persist::write_device_cert(&paths.device_cert, &certificate).unwrap();
    let record = identities.join("member.json");
    let bytes = runtime_identity::public_member_record(
        "Approving person",
        [10, 20, 30],
        certificate.user_public_key,
    )
    .unwrap();
    tilo_files::native::atomic::write_with_mode(&record, &bytes, 0o600).unwrap();
    let mut fleet = tilo_identity_host::device_fleet::DeviceFleet::new(hex::encode(
        certificate.user_public_key,
    ));
    fleet.certificates.push(certificate.clone());
    tilo_files::native::atomic::write_with_mode(
        &identities
            .join("fleets")
            .join(format!("{}.json", hex::encode(certificate.user_public_key))),
        &serde_json::to_vec(&fleet).unwrap(),
        0o600,
    )
    .unwrap();
    let custody = Custody {
        root_pin: PinnedDirectory::open_absolute(&system).unwrap(),
        identity_pin: PinnedDirectory::open_absolute(&identities).unwrap(),
        root: system,
        identities,
        status: directory.join("status.json"),
        pin: PinnedDirectory::open_absolute(&directory).unwrap(),
    };
    let completed = serde_json::json!({"state":"complete", "identity":hex::encode(certificate.user_public_key), "record_path":record, "verification_code":"123456"});
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(r#"printf '%s' "$2" > "$1""#)
        .arg("fixture")
        .arg(&custody.status)
        .arg(completed.to_string());
    let mut pairing = Pairing::default();
    pairing
        .begin(command, custody, Duration::from_secs(5))
        .unwrap();
    pairing.approved_label = Some(certificate.device_label);
    (temporary, pairing)
}

/// Waiting public status from an inert owned process; never invokes enrollment.
#[cfg(any(test, feature = "test-support"))]
pub fn waiting_fixture() -> (tempfile::TempDir, Pairing) {
    use std::os::unix::fs::PermissionsExt as _;
    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary.path().canonicalize().unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let root = directory.join("future-device");
    let identities = directory.join("identities");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&identities).unwrap();
    let custody = Custody {
        root_pin: PinnedDirectory::open_absolute(&root).unwrap(),
        identity_pin: PinnedDirectory::open_absolute(&identities).unwrap(),
        root,
        identities,
        status: directory.join("status.json"),
        pin: PinnedDirectory::open_absolute(&directory).unwrap(),
    };
    let mut command = Command::new("/bin/sh");
    command.arg("-c")
        .arg(r#"printf '%s' '{"state":"waiting","verification_code":"123456"}' > "$1"; exec sleep 120"#)
        .arg("fixture").arg(&custody.status);
    let mut pairing = Pairing::default();
    pairing
        .begin(command, custody, Duration::from_secs(180))
        .unwrap();
    pairing.approved_label = Some("My future computer".into());
    (temporary, pairing)
}

#[cfg(any(test, feature = "test-support"))]
pub fn failed_fixture() -> (tempfile::TempDir, Pairing) {
    use std::os::unix::fs::PermissionsExt as _;

    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary.path().canonicalize().unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let root = directory.join("future-device");
    let identities = directory.join("identities");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&identities).unwrap();
    let custody = Custody {
        root_pin: PinnedDirectory::open_absolute(&root).unwrap(),
        identity_pin: PinnedDirectory::open_absolute(&identities).unwrap(),
        root,
        identities,
        status: directory.join("status.json"),
        pin: PinnedDirectory::open_absolute(&directory).unwrap(),
    };
    let record = custody.identities.join("forged.json");
    let fake =
        serde_json::json!({"state":"complete", "identity":"03".repeat(32), "record_path":record});
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(r#"printf '%s' "$2" > "$1""#)
        .arg("fixture")
        .arg(&custody.status)
        .arg(fake.to_string());
    let mut pairing = Pairing::default();
    pairing
        .begin(command, custody, Duration::from_secs(5))
        .unwrap();
    (temporary, pairing)
}

fn ensure_private_tree(path: &Path) -> std::io::Result<Arc<PinnedDirectory>> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => {
            let pin = PinnedDirectory::open_absolute(path)?;
            pin.validate_destination()?;
            Ok(pin)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = ensure_private_tree(path.parent().ok_or(error)?)?;
            let child = parent.create_child(path.file_name().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "Missing custody name")
            })?)?;
            crate::migration_durability::sync_directory(&child.file)?;
            crate::migration_durability::sync_directory(&parent.file)?;
            Ok(child)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::fs::PermissionsExt, time::Instant};

    fn fixture() -> (tempfile::TempDir, Custody) {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().canonicalize().unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let root = directory.join("future-device");
        let identities = directory.join("identities");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&identities).unwrap();
        let custody = Custody {
            root_pin: PinnedDirectory::open_absolute(&root).unwrap(),
            identity_pin: PinnedDirectory::open_absolute(&identities).unwrap(),
            root,
            identities,
            status: directory.join("status.json"),
            pin: PinnedDirectory::open_absolute(&directory).unwrap(),
        };
        (temporary, custody)
    }
    fn wait(pairing: &mut Pairing, ready: impl Fn(&Pairing) -> bool) {
        let deadline = tilo_test_env::hang_deadline();
        while !ready(pairing) {
            assert!(
                Instant::now() < deadline,
                "controller never observed helper result"
            );
            pairing.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    #[test]
    fn missing_or_unsafe_helper_fails_before_creating_custody() {
        let mut env = tilo_test_env::ScopedEnv::new();
        let temporary = tempfile::tempdir().unwrap();
        let state = temporary.path().join("state");
        env.set("XDG_STATE_HOME", state.as_os_str());
        let current = std::env::current_exe().unwrap().canonicalize().unwrap();
        let helper = temporary.path().join("tilo-enroll");
        let mut pairing = Pairing::default();
        let mut check = || {
            assert_eq!(
                pairing.start_resolving("Future computer", 1, 1, || {
                    trusted_helper_from(&current, [helper.clone()])
                }),
                Err("installer-pairing-component-missing")
            );
            assert!(!pairing.busy());
            assert!(!pairing.ready());
            assert!(pairing.custody.is_none());
            assert!(pairing.prepared().is_none());
            assert!(!state.exists());
        };
        check();
        std::fs::write(&helper, b"must never execute").unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o600)).unwrap();
        check();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o777)).unwrap();
        check();
        std::fs::remove_file(&helper).unwrap();
        std::os::unix::fs::symlink(&current, &helper).unwrap();
        check();
        std::fs::remove_file(&helper).unwrap();
        std::fs::create_dir(&helper).unwrap();
        check();
    }

    #[test]
    fn trusted_helper_accepts_owned_executable_and_falls_back_after_unsafe_candidate() {
        let temporary = tempfile::tempdir().unwrap();
        let current = std::env::current_exe().unwrap().canonicalize().unwrap();
        let unsafe_candidate = temporary.path().join("tilo-enroll");
        std::os::unix::fs::symlink(&current, &unsafe_candidate).unwrap();
        assert_eq!(
            trusted_helper_from(&current, [unsafe_candidate, current.clone()]),
            Some(current)
        );
    }

    #[test]
    fn explicit_pairing_observes_only_public_code_and_cancel_reaps_process_tree() {
        let (temporary, custody) = fixture();
        let child = temporary.path().join("child.pid");
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(r#"sleep 60 & child=$!; printf '%s' "$child" > "$2"; printf '%s' '{"state":"waiting_for_approval","verification_code":"ABCD-1234","error":"PRIVATE_DETAIL"}' > "$1"; wait"#)
            .arg("fixture").arg(&custody.status).arg(&child);
        let mut pairing = Pairing::default();
        assert!(!pairing.busy());
        pairing
            .begin(command, custody, Duration::from_secs(60))
            .unwrap();
        wait(&mut pairing, |value| value.code.is_some());
        assert_eq!(pairing.projection()["verification_code"], "ABCD-1234");
        assert!(!pairing.projection().to_string().contains("PRIVATE_DETAIL"));
        assert!(!pairing.ready());
        let pid = std::fs::read_to_string(child)
            .unwrap()
            .parse::<i32>()
            .unwrap();
        pairing.cancel().unwrap();
        assert!(!pairing.busy());
        assert!(!pairing.ready());
        assert_eq!(pairing.projection()["state"], "cancelled");
        // A killed orphan may briefly remain a zombie; it must never run.
        if let Ok(status) = std::fs::read_to_string(format!("/proc/{pid}/status")) {
            assert!(status.lines().any(|line| line.starts_with("State:")
                && (line.contains("Z (zombie)") || line.contains("X (dead)"))));
        }
    }
    #[test]
    fn forged_complete_status_never_becomes_enrolled_custody() {
        let (_temporary, custody) = fixture();
        let record = custody.identities.join("forged.json");
        let fake = serde_json::json!({"state":"complete", "identity":"03".repeat(32), "record_path": record});
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(r#"printf '%s' "$2" > "$1""#)
            .arg("fixture")
            .arg(&custody.status)
            .arg(fake.to_string());
        let mut pairing = Pairing::default();
        pairing
            .begin(command, custody, tilo_test_env::hang_bound())
            .unwrap();
        wait(&mut pairing, |value| !value.busy());
        assert_eq!(pairing.projection()["state"], "failed");
        assert!(!pairing.ready());
    }
    #[test]
    fn status_reader_refuses_redirects_and_nonregular_files() {
        let (temporary, custody) = fixture();
        let external = temporary.path().join("external");
        std::fs::write(&external, b"{}").unwrap();
        std::os::unix::fs::symlink(&external, &custody.status).unwrap();
        assert!(read_status(&custody).is_err());
        std::fs::remove_file(&custody.status).unwrap();
        let path = std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(
            custody.status.as_os_str(),
        ))
        .unwrap();
        // SAFETY: path is a valid fixture-only NUL-terminated pathname.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        assert!(read_status(&custody).is_err());
    }
}
