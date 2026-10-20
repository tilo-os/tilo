#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::unreachable,
    clippy::too_many_lines,
    clippy::items_after_statements,
    reason = "in the TPM simulator fixture these are the assertion, not a hazard"
)]

use super::native::MAX_SEAL_INPUT;
use super::*;
use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::{
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU16, Ordering},
    time::Duration,
};
use tilo_rollback::exec::Runner;
use tilo_rollback::{
    exec,
    policy::{self, Workspace},
};

struct Tpm {
    _state: tempfile::TempDir,
    port: u16,
    child: Child,
}

impl Tpm {
    fn start() -> Self {
        for tool in ["swtpm", "swtpm_setup", "tpm2_createek", "tpm2_getcap"] {
            let ok = Command::new("sh")
                .arg("-c")
                .arg(format!("command -v {tool} >/dev/null 2>&1"))
                .status()
                .is_ok_and(|status| status.success());
            assert!(ok, "the prepared TPM handoff test needs {tool} installed");
        }
        let mut last = String::new();
        for _ in 0..8 {
            match Self::try_start() {
                Ok(tpm) => return tpm,
                Err(error) => last = error,
            }
        }
        panic!("could not start swtpm after 8 attempts: {last}");
    }

    fn try_start() -> Result<Self, String> {
        let state = tempfile::tempdir().map_err(|error| error.to_string())?;
        let (port, control) = free_port_pair();
        let setup_log = state.path().join("setup.log");
        let _ = Command::new("swtpm_setup")
            .args(["--tpm2", "--tpmstate"])
            .arg(state.path())
            .stdout(Stdio::from(
                std::fs::File::create(&setup_log).map_err(|error| error.to_string())?,
            ))
            .stderr(Stdio::from(
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&setup_log)
                    .map_err(|error| error.to_string())?,
            ))
            .status();
        let log = state.path().join("swtpm.log");
        let child = Command::new("swtpm")
            .arg("socket")
            .arg("--tpm2")
            .arg(format!("--tpmstate=dir={}", state.path().display()))
            .arg(format!("--server=type=tcp,port={port}"))
            .arg(format!("--ctrl=type=tcp,port={control}"))
            .arg("--flags=not-need-init,startup-clear")
            .stdin(Stdio::null())
            .stdout(Stdio::from(
                std::fs::File::create(&log).map_err(|error| error.to_string())?,
            ))
            .stderr(Stdio::from(
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&log)
                    .map_err(|error| error.to_string())?,
            ))
            .spawn()
            .map_err(|error| error.to_string())?;
        let mut tpm = Self {
            _state: state,
            port,
            child,
        };
        let mut last = String::new();
        for _ in 0..300 {
            std::thread::sleep(Duration::from_millis(100));
            if let Ok(Some(status)) = tpm.child.try_wait() {
                return Err(format!("swtpm exited ({status})"));
            }
            match Command::new("tpm2_getcap")
                .arg("properties-fixed")
                .env("TPM2TOOLS_TCTI", tpm.tcti())
                .output()
            {
                Ok(output) if output.status.success() => return Ok(tpm),
                Ok(output) => last = String::from_utf8_lossy(&output.stderr).into_owned(),
                Err(error) => last = error.to_string(),
            }
        }
        Err(format!("swtpm never answered: {last}"))
    }

    fn tcti(&self) -> String {
        format!("swtpm:host=127.0.0.1,port={}", self.port)
    }
}

impl Drop for Tpm {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port_pair() -> (u16, u16) {
    static SKEW: AtomicU16 = AtomicU16::new(0);
    let skew = SKEW.fetch_add(2, Ordering::Relaxed);
    for attempt in 0..64u16 {
        let base = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            listener.local_addr().expect("local address").port()
        };
        let base = base
            .checked_add(skew)
            .and_then(|base| base.checked_add(attempt.saturating_mul(2)))
            .unwrap_or(base);
        if base >= u16::MAX - 1 {
            continue;
        }
        if std::net::TcpListener::bind(("127.0.0.1", base)).is_ok()
            && std::net::TcpListener::bind(("127.0.0.1", base.saturating_add(1))).is_ok()
        {
            return (base, base.saturating_add(1));
        }
    }
    panic!("no adjacent free TCP port pair for swtpm");
}

