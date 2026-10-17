//! Fixed installed candidate rehearsal after genuine pending preparation.
use crate::setup::{
    home_capture::runtime::{CaptureServicePins, verify_sidecar},
    original_admission::{ConsumerCapability, PROOF_BOUND},
};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    process::{Command, Stdio},
};
const HELPER: &str = "/usr/libexec/tilo-prepared-setup";
const PRODUCER: &str = "/usr/libexec/tilo-verify-original-source-consumer-capability";
const SIDECAR: &str = "/usr/share/tilo-imager/capture";
const RECEIPT: &str = "/usr/share/tilo/original-source-consumer-capabilities.json";
const PINS: &str = "/usr/lib/tilo-qualification/pins.json";
const ARTIFACT_BOUND: u64 = 1024 * 1024 * 1024;
type Stamp = (u64, u64, u64, u32, u32, u32, i64, i64, i64, i64);
fn stamp(file: &File) -> Stamp {
    let m = file.metadata().unwrap();
    (
        m.dev(),
        m.ino(),
        m.len(),
        m.uid(),
        m.gid(),
        m.mode(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    )
}
fn protected(path: &str, bound: u64, executable: bool) -> File {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .expect("fixed installed candidate artifact must exist without redirects");
    let m = file.metadata().unwrap();
    assert!(m.is_file() && m.uid() == 0 && m.nlink() == 1);
    assert!(m.mode() & 0o022 == 0 && m.len() > 0 && m.len() <= bound);
    assert!(!executable || m.mode() & 0o111 != 0);
    let named = fs::symlink_metadata(path).unwrap();
    assert!(named.is_file());
    assert_eq!((m.dev(), m.ino()), (named.dev(), named.ino()));
    file
}
fn read(file: &File, bound: u64) -> Vec<u8> {
    let mut copy = file.try_clone().unwrap();
    copy.seek(SeekFrom::Start(0)).unwrap();
    let mut bytes = Vec::new();
    copy.take(bound + 1).read_to_end(&mut bytes).unwrap();
    assert!(bytes.len() as u64 <= bound);
    bytes
}
fn digest(file: &File) -> String {
    let before = stamp(file);
    let mut copy = file.try_clone().unwrap();
    copy.seek(SeekFrom::Start(0)).unwrap();
    let mut hash = Sha256::new();
    let mut chunk = [0_u8; 64 * 1024];
    loop {
        let count = copy.read(&mut chunk).unwrap();
        if count == 0 {
            break;
        }
        hash.update(&chunk[..count]);
    }
    assert_eq!(
        stamp(file),
        before,
        "candidate artifact changed during hashing"
    );
    hex::encode(hash.finalize())
}
fn proof(bytes: &[u8]) -> ConsumerCapability {
    assert!(bytes.len() <= PROOF_BOUND);
    let value: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    assert!(ConsumerCapability::wire_shape(&value));
    let result: ConsumerCapability = serde_json::from_slice(bytes).unwrap();
    result.validate().unwrap();
    result
}
pub(super) fn qualify() {
    assert_eq!(rustix::process::getuid().as_raw(), 0);
    assert_eq!(rustix::process::geteuid().as_raw(), 0);
    assert!(
        matches!(fs::symlink_metadata(RECEIPT),Err(e) if e.kind()==std::io::ErrorKind::NotFound),
        "candidate must start without a fabricated or previous qualification receipt"
    );
    if super::paired::user_applications() {
        for path in [
            "/usr/share/tilo/applications-consumer-capabilities.json",
            "/usr/share/tilo/applications-consumer-native-qualification.json",
        ] {
            assert!(
                matches!(fs::symlink_metadata(path), Err(e) if e.kind()==std::io::ErrorKind::NotFound)
            );
        }
    }
    let helper = protected(HELPER, ARTIFACT_BOUND, true);
    // Applications are produced by the helper's own verifier mode.
    let (selected_producer, producer_mode, producer_bound) = if super::paired::user_applications() {
        (
            HELPER,
            Some("--verify-applications-consumer-capability"),
            ARTIFACT_BOUND,
        )
    } else {
        (PRODUCER, None, 1024 * 1024)
    };
    let producer = protected(selected_producer, producer_bound, true);
    let launcher = protected("/usr/bin/tilo-launcher", ARTIFACT_BOUND, true);
    let pins_file = protected(PINS, 16 * 1024, false);
    let pins: CaptureServicePins = serde_json::from_slice(&read(&pins_file, 16 * 1024)).unwrap();
    pins.validate().unwrap();
    // The signed validation extension is one exact current worker cohort,
    // including Home and mounted routes; wire phases still enforce their
    // own material-specific protocol inside the authenticated worker.
    let protocol = 6;
    assert_eq!(
        crate::setup::home_capture::worker::APPLICATIONS_PROTOCOL,
        protocol
    );
    assert_eq!(pins.worker_protocol, protocol);
    let service = verify_sidecar(Path::new(SIDECAR), &pins).unwrap();
    let helper_sha = digest(&helper);
    let launcher_sha = digest(&launcher);
    let producer_sha = digest(&producer);
    let pins_stamp = stamp(&pins_file);
    // Fixed dispatcher runs its real factory once: no source, proof or grant arguments.
    assert!(
        Command::new(selected_producer)
            .args(producer_mode)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .status()
            .expect("installed producer must execute")
            .success()
    );
    let receipt = protected(RECEIPT, PROOF_BOUND as u64, false);
    let receipt_stamp = stamp(&receipt);
    let bytes = read(&receipt, PROOF_BOUND as u64);
    let qualified = proof(&bytes);
    assert_eq!(qualified.original_source_protocol, protocol);
    assert_eq!(qualified.prepared_helper_sha256, helper_sha);
    assert_eq!(qualified.installed_launcher_sha256, launcher_sha);
    assert_eq!(
        serde_json::to_value(&qualified.capture_pins).unwrap(),
        serde_json::to_value(&pins).unwrap()
    );
    let after = verify_sidecar(Path::new(SIDECAR), &pins).unwrap();
    assert_eq!(after.package_hash, service.package_hash);
    assert_eq!(after.root, service.root);
    assert_eq!(digest(&helper), helper_sha);
    assert_eq!(digest(&launcher), launcher_sha);
    assert_eq!(digest(&producer), producer_sha);
    assert_eq!(stamp(&pins_file), pins_stamp);
    let output = tempfile::tempfile().unwrap();
    // A distinct nonroot process with empty supplementary groups reattests the
    // fixed receipt; this action never reruns the original export.
    assert!(
        Command::new("/usr/bin/setpriv")
            .args([
                "--reuid=1000",
                "--regid=1001",
                "--clear-groups",
                "--no-new-privs",
                "--",
                PRODUCER,
                "--check"
            ])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::from(output.try_clone().unwrap()))
            .status()
            .expect("rootless installed reattestation must execute")
            .success()
    );
    assert_eq!(proof(&read(&output, PROOF_BOUND as u64)), qualified);
    assert_eq!(stamp(&receipt), receipt_stamp);
    let named = protected(RECEIPT, PROOF_BOUND as u64, false);
    assert_eq!(stamp(&named), receipt_stamp);
    assert_eq!(read(&named, PROOF_BOUND as u64), bytes);
    assert_eq!(digest(&helper), helper_sha);
    assert_eq!(digest(&producer), producer_sha);
    if super::paired::user_applications() {
        let path = "/usr/share/tilo/applications-consumer-capabilities.json";
        let native_path = "/usr/share/tilo/applications-consumer-native-qualification.json";
        let cap_file = protected(path, 4096, false);
        let native_file = protected(native_path, 8192, false);
        let cap_stamp = stamp(&cap_file);
        let native_stamp = stamp(&native_file);
        let cap: crate::setup::applications_admission::ConsumerCapability =
            serde_json::from_slice(&read(&cap_file, 4096)).unwrap();
        cap.validate().unwrap();
        assert_eq!(cap.prepared_helper_sha256, helper_sha);
        assert_eq!(cap.qualification_script_sha256, producer_sha);
        assert_eq!(cap.native_qualification_sha256, digest(&native_file));
        let actual = crate::setup::applications_admission::installed().unwrap();
        assert_eq!(actual, cap);
        let output = tempfile::tempfile().unwrap();
        assert!(
            Command::new("/usr/bin/setpriv")
                .args([
                    "--reuid=1000",
                    "--regid=1001",
                    "--clear-groups",
                    "--no-new-privs",
                    "--",
                    selected_producer,
                ])
                .args(producer_mode)
                .arg("--check")
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("LANG", "C")
                .env("LC_ALL", "C")
                .stdin(Stdio::null())
                .stdout(Stdio::from(output.try_clone().unwrap()))
                .status()
                .unwrap()
                .success()
        );
        let checked: crate::setup::applications_admission::ConsumerCapability =
            serde_json::from_slice(&read(&output, 4096)).unwrap();
        assert_eq!(checked, cap);
        assert_eq!(stamp(&cap_file), cap_stamp);
        assert_eq!(stamp(&native_file), native_stamp);
        assert!(
            matches!(fs::symlink_metadata("/run/tilo-applications-qualification"), Err(e) if e.kind()==std::io::ErrorKind::NotFound),
            "receiving target must have retired before any ready receipt"
        );
    }
}