struct Fixture {
    _tpm: Tpm,
    scratch: tempfile::TempDir,
    runner: exec::Real,
}

impl Fixture {
    fn new() -> Self {
        let tpm = Tpm::start();
        let scratch = tempfile::tempdir().expect("scratch");
        let runner = exec::Real::new()
            .with_tcti(Some(&tpm.tcti()))
            .with_cwd(scratch.path());
        Self {
            _tpm: tpm,
            scratch,
            runner,
        }
    }

    fn ws(&self) -> Workspace {
        Workspace::new(self.scratch.path())
    }

    fn target(&self, generation: u64) -> (SignedTarget, PathBuf) {
        let key = tilo_crypto::generate_tpm_authority_key().expect("authority key");
        let public = self.scratch.path().join("authority.public.pem");
        std::fs::write(&public, key.public_key_pem()).unwrap();
        let ws = self.ws();
        let approved = policy::approved_policy(&self.runner, &ws, "sha256:11", None)
            .expect("live PCR11 policy");
        let mut message = approved.clone();
        message.extend_from_slice(&generation.to_be_bytes());
        let signature =
            tilo_crypto::sign_rsa_sha256(key.private_key_pem(), &message).expect("sign prediction");
        let prediction = serde_json::json!({
            "generation": generation,
            "pcr_bank": "sha256",
            "pcrs": [11],
            "approved_policy": b64(&approved),
            "signature": b64(&signature),
            "label": "actual swtpm target",
        });
        let uki = b"an exact signed target UKI fixture".to_vec();
        let uki_signature =
            tilo_crypto::sign_rsa_sha256(key.private_key_pem(), &uki).expect("sign UKI");
        let prediction_bytes = serde_json::to_vec(&prediction).unwrap();
        let target = review(
            &prediction_bytes,
            key.public_key_pem().as_bytes(),
            &uki,
            &uki_signature,
            generation,
        )
        .expect("verified target policy");
        let bundle = self.bundle(generation);
        let predictions = bundle.join(PREDICTIONS_DIR);
        std::fs::create_dir_all(&predictions).unwrap();
        std::fs::set_permissions(&bundle, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&predictions, std::fs::Permissions::from_mode(0o700)).unwrap();
        for (path, bytes) in [
            (bundle.join(TARGET_UKI_FILE), uki.as_slice()),
            (
                predictions.join(format!("generation-{generation}.json")),
                prediction_bytes.as_slice(),
            ),
            (
                predictions.join(format!("generation-{generation}.json.uki.sig")),
                uki_signature.as_slice(),
            ),
        ] {
            std::fs::write(&path, bytes).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        (target, public)
    }

    fn bundle(&self, generation: u64) -> PathBuf {
        self.scratch.path().join(format!("bundle-{generation}"))
    }

    fn private_package(&self, name: &str, bytes: &[u8], mode: u32) -> PathBuf {
        let directory = self.scratch.path().join(name);
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.join("prepared-setup.sealed");
        std::fs::write(&path, bytes).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    }
}

fn caller_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

fn authority(public: &Path) -> Vec<u8> {
    std::fs::read(public).unwrap()
}

#[test]
fn the_helper_reader_refuses_an_oversized_request_before_any_tpm_operation() {
    let input = vec![b' '; MAX_SEAL_INPUT + 1];
    let mut output = Vec::new();
    let error = seal_from_reader(&input[..], &mut output)
        .expect_err("the root helper must not grow its heap to the caller's pipe");
    assert!(error.to_string().contains("exceeds its bound"));
    assert!(output.is_empty());
}

#[test]
fn actual_swtpm_seals_to_the_signed_target_and_this_tpm_and_refuses_another() {
    let fixture = Fixture::new();
    let (target, public) = fixture.target(7);
    let secret = generate_secret().expect("machine secret");
    let request = SealRequest {
        target: &target,
        source_public_key: [7; 32],
        package_sha256: [8; 32],
        receiver_uid: RECEIVER_UID,
        secret: secret.as_bytes(),
    };
    let ws = fixture.ws();
    let sealed =
        seal(&fixture.runner, &ws, &request, &authority(&public)).expect("seal to actual TPM");
    sealed.validate().unwrap();
    let encoded = sealed.encode().unwrap();
    assert!(!String::from_utf8_lossy(&encoded).contains(secret.as_str()));
    assert_eq!(sealed.generation(), 7);
    assert_eq!(sealed.source_public_key(), [7; 32]);
    assert_eq!(sealed.package_sha256(), [8; 32]);
    let sidecar = SealedPreparation::decode(&encoded).unwrap();
    let opened = unseal(&fixture.runner, &ws, &sidecar, &authority(&public), 7)
        .expect("policy and TPM are present")
        .expect("the signed target state matches");
    assert_eq!(opened.as_slice(), secret.as_bytes());

    let elsewhere = Fixture::new();
    let public_bytes = std::fs::read(&public).unwrap();
    let other_public = elsewhere.scratch.path().join("authority.public.pem");
    std::fs::write(&other_public, public_bytes).unwrap();
    let error = unseal(
        &elsewhere.runner,
        &elsewhere.ws(),
        &sidecar,
        &authority(&other_public),
        7,
    )
    .expect_err("a copied sidecar must name another TPM");
    assert!(
        error.to_string().contains("another TPM"),
        "unexpected error: {error}"
    );
}

#[test]
fn changing_the_signed_boot_or_platform_state_refuses_to_unseal() {
    let fixture = Fixture::new();
    let (target, public) = fixture.target(11);
    let secret = generate_secret().unwrap();
    let request = SealRequest {
        target: &target,
        source_public_key: [9; 32],
        package_sha256: [10; 32],
        receiver_uid: RECEIVER_UID,
        secret: secret.as_bytes(),
    };
    let ws = fixture.ws();
    let sealed = seal(&fixture.runner, &ws, &request, &authority(&public)).expect("seal");
    let pcr11 = fixture
        .runner
        .run(
            "tpm2_pcrextend",
            &[OsStr::new(
                "11:sha256=0000000000000000000000000000000000000000000000000000000000000001",
            )],
            None,
        )
        .expect("extend PCR 11");
    assert!(pcr11.ok(), "{}", pcr11.last_error_line());
    assert!(
        unseal(&fixture.runner, &ws, &sealed, &authority(&public), 11)
            .expect("a changed PCR is an expected mismatch")
            .is_none(),
        "a changed PCR 11 must not satisfy the signed target prediction"
    );
}

#[test]
fn a_wrong_generation_or_authority_is_a_hard_refusal() {
    let fixture = Fixture::new();
    let (target, public) = fixture.target(3);
    let secret = generate_secret().unwrap();
    let ws = fixture.ws();
    let sealed = seal(
        &fixture.runner,
        &ws,
        &SealRequest {
            target: &target,
            source_public_key: [1; 32],
            package_sha256: [2; 32],
            receiver_uid: RECEIVER_UID,
            secret: secret.as_bytes(),
        },
        &authority(&public),
    )
    .expect("seal");
    assert!(
        unseal(&fixture.runner, &ws, &sealed, &authority(&public), 4)
            .expect_err("another generation must be refused")
            .to_string()
            .contains("generation")
    );
    let other = tilo_crypto::generate_tpm_authority_key().unwrap();
    let other_public = fixture.scratch.path().join("other.public.pem");
    std::fs::write(&other_public, other.public_key_pem()).unwrap();
    assert!(
        unseal(&fixture.runner, &ws, &sealed, &authority(&other_public), 3)
            .expect_err("another authority must be refused")
            .to_string()
            .contains("release authority")
    );
}

#[test]
fn the_privileged_seal_helper_accepts_json_on_stdin_and_outputs_only_the_public_sidecar() {
    let fixture = Fixture::new();
    let (target, public) = fixture.target(5);
    let secret = generate_secret().unwrap();
    let package_bytes = b"the exact canonical prepared package";
    let package = fixture.private_package("caller-package", package_bytes, 0o600);
    let digest: [u8; 32] = sha256(package_bytes);
    let input = serde_json::to_vec(&serde_json::json!({
        "generation": target.generation(),
        "target_uki_sha256": target.target_uki_sha256(),
        "target_bundle_path": fixture.bundle(target.generation()).to_str().unwrap(),
        "package_path": package.to_str().unwrap(),
        "source_public_key": vec![4u8; 32],
        "package_sha256": digest,
        "secret": secret.as_str(),
    }))
    .unwrap();
    let mut output = Vec::new();
    seal_from_json_with(
        &input,
        &mut output,
        &authority(&public),
        caller_uid(),
        &fixture.runner,
        &fixture.ws(),
    )
    .expect("privileged helper seals");
    assert!(!String::from_utf8_lossy(&output).contains(secret.as_str()));
    assert!(!String::from_utf8_lossy(&output).contains(package.to_str().unwrap()));
    let sealed = SealedPreparation::decode(&output).unwrap();
    assert_eq!(sealed.source_public_key(), [4; 32]);
    assert_eq!(sealed.package_sha256(), digest);
    assert_eq!(sealed.receiver_uid(), RECEIVER_UID);
    assert_eq!(sealed.generation(), 5);
    let opened = unseal(
        &fixture.runner,
        &fixture.ws(),
        &sealed,
        &authority(&public),
        5,
    )
    .expect("same TPM and target")
    .expect("matching live PCRs");
    assert_eq!(opened.as_slice(), secret.as_bytes());
}

#[test]
fn the_helper_refuses_a_target_or_package_the_release_authority_did_not_bind() {
    let fixture = Fixture::new();
    let (target, public) = fixture.target(6);
    let secret = generate_secret().unwrap();
    let package_bytes = b"the exact canonical prepared package";
    let package = fixture.private_package("caller-package", package_bytes, 0o600);
    let digest: [u8; 32] = sha256(package_bytes);
    let request = |generation: u64, uki: [u8; 32], package_sha256: [u8; 32]| {
        serde_json::to_vec(&serde_json::json!({
            "generation": generation,
            "target_uki_sha256": uki,
            "target_bundle_path": fixture.bundle(generation).to_str().unwrap(),
            "package_path": package.to_str().unwrap(),
            "source_public_key": vec![4u8; 32],
            "package_sha256": package_sha256,
            "secret": secret.as_str(),
        }))
        .unwrap()
    };
    let mut output = Vec::new();
    assert!(
        seal_from_json_with(
            &request(target.generation() + 1, target.target_uki_sha256(), digest),
            &mut output,
            &authority(&public),
            caller_uid(),
            &fixture.runner,
            &fixture.ws(),
        )
        .is_err(),
        "a caller-chosen generation cannot replace the reviewed target"
    );
    assert!(
        seal_from_json_with(
            &request(target.generation(), [9; 32], digest),
            &mut output,
            &authority(&public),
            caller_uid(),
            &fixture.runner,
            &fixture.ws(),
        )
        .is_err(),
        "a caller-chosen UKI cannot replace the reviewed target"
    );
    assert!(
        seal_from_json_with(
            &request(target.generation(), target.target_uki_sha256(), [8; 32]),
            &mut output,
            &authority(&public),
            caller_uid(),
            &fixture.runner,
            &fixture.ws(),
        )
        .is_err(),
        "the package digest must be the bytes the helper hashes"
    );
}

#[test]
fn the_helper_refuses_a_package_the_original_caller_does_not_privately_own() {
    let fixture = Fixture::new();
    let (target, public) = fixture.target(6);
    let secret = generate_secret().unwrap();
    let package_bytes = b"the exact canonical prepared package";
    let digest: [u8; 32] = sha256(package_bytes);
    let input = |package: &Path| {
        serde_json::to_vec(&serde_json::json!({
            "generation": target.generation(),
            "target_uki_sha256": target.target_uki_sha256(),
            "package_path": package.to_str().unwrap(),
            "source_public_key": vec![4u8; 32],
            "package_sha256": digest,
            "secret": secret.as_str(),
        }))
        .unwrap()
    };
    let mut output = Vec::new();
    let public_package = fixture.private_package("public-package", package_bytes, 0o644);
    assert!(
        seal_from_json_with(
            &input(&public_package),
            &mut output,
            &authority(&public),
            caller_uid(),
            &fixture.runner,
            &fixture.ws(),
        )
        .is_err(),
        "a world-readable package is not the caller's private artifact"
    );
    let private_package = fixture.private_package("private-package", package_bytes, 0o600);
    assert!(
        seal_from_json_with(
            &input(&private_package),
            &mut output,
            &authority(&public),
            caller_uid().saturating_add(1),
            &fixture.runner,
            &fixture.ws(),
        )
        .is_err(),
        "another UID cannot authorize this package"
    );
    let linked = private_package
        .parent()
        .unwrap()
        .join("linked-package.sealed");
    std::os::unix::fs::symlink(&private_package, &linked).unwrap();
    assert!(
        seal_from_json_with(
            &input(&linked),
            &mut output,
            &authority(&public),
            caller_uid(),
            &fixture.runner,
            &fixture.ws(),
        )
        .is_err(),
        "a symlink is never the caller's package"
    );
    std::fs::set_permissions(
        private_package.parent().unwrap(),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(
        seal_from_json_with(
            &input(&private_package),
            &mut output,
            &authority(&public),
            caller_uid(),
            &fixture.runner,
            &fixture.ws(),
        )
        .is_err(),
        "a package directory readable by others is refused"
    );
}

#[test]
fn a_target_bundle_is_reviewed_from_its_signed_files_without_a_live_image() {
    let fixture = Fixture::new();
    let (target, public) = fixture.target(17);
    let bundle = fixture.bundle(17);
    let reviewed =
        review_bundle(&bundle, &authority(&public)).expect("the signed bundle is a target policy");
    assert_eq!(reviewed.generation(), target.generation());
    assert_eq!(reviewed.target_uki_sha256(), target.target_uki_sha256());

    assert!(
        review_bundle(&bundle, PINNED_RELEASE_AUTHORITY_PEM.as_bytes()).is_err(),
        "a bundle signed by another authority must not be accepted by the pinned helper"
    );
    assert!(
        review_bundle(&fixture.scratch.path().join("missing"), &authority(&public)).is_err(),
        "a missing bundle is a refusal, not an empty policy"
    );
}

#[test]
fn the_helper_reviews_the_callers_held_bundle_and_never_the_callers_authority() {
    let fixture = Fixture::new();
    let (target, public) = fixture.target(18);
    let secret = generate_secret().unwrap();
    let package_bytes = b"the exact canonical prepared package";
    let package = fixture.private_package("bundle-package", package_bytes, 0o600);
    let input = serde_json::to_vec(&serde_json::json!({
        "generation": target.generation(),
        "target_uki_sha256": target.target_uki_sha256(),
        "target_bundle_path": fixture.bundle(18).to_str().unwrap(),
        "package_path": package.to_str().unwrap(),
        "source_public_key": vec![4u8; 32],
        "package_sha256": sha256(package_bytes),
        "secret": secret.as_str(),
    }))
    .unwrap();
    let mut output = Vec::new();
    assert!(
        seal_from_json_with(
            &input,
            &mut output,
            PINNED_RELEASE_AUTHORITY_PEM.as_bytes(),
            caller_uid(),
            &fixture.runner,
            &fixture.ws(),
        )
        .is_err(),
        "the caller cannot replace the pinned release authority with its own bundle"
    );
    assert!(output.is_empty());

    let mut output = Vec::new();
    seal_from_json_with(
        &input,
        &mut output,
        &authority(&public),
        caller_uid(),
        &fixture.runner,
        &fixture.ws(),
    )
    .expect("the same signed bundle is accepted under its actual authority");
    assert!(!output.is_empty());

    let mut output = Vec::new();
    assert!(
        seal_from_json_with(
            &input,
            &mut output,
            &authority(&public),
            caller_uid().saturating_add(1),
            &fixture.runner,
            &fixture.ws(),
        )
        .is_err(),
        "another UID cannot authorize the caller's target bundle"
    );
    assert!(output.is_empty());

    std::fs::set_permissions(fixture.bundle(18), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        seal_from_json_with(
            &input,
            &mut output,
            &authority(&public),
            caller_uid(),
            &fixture.runner,
            &fixture.ws(),
        )
        .is_err(),
        "a target bundle directory readable by others is refused"
    );
}

#[test]
fn a_forged_prediction_under_the_real_authority_digest_is_refused() {
    let fixture = Fixture::new();
    let (mut target, public) = fixture.target(9);
    target.approved_policy = b64(&[3; 32]);
    target.signature = b64(&vec![4; 256]);
    let secret = generate_secret().unwrap();
    let error = seal(
        &fixture.runner,
        &fixture.ws(),
        &SealRequest {
            target: &target,
            source_public_key: [7; 32],
            package_sha256: [8; 32],
            receiver_uid: RECEIVER_UID,
            secret: secret.as_bytes(),
        },
        &authority(&public),
    )
    .expect_err("the release authority did not sign this prediction");
    assert!(
        error.to_string().contains("prediction") || error.to_string().contains("signature"),
        "unexpected error: {error}"
    );
}

#[test]
fn a_sidecar_with_a_forged_policy_signature_is_refused_before_unsealing() {
    let fixture = Fixture::new();
    let (target, public) = fixture.target(13);
    let secret = generate_secret().unwrap();
    let sealed = seal(
        &fixture.runner,
        &fixture.ws(),
        &SealRequest {
            target: &target,
            source_public_key: [7; 32],
            package_sha256: [8; 32],
            receiver_uid: RECEIVER_UID,
            secret: secret.as_bytes(),
        },
        &authority(&public),
    )
    .expect("seal to actual TPM");
    let mut forged = sealed;
    forged.policy.approved_policy = b64(&[3; 32]);
    forged.policy.signature = b64(&vec![4; 256]);
    forged.seal_binding().unwrap();
    forged.validate().unwrap();
    let error = unseal(
        &fixture.runner,
        &fixture.ws(),
        &forged,
        &authority(&public),
        13,
    )
    .expect_err("a forged prediction under the real authority digest must be refused");
    assert!(
        error.to_string().contains("prediction") || error.to_string().contains("signature"),
        "unexpected error: {error}"
    );
}

#[test]
fn a_changed_pcr7_platform_state_or_edited_sidecar_is_refused() {
    let fixture = Fixture::new();
    let (target, public) = fixture.target(12);
    let secret = generate_secret().unwrap();
    let ws = fixture.ws();
    let sealed = seal(
        &fixture.runner,
        &ws,
        &SealRequest {
            target: &target,
            source_public_key: [9; 32],
            package_sha256: [10; 32],
            receiver_uid: RECEIVER_UID,
            secret: secret.as_bytes(),
        },
        &authority(&public),
    )
    .expect("seal");
    assert!(
        unseal(&fixture.runner, &ws, &sealed, &authority(&public), 12)
            .expect("matching PCR 7")
            .is_some()
    );

    let mut replay = sealed.clone();
    replay.package_sha256 = [11; 32];
    assert!(
        replay.encode().is_err(),
        "a sidecar repointed at another package is refused"
    );
    let mut edited = sealed.clone();
    edited.seal_nonce = b64(&[1; 32]);
    assert!(
        edited.encode().is_err(),
        "an edited nonce breaks the public binding"
    );

    let second = seal(
        &fixture.runner,
        &ws,
        &SealRequest {
            target: &target,
            source_public_key: [9; 32],
            package_sha256: [10; 32],
            receiver_uid: RECEIVER_UID,
            secret: secret.as_bytes(),
        },
        &authority(&public),
    )
    .expect("second seal");
    assert_ne!(
        sealed.encode().unwrap(),
        second.encode().unwrap(),
        "each seal carries a fresh nonce"
    );

    let pcr7 = fixture
        .runner
        .run(
            "tpm2_pcrextend",
            &[OsStr::new(
                "7:sha256=0000000000000000000000000000000000000000000000000000000000000001",
            )],
            None,
        )
        .expect("extend PCR 7");
    assert!(pcr7.ok(), "{}", pcr7.last_error_line());
    match unseal(&fixture.runner, &ws, &sealed, &authority(&public), 12) {
        Ok(None) | Err(_) => {}
        Ok(Some(_)) => panic!("a changed platform boot state must not satisfy the policy"),
    }
}

#[test]
fn a_real_swtpm_sidecar_and_a_large_mail_journal_compose_without_growing_the_package() {
    use crate::account_transfer::ThunderbirdSelection;
    use crate::browser_selection::SelectedSources;
    use crate::setup::{
        deferred_handoff,
        migration_handoff::{self, Categories, HomePolicy},
        portable_transfer::{ExportReview, Purpose},
        prepared_identity::PreparedIdentity,
        prepared_setup::{IDENTITY_FILE, LIMITS, Manifest},
    };
    use tilo_identity_host::provisioning::IdentityStorage;

    fn write(path: &Path, bytes: impl AsRef<[u8]>) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    // A mailbox larger than the former 64 MiB inspection cap. It never enters
    // the package: the journal keeps only the reviewed message and folder
    // metadata, and first boot streams the original bytes.
    let fixture = Fixture::new();
    let home = fixture.scratch.path().join("source-home");
    std::fs::create_dir(&home).unwrap();
    let profile = home.join(".thunderbird/abc.default");
    write(
        &home.join(".thunderbird/profiles.ini"),
        "[Profile0]\nName=Personal\nIsRelative=1\nPath=abc.default\n",
    );
    write(
        &profile.join("prefs.js"),
        r#"user_pref("mail.accountmanager.accounts", "account1");
user_pref("mail.account.account1.server", "server1");
user_pref("mail.account.account1.identities", "id1");
user_pref("mail.server.server1.type", "imap");
user_pref("mail.server.server1.hostname", "imap.example.test");
user_pref("mail.server.server1.directory-rel", "[ProfD]ImapMail/imap.example.test");
user_pref("mail.identity.id1.useremail", "person@example.test");
user_pref("mail.identity.id1.fullName", "Personal");
"#,
    );
    const COUNT: usize = 1400;
    const MESSAGE_BYTES: usize = 48 * 1024;
    let mut inbox = Vec::new();
    let filler = vec![b'x'; MESSAGE_BYTES];
    for index in 0..COUNT {
        inbox.extend_from_slice(
            format!(
                "From - Mon Jan 01 00:00:00 2024\r\nFrom: Ada <ada@example.test>\r\nSubject: Message {index}\r\nDate: Mon, 01 Jan 2024 00:00:00 +0000\r\nMessage-ID: <{index}@example.test>\r\nX-Mozilla-Status: 0001\r\nContent-Type: text/plain\r\n\r\n"
            )
            .as_bytes(),
        );
        inbox.extend_from_slice(&filler);
        inbox.extend_from_slice(b"\r\n");
    }
    let mbox = profile.join("ImapMail/imap.example.test/Inbox");
    write(&mbox, &inbox);
    drop(inbox);
    assert!(
        std::fs::metadata(&mbox).unwrap().len() > 64 * 1024 * 1024,
        "the fixture must exceed the old whole-mail inspection bound"
    );

    let inventory = crate::account_sources::discover(&crate::account_sources::SourceRoots {
        home: home.clone(),
        config: home.join(".config"),
    });
    let reviewed = inventory.thunderbird_reviewed.first().unwrap().clone();
    let selection = ThunderbirdSelection::all(&reviewed);
    let mut plan = migration_handoff::capture_fixture(
        &home,
        &uuid::Uuid::new_v4().to_string(),
        Categories {
            home: true,
            flatpak: false,
            accounts: false,
        },
    )
    .unwrap();
    plan.home_policy = HomePolicy::Copy;
    plan.home_exclusions = Vec::new();
    plan.set_fixture_original_uid(deferred_handoff::ORIGINAL_UID);
    plan.validate().unwrap();
    let prepared = deferred_handoff::prepare(&deferred_handoff::Prepare {
        source_home: &home,
        plan: &plan,
        exclusions: &plan.home_exclusions,
        mail: &[(reviewed, selection)],
        browser: &SelectedSources::default(),
        applications: false,
        accounts: false,
        account_sources: &[],
        accounts_in_package: false,
        browser_in_package: false,
    })
    .unwrap();
    plan.attach_deferred_handoff(prepared.binding.clone())
        .unwrap();
    let journal_bytes = std::fs::read(&prepared.journal).unwrap();
    assert!(
        journal_bytes.len() < 16 * 1024,
        "the journal carries reviewed metadata, not a {} MiB mailbox",
        std::fs::metadata(&mbox).unwrap().len() / (1024 * 1024)
    );

    // A real prepared identity package, not a synthetic blob. Its exact bytes,
    // not the mailbox, are what the TPM sidecar binds.
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set(runtime_identity::FILE_SECRETS_ENV, "1");
    std::fs::set_permissions(
        fixture.scratch.path(),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let staging = fixture.scratch.path().join("tpm-staging");
    std::fs::create_dir(&staging).unwrap();
    std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o700)).unwrap();
    let package_password = "a sufficiently long preparation password";
    let identity = PreparedIdentity::create(
        &fixture.scratch.path().join("tpm-identity"),
        "Ada",
        [5, 6, 7],
        IdentityStorage::Files,
    )
    .unwrap();
    identity
        .export(
            &staging.join(IDENTITY_FILE),
            Zeroizing::new(package_password.into()),
        )
        .unwrap();
    Manifest::new("tpm-composite".into(), identity.public_key(), Vec::new())
        .unwrap()
        .write(&staging)
        .unwrap();
    let package = fixture.scratch.path().join("prepared-setup.sealed");
    ExportReview::review(&staging, &package, Purpose::PreparedSetup, LIMITS)
        .unwrap()
        .export(
            Zeroizing::new(package_password.as_bytes().to_vec()),
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();
    std::fs::set_permissions(&package, std::fs::Permissions::from_mode(0o600)).unwrap();
    let package_bytes = std::fs::read(&package).unwrap();
    let package_sha256: [u8; 32] = sha256(&package_bytes);
    assert!(
        u64::try_from(package_bytes.len()).unwrap_or(u64::MAX)
            <= crate::staging::prepared_setup::SLOT_BYTES,
        "the identity package must stay inside the canonical prepared-setup bound"
    );
    let (target, public) = fixture.target(13);
    let secret = generate_secret().unwrap();
    let input = serde_json::to_vec(&serde_json::json!({
        "generation": target.generation(),
        "target_uki_sha256": target.target_uki_sha256(),
        "target_bundle_path": fixture.bundle(target.generation()).to_str().unwrap(),
        "package_path": package.to_str().unwrap(),
        "source_public_key": identity.public_key(),
        "package_sha256": package_sha256,
        "secret": secret.as_str(),
    }))
    .unwrap();
    let mut output = Vec::new();
    seal_from_json_with(
        &input,
        &mut output,
        &authority(&public),
        caller_uid(),
        &fixture.runner,
        &fixture.ws(),
    )
    .expect("real TPM sidecar over the real identity package");
    let sealed = SealedPreparation::decode(&output).unwrap();
    assert_eq!(sealed.package_sha256(), package_sha256);
    assert_eq!(sealed.generation(), 13);
    let opened = unseal(
        &fixture.runner,
        &fixture.ws(),
        &sealed,
        &authority(&public),
        13,
    )
    .expect("same TPM and target")
    .expect("matching PCRs");
    assert_eq!(opened.as_slice(), secret.as_bytes());

    // The retained source still streams the whole mailbox after first boot;
    // the sidecar did not force it into the bounded package.
    let target_home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let receipt = deferred_handoff::import_selected_from_source(
        &plan,
        &home,
        target_home.path(),
        workspace.path(),
        [7; 32],
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap_or_else(|error| panic!("the retained mailbox must stream: {error}"));
    assert_eq!(receipt.mail.len(), 1);
    assert!(receipt.mail[0].accounts.iter().any(|account| {
        account
            .folders
            .iter()
            .any(|folder| folder.messages == COUNT as u64)
    }));
}
